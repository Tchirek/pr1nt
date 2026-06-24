"use client";

import {
  AlertCircle,
  CheckCircle2,
  CreditCard,
  LoaderCircle,
  Printer,
  RefreshCw,
  WalletCards,
} from "lucide-react";
import { startTransition, useEffect, useRef, useState } from "react";

import type { ColorMode, PricesConfig, QRCodesConfig, QueueJobRecord } from "../../cloudflare/kv-schema";
import {
  FileUpload,
  type ConfirmedUploadPayload,
  type PreparedDocument,
  type PreparationProgress,
} from "./FileUpload";

type PaymentMethod = "alipay" | "wechat";
type PrintPhase = "idle" | "awaiting_payment" | "queued" | "printing" | "done" | "failed";

interface SubmitJobResponse {
  job_id: string;
  total_price: number;
}

interface PrintFlowProps {
  userName: string;
  prices: PricesConfig;
  qrCodes: QRCodesConfig;
  incomingPreparedDocument?: PreparedDocument | null;
  prepareFile: (file: File, onProgress?: (progress: PreparationProgress) => void) => Promise<PreparedDocument>;
  submitJob: (input: {
    document_id: string;
    document_token: string;
    user_name: string;
    color_mode: ColorMode;
    copy_count: number;
  }) => Promise<SubmitJobResponse>;
  getJobStatus: (jobId: string) => Promise<QueueJobRecord | null>;
}

const COPY_COUNT_OPTIONS = [1, 2, 3, 4, 5] as const;
const MAX_TOTAL_PRINT_PAGES = 60;

