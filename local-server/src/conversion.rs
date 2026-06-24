use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};

use tokio::{fs, process::Command};
use tracing::warn;

use crate::{
    config::{DocumentConverterConfig, DocumentConverterKind},
    ApiError,
};
use lopdf::{dictionary, Document, Object, Stream};

const BASE_CONVERSION_TIMEOUT_SECONDS: u64 = 180;
const MAX_CONVERSION_TIMEOUT_SECONDS: u64 = 900;

pub(crate) async fn convert_document_to_pdf(
    converter: &DocumentConverterConfig,
    source_path: &Path,
    out_dir: &Path,
) -> Result<PathBuf, ApiError> {
    let expected_pdf = out_dir.join(format!("{}.pdf", file_stem_for_output(source_path)?));
    let conversion_timeout = conversion_timeout_for(source_path).await;

    match converter.kind {
        DocumentConverterKind::LibreOffice => {
            let mut command = Command::new(&converter.libreoffice_path);
            command.args([
                "--headless",
                "--nologo",
                "--nofirststartwizard",
                "--convert-to",
                "pdf",
                "--outdir",
                out_dir.to_str().ok_or_else(|| {
                    ApiError::internal("conversion output directory is not valid UTF-8")
                })?,
                source_path
                    .to_str()
                    .ok_or_else(|| ApiError::internal("source file path is not valid UTF-8"))?,
            ]);
            let output =
                command_output_with_timeout(command, conversion_timeout, "LibreOffice conversion")
                    .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                return Err(ApiError::bad_request(format!(
                    "document conversion failed: {} {}",
                    stdout.trim(),
                    stderr.trim()
                )));
            }
        }
        DocumentConverterKind::Wps => {
            convert_document_via_powershell_suite(
                "wps",
                source_path,
                &expected_pdf,
                conversion_timeout,
            )
            .await?;
        }
        DocumentConverterKind::Office => {
            convert_document_via_powershell_suite(
                "office",
                source_path,
                &expected_pdf,
                conversion_timeout,
            )
            .await?;
        }
    }

    if !expected_pdf.exists() {
        return Err(ApiError::bad_request(
            "document conversion did not produce a PDF output file",
        ));
    }

    Ok(expected_pdf)
}

pub(crate) async fn convert_image_to_pdf(
    source_path: &Path,
    out_dir: &Path,
) -> Result<PathBuf, ApiError> {
    let source_path = source_path.to_path_buf();
    let output_path = out_dir.join("image-source.pdf");
    let blocking_output_path = output_path.clone();

    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let image = image::open(&source_path)
            .map_err(|error| ApiError::bad_request(format!("failed to decode image: {error}")))?;
        let width = image.width();
        let height = image.height();
        if width == 0 || height == 0 {
            return Err(ApiError::bad_request("image has no printable pixels"));
        }

        let rgb = image.to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
            .encode(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .map_err(|error| {
                ApiError::bad_request(format!("failed to encode image for PDF: {error}"))
            })?;

        let pdf = build_single_image_pdf(&jpeg, width, height)?;
        std::fs::write(&blocking_output_path, pdf)
            .map_err(|error| ApiError::internal(format!("failed to write image PDF: {error}")))?;
        Ok(())
    })
    .await
    .map_err(|error| ApiError::internal(format!("image conversion task failed: {error}")))??;

    Ok(output_path)
}

pub(crate) fn source_extension(file_name: &str) -> String {
    Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub(crate) fn is_supported_source_extension(extension: &str) -> bool {
    matches!(
        extension,
        "pdf"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "rtf"
            | "txt"
            | "csv"
            | "odt"
            | "ods"
            | "odp"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "bmp"
            | "gif"
    )
}

pub(crate) fn is_supported_image_extension(extension: &str) -> bool {
    matches!(extension, "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif")
}

pub(crate) async fn cleanup_conversion_dir(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path).await {
        if error.kind() == std::io::ErrorKind::NotFound {
            return;
        }
        warn!(
            "failed to clean conversion directory {}: {error}",
            path.display()
        );
    }
}

