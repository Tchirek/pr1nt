import { AwsClient } from "aws4fetch";
import { Buffer } from "node:buffer";

import type { D1Database, D1PreparedStatement, D1Result, R2Bucket } from "@cloudflare/workers-types";

import type { ColorMode, PricesConfig, QueueJobRecord } from "../../../cloudflare/kv-schema";

export const MAX_UPLOAD_SIZE_BYTES = 256 * 1024 * 1024;
export const DOCUMENT_TOKEN_TTL_MS = 3 * 24 * 60 * 60 * 1000;
export const DOCUMENT_RECORD_TTL_MS = 3 * 24 * 60 * 60 * 1000;
export const HANDOFF_TTL_MS = 15 * 60 * 1000;
export const CLAIM_LEASE_MS = 2 * 60 * 1000;

const PRESIGNED_UPLOAD_TTL_SECONDS = 15 * 60;
const MAX_COPY_COUNT = 5;
const MAX_TOTAL_PRINT_PAGES = 60;

const SUPPORTED_EXTENSIONS = new Set([
  "pdf",
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
  "jpg",
  "jpeg",
  "png",
  "webp",
  "bmp",
  "gif",
]);

export interface PrintSyncBindings {
  PRINT_DB: D1Database;
  PRINT_STAGING: R2Bucket;
  UPLOAD_SIGNING_SECRET: string;
  PRINT_SYNC_SECRET: string;
  NORMALPICS_HANDOFF_SECRET?: string;
  NORMALPICS_ORIGINS?: string;
  R2_ACCOUNT_ID: string;
  R2_BUCKET_NAME: string;
  R2_ACCESS_KEY_ID: string;
  R2_SECRET_ACCESS_KEY: string;
}

export type DocumentStatus =
  | "uploading"
  | "pending"
  | "downloading"
  | "converting"
  | "ready"
  | "failed"
  | "expired";

export interface DocumentRow {
  id: string;
  source_type: string;
  source_name: string;
  display_name: string | null;
  mime_type: string;
  declared_size: number;
  actual_size: number | null;
  sha256: string | null;
  r2_key: string;
  status: DocumentStatus;
  error: string | null;
  page_count: number | null;
  prepared_device_id: string | null;
  claim_device_id: string | null;
  claim_expires_at: number | null;
  created_at: number;
  uploaded_at: number | null;
  prepared_at: number | null;
  expires_at: number;
  source_deleted_at: number | null;
}

export interface PrintJobRow {
  id: string;
  document_id: string;
  target_device_id: string;
  user_name: string;
  file_name: string;
  page_count: number;
  copy_count: number;
  color_mode: ColorMode;
  price_per_page: number;
  total_price: number;
  status: "queued" | "printing" | "done" | "failed";
  detail: string | null;
  pages_printed: number;
  total_pages: number;
  claim_device_id: string | null;
  claim_expires_at: number | null;
  created_at: number;
  updated_at: number;
}

export interface UploadSession {
  document_id: string;
  upload_url: string;
  upload_token: string;
  upload_headers: Record<string, string>;
  expires_at: string;
}

export interface PreparationStatus {
  document_id: string;
  status: DocumentStatus;
  source_name: string;
  file_name: string;
  page_count: number | null;
  declared_size: number;
  actual_size: number | null;
  error: string | null;
  created_at: string;
  uploaded_at: string | null;
  prepared_at: string | null;
  expires_at: string;
}

export interface PendingWorkResponse {
  documents: Array<{
    id: string;
    source_name: string;
    mime_type: string;
    declared_size: number;
    status: DocumentStatus;
  }>;
  jobs: Array<{
    id: string;
    document_id: string;
    user_name: string;
    file_name: string;
    page_count: number;
    copy_count: number;
    color_mode: ColorMode;
    total_pages: number;
    status: PrintJobRow["status"];
  }>;
}

interface DocumentTokenPayload {
  kind: "document";
  document_id: string;
  exp: number;
}

export class PrintSyncError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

export async function createUploadSession(
  env: PrintSyncBindings,
  input: {
    fileName: string;
    mimeType: string;
    sizeBytes: number;
    sourceType?: string;
  },
): Promise<UploadSession> {
  const sourceName = sanitizeSourceName(input.fileName);
  const mimeType = sanitizeMimeType(input.mimeType);
  const sizeBytes = validateSize(input.sizeBytes);
  validateSupportedSource(sourceName);

  const documentId = crypto.randomUUID();
  const now = Date.now();
  const expiresAt = now + DOCUMENT_RECORD_TTL_MS;
  const tokenExpiresAt = now + DOCUMENT_TOKEN_TTL_MS;
  const r2Key = stagingKey(documentId, now);

  await env.PRINT_DB.prepare(
    `INSERT INTO print_documents (
      id, source_type, source_name, mime_type, declared_size, r2_key, status,
      created_at, expires_at
    ) VALUES (?, ?, ?, ?, ?, ?, 'uploading', ?, ?)`,
  )
    .bind(
      documentId,
      sanitizeSourceType(input.sourceType),
      sourceName,
      mimeType,
      sizeBytes,
      r2Key,
      now,
      expiresAt,
    )
    .run();

  const uploadUrl = await generatePresignedPut(env, r2Key, mimeType, PRESIGNED_UPLOAD_TTL_SECONDS);
  const uploadToken = await signDocumentToken(env.UPLOAD_SIGNING_SECRET, {
    kind: "document",
    document_id: documentId,
    exp: tokenExpiresAt,
  });

  await cleanupExpiredRecords(env).catch((error) => {
    console.warn("[print-sync] cleanup after upload session failed", error);
  });

  return {
    document_id: documentId,
    upload_url: uploadUrl,
    upload_token: uploadToken,
    upload_headers: {
      "Content-Type": mimeType,
    },
    expires_at: new Date(tokenExpiresAt).toISOString(),
  };
}

