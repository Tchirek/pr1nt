//! Axum HTTP and WebSocket handlers for the public and admin servers.
use crate::auth::{
    extract_bearer, verify_admin, verify_direct_upload_token, verify_upload_auth, DirectUploadKind,
};
use crate::config::{
    normalize_document_converter_config, persist_runtime_config, AdminConfigResponse,
    AdminConfigUpdate,
};
use crate::conversion::{
    build_converted_pdf_name, cleanup_conversion_dir, convert_document_to_pdf,
    is_supported_source_extension, source_extension,
};
use crate::diagnostics::*;
use crate::error::*;
use crate::model::*;
use crate::prepare::*;
use crate::printing::*;
use crate::text::{admin_retry_text, format_bytes, now_iso, queue_waiting_text};
use crate::util::*;
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Multipart, Path as AxumPath, State,
    },
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use futures::StreamExt;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{fs, io::AsyncWriteExt, sync::broadcast};
use tracing::{error, warn};
use uuid::Uuid;

pub(crate) async fn post_print(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<AcceptedPrintResponse>, ApiError> {
    let hinted_job_id = parse_optional_header(&headers, "x-upload-job-id")
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned());
    let hinted_preview_cache_id = parse_optional_header(&headers, "x-preview-cache-id")
        .filter(|value| is_valid_preview_cache_id(value));
    let auth_payload = verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Print,
        hinted_job_id.as_deref(),
        hinted_preview_cache_id.as_deref(),
    )?;

    let hinted_total_bytes = parse_optional_u64_header(&headers, "x-upload-file-size");
    let hinted_file_name = parse_optional_header(&headers, "x-upload-file-name")
        .map(|value| sanitize_filename(&value))
        .filter(|value| !value.trim().is_empty());
    let hinted_user_name = parse_optional_header(&headers, "x-upload-user-name")
        .filter(|value| !value.trim().is_empty());
    let hinted_printer_mode = parse_optional_header(&headers, "x-upload-printer-mode")
        .and_then(|value| PrinterMode::parse(&value).ok());
    let activity_id = hinted_job_id
        .clone()
        .map(|value| format!("upload:{value}"))
        .unwrap_or_else(|| format!("print-upload-{}", Uuid::new_v4()));

    let mut pdf_bytes: Option<Vec<u8>> = None;
    let mut file_name: Option<String> = hinted_file_name.clone();
    let mut job_id: Option<String> = hinted_job_id.clone();
    let mut user_name: Option<String> = hinted_user_name.clone();
    let mut printer_mode: Option<PrinterMode> = hinted_printer_mode;
    let mut preview_cache_id: Option<String> = hinted_preview_cache_id.clone();
    let mut page_count: Option<u32> = parse_optional_header(&headers, "x-upload-page-count")
        .and_then(|value| value.parse::<u32>().ok());
    let mut copy_count: Option<u32> = parse_optional_header(&headers, "x-upload-copy-count")
        .and_then(|value| parse_copy_count(&value).ok());

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("invalid multipart payload: {error}")))?
    {
        let field_name = field.name().unwrap_or_default().to_owned();
        match field_name.as_str() {
            "pdf" => {
                let incoming_name = field.file_name().unwrap_or("document.pdf");
                let sanitized_name = sanitize_filename(incoming_name);
                file_name = Some(sanitized_name.clone());
                let bytes = read_field_bytes_with_progress(
                    &state,
                    field,
                    &activity_id,
                    LiveActivityKind::PrintUpload,
                    &sanitized_name,
                    hinted_user_name.clone(),
                    hinted_printer_mode,
                    hinted_total_bytes,
                    MAX_UPLOAD_SIZE_BYTES,
                    "PDF file exceeds the 256 MB limit",
                    "failed to read PDF bytes",
                )
                .await?;
                pdf_bytes = Some(bytes);
            }
            "job_id" => {
                job_id = Some(
                    field
                        .text()
                        .await
                        .map_err(|error| {
                            ApiError::bad_request(format!("invalid job_id field: {error}"))
                        })?
                        .trim()
                        .to_owned(),
                );
            }
            "printer" | "color_mode" => {
                let raw = field.text().await.map_err(|error| {
                    ApiError::bad_request(format!("invalid printer field: {error}"))
                })?;
                printer_mode = Some(PrinterMode::parse(&raw)?);
            }
            "user_name" => {
                user_name = Some(
                    field
                        .text()
                        .await
                        .map_err(|error| {
                            ApiError::bad_request(format!("invalid user_name field: {error}"))
                        })?
                        .trim()
                        .to_owned(),
                );
            }
            "file_name" => {
                file_name = Some(sanitize_filename(
                    field
                        .text()
                        .await
                        .map_err(|error| {
                            ApiError::bad_request(format!("invalid file_name field: {error}"))
                        })?
                        .trim(),
                ));
            }
            "preview_cache_id" => {
                let raw = field.text().await.map_err(|error| {
                    ApiError::bad_request(format!("invalid preview_cache_id field: {error}"))
                })?;
                let value = raw.trim();
                if !is_valid_preview_cache_id(value) {
                    return Err(ApiError::bad_request("invalid preview_cache_id"));
                }
                preview_cache_id = Some(value.to_owned());
            }
            "page_count" => {
                let raw = field.text().await.map_err(|error| {
                    ApiError::bad_request(format!("invalid page_count field: {error}"))
                })?;
                page_count = Some(parse_page_count(&raw)?);
            }
            "copy_count" => {
                let raw = field.text().await.map_err(|error| {
                    ApiError::bad_request(format!("invalid copy_count field: {error}"))
                })?;
                copy_count = Some(parse_copy_count(&raw)?);
            }
            _ => {}
        }
    }

    let job_id = job_id
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let user_name = user_name
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing user_name"))?;
    let printer_mode = printer_mode.ok_or_else(|| ApiError::bad_request("missing printer mode"))?;
    let page_count = page_count.ok_or_else(|| ApiError::bad_request("missing page_count"))?;
    let copy_count = copy_count.unwrap_or(1);
    if auth_payload
        .page_count
        .is_some_and(|expected| expected != page_count)
    {
        return Err(ApiError::unauthorized(
            "direct upload token does not match page count",
        ));
    }
    if auth_payload
        .copy_count
        .is_some_and(|expected| expected != copy_count)
    {
        return Err(ApiError::unauthorized(
            "direct upload token does not match copy count",
        ));
    }
    let total_pages = checked_total_print_pages(page_count, copy_count)?;
    let file_name = file_name.unwrap_or_else(|| "document.pdf".to_owned());
    let stored_file_name = format!("{}-{}", job_id, sanitize_filename(&file_name));
    let pdf_path = state.storage_dir.join(stored_file_name);
    let (pdf_size, activity_summary, activity_detail) = if let Some(pdf_bytes) = pdf_bytes {
        fs::write(&pdf_path, &pdf_bytes)
            .await
            .map_err(|error| ApiError::internal(format!("failed to store PDF locally: {error}")))?;
        (
            pdf_bytes.len() as u64,
            "打印文件已接收完成，正在写入本地暂存。".to_owned(),
            format!(
                "{} | {} 页 × {} 份 | {}",
                user_name,
                page_count,
                copy_count,
                printer_mode.as_str()
            ),
        )
    } else if let Some(cache_id) = preview_cache_id.as_deref() {
        if hinted_preview_cache_id.as_deref() != Some(cache_id)
            && extract_bearer(&headers).as_deref() != Some(state.shared_secret.as_ref().as_str())
        {
            return Err(ApiError::unauthorized(
                "preview cache id must be signed in the direct upload token",
            ));
        }

        let cached_path = preview_cache_path(state.storage_dir.as_ref(), cache_id);
        let metadata = fs::metadata(&cached_path)
            .await
            .map_err(|_| ApiError::not_found("preview PDF cache not found or expired"))?;
        fs::copy(&cached_path, &pdf_path).await.map_err(|error| {
            ApiError::internal(format!("failed to copy preview PDF cache: {error}"))
        })?;
        if let Err(error) = fs::remove_file(&cached_path).await {
            warn!(
                "failed to remove consumed preview cache {}: {error}",
                cached_path.display()
            );
        }
        (
            metadata.len(),
            "已复用本地预览 PDF，无需重新上传。".to_owned(),
            format!(
                "缓存 {} | {} | {} 页 × {} 份",
                cache_id, user_name, page_count, copy_count
            ),
        )
    } else {
        return Err(ApiError::bad_request(
            "missing PDF file or preview cache id",
        ));
    };

    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Received,
        file_name.clone(),
        Some(user_name.clone()),
        Some(printer_mode),
        pdf_size,
        hinted_total_bytes.or(Some(pdf_size)),
        activity_summary,
        Some(activity_detail),
    )
    .await;

    let printer_name = configured_printer_name(&state, printer_mode).await;

    let record = LocalJobRecord {
        job: QueueJobRecord {
            id: job_id.clone(),
            user_name,
            file_name: file_name.clone(),
            page_count,
            copy_count,
            color_mode: printer_mode.as_str().to_owned(),
            status: JobStatus::Queued,
            submitted_at: now_iso(),
            detail: Some(queue_waiting_text()),
            pages_printed: Some(0),
            total_pages: Some(total_pages),
        },
        printer: printer_mode,
        printer_name: printer_name.clone(),
        pdf_path: pdf_path.to_string_lossy().to_string(),
        attempts: 1,
        updated_at: now_iso(),
        document_id: None,
        sync_managed: false,
    };

    {
        let mut jobs = state.jobs.write().await;
        jobs.insert(job_id.clone(), record.clone());
    }

    set_job_status(&state, &job_id, JobStatus::Queued, None).await?;
    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Ready,
        file_name.clone(),
        Some(record.job.user_name.clone()),
        Some(printer_mode),
        pdf_size,
        hinted_total_bytes.or(Some(pdf_size)),
        "打印文件已入队，可继续观察右侧任务状态。".to_owned(),
        Some(format!("任务 {} 已进入队列。", job_id)),
    )
    .await;

    let task_state = state.clone();
    let task_job_id = job_id.clone();
    tokio::spawn(async move {
        if let Err(error) = process_job(task_state, task_job_id).await {
            error!("print task failed: {error}");
        }
    });

    Ok(Json(AcceptedPrintResponse {
        accepted: true,
        job_id,
    }))
}

