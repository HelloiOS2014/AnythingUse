/**
 * LCU Chrome Control — MV3 service worker (D3 product backend)
 *
 * Surface for ControlTarget::ChromeTab:
 * - Real user Chrome via chrome.debugger / CDP
 * - Native Messaging bridge to Runtime private Unix path (no TCP control plane)
 * - Task tab group + tab lease (claim / handoff / cleanup)
 * - Observation + action mapping for frozen P3 contract
 * - taken_over / target_lost on user activate or tab close
 * - No AX, no AppleScript, no Playwright, no history/downloads menu hacks
 */

const NATIVE_HOST = "com.lcu.chrome_control";
const TASK_GROUP_TITLE = "LCU Task";
const TASK_GROUP_COLOR = "blue";
const DEBUGGER_PROTOCOL = "1.3";
const RECONNECT_MS = 2500;
const PROFILE_STORAGE_KEY = "anythinguseProfileKey";
let cachedProfileKey = null;

/** @type {chrome.runtime.Port | null} */
let nativePort = null;
let reconnectTimer = null;

async function getProfileKey() {
  if (cachedProfileKey) return cachedProfileKey;
  const stored = await chrome.storage.local.get(PROFILE_STORAGE_KEY);
  cachedProfileKey = stored?.[PROFILE_STORAGE_KEY] || `profile_${crypto.randomUUID()}`;
  if (!stored?.[PROFILE_STORAGE_KEY]) {
    await chrome.storage.local.set({ [PROFILE_STORAGE_KEY]: cachedProfileKey });
  }
  return cachedProfileKey;
}

/**
 * @typedef {object} TabLease
 * @property {string} leaseId
 * @property {number} tabId
 * @property {number | undefined} groupId
 * @property {number | null} userActiveTabId
 * @property {number | null} userWindowId
 * @property {boolean} debuggerAttached
 * @property {number} claimedAt
 * @property {string} url
 * @property {string} profile
 * @property {string | null} taskId
 * @property {string} controlState  // none | taken_over | target_lost
 * @property {Record<string, string>} elementMap  // element_id -> css selector
 */

/** @type {TabLease | null} */
let lease = null;

function log(...args) {
  console.log("[lcu-chrome-control]", ...args);
}

function nowId(prefix) {
  return `${prefix}_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`;
}

async function getActiveTab() {
  const tabs = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (tabs[0]) return tabs[0];
  const any = await chrome.tabs.query({ active: true, currentWindow: true });
  return any[0] || null;
}

function reply(port, id, ok, payload) {
  if (!port) return;
  try {
    if (ok) {
      port.postMessage({ id, ok: true, result: payload });
    } else {
      port.postMessage({ id, ok: false, error: String(payload) });
    }
  } catch (err) {
    log("reply failed", err);
  }
}

function emitEvent(type, payload = {}) {
  if (!nativePort) return;
  try {
    nativePort.postMessage({ type: "event", event: type, ...payload, ts: Date.now() });
  } catch (err) {
    log("emitEvent failed", err);
  }
}

async function connectNative() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (nativePort) {
    try {
      nativePort.disconnect();
    } catch {
      /* ignore */
    }
    nativePort = null;
  }

  try {
    nativePort = chrome.runtime.connectNative(NATIVE_HOST);
  } catch (err) {
    log("connectNative threw", err);
    scheduleReconnect();
    return;
  }

  nativePort.onMessage.addListener((msg) => {
    void handleNativeMessage(nativePort, msg);
  });

  nativePort.onDisconnect.addListener(() => {
    const err = chrome.runtime.lastError?.message;
    log("native port disconnected", err || "(no error)");
    nativePort = null;
    // Host gone: release debugger/lease so we never leave a stuck attachment.
    void forceCleanup("native_host_disconnected");
    scheduleReconnect();
  });

  log("native host connected");
  try {
    nativePort.postMessage({
      type: "hello",
      extensionId: chrome.runtime.id,
      version: chrome.runtime.getManifest().version,
      profile: await getProfileKey(),
      surface: "chrome_tab",
    });
  } catch (err) {
    log("hello failed", err);
  }
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    void connectNative();
  }, RECONNECT_MS);
}

/**
 * @param {chrome.runtime.Port | null} port
 * @param {any} msg
 */
