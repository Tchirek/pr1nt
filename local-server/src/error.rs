//! API error types and their mapping to HTTP responses.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Serialize)]
pub(crate) struct ErrorBody {
    pub(crate) error: String,
}

#[derive(Debug, Error)]
pub(crate) enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub(crate) fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub(crate) fn upstream(message: impl Into<String>) -> Self {
        Self::Upstream(message.into())
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Json(ErrorBody {
            error: self.to_string(),
        });
        (status, body).into_response()
    }
}

#[derive(Debug, Error)]
pub(crate) enum PrintFailure {
    #[error("\u{6253}\u{5370}\u{673a}\u{79bb}\u{7ebf}\u{6216}\u{4e0d}\u{53ef}\u{7528}\u{3002}")]
    PrinterOffline,
    #[error("\u{6253}\u{5370}\u{673a}\u{7f3a}\u{7eb8}\u{ff0c}\u{8bf7}\u{8054}\u{7cfb}\u{7ba1}\u{7406}\u{5458}\u{8865}\u{7eb8}\u{3002}")]
    OutOfPaper,
    #[error("PDF \u{6587}\u{4ef6}\u{635f}\u{574f}\u{6216}\u{65e0}\u{6cd5}\u{88ab} SumatraPDF \u{8bfb}\u{53d6}\u{3002}")]
    FileCorrupt,
    #[error("\u{6253}\u{5370}\u{8d85}\u{65f6}\u{ff0c}\u{4efb}\u{52a1}\u{5df2}\u{81ea}\u{52a8}\u{505c}\u{6b62}\u{3002}")]
    Timeout,
    #[error("\u{4efb}\u{52a1}\u{5df2}\u{88ab}\u{7ba1}\u{7406}\u{5458}\u{53d6}\u{6d88}\u{3002}")]
    Cancelled,
    #[error("{0}")]
    Unknown(String),
}

#[derive(Debug, Error)]
#[allow(dead_code)]
pub(crate) enum ConversionFailure {
    #[error("未配置文档转换器，请在部署机上启用 LibreOffice。")]
    NotConfigured,
    #[error("当前文件类型不支持自动转换。")]
    UnsupportedType,
    #[error("文档转换失败：{0}")]
    CommandFailed(String),
}

impl PrintFailure {
    pub(crate) fn detail(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(ApiError::bad_request("x").status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            ApiError::unauthorized("x").status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(ApiError::conflict("x").status(), StatusCode::CONFLICT);
        assert_eq!(ApiError::not_found("x").status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::upstream("x").status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            ApiError::internal("x").status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn message_is_preserved() {
        assert_eq!(ApiError::bad_request("nope").to_string(), "nope");
    }
}
