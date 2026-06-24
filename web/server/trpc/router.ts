import { initTRPC, TRPCError } from "@trpc/server";
import { z } from "zod";

import {
  COLOR_MODES,
  KV_KEYS,
  createQueueJobRecord,
  type PricesConfig,
  type PrintersConfig,
  type QRCodesConfig,
  type QueueJobRecord,
} from "../../../cloudflare/kv-schema";
import {
  PrintSyncError,
  createUploadSession,
  consumePrintHandoff,
  getPreparationStatus,
  getPrintJobStatus,
  notifyUpload,
  submitPrintJob as submitSyncedPrintJob,
  type PrintSyncBindings,
} from "../print-sync/service";

export interface KvNamespaceLike {
  get(key: string, type: "text"): Promise<string | null>;
  get<T>(key: string, type: "json"): Promise<T | null>;
  put(key: string, value: string): Promise<void>;
}

export interface AppBindings extends PrintSyncBindings {
  PRINT_KV: KvNamespaceLike;
  PRINT_SHARED_SECRET: string;
  LOCAL_SERVER_BASE_URL: string;
  UPLOAD_SIGNING_SECRET: string;
  ADMIN_TOKEN?: string;
  DEFAULT_BW_PRICE?: string;
  DEFAULT_COLOR_PRICE?: string;
  DEFAULT_ALIPAY_QR?: string;
  DEFAULT_WECHAT_QR?: string;
  DEFAULT_NOTICE_MARKDOWN?: string;
  DEFAULT_BW_PRINTER?: string;
  DEFAULT_COLOR_PRINTER?: string;
}

export interface TrpcContext {
  request: Request;
  env: AppBindings;
  kv: KvNamespaceLike;
}

const userNameSchema = z
  .string()
  .trim()
  .min(2, "\u{7528}\u{6237}\u{540d}\u{81f3}\u{5c11}\u{9700}\u{8981} 2 \u{4e2a}\u{5b57}\u{7b26}\u{3002}")
  .max(24, "\u{7528}\u{6237}\u{540d}\u{6700}\u{591a} 24 \u{4e2a}\u{5b57}\u{7b26}\u{3002}")
  .refine(
    (value) => /^[\p{L}\p{N}_\-\s]+$/u.test(value),
    "\u{7528}\u{6237}\u{540d}\u{4ec5}\u{652f}\u{6301}\u{4e2d}\u{82f1}\u{6587}\u{3001}\u{6570}\u{5b57}\u{3001}\u{7a7a}\u{683c}\u{3001}\u{4e0b}\u{5212}\u{7ebf}\u{548c}\u{77ed}\u{6a2a}\u{7ebf}\u{3002}",
  );

const pricesSchema = z.object({
  bw_per_page: z.number().nonnegative(),
  color_per_page: z.number().nonnegative(),
});

const qrCodesSchema = z.object({
  alipay_url: z.string().min(1),
  wechat_url: z.string().min(1),
});

const printersSchema = z.object({
  bw: z.string().min(1),
  color: z.string().min(1),
});

const MAX_COPY_COUNT = 5;
const MAX_TOTAL_PRINT_PAGES = 60;
const MAX_UPLOAD_SIZE_BYTES = 256 * 1024 * 1024;
const PREPARE_WS_CHUNK_SIZE_BYTES = 512 * 1024;
const PREPARE_WS_LANE_COUNT = 32;

const localServerTargetSchema = z.object({
  upload_url: z.string().url(),
  raw_upload_url: z.string().url().optional(),
  chunk_upload_url: z.string().url().optional(),
  complete_upload_url: z.string().url().optional(),
  websocket_upload_url: z.string().url().optional(),
  chunk_size_bytes: z.number().int().positive().optional(),
  lane_count: z.number().int().positive().optional(),
  upload_token: z.string().min(1),
  expires_at: z.string().min(1),
});

const directPrepareUploadInputSchema = z
  .object({
    upload_id: z.string().uuid(),
    total_bytes: z.number().int().positive().max(MAX_UPLOAD_SIZE_BYTES),
  })
  .optional();

const documentIdSchema = z.string().uuid();
const documentTokenSchema = z.string().trim().min(20).max(4096);