async function handleNativeMessage(port, msg) {
  if (!msg || typeof msg !== "object") return;

  if (msg.type === "ping" && msg.id == null) {
    port?.postMessage({ type: "pong", ts: Date.now() });
    return;
  }

  const id = msg.id;
  const method = msg.method;
  if (id == null || !method) return;

  try {
    const result = await dispatch(method, msg.params || {});
    reply(port, id, true, result);
  } catch (err) {
    reply(port, id, false, err?.message || String(err));
  }
}

/**
 * @param {string} method
 * @param {Record<string, any>} params
 */
async function dispatch(method, params) {
  switch (method) {
    case "ping":
      return {
        pong: true,
        extensionId: chrome.runtime.id,
        profile: await getProfileKey(),
        lease: summarizeLease(),
        activeTab: await snapshotActive(),
        surface: "chrome_tab",
      };
    case "get_state":
      return {
        extensionId: chrome.runtime.id,
        profile: await getProfileKey(),
        lease: summarizeLease(),
        activeTab: await snapshotActive(),
        controlState: lease?.controlState || "none",
      };
    case "start_task":
    case "claim":
      return startTask(params);
    case "handoff":
      return handoff(params);
    case "navigate":
      return navigate(params);
    case "type":
      return typeText(params);
    case "click":
      return click(params);
    case "read":
      return read(params);
    case "observe":
      return observe(params);
    case "act":
      return act(params);
    case "end_task":
    case "release":
      return endTask(params);
    case "cleanup":
      return forceCleanup(params?.reason || "cleanup");
    default:
      throw new Error(`unknown method: ${method}`);
  }
}

function summarizeLease() {
  if (!lease) return null;
  return {
    leaseId: lease.leaseId,
    tabId: lease.tabId,
    groupId: lease.groupId ?? null,
    userActiveTabId: lease.userActiveTabId,
    debuggerAttached: lease.debuggerAttached,
    claimedAt: lease.claimedAt,
    url: lease.url,
    profile: lease.profile,
    taskId: lease.taskId,
    controlState: lease.controlState,
    chromeTab: {
      profile: lease.profile,
      tab_id: lease.tabId,
    },
  };
}

function requireAutoControl() {
  const L = requireLease();
  if (L.controlState === "taken_over") {
    throw new Error("control_state=taken_over: user took over task tab");
  }
  if (L.controlState === "target_lost") {
    throw new Error("control_state=target_lost: task tab gone");
  }
  return L;
}

async function snapshotActive() {
  const tab = await getActiveTab();
  if (!tab) return null;
  return {
    tabId: tab.id ?? null,
    windowId: tab.windowId ?? null,
    url: tab.url || "",
    title: tab.title || "",
    active: !!tab.active,
  };
}

async function ensureUserTabStillActive(expectedTabId) {
  if (expectedTabId == null) return { ok: true, activeTabId: null };
  const active = await getActiveTab();
  const activeTabId = active?.id ?? null;
  return {
    ok: activeTabId === expectedTabId,
    activeTabId,
    expectedTabId,
  };
}

/**
 * Claim URL must allow chrome.debugger.attach.
 * chrome://, chrome-extension://, edge://, devtools:// are blocked by Chromium.
 * Default to a neutral https page; never chrome://newtab.
 */
function sanitizeClaimUrl(raw) {
  const fallback = "https://example.com/";
  const s = String(raw || "").trim();
  if (!s) return fallback;
  const lower = s.toLowerCase();
  if (
    lower.startsWith("chrome://") ||
    lower.startsWith("chrome-extension://") ||
    lower.startsWith("chrome-search://") ||
    lower.startsWith("devtools://") ||
    lower.startsWith("edge://") ||
    lower.startsWith("about:")
  ) {
    log("sanitizeClaimUrl rewrite", s, "→", fallback);
    return fallback;
  }
  return s;
}

/**
 * Claim a background task tab in the LCU Task group and attach debugger.
 * Params: { url?, taskId?, profile? }
 */
