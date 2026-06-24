import { claimWork, jsonError, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { claimRequestSchema } from "@/server/print-sync/schemas";

export async function POST(request: Request) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    const body = await readJsonBody(request, claimRequestSchema);
    return Response.json(
      await claimWork(env, {
        kind: body.kind,
        id: body.id,
        deviceId,
        recoverReady: Boolean(body.recover_ready),
      }),
    );
  } catch (error) {
    return jsonError(error);
  }
}
