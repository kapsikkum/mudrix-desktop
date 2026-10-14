import {
  app,
  BrowserWindow,
  session,
  ipcMain,
  systemPreferences,
  desktopCapturer,
  protocol,
  net,
  Notification,
  shell,
} from 'electron';
import { autoUpdater } from 'electron-updater';
import * as path from 'path';
import * as http from 'http';
import { existsSync, statSync } from 'fs';
import { pathToFileURL } from 'url';
import { SsoLoopback } from './ssoLoopback';

let mainWindow: BrowserWindow | null = null;
let pendingRoomUrl: string | null = null;
let latestVersion: string | null = null;

const PROTOCOL = 'org.wizardchat.desktop';
const OIDC_LOOPBACK_PORT = 34759;
const DEFAULT_BG = '#1c1c30';
const DEFAULT_FG = '#e8e8f0';
const TITLEBAR_HEIGHT = 32;
const EXTERNAL_SCHEMES = new Set(['http:', 'https:', 'mailto:']);

function isExternalUrl(url: string): boolean {
  try {
    return EXTERNAL_SCHEMES.has(new URL(url).protocol);
  } catch {
    return false;
  }
}

function openExternal(url: string): void {
  try {
    void shell.openExternal(url);
  } catch (err) {
    console.error('[main] openExternal failed', url, err);
  }
}

function getWebDistPath(): string {
  if (app.isPackaged) return path.join(process.resourcesPath, 'web-app');
  return path.join(__dirname, '..', 'wizardchat-web', 'dist');
}

function getIconPath(): string | undefined {
  switch (process.platform) {
    case 'darwin': return path.join(__dirname, '..', 'icons', 'icon.icns');
    case 'win32':  return path.join(__dirname, '..', 'icons', 'icon.ico');
    default:
      return app.isPackaged
        ? path.join(process.resourcesPath, 'icons', 'source', 'icon.png')
        : path.join(__dirname, '..', 'icons', 'source', 'icon.png');
  }
}

function createWindow(): void {
  mainWindow = new BrowserWindow({
    width: 1200,
    height: 800,
    minWidth: 800,
    minHeight: 600,
    title: 'WizardChat',
    icon: getIconPath(),
    backgroundColor: DEFAULT_BG,
    titleBarStyle: 'hidden',
    trafficLightPosition: { x: 12, y: 8 },
    ...(process.platform !== 'darwin' ? {
      titleBarOverlay: { color: DEFAULT_BG, symbolColor: DEFAULT_FG, height: TITLEBAR_HEIGHT },
    } : {}),
    webPreferences: {
      nodeIntegration: false,
      contextIsolation: true,
      preload: path.join(__dirname, 'preload.js'),
      webSecurity: true,
    },
  });

  // non-org.wizardchat.desktop:// requests get handed to the system browser
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    if (isExternalUrl(url)) {
      openExternal(url);
    } else {
      console.warn('[main] blocked popup for non-external url', url);
    }
    return { action: 'deny' };
  });

  mainWindow.webContents.on('will-navigate', (event, url) => {
    if (!isExternalUrl(url)) return;
    if (isSsoRedirect(url)) {
      event.preventDefault();
      void openSsoInBrowser(url);
      return;
    }
    event.preventDefault();
    openExternal(url);
  });

  mainWindow.loadURL(`${PROTOCOL}://app/`);

  mainWindow.webContents.on('did-finish-load', () => {
    if (pendingRoomUrl) {
      mainWindow?.webContents.send('open-room', pendingRoomUrl);
      pendingRoomUrl = null;
    }
    if (pendingDeepLink) {
      const deepLink = pendingDeepLink;
      pendingDeepLink = null;
      mainWindow?.webContents.loadURL(deepLink);
    }
  });

  if (!app.isPackaged) {
    mainWindow.webContents.openDevTools();
  }

  mainWindow.on('closed', () => { mainWindow = null; });
}

protocol.registerSchemesAsPrivileged([{
  scheme: PROTOCOL,
  privileges: { standard: true, secure: true, supportFetchAPI: true, corsEnabled: true },
}]);

app.setName('WizardChat');
app.setAppUserModelId('org.wizardchat.desktop');

let pendingDeepLink: string | null = null;
let protocolReady = false;

function isSsoRedirect(url: string): boolean {
  try {
    return new URL(url).pathname.includes('/_matrix/client/v3/login/sso/redirect');
  } catch {
    return false;
  }
}

function isDeepLink(url: string): boolean {
  return url.startsWith(`${PROTOCOL}:`);
}

function focusMainWindow(): void {
  if (!mainWindow || mainWindow.isDestroyed()) return;
  if (mainWindow.isMinimized()) mainWindow.restore();
  mainWindow.show();
  mainWindow.focus();
}

