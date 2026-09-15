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
    helper_generation: i64,
    elements: Value,
    image_path: Option<String>,
    last_action_summary: Option<String>,
    summary: Option<String>,
    error: Option<String>,
    touch_epoch: u64,
    pending_action: Option<Action>,
    pending_effect: Option<EffectClaim>,
}

struct Inner {
    tasks: HashMap<String, Task>,
    last_activity: Instant,
    touch_epoch: u64,
    getevent_ok: bool,
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
        touch_epoch: 0,
        getevent_ok: false,
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
    start_getevent(serial.clone(), inner);
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
    let epoch = inner.lock().map(|g| g.touch_epoch).unwrap_or(0);
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
        helper_generation: 0,
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
    let gen = dump.get("observationId").and_then(|v| v.as_i64()).unwrap_or(0);
    task.helper_generation = gen;
    task.observation_id = Some(format!("obs_{gen}"));
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
    let epoch = g.touch_epoch;
    let t = match g.tasks.get_mut(id) {
        Some(t) => t,
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    if t.state == "paused" {
        return json!({"ok": false, "error": "task is paused", "data": task_view(t)});
    }
    if t.state == "succeeded" || t.state == "failed" || t.state == "cancelled" {
        return json!({"ok": false, "error": format!("task is {}", t.state)});
    }
    if t.touch_epoch != epoch {
        t.state = "paused".into();
        t.wait_reason = Some("taken_over".into());
        t.last_action_summary = Some("paused: real touch on device".into());
        return json!({"ok": false, "error": "taken_over", "data": task_view(t)});
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
    let epoch = g.touch_epoch;
    let t = match g.tasks.get_mut(id) {
        Some(t) => t,
        None => return json!({"ok": false, "error": format!("unknown task {id}")}),
    };
    if t.state == "paused" {
        return json!({"ok": false, "error": "task is paused"});
    }
    if t.observation_id.as_deref() != Some(obs) {
        return json!({"ok": false, "error": "stale observation_id"});
    }
    if t.touch_epoch != epoch {
        t.state = "paused".into();
        t.wait_reason = Some("taken_over".into());
        return json!({"ok": false, "error": "taken_over"});
    }
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
    let gen = t.helper_generation;
    let serial = t.serial.clone();
    let extra = match &action {
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            json!({"elementId": element_id, "observationId": gen})
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            json!({"elementId": element_id, "observationId": gen, "text": value})
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
            json!({"elementId": eid, "observationId": gen, "dx": delta_x, "dy": delta_y})
        }
        Action::Semantic(SemanticAction::Focus { element_id }) => {
            json!({"elementId": element_id, "observationId": gen})
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
            let epoch = g.touch_epoch;
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
        Some(t) => json!({"ok": true, "data": task_view(t)}),
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
        let (serial, gen, action) = {
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
            (t.serial.clone(), t.helper_generation, action)
        };
        match helper_call(&serial, gen, &action) {
            Ok(op) => {
                let mut g = inner.lock().expect("inner");
                let epoch = g.touch_epoch;
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

fn helper_call(serial: &str, gen: i64, action: &Action) -> Result<&'static str> {
    let (op, extra) = match action {
        Action::Semantic(SemanticAction::Invoke { element_id }) => (
            "invoke",
            json!({"elementId": element_id, "observationId": gen}),
        ),
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => (
            "set_value",
            json!({"elementId": element_id, "observationId": gen, "text": value}),
        ),
        Action::Semantic(SemanticAction::Scroll {
            element_id: Some(eid),
            delta_x,
            delta_y,
        }) => (
            "scroll",
            json!({"elementId": eid, "observationId": gen, "dx": delta_x, "dy": delta_y}),
        ),
        Action::Semantic(SemanticAction::Focus { element_id }) => (
            "invoke",
            json!({"elementId": element_id, "observationId": gen}),
        ),
        _ => bail!("action cannot be approved for helper execution"),
    };
    helper_rpc(serial, helper::wrap_op(op, extra)).and_then(helper::unwrap_ok)?;
    Ok(op)
}

fn start_getevent(serial: String, inner: &Arc<Mutex<Inner>>) {
    let watch = inner.clone();
    thread::spawn(move || {
        let adb = match std::env::var("LAU_ADB_BIN") {
            Ok(p) if !p.is_empty() => p,
            _ => "adb".into(),
        };
        let mut child = match Command::new(adb)
            .args(["-s", &serial, "shell", "getevent", "-lt"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => {
                if let Ok(mut g) = watch.lock() {
                    g.getevent_ok = false;
                }
                return;
            }
        };
        if let Ok(mut g) = watch.lock() {
            g.getevent_ok = true;
        }
        if let Some(out) = child.stdout.take() {
            let reader = BufReader::new(out);
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("BTN_TOUCH") || line.contains("ABS_MT_TRACKING_ID") {
                    if let Ok(mut g) = watch.lock() {
                        g.touch_epoch = g.touch_epoch.saturating_add(1);
                    }
                }
            }
        }
        if let Ok(mut g) = watch.lock() {
            g.getevent_ok = false;
        }
    });
}