async function startTask(params = {}) {
  if (lease) {
    throw new Error(
      `lease already held: ${lease.leaseId} tab=${lease.tabId} (use handoff or cleanup first)`
    );
  }

  // Background task tab only; navigation is done later via normal VLM actions.
  // Debugger cannot attach to chrome:// / chrome-extension:// / devtools:// pages.
  const url = sanitizeClaimUrl(params.url);
  const profile = params.profile || (await getProfileKey());
  const taskId = params.taskId || params.task_id || null;

  const userTab = await getActiveTab();
  const userActiveTabId = userTab?.id ?? null;
  const userWindowId = userTab?.windowId ?? undefined;

  // Create inactive so we do not steal the user's active tab.
  // Never write active:true on the user's tab (claim-time restore was a focus steal:
  // if the user switched tabs during create/group, force-restoring A yanks them back).
  const tab = await chrome.tabs.create({
    url,
    active: false,
    windowId: userWindowId,
  });
  if (tab.id == null) {
    throw new Error("tabs.create returned no tab id");
  }

  let groupId;
  try {
    groupId = await chrome.tabs.group({
      tabIds: [tab.id],
      createProperties: userWindowId != null ? { windowId: userWindowId } : undefined,
    });
    await chrome.tabGroups.update(groupId, {
      title: TASK_GROUP_TITLE,
      color: TASK_GROUP_COLOR,
      collapsed: false,
    });
  } catch (err) {
    await safeRemoveTab(tab.id);
    throw new Error(`tab group failed: ${err?.message || err}`);
  }

  // Observational only — never call tabs.update({ active: true }).
  const selection = await ensureUserTabStillActive(userActiveTabId);

  try {
    await chrome.debugger.attach({ tabId: tab.id }, DEBUGGER_PROTOCOL);
  } catch (err) {
    await safeUngroupAndClose(tab.id, groupId);
    throw new Error(`debugger.attach failed: ${err?.message || err}`);
  }

  try {
    await sendCdp(tab.id, "Page.enable", {});
    await sendCdp(tab.id, "Runtime.enable", {});
    await sendCdp(tab.id, "DOM.enable", {});
  } catch (err) {
    await safeDetach(tab.id);
    await safeUngroupAndClose(tab.id, groupId);
    throw new Error(`CDP enable failed: ${err?.message || err}`);
  }

  lease = {
    leaseId: nowId("lease"),
    tabId: tab.id,
    groupId,
    userActiveTabId,
    userWindowId,
    debuggerAttached: true,
    claimedAt: Date.now(),
    url,
    profile,
    taskId,
    controlState: "none",
    elementMap: {},
  };

  log("lease claimed", summarizeLease());

  return {
    lease: summarizeLease(),
    chromeTab: { profile, tab_id: tab.id },
    controlState: "none",
    userSelectionPreserved: selection.ok,
    activeTab: await snapshotActive(),
    selection,
  };
}

/**
 * Handoff lease ownership to another task id without releasing debugger/tab.
 * Params: { taskId }
 */
async function handoff(params = {}) {
  const L = requireLease();
  const nextTask = params.taskId || params.task_id;
  if (!nextTask) throw new Error("handoff requires params.taskId");
  if (L.controlState !== "none") {
    throw new Error(`cannot handoff while control_state=${L.controlState}`);
  }
  const prev = L.taskId;
  L.taskId = String(nextTask);
  log("lease handoff", { from: prev, to: L.taskId, leaseId: L.leaseId });
  return {
    lease: summarizeLease(),
    previousTaskId: prev,
    chromeTab: { profile: L.profile, tab_id: L.tabId },
  };
}

function requireLease() {
  if (!lease) throw new Error("no active tab lease");
  return lease;
}

/**
 * @param {number} tabId
 * @param {string} method
 * @param {object} [commandParams]
 */
function sendCdp(tabId, method, commandParams = {}) {
  return new Promise((resolve, reject) => {
    chrome.debugger.sendCommand({ tabId }, method, commandParams, (result) => {
      const err = chrome.runtime.lastError;
      if (err) {
        reject(new Error(`${method}: ${err.message}`));
        return;
      }
      resolve(result);
    });
  });
}

async function waitForLoad(tabId, timeoutMs = 15000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await sendCdp(tabId, "Runtime.evaluate", {
        expression: "document.readyState",
        returnByValue: true,
      });
      const state = res?.result?.value;
      if (state === "interactive" || state === "complete") {
        return state;
      }
    } catch {
      /* navigating */
    }
    await sleep(100);
  }
  throw new Error("timeout waiting for document readyState");
}

async function navigate(params) {
  const L = requireAutoControl();
  const url = params?.url;
  if (!url) throw new Error("navigate requires params.url");

  await sendCdp(L.tabId, "Page.navigate", { url });
  const ready = await waitForLoad(L.tabId);
  L.url = url;
  L.elementMap = {};

  const selection = await ensureUserTabStillActive(L.userActiveTabId);
  return {
    url,
    ready,
    userSelectionPreserved: selection.ok,
    selection,
    lease: summarizeLease(),
  };
}

