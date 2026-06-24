export const MAX_UPLOAD_SIZE_BYTES = 256 * 1024 * 1024;
export const MAX_UPLOAD_SIZE_LABEL = "256 MB";

export const PDF_EXTENSIONS = new Set(["pdf"]);

export const CONVERTIBLE_EXTENSIONS = new Set([
  "doc",
  "docx",
  "xls",
  "xlsx",
  "ppt",
  "pptx",
  "rtf",
  "txt",
  "csv",
  "odt",
  "ods",
  "odp",
]);

export const ACCEPT_ATTRIBUTE = [
  ".pdf",
  ".doc",
  ".docx",
  ".xls",
  ".xlsx",
  ".ppt",
  ".pptx",
  ".rtf",
  ".txt",
  ".csv",
  ".odt",
  ".ods",
  ".odp",
].join(",");

export function getExtension(fileName: string): string {
  const normalized = fileName.trim().toLowerCase();
  const extension = normalized.split(".").pop() ?? "";
  return extension === normalized ? "" : extension;
}

export function isPdfFileName(fileName: string): boolean {
  return PDF_EXTENSIONS.has(getExtension(fileName));
}

export function isConvertibleFileName(fileName: string): boolean {
  const extension = getExtension(fileName);
  return PDF_EXTENSIONS.has(extension) || CONVERTIBLE_EXTENSIONS.has(extension);
}

export function buildConvertedPdfName(fileName: string): string {
  const trimmed = fileName.trim();
  const dotIndex = trimmed.lastIndexOf(".");
  const stem = dotIndex <= 0 ? trimmed : trimmed.slice(0, dotIndex);
  return `${stem || "document"}.pdf`;
}
