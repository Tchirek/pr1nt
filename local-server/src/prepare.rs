//! Upload preparation: chunked/WebSocket intake, manifests, and PDF staging.
use crate::conversion::{
    build_converted_pdf_name, cleanup_conversion_dir, convert_document_to_pdf,
    convert_image_to_pdf, is_supported_image_extension, is_supported_source_extension,
    source_extension,
};
use crate::error::*;
use crate::model::*;
use crate::printing::*;
use crate::text::{format_bytes, now_iso};
use crate::util::*;
use axum::{
    body::Body,
    extract::ws::{Message, WebSocket},
    http::HeaderMap,
};
use futures::StreamExt;
use std::{
    collections::BTreeSet,
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    fs,
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::Mutex,
};
use tracing::warn;

pub(crate) fn parse_prepare_source_headers(headers: &HeaderMap) -> Result<(u64, String), ApiError> {
    let total_bytes = parse_optional_u64_header(headers, "x-upload-file-size")
        .ok_or_else(|| ApiError::bad_request("missing upload file size"))?;
    let source_name = parse_optional_decoded_header(headers, "x-upload-file-name")
        .unwrap_or_else(|| "document".to_owned());
    let source_name = validate_prepare_source(total_bytes, &source_name)?;

    Ok((total_bytes, source_name))
}

pub(crate) fn validate_prepare_source(
    total_bytes: u64,
    source_name: &str,
) -> Result<String, ApiError> {
    if total_bytes == 0 {
        return Err(ApiError::bad_request("empty source upload"));
    }
    if total_bytes as usize > MAX_UPLOAD_SIZE_BYTES {
        return Err(ApiError::bad_request(
            "source file exceeds the 256 MB limit",
        ));
    }

    let source_name = sanitize_source_filename(source_name);
    if source_name.trim().is_empty() {
        return Err(ApiError::bad_request("missing source file name"));
    }
    let extension = source_extension(&source_name);
    if extension.is_empty() || !is_supported_source_extension(&extension) {
        return Err(ApiError::bad_request("unsupported source file type"));
    }

    Ok(source_name)
}

pub(crate) fn validate_prepare_upload_id(upload_id: &str) -> Result<(), ApiError> {
    if is_valid_preview_cache_id(upload_id) {
        Ok(())
    } else {
        Err(ApiError::bad_request("invalid prepare upload id"))
    }
}

pub(crate) fn prepare_upload_part_path(storage_dir: &Path, upload_id: &str) -> PathBuf {
    storage_dir.join(format!("prepare-upload-{upload_id}.part"))
}

pub(crate) fn prepare_chunk_response(
    upload_id: &str,
    received_bytes: u64,
    total_bytes: u64,
) -> PrepareChunkResponse {
    PrepareChunkResponse {
        upload_id: upload_id.to_owned(),
        received_bytes,
        total_bytes,
        percent: prepare_confirmed_percent(received_bytes, total_bytes),
    }
}

pub(crate) fn prepare_confirmed_percent(received_bytes: u64, total_bytes: u64) -> u8 {
    if total_bytes == 0 {
        return 100;
    }
    (((received_bytes as f64 / total_bytes as f64) * 100.0).round() as u8).min(100)
}

pub(crate) async fn send_prepare_ws_message(
    socket: &mut WebSocket,
    message: PrepareWsServerMessage,
) -> Result<(), ApiError> {
    let payload = serde_json::to_string(&message).map_err(|error| {
        ApiError::internal(format!(
            "failed to encode prepare websocket message: {error}"
        ))
    })?;
    socket.send(Message::Text(payload)).await.map_err(|error| {
        ApiError::internal(format!("failed to send prepare websocket message: {error}"))
    })
}

pub(crate) async fn send_prepare_ws_error(socket: &mut WebSocket, message: &str) {
    let _ = send_prepare_ws_message(
        socket,
        PrepareWsServerMessage::Error {
            message: message.to_owned(),
        },
    )
    .await;
}

pub(crate) fn prepared_ws_message(response: PreparedPrintResponse) -> PrepareWsServerMessage {
    PrepareWsServerMessage::Prepared {
        prepared_id: response.prepared_id,
        page_count: response.page_count,
        file_name: response.file_name,
        source_name: response.source_name,
    }
}

pub(crate) fn prepare_ws_ready_message(manifest: &PrepareUploadManifest) -> PrepareWsServerMessage {
    let progress = manifest_progress(manifest);
    PrepareWsServerMessage::Ready {
        upload_id: manifest.upload_id.clone(),
        confirmed_chunks: manifest.confirmed_chunks.iter().copied().collect(),
        received_bytes: progress.received_bytes,
        total_bytes: progress.total_bytes,
        percent: progress.percent,
    }
}

