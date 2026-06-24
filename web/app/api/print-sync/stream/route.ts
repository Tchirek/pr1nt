import { jsonError, latestEventId, requireDaemonSecret } from "@/server/print-sync/service";
import { printSyncEnv } from "@/server/print-sync/http";

export async function GET(request: Request) {
  try {
    const env = printSyncEnv();
    requireDaemonSecret(request, env);
    const encoder = new TextEncoder();
    let closed = false;
    let lastEventId = await latestEventId(env);
    let pollTimer: ReturnType<typeof setInterval> | undefined;
    let pingTimer: ReturnType<typeof setInterval> | undefined;

    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        const send = (value: string) => {
          if (closed) {
            return;
          }
          try {
            controller.enqueue(encoder.encode(value));
          } catch {
            closed = true;
          }
        };
        const cleanup = () => {
          closed = true;
          if (pollTimer) clearInterval(pollTimer);
          if (pingTimer) clearInterval(pingTimer);
          try {
            controller.close();
          } catch {
            // The client may already have closed the stream.
          }
        };

        send(": connected\n\n");
        pingTimer = setInterval(() => send(": ping\n\n"), 25_000);
        pollTimer = setInterval(() => {
          void latestEventId(env)
            .then((nextEventId) => {
              if (nextEventId <= lastEventId) {
                return;
              }
              lastEventId = nextEventId;
              send(`data: ${JSON.stringify({ event_id: nextEventId })}\n\n`);
            })
            .catch((error) => {
              console.warn("[print-sync] SSE poll failed", error);
            });
        }, 2_000);
        request.signal.addEventListener("abort", cleanup, { once: true });
      },
      cancel() {
        closed = true;
        if (pollTimer) clearInterval(pollTimer);
        if (pingTimer) clearInterval(pingTimer);
      },
    });

    return new Response(stream, {
      headers: {
        "Cache-Control": "no-cache, no-transform",
        Connection: "keep-alive",
        "Content-Type": "text/event-stream; charset=utf-8",
        "X-Accel-Buffering": "no",
      },
    });
  } catch (error) {
    return jsonError(error);
  }
}
