import { getCloudflareContext } from "@opennextjs/cloudflare";

import { KV_KEYS, type QRCodesConfig } from "../../../../../cloudflare/kv-schema";
import type { AppBindings } from "@/server/trpc/router";

type QRCodeMethod = "alipay" | "wechat";

const DATA_URL_PATTERN = /^data:([^;,]+);base64,(.+)$/i;

export async function GET(
  _request: Request,
  { params }: { params: Promise<{ method: string }> },
): Promise<Response> {
  const { method } = await params;
  if (method !== "alipay" && method !== "wechat") {
    return Response.json({ error: "Unknown QR code method." }, { status: 404 });
  }

  const { env } = getCloudflareContext();
  const qrCodes = await readQRCodes(env as AppBindings);
  const source = method === "alipay" ? qrCodes.alipay_url : qrCodes.wechat_url;

  if (!source) {
    return Response.json({ error: "QR code is not configured." }, { status: 404 });
  }

  if (/^https?:\/\//i.test(source)) {
    return Response.redirect(source, 302);
  }

  const parsed = parseDataUrl(source);
  if (!parsed) {
    return Response.json({ error: "QR code format is invalid." }, { status: 502 });
  }

  const body = parsed.bytes.buffer as ArrayBuffer;

  return new Response(body, {
    headers: {
      "Cache-Control": "public, max-age=300",
      "Content-Length": String(parsed.bytes.byteLength),
      "Content-Type": parsed.contentType,
    },
  });
}

async function readQRCodes(env: AppBindings): Promise<QRCodesConfig> {
  const fallback: QRCodesConfig = {
    alipay_url: env.DEFAULT_ALIPAY_QR ?? "",
    wechat_url: env.DEFAULT_WECHAT_QR ?? "",
  };

  try {
    const raw = await env.PRINT_KV.get(KV_KEYS.qrcodes, "text");
    if (!raw) {
      return fallback;
    }

    const parsed = JSON.parse(raw) as Partial<QRCodesConfig>;
    return {
      alipay_url: typeof parsed.alipay_url === "string" ? parsed.alipay_url : fallback.alipay_url,
      wechat_url: typeof parsed.wechat_url === "string" ? parsed.wechat_url : fallback.wechat_url,
    };
  } catch {
    return fallback;
  }
}

function parseDataUrl(source: string): { bytes: Uint8Array<ArrayBuffer>; contentType: string } | null {
  const match = DATA_URL_PATTERN.exec(source.trim());
  if (!match) {
    return null;
  }

  try {
    const binary = atob(match[2]);
    const bytes: Uint8Array<ArrayBuffer> = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }

    return {
      bytes,
      contentType: match[1],
    };
  } catch {
    return null;
  }
}