async function typeText(params) {
  const L = requireAutoControl();
  const selector = params?.selector || resolveSelector(params?.element_id);
  const text = params?.text ?? params?.value ?? "";
  if (!selector) throw new Error("type requires params.selector or element_id");

  await waitForLoad(L.tabId);
  const expression = `
    (() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) return { ok: false, error: "selector not found: " + ${JSON.stringify(selector)} };
      el.focus();
      if ("value" in el) {
        el.value = ${JSON.stringify(text)};
      } else {
        el.textContent = ${JSON.stringify(text)};
      }
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
      return { ok: true, value: ("value" in el) ? el.value : el.textContent };
    })()
  `;
  const res = await sendCdp(L.tabId, "Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: false,
  });
  const value = res?.result?.value;
  if (!value?.ok) {
    throw new Error(value?.error || "type failed");
  }

  const selection = await ensureUserTabStillActive(L.userActiveTabId);
  return {
    selector,
    text,
    value: value.value,
    userSelectionPreserved: selection.ok,
    selection,
  };
}

async function click(params) {
  const L = requireAutoControl();
  const selector = params?.selector || resolveSelector(params?.element_id);
  if (!selector && (params?.x == null || params?.y == null)) {
    throw new Error("click requires selector/element_id or x/y");
  }

  await waitForLoad(L.tabId);

  if (selector) {
    const expression = `
      (() => {
        const el = document.querySelector(${JSON.stringify(selector)});
        if (!el) return { ok: false, error: "selector not found: " + ${JSON.stringify(selector)} };
        el.click();
        return { ok: true, tag: el.tagName, id: el.id || null };
      })()
    `;
    const res = await sendCdp(L.tabId, "Runtime.evaluate", {
      expression,
      returnByValue: true,
    });
    const value = res?.result?.value;
    if (!value?.ok) {
      throw new Error(value?.error || "click failed");
    }
    const selection = await ensureUserTabStillActive(L.userActiveTabId);
    return {
      selector,
      target: value,
      userSelectionPreserved: selection.ok,
      selection,
    };
  }

  // Normalized [0,1] coords → CSS pixels via CDP Input.dispatchMouseEvent.
  const metrics = await sendCdp(L.tabId, "Runtime.evaluate", {
    expression:
      "({ w: window.innerWidth || 1, h: window.innerHeight || 1 })",
    returnByValue: true,
  });
  const w = metrics?.result?.value?.w || 1;
  const h = metrics?.result?.value?.h || 1;
  const x = Number(params.x) * w;
  const y = Number(params.y) * h;
  await sendCdp(L.tabId, "Input.dispatchMouseEvent", {
    type: "mousePressed",
    x,
    y,
    button: params.button === "right" ? "right" : "left",
    clickCount: 1,
  });
  await sendCdp(L.tabId, "Input.dispatchMouseEvent", {
    type: "mouseReleased",
    x,
    y,
    button: params.button === "right" ? "right" : "left",
    clickCount: 1,
  });
  const selection = await ensureUserTabStillActive(L.userActiveTabId);
  return {
    x: params.x,
    y: params.y,
    pixel: { x, y },
    userSelectionPreserved: selection.ok,
    selection,
  };
}

async function read(params) {
  const L = requireAutoControl();
  const selector = params?.selector || resolveSelector(params?.element_id);
  if (!selector) throw new Error("read requires params.selector or element_id");

  await waitForLoad(L.tabId);
  const expression = `
    (() => {
      const el = document.querySelector(${JSON.stringify(selector)});
      if (!el) return { ok: false, error: "selector not found: " + ${JSON.stringify(selector)} };
      return {
        ok: true,
        text: (el.innerText != null ? el.innerText : el.textContent) || "",
        value: ("value" in el) ? el.value : null,
        status: el.dataset ? (el.dataset.status || null) : null,
      };
    })()
  `;
  const res = await sendCdp(L.tabId, "Runtime.evaluate", {
    expression,
    returnByValue: true,
  });
  const value = res?.result?.value;
  if (!value?.ok) {
    throw new Error(value?.error || "read failed");
  }

  const selection = await ensureUserTabStillActive(L.userActiveTabId);
  return {
    selector,
    text: value.text,
    value: value.value,
    status: value.status,
    userSelectionPreserved: selection.ok,
    selection,
  };
}

