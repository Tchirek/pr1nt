const SESSION_TOKEN_KEY = "609-local-admin:token";
const SAMPLE_PAGES = 12;

const state = {
  token: sessionStorage.getItem(SESSION_TOKEN_KEY) ?? "",
  config: {
    prices: { bw_per_page: 0, color_per_page: 0 },
    qrcodes: { alipay_url: "", wechat_url: "" },
    notice_markdown: "",
    printers: { bw: "", color: "" },
    document_converter: {
      kind: "libreoffice",
      libreoffice_path: "soffice",
    },
  },
  printers: [],
  jobs: { active: [], history: [] },
  liveActivities: [],
  liveStream: {
    connected: false,
    lastEventAt: "",
  },
  diagnostics: {
    generated_at: "",
    summary: null,
    local_ws_url: "",
    checks: [],
    websocket_probe: null,
  },
};

const elements = {
  authForm: document.querySelector("#auth-form"),
  authStatus: document.querySelector("#auth-status"),
  tokenInput: document.querySelector("#admin-token"),
  flash: document.querySelector("#flash"),
  priceBw: document.querySelector("#price-bw"),
  priceColor: document.querySelector("#price-color"),
  priceExample: document.querySelector("#price-example"),
  printerBw: document.querySelector("#printer-bw"),
  printerColor: document.querySelector("#printer-color"),
  converterKind: document.querySelector("#converter-kind"),
  libreofficePath: document.querySelector("#libreoffice-path"),
  converterHelp: document.querySelector("#converter-help"),
  qrAlipayFile: document.querySelector("#qr-alipay-file"),
  qrWechatFile: document.querySelector("#qr-wechat-file"),
  qrAlipayPreview: document.querySelector("#qr-alipay-preview"),
  qrWechatPreview: document.querySelector("#qr-wechat-preview"),
  noticeMarkdown: document.querySelector("#notice-markdown"),
  noticePreview: document.querySelector("#notice-preview"),
  saveConfigButton: document.querySelector("#save-config"),
  runDiagnosticsButton: document.querySelector("#run-diagnostics"),
  probeWebSocketButton: document.querySelector("#probe-websocket"),
  diagnosticsMeta: document.querySelector("#diagnostics-meta"),
  diagnosticsList: document.querySelector("#diagnostics-list"),
  refreshPrintersButton: document.querySelector("#refresh-printers"),
  refreshJobsButton: document.querySelector("#refresh-jobs"),
  activeJobs: document.querySelector("#active-jobs"),
  historyJobs: document.querySelector("#history-jobs"),
  liveStreamStatus: document.querySelector("#live-stream-status"),
  liveActivities: document.querySelector("#live-activities"),
};

let statusStreamSocket = null;
let statusStreamReconnectTimer = null;
let statusStreamReconnectAttempt = 0;

bootstrap().catch((error) => {
  showFlash(error.message, "error");
});

async function bootstrap() {
  elements.tokenInput.value = state.token;
  bindEvents();
  renderNoticePreview();
  renderPriceExample();
  renderLiveActivities();

  if (state.token) {
    await loadAll();
    connectStatusStream();
    elements.authStatus.textContent = "\u5df2\u8fde\u63a5\u672c\u5730\u7ba1\u7406 API\u3002";
  }

  window.setInterval(() => {
    if (!state.token) {
      return;
    }
    void Promise.all([loadJobs(), loadLiveActivities()]);
  }, 5000);
}