const queueJobSchema = z.object({
  id: z.string().min(1),
  user_name: z.string().min(1),
  file_name: z.string().min(1),
  page_count: z.number().int().positive(),
  copy_count: z.number().int().positive().max(MAX_COPY_COUNT).optional(),
  color_mode: z.enum(COLOR_MODES),
  status: z.enum(["queued", "downloading", "printing", "done", "failed"]),
  submitted_at: z.string().min(1),
  detail: z.string().nullable().optional(),
  pages_printed: z.number().int().nonnegative().optional(),
  total_pages: z.number().int().positive().optional(),
});

const t = initTRPC.context<TrpcContext>().create();

const publicProcedure = t.procedure;
const adminProcedure = t.procedure.use(async ({ ctx, next }) => {
  const expected = ctx.env.ADMIN_TOKEN;
  const received =
    extractBearer(ctx.request.headers.get("authorization")) ?? ctx.request.headers.get("x-admin-token") ?? undefined;

  if (!expected || received !== expected) {
    throw new TRPCError({ code: "UNAUTHORIZED", message: "Admin token required." });
  }

  return next();
});

export function createTrpcContext(request: Request, env: AppBindings): TrpcContext {
  return {
    request,
    env,
    kv: env.PRINT_KV,
  };
}

export const configRouter = t.router({
  getPrices: publicProcedure.query(async ({ ctx }) => {
    return readJson<PricesConfig>(ctx.kv, KV_KEYS.prices, defaultPrices(ctx.env));
  }),
  getQRCodes: publicProcedure.query(async ({ ctx }) => {
    const qrCodes = await readJson<QRCodesConfig>(ctx.kv, KV_KEYS.qrcodes, defaultQRCodes(ctx.env));

    return {
      alipay_url: qrCodes.alipay_url ? "/api/qrcode/alipay" : "",
      wechat_url: qrCodes.wechat_url ? "/api/qrcode/wechat" : "",
    };
  }),
  getNotice: publicProcedure.query(async ({ ctx }) => {
    return (await ctx.kv.get(KV_KEYS.notice, "text")) ?? ctx.env.DEFAULT_NOTICE_MARKDOWN ?? "";
  }),
  getLocalServerUrl: publicProcedure.query(async ({ ctx }) => {
    return ctx.env.LOCAL_SERVER_BASE_URL ?? "";
  }),
  createDirectPreviewUpload: publicProcedure.output(localServerTargetSchema).mutation(async ({ ctx }) => {
    const localServerBaseUrl = requireBinding(ctx.env.LOCAL_SERVER_BASE_URL, "LOCAL_SERVER_BASE_URL");
    const printSharedSecret = requireBinding(ctx.env.PRINT_SHARED_SECRET, "PRINT_SHARED_SECRET");
    const expiresAt = Date.now() + 5 * 60_000;
    const uploadToken = await createDirectUploadToken(printSharedSecret, {
      kind: "preview",
      exp: expiresAt,
    });

    return {
      upload_url: joinUrl(localServerBaseUrl, "/api/convert-preview"),
      raw_upload_url: joinUrl(localServerBaseUrl, "/api/convert-preview/raw"),
      upload_token: uploadToken,
      expires_at: new Date(expiresAt).toISOString(),
    };
  }),
  createDirectPrepareUpload: publicProcedure
    .input(directPrepareUploadInputSchema)
    .output(localServerTargetSchema)
    .mutation(async ({ ctx, input }) => {
      const localServerBaseUrl = requireBinding(ctx.env.LOCAL_SERVER_BASE_URL, "LOCAL_SERVER_BASE_URL");
      const printSharedSecret = requireBinding(ctx.env.PRINT_SHARED_SECRET, "PRINT_SHARED_SECRET");
      const expiresAt = Date.now() + 15 * 60_000;
      const uploadToken = await createDirectUploadToken(printSharedSecret, input
        ? {
            kind: "prepare_ws",
            upload_id: input.upload_id,
            total_bytes: input.total_bytes,
            exp: expiresAt,
          }
        : {
            kind: "prepare",
            exp: expiresAt,
          });

      return {
        upload_url: joinUrl(localServerBaseUrl, "/api/prepare/raw"),
        raw_upload_url: joinUrl(localServerBaseUrl, "/api/prepare/raw"),
        chunk_upload_url: joinUrl(localServerBaseUrl, "/api/prepare/chunk"),
        complete_upload_url: joinUrl(localServerBaseUrl, "/api/prepare/complete"),
        websocket_upload_url: input ? joinWebSocketUrl(localServerBaseUrl, "/ws/prepare") : undefined,
        chunk_size_bytes: input ? PREPARE_WS_CHUNK_SIZE_BYTES : undefined,
        lane_count: input ? PREPARE_WS_LANE_COUNT : undefined,
        upload_token: uploadToken,
        expires_at: new Date(expiresAt).toISOString(),
      };
    }),
  getPrinters: adminProcedure.query(async ({ ctx }) => {
    return readJson<PrintersConfig>(ctx.kv, KV_KEYS.printers, defaultPrinters(ctx.env));
  }),
  setPrices: adminProcedure.input(pricesSchema).mutation(async ({ ctx, input }) => {
    await ctx.kv.put(KV_KEYS.prices, JSON.stringify(input));
    return input;
  }),
  setQRCodes: adminProcedure.input(qrCodesSchema).mutation(async ({ ctx, input }) => {
    await ctx.kv.put(KV_KEYS.qrcodes, JSON.stringify(input));
    return input;
  }),
  setNotice: adminProcedure
    .input(z.object({ markdown: z.string() }))
    .mutation(async ({ ctx, input }) => {
      await ctx.kv.put(KV_KEYS.notice, input.markdown);
      return { markdown: input.markdown };
    }),
  setPrinters: adminProcedure.input(printersSchema).mutation(async ({ ctx, input }) => {
    await ctx.kv.put(KV_KEYS.printers, JSON.stringify(input));
    return input;
  }),
});

