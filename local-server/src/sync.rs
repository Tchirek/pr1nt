//! Background print-sync client: pull loops, leases, and document preparation.
use crate::conversion::{build_converted_pdf_name, cleanup_conversion_dir};
use crate::error::*;
use crate::model::*;
use crate::prepare::*;
use crate::print_sync_client::PrintSyncClient;
use crate::printing::*;
use crate::text::{format_bytes, now_iso, queue_waiting_text};
use crate::util::*;
use axum::http::{header, StatusCode};
use futures::StreamExt;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::{
    env,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::{fs, io::AsyncWriteExt};
use tracing::{error, info, warn};

pub(crate) async fn run_print_sync(state: AppState) {
    let polling_state = state.clone();
    tokio::spawn(async move {
        loop {
            if let Err(error) = catch_up_print_sync(&polling_state).await {
                warn!("print sync catch-up failed: {error}");
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });

    let heartbeat_state = state.clone();
    tokio::spawn(async move {
        run_print_sync_heartbeat_loop(heartbeat_state).await;
    });

    run_print_sync_sse_loop(state).await;
}

pub(crate) async fn run_print_sync_sse_loop(state: AppState) {
    let Some(sync) = state.print_sync.clone() else {
        return;
    };
    let mut delay = Duration::from_secs(1);

    loop {
        let response = sync
            .get("/api/print-sync/stream")
            .header(header::ACCEPT, "text/event-stream")
            .send()
            .await;

        match response {
            Ok(response) if response.status().is_success() => {
                info!("print sync SSE connected");
                delay = Duration::from_secs(1);
                let mut stream = response.bytes_stream();
                let mut buffer = String::new();

                while let Some(next) = stream.next().await {
                    let chunk = match next {
                        Ok(value) => value,
                        Err(error) => {
                            warn!("print sync SSE read failed: {error}");
                            break;
                        }
                    };
                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    while let Some(line_end) = buffer.find('\n') {
                        let line = buffer[..line_end].trim_end_matches('\r').to_owned();
                        buffer.drain(..=line_end);
                        if line.starts_with("data:") {
                            let catchup_state = state.clone();
                            tokio::spawn(async move {
                                if let Err(error) = catch_up_print_sync(&catchup_state).await {
                                    warn!("print sync SSE catch-up failed: {error}");
                                }
                            });
                        }
                    }
                }
            }
            Ok(response) => {
                warn!("print sync SSE returned HTTP {}", response.status());
            }
            Err(error) => {
                warn!("print sync SSE connection failed: {error}");
            }
        }

        tokio::time::sleep(jitter_duration(delay)).await;
        delay = std::cmp::min(delay.saturating_mul(2), Duration::from_secs(32));
    }
}

pub(crate) async fn run_print_sync_heartbeat_loop(state: AppState) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let Some(sync) = state.print_sync.clone() else {
            return;
        };

        let heartbeat = PrintSyncHeartbeatRequest {
            kind: None,
            id: None,
            phase: None,
        };
        if let Err(error) = print_sync_post::<_, PrintSyncOkResponse>(
            &sync,
            "/api/print-sync/heartbeat",
            &heartbeat,
        )
        .await
        {
            warn!("print sync heartbeat failed: {error}");
        }

        let active_jobs = {
            let jobs = state.jobs.read().await;
            jobs.values()
                .filter(|record| record.sync_managed && !record.job.status.is_terminal())
                .map(|record| record.job.id.clone())
                .collect::<Vec<_>>()
        };

        for job_id in active_jobs {
            let request = PrintSyncHeartbeatRequest {
                kind: Some("job"),
                id: Some(&job_id),
                phase: None,
            };
            if let Err(error) = print_sync_post::<_, PrintSyncOkResponse>(
                &sync,
                "/api/print-sync/heartbeat",
                &request,
            )
            .await
            {
                warn!("failed to renew print sync job lease {job_id}: {error}");
            }
        }
    }
}

pub(crate) async fn catch_up_print_sync(state: &AppState) -> Result<(), ApiError> {
    let _guard = state.sync_catchup_lock.lock().await;
    let Some(sync) = state.print_sync.clone() else {
        return Ok(());
    };

    let response = sync
        .get("/api/print-sync/pending")
        .send()
        .await
        .map_err(|error| {
            ApiError::upstream(format!("print sync pending request failed: {error}"))
        })?;
    let pending = parse_print_sync_response::<PendingPrintSyncResponse>(response).await?;

    for document in pending.documents {
        if let Err(error) = process_pending_document(state.clone(), document.clone()).await {
            warn!(
                "failed to prepare synced document {} ({}; {}; {} bytes; status={}): {error}",
                document.id,
                document.source_name,
                document.mime_type,
                document.declared_size,
                document.status
            );
        }
    }

    for job in pending.jobs {
        if let Err(error) = accept_pending_print_job(state.clone(), job.clone()).await {
            warn!(
                "failed to accept synced print job {} (document {}; status={:?}): {error}",
                job.id, job.document_id, job.status
            );
        }
    }

    Ok(())
}

pub(crate) async fn process_pending_document(
    state: AppState,
    pending: PendingDocumentWork,
) -> Result<(), ApiError> {
    let sync = state
        .print_sync
        .clone()
        .ok_or_else(|| ApiError::internal("print sync is not configured"))?;
    let claim = PrintSyncClaimRequest {
        kind: "document",
        id: &pending.id,
        recover_ready: false,
    };
    let claimed =
        print_sync_post::<_, ClaimedDocumentWork>(&sync, "/api/print-sync/claim", &claim).await?;

    let heartbeat_state = state.clone();
    let heartbeat_document_id = claimed.id.clone();
    let heartbeat_task = tokio::spawn(async move {
        run_document_lease_heartbeat(heartbeat_state, heartbeat_document_id).await;
    });

    let result = prepare_claimed_document(&state, &sync, &claimed).await;
    heartbeat_task.abort();

    if let Err(error) = &result {
        cleanup_failed_synced_document(state.storage_dir.as_ref(), &claimed.id).await;
        let detail = error.to_string();
        let request = PrintSyncFailRequest {
            document_id: &claimed.id,
            error: &detail,
        };
        if let Err(report_error) =
            print_sync_post::<_, PrintSyncOkResponse>(&sync, "/api/print-sync/fail", &request).await
        {
            warn!(
                "failed to report preparation failure for {}: {report_error}",
                claimed.id
            );
        }
    }

    result
}

pub(crate) async fn run_document_lease_heartbeat(state: AppState, document_id: String) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let Some(sync) = state.print_sync.clone() else {
            return;
        };
        let request = PrintSyncHeartbeatRequest {
            kind: Some("document"),
            id: Some(&document_id),
            phase: None,
        };
        if let Err(error) =
            print_sync_post::<_, PrintSyncOkResponse>(&sync, "/api/print-sync/heartbeat", &request)
                .await
        {
            warn!("failed to renew document lease {document_id}: {error}");
            return;
        }
    }
}

