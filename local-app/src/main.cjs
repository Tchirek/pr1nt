const { app, BrowserWindow, Menu, ipcMain, shell } = require('electron');
const { spawn } = require('node:child_process');
const http = require('node:http');
const path = require('node:path');
const fs = require('node:fs');

const PUBLIC_URL = 'http://127.0.0.1:8788';
const ADMIN_URL = 'http://127.0.0.1:8789/admin';
const PRINT_SITE_URL = 'https://print.example.com';

let mainWindow = null;
let serverProcess = null;
let lastState = {
  phase: 'starting',
  message: '正在启动本地打印服务',
  publicUrl: PUBLIC_URL,
  adminUrl: ADMIN_URL,
  printSiteUrl: PRINT_SITE_URL
};

function projectRoot() {
  if (app.isPackaged) return process.resourcesPath;
  return path.resolve(__dirname, '..', '..');
}

function serverDir() {
  return path.join(projectRoot(), 'local-server');
}

function serverExe() {
  return path.join(serverDir(), 'local-server.exe');
}

function iconPath() {
  return path.join(__dirname, 'assets', 'icon.ico');
}

function ensureRuntimeDirs() {
  const spool = path.join(serverDir(), 'print-spool');
  fs.mkdirSync(spool, { recursive: true });
}

function publishState(next) {
  lastState = { ...lastState, ...next };
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.send('service:state', lastState);
  }
}

function createWindow() {
  Menu.setApplicationMenu(null);
  mainWindow = new BrowserWindow({
    width: 1180,
    height: 780,
    minWidth: 920,
    minHeight: 620,
    title: 'Room 101 Print Service',
    icon: iconPath(),
    backgroundColor: '#f8f4ec',
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false
    }
  });

  mainWindow.once('ready-to-show', () => {
    mainWindow.show();
    publishState(lastState);
  });

  mainWindow.loadFile(path.join(__dirname, 'renderer.html'));
}

function startServer() {
  if (serverProcess && !serverProcess.killed) return;

  const exe = serverExe();
  if (!fs.existsSync(exe)) {
    publishState({
      phase: 'failed',
      message: `未找到 local-server.exe：${exe}`
    });
    return;
  }

  ensureRuntimeDirs();
  publishState({ phase: 'starting', message: '正在启动本地打印服务' });

  serverProcess = spawn(exe, {
    cwd: serverDir(),
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env,
      RUST_LOG: process.env.RUST_LOG || 'info'
    }
  });

  serverProcess.stdout.on('data', (chunk) => {
    const text = chunk.toString('utf8');
    if (text.includes('public print server listening')) {
      publishState({ phase: 'checking', message: '本地服务已启动，正在检查后台' });
    }
  });

  serverProcess.stderr.on('data', (chunk) => {
    publishState({ phase: 'warning', message: chunk.toString('utf8').trim().slice(0, 240) });
  });

  serverProcess.on('exit', (code) => {
    serverProcess = null;
    publishState({
      phase: 'failed',
      message: `本地打印服务已退出${typeof code === 'number' ? `（代码 ${code}）` : ''}`
    });
  });

  waitForAdminReady();
}

function stopServer() {
  if (!serverProcess) return;
  const child = serverProcess;
  serverProcess = null;
  child.kill();
}

function restartServer() {
  publishState({ phase: 'starting', message: '正在重启本地打印服务' });
  stopServer();
  setTimeout(startServer, 600);
}

function request(url, timeoutMs = 1600) {
  return new Promise((resolve) => {
    const req = http.get(url, (res) => {
      res.resume();
      resolve(res.statusCode && res.statusCode >= 200 && res.statusCode < 500);
    });
    req.on('error', () => resolve(false));
    req.setTimeout(timeoutMs, () => {
      req.destroy();
      resolve(false);
    });
  });
}

async function waitForAdminReady() {
  const started = Date.now();
  while (Date.now() - started < 20000) {
    const ok = await request(ADMIN_URL);
    if (ok) {
      publishState({
        phase: 'ready',
        message: '本地打印服务运行中',
        adminUrl: `${ADMIN_URL}?embedded=1`
      });
      return;
    }
    publishState({ phase: 'checking', message: '正在等待本地后台就绪' });
    await new Promise((resolve) => setTimeout(resolve, 700));
  }
  publishState({ phase: 'failed', message: '本地后台启动超时，请检查端口 8788 / 8789 是否被占用' });
}

app.whenReady().then(() => {
  app.setAppUserModelId('top.tchirek.pr1nt.local');
  createWindow();
  startServer();
});

app.on('before-quit', () => {
  stopServer();
});

app.on('window-all-closed', () => {
  app.quit();
});

ipcMain.handle('service:get-state', () => lastState);
ipcMain.handle('service:restart', () => {
  restartServer();
  return lastState;
});
ipcMain.handle('service:open-print-site', () => shell.openExternal(PRINT_SITE_URL));
ipcMain.handle('service:open-admin', () => shell.openExternal(ADMIN_URL));
