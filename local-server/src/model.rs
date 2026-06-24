//! Shared application state and serializable domain types.
use crate::cloudflare_kv::CloudflareKvClient;
use crate::config::{CachedConfig, DocumentConverterConfig, PrintersConfig};
use crate::error::*;
use crate::print_sync_client::PrintSyncClient;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{broadcast, Mutex, RwLock};

pub(crate) const MAX_COPY_COUNT: u32 = 5;
pub(crate) const MAX_TOTAL_PRINT_PAGES: u32 = 60;
pub(crate) const MAX_UPLOAD_SIZE_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_HTTP_BODY_SIZE_BYTES: usize = MAX_UPLOAD_SIZE_BYTES + 8 * 1024 * 1024;
pub(crate) const PREPARE_WS_MIN_CHUNK_SIZE_BYTES: u64 = 128 * 1024;
pub(crate) const PREPARE_WS_MAX_CHUNK_SIZE_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const PREPARE_UPLOAD_TTL: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) shared_secret: Arc<String>,
    pub(crate) admin_token: Arc<String>,
    pub(crate) sumatra_path: Arc<String>,
    pub(crate) public_ws_url: Arc<String>,
    pub(crate) document_converter: Arc<RwLock<DocumentConverterConfig>>,
    pub(crate) runtime_config_path: Arc<PathBuf>,
    pub(crate) storage_dir: Arc<PathBuf>,
    pub(crate) admin_static_dir: Arc<PathBuf>,
    pub(crate) printers: Arc<RwLock<PrintersConfig>>,
    pub(crate) cached_config: Arc<RwLock<CachedConfig>>,
    pub(crate) jobs: Arc<RwLock<HashMap<String, LocalJobRecord>>>,
    pub(crate) activities: Arc<RwLock<HashMap<String, LiveActivityRecord>>>,
    pub(crate) active_queue: Arc<RwLock<Vec<String>>>,
    pub(crate) prepare_upload_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    pub(crate) sync_catchup_lock: Arc<Mutex<()>>,
    pub(crate) status_tx: broadcast::Sender<StatusEvent>,
    pub(crate) bw_lock: Arc<Mutex<()>>,
    pub(crate) color_lock: Arc<Mutex<()>>,
    pub(crate) cloudflare: Option<CloudflareKvClient>,
    pub(crate) print_sync: Option<PrintSyncClient>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PrinterMode {
    Bw,
    Color,
}