pub(crate) async fn prepare_upload_lock(state: &AppState, upload_id: &str) -> Arc<Mutex<()>> {
    let mut locks = state.prepare_upload_locks.lock().await;
    locks
        .entry(upload_id.to_owned())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(crate) async fn initialize_prepare_ws_upload(
    state: &AppState,
    upload_id: &str,
    source_name: &str,
    total_bytes: u64,
    chunk_size_bytes: u64,
) -> Result<PrepareUploadManifest, ApiError> {
    validate_prepare_upload_id(upload_id)?;
    let source_name = validate_prepare_source(total_bytes, source_name)?;
    if !is_supported_prepare_ws_chunk_size(chunk_size_bytes) {
        return Err(ApiError::bad_request(
            "prepare websocket chunk size mismatch",
        ));
    }

    let upload_lock = prepare_upload_lock(state, upload_id).await;
    let _guard = upload_lock.lock().await;
    let manifest_path = prepare_upload_manifest_path(state.storage_dir.as_ref(), upload_id);
    let part_path = prepare_upload_part_path(state.storage_dir.as_ref(), upload_id);
    let prepared_path = preview_cache_path(state.storage_dir.as_ref(), upload_id);

    let manifest = if fs::metadata(&manifest_path).await.is_ok()
        || fs::metadata(prepare_upload_manifest_temp_path(
            state.storage_dir.as_ref(),
            upload_id,
        ))
        .await
        .is_ok()
    {
        let mut existing =
            load_prepare_upload_manifest(state.storage_dir.as_ref(), upload_id).await?;
        validate_prepare_manifest(
            &existing,
            upload_id,
            &source_name,
            total_bytes,
            chunk_size_bytes,
        )?;

        if fs::metadata(&prepared_path).await.is_err() {
            let part_metadata = fs::metadata(&part_path).await.map_err(|error| {
                ApiError::conflict(format!("prepare upload data is missing: {error}"))
            })?;
            if part_metadata.len() > total_bytes {
                return Err(ApiError::conflict(
                    "prepare upload data is larger than its manifest",
                ));
            }
        }

        existing.updated_at = now_iso();
        save_prepare_upload_manifest(state.storage_dir.as_ref(), &existing).await?;
        existing
    } else {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&part_path)
            .await
            .map_err(|error| {
                ApiError::internal(format!("failed to create prepare upload file: {error}"))
            })?;
        drop(file);

        let created = PrepareUploadManifest {
            upload_id: upload_id.to_owned(),
            source_name: source_name.clone(),
            total_bytes,
            chunk_size_bytes,
            confirmed_chunks: BTreeSet::new(),
            updated_at: now_iso(),
        };
        save_prepare_upload_manifest(state.storage_dir.as_ref(), &created).await?;
        created
    };

    let progress = manifest_progress(&manifest);
    upsert_live_activity(
        state,
        &format!("prepare:{upload_id}"),
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Receiving,
        manifest.source_name.clone(),
        None,
        None,
        progress.received_bytes,
        Some(progress.total_bytes),
        receiving_activity_summary(
            LiveActivityKind::PrintUpload,
            progress.received_bytes,
            Some(progress.total_bytes),
        ),
        Some(format!(
            "Confirmed by local server: {} / {}",
            format_bytes(progress.received_bytes),
            format_bytes(progress.total_bytes)
        )),
    )
    .await;

    Ok(manifest)
}

pub(crate) fn validate_prepare_manifest(
    manifest: &PrepareUploadManifest,
    upload_id: &str,
    source_name: &str,
    total_bytes: u64,
    chunk_size_bytes: u64,
) -> Result<(), ApiError> {
    if manifest.upload_id != upload_id
        || manifest.source_name != source_name
        || manifest.total_bytes != total_bytes
        || manifest.chunk_size_bytes != chunk_size_bytes
    {
        return Err(ApiError::conflict(
            "prepare upload session metadata does not match",
        ));
    }
    Ok(())
}

pub(crate) fn is_supported_prepare_ws_chunk_size(chunk_size_bytes: u64) -> bool {
    chunk_size_bytes.is_power_of_two()
        && (PREPARE_WS_MIN_CHUNK_SIZE_BYTES..=PREPARE_WS_MAX_CHUNK_SIZE_BYTES)
            .contains(&chunk_size_bytes)
}

pub(crate) async fn accept_prepare_ws_chunk(
    state: &AppState,
    upload_id: &str,
    chunk_index: u32,
    chunk: &[u8],
) -> Result<PrepareWsServerMessage, ApiError> {
    let upload_lock = prepare_upload_lock(state, upload_id).await;
    let _guard = upload_lock.lock().await;
    let mut manifest = load_prepare_upload_manifest(state.storage_dir.as_ref(), upload_id).await?;
    let expected_len = expected_prepare_chunk_len(&manifest, chunk_index)
        .ok_or_else(|| ApiError::bad_request("prepare websocket chunk index is out of range"))?;
    if chunk.len() as u64 != expected_len {
        return Err(ApiError::bad_request(format!(
            "prepare websocket chunk length mismatch: expected {expected_len}, received {}",
            chunk.len()
        )));
    }

    if !manifest.confirmed_chunks.contains(&chunk_index) {
        let part_path = prepare_upload_part_path(state.storage_dir.as_ref(), upload_id);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&part_path)
            .await
            .map_err(|error| {
                ApiError::internal(format!("failed to open prepare upload file: {error}"))
            })?;
        file.seek(SeekFrom::Start(
            u64::from(chunk_index) * manifest.chunk_size_bytes,
        ))
        .await
        .map_err(|error| {
            ApiError::internal(format!("failed to seek prepare upload file: {error}"))
        })?;
        file.write_all(chunk).await.map_err(|error| {
            ApiError::internal(format!("failed to write prepare upload chunk: {error}"))
        })?;
        file.flush().await.map_err(|error| {
            ApiError::internal(format!("failed to flush prepare upload chunk: {error}"))
        })?;

        manifest.confirmed_chunks.insert(chunk_index);
        manifest.updated_at = now_iso();
        save_prepare_upload_manifest(state.storage_dir.as_ref(), &manifest).await?;
    }

    let progress = manifest_progress(&manifest);
    upsert_live_activity(
        state,
        &format!("prepare:{upload_id}"),
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Receiving,
        manifest.source_name.clone(),
        None,
        None,
        progress.received_bytes,
        Some(progress.total_bytes),
        receiving_activity_summary(
            LiveActivityKind::PrintUpload,
            progress.received_bytes,
            Some(progress.total_bytes),
        ),
        Some(format!(
            "Confirmed by local server: {} / {}",
            format_bytes(progress.received_bytes),
            format_bytes(progress.total_bytes)
        )),
    )
    .await;

    Ok(PrepareWsServerMessage::Ack {
        upload_id: upload_id.to_owned(),
        chunk_index,
        received_bytes: progress.received_bytes,
        total_bytes: progress.total_bytes,
        percent: progress.percent,
    })
}

