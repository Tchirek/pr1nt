use axum::http::{header, HeaderMap};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::ApiError;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DirectUploadKind {
    Prepare,
    PrepareWs,
    Preview,
    Print,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DirectUploadTokenPayload {
    pub(crate) kind: DirectUploadKind,
    #[serde(default)]
    pub(crate) upload_id: Option<String>,
    #[serde(default)]
    pub(crate) total_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) job_id: Option<String>,
    #[serde(default)]
    pub(crate) preview_id: Option<String>,
    #[serde(default)]
    pub(crate) page_count: Option<u32>,
    #[serde(default)]
    pub(crate) copy_count: Option<u32>,
    pub(crate) exp: u64,
}

pub(crate) fn verify_upload_auth(
    headers: &HeaderMap,
    shared_secret: &str,
    expected_kind: DirectUploadKind,
    expected_job_id: Option<&str>,
    expected_preview_id: Option<&str>,
) -> Result<DirectUploadTokenPayload, ApiError> {
    let token =
        extract_bearer(headers).ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
    if token == shared_secret {
        return Ok(DirectUploadTokenPayload {
            kind: expected_kind,
            upload_id: None,
            total_bytes: None,
            job_id: expected_job_id.map(ToOwned::to_owned),
            preview_id: expected_preview_id.map(ToOwned::to_owned),
            page_count: None,
            copy_count: None,
            exp: u64::MAX,
        });
    }

    verify_direct_upload_token(
        shared_secret,
        &token,
        expected_kind,
        expected_job_id,
        expected_preview_id,
    )
}

pub(crate) fn verify_direct_upload_token(
    shared_secret: &str,
    token: &str,
    expected_kind: DirectUploadKind,
    expected_job_id: Option<&str>,
    expected_preview_id: Option<&str>,
) -> Result<DirectUploadTokenPayload, ApiError> {
    let (encoded_payload, encoded_signature) = token
        .split_once('.')
        .ok_or_else(|| ApiError::unauthorized("invalid direct upload token format"))?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(encoded_payload)
        .map_err(|_| ApiError::unauthorized("invalid direct upload token payload"))?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| ApiError::unauthorized("invalid direct upload token signature"))?;

    let mut mac = Hmac::<Sha256>::new_from_slice(shared_secret.as_bytes())
        .map_err(|_| ApiError::unauthorized("invalid direct upload signing key"))?;
    mac.update(encoded_payload.as_bytes());
    mac.verify_slice(&signature_bytes)
        .map_err(|_| ApiError::unauthorized("direct upload token verification failed"))?;

    let payload = serde_json::from_slice::<DirectUploadTokenPayload>(&payload_bytes)
        .map_err(|_| ApiError::unauthorized("invalid direct upload token body"))?;

    if payload.kind != expected_kind {
        return Err(ApiError::unauthorized("direct upload token kind mismatch"));
    }

    if payload.exp < Utc::now().timestamp_millis() as u64 {
        return Err(ApiError::unauthorized("direct upload token expired"));
    }

    if let Some(expected_job_id) = expected_job_id {
        if payload.job_id.as_deref() != Some(expected_job_id) {
            return Err(ApiError::unauthorized(
                "direct upload token does not match job",
            ));
        }
    }

    if let Some(expected_preview_id) = expected_preview_id {
        if payload.preview_id.as_deref() != Some(expected_preview_id) {
            return Err(ApiError::unauthorized(
                "direct upload token does not match preview cache",
            ));
        }
    }

    Ok(payload)
}

pub(crate) fn verify_admin(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    let token = extract_bearer(headers)
        .or_else(|| {
            headers
                .get("x-admin-token")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| ApiError::unauthorized("missing admin token"))?;

    if token.trim() == expected.trim() {
        return Ok(());
    }
    Err(ApiError::unauthorized("invalid admin token"))
}

pub(crate) fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(ToOwned::to_owned)
}