export async function notifyUpload(
  env: PrintSyncBindings,
  documentId: string,
  token: string,
): Promise<PreparationStatus> {
  await requireDocumentToken(env, token, documentId);
  const row = await getDocument(env, documentId);
  if (!row) {
    throw new PrintSyncError(404, "document_not_found");
  }
  if (row.status !== "uploading") {
    return preparationStatus(row);
  }
  if (row.expires_at <= Date.now()) {
    await expireDocument(env, row);
    throw new PrintSyncError(410, "document_expired");
  }

  const object = await env.PRINT_STAGING.head(row.r2_key);
  if (!object) {
    throw new PrintSyncError(400, "upload_not_found");
  }
  if (object.size <= 0 || object.size > MAX_UPLOAD_SIZE_BYTES || object.size !== row.declared_size) {
    await env.PRINT_STAGING.delete(row.r2_key).catch(() => undefined);
    await env.PRINT_DB.prepare(
      `UPDATE print_documents
       SET status = 'failed', error = ?, actual_size = ?
       WHERE id = ? AND status = 'uploading'`,
    )
      .bind("uploaded_size_mismatch", object.size, row.id)
      .run();
    throw new PrintSyncError(400, "uploaded_size_mismatch");
  }

  const now = Date.now();
  await env.PRINT_DB.batch([
    env.PRINT_DB.prepare(
      `UPDATE print_documents
       SET status = 'pending', actual_size = ?, uploaded_at = ?, error = NULL
       WHERE id = ? AND status = 'uploading'`,
    ).bind(object.size, now, row.id),
    eventStatement(env, "document_pending", row.id, now),
  ]);

  const updated = await getDocument(env, row.id);
  if (!updated) {
    throw new PrintSyncError(404, "document_not_found");
  }
  return preparationStatus(updated);
}

export async function getPreparationStatus(
  env: PrintSyncBindings,
  documentId: string,
  token: string,
): Promise<PreparationStatus> {
  await requireDocumentToken(env, token, documentId);
  const row = await getDocument(env, documentId);
  if (!row) {
    throw new PrintSyncError(404, "document_not_found");
  }
  return preparationStatus(row);
}

export async function createNormalPicsHandoff(
  env: PrintSyncBindings,
  input: {
    sourceType?: string;
    fileName: string;
    mimeType: string;
    sizeBytes: number;
  },
): Promise<UploadSession & { handoff_token: string }> {
  const session = await createUploadSession(env, {
    ...input,
    sourceType: input.sourceType === "normaldocs" ? "normaldocs" : "normalpics",
  });
  const handoffToken = randomToken();
  const tokenHash = await sha256Hex(handoffToken);
  const now = Date.now();

  await env.PRINT_DB.prepare(
    `INSERT INTO print_handoffs (token_hash, document_id, created_at, expires_at)
     VALUES (?, ?, ?, ?)`,
  )
    .bind(tokenHash, session.document_id, now, now + HANDOFF_TTL_MS)
    .run();

  return {
    ...session,
    handoff_token: handoffToken,
  };
}

export async function consumePrintHandoff(
  env: PrintSyncBindings,
  handoffToken: string,
): Promise<{ document_token: string; preparation: PreparationStatus }> {
  const token = handoffToken.trim();
  if (!token || token.length > 256) {
    throw new PrintSyncError(400, "invalid_handoff_token");
  }
  const tokenHash = await sha256Hex(token);
  const now = Date.now();
  const result = await env.PRINT_DB.prepare(
    `UPDATE print_handoffs
     SET consumed_at = ?
     WHERE token_hash = ? AND consumed_at IS NULL AND expires_at > ?`,
  )
    .bind(now, tokenHash, now)
    .run();

  if (!didChange(result)) {
    throw new PrintSyncError(410, "handoff_expired_or_consumed");
  }

  const documentId = await env.PRINT_DB.prepare(
    `SELECT document_id FROM print_handoffs WHERE token_hash = ?`,
  )
    .bind(tokenHash)
    .first<string>("document_id");
  if (!documentId) {
    throw new PrintSyncError(404, "document_not_found");
  }
  const row = await getDocument(env, documentId);
  if (!row) {
    throw new PrintSyncError(404, "document_not_found");
  }

  return {
    document_token: await signDocumentToken(env.UPLOAD_SIGNING_SECRET, {
      kind: "document",
      document_id: documentId,
      exp: Math.min(row.expires_at, now + DOCUMENT_TOKEN_TTL_MS),
    }),
    preparation: preparationStatus(row),
  };
}