pub(crate) fn build_converted_pdf_name(source_name: &str) -> String {
    let stem = Path::new(source_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("document");

    format!("{stem}.pdf")
}

fn file_stem_for_output(path: &Path) -> Result<String, ApiError> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ApiError::internal("source file name is not valid UTF-8"))
}

async fn convert_document_via_powershell_suite(
    suite: &str,
    source_path: &Path,
    destination_path: &Path,
    conversion_timeout: Duration,
) -> Result<(), ApiError> {
    let mode = powershell_conversion_mode(source_path, suite)?;
    let source = source_path
        .to_str()
        .ok_or_else(|| ApiError::internal("source file path is not valid UTF-8"))?;
    let destination = destination_path
        .to_str()
        .ok_or_else(|| ApiError::internal("converted PDF path is not valid UTF-8"))?;
    let command_script = build_powershell_conversion_command(suite, mode, source, destination);

    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-STA",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &command_script,
    ]);
    let output =
        command_output_with_timeout(command, conversion_timeout, "PowerShell document converter")
            .await?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(ApiError::bad_request(format!(
        "document conversion failed: {} {}",
        stdout.trim(),
        stderr.trim()
    )))
}

async fn command_output_with_timeout(
    mut command: Command,
    timeout: Duration,
    label: &str,
) -> Result<Output, ApiError> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command
        .spawn()
        .map_err(|error| ApiError::internal(format!("failed to launch {label}: {error}")))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(ApiError::internal(format!(
            "failed to collect {label} output: {error}"
        ))),
        Err(_) => Err(ApiError::bad_request(format!(
            "{label} timed out after {} seconds",
            timeout.as_secs()
        ))),
    }
}

async fn conversion_timeout_for(source_path: &Path) -> Duration {
    let size_bytes = fs::metadata(source_path)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let size_mb = size_bytes.div_ceil(1024 * 1024).max(1);
    let seconds = BASE_CONVERSION_TIMEOUT_SECONDS
        .saturating_add(size_mb.saturating_mul(2))
        .min(MAX_CONVERSION_TIMEOUT_SECONDS);

    Duration::from_secs(seconds)
}

fn build_powershell_conversion_command(
    suite: &str,
    mode: &str,
    source: &str,
    destination: &str,
) -> String {
    format!(
        "& {{\n{script}\n}} -Suite '{suite}' -Mode '{mode}' -SourcePath '{source}' -DestinationPath '{destination}'",
        script = POWERSHELL_COM_CONVERSION_SCRIPT,
        suite = escape_powershell_single_quoted(suite),
        mode = escape_powershell_single_quoted(mode),
        source = escape_powershell_single_quoted(source),
        destination = escape_powershell_single_quoted(destination),
    )
}

fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn powershell_conversion_mode(source_path: &Path, suite: &str) -> Result<&'static str, ApiError> {
    let extension = source_extension(
        source_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default(),
    );

    match extension.as_str() {
        "doc" | "docx" | "rtf" | "txt" => Ok("word"),
        "xls" | "xlsx" | "csv" => Ok("excel"),
        "ppt" | "pptx" => Ok("powerpoint"),
        "odt" | "ods" | "odp" => Err(ApiError::bad_request(format!(
            "{suite} conversion does not support OpenDocument files. Switch the converter to LibreOffice and retry."
        ))),
        _ => Err(ApiError::bad_request("unsupported source file type")),
    }
}

