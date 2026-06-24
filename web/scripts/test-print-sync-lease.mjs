import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { createTRPCProxyClient, httpBatchLink } from "@trpc/client";

const filePath = process.argv[2];
const syncSecret = process.env.PRINT_SYNC_SECRET;
if (!filePath || !syncSecret) {
  throw new Error("Usage: PRINT_SYNC_SECRET=... node scripts/test-print-sync-lease.mjs <file>");
}

const siteUrl = (process.env.PRINT_SITE_URL || "https://print.example.com").replace(/\/$/, "");
const firstDevice = "lease-test-a";
const secondDevice = "lease-test-b";
const leaseWaitMs = Number(process.env.LEASE_WAIT_MS || 125_000);
const fileName = path.basename(filePath);
const fileInfo = await stat(filePath);
const bytes = await readFile(filePath);
const startedAt = performance.now();

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

await daemonPost("/api/print-sync/claim", firstDevice, {
  kind: "document",
  id: session.document_id,
  recover_ready: false,
});

let activeLeaseRejected = false;
try {
  await daemonPost("/api/print-sync/claim", secondDevice, {
    kind: "document",
    id: session.document_id,
    recover_ready: false,
  });
} catch (error) {
  activeLeaseRejected = String(error).includes("409");
}
if (!activeLeaseRejected) {
  throw new Error("Second device was able to claim an active lease");
}

await sleep(leaseWaitMs);
await daemonPost("/api/print-sync/claim", secondDevice, {
  kind: "document",
  id: session.document_id,
  recover_ready: false,
});

for (let attempt = 0; attempt < 2; attempt += 1) {
  await daemonPost("/api/print-sync/fail", secondDevice, {
    document_id: session.document_id,
    error: "lease_validation_complete",
  });
}

const preparation = await client.document.getPreparationStatus.query({
  document_id: session.document_id,
  document_token: session.upload_token,
});
if (preparation.status !== "failed") {
  throw new Error(`Expected failed status, received ${preparation.status}`);
}

console.log(JSON.stringify({
  document_id: session.document_id,
  active_lease_rejected: activeLeaseRejected,
  expired_lease_reclaimed: true,
  duplicate_fail_idempotent: true,
  final_status: preparation.status,
  elapsed_ms: Math.round(performance.now() - startedAt),
}, null, 2));

async function daemonPost(endpoint, deviceId, body) {
  const response = await fetch(`${siteUrl}${endpoint}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "x-device-id": deviceId,
      "x-print-sync-secret": syncSecret,
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`${endpoint} failed: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
