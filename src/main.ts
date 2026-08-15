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
} from 'electron';
import { autoUpdater } from 'electron-updater';
import * as path from 'path';
import { pathToFileURL } from 'url';

let mainWindow: BrowserWindow | null = null;

const PROTOCOL = 'wizardchat';
const DEFAULT_BG = '#1c1c30';
const DEFAULT_FG = '#e8e8f0';
const TITLEBAR_HEIGHT = 32;

function getWebDistPath(): string {
  if (app.isPackaged) return path.join(process.resourcesPath, 'web-app');
  return path.join(__dirname, '..', 'wizardchat-web', 'dist');
}

function getIconPath(): string | undefined {
  switch (process.platform) {
    case 'darwin': return path.join(__dirname, '..', 'icons', 'icon.icns');
    case 'win32':  return path.join(__dirname, '..', 'icons', 'icon.ico');
    default:       return path.join(__dirname, '..', 'icons', 'source', 'icon.png');
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

  mainWindow.loadURL(`${PROTOCOL}://app/`);

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

app.whenReady().then(async () => {
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
    if (!absPath.startsWith(path.normalize(webRoot))) return new Response('Forbidden', { status: 403 });
    return net.fetch(pathToFileURL(absPath).toString());
  });

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

  createWindow();

  if (app.isPackaged) {
    autoUpdater.setFeedURL({ provider: 'gitlab', projectId: '82340703' });
    autoUpdater.checkForUpdates();
    setInterval(() => autoUpdater.checkForUpdates(), 10 * 60 * 1000);
    autoUpdater.on('update-available', (info: { version: string }) => {
      mainWindow?.webContents.send('update-available', info.version);
    });
    autoUpdater.on('download-progress', (p: { percent: number }) => {
      mainWindow?.webContents.send('update-progress', p.percent);
    });
    autoUpdater.on('update-downloaded', () => {
      mainWindow?.webContents.send('update-downloaded');
    });
  }

  ipcMain.on('install-update', () => autoUpdater.quitAndInstall());

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

  ipcMain.handle('show-notification', (_e, payload: { title: string; body: string; icon?: string; silent?: boolean }) => {
    try {
      const n = new Notification({ title: payload.title, body: payload.body, silent: payload.silent ?? true });
      n.on('click', () => {
        if (mainWindow) {
          if (mainWindow.isMinimized()) mainWindow.restore();
          mainWindow.show();
          mainWindow.focus();
        }
      });
      n.show();
      return true;
    } catch (err) {
      console.error('[main] show-notification failed', err);
      return false;
    }
  });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
