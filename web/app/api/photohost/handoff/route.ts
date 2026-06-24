import { createNormalPicsHandoff, jsonError, requireNormalPicsSecret } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { photohostHandoffRequestSchema } from "@/server/print-sync/schemas";

export async function POST(request: Request) {
  try {
    const env = printSyncEnv();
    requireNormalPicsSecret(request, env);
    const body = await readJsonBody(request, photohostHandoffRequestSchema);
    const session = await createNormalPicsHandoff(env, {
      sourceType: body.source_type,
      fileName: body.file_name,
      mimeType: body.mime_type,
      sizeBytes: body.size_bytes,
    });
    return Response.json({
      ...session,
      notify_url: new URL("/api/print-upload/notify", request.url).toString(),
    });
  } catch (error) {
    return jsonError(error);
  }
}
