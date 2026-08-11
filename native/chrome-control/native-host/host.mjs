#!/usr/bin/env node
/**
 * LCU Chrome Control — Native Messaging Host (D3 product)
 *
 * Chrome launches this process when the extension calls connectNative().
 * Protocol on stdin/stdout: 4-byte little-endian length + UTF-8 JSON.
 *
 * Control plane (product invariant):
 *   - Unix domain socket ONLY under the Runtime private entry directory
 *   - Path: <runtime_root>/chrome-control.sock
 *     default runtime_root = ~/Library/Application Support/AnythingUse
 *   - NEVER binds TCP for control
 *
 * Bridge:
 *   Runtime / Rust adapter (JSON-lines) → private Unix socket → host
 *   → Native Messaging stdio → extension service worker
 *
 * The host may optionally open a client connection to runtime.sock to announce
 * presence when Runtime is already listening (I1 wires full accept). That is
 * outbound connect only — still no TCP.
 */

import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const HOST_NAME = "com.lcu.chrome_control";

/** Runtime private entry root (same layout as crates/lcu-runtime paths.rs). */
function runtimeRoot() {
  if (process.env.LCU_RUNTIME_ROOT) {
    return path.resolve(process.env.LCU_RUNTIME_ROOT);
  }
  // macOS Application Support
  return path.join(
    os.homedir(),
    "Library",
    "Application Support",
    "AnythingUse"
  );
}

const RUNTIME_ROOT = runtimeRoot();
const SOCK_PATH = path.join(RUNTIME_ROOT, "chrome-control.sock");
const RUNTIME_SOCK = path.join(RUNTIME_ROOT, "runtime.sock");
const LOG_PATH = path.join(RUNTIME_ROOT, "logs", "chrome-control-host.log");
const MAX_LOG_BYTES = 5 * 1024 * 1024;
const MAX_LOG_FILES = 5;

const pending = new Map(); // id -> { resolve, reject, timer }
let nextId = 1;
let chromeAlive = true;
/** @type {import('node:net').Socket[]} */
const clients = [];
let extensionHello = null;
/** Hard invariant: product control never listens on TCP. */
const LISTENS_TCP = false;

function ensureDirs() {
  fs.mkdirSync(RUNTIME_ROOT, { recursive: true, mode: 0o700 });
  fs.mkdirSync(path.join(RUNTIME_ROOT, "logs"), { recursive: true, mode: 0o700 });
  try {
    fs.chmodSync(RUNTIME_ROOT, 0o700);
  } catch {
    /* ignore */
  }
}

function log(...args) {
  const line = `[${new Date().toISOString()}] ${args
    .map((a) => (typeof a === "string" ? a : JSON.stringify(a)))
    .join(" ")}\n`;
  try {
    ensureDirs();
    if (fs.existsSync(LOG_PATH) && fs.statSync(LOG_PATH).size >= MAX_LOG_BYTES) {
      fs.rmSync(`${LOG_PATH}.${MAX_LOG_FILES - 1}`, { force: true });
      for (let i = MAX_LOG_FILES - 2; i >= 1; i -= 1) {
        if (fs.existsSync(`${LOG_PATH}.${i}`)) {
          fs.renameSync(`${LOG_PATH}.${i}`, `${LOG_PATH}.${i + 1}`);
        }
      }
      fs.renameSync(LOG_PATH, `${LOG_PATH}.1`);
    }
    fs.appendFileSync(LOG_PATH, line);
  } catch {
    /* ignore */
  }
}