export function PrintFlow({
  userName,
  prices,
  qrCodes,
  incomingPreparedDocument,
  prepareFile,
  submitJob,
  getJobStatus,
}: PrintFlowProps) {
  const [phase, setPhase] = useState<PrintPhase>("idle");
  const [colorMode, setColorMode] = useState<ColorMode>("bw");
  const [copyCount, setCopyCount] = useState(1);
  const [paymentMethod, setPaymentMethod] = useState<PaymentMethod>("wechat");
  const [confirmedUpload, setConfirmedUpload] = useState<ConfirmedUploadPayload | null>(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const [statusText, setStatusText] = useState("就绪");
  const [error, setError] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [pagesPrinted, setPagesPrinted] = useState<number | null>(null);
  const [totalPages, setTotalPages] = useState<number | null>(null);
  const handledIncomingIdRef = useRef<string | null>(null);
  const latestStatusRef = useRef<QueueJobRecord["status"] | null>(null);

  useEffect(() => {
    if (paymentMethod === "alipay" && !qrCodes.alipay_url && qrCodes.wechat_url) {
      setPaymentMethod("wechat");
    } else if (paymentMethod === "wechat" && !qrCodes.wechat_url && qrCodes.alipay_url) {
      setPaymentMethod("alipay");
    }
  }, [paymentMethod, qrCodes.alipay_url, qrCodes.wechat_url]);

  useEffect(() => {
    if (!incomingPreparedDocument || handledIncomingIdRef.current === incomingPreparedDocument.preparedId) {
      return;
    }
    handledIncomingIdRef.current = incomingPreparedDocument.preparedId;
    setColorMode("color");
    setCopyCount(1);
    handlePrepared({
      preparedId: incomingPreparedDocument.preparedId,
      documentToken: incomingPreparedDocument.documentToken,
      pageCount: incomingPreparedDocument.pageCount,
      sourceName: incomingPreparedDocument.sourceName,
      displayName: incomingPreparedDocument.displayName,
      fileSize: 0,
    });
  }, [incomingPreparedDocument]);

  useEffect(() => {
    if (!jobId || phase === "done" || phase === "failed") {
      return undefined;
    }
    const refresh = async () => {
      const job = await getJobStatus(jobId);
      if (job) {
        applyStatus(job.status, job.detail, job.pages_printed, job.total_pages);
      }
    };
    void refresh();
    const interval = window.setInterval(() => void refresh(), 1500);
    return () => window.clearInterval(interval);
  }, [getJobStatus, jobId, phase]);

  const currentQrCode = paymentMethod === "alipay" ? qrCodes.alipay_url : qrCodes.wechat_url;
  const totalPrintPages = confirmedUpload ? confirmedUpload.pageCount * copyCount : 0;
  const isTotalPagesTooLarge = totalPrintPages > MAX_TOTAL_PRINT_PAGES;
  const payableAmount = confirmedUpload
    ? totalPrintPages * (colorMode === "color" ? prices.color_per_page : prices.bw_per_page)
    : 0;

  function handlePrepared(payload: ConfirmedUploadPayload) {
    setError(null);
    setConfirmedUpload(payload);
    setPagesPrinted(0);
    setTotalPages(payload.pageCount);
    setStatusText("待付款");
    startTransition(() => setPhase("awaiting_payment"));
  }

  function resetEntireFlow() {
    setPhase("idle");
    setColorMode("bw");
    setCopyCount(1);
    setPaymentMethod("wechat");
    setConfirmedUpload(null);
    setJobId(null);
    latestStatusRef.current = null;
    setStatusText("就绪");
    setError(null);
    setPagesPrinted(null);
    setTotalPages(null);
  }

  async function handlePaidSubmission() {
    if (!confirmedUpload || !currentQrCode || isTotalPagesTooLarge) {
      return;
    }
    setIsSubmitting(true);
    setError(null);
    try {
      latestStatusRef.current = null;
      const submission = await submitJob({
        document_id: confirmedUpload.preparedId,
        document_token: confirmedUpload.documentToken,
        user_name: userName,
        color_mode: colorMode,
        copy_count: copyCount,
      });
      setJobId(submission.job_id);
      setPagesPrinted(0);
      setTotalPages(totalPrintPages);
      setStatusText("排队中");
      startTransition(() => setPhase("queued"));
    } catch (submissionError) {
      setError(submissionError instanceof Error ? submissionError.message : "提交失败。");
    } finally {
      setIsSubmitting(false);
    }
  }

  function applyStatus(
    status: QueueJobRecord["status"],
    detail?: string,
    nextPagesPrinted?: number,
    nextTotalPages?: number,
  ) {
    const previousStatus = latestStatusRef.current;
    if (previousStatus && statusRank(status) < statusRank(previousStatus)) {
      return;
    }
    latestStatusRef.current = status;
    setStatusText(detail ?? fallbackStatusText(status));
    if (typeof nextPagesPrinted === "number") setPagesPrinted(nextPagesPrinted);
    if (typeof nextTotalPages === "number") setTotalPages(nextTotalPages);
    startTransition(() => {
      if (status === "queued" || status === "downloading") setPhase("queued");
      else if (status === "printing") setPhase("printing");
      else if (status === "done") setPhase("done");
      else if (status === "failed") setPhase("failed");
    });
  }

  return (
    <div className="space-y-4">
      <section className="rounded-[24px] border border-line bg-panel/95 p-4 shadow-card">
        <p className="text-xs uppercase tracking-[0.2em] text-stone-500">姓名</p>
        <p className="mt-1 text-lg font-semibold text-ink">{userName}</p>
      </section>

      {!confirmedUpload && !jobId ? (
        <FileUpload prepareFile={prepareFile} onConfirmed={handlePrepared} onReset={resetEntireFlow} />
      ) : null}

      {error && !confirmedUpload ? <AlertBanner message={error} /> : null}

      {phase === "awaiting_payment" && confirmedUpload ? (
        <section className="rounded-[28px] border border-line bg-panel p-5 shadow-card sm:p-7">
          <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_300px]">
            <div>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div>
                  <h2 className="text-3xl font-semibold tracking-tight text-ink">付款</h2>
                  <p className="mt-2 text-sm text-stone-600">真实页数：{confirmedUpload.pageCount}</p>
                </div>
                <button className="rounded-full border border-line px-4 py-2 text-sm font-medium text-ink transition hover:border-ink" type="button" onClick={resetEntireFlow}>
                  换文件
                </button>
              </div>

              <div className="mt-5 grid gap-3 sm:grid-cols-2">
                <ModeButton active={colorMode === "bw"} label="黑白" value={`${prices.bw_per_page.toFixed(2)} / 页`} onClick={() => setColorMode("bw")} />
                <ModeButton active={colorMode === "color"} label="彩色" value={`${prices.color_per_page.toFixed(2)} / 页`} onClick={() => setColorMode("color")} />
              </div>

              <div className="mt-5 grid gap-3 sm:grid-cols-4">
                <InfoCard label="文件" value={confirmedUpload.displayName} />
                <InfoCard label="页数" value={`${confirmedUpload.pageCount}`} />
                <InfoCard label="份数" value={`${copyCount}`} />
                <InfoCard label="金额" value={`${payableAmount.toFixed(2)} 元`} strong />
              </div>

              <div className="mt-5 flex flex-wrap items-center gap-3">
                <label className="inline-flex items-center gap-3 rounded-full border border-line bg-white px-4 py-2 text-sm font-medium text-ink">
                  <span className="text-stone-500">份数</span>
                  <select className="bg-transparent text-base font-semibold outline-none" value={copyCount} onChange={(event) => setCopyCount(Number(event.target.value))}>
                    {COPY_COUNT_OPTIONS.map((option) => <option key={option} value={option}>{option}</option>)}
                  </select>
                </label>
                <button
                  className="inline-flex items-center justify-center rounded-full border border-ink bg-ink px-5 py-3 text-sm font-medium text-white transition hover:bg-stone-800 disabled:cursor-not-allowed disabled:border-stone-300 disabled:bg-stone-300"
                  disabled={isSubmitting || !currentQrCode || isTotalPagesTooLarge}
                  type="button"
                  onClick={() => void handlePaidSubmission()}
                >
                  {isSubmitting ? <LoaderCircle className="mr-2 h-4 w-4 animate-spin" /> : <CreditCard className="mr-2 h-4 w-4" />}
                  已付款，打印
                </button>
              </div>

              {isTotalPagesTooLarge ? <AlertBanner className="mt-5" message="总页数过多，请减少份数。" /> : null}
              {error ? <AlertBanner className="mt-5" message={error} /> : null}
            </div>

            <aside className="rounded-[24px] border border-line bg-[#f7f2e8] p-4">
              <div className="flex items-center justify-between gap-3">
                <h3 className="text-xl font-semibold text-ink">{paymentMethod === "alipay" ? "支付宝" : "微信"}</h3>
                <WalletCards className="h-5 w-5 text-emerald-700" />
              </div>
              <div className="mt-4 flex gap-2 rounded-full border border-line bg-white/80 p-1">
                {(["alipay", "wechat"] as const).map((method) => {
                  const available = method === "alipay" ? Boolean(qrCodes.alipay_url) : Boolean(qrCodes.wechat_url);
                  return (
                    <button key={method} className={`flex-1 rounded-full px-3 py-2 text-sm font-medium transition ${paymentMethod === method ? "bg-ink text-white" : "text-stone-600"} ${!available ? "opacity-45" : ""}`} disabled={!available} type="button" onClick={() => setPaymentMethod(method)}>
                      {method === "alipay" ? "支付宝" : "微信"}
                    </button>
                  );
                })}
              </div>
              <div className="mt-4 overflow-hidden rounded-[20px] border border-line bg-white p-3">
                {currentQrCode ? <img alt="收款码" className="aspect-square w-full rounded-2xl object-cover" src={currentQrCode} /> : <div className="flex aspect-square items-center justify-center text-sm text-stone-500">暂无收款码</div>}
              </div>
            </aside>
          </div>
        </section>
      ) : null}

      {(jobId || ["queued", "printing", "done", "failed"].includes(phase)) && confirmedUpload ? (
        <section className="rounded-[28px] border border-line bg-panel p-5 shadow-card sm:p-7">
          <div className="flex items-start gap-3">
            <div className={`inline-flex h-11 w-11 items-center justify-center rounded-2xl ${statusIconClassName(phase, error)}`}>
              {phase === "done" ? <CheckCircle2 className="h-5 w-5" /> : phase === "failed" || error ? <AlertCircle className="h-5 w-5" /> : <Printer className="h-5 w-5" />}
            </div>
            <div className="min-w-0 flex-1">
              <h2 className="text-2xl font-semibold text-ink">{statusTitle(phase)}</h2>
              <p className="mt-2 text-sm text-stone-600">{statusText}</p>
            </div>
          </div>
          <div className="mt-5 grid gap-3 sm:grid-cols-3">
            <InfoCard label="文件" value={confirmedUpload.displayName} />
            <InfoCard label="打印" value={`${colorMode === "color" ? "彩色" : "黑白"} x ${copyCount}`} />
            <InfoCard label="进度" value={pagesPrinted !== null && totalPages ? `${pagesPrinted}/${totalPages}` : "等待中"} />
          </div>
          {phase === "printing" || phase === "done" ? (
            <div className="mt-5 h-2 overflow-hidden rounded-full bg-stone-100">
              <div className="h-full rounded-full bg-emerald-600 transition-all" style={{ width: `${calculatePageProgressPercent(pagesPrinted, totalPages, phase === "done")}%` }} />
            </div>
          ) : null}
          {phase === "done" || phase === "failed" ? (
            <button className="mt-5 inline-flex items-center justify-center rounded-full border border-ink bg-ink px-5 py-3 text-sm font-medium text-white transition hover:bg-stone-800" type="button" onClick={resetEntireFlow}>
              <RefreshCw className="mr-2 h-4 w-4" />
              再来一份
            </button>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}

function ModeButton({ active, label, value, onClick }: { active: boolean; label: string; value: string; onClick: () => void }) {
  return (
    <button className={`rounded-[20px] border p-4 text-left transition ${active ? "border-emerald-600 bg-emerald-600 text-white" : "border-line bg-canvas/70 text-ink hover:border-emerald-500"}`} type="button" onClick={onClick}>
      <p className="text-lg font-semibold">{label}</p>
      <p className={`mt-1 text-sm ${active ? "text-white/85" : "text-stone-600"}`}>{value}</p>
    </button>
  );
}

function InfoCard({ label, value, strong = false }: { label: string; value: string; strong?: boolean }) {
  return (
    <div className="rounded-2xl border border-line bg-canvas/70 p-4">
      <p className="text-xs uppercase tracking-[0.18em] text-stone-500">{label}</p>
      <p className={`mt-2 line-clamp-2 break-words ${strong ? "text-xl font-semibold" : "text-sm font-medium"} text-ink`}>{value}</p>
    </div>
  );
}

function AlertBanner({ className = "", message }: { className?: string; message: string }) {
  return (
    <div className={`${className} flex items-start gap-3 rounded-2xl border border-danger/20 bg-danger/5 p-4 text-sm text-danger`}>
      <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
      <span>{message}</span>
    </div>
  );
}

function statusRank(status: QueueJobRecord["status"]): number {
  if (status === "queued") return 1;
  if (status === "downloading") return 2;
  if (status === "printing") return 3;
  return 4;
}

function fallbackStatusText(status: QueueJobRecord["status"]): string {
  if (status === "queued" || status === "downloading") return "排队中";
  if (status === "printing") return "打印中";
  if (status === "done") return "完成";
  return "失败";
}

function statusTitle(phase: PrintPhase): string {
  if (phase === "queued") return "排队中";
  if (phase === "printing") return "打印中";
  if (phase === "done") return "完成";
  if (phase === "failed") return "失败";
  return "准备中";
}

function statusIconClassName(phase: PrintPhase, error: string | null): string {
  if (phase === "done") return "bg-emerald-600 text-white";
  if (phase === "failed" || error) return "bg-danger text-white";
  return "bg-ink text-white";
}

function calculatePageProgressPercent(pagesPrinted: number | null, totalPages: number | null, done: boolean): number {
  if (done) return 100;
  if (pagesPrinted === null || totalPages === null || totalPages <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((pagesPrinted / totalPages) * 100)));
}
