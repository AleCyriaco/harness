/**
 * Minimal harness daemon client (TCP NDJSON).
 * Usage: const c = await HarnessClient.connect("127.0.0.1:19876");
 */
import net from "node:net";

export type ServerMsg = { type: string; [k: string]: unknown };

export class HarnessClient {
  private socket: net.Socket;
  private buf = "";
  private queue: ServerMsg[] = [];
  private waiters: Array<(m: ServerMsg) => void> = [];

  private constructor(socket: net.Socket) {
    this.socket = socket;
    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => {
      this.buf += chunk;
      let idx;
      while ((idx = this.buf.indexOf("\n")) >= 0) {
        const line = this.buf.slice(0, idx);
        this.buf = this.buf.slice(idx + 1);
        if (!line.trim()) continue;
        try {
          const msg = JSON.parse(line) as ServerMsg;
          const w = this.waiters.shift();
          if (w) w(msg);
          else this.queue.push(msg);
        } catch {
          /* ignore */
        }
      }
    });
  }

  static connect(hostPort = "127.0.0.1:19876"): Promise<HarnessClient> {
    const [host, portS] = hostPort.split(":");
    const port = Number(portS || 19876);
    return new Promise((resolve, reject) => {
      const s = net.createConnection({ host, port }, () => {
        resolve(new HarnessClient(s));
      });
      s.on("error", reject);
    });
  }

  private send(obj: unknown) {
    this.socket.write(JSON.stringify(obj) + "\n");
  }

  private next(): Promise<ServerMsg> {
    if (this.queue.length) return Promise.resolve(this.queue.shift()!);
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  async createSession(mode = "code") {
    this.send({ type: "create_session", mode });
    for (;;) {
      const m = await this.next();
      if (m.type === "session_created") return m;
      if (m.type === "error") throw new Error(String(m.message));
    }
  }

  async run(sessionId: string, text: string, onEvent?: (e: ServerMsg) => void) {
    this.send({ type: "user_message", session_id: sessionId, text });
    for (;;) {
      const m = await this.next();
      onEvent?.(m);
      if (m.type === "event" && (m as any).event === "done") return m;
      if (m.type === "error") throw new Error(String(m.message));
    }
  }

  close() {
    this.socket.end();
  }
}
