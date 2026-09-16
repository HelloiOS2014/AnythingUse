//! On-demand `lau` daemon: holds task state across CLI processes, then idle-exits.
//! Not a launchd service. Mirrors `lcu-desktop` lifecycle (spawn on first use,
//! exit after idle). ADB is transport/observation only.

use anyhow::{bail, Context, Result};
use anything_core::{Action, EffectClaim, EffectKind, SemanticAction};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::helper;
use crate::{helper_rpc, parse_devices, resolve_target, run_adb, run_adb_bytes, TargetResolution};

const IDLE_DEFAULT_SECS: u64 = 60;

/// Diagnostic log bound (plan §6).
const LOG_MAX_BYTES: u64 = 64 * 1024;

pub fn sock_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/AnythingUse/lau/lau.sock")
}

fn idle_secs() -> u64 {
    std::env::var("LAU_IDLE_EXIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(IDLE_DEFAULT_SECS)
}

pub fn ping_daemon() -> bool {
    rpc(&json!({"op": "ping"})).is_ok()
}

pub fn rpc(req: &Value) -> Result<Value> {
    let path = sock_path();
    let mut stream = UnixStream::connect(&path).with_context(|| format!("connect {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.write_all(format!("{req}\n").as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(line.trim()).context("daemon response is not JSON")
}

pub fn ensure_daemon() -> Result<()> {
    if ping_daemon() {
        return Ok(());
    }
    let path = sock_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if path.exists() && !ping_daemon() {
        let _ = fs::remove_file(&path);
    }
    let exe = std::env::current_exe().context("current_exe")?;
    Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn lau daemon")?;
    for _ in 0..50 {
        thread::sleep(Duration::from_millis(50));
        if ping_daemon() {
            return Ok(());
        }
    }
    bail!("lau daemon did not become ready")
}

struct Task {
    id: String,
    goal: String,
    app: String,
    serial: String,
    actor: String,
    state: String,
    wait_reason: Option<String>,
    step: u32,
    observation_id: Option<String>,
    /// R4 gate: the human performs the action; nothing is stored for replay.
    takeover: bool,
    /// App access (plan §5.4 / D8): stable identity of the controlled package,
    /// its display label, and whether this task may proceed.
    app_key: Option<String>,
    app_label: Option<String>,
    app_allowed: bool,
    /// Parked consequence gate: only what the human is being asked about is kept.
    /// The proposal itself is deliberately NOT stored — approval never replays it
    /// (plan §5.4 / D9).
    pending_identity: Option<String>,
    pending_brief: Option<String>,
    /// One-time grant produced by an approval; consumed by a matching, freshly
    /// observed proposal.
    grant: Option<Grant>,
    elements: Value,
    image_path: Option<String>,
    last_action_summary: Option<String>,
    summary: Option<String>,
    error: Option<String>,
    touch_epoch: u64,
}

/// Per-device hardware-touch watch. The epoch is keyed by serial (never one
/// global counter) and the watch must be live before any task on that device
/// may be steered — a dead stream pauses the task instead of silently
/// pretending the user is co-existing.
struct TouchWatch {
    /// Bumped on every real hardware touch sequence seen for this device.
    epoch: u64,
    /// True while the `getevent` reader is attached and streaming.
    healthy: bool,
    /// Why the watch is not live (spawn failure / stream ended / never started).
    dead_reason: Option<String>,
}

impl TouchWatch {
    fn starting(epoch: u64) -> Self {
        Self {
            epoch,
            healthy: false,
            dead_reason: None,
        }
    }
}

/// Snapshot of a device watch, taken before mutably borrowing a task.
struct WatchState {
    epoch: u64,
    healthy: bool,
    dead_reason: Option<String>,
}

impl WatchState {
    fn reason(&self) -> String {
        self.dead_reason
            .clone()
            .unwrap_or_else(|| "touch watch is not attached".into())
    }
}

fn watch_state(inner: &Inner, serial: &str) -> WatchState {
    match inner.watches.get(serial) {
        Some(w) => WatchState {
            epoch: w.epoch,
            healthy: w.healthy,
            dead_reason: w.dead_reason.clone(),
        },
        None => WatchState {
            epoch: 0,
            healthy: false,
            dead_reason: Some("no touch watch for this device".into()),
        },
    }
}

fn watch_epoch(inner: &Arc<Mutex<Inner>>, serial: &str) -> u64 {
    inner
        .lock()
        .map(|g| g.watches.get(serial).map(|w| w.epoch).unwrap_or(0))
        .unwrap_or(0)
}

/// Fail-closed guard shared by `decide` and `act`. A task may only be steered
/// while its device's touch watch is live and has not seen a real touch.
/// Returns the error code to report; the task is already paused.
fn watch_gate(t: &mut Task, watch: &WatchState) -> Option<&'static str> {
    if !watch.healthy {
        t.state = "paused".into();
        t.wait_reason = Some("watch_unavailable".into());
        t.error = Some(watch.reason());
        return Some("watch_unavailable");
    }
    if t.touch_epoch != watch.epoch {
        t.state = "paused".into();
        t.wait_reason = Some("taken_over".into());
        t.last_action_summary = Some("paused: real touch on device".into());
        return Some("taken_over");
    }
    None
}

/// One-time approval for an exact consequence (plan §5.4 / D9).
struct Grant {
    identity: String,
    expires: Instant,
}

/// A persisted `always_allow` app-access decision, keyed by identity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PermissionEntry {
    label: String,
    decided_at: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PermissionFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    permissions: HashMap<String, PermissionEntry>,
}

fn permissions_path() -> PathBuf {
    sock_path().with_file_name("app_permissions.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load persisted app-access decisions. A missing or unreadable file is simply
/// "nothing is allowed yet" — never an error that could be mistaken for a grant.
fn load_permissions() -> HashMap<String, PermissionEntry> {
    let Ok(raw) = std::fs::read_to_string(permissions_path()) else {
        return HashMap::new();
    };
    serde_json::from_str::<PermissionFile>(&raw)
        .map(|f| f.permissions)
        .unwrap_or_default()
}

/// Persist decisions with 0600 (they are a security boundary, not user data).
fn save_permissions(permissions: &HashMap<String, PermissionEntry>) {
    let file = PermissionFile {
        version: 1,
        permissions: permissions.clone(),
    };
    let Ok(raw) = serde_json::to_string_pretty(&file) else {
        return;
    };
    let path = permissions_path();
    if std::fs::write(&path, raw).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

/// How long an approval stays usable for a matching fresh proposal.
const GRANT_TTL_SECS: u64 = 300;

/// Readable, credential-free identity of one consequence. A grant matches only
/// the same app + action shape + declared effect.
fn consequence_identity(app: &str, action: &Action, effect: Option<&EffectClaim>) -> String {
    let kind = effect
        .map(|e| format!("{:?}", e.kind))
        .unwrap_or_else(|| "none".into());
    format!("{app}|{}|{kind}", action_brief(action))
}

struct Inner {
    tasks: HashMap<String, Task>,
    last_activity: Instant,
    /// One hardware-touch watch per device serial.
    watches: HashMap<String, TouchWatch>,
    /// Persisted `always_allow` app-access decisions (plan §5.4).
    permissions: HashMap<String, PermissionEntry>,
}

pub fn daemon_main() -> Result<()> {
    let path = sock_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    rotate_log();
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    listener.set_nonblocking(true)?;
    let inner = Arc::new(Mutex::new(Inner {
        tasks: HashMap::new(),
        last_activity: Instant::now(),
        watches: HashMap::new(),
        permissions: load_permissions(),
    }));
    let idle = Duration::from_secs(idle_secs());
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Ok(mut g) = inner.lock() {
                    g.last_activity = Instant::now();
                }
                let _ = handle_client(stream, &inner);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(200));
            }
            Err(e) => bail!("accept: {e}"),
        }
        let g = inner.lock().expect("inner");
        let busy = g.tasks.values().any(|t| {
            t.state == "waiting_actor" || t.state == "running" || t.state == "paused"
        });
        if !busy && g.last_activity.elapsed() > idle {
            drop(g);
            let _ = fs::remove_file(&path);
            return Ok(());
        }
    }
}

