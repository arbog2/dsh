import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createIcons, Languages, PanelTop, RefreshCw, RotateCw, Trash2 } from "lucide";
import "./styles.css";

type Language = "en" | "zh";

interface HarnessStatus {
  revision: number;
  state: "initializing" | "starting" | "running" | "updating" | "stopping" | "error";
  message: string;
  detail: string;
  port: number | null;
  url: string | null;
  progress: number;
  progressLabel: string;
  updateInProgress: boolean;
}

interface LogEvent {
  level: "info" | "warn" | "error";
  line: string;
}

const translations: Record<Language, Record<string, string>> = {
  en: {
    open: "Open Harness",
    showDrawer: "Status and log",
    hideDrawer: "Hide status and log",
    restart: "Restart",
    update: "Update",
    languageShort: "中文",
    switchLanguage: "Switch to Chinese",
    service: "Service",
    activity: "Activity",
    runtimeLog: "Runtime log",
    clearLog: "Clear log",
    waiting: "Waiting for the service.",
    logCleared: "Log cleared.",
    progressAria: "Harness update progress",
    stateInitializing: "Initializing",
    stateStarting: "Starting",
    stateRunning: "Running",
    stateUpdating: "Updating",
    stateStopping: "Stopping",
    stateError: "Error",
    unavailable: "Harness is unavailable",
  },
  zh: {
    open: "打开 Harness",
    showDrawer: "状态与日志",
    hideDrawer: "收起状态与日志",
    restart: "重启",
    update: "更新",
    languageShort: "EN",
    switchLanguage: "切换到英文",
    service: "服务",
    activity: "活动",
    runtimeLog: "运行日志",
    clearLog: "清空日志",
    waiting: "等待服务启动。",
    logCleared: "日志已清空。",
    progressAria: "Harness 更新进度",
    stateInitializing: "初始化中",
    stateStarting: "启动中",
    stateRunning: "运行中",
    stateUpdating: "更新中",
    stateStopping: "停止中",
    stateError: "错误",
    unavailable: "Harness 当前不可用",
  },
};

const chinesePhrases: Record<string, string> = {
  "Preparing the local runtime": "正在准备本地运行环境",
  "The first launch can take a moment while files are prepared.":
    "首次启动需要准备运行时文件，请稍候。",
  "Starting Harness": "正在启动 Harness",
  "Launching the bundled Node.js service.": "正在启动内置 Node.js 服务。",
  "Waiting for the Web service": "正在等待 Web 服务",
  "The Harness window opens when the local endpoint is ready.":
    "本地服务就绪后会自动打开 Harness 窗口。",
  "Health check": "健康检查",
  "Harness is running": "Harness 正在运行",
  "The local Web service is ready.": "本地 Web 服务已就绪。",
  Ready: "就绪",
  "Updating Harness": "正在更新 Harness",
  "Fetching the latest source revision.": "正在获取最新源码版本。",
  "Git fetch": "获取 Git 更新",
  "Applying source": "应用源码",
  "Updating the writable checkout.": "正在更新可写源码目录。",
  "Installing dependencies": "安装依赖",
  "Resolving the locked Harness workspace.": "正在解析 Harness 的锁定工作区依赖。",
  "Cleaning build state": "清理构建状态",
  "Removing stale incremental build artifacts.": "正在删除过期的增量构建产物。",
  "Building Harness": "构建 Harness",
  "Compiling the host, client, and Web application.": "正在编译 Host、Client 和 Web 应用。",
  "Packaging runtime": "打包运行时",
  "Creating a production-only dependency tree.": "正在创建仅包含生产依赖的运行树。",
  "Activating runtime": "启用新运行时",
  "Switching to the newly built Harness release.": "正在切换到新构建的 Harness 版本。",
  "Harness updated": "Harness 已更新",
  "The latest revision is running.": "最新版本正在运行。",
  Updated: "已更新",
  "Update failed": "更新失败",
  "Previous version restored": "已恢复上一版本",
  "Update failed; current runtime kept": "更新失败，继续使用当前运行时",
  "Harness stopped": "Harness 已停止",
  "Stopping Harness": "正在停止 Harness",
  "Closing the local service process tree.": "正在关闭本地服务进程树。",
  Stopping: "停止中",
  "Startup failed": "启动失败",
  "Initialization failed": "初始化失败",
  Initialization: "初始化",
  Error: "错误",
};

const chinesePrefixes: Array<[string, string]> = [
  ["Update failed and the previous version was restored:", "更新失败，已恢复上一版本："],
  ["Update failed:", "更新失败："],
  ["Harness is ready at ", "Harness 已就绪："],
];