export async function submitPrintJob(
  env: PrintSyncBindings,
  input: {
    documentId: string;
    documentToken: string;
    userName: string;
    colorMode: ColorMode;
    copyCount: number;
    prices: PricesConfig;
  },
): Promise<{ job_id: string; total_price: number }> {
  await requireDocumentToken(env, input.documentToken, input.documentId);
  const document = await getDocument(env, input.documentId);
  if (!document) {
    throw new PrintSyncError(404, "document_not_found");
  }
  if (document.status !== "ready" || !document.page_count || !document.prepared_device_id) {
    throw new PrintSyncError(409, "document_not_ready");
  }

  const copyCount = Math.floor(input.copyCount);
  if (!Number.isFinite(copyCount) || copyCount < 1 || copyCount > MAX_COPY_COUNT) {
    throw new PrintSyncError(400, "invalid_copy_count");
  }
  const totalPages = document.page_count * copyCount;
  if (totalPages > MAX_TOTAL_PRINT_PAGES) {
    throw new PrintSyncError(400, "too_many_print_pages");
  }

  const existing = await env.PRINT_DB.prepare(
    `SELECT * FROM print_jobs WHERE document_id = ?`,
  )
    .bind(document.id)
    .first<PrintJobRow>();
  if (existing) {
    return {
      job_id: existing.id,
      total_price: existing.total_price,
    };
  }

  const pricePerPage = input.colorMode === "color" ? input.prices.color_per_page : input.prices.bw_per_page;
  const totalPrice = roundMoney(pricePerPage * totalPages);
  const now = Date.now();
  const jobId = crypto.randomUUID();
  const fileName = document.display_name || convertedPdfName(document.source_name);

  try {
    await env.PRINT_DB.batch([
      env.PRINT_DB.prepare(
        `INSERT INTO print_jobs (
          id, document_id, target_device_id, user_name, file_name, page_count,
          copy_count, color_mode, price_per_page, total_price, status, detail,
          pages_printed, total_pages, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?, 0, ?, ?, ?)`,
      ).bind(
        jobId,
        document.id,
        document.prepared_device_id,
        input.userName,
        fileName,
        document.page_count,
        copyCount,
        input.colorMode,
        pricePerPage,
        totalPrice,
        "排队中",
        totalPages,
        now,
        now,
      ),
      eventStatement(env, "job_queued", jobId, now),
    ]);
  } catch {
    const concurrent = await env.PRINT_DB.prepare(
      `SELECT * FROM print_jobs WHERE document_id = ?`,
    )
      .bind(document.id)
      .first<PrintJobRow>();
    if (concurrent) {
      return {
        job_id: concurrent.id,
        total_price: concurrent.total_price,
      };
    }
    throw new PrintSyncError(500, "job_create_failed");
  }

  return {
    job_id: jobId,
    total_price: totalPrice,
  };
}

export async function getPrintJobStatus(
  env: PrintSyncBindings,
  jobId: string,
): Promise<QueueJobRecord | null> {
  const row = await env.PRINT_DB.prepare(`SELECT * FROM print_jobs WHERE id = ?`)
    .bind(jobId)
    .first<PrintJobRow>();
  return row ? queueJobRecord(row) : null;
}

export async function pendingWork(
  env: PrintSyncBindings,
  deviceId: string,
): Promise<PendingWorkResponse> {
  const now = Date.now();
  const legacyDeviceId = legacyDevicePrefix(deviceId);
  await env.PRINT_DB.prepare(
    `UPDATE print_jobs
     SET status = 'failed',
         detail = '打印电脑在打印过程中断线，任务已停止以避免重复打印。',
         claim_expires_at = NULL,
         updated_at = ?
     WHERE status = 'printing'
       AND claim_expires_at IS NOT NULL
       AND claim_expires_at < ?`,
  )
    .bind(now, now)
    .run();

  const documents = await env.PRINT_DB.prepare(
    `SELECT id, source_name, mime_type, declared_size, status
     FROM print_documents
     WHERE expires_at > ?
       AND (
         status = 'pending'
         OR (
           status IN ('downloading', 'converting')
           AND (claim_expires_at IS NULL OR claim_expires_at < ?)
         )
       )
     ORDER BY created_at ASC
     LIMIT 50`,
  )
    .bind(now, now)
    .all<{
      id: string;
      source_name: string;
      mime_type: string;
      declared_size: number;
      status: DocumentStatus;
    }>();

  const jobs = await env.PRINT_DB.prepare(
    `SELECT id, document_id, user_name, file_name, page_count, copy_count,
            color_mode, total_pages, status
     FROM print_jobs
     WHERE (target_device_id = ? OR (? IS NOT NULL AND target_device_id = ?))
       AND status = 'queued'
       AND (claim_device_id IS NULL OR claim_device_id = ? OR claim_expires_at IS NULL OR claim_expires_at < ?)
     ORDER BY created_at ASC
     LIMIT 50`,
  )
    .bind(deviceId, legacyDeviceId, legacyDeviceId, deviceId, now)
    .all<{
      id: string;
      document_id: string;
      user_name: string;
      file_name: string;
      page_count: number;
      copy_count: number;
      color_mode: ColorMode;
      total_pages: number;
      status: PrintJobRow["status"];
    }>();

  await cleanupExpiredRecords(env).catch((error) => {
    console.warn("[print-sync] pending cleanup failed", error);
  });

  return {
    documents: documents.results ?? [],
    jobs: jobs.results ?? [],
  };
}

