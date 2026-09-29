import * as http from 'http';
import { randomUUID } from 'crypto';

const HOST = '127.0.0.1';
const NONCE_PARAM = '_sso';
const IDLE_TIMEOUT_MS = 10 * 60 * 1000;

const DONE_PAGE = `<!doctype html>
<html><head><meta charset="utf-8"><title>WizardChat</title>
<style>
  body { margin:0; height:100vh; display:flex; align-items:center; justify-content:center;
         background:#1c1c30; color:#e8e8f0;
         font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
         font-size:20px; font-weight:600; }
</style></head>
<body>WizardChat</body></html>`;

export interface SsoCallback {
  loginToken: string;
  state: string | null;
}

export class SsoLoopback {
  private server: http.Server | null = null;
  private port = 0;
  private nonce = randomUUID();
  private idleTimer: NodeJS.Timeout | null = null;
  private consumed = false;

  constructor(
    private readonly onCallback: (callback: SsoCallback) => void,
    private readonly onFailure: () => void,
  ) {}

  async buildBrowserUrl(ssoUrl: string): Promise<string | null> {
    let parsed: URL;
    let redirectParam: string | null;
    try {
      parsed = new URL(ssoUrl);
      redirectParam = parsed.searchParams.get('redirectUrl');
    } catch {
      return null;
    }
    if (!redirectParam) return null;

    let carried: URLSearchParams;
    try {
      carried = new URL(redirectParam).searchParams;
    } catch {
      return null;
    }

    const port = await this.ensureListening();
    const loopback = new URLSearchParams(carried);
    loopback.set(NONCE_PARAM, this.nonce);

    const out = new URL(ssoUrl);
    out.searchParams.set('redirectUrl', `http://${HOST}:${port}/?${loopback.toString()}`);
    return out.toString();
  }

  private ensureListening(): Promise<number> {
    if (this.server && this.server.listening) {
      this.armIdleTimeout();
      return Promise.resolve(this.port);
    }

    return new Promise((resolve, reject) => {
      const server = http.createServer((req, res) => this.handle(req, res));
      server.once('error', reject);
      server.listen(0, HOST, () => {
        const address = server.address();
        this.port = typeof address === 'object' && address ? address.port : 0;
        this.server = server;
        this.consumed = false;
        this.armIdleTimeout();
        resolve(this.port);
      });
    });
  }

  private armIdleTimeout(): void {
    if (this.idleTimer) clearTimeout(this.idleTimer);
    this.idleTimer = setTimeout(() => this.close(), IDLE_TIMEOUT_MS);
    this.idleTimer.unref?.();
  }

  private handle(req: http.IncomingMessage, res: http.ServerResponse): void {
    const expectedHost = `${HOST}:${this.port}`;
    const hostOk = (req.headers.host ?? '').toLowerCase() === expectedHost;

    let params: URLSearchParams;
    try {
      params = new URL(req.url ?? '/', `http://${expectedHost}`).searchParams;
    } catch {
      params = new URLSearchParams();
    }

    const nonceOk = params.get(NONCE_PARAM) === this.nonce;
    const accepted = hostOk && nonceOk && !this.consumed && req.method === 'GET';
    if (!accepted) {
      res.writeHead(400, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(DONE_PAGE);
      return;
    }

    this.consumed = true;
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(DONE_PAGE);

    const loginToken = params.get('loginToken');
    if (!loginToken) {
      setImmediate(() => {
        this.close();
        this.onFailure();
      });
      return;
    }

    const callback: SsoCallback = {
      loginToken,
      state: params.get('state'),
    };

    setImmediate(() => {
      this.close();
      this.onCallback(callback);
    });
  }

  close(): void {
    if (this.idleTimer) {
      clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
    this.nonce = randomUUID();
    this.consumed = false;
    if (this.server) {
      this.server.close();
      this.server = null;
    }
    this.port = 0;
  }
}
