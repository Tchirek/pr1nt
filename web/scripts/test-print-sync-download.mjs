import { createHash } from "node:crypto";

const [
  documentId,
  expectedSha256,
  expectedSizeText,
  pageCountText,
  fileName = "prepared.pdf",
] = process.argv.slice(2);
const syncSecret = process.env.PRINT_SYNC_SECRET;

if (!documentId || !expectedSha256 || !expectedSizeText || !pageCountText || !syncSecret) {
  throw new Error(
    "Usage: PRINT_SYNC_SECRET=... node scripts/test-print-sync-download.mjs "
      + "<document-id> <expected-sha256> <expected-size> <page-count> [file-name]",
  );
}

const siteUrl = (process.env.PRINT_SITE_URL || "https://print.example.com").replace(/\/$/, "");
const deviceId = process.env.PRINT_SYNC_TEST_DEVICE_ID || "download-test";
const expectedSize = Number(expectedSizeText);
const pageCount = Number(pageCountText);

await daemonPost("/api/print-sync/claim", {
  kind: "document",
  id: documentId,
  recover_ready: true,
});

const response = await fetch(`${siteUrl}/api/print-sync/download/${encodeURIComponent(documentId)}`, {
  headers: daemonHeaders(),
  redirect: "manual",
});
if (!response.ok) {
  throw new Error(`download failed: ${response.status} ${await response.text()}`);
}
if (response.status !== 200) {
  throw new Error(`expected a 200 streaming response, received ${response.status}`);
}
if (response.headers.get("location")) {
  throw new Error("download unexpectedly returned a redirect location");
}

const hash = createHash("sha256");
let receivedSize = 0;
for await (const chunk of response.body) {
  receivedSize += chunk.byteLength;
  hash.update(chunk);
}
const sha256 = hash.digest("hex");

if (receivedSize !== expectedSize) {
  throw new Error(`size mismatch: expected ${expectedSize}, received ${receivedSize}`);
}
if (sha256 !== expectedSha256.toLowerCase()) {
  throw new Error(`sha256 mismatch: expected ${expectedSha256}, received ${sha256}`);
}

await daemonPost("/api/print-sync/confirm", {
  document_id: documentId,
  sha256,
  size_bytes: receivedSize,
  page_count: pageCount,
  file_name: fileName,
});

console.log(JSON.stringify({
  document_id: documentId,
  status: response.status,
  content_length: response.headers.get("content-length"),
  content_type: response.headers.get("content-type"),
  redirect_location: response.headers.get("location"),
  received_size: receivedSize,
  sha256,
  restored_status: "ready",
}, null, 2));

async function daemonPost(endpoint, body) {
  const response = await fetch(`${siteUrl}${endpoint}`, {
    method: "POST",
    headers: {
      ...daemonHeaders(),
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`${endpoint} failed: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

function daemonHeaders() {
  return {
    "x-device-id": deviceId,
    "x-print-sync-secret": syncSecret,
  };
}