export async function claimWork(
  env: PrintSyncBindings,
  input: {
    kind: "document" | "job";
    id: string;
    deviceId: string;
    recoverReady?: boolean;
  },
): Promise<DocumentRow | PrintJobRow> {
  const now = Date.now();
  const claimExpiresAt = now + CLAIM_LEASE_MS;
  const legacyDeviceId = legacyDevicePrefix(input.deviceId);

  if (input.kind === "document") {
    const result = await env.PRINT_DB.prepare(
      `UPDATE print_documents
       SET status = CASE WHEN status IN ('pending', 'ready') THEN 'downloading' ELSE status END,
           claim_device_id = ?,
           claim_expires_at = ?,
           error = NULL
       WHERE id = ?
         AND expires_at > ?
         AND (
           status = 'pending'
           OR (
             status IN ('downloading', 'converting')
             AND (claim_device_id = ? OR claim_expires_at IS NULL OR claim_expires_at < ?)
           )
           OR (
             ? = 1
             AND status = 'ready'
             AND (prepared_device_id = ? OR (? IS NOT NULL AND prepared_device_id = ?))
             AND source_deleted_at IS NULL
           )
         )`,
    )
      .bind(
        input.deviceId,
        claimExpiresAt,
        input.id,
        now,
        input.deviceId,
        now,
        input.recoverReady ? 1 : 0,
        input.deviceId,
        legacyDeviceId,
        legacyDeviceId,
      )
      .run();
    if (!didChange(result)) {
      throw new PrintSyncError(409, "document_not_claimable");
    }
    const row = await getDocument(env, input.id);
    if (!row) {
      throw new PrintSyncError(404, "document_not_found");
    }
    return row;
  }

  const result = await env.PRINT_DB.prepare(
    `UPDATE print_jobs
     SET claim_device_id = ?, claim_expires_at = ?, updated_at = ?
     WHERE id = ?
       AND (target_device_id = ? OR (? IS NOT NULL AND target_device_id = ?))
       AND status = 'queued'
       AND (claim_device_id IS NULL OR claim_device_id = ? OR claim_expires_at IS NULL OR claim_expires_at < ?)`,
  )
    .bind(
      input.deviceId,
      claimExpiresAt,
      now,
      input.id,
      input.deviceId,
      legacyDeviceId,
      legacyDeviceId,
      input.deviceId,
      now,
    )
    .run();
  if (!didChange(result)) {
    throw new PrintSyncError(409, "job_not_claimable");
  }
  const row = await env.PRINT_DB.prepare(`SELECT * FROM print_jobs WHERE id = ?`)
    .bind(input.id)
    .first<PrintJobRow>();
  if (!row) {
    throw new PrintSyncError(404, "job_not_found");
  }
  return row;
}

export async function heartbeatWork(
  env: PrintSyncBindings,
  input: {
    kind: "document" | "job";
    id: string;
    deviceId: string;
    phase?: "downloading" | "converting";
  },
): Promise<void> {
  const now = Date.now();
  const claimExpiresAt = now + CLAIM_LEASE_MS;

  if (input.kind === "document") {
    const result = input.phase
      ? await env.PRINT_DB.prepare(
          `UPDATE print_documents
           SET status = ?, claim_expires_at = ?
           WHERE id = ? AND claim_device_id = ? AND status IN ('downloading', 'converting')`,
        )
          .bind(input.phase, claimExpiresAt, input.id, input.deviceId)
          .run()
      : await env.PRINT_DB.prepare(
          `UPDATE print_documents
           SET claim_expires_at = ?
           WHERE id = ? AND claim_device_id = ? AND status IN ('downloading', 'converting')`,
        )
          .bind(claimExpiresAt, input.id, input.deviceId)
          .run();
    if (!didChange(result)) {
      throw new PrintSyncError(409, "document_claim_lost");
    }
    return;
  }

  const result = await env.PRINT_DB.prepare(
    `UPDATE print_jobs
     SET claim_expires_at = ?, updated_at = ?
     WHERE id = ? AND claim_device_id = ? AND status IN ('queued', 'printing')`,
  )
    .bind(claimExpiresAt, now, input.id, input.deviceId)
    .run();
  if (!didChange(result)) {
    throw new PrintSyncError(409, "job_claim_lost");
  }
}

