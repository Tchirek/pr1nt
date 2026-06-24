import { allowedNormalPicsOrigin, jsonError, notifyUpload } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { notifyUploadRequestSchema } from "@/server/print-sync/schemas";

export async function OPTIONS(request: Request) {
  const env = printSyncEnv();
  const origin = allowedNormalPicsOrigin(request, env);
  return new Response(null, {
    status: origin ? 204 : 403,
    headers: corsHeaders(origin),
  });
}

export async function POST(request: Request) {
  const env = printSyncEnv();
  const origin = allowedNormalPicsOrigin(request, env);
  if (request.headers.get("origin") && !origin) {
    return Response.json({ error: "origin_not_allowed" }, { status: 403 });
  }

  try {
    const body = await readJsonBody(request, notifyUploadRequestSchema);
    const preparation = await notifyUpload(env, body.document_id, body.document_token);
    return Response.json(preparation, {
      headers: corsHeaders(origin),
    });
  } catch (error) {
    const response = jsonError(error);
    for (const [name, value] of Object.entries(corsHeaders(origin))) {
      response.headers.set(name, value);
    }
    return response;
  }
}

function corsHeaders(origin: string | null): Record<string, string> {
  if (!origin) {
    return {};
  }
  return {
    "Access-Control-Allow-Origin": origin,
    "Access-Control-Allow-Methods": "POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
    "Access-Control-Max-Age": "86400",
    Vary: "Origin",
  };
}
