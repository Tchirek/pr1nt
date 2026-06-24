import { createHmac, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const websocketUrl = process.argv[2] || "ws://127.0.0.1:8788/ws/prepare";
const CHUNK_SIZE = 2 * 1024 * 1024;
const sizes = [64 * 1024, 256 * 1024, 1024 * 1024, 2 * 1024 * 1024];
const secret = process.env.PRINT_SHARED_SECRET || await readSharedSecret();

for (const totalBytes of sizes) {
  const result = await benchmarkChunk(totalBytes);
  console.log(
    `${String(totalBytes).padStart(8)} bytes | open ${result.openMs.toFixed(0).padStart(5)} ms`
    + ` | ready ${result.readyMs.toFixed(0).padStart(5)} ms`
    + ` | ack ${result.ackMs.toFixed(0).padStart(6)} ms`
    + ` | ${(totalBytes / 1024 / 1024 / (result.ackMs / 1000)).toFixed(3)} MiB/s`,
  );
}

function benchmarkChunk(totalBytes) {
  const uploadId = randomUUID();
  const token = signToken(secret, {
    kind: "prepare_ws",
    upload_id: uploadId,
    total_bytes: totalBytes,
    exp: Date.now() + 15 * 60_000,
  });
  const frame = Buffer.alloc(4 + totalBytes);
  frame.writeUInt32BE(0, 0);
  const startedAt = performance.now();

  return new Promise((resolveBenchmark, rejectBenchmark) => {
    const socket = new WebSocket(websocketUrl);
    let openedAt = 0;
    let readyAt = 0;
    const timeout = setTimeout(() => {
      socket.close();
      rejectBenchmark(new Error(`benchmark timeout for ${totalBytes} bytes`));
    }, 5 * 60_000);

    socket.addEventListener("open", () => {
      openedAt = performance.now();
      socket.send(JSON.stringify({
        type: "hello",
        token,
        upload_id: uploadId,
        total_bytes: totalBytes,
        source_name: `benchmark-${totalBytes}.pdf`,
        chunk_size_bytes: CHUNK_SIZE,
      }));
    });
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(String(event.data));
      if (message.type === "error") {
        clearTimeout(timeout);
        socket.close();
        rejectBenchmark(new Error(message.message));
        return;
      }
      if (message.type === "ready") {
        readyAt = performance.now();
        socket.send(frame);
        return;
      }
      if (message.type === "ack") {
        const acknowledgedAt = performance.now();
        clearTimeout(timeout);
        socket.close();
        resolveBenchmark({
          openMs: openedAt - startedAt,
          readyMs: readyAt - openedAt,
          ackMs: acknowledgedAt - readyAt,
        });
      }
    });
    socket.addEventListener("error", () => {
      clearTimeout(timeout);
      rejectBenchmark(new Error(`websocket connection failed for ${totalBytes} bytes`));
    });
  });
}

async function readSharedSecret() {
  const scriptDir = fileURLToPath(new URL(".", import.meta.url));
  const envText = await readFile(resolve(scriptDir, "../.env"), "utf8");
  const line = envText
    .split(/\r?\n/u)
    .find((value) => value.trim().startsWith("PRINT_SHARED_SECRET="));
  if (!line) {
    throw new Error("PRINT_SHARED_SECRET is not configured");
  }
  return line.slice(line.indexOf("=") + 1).trim().replace(/^["']|["']$/gu, "");
}

function signToken(sharedSecret, payload) {
  const encodedPayload = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signature = createHmac("sha256", sharedSecret).update(encodedPayload).digest("base64url");
  return `${encodedPayload}.${signature}`;
}
