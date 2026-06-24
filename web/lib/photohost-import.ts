import { PDFDocument } from "pdf-lib";

export interface IncomingPhotoPrint {
  id: string;
  sourceUrl: string;
  filename: string;
}

const A4_PORTRAIT = { width: 595.28, height: 841.89 };
const A4_LANDSCAPE = { width: 841.89, height: 595.28 };
const PAGE_MARGIN = 18;
const MAX_CANVAS_SIDE = 2400;
const IMPORT_TIMEOUT_MS = 15000;
const DECODE_TIMEOUT_MS = 12000;
const ENCODE_TIMEOUT_MS = 12000;

interface DecodedPhoto {
  source: CanvasImageSource;
  width: number;
  height: number;
  close?: () => void;
}

export async function buildPhotoPrintPdf(input: IncomingPhotoPrint): Promise<File> {
  const imageBlob = await fetchImportedPhoto(input);
  const image = await decodeImage(imageBlob);
  try {
    const canvas = rasterizeImage(image);
    const jpegBlob = await canvasToBlob(canvas, "image/jpeg", 0.92);
    const jpegBytes = new Uint8Array(await jpegBlob.arrayBuffer());
    const pdfBytes = await buildJpegPdf(jpegBytes, canvas.width, canvas.height);
    const pdfBuffer = bytesToArrayBuffer(pdfBytes);

    return new File([pdfBuffer], pdfFileName(input.filename), {
      type: "application/pdf",
      lastModified: Date.now(),
    });
  } finally {
    image.close?.();
  }
}

async function fetchImportedPhoto(input: IncomingPhotoPrint): Promise<Blob> {
  const params = new URLSearchParams({
    src: input.sourceUrl,
    filename: input.filename,
  });
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), IMPORT_TIMEOUT_MS);
  let response: Response;

  try {
    response = await fetch(`/api/photohost/import?${params.toString()}`, {
      cache: "no-store",
      signal: controller.signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new Error("NormalPics photo fetch timed out.");
    }
    throw error;
  } finally {
    window.clearTimeout(timeout);
  }

  if (!response.ok) {
    throw new Error("NormalPics photo import failed.");
  }

  const blob = await response.blob();
  if (!blob.type.startsWith("image/")) {
    throw new Error("Imported file is not an image.");
  }
  return blob;
}

async function decodeImage(blob: Blob): Promise<DecodedPhoto> {
  if ("createImageBitmap" in window) {
    try {
      const bitmap = await withTimeout(
        window.createImageBitmap(blob),
        DECODE_TIMEOUT_MS,
        "NormalPics photo decode timed out.",
      );

      if (bitmap.width > 0 && bitmap.height > 0) {
        return {
          source: bitmap,
          width: bitmap.width,
          height: bitmap.height,
          close: () => bitmap.close(),
        };
      }
    } catch {
      // Fall through to the HTMLImageElement path for older WebKit/WebP edge cases.
    }
  }

  return decodeImageElement(blob);
}

function decodeImageElement(blob: Blob): Promise<DecodedPhoto> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(blob);
    const image = new Image();
    let settled = false;
    const timeout = window.setTimeout(() => {
      fail(new Error("NormalPics photo decode timed out."));
    }, DECODE_TIMEOUT_MS);

    const cleanup = () => {
      window.clearTimeout(timeout);
      image.onload = null;
      image.onerror = null;
    };

    const fail = (error: Error) => {
      if (settled) return;
      settled = true;
      cleanup();
      URL.revokeObjectURL(url);
      reject(error);
    };

    image.decoding = "async";
    image.onload = () => {
      if (settled) return;
      const width = image.naturalWidth || image.width || 0;
      const height = image.naturalHeight || image.height || 0;
      if (width <= 0 || height <= 0) {
        fail(new Error("Imported image has no visible pixels."));
        return;
      }

      settled = true;
      cleanup();
      resolve({
        source: image,
        width,
        height,
        close: () => URL.revokeObjectURL(url),
      });
    };
    image.onerror = () => {
      fail(new Error("Unable to decode imported image."));
    };
    image.src = url;
  });
}

function rasterizeImage(image: DecodedPhoto): HTMLCanvasElement {
  const sourceWidth = image.width || 1;
  const sourceHeight = image.height || 1;
  const ratio = Math.min(1, MAX_CANVAS_SIDE / Math.max(sourceWidth, sourceHeight));
  const width = Math.max(1, Math.round(sourceWidth * ratio));
  const height = Math.max(1, Math.round(sourceHeight * ratio));
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;

  const context = canvas.getContext("2d", { alpha: false });
  if (!context) {
    throw new Error("Unable to prepare print canvas.");
  }

  context.fillStyle = "#ffffff";
  context.fillRect(0, 0, width, height);
  context.imageSmoothingEnabled = true;
  context.imageSmoothingQuality = "high";
  context.drawImage(image.source, 0, 0, width, height);
  return canvas;
}

function canvasToBlob(canvas: HTMLCanvasElement, type: string, quality: number): Promise<Blob> {
  return new Promise((resolve, reject) => {
    const timeout = window.setTimeout(() => {
      reject(new Error("NormalPics photo encode timed out."));
    }, ENCODE_TIMEOUT_MS);

    try {
      canvas.toBlob((blob) => {
        window.clearTimeout(timeout);
        if (blob) resolve(blob);
        else reject(new Error("Unable to encode image for PDF."));
      }, type, quality);
    } catch (error) {
      window.clearTimeout(timeout);
      reject(error instanceof Error ? error : new Error("Unable to encode image for PDF."));
    }
  });
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timeout = window.setTimeout(() => reject(new Error(message)), timeoutMs);
    promise
      .then(resolve, reject)
      .finally(() => window.clearTimeout(timeout));
  });
}

async function buildJpegPdf(jpegBytes: Uint8Array, imageWidth: number, imageHeight: number): Promise<Uint8Array> {
  const pageSize = imageWidth >= imageHeight ? A4_LANDSCAPE : A4_PORTRAIT;
  const maxWidth = pageSize.width - PAGE_MARGIN * 2;
  const maxHeight = pageSize.height - PAGE_MARGIN * 2;
  const scale = Math.min(maxWidth / imageWidth, maxHeight / imageHeight);
  const drawWidth = imageWidth * scale;
  const drawHeight = imageHeight * scale;
  const drawX = (pageSize.width - drawWidth) / 2;
  const drawY = (pageSize.height - drawHeight) / 2;

  const pdf = await PDFDocument.create();
  const page = pdf.addPage([pageSize.width, pageSize.height]);
  const embeddedImage = await pdf.embedJpg(jpegBytes);
  page.drawImage(embeddedImage, {
    x: drawX,
    y: drawY,
    width: drawWidth,
    height: drawHeight,
  });

  return pdf.save();
}

function bytesToArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy.buffer;
}

function pdfFileName(filename: string): string {
  const clean = filename.replace(/[<>:"/\\|?*\u0000-\u001F]/g, "").trim();
  const stem = clean.replace(/\.[^.]+$/u, "") || "NormalPics";
  return `${stem}.pdf`;
}