/**
 * Build a P3-mapped observation from the task tab DOM (no AX tree).
 */
async function observe(_params = {}) {
  const L = requireAutoControl();
  await waitForLoad(L.tabId);

  const expression = `
    (() => {
      const vw = window.innerWidth || 1;
      const vh = window.innerHeight || 1;
      const nodes = [];
      const pick = document.querySelectorAll(
        "a[href], button, input, textarea, select, [role='button'], [role='link'], [role='textbox'], [contenteditable='true']"
      );
      let i = 0;
      for (const el of pick) {
        if (i >= 80) break;
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) continue;
        let selector = null;
        if (el.id) selector = "#" + CSS.escape(el.id);
        else if (el.name) selector = el.tagName.toLowerCase() + "[name=" + JSON.stringify(el.name) + "]";
        else {
          const path = [];
          let cur = el;
          while (cur && cur.nodeType === 1 && path.length < 5) {
            let part = cur.tagName.toLowerCase();
            if (cur.id) { path.unshift("#" + CSS.escape(cur.id)); break; }
            const parent = cur.parentElement;
            if (parent) {
              const sibs = Array.from(parent.children).filter((c) => c.tagName === cur.tagName);
              if (sibs.length > 1) part += ":nth-of-type(" + (sibs.indexOf(cur) + 1) + ")";
            }
            path.unshift(part);
            cur = parent;
          }
          selector = path.join(" > ");
        }
        const id = "el_" + i;
        const role = (el.getAttribute("role")
          || (el.tagName === "A" ? "link"
          : el.tagName === "BUTTON" ? "button"
          : el.tagName === "INPUT" || el.tagName === "TEXTAREA" ? "textbox"
          : el.tagName === "SELECT" ? "combobox"
          : el.tagName.toLowerCase())).toLowerCase();
        const label = el.getAttribute("aria-label")
          || el.getAttribute("placeholder")
          || (el.innerText || "").trim().slice(0, 80)
          || el.id
          || null;
        const value = ("value" in el) ? String(el.value ?? "") : null;
        const actions = [];
        if (role === "button" || role === "link") actions.push("invoke");
        if (role === "textbox" || role === "combobox" || el.tagName === "INPUT" || el.tagName === "TEXTAREA") {
          actions.push("set_value", "focus");
        }
        actions.push("focus");
        nodes.push({
          id,
          role,
          label,
          value,
          frame: {
            x: r.x / vw,
            y: r.y / vh,
            width: r.width / vw,
            height: r.height / vh,
          },
          actions,
          selector,
        });
        i += 1;
      }
      return {
        title: document.title || "",
        url: location.href || "",
        viewport: { width: vw, height: vh },
        elements: nodes,
      };
    })()
  `;

  const res = await sendCdp(L.tabId, "Runtime.evaluate", {
    expression,
    returnByValue: true,
  });
  const page = res?.result?.value;
  if (!page) throw new Error("observe evaluate returned empty");

  L.elementMap = {};
  for (const el of page.elements || []) {
    if (el.id && el.selector) L.elementMap[el.id] = el.selector;
  }
  L.url = page.url || L.url;

  const selection = await ensureUserTabStillActive(L.userActiveTabId);

  // CDP screenshot of the task tab only (does not activate the tab).
  let imagePngB64 = null;
  try {
    const shot = await sendCdp(L.tabId, "Page.captureScreenshot", {
      format: "png",
      fromSurface: true,
      captureBeyondViewport: false,
    });
    if (shot?.data) imagePngB64 = shot.data;
  } catch (err) {
    log("observe screenshot failed", err?.message || err);
  }

  return {
    observation: {
      // Maps to AppObservation-shaped fields on the Rust adapter side.
      target: {
        app_id: "com.google.Chrome",
        // Synthetic: Chrome surface uses tab id, not CGWindow.
        pid: 0,
        window_id: L.tabId,
        window_title: page.title || "",
      },
      chrome_tab: {
        profile: L.profile,
        tab_id: L.tabId,
      },
      window_frame: {
        x: 0,
        y: 0,
        width: page.viewport?.width || 0,
        height: page.viewport?.height || 0,
      },
      model_size: {
        width: Math.round(page.viewport?.width || 0),
        height: Math.round(page.viewport?.height || 0),
      },
      elements: (page.elements || []).map((el) => ({
        id: el.id,
        role: el.role,
        label: el.label,
        value: el.value,
        frame: el.frame,
        actions: el.actions,
      })),
      capture_backend: "chrome_debugger_cdp",
      image_png_b64: imagePngB64,
      page_url: page.url,
      page_title: page.title,
    },
    controlState: L.controlState,
    lease: summarizeLease(),
    userSelectionPreserved: selection.ok,
    selection,
  };
}