function bindEvents() {
  // 添加全局按钮点击波纹效果
  document.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (button && !button.disabled) {
      createRipple(button, event);
    }
  });

  elements.authForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = elements.authForm.querySelector("button[type='submit']");
    setButtonLoading(button, true);
    
    try {
      state.token = elements.tokenInput.value.trim();
      sessionStorage.setItem(SESSION_TOKEN_KEY, state.token);

      if (!state.token) {
        showFlash("\u8bf7\u5148\u8f93\u5165\u7ba1\u7406\u4ee4\u724c\u3002", "error");
        return;
      }

      await loadAll();
      connectStatusStream();
      elements.authStatus.textContent = "\u5df2\u8fde\u63a5\u672c\u5730\u7ba1\u7406API\u3002";
      showFlash("\u7ba1\u7406\u7aef\u8fde\u63a5\u6210\u529f\u3002", "success");
    } finally {
      setButtonLoading(button, false);
    }
  });

  elements.priceBw.addEventListener("input", renderPriceExample);
  elements.priceColor.addEventListener("input", renderPriceExample);
  elements.noticeMarkdown.addEventListener("input", renderNoticePreview);
  elements.converterKind.addEventListener("change", renderConverterHelp);
  elements.libreofficePath.addEventListener("input", renderConverterHelp);
  elements.runDiagnosticsButton.addEventListener("click", async () => {
    await withButtonLoading(elements.runDiagnosticsButton, () => loadDiagnostics());
  });
  elements.probeWebSocketButton.addEventListener("click", async () => {
    await runWebSocketProbe();
  });
  elements.refreshPrintersButton.addEventListener("click", async () => {
    await withButtonLoading(elements.refreshPrintersButton, () => loadPrinters());
  });
  elements.refreshJobsButton.addEventListener("click", async () => {
    await withButtonLoading(elements.refreshJobsButton, () => loadJobs());
  });
  elements.saveConfigButton.addEventListener("click", async () => {
    await withButtonLoading(elements.saveConfigButton, () => saveConfig());
  });
  elements.qrAlipayFile.addEventListener("change", async (event) => {
    state.config.qrcodes.alipay_url = await fileInputToDataUrl(event.target);
    renderQrPreviews();
  });
  elements.qrWechatFile.addEventListener("change", async (event) => {
    state.config.qrcodes.wechat_url = await fileInputToDataUrl(event.target);
    renderQrPreviews();
  });

  elements.activeJobs.addEventListener("click", handleJobAction);
  elements.historyJobs.addEventListener("click", handleJobAction);
}

// 按钮加载状态辅助函数
function setButtonLoading(button, isLoading) {
  if (!button) return;
  if (isLoading) {
    button.classList.add("loading");
    button.disabled = true;
  } else {
    button.classList.remove("loading");
    button.disabled = false;
  }
}

// 包裹异步函数，自动管理按钮加载状态
async function withButtonLoading(button, fn) {
  try {
    setButtonLoading(button, true);
    await fn();
  } finally {
    setButtonLoading(button, false);
  }
}

// 创建波纹点击效果
function createRipple(button, event) {
  const rect = button.getBoundingClientRect();
  const ripple = document.createElement("span");
  ripple.className = "ripple";
  
  const size = Math.max(rect.width, rect.height);
  ripple.style.width = ripple.style.height = `${size}px`;
  ripple.style.left = `${event.clientX - rect.left - size / 2}px`;
  ripple.style.top = `${event.clientY - rect.top - size / 2}px`;
  
  button.appendChild(ripple);
  
  setTimeout(() => {
    ripple.remove();
  }, 600);
}

// 添加按钮成功反馈
function flashButtonSuccess(button) {
  button.classList.add("success-flash");
  setTimeout(() => {
    button.classList.remove("success-flash");
  }, 600);
}

async function loadAll() {
  await Promise.all([loadConfig(), loadPrinters(), loadJobs(), loadLiveActivities()]);
  await loadDiagnostics();
}

async function loadConfig() {
  const data = await api("/admin/config", { method: "GET" });
  state.config = {
    prices: data.prices ?? state.config.prices,
    qrcodes: data.qrcodes ?? state.config.qrcodes,
    notice_markdown: data.notice_markdown ?? "",
    printers: data.printers ?? state.config.printers,
    document_converter: data.document_converter ?? state.config.document_converter,
  };
  populateForm();
}

async function loadPrinters() {
  state.printers = await api("/admin/printers", { method: "GET" });
  renderPrinterOptions();
}

async function loadJobs() {
  state.jobs = await api("/admin/jobs", { method: "GET" });
  renderJobs();
}

async function loadLiveActivities() {
  state.liveActivities = await api("/admin/live-activities", { method: "GET" });
  renderLiveActivities();
}

async function loadDiagnostics() {
  setDiagnosticsBusy(true, "\u6B63\u5728\u8FD0\u884C\u8BCA\u65AD\u2026");
  try {
    const data = await api("/admin/diagnostics", { method: "GET" });
    state.diagnostics = {
      generated_at: data.generated_at ?? "",
      summary: data.summary ?? null,
      local_ws_url: data.local_ws_url ?? buildLocalWebSocketUrl(),
      checks: Array.isArray(data.checks) ? data.checks : [],
      websocket_probe: state.diagnostics.websocket_probe,
    };
    renderDiagnostics();
  } finally {
    setDiagnosticsBusy(false);
  }
}

function populateForm() {
  elements.priceBw.value = `${state.config.prices.bw_per_page ?? 0}`;
  elements.priceColor.value = `${state.config.prices.color_per_page ?? 0}`;
  elements.noticeMarkdown.value = state.config.notice_markdown ?? "";
  elements.converterKind.value = state.config.document_converter.kind ?? "libreoffice";
  elements.libreofficePath.value = state.config.document_converter.libreoffice_path ?? "soffice";
  renderNoticePreview();
  renderQrPreviews();
  renderPrinterOptions();
  renderPriceExample();
  renderConverterHelp();
}