function normalizeDeepLink(url: string): string {
  try {
    const parsed = new URL(url);

    if (parsed.searchParams.has('code') || parsed.searchParams.has('error')) {
      return `${PROTOCOL}://app/oidc/callback${parsed.search}`;
    }
    if (parsed.searchParams.has('loginToken')) {
      return `${PROTOCOL}://app/sso/callback${parsed.search}`;
    }

    if (parsed.protocol === `${PROTOCOL}:` && (parsed.host !== 'app' || !parsed.host)) {
      return `${PROTOCOL}://app${parsed.pathname}${parsed.search}`;
    }
  } catch {
    // Fall back to the original URL if parsing fails.
  }
  return url;
}

function handleDeepLink(url: string): void {
  if (!isDeepLink(url)) return;
  const targetUrl = normalizeDeepLink(url);
  if (protocolReady && mainWindow && !mainWindow.isDestroyed()) {
    focusMainWindow();
    mainWindow.webContents.loadURL(targetUrl);
  } else {
    pendingDeepLink = targetUrl;
  }
}

if (app.isPackaged) {
  app.setAsDefaultProtocolClient(PROTOCOL);
} else {
  app.setAsDefaultProtocolClient(PROTOCOL, process.execPath, [path.resolve(process.argv[1] ?? '.')]);
}

function loadSsoCallbackRoute(params: URLSearchParams): void {
  const url = new URL(`${PROTOCOL}://app/sso/callback`);
  url.search = params.toString();

  focusMainWindow();
  if (mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.loadURL(url.toString());
  }
}

function openSsoCallbackInApp({ loginToken, state }: { loginToken: string; state: string | null }): void {
  const params = new URLSearchParams({ loginToken });
  if (state) params.set('state', state);
  loadSsoCallbackRoute(params);
}

function onSsoCallbackFailure(): void {
  loadSsoCallbackRoute(new URLSearchParams());
}

function handleLoopbackCallback(params: URLSearchParams): void {
  const loginToken = params.get('loginToken');
  if (loginToken) {
    openSsoCallbackInApp({ loginToken, state: params.get('state') });
    return;
  }
  if (params.has('code') || params.has('error')) {
    const qs = params.toString();
    focusMainWindow();
    if (mainWindow && !mainWindow.isDestroyed()) {
      mainWindow.webContents.loadURL(`${PROTOCOL}://app/oidc/callback${qs ? `?${qs}` : ''}`);
    }
    return;
  }
  onSsoCallbackFailure();
}

const loopback = new SsoLoopback(OIDC_LOOPBACK_PORT, handleLoopbackCallback, onSsoCallbackFailure);

async function openSsoInBrowser(ssoUrl: string): Promise<void> {
  try {
    const rewritten = await loopback.buildBrowserUrl(ssoUrl);
    if (!rewritten) {
      openExternal(ssoUrl);
      return;
    }
    openExternal(rewritten);
  } catch (err) {
    console.error('[main] failed to start SSO loopback', err);
    openExternal(ssoUrl);
  }
}

app.on('open-url', (event, url) => {
  event.preventDefault();
  handleDeepLink(url);
});