let language = loadLanguage();
const elements = {
  statusDot: requiredElement("status-dot"),
  statusLabel: requiredElement("status-label"),
  statusUrl: requiredElement("status-url"),
  statusMessage: requiredElement("status-message"),
  statusDetail: requiredElement("status-detail"),
  progressLabel: requiredElement("progress-label"),
  progressValue: requiredElement("progress-value"),
  progressTrack: requiredElement("progress-track"),
  progressBar: requiredElement("progress-bar"),
  logOutput: requiredElement("log-output"),
  drawerButton: requiredButton("drawer-button"),
  restartButton: requiredButton("restart-button"),
  updateButton: requiredButton("update-button"),
  languageButton: requiredButton("language-button"),
  clearLogButton: requiredButton("clear-log-button"),
};

const logHistory: LogEvent[] = [];
let logEmptyKey = "waiting";
let drawerOpen = true;
let harnessReady = false;

let currentStatus: HarnessStatus = {
  revision: -1,
  state: "initializing",
  message: "Preparing the local runtime",
  detail: "The first launch can take a moment while files are prepared.",
  port: null,
  url: null,
  progress: 0,
  progressLabel: "Initialization",
  updateInProgress: false,
};

createIcons({
  icons: { Languages, PanelTop, RefreshCw, RotateCw, Trash2 },
  attrs: { "stroke-width": "1.8" },
});

elements.drawerButton.addEventListener("click", () => {
  setDrawer(!drawerOpen);
});

elements.restartButton.addEventListener("click", () => {
  setDrawer(true);
  void runAction("restart_harness");
});

elements.updateButton.addEventListener("click", () => {
  setDrawer(true);
  void runAction("update_harness");
});

elements.languageButton.addEventListener("click", () => {
  const nextLanguage: Language = language === "zh" ? "en" : "zh";
  setLanguage(nextLanguage);
});

elements.clearLogButton.addEventListener("click", () => {
  logHistory.length = 0;
  logEmptyKey = "logCleared";
  renderLogHistory();
});

applyLanguage();
setDrawer(true, true);

void initializeStatusStream().catch((error: unknown) => {
  renderError(error);
});

async function initializeStatusStream(): Promise<void> {
  await Promise.all([
    listen<HarnessStatus>("harness-status", (event) => {
      renderStatus(event.payload);
    }),
    listen<LogEvent>("harness-log", (event) => {
      appendLog(event.payload);
    }),
  ]);
  renderStatus(await invoke<HarnessStatus>("get_status"));
}

async function runAction(command: string): Promise<void> {
  try {
    const status = await invoke<HarnessStatus>(command);
    renderStatus(status);
  } catch (error: unknown) {
    renderError(error);
  }
}

function renderStatus(status: HarnessStatus): void {
  if (status.revision < currentStatus.revision) return;
  const previousState = currentStatus.state;
  currentStatus = status;
  elements.statusMessage.textContent = translateBackendText(status.message);
  elements.statusDetail.textContent = translateBackendText(status.detail);
  elements.statusLabel.textContent = labelForState(status.state);
  elements.statusUrl.textContent = status.url ?? "";
  elements.statusUrl.title = status.url ?? "";

  elements.statusDot.className = `status-dot ${status.state}`;
  elements.progressBar.style.width = `${clamp(status.progress)}%`;
  elements.progressValue.textContent = `${clamp(status.progress)}%`;
  elements.progressLabel.textContent = translateBackendText(status.progressLabel);
  elements.progressTrack.setAttribute("aria-valuenow", String(clamp(status.progress)));
  elements.progressTrack.classList.toggle("indeterminate", status.state === "updating" && status.progress < 5);

  const running = status.state === "running" && status.url !== null;
  const busy = status.updateInProgress || status.state === "updating" || status.state === "stopping";
  elements.drawerButton.disabled = !running && status.state !== "updating" && status.state !== "error";
  elements.restartButton.disabled = !running || busy || status.state === "starting" || status.state === "initializing";
  elements.updateButton.disabled = busy;

  if (running) {
    const wasRecovering =
      previousState === "updating" ||
      previousState === "starting" ||
      previousState === "stopping";
    if (!harnessReady) {
      harnessReady = true;
      document.body.classList.add("harness-ready");
      setDrawer(false);
    } else if (wasRecovering) {
      setDrawer(false);
    }
  } else if (
    status.state === "updating" ||
    status.state === "starting" ||
    status.state === "error"
  ) {
    setDrawer(true);
  }
}

function appendLog(event: LogEvent): void {
  logHistory.push(event);
  while (logHistory.length > 500) logHistory.shift();
  logEmptyKey = "waiting";
  renderLogEntry(event);
  while (elements.logOutput.childElementCount > 500) {
    elements.logOutput.firstElementChild?.remove();
  }
  elements.logOutput.scrollTop = elements.logOutput.scrollHeight;
}