export async function documentDownloadResponse(
  env: PrintSyncBindings,
  documentId: string,
  deviceId: string,
): Promise<Response> {
  const row = await getDocument(env, documentId);
  if (!row || row.source_deleted_at) {
    throw new PrintSyncError(404, "document_source_not_found");
  }
  if (
    row.claim_device_id !== deviceId
    || !row.claim_expires_at
    || row.claim_expires_at <= Date.now()
    || !["downloading", "converting"].includes(row.status)
  ) {
    throw new PrintSyncError(409, "document_claim_required");
  }

  const object = await env.PRINT_STAGING.get(row.r2_key);
  if (!object) {
    throw new PrintSyncError(404, "document_source_not_found");
  }
  if (object.size !== row.declared_size) {
    throw new PrintSyncError(409, "document_source_size_mismatch");
  }

  const headers = new Headers({
    "Cache-Control": "private, no-store",
    "Content-Length": String(object.size),
    "Content-Type": object.httpMetadata?.contentType || row.mime_type || "application/octet-stream",
    "X-Content-Type-Options": "nosniff",
  });
  if (object.etag) {
    headers.set("ETag", object.etag);
  }
  return new Response(object.body as ReadableStream<Uint8Array>, { headers });
}

export async function confirmDocument(
  env: PrintSyncBindings,
  input: {
    documentId: string;
    deviceId: string;
    sha256: string;
    sizeBytes: number;
    pageCount: number;
    fileName: string;
  },
): Promise<PreparationStatus> {
  const existing = await getDocument(env, input.documentId);
  if (!existing) {
    throw new PrintSyncError(404, "document_not_found");
  }
  if (existing.status === "ready") {
    return preparationStatus(existing);
  }

  const sha256 = input.sha256.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/u.test(sha256)) {
    throw new PrintSyncError(400, "invalid_sha256");
  }
  const sizeBytes = validateSize(input.sizeBytes);
  if (sizeBytes !== existing.declared_size) {
    throw new PrintSyncError(400, "downloaded_size_mismatch");
  }
  const pageCount = Math.floor(input.pageCount);
  if (!Number.isFinite(pageCount) || pageCount < 1 || pageCount > 1000) {
    throw new PrintSyncError(400, "invalid_page_count");
  }
  const fileName = sanitizeOutputName(input.fileName);
  const now = Date.now();
  const result = await env.PRINT_DB.prepare(
    `UPDATE print_documents
     SET status = 'ready',
         display_name = ?,
         actual_size = ?,
         sha256 = ?,
         page_count = ?,
         prepared_device_id = ?,
         prepared_at = ?,
         claim_device_id = NULL,
         claim_expires_at = NULL,
         error = NULL
     WHERE id = ?
       AND claim_device_id = ?
       AND status IN ('downloading', 'converting')`,
  )
    .bind(fileName, sizeBytes, sha256, pageCount, input.deviceId, now, input.documentId, input.deviceId)
    .run();
  if (!didChange(result)) {
    throw new PrintSyncError(409, "document_claim_lost");
  }

  await env.PRINT_DB.prepare(
    `INSERT INTO print_events (kind, entity_id, created_at) VALUES ('document_ready', ?, ?)`,
  )
    .bind(input.documentId, now)
    .run();

  const updated = await getDocument(env, input.documentId);
  if (!updated) {
    throw new PrintSyncError(404, "document_not_found");
  }
  return preparationStatus(updated);
}

export async function failDocument(
  env: PrintSyncBindings,
  input: {
    documentId: string;
    deviceId: string;
    error: string;
  },
): Promise<void> {
  const error = input.error.trim().slice(0, 1000) || "document_preparation_failed";
  const result = await env.PRINT_DB.prepare(
    `UPDATE print_documents
     SET status = 'failed',
         error = ?,
         claim_device_id = NULL,
         claim_expires_at = NULL
     WHERE id = ?
       AND claim_device_id = ?
       AND status IN ('downloading', 'converting')`,
  )
    .bind(error, input.documentId, input.deviceId)
    .run();
  if (!didChange(result)) {
    const existing = await getDocument(env, input.documentId);
    if (!existing || !["ready", "failed", "expired"].includes(existing.status)) {
      throw new PrintSyncError(409, "document_claim_lost");
    }
    if (existing.status === "failed") {
      await deleteDocumentSource(env, existing.id, Date.now());
    }
    return;
  }
  await deleteDocumentSource(env, input.documentId, Date.now());
}