fn build_single_image_pdf(
    jpeg: &[u8],
    image_width: u32,
    image_height: u32,
) -> Result<Vec<u8>, ApiError> {
    const A4_PORTRAIT_WIDTH: f64 = 595.28;
    const A4_PORTRAIT_HEIGHT: f64 = 841.89;
    const PAGE_MARGIN: f64 = 18.0;

    let (page_width, page_height) = if image_width >= image_height {
        (A4_PORTRAIT_HEIGHT, A4_PORTRAIT_WIDTH)
    } else {
        (A4_PORTRAIT_WIDTH, A4_PORTRAIT_HEIGHT)
    };
    let max_width = page_width - PAGE_MARGIN * 2.0;
    let max_height = page_height - PAGE_MARGIN * 2.0;
    let scale = (max_width / f64::from(image_width)).min(max_height / f64::from(image_height));
    let draw_width = f64::from(image_width) * scale;
    let draw_height = f64::from(image_height) * scale;
    let draw_x = (page_width - draw_width) / 2.0;
    let draw_y = (page_height - draw_height) / 2.0;
    let content =
        format!("q\n{draw_width:.2} 0 0 {draw_height:.2} {draw_x:.2} {draw_y:.2} cm\n/Im0 Do\nQ\n");

    let mut doc = Document::with_version("1.4");
    let pages_id = doc.new_object_id();
    let image_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => i64::from(image_width),
            "Height" => i64::from(image_height),
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg.to_vec(),
    ));
    let resources_id = doc.add_object(dictionary! {
        "XObject" => dictionary! {
            "Im0" => image_id,
        },
    });
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Resources" => resources_id,
        "Contents" => content_id,
        "MediaBox" => vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(page_width as f32),
            Object::Real(page_height as f32),
        ],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut output = Vec::with_capacity(jpeg.len() + 2048);
    doc.save_to(&mut output)
        .map_err(|error| ApiError::internal(format!("failed to write image PDF: {error}")))?;
    Ok(output)
}

const POWERSHELL_COM_CONVERSION_SCRIPT: &str = r#"
param(
  [string]$Suite,
  [string]$Mode,
  [string]$SourcePath,
  [string]$DestinationPath
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Release-ComObject([object]$Value) {
  if ($null -ne $Value) {
    try {
      [void][System.Runtime.InteropServices.Marshal]::FinalReleaseComObject($Value)
    } catch {
    }
  }
}

switch ($Suite) {
  'office' {
    $progIds = @{
      word = 'Word.Application'
      excel = 'Excel.Application'
      powerpoint = 'PowerPoint.Application'
    }
  }
  'wps' {
    $progIds = @{
      word = 'kwps.Application'
      excel = 'ket.Application'
      powerpoint = 'kwpp.Application'
    }
  }
  default {
    throw "Unsupported conversion suite: $Suite"
  }
}

$app = $null
$document = $null

try {
  try {
    $app = New-Object -ComObject $progIds[$Mode]
  } catch {
    throw "Unable to start $Suite for $Mode documents. Confirm that the selected office suite is installed on this PC."
  }

  switch ($Mode) {
    'word' {
      try { $app.Visible = $false } catch {}
      try { $app.DisplayAlerts = 0 } catch {}
      $document = $app.Documents.Open($SourcePath, $false, $true)
      $document.ExportAsFixedFormat($DestinationPath, 17)
      $document.Close($false)
    }
    'excel' {
      try { $app.Visible = $false } catch {}
      try { $app.DisplayAlerts = $false } catch {}
      $document = $app.Workbooks.Open($SourcePath, $null, $true)
      $document.ExportAsFixedFormat(0, $DestinationPath)
      $document.Close($false)
    }
    'powerpoint' {
      $document = $app.Presentations.Open($SourcePath, $true, $false, $false)
      $document.SaveAs($DestinationPath, 32)
      $document.Close()
    }
    default {
      throw "Unsupported conversion mode: $Mode"
    }
  }
} finally {
  if ($null -ne $document) {
    Release-ComObject $document
  }
  if ($null -ne $app) {
    try {
      $app.Quit()
    } catch {
    }
    Release-ComObject $app
  }
  [GC]::Collect()
  [GC]::WaitForPendingFinalizers()
}
"#;
