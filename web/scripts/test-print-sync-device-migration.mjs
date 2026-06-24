import { createHash } from "node:crypto";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { createTRPCProxyClient, httpBatchLink } from "@trpc/client";

const filePath = process.argv[2];
const syncSecret = process.env.PRINT_SYNC_SECRET;
if (!filePath || !syncSecret) {
  throw new Error(
    "Usage: PRINT_SYNC_SECRET=... node scripts/test-print-sync-device-migration.mjs <file>",
  );
}

const siteUrl = (process.env.PRINT_SITE_URL || "https://print.example.com").replace(/\/$/, "");
const legacyDeviceId = "migration-test";
const currentDeviceId = `${legacyDeviceId}:instance-a`;
const fileName = path.basename(filePath);
const fileInfo = await stat(filePath);
const bytes = await readFile(filePath);
const expectedSha256 = createHash("sha256").update(bytes).digest("hex");

const client = createTRPCProxyClient({
  links: [
    httpBatchLink({
      url: `${siteUrl}/api/trpc`,
    }),
  ],
});

const session = await client.document.createUploadSession.mutate({
  file_name: fileName,
  mime_type: "application/pdf",
  size_bytes: fileInfo.size,
});
const upload = await fetch(session.upload_url, {
  method: "PUT",
  headers: session.upload_headers,
  body: bytes,
});
if (!upload.ok) {
  throw new Error(`R2 upload failed: ${upload.status} ${await upload.text()}`);
}
await client.document.notifyUpload.mutate({
  document_id: session.document_id,
  document_token: session.upload_token,
});

await prepareAs(legacyDeviceId, false);
await prepareAs(currentDeviceId, true);

const preparation = await client.document.getPreparationStatus.query({
  document_id: session.document_id,
  document_token: session.upload_token,
});
if (preparation.status !== "ready") {
  throw new Error(`expected ready status, received ${preparation.status}`);
}

console.log(JSON.stringify({
  document_id: session.document_id,
  legacy_device_id: legacyDeviceId,
  current_device_id: currentDeviceId,
  legacy_ready_recovered_by_suffixed_device: true,
  final_status: preparation.status,
  size_bytes: preparation.actual_size,
  page_count: preparation.page_count,
  sha256: expectedSha256,
}, null, 2));

async function prepareAs(deviceId, recoverReady) {
  await daemonPost("/api/print-sync/claim", deviceId, {
    kind: "document",
    id: session.document_id,
    recover_ready: recoverReady,
  });

  const response = await fetch(
    `${siteUrl}/api/print-sync/download/${encodeURIComponent(session.document_id)}`,
    { headers: daemonHeaders(deviceId) },
  );
  if (!response.ok) {
    throw new Error(`download failed for ${deviceId}: ${response.status} ${await response.text()}`);
  }
  const downloaded = Buffer.from(await response.arrayBuffer());
  const sha256 = createHash("sha256").update(downloaded).digest("hex");
  if (downloaded.byteLength !== fileInfo.size || sha256 !== expectedSha256) {
    throw new Error(`download verification failed for ${deviceId}`);
  }

  await daemonPost("/api/print-sync/confirm", deviceId, {
    document_id: session.document_id,
    sha256,
    size_bytes: downloaded.byteLength,
    page_count: 1,
    file_name: fileName,
  });
}

async function daemonPost(endpoint, deviceId, body) {
  const response = await fetch(`${siteUrl}${endpoint}`, {
    method: "POST",
    headers: {
      ...daemonHeaders(deviceId),
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`${endpoint} failed for ${deviceId}: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

function daemonHeaders(deviceId) {
  return {
    "x-device-id": deviceId,
    "x-print-sync-secret": syncSecret,
  };
}