pub(crate) async fn prepare_claimed_document(
    state: &AppState,
    sync: &PrintSyncClient,
    claimed: &ClaimedDocumentWork,
) -> Result<(), ApiError> {
    let source_name = sanitize_source_filename(&claimed.source_name);
    let source_dir = state
        .storage_dir
        .join(format!("sync-source-{}", claimed.id));
    let conversion_dir = state
        .storage_dir
        .join(format!("sync-conversion-{}", claimed.id));
    let source_path = source_dir.join(&source_name);
    let part_path = source_dir.join(format!("{source_name}.part"));
    let prepared_path = prepared_document_path(state.storage_dir.as_ref(), &claimed.id);
    let activity_id = format!("sync:{}", claimed.id);

    fs::create_dir_all(&source_dir).await.map_err(|error| {
        ApiError::internal(format!("failed to create sync source directory: {error}"))
    })?;
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to create sync conversion directory: {error}"
        ))
    })?;

    upsert_live_activity(
        state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Receiving,
        source_name.clone(),
        None,
        None,
        0,
        Some(claimed.declared_size),
        "正在从临时存储下载文件。".to_owned(),
        Some(format!(
            "{} | {}",
            claimed.mime_type,
            format_bytes(claimed.declared_size)
        )),
    )
    .await;

    let response = sync
        .get(&format!("/api/print-sync/download/{}", claimed.id))
        .send()
        .await
        .map_err(|error| {
            ApiError::upstream(format!(
                "document download request failed: {}",
                format_reqwest_error(&error)
            ))
        })?;
    if !response.status().is_success() {
        return Err(print_sync_http_error(response).await);
    }

    let mut file = fs::File::create(&part_path).await.map_err(|error| {
        ApiError::internal(format!("failed to create download part file: {error}"))
    })?;
    let mut hash = Sha256::new();
    let mut size_bytes = 0u64;
    let mut stream = response.bytes_stream();
    let mut last_reported = 0u64;

    while let Some(next) = stream.next().await {
        let chunk =
            next.map_err(|error| ApiError::upstream(format!("document download failed: {error}")))?;
        size_bytes = size_bytes.saturating_add(chunk.len() as u64);
        if size_bytes > claimed.declared_size || size_bytes > MAX_UPLOAD_SIZE_BYTES as u64 {
            return Err(ApiError::bad_request(
                "downloaded document exceeds declared size",
            ));
        }
        hash.update(&chunk);
        file.write_all(&chunk).await.map_err(|error| {
            ApiError::internal(format!("failed to write downloaded document: {error}"))
        })?;

        if size_bytes.saturating_sub(last_reported) >= 1024 * 1024
            || size_bytes == claimed.declared_size
        {
            upsert_live_activity(
                state,
                &activity_id,
                LiveActivityKind::PrintUpload,
                LiveActivityStage::Receiving,
                source_name.clone(),
                None,
                None,
                size_bytes,
                Some(claimed.declared_size),
                format!(
                    "正在下载文件：{} / {}",
                    format_bytes(size_bytes),
                    format_bytes(claimed.declared_size)
                ),
                None,
            )
            .await;
            last_reported = size_bytes;
        }
    }

    if size_bytes != claimed.declared_size {
        return Err(ApiError::bad_request(format!(
            "downloaded document size mismatch: {size_bytes} / {}",
            claimed.declared_size
        )));
    }
    file.flush().await.map_err(|error| {
        ApiError::internal(format!("failed to flush downloaded document: {error}"))
    })?;
    file.sync_data().await.map_err(|error| {
        ApiError::internal(format!("failed to sync downloaded document: {error}"))
    })?;
    drop(file);
    if fs::metadata(&source_path).await.is_ok() {
        fs::remove_file(&source_path).await.map_err(|error| {
            ApiError::internal(format!("failed to replace downloaded document: {error}"))
        })?;
    }
    fs::rename(&part_path, &source_path)
        .await
        .map_err(|error| {
            ApiError::internal(format!("failed to finalize downloaded document: {error}"))
        })?;

    let heartbeat = PrintSyncHeartbeatRequest {
        kind: Some("document"),
        id: Some(&claimed.id),
        phase: Some("converting"),
    };
    print_sync_post::<_, PrintSyncOkResponse>(sync, "/api/print-sync/heartbeat", &heartbeat)
        .await?;

    let pdf_size = prepare_print_pdf_to_path(
        state,
        &conversion_dir,
        &source_path,
        &source_name,
        &prepared_path,
        &activity_id,
        None,
        None,
        size_bytes,
        Some(size_bytes),
    )
    .await?;
    let page_count = count_pdf_pages(&prepared_path).await?;
    let sha256 = format!("{:x}", hash.finalize());
    let display_name = build_converted_pdf_name(&source_name);
    let confirm = PrintSyncConfirmRequest {
        document_id: &claimed.id,
        sha256: &sha256,
        size_bytes,
        page_count,
        file_name: &display_name,
    };
    let _: serde_json::Value = print_sync_post(sync, "/api/print-sync/confirm", &confirm).await?;

    upsert_live_activity(
        state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Ready,
        display_name,
        None,
        None,
        pdf_size,
        Some(pdf_size),
        format!("文件已准备完成，共 {page_count} 页。"),
        Some(format!("SHA-256 {sha256}")),
    )
    .await;

    cleanup_conversion_dir(&conversion_dir).await;
    if let Err(error) = fs::remove_dir_all(&source_dir).await {
        warn!(
            "failed to clean sync source directory {}: {error}",
            source_dir.display()
        );
    }
    Ok(())
}