pub(crate) async fn post_prepare_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<PreparedPrintResponse>, ApiError> {
    verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Prepare,
        None,
        None,
    )?;

    let prepared_id = Uuid::new_v4().to_string();
    let activity_id = format!("prepare:{prepared_id}");
    let hinted_total_bytes = parse_optional_u64_header(&headers, "x-upload-file-size");
    let source_name = parse_optional_decoded_header(&headers, "x-upload-file-name")
        .map(|value| sanitize_source_filename(&value))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "document".to_owned());

    let extension = source_extension(&source_name);
    if extension.is_empty() || !is_supported_source_extension(&extension) {
        return Err(ApiError::bad_request("unsupported source file type"));
    }

    let conversion_dir = state
        .storage_dir
        .join(format!("prepare-source-{prepared_id}"));
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to create prepare conversion directory: {error}"
        ))
    })?;
    let source_path = conversion_dir.join(&source_name);
    let source_size = match write_raw_print_source_with_progress(
        &state,
        body,
        &source_path,
        &activity_id,
        &source_name,
        None,
        None,
        hinted_total_bytes,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    let prepared_path = preview_cache_path(state.storage_dir.as_ref(), &prepared_id);
    let _pdf_size = match prepare_print_pdf_to_path(
        &state,
        &conversion_dir,
        &source_path,
        &source_name,
        &prepared_path,
        &activity_id,
        None,
        None,
        source_size,
        hinted_total_bytes,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    let page_count = match count_pdf_pages(&prepared_path).await {
        Ok(value) => value.max(1),
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            if let Err(remove_error) = fs::remove_file(&prepared_path).await {
                warn!(
                    "failed to remove prepared PDF {} after page count error: {remove_error}",
                    prepared_path.display()
                );
            }
            return Err(error);
        }
    };

    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Ready,
        source_name.clone(),
        None,
        None,
        source_size,
        hinted_total_bytes.or(Some(source_size)),
        format!("已计页：{page_count} 页。"),
        Some(prepared_id.clone()),
    )
    .await;
    cleanup_conversion_dir(&conversion_dir).await;

    Ok(Json(PreparedPrintResponse {
        prepared_id,
        page_count,
        file_name: build_converted_pdf_name(&source_name),
        source_name,
    }))
}

