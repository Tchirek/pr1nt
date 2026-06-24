import { z } from "zod";

export const claimRequestSchema = z.object({
  kind: z.enum(["document", "job"]),
  id: z.string().min(1).max(128),
  recover_ready: z.boolean().optional(),
});

export const confirmDocumentRequestSchema = z.object({
  document_id: z.string().min(1).max(128),
  sha256: z.string().regex(/^[0-9a-fA-F]{64}$/u),
  size_bytes: z.number().finite().positive(),
  page_count: z.number().finite().positive(),
  file_name: z.string().min(1).max(260),
});

export const failDocumentRequestSchema = z.object({
  document_id: z.string().min(1).max(128),
  error: z.string().max(1000).optional(),
});

export const heartbeatRequestSchema = z.object({
  kind: z.enum(["document", "job"]).optional(),
  id: z.string().min(1).max(128).optional(),
  phase: z.enum(["downloading", "converting"]).optional(),
}).refine((value) => (!value.kind && !value.id) || (Boolean(value.kind) && Boolean(value.id)), {
  message: "heartbeat_kind_and_id_must_match",
});

export const jobStatusRequestSchema = z.object({
  job_id: z.string().min(1).max(128),
  status: z.enum(["queued", "printing", "done", "failed"]),
  detail: z.string().max(1000).nullable().optional(),
  pages_printed: z.number().finite().nonnegative().nullable().optional(),
  total_pages: z.number().finite().positive().nullable().optional(),
});

export const notifyUploadRequestSchema = z.object({
  document_id: z.string().min(1).max(128),
  document_token: z.string().min(1).max(2048),
});

export const photohostHandoffRequestSchema = z.object({
  source_type: z.enum(["normalpics", "normaldocs"]).default("normalpics"),
  file_name: z.string().min(1).max(260),
  mime_type: z.string().min(1).max(160).default("application/octet-stream"),
  size_bytes: z.number().finite().positive(),
});
