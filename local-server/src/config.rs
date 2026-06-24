use std::{env, path::Path, process::Stdio};

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::ApiError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DocumentConverterKind {
    LibreOffice,
    Wps,
    Office,
}

impl DocumentConverterKind {
    pub(crate) fn as_label(self) -> &'static str {
        match self {
            Self::LibreOffice => "LibreOffice",
            Self::Wps => "WPS Office",
            Self::Office => "Microsoft Office",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DocumentConverterConfig {
    pub(crate) kind: DocumentConverterKind,
    pub(crate) libreoffice_path: String,
}

impl Default for DocumentConverterConfig {
    fn default() -> Self {
        Self {
            kind: DocumentConverterKind::LibreOffice,
            libreoffice_path: "soffice".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalRuntimeConfig {
    document_converter: DocumentConverterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PricesConfig {
    pub(crate) bw_per_page: f64,
    pub(crate) color_per_page: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QRCodesConfig {
    pub(crate) alipay_url: String,
    pub(crate) wechat_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PrintersConfig {
    pub(crate) bw: String,
    pub(crate) color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedConfig {
    pub(crate) prices: PricesConfig,
    pub(crate) qrcodes: QRCodesConfig,
    pub(crate) notice_markdown: String,
    pub(crate) printers: PrintersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AdminConfigResponse {
    pub(crate) prices: PricesConfig,
    pub(crate) qrcodes: QRCodesConfig,
    pub(crate) notice_markdown: String,
    pub(crate) printers: PrintersConfig,
    pub(crate) document_converter: DocumentConverterConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AdminConfigUpdate {
    pub(crate) prices: Option<PricesConfig>,
    pub(crate) qrcodes: Option<QRCodesConfig>,
    pub(crate) printers: Option<PrintersConfig>,
    pub(crate) notice_markdown: Option<String>,
    pub(crate) document_converter: Option<DocumentConverterConfig>,
}

pub(crate) fn require_configured_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    configured_env(name).map_err(|_| {
        format!(
            "missing or placeholder environment variable: {name}. Edit local-server/.env in the deployment folder before starting the server."
        )
        .into()
    })
}

pub(crate) fn configured_env(name: &str) -> Result<String, env::VarError> {
    let value = env::var(name)?;
    let trimmed = value.trim();
    if trimmed.is_empty() || is_placeholder_env_value(trimmed) {
        return Err(env::VarError::NotPresent);
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn load_document_converter(
    runtime_config_path: &Path,
) -> Result<DocumentConverterConfig, Box<dyn std::error::Error>> {
    let env_default = document_converter_from_env()?;
    if !runtime_config_path.exists() {
        return Ok(env_default);
    }

    let content = std::fs::read_to_string(runtime_config_path)?;
    let runtime_config = serde_json::from_str::<LocalRuntimeConfig>(&content)?;
    normalize_document_converter_config(runtime_config.document_converter)
        .map_err(|error| error.into())
}

pub(crate) fn document_converter_from_env(
) -> Result<DocumentConverterConfig, Box<dyn std::error::Error>> {
    let libreoffice_path = env::var("LIBREOFFICE_PATH").unwrap_or_else(|_| "soffice".to_owned());
    let kind = match env::var("DOCUMENT_CONVERTER")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    {
        None => detect_default_document_converter(),
        Some(value) => match value.as_str() {
            "auto" => detect_default_document_converter(),
            "libreoffice" => DocumentConverterKind::LibreOffice,
            "wps" => DocumentConverterKind::Wps,
            "office" => DocumentConverterKind::Office,
            value => return Err(format!("unsupported DOCUMENT_CONVERTER value: {value}").into()),
        },
    };

    normalize_document_converter_config(DocumentConverterConfig {
        kind,
        libreoffice_path,
    })
    .map_err(|error| error.into())
}

pub(crate) fn detect_default_document_converter() -> DocumentConverterKind {
    if com_prog_id_registered("Word.Application") {
        return DocumentConverterKind::Office;
    }

    if com_prog_id_registered("kwps.Application") {
        return DocumentConverterKind::Wps;
    }

    DocumentConverterKind::LibreOffice
}

pub(crate) fn normalize_document_converter_config(
    mut config: DocumentConverterConfig,
) -> Result<DocumentConverterConfig, String> {
    config.libreoffice_path = config.libreoffice_path.trim().to_owned();
    if config.libreoffice_path.is_empty() {
        config.libreoffice_path = "soffice".to_owned();
    }

    Ok(config)
}

pub(crate) async fn persist_runtime_config(
    runtime_config_path: &Path,
    document_converter: &DocumentConverterConfig,
) -> Result<(), ApiError> {
    let payload = serde_json::to_vec_pretty(&LocalRuntimeConfig {
        document_converter: document_converter.clone(),
    })
    .map_err(|error| {
        ApiError::internal(format!("failed to encode local runtime config: {error}"))
    })?;

    if let Some(parent) = runtime_config_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await.map_err(|error| {
                ApiError::internal(format!(
                    "failed to create runtime config directory: {error}"
                ))
            })?;
        }
    }

    fs::write(runtime_config_path, payload)
        .await
        .map_err(|error| {
            ApiError::internal(format!("failed to persist local runtime config: {error}"))
        })
}

fn is_placeholder_env_value(value: &str) -> bool {
    value.trim().starts_with("replace-with-")
}

fn com_prog_id_registered(prog_id: &str) -> bool {
    let key = format!(r"HKCR\{}\CLSID", prog_id);
    std::process::Command::new("reg")
        .args(["query", &key])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