pub(crate) async fn post_prepare_chunk(
    State(state): State<AppState>,
    AxumPath(upload_id): AxumPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<PrepareChunkResponse>, ApiError> {
    verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Prepare,
        None,
        None,
    )?;
    validate_prepare_upload_id(&upload_id)?;

    let (total_bytes, source_name) = parse_prepare_source_headers(&headers)?;
    let offset = parse_optional_u64_header(&headers, "x-upload-offset")
        .ok_or_else(|| ApiError::bad_request("missing upload offset"))?;
    let activity_id = format!("prepare:{upload_id}");
    let part_path = prepare_upload_part_path(state.storage_dir.as_ref(), &upload_id);
    let current_size = match fs::metadata(&part_path).await {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(ApiError::internal(format!(
                "failed to inspect partial upload: {error}"
            )))
        }
    };

    if current_size > total_bytes {
        fail_raw_upload_file(
            &state,
            &part_path,
            &activity_id,
            &source_name,
            None,
            None,
            current_size,
            Some(total_bytes),
            "partial upload is larger than expected",
        )
        .await;
        return Err(ApiError::bad_request(
            "partial upload is larger than expected",
        ));
    }

    if offset < current_size {
        return Ok(Json(prepare_chunk_response(
            &upload_id,
            current_size,
            total_bytes,
        )));
    }

    if offset > current_size {
        return Err(ApiError::conflict(format!(
            "upload offset mismatch; server has {current_size} bytes"
        )));
    }

    let mut stream = body.into_data_stream();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part_path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to open partial upload: {error}")))?;
    let mut received_bytes = current_size;

    while let Some(next_chunk) = tokio::time::timeout(Duration::from_secs(20), stream.next())
        .await
        .map_err(|_| ApiError::bad_request("upload chunk stalled"))?
    {
        let chunk = next_chunk.map_err(|error| {
            ApiError::bad_request(format!("failed to receive upload chunk: {error}"))
        })?;
        let next_size = received_bytes.saturating_add(chunk.len() as u64);
        if next_size > total_bytes || next_size as usize > MAX_UPLOAD_SIZE_BYTES {
            fail_raw_upload_file(
                &state,
                &part_path,
                &activity_id,
                &source_name,
                None,
                None,
                next_size,
                Some(total_bytes),
                "source file exceeds the declared upload size",
            )
            .await;
            return Err(ApiError::bad_request(
                "source file exceeds the declared upload size",
            ));
        }

        file.write_all(&chunk).await.map_err(|error| {
            ApiError::internal(format!("failed to write upload chunk: {error}"))
        })?;
        received_bytes = next_size;
    }

    file.flush()
        .await
        .map_err(|error| ApiError::internal(format!("failed to flush upload chunk: {error}")))?;

    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Receiving,
        source_name,
        None,
        None,
        received_bytes,
        Some(total_bytes),
        receiving_activity_summary(
            LiveActivityKind::PrintUpload,
            received_bytes,
            Some(total_bytes),
        ),
        Some(format!(
            "Confirmed by local server: {} / {}",
            format_bytes(received_bytes),
            format_bytes(total_bytes)
        )),
    )
    .await;

    Ok(Json(prepare_chunk_response(
        &upload_id,
        received_bytes,
        total_bytes,
    )))
}

pub(crate) async fn post_prepare_complete(
    State(state): State<AppState>,
    AxumPath(upload_id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Json<PreparedPrintResponse>, ApiError> {
    verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Prepare,
        None,
        None,
    )?;
    validate_prepare_upload_id(&upload_id)?;

    let (total_bytes, source_name) = parse_prepare_source_headers(&headers)?;
    let activity_id = format!("prepare:{upload_id}");
    let prepared_path = preview_cache_path(state.storage_dir.as_ref(), &upload_id);

    if fs::metadata(&prepared_path).await.is_ok() {
        let page_count = count_pdf_pages(&prepared_path).await?.max(1);
        return Ok(Json(PreparedPrintResponse {
            prepared_id: upload_id,
            page_count,
            file_name: build_converted_pdf_name(&source_name),
            source_name,
        }));
    }

    let part_path = prepare_upload_part_path(state.storage_dir.as_ref(), &upload_id);
    let received_bytes = fs::metadata(&part_path)
        .await
        .map_err(|error| ApiError::conflict(format!("partial upload is not ready: {error}")))?
        .len();

    if received_bytes != total_bytes {
        upsert_live_activity(
            &state,
            &activity_id,
            LiveActivityKind::PrintUpload,
            LiveActivityStage::Receiving,
            source_name.clone(),
            None,
            None,
            received_bytes,
            Some(total_bytes),
            receiving_activity_summary(
                LiveActivityKind::PrintUpload,
                received_bytes,
                Some(total_bytes),
            ),
            Some("Upload is not complete yet.".to_owned()),
        )
        .await;
        return Err(ApiError::conflict(format!(
            "upload is incomplete: {} / {}",
            format_bytes(received_bytes),
            format_bytes(total_bytes)
        )));
    }

    let conversion_dir = state
        .storage_dir
        .join(format!("prepare-source-{upload_id}"));
    cleanup_conversion_dir(&conversion_dir).await;
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to create prepare conversion directory: {error}"
        ))
    })?;
    let source_path = conversion_dir.join(&source_name);
    fs::rename(&part_path, &source_path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to stage prepared source: {error}")))?;

    let _pdf_size = match prepare_print_pdf_to_path(
        &state,
        &conversion_dir,
        &source_path,
        &source_name,
        &prepared_path,
        &activity_id,
        None,
        None,
        received_bytes,
        Some(total_bytes),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    let page_count = match count_pdf_pages(&prepared_path).await {
        Ok(value) => value.max(1),
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            if let Err(remove_error) = fs::remove_file(&prepared_path).await {
                warn!(
                    "failed to remove prepared PDF {} after page count error: {remove_error}",
                    prepared_path.display()
                );
            }
            return Err(error);
        }
    };

    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Ready,
        source_name.clone(),
        None,
        None,
        received_bytes,
        Some(total_bytes),
        format!("Prepared {page_count} printable pages."),
        Some(upload_id.clone()),
    )
    .await;
    cleanup_conversion_dir(&conversion_dir).await;

    Ok(Json(PreparedPrintResponse {
        prepared_id: upload_id,
        page_count,
        file_name: build_converted_pdf_name(&source_name),
        source_name,
    }))
}