export const documentRouter = t.router({
  createUploadSession: publicProcedure
    .input(
      z.object({
        file_name: z.string().trim().min(1).max(240),
        mime_type: z.string().trim().max(120).default("application/octet-stream"),
        size_bytes: z.number().int().positive().max(MAX_UPLOAD_SIZE_BYTES),
      }),
    )
    .mutation(async ({ ctx, input }) => {
      try {
        return await createUploadSession(ctx.env, {
          fileName: input.file_name,
          mimeType: input.mime_type,
          sizeBytes: input.size_bytes,
        });
      } catch (error) {
        throw asTrpcError(error);
      }
    }),
  notifyUpload: publicProcedure
    .input(
      z.object({
        document_id: documentIdSchema,
        document_token: documentTokenSchema,
      }),
    )
    .mutation(async ({ ctx, input }) => {
      try {
        return await notifyUpload(ctx.env, input.document_id, input.document_token);
      } catch (error) {
        throw asTrpcError(error);
      }
    }),
  getPreparationStatus: publicProcedure
    .input(
      z.object({
        document_id: documentIdSchema,
        document_token: documentTokenSchema,
      }),
    )
    .query(async ({ ctx, input }) => {
      try {
        return await getPreparationStatus(ctx.env, input.document_id, input.document_token);
      } catch (error) {
        throw asTrpcError(error);
      }
    }),
  consumePrintHandoff: publicProcedure
    .input(z.object({ handoff_token: z.string().trim().min(20).max(256) }))
    .mutation(async ({ ctx, input }) => {
      try {
        return await consumePrintHandoff(ctx.env, input.handoff_token);
      } catch (error) {
        throw asTrpcError(error);
      }
    }),
});

