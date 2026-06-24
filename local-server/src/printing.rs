//! Job execution: spooling, printer control, status, persistence, live activity.
use crate::config::{PricesConfig, PrintersConfig, QRCodesConfig};
use crate::error::*;
use crate::model::*;
use crate::sync::*;
use crate::text::{
    done_text, file_transfer_text, now_iso, printing_text, queue_position_text, queue_waiting_text,
};
use crate::util::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{fs, process::Command};
use tracing::warn;

pub(crate) async fn process_job(state: AppState, job_id: String) -> Result<(), ApiError> {
    let (printer_mode, printer_name, pdf_path, page_count, copy_count) = {
        let jobs = state.jobs.read().await;
        let record = jobs
            .get(&job_id)
            .ok_or_else(|| ApiError::not_found("job not found"))?;
        (
            record.printer,
            record.printer_name.clone(),
            PathBuf::from(record.pdf_path.clone()),
            record.job.page_count,
            record.job.copy_count,
        )
    };

    let printer_guard = match printer_mode {
        PrinterMode::Bw => state.bw_lock.clone().lock_owned().await,
        PrinterMode::Color => state.color_lock.clone().lock_owned().await,
    };

    if job_was_cancelled(&state, &job_id).await? {
        return Ok(());
    }

    set_job_status(
        &state,
        &job_id,
        JobStatus::Downloading,
        Some(file_transfer_text()),
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    if job_was_cancelled(&state, &job_id).await? {
        return Ok(());
    }

    set_job_status(
        &state,
        &job_id,
        JobStatus::Printing,
        Some(printing_text(&printer_name)),
    )
    .await?;

    let timeout_seconds = 180u64.saturating_mul(u64::from(copy_count.max(1)));
    let print_result = tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        execute_print(
            &state,
            &job_id,
            &pdf_path,
            &printer_name,
            page_count,
            copy_count,
        ),
    )
    .await;

    drop(printer_guard);

    match print_result {
        Ok(Ok(())) => {
            set_job_status(&state, &job_id, JobStatus::Done, Some(done_text())).await?;
            Ok(())
        }
        Ok(Err(error)) => {
            set_job_status(&state, &job_id, JobStatus::Failed, Some(error.detail())).await?;
            Ok(())
        }
        Err(_) => {
            set_job_status(
                &state,
                &job_id,
                JobStatus::Failed,
                Some(PrintFailure::Timeout.detail()),
            )
            .await?;
            Ok(())
        }
    }
}

pub(crate) async fn execute_print(
    state: &AppState,
    job_id: &str,
    pdf_path: &Path,
    printer_name: &str,
    page_count: u32,
    copy_count: u32,
) -> Result<(), PrintFailure> {
    let total_pages = page_count.saturating_mul(copy_count.max(1));
    let pdf_path = pdf_path
        .to_str()
        .ok_or_else(|| PrintFailure::Unknown("temporary PDF path is not valid UTF-8".to_owned()))?
        .to_owned();

    for copy_index in 0..copy_count.max(1) {
        let page_offset = copy_index.saturating_mul(page_count);
        let mut command = Command::new(state.sumatra_path.as_ref());
        command
            .args([
                "-print-to",
                printer_name,
                "-print-settings",
                "simplex",
                &pdf_path,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            PrintFailure::Unknown(format!("failed to launch SumatraPDF: {error}"))
        })?;

        loop {
            if let Some(exit_status) = child.try_wait().map_err(|error| {
                PrintFailure::Unknown(format!("failed to wait for SumatraPDF: {error}"))
            })? {
                let output = child.wait_with_output().await.map_err(|error| {
                    PrintFailure::Unknown(format!("failed to collect SumatraPDF output: {error}"))
                })?;

                if exit_status.success() {
                    let printed_pages = page_offset.saturating_add(page_count).min(total_pages);
                    let detail = printing_progress_text(printer_name, printed_pages, total_pages);
                    let _ =
                        set_job_print_progress(state, job_id, printed_pages, total_pages, detail)
                            .await;
                    break;
                }

                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(classify_print_failure(&stdout, &stderr, exit_status.code()));
            }

            if let Ok(Some(progress)) = probe_printer_progress(printer_name).await {
                if let Some(pages_printed) = progress.pages_printed {
                    let observed_copy_total =
                        progress.total_pages.unwrap_or(page_count).max(page_count);
                    let current_copy_pages = pages_printed.min(observed_copy_total).min(page_count);
                    let printed_pages = page_offset
                        .saturating_add(current_copy_pages)
                        .min(total_pages);
                    let detail = printing_progress_text(printer_name, printed_pages, total_pages);
                    let _ =
                        set_job_print_progress(state, job_id, printed_pages, total_pages, detail)
                            .await;
                } else if progress
                    .document_name
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    let detail = printing_progress_text(printer_name, page_offset, total_pages);
                    let _ = set_job_print_progress(state, job_id, page_offset, total_pages, detail)
                        .await;
                }
            } else if let Some(detail) = printing_waiting_text(printer_name) {
                let _ =
                    set_job_print_progress(state, job_id, page_offset, total_pages, detail).await;
            }

            tokio::time::sleep(Duration::from_millis(900)).await;
        }
    }

    Ok(())
}