function renderPriceExample() {
  const bw = Number.parseFloat(elements.priceBw.value || "0");
  const color = Number.parseFloat(elements.priceColor.value || "0");
  elements.priceExample.textContent =
    `\u9ed1\u767d ${SAMPLE_PAGES} \u9875 = ${(bw * SAMPLE_PAGES).toFixed(2)} \u5143 | ` +
    `\u5f69\u8272 ${SAMPLE_PAGES} \u9875 = ${(color * SAMPLE_PAGES).toFixed(2)} \u5143`;
}

function renderQrPreviews() {
  elements.qrAlipayPreview.src = state.config.qrcodes.alipay_url || "";
  elements.qrWechatPreview.src = state.config.qrcodes.wechat_url || "";
}

function renderPrinterOptions() {
  renderSelectOptions(elements.printerBw, state.printers, state.config.printers.bw);
  renderSelectOptions(elements.printerColor, state.printers, state.config.printers.color);
}

function renderConverterHelp() {
  const kind = elements.converterKind.value || "libreoffice";
  const libreofficePath = elements.libreofficePath.value.trim() || "soffice";
  elements.libreofficePath.disabled = kind !== "libreoffice";

  state.config.document_converter = {
    kind,
    libreoffice_path: libreofficePath,
  };

  if (kind === "libreoffice") {
    elements.converterHelp.textContent =
      `\u9002\u5408\u90E8\u7F72\u673A\u5DF2\u5B89\u88C5 LibreOffice \u7684\u573A\u666F\u3002\u5C06\u4F7F\u7528 ${libreofficePath} \u8FDB\u884C\u65E0\u754C\u9762\u8F6C PDF\u3002`;
    return;
  }

  if (kind === "wps") {
    elements.converterHelp.textContent =
      "\u4F7F\u7528 WPS Office \u7684 COM \u81EA\u52A8\u5316\u63A5\u53E3\u8F6C PDF\u3002\u9002\u5408\u90E8\u7F72\u673A\u5DF2\u5B89\u88C5 WPS \u7684\u573A\u666F\u3002OpenDocument \u6587\u4EF6\u8BF7\u5207\u56DE LibreOffice\u3002";
    return;
  }

  elements.converterHelp.textContent =
    "\u4F7F\u7528 Microsoft Office \u7684 COM \u81EA\u52A8\u5316\u63A5\u53E3\u8F6C PDF\u3002\u9002\u5408\u90E8\u7F72\u673A\u5DF2\u5B89\u88C5 Office \u7684\u573A\u666F\u3002OpenDocument \u6587\u4EF6\u8BF7\u5207\u56DE LibreOffice\u3002";
}

function renderSelectOptions(select, printerNames, selectedPrinter) {
  const options = printerNames.length > 0 ? printerNames : [selectedPrinter].filter(Boolean);
  select.innerHTML = options
    .map((printerName) => {
      const selected = printerName === selectedPrinter ? " selected" : "";
      return `<option value="${escapeHtml(printerName)}"${selected}>${escapeHtml(printerName)}</option>`;
    })
    .join("");
}

function renderNoticePreview() {
  state.config.notice_markdown = elements.noticeMarkdown.value;
  elements.noticePreview.innerHTML = markdownToHtml(state.config.notice_markdown);
}

function renderJobs() {
  renderJobList(elements.activeJobs, state.jobs.active, true);
  renderJobList(elements.historyJobs, state.jobs.history, false);
}