pub(crate) async fn complete_prepare_ws_upload(
    state: &AppState,
    upload_id: &str,
) -> Result<PreparedPrintResponse, ApiError> {
    let upload_lock = prepare_upload_lock(state, upload_id).await;
    let _guard = upload_lock.lock().await;
    let mut manifest = load_prepare_upload_manifest(state.storage_dir.as_ref(), upload_id).await?;
    let prepared_path = preview_cache_path(state.storage_dir.as_ref(), upload_id);
    if fs::metadata(&prepared_path).await.is_ok() {
        return prepared_response_from_path(&prepared_path, upload_id, &manifest.source_name).await;
    }

    let progress = manifest_progress(&manifest);
    if progress.received_bytes != progress.total_bytes
        || manifest.confirmed_chunks.len() != prepare_chunk_count(&manifest) as usize
    {
        upsert_live_activity(
            state,
            &format!("prepare:{upload_id}"),
            LiveActivityKind::PrintUpload,
            LiveActivityStage::Receiving,
            manifest.source_name.clone(),
            None,
            None,
            progress.received_bytes,
            Some(progress.total_bytes),
            receiving_activity_summary(
                LiveActivityKind::PrintUpload,
                progress.received_bytes,
                Some(progress.total_bytes),
            ),
            Some("Upload is not complete yet.".to_owned()),
        )
        .await;
        return Err(ApiError::conflict(format!(
            "upload is incomplete: {} / {}",
            format_bytes(progress.received_bytes),
            format_bytes(progress.total_bytes)
        )));
    }

    let activity_id = format!("prepare:{upload_id}");
    let conversion_dir = state
        .storage_dir
        .join(format!("prepare-source-{upload_id}"));
    cleanup_conversion_dir(&conversion_dir).await;
    fs::create_dir_all(&conversion_dir).await.map_err(|error| {
        ApiError::internal(format!(
            "failed to create prepare conversion directory: {error}"
        ))
    })?;
    let source_path = conversion_dir.join(&manifest.source_name);
    let part_path = prepare_upload_part_path(state.storage_dir.as_ref(), upload_id);
    let part_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&part_path)
        .await
        .map_err(|error| {
            ApiError::internal(format!("failed to open completed prepare upload: {error}"))
        })?;
    part_file.sync_data().await.map_err(|error| {
        ApiError::internal(format!(
            "failed to persist completed prepare upload: {error}"
        ))
    })?;
    drop(part_file);
    fs::copy(&part_path, &source_path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to stage prepared source: {error}")))?;

    if let Err(error) = prepare_print_pdf_to_path(
        state,
        &conversion_dir,
        &source_path,
        &manifest.source_name,
        &prepared_path,
        &activity_id,
        None,
        None,
        progress.received_bytes,
        Some(progress.total_bytes),
    )
    .await
    {
        cleanup_conversion_dir(&conversion_dir).await;
        return Err(error);
    }

    let response =
        match prepared_response_from_path(&prepared_path, upload_id, &manifest.source_name).await {
            Ok(value) => value,
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

    manifest.updated_at = now_iso();
    save_prepare_upload_manifest(state.storage_dir.as_ref(), &manifest).await?;
    if let Err(error) = fs::remove_file(&part_path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "failed to remove completed prepare upload {}: {error}",
                part_path.display()
            );
        }
    }
    upsert_live_activity(
        state,
        &activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Ready,
        manifest.source_name.clone(),
        None,
        None,
        progress.received_bytes,
        Some(progress.total_bytes),
        format!("Prepared {} printable pages.", response.page_count),
        Some(upload_id.to_owned()),
    )
    .await;
    cleanup_conversion_dir(&conversion_dir).await;

    Ok(response)
}

pub(crate) async fn prepared_response_from_path(
    prepared_path: &Path,
    upload_id: &str,
    source_name: &str,
) -> Result<PreparedPrintResponse, ApiError> {
    let page_count = count_pdf_pages(prepared_path).await?.max(1);
    Ok(PreparedPrintResponse {
        prepared_id: upload_id.to_owned(),
        page_count,
        file_name: build_converted_pdf_name(source_name),
        source_name: source_name.to_owned(),
    })
}

