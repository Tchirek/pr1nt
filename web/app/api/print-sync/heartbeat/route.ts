import { heartbeatWork, jsonError, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { heartbeatRequestSchema } from "@/server/print-sync/schemas";

export async function POST(request: Request) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    const body = await readJsonBody(request, heartbeatRequestSchema);
    if (body.kind && body.id) {
      await heartbeatWork(env, {
        kind: body.kind,
        id: body.id,
        deviceId,
        phase: body.phase,
      });
    }
    return Response.json({ ok: true });
  } catch (error) {
    return jsonError(error);
  }
}