export const queueRouter = t.router({
  submitPrintJob: publicProcedure
    .input(
      z.object({
        document_id: documentIdSchema,
        document_token: documentTokenSchema,
        user_name: userNameSchema,
        color_mode: z.enum(COLOR_MODES),
        copy_count: z.number().int().positive().max(MAX_COPY_COUNT).default(1),
      }),
    )
    .mutation(async ({ ctx, input }) => {
      const normalizedName = normalizeUserName(input.user_name);
      const registry = await readStringArray(ctx.kv, KV_KEYS.usersRegistry);
      if (!registry.some((registeredName) => registeredName === normalizedName)) {
        throw new TRPCError({
          code: "PRECONDITION_FAILED",
          message: "\u{7528}\u{6237}\u{540d}\u{672a}\u{6ce8}\u{518c}\u{6216}\u{5df2}\u{5931}\u{6548}\uff0c\u{8bf7}\u{91cd}\u{65b0}\u{786e}\u{8ba4}\u{59d3}\u{540d}\u{3002}",
        });
      }

      try {
        const prices = await readJson<PricesConfig>(ctx.kv, KV_KEYS.prices, defaultPrices(ctx.env));
        return await submitSyncedPrintJob(ctx.env, {
          documentId: input.document_id,
          documentToken: input.document_token,
          userName: normalizedName,
          colorMode: input.color_mode,
          copyCount: input.copy_count,
          prices,
        });
      } catch (error) {
        throw asTrpcError(error);
      }
    }),
  submitJob: publicProcedure
    .input(
      z.object({
        user_name: userNameSchema,
        color_mode: z.enum(COLOR_MODES),
        page_count: z.number().int().positive().max(1000),
        copy_count: z.number().int().positive().max(MAX_COPY_COUNT).default(1),
        file_name: z.string().trim().min(1).max(240),
        prepared_id: z
          .string()
          .trim()
          .regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu)
          .optional(),
        preview_cache_id: z
          .string()
          .trim()
          .regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu)
          .optional(),
      }),
    )
    .mutation(async ({ ctx, input }) => {
      const normalizedName = normalizeUserName(input.user_name);
      const registry = await readStringArray(ctx.kv, KV_KEYS.usersRegistry);
      const totalPrintPages = input.page_count * input.copy_count;

      if (totalPrintPages > MAX_TOTAL_PRINT_PAGES) {
        throw new TRPCError({
          code: "BAD_REQUEST",
          message: "这份任务页数偏多，请减少份数或拆分文件后再提交。",
        });
      }

      if (!registry.some((registeredName) => registeredName === normalizedName)) {
        throw new TRPCError({
          code: "PRECONDITION_FAILED",
          message: "\u{7528}\u{6237}\u{540d}\u{672a}\u{6ce8}\u{518c}\u{6216}\u{5df2}\u{5931}\u{6548}\uff0c\u{8bf7}\u{91cd}\u{65b0}\u{786e}\u{8ba4}\u{59d3}\u{540d}\u{3002}",
        });
      }

      const jobId = crypto.randomUUID();
      const submittedAt = new Date().toISOString();
      const fileName = sanitizeFileName(input.file_name);
      const preparedId = input.prepared_id ?? input.preview_cache_id;
      const job = createQueueJobRecord({
        id: jobId,
        user_name: normalizedName,
        file_name: fileName,
        page_count: input.page_count,
        copy_count: input.copy_count,
        color_mode: input.color_mode,
        status: "downloading",
        submitted_at: submittedAt,
        detail: "正在接收文件，尚未进入打印队列。",
        pages_printed: 0,
        total_pages: totalPrintPages,
      });

      await writeQueueJob(ctx.kv, job);

      const expiresAt = Date.now() + 10 * 60_000;
      const printSharedSecret = requireBinding(ctx.env.PRINT_SHARED_SECRET, "PRINT_SHARED_SECRET");
      const directUploadToken = await createDirectUploadToken(printSharedSecret, {
        kind: "print",
        job_id: jobId,
        preview_id: preparedId,
        page_count: input.page_count,
        copy_count: input.copy_count,
        exp: expiresAt,
      });
      const localServerBaseUrl = requireBinding(ctx.env.LOCAL_SERVER_BASE_URL, "LOCAL_SERVER_BASE_URL");

      return {
        job_id: jobId,
        direct_upload_url: joinUrl(localServerBaseUrl, "/api/print"),
        direct_upload_token: directUploadToken,
        expires_at: new Date(expiresAt).toISOString(),
      };
    }),
  reportSubmissionFailure: publicProcedure
    .input(
      z.object({
        job_id: z.string().min(1),
        direct_upload_token: z.string().min(1),
        detail: z.string().trim().max(500).optional(),
      }),
    )
    .mutation(async ({ ctx, input }) => {
      const printSharedSecret = requireBinding(ctx.env.PRINT_SHARED_SECRET, "PRINT_SHARED_SECRET");
      await verifyDirectUploadToken(printSharedSecret, input.direct_upload_token, "print", input.job_id);

      const job = await readQueueJob(ctx.kv, input.job_id);
      if (!job || job.status !== "downloading") {
        return { ok: true };
      }

      await writeQueueJob(ctx.kv, {
        ...job,
        status: "failed",
        detail: input.detail || "文件未能成功送达本地打印服务，请重新提交。",
        pages_printed: job.pages_printed ?? 0,
        total_pages: job.total_pages ?? job.page_count * (job.copy_count ?? 1),
      });
      await removeActiveJob(ctx.kv, input.job_id);

      return { ok: true };
    }),
  getJobStatus: publicProcedure.input(z.object({ job_id: z.string().min(1) })).query(async ({ ctx, input }) => {
    try {
      return (await getPrintJobStatus(ctx.env, input.job_id)) ?? (await readQueueJob(ctx.kv, input.job_id));
    } catch {
      return readQueueJob(ctx.kv, input.job_id);
    }
  }),
});

