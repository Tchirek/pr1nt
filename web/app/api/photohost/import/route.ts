import { NextRequest } from "next/server";

const ALLOWED_HOSTS = new Set([
  "pics.example.com",
  "api.pics.example.com",
  "photohost-worker.yhyhyhyhgsgsgsgs.workers.dev",
]);

const MAX_IMPORT_BYTES = 24 * 1024 * 1024;

export async function GET(request: NextRequest) {
  const source = request.nextUrl.searchParams.get("src") || "";
  let sourceUrl: URL;

  try {
    sourceUrl = new URL(source);
  } catch {
    return Response.json({ error: "invalid_source" }, { status: 400 });
  }

  if (sourceUrl.protocol !== "https:" || !ALLOWED_HOSTS.has(sourceUrl.hostname)) {
    return Response.json({ error: "source_not_allowed" }, { status: 400 });
  }

  if (!sourceUrl.pathname.startsWith("/img/") && !sourceUrl.pathname.startsWith("/api/download/file/")) {
    return Response.json({ error: "source_path_not_allowed" }, { status: 400 });
  }

  const upstream = await fetch(sourceUrl, {
    cache: "no-store",
    headers: {
      Accept: "image/avif,image/webp,image/png,image/jpeg,image/*,*/*;q=0.8",
    },
  });

  if (!upstream.ok) {
    return Response.json({ error: "source_unavailable" }, { status: upstream.status });
  }

  const contentLength = Number(upstream.headers.get("content-length") || "0");
  if (contentLength > MAX_IMPORT_BYTES) {
    return Response.json({ error: "source_too_large" }, { status: 413 });
  }

  const contentType = upstream.headers.get("content-type") || "application/octet-stream";
  if (!contentType.toLowerCase().startsWith("image/")) {
    return Response.json({ error: "source_not_image" }, { status: 415 });
  }

  const bytes = await upstream.arrayBuffer();
  if (bytes.byteLength > MAX_IMPORT_BYTES) {
    return Response.json({ error: "source_too_large" }, { status: 413 });
  }

  return new Response(bytes, {
    headers: {
      "Cache-Control": "no-store",
      "Content-Length": String(bytes.byteLength),
      "Content-Type": contentType,
    },
  });
}