pub(crate) async fn set_job_print_progress(
    state: &AppState,
    job_id: &str,
    pages_printed: u32,
    total_pages: u32,
    detail: String,
) -> Result<(), ApiError> {
    let updated_record = {
        let mut jobs = state.jobs.write().await;
        let record = jobs
            .get_mut(job_id)
            .ok_or_else(|| ApiError::not_found("job not found"))?;
        record.job.status = JobStatus::Printing;
        record.job.detail = Some(detail.clone());
        record.job.pages_printed = Some(pages_printed.min(total_pages));
        record.job.total_pages = Some(total_pages);
        record.updated_at = now_iso();
        record.clone()
    };

    persist_job_record(state, &updated_record).await;
    let _ = state.status_tx.send(StatusEvent {
        kind: StatusStreamKind::Job,
        job_id: job_id.to_owned(),
        status: JobStatus::Printing,
        detail: Some(detail),
        pages_printed: updated_record.job.pages_printed,
        total_pages: updated_record.job.total_pages,
        activity: None,
    });

    Ok(())
}

pub(crate) async fn probe_printer_progress(
    printer_name: &str,
) -> Result<Option<WindowsPrintJobProbe>, PrintFailure> {
    let escaped_printer_name = escape_powershell_single_quote(printer_name);
    let command = format!(
        "$ErrorActionPreference='Stop'; $jobs = Get-PrintJob -PrinterName '{escaped_printer_name}' | Sort-Object ID -Descending; if (-not $jobs) {{ return }}; $job = $jobs | Select-Object -First 1 DocumentName, PagesPrinted, TotalPages; $job | ConvertTo-Json -Compress"
    );

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &command,
        ])
        .output()
        .await
        .map_err(|error| {
            PrintFailure::Unknown(format!("failed to query print progress: {error}"))
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload = stdout.trim();
    if payload.is_empty() {
        return Ok(None);
    }

    serde_json::from_str::<WindowsPrintJobProbe>(payload)
        .map(Some)
        .map_err(|error| {
            PrintFailure::Unknown(format!("failed to parse print progress payload: {error}"))
        })
}

pub(crate) fn printing_waiting_text(printer_name: &str) -> Option<String> {
    Some(format!("正在发送到打印机：{printer_name}"))
}

pub(crate) fn escape_powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

pub(crate) fn printing_progress_text(
    printer_name: &str,
    pages_printed: u32,
    total_pages: u32,
) -> String {
    let safe_total = total_pages.max(1);
    let safe_printed = pages_printed.min(safe_total);
    format!("打印中：{printer_name}（{safe_printed}/{safe_total} 张）")
}

pub(crate) fn classify_print_failure(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> PrintFailure {
    let message = format!("{stdout}\n{stderr}").to_ascii_lowercase();

    if message.contains("paper") || message.contains("out of paper") {
        return PrintFailure::OutOfPaper;
    }
    if message.contains("offline")
        || message.contains("not ready")
        || message.contains("printer") && message.contains("not found")
    {
        return PrintFailure::PrinterOffline;
    }
    if message.contains("corrupt")
        || message.contains("damaged")
        || message.contains("cannot open")
        || message.contains("failed to load")
    {
        return PrintFailure::FileCorrupt;
    }

    PrintFailure::Unknown(format!(
        "\u{6253}\u{5370}\u{5931}\u{8d25}\u{ff0c}\u{9000}\u{51fa}\u{7801}\u{4e3a} {:?}\u{3002} SumatraPDF \u{8f93}\u{51fa}\u{ff1a}{}",
        exit_code,
        stderr.trim()
    ))
}

pub(crate) async fn set_job_status(
    state: &AppState,
    job_id: &str,
    status: JobStatus,
    detail: Option<String>,
) -> Result<(), ApiError> {
    let mut updated_record = {
        let mut jobs = state.jobs.write().await;
        let record = jobs
            .get_mut(job_id)
            .ok_or_else(|| ApiError::not_found("job not found"))?;

        record.job.status = status;
        record.job.detail = detail.clone();
        let total_pages = record
            .job
            .page_count
            .saturating_mul(record.job.copy_count.max(1));
        if status == JobStatus::Queued {
            record.job.pages_printed = Some(0);
            record.job.total_pages = Some(total_pages);
        }
        if status == JobStatus::Done {
            record.job.pages_printed = Some(total_pages);
            record.job.total_pages = Some(total_pages);
        }
        record.updated_at = now_iso();
        record.clone()
    };

    let queue_snapshot = {
        let mut queue = state.active_queue.write().await;
        match status {
            JobStatus::Queued => {
                if !queue.iter().any(|queued_job_id| queued_job_id == job_id) {
                    queue.push(job_id.to_owned());
                }
            }
            JobStatus::Downloading | JobStatus::Printing | JobStatus::Done | JobStatus::Failed => {
                queue.retain(|queued_job_id| queued_job_id != job_id);
            }
        }
        queue.clone()
    };

    if status == JobStatus::Queued {
        let queue_detail = queue_position_message(state, job_id).await?;
        {
            let mut jobs = state.jobs.write().await;
            let record = jobs
                .get_mut(job_id)
                .ok_or_else(|| ApiError::not_found("job not found"))?;
            record.job.detail = Some(queue_detail.clone());
            record.updated_at = now_iso();
            updated_record = record.clone();
        }
    }

    persist_job_record(state, &updated_record).await;
    persist_active_queue(state, &queue_snapshot).await;

    let event = StatusEvent {
        kind: StatusStreamKind::Job,
        job_id: job_id.to_owned(),
        status,
        detail: updated_record.job.detail.clone(),
        pages_printed: updated_record.job.pages_printed,
        total_pages: updated_record.job.total_pages,
        activity: None,
    };
    let _ = state.status_tx.send(event);

    rebroadcast_waiting_positions(state).await;
    Ok(())
}

pub(crate) async fn queue_position_message(
    state: &AppState,
    job_id: &str,
) -> Result<String, ApiError> {
    let queue = state.active_queue.read().await.clone();
    let jobs = state.jobs.read().await;
    let current = jobs
        .get(job_id)
        .ok_or_else(|| ApiError::not_found("job not found"))?;

    let mut position = 0usize;
    for queued_job_id in queue {
        let Some(record) = jobs.get(&queued_job_id) else {
            continue;
        };

        if record.printer == current.printer {
            position += 1;
        }

        if queued_job_id == job_id {
            return Ok(queue_position_text(position.max(1)));
        }
    }

    Ok(queue_waiting_text())
}

pub(crate) async fn rebroadcast_waiting_positions(state: &AppState) {
    let queue = state.active_queue.read().await.clone();

    for queued_job_id in queue {
        let message = match queue_position_message(state, &queued_job_id).await {
            Ok(value) => value,
            Err(error) => {
                warn!("failed to compute queue position: {error}");
                continue;
            }
        };

        let updated_record = {
            let mut jobs = state.jobs.write().await;
            let Some(record) = jobs.get_mut(&queued_job_id) else {
                continue;
            };
            record.job.detail = Some(message.clone());
            record.updated_at = now_iso();
            record.clone()
        };

        persist_job_record(state, &updated_record).await;
        let _ = state.status_tx.send(StatusEvent {
            kind: StatusStreamKind::Job,
            job_id: queued_job_id,
            status: JobStatus::Queued,
            detail: Some(message),
            pages_printed: updated_record.job.pages_printed,
            total_pages: updated_record.job.total_pages,
            activity: None,
        });
    }
}

pub(crate) async fn persist_job_record(state: &AppState, record: &LocalJobRecord) {
    if record.sync_managed {
        if let Err(error) = persist_sync_job_journal(state, record).await {
            warn!(
                "failed to persist local print sync job {}: {error}",
                record.job.id
            );
        }
        if let Some(sync) = &state.print_sync {
            let status = match record.job.status {
                JobStatus::Queued | JobStatus::Downloading => "queued",
                JobStatus::Printing => "printing",
                JobStatus::Done => "done",
                JobStatus::Failed => "failed",
            };
            let request = PrintSyncJobStatusRequest {
                job_id: &record.job.id,
                status,
                detail: record.job.detail.as_deref(),
                pages_printed: record.job.pages_printed,
                total_pages: record.job.total_pages,
            };
            if let Err(error) = print_sync_post::<_, serde_json::Value>(
                sync,
                "/api/print-sync/job-status",
                &request,
            )
            .await
            {
                warn!(
                    "failed to sync print job {} to Worker: {error}",
                    record.job.id
                );
            }
        }
        return;
    }

    if let Some(cloudflare) = &state.cloudflare {
        if let Err(error) = cloudflare
            .put_json(&format!("queue:job:{}", record.job.id), &record.job)
            .await
        {
            warn!(
                "failed to sync job {} to Cloudflare KV: {error}",
                record.job.id
            );
        }
    }
}

pub(crate) async fn persist_sync_job_journal(
    state: &AppState,
    record: &LocalJobRecord,
) -> Result<(), ApiError> {
    let path = sync_job_record_path(state.storage_dir.as_ref(), &record.job.id);
    let temp_path = path.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(record).map_err(|error| {
        ApiError::internal(format!("failed to encode sync job journal: {error}"))
    })?;
    fs::write(&temp_path, payload).await.map_err(|error| {
        ApiError::internal(format!("failed to write sync job journal: {error}"))
    })?;
    if fs::metadata(&path).await.is_ok() {
        fs::remove_file(&path).await.map_err(|error| {
            ApiError::internal(format!("failed to replace sync job journal: {error}"))
        })?;
    }
    fs::rename(&temp_path, &path).await.map_err(|error| {
        ApiError::internal(format!("failed to finalize sync job journal: {error}"))
    })?;
    Ok(())
}

pub(crate) async fn load_persisted_sync_jobs(state: &AppState) -> Result<(), ApiError> {
    let mut entries = fs::read_dir(state.storage_dir.as_ref())
        .await
        .map_err(|error| {
            ApiError::internal(format!("failed to scan sync job journals: {error}"))
        })?;
    let mut records = Vec::new();

    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        ApiError::internal(format!("failed to read sync job journal entry: {error}"))
    })? {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with("sync-job-") || !file_name.ends_with(".json") {
            continue;
        }
        let bytes = match fs::read(entry.path()).await {
            Ok(value) => value,
            Err(error) => {
                warn!(
                    "failed to read sync job journal {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        let mut record = match serde_json::from_slice::<LocalJobRecord>(&bytes) {
            Ok(value) if value.sync_managed => value,
            Ok(_) => continue,
            Err(error) => {
                warn!(
                    "failed to decode sync job journal {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };

        if record.job.status == JobStatus::Printing {
            record.job.status = JobStatus::Failed;
            record.job.detail =
                Some("打印服务在打印过程中重启，任务已停止以避免重复打印。".to_owned());
            record.updated_at = now_iso();
        }

        if record.job.status.is_terminal() {
            records.push(record);
        }
    }

    for record in records {
        state
            .jobs
            .write()
            .await
            .insert(record.job.id.clone(), record.clone());
        persist_job_record(state, &record).await;
    }
    Ok(())
}

pub(crate) fn sync_job_record_path(storage_dir: &Path, job_id: &str) -> PathBuf {
    storage_dir.join(format!("sync-job-{job_id}.json"))
}

pub(crate) async fn persist_active_queue(state: &AppState, queue: &[String]) {
    let legacy_queue = {
        let jobs = state.jobs.read().await;
        queue
            .iter()
            .filter(|job_id| jobs.get(*job_id).is_some_and(|record| !record.sync_managed))
            .cloned()
            .collect::<Vec<_>>()
    };

    if let Some(cloudflare) = &state.cloudflare {
        if let Err(error) = cloudflare.put_json("queue:active", &legacy_queue).await {
            warn!("failed to sync queue:active to Cloudflare KV: {error}");
        }
    }
}

pub(crate) async fn list_live_activities(state: &AppState) -> Vec<LiveActivityRecord> {
    let activities = state.activities.read().await;
    let mut items = activities.values().cloned().collect::<Vec<_>>();
    items.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    items
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_live_activity(
    state: &AppState,
    id: &str,
    kind: LiveActivityKind,
    stage: LiveActivityStage,
    file_name: String,
    user_name: Option<String>,
    printer: Option<PrinterMode>,
    received_bytes: u64,
    total_bytes: Option<u64>,
    summary: String,
    detail: Option<String>,
) -> LiveActivityRecord {
    let record = {
        let mut activities = state.activities.write().await;
        let existing = activities.get(id).cloned();
        let now = now_iso();
        let created_at = existing
            .as_ref()
            .map(|value| value.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let next = LiveActivityRecord {
            id: id.to_owned(),
            kind,
            stage,
            file_name: if file_name.trim().is_empty() {
                existing
                    .as_ref()
                    .map(|value| value.file_name.clone())
                    .unwrap_or_else(|| "document".to_owned())
            } else {
                file_name
            },
            user_name: user_name
                .or_else(|| existing.as_ref().and_then(|value| value.user_name.clone())),
            printer: printer.or_else(|| existing.as_ref().and_then(|value| value.printer)),
            received_bytes,
            total_bytes,
            percent: activity_percent(received_bytes, total_bytes, stage),
            summary,
            detail,
            created_at,
            updated_at: now,
        };
        activities.insert(id.to_owned(), next.clone());
        prune_live_activities(&mut activities);
        next
    };

    broadcast_live_activity(state, record.clone());
    record
}

pub(crate) fn activity_percent(
    received_bytes: u64,
    total_bytes: Option<u64>,
    stage: LiveActivityStage,
) -> Option<u8> {
    if matches!(stage, LiveActivityStage::Ready) {
        return Some(100);
    }

    let total = total_bytes?;
    if total == 0 {
        return Some(100);
    }

    let percent = ((received_bytes as f64 / total as f64) * 100.0).round() as u8;
    Some(percent.min(100))
}

pub(crate) fn prune_live_activities(activities: &mut HashMap<String, LiveActivityRecord>) {
    const MAX_ACTIVITY_COUNT: usize = 48;

    if activities.len() <= MAX_ACTIVITY_COUNT {
        return;
    }

    let mut terminal_records = activities
        .values()
        .filter(|value| value.stage.is_terminal())
        .cloned()
        .collect::<Vec<_>>();
    terminal_records.sort_by(|left, right| left.updated_at.cmp(&right.updated_at));

    while activities.len() > MAX_ACTIVITY_COUNT {
        let Some(oldest_terminal) = terminal_records.first() else {
            break;
        };
        let oldest_id = oldest_terminal.id.clone();
        terminal_records.remove(0);
        activities.remove(&oldest_id);
    }
}

pub(crate) fn broadcast_live_activity(state: &AppState, activity: LiveActivityRecord) {
    let _ = state.status_tx.send(StatusEvent {
        kind: StatusStreamKind::Activity,
        job_id: activity.id.clone(),
        status: activity_stage_status(activity.stage),
        detail: Some(activity.summary.clone()),
        pages_printed: None,
        total_pages: None,
        activity: Some(activity),
    });
}

pub(crate) fn activity_stage_status(stage: LiveActivityStage) -> JobStatus {
    match stage {
        LiveActivityStage::Receiving | LiveActivityStage::Received => JobStatus::Downloading,
        LiveActivityStage::Converting => JobStatus::Printing,
        LiveActivityStage::Ready => JobStatus::Done,
        LiveActivityStage::Failed => JobStatus::Failed,
    }
}

pub(crate) async fn list_available_printers() -> Result<Vec<String>, ApiError> {
    let output = Command::new("cmd")
        .args(["/C", "wmic printer get name"])
        .output()
        .await
        .map_err(|error| ApiError::internal(format!("failed to enumerate printers: {error}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ApiError::internal(format!(
            "printer enumeration failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .skip(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>())
}

pub(crate) async fn configured_printer_name(state: &AppState, mode: PrinterMode) -> String {
    let configured = {
        let printers = state.printers.read().await;
        match mode {
            PrinterMode::Bw => printers.bw.clone(),
            PrinterMode::Color => printers.color.clone(),
        }
    };

    match list_available_printers().await {
        Ok(available) => resolve_printer_alias(&configured, &available).unwrap_or(configured),
        Err(error) => {
            warn!("failed to resolve configured printer name: {error}");
            configured
        }
    }
}

pub(crate) fn resolve_printer_alias(configured: &str, available: &[String]) -> Option<String> {
    let configured = configured.trim();
    if configured.is_empty() {
        return None;
    }

    if let Some(exact) = available.iter().find(|name| name.trim() == configured) {
        return Some(exact.clone());
    }

    let normalized_configured = normalize_printer_alias(configured);
    if normalized_configured.is_empty() {
        return None;
    }

    available
        .iter()
        .find(|name| normalize_printer_alias(name) == normalized_configured)
        .cloned()
        .or_else(|| {
            available
                .iter()
                .filter_map(|name| {
                    let normalized_name = normalize_printer_alias(name);
                    let score = if normalized_name.ends_with(&normalized_configured) {
                        Some(3)
                    } else if normalized_name.contains(&normalized_configured) {
                        Some(2)
                    } else if normalized_configured.contains(&normalized_name) {
                        Some(1)
                    } else {
                        None
                    }?;
                    Some((score, name))
                })
                .max_by_key(|(score, name)| (*score, std::cmp::Reverse(name.len())))
                .map(|(_, name)| name.clone())
        })
}

pub(crate) fn normalize_printer_alias(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace("\\\\", "")
        .replace([' ', '\t', '\r', '\n'], "")
}

pub(crate) async fn sync_cached_config(state: &AppState) -> Result<(), ApiError> {
    let Some(cloudflare) = &state.cloudflare else {
        return Ok(());
    };

    let mut cached = state.cached_config.read().await.clone();

    if let Some(prices) = cloudflare.get_json::<PricesConfig>("config:prices").await? {
        cached.prices = prices;
    }
    if let Some(qrcodes) = cloudflare
        .get_json::<QRCodesConfig>("config:qrcodes")
        .await?
    {
        cached.qrcodes = qrcodes;
    }
    if let Some(printers) = cloudflare
        .get_json::<PrintersConfig>("config:printers")
        .await?
    {
        cached.printers = printers.clone();
        *state.printers.write().await = printers;
    }
    if let Some(notice_markdown) = cloudflare.get_text("config:notice_markdown").await? {
        cached.notice_markdown = notice_markdown;
    }

    *state.cached_config.write().await = cached;
    Ok(())
}

pub(crate) async fn job_was_cancelled(state: &AppState, job_id: &str) -> Result<bool, ApiError> {
    let jobs = state.jobs.read().await;
    let Some(record) = jobs.get(job_id) else {
        return Err(ApiError::not_found("job not found"));
    };

    Ok(record.job.status == JobStatus::Failed
        && record
            .job
            .detail
            .as_deref()
            .is_some_and(|detail| detail == admin_cancelled_text()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_text_clamps_pages() {
        let text = printing_progress_text("HP", 5, 3);
        assert!(text.contains("3/3"), "{text}");
        assert!(text.contains("HP"), "{text}");
        assert!(printing_progress_text("HP", 2, 0).contains("1/1"));
    }

    #[test]
    fn powershell_quote_escaping() {
        assert_eq!(escape_powershell_single_quote("a'b'c"), "a''b''c");
        assert_eq!(escape_powershell_single_quote("plain"), "plain");
    }

    #[test]
    fn classify_failure_by_keywords() {
        assert!(matches!(
            classify_print_failure("Out of paper", "", None),
            PrintFailure::OutOfPaper
        ));
        assert!(matches!(
            classify_print_failure("", "Printer offline", None),
            PrintFailure::PrinterOffline
        ));
        assert!(matches!(
            classify_print_failure("", "file is corrupt", None),
            PrintFailure::FileCorrupt
        ));
        assert!(matches!(
            classify_print_failure("", "weird", Some(1)),
            PrintFailure::Unknown(_)
        ));
    }

    #[test]
    fn activity_percent_rules() {
        assert_eq!(
            activity_percent(50, Some(100), LiveActivityStage::Receiving),
            Some(50)
        );
        assert_eq!(
            activity_percent(0, None, LiveActivityStage::Receiving),
            None
        );
        assert_eq!(
            activity_percent(0, Some(0), LiveActivityStage::Receiving),
            Some(100)
        );
        assert_eq!(
            activity_percent(1, Some(3), LiveActivityStage::Ready),
            Some(100)
        );
        assert_eq!(
            activity_percent(999, Some(100), LiveActivityStage::Converting),
            Some(100)
        );
    }

    #[test]
    fn stage_to_status_mapping() {
        assert_eq!(
            activity_stage_status(LiveActivityStage::Receiving),
            JobStatus::Downloading
        );
        assert_eq!(
            activity_stage_status(LiveActivityStage::Converting),
            JobStatus::Printing
        );
        assert_eq!(
            activity_stage_status(LiveActivityStage::Ready),
            JobStatus::Done
        );
        assert_eq!(
            activity_stage_status(LiveActivityStage::Failed),
            JobStatus::Failed
        );
    }

    #[test]
    fn printer_alias_resolution() {
        let available = vec!["HP LaserJet Pro".to_string(), "Canon MF".to_string()];
        assert_eq!(
            resolve_printer_alias("HP LaserJet Pro", &available).as_deref(),
            Some("HP LaserJet Pro")
        );
        assert_eq!(
            resolve_printer_alias("hp  laserjet  pro", &available).as_deref(),
            Some("HP LaserJet Pro")
        );
        assert_eq!(resolve_printer_alias("", &available), None);
        assert_eq!(resolve_printer_alias("Brother", &available), None);
    }

    #[test]
    fn alias_normalization() {
        assert_eq!(normalize_printer_alias("  HP  Laser\tJet "), "hplaserjet");
    }
}