async function onReady(): Promise<void> {
  protocol.handle(PROTOCOL, (request) => {
    const webRoot = getWebDistPath();
    const url = new URL(request.url);
    let relPath = url.pathname;

    if (relPath === '/' || relPath === '') relPath = '/index.html';

    if (relPath === '/icon.png') {
      const iconPath = app.isPackaged
        ? path.join(process.resourcesPath, 'icons', 'source', 'icon.png')
        : path.join(__dirname, '..', 'icons', 'source', 'icon.png');
      return net.fetch(pathToFileURL(iconPath).toString());
    }

    const absPath = path.normalize(path.join(webRoot, relPath));
    const rel = path.relative(webRoot, absPath);
    if (path.isAbsolute(rel) || rel.split(path.sep).includes('..')) {
      return new Response('Forbidden', { status: 403 });
    }

    if (!existsSync(absPath) || !statSync(absPath).isFile()) {
      const acceptsHtml = (request.headers.get('accept') ?? '').includes('text/html');
      if (!acceptsHtml) return new Response('Not Found', { status: 404 });
      return net.fetch(pathToFileURL(path.join(webRoot, 'index.html')).toString());
    }

    return net.fetch(pathToFileURL(absPath).toString());
  });

  protocolReady = true;
  if (!pendingDeepLink) {
    const startupDeepLink = process.argv.find((arg) => isDeepLink(arg));
    if (startupDeepLink) pendingDeepLink = normalizeDeepLink(startupDeepLink);
  }
  if (pendingDeepLink) {
    const deepLink = pendingDeepLink;
    pendingDeepLink = null;
    handleDeepLink(deepLink);
  }

  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    const headers = { ...details.responseHeaders };

    const strip = (name: string) => {
      const lower = name.toLowerCase();
      for (const key of Object.keys(headers)) {
        if (key.toLowerCase() === lower) delete headers[key];
      }
    };

    strip('access-control-allow-origin');
    strip('access-control-allow-headers');
    strip('access-control-allow-methods');
    strip('content-security-policy');
    strip('x-frame-options');

    headers['Access-Control-Allow-Origin'] = ['*'];
    headers['Access-Control-Allow-Headers'] = ['*'];
    headers['Access-Control-Allow-Methods'] = ['GET, POST, PUT, DELETE, OPTIONS'];

    callback({ responseHeaders: headers });
  });

  session.defaultSession.setPermissionRequestHandler(async (_wc, permission, callback, details) => {
    const types = (details as { mediaTypes?: ('audio' | 'video')[] }).mediaTypes;

    if (permission === 'media') {
      if (process.platform === 'darwin') {
        let granted = true;
        if (types?.includes('audio')) granted = granted && await systemPreferences.askForMediaAccess('microphone');
        if (types?.includes('video')) granted = granted && await systemPreferences.askForMediaAccess('camera');
        callback(granted);
      } else {
        callback(true);
      }
      return;
    }

    if (permission === 'display-capture') {
      callback(true);
      return;
    }

    callback(permission === 'mediaKeySystem');
  });

  ipcMain.handle('get-app-version', () => app.getVersion());

  ipcMain.handle('get-desktop-sources', async (_e, options) => {
    const sources = await desktopCapturer.getSources(options);
    return sources.map(s => ({
      id: s.id,
      name: s.name,
      display_id: s.display_id,
      thumbnail: {
        dataUrl: s.thumbnail.toDataURL(),
      },
      appIcon: s.appIcon?.toDataURL() ?? null,
    }));
  });

  let pendingPickerCallback: ((source: any) => void) | null = null;

  ipcMain.on('desktop-source-selected', (_e, source) => {
    pendingPickerCallback?.(source);
    pendingPickerCallback = null;
  });

  session.defaultSession.setDisplayMediaRequestHandler((_request, callback) => {
    pendingPickerCallback = (source: any) => {
      if (source) {
        callback({ video: source } as any);
      } else {
        callback(null as any);
      }
    };
    mainWindow?.webContents.send('open-desktop-picker');
  });

  void loopback.ensureListening().catch((err) => {
    console.error('[main] loopback server error', err);
  });

  createWindow();

  if (app.isPackaged) {
    autoUpdater.setFeedURL({ provider: 'gitlab', projectId: '82340703' });

    if (process.platform === 'darwin') {
      autoUpdater.autoDownload = false;
    }

    autoUpdater.checkForUpdates();
    setInterval(() => autoUpdater.checkForUpdates(), 10 * 60 * 1000);

    autoUpdater.on('update-available', (info: { version: string }) => {
      latestVersion = info.version;
      mainWindow?.webContents.send('update-available', info.version);
      if (process.platform === 'darwin') {
        mainWindow?.webContents.send('update-downloaded');
      }
    });

    autoUpdater.on('download-progress', (p: { percent: number }) => {
      mainWindow?.webContents.send('update-progress', p.percent);
    });

    autoUpdater.on('update-downloaded', () => {
      mainWindow?.webContents.send('update-downloaded');
    });
  }

  ipcMain.on('install-update', () => {
    if (process.platform === 'darwin') {
      const version = latestVersion ?? app.getVersion();
      const gitlabUrl = `https://gitlab.com/api/v4/projects/82340703/packages/generic/wizardchat-desktop/v${version}/WizardChat-${version}-arm64.dmg`;      openExternal(gitlabUrl);
    } else {
      autoUpdater.quitAndInstall();
    }
  });

  ipcMain.on('window-minimize', () => mainWindow?.minimize());
  ipcMain.on('window-maximize', () => {
    if (!mainWindow) return;
    mainWindow.isMaximized() ? mainWindow.unmaximize() : mainWindow.maximize();
  });
  ipcMain.on('window-close', () => mainWindow?.close());

  ipcMain.on('set-theme-color', (_e, bgColor: string, symbolColor: string) => {
    if (!mainWindow) return;
    mainWindow.setBackgroundColor(bgColor);
    if (process.platform !== 'darwin') {
      mainWindow.setTitleBarOverlay({ color: bgColor, symbolColor, height: TITLEBAR_HEIGHT });
    }
  });

  ipcMain.handle('show-notification', (_e, payload: { title: string; body: string; icon?: string; silent?: boolean; roomUrl?: string }) => {
    try {
      const { roomUrl } = payload;
      const n = new Notification({ title: payload.title, body: payload.body, silent: payload.silent ?? true });
      n.on('click', () => {
        if (mainWindow && !mainWindow.isDestroyed()) {
          if (mainWindow.isMinimized()) mainWindow.restore();
          mainWindow.show();
          mainWindow.focus();
          if (roomUrl) mainWindow.webContents.send('open-room', roomUrl);
        } else if (roomUrl) {
          pendingRoomUrl = roomUrl;
          createWindow();
        }
      });
      n.show();
      return true;
    } catch (err) {
      console.error('[main] show-notification failed', err);
      return false;
    }
  });

  ipcMain.handle('get-pending-room', () => {
    const url = pendingRoomUrl;
    pendingRoomUrl = null;
    return url;
  });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
}

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', (_event, argv) => {
    const deepLink = argv.find((arg) => isDeepLink(arg));
    if (deepLink) handleDeepLink(deepLink);
    focusMainWindow();
  });

  app.whenReady().then(onReady);
}

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});

app.on('will-quit', () => {
  loopback.close();
});