function resolveSelector(elementId) {
  if (!elementId || !lease) return null;
  if (lease.elementMap[elementId]) return lease.elementMap[elementId];
  // Allow raw css: / id: prefixes without a prior observe.
  if (String(elementId).startsWith("css:")) return String(elementId).slice(4);
  if (String(elementId).startsWith("#") || String(elementId).includes(" ")) {
    return String(elementId);
  }
  if (String(elementId).startsWith("id:")) return "#" + CSS.escape(String(elementId).slice(3));
  return null;
}

/**
 * Execute a frozen P3 Action payload against the leased tab.
 * Params: { action: { kind, ... } }  (serde shape of lcu_core::Action)
 */
async function act(params = {}) {
  const L = requireAutoControl();
  const action = params.action;
  if (!action || typeof action !== "object") {
    throw new Error("act requires params.action");
  }
  const kind = action.kind;

  if (kind === "observe") {
    return { kind, ...(await observe({})) };
  }

  if (kind === "wait") {
    const ms = Math.min(Number(action.milliseconds || 0), 30_000);
    await sleep(ms);
    return { kind, waited_ms: ms, success: true };
  }

  if (kind === "done" || kind === "fail" || kind === "request_user") {
    return { kind, success: true, passthrough: true, action };
  }

  if (kind === "semantic") {
    // serde tag is flattened as action.type under Semantic variant in some forms;
    // also accept nested { kind: "semantic", type, ... } or { kind, semantic: {...} }
    const sem = action.type
      ? action
      : action.semantic || action;
    const t = sem.type;
    if (t === "invoke") {
      const r = await click({ element_id: sem.element_id });
      return { kind, type: t, success: true, ...r };
    }
    if (t === "set_value") {
      const r = await typeText({ element_id: sem.element_id, text: sem.value ?? "" });
      return { kind, type: t, success: true, ...r };
    }
    if (t === "focus") {
      const selector = resolveSelector(sem.element_id);
      if (!selector) throw new Error("focus requires known element_id");
      await sendCdp(L.tabId, "Runtime.evaluate", {
        expression: `(() => { const el = document.querySelector(${JSON.stringify(
          selector
        )}); if (!el) throw new Error("not found"); el.focus(); return true; })()`,
        returnByValue: true,
      });
      return { kind, type: t, success: true, element_id: sem.element_id };
    }
    if (t === "scroll") {
      const dx = Number(sem.delta_x || 0);
      const dy = Number(sem.delta_y || 0);
      await sendCdp(L.tabId, "Runtime.evaluate", {
        expression: `window.scrollBy(${dx}, ${dy}); true`,
        returnByValue: true,
      });
      return { kind, type: t, success: true, delta_x: dx, delta_y: dy };
    }
    throw new Error(`unsupported semantic type: ${t}`);
  }

  if (kind === "targeted") {
    const ti = action.type ? action : action.targeted || action;
    const t = ti.type;
    if (t === "click") {
      const r = await click({
        x: ti.x,
        y: ti.y,
        button: ti.button,
      });
      return { kind, type: t, success: true, ...r };
    }
    if (t === "type_text") {
      // Type into currently focused element in the task tab.
      const text = ti.text ?? "";
      await sendCdp(L.tabId, "Runtime.evaluate", {
        expression: `
          (() => {
            const el = document.activeElement;
            if (!el) return { ok: false, error: "no active element" };
            if ("value" in el) {
              el.value = (el.value || "") + ${JSON.stringify(text)};
            } else {
              el.textContent = (el.textContent || "") + ${JSON.stringify(text)};
            }
            el.dispatchEvent(new Event("input", { bubbles: true }));
            el.dispatchEvent(new Event("change", { bubbles: true }));
            return { ok: true };
          })()
        `,
        returnByValue: true,
      });
      return { kind, type: t, success: true, text };
    }
    if (t === "key_combo") {
      const keys = Array.isArray(ti.keys) ? ti.keys : [];
      // Minimal CDP key events for common combos (Enter, Tab, Escape, Meta+a).
      for (const key of keys) {
        const def = keyDef(key);
        await sendCdp(L.tabId, "Input.dispatchKeyEvent", {
          type: "keyDown",
          ...def,
        });
        await sendCdp(L.tabId, "Input.dispatchKeyEvent", {
          type: "keyUp",
          ...def,
        });
      }
      return { kind, type: t, success: true, keys };
    }
    throw new Error(`unsupported targeted type: ${t}`);
  }

  throw new Error(`unsupported action kind: ${kind}`);
}