pub(crate) async fn post_print_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<AcceptedPrintResponse>, ApiError> {
    let hinted_job_id = parse_optional_header(&headers, "x-upload-job-id")
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned());
    let auth_payload = verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Print,
        hinted_job_id.as_deref(),
        None,
    )?;

    let job_id = hinted_job_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    if state.jobs.read().await.contains_key(&job_id) {
        return Ok(Json(AcceptedPrintResponse {
            accepted: true,
            job_id,
        }));
    }

    let hinted_total_bytes = parse_optional_u64_header(&headers, "x-upload-file-size");
    let file_name = parse_optional_decoded_header(&headers, "x-upload-file-name")
        .map(|value| sanitize_source_filename(&value))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "document".to_owned());
    let user_name = parse_optional_decoded_header(&headers, "x-upload-user-name")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("missing user_name"))?;
    let printer_mode = parse_optional_header(&headers, "x-upload-printer-mode")
        .and_then(|value| PrinterMode::parse(&value).ok())
        .ok_or_else(|| ApiError::bad_request("missing printer mode"))?;
    let copy_count = parse_optional_header(&headers, "x-upload-copy-count")
        .and_then(|value| parse_copy_count(&value).ok())
        .unwrap_or(1);

    if auth_payload
        .copy_count
        .is_some_and(|expected| expected != copy_count)
    {
        return Err(ApiError::unauthorized(
            "direct upload token does not match copy count",
        ));
    }

    let extension = source_extension(&file_name);
    if extension.is_empty() || !is_supported_source_extension(&extension) {
        return Err(ApiError::bad_request("unsupported source file type"));
    }

    let activity_id = format!("upload:{job_id}");
    let conversion_dir = state.storage_dir.join(format!("print-source-{job_id}"));
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to create print conversion directory: {error}"
        ))
    })?;
    let source_path = conversion_dir.join(&file_name);
    let source_size = match write_raw_print_source_with_progress(
        &state,
        body,
        &source_path,
        &activity_id,
        &file_name,
        Some(user_name.clone()),
        Some(printer_mode),
        hinted_total_bytes,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    let (pdf_path, pdf_size) = match prepare_print_pdf(
        &state,
        &conversion_dir,
        &source_path,
        &file_name,
        &job_id,
        &activity_id,
        Some(user_name.clone()),
        Some(printer_mode),
        source_size,
        hinted_total_bytes,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    let page_count = match count_pdf_pages(&pdf_path).await {
        Ok(value) => value.max(1),
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };
    if auth_payload
        .page_count
        .is_some_and(|expected| expected != page_count)
    {
        cleanup_conversion_dir(&conversion_dir).await;
        if let Err(error) = fs::remove_file(&pdf_path).await {
            warn!(
                "failed to remove mismatched prepared PDF {}: {error}",
                pdf_path.display()
            );
        }
        return Err(ApiError::unauthorized(
            "direct upload token does not match prepared page count",
        ));
    }
    let total_pages = match checked_total_print_pages(page_count, copy_count) {
        Ok(value) => value,
        Err(error) => {
            upsert_live_activity(
                &state,
                &activity_id,
                LiveActivityKind::PrintUpload,
                LiveActivityStage::Failed,
                file_name.clone(),
                Some(user_name.clone()),
                Some(printer_mode),
                source_size,
                hinted_total_bytes.or(Some(source_size)),
                "页数超过限制，未提交打印。".to_owned(),
                Some(format!("{page_count} 页 x {copy_count} 份")),
            )
            .await;
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    cleanup_conversion_dir(&conversion_dir).await;

    accept_stored_print_job(
        &state,
        job_id,
        file_name,
        user_name,
        printer_mode,
        page_count,
        copy_count,
        total_pages,
        pdf_path,
        pdf_size,
        hinted_total_bytes,
        &activity_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn accept_stored_print_job(
    state: &AppState,
    job_id: String,
    file_name: String,
    user_name: String,
    printer_mode: PrinterMode,
    page_count: u32,
    copy_count: u32,
    total_pages: u32,
    pdf_path: PathBuf,
    pdf_size: u64,
    total_bytes: Option<u64>,
    activity_id: &str,
) -> Result<Json<AcceptedPrintResponse>, ApiError> {
    upsert_live_activity(
        state,
        activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Received,
        file_name.clone(),
        Some(user_name.clone()),
        Some(printer_mode),
        pdf_size,
        total_bytes.or(Some(pdf_size)),
        "Print file received and stored locally.".to_owned(),
        Some(format!(
            "{} | {} pages x {} copies | {}",
            user_name,
            page_count,
            copy_count,
            printer_mode.as_str()
        )),
    )
    .await;

    let printer_name = configured_printer_name(state, printer_mode).await;

    let record = LocalJobRecord {
        job: QueueJobRecord {
            id: job_id.clone(),
            user_name,
            file_name: file_name.clone(),
            page_count,
            copy_count,
            color_mode: printer_mode.as_str().to_owned(),
            status: JobStatus::Queued,
            submitted_at: now_iso(),
            detail: Some(queue_waiting_text()),
            pages_printed: Some(0),
            total_pages: Some(total_pages),
        },
        printer: printer_mode,
        printer_name,
        pdf_path: pdf_path.to_string_lossy().to_string(),
        attempts: 1,
        updated_at: now_iso(),
        document_id: None,
        sync_managed: false,
    };

    {
        let mut jobs = state.jobs.write().await;
        if jobs.contains_key(&job_id) {
            return Ok(Json(AcceptedPrintResponse {
                accepted: true,
                job_id,
            }));
        }
        jobs.insert(job_id.clone(), record.clone());
    }

    set_job_status(state, &job_id, JobStatus::Queued, None).await?;
    upsert_live_activity(
        state,
        activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Ready,
        file_name,
        Some(record.job.user_name.clone()),
        Some(printer_mode),
        pdf_size,
        total_bytes.or(Some(pdf_size)),
        "Print file queued.".to_owned(),
        Some(format!("Job {job_id} entered the print queue.")),
    )
    .await;

    let task_state = state.clone();
    let task_job_id = job_id.clone();
    tokio::spawn(async move {
        if let Err(error) = process_job(task_state, task_job_id).await {
            error!("print task failed: {error}");
        }
    });

    Ok(Json(AcceptedPrintResponse {
        accepted: true,
        job_id,
    }))
}

pub(crate) async fn post_convert_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Preview,
        None,
        None,
    )?;

    let hinted_total_bytes = parse_optional_u64_header(&headers, "x-upload-file-size");
    let hinted_file_name = parse_optional_header(&headers, "x-upload-file-name")
        .map(|value| sanitize_source_filename(&value))
        .filter(|value| !value.trim().is_empty());
    let activity_id = parse_optional_header(&headers, "x-upload-request-id")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("convert-preview-{}", Uuid::new_v4()));

    let mut source_bytes: Option<Vec<u8>> = None;
    let mut source_name: Option<String> = hinted_file_name.clone();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(format!("invalid multipart payload: {error}")))?
    {
        let field_name = field.name().unwrap_or_default().to_owned();
        if field_name != "file" {
            continue;
        }

        let incoming_name = field.file_name().unwrap_or("document");
        let sanitized_name = sanitize_source_filename(incoming_name);
        source_name = Some(sanitized_name.clone());
        let bytes = read_field_bytes_with_progress(
            &state,
            field,
            &activity_id,
            LiveActivityKind::ConvertPreview,
            &sanitized_name,
            None,
            None,
            hinted_total_bytes,
            MAX_UPLOAD_SIZE_BYTES,
            "source file exceeds the 256 MB limit",
            "failed to read source file bytes",
        )
        .await?;
        source_bytes = Some(bytes);
    }

    let source_bytes = source_bytes.ok_or_else(|| ApiError::bad_request("missing source file"))?;
    let source_name = source_name.unwrap_or_else(|| "document".to_owned());
    let extension = source_extension(&source_name);
    if extension.is_empty() || !is_supported_source_extension(&extension) {
        upsert_live_activity(
            &state,
            &activity_id,
            LiveActivityKind::ConvertPreview,
            LiveActivityStage::Failed,
            source_name.clone(),
            None,
            None,
            source_bytes.len() as u64,
            hinted_total_bytes.or(Some(source_bytes.len() as u64)),
            "文档类型不受支持，无法转换。".to_owned(),
            Some(source_name.clone()),
        )
        .await;
        return Err(ApiError::bad_request("unsupported source file type"));
    }

    if extension == "pdf" {
        let preview_cache_id = cache_preview_pdf(&state, &source_bytes).await?;
        upsert_live_activity(
            &state,
            &activity_id,
            LiveActivityKind::ConvertPreview,
            LiveActivityStage::Ready,
            source_name.clone(),
            None,
            None,
            source_bytes.len() as u64,
            hinted_total_bytes.or(Some(source_bytes.len() as u64)),
            "PDF 已接收，无需转换，可直接预览。".to_owned(),
            None,
        )
        .await;
        return pdf_response(
            source_bytes,
            &build_converted_pdf_name(&source_name),
            Some(&preview_cache_id),
        );
    }

    let conversion_dir = state
        .storage_dir
        .join(format!("convert-{}", Uuid::new_v4()));
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!("failed to create conversion directory: {error}"))
    })?;

    let source_path = conversion_dir.join(&source_name);
    fs::write(&source_path, &source_bytes)
        .await
        .map_err(|error| ApiError::internal(format!("failed to cache source file: {error}")))?;

    let converter = state.document_converter.read().await.clone();
    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::ConvertPreview,
        LiveActivityStage::Converting,
        source_name.clone(),
        None,
        None,
        source_bytes.len() as u64,
        hinted_total_bytes.or(Some(source_bytes.len() as u64)),
        format!("正在使用 {} 转换为 PDF。", converter.kind.as_label()),
        Some(source_name.clone()),
    )
    .await;
    let converted_path =
        match convert_document_to_pdf(&converter, &source_path, &conversion_dir).await {
            Ok(path) => path,
            Err(error) => {
                upsert_live_activity(
                    &state,
                    &activity_id,
                    LiveActivityKind::ConvertPreview,
                    LiveActivityStage::Failed,
                    source_name.clone(),
                    None,
                    None,
                    source_bytes.len() as u64,
                    hinted_total_bytes.or(Some(source_bytes.len() as u64)),
                    "文档转换失败。".to_owned(),
                    Some(error.to_string()),
                )
                .await;
                cleanup_conversion_dir(&conversion_dir).await;
                return Err(error);
            }
        };
    let converted_bytes = fs::read(&converted_path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to read converted PDF: {error}")))?;
    let preview_cache_id = cache_preview_pdf(&state, &converted_bytes).await?;

    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::ConvertPreview,
        LiveActivityStage::Ready,
        source_name.clone(),
        None,
        None,
        source_bytes.len() as u64,
        hinted_total_bytes.or(Some(source_bytes.len() as u64)),
        "文档转换完成，预览 PDF 已就绪。".to_owned(),
        Some(build_converted_pdf_name(&source_name)),
    )
    .await;

    cleanup_conversion_dir(&conversion_dir).await;

    pdf_response(
        converted_bytes,
        &build_converted_pdf_name(&source_name),
        Some(&preview_cache_id),
    )
}

