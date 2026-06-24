import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { createTRPCProxyClient, httpBatchLink } from "@trpc/client";

const filePath = process.argv[2];
if (!filePath) {
  throw new Error("Usage: node scripts/test-r2-print-sync.mjs <file>");
}

const siteUrl = (process.env.PRINT_SITE_URL || "https://print.example.com").replace(/\/$/, "");
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
  mime_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  size_bytes: fileInfo.size,
});
const sessionAt = performance.now();

let upload;
for (let attempt = 1; attempt <= 3; attempt += 1) {
  try {
    upload = await fetch(session.upload_url, {
      method: "PUT",
      headers: session.upload_headers,
      body: bytes,
    });
    if (!upload.ok) {
      throw new Error(`R2 upload failed: ${upload.status} ${await upload.text()}`);
    }
    break;
  } catch (error) {
    if (attempt >= 3) throw error;
    await new Promise((resolve) => setTimeout(resolve, attempt * 600));
  }
}
const uploadedAt = performance.now();

await client.document.notifyUpload.mutate({
  document_id: session.document_id,
  document_token: session.upload_token,
});
const notifiedAt = performance.now();

let lastStatus = "";
while (performance.now() - startedAt < 20 * 60_000) {
  const preparation = await client.document.getPreparationStatus.query({
    document_id: session.document_id,
    document_token: session.upload_token,
  });
  if (preparation.status !== lastStatus) {
    lastStatus = preparation.status;
    console.log(JSON.stringify({
      status: preparation.status,
      elapsed_ms: Math.round(performance.now() - startedAt),
      error: preparation.error,
    }));
  }
  if (preparation.status === "ready") {
    console.log(JSON.stringify({
      document_id: preparation.document_id,
      file_name: preparation.file_name,
      page_count: preparation.page_count,
      size_bytes: preparation.actual_size,
      timings_ms: {
        create_session: Math.round(sessionAt - startedAt),
        r2_upload: Math.round(uploadedAt - sessionAt),
        notify: Math.round(notifiedAt - uploadedAt),
        ready_total: Math.round(performance.now() - startedAt),
      },
    }, null, 2));
    process.exit(0);
  }
  if (preparation.status === "failed" || preparation.status === "expired") {
    throw new Error(preparation.error || `Preparation ${preparation.status}`);
  }
  await new Promise((resolve) => setTimeout(resolve, 1000));
}

throw new Error("Preparation timed out.");