function keyDef(key) {
  const k = String(key);
  const lower = k.toLowerCase();
  if (lower === "enter" || lower === "return") {
    return { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 };
  }
  if (lower === "tab") {
    return { key: "Tab", code: "Tab", windowsVirtualKeyCode: 9 };
  }
  if (lower === "escape" || lower === "esc") {
    return { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 };
  }
  if (lower === "backspace") {
    return { key: "Backspace", code: "Backspace", windowsVirtualKeyCode: 8 };
  }
  if (k.length === 1) {
    return {
      key: k,
      text: k,
      unmodifiedText: k,
      windowsVirtualKeyCode: k.toUpperCase().charCodeAt(0),
    };
  }
  return { key: k, code: k };
}

/**
 * Release debugger + tab lease. Optionally close the task tab.
 *
 * Never re-activates the user tab recorded at claim time — that steals focus
 * from whatever the user is currently viewing.
 *
 * Before closing the task tab, re-query whether it is currently the active tab.
 * If the user is viewing it (or already taken_over), only detach + release —
 * never close.
 */
async function endTask(params = {}) {
  const current = lease;
  if (!current) {
    return { released: false, reason: "no lease", debuggerAttached: false };
  }

  // Extension-owned `taken_over` is authoritative. Once the user has taken over
  // the task tab, no caller (including cleanup with closeTab:true) may close it.
  const takenOver = current.controlState === "taken_over";
  let closeTab = false;
  if (takenOver) {
    closeTab = false;
  } else if (params.closeTab !== undefined || params.close_tab !== undefined) {
    closeTab = params.closeTab !== false && params.close_tab !== false;
  } else {
    closeTab = true;
  }

  const tabId = current.tabId;
  const groupId = current.groupId;

  // Re-check live active tab right before close — claim-time userActiveTabId is
  // not authority for "user is looking at task tab now".
  if (closeTab) {
    try {
      const active = await getActiveTab();
      if (active?.id === tabId) {
        closeTab = false;
        log("endTask: task tab is currently active — detach only, keep tab", tabId);
      }
    } catch (err) {
      // Fail closed: if we cannot prove the tab is not active, do not close.
      closeTab = false;
      log("endTask: active-tab recheck failed — refuse close", err?.message || err);
    }
  }

  // Always detach debugger first so cancel/fail/complete/takeover never leave attach stuck.
  if (current.debuggerAttached) {
    await safeDetach(tabId);
  }
  // Ensure flag is cleared even if detach was already external.
  current.debuggerAttached = false;

  // Final close decision, adjacent to the close itself. Between the active-tab
  // recheck above and here there was an await (safeDetach); the user may have
  // clicked the task tab in that window (onActivated sets taken_over). Re-read
  // the lease state and the live active tab: either one matching means keep.
  if (closeTab) {
    const finalTakenOver = lease?.controlState === "taken_over";
    if (finalTakenOver) {
      closeTab = false;
      log("endTask: user took over during detach — keep tab", tabId);
    } else {
      try {
        const active = await getActiveTab();
        if (active?.id === tabId) {
          closeTab = false;
          log("endTask: task tab became active during detach — keep tab", tabId);
        }
      } catch (err) {
        closeTab = false;
        log("endTask: final active-tab recheck failed — refuse close", err?.message || err);
      }
    }
  }

  if (closeTab) {
    await safeUngroupAndClose(tabId, groupId);
  }

  lease = null;
  log("lease released", current.leaseId, { closeTab, takenOver });

  return {
    released: true,
    leaseId: current.leaseId,
    debuggerAttached: false,
    tabClosed: closeTab,
    // Report current active tab without mutating selection.
    activeTab: await snapshotActive(),
    controlState: "none",
  };
}

/**
 * Force cleanup on cancel / crash / disconnect / user takeover terminal.
 * Always detaches debugger and clears the lease; never leaves attach stuck.
 */