function renderLiveActivities() {
  const activities = [...state.liveActivities].sort((left, right) =>
    String(right.updated_at ?? "").localeCompare(String(left.updated_at ?? "")),
  );

  const streamLabel = state.liveStream.connected
    ? `实时状态已连接${state.liveStream.lastEventAt ? ` | 最近更新 ${formatDate(state.liveStream.lastEventAt)}` : ""}`
    : "实时状态未连接，当前会自动继续重连。";
  elements.liveStreamStatus.textContent = streamLabel;

  if (!activities.length) {
    elements.liveActivities.className = "jobs-list empty-state";
    elements.liveActivities.textContent = "还没有实时活动。上传文件或转换文档后，这里会立即出现进度。";
    return;
  }

  elements.liveActivities.className = "jobs-list";
  elements.liveActivities.innerHTML = activities
    .map((activity) => {
      const progressPercent =
        typeof activity.percent === "number" && Number.isFinite(activity.percent) ? Math.max(0, Math.min(100, activity.percent)) : null;
      const stage = normalizeActivityStage(activity.stage);
      const byteText = formatByteProgress(activity.received_bytes, activity.total_bytes);
      const userMeta = activity.user_name ? `<div><strong>用户</strong><br />${escapeHtml(activity.user_name)}</div>` : "";
      const printerMeta = activity.printer ? `<div><strong>模式</strong><br />${escapeHtml(activity.printer === "color" ? "彩色" : "黑白")}</div>` : "";

      return `
        <article class="job-card live-card">
          <div class="panel-header">
            <div>
              <p class="eyebrow">${escapeHtml(activity.kind === "convert_preview" ? "Convert Preview" : "Print Upload")}</p>
              <h3>${escapeHtml(activity.file_name || "未命名文件")}</h3>
            </div>
            <span class="tag ${activityStageTagClass(stage)}">${activityStageLabel(stage)}</span>
          </div>
          <p class="muted">${escapeHtml(activity.summary ?? "")}</p>
          ${progressPercent !== null ? `
            <div class="progress-block">
              <div class="progress-label">
                <span>${escapeHtml(byteText)}</span>
                <strong>${progressPercent}%</strong>
              </div>
              <div class="progress-bar" aria-hidden="true"><span style="width:${progressPercent}%"></span></div>
            </div>
          ` : `<p class="muted">${escapeHtml(byteText)}</p>`}
          ${activity.detail ? `<p class="muted">${escapeHtml(activity.detail)}</p>` : ""}
          <div class="job-meta">
            <div><strong>链路</strong><br />${escapeHtml(activity.kind === "convert_preview" ? "预览转换" : "打印上传")}</div>
            ${userMeta}
            ${printerMeta}
            <div><strong>更新</strong><br />${formatDate(activity.updated_at)}</div>
          </div>
        </article>
      `;
    })
    .join("");
}

function renderDiagnostics() {
  const checks = [...state.diagnostics.checks];
  if (state.diagnostics.websocket_probe) {
    checks.push(state.diagnostics.websocket_probe);
  }

  if (!checks.length) {
    elements.diagnosticsList.className = "diagnostics-list empty-state";
    elements.diagnosticsList.textContent = "\u8FD8\u6CA1\u6709\u8BCA\u65AD\u7ED3\u679C\u3002";
    elements.diagnosticsMeta.textContent = "\u70B9\u51FB\u300C\u8FD0\u884C\u8BCA\u65AD\u300D\u5F00\u59CB\u68C0\u67E5\u3002";
    return;
  }

  const summary = state.diagnostics.summary;
  const summaryText = summary
    ? `\u901A\u8FC7 ${summary.passed} \u9879 | \u8B66\u544A ${summary.warned} \u9879 | \u5931\u8D25 ${summary.failed} \u9879`
    : "\u8BCA\u65AD\u5DF2\u5B8C\u6210";
  const generatedAt = state.diagnostics.generated_at ? formatDate(state.diagnostics.generated_at) : "--";
  const localWsUrl = state.diagnostics.local_ws_url || buildLocalWebSocketUrl();
  elements.diagnosticsMeta.textContent =
    `${summaryText} | ${generatedAt} | WS: ${localWsUrl}`;

  elements.diagnosticsList.className = "diagnostics-list";
  elements.diagnosticsList.innerHTML = checks
    .map((check) => {
      const status = normalizeDiagnosticStatus(check.status);
      const detail = check.detail ? `<pre class="diagnostic-detail">${escapeHtml(check.detail)}</pre>` : "";
      const hint = check.hint ? `<p class="diagnostic-hint">${escapeHtml(check.hint)}</p>` : "";

      return `
        <article class="diagnostic-card" data-status="${status}">
          <div class="panel-header">
            <div>
              <p class="eyebrow">${escapeHtml(check.id ?? status)}</p>
              <h3>${escapeHtml(check.label ?? "\u672A\u547D\u540D\u8BCA\u65AD")}</h3>
            </div>
            <span class="tag status-${status}">${diagnosticStatusLabel(status)}</span>
          </div>
          <p>${escapeHtml(check.summary ?? "")}</p>
          ${detail}
          ${hint}
        </article>
      `;
    })
    .join("");
}