pub(crate) fn prepare_chunk_count(manifest: &PrepareUploadManifest) -> u32 {
    manifest.total_bytes.div_ceil(manifest.chunk_size_bytes) as u32
}

pub(crate) fn expected_prepare_chunk_len(
    manifest: &PrepareUploadManifest,
    chunk_index: u32,
) -> Option<u64> {
    if chunk_index >= prepare_chunk_count(manifest) {
        return None;
    }
    let offset = u64::from(chunk_index) * manifest.chunk_size_bytes;
    Some((manifest.total_bytes - offset).min(manifest.chunk_size_bytes))
}

pub(crate) fn manifest_progress(manifest: &PrepareUploadManifest) -> PrepareUploadProgress {
    let received_bytes = manifest
        .confirmed_chunks
        .iter()
        .filter_map(|chunk_index| expected_prepare_chunk_len(manifest, *chunk_index))
        .sum::<u64>()
        .min(manifest.total_bytes);
    PrepareUploadProgress {
        received_bytes,
        total_bytes: manifest.total_bytes,
        percent: prepare_confirmed_percent(received_bytes, manifest.total_bytes),
    }
}

pub(crate) async fn load_prepare_upload_manifest(
    storage_dir: &Path,
    upload_id: &str,
) -> Result<PrepareUploadManifest, ApiError> {
    validate_prepare_upload_id(upload_id)?;
    let path = prepare_upload_manifest_path(storage_dir, upload_id);
    let bytes = match fs::read(&path).await {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let temp_path = prepare_upload_manifest_temp_path(storage_dir, upload_id);
            fs::read(&temp_path).await.map_err(|temp_error| {
                ApiError::conflict(format!(
                    "prepare upload manifest is not ready: {temp_error}"
                ))
            })?
        }
        Err(error) => {
            return Err(ApiError::internal(format!(
                "failed to read prepare upload manifest: {error}"
            )))
        }
    };
    let manifest = serde_json::from_slice::<PrepareUploadManifest>(&bytes).map_err(|error| {
        ApiError::internal(format!("failed to decode prepare upload manifest: {error}"))
    })?;
    if manifest.upload_id != upload_id {
        return Err(ApiError::conflict("prepare upload manifest id mismatch"));
    }
    Ok(manifest)
}

pub(crate) async fn save_prepare_upload_manifest(
    storage_dir: &Path,
    manifest: &PrepareUploadManifest,
) -> Result<(), ApiError> {
    let payload = serde_json::to_vec(manifest).map_err(|error| {
        ApiError::internal(format!("failed to encode prepare upload manifest: {error}"))
    })?;
    let path = prepare_upload_manifest_path(storage_dir, &manifest.upload_id);
    let temp_path = prepare_upload_manifest_temp_path(storage_dir, &manifest.upload_id);
    let mut file = fs::File::create(&temp_path).await.map_err(|error| {
        ApiError::internal(format!("failed to create prepare upload manifest: {error}"))
    })?;
    file.write_all(&payload).await.map_err(|error| {
        ApiError::internal(format!("failed to write prepare upload manifest: {error}"))
    })?;
    file.flush().await.map_err(|error| {
        ApiError::internal(format!("failed to flush prepare upload manifest: {error}"))
    })?;
    drop(file);

    if let Err(error) = fs::remove_file(&path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(ApiError::internal(format!(
                "failed to replace prepare upload manifest: {error}"
            )));
        }
    }
    fs::rename(&temp_path, &path).await.map_err(|error| {
        ApiError::internal(format!("failed to commit prepare upload manifest: {error}"))
    })
}

pub(crate) fn prepare_upload_manifest_path(storage_dir: &Path, upload_id: &str) -> PathBuf {
    storage_dir.join(format!("prepare-upload-{upload_id}.manifest.json"))
}

