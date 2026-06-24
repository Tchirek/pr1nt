import { getCloudflareContext } from "@opennextjs/cloudflare";
import { z } from "zod";

import { PrintSyncError, type PrintSyncBindings } from "./service";

export function printSyncEnv(): PrintSyncBindings {
  const { env } = getCloudflareContext();
  return assertPrintSyncBindings(env);
}

export async function readJsonBody<TSchema extends z.ZodTypeAny>(
  request: Request,
  schema: TSchema,
): Promise<z.infer<TSchema>> {
  let body: unknown;
  try {
    body = await request.json();
  } catch {
    throw new PrintSyncError(400, "invalid_json");
  }
  const parsed = schema.safeParse(body);
  if (!parsed.success) {
    throw new PrintSyncError(400, "invalid_request_body");
  }
  return parsed.data;
}

function assertPrintSyncBindings(value: unknown): PrintSyncBindings {
  if (!isRecord(value)) {
    throw new PrintSyncError(500, "print_sync_env_missing");
  }

  const uploadSigningSecret = requiredEnvString(value, "UPLOAD_SIGNING_SECRET");
  const printSyncSecret = requiredEnvString(value, "PRINT_SYNC_SECRET");
  const r2AccountId = requiredEnvString(value, "R2_ACCOUNT_ID");
  const r2BucketName = requiredEnvString(value, "R2_BUCKET_NAME");
  const r2AccessKeyId = requiredEnvString(value, "R2_ACCESS_KEY_ID");
  const r2SecretAccessKey = requiredEnvString(value, "R2_SECRET_ACCESS_KEY");

  if (!isD1Database(value.PRINT_DB)) {
    throw new PrintSyncError(500, "print_sync_env_print_db_missing");
  }
  if (!isR2Bucket(value.PRINT_STAGING)) {
    throw new PrintSyncError(500, "print_sync_env_print_staging_missing");
  }

  return {
    PRINT_DB: value.PRINT_DB,
    PRINT_STAGING: value.PRINT_STAGING,
    UPLOAD_SIGNING_SECRET: uploadSigningSecret,
    PRINT_SYNC_SECRET: printSyncSecret,
    NORMALPICS_HANDOFF_SECRET: typeof value.NORMALPICS_HANDOFF_SECRET === "string"
      ? value.NORMALPICS_HANDOFF_SECRET
      : undefined,
    NORMALPICS_ORIGINS: typeof value.NORMALPICS_ORIGINS === "string" ? value.NORMALPICS_ORIGINS : undefined,
    R2_ACCOUNT_ID: r2AccountId,
    R2_BUCKET_NAME: r2BucketName,
    R2_ACCESS_KEY_ID: r2AccessKeyId,
    R2_SECRET_ACCESS_KEY: r2SecretAccessKey,
  };
}

function requiredEnvString(env: Record<string, unknown>, key: string): string {
  const value = env[key];
  if (typeof value !== "string" || !value.trim()) {
    throw new PrintSyncError(500, `print_sync_env_${key.toLowerCase()}_missing`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function isD1Database(value: unknown): value is PrintSyncBindings["PRINT_DB"] {
  return isRecord(value) && typeof value.prepare === "function" && typeof value.batch === "function";
}

function isR2Bucket(value: unknown): value is PrintSyncBindings["PRINT_STAGING"] {
  return isRecord(value)
    && typeof value.head === "function"
    && typeof value.get === "function"
    && typeof value.delete === "function";
}
