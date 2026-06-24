//! Small stateless helpers: input sanitizing, header parsing, PDF responses.
use crate::error::*;
use crate::model::*;
use crate::text::format_bytes;
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tokio::fs;
use tracing::warn;
use uuid::Uuid;

pub(crate) fn sanitize_filename(input: &str) -> String {
    let filtered = input
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        })
        .filter(|character| !character.is_control())
        .collect::<String>();

    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        "document.pdf".to_owned()
    } else if trimmed.to_ascii_lowercase().ends_with(".pdf") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}.pdf")
    }
}

pub(crate) fn sanitize_source_filename(input: &str) -> String {
    let filtered = input
        .chars()
        .filter(|character| {
            !matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        })
        .filter(|character| !character.is_control())
        .collect::<String>();

    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        "document".to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub(crate) fn parse_page_count(raw: &str) -> Result<u32, ApiError> {
    raw.trim()
        .parse::<u32>()
        .map_err(|_| ApiError::bad_request("page_count must be a positive integer"))
        .and_then(|value| {
            if value == 0 {
                Err(ApiError::bad_request(
                    "page_count must be greater than zero",
                ))
            } else {
                Ok(value)
            }
        })
}

pub(crate) fn parse_copy_count(raw: &str) -> Result<u32, ApiError> {
    raw.trim()
        .parse::<u32>()
        .map_err(|_| ApiError::bad_request("copy_count must be a positive integer"))
        .and_then(|value| {
            if value == 0 {
                Err(ApiError::bad_request(
                    "copy_count must be greater than zero",
                ))
            } else if value > MAX_COPY_COUNT {
                Err(ApiError::bad_request("copy_count is too large"))
            } else {
                Ok(value)
            }
        })
}

pub(crate) fn checked_total_print_pages(page_count: u32, copy_count: u32) -> Result<u32, ApiError> {
    let total = page_count
        .checked_mul(copy_count)
        .ok_or_else(|| ApiError::bad_request("print job is too large"))?;

    if total > MAX_TOTAL_PRINT_PAGES {
        return Err(ApiError::bad_request("print job is too large"));
    }

    Ok(total)
}

pub(crate) fn parse_optional_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn parse_optional_decoded_header(headers: &HeaderMap, name: &str) -> Option<String> {
    parse_optional_header(headers, name).and_then(|value| {
        urlencoding::decode(&value)
            .ok()
            .map(|decoded| decoded.trim().to_owned())
            .filter(|decoded| !decoded.is_empty())
    })
}

pub(crate) fn parse_optional_u64_header(headers: &HeaderMap, name: &str) -> Option<u64> {
    parse_optional_header(headers, name).and_then(|value| value.parse::<u64>().ok())
}

pub(crate) fn receiving_activity_summary(
    kind: LiveActivityKind,
    received_bytes: u64,
    total_bytes: Option<u64>,
) -> String {
    let prefix = match kind {
        LiveActivityKind::PrintUpload => "正在接收打印文件",
        LiveActivityKind::ConvertPreview => "正在接收待转换文档",
    };

    match total_bytes {
        Some(total) if total > 0 => {
            let percent = ((received_bytes as f64 / total as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u8;
            format!(
                "{prefix}（{} / {}，{}%）",
                format_bytes(received_bytes),
                format_bytes(total),
                percent
            )
        }
        _ => format!("{prefix}（{}）", format_bytes(received_bytes)),
    }
}

pub(crate) fn admin_cancelled_text() -> String {
    PrintFailure::Cancelled.detail()
}

pub(crate) async fn cache_preview_pdf(state: &AppState, bytes: &[u8]) -> Result<String, ApiError> {
    cleanup_stale_preview_cache(state.storage_dir.as_ref()).await;

    let cache_id = Uuid::new_v4().to_string();
    let cache_path = preview_cache_path(state.storage_dir.as_ref(), &cache_id);
    fs::write(&cache_path, bytes)
        .await
        .map_err(|error| ApiError::internal(format!("failed to cache preview PDF: {error}")))?;

    Ok(cache_id)
}

pub(crate) fn preview_cache_path(storage_dir: &Path, cache_id: &str) -> PathBuf {
    storage_dir.join(format!("preview-cache-{cache_id}.pdf"))
}

pub(crate) fn is_valid_preview_cache_id(value: &str) -> bool {
    Uuid::parse_str(value).is_ok()
}

pub(crate) async fn cleanup_stale_preview_cache(storage_dir: &Path) {
    const PREVIEW_CACHE_TTL: Duration = Duration::from_secs(2 * 60 * 60);

    let mut entries = match fs::read_dir(storage_dir).await {
        Ok(entries) => entries,
        Err(error) => {
            warn!(
                "failed to scan preview cache directory {}: {error}",
                storage_dir.display()
            );
            return;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with("preview-cache-") || !file_name.ends_with(".pdf") {
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
        if age <= PREVIEW_CACHE_TTL {
            continue;
        }

        if let Err(error) = fs::remove_file(entry.path()).await {
            warn!(
                "failed to remove stale preview cache {}: {error}",
                entry.path().display()
            );
        }
    }
}

pub(crate) fn pdf_response(
    bytes: Vec<u8>,
    file_name: &str,
    preview_cache_id: Option<&str>,
) -> Result<Response, ApiError> {
    let file_name_urlencoded = urlencoding::encode(file_name).to_string();
    let file_name_header = header::HeaderValue::from_str(&file_name_urlencoded)
        .map_err(|error| ApiError::internal(format!("invalid converted file name: {error}")))?;
    let content_length = header::HeaderValue::from(bytes.len());
    let preview_cache_header = preview_cache_id
        .map(header::HeaderValue::from_str)
        .transpose()
        .map_err(|error| ApiError::internal(format!("invalid preview cache id: {error}")))?;

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(header::CONTENT_LENGTH, content_length)
        .header("x-converted-filename", file_name_header);

    if let Some(preview_cache_header) = preview_cache_header {
        builder = builder.header("x-preview-cache-id", preview_cache_header);
    }

    builder
        .body(axum::body::Body::from(bytes))
        .map_err(|error| ApiError::internal(format!("failed to build PDF response: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;

    #[test]
    fn sanitize_filename_strips_illegal_and_forces_pdf() {
        assert_eq!(sanitize_filename("a/b:c*.txt"), "abc.txt.pdf");
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("REPORT.PDF"), "REPORT.PDF");
        assert_eq!(sanitize_filename("   "), "document.pdf");
    }

    #[test]
    fn sanitize_source_filename_keeps_extension() {
        assert_eq!(sanitize_source_filename("a/b.docx"), "ab.docx");
        assert_eq!(sanitize_source_filename("   "), "document");
    }

    #[test]
    fn parse_page_count_rejects_zero_and_garbage() {
        assert_eq!(parse_page_count(" 10 ").unwrap(), 10);
        assert!(matches!(
            parse_page_count("0"),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            parse_page_count("x"),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_copy_count_enforces_bounds() {
        assert_eq!(parse_copy_count("5").unwrap(), 5);
        assert!(parse_copy_count("0").is_err());
        assert!(parse_copy_count("6").is_err());
    }

    #[test]
    fn checked_total_caps_and_detects_overflow() {
        assert_eq!(checked_total_print_pages(30, 2).unwrap(), 60);
        assert!(checked_total_print_pages(10, 7).is_err());
        assert!(checked_total_print_pages(u32::MAX, 2).is_err());
    }

    #[test]
    fn preview_cache_id_validation() {
        assert!(is_valid_preview_cache_id(
            "550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!is_valid_preview_cache_id("not-a-uuid"));
    }
}
