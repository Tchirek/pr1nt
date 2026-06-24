import { documentDownloadResponse, jsonError, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv } from "@/server/print-sync/http";

export async function GET(
  request: Request,
  context: { params: Promise<{ id: string }> },
) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    const { id } = await context.params;
    return documentDownloadResponse(env, id, deviceId);
  } catch (error) {
    return jsonError(error);
  }
}
