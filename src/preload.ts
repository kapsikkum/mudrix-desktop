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
  showNotification: (payload: { title: string; body: string; icon?: string; silent?: boolean }) =>
    ipcRenderer.invoke('show-notification', payload),
});
