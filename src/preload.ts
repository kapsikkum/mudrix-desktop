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
  minimizeWindow: () => ipcRenderer.send('window-minimize'),
  maximizeWindow: () => ipcRenderer.send('window-maximize'),
  closeWindow: () => ipcRenderer.send('window-close'),
  getPlatform: () => process.platform,
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

const MACOS_BAR_HEIGHT = 28;
const WIN_BAR_HEIGHT = 32;

const THEME_MAP: Record<string, { bar: string; text: string }> = {
  '#11111e': { bar: '#1c1c30', text: '#e8e8f0' }, // Carbon
  '#0d1117': { bar: '#161b22', text: '#e6edf3' }, // Parrot
  '#ffffff': { bar: '#f5f3ff', text: '#1a1a2e' }, // Ethereal
};

const FALLBACK = { bar: '#1c1c30', text: '#e8e8f0' };

function rgbToHex(rgb: string): string {
  const m = rgb.match(/rgb\(\s*(\d+),\s*(\d+),\s*(\d+)\)/);
  if (!m) return rgb.toLowerCase();
  return '#' + [m[1], m[2], m[3]].map((v) => parseInt(v, 10).toString(16).padStart(2, '0')).join('');
}

function currentColors() {
  const hex = rgbToHex(getComputedStyle(document.body).backgroundColor);
  return THEME_MAP[hex] ?? FALLBACK;
}

function syncToMain() {
  const { bar, text } = currentColors();
  ipcRenderer.send('set-theme-color', bar, text);
}

window.addEventListener('DOMContentLoaded', () => {
  if (process.platform === 'darwin') {
    const style = document.createElement('style');
    style.textContent = `
      html, body { overflow: hidden; }
      #root { margin-top: ${MACOS_BAR_HEIGHT}px; height: calc(100vh - ${MACOS_BAR_HEIGHT}px); }
    `;
    document.head.appendChild(style);

    const strip = document.createElement('div');
    strip.style.cssText = `position:fixed;top:0;left:0;right:0;height:${MACOS_BAR_HEIGHT}px;z-index:2147483647;background:${FALLBACK.bar};-webkit-app-region:drag;user-select:none`;
    document.body.appendChild(strip);

    const apply = () => requestAnimationFrame(() => {
      strip.style.background = currentColors().bar;
      syncToMain();
    });
    apply();
    new MutationObserver(apply).observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
    return;
  }

  const style = document.createElement('style');
  style.textContent = `
    html, body { overflow: hidden; }
    #root { margin-top: ${WIN_BAR_HEIGHT}px; height: calc(100vh - ${WIN_BAR_HEIGHT}px); }
  `;
  document.head.appendChild(style);

  const strip = document.createElement('div');
  strip.style.cssText = `position:fixed;top:0;left:0;right:0;height:${WIN_BAR_HEIGHT}px;z-index:2147483647;background:${FALLBACK.bar};-webkit-app-region:drag;user-select:none;display:flex;align-items:center;gap:7px;padding-left:10px;box-sizing:border-box`;

  const img = document.createElement('img');
  img.src = 'wizardchat://app/assets/images/logo.svg';
  img.style.cssText = `width:16px;height:16px;border-radius:3px;flex-shrink:0;pointer-events:none`;
  img.onerror = () => { img.style.display = 'none'; };

  const label = document.createElement('span');
  label.textContent = 'WizardChat';
  label.style.cssText = `font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;font-size:12px;font-weight:500;color:${FALLBACK.text};opacity:0.85;pointer-events:none`;

  strip.appendChild(img);
  strip.appendChild(label);
  document.body.appendChild(strip);

  const apply = () => requestAnimationFrame(() => {
    const { bar, text } = currentColors();
    strip.style.background = bar;
    label.style.color = text;
    syncToMain();
  });
  apply();
  new MutationObserver(apply).observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
});
