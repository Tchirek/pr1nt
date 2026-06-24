"use client";

import { AlertTriangle, LoaderCircle, Upload } from "lucide-react";
import { type ChangeEvent, useRef, useState } from "react";

import {
  ACCEPT_ATTRIBUTE,
  MAX_UPLOAD_SIZE_BYTES,
  MAX_UPLOAD_SIZE_LABEL,
  isConvertibleFileName,
} from "../lib/document-types";

export interface ConfirmedUploadPayload {
  preparedId: string;
  documentToken: string;
  pageCount: number;
  sourceName: string;
  displayName: string;
  fileSize: number;
}

export interface PreparedDocument {
  preparedId: string;
  documentToken: string;
  pageCount: number;
  sourceName: string;
  displayName: string;
}

export interface PreparationProgress {
  phase: "uploading" | "processing";
  percent: number | null;
  message: string;
}

interface FileUploadProps {
  disabled?: boolean;
  onConfirmed: (payload: ConfirmedUploadPayload) => void;
  onReset?: () => void;
  prepareFile: (file: File, onProgress?: (progress: PreparationProgress) => void) => Promise<PreparedDocument>;
}

export function FileUpload({
  disabled = false,
  onConfirmed,
  onReset,
  prepareFile,
}: FileUploadProps) {
  const [error, setError] = useState<string | null>(null);
  const [isDragOver, setIsDragOver] = useState(false);
  const [isPreparing, setIsPreparing] = useState(false);
  const [progress, setProgress] = useState<PreparationProgress | null>(null);
  const [activeFileName, setActiveFileName] = useState("");
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  async function handleFileSelection(event: ChangeEvent<HTMLInputElement>) {
    const file = event.currentTarget.files?.[0] ?? null;
    event.currentTarget.value = "";
    await prepare(file);
  }

  async function prepare(file: File | null) {
    onReset?.();
    setError(null);
    setProgress(null);
    setActiveFileName("");

    if (!file) {
      return;
    }

    if (!isConvertibleFileName(file.name)) {
      setError("文件类型不支持。");
      return;
    }

    if (file.size > MAX_UPLOAD_SIZE_BYTES) {
      setError(`不能超过 ${MAX_UPLOAD_SIZE_LABEL}。`);
      return;
    }

    setIsPreparing(true);
    setActiveFileName(file.name);
    try {
      const prepared = await prepareFile(file, setProgress);
      onConfirmed({
        preparedId: prepared.preparedId,
        documentToken: prepared.documentToken,
        pageCount: prepared.pageCount,
        sourceName: prepared.sourceName,
        displayName: prepared.displayName,
        fileSize: file.size,
      });
    } catch (prepareError) {
      setError(prepareError instanceof Error ? prepareError.message : "文件没能送达打印机。");
    } finally {
      setIsPreparing(false);
    }
  }

  function openPicker() {
    if (disabled || isPreparing) {
      return;
    }
    fileInputRef.current?.click();
  }

  function handleDragOver(event: React.DragEvent<HTMLButtonElement>) {
    event.preventDefault();
    if (!disabled && !isPreparing) {
      setIsDragOver(true);
    }
  }

  function handleDragLeave(event: React.DragEvent<HTMLButtonElement>) {
    event.preventDefault();
    if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
      setIsDragOver(false);
    }
  }

  async function handleDrop(event: React.DragEvent<HTMLButtonElement>) {
    event.preventDefault();
    setIsDragOver(false);
    if (disabled || isPreparing) {
      return;
    }
    await prepare(event.dataTransfer.files?.[0] ?? null);
  }

  const progressPercent = progress?.percent ?? 0;

  return (
    <section className="rounded-[28px] border border-line bg-panel p-5 shadow-card sm:p-7">
      <button
        className={`flex w-full flex-col items-center justify-center rounded-[24px] border border-dashed px-6 py-14 text-center transition ${
          isDragOver
            ? "border-emerald-600 bg-emerald-50"
            : "border-line bg-[linear-gradient(180deg,rgba(255,255,255,0.98),rgba(247,242,232,0.9))] hover:border-emerald-500"
        } ${disabled || isPreparing ? "cursor-not-allowed opacity-80" : ""}`}
        type="button"
        onClick={openPicker}
        onDragLeave={handleDragLeave}
        onDragOver={handleDragOver}
        onDrop={(event) => void handleDrop(event)}
      >
        {isPreparing ? (
          <LoaderCircle className="h-10 w-10 animate-spin text-emerald-700" />
        ) : (
          <Upload className="h-10 w-10 text-emerald-700" />
        )}

        <p className="mt-4 text-2xl font-semibold text-ink">{isPreparing ? "计页中" : "选文件"}</p>
        <p className="mt-2 text-sm text-stone-600">
          {isPreparing ? progress?.message ?? "正在送达打印机" : `PDF / Word / PPT / Excel，${MAX_UPLOAD_SIZE_LABEL} 内`}
        </p>

        {activeFileName ? <p className="mt-3 max-w-full truncate text-xs text-stone-500">{activeFileName}</p> : null}

        <input
          ref={fileInputRef}
          accept={ACCEPT_ATTRIBUTE}
          className="sr-only"
          disabled={disabled || isPreparing}
          type="file"
          onChange={(event) => void handleFileSelection(event)}
        />
      </button>

      {isPreparing ? (
        <div className="mt-4 h-1.5 overflow-hidden rounded-full bg-stone-100">
          <div className="h-full rounded-full bg-emerald-600" style={{ width: `${progressPercent}%` }} />
        </div>
      ) : null}

      {error ? (
        <div className="mt-4 flex items-start gap-3 rounded-2xl border border-danger/20 bg-danger/5 p-4 text-sm text-danger">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{error}</span>
        </div>
      ) : null}
    </section>
  );
}