export async function reportJobStatus(
  env: PrintSyncBindings,
  input: {
    jobId: string;
    deviceId: string;
    status: "queued" | "printing" | "done" | "failed";
    detail?: string | null;
    pagesPrinted?: number | null;
    totalPages?: number | null;
  },
): Promise<QueueJobRecord> {
  const existing = await env.PRINT_DB.prepare(`SELECT * FROM print_jobs WHERE id = ?`)
    .bind(input.jobId)
    .first<PrintJobRow>();
  if (!existing) {
    throw new PrintSyncError(404, "job_not_found");
  }
  if (existing.status === "done" || existing.status === "failed") {
    if (existing.status === "done" && input.status === "done") {
      await deleteDocumentSource(env, existing.document_id, Date.now());
    }
    return queueJobRecord(existing);
  }
  if (existing.claim_device_id !== input.deviceId || !deviceCanOwnTarget(input.deviceId, existing.target_device_id)) {
    throw new PrintSyncError(409, "job_claim_lost");
  }
  if (jobStatusRank(input.status) < jobStatusRank(existing.status)) {
    return queueJobRecord(existing);
  }

  const now = Date.now();
  const totalPages = clampInteger(input.totalPages ?? existing.total_pages, 1, existing.total_pages);
  const pagesPrinted = clampInteger(input.pagesPrinted ?? existing.pages_printed, 0, totalPages);
  const detail = input.detail?.trim().slice(0, 1000) || null;
  const claimExpiresAt = input.status === "done" || input.status === "failed" ? null : now + CLAIM_LEASE_MS;

  await env.PRINT_DB.batch([
    env.PRINT_DB.prepare(
      `UPDATE print_jobs
       SET status = ?,
           detail = ?,
           pages_printed = ?,
           total_pages = ?,
           claim_expires_at = ?,
           updated_at = ?
       WHERE id = ? AND claim_device_id = ?`,
    ).bind(input.status, detail, pagesPrinted, totalPages, claimExpiresAt, now, input.jobId, input.deviceId),
    eventStatement(env, `job_${input.status}`, input.jobId, now),
  ]);

  if (input.status === "done") {
    await deleteDocumentSource(env, existing.document_id, now);
  }

  const updated = await env.PRINT_DB.prepare(`SELECT * FROM print_jobs WHERE id = ?`)
    .bind(input.jobId)
    .first<PrintJobRow>();
  if (!updated) {
    throw new PrintSyncError(404, "job_not_found");
  }
  return queueJobRecord(updated);
}

export async function latestEventId(env: PrintSyncBindings): Promise<number> {
  return (
    (await env.PRINT_DB.prepare(`SELECT COALESCE(MAX(id), 0) AS id FROM print_events`).first<number>("id")) ?? 0
  );
}

export async function cleanupExpiredRecords(env: PrintSyncBindings): Promise<void> {
  const now = Date.now();
  const expired = await env.PRINT_DB.prepare(
    `SELECT id, r2_key
     FROM print_documents AS d
     WHERE d.expires_at <= ?
       AND d.status != 'expired'
       AND NOT EXISTS (
         SELECT 1 FROM print_jobs AS j
         WHERE j.document_id = d.id AND j.status IN ('queued', 'printing')
       )
     ORDER BY d.expires_at ASC
     LIMIT 50`,
  )
    .bind(now)
    .all<{ id: string; r2_key: string }>();

  for (const row of expired.results ?? []) {
    await env.PRINT_DB.prepare(
      `UPDATE print_documents
       SET status = 'expired', error = 'document_expired', claim_device_id = NULL, claim_expires_at = NULL
       WHERE id = ? AND status != 'expired'`,
    )
      .bind(row.id)
      .run();
    await env.PRINT_STAGING.delete(row.r2_key).catch((error) => {
      console.warn(`[print-sync] failed to delete expired source ${row.id}`, error);
    });
  }

  await env.PRINT_DB.prepare(
    `DELETE FROM print_handoffs WHERE expires_at <= ? OR consumed_at IS NOT NULL`,
  )
    .bind(now - 60 * 60 * 1000)
    .run();
  await env.PRINT_DB.prepare(
    `DELETE FROM print_events WHERE created_at < ?`,
  )
    .bind(now - 24 * 60 * 60 * 1000)
    .run();
}

export function requireDaemonSecret(request: Request, env: PrintSyncBindings): string {
  const expected = env.PRINT_SYNC_SECRET?.trim();
  const received = request.headers.get("x-print-sync-secret")?.trim();
  if (!expected || !received || received !== expected) {
    throw new PrintSyncError(401, "unauthorized");
  }
  const deviceId = sanitizeDeviceId(request.headers.get("x-device-id") || "");
  if (!deviceId) {
    throw new PrintSyncError(400, "device_id_required");
  }
  return deviceId;
}

export function requireNormalPicsSecret(request: Request, env: PrintSyncBindings): void {
  const expected = env.NORMALPICS_HANDOFF_SECRET?.trim();
  const received = extractBearer(request.headers.get("authorization"));
  if (!expected || !received || received !== expected) {
    throw new PrintSyncError(401, "unauthorized");
  }
}