async function forceCleanup(reason = "force") {
  log("forceCleanup", reason);
  if (!lease) {
    return { cleaned: false, reason: "no lease", trigger: reason, debuggerAttached: false };
  }
  const reasonStr = String(reason || "");
  // User viewing the task tab: detach+release lease but keep the tab open.
  const keepTab =
    reasonStr.includes("taken_over") ||
    reasonStr.includes("user_activated") ||
    reasonStr.includes("debugger_canceled");
  try {
    const result = await endTask({ closeTab: !keepTab });
    return { cleaned: true, trigger: reason, ...result };
  } catch (err) {
    // Best-effort detach even if endTask throws — never leave debugger attached.
    const tabId = lease?.tabId;
    try {
      if (tabId != null) await safeDetach(tabId);
    } catch {
      /* ignore */
    }
    if (!keepTab && tabId != null) {
      try {
        await safeRemoveTab(tabId);
      } catch {
        /* ignore */
      }
    }
    lease = null;
    return {
      cleaned: true,
      trigger: reason,
      debuggerAttached: false,
      error: String(err?.message || err),
    };
  }
}

async function safeDetach(tabId) {
  try {
    await new Promise((resolve) => {
      chrome.debugger.detach({ tabId }, () => {
        void chrome.runtime.lastError;
        resolve();
      });
    });
  } catch {
    /* ignore */
  }
  if (lease && lease.tabId === tabId) {
    lease.debuggerAttached = false;
  }
}

async function safeRemoveTab(tabId) {
  try {
    await chrome.tabs.remove(tabId);
  } catch {
    /* ignore */
  }
}

async function safeUngroupAndClose(tabId, _groupId) {
  await safeRemoveTab(tabId);
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

function markTakenOver(reason) {
  if (!lease) return;
  if (lease.controlState === "target_lost") return;
  lease.controlState = "taken_over";
  log("taken_over", reason, lease.leaseId);
  // Detach debugger immediately so takeover truly releases automation control.
  // Keep the tab open (user is viewing it) and keep lease until Runtime release.
  const tabId = lease.tabId;
  const wasAttached = lease.debuggerAttached;
  lease.debuggerAttached = false;
  if (wasAttached) {
    void safeDetach(tabId);
  }
  emitEvent("control_state", {
    controlState: "taken_over",
    reason,
    lease: summarizeLease(),
  });
}

function markTargetLost(reason) {
  if (!lease) return;
  lease.controlState = "target_lost";
  // Tab may already be gone; still best-effort detach then drop lease.
  if (lease.debuggerAttached) {
    void safeDetach(lease.tabId);
  }
  lease.debuggerAttached = false;
  log("target_lost", reason, lease.leaseId);
  emitEvent("control_state", {
    controlState: "target_lost",
    reason,
    lease: summarizeLease(),
  });
}

// Debugger detached externally → treat as loss of control surface.
chrome.debugger.onDetach.addListener((source, reason) => {
  log("debugger detached", source, reason);
  if (lease && source.tabId === lease.tabId) {
    lease.debuggerAttached = false;
    if (reason === "canceled_by_user") {
      markTakenOver("debugger_canceled_by_user");
    } else if (lease.controlState === "none") {
      // Target may still exist; keep lease but flag taken_over if user cancelled.
      if (String(reason).includes("canceled") || String(reason).includes("replaced")) {
        markTakenOver(String(reason));
      }
    }
  }
});

// User selects the agent task tab → taken_over (yield automation + detach).
chrome.tabs.onActivated.addListener((activeInfo) => {
  if (!lease) return;
  if (activeInfo.tabId === lease.tabId) {
    markTakenOver("user_activated_task_tab");
  }
});

// Task tab closed → target_lost + clear lease (debugger already gone with the tab).
chrome.tabs.onRemoved.addListener((tabId) => {
  if (!lease || lease.tabId !== tabId) return;
  markTargetLost("tab_removed");
  lease = null;
});

chrome.action.onClicked.addListener(() => {
  log("action clicked; native connected=", !!nativePort, "lease=", summarizeLease());
  if (!nativePort) void connectNative();
});

// Best-effort release if the service worker is suspended while holding a lease.
// (MV3 may kill SW; debugger detach listeners above still fire.)
self.addEventListener?.("unload", () => {
  void forceCleanup("service_worker_unload");
});

void connectNative();
log("service worker started", chrome.runtime.id);
