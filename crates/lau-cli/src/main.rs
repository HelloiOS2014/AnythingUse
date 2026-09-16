//! `lau` — AnythingUse / Local Android Use.
//!
//! Device connectivity (`doctor`) and observation (`screenshot`) over ADB;
//! semantic `dump`/`invoke`/`set_value`/`scroll` through the on-device
//! AccessibilityService helper; agent task loop (`run`/`decide`/`act`) through
//! the on-demand daemon. ADB is transport/observation only — never input
//! injection. Status and known gaps: `docs/lau-android-plan.md` §0.

mod daemon;
mod helper;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXIT_USAGE: i32 = 64;
const EXIT_ADB_UNAVAILABLE: i32 = 69;
const EXIT_INTERNAL: i32 = 70;

#[derive(Parser)]
#[command(
    name = "lau",
    about = "AnythingUse — Local Android Use (lau): control surface for a connected Android device",
    version
)]
struct Cli {
    /// Target device serial (defaults to the single authorized device)
    #[arg(long, global = true, env = "LAU_SERIAL")]
    serial: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check ADB, connected device, and authorization
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Capture a PNG screenshot from the device
    Screenshot {
        /// Output file (defaults to a new file under the system temp dir)
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Dump the current accessibility tree (helper required)
    Dump {
        #[arg(long)]
        json: bool,
    },
    /// Semantic click on a dumped element
    Invoke {
        element_id: String,
        /// Opaque observation token from `lau dump` (`<sessionId>:<generation>`)
        #[arg(long)]
        observation_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Semantic set-text on a dumped element (unicode / CJK)
    SetValue {
        element_id: String,
        value: String,
        /// Opaque observation token from `lau dump` (`<sessionId>:<generation>`)
        #[arg(long)]
        observation_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Semantic scroll. dy>0 forward/down, dy<0 backward/up
    Scroll {
        element_id: String,
        /// Opaque observation token from `lau dump` (`<sessionId>:<generation>`)
        #[arg(long)]
        observation_id: String,
        #[arg(long, default_value_t = 0.0)]
        dx: f64,
        #[arg(long, default_value_t = 1.0)]
        dy: f64,
        #[arg(long)]
        json: bool,
    },
    /// Report the current foreground package and screen state
    Foreground {
        #[arg(long)]
        json: bool,
    },
    /// Launch an app via the helper (not adb am start)
    Launch {
        package: String,
        #[arg(long)]
        json: bool,
    },
    /// Submit an agent task (starts the on-demand daemon)
    Run {
        goal: String,
        #[arg(long)]
        app: String,
        #[arg(long, default_value = "agent")]
        actor: String,
        #[arg(long)]
        json: bool,
    },
    /// Compact observation for an agent task (`--wait` polls until one is available)
    Decide {
        task_id: String,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// Submit an observation-bound action
    Act {
        task_id: String,
        #[arg(long)]
        observation_id: String,
        #[arg(long)]
        action: String,
        #[arg(long)]
        effect: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Status {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    Result {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    Cancel {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Resume a task paused by takeover or a lost touch watch
    Resume {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Open the Mac approval dialog for a parked consequence (never auto-approves)
    Approve {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Internal: on-demand daemon (do not invoke by hand)
    #[command(hide = true)]
    Daemon,
}

pub(crate) struct Device {
    serial: String,
    state: String,
    props: Vec<String>,
}

impl Device {
    fn prop(&self, key: &str) -> Option<&str> {
        self.props
            .iter()
            .find_map(|p| p.strip_prefix(key).and_then(|v| v.strip_prefix(':'))
        )
    }
}

/// ADB binary used by the CLI and by the daemon's `getevent` watch.
pub(crate) fn adb_path() -> PathBuf {
    match std::env::var("LAU_ADB_BIN") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => PathBuf::from("adb"),
    }
}

pub(crate) fn run_adb(serial: Option<&str>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new(adb_path());
    if let Some(s) = serial {
        cmd.args(["-s", s]);
    }
    cmd.args(args);
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn {} — is ADB installed?", adb_path().display()))?;
    if !out.status.success() {
        bail!(
            "adb {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(crate) fn run_adb_bytes(serial: Option<&str>, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new(adb_path());
    if let Some(s) = serial {
        cmd.args(["-s", s]);
    }
    cmd.args(args);
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn {} — is ADB installed?", adb_path().display()))?;
    if !out.status.success() {
        bail!(
            "adb {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

pub(crate) fn parse_devices(raw: &str) -> Vec<Device> {
    raw.lines()
        .skip_while(|l| !l.contains("List of devices"))
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let props = parts.map(|s| s.to_string()).collect();
            Some(Device { serial, state, props })
        })
        .collect()
}

fn authorized_devices(devices: &[Device]) -> Vec<&Device> {
    devices
        .iter()
        .filter(|d| d.state == "device")
        .collect()
}

pub(crate) enum TargetResolution {
    Resolved(String),
    Ambiguous(String),
    Unavailable(String),
}

/// Resolve the target serial. Ambiguous/Unavailable carry a user-facing message
/// that must become a doctor blocker (never silently swallowed).
pub(crate) fn resolve_target(cli_serial: Option<&str>, devices: &[Device]) -> TargetResolution {
    if let Some(s) = cli_serial {
        return match devices.iter().find(|d| d.serial == s) {
            Some(d) if d.state == "device" => TargetResolution::Resolved(s.to_string()),
            Some(d) => TargetResolution::Unavailable(format!(
                "device {} is {} — check the phone (USB debugging prompt / cable)",
                s, d.state
            )),
            None => TargetResolution::Ambiguous(format!(
                "serial {} not found among connected devices",
                s
            )),
        };
    }
    let authorized = authorized_devices(devices);
    match authorized.len() {
        0 => {
            let mut msg = String::from("no authorized device connected");
            for d in devices.iter().filter(|d| d.state == "unauthorized") {
                msg.push_str(&format!(
                    "; device {} is unauthorized — accept the USB debugging prompt on the phone",
                    d.serial
                ));
            }
            TargetResolution::Unavailable(msg)
        }
        1 => TargetResolution::Resolved(authorized[0].serial.clone()),
        _ => TargetResolution::Ambiguous(
            "multiple authorized devices; pass --serial (or LAU_SERIAL)".into(),
        ),
    }
}

fn shell_prop(serial: &str, prop: &str) -> Result<String> {
    let raw = run_adb(Some(serial), &["shell", "getprop", prop])?;
    Ok(raw.trim().trim_matches('\r').to_string())
}

fn ensure_forward(serial: &str, port: u16) -> Result<()> {
    let spec = format!("tcp:{port}");
    let dest = format!("localabstract:{}", helper::SOCKET_NAME);
    run_adb(Some(serial), &["forward", &spec, &dest])?;
    Ok(())
}

pub(crate) fn helper_rpc(serial: &str, req: Value) -> Result<Value> {
    helper::rpc(serial, |s, p| ensure_forward(s, p), req)
}

fn helper_status(serial: &str) -> (Value, Vec<String>) {
    let installed = run_adb(Some(serial), &["shell", "pm", "path", helper::PACKAGE])
        .map(|s| s.contains(helper::PACKAGE))
        .unwrap_or(false);
    let enabled_raw = run_adb(
        Some(serial),
        &[
            "shell",
            "settings",
            "get",
            "secure",
            "enabled_accessibility_services",
        ],
    )
    .unwrap_or_default();
    let enabled =
        enabled_raw.contains(helper::PACKAGE) || enabled_raw.contains(helper::SERVICE);
    let dumpsys = run_adb(Some(serial), &["shell", "dumpsys", "accessibility"]).unwrap_or_default();
    let bound = bound_from_dumpsys(&dumpsys);
    let ping = helper_rpc(serial, helper::wrap_op("ping", json!({})))
        .ok()
        .and_then(|v| helper::unwrap_ok(v).ok())
        .is_some();
    let mut blockers = Vec::new();
    if !installed {
        blockers.push("helper APK not installed — run scripts/install-android-helper.sh".into());
    } else if !enabled {
        blockers.push(
            "helper AccessibilityService not enabled — Settings → Accessibility → AnythingUse LAU"
                .into(),
        );
    } else if !ping {
        blockers.push("helper socket not responding — toggle AnythingUse LAU off/on".into());
    }
    // Plan §5.5: `enabled` gates; `bound`/`ping` are diagnostics. A live socket
    // with the service disabled is a stale instance, not health.
    let mut notes: Vec<String> = Vec::new();
    if !enabled && ping {
        notes.push(
            "helper socket still answers while the service is disabled (stale instance) — \
             `enabled` is authoritative"
                .into(),
        );
    }
    if enabled && !bound {
        notes.push(
            "service is enabled but does not appear in `dumpsys accessibility` Bound services \
             (may still be binding)"
                .into(),
        );
    }
    let value = json!({
        "installed": installed,
        "enabled": enabled,
        "bound": bound,
        "ping": ping,
        "notes": notes,
        "blockers": blockers,
    });
    (value, blockers)
}

/// `bound` must come from the `Bound services:` block only: a whole-dumpsys
/// substring match also hits the accessibility *button* entry, which survives
/// the service being disabled (finding #20). The block is multi-line and lists
/// services by `android:label`, so both the component id and the label count.
fn bound_from_dumpsys(dumpsys: &str) -> bool {
    let mut in_block = false;
    for line in dumpsys.lines() {
        let t = line.trim_start();
        if t.starts_with("Bound services:") {
            in_block = true;
        } else if in_block
            && (t.starts_with("Enabled services:") || t.starts_with("Binding services:"))
        {
            in_block = false;
        }
        if in_block && (t.contains(helper::PACKAGE) || t.contains(HELPER_SERVICE_LABEL)) {
            return true;
        }
    }
    false
}

/// The service's `android:label` (see `native/android-helper/.../strings.xml`);
/// `dumpsys accessibility` prints bound services by label, not by component.
const HELPER_SERVICE_LABEL: &str = "AnythingUse LAU";

fn doctor(json: bool, cli_serial: Option<&str>) -> Result<i32> {
    let version_raw = match run_adb(None, &["version"]) {
        Ok(v) => v,
        Err(_) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "status": "adb_unavailable",
                        "error": format!("adb binary not found at {}", adb_path().display()),
                    })
                );
            } else {
                eprintln!("adb not found at {} — set LAU_ADB_BIN or install platform-tools", adb_path().display());
            }
            return Ok(EXIT_ADB_UNAVAILABLE);
        }
    };
    let adb_version = version_raw
        .lines()
        .find(|l| l.starts_with("Android Debug Bridge version"))
        .map(|l| l.trim_start_matches("Android Debug Bridge version ").trim().to_string())
        .unwrap_or_default();

    let devices_raw = run_adb(None, &["devices", "-l"])?;
    let devices = parse_devices(&devices_raw);

    let (mut blockers, mut status_name) = (Vec::<String>::new(), "ok");
    let target = match resolve_target(cli_serial, &devices) {
        TargetResolution::Resolved(s) => Some(s),
        TargetResolution::Ambiguous(msg) => {
            blockers.push(msg);
            status_name = "target_unresolved";
            None
        }
        TargetResolution::Unavailable(msg) => {
            blockers.push(msg);
            status_name = "device_unavailable";
            None
        }
    };
    let (android_version, model) = match &target {
        Some(s) => (
            shell_prop(s, "ro.build.version.release").ok(),
            shell_prop(s, "ro.product.model").ok(),
        ),
        None => (None, None),
    };
    let helper_info = match &target {
        Some(s) => {
            let (value, extra) = helper_status(s);
            blockers.extend(extra);
            if status_name == "ok" && !blockers.is_empty() {
                status_name = "helper_unavailable";
            }
            value
        }
        None => json!(null),
    };

    if json {
        println!(
            "{}",
            json!({
                "status": status_name,
                "data": {
                    "adb": {
                        "path": adb_path().display().to_string(),
                        "version": adb_version,
                    },
                    "devices": devices.iter().map(|d| json!({
                        "serial": d.serial,
                        "state": d.state,
                        "model": d.prop("model"),
                        "product": d.prop("product"),
                        "transport": d.prop("transport_id"),
                    })).collect::<Vec<_>>(),
                    "target": target,
                    "android_version": android_version,
                    "model": model,
                    "helper": helper_info,
                    "blockers": blockers,
                }
            })
        );
    } else {
        println!("adb {} ({})", adb_version, adb_path().display());
        for d in &devices {
            println!("device {} [{}] model={}", d.serial, d.state, d.prop("model").unwrap_or("?"));
        }
        if let Some(t) = &target {
            println!("target {} — Android {} ({})", t, android_version.clone().unwrap_or_default(), model.clone().unwrap_or_default());
        }
        println!("helper {}", helper_info);
        for b in &blockers {
            eprintln!("blocker: {}", b);
        }
    }
    Ok(if blockers.is_empty() { 0 } else { 3 })
}

fn screenshot(json: bool, out: Option<PathBuf>, cli_serial: Option<&str>) -> Result<i32> {
    let devices_raw = run_adb(None, &["devices", "-l"])?;
    let devices = parse_devices(&devices_raw);
    let resolution = resolve_target(cli_serial, &devices);
    let serial = match resolution {
        TargetResolution::Resolved(s) => s,
        TargetResolution::Ambiguous(msg) => {
            if json {
                println!("{}", json!({"status": "target_unresolved", "error": msg}));
            } else {
                eprintln!("lau: {}", msg);
            }
            return Ok(3);
        }
        TargetResolution::Unavailable(msg) => {
            if json {
                println!("{}", json!({"status": "device_unavailable", "error": msg}));
            } else {
                eprintln!("lau: {}", msg);
            }
            return Ok(3);
        }
    };

    // Plan §5.3: never hand back a lock-screen frame as if it were a live view.
    // `foreground` answers in every screen state and never wakes the device.
    let state = match helper_rpc(&serial, helper::wrap_op("foreground", json!({})))
        .and_then(helper::unwrap_ok)
    {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("{e:#}");
            if json {
                println!("{}", json!({"status": "error", "error": msg}));
            } else {
                eprintln!("lau: {msg}");
            }
            return Ok(3);
        }
    };
    let is_interactive = state
        .get("isInteractive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let keyguard_locked = state
        .get("keyguardLocked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    for (blocked, code) in [
        (!is_interactive, "screen_off: display is not interactive"),
        (keyguard_locked, "device_locked: device is locked"),
    ] {
        if blocked {
            if json {
                println!("{}", json!({"status": "error", "error": code}));
            } else {
                eprintln!("lau: {code}");
            }
            return Ok(3);
        }
    }

    let bytes = run_adb_bytes(Some(&serial), &["exec-out", "screencap", "-p"])?;
    if bytes.is_empty() || bytes.len() < 8 || &bytes[..4] != b"\x89PNG" {
        if json {
            println!("{}", json!({"status": "error", "error": "screencap returned no PNG"}));
        } else {
            eprintln!("screencap returned no PNG");
        }
        return Ok(EXIT_INTERNAL);
    }

    let path = out.unwrap_or_else(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("lau-{}-{}.png", nanos, &serial[..serial.len().min(6)]))
    });
    std::fs::write(&path, &bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;

    let sha = hex::encode(Sha256::digest(&bytes));
    if json {
        println!(
            "{}",
            json!({
                "status": "ok",
                "data": {
                    "serial": serial,
                    "image_path": path.display().to_string(),
                    "bytes": bytes.len(),
                    "sha256": sha,
                    "isInteractive": is_interactive,
                    "keyguardLocked": keyguard_locked,
                }
            })
        );
    } else {
        println!("{}", path.display());
    }
    Ok(0)
}

/// Errors that only a human can clear (`lau resume`, or fixing the device):
/// `decide --wait` keeps polling through them instead of giving up.
const RETRYABLE_WAIT_ERRORS: [&str; 3] = ["taken_over", "task is paused", "watch_unavailable"];

fn daemon_cmd(req: Value, json: bool) -> Result<i32> {
    daemon::ensure_daemon()?;
    let resp = daemon::rpc(&req)?;
    Ok(emit_daemon_response(resp, json))
}

/// `lau decide --wait`: poll until a fresh observation is available. Waits
/// through pauses (a human may `lau resume`) but returns immediately on a
/// consequence gate, on a terminal task, or on an unknown task id.
fn decide_cmd(task_id: &str, wait: bool, json: bool) -> Result<i32> {
    let deadline = Instant::now() + Duration::from_secs(decide_wait_secs());
    loop {
        daemon::ensure_daemon()?;
        let resp = daemon::rpc(&json!({"op": "decide", "task_id": task_id}))?;
        let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let retry = wait
            && !ok
            && RETRYABLE_WAIT_ERRORS.contains(&err)
            && Instant::now() < deadline;
        if !retry {
            return Ok(emit_daemon_response(resp, json));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn decide_wait_secs() -> u64 {
    std::env::var("LAU_DECIDE_WAIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(600)
}

fn emit_daemon_response(resp: Value, json: bool) -> i32 {
    let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let err = resp
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let data = resp.get("data").cloned().unwrap_or(json!({}));
    if ok {
        if json {
            println!("{}", json!({"status": "ok", "data": data}));
        } else {
            println!("{data}");
        }
        return 0;
    }
    let status = if err == "waiting_user" {
        "waiting_user"
    } else if RETRYABLE_WAIT_ERRORS.contains(&err.as_str()) {
        // Paused: only a human action clears it (plan §0 #1/#2).
        "paused"
    } else {
        "error"
    };
    if json {
        println!("{}", json!({"status": status, "error": err, "data": data}));
    } else {
        eprintln!("lau: {err}");
    }
    if err == "waiting_user" {
        2
    } else {
        3
    }
}

fn helper_op(json: bool, cli_serial: Option<&str>, op: &str, extra: Value) -> Result<i32> {
    let devices_raw = run_adb(None, &["devices", "-l"])?;
    let devices = parse_devices(&devices_raw);
    let serial = match resolve_target(cli_serial, &devices) {
        TargetResolution::Resolved(s) => s,
        TargetResolution::Ambiguous(msg) => {
            if json {
                println!("{}", json!({"status": "target_unresolved", "error": msg}));
            } else {
                eprintln!("lau: {}", msg);
            }
            return Ok(3);
        }
        TargetResolution::Unavailable(msg) => {
            if json {
                println!("{}", json!({"status": "device_unavailable", "error": msg}));
            } else {
                eprintln!("lau: {}", msg);
            }
            return Ok(3);
        }
    };
    match helper_rpc(&serial, helper::wrap_op(op, extra)).and_then(helper::unwrap_ok) {
        Ok(data) => {
            if json {
                println!("{}", json!({"status": "ok", "data": data}));
            } else {
                println!("{data}");
            }
            Ok(0)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if json {
                println!("{}", json!({"status": "error", "error": msg}));
            } else {
                eprintln!("lau: {msg}");
            }
            Ok(3)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let code = match &cli.command {
        Commands::Doctor { json } => doctor(*json, cli.serial.as_deref()),
        Commands::Screenshot { out, json } => screenshot(*json, out.clone(), cli.serial.as_deref()),
        Commands::Dump { json } => helper_op(*json, cli.serial.as_deref(), "dump", json!({})),
        Commands::Invoke {
            element_id,
            observation_id,
            json,
        } => helper_op(
            *json,
            cli.serial.as_deref(),
            "invoke",
            json!({"elementId": element_id, "observationId": observation_id}),
        ),
        Commands::SetValue {
            element_id,
            value,
            observation_id,
            json,
        } => helper_op(
            *json,
            cli.serial.as_deref(),
            "set_value",
            json!({"elementId": element_id, "observationId": observation_id, "text": value}),
        ),
        Commands::Scroll {
            element_id,
            observation_id,
            dx,
            dy,
            json,
        } => helper_op(
            *json,
            cli.serial.as_deref(),
            "scroll",
            json!({"elementId": element_id, "observationId": observation_id, "dx": dx, "dy": dy}),
        ),
        Commands::Foreground { json } => {
            helper_op(*json, cli.serial.as_deref(), "foreground", json!({}))
        }
        Commands::Launch { package, json } => helper_op(
            *json,
            cli.serial.as_deref(),
            "launch",
            json!({"packageName": package}),
        ),
        Commands::Run {
            goal,
            app,
            actor,
            json,
        } => daemon_cmd(
            json!({
                "op": "run",
                "goal": goal,
                "app": app,
                "actor": actor,
                "serial": cli.serial,
            }),
            *json,
        ),
        Commands::Decide {
            task_id,
            wait,
            json,
        } => decide_cmd(&task_id, *wait, *json),
        Commands::Act {
            task_id,
            observation_id,
            action,
            effect,
            json,
        } => match (
            serde_json::from_str::<Value>(action),
            effect
                .as_ref()
                .map(|e| serde_json::from_str::<Value>(e))
                .transpose(),
        ) {
            (Err(e), _) => {
                eprintln!("lau: action JSON: {e}");
                Ok(EXIT_USAGE)
            }
            (_, Err(e)) => {
                eprintln!("lau: effect JSON: {e}");
                Ok(EXIT_USAGE)
            }
            (Ok(action_v), Ok(effect_v)) => {
                let mut req = json!({
                    "op": "act",
                    "task_id": task_id,
                    "observation_id": observation_id,
                    "action": action_v,
                });
                if let Some(v) = effect_v {
                    req["effect"] = v;
                }
                daemon_cmd(req, *json)
            }
        }
        Commands::Status { task_id, json } => {
            daemon_cmd(json!({"op": "status", "task_id": task_id}), *json)
        }
        Commands::Result { task_id, json } => {
            daemon_cmd(json!({"op": "result", "task_id": task_id}), *json)
        }
        Commands::Cancel { task_id, json } => {
            daemon_cmd(json!({"op": "cancel", "task_id": task_id}), *json)
        }
        Commands::Resume { task_id, json } => {
            daemon_cmd(json!({"op": "resume", "task_id": task_id}), *json)
        }
        Commands::Approve { task_id, json } => {
            daemon_cmd(json!({"op": "approve", "task_id": task_id}), *json)
        }
        Commands::Daemon => daemon::daemon_main().map(|_| 0),
    };
    match code {
        Ok(code) if code == EXIT_USAGE => std::process::exit(EXIT_USAGE),
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("lau: {:#}", e);
            std::process::exit(EXIT_INTERNAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_comes_from_the_bound_services_block_only() {
        // Verbatim shape of `dumpsys accessibility` on the test device
        // (Xiaomi 2211133C / Android 16), with the service DISABLED: the button
        // entry still names our component, the Bound services block does not.
        let disabled = concat!(
            "     button:{dev.anythinguse.lau.helper/dev.anythinguse.lau.helper.LauAccessibilityService, ",
            "com.android.settings/com.android.settings.accessibility.accessibilitymenu.AccessibilityMenuService}\n",
            "     Bound services:{Service[label=无障碍功能菜单, feedbackType[FEEDBACK_GENERIC], capabilities=8, eventTypes=, notificationTimeout=0, requestA11yBtn=true]}\n",
            "     Enabled services:{{com.android.settings/com.android.settings.accessibility.accessibilitymenu.AccessibilityMenuService}}\n",
            "     Binding services:{}\n",
        );
        assert!(!bound_from_dumpsys(disabled));

        // Same tool, service ENABLED: the block is multi-line and names our
        // service by its android:label.
        let enabled = concat!(
            "     button:{dev.anythinguse.lau.helper/dev.anythinguse.lau.helper.LauAccessibilityService}\n",
            "     Bound services:{Service[label=无障碍功能菜单, feedbackType[FEEDBACK_GENERIC], capabilities=8, eventTypes=, notificationTimeout=0, requestA11yBtn=true], \n",
            "                     Service[label=AnythingUse LAU, feedbackType[FEEDBACK_GENERIC], capabilities=33]}\n",
            "     Enabled services:{{com.android.settings/com.android.settings.accessibility.accessibilitymenu.AccessibilityMenuService}, {dev.anythinguse.lau.helper/dev.anythinguse.lau.helper.LauAccessibilityService}}\n",
            "     Binding services:{}\n",
        );
        assert!(bound_from_dumpsys(enabled));

        assert!(!bound_from_dumpsys(""));
        assert!(!bound_from_dumpsys("no bound services line at all\n"));
    }
}
