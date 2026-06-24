import { createHmac, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { basename, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const CHUNK_SIZE = 512 * 1024;
const LANE_COUNT = 32;
const sourcePath = process.argv[2] ? resolve(process.argv[2]) : "";
const websocketUrl = process.argv[3] || "ws://127.0.0.1:8788/ws/prepare";

if (!sourcePath) {
  throw new Error("Usage: node scripts/test-prepare-ws.mjs <document-path> [websocket-url]");
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
const queue = Array.from({ length: chunkCount }, (_, index) => index);
const queued = new Set(queue);
const confirmed = new Set();
const lanes = Array.from({ length: Math.min(LANE_COUNT, chunkCount) }, (_, id) => ({
  id,
  socket: null,
  ready: false,
  inFlight: null,
  reconnectTimer: null,
}));

let phase = "duplicate";
let missingCompleteVerified = false;
let duplicatePending = false;
let duplicateVerified = false;
let singleDisconnectInjected = false;
let allDisconnectInjected = false;
let conversionDisconnectInjected = false;
let pauseReconnect = false;
let completing = false;
let settled = false;
let lastReceivedBytes = 0;
let lastPercent = 0;
const startedAt = performance.now();

console.log(`[prepare-ws] ${sourceName}`);
console.log(`[prepare-ws] ${source.length} bytes, ${chunkCount} chunks, upload ${uploadId}`);
await verifyExpiredTokenRejected();
console.log("[prepare-ws] expired token was rejected");

const prepared = await new Promise((resolvePrepared, rejectPrepared) => {
  const timeout = setTimeout(() => fail(new Error("prepare websocket test timed out")), 20 * 60_000);

  function fail(error) {
    if (settled) {
      return;
    }
    settled = true;
    clearTimeout(timeout);
    closeAll();
    rejectPrepared(error);
  }

  function finish(payload) {
    if (settled) {
      return;
    }
    settled = true;
    clearTimeout(timeout);
    closeAll();
    resolvePrepared(payload);
  }

  function closeAll() {
    for (const lane of lanes) {
      if (lane.reconnectTimer) {
        clearTimeout(lane.reconnectTimer);
        lane.reconnectTimer = null;
      }
      if (lane.socket && lane.socket.readyState < WebSocket.CLOSING) {
        lane.socket.close(1000, "test phase complete");
      }
      lane.socket = null;
      lane.ready = false;
    }
  }

  function isInFlight(index) {
    return lanes.some((lane) => lane.inFlight === index);
  }

  function enqueue(index, front = false) {
    if (confirmed.has(index) || queued.has(index) || isInFlight(index)) {
      return;
    }
    queued.add(index);
    if (front) {
      queue.unshift(index);
    } else {
      queue.push(index);
    }
  }

  function takeNext() {
    while (queue.length > 0) {
      const index = queue.shift();
      queued.delete(index);
      if (!confirmed.has(index) && !isInFlight(index)) {
        return index;
      }
    }
    return null;
  }

  function mergeConfirmed(indices) {
    for (const index of indices) {
      if (!Number.isInteger(index) || index < 0 || index >= chunkCount) {
        fail(new Error(`invalid confirmed chunk ${index}`));
        return;
      }
      confirmed.add(index);
      queued.delete(index);
    }
    for (const lane of lanes) {
      if (lane.inFlight !== null && confirmed.has(lane.inFlight)) {
        lane.inFlight = null;
      }
    }
  }

  function validateProgress(message) {
    if (message.upload_id !== uploadId || message.total_bytes !== source.length) {
      throw new Error("server progress does not match upload session");
    }
    if (message.received_bytes < lastReceivedBytes) {
      return;
    }
    lastReceivedBytes = message.received_bytes;
    lastPercent = message.percent;
    console.log(
      `[prepare-ws] ACK ${String(message.percent).padStart(3)}% `
      + `${message.received_bytes}/${message.total_bytes}`,
    );
  }

  function dispatchLane(lane) {
    if (
      settled
      || completing
      || !lane.ready
      || lane.inFlight !== null
      || !lane.socket
      || lane.socket.readyState !== WebSocket.OPEN
    ) {
      return;
    }
    const index = takeNext();
    if (index === null) {
      return;
    }
    lane.inFlight = index;
    lane.socket.send(buildFrame(index));

    if (phase === "parallel" && !singleDisconnectInjected) {
      singleDisconnectInjected = true;
      console.log(`[prepare-ws] injecting single-lane disconnect on chunk ${index}`);
      setTimeout(() => lane.socket?.close(), 0);
    }
  }

  function dispatchAll() {
    if (settled || completing || phase === "duplicate") {
      return;
    }
    if (confirmed.size === chunkCount) {
      const lane = lanes.find((item) => item.ready && item.socket?.readyState === WebSocket.OPEN);
      if (!lane) {
        return;
      }
      completing = true;
      console.log("[prepare-ws] all chunks confirmed, requesting local conversion");
      for (const other of lanes) {
        if (other.id !== lane.id && other.socket?.readyState === WebSocket.OPEN) {
          other.socket.close(1000, "conversion starts");
        }
      }
      lane.socket.send(JSON.stringify({ type: "complete" }));
      return;
    }
    for (const lane of lanes) {
      dispatchLane(lane);
    }
  }

  function scheduleReconnect(lane) {
    if (settled || pauseReconnect || lane.reconnectTimer) {
      return;
    }
    lane.reconnectTimer = setTimeout(() => {
      lane.reconnectTimer = null;
      openLane(lane);
    }, 200);
  }

  function openParallelPhase() {
    phase = "parallel";
    pauseReconnect = false;
    console.log("[prepare-ws] opening 16 lanes from persisted ready manifest");
    for (const lane of lanes) {
      openLane(lane);
    }
  }

  function injectAllLaneDisconnect() {
    if (allDisconnectInjected || confirmed.size < Math.min(4, chunkCount)) {
      return;
    }
    allDisconnectInjected = true;
    pauseReconnect = true;
    console.log("[prepare-ws] injecting all-lane disconnect");
    for (const lane of lanes) {
      lane.socket?.close();
    }
    setTimeout(() => {
      pauseReconnect = false;
      console.log("[prepare-ws] reconnecting all lanes and requesting confirmed manifest");
      for (const lane of lanes) {
        openLane(lane);
      }
    }, 500);
  }

  function handleMessage(lane, event) {
    const message = JSON.parse(String(event.data));
    if (message.type === "error") {
      if (phase === "missing_complete") {
        missingCompleteVerified = true;
        phase = "duplicate";
        console.log("[prepare-ws] incomplete upload was rejected");
        lane.inFlight = 0;
        lane.socket.send(buildFrame(0));
        return;
      }
      fail(new Error(message.message || "server returned an error"));
      return;
    }
    if (message.type === "prepared") {
      finish(message);
      return;
    }
    if (message.type === "processing") {
      validateProgress(message);
      if (completing && !conversionDisconnectInjected) {
        conversionDisconnectInjected = true;
        console.log("[prepare-ws] injecting disconnect during local conversion");
        lane.socket.close();
      }
      return;
    }
    if (message.type === "ready") {
      lane.ready = true;
      mergeConfirmed(message.confirmed_chunks);
      validateProgress(message);
      if (phase === "duplicate") {
        if (!missingCompleteVerified && confirmed.size === 0) {
          phase = "missing_complete";
          lane.socket.send(JSON.stringify({ type: "complete" }));
        } else if (confirmed.has(0)) {
          duplicateVerified = true;
          openParallelPhase();
        } else {
          lane.inFlight = 0;
          lane.socket.send(buildFrame(0));
        }
      } else {
        dispatchAll();
      }
      return;
    }
    if (message.type !== "ack") {
      fail(new Error(`unexpected server message ${message.type}`));
      return;
    }

    validateProgress(message);
    const wasConfirmed = confirmed.has(message.chunk_index);
    confirmed.add(message.chunk_index);
    queued.delete(message.chunk_index);
    if (lane.inFlight === message.chunk_index) {
      lane.inFlight = null;
    }

    if (phase === "duplicate" && message.chunk_index === 0) {
      if (!duplicatePending) {
        duplicatePending = true;
        lane.inFlight = 0;
        console.log("[prepare-ws] sending duplicate chunk 0");
        lane.socket.send(buildFrame(0));
        return;
      }
      if (message.received_bytes !== Math.min(CHUNK_SIZE, source.length) || !wasConfirmed) {
        fail(new Error("duplicate chunk changed confirmed byte count"));
        return;
      }
      duplicateVerified = true;
      console.log("[prepare-ws] duplicate chunk was idempotent");
      pauseReconnect = true;
      lane.socket.close();
      setTimeout(openParallelPhase, 300);
      return;
    }

    injectAllLaneDisconnect();
    dispatchAll();
  }

  function openLane(lane) {
    if (settled || lane.socket) {
      return;
    }
    const socket = new WebSocket(websocketUrl);
    lane.socket = socket;
    lane.ready = false;
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
    socket.addEventListener("message", (event) => {
      if (!settled && lane.socket === socket) {
        handleMessage(lane, event);
      }
    });
    socket.addEventListener("error", () => {
      if (socket.readyState < WebSocket.CLOSING) {
        socket.close();
      }
    });
    socket.addEventListener("close", () => {
      if (lane.socket !== socket) {
        return;
      }
      lane.socket = null;
      lane.ready = false;
      if (lane.inFlight !== null) {
        const interrupted = lane.inFlight;
        lane.inFlight = null;
        enqueue(interrupted, true);
      }
      if (completing) {
        completing = false;
      }
      scheduleReconnect(lane);
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

  openLane(lanes[0]);
});

const elapsedSeconds = (performance.now() - startedAt) / 1000;
const repeatedPrepared = await requestFirstServerMessage(uploadId, token, source.length, sourceName);
if (repeatedPrepared.type !== "prepared" || repeatedPrepared.prepared_id !== prepared.prepared_id) {
  throw new Error("completed upload did not return the same prepared result");
}
console.log(`[prepare-ws] prepared ${prepared.page_count} pages as ${prepared.file_name}`);
console.log(`[prepare-ws] missing_complete=${missingCompleteVerified} duplicate=${duplicateVerified} single_disconnect=${singleDisconnectInjected} all_disconnect=${allDisconnectInjected} conversion_disconnect=${conversionDisconnectInjected}`);
console.log("[prepare-ws] completed upload reconnect returned the same prepared result");
console.log(`[prepare-ws] final ACK ${lastPercent}% (${lastReceivedBytes}/${source.length})`);
console.log(`[prepare-ws] completed in ${elapsedSeconds.toFixed(2)}s`);

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

async function verifyExpiredTokenRejected() {
  const expiredUploadId = randomUUID();
  const expiredToken = signToken(secret, {
    kind: "prepare_ws",
    upload_id: expiredUploadId,
    total_bytes: source.length,
    exp: Date.now() - 1_000,
  });
  const message = await requestFirstServerMessage(
    expiredUploadId,
    expiredToken,
    source.length,
    sourceName,
  );
  if (message.type !== "error") {
    throw new Error("expired token was not rejected");
  }
}

function requestFirstServerMessage(sessionId, sessionToken, totalBytes, fileName) {
  return new Promise((resolveMessage, rejectMessage) => {
    const socket = new WebSocket(websocketUrl);
    const timeout = setTimeout(() => {
      socket.close();
      rejectMessage(new Error("server message timeout"));
    }, 15_000);

    socket.addEventListener("open", () => {
      socket.send(JSON.stringify({
        type: "hello",
        token: sessionToken,
        upload_id: sessionId,
        total_bytes: totalBytes,
        source_name: fileName,
        chunk_size_bytes: CHUNK_SIZE,
      }));
    });
    socket.addEventListener("message", (event) => {
      clearTimeout(timeout);
      socket.close();
      resolveMessage(JSON.parse(String(event.data)));
    });
    socket.addEventListener("error", () => {
      clearTimeout(timeout);
      rejectMessage(new Error("websocket connection failed"));
    });
  });
}