fn handle_client(mut stream: UnixStream, inner: &Arc<Mutex<Inner>>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let req: Value = serde_json::from_str(line.trim()).unwrap_or(json!({}));
    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let resp = match op {
        "ping" => json!({"ok": true, "data": {"pong": true}}),
        "run" => op_run(&req, inner),
        "decide" => op_decide(&req, inner),
        "act" => op_act(&req, inner),
        "status" => op_status(&req, inner),
        "result" => op_result(&req, inner),
        "cancel" => op_cancel(&req, inner),
        "resume" => op_resume(&req, inner),
        "permissions_list" => op_permissions_list(inner),
        "permissions_revoke" => op_permissions_revoke(&req, inner),
        "approve" => op_approve(&req, inner),
        _ => json!({"ok": false, "error": "unknown op"}),
    };
    let payload = format!("{resp}\n");
    // Plan §6: op + byte length only (never the payload) so a recurrence of the
    // 8192-byte truncation (§0 #19) leaves a trace.
    log_response(op, payload.len());
    stream.write_all(payload.as_bytes())?;
    Ok(())
}

/// Append one diagnostic line. Bounded: the file is truncated at daemon start
/// once it grows past [`LOG_MAX_BYTES`].
fn log_response(op: &str, bytes: usize) {
    let path = sock_path().with_file_name("daemon.log");
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("{secs} op={op} bytes={bytes}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

fn rotate_log() {
    let path = sock_path().with_file_name("daemon.log");
    if let Ok(md) = std::fs::metadata(&path) {
        if md.len() > LOG_MAX_BYTES {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn op_run(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let goal = req.get("goal").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let app = req.get("app").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if goal.is_empty() || app.is_empty() {
        return json!({"ok": false, "error": "goal and --app are required"});
    }
    let devices = match run_adb(None, &["devices", "-l"]) {
        Ok(raw) => parse_devices(&raw),
        Err(e) => return json!({"ok": false, "error": format!("{e:#}")}),
    };
    let serial = match resolve_target(req.get("serial").and_then(|v| v.as_str()), &devices) {
        TargetResolution::Resolved(s) => s,
        TargetResolution::Ambiguous(m) | TargetResolution::Unavailable(m) => {
            return json!({"ok": false, "error": m});
        }
    };
    // Fail closed at submission: no live hardware-touch watch means no task.
    if let Err(e) = ensure_watch(&serial, inner) {
        return json!({
            "ok": false,
            "error": format!("watch_unavailable: {e}"),
        });
    }
    let fg = helper_rpc(&serial, helper::wrap_op("foreground", json!({})))
        .ok()
        .and_then(|v| helper::unwrap_ok(v).ok());
    let pkg = fg
        .as_ref()
        .and_then(|v| v.get("packageName"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if pkg != app {
        if let Err(e) = helper_rpc(
            &serial,
            helper::wrap_op("launch", json!({"packageName": app})),
        )
        .and_then(helper::unwrap_ok)
        {
            return json!({"ok": false, "error": format!("launch {app}: {e:#}")});
        }
        thread::sleep(Duration::from_millis(1500));
    }
    let id = format!(
        "task_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let epoch = watch_epoch(inner, &serial);
    let task = Task {
        id: id.clone(),
        goal,
        app,
        serial,
        actor: req
            .get("actor")
            .and_then(|v| v.as_str())
            .unwrap_or("agent")
            .to_string(),
        state: "waiting_actor".into(),
        wait_reason: Some("agent_decision".into()),
        step: 0,
        observation_id: None,
        takeover: false,
        app_key: None,
        app_label: None,
        app_allowed: false,
        pending_identity: None,
        pending_brief: None,
        grant: None,
        elements: json!([]),
        image_path: None,
        last_action_summary: None,
        summary: None,
        error: None,
        touch_epoch: epoch,
    };
    inner.lock().expect("inner").tasks.insert(id.clone(), task);
    json!({"ok": true, "data": {"task_id": id, "state": "waiting_actor"}})
}

fn capture(task: &mut Task) -> Result<()> {
    let dump = helper_rpc(&task.serial, helper::wrap_op("dump", json!({})))
        .and_then(helper::unwrap_ok)?;
    // Opaque `<sessionId>:<generation>` token (plan §4): carried verbatim.
    let token = dump
        .get("observationId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let Some(token) = token else {
        bail!("helper dump returned no observationId");
    };
    task.observation_id = Some(token);
    task.elements = dump.get("elements").cloned().unwrap_or(json!([]));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("lau-{}-{}.png", nanos, &task.id[5..task.id.len().min(16)]));
    let bytes = run_adb_bytes(Some(&task.serial), &["exec-out", "screencap", "-p"])?;
    if bytes.len() >= 8 && &bytes[..4] == b"\x89PNG" {
        let _ = fs::write(&path, &bytes);
        task.image_path = Some(path.display().to_string());
    }
    Ok(())
}

fn task_view(t: &Task) -> Value {
    json!({
        "task_id": t.id,
        "goal": t.goal,
        "state": t.state,
        "wait_reason": t.wait_reason,
        "takeover": t.takeover,
        "app_key": t.app_key,
        "app_label": t.app_label,
        "app_allowed": t.app_allowed,
        "actor": t.actor,
        "app": t.app,
        "serial": t.serial,
        "step": t.step,
        "observation_id": t.observation_id,
        "last_action_summary": t.last_action_summary,
        "summary": t.summary,
        "error": t.error,
    })
}

fn decide_view(t: &Task) -> Value {
    json!({
        "task_id": t.id,
        "goal": t.goal,
        "step": t.step,
        "observation_id": t.observation_id,
        "target": {
            "app_id": t.app,
            "package": t.app,
        },
        "elements": t.elements,
        "image_path": t.image_path,
        "last_action_summary": t.last_action_summary,
        "ui_state": "captured",
    })
}

fn op_decide(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    // App access comes first: without it nothing about the package is touched.
    if let Some(resp) = ensure_app_access(id, inner) {
        return resp;
    }
    let mut g = inner.lock().expect("inner");
    let serial = match g.tasks.get(id) {
        Some(t) => t.serial.clone(),
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    let watch = watch_state(&g, &serial);
    let t = match g.tasks.get_mut(id) {
        Some(t) => t,
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    if t.state == "paused" {
        return json!({"ok": false, "error": "task is paused", "data": task_view(t)});
    }
    if t.state == "succeeded" || t.state == "failed" || t.state == "cancelled" {
        return json!({"ok": false, "error": format!("task is {}", t.state), "data": task_view(t)});
    }
    if let Some(err) = watch_gate(t, &watch) {
        return json!({"ok": false, "error": err, "data": task_view(t)});
    }
    if let Err(e) = capture(t) {
        t.state = "failed".into();
        t.error = Some(format!("{e:#}"));
        return json!({"ok": false, "error": format!("{e:#}")});
    }
    t.state = "waiting_actor".into();
    t.wait_reason = Some("agent_decision".into());
    json!({"ok": true, "data": decide_view(t)})
}

fn op_act(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    // App access comes first: without it nothing about the package is touched.
    if let Some(resp) = ensure_app_access(id, inner) {
        return resp;
    }
    let obs = req.get("observation_id").and_then(|v| v.as_str()).unwrap_or("");
    let action_v = req.get("action").cloned().unwrap_or(json!({}));
    let action: Action = match serde_json::from_value(action_v) {
        Ok(a) => a,
        Err(e) => return json!({"ok": false, "error": format!("invalid action: {e}")}),
    };
    let effect: Option<EffectClaim> = req
        .get("effect")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok());
    let mut g = inner.lock().expect("inner");
    let watch_serial = match g.tasks.get(id) {
        Some(t) => t.serial.clone(),
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    let watch = watch_state(&g, &watch_serial);
    let t = match g.tasks.get_mut(id) {
        Some(t) => t,
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    if t.state == "paused" {
        return json!({"ok": false, "error": "task is paused", "data": task_view(t)});
    }
    if t.observation_id.as_deref() != Some(obs) {
        return json!({"ok": false, "error": "stale observation_id", "data": task_view(t)});
    }
    if let Some(err) = watch_gate(t, &watch) {
        return json!({"ok": false, "error": err, "data": task_view(t)});
    }
    // The token is opaque (`<sessionId>:<generation>`, plan §4): carry it, never
    // parse or rebuild it.
    let token = match t.observation_id.clone() {
        Some(token) => token,
        None => return json!({"ok": false, "error": "no observation bound to this task"}),
    };
    match &action {
        Action::Done { summary } => {
            t.summary = Some(summary.clone());
            t.last_action_summary = Some(summary.clone());
            if let Err(e) = capture(t) {
                t.state = "failed".into();
                t.error = Some(format!("re-observe after done: {e:#}"));
                return json!({"ok": false, "error": t.error.clone()});
            }
            t.state = "succeeded".into();
            t.wait_reason = None;
            return json!({"ok": true, "data": task_view(t)});
        }
        Action::Fail { reason } => {
            t.state = "failed".into();
            t.error = Some(reason.clone());
            t.wait_reason = None;
            return json!({"ok": true, "data": task_view(t)});
        }
        Action::Semantic(_) | Action::Targeted(_) => {
            // Runtime-side evidence floor (plan §5.6): the Actor's claim can only
            // raise it, never lower it.
            let judged = crate::evidence::judge(
                t.elements.as_array().map(|v| v.as_slice()).unwrap_or(&[]),
                &action,
                effect.as_ref(),
            );
            if judged.unknown {
                t.state = "waiting_actor".into();
                t.wait_reason = Some("consequence".into());
                t.error = Some(format!(
                    "actor cannot classify consequence: {}",
                    judged.rationale
                ));
                let view = task_view(t);
                return json!({
                    "ok": false,
                    "error": "waiting_user",
                    "wait_reason": "actor_cannot_classify",
                    "data": view
                });
            }
            if judged.risk.requires_user_takeover() {
                // R4 → human takeover. The proposal is discarded, never replayed.
                t.state = "waiting_actor".into();
                t.wait_reason = Some("takeover".into());
                t.takeover = true;
                t.pending_identity = None;
                t.pending_brief = None;
                let body = format!(
                    "LAU judged this action R4: {}\n\nApp: {}\nAction: {}",
                    judged.rationale,
                    t.app,
                    action_brief(&action)
                );
                let view = task_view(t);
                let tid = t.id.clone();
                drop(g);
                spawn_mac_takeover(inner.clone(), tid, body);
                return json!({
                    "ok": false,
                    "error": "waiting_user",
                    "wait_reason": "takeover",
                    "data": view
                });
            }
            if judged.risk.requires_per_action_approval() {
                let kind = effect
                    .as_ref()
                    .map(|e| e.kind)
                    .unwrap_or(EffectKind::Unknown);
                let identity = consequence_identity(&t.app, &action, effect.as_ref());
                // Plan §5.4 / D9: an approval is a one-time grant for exactly this
                // consequence, consumed by a *fresh* matching proposal. The parked
                // proposal is never stored for replay.
                let approved = match t.grant.take() {
                    Some(g) if g.identity == identity && g.expires > Instant::now() => true,
                    _ => false,
                };
                if !approved {
                    t.state = "waiting_actor".into();
                    t.wait_reason = Some("consequence".into());
                    t.pending_identity = Some(identity);
                    t.pending_brief = Some(action_brief(&action));
                    let summary = effect
                        .as_ref()
                        .and_then(|e| e.summary.clone())
                        .unwrap_or_else(|| format!("{kind:?}"));
                    let body = format!(
                        "LAU wants to run a {} action on {}\n\n{}\n\nRisk: {}\n\nAllow? (Mac dialog — not the phone)",
                        format!("{kind:?}").to_lowercase(),
                        t.app,
                        summary,
                        judged.rationale
                    );
                    let view = task_view(t);
                    let tid = t.id.clone();
                    drop(g);
                    spawn_mac_approval(inner.clone(), tid, body);
                    return json!({
                        "ok": false,
                        "error": "waiting_user",
                        "wait_reason": "consequence",
                        "data": view
                    });
                }
                t.last_action_summary =
                    Some("executing the approved consequence (one-time grant)".into());
            }
        }
        _ => {}
    }
    let serial = t.serial.clone();
    let extra = match &action {
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            json!({"elementId": element_id, "observationId": &token})
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            json!({"elementId": element_id, "observationId": &token, "text": value})
        }
        Action::Semantic(SemanticAction::Scroll {
            element_id,
            delta_x,
            delta_y,
        }) => {
            let eid = match element_id {
                Some(id) => id,
                None => {
                    return json!({"ok": false, "error": "scroll requires element_id"});
                }
            };
            json!({"elementId": eid, "observationId": &token, "dx": delta_x, "dy": delta_y})
        }
        Action::Semantic(SemanticAction::Focus { element_id }) => {
            json!({"elementId": element_id, "observationId": &token})
        }
        Action::Semantic(SemanticAction::Navigate { .. }) => {
            return json!({"ok": false, "error": "navigate is Chrome-only; not a lau action"});
        }
        Action::Targeted(_) => {
            return json!({
                "ok": false,
                "error": "semantic_action_required: use invoke/set_value/scroll, not coordinate input"
            });
        }
        Action::Observe | Action::Wait { .. } | Action::RequestUser { .. } => {
            t.step += 1;
            t.last_action_summary = Some("observe/wait".into());
            return json!({"ok": true, "data": task_view(t)});
        }
        Action::Done { .. } | Action::Fail { .. } => unreachable!(),
    };
    let op = match &action {
        Action::Semantic(SemanticAction::Invoke { .. }) => "invoke",
        Action::Semantic(SemanticAction::SetValue { .. }) => "set_value",
        Action::Semantic(SemanticAction::Scroll { .. }) => "scroll",
        Action::Semantic(SemanticAction::Focus { .. }) => "invoke",
        _ => "invoke",
    };
    drop(g);
    match helper_rpc(&serial, helper::wrap_op(op, extra)).and_then(helper::unwrap_ok) {
        Ok(_) => {
            let mut g = inner.lock().expect("inner");
            let epoch = watch_state(&g, &serial).epoch;
            if let Some(t) = g.tasks.get_mut(id) {
                t.step += 1;
                t.last_action_summary = Some(format!("{op} ok"));
                t.state = "waiting_actor".into();
                t.wait_reason = Some("agent_decision".into());
                t.touch_epoch = epoch;
                return json!({"ok": true, "data": task_view(t)});
            }
            json!({"ok": false, "error": "task disappeared"})
        }
        Err(e) => {
            let mut g = inner.lock().expect("inner");
            if let Some(t) = g.tasks.get_mut(id) {
                t.last_action_summary = Some(format!("{e:#}"));
            }
            json!({"ok": false, "error": format!("{e:#}")})
        }
    }
}

fn op_status(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let g = inner.lock().expect("inner");
    match g.tasks.get(id) {
        Some(t) => {
            let mut view = task_view(t);
            let watch = watch_state(&g, &t.serial);
            if let Some(obj) = view.as_object_mut() {
                obj.insert(
                    "watch".into(),
                    json!({"healthy": watch.healthy, "dead_reason": watch.dead_reason}),
                );
            }
            json!({"ok": true, "data": view})
        }
        None => json!({"ok": false, "error": format!("unknown task {id}")}),
    }
}

fn op_result(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    op_status(req, inner)
}

/// Persisted app-access decisions (plan §5.4). Read/revoke only — the CLI has no
/// approve path.
fn op_permissions_list(inner: &Arc<Mutex<Inner>>) -> Value {
    let g = inner.lock().expect("inner");
    let mut list: Vec<Value> = g
        .permissions
        .iter()
        .map(|(key, entry)| {
            json!({
                "key": key,
                "label": entry.label,
                "decided_at": entry.decided_at,
            })
        })
        .collect();
    list.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    json!({"ok": true, "data": {"permissions": list}})
}

fn op_permissions_revoke(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let key = req.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() {
        return json!({"ok": false, "error": "key is required"});
    }
    let mut g = inner.lock().expect("inner");
    let removed = g.permissions.remove(key).is_some();
    if removed {
        save_permissions(&g.permissions);
    }
    json!({"ok": true, "data": {"revoked": removed, "key": key}})
}

fn op_cancel(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let mut g = inner.lock().expect("inner");
    match g.tasks.get_mut(id) {
        Some(t) => {
            t.state = "cancelled".into();
            t.wait_reason = None;
            t.pending_identity = None;
            t.pending_brief = None;
            t.grant = None;
            json!({"ok": true, "data": task_view(t)})
        }
        None => json!({"ok": false, "error": format!("unknown task {id}")}),
    }
}

/// Clear a pause caused by takeover or a lost touch watch. Fail closed: the
/// device watch must be live again, and the pre-pause observation is dropped so
/// the next `act` cannot bind to a stale frame.
fn op_resume(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let serial = {
        let g = inner.lock().expect("inner");
        match g.tasks.get(id) {
            Some(t) if t.state == "paused" => t.serial.clone(),
            Some(t) => {
                return json!({
                    "ok": false,
                    "error": format!("task is {}", t.state),
                    "data": task_view(t),
                })
            }
            None => return json!({"ok": false, "error": format!("unknown task {id}")}),
        }
    };
    if let Err(e) = ensure_watch(&serial, inner) {
        return json!({"ok": false, "error": format!("watch_unavailable: {e}")});
    }
    let mut g = inner.lock().expect("inner");
    let epoch = watch_state(&g, &serial).epoch;
    let t = match g.tasks.get_mut(id) {
        Some(t) => t,
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    t.state = "waiting_actor".into();
    t.wait_reason = Some("agent_decision".into());
    t.touch_epoch = epoch;
    // Resume never continues a stored action or a pre-pause frame.
    t.observation_id = None;
    t.pending_identity = None;
    t.pending_brief = None;
    t.grant = None;
    t.takeover = false;
    t.error = None;
    t.last_action_summary = Some("resumed; re-observe with the next decide".into());
    json!({"ok": true, "data": task_view(t)})
}

fn op_approve(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let g = inner.lock().expect("inner");
    let t = match g.tasks.get(id) {
        Some(t) => t,
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    if t.takeover {
        return json!({
            "ok": false,
            "error": "takeover in progress — do it on the phone; the Mac dialog is the only control"
        });
    }
    if t.wait_reason.as_deref() != Some("consequence") || t.pending_identity.is_none() {
        return json!({"ok": false, "error": "no pending consequence grant"});
    }
    let brief = t
        .pending_brief
        .clone()
        .unwrap_or_else(|| "pending action".into());
    let body = format!(
        "LAU pending on {}\n\n{}\n\nAllow? (Mac dialog — not the phone)",
        t.app, brief
    );
    let tid = t.id.clone();
    drop(g);
    spawn_mac_approval(inner.clone(), tid, body);
    json!({"ok": true, "data": {"opened": "mac_dialog"}})
}

/// Consequence gate (plan §5.4 / D9). Approving produces a one-time grant for
/// exactly that consequence and forces a fresh observation; the parked proposal
/// is **never** executed here.
fn spawn_mac_approval(inner: Arc<Mutex<Inner>>, task_id: String, body: String) {
    thread::spawn(move || {
        let allowed = mac_dialog_allow(&body);
        let mut g = match inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(t) = g.tasks.get_mut(&task_id) else {
            return;
        };
        if t.wait_reason.as_deref() != Some("consequence") {
            return;
        }
        if !allowed {
            t.state = "failed".into();
            t.error = Some("user denied on Mac dialog".into());
            t.wait_reason = None;
            t.pending_identity = None;
            t.pending_brief = None;
            return;
        }
        let identity = match t.pending_identity.take() {
            Some(identity) => identity,
            None => return,
        };
        t.pending_brief = None;
        t.grant = Some(Grant {
            identity,
            expires: Instant::now() + Duration::from_secs(GRANT_TTL_SECS),
        });
        t.observation_id = None;
        t.state = "waiting_actor".into();
        t.wait_reason = Some("agent_decision".into());
        t.last_action_summary =
            Some("approved; re-observe and re-propose (one-time grant)".into());
    });
}

/// App access gate (plan §5.4, D8): the first control of a package needs a human
/// decision on the Mac. Returns `Some(response)` when the caller must stop.
fn ensure_app_access(id: &str, inner: &Arc<Mutex<Inner>>) -> Option<Value> {
    let (app, serial, allowed, parked, cached, terminal) = {
        let g = inner.lock().ok()?;
        let t = g.tasks.get(id)?;
        (
            t.app.clone(),
            t.serial.clone(),
            t.app_allowed,
            t.wait_reason.as_deref() == Some("app_access"),
            t.app_key.clone(),
            t.state == "succeeded" || t.state == "failed" || t.state == "cancelled",
        )
    };
    // A terminal task must stay terminal: a denied task must never be resurrected
    // into a fresh gate by a later call (observed live on 2026-09-15).
    if terminal {
        return None;
    }
    if allowed {
        return None;
    }
    if parked {
        let g = inner.lock().ok()?;
        let t = g.tasks.get(id)?;
        return Some(json!({
            "ok": false,
            "error": "waiting_user",
            "wait_reason": "app_access",
            "data": task_view(t)
        }));
    }
    // Establish the stable identity once per task (helper RPC; no lock held).
    let key = match cached {
        Some(key) => key,
        None => {
            let identity = helper_rpc(
                &serial,
                helper::wrap_op("app_identity", json!({"packageName": app})),
            )
            .and_then(helper::unwrap_ok);
            match identity {
                Ok(v) => {
                    let cert = v.get("certSha256").and_then(|x| x.as_str()).unwrap_or("");
                    let label = v
                        .get("label")
                        .and_then(|x| x.as_str())
                        .unwrap_or(&app)
                        .to_string();
                    let key = if cert.is_empty() {
                        format!("{app}#unsigned")
                    } else {
                        format!("{app}#{cert}")
                    };
                    let mut g = inner.lock().ok()?;
                    if let Some(t) = g.tasks.get_mut(id) {
                        t.app_key = Some(key.clone());
                        t.app_label = Some(label);
                    }
                    key
                }
                Err(e) => {
                    // No identity → no control. Fail closed, and say why.
                    let mut g = inner.lock().ok()?;
                    if let Some(t) = g.tasks.get_mut(id) {
                        t.state = "failed".into();
                        t.wait_reason = None;
                        t.error = Some(format!("cannot establish app identity: {e:#}"));
                    }
                    let t = g.tasks.get(id)?;
                    return Some(json!({
                        "ok": false,
                        "error": "app_identity_unavailable",
                        "data": task_view(t)
                    }));
                }
            }
        }
    };
    let persisted = {
        let g = inner.lock().ok()?;
        g.permissions.contains_key(&key)
    };
    if persisted {
        let mut g = inner.lock().ok()?;
        if let Some(t) = g.tasks.get_mut(id) {
            t.app_allowed = true;
            t.last_action_summary = Some("app access: always_allow (persisted)".into());
        }
        return None;
    }
    let (label, view) = {
        let mut g = inner.lock().ok()?;
        let t = g.tasks.get_mut(id)?;
        t.state = "waiting_actor".into();
        t.wait_reason = Some("app_access".into());
        let label = t.app_label.clone().unwrap_or_else(|| app.clone());
        (label, task_view(t))
    };
    spawn_mac_app_access(inner.clone(), id.to_string(), app, label, key);
    Some(json!({
        "ok": false,
        "error": "waiting_user",
        "wait_reason": "app_access",
        "data": view
    }))
}

/// Ask the human for app access. `always_allow` is persisted; `allow_once` lives
/// only for this task; anything else (including a failed dialog) fails the task.
fn spawn_mac_app_access(
    inner: Arc<Mutex<Inner>>,
    task_id: String,
    app: String,
    label: String,
    key: String,
) {
    thread::spawn(move || {
        let body = format!(
            "AnythingUse wants to control:\n\n{label}  ({app})\n\nIdentity:\n{key}\n\n\
             It prefers semantic actions and never restores your previous app.\n\
             This permission does NOT authorise sending, deleting, paying or any other\n\
             consequence — those are confirmed separately, one action at a time.\n\nAllow?"
        );
        let answer = mac_dialog_app_access(&body);
        let mut g = match inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        {
            let Some(t) = g.tasks.get_mut(&task_id) else {
                return;
            };
            if t.wait_reason.as_deref() != Some("app_access") {
                return;
            }
        }
        if answer == AppAccessAnswer::Always {
            g.permissions.insert(
                key.clone(),
                PermissionEntry {
                    label: label.clone(),
                    decided_at: now_secs(),
                },
            );
            save_permissions(&g.permissions);
        }
        if let Some(t) = g.tasks.get_mut(&task_id) {
            match answer {
                AppAccessAnswer::Once | AppAccessAnswer::Always => {
                    t.app_allowed = true;
                    t.wait_reason = Some("agent_decision".into());
                    // Any proposal made before the gate is discarded.
                    t.observation_id = None;
                    t.last_action_summary = Some(
                        match answer {
                            AppAccessAnswer::Always => "app access: always_allow",
                            _ => "app access: allow_once",
                        }
                        .into(),
                    );
                }
                AppAccessAnswer::Deny => {
                    t.state = "failed".into();
                    t.error = Some("app access denied by the user".into());
                    t.wait_reason = None;
                }
            }
        }
    });
}

/// Which button the human pressed on a three-way app-access dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppAccessAnswer {
    Once,
    Always,
    Deny,
}

/// Three-button osascript dialog. Anything other than a clean "Allow once" /
/// "Always allow" answer (cancel, error, no GUI) is a deny — fail closed.
fn mac_dialog_app_access(body: &str) -> AppAccessAnswer {
    let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"try
  set r to display dialog "{escaped}" with title "AnythingUse LAU — app access" buttons {{"Deny", "Always allow", "Allow once"}} default button "Allow once" cancel button "Deny" with icon caution
  return button returned of r
on error number -128
  return "Deny"
end try"#
    );
    match Command::new("osascript").arg("-e").arg(&script).output() {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout).to_lowercase();
            if out.contains("allow once") {
                AppAccessAnswer::Once
            } else if out.contains("always allow") {
                AppAccessAnswer::Always
            } else {
                AppAccessAnswer::Deny
            }
        }
        _ => AppAccessAnswer::Deny,
    }
}

fn mac_dialog_choice(body: &str, title: &str, deny_label: &str, ok_label: &str) -> bool {
    let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"display dialog "{escaped}" buttons {{"{deny_label}", "{ok_label}"}} default button "{deny_label}" with title "{title}""#
    );
    let out = Command::new("osascript").arg("-e").arg(&script).output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).contains(ok_label)
        }
        _ => false,
    }
}

fn mac_dialog_allow(body: &str) -> bool {
    mac_dialog_choice(body, "AnythingUse LAU", "Deny", "Allow")
}

/// Short, credential-free description of a proposal for a human-facing dialog.
fn action_brief(action: &Action) -> String {
    match action {
        Action::Semantic(SemanticAction::Invoke { element_id }) => format!("invoke {element_id}"),
        Action::Semantic(SemanticAction::SetValue { element_id, .. }) => {
            format!("set_value {element_id} (value withheld)")
        }
        Action::Semantic(SemanticAction::Scroll {
            element_id,
            delta_y,
            ..
        }) => format!("scroll {element_id:?} dy={delta_y}"),
        Action::Semantic(SemanticAction::Focus { element_id }) => format!("focus {element_id}"),
        Action::Semantic(SemanticAction::Navigate { .. }) => "navigate".into(),
        Action::Targeted(_) => "coordinate input".into(),
        other => format!("{other:?}"),
    }
}

/// R4 takeover (plan §5.4): the human does the action on the phone; on Done the
/// proposal is discarded and the task re-observes. Nothing is ever replayed.
fn spawn_mac_takeover(inner: Arc<Mutex<Inner>>, task_id: String, body: String) {
    thread::spawn(move || {
        let cancelled = |inner: &Arc<Mutex<Inner>>, reason: &str| {
            if let Ok(mut g) = inner.lock() {
                if let Some(t) = g.tasks.get_mut(&task_id) {
                    t.takeover = false;
                    t.state = "failed".into();
                    t.error = Some(reason.to_string());
                    t.wait_reason = None;
                }
            }
        };
        let start = mac_dialog_choice(
            &format!(
                "{body}\n\nDo the action yourself on the phone — AnythingUse will NOT run it for you.",
            ),
            "AnythingUse LAU — start takeover",
            "Cancel",
            "Start takeover",
        );
        if !start {
            cancelled(&inner, "user cancelled the takeover");
            return;
        }
        let done = mac_dialog_choice(
            "Takeover in progress.\n\nClick Done only after you finished the action yourself on the phone.",
            "AnythingUse LAU — takeover done?",
            "Cancel",
            "Done",
        );
        if !done {
            cancelled(&inner, "user cancelled the takeover");
            return;
        }
        let mut g = match inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let serial = match g.tasks.get(&task_id) {
            Some(t) if t.wait_reason.as_deref() == Some("takeover") => t.serial.clone(),
            _ => return,
        };
        let epoch = watch_state(&g, &serial).epoch;
        if let Some(t) = g.tasks.get_mut(&task_id) {
            t.takeover = false;
            t.state = "waiting_actor".into();
            t.wait_reason = Some("agent_decision".into());
            t.pending_identity = None;
            t.pending_brief = None;
            t.observation_id = None;
            t.touch_epoch = epoch;
            t.last_action_summary = Some("takeover done by the human; re-observe".into());
        }
    });
}


/// Start (or restart) the per-device `getevent` watch and wait until it is
/// actually attached. Fail closed: while the watch is not live, no agent task is
/// accepted for that device (plan §6 — never claim coexistence without it).
fn ensure_watch(serial: &str, inner: &Arc<Mutex<Inner>>) -> std::result::Result<(), String> {
    {
        let g = inner.lock().map_err(|_| "daemon state poisoned".to_string())?;
        if let Some(w) = g.watches.get(serial) {
            if w.healthy {
                return Ok(());
            }
        }
    }
    {
        let mut g = inner.lock().map_err(|_| "daemon state poisoned".to_string())?;
        // Keep the previous epoch across a restart: other tasks on this device
        // must not read a reset counter as a takeover.
        let epoch = g.watches.get(serial).map(|w| w.epoch).unwrap_or(0);
        g.watches
            .insert(serial.to_string(), TouchWatch::starting(epoch));
    }
    spawn_getevent(serial.to_string(), Arc::clone(inner));
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(50));
        let g = match inner.lock() {
            Ok(g) => g,
            Err(_) => return Err("daemon state poisoned".into()),
        };
        match g.watches.get(serial) {
            Some(w) if w.healthy => return Ok(()),
            Some(w) if w.dead_reason.is_some() => {
                return Err(w.dead_reason.clone().unwrap_or_else(|| "watch died".into()))
            }
            _ => {}
        }
    }
    Err("getevent watch did not attach within 2s".into())
}

/// Watch the device's hardware touch stream. AnythingUse injects through the
/// helper's AccessibilityService, which never emits `getevent` frames, so a
/// frame on this stream is the user.
fn spawn_getevent(serial: String, inner: Arc<Mutex<Inner>>) {
    thread::spawn(move || {
        let child = Command::new(crate::adb_path())
            .args(["-s", &serial, "shell", "getevent", "-lt"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                if let Ok(mut g) = inner.lock() {
                    if let Some(w) = g.watches.get_mut(&serial) {
                        w.healthy = false;
                        w.dead_reason = Some(format!("getevent spawn failed: {e}"));
                    }
                }
                return;
            }
        };
        if let Ok(mut g) = inner.lock() {
            if let Some(w) = g.watches.get_mut(&serial) {
                w.healthy = true;
                w.dead_reason = None;
            }
        }
        if let Some(out) = child.stdout.take() {
            let reader = BufReader::new(out);
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("BTN_TOUCH") || line.contains("ABS_MT_TRACKING_ID") {
                    if let Ok(mut g) = inner.lock() {
                        if let Some(w) = g.watches.get_mut(&serial) {
                            w.epoch = w.epoch.saturating_add(1);
                        }
                    }
                }
            }
        }
        let _ = child.wait();
        // The stream is gone: everything on this device must stop until resume.
        if let Ok(mut g) = inner.lock() {
            if let Some(w) = g.watches.get_mut(&serial) {
                w.healthy = false;
                w.dead_reason = Some("getevent stream ended".into());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(touch_epoch: u64) -> Task {
        Task {
            id: "task_test".into(),
            goal: "open dark mode".into(),
            app: "com.android.settings".into(),
            serial: "SERIAL".into(),
            actor: "agent".into(),
            state: "waiting_actor".into(),
            wait_reason: Some("agent_decision".into()),
            step: 0,
            observation_id: Some("abcd1234:1".into()),
            pending_identity: None,
            pending_brief: None,
            grant: None,
            takeover: false,
            app_key: Some("com.android.settings#deadbeef".into()),
            app_label: Some("设置".into()),
            app_allowed: true,
            elements: json!([]),
            image_path: None,
            last_action_summary: None,
            summary: None,
            error: None,
            touch_epoch,
        }
    }

    fn watch(epoch: u64, healthy: bool, dead_reason: Option<&str>) -> WatchState {
        WatchState {
            epoch,
            healthy,
            dead_reason: dead_reason.map(str::to_string),
        }
    }

    #[test]
    fn a_grant_matches_only_the_same_consequence() {
        // Plan §5.4 / D9: an approval is bound to exactly one consequence.
        let invoke_e1 = Action::Semantic(anything_core::SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let invoke_e2 = Action::Semantic(anything_core::SemanticAction::Invoke {
            element_id: "e2".into(),
        });
        let base = consequence_identity(
            "com.android.settings",
            &invoke_e1,
            Some(&EffectClaim::new(EffectKind::ExternalSubmit, "send")),
        );
        // Same app, action and declared effect → same identity (the grant hits).
        assert_eq!(
            base,
            consequence_identity(
                "com.android.settings",
                &invoke_e1,
                Some(&EffectClaim::new(EffectKind::ExternalSubmit, "send")),
            )
        );
        // A different element, or a different declared effect, is a different
        // consequence and must be gated again.
        assert_ne!(
            base,
            consequence_identity(
                "com.android.settings",
                &invoke_e2,
                Some(&EffectClaim::new(EffectKind::ExternalSubmit, "send")),
            )
        );
        assert_ne!(
            base,
            consequence_identity(
                "com.android.settings",
                &invoke_e1,
                Some(&EffectClaim::new(EffectKind::Destructive, "send")),
            )
        );
        assert_ne!(
            base,
            consequence_identity("com.other.app", &invoke_e1, None)
        );
    }

    #[test]
    fn a_denied_task_is_not_resurrected_into_a_new_gate() {
        // Observed live: after app access was denied the task was `failed`, and a
        // later decide call parked a brand-new gate (and another dialog).
        let inner = Arc::new(Mutex::new(empty_inner()));
        {
            let mut g = inner.lock().unwrap();
            let mut t = task(1);
            t.state = "failed".into();
            t.wait_reason = None;
            t.app_allowed = false;
            g.tasks.insert(t.id.clone(), t);
        }
        assert!(ensure_app_access("task_test", &inner).is_none());
        let g = inner.lock().unwrap();
        let t = g.tasks.get("task_test").unwrap();
        assert_eq!(t.state, "failed");
        assert_eq!(t.wait_reason, None);
    }

    fn empty_inner() -> Inner {
        Inner {
            tasks: HashMap::new(),
            last_activity: Instant::now(),
            watches: HashMap::new(),
            permissions: HashMap::new(),
        }
    }

    #[test]
    fn missing_watch_pauses_instead_of_acting() {
        let mut t = task(3);
        let err = watch_gate(&mut t, &watch(3, false, Some("getevent stream ended")));
        assert_eq!(err, Some("watch_unavailable"));
        assert_eq!(t.state, "paused");
        assert_eq!(t.wait_reason.as_deref(), Some("watch_unavailable"));
        assert_eq!(t.error.as_deref(), Some("getevent stream ended"));
    }

    #[test]
    fn real_touch_since_the_baseline_pauses_as_taken_over() {
        let mut t = task(3);
        let err = watch_gate(&mut t, &watch(4, true, None));
        assert_eq!(err, Some("taken_over"));
        assert_eq!(t.state, "paused");
        assert_eq!(t.wait_reason.as_deref(), Some("taken_over"));
    }

    #[test]
    fn live_watch_at_the_same_epoch_lets_the_step_through() {
        let mut t = task(7);
        assert_eq!(watch_gate(&mut t, &watch(7, true, None)), None);
        assert_eq!(t.state, "waiting_actor");
        assert!(t.error.is_none());
    }

    #[test]
    fn unknown_device_serial_is_never_healthy() {
        let state = watch_state(&empty_inner(), "SERIAL");
        assert!(!state.healthy);
        assert!(state.reason().contains("no touch watch"));
    }

    #[test]
    fn watch_health_is_tracked_per_serial_not_globally() {
        let mut inner = empty_inner();
        inner.watches.insert(
            "A".into(),
            TouchWatch {
                epoch: 5,
                healthy: true,
                dead_reason: None,
            },
        );
        inner.watches.insert(
            "B".into(),
            TouchWatch {
                epoch: 0,
                healthy: false,
                dead_reason: Some("getevent stream ended".into()),
            },
        );
        assert!(watch_state(&inner, "A").healthy);
        assert_eq!(watch_state(&inner, "A").epoch, 5);
        // A's live watch must not make B usable.
        assert!(!watch_state(&inner, "B").healthy);
    }
}
