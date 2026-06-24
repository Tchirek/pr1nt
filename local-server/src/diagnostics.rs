//! Admin diagnostics for printers, storage, converters, and Cloudflare sync.
use crate::config::DocumentConverterKind;
use crate::error::*;
use crate::model::*;
use crate::printing::*;
use crate::text::now_iso;
use std::path::Path;
use tokio::{fs, process::Command};
use tracing::warn;
use uuid::Uuid;

pub(crate) async fn run_admin_diagnostics(state: &AppState) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();

    checks.push(check_admin_static_assets(state).await);
    checks.push(check_storage_directory(state).await);
    checks.push(check_sumatra_binary(state).await);

    match list_available_printers().await {
        Ok(printers) => {
            checks.push(diagnostic_check(
                "printers-discovered",
                "打印机枚举",
                DiagnosticStatus::Pass,
                format!("已发现 {} 台 Windows 打印机。", printers.len()),
                Some(printers.join("\n")),
                None,
            ));
            checks.push(check_configured_printers(state, &printers).await);
        }
        Err(error) => {
            checks.push(diagnostic_check(
                "printers-discovered",
                "打印机枚举",
                DiagnosticStatus::Fail,
                "无法读取 Windows 打印机列表。",
                Some(error.to_string()),
                Some(
                    "确认打印服务有权限执行 `wmic printer get name`，并检查打印机驱动是否正常。"
                        .to_owned(),
                ),
            ));
        }
    }

    checks.push(check_document_converter(state).await);
    checks.push(check_cloudflare_kv_sync(state).await);

    checks
}

pub(crate) fn summarize_diagnostics(checks: &[DiagnosticCheck]) -> AdminDiagnosticsSummary {
    let mut summary = AdminDiagnosticsSummary {
        passed: 0,
        warned: 0,
        failed: 0,
    };

    for check in checks {
        match check.status {
            DiagnosticStatus::Pass => summary.passed += 1,
            DiagnosticStatus::Warn => summary.warned += 1,
            DiagnosticStatus::Fail => summary.failed += 1,
        }
    }

    summary
}

pub(crate) fn diagnostic_check(
    id: impl Into<String>,
    label: impl Into<String>,
    status: DiagnosticStatus,
    summary: impl Into<String>,
    detail: Option<String>,
    hint: Option<String>,
) -> DiagnosticCheck {
    DiagnosticCheck {
        id: id.into(),
        label: label.into(),
        status,
        summary: summary.into(),
        detail,
        hint,
    }
}

pub(crate) async fn check_admin_static_assets(state: &AppState) -> DiagnosticCheck {
    let base_dir = state.admin_static_dir.as_ref();
    let index_path = base_dir.join("index.html");
    let app_path = base_dir.join("app.js");
    let styles_path = base_dir.join("styles.css");

    let missing = [
        index_path.as_path(),
        app_path.as_path(),
        styles_path.as_path(),
    ]
    .into_iter()
    .filter(|path| !path.exists())
    .map(|path| path.display().to_string())
    .collect::<Vec<_>>();

    if missing.is_empty() {
        return diagnostic_check(
            "admin-assets",
            "本地后台静态资源",
            DiagnosticStatus::Pass,
            "本地后台页面资源完整。",
            Some(base_dir.display().to_string()),
            None,
        );
    }

    diagnostic_check(
        "admin-assets",
        "本地后台静态资源",
        DiagnosticStatus::Fail,
        "后台页面文件不完整，/admin 可能无法正常打开。",
        Some(missing.join("\n")),
        Some("确认部署包中同时包含 local-admin 目录，并与 local-server 目录保持同级。".to_owned()),
    )
}

pub(crate) async fn check_storage_directory(state: &AppState) -> DiagnosticCheck {
    let storage_dir = state.storage_dir.as_ref();
    let probe_path = storage_dir.join(format!(".diagnostic-write-{}", Uuid::new_v4()));

    match fs::create_dir_all(storage_dir).await {
        Ok(()) => {}
        Err(error) => {
            return diagnostic_check(
                "storage-dir",
                "打印暂存目录",
                DiagnosticStatus::Fail,
                "无法创建打印暂存目录。",
                Some(format!("{}\n{}", storage_dir.display(), error)),
                Some("检查 PRINT_STORAGE_DIR 路径是否可写。".to_owned()),
            );
        }
    }

    match fs::write(&probe_path, b"609-diagnostic").await {
        Ok(()) => {
            let _ = fs::remove_file(&probe_path).await;
            diagnostic_check(
                "storage-dir",
                "打印暂存目录",
                DiagnosticStatus::Pass,
                "打印暂存目录可写。",
                Some(storage_dir.display().to_string()),
                None,
            )
        }
        Err(error) => diagnostic_check(
            "storage-dir",
            "打印暂存目录",
            DiagnosticStatus::Fail,
            "打印暂存目录不可写，上传和转换会失败。",
            Some(format!("{}\n{}", storage_dir.display(), error)),
            Some("检查磁盘权限、防病毒软件拦截，或将 PRINT_STORAGE_DIR 改到可写目录。".to_owned()),
        ),
    }
}

