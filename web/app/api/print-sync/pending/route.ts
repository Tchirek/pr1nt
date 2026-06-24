import { jsonError, pendingWork, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv } from "@/server/print-sync/http";

export async function GET(request: Request) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    return Response.json(await pendingWork(env, deviceId));
  } catch (error) {
    return jsonError(error);
  }
}