pub(crate) async fn post_convert_preview_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    verify_upload_auth(
        &headers,
        &state.shared_secret,
        DirectUploadKind::Preview,
        None,
        None,
    )?;

    let hinted_total_bytes = parse_optional_u64_header(&headers, "x-upload-file-size");
    let source_name = parse_optional_decoded_header(&headers, "x-upload-file-name")
        .map(|value| sanitize_source_filename(&value))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "document".to_owned());
    let activity_id = parse_optional_header(&headers, "x-upload-request-id")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("convert-preview-{}", Uuid::new_v4()));

    let extension = source_extension(&source_name);
    if extension.is_empty() || !is_supported_source_extension(&extension) {
        upsert_live_activity(
            &state,
            &activity_id,
            LiveActivityKind::ConvertPreview,
            LiveActivityStage::Failed,
            source_name.clone(),
            None,
            None,
            0,
            hinted_total_bytes,
            "文档类型不受支持，无法转换。".to_owned(),
            Some(source_name.clone()),
        )
        .await;
        return Err(ApiError::bad_request("unsupported source file type"));
    }

    let conversion_dir = state
        .storage_dir
        .join(format!("convert-{}", Uuid::new_v4()));
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!("failed to create conversion directory: {error}"))
    })?;

    let source_path = conversion_dir.join(&source_name);
    let source_size = match write_raw_source_with_progress(
        &state,
        body,
        &source_path,
        &activity_id,
        &source_name,
        hinted_total_bytes,
    )
    .await
    {
        Ok(size) => size,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    if extension == "pdf" {
        let source_bytes = match fs::read(&source_path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                cleanup_conversion_dir(&conversion_dir).await;
                return Err(ApiError::internal(format!(
                    "failed to read received PDF: {error}"
                )));
            }
        };
        let preview_cache_id = match cache_preview_pdf(&state, &source_bytes).await {
            Ok(cache_id) => cache_id,
            Err(error) => {
                cleanup_conversion_dir(&conversion_dir).await;
                return Err(error);
            }
        };
        upsert_live_activity(
            &state,
            &activity_id,
            LiveActivityKind::ConvertPreview,
            LiveActivityStage::Ready,
            source_name.clone(),
            None,
            None,
            source_size,
            hinted_total_bytes.or(Some(source_size)),
            "PDF 已直传到本地服务，可直接预览。".to_owned(),
            None,
        )
        .await;
        cleanup_conversion_dir(&conversion_dir).await;
        return pdf_response(
            source_bytes,
            &build_converted_pdf_name(&source_name),
            Some(&preview_cache_id),
        );
    }

    let converter = state.document_converter.read().await.clone();
    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::ConvertPreview,
        LiveActivityStage::Converting,
        source_name.clone(),
        None,
        None,
        source_size,
        hinted_total_bytes.or(Some(source_size)),
        format!("正在使用 {} 转换为 PDF。", converter.kind.as_label()),
        Some(source_name.clone()),
    )
    .await;

    let converted_path =
        match convert_document_to_pdf(&converter, &source_path, &conversion_dir).await {
            Ok(path) => path,
            Err(error) => {
                upsert_live_activity(
                    &state,
                    &activity_id,
                    LiveActivityKind::ConvertPreview,
                    LiveActivityStage::Failed,
                    source_name.clone(),
                    None,
                    None,
                    source_size,
                    hinted_total_bytes.or(Some(source_size)),
                    "文档转换失败。".to_owned(),
                    Some(error.to_string()),
                )
                .await;
                cleanup_conversion_dir(&conversion_dir).await;
                return Err(error);
            }
        };

    let converted_bytes = match fs::read(&converted_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(ApiError::internal(format!(
                "failed to read converted PDF: {error}"
            )));
        }
    };
    let preview_cache_id = match cache_preview_pdf(&state, &converted_bytes).await {
        Ok(cache_id) => cache_id,
        Err(error) => {
            cleanup_conversion_dir(&conversion_dir).await;
            return Err(error);
        }
    };

    upsert_live_activity(
        &state,
        &activity_id,
        LiveActivityKind::ConvertPreview,
        LiveActivityStage::Ready,
        source_name.clone(),
        None,
        None,
        source_size,
        hinted_total_bytes.or(Some(source_size)),
        "文档转换完成，预览 PDF 已就绪。".to_owned(),
        Some(build_converted_pdf_name(&source_name)),
    )
    .await;

    cleanup_conversion_dir(&conversion_dir).await;

    pdf_response(
        converted_bytes,
        &build_converted_pdf_name(&source_name),
        Some(&preview_cache_id),
    )
}