function renderJobList(target, jobs, active) {
  if (!jobs.length) {
    target.className = "jobs-list empty-state";
    target.textContent = active
      ? "\u6682\u65e0\u6d3b\u52a8\u4efb\u52a1\u3002"
      : "\u6682\u65e0\u5386\u53f2\u8bb0\u5f55\u3002";
    return;
  }

  target.className = "jobs-list";
  target.innerHTML = jobs
    .map((job) => {
      const status = escapeHtml(job.status);
      const detail = escapeHtml(job.detail ?? "");
      const printer = job.printer === "color" ? "\u5f69\u8272" : "\u9ed1\u767d";
      const progressPercent = calculatePrintProgress(job.pages_printed, job.total_pages, job.status);
      const progressLabel = formatPrintProgress(job.pages_printed, job.total_pages, job.status);
      const copyCount = Number.isFinite(job.copy_count) && job.copy_count > 1 ? job.copy_count : 1;
      const pageLabel = copyCount > 1 ? `${job.page_count} \u9875 \u00d7 ${copyCount} \u4efd` : `${job.page_count} \u9875`;
      const actionButtons = active
        ? `<button class="secondary" data-action="cancel" data-job-id="${escapeHtml(job.id)}">\u53d6\u6d88</button>`
        : `<button class="secondary" data-action="retry" data-job-id="${escapeHtml(job.id)}">\u91cd\u8bd5</button>`;

      return `
        <article class="job-card">
          <div class="panel-header">
            <div>
              <p class="eyebrow">${escapeHtml(job.job?.id ?? job.id)}</p>
              <h3>${escapeHtml(job.file_name)}</h3>
            </div>
            <span class="tag">${status}</span>
          </div>
          <p class="muted">${escapeHtml(job.user_name)} | ${printer} | ${pageLabel}</p>
          ${detail ? `<p class="muted">${detail}</p>` : ""}
          <div class="progress-block">
            <div class="progress-label">
              <span>打印进度</span>
              <strong>${escapeHtml(progressLabel)}</strong>
            </div>
            <div class="progress-bar" aria-hidden="true"><span style="width:${progressPercent}%"></span></div>
          </div>
          <div class="job-meta">
            <div><strong>\u63d0\u4ea4</strong><br />${formatDate(job.submitted_at)}</div>
            <div><strong>\u66f4\u65b0</strong><br />${formatDate(job.updated_at)}</div>
            <div><strong>\u5c1d\u8bd5</strong><br />${job.attempts}</div>
          </div>
          <div class="job-actions">${actionButtons}</div>
        </article>
      `;
    })
    .join("");
}

async function handleJobAction(event) {
  const button = event.target.closest("button[data-action]");
  if (!button) {
    return;
  }

  const jobId = button.dataset.jobId;
  const action = button.dataset.action;
  if (!jobId || !action) {
    return;
  }

  const originalText = button.textContent;
  button.disabled = true;
  button.textContent = action === "retry" ? "重试中..." : "取消中...";
  
  try {
    const route = action === "retry" ? `/admin/jobs/${jobId}/retry` : `/admin/jobs/${jobId}/cancel`;
    await api(route, { method: "POST" });
    await loadJobs();
    showFlash(
      action === "retry"
        ? "\u5df2\u91cd\u8bd5\u8be5\u6253\u5370\u4efb\u52a1\u3002"
        : "\u5df2\u53d6\u6d88\u8be5\u6253\u5370\u4efb\u52a1\u3002",
      "success",
    );
  } catch (error) {
    button.disabled = false;
    button.textContent = originalText;
    showFlash(error.message || "操作失败，请重试", "error");
  }
}

async function saveConfig() {
  state.config = {
    prices: {
      bw_per_page: Number.parseFloat(elements.priceBw.value || "0"),
      color_per_page: Number.parseFloat(elements.priceColor.value || "0"),
    },
    qrcodes: state.config.qrcodes,
    notice_markdown: elements.noticeMarkdown.value,
    printers: {
      bw: elements.printerBw.value,
      color: elements.printerColor.value,
    },
    document_converter: {
      kind: elements.converterKind.value,
      libreoffice_path: elements.libreofficePath.value.trim() || "soffice",
    },
  };

  await api("/admin/config", {
    method: "POST",
    body: JSON.stringify(state.config),
    headers: {
      "Content-Type": "application/json",
    },
  });

  showFlash("\u914d\u7f6e\u5df2\u4fdd\u5b58\u3002Cloudflare KV \u548C\u672C\u5730\u8F6C\u6362\u5668\u8BBE\u7F6E\u90FD\u5DF2\u66F4\u65B0\u3002", "success");
  await loadDiagnostics();
}

