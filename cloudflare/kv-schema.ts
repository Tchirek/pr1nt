export const COLOR_MODES = ["bw", "color"] as const;
export type ColorMode = (typeof COLOR_MODES)[number];

export const QUEUE_JOB_STATUSES = [
  "queued",
  "downloading",
  "printing",
  "done",
  "failed",
] as const;
export type QueueJobStatus = (typeof QUEUE_JOB_STATUSES)[number];

export interface PricesConfig {
  bw_per_page: number;
  color_per_page: number;
}

export interface QRCodesConfig {
  alipay_url: string;
  wechat_url: string;
}

export interface PrintersConfig {
  bw: string;
  color: string;
}

export interface QueueJobRecord {
  id: string;
  user_name: string;
  file_name: string;
  page_count: number;
  copy_count?: number;
  color_mode: ColorMode;
  status: QueueJobStatus;
  submitted_at: string;
  detail?: string;
  pages_printed?: number;
  total_pages?: number;
}

export interface KVSchema {
  "config:prices": PricesConfig;
  "config:qrcodes": QRCodesConfig;
  "config:notice_markdown": string;
  "config:printers": PrintersConfig;
  "queue:active": string[];
  "users:registry": string[];
}

export const KV_KEYS = {
  prices: "config:prices",
  qrcodes: "config:qrcodes",
  notice: "config:notice_markdown",
  printers: "config:printers",
  queueActive: "queue:active",
  usersRegistry: "users:registry",
  queueJob: (jobId: string) => `queue:job:${jobId}`,
} as const;

export type KVStaticKey = keyof KVSchema;
export type KVJobKey = `queue:job:${string}`;
export type KVKey = KVStaticKey | KVJobKey;

export function createQueueJobRecord(input: Omit<QueueJobRecord, "submitted_at"> & { submitted_at?: string }): QueueJobRecord {
  return {
    ...input,
    submitted_at: input.submitted_at ?? new Date().toISOString(),
  };
}
