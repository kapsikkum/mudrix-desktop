import { app, BrowserWindow, session, ipcMain, systemPreferences, protocol, net } from 'electron';
import { autoUpdater } from 'electron-updater';
import * as path from 'path';
import { pathToFileURL } from 'url';

let mainWindow: BrowserWindow | null = null;

const PROTOCOL = 'wizardchat';

function getWebDistPath(): string {
  if (app.isPackaged) {
    return path.join(process.resourcesPath, 'web-app');
  }
  return path.join(__dirname, '..', 'wizardchat-web', 'dist');
}

function getIconPath(): string | undefined {
  switch (process.platform) {
    case 'darwin':
      return path.join(__dirname, '..', 'icons', 'icon.icns');
    case 'win32':
      return path.join(__dirname, '..', 'icons', 'icon.ico');
    default:
      return path.join(__dirname, '..', 'icons', 'source', 'icon.png');
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

  mainWindow.on('closed', () => {
    mainWindow = null;
  });
}

protocol.registerSchemesAsPrivileged([
  {
    scheme: PROTOCOL,
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      corsEnabled: true,
    },
  },
]);

app.setName('WizardChat');

app.whenReady().then(async () => {
  if (process.platform === 'darwin') {
    await systemPreferences.askForMediaAccess('camera');
    await systemPreferences.askForMediaAccess('microphone');
  }

  protocol.handle(PROTOCOL, (request) => {
    const webRoot = getWebDistPath();
    const url = new URL(request.url);
    let relPath = url.pathname;

    if (relPath === '/' || relPath === '') {
      relPath = '/index.html';
    }

    const absPath = path.normalize(path.join(webRoot, relPath));
    if (!absPath.startsWith(path.normalize(webRoot))) {
      return new Response('Forbidden', { status: 403 });
    }

    return net.fetch(pathToFileURL(absPath).toString());
  });

  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    const headers = { ...details.responseHeaders };
    headers['Access-Control-Allow-Origin'] = ['*'];
    headers['Access-Control-Allow-Headers'] = ['*'];
    headers['Access-Control-Allow-Methods'] = ['GET, POST, PUT, DELETE, OPTIONS'];
    delete headers['content-security-policy'];
    delete headers['Content-Security-Policy'];
    delete headers['x-frame-options'];
    delete headers['X-Frame-Options'];
    callback({ responseHeaders: headers });
  });

  session.defaultSession.setPermissionRequestHandler((_wc, permission, callback) => {
    callback(['media', 'mediaKeySystem', 'getUserMedia'].includes(permission));
  });

  createWindow();

  if (app.isPackaged) {
    autoUpdater.checkForUpdates();
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

  ipcMain.on('install-update', () => {
    autoUpdater.quitAndInstall();
  });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});