async function runWebSocketProbe() {
  const wsUrl = state.diagnostics.local_ws_url || buildLocalWebSocketUrl();
  elements.probeWebSocketButton.disabled = true;
  state.diagnostics.websocket_probe = {
    id: "websocket-probe",
    label: "\u672C\u673A WebSocket \u5E7F\u64AD",
    status: "warn",
    summary: `\u6B63\u5728\u6D4B\u8BD5 ${wsUrl}`,
    detail: "",
    hint: "\u8FD9\u4E00\u9879\u4F1A\u5728\u6D4F\u89C8\u5668\u548C\u672C\u673A Axum \u516C\u7F51\u7AEF\u53E3\u4E4B\u95F4\u53D1\u9001\u4E00\u6B21\u6D4B\u8BD5\u5E7F\u64AD\u3002",
  };
  renderDiagnostics();

  let socket = null;
  try {
    socket = await openWebSocket(wsUrl);
    const messages = [];
    socket.addEventListener("message", (event) => {
      try {
        messages.push(JSON.parse(event.data));
      } catch {
      }
    });

    const probe = await api("/admin/diagnostics/ws-probe", { method: "POST" });
    const matched = await waitForMessage(messages, probe.job_id, 4000);
    state.diagnostics.websocket_probe = {
      id: "websocket-probe",
      label: "\u672C\u673A WebSocket \u5E7F\u64AD",
      status: "pass",
      summary: "\u6D4F\u89C8\u5668\u5DF2\u6536\u5230\u672C\u673A\u72B6\u6001\u5E7F\u64AD\u3002",
      detail: matched.detail || probe.detail || probe.job_id,
      hint: null,
    };
    renderDiagnostics();
    showFlash("WebSocket \u6D4B\u8BD5\u901A\u8FC7\u3002", "success");
  } catch (error) {
    state.diagnostics.websocket_probe = {
      id: "websocket-probe",
      label: "\u672C\u673A WebSocket \u5E7F\u64AD",
      status: "fail",
      summary: "WebSocket \u6D4B\u8BD5\u5931\u8D25\u3002",
      detail: error instanceof Error ? error.message : "\u672A\u77E5\u9519\u8BEF",
      hint: "\u68C0\u67E5 8788 \u7AEF\u53E3\u662F\u5426\u5DF2\u542F\u52A8\uFF0C\u4EE5\u53CA\u6D4F\u89C8\u5668\u662F\u5426\u88AB\u5B89\u5168\u8F6F\u4EF6\u62E6\u622A\u672C\u673A WebSocket \u8FDE\u63A5\u3002",
    };
    renderDiagnostics();
    showFlash(error instanceof Error ? error.message : "WebSocket \u6D4B\u8BD5\u5931\u8D25\u3002", "error");
  } finally {
    socket?.close();
    elements.probeWebSocketButton.disabled = false;
  }
}

function connectStatusStream() {
  if (!state.token) {
    return;
  }

  if (statusStreamReconnectTimer !== null) {
    window.clearTimeout(statusStreamReconnectTimer);
    statusStreamReconnectTimer = null;
  }

  if (statusStreamSocket && (statusStreamSocket.readyState === WebSocket.OPEN || statusStreamSocket.readyState === WebSocket.CONNECTING)) {
    return;
  }

  const wsUrl = buildLocalWebSocketUrl();
  statusStreamSocket = new WebSocket(wsUrl);
  elements.liveStreamStatus.textContent = `正在连接 ${wsUrl}`;

  statusStreamSocket.addEventListener("open", () => {
    state.liveStream.connected = true;
    statusStreamReconnectAttempt = 0;
    renderLiveActivities();
  });

  statusStreamSocket.addEventListener("message", (event) => {
    let payload = null;
    try {
      payload = JSON.parse(event.data);
    } catch {
      return;
    }

    state.liveStream.connected = true;
    state.liveStream.lastEventAt = new Date().toISOString();
    applyStatusStreamEvent(payload);
    renderLiveActivities();
  });

  statusStreamSocket.addEventListener("error", () => {
    state.liveStream.connected = false;
    renderLiveActivities();
  });

  statusStreamSocket.addEventListener("close", () => {
    state.liveStream.connected = false;
    renderLiveActivities();
    statusStreamSocket = null;

    if (!state.token) {
      return;
    }

    const reconnectDelay = Math.min(5000, 1200 + statusStreamReconnectAttempt * 600);
    statusStreamReconnectAttempt += 1;
    statusStreamReconnectTimer = window.setTimeout(() => {
      connectStatusStream();
    }, reconnectDelay);
  });
}