pub(crate) async fn accept_pending_print_job(
    state: AppState,
    pending: PendingPrintJob,
) -> Result<(), ApiError> {
    let existing = {
        let jobs = state.jobs.read().await;
        jobs.get(&pending.id).cloned()
    };
    if let Some(record) = existing {
        if record.sync_managed {
            persist_job_record(&state, &record).await;
            return Ok(());
        }
    }

    let sync = state
        .print_sync
        .clone()
        .ok_or_else(|| ApiError::internal("print sync is not configured"))?;
    let claim = PrintSyncClaimRequest {
        kind: "job",
        id: &pending.id,
        recover_ready: false,
    };
    let claimed =
        print_sync_post::<_, ClaimedPrintJob>(&sync, "/api/print-sync/claim", &claim).await?;
    let printer = PrinterMode::parse(&claimed.color_mode)?;
    let total_pages = checked_total_print_pages(claimed.page_count, claimed.copy_count)?;
    if total_pages != claimed.total_pages {
        return Err(ApiError::bad_request(
            "remote print job total page count mismatch",
        ));
    }

    let pdf_path = prepared_document_path(state.storage_dir.as_ref(), &claimed.document_id);
    if fs::metadata(&pdf_path).await.is_err() {
        let recovery_claim = PrintSyncClaimRequest {
            kind: "document",
            id: &claimed.document_id,
            recover_ready: true,
        };
        let recovered_document = print_sync_post::<_, ClaimedDocumentWork>(
            &sync,
            "/api/print-sync/claim",
            &recovery_claim,
        )
        .await?;
        let heartbeat_state = state.clone();
        let heartbeat_document_id = recovered_document.id.clone();
        let heartbeat_task = tokio::spawn(async move {
            run_document_lease_heartbeat(heartbeat_state, heartbeat_document_id).await;
        });
        let recovery_result = prepare_claimed_document(&state, &sync, &recovered_document).await;
        heartbeat_task.abort();
        if let Err(error) = recovery_result {
            let detail = format!("Prepared PDF recovery failed: {error}");
            let request = PrintSyncJobStatusRequest {
                job_id: &claimed.id,
                status: "failed",
                detail: Some(&detail),
                pages_printed: Some(0),
                total_pages: Some(total_pages),
            };
            let _: serde_json::Value =
                print_sync_post(&sync, "/api/print-sync/job-status", &request).await?;
            return Err(ApiError::not_found(detail));
        }
    }

    let printer_name = configured_printer_name(&state, printer).await;
    let record = LocalJobRecord {
        job: QueueJobRecord {
            id: claimed.id.clone(),
            user_name: claimed.user_name,
            file_name: claimed.file_name,
            page_count: claimed.page_count,
            copy_count: claimed.copy_count,
            color_mode: printer.as_str().to_owned(),
            status: JobStatus::Queued,
            submitted_at: now_iso(),
            detail: Some(queue_waiting_text()),
            pages_printed: Some(0),
            total_pages: Some(total_pages),
        },
        printer,
        printer_name,
        pdf_path: pdf_path.to_string_lossy().to_string(),
        attempts: 1,
        updated_at: now_iso(),
        document_id: Some(claimed.document_id),
        sync_managed: true,
    };

    {
        let mut jobs = state.jobs.write().await;
        if jobs.contains_key(&record.job.id) {
            return Ok(());
        }
        jobs.insert(record.job.id.clone(), record.clone());
    }

    set_job_status(
        &state,
        &record.job.id,
        JobStatus::Queued,
        Some(queue_waiting_text()),
    )
    .await?;
    let task_state = state.clone();
    let task_job_id = record.job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = process_job(task_state, task_job_id).await {
            error!("synced print task failed: {error}");
        }
    });

    Ok(())
}

