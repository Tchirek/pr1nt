"use client";

import { AlertCircle, LoaderCircle } from "lucide-react";
import { useEffect, useState } from "react";

import type { PricesConfig, QRCodesConfig } from "../../cloudflare/kv-schema";
import type { PreparedDocument, PreparationProgress } from "@/components/FileUpload";
import { PrintFlow } from "@/components/PrintFlow";
import { UserNameGate } from "@/components/UserNameGate";
import {
  isConvertibleFileName,
  MAX_UPLOAD_SIZE_BYTES,
  MAX_UPLOAD_SIZE_LABEL,
} from "@/lib/document-types";
import { trpcClient } from "@/lib/trpc-client";

const fallbackPrices: PricesConfig = {
  bw_per_page: 0,
  color_per_page: 0,
};

const fallbackQRCodes: QRCodesConfig = {
  alipay_url: "",
  wechat_url: "",
};

const ADDRESS_TEXT = "Room 101";

interface UploadSession {
  document_id: string;
  upload_url: string;
  upload_token: string;
  upload_headers: Record<string, string>;
}

interface PreparationStatus {
  document_id: string;
  status: "uploading" | "pending" | "downloading" | "converting" | "ready" | "failed" | "expired";
  source_name: string;
  file_name: string;
  page_count: number | null;
  error: string | null;
}

export default function HomePage() {
  const [prices, setPrices] = useState<PricesConfig>(fallbackPrices);
  const [qrCodes, setQrCodes] = useState<QRCodesConfig>(fallbackQRCodes);
  const [incomingPreparedDocument, setIncomingPreparedDocument] = useState<PreparedDocument | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isLoadingHandoff, setIsLoadingHandoff] = useState(false);
  const [handoffSource, setHandoffSource] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let isMounted = true;

    async function loadConfig() {
      try {
        const [nextPrices, nextQRCodes] = await Promise.all([
          trpcClient.config.getPrices.query(),
          trpcClient.config.getQRCodes.query(),
        ]);
        if (!isMounted) {
          return;
        }
        setPrices(nextPrices);
        setQrCodes(nextQRCodes);
        setError(null);
      } catch (loadError) {
        if (isMounted) {
          setError(loadError instanceof Error ? loadError.message : "无法加载打印配置。");
        }
      } finally {
        if (isMounted) {
          setIsLoading(false);
        }
      }
    }

    void loadConfig();
    const interval = window.setInterval(() => void loadConfig(), 20_000);
    return () => {
      isMounted = false;
      window.clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    const searchParams = new URLSearchParams(window.location.search);
    const handoffToken = searchParams.get("handoff");
    if (!handoffToken) {
      return;
    }
    setHandoffSource(searchParams.get("source"));

    let cancelled = false;
    setIsLoadingHandoff(true);
    setError(null);

    void trpcClient.document.consumePrintHandoff
      .mutate({ handoff_token: handoffToken })
      .then(async ({ document_token: documentToken, preparation }) => {
        window.history.replaceState(null, "", window.location.pathname || "/");
        const ready = await waitForPreparation(preparation.document_id, documentToken);
        if (!cancelled) {
          setIncomingPreparedDocument(toPreparedDocument(ready, documentToken));
        }
      })
      .catch((handoffError) => {
        if (!cancelled) {
          setError(handoffError instanceof Error ? handoffError.message : "NormalPics 打印交接已失效。");
        }
      })
      .finally(() => {
        if (!cancelled) {
          setIsLoadingHandoff(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <main className="relative min-h-screen overflow-hidden px-4 pb-32 pt-6 sm:px-6 lg:px-8">
      <div className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[440px] bg-[radial-gradient(circle_at_top,rgba(112,167,124,0.22),transparent_58%),radial-gradient(circle_at_20%_20%,rgba(238,209,131,0.24),transparent_28%)]" />

      <div className="mx-auto flex min-h-[calc(100vh-9rem)] max-w-5xl flex-col">
        <section className="flex-1">
          {isLoading ? (
            <LoadingCard text="正在准备打印页面..." />
          ) : (
            <div className="space-y-5">
              {error ? (
                <div className="flex items-start gap-3 rounded-3xl border border-warning/25 bg-warning/5 p-4 text-sm text-warning">
                  <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
                  <span>{error}</span>
                </div>
              ) : null}

              {isLoadingHandoff ? <LoadingCard text={handoffLoadingText(handoffSource)} /> : null}

              <UserNameGate
                checkNameAvailable={async (name) => trpcClient.user.checkNameAvailable.query({ name })}
                registerName={async (name) => trpcClient.user.registerName.mutate({ name })}
              >
                {(userName) => (
                  <PrintFlow
                    getJobStatus={async (jobId) => trpcClient.queue.getJobStatus.query({ job_id: jobId })}
                    incomingPreparedDocument={incomingPreparedDocument}
                    prepareFile={preparePrintFile}
                    prices={prices}
                    qrCodes={qrCodes}
                    submitJob={async (input) => trpcClient.queue.submitPrintJob.mutate(input)}
                    userName={userName}
                  />
                )}
              </UserNameGate>
            </div>
          )}
        </section>
      </div>

      <footer className="fixed inset-x-0 bottom-0 z-40 border-t border-line bg-panel/95 backdrop-blur">
        <div className="mx-auto max-w-5xl px-4 py-4 text-center text-sm font-semibold tracking-[0.04em] text-ink sm:text-base">
          {ADDRESS_TEXT}
        </div>
      </footer>
    </main>
  );
}

function LoadingCard({ text }: { text: string }) {
  return (
    <section className="rounded-[32px] border border-line bg-panel p-6 shadow-card sm:p-8">
      <div className="flex items-center gap-3 text-sm text-stone-600">
        <LoaderCircle className="h-5 w-5 animate-spin" />
        <span>{text}</span>
      </div>
    </section>
  );
}

function handoffLoadingText(source: string | null): string {
  if (source === "normaldocs") return "正在准备 NormalDocs 文档...";
  if (source === "normalpics") return "正在准备 NormalPics 图片...";
  return "正在准备外部文件...";
}

async function preparePrintFile(
  file: File,
  onProgress?: (progress: PreparationProgress) => void,
): Promise<PreparedDocument> {
  if (!isConvertibleFileName(file.name)) {
    throw new Error("当前文件类型不受支持。");
  }
  if (file.size > MAX_UPLOAD_SIZE_BYTES) {
    throw new Error(`文件不能超过 ${MAX_UPLOAD_SIZE_LABEL}。`);
  }
  if (file.size <= 0) {
    throw new Error("文件不能为空。");
  }

  const session = await trpcClient.document.createUploadSession.mutate({
    file_name: file.name,
    mime_type: file.type || "application/octet-stream",
    size_bytes: file.size,
  });
  await uploadToR2(file, session, onProgress);
  await trpcClient.document.notifyUpload.mutate({
    document_id: session.document_id,
    document_token: session.upload_token,
  });
  const ready = await waitForPreparation(session.document_id, session.upload_token, onProgress);
  return toPreparedDocument(ready, session.upload_token);
}

async function uploadToR2(
  file: File,
  session: UploadSession,
  onProgress?: (progress: PreparationProgress) => void,
): Promise<void> {
  let lastError: Error | null = null;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    onProgress?.({
      phase: "uploading",
      percent: 0,
      message: attempt === 1 ? "上传中" : `连接中断，正在重试 ${attempt}/3`,
    });
    try {
      await uploadToR2Once(file, session, onProgress);
      return;
    } catch (error) {
      lastError = error instanceof Error ? error : new Error("上传失败。");
      if (attempt < 3) {
        await sleep(600 * attempt);
      }
    }
  }
  throw lastError ?? new Error("上传失败，请重试。");
}

function uploadToR2Once(
  file: File,
  session: UploadSession,
  onProgress?: (progress: PreparationProgress) => void,
): Promise<void> {
  return new Promise<void>((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      xhr.open("PUT", session.upload_url);
      xhr.timeout = 20 * 60_000;
      for (const [name, value] of Object.entries(session.upload_headers)) {
        xhr.setRequestHeader(name, value);
      }
      xhr.upload.onprogress = (event) => {
        if (!event.lengthComputable) {
          return;
        }
        const percent = Math.max(0, Math.min(100, Math.round((event.loaded / event.total) * 100)));
        onProgress?.({
          phase: "uploading",
          percent,
          message: `上传中 ${percent}%`,
        });
      };
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          resolve();
        } else {
          reject(new Error(`上传失败（${xhr.status || "网络错误"}）。`));
        }
      };
      xhr.onerror = () => reject(new Error("上传失败，请检查网络后重试。"));
      xhr.ontimeout = () => reject(new Error("上传超时，请重试。"));
      xhr.onabort = () => reject(new Error("上传已取消。"));
      xhr.send(file);
    });
}

