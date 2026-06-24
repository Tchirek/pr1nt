import { confirmDocument, jsonError, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { confirmDocumentRequestSchema } from "@/server/print-sync/schemas";

export async function POST(request: Request) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    const body = await readJsonBody(request, confirmDocumentRequestSchema);
    return Response.json(
      await confirmDocument(env, {
        documentId: body.document_id,
        deviceId,
        sha256: body.sha256,
        sizeBytes: body.size_bytes,
        pageCount: body.page_count,
        fileName: body.file_name,
      }),
    );
  } catch (error) {
    return jsonError(error);
  }
}