pub(crate) async fn print_sync_post<B: Serialize + ?Sized, T: DeserializeOwned>(
    sync: &PrintSyncClient,
    path: &str,
    body: &B,
) -> Result<T, ApiError> {
    let response = sync
        .post(path)
        .json(body)
        .send()
        .await
        .map_err(|error| ApiError::upstream(format!("print sync request failed: {error}")))?;
    parse_print_sync_response(response).await
}

pub(crate) async fn parse_print_sync_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ApiError> {
    if !response.status().is_success() {
        return Err(print_sync_http_error(response).await);
    }
    response
        .json::<T>()
        .await
        .map_err(|error| ApiError::upstream(format!("invalid print sync response: {error}")))
}

pub(crate) async fn print_sync_http_error(response: reqwest::Response) -> ApiError {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "unknown print sync error".to_owned());
    if status == StatusCode::CONFLICT {
        return ApiError::conflict(format!("print sync conflict: {body}"));
    }
    if status == StatusCode::NOT_FOUND {
        return ApiError::not_found(format!("print sync item not found: {body}"));
    }
    if status == StatusCode::BAD_REQUEST {
        return ApiError::bad_request(format!("print sync request rejected: {body}"));
    }
    ApiError::upstream(format!("print sync HTTP {status}: {body}"))
}

pub(crate) fn prepared_document_path(storage_dir: &Path, document_id: &str) -> PathBuf {
    storage_dir.join(format!("prepared-document-{document_id}.pdf"))
}

pub(crate) async fn cleanup_failed_synced_document(storage_dir: &Path, document_id: &str) {
    let source_dir = storage_dir.join(format!("sync-source-{document_id}"));
    let conversion_dir = storage_dir.join(format!("sync-conversion-{document_id}"));
    for path in [&source_dir, &conversion_dir] {
        if let Err(error) = fs::remove_dir_all(path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "failed to clean failed sync directory {}: {error}",
                    path.display()
                );
            }
        }
    }

    let prepared_path = prepared_document_path(storage_dir, document_id);
    if let Err(error) = fs::remove_file(&prepared_path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "failed to remove failed prepared PDF {}: {error}",
                prepared_path.display()
            );
        }
    }
}

pub(crate) fn sanitize_sync_device_id(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':')
        })
        .take(120)
        .collect()
}

pub(crate) fn env_flag_enabled(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn format_reqwest_error(error: &reqwest::Error) -> String {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(next) = source {
        let next_detail = next.to_string();
        if !next_detail.is_empty() && !detail.contains(&next_detail) {
            detail.push_str(": ");
            detail.push_str(&next_detail);
        }
        source = next.source();
    }
    detail
}

pub(crate) fn jitter_duration(base: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|value| value.subsec_nanos())
        .unwrap_or(0);
    let percent = 80 + (nanos % 41) as u64;
    Duration::from_millis(base.as_millis() as u64 * percent / 100)
}