pub(crate) async fn check_sumatra_binary(state: &AppState) -> DiagnosticCheck {
    let configured = state.sumatra_path.as_ref().clone();

    match resolve_command_location(&configured).await {
        Ok(location) => diagnostic_check(
            "sumatra",
            "SumatraPDF 可执行文件",
            DiagnosticStatus::Pass,
            "已找到 SumatraPDF，可用于实际打印。",
            Some(location),
            None,
        ),
        Err(detail) => diagnostic_check(
            "sumatra",
            "SumatraPDF 可执行文件",
            DiagnosticStatus::Fail,
            "未找到 SumatraPDF，可预览但无法出纸。",
            Some(detail),
            Some(
                "检查 SUMATRA_PDF_PATH，或确认部署包内的 bin\\SumatraPDF.exe 仍然存在。".to_owned(),
            ),
        ),
    }
}

pub(crate) async fn check_configured_printers(
    state: &AppState,
    printers: &[String],
) -> DiagnosticCheck {
    let configured = state.printers.read().await.clone();
    let mut missing = Vec::new();
    let mut resolved = Vec::new();

    match resolve_printer_alias(&configured.bw, printers) {
        Some(actual) => resolved.push(format!("黑白：{} -> {}", configured.bw, actual)),
        None => missing.push(format!("黑白打印机未找到：{}", configured.bw)),
    }

    match resolve_printer_alias(&configured.color, printers) {
        Some(actual) => resolved.push(format!("彩色：{} -> {}", configured.color, actual)),
        None => missing.push(format!("彩色打印机未找到：{}", configured.color)),
    }

    if missing.is_empty() {
        return diagnostic_check(
            "configured-printers",
            "已配置打印机匹配",
            DiagnosticStatus::Pass,
            "黑白和彩色打印机都能在系统中找到，短名会自动解析为实际 Windows 名称。",
            Some(resolved.join("\n")),
            None,
        );
    }

    diagnostic_check(
        "configured-printers",
        "已配置打印机匹配",
        DiagnosticStatus::Fail,
        "当前配置的打印机名称无法解析到 Windows 实际打印机列表。",
        Some(missing.join("\n")),
        Some(
            "检查打印机是否在线，或在后台选择一个能在 Windows 打印机列表中找到的名称。".to_owned(),
        ),
    )
}

pub(crate) async fn check_document_converter(state: &AppState) -> DiagnosticCheck {
    let converter = state.document_converter.read().await.clone();

    match converter.kind {
        DocumentConverterKind::LibreOffice => {
            let output = Command::new(&converter.libreoffice_path)
                .args(["--version"])
                .output()
                .await;

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

                    let mut details = vec![format!("路径: {}", converter.libreoffice_path)];
                    if !stdout.is_empty() {
                        details.push(format!("版本: {}", stdout));
                    }
                    if !stderr.is_empty() {
                        details.push(format!("提示: {}", stderr));
                    }

                    diagnostic_check(
                        "converter",
                        "文档转 PDF 引擎",
                        DiagnosticStatus::Pass,
                        "LibreOffice 可正常启动。",
                        Some(details.join("\n")),
                        None,
                    )
                }
                Ok(output) => diagnostic_check(
                    "converter",
                    "文档转 PDF 引擎",
                    DiagnosticStatus::Fail,
                    "LibreOffice 启动失败，非 PDF 文档无法转换。",
                    Some(format!(
                        "路径: {}\n{}",
                        converter.libreoffice_path,
                        compose_process_error(&output)
                    )),
                    Some("检查 LibreOffice 路径是否正确，或在后台将转换器切换到 WPS / Microsoft Office。".to_owned()),
                ),
                Err(error) => diagnostic_check(
                    "converter",
                    "文档转 PDF 引擎",
                    DiagnosticStatus::Fail,
                    "无法启动 LibreOffice，非 PDF 文档无法转换。",
                    Some(format!("路径: {}\n错误: {}", converter.libreoffice_path, error)),
                    Some("如果部署机没有 LibreOffice，请在后台切换到 WPS 或 Microsoft Office。".to_owned()),
                ),
            }
        }
        DocumentConverterKind::Wps => {
            check_com_converter_suite(
                "WPS Office",
                &[
                    ("word", "kwps.Application"),
                    ("excel", "ket.Application"),
                    ("powerpoint", "kwpp.Application"),
                ],
            )
            .await
        }
        DocumentConverterKind::Office => {
            check_com_converter_suite(
                "Microsoft Office",
                &[
                    ("word", "Word.Application"),
                    ("excel", "Excel.Application"),
                    ("powerpoint", "PowerPoint.Application"),
                ],
            )
            .await
        }
    }
}