function applyStatusStreamEvent(payload) {
  if (!payload || typeof payload !== "object") {
    return;
  }

  if (payload.kind === "activity" && payload.activity) {
    upsertLiveActivity(payload.activity);
    return;
  }

  if (payload.kind === "job" && payload.job_id) {
    applyJobEvent(payload);
  }
}

function upsertLiveActivity(activity) {
  const next = [...state.liveActivities];
  const index = next.findIndex((item) => item.id === activity.id);
  if (index >= 0) {
    next[index] = { ...next[index], ...activity };
  } else {
    next.unshift(activity);
  }
  state.liveActivities = next.slice(0, 48);
}

function applyJobEvent(payload) {
  const nextJobs = [...state.jobs.active, ...state.jobs.history];
  const index = nextJobs.findIndex((job) => job.id === payload.job_id);
  if (index < 0) {
    void loadJobs();
    return;
  }

  const updated = {
    ...nextJobs[index],
    status: payload.status ?? nextJobs[index].status,
    detail: payload.detail ?? nextJobs[index].detail,
    pages_printed: typeof payload.pages_printed === "number" ? payload.pages_printed : nextJobs[index].pages_printed,
    total_pages: typeof payload.total_pages === "number" ? payload.total_pages : nextJobs[index].total_pages,
    updated_at: new Date().toISOString(),
  };
  nextJobs[index] = updated;

  state.jobs = {
    active: nextJobs
      .filter((job) => job.status !== "done" && job.status !== "failed")
      .sort((left, right) => String(left.updated_at ?? "").localeCompare(String(right.updated_at ?? ""))),
    history: nextJobs
      .filter((job) => job.status === "done" || job.status === "failed")
      .sort((left, right) => String(right.updated_at ?? "").localeCompare(String(left.updated_at ?? ""))),
  };
  renderJobs();
}

async function api(path, options) {
  if (!state.token) {
    throw new Error("\u8bf7\u5148\u8fde\u63a5\u7ba1\u7406\u4ee4\u724c\u3002");
  }

  const response = await fetch(path, {
    ...options,
    headers: {
      Authorization: `Bearer ${state.token}`,
      ...(options?.headers ?? {}),
    },
  });

  if (!response.ok) {
    const payload = await response.json().catch(() => null);
    if (response.status === 401) {
      throw new Error("管理令牌无效。请确认 local-server/.env 里的 LOCAL_ADMIN_TOKEN，或重新输入正确令牌。");
    }
    throw new Error(payload?.error ?? `Request failed with status ${response.status}`);
  }

  return response.json().catch(() => null);
}

async function fileInputToDataUrl(target) {
  const file = target.files?.[0];
  if (!file) {
    return "";
  }

  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : "");
    reader.onerror = () => reject(new Error("\u65e0\u6cd5\u8bfb\u53d6\u4e8c\u7ef4\u7801\u56fe\u7247\u3002"));
    reader.readAsDataURL(file);
  });
}

function markdownToHtml(markdown) {
  const escaped = escapeHtml(markdown);
  const lines = escaped.split(/\r?\n/);
  const html = [];
  let inList = false;

  for (const line of lines) {
    if (/^-\s+/.test(line)) {
      if (!inList) {
        html.push("<ul>");
        inList = true;
      }
      html.push(`<li>${formatInlineMarkdown(line.replace(/^-\s+/, ""))}</li>`);
      continue;
    }

    if (inList) {
      html.push("</ul>");
      inList = false;
    }

    if (!line.trim()) {
      html.push("");
      continue;
    }

    if (line.startsWith("### ")) {
      html.push(`<h3>${formatInlineMarkdown(line.slice(4))}</h3>`);
      continue;
    }
    if (line.startsWith("## ")) {
      html.push(`<h2>${formatInlineMarkdown(line.slice(3))}</h2>`);
      continue;
    }
    if (line.startsWith("# ")) {
      html.push(`<h1>${formatInlineMarkdown(line.slice(2))}</h1>`);
      continue;
    }

    html.push(`<p>${formatInlineMarkdown(line)}</p>`);
  }

  if (inList) {
    html.push("</ul>");
  }

  return html.join("");
}

function formatInlineMarkdown(value) {
  return value
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/`(.+?)`/g, "<code>$1</code>");
}