pub(crate) fn prepare_upload_manifest_temp_path(storage_dir: &Path, upload_id: &str) -> PathBuf {
    storage_dir.join(format!("prepare-upload-{upload_id}.manifest.tmp"))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn fail_raw_upload_file(
    state: &AppState,
    target_path: &Path,
    activity_id: &str,
    file_name: &str,
    user_name: Option<String>,
    printer: Option<PrinterMode>,
    received_bytes: u64,
    total_bytes: Option<u64>,
    detail: &str,
) {
    if let Err(error) = fs::remove_file(target_path).await {
        warn!(
            "failed to remove partial upload {}: {error}",
            target_path.display()
        );
    }

    upsert_live_activity(
        state,
        activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Failed,
        file_name.to_owned(),
        user_name,
        printer,
        received_bytes,
        total_bytes,
        "Print upload failed.".to_owned(),
        Some(detail.to_owned()),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_raw_print_source_with_progress(
    state: &AppState,
    body: Body,
    target_path: &Path,
    activity_id: &str,
    file_name: &str,
    user_name: Option<String>,
    printer: Option<PrinterMode>,
    total_bytes: Option<u64>,
) -> Result<u64, ApiError> {
    let mut stream = body.into_data_stream();
    let mut file = fs::File::create(target_path).await.map_err(|error| {
        ApiError::internal(format!("failed to create local source file: {error}"))
    })?;
    let mut received_bytes = 0u64;
    let mut last_reported = 0u64;

    upsert_live_activity(
        state,
        activity_id,
        LiveActivityKind::PrintUpload,
        LiveActivityStage::Receiving,
        file_name.to_owned(),
        user_name.clone(),
        printer,
        0,
        total_bytes,
        receiving_activity_summary(LiveActivityKind::PrintUpload, 0, total_bytes),
        None,
    )
    .await;

    loop {
        let next_chunk = tokio::time::timeout(Duration::from_secs(20), stream.next()).await;
        match next_chunk {
            Ok(Some(Ok(chunk))) => {
                received_bytes = received_bytes.saturating_add(chunk.len() as u64);
                if received_bytes as usize > MAX_UPLOAD_SIZE_BYTES {
                    fail_raw_upload_file(
                        state,
                        target_path,
                        activity_id,
                        file_name,
                        user_name.clone(),
                        printer,
                        received_bytes,
                        total_bytes,
                        "source file exceeds the 256 MB limit",
                    )
                    .await;
                    return Err(ApiError::bad_request(
                        "source file exceeds the 256 MB limit",
                    ));
                }

                file.write_all(&chunk).await.map_err(|error| {
                    ApiError::internal(format!("failed to write local source file: {error}"))
                })?;

                let reached_known_end = total_bytes.is_some_and(|value| received_bytes >= value);
                if received_bytes.saturating_sub(last_reported) >= 256 * 1024 || reached_known_end {
                    upsert_live_activity(
                        state,
                        activity_id,
                        LiveActivityKind::PrintUpload,
                        LiveActivityStage::Receiving,
                        file_name.to_owned(),
                        user_name.clone(),
                        printer,
                        received_bytes,
                        total_bytes,
                        receiving_activity_summary(
                            LiveActivityKind::PrintUpload,
                            received_bytes,
                            total_bytes,
                        ),
                        Some(format!("已接收 {}", format_bytes(received_bytes))),
                    )
                    .await;
                    last_reported = received_bytes;
                }
            }
            Ok(Some(Err(error))) => {
                let detail = format!("failed to receive source stream: {error}");
                fail_raw_upload_file(
                    state,
                    target_path,
                    activity_id,
                    file_name,
                    user_name,
                    printer,
                    received_bytes,
                    total_bytes,
                    &detail,
                )
                .await;
                return Err(ApiError::bad_request(detail));
            }
            Ok(None) => break,
            Err(_) => {
                let detail = "upload stalled before the source file was fully received";
                fail_raw_upload_file(
                    state,
                    target_path,
                    activity_id,
                    file_name,
                    user_name,
                    printer,
                    received_bytes,
                    total_bytes,
                    detail,
                )
                .await;
                return Err(ApiError::bad_request(detail));
            }
        }
    }

    file.flush().await.map_err(|error| {
        ApiError::internal(format!("failed to flush local source file: {error}"))
    })?;

    if received_bytes == 0 {
        fail_raw_upload_file(
            state,
            target_path,
            activity_id,
            file_name,
            user_name,
            printer,
            received_bytes,
            total_bytes,
            "empty source upload",
        )
        .await;
        return Err(ApiError::bad_request("empty source upload"));
    }

    if let Some(expected) = total_bytes {
        if received_bytes != expected {
            let detail = format!(
                "incomplete source upload: received {} of {}",
                format_bytes(received_bytes),
                format_bytes(expected)
            );
            fail_raw_upload_file(
                state,
                target_path,
                activity_id,
                file_name,
                user_name,
                printer,
                received_bytes,
                total_bytes,
                &detail,
            )
            .await;
            return Err(ApiError::bad_request(detail));
        }
    }

    Ok(received_bytes)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_print_pdf(
    state: &AppState,
    conversion_dir: &Path,
    source_path: &Path,
    source_name: &str,
    job_id: &str,
    activity_id: &str,
    user_name: Option<String>,
    printer: Option<PrinterMode>,
    received_bytes: u64,
    total_bytes: Option<u64>,
) -> Result<(PathBuf, u64), ApiError> {
    let final_pdf_name = sanitize_filename(&build_converted_pdf_name(source_name));
    let final_pdf_path = state.storage_dir.join(format!("{job_id}-{final_pdf_name}"));
    let pdf_size = prepare_print_pdf_to_path(
        state,
        conversion_dir,
        source_path,
        source_name,
        &final_pdf_path,
        activity_id,
        user_name,
        printer,
        received_bytes,
        total_bytes,
    )
    .await?;

    Ok((final_pdf_path, pdf_size))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare_print_pdf_to_path(
    state: &AppState,
    conversion_dir: &Path,
    source_path: &Path,
    source_name: &str,
    output_pdf_path: &Path,
    activity_id: &str,
    user_name: Option<String>,
    printer: Option<PrinterMode>,
    received_bytes: u64,
    total_bytes: Option<u64>,
) -> Result<u64, ApiError> {
    let extension = source_extension(source_name);
    let prepared_pdf_path = if extension == "pdf" {
        source_path.to_path_buf()
    } else if is_supported_image_extension(&extension) {
        upsert_live_activity(
            state,
            activity_id,
            LiveActivityKind::PrintUpload,
            LiveActivityStage::Converting,
            source_name.to_owned(),
            user_name.clone(),
            printer,
            received_bytes,
            total_bytes.or(Some(received_bytes)),
            "正在将图片排版为 A4 PDF。".to_owned(),
            None,
        )
        .await;
        convert_image_to_pdf(source_path, conversion_dir).await?
    } else {
        let converter = state.document_converter.read().await.clone();
        upsert_live_activity(
            state,
            activity_id,
            LiveActivityKind::PrintUpload,
            LiveActivityStage::Converting,
            source_name.to_owned(),
            user_name.clone(),
            printer,
            received_bytes,
            total_bytes.or(Some(received_bytes)),
            format!("正在使用 {} 转换。", converter.kind.as_label()),
            None,
        )
        .await;

        match convert_document_to_pdf(&converter, source_path, conversion_dir).await {
            Ok(path) => path,
            Err(error) => {
                upsert_live_activity(
                    state,
                    activity_id,
                    LiveActivityKind::PrintUpload,
                    LiveActivityStage::Failed,
                    source_name.to_owned(),
                    user_name,
                    printer,
                    received_bytes,
                    total_bytes.or(Some(received_bytes)),
                    "文档转换失败。".to_owned(),
                    Some(error.to_string()),
                )
                .await;
                return Err(error);
            }
        }
    };

    fs::copy(&prepared_pdf_path, output_pdf_path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to store prepared PDF: {error}")))?;
    let metadata = fs::metadata(output_pdf_path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to inspect prepared PDF: {error}")))?;

    Ok(metadata.len())
}

pub(crate) async fn count_pdf_pages(pdf_path: &Path) -> Result<u32, ApiError> {
    let path = pdf_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let document = lopdf::Document::load(&path).map_err(|error| {
            ApiError::bad_request(format!("failed to read PDF page count: {error}"))
        })?;
        let page_count = document.get_pages().len();
        u32::try_from(page_count)
            .map_err(|_| ApiError::bad_request("PDF page count is too large"))
            .and_then(|value| {
                if value == 0 {
                    Err(ApiError::bad_request("PDF has no printable pages"))
                } else {
                    Ok(value)
                }
            })
    })
    .await
    .map_err(|error| ApiError::internal(format!("failed to count PDF pages: {error}")))?
}

pub(crate) async fn write_raw_source_with_progress(
    state: &AppState,
    body: Body,
    target_path: &Path,
    activity_id: &str,
    file_name: &str,
    total_bytes: Option<u64>,
) -> Result<u64, ApiError> {
    let mut stream = body.into_data_stream();
    let mut file = fs::File::create(target_path).await.map_err(|error| {
        ApiError::internal(format!("failed to create local source file: {error}"))
    })?;
    let mut received_bytes = 0u64;
    let mut last_reported = 0u64;

    upsert_live_activity(
        state,
        activity_id,
        LiveActivityKind::ConvertPreview,
        LiveActivityStage::Receiving,
        file_name.to_owned(),
        None,
        None,
        0,
        total_bytes,
        receiving_activity_summary(LiveActivityKind::ConvertPreview, 0, total_bytes),
        None,
    )
    .await;

    loop {
        let next_chunk = tokio::time::timeout(Duration::from_secs(20), stream.next()).await;
        match next_chunk {
            Ok(Some(Ok(chunk))) => {
                received_bytes = received_bytes.saturating_add(chunk.len() as u64);
                if received_bytes as usize > MAX_UPLOAD_SIZE_BYTES {
                    fail_raw_source_file(
                        state,
                        target_path,
                        activity_id,
                        file_name,
                        received_bytes,
                        total_bytes,
                        "source file exceeds the 256 MB limit",
                    )
                    .await;
                    return Err(ApiError::bad_request(
                        "source file exceeds the 256 MB limit",
                    ));
                }

                file.write_all(&chunk).await.map_err(|error| {
                    ApiError::internal(format!("failed to write local source file: {error}"))
                })?;

                let reached_known_end = total_bytes.is_some_and(|value| received_bytes >= value);
                if received_bytes.saturating_sub(last_reported) >= 256 * 1024 || reached_known_end {
                    upsert_live_activity(
                        state,
                        activity_id,
                        LiveActivityKind::ConvertPreview,
                        LiveActivityStage::Receiving,
                        file_name.to_owned(),
                        None,
                        None,
                        received_bytes,
                        total_bytes,
                        receiving_activity_summary(
                            LiveActivityKind::ConvertPreview,
                            received_bytes,
                            total_bytes,
                        ),
                        Some(format!("已接收 {}", format_bytes(received_bytes))),
                    )
                    .await;
                    last_reported = received_bytes;
                }
            }
            Ok(Some(Err(error))) => {
                let detail = format!("failed to receive source stream: {error}");
                fail_raw_source_file(
                    state,
                    target_path,
                    activity_id,
                    file_name,
                    received_bytes,
                    total_bytes,
                    &detail,
                )
                .await;
                return Err(ApiError::bad_request(detail));
            }
            Ok(None) => break,
            Err(_) => {
                let detail = "upload stalled before the source file was fully received";
                fail_raw_source_file(
                    state,
                    target_path,
                    activity_id,
                    file_name,
                    received_bytes,
                    total_bytes,
                    detail,
                )
                .await;
                return Err(ApiError::bad_request(detail));
            }
        }
    }

    file.flush().await.map_err(|error| {
        ApiError::internal(format!("failed to flush local source file: {error}"))
    })?;

    if received_bytes == 0 {
        fail_raw_source_file(
            state,
            target_path,
            activity_id,
            file_name,
            received_bytes,
            total_bytes,
            "empty source upload",
        )
        .await;
        return Err(ApiError::bad_request("empty source upload"));
    }

    if let Some(expected) = total_bytes {
        if received_bytes != expected {
            let detail = format!(
                "incomplete source upload: received {} of {}",
                format_bytes(received_bytes),
                format_bytes(expected)
            );
            fail_raw_source_file(
                state,
                target_path,
                activity_id,
                file_name,
                received_bytes,
                total_bytes,
                &detail,
            )
            .await;
            return Err(ApiError::bad_request(detail));
        }
    }

    if received_bytes != last_reported {
        upsert_live_activity(
            state,
            activity_id,
            LiveActivityKind::ConvertPreview,
            LiveActivityStage::Receiving,
            file_name.to_owned(),
            None,
            None,
            received_bytes,
            total_bytes.or(Some(received_bytes)),
            receiving_activity_summary(
                LiveActivityKind::ConvertPreview,
                received_bytes,
                total_bytes.or(Some(received_bytes)),
            ),
            Some(format!("已接收 {}", format_bytes(received_bytes))),
        )
        .await;
    }

    Ok(received_bytes)
}

pub(crate) async fn fail_raw_source_file(
    state: &AppState,
    target_path: &Path,
    activity_id: &str,
    file_name: &str,
    received_bytes: u64,
    total_bytes: Option<u64>,
    detail: &str,
) {
    if let Err(error) = fs::remove_file(target_path).await {
        warn!(
            "failed to remove partial source upload {}: {error}",
            target_path.display()
        );
    }

    upsert_live_activity(
        state,
        activity_id,
        LiveActivityKind::ConvertPreview,
        LiveActivityStage::Failed,
        file_name.to_owned(),
        None,
        None,
        received_bytes,
        total_bytes,
        "Source upload failed.".to_owned(),
        Some(detail.to_owned()),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_field_bytes_with_progress(
    state: &AppState,
    mut field: axum::extract::multipart::Field<'_>,
    activity_id: &str,
    kind: LiveActivityKind,
    file_name: &str,
    user_name: Option<String>,
    printer: Option<PrinterMode>,
    total_bytes: Option<u64>,
    max_bytes: usize,
    oversize_message: &'static str,
    read_error_label: &'static str,
) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::new();
    let mut received_bytes = 0u64;
    let mut last_reported = 0u64;
    let mut consecutive_empty_reads = 0u64;
    let max_empty_reads = 4;

    upsert_live_activity(
        state,
        activity_id,
        kind,
        LiveActivityStage::Receiving,
        file_name.to_owned(),
        user_name.clone(),
        printer,
        0,
        total_bytes,
        receiving_activity_summary(kind, 0, total_bytes),
        None,
    )
    .await;

    loop {
        match tokio::time::timeout(Duration::from_secs(15), field.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                consecutive_empty_reads = 0;
                received_bytes += chunk.len() as u64;
                if received_bytes as usize > max_bytes {
                    upsert_live_activity(
                        state,
                        activity_id,
                        kind,
                        LiveActivityStage::Failed,
                        file_name.to_owned(),
                        user_name.clone(),
                        printer,
                        received_bytes,
                        total_bytes,
                        "接收的文件超出大小限制。".to_owned(),
                        Some(oversize_message.to_owned()),
                    )
                    .await;
                    return Err(ApiError::bad_request(oversize_message));
                }

                bytes.extend_from_slice(&chunk);

                let reached_known_end = total_bytes.is_some_and(|value| received_bytes >= value);
                if received_bytes.saturating_sub(last_reported) >= 256 * 1024 || reached_known_end {
                    upsert_live_activity(
                        state,
                        activity_id,
                        kind,
                        LiveActivityStage::Receiving,
                        file_name.to_owned(),
                        user_name.clone(),
                        printer,
                        received_bytes,
                        total_bytes,
                        receiving_activity_summary(kind, received_bytes, total_bytes),
                        Some(format!("已接收 {}", format_bytes(received_bytes))),
                    )
                    .await;
                    last_reported = received_bytes;

                    if reached_known_end && total_bytes.is_some_and(|v| received_bytes >= v) {
                        break;
                    }
                }
            }
            Ok(Ok(None)) => {
                break;
            }
            Ok(Err(error)) => {
                let detail = format!("{read_error_label}: {error}");
                upsert_live_activity(
                    state,
                    activity_id,
                    kind,
                    LiveActivityStage::Failed,
                    file_name.to_owned(),
                    user_name.clone(),
                    printer,
                    received_bytes,
                    total_bytes,
                    "Upload failed while receiving the file.".to_owned(),
                    Some(detail.clone()),
                )
                .await;
                return Err(ApiError::bad_request(detail));
            }
            Err(_) => {
                consecutive_empty_reads += 1;
                tracing::warn!(
                    "upload stalled while receiving multipart file ({}/{}), received {} bytes",
                    consecutive_empty_reads,
                    max_empty_reads,
                    received_bytes
                );
                if consecutive_empty_reads >= max_empty_reads {
                    let detail = "Upload stalled while receiving the file.";
                    upsert_live_activity(
                        state,
                        activity_id,
                        kind,
                        LiveActivityStage::Failed,
                        file_name.to_owned(),
                        user_name.clone(),
                        printer,
                        received_bytes,
                        total_bytes,
                        "Upload timed out.".to_owned(),
                        Some(detail.to_owned()),
                    )
                    .await;
                    return Err(ApiError::bad_request(detail));
                }
                if consecutive_empty_reads >= max_empty_reads {
                    return Err(ApiError::bad_request("接收超时，网络可能不稳定"));
                }
                if consecutive_empty_reads.is_multiple_of(10) {
                    tracing::warn!(
                        "接收文件超时重试（{}/{}），已接收 {} 字节",
                        consecutive_empty_reads,
                        max_empty_reads,
                        received_bytes
                    );
                }
            }
        }
    }

    if received_bytes != last_reported {
        upsert_live_activity(
            state,
            activity_id,
            kind,
            LiveActivityStage::Receiving,
            file_name.to_owned(),
            user_name,
            printer,
            received_bytes,
            total_bytes.or(Some(received_bytes)),
            receiving_activity_summary(kind, received_bytes, total_bytes.or(Some(received_bytes))),
            Some(format!("已接收 {}", format_bytes(received_bytes))),
        )
        .await;
    }

    Ok(bytes)
}

pub(crate) async fn cleanup_stale_prepare_uploads(storage_dir: &Path) {
    let mut entries = match fs::read_dir(storage_dir).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                "failed to scan prepare upload directory {}: {error}",
                storage_dir.display()
            );
            return;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        let is_prepare_file = file_name.starts_with("prepare-upload-")
            && (file_name.ends_with(".part")
                || file_name.ends_with(".manifest.json")
                || file_name.ends_with(".manifest.tmp"));
        let is_prepare_dir = file_name.starts_with("prepare-source-");
        if !is_prepare_file && !is_prepare_dir {
            continue;
        }

        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let Ok(modified_at) = metadata.modified() else {
            continue;
        };
        let Ok(age) = SystemTime::now().duration_since(modified_at) else {
            continue;
        };
        if age <= PREPARE_UPLOAD_TTL {
            continue;
        }

        let result = if metadata.is_dir() {
            fs::remove_dir_all(entry.path()).await
        } else {
            fs::remove_file(entry.path()).await
        };
        if let Err(error) = result {
            warn!(
                "failed to remove stale prepare upload {}: {error}",
                entry.path().display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(
        total_bytes: u64,
        chunk_size_bytes: u64,
        confirmed: &[u32],
    ) -> PrepareUploadManifest {
        PrepareUploadManifest {
            upload_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
            source_name: "doc.pdf".to_owned(),
            total_bytes,
            chunk_size_bytes,
            confirmed_chunks: confirmed.iter().copied().collect(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn confirmed_percent_rounds_and_caps() {
        assert_eq!(prepare_confirmed_percent(0, 0), 100);
        assert_eq!(prepare_confirmed_percent(50, 100), 50);
        assert_eq!(prepare_confirmed_percent(1, 3), 33);
        assert_eq!(prepare_confirmed_percent(999, 100), 100);
    }

    #[test]
    fn ws_chunk_size_power_of_two_in_range() {
        assert!(is_supported_prepare_ws_chunk_size(128 * 1024));
        assert!(is_supported_prepare_ws_chunk_size(256 * 1024));
        assert!(is_supported_prepare_ws_chunk_size(2 * 1024 * 1024));
        assert!(!is_supported_prepare_ws_chunk_size(64 * 1024));
        assert!(!is_supported_prepare_ws_chunk_size(4 * 1024 * 1024));
        assert!(!is_supported_prepare_ws_chunk_size(192 * 1024));
    }

    #[test]
    fn upload_id_validation() {
        assert!(validate_prepare_upload_id("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_prepare_upload_id("nope").is_err());
    }

    #[test]
    fn source_validation_rejects_bad_inputs() {
        assert!(validate_prepare_source(0, "a.pdf").is_err());
        assert!(validate_prepare_source((MAX_UPLOAD_SIZE_BYTES as u64) + 1, "a.pdf").is_err());
        assert!(validate_prepare_source(10, "noextension").is_err());
    }

    #[test]
    fn chunk_count_and_lengths() {
        let m = manifest(300, 128, &[]);
        assert_eq!(prepare_chunk_count(&m), 3);
        assert_eq!(expected_prepare_chunk_len(&m, 0), Some(128));
        assert_eq!(expected_prepare_chunk_len(&m, 2), Some(44));
        assert_eq!(expected_prepare_chunk_len(&m, 3), None);
    }

    #[test]
    fn manifest_progress_sums_confirmed_chunks() {
        let m = manifest(300, 128, &[0, 2]);
        let progress = manifest_progress(&m);
        assert_eq!(progress.received_bytes, 172);
        assert_eq!(progress.total_bytes, 300);
        assert_eq!(progress.percent, 57);
    }
}