function sendToChrome(msg) {
  if (!chromeAlive) throw new Error("chrome stdin/stdout closed");
  const body = Buffer.from(JSON.stringify(msg), "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32LE(body.length, 0);
  process.stdout.write(header);
  process.stdout.write(body);
}

function readChromeMessages() {
  let buf = Buffer.alloc(0);

  process.stdin.on("data", (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    while (buf.length >= 4) {
      const len = buf.readUInt32LE(0);
      if (buf.length < 4 + len) break;
      const json = buf.subarray(4, 4 + len).toString("utf8");
      buf = buf.subarray(4 + len);
      try {
        const msg = JSON.parse(json);
        onChromeMessage(msg);
      } catch (err) {
        log("bad chrome json", String(err), json.slice(0, 200));
      }
    }
  });

  process.stdin.on("end", () => {
    log("chrome stdin end");
    chromeAlive = false;
    failAllPending("extension disconnected");
    shutdown(0);
  });

  process.stdin.on("error", (err) => {
    log("chrome stdin error", String(err));
    chromeAlive = false;
    failAllPending(String(err));
    shutdown(1);
  });
}

function onChromeMessage(msg) {
  log("from-extension", summarizeForLog(msg));

  if (msg && msg.type === "hello") {
    extensionHello = msg;
    broadcastEvent({ type: "extension_hello", hello: msg });
    // Best-effort announce to Runtime private entry (if accept loop is up).
    announceToRuntime({ type: "chrome_host_hello", hello: msg });
    return;
  }

  if (msg && msg.type === "pong") {
    broadcastEvent(msg);
    return;
  }

  if (msg && msg.type === "event") {
    broadcastEvent({ type: "extension_event", message: msg });
    // Forward control_state to runtime when present.
    if (msg.event === "control_state") {
      announceToRuntime({
        type: "chrome_control_state",
        controlState: msg.controlState,
        reason: msg.reason,
        lease: msg.lease,
      });
    }
    return;
  }

  if (msg && msg.id != null && pending.has(msg.id)) {
    const entry = pending.get(msg.id);
    pending.delete(msg.id);
    clearTimeout(entry.timer);
    if (msg.ok === false) {
      entry.reject(new Error(msg.error || "extension error"));
    } else {
      entry.resolve(msg.result);
    }
    return;
  }

  broadcastEvent({ type: "extension_event", message: msg });
}

function summarizeForLog(msg) {
  if (!msg || typeof msg !== "object") return msg;
  if (msg.observation) {
    return { ...msg, observation: { elements: msg.observation.elements?.length } };
  }
  return msg;
}

function failAllPending(reason) {
  for (const [id, entry] of pending.entries()) {
    clearTimeout(entry.timer);
    entry.reject(new Error(reason));
    pending.delete(id);
  }
}

function callExtension(method, params = {}, timeoutMs = 60000) {
  return new Promise((resolve, reject) => {
    if (!chromeAlive) {
      reject(new Error("native host not connected to extension"));
      return;
    }
    const id = nextId++;
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`timeout calling ${method}`));
    }, timeoutMs);
    pending.set(id, { resolve, reject, timer });
    try {
      sendToChrome({ id, method, params });
    } catch (err) {
      clearTimeout(timer);
      pending.delete(id);
      reject(err);
    }
  });
}

function writeJson(socket, obj) {
  try {
    socket.write(JSON.stringify(obj) + "\n");
  } catch (err) {
    log("write client failed", String(err));
  }
}

function broadcastEvent(obj) {
  for (const c of clients) {
    writeJson(c, { event: true, ...obj });
  }
}

/**
 * Outbound client connect to Runtime private entry (unix only). Non-fatal.
 */
function announceToRuntime(payload) {
  try {
    if (!fs.existsSync(RUNTIME_SOCK)) return;
    const sock = net.createConnection(RUNTIME_SOCK);
    sock.setTimeout(500);
    sock.on("connect", () => {
      try {
        sock.write(
          JSON.stringify({
            ...payload,
            host: HOST_NAME,
            chromeControlSock: SOCK_PATH,
            listensTcp: LISTENS_TCP,
            ts: Date.now(),
          }) + "\n"
        );
      } catch {
        /* ignore */
      }
      sock.end();
    });
    sock.on("error", () => {
      try {
        sock.destroy();
      } catch {
        /* ignore */
      }
    });
    sock.on("timeout", () => {
      try {
        sock.destroy();
      } catch {
        /* ignore */
      }
    });
  } catch (err) {
    log("announceToRuntime failed", String(err));
  }
}