function formatDate(value) {
  if (!value) {
    return "--";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function showFlash(message, kind) {
  elements.flash.innerHTML = "";
  const textSpan = document.createElement("span");
  textSpan.textContent = message;
  textSpan.style.lineHeight = "1.6";
  
  const closeBtn = document.createElement("button");
  closeBtn.className = "flash-close";
  closeBtn.textContent = "×";
  closeBtn.addEventListener("click", () => {
    elements.flash.classList.add("hidden");
  });
  
  elements.flash.appendChild(textSpan);
  elements.flash.appendChild(closeBtn);
  elements.flash.className = `flash ${kind}`;
  elements.flash.classList.remove("hidden");
  
  // 5秒后自动隐藏成功提示
  if (kind === "success") {
    clearTimeout(elements.flash._timeout);
    elements.flash._timeout = setTimeout(() => {
      elements.flash.classList.add("hidden");
    }, 5000);
  }
}

function setDiagnosticsBusy(isBusy, text = "") {
  elements.runDiagnosticsButton.disabled = isBusy;
  if (isBusy) {
    elements.diagnosticsMeta.textContent = text;
  }
}

function normalizeDiagnosticStatus(status) {
  if (status === "pass" || status === "warn" || status === "fail") {
    return status;
  }
  return "warn";
}

function diagnosticStatusLabel(status) {
  if (status === "pass") {
    return "\u901A\u8FC7";
  }
  if (status === "fail") {
    return "\u5931\u8D25";
  }
  return "\u8B66\u544A";
}

function buildLocalWebSocketUrl() {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.hostname}:8788/ws/status`;
}

function openWebSocket(url) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    const timeout = window.setTimeout(() => {
      socket.close();
      reject(new Error(`WebSocket \u8FDE\u63A5\u8D85\u65F6\uFF1A${url}`));
    }, 3000);

    socket.addEventListener("open", () => {
      window.clearTimeout(timeout);
      resolve(socket);
    }, { once: true });

    socket.addEventListener("error", () => {
      window.clearTimeout(timeout);
      reject(new Error(`WebSocket \u65E0\u6CD5\u8FDE\u4E0A\uFF1A${url}`));
    }, { once: true });
  });
}

function waitForMessage(messages, jobId, timeoutMs) {
  return new Promise((resolve, reject) => {
    const startedAt = Date.now();
    const timer = window.setInterval(() => {
      const matched = messages.find((message) => message?.job_id === jobId);
      if (matched) {
        window.clearInterval(timer);
        resolve(matched);
        return;
      }

      if (Date.now() - startedAt > timeoutMs) {
        window.clearInterval(timer);
        reject(new Error(`\u5728 ${timeoutMs}ms \u5185\u672A\u6536\u5230 job_id=${jobId} \u7684 WebSocket \u5E7F\u64AD\u3002`));
      }
    }, 100);
  });
}

function normalizeActivityStage(stage) {
  if (stage === "receiving" || stage === "received" || stage === "converting" || stage === "ready" || stage === "failed") {
    return stage;
  }
  return "received";
}

function activityStageLabel(stage) {
  switch (stage) {
    case "receiving":
      return "接收中";
    case "received":
      return "已收齐";
    case "converting":
      return "转换中";
    case "ready":
      return "已完成";
    case "failed":
      return "失败";
    default:
      return "处理中";
  }
}

function activityStageTagClass(stage) {
  switch (stage) {
    case "ready":
      return "status-pass";
    case "failed":
      return "status-fail";
    default:
      return "status-warn";
  }
}

function formatByteProgress(receivedBytes, totalBytes) {
  const received = formatBytes(receivedBytes ?? 0);
  if (typeof totalBytes === "number" && Number.isFinite(totalBytes) && totalBytes > 0) {
    return `${received} / ${formatBytes(totalBytes)}`;
  }
  return received;
}

function formatBytes(bytes) {
  const value = Number(bytes ?? 0);
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  if (value >= 1024 * 1024) {
    return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  }
  if (value >= 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${Math.round(value)} B`;
}

function calculatePrintProgress(pagesPrinted, totalPages, status) {
  if (status === "done") {
    return 100;
  }
  if (typeof totalPages === "number" && totalPages > 0 && typeof pagesPrinted === "number") {
    return Math.max(0, Math.min(100, Math.round((pagesPrinted / totalPages) * 100)));
  }
  if (status === "queued" || status === "downloading") {
    return 8;
  }
  if (status === "printing") {
    return 30;
  }
  return 0;
}

function formatPrintProgress(pagesPrinted, totalPages, status) {
  if (status === "queued") {
    return "等待排队";
  }
  if (status === "downloading") {
    return "文件传输";
  }
  if (typeof pagesPrinted === "number" && typeof totalPages === "number" && totalPages > 0) {
    return `${pagesPrinted}/${totalPages} 张`;
  }
  if (status === "done") {
    return "已完成";
  }
  if (status === "failed") {
    return "已失败";
  }
  return "处理中";
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}
