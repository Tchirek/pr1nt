import { createHmac, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { basename, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const sourcePath = process.argv[2] ? resolve(process.argv[2]) : "";
const websocketUrl = process.argv[3] || "ws://127.0.0.1:8788/ws/prepare";
const laneLimit = Number.parseInt(process.argv[4] || "32", 10);
const CHUNK_SIZE = Number.parseInt(process.argv[5] || String(512 * 1024), 10);

if (!sourcePath) {
  throw new Error("Usage: node scripts/benchmark-prepare-ws-file.mjs <document-path> [websocket-url] [lanes] [chunk-size]");
}

const source = await readFile(sourcePath);
const sourceName = basename(sourcePath);
const uploadId = randomUUID();
const secret = process.env.PRINT_SHARED_SECRET || await readSharedSecret();
const token = signToken(secret, {
  kind: "prepare_ws",
  upload_id: uploadId,
  total_bytes: source.length,
  exp: Date.now() + 15 * 60_000,
});
const chunkCount = Math.ceil(source.length / CHUNK_SIZE);
const laneCount = Math.min(laneLimit, chunkCount);
const queue = Array.from({ length: chunkCount }, (_, index) => index);
const confirmed = new Set();
const lanes = Array.from({ length: laneCount }, (_, id) => ({
  id,
  socket: null,
  ready: false,
  inFlight: null,
}));
const startedAt = performance.now();
let firstReadyAt = 0;
let lastReceivedBytes = 0;

console.log(`[prepare-ws-file] ${sourceName}`);
console.log(`[prepare-ws-file] ${source.length} bytes, ${chunkCount} chunks, ${laneCount} lanes, upload ${uploadId}`);

await new Promise((resolveUpload, rejectUpload) => {
  const timeout = setTimeout(() => fail(new Error("file benchmark timed out")), 20 * 60_000);
  let settled = false;

  function fail(error) {
    if (settled) {
      return;
    }
    settled = true;
    clearTimeout(timeout);
    closeAll();
    rejectUpload(error);
  }

  function finish() {
    if (settled) {
      return;
    }
    settled = true;
    clearTimeout(timeout);
    closeAll();
    resolveUpload();
  }

  function closeAll() {
    for (const lane of lanes) {
      lane.socket?.close();
      lane.socket = null;
    }
  }

  function dispatch(lane) {
    if (!lane.ready || lane.inFlight !== null || !lane.socket || lane.socket.readyState !== WebSocket.OPEN) {
      return;
    }
    while (queue.length > 0) {
      const index = queue.shift();
      if (confirmed.has(index)) {
        continue;
      }
      lane.inFlight = index;
      lane.socket.send(buildFrame(index));
      return;
    }
  }

  function dispatchAll() {
    for (const lane of lanes) {
      dispatch(lane);
    }
  }

  function handleMessage(lane, event) {
    const message = JSON.parse(String(event.data));
    if (message.type === "error") {
      fail(new Error(message.message));
      return;
    }
    if (message.type === "ready") {
      lane.ready = true;
      if (!firstReadyAt) {
        firstReadyAt = performance.now();
      }
      for (const index of message.confirmed_chunks) {
        confirmed.add(index);
      }
      dispatchAll();
      return;
    }
    if (message.type !== "ack") {
      return;
    }

    confirmed.add(message.chunk_index);
    if (lane.inFlight === message.chunk_index) {
      lane.inFlight = null;
    }
    if (message.received_bytes >= lastReceivedBytes) {
      lastReceivedBytes = message.received_bytes;
      const elapsed = (performance.now() - firstReadyAt) / 1000;
      const rate = message.received_bytes / 1024 / 1024 / elapsed;
      console.log(
        `[prepare-ws-file] ACK ${String(message.percent).padStart(3)}% `
        + `${message.received_bytes}/${message.total_bytes} | ${rate.toFixed(3)} MiB/s`,
      );
    }
    if (confirmed.size === chunkCount) {
      finish();
      return;
    }
    dispatchAll();
  }

  function openLane(lane) {
    const socket = new WebSocket(websocketUrl);
    lane.socket = socket;
    socket.addEventListener("open", () => {
      socket.send(JSON.stringify({
        type: "hello",
        token,
        upload_id: uploadId,
        total_bytes: source.length,
        source_name: sourceName,
        chunk_size_bytes: CHUNK_SIZE,
      }));
    });
    socket.addEventListener("message", (event) => handleMessage(lane, event));
    socket.addEventListener("error", () => fail(new Error(`lane ${lane.id} failed`)));
    socket.addEventListener("close", () => {
      if (!settled && lane.inFlight !== null) {
        fail(new Error(`lane ${lane.id} closed with chunk ${lane.inFlight}`));
      }
    });
  }

  function buildFrame(index) {
    const start = index * CHUNK_SIZE;
    const end = Math.min(start + CHUNK_SIZE, source.length);
    const frame = Buffer.allocUnsafe(4 + end - start);
    frame.writeUInt32BE(index, 0);
    source.copy(frame, 4, start, end);
    return frame;
  }

  for (const lane of lanes) {
    openLane(lane);
  }
});

const elapsedSeconds = (performance.now() - firstReadyAt) / 1000;
console.log(
  `[prepare-ws-file] complete in ${elapsedSeconds.toFixed(2)}s, `
  + `${(source.length / 1024 / 1024 / elapsedSeconds).toFixed(3)} MiB/s`,
);

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
