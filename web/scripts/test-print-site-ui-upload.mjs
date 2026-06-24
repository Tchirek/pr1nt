import { spawn } from "node:child_process";
import { mkdtemp, rm, stat } from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const filePath = path.resolve(process.argv[2] || "");
if (!process.argv[2]) {
  throw new Error("Usage: node scripts/test-print-site-ui-upload.mjs <file>");
}

const fileInfo = await stat(filePath);
const siteUrl = process.env.PRINT_SITE_URL || "https://print.example.com";
const edgePath = process.env.EDGE_PATH
  || "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const userDataDir = await mkdtemp(path.join(os.tmpdir(), "609-ui-upload-test-"));
const port = await availablePort();
const edge = spawn(edgePath, [
  "--headless=new",
  "--disable-gpu",
  "--no-first-run",
  "--no-default-browser-check",
  `--remote-debugging-port=${port}`,
  `--user-data-dir=${userDataDir}`,
  "--window-size=1440,1000",
  siteUrl,
], {
  stdio: "ignore",
});

try {
  const target = await waitForTarget(port);
  const cdp = await connectCdp(target.webSocketDebuggerUrl);
  const requests = new Map();
  const respondedRequests = new Set();
  const networkLog = [];
  const consoleLog = [];
  const progressSamples = [];
  let activeFileNameSeen = false;

  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("DOM.enable");
  await cdp.send("Network.enable");
  cdp.on("Network.requestWillBeSent", ({ requestId, request }) => {
    requests.set(requestId, { method: request.method, url: request.url });
  });
  cdp.on("Network.responseReceived", ({ requestId, response }) => {
    if (!isRelevantUrl(response.url)) return;
    respondedRequests.add(requestId);
    networkLog.push({
      event: "response",
      method: requests.get(requestId)?.method,
      status: response.status,
      url: safeUrl(response.url),
    });
  });
  cdp.on("Network.loadingFailed", ({ requestId, errorText, corsErrorStatus }) => {
    if (respondedRequests.has(requestId)) return;
    const request = requests.get(requestId);
    if (!request || !isRelevantUrl(request.url)) return;
    networkLog.push({
      event: "failed",
      method: request.method,
      error: errorText,
      cors_error: corsErrorStatus,
      url: safeUrl(request.url),
    });
  });
  cdp.on("Runtime.consoleAPICalled", ({ type, args }) => {
    if (type !== "warning" && type !== "error") return;
    consoleLog.push({
      type,
      text: args.map((arg) => arg.value || arg.description || "").join(" "),
    });
  });

  await waitFor(cdp, `location.hostname === "print.example.com"`);
  await evaluate(cdp, `localStorage.setItem("609-reading-room:user-name", "Codex UI Upload Test")`);
  await cdp.send("Page.reload");
  await waitFor(cdp, `Boolean(document.querySelector('input[type="file"]'))`);
  requests.clear();
  respondedRequests.clear();
  networkLog.length = 0;
  consoleLog.length = 0;

  const document = await cdp.send("DOM.getDocument");
  const fileInput = await cdp.send("DOM.querySelector", {
    nodeId: document.root.nodeId,
    selector: 'input[type="file"]',
  });
  if (!fileInput.nodeId) {
    throw new Error("file input was not found");
  }

  const startedAt = performance.now();
  await cdp.send("DOM.setFileInputFiles", {
    files: [filePath],
    nodeId: fileInput.nodeId,
  });

  while (performance.now() - startedAt < 2 * 60_000) {
    const state = await evaluate(cdp, `(() => {
      const bars = Array.from(document.querySelectorAll('div[style*="width"]'));
      const progress = bars
        .map((element) => element.style.width)
        .find((width) => width.endsWith("%")) || null;
      const errors = Array.from(document.querySelectorAll(".text-danger, .text-warning"))
        .map((element) => element.textContent?.trim())
        .filter(Boolean);
      const modes = Array.from(document.querySelectorAll("button"))
        .filter((button) => button.className.includes("rounded-[20px]"));
      const cards = Array.from(document.querySelectorAll("div"))
        .filter((element) => element.className.includes("rounded-2xl")
          && element.className.includes("border-line")
          && element.querySelectorAll("p").length === 2)
        .map((element) => element.textContent?.trim());
      return {
        active_file_name: Array.from(document.querySelectorAll("p"))
          .map((element) => element.textContent?.trim())
          .find((text) => text === ${JSON.stringify(path.basename(filePath))}) || null,
        progress,
        errors,
        mode_count: modes.length,
        info_cards: cards,
      };
    })()`);

    if (state.active_file_name) {
      activeFileNameSeen = true;
    }
    if (
      state.active_file_name
      && state.progress
      && !(progressSamples.includes("100%") && state.progress === "0%")
      && progressSamples.at(-1) !== state.progress
    ) {
      progressSamples.push(state.progress);
    }
    if (state.errors.length > 0) {
      throw new Error(`print site displayed an error: ${state.errors.join(" | ")}`);
    }
    if (state.mode_count === 2) {
      const pageCard = state.info_cards.find((card) => card?.endsWith("13"));
      if (!pageCard) {
        throw new Error(`ready UI did not show the expected 13 pages: ${JSON.stringify(state.info_cards)}`);
      }

      console.log(JSON.stringify({
        site_url: siteUrl,
        file_name: path.basename(filePath),
        file_size: fileInfo.size,
        ready_elapsed_ms: Math.round(performance.now() - startedAt),
        active_file_name_seen: activeFileNameSeen,
        progress_samples: progressSamples,
        info_cards: state.info_cards,
        errors: state.errors,
        network_log: networkLog,
        console_log: consoleLog,
        stopped_before_payment_or_print: true,
      }, null, 2));
      cdp.close();
      process.exitCode = 0;
      break;
    }

    await sleep(250);
  }

  if (process.exitCode !== 0) {
    throw new Error("print site UI upload did not become ready within 120 seconds");
  }
} finally {
  edge.kill();
  await rm(userDataDir, { recursive: true, force: true }).catch(() => undefined);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function availablePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : 0;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function waitForTarget(port) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/list`);
      const targets = await response.json();
      const target = targets.find((candidate) => candidate.type === "page");
      if (target?.webSocketDebuggerUrl) return target;
    } catch {
      // Edge may still be starting.
    }
    await sleep(100);
  }
  throw new Error("Edge DevTools target did not become available");
}

async function connectCdp(url) {
  const socket = new WebSocket(url);
  let nextId = 1;
  const pending = new Map();
  const listeners = new Map();
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  socket.addEventListener("message", ({ data }) => {
    const message = JSON.parse(String(data));
    if (!message.id) {
      for (const listener of listeners.get(message.method) || []) {
        listener(message.params || {});
      }
      return;
    }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error) request.reject(new Error(message.error.message));
    else request.resolve(message.result);
  });

  return {
    send(method, params = {}) {
      const id = nextId;
      nextId += 1;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        socket.send(JSON.stringify({ id, method, params }));
      });
    },
    close() {
      socket.close();
    },
    on(method, listener) {
      const methodListeners = listeners.get(method) || [];
      methodListeners.push(listener);
      listeners.set(method, methodListeners);
    },
  };
}

async function evaluate(cdp, expression) {
  const result = await cdp.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.text || "Runtime.evaluate failed");
  }
  return result.result.value;
}

async function waitFor(cdp, expression) {
  for (let attempt = 0; attempt < 300; attempt += 1) {
    try {
      if (await evaluate(cdp, expression)) return;
    } catch {
      // Navigation can briefly invalidate the current execution context.
    }
    await sleep(100);
  }
  throw new Error(`condition was not met: ${expression}`);
}

function isRelevantUrl(url) {
  return url.includes("/api/trpc")
    || url.includes("r2.cloudflarestorage.com");
}

function safeUrl(value) {
  const url = new URL(value);
  return `${url.origin}${url.pathname}`;
}