pub(crate) async fn get_public_job_status(
    State(state): State<AppState>,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<QueueJobRecord>, ApiError> {
    let jobs = state.jobs.read().await;
    let record = jobs
        .get(&job_id)
        .ok_or_else(|| ApiError::not_found("job not found"))?;

    Ok(Json(record.job.clone()))
}

pub(crate) async fn ws_status(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

pub(crate) async fn ws_prepare(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.max_frame_size((PREPARE_WS_MAX_CHUNK_SIZE_BYTES + 4) as usize)
        .max_message_size((PREPARE_WS_MAX_CHUNK_SIZE_BYTES + 4) as usize)
        .on_upgrade(move |socket| handle_prepare_websocket(socket, state))
}

pub(crate) async fn handle_prepare_websocket(mut socket: WebSocket, state: AppState) {
    let hello = match tokio::time::timeout(Duration::from_secs(15), socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            match serde_json::from_str::<PrepareWsClientMessage>(text.as_str()) {
                Ok(PrepareWsClientMessage::Hello {
                    token,
                    upload_id,
                    total_bytes,
                    source_name,
                    chunk_size_bytes,
                }) => (token, upload_id, total_bytes, source_name, chunk_size_bytes),
                Ok(PrepareWsClientMessage::Complete) => {
                    send_prepare_ws_error(&mut socket, "hello message required").await;
                    return;
                }
                Err(error) => {
                    send_prepare_ws_error(
                        &mut socket,
                        &format!("invalid prepare websocket hello: {error}"),
                    )
                    .await;
                    return;
                }
            }
        }
        Ok(Some(Ok(_))) => {
            send_prepare_ws_error(&mut socket, "hello text message required").await;
            return;
        }
        Ok(Some(Err(error))) => {
            warn!("prepare websocket hello receive error: {error}");
            return;
        }
        Ok(None) | Err(_) => {
            send_prepare_ws_error(&mut socket, "prepare websocket hello timeout").await;
            return;
        }
    };

    let (token, upload_id, total_bytes, source_name, chunk_size_bytes) = hello;
    let source_name = match validate_prepare_source(total_bytes, &source_name) {
        Ok(value) => value,
        Err(error) => {
            send_prepare_ws_error(&mut socket, &error.to_string()).await;
            return;
        }
    };
    if let Err(error) = validate_prepare_upload_id(&upload_id) {
        send_prepare_ws_error(&mut socket, &error.to_string()).await;
        return;
    }
    if !is_supported_prepare_ws_chunk_size(chunk_size_bytes) {
        send_prepare_ws_error(&mut socket, "prepare websocket chunk size mismatch").await;
        return;
    }

    let auth_payload = match verify_direct_upload_token(
        &state.shared_secret,
        &token,
        DirectUploadKind::PrepareWs,
        None,
        None,
    ) {
        Ok(value) => value,
        Err(error) => {
            send_prepare_ws_error(&mut socket, &error.to_string()).await;
            return;
        }
    };
    if auth_payload.upload_id.as_deref() != Some(upload_id.as_str())
        || auth_payload.total_bytes != Some(total_bytes)
    {
        send_prepare_ws_error(
            &mut socket,
            "prepare websocket token does not match upload session",
        )
        .await;
        return;
    }

    let prepared_path = preview_cache_path(state.storage_dir.as_ref(), &upload_id);
    if fs::metadata(&prepared_path).await.is_ok() {
        let prepared_source_name =
            match load_prepare_upload_manifest(state.storage_dir.as_ref(), &upload_id).await {
                Ok(manifest) => manifest.source_name,
                Err(_) => source_name.clone(),
            };
        match prepared_response_from_path(&prepared_path, &upload_id, &prepared_source_name).await {
            Ok(response) => {
                let _ = send_prepare_ws_message(&mut socket, prepared_ws_message(response)).await;
            }
            Err(error) => {
                send_prepare_ws_error(&mut socket, &error.to_string()).await;
            }
        }
        return;
    }

    let manifest = match initialize_prepare_ws_upload(
        &state,
        &upload_id,
        &source_name,
        total_bytes,
        chunk_size_bytes,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            send_prepare_ws_error(&mut socket, &error.to_string()).await;
            return;
        }
    };
    if fs::metadata(&prepared_path).await.is_ok() {
        match prepared_response_from_path(&prepared_path, &upload_id, &manifest.source_name).await {
            Ok(response) => {
                let _ = send_prepare_ws_message(&mut socket, prepared_ws_message(response)).await;
            }
            Err(error) => {
                send_prepare_ws_error(&mut socket, &error.to_string()).await;
            }
        }
        return;
    }
    if send_prepare_ws_message(&mut socket, prepare_ws_ready_message(&manifest))
        .await
        .is_err()
    {
        return;
    }

    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            inbound = socket.next() => {
                match inbound {
                    Some(Ok(Message::Binary(payload))) => {
                        if payload.len() < 5 {
                            send_prepare_ws_error(&mut socket, "prepare websocket chunk is empty").await;
                            continue;
                        }
                        let chunk_index = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                        match accept_prepare_ws_chunk(&state, &upload_id, chunk_index, &payload[4..]).await {
                            Ok(message) => {
                                if send_prepare_ws_message(&mut socket, message).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                send_prepare_ws_error(&mut socket, &error.to_string()).await;
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<PrepareWsClientMessage>(text.as_str()) {
                            Ok(PrepareWsClientMessage::Complete) => {
                                let progress = match load_prepare_upload_manifest(state.storage_dir.as_ref(), &upload_id).await {
                                    Ok(value) => manifest_progress(&value),
                                    Err(error) => {
                                        send_prepare_ws_error(&mut socket, &error.to_string()).await;
                                        continue;
                                    }
                                };
                                if send_prepare_ws_message(
                                    &mut socket,
                                    PrepareWsServerMessage::Processing {
                                        upload_id: upload_id.clone(),
                                        received_bytes: progress.received_bytes,
                                        total_bytes: progress.total_bytes,
                                        percent: progress.percent,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }

                                match complete_prepare_ws_upload(&state, &upload_id).await {
                                    Ok(response) => {
                                        let _ = send_prepare_ws_message(&mut socket, prepared_ws_message(response)).await;
                                        break;
                                    }
                                    Err(error) => {
                                        send_prepare_ws_error(&mut socket, &error.to_string()).await;
                                    }
                                }
                            }
                            Ok(PrepareWsClientMessage::Hello { .. }) => {
                                match load_prepare_upload_manifest(state.storage_dir.as_ref(), &upload_id).await {
                                    Ok(value) => {
                                        let _ = send_prepare_ws_message(&mut socket, prepare_ws_ready_message(&value)).await;
                                    }
                                    Err(error) => {
                                        send_prepare_ws_error(&mut socket, &error.to_string()).await;
                                    }
                                }
                            }
                            Err(error) => {
                                send_prepare_ws_error(&mut socket, &format!("invalid prepare websocket message: {error}")).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Err(error)) => {
                        warn!("prepare websocket receive error: {error}");
                        break;
                    }
                }
            }
        }
    }
}

pub(crate) async fn handle_websocket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.status_tx.subscribe();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));

    loop {
        tokio::select! {
            message = rx.recv() => {
                match message {
                    Ok(event) => {
                        let payload = match serde_json::to_string(&event) {
                            Ok(value) => value,
                            Err(error) => {
                                warn!("failed to serialize websocket payload: {error}");
                                continue;
                            }
                        };

                        if socket.send(Message::Text(payload)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("websocket client lagged behind by {skipped} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = heartbeat.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            inbound = socket.next() => {
                match inbound {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        warn!("websocket receive error: {error}");
                        break;
                    }
                }
            }
        }
    }
}

pub(crate) async fn get_admin_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminConfigResponse>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;
    if let Err(error) = sync_cached_config(&state).await {
        warn!("failed to refresh Cloudflare KV config for admin page; using cached local config: {error}");
    }

    let cached = state.cached_config.read().await.clone();
    let document_converter = state.document_converter.read().await.clone();
    Ok(Json(AdminConfigResponse {
        prices: cached.prices,
        qrcodes: cached.qrcodes,
        notice_markdown: cached.notice_markdown,
        printers: cached.printers,
        document_converter,
    }))
}

pub(crate) async fn post_admin_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<AdminConfigUpdate>,
) -> Result<Json<AdminConfigResponse>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    let mut next_config = state.cached_config.read().await.clone();
    let mut next_converter = state.document_converter.read().await.clone();

    if let Some(prices) = payload.prices {
        next_config.prices = prices;
    }
    if let Some(qrcodes) = payload.qrcodes {
        next_config.qrcodes = qrcodes;
    }
    if let Some(printers) = payload.printers {
        next_config.printers = printers.clone();
        *state.printers.write().await = printers;
    }
    if let Some(notice_markdown) = payload.notice_markdown {
        next_config.notice_markdown = notice_markdown;
    }
    if let Some(document_converter) = payload.document_converter {
        next_converter = normalize_document_converter_config(document_converter)
            .map_err(ApiError::bad_request)?;
    }

    if let Some(cloudflare) = &state.cloudflare {
        cloudflare
            .put_json("config:prices", &next_config.prices)
            .await?;
        cloudflare
            .put_json("config:qrcodes", &next_config.qrcodes)
            .await?;
        cloudflare
            .put_json("config:printers", &next_config.printers)
            .await?;
        cloudflare
            .put_text("config:notice_markdown", &next_config.notice_markdown)
            .await?;
    }

    *state.cached_config.write().await = next_config.clone();
    *state.document_converter.write().await = next_converter.clone();
    persist_runtime_config(state.runtime_config_path.as_ref(), &next_converter).await?;

    Ok(Json(AdminConfigResponse {
        prices: next_config.prices,
        qrcodes: next_config.qrcodes,
        notice_markdown: next_config.notice_markdown,
        printers: next_config.printers,
        document_converter: next_converter,
    }))
}