pub(crate) async fn check_com_converter_suite(
    suite_name: &str,
    components: &[(&str, &str)],
) -> DiagnosticCheck {
    let mut ready = Vec::new();
    let mut failed = Vec::new();
    let mut warnings = Vec::new();

    for (mode, prog_id) in components {
        match probe_com_prog_id_detailed(prog_id, mode).await {
            Ok((status, detail)) => {
                ready.push(format!("{mode}: {detail}"));
                if !status.is_empty() {
                    warnings.push(format!("{mode} 信息: {status}"));
                }
            }
            Err(detail) => failed.push(format!("{mode}: {detail}")),
        }
    }

    if failed.is_empty() {
        let (status, summary) = if warnings.is_empty() {
            (
                DiagnosticStatus::Pass,
                format!("{suite_name} 的 COM 自动化组件可用。"),
            )
        } else {
            (
                DiagnosticStatus::Pass,
                format!("{suite_name} 的 COM 自动化组件可用（有附加信息）。"),
            )
        };

        let mut details = ready;
        details.extend(warnings);

        return diagnostic_check(
            "converter",
            "文档转 PDF 引擎",
            status,
            summary,
            Some(details.join("\n")),
            None,
        );
    }

    let mut all_details = failed;
    all_details.extend(warnings);

    diagnostic_check(
        "converter",
        "文档转 PDF 引擎",
        DiagnosticStatus::Fail,
        format!("{suite_name} 的部分 COM 组件不可用，相关文档类型转换会失败。"),
        Some(all_details.join("\n")),
        Some("重新安装对应办公套件，或切换到 LibreOffice 转换器。".to_owned()),
    )
}

pub(crate) async fn probe_com_prog_id_detailed(
    prog_id: &str,
    _mode: &str,
) -> Result<(String, String), String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $app = New-Object -ComObject '{prog_id}'; \
         $version = ''; \
         try {{ \
             if ($null -ne $app.Version) {{ $version = $app.Version }} \
             elseif ($null -ne $app.ProductCode) {{ $version = $app.ProductCode }} \
             else {{ $version = 'available' }} \
         }} catch {{ $version = 'available' }}; \
         try {{ $app.Quit() }} catch {{}}; \
         $version"
    );

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .await
        .map_err(|error| error.to_string())?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let version = if stdout.is_empty() {
            "可启动".to_string()
        } else {
            format!("可启动（版本: {stdout}）")
        };
        return Ok((stdout, version));
    }

    Err(compose_process_error(&output))
}

pub(crate) async fn check_cloudflare_kv_sync(state: &AppState) -> DiagnosticCheck {
    let Some(cloudflare) = &state.cloudflare else {
        return diagnostic_check(
            "cloudflare-kv",
            "Cloudflare KV 同步",
            DiagnosticStatus::Warn,
            "未配置 Cloudflare KV 同步凭据。",
            Some(
                "缺少 CLOUDFLARE_ACCOUNT_ID / CLOUDFLARE_KV_NAMESPACE_ID / CLOUDFLARE_API_TOKEN"
                    .to_owned(),
            ),
            Some("未配置时，本地后台改价和队列状态不会同步到线上站点。".to_owned()),
        );
    };

    let probe_key = format!("diagnostics:ping:{}", Uuid::new_v4());
    let probe_value = format!("609 diagnostic {}", now_iso());

    let result = async {
        cloudflare.put_text(&probe_key, &probe_value).await?;
        let read_back = cloudflare.get_text(&probe_key).await?;
        if read_back.as_deref() != Some(probe_value.as_str()) {
            return Err(ApiError::upstream(
                "Cloudflare KV round-trip value mismatch",
            ));
        }
        cloudflare.delete_text(&probe_key).await?;
        Ok::<(), ApiError>(())
    }
    .await;

    if let Err(error) = cloudflare.delete_text(&probe_key).await {
        warn!("failed to clean up diagnostic Cloudflare KV key: {error}");
    }

    match result {
        Ok(()) => diagnostic_check(
            "cloudflare-kv",
            "Cloudflare KV 同步",
            DiagnosticStatus::Pass,
            "Cloudflare KV 读写往返正常。",
            Some(format!("namespace_id={}", cloudflare.namespace_id())),
            None,
        ),
        Err(error) => diagnostic_check(
            "cloudflare-kv",
            "Cloudflare KV 同步",
            DiagnosticStatus::Fail,
            "Cloudflare KV 已配置，但读写测试失败。",
            Some(error.to_string()),
            Some(
                "检查 API Token 权限、Account ID、KV Namespace ID，以及部署机的网络连通性。"
                    .to_owned(),
            ),
        ),
    }
}

pub(crate) async fn resolve_command_location(configured: &str) -> Result<String, String> {
    let configured_path = Path::new(configured);
    if configured.contains('\\') || configured.contains('/') || configured_path.is_absolute() {
        if configured_path.exists() {
            return Ok(configured_path.display().to_string());
        }
        return Err(format!("未找到路径：{}", configured_path.display()));
    }

    let output = Command::new("where.exe")
        .arg(configured)
        .output()
        .await
        .map_err(|error| format!("无法执行 where.exe：{error}"))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(first) = stdout.lines().map(str::trim).find(|line| !line.is_empty()) {
            return Ok(first.to_owned());
        }
    }

    Err(compose_process_error(&output))
}

pub(crate) fn compose_process_error(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();

    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => format!("process exited with status {:?}", output.status.code()),
    }
}