export const userRouter = t.router({
  checkNameAvailable: publicProcedure.input(z.object({ name: userNameSchema })).query(async ({ ctx, input }) => {
    const registry = await readStringArray(ctx.kv, KV_KEYS.usersRegistry);
    const normalized = normalizeUserName(input.name);
    return {
      name: normalized,
      available: !registry.includes(normalized),
    };
  }),
  registerName: publicProcedure.input(z.object({ name: userNameSchema })).mutation(async ({ ctx, input }) => {
    const normalized = normalizeUserName(input.name);

    for (let attempt = 0; attempt < 3; attempt += 1) {
      const registry = await readStringArray(ctx.kv, KV_KEYS.usersRegistry);
      if (registry.includes(normalized)) {
        throw new TRPCError({
          code: "CONFLICT",
          message: "\u{8be5}\u{7528}\u{6237}\u{540d}\u{5df2}\u{88ab}\u{5360}\u{7528}\uff0c\u{8bf7}\u{66f4}\u{6362}\u{4e00}\u{4e2a}\u{3002}",
        });
      }

      const nextRegistry = [...registry, normalized].sort((left, right) => left.localeCompare(right, "zh-Hans-CN"));
      await ctx.kv.put(KV_KEYS.usersRegistry, JSON.stringify(nextRegistry));

      const verifiedRegistry = await readStringArray(ctx.kv, KV_KEYS.usersRegistry);
      if (verifiedRegistry.includes(normalized)) {
        return {
          name: normalized,
          registry_size: verifiedRegistry.length,
        };
      }
    }

    throw new TRPCError({
      code: "INTERNAL_SERVER_ERROR",
      message: "\u{7528}\u{6237}\u{540d}\u{6ce8}\u{518c}\u{5931}\u{8d25}\uff0c\u{8bf7}\u{7a0d}\u{540e}\u{91cd}\u{8bd5}\u{3002}",
    });
  }),
});

export const appRouter = t.router({
  config: configRouter,
  document: documentRouter,
  queue: queueRouter,
  user: userRouter,
});

export type AppRouter = typeof appRouter;

export interface UploadTokenPayload {
  jobId: string;
  exp: number;
}

export interface DirectUploadTokenPayload {
  kind: "prepare" | "prepare_ws" | "preview" | "print";
  upload_id?: string;
  total_bytes?: number;
  job_id?: string;
  preview_id?: string;
  page_count?: number;
  copy_count?: number;
  exp: number;
}

export async function createUploadToken(secret: string, payload: UploadTokenPayload): Promise<string> {
  const encodedPayload = encodeBase64Url(JSON.stringify(payload));
  const signature = await signHmacSha256(secret, encodedPayload);
  return `${encodedPayload}.${signature}`;
}

export async function createDirectUploadToken(secret: string, payload: DirectUploadTokenPayload): Promise<string> {
  const encodedPayload = encodeBase64Url(JSON.stringify(payload));
  const signature = await signHmacSha256(secret, encodedPayload);
  return `${encodedPayload}.${signature}`;
}