pub(crate) async fn get_admin_printers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<String>>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    Ok(Json(list_available_printers().await?))
}

pub(crate) async fn get_admin_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminJobsResponse>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    let jobs = state.jobs.read().await;
    let mut active = Vec::new();
    let mut history = Vec::new();

    for record in jobs.values() {
        if record.job.status.is_terminal() {
            history.push(record.clone());
        } else {
            active.push(record.clone());
        }
    }

    active.sort_by(|left, right| left.updated_at.cmp(&right.updated_at));
    history.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    Ok(Json(AdminJobsResponse { active, history }))
}

pub(crate) async fn get_admin_live_activities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<LiveActivityRecord>>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    Ok(Json(list_live_activities(&state).await))
}

pub(crate) async fn get_admin_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminDiagnosticsResponse>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    let checks = run_admin_diagnostics(&state).await;
    let summary = summarize_diagnostics(&checks);

    Ok(Json(AdminDiagnosticsResponse {
        generated_at: now_iso(),
        summary,
        local_ws_url: state.public_ws_url.as_ref().clone(),
        checks,
    }))
}

pub(crate) async fn post_admin_ws_probe(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminWsProbeResponse>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    let job_id = format!("diag-ws-{}", Uuid::new_v4());
    let detail = format!("WebSocket probe emitted at {}", now_iso());
    let status = JobStatus::Queued;

    let _ = state.status_tx.send(StatusEvent {
        kind: StatusStreamKind::Job,
        job_id: job_id.clone(),
        status,
        detail: Some(detail.clone()),
        pages_printed: None,
        total_pages: None,
        activity: None,
    });

    Ok(Json(AdminWsProbeResponse {
        job_id,
        status,
        detail,
        ws_url: state.public_ws_url.as_ref().clone(),
    }))
}