async function handleClientRequest(socket, req) {
  const id = req.id ?? null;
  const method = req.method;

  if (!method) {
    writeJson(socket, { id, ok: false, error: "missing method" });
    return;
  }

  try {
    if (method === "host_ping") {
      writeJson(socket, {
        id,
        ok: true,
        result: {
          host: HOST_NAME,
          chromeAlive,
          extensionHello,
          pending: pending.size,
          clients: clients.length,
          sockPath: SOCK_PATH,
          runtimeRoot: RUNTIME_ROOT,
          runtimeSock: RUNTIME_SOCK,
          listensTcp: LISTENS_TCP,
          pid: process.pid,
          surface: "chrome_tab",
        },
      });
      return;
    }

    if (method === "host_info") {
      writeJson(socket, {
        id,
        ok: true,
        result: {
          host: HOST_NAME,
          sockPath: SOCK_PATH,
          runtimeRoot: RUNTIME_ROOT,
          listensTcp: LISTENS_TCP,
          chromeAlive,
          extensionHello,
        },
      });
      return;
    }

    let params = req.params || {};
    // Defense in depth: when the user has taken over the task tab, never allow
    // callers (or a stale extension build) to close it via closeTab:true.
    // Extension-owned taken_over is authoritative — host enforces the same rule
    // before forwarding end_task / release / cleanup.
    if (method === "end_task" || method === "release" || method === "cleanup") {
      try {
        const state = await callExtension("get_state", {}, 5000);
        const control =
          state?.controlState ||
          state?.control_state ||
          state?.lease?.controlState ||
          state?.lease?.control_state ||
          "";
        const reason = String(params?.reason || "");
        const takenOver =
          control === "taken_over" ||
          reason.includes("taken_over") ||
          reason.includes("user_activated");
        if (takenOver) {
          params = { ...params, closeTab: false, close_tab: false };
          log("host guard: taken_over → force closeTab=false", method, control, reason);
        }
      } catch (err) {
        log("host guard get_state failed", String(err?.message || err));
      }
    }

    const result = await callExtension(method, params, req.timeoutMs || 60000);
    writeJson(socket, { id, ok: true, result });
  } catch (err) {
    writeJson(socket, { id, ok: false, error: err?.message || String(err) });
  }
}

function startSocketServer() {
  ensureDirs();
  try {
    if (fs.existsSync(SOCK_PATH)) fs.unlinkSync(SOCK_PATH);
  } catch {
    /* ignore */
  }

  // Explicit guard: refuse any TCP listen configuration.
  if (process.env.LCU_CHROME_CONTROL_TCP) {
    log("refusing TCP listen request (LCU_CHROME_CONTROL_TCP set)");
    throw new Error("chrome control host must not listen on TCP");
  }

  const server = net.createServer((socket) => {
    clients.push(socket);
    log("adapter connected", clients.length);
    let buf = "";

    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      buf += chunk;
      let idx;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx).trim();
        buf = buf.slice(idx + 1);
        if (!line) continue;
        let req;
        try {
          req = JSON.parse(line);
        } catch (err) {
          writeJson(socket, { ok: false, error: `bad json: ${err.message}` });
          continue;
        }
        void handleClientRequest(socket, req);
      }
    });

    socket.on("close", () => {
      const i = clients.indexOf(socket);
      if (i >= 0) clients.splice(i, 1);
      log("adapter disconnected", clients.length);
    });

    socket.on("error", (err) => {
      log("adapter socket error", String(err));
    });

    writeJson(socket, {
      event: true,
      type: "host_ready",
      host: HOST_NAME,
      extensionHello,
      chromeAlive,
      sockPath: SOCK_PATH,
      listensTcp: LISTENS_TCP,
    });
  });

  server.on("error", (err) => {
    log("socket server error", String(err));
  });

  // Listen on Unix domain path only — never a TCP port.
  server.listen(SOCK_PATH, () => {
    try {
      fs.chmodSync(SOCK_PATH, 0o600);
    } catch {
      /* ignore */
    }
    log("listening (unix)", SOCK_PATH, { listensTcp: LISTENS_TCP });
  });

  return server;
}

let socketServer = null;

function shutdown(code) {
  try {
    if (socketServer) socketServer.close();
  } catch {
    /* ignore */
  }
  try {
    if (fs.existsSync(SOCK_PATH)) fs.unlinkSync(SOCK_PATH);
  } catch {
    /* ignore */
  }
  setTimeout(() => process.exit(code), 30);
}

process.on("SIGINT", () => shutdown(0));
process.on("SIGTERM", () => shutdown(0));

process.stdout.on("error", () => {
  chromeAlive = false;
});

log("host starting", {
  pid: process.pid,
  node: process.version,
  dir: __dirname,
  runtimeRoot: RUNTIME_ROOT,
  sockPath: SOCK_PATH,
  listensTcp: LISTENS_TCP,
});
readChromeMessages();
socketServer = startSocketServer();
