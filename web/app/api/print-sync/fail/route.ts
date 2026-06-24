import { failDocument, jsonError, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { failDocumentRequestSchema } from "@/server/print-sync/schemas";

export async function POST(request: Request) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    const body = await readJsonBody(request, failDocumentRequestSchema);
    await failDocument(env, {
      documentId: body.document_id,
      deviceId,
      error: body.error || "document_preparation_failed",
    });
    return Response.json({ ok: true });
  } catch (error) {
    return jsonError(error);
  }
}
