import { jsonError, reportJobStatus, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv, readJsonBody } from "@/server/print-sync/http";
import { jobStatusRequestSchema } from "@/server/print-sync/schemas";

export async function POST(request: Request) {
  try {
    const env = printSyncEnv();
    const deviceId = requireDaemonSecret(request, env);
    const body = await readJsonBody(request, jobStatusRequestSchema);
    return Response.json(
      await reportJobStatus(env, {
        jobId: body.job_id,
        deviceId,
        status: body.status,
        detail: body.detail,
        pagesPrinted: body.pages_printed,
        totalPages: body.total_pages,
      }),
    );
  } catch (error) {
    return jsonError(error);
  }
}
