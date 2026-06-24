import { createTRPCProxyClient, httpBatchLink } from "@trpc/client";

const imageId = process.argv[2];
if (!imageId) {
  throw new Error("Usage: node scripts/test-normalpics-handoff.mjs <image-id>");
}

const normalPicsUrl = (process.env.NORMALPICS_API_URL || "https://api.pics.example.com").replace(/\/$/, "");
const printSiteUrl = (process.env.PRINT_SITE_URL || "https://print.example.com").replace(/\/$/, "");
const startedAt = performance.now();

const client = createTRPCProxyClient({
  links: [
    httpBatchLink({
      url: `${printSiteUrl}/api/trpc`,
    }),
  ],
});

const handoffResponse = await fetch(`${normalPicsUrl}/api/print/handoff`, {
  method: "POST",
  headers: {
    "Content-Type": "application/json",
  },
  body: JSON.stringify({ imageId }),
});
if (!handoffResponse.ok) {
  throw new Error(`NormalPics handoff failed: ${handoffResponse.status} ${await handoffResponse.text()}`);
}
const session = await handoffResponse.json();
const handoffAt = performance.now();

const sourceResponse = await fetch(session.source_url, { cache: "no-store" });
if (!sourceResponse.ok) {
  throw new Error(`NormalPics source download failed: ${sourceResponse.status}`);
}
const sourceBytes = await sourceResponse.arrayBuffer();
if (sourceBytes.byteLength <= 0) {
  throw new Error("NormalPics source download was empty");
}
const sourceAt = performance.now();

const uploadResponse = await fetch(session.upload_url, {
  method: "PUT",
  headers: session.upload_headers,
  body: sourceBytes,
});
if (!uploadResponse.ok) {
  throw new Error(`609 R2 upload failed: ${uploadResponse.status} ${await uploadResponse.text()}`);
}
const uploadAt = performance.now();

for (let attempt = 0; attempt < 2; attempt += 1) {
  const notifyResponse = await fetch(session.notify_url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      document_id: session.document_id,
      document_token: session.upload_token,
    }),
  });
  if (!notifyResponse.ok) {
    throw new Error(`609 upload notification failed: ${notifyResponse.status} ${await notifyResponse.text()}`);
  }
}
const notifiedAt = performance.now();

const consumed = await client.document.consumePrintHandoff.mutate({
  handoff_token: session.handoff_token,
});
let duplicateConsumeRejected = false;
try {
  await client.document.consumePrintHandoff.mutate({
    handoff_token: session.handoff_token,
  });
} catch {
  duplicateConsumeRejected = true;
}
if (!duplicateConsumeRejected) {
  throw new Error("Handoff token was accepted more than once");
}

let preparation = consumed.preparation;
let lastStatus = "";
while (performance.now() - startedAt < 10 * 60_000) {
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
      image_id: imageId,
      document_id: preparation.document_id,
      file_name: preparation.file_name,
      page_count: preparation.page_count,
      size_bytes: preparation.actual_size,
      duplicate_notify_idempotent: true,
      duplicate_handoff_rejected: duplicateConsumeRejected,
      timings_ms: {
        create_handoff: Math.round(handoffAt - startedAt),
        source_download: Math.round(sourceAt - handoffAt),
        r2_upload: Math.round(uploadAt - sourceAt),
        notify_twice: Math.round(notifiedAt - uploadAt),
        ready_total: Math.round(performance.now() - startedAt),
      },
    }, null, 2));
    process.exit(0);
  }
  if (preparation.status === "failed" || preparation.status === "expired") {
    throw new Error(preparation.error || `Preparation ${preparation.status}`);
  }
  await new Promise((resolve) => setTimeout(resolve, 800));
  preparation = await client.document.getPreparationStatus.query({
    document_id: session.document_id,
    document_token: consumed.document_token,
  });
}

throw new Error("NormalPics print handoff preparation timed out");