export function allowedNormalPicsOrigin(request: Request, env: PrintSyncBindings): string | null {
  const origin = request.headers.get("origin");
  if (!origin) {
    return null;
  }
  const allowed = new Set(
    (env.NORMALPICS_ORIGINS || "https://pics.example.com")
      .split(/[,\s;]+/u)
      .map((value) => value.trim())
      .filter(Boolean),
  );
  return allowed.has(origin) ? origin : null;
}

export function jsonError(error: unknown): Response {
  if (error instanceof PrintSyncError) {
    return Response.json({ error: error.message }, { status: error.status });
  }
  console.error("[print-sync] unhandled error", error);
  return Response.json({ error: "internal_error" }, { status: 500 });
}

async function getDocument(env: PrintSyncBindings, documentId: string): Promise<DocumentRow | null> {
  return env.PRINT_DB.prepare(`SELECT * FROM print_documents WHERE id = ?`)
    .bind(documentId)
    .first<DocumentRow>();
}

async function deleteDocumentSource(env: PrintSyncBindings, documentId: string, now: number): Promise<void> {
  const document = await getDocument(env, documentId);
  if (!document || document.source_deleted_at) {
    return;
  }
  try {
    await env.PRINT_STAGING.delete(document.r2_key);
    await env.PRINT_DB.prepare(
      `UPDATE print_documents SET source_deleted_at = ? WHERE id = ? AND source_deleted_at IS NULL`,
    )
      .bind(now, document.id)
      .run();
  } catch (error) {
    console.warn("[print-sync] failed to delete R2 source", error);
  }
}

async function requireDocumentToken(
  env: PrintSyncBindings,
  token: string,
  expectedDocumentId: string,
): Promise<DocumentTokenPayload> {
  const payload = await verifyDocumentToken(env.UPLOAD_SIGNING_SECRET, token);
  if (!payload || payload.document_id !== expectedDocumentId) {
    throw new PrintSyncError(401, "invalid_document_token");
  }
  return payload;
}

function preparationStatus(row: DocumentRow): PreparationStatus {
  return {
    document_id: row.id,
    status: row.status,
    source_name: row.source_name,
    file_name: row.display_name || convertedPdfName(row.source_name),
    page_count: row.page_count,
    declared_size: row.declared_size,
    actual_size: row.actual_size,
    error: row.error,
    created_at: new Date(row.created_at).toISOString(),
    uploaded_at: row.uploaded_at ? new Date(row.uploaded_at).toISOString() : null,
    prepared_at: row.prepared_at ? new Date(row.prepared_at).toISOString() : null,
    expires_at: new Date(row.expires_at).toISOString(),
  };
}

function queueJobRecord(row: PrintJobRow): QueueJobRecord {
  return {
    id: row.id,
    user_name: row.user_name,
    file_name: row.file_name,
    page_count: row.page_count,
    copy_count: row.copy_count,
    color_mode: row.color_mode,
    status: row.status,
    submitted_at: new Date(row.created_at).toISOString(),
    detail: row.detail ?? undefined,
    pages_printed: row.pages_printed,
    total_pages: row.total_pages,
  };
}

function eventStatement(env: PrintSyncBindings, kind: string, entityId: string, now: number): D1PreparedStatement {
  return env.PRINT_DB.prepare(
    `INSERT INTO print_events (kind, entity_id, created_at) VALUES (?, ?, ?)`,
  ).bind(kind, entityId, now);
}

async function expireDocument(env: PrintSyncBindings, row: DocumentRow): Promise<void> {
  await env.PRINT_DB.prepare(
    `UPDATE print_documents
     SET status = 'expired', error = 'document_expired', claim_device_id = NULL, claim_expires_at = NULL
     WHERE id = ?`,
  )
    .bind(row.id)
    .run();
  await env.PRINT_STAGING.delete(row.r2_key).catch(() => undefined);
}

function didChange(result: D1Result | { meta?: { changes?: number } }): boolean {
  return Number(result.meta?.changes || 0) > 0;
}

function validateSize(value: number): number {
  const size = Math.floor(value);
  if (!Number.isFinite(size) || size <= 0 || size > MAX_UPLOAD_SIZE_BYTES) {
    throw new PrintSyncError(400, "invalid_file_size");
  }
  return size;
}

function validateSupportedSource(fileName: string): void {
  const extension = fileName.toLowerCase().split(".").pop() || "";
  if (!SUPPORTED_EXTENSIONS.has(extension)) {
    throw new PrintSyncError(415, "unsupported_file_type");
  }
}