function renderLogEntry(event: LogEvent): void {
  elements.logOutput.querySelector(".log-empty")?.remove();
  const row = document.createElement("div");
  row.className = `log-row ${event.level}`;
  const time = document.createElement("span");
  time.className = "log-time";
  time.textContent = new Date().toLocaleTimeString([], { hour12: false });
  const line = document.createElement("span");
  line.className = "log-line";
  line.textContent = translateBackendText(event.line);
  row.append(time, line);
  elements.logOutput.append(row);
}

function renderLogHistory(): void {
  elements.logOutput.replaceChildren();
  if (logHistory.length === 0) {
    const empty = document.createElement("div");
    empty.className = "log-empty";
    empty.textContent = t(logEmptyKey);
    elements.logOutput.append(empty);
    return;
  }
  for (const event of logHistory) renderLogEntry(event);
  elements.logOutput.scrollTop = elements.logOutput.scrollHeight;
}

function renderError(error: unknown): void {
  const detail = error instanceof Error ? error.message : String(error);
  renderStatus({
    ...currentStatus,
    state: "error",
    message: "Harness is unavailable",
    detail,
    progress: 0,
    progressLabel: "Error",
    updateInProgress: false,
  });
  appendLog({ level: "error", line: detail });
}

function labelForState(state: HarnessStatus["state"]): string {
  switch (state) {
    case "initializing":
      return t("stateInitializing");
    case "starting":
      return t("stateStarting");
    case "running":
      return t("stateRunning");
    case "updating":
      return t("stateUpdating");
    case "stopping":
      return t("stateStopping");
    case "error":
      return t("stateError");
  }
}

function setLanguage(nextLanguage: Language): void {
  if (nextLanguage === language) return;
  language = nextLanguage;
  try {
    localStorage.setItem("deepseek-harness-language", language);
  } catch {
    // Storage may be unavailable in hardened webview contexts.
  }
  applyLanguage();
  renderStatus(currentStatus);
  renderLogHistory();
}

function setDrawer(open: boolean, force = false): void {
  if (!force && open === drawerOpen) return;
  drawerOpen = open;
  document.body.classList.toggle("drawer-open", open);
  const labelKey = open ? "hideDrawer" : "showDrawer";
  elements.drawerButton.dataset.i18nTitle = labelKey;
  elements.drawerButton.title = t(labelKey);
  const label = elements.drawerButton.querySelector("[data-i18n]");
  if (label instanceof HTMLElement) {
    label.dataset.i18n = labelKey;
    label.textContent = t(labelKey);
  }
  void invoke("set_drawer_open", { open }).catch((error: unknown) => {
    renderError(error);
  });
}

function applyLanguage(): void {
  document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
  for (const element of document.querySelectorAll<HTMLElement>("[data-i18n]")) {
    const key = element.dataset.i18n;
    if (key) element.textContent = t(key);
  }
  for (const element of document.querySelectorAll<HTMLElement>("[data-i18n-title]")) {
    const key = element.dataset.i18nTitle;
    if (key) element.title = t(key);
  }
  for (const element of document.querySelectorAll<HTMLElement>("[data-i18n-aria-label]")) {
    const key = element.dataset.i18nAriaLabel;
    if (key) element.setAttribute("aria-label", t(key));
  }
}

function translateBackendText(value: string): string {
  if (language === "en") return value;
  const exact = chinesePhrases[value];
  if (exact) return exact;
  for (const [englishPrefix, chinesePrefix] of chinesePrefixes) {
    if (value.startsWith(englishPrefix)) {
      return `${chinesePrefix}${value.slice(englishPrefix.length)}`;
    }
  }
  return value
    .replace(
      /^Started Harness process (\d+) on port (\d+)$/u,
      "已启动 Harness 进程 $1，端口 $2",
    )
    .replace(
      /^Stopping Harness process (\d+)$/u,
      "正在停止 Harness 进程 $1",
    );
}

function t(key: string): string {
  return translations[language][key] ?? translations.en[key] ?? key;
}

function loadLanguage(): Language {
  try {
    const stored = localStorage.getItem("deepseek-harness-language");
    if (stored === "en" || stored === "zh") return stored;
  } catch {
    // Fall back to the system language.
  }
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

function clamp(value: number): number {
  return Math.max(0, Math.min(100, Math.round(value)));
}

function requiredElement(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`Missing element #${id}`);
  }
  return element;
}

function requiredButton(id: string): HTMLButtonElement {
  const element = requiredElement(id);
  if (!(element instanceof HTMLButtonElement)) {
    throw new Error(`#${id} is not a button`);
  }
  return element;
}
