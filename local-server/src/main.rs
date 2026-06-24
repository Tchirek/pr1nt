mod auth;
mod cloudflare_kv;
mod config;
mod conversion;
mod diagnostics;
mod error;
mod handlers;
mod model;
mod paths;
mod prepare;
mod print_sync_client;
mod printing;
mod sync;
mod text;
mod util;

use std::{collections::HashMap, env, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderName, Method},
    routing::{get, post},
    Router,
};
use tokio::{
    fs,
    sync::{broadcast, Mutex, RwLock},
};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

use crate::cloudflare_kv::CloudflareKvClient;
use crate::config::{
    configured_env, load_document_converter, require_configured_env, CachedConfig, PricesConfig,
    PrintersConfig, QRCodesConfig,
};
use crate::error::*;
use crate::handlers::*;
use crate::model::*;
use crate::paths::{resolve_command_path, resolve_relative_path};
use crate::prepare::*;
use crate::print_sync_client::PrintSyncClient;
use crate::printing::*;
use crate::sync::*;
use crate::util::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable_dir = env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(env::current_dir()?);
    let dotenv_path = executable_dir.join(".env");
    let dotenv_result = dotenvy::from_path_override(&dotenv_path);

    fmt()
        .with_env_filter(EnvFilter::from_env("RUST_LOG").add_directive("info".parse()?))
        .init();

    match dotenv_result {
        Ok(_) => info!("loaded environment from {}", dotenv_path.display()),
        Err(error) => warn!(
            "failed to load environment from {}: {}",
            dotenv_path.display(),
            error
        ),
    }

    let public_addr: SocketAddr = env::var("PUBLIC_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8788".to_owned())
        .parse()?;
    let admin_addr: SocketAddr = env::var("ADMIN_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8789".to_owned())
        .parse()?;
    let public_ws_url = format!("ws://127.0.0.1:{}/ws/status", public_addr.port());

    let shared_secret = require_configured_env("PRINT_SHARED_SECRET")?;
    let admin_token = require_configured_env("LOCAL_ADMIN_TOKEN")?;
    let sumatra_path = resolve_command_path(
        &executable_dir,
        &env::var("SUMATRA_PDF_PATH").unwrap_or_else(|_| "SumatraPDF".to_owned()),
    );
    let runtime_config_path = resolve_relative_path(
        &executable_dir,
        &env::var("LOCAL_RUNTIME_CONFIG_PATH")
            .unwrap_or_else(|_| "./runtime-config.json".to_owned()),
    );
    let document_converter = load_document_converter(&runtime_config_path)?;
    let storage_dir = resolve_relative_path(
        &executable_dir,
        &env::var("PRINT_STORAGE_DIR").unwrap_or_else(|_| "./print-spool".to_owned()),
    );
    let admin_static_dir = resolve_relative_path(
        &executable_dir,
        &env::var("ADMIN_STATIC_DIR").unwrap_or_else(|_| "../local-admin".to_owned()),
    );

    fs::create_dir_all(&storage_dir).await?;

    let initial_printers = PrintersConfig {
        bw: env::var("BW_PRINTER_NAME").unwrap_or_else(|_| "BlackAndWhitePrinter".to_owned()),
        color: env::var("COLOR_PRINTER_NAME").unwrap_or_else(|_| "ColorPrinter".to_owned()),
    };

    let initial_config = CachedConfig {
        prices: PricesConfig {
            bw_per_page: env::var("DEFAULT_BW_PRICE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0.0),
            color_per_page: env::var("DEFAULT_COLOR_PRICE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0.0),
        },
        qrcodes: QRCodesConfig {
            alipay_url: env::var("DEFAULT_ALIPAY_QR").unwrap_or_default(),
            wechat_url: env::var("DEFAULT_WECHAT_QR").unwrap_or_default(),
        },
        notice_markdown: env::var("DEFAULT_NOTICE_MARKDOWN").unwrap_or_default(),
        printers: initial_printers.clone(),
    };

    let cloudflare = match (
        configured_env("CLOUDFLARE_ACCOUNT_ID"),
        configured_env("CLOUDFLARE_KV_NAMESPACE_ID"),
        configured_env("CLOUDFLARE_API_TOKEN"),
    ) {
        (Ok(account_id), Ok(namespace_id), Ok(api_token)) => {
            Some(CloudflareKvClient::new(account_id, namespace_id, api_token))
        }
        _ => {
            warn!("Cloudflare KV credentials are not fully configured; remote config sync is disabled.");
            None
        }
    };

    let print_sync = match (
        configured_env("PRINT_WORKER_BASE_URL"),
        configured_env("PRINT_SYNC_SECRET"),
    ) {
        (Ok(base_url), Ok(secret)) => {
            let device_prefix = env::var("PRINT_SYNC_DEVICE_ID")
                .ok()
                .map(|value| sanitize_sync_device_id(&value))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "609-localserver".to_owned());
            let machine_id = env::var("PRINT_SYNC_INSTANCE_ID")
                .ok()
                .map(|value| sanitize_sync_device_id(&value))
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    env::var("COMPUTERNAME")
                        .ok()
                        .map(|value| sanitize_sync_device_id(&value))
                        .filter(|value| !value.is_empty())
                })
                .or_else(|| {
                    env::var("HOSTNAME")
                        .ok()
                        .map(|value| sanitize_sync_device_id(&value))
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or_else(|| "unknown-machine".to_owned());
            let device_id = sanitize_sync_device_id(&format!("{device_prefix}:{machine_id}"));
            info!("print sync device id: {device_id}");
            Some(PrintSyncClient::new(base_url, secret, device_id))
        }
        _ => {
            warn!("Print sync worker URL or secret is not configured; R2 pull sync is disabled.");
            None
        }
    };

    let (status_tx, _) = broadcast::channel(256);
    let state = AppState {
        shared_secret: Arc::new(shared_secret),
        admin_token: Arc::new(admin_token),
        sumatra_path: Arc::new(sumatra_path),
        public_ws_url: Arc::new(public_ws_url),
        document_converter: Arc::new(RwLock::new(document_converter)),
        runtime_config_path: Arc::new(runtime_config_path),
        storage_dir: Arc::new(storage_dir),
        admin_static_dir: Arc::new(admin_static_dir),
        printers: Arc::new(RwLock::new(initial_printers)),
        cached_config: Arc::new(RwLock::new(initial_config)),
        jobs: Arc::new(RwLock::new(HashMap::new())),
        activities: Arc::new(RwLock::new(HashMap::new())),
        active_queue: Arc::new(RwLock::new(Vec::new())),
        prepare_upload_locks: Arc::new(Mutex::new(HashMap::new())),
        sync_catchup_lock: Arc::new(Mutex::new(())),
        status_tx,
        bw_lock: Arc::new(Mutex::new(())),
        color_lock: Arc::new(Mutex::new(())),
        cloudflare,
        print_sync,
    };

    if let Err(error) = sync_cached_config(&state).await {
        warn!("failed to load Cloudflare KV config; local defaults will be used: {error}");
    }
    if let Err(error) = load_persisted_sync_jobs(&state).await {
        warn!("failed to load persisted print sync jobs: {error}");
    }

    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30 * 60));
        loop {
            interval.tick().await;
            cleanup_stale_preview_cache(cleanup_state.storage_dir.as_ref()).await;
            cleanup_stale_prepare_uploads(cleanup_state.storage_dir.as_ref()).await;
        }
    });

    if state.print_sync.is_some() {
        let sync_state = state.clone();
        tokio::spawn(async move {
            run_print_sync(sync_state).await;
        });
    }

    let legacy_tunnel_upload_enabled = env_flag_enabled("LEGACY_TUNNEL_UPLOAD_ENABLED");
    if legacy_tunnel_upload_enabled {
        warn!(
            "legacy tunnel upload routes are enabled; user document bytes may traverse the tunnel"
        );
    } else {
        info!("legacy tunnel upload routes are disabled");
    }

    let mut public_router = Router::new()
        .route("/api/jobs/:job_id/status", get(get_public_job_status))
        .route("/ws/status", get(ws_status));

    if legacy_tunnel_upload_enabled {
        public_router = public_router
            .route("/api/prepare/raw", post(post_prepare_raw))
            .route("/api/prepare/chunk/:upload_id", post(post_prepare_chunk))
            .route(
                "/api/prepare/complete/:upload_id",
                post(post_prepare_complete),
            )
            .route("/api/convert-preview", post(post_convert_preview))
            .route("/api/convert-preview/raw", post(post_convert_preview_raw))
            .route("/api/print", post(post_print))
            .route("/api/print/raw", post(post_print_raw))
            .route("/ws/prepare", get(ws_prepare));
    }

    let public_router = public_router
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_SIZE_BYTES))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                .allow_headers(Any)
                .expose_headers([
                    header::CONTENT_LENGTH,
                    HeaderName::from_static("x-converted-filename"),
                    HeaderName::from_static("x-preview-cache-id"),
                ]),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let admin_router = Router::new()
        .route(
            "/admin/config",
            get(get_admin_config).post(post_admin_config),
        )
        .route("/admin/diagnostics", get(get_admin_diagnostics))
        .route("/admin/diagnostics/ws-probe", post(post_admin_ws_probe))
        .route("/admin/live-activities", get(get_admin_live_activities))
        .route("/admin/printers", get(get_admin_printers))
        .route("/admin/jobs", get(get_admin_jobs))
        .route("/admin/jobs/:job_id/retry", post(retry_admin_job))
        .route("/admin/jobs/:job_id/cancel", post(cancel_admin_job))
        .nest_service(
            "/admin",
            ServeDir::new((*state.admin_static_dir).clone()).append_index_html_on_directories(true),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let public_listener = tokio::net::TcpListener::bind(public_addr).await?;
    let admin_listener = tokio::net::TcpListener::bind(admin_addr).await?;

    info!("public print server listening on {public_addr}");
    info!("local admin server listening on {admin_addr}");

    let public_server = axum::serve(public_listener, public_router);
    let admin_server = axum::serve(admin_listener, admin_router);

    tokio::try_join!(public_server, admin_server)?;
    Ok(())
}