export async function verifyDirectUploadToken(
  secret: string,
  token: string,
  expectedKind: DirectUploadTokenPayload["kind"],
  expectedJobId?: string,
  expectedPreviewId?: string,
): Promise<DirectUploadTokenPayload> {
  const [encodedPayload, signature] = token.split(".");
  if (!encodedPayload || !signature) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "上传令牌格式无效。",
    });
  }

  const expectedSignature = await signHmacSha256(secret, encodedPayload);
  if (signature !== expectedSignature) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "上传令牌校验失败。",
    });
  }

  const payload = JSON.parse(decodeBase64Url(encodedPayload)) as DirectUploadTokenPayload;
  if (payload.kind !== expectedKind) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "上传令牌类型不匹配。",
    });
  }

  if (expectedJobId && payload.job_id !== expectedJobId) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "上传令牌与任务不匹配。",
    });
  }

  if (expectedPreviewId && payload.preview_id !== expectedPreviewId) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "上传令牌与预览缓存不匹配。",
    });
  }

  if (payload.exp < Date.now()) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "上传令牌已过期，请重新提交任务。",
    });
  }

  return payload;
}

export async function verifyUploadToken(
  secret: string,
  token: string,
  expectedJobId: string,
): Promise<UploadTokenPayload> {
  const [encodedPayload, signature] = token.split(".");
  if (!encodedPayload || !signature) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "\u{4e0a}\u{4f20}\u{4ee4}\u{724c}\u{683c}\u{5f0f}\u{65e0}\u{6548}\u{3002}",
    });
  }

  const expectedSignature = await signHmacSha256(secret, encodedPayload);
  if (signature !== expectedSignature) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "\u{4e0a}\u{4f20}\u{4ee4}\u{724c}\u{6821}\u{9a8c}\u{5931}\u{8d25}\u{3002}",
    });
  }

  const payload = JSON.parse(decodeBase64Url(encodedPayload)) as UploadTokenPayload;
  if (payload.jobId !== expectedJobId) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "\u{4e0a}\u{4f20}\u{4ee4}\u{724c}\u{4e0e}\u{4efb}\u{52a1}\u{4e0d}\u{5339}\u{914d}\u{3002}",
    });
  }
  if (payload.exp < Date.now()) {
    throw new TRPCError({
      code: "UNAUTHORIZED",
      message: "\u{4e0a}\u{4f20}\u{4ee4}\u{724c}\u{5df2}\u{8fc7}\u{671f}\uff0c\u{8bf7}\u{91cd}\u{65b0}\u{63d0}\u{4ea4}\u{4efb}\u{52a1}\u{3002}",
    });
  }

  return payload;
}

export async function readQueueJob(kv: KvNamespaceLike, jobId: string): Promise<QueueJobRecord | null> {
  const value = await kv.get<QueueJobRecord>(KV_KEYS.queueJob(jobId), "json");
  if (!value) {
    return null;
  }

  const parsed = queueJobSchema.parse(value);
  return {
    ...parsed,
    detail: parsed.detail ?? undefined,
  };
}

export async function writeQueueJob(kv: KvNamespaceLike, job: QueueJobRecord): Promise<void> {
  await kv.put(KV_KEYS.queueJob(job.id), JSON.stringify(job));
}

export async function appendActiveJob(kv: KvNamespaceLike, jobId: string): Promise<string[]> {
  const queue = await readStringArray(kv, KV_KEYS.queueActive);
  if (!queue.includes(jobId)) {
    queue.push(jobId);
    await kv.put(KV_KEYS.queueActive, JSON.stringify(queue));
  }
  return queue;
}

export async function removeActiveJob(kv: KvNamespaceLike, jobId: string): Promise<string[]> {
  const queue = (await readStringArray(kv, KV_KEYS.queueActive)).filter((queuedJobId) => queuedJobId !== jobId);
  await kv.put(KV_KEYS.queueActive, JSON.stringify(queue));
  return queue;
}