async function waitForPreparation(
  documentId: string,
  documentToken: string,
  onProgress?: (progress: PreparationProgress) => void,
): Promise<PreparationStatus> {
  const deadline = Date.now() + 30 * 60_000;

  while (Date.now() < deadline) {
    const status = await trpcClient.document.getPreparationStatus.query({
      document_id: documentId,
      document_token: documentToken,
    });
    if (status.status === "ready" && status.page_count) {
      return status;
    }
    if (status.status === "failed" || status.status === "expired") {
      throw new Error(status.error || "文件准备失败，请重新上传。");
    }

    onProgress?.({
      phase: "processing",
      percent: null,
      message: preparationMessage(status.status),
    });
    await sleep(1200);
  }

  throw new Error("文件准备等待超时，请稍后重新打开页面查看。");
}

function toPreparedDocument(status: PreparationStatus, documentToken: string): PreparedDocument {
  if (!status.page_count) {
    throw new Error("文件准备结果缺少页数。");
  }
  return {
    preparedId: status.document_id,
    documentToken,
    pageCount: status.page_count,
    sourceName: status.source_name,
    displayName: status.file_name,
  };
}

function preparationMessage(status: PreparationStatus["status"]): string {
  switch (status) {
    case "uploading":
      return "等待上传完成";
    case "pending":
      return "等待打印电脑处理";
    case "downloading":
      return "打印电脑正在下载";
    case "converting":
      return "转换并计页中";
    default:
      return "准备中";
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}
