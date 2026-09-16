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
    /// Observation token the parked consequence action was bound to.
    pending_observation: Option<String>,
    elements: Value,
    image_path: Option<String>,
    last_action_summary: Option<String>,
    summary: Option<String>,
    error: Option<String>,
    touch_epoch: u64,
    pending_action: Option<Action>,
    pending_effect: Option<EffectClaim>,
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

struct Inner {
    tasks: HashMap<String, Task>,
    last_activity: Instant,
    /// One hardware-touch watch per device serial.
    watches: HashMap<String, TouchWatch>,
}

pub fn daemon_main() -> Result<()> {
    let path = sock_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    listener.set_nonblocking(true)?;
    let inner = Arc::new(Mutex::new(Inner {
        tasks: HashMap::new(),
        last_activity: Instant::now(),
        watches: HashMap::new(),
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
        "approve" => op_approve(&req, inner),
        _ => json!({"ok": false, "error": "unknown op"}),
    };
    stream.write_all(format!("{resp}\n").as_bytes())?;
    Ok(())
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
        pending_observation: None,
        elements: json!([]),
        image_path: None,
        last_action_summary: None,
        summary: None,
        error: None,
        touch_epoch: epoch,
        pending_action: None,
        pending_effect: None,
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
            if effect.is_none() {
                t.state = "failed".into();
                t.error = Some("executable action has no effect declaration".into());
                return json!({"ok": false, "error": t.error.clone()});
            }
            let kind = effect.as_ref().unwrap().kind;
            if matches!(
                kind,
                EffectKind::Destructive
                    | EffectKind::ExternalCommunication
                    | EffectKind::ExternalSubmit
                    | EffectKind::PermissionChange
                    | EffectKind::Financial
                    | EffectKind::Credential
                    | EffectKind::Unknown
            ) {
                t.state = "waiting_actor".into();
                t.wait_reason = Some("consequence".into());
                t.pending_action = Some(action.clone());
                t.pending_effect = effect.clone();
                t.pending_observation = Some(token.clone());
                let summary = effect
                    .as_ref()
                    .and_then(|e| e.summary.clone())
                    .unwrap_or_else(|| format!("{kind:?}"));
                let body = format!(
                    "LAU wants to run a {} action on {}\n\n{}\n\nAllow? (Mac dialog — not the phone)",
                    format!("{kind:?}").to_lowercase(),
                    t.app,
                    summary
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

fn op_cancel(req: &Value, inner: &Arc<Mutex<Inner>>) -> Value {
    let id = req.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    let mut g = inner.lock().expect("inner");
    match g.tasks.get_mut(id) {
        Some(t) => {
            t.state = "cancelled".into();
            t.wait_reason = None;
            t.pending_action = None;
            t.pending_effect = None;
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
    t.pending_action = None;
    t.pending_effect = None;
    t.pending_observation = None;
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
    if t.wait_reason.as_deref() != Some("consequence") || t.pending_action.is_none() {
        return json!({"ok": false, "error": "no pending consequence grant"});
    }
    let summary = t
        .pending_effect
        .as_ref()
        .and_then(|e| e.summary.clone())
        .unwrap_or_else(|| "pending action".into());
    let body = format!(
        "LAU pending {} on {}\n\n{}\n\nAllow? (Mac dialog — not the phone)",
        t.app,
        t.id,
        summary
    );
    let tid = t.id.clone();
    drop(g);
    spawn_mac_approval(inner.clone(), tid, body);
    json!({"ok": true, "data": {"opened": "mac_dialog"}})
}

fn spawn_mac_approval(inner: Arc<Mutex<Inner>>, task_id: String, body: String) {
    thread::spawn(move || {
        let allowed = mac_dialog_allow(&body);
        let (serial, token, action) = {
            let mut g = inner.lock().expect("inner");
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
                t.pending_action = None;
                t.pending_effect = None;
                return;
            }
            let action = match t.pending_action.take() {
                Some(a) => a,
                None => return,
            };
            t.pending_effect = None;
            let token = match t.pending_observation.take() {
                Some(token) => token,
                None => return,
            };
            (t.serial.clone(), token, action)
        };
        match helper_call(&serial, &token, &action) {
            Ok(op) => {
                let mut g = inner.lock().expect("inner");
                let epoch = watch_state(&g, &serial).epoch;
                if let Some(t) = g.tasks.get_mut(&task_id) {
                    t.step += 1;
                    t.last_action_summary = Some(format!("{op} ok (approved on Mac)"));
                    t.state = "waiting_actor".into();
                    t.wait_reason = Some("agent_decision".into());
                    t.touch_epoch = epoch;
                }
            }
            Err(e) => {
                let mut g = inner.lock().expect("inner");
                if let Some(t) = g.tasks.get_mut(&task_id) {
                    t.state = "failed".into();
                    t.error = Some(format!("{e:#}"));
                    t.wait_reason = None;
                }
            }
        }
    });
}

fn mac_dialog_allow(body: &str) -> bool {
    let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"display dialog "{escaped}" buttons {{"Deny", "Allow"}} default button "Deny" with title "AnythingUse LAU""#
    );
    let out = Command::new("osascript").arg("-e").arg(&script).output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).contains("Allow")
        }
        _ => false,
    }
}

fn helper_call(serial: &str, token: &str, action: &Action) -> Result<&'static str> {
    let (op, extra) = match action {
        Action::Semantic(SemanticAction::Invoke { element_id }) => (
            "invoke",
            json!({"elementId": element_id, "observationId": token}),
        ),
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => (
            "set_value",
            json!({"elementId": element_id, "observationId": token, "text": value}),
        ),
        Action::Semantic(SemanticAction::Scroll {
            element_id: Some(eid),
            delta_x,
            delta_y,
        }) => (
            "scroll",
            json!({"elementId": eid, "observationId": token, "dx": delta_x, "dy": delta_y}),
        ),
        Action::Semantic(SemanticAction::Focus { element_id }) => (
            "invoke",
            json!({"elementId": element_id, "observationId": token}),
        ),
        _ => bail!("action cannot be approved for helper execution"),
    };
    helper_rpc(serial, helper::wrap_op(op, extra)).and_then(helper::unwrap_ok)?;
    Ok(op)
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
            pending_observation: None,
            elements: json!([]),
            image_path: None,
            last_action_summary: None,
            summary: None,
            error: None,
            touch_epoch,
            pending_action: None,
            pending_effect: None,
        }
    }

    fn watch(epoch: u64, healthy: bool, dead_reason: Option<&str>) -> WatchState {
        WatchState {
            epoch,
            healthy,
            dead_reason: dead_reason.map(str::to_string),
        }
    }

    fn empty_inner() -> Inner {
        Inner {
            tasks: HashMap::new(),
            last_activity: Instant::now(),
            watches: HashMap::new(),
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