impl PrinterMode {
    pub(crate) fn parse(raw: &str) -> Result<Self, ApiError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bw" => Ok(Self::Bw),
            "color" => Ok(Self::Color),
            _ => Err(ApiError::bad_request("printer must be `bw` or `color`")),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Bw => "bw",
            Self::Color => "color",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JobStatus {
    Queued,
    Downloading,
    Printing,
    Done,
    Failed,
}

impl JobStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StatusStreamKind {
    Job,
    Activity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveActivityKind {
    PrintUpload,
    ConvertPreview,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveActivityStage {
    Receiving,
    Received,
    Converting,
    Ready,
    Failed,
}

impl LiveActivityStage {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LiveActivityRecord {
    pub(crate) id: String,
    pub(crate) kind: LiveActivityKind,
    pub(crate) stage: LiveActivityStage,
    pub(crate) file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) user_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) printer: Option<PrinterMode>,
    pub(crate) received_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) percent: Option<u8>,
    pub(crate) summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PreparedPrintResponse {
    pub(crate) prepared_id: String,
    pub(crate) page_count: u32,
    pub(crate) file_name: String,
    pub(crate) source_name: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PrepareChunkResponse {
    pub(crate) upload_id: String,
    pub(crate) received_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PrepareUploadManifest {
    pub(crate) upload_id: String,
    pub(crate) source_name: String,
    pub(crate) total_bytes: u64,
    pub(crate) chunk_size_bytes: u64,
    pub(crate) confirmed_chunks: BTreeSet<u32>,
    pub(crate) updated_at: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PrepareUploadProgress {
    pub(crate) received_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) percent: u8,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PrepareWsClientMessage {
    Hello {
        token: String,
        upload_id: String,
        total_bytes: u64,
        source_name: String,
        chunk_size_bytes: u64,
    },
    Complete,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PrepareWsServerMessage {
    Ready {
        upload_id: String,
        confirmed_chunks: Vec<u32>,
        received_bytes: u64,
        total_bytes: u64,
        percent: u8,
    },
    Ack {
        upload_id: String,
        chunk_index: u32,
        received_bytes: u64,
        total_bytes: u64,
        percent: u8,
    },
    Processing {
        upload_id: String,
        received_bytes: u64,
        total_bytes: u64,
        percent: u8,
    },
    Prepared {
        prepared_id: String,
        page_count: u32,
        file_name: String,
        source_name: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QueueJobRecord {
    pub(crate) id: String,
    pub(crate) user_name: String,
    pub(crate) file_name: String,
    pub(crate) page_count: u32,
    #[serde(default = "default_copy_count")]
    pub(crate) copy_count: u32,
    pub(crate) color_mode: String,
    pub(crate) status: JobStatus,
    pub(crate) submitted_at: String,
    pub(crate) detail: Option<String>,
    pub(crate) pages_printed: Option<u32>,
    pub(crate) total_pages: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalJobRecord {
    #[serde(flatten)]
    pub(crate) job: QueueJobRecord,
    pub(crate) printer: PrinterMode,
    pub(crate) printer_name: String,
    pub(crate) pdf_path: String,
    pub(crate) attempts: u32,
    pub(crate) updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) document_id: Option<String>,
    #[serde(default)]
    pub(crate) sync_managed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PendingPrintSyncResponse {
    #[serde(default)]
    pub(crate) documents: Vec<PendingDocumentWork>,
    #[serde(default)]
    pub(crate) jobs: Vec<PendingPrintJob>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PendingDocumentWork {
    pub(crate) id: String,
    pub(crate) source_name: String,
    pub(crate) mime_type: String,
    pub(crate) declared_size: u64,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PendingPrintJob {
    pub(crate) id: String,
    pub(crate) document_id: String,
    pub(crate) status: JobStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaimedDocumentWork {
    pub(crate) id: String,
    pub(crate) source_name: String,
    pub(crate) mime_type: String,
    pub(crate) declared_size: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaimedPrintJob {
    pub(crate) id: String,
    pub(crate) document_id: String,
    pub(crate) user_name: String,
    pub(crate) file_name: String,
    pub(crate) page_count: u32,
    pub(crate) copy_count: u32,
    pub(crate) color_mode: String,
    pub(crate) total_pages: u32,
}

#[derive(Debug, Serialize)]
pub(crate) struct PrintSyncClaimRequest<'a> {
    pub(crate) kind: &'a str,
    pub(crate) id: &'a str,
    pub(crate) recover_ready: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PrintSyncHeartbeatRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) phase: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PrintSyncConfirmRequest<'a> {
    pub(crate) document_id: &'a str,
    pub(crate) sha256: &'a str,
    pub(crate) size_bytes: u64,
    pub(crate) page_count: u32,
    pub(crate) file_name: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct PrintSyncFailRequest<'a> {
    pub(crate) document_id: &'a str,
    pub(crate) error: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct PrintSyncJobStatusRequest<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pages_printed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_pages: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PrintSyncOkResponse {
    #[allow(dead_code)]
    pub(crate) ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusEvent {
    pub(crate) kind: StatusStreamKind,
    pub(crate) job_id: String,
    pub(crate) status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pages_printed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_pages: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) activity: Option<LiveActivityRecord>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AdminJobsResponse {
    pub(crate) active: Vec<LocalJobRecord>,
    pub(crate) history: Vec<LocalJobRecord>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiagnosticStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticCheck {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) status: DiagnosticStatus,
    pub(crate) summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AdminDiagnosticsSummary {
    pub(crate) passed: usize,
    pub(crate) warned: usize,
    pub(crate) failed: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AdminDiagnosticsResponse {
    pub(crate) generated_at: String,
    pub(crate) summary: AdminDiagnosticsSummary,
    pub(crate) local_ws_url: String,
    pub(crate) checks: Vec<DiagnosticCheck>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AdminWsProbeResponse {
    pub(crate) job_id: String,
    pub(crate) status: JobStatus,
    pub(crate) detail: String,
    pub(crate) ws_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WindowsPrintJobProbe {
    #[serde(rename = "DocumentName")]
    pub(crate) document_name: Option<String>,
    #[serde(rename = "PagesPrinted")]
    pub(crate) pages_printed: Option<u32>,
    #[serde(rename = "TotalPages")]
    pub(crate) total_pages: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AcceptedPrintResponse {
    pub(crate) accepted: bool,
    pub(crate) job_id: String,
}

pub(crate) fn default_copy_count() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_mode_parses_case_insensitively() {
        assert_eq!(PrinterMode::parse("bw").unwrap(), PrinterMode::Bw);
        assert_eq!(PrinterMode::parse(" COLOR ").unwrap(), PrinterMode::Color);
        assert!(PrinterMode::parse("duplex").is_err());
    }

    #[test]
    fn printer_mode_as_str() {
        assert_eq!(PrinterMode::Bw.as_str(), "bw");
        assert_eq!(PrinterMode::Color.as_str(), "color");
    }

    #[test]
    fn job_status_terminality() {
        assert!(JobStatus::Done.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(!JobStatus::Queued.is_terminal());
        assert!(!JobStatus::Printing.is_terminal());
    }

    #[test]
    fn activity_stage_terminality() {
        assert!(LiveActivityStage::Ready.is_terminal());
        assert!(LiveActivityStage::Failed.is_terminal());
        assert!(!LiveActivityStage::Receiving.is_terminal());
    }
}
