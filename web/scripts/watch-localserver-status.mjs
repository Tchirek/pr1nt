const tunnelUrl = (process.env.PRINT_LOCALSERVER_TUNNEL || "https://print-api.example.com").replace(/\/$/, "");
const fileNameFilter = process.argv[2] || "";
const timeoutMs = Number(process.env.STATUS_WATCH_TIMEOUT_MS || 10 * 60_000);
const websocketUrl = `${tunnelUrl.replace(/^http/u, "ws")}/ws/status`;
const startedAt = performance.now();
let sawReceiving = false;
let sawComplete = false;

const socket = new WebSocket(websocketUrl);
const timeout = setTimeout(() => {
  socket.close();
  throw new Error(`status watch timed out after ${timeoutMs}ms`);
}, timeoutMs);

socket.addEventListener("open", () => {
  console.log(JSON.stringify({ event: "connected", websocket_url: websocketUrl }));
});

socket.addEventListener("message", ({ data }) => {
  const event = JSON.parse(String(data));
  const activity = event.activity;
  if (!activity || (fileNameFilter && activity.file_name !== fileNameFilter)) {
    return;
  }

  const summary = {
    event: "activity",
    elapsed_ms: Math.round(performance.now() - startedAt),
    file_name: activity.file_name,
    stage: activity.stage,
    received_bytes: activity.received_bytes,
    total_bytes: activity.total_bytes,
    percent: activity.percent,
    status: event.status,
    detail: event.detail,
  };
  console.log(JSON.stringify(summary));

  if (activity.stage === "receiving") {
    sawReceiving = true;
  }
  if (sawReceiving && activity.percent >= 100) {
    sawComplete = true;
    clearTimeout(timeout);
    socket.close();
  }
});

socket.addEventListener("close", () => {
  clearTimeout(timeout);
  if (!sawComplete) {
    throw new Error("status socket closed before receiving reached 100%");
  }
  console.log(JSON.stringify({
    event: "complete",
    file_name: fileNameFilter || null,
    receiving_observed: sawReceiving,
    percent_100_observed: sawComplete,
  }, null, 2));
});

socket.addEventListener("error", () => {
  clearTimeout(timeout);
  throw new Error(`failed to connect to ${websocketUrl}`);
});
