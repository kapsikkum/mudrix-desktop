import { contextBridge, ipcRenderer } from 'electron';

contextBridge.exposeInMainWorld('electronAPI', {
  onUpdateAvailable: (cb: (version: string) => void) => {
    ipcRenderer.on('update-available', (_e, version) => cb(version));
  },
  onUpdateDownloaded: (cb: () => void) => {
    ipcRenderer.on('update-downloaded', () => cb());
  },
  onUpdateProgress: (cb: (pct: number) => void) => {
    ipcRenderer.on('update-progress', (_e, pct) => cb(pct));
  },
  installUpdate: () => ipcRenderer.send('install-update'),
  showNotification: (payload: { title: string; body: string; icon?: string; silent?: boolean; roomUrl?: string }) =>
    ipcRenderer.invoke('show-notification', payload),
  onOpenRoom: (cb: (url: string) => void) => {
    ipcRenderer.on('open-room', (_e, url) => cb(url));
  },
  getPendingRoom: () => ipcRenderer.invoke('get-pending-room'),
  getPlatform: () => process.platform,
  // The web app draws the title bar; these keep the native window chrome in sync with it
  setTitleBarTheme: (background: string, foreground: string) =>
    ipcRenderer.send('set-theme-color', background, foreground),
  onFullscreenChange: (cb: (fullscreen: boolean) => void) => {
    ipcRenderer.on('fullscreen-changed', (_e, fullscreen: boolean) => cb(fullscreen));
  },
  getAppVersion: () => ipcRenderer.invoke('get-app-version'),
  getDesktopSources: (options: { types: string[]; thumbnailSize?: { width: number; height: number } }) =>
    ipcRenderer.invoke('get-desktop-sources', options),
  onOpenDesktopPicker: (cb: () => void) => {
    ipcRenderer.on('open-desktop-picker', () => cb());
  },
  selectDesktopSource: (source: any) => {
    ipcRenderer.send('desktop-source-selected', source);
  },
});