pub(crate) async fn retry_admin_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<LocalJobRecord>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    {
        let mut jobs = state.jobs.write().await;
        let Some(record) = jobs.get_mut(&job_id) else {
            return Err(ApiError::not_found("job not found"));
        };

        if !record.job.status.is_terminal() {
            return Err(ApiError::conflict("job is already active"));
        }

        if !Path::new(&record.pdf_path).exists() {
            return Err(ApiError::not_found("cached PDF for retry no longer exists"));
        }

        record.attempts += 1;
        record.updated_at = now_iso();
    }

    set_job_status(&state, &job_id, JobStatus::Queued, Some(admin_retry_text())).await?;

    let task_state = state.clone();
    let task_job_id = job_id.clone();
    tokio::spawn(async move {
        if let Err(error) = process_job(task_state, task_job_id).await {
            error!("retried print task failed: {error}");
        }
    });

    let jobs = state.jobs.read().await;
    let record = jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("job not found after retry"))?;
    Ok(Json(record))
}

pub(crate) async fn cancel_admin_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> Result<Json<LocalJobRecord>, ApiError> {
    verify_admin(&headers, &state.admin_token)?;

    let current_status = {
        let jobs = state.jobs.read().await;
        jobs.get(&job_id)
            .map(|record| record.job.status)
            .ok_or_else(|| ApiError::not_found("job not found"))?
    };

    if current_status == JobStatus::Printing {
        return Err(ApiError::conflict(
            "job is already printing and cannot be cancelled safely",
        ));
    }

    set_job_status(
        &state,
        &job_id,
        JobStatus::Failed,
        Some(PrintFailure::Cancelled.detail()),
    )
    .await?;

    let jobs = state.jobs.read().await;
    let record = jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("job not found after cancellation"))?;
    Ok(Json(record))
}