export async function readStringArray(kv: KvNamespaceLike, key: string): Promise<string[]> {
  const value = await kv.get<string[]>(key, "json");
  if (!value) {
    return [];
  }

  if (!Array.isArray(value)) {
    throw new TRPCError({ code: "INTERNAL_SERVER_ERROR", message: `KV key ${key} does not contain a string array.` });
  }

  return value
    .filter((entry): entry is string => typeof entry === "string")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

export async function readJson<T>(kv: KvNamespaceLike, key: string, fallback: T): Promise<T> {
  try {
    const value = await kv.get<T>(key, "json");
    return value ?? fallback;
  } catch {
    return fallback;
  }
}

export function normalizeUserName(name: string): string {
  return name.trim().replace(/\s+/g, " ");
}

export function sanitizeFileName(fileName: string): string {
  const cleaned = fileName.replace(/[<>:"/\\|?*\u0000-\u001F]/g, "").trim();
  if (!cleaned) {
    return "document.pdf";
  }

  return cleaned.toLowerCase().endsWith(".pdf") ? cleaned : `${cleaned}.pdf`;
}

function requireBinding(value: string | undefined, name: string): string {
  if (!value?.trim()) {
    throw new TRPCError({
      code: "INTERNAL_SERVER_ERROR",
      message: `Cloudflare Worker variable ${name} is missing.`,
    });
  }

  return value.trim();
}

function joinUrl(baseUrl: string, path: string): string {
  return new URL(path, baseUrl.endsWith("/") ? baseUrl : `${baseUrl}/`).toString();
}

function joinWebSocketUrl(baseUrl: string, path: string): string {
  const url = new URL(path, baseUrl.endsWith("/") ? baseUrl : `${baseUrl}/`);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

function defaultPrices(env: AppBindings): PricesConfig {
  return {
    bw_per_page: parseFiniteNumber(env.DEFAULT_BW_PRICE),
    color_per_page: parseFiniteNumber(env.DEFAULT_COLOR_PRICE),
  };
}

function defaultQRCodes(env: AppBindings): QRCodesConfig {
  return {
    alipay_url: env.DEFAULT_ALIPAY_QR ?? "",
    wechat_url: env.DEFAULT_WECHAT_QR ?? "",
  };
}

function defaultPrinters(env: AppBindings): PrintersConfig {
  return {
    bw: env.DEFAULT_BW_PRINTER ?? "",
    color: env.DEFAULT_COLOR_PRINTER ?? "",
  };
}

function parseFiniteNumber(value?: string): number {
  if (!value) {
    return 0;
  }

  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function extractBearer(headerValue: string | null): string | undefined {
  if (!headerValue?.startsWith("Bearer ")) {
    return undefined;
  }
  return headerValue.slice("Bearer ".length).trim();
}

function asTrpcError(error: unknown): TRPCError {
  if (!(error instanceof PrintSyncError)) {
    console.error("[print-sync] tRPC error", error);
    return new TRPCError({ code: "INTERNAL_SERVER_ERROR", message: "Print service request failed." });
  }

  const code =
    error.status === 400
      ? "BAD_REQUEST"
      : error.status === 401
        ? "UNAUTHORIZED"
        : error.status === 404
          ? "NOT_FOUND"
          : error.status === 409
            ? "CONFLICT"
            : error.status === 410
              ? "TIMEOUT"
              : "INTERNAL_SERVER_ERROR";
  return new TRPCError({ code, message: error.message });
}

async function signHmacSha256(secret: string, payload: string): Promise<string> {
  const encoder = new TextEncoder();
  const cryptoKey = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );

  const signature = await crypto.subtle.sign("HMAC", cryptoKey, encoder.encode(payload));
  return encodeBase64Url(new Uint8Array(signature));
}

function encodeBase64Url(value: string | Uint8Array): string {
  const bytes = typeof value === "string" ? new TextEncoder().encode(value) : value;
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }

  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/u, "");
}

function decodeBase64Url(value: string): string {
  const normalized = value.replace(/-/g, "+").replace(/_/g, "/");
  const padding = normalized.length % 4 === 0 ? "" : "=".repeat(4 - (normalized.length % 4));
  const binary = atob(`${normalized}${padding}`);
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}