function sanitizeSourceName(value: string): string {
  const cleaned = value
    .replace(/[<>:"/\\|?*\u0000-\u001f]/gu, "")
    .replace(/\s+/gu, " ")
    .trim()
    .slice(0, 240);
  if (!cleaned || !cleaned.includes(".")) {
    throw new PrintSyncError(400, "invalid_file_name");
  }
  return cleaned;
}

function sanitizeOutputName(value: string): string {
  const cleaned = value
    .replace(/[<>:"/\\|?*\u0000-\u001f]/gu, "")
    .replace(/\s+/gu, " ")
    .trim()
    .slice(0, 240);
  return cleaned.toLowerCase().endsWith(".pdf") ? cleaned : `${cleaned || "document"}.pdf`;
}

function sanitizeMimeType(value: string): string {
  const mimeType = value.trim().toLowerCase().slice(0, 120);
  return /^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+$/u.test(mimeType)
    ? mimeType
    : "application/octet-stream";
}

function sanitizeSourceType(value?: string): string {
  const sourceType = value?.trim().toLowerCase().replace(/[^a-z0-9_-]/gu, "").slice(0, 32);
  return sourceType || "web";
}

function sanitizeDeviceId(value: string): string {
  return value.trim().replace(/[^a-zA-Z0-9_.:-]/gu, "").slice(0, 120);
}

function legacyDevicePrefix(deviceId: string): string | null {
  const separator = deviceId.lastIndexOf(":");
  return separator > 0 ? deviceId.slice(0, separator) : null;
}

function deviceCanOwnTarget(deviceId: string, targetDeviceId: string): boolean {
  return targetDeviceId === deviceId || legacyDevicePrefix(deviceId) === targetDeviceId;
}

function convertedPdfName(sourceName: string): string {
  const stem = sourceName.replace(/\.[^.]+$/u, "").trim() || "document";
  return `${stem}.pdf`;
}

function stagingKey(documentId: string, now: number): string {
  const day = new Date(now).toISOString().slice(0, 10);
  return `print-staging/${day}/${documentId}`;
}

function roundMoney(value: number): number {
  return Math.round((value + Number.EPSILON) * 100) / 100;
}

function jobStatusRank(status: PrintJobRow["status"]): number {
  if (status === "queued") {
    return 0;
  }
  if (status === "printing") {
    return 1;
  }
  return 2;
}

function clampInteger(value: number, min: number, max: number): number {
  const integer = Math.floor(Number.isFinite(value) ? value : min);
  return Math.max(min, Math.min(max, integer));
}

function extractBearer(value: string | null): string | null {
  const match = value?.match(/^Bearer\s+(.+)$/iu);
  return match?.[1]?.trim() || null;
}

function randomToken(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return base64UrlEncode(bytes);
}

function r2Client(env: PrintSyncBindings): AwsClient {
  return new AwsClient({
    accessKeyId: env.R2_ACCESS_KEY_ID,
    secretAccessKey: env.R2_SECRET_ACCESS_KEY,
    service: "s3",
    region: "auto",
  });
}

function r2ObjectUrl(env: PrintSyncBindings, key: string): URL {
  const safeKey = key.split("/").map(encodeURIComponent).join("/");
  return new URL(`https://${env.R2_ACCOUNT_ID}.r2.cloudflarestorage.com/${env.R2_BUCKET_NAME}/${safeKey}`);
}

async function generatePresignedPut(
  env: PrintSyncBindings,
  key: string,
  contentType: string,
  expiresIn: number,
): Promise<string> {
  const url = r2ObjectUrl(env, key);
  url.searchParams.set("X-Amz-Expires", String(expiresIn));
  const signed = await r2Client(env).sign(
    new Request(url, {
      method: "PUT",
      headers: {
        "Content-Type": contentType,
      },
    }),
    {
      aws: {
        signQuery: true,
        allHeaders: true,
      },
    },
  );
  return signed.url;
}

async function signDocumentToken(secret: string, payload: DocumentTokenPayload): Promise<string> {
  const encodedPayload = base64UrlEncode(new TextEncoder().encode(JSON.stringify(payload)));
  const signature = await signHmac(secret, encodedPayload);
  return `${encodedPayload}.${base64UrlEncode(signature)}`;
}

async function verifyDocumentToken(secret: string, token: string): Promise<DocumentTokenPayload | null> {
  const [encodedPayload, encodedSignature] = token.trim().split(".");
  if (!encodedPayload || !encodedSignature) {
    return null;
  }
  const expected = await signHmac(secret, encodedPayload);
  if (!timingSafeEqual(expected, base64UrlDecode(encodedSignature))) {
    return null;
  }
  try {
    const payload = JSON.parse(new TextDecoder().decode(base64UrlDecode(encodedPayload))) as DocumentTokenPayload;
    if (payload.kind !== "document" || !payload.document_id || payload.exp <= Date.now()) {
      return null;
    }
    return payload;
  } catch {
    return null;
  }
}

async function signHmac(secret: string, value: string): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(value));
  return new Uint8Array(signature);
}

async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function timingSafeEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) {
    return false;
  }
  let difference = 0;
  for (let index = 0; index < left.byteLength; index += 1) {
    difference |= left[index] ^ right[index];
  }
  return difference === 0;
}

function base64UrlEncode(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64url");
}

function base64UrlDecode(value: string): Uint8Array {
  return new Uint8Array(Buffer.from(value, "base64url"));
}
