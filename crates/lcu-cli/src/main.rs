//! `lcu` — sole external entry for humans and agents.
//!
//! Agents never talk to Runtime IPC directly; they only invoke this CLI.
//! Production path: CLI → private Unix socket → desktop-owned Runtime.
//! Dev fallback: `LCU_EMBEDDED_RUNTIME=1` embeds Runtime in-process for tests.

use std::path::PathBuf;
use std::process::ExitCode as StdExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use lcu_core::error::ErrorCode;
use lcu_core::protocol::{
    DoctorReport, ExitCode, JsonEnvelope, PermissionCheck, PrivateEntryStatus,
    PROTOCOL_SCHEMA_VERSION,
};
use lcu_core::schema::SchemaDocument;
use lcu_chrome::{default_chrome_control_sock, ProductBackend};
use lcu_platform::NullBackend;
use lcu_platform_macos::MacosBackend;
use lcu_runtime::ipc::call_runtime_blocking;
use lcu_runtime::paths::RuntimePaths;
use lcu_runtime::{InternalRequest, InternalResponse, Runtime};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(name = "lcu", version, about = "Local Computer Use CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Environment and runtime diagnosis.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Submit a natural-language task.
    Run {
        goal: String,
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        json: bool,
        /// Block until the task reaches a terminal state (or times out).
        #[arg(long, default_value_t = false)]
        wait: bool,
        /// Optional per-task step budget (defaults to Runtime global limit).
        #[arg(long)]
        max_steps: Option<u32>,
        /// Display-only: `human` (default) or `agent`.
        #[arg(long, default_value = "human")]
        source: String,
        /// Display-only source label (e.g. codex, grok). Not authentication.
        #[arg(long)]
        source_name: Option<String>,
        /// Decision maker: `agent` or `vlm`; omitted follows Runtime
        /// `LCU_VISION_ACTOR` (unset/auto defaults to agent).
        #[arg(long)]
        actor: Option<String>,
        /// Control mode: `auto` (default; background first, one foreground
        /// grant when needed), `background_only` (never activate), `foreground`
        /// (one task-scoped foreground grant at task start).
        #[arg(long)]
        control_mode: Option<String>,
    },
    /// List tasks.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show task status.
    Status {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a task.
    Cancel {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Pause a running task (user takeover boundary).
    Pause {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Resume a user-paused task.
    Resume {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Show task result summary (no screenshots).
    Result {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Watch task events as JSONL lines (poll-based; incremental best-effort).
    Watch {
        task_id: String,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
    },
    /// Open the GUI approval surface only — never completes approval itself.
    Approve {
        approval_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the public command/schema contract version.
    Schema {
        #[arg(long)]
        json: bool,
    },
    /// Fetch the observation the worker waits on for an `--actor agent` task.
    Decide {
        task_id: String,
        #[arg(long)]
        json: bool,
        /// Poll until a decision is available (default: fail fast).
        #[arg(long, default_value_t = false)]
        wait: bool,
    },
    /// Submit an agent decision for a pending observation.
    Act {
        task_id: String,
        #[arg(long)]
        observation_id: String,
        /// Action JSON, e.g. '{"kind":"semantic","type":"invoke","element_id":"e1"}'.
        #[arg(long)]
        action: String,
        /// Closed-set consequence claim JSON, e.g.
        /// '{"kind":"navigate","summary":"open"}' (required for executable actions).
        #[arg(long)]
        effect: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
fn main() -> StdExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            use clap::error::ErrorKind;
            // Help/version are successful output (exit 0); any other parse
            // failure is a usage error and must exit 64 per the contract, not
            // clap's default 2 (which collides with waiting_user).
            match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    err.print().ok();
                    return StdExitCode::SUCCESS;
                }
                _ => {
                    err.print().ok();
                    return StdExitCode::from(64);
                }
            }
        }
    };
    match dispatch(cli) {
        Ok(code) => StdExitCode::from(code.as_i32() as u8),
        Err(code) => StdExitCode::from(code.as_i32() as u8),
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode, ExitCode> {
    match cli.command {
        Commands::Schema { json } => {
            let doc = SchemaDocument::current();
            if json {
                print_json(&JsonEnvelope::ok(doc));
            } else {
                println!(
                    "schema_version={} internal_protocol={}",
                    doc.schema_version, doc.internal_protocol_version
                );
            }
            Ok(ExitCode::Success)
        }
        Commands::Doctor { json } => doctor(json),
        Commands::Decide {
            task_id,
            json,
            wait,
        } => decide(task_id, json, wait),
        Commands::Act {
            task_id,
            observation_id,
            action,
            effect,
            json,
        } => act(task_id, observation_id, action, effect, json),
        Commands::Run {
            goal,
            app,
            json,
            wait,
            max_steps,
            source,
            source_name,
            actor,
            control_mode,
        } => {
            let resp = call(InternalRequest::SubmitTask {
                goal,
                app_id: app,
                source: Some(source),
                source_name,
                max_steps,
                actor,
                control_mode,
            })?;
            if wait {
                wait_for_task(resp, json)
            } else {
                handle_task_response(resp, json)
            }
        }
        Commands::List { json } => match call(InternalRequest::List)? {
            InternalResponse::Tasks { tasks } => {
                if json {
                    print_json(&JsonEnvelope::ok(tasks));
                } else if tasks.is_empty() {
                    println!("no tasks");
                } else {
                    for t in tasks {
                        println!("{} {:?}", t.task_id.0, t.state);
                    }
                }
                Ok(ExitCode::Success)
            }
            other => map_error_response(other, json),
        },
        Commands::Status { task_id, json } => {
            handle_task_response(call(InternalRequest::Status { task_id })?, json)
        }
        Commands::Cancel { task_id, json } => {
            handle_task_response(call(InternalRequest::Cancel { task_id })?, json)
        }
        Commands::Pause { task_id, json } => {
            handle_task_response(call(InternalRequest::Pause { task_id })?, json)
        }
        Commands::Resume { task_id, json } => {
            handle_task_response(call(InternalRequest::Resume { task_id })?, json)
        }
        Commands::Result { task_id, json } => {
            handle_task_response(call(InternalRequest::Result { task_id })?, json)
        }
        Commands::Watch {
            task_id,
            seconds,
            interval_ms,
        } => watch_task(task_id, seconds, interval_ms),
        Commands::Approve { approval_id, json } => {
            // Contract: never complete a gate in CLI.
            match call(InternalRequest::OpenGateUi { grant_id: approval_id })? {
                InternalResponse::GateUi { launch } => {
                    if !launch.gui_only {
                        emit_error(
                            json,
                            ErrorCode::InternalError,
                            "gate path must be GUI-only",
                        );
                        return Err(ExitCode::InternalError);
                    }
                    if json {
                        print_json(&JsonEnvelope::ok(launch));
                    } else {
                        println!("{}", launch.message);
                    }
                    Ok(ExitCode::WaitingUser)
                }
                InternalResponse::Error { code, message } => {
                    // Even on error, never invent a CLI approval success.
                    emit_error(json, code, message);
                    Err(code.exit_code())
                }
                other => map_error_response(other, json),
            }
        }
    }
}

fn doctor(json: bool) -> Result<ExitCode, ExitCode> {
    match call(InternalRequest::Doctor) {
        Ok(InternalResponse::Doctor { report }) => {
            emit_doctor(report, json);
            Ok(ExitCode::Success)
        }
        Ok(InternalResponse::Error { code, message }) => {
            emit_error(json, code, message);
            Err(code.exit_code())
        }
        Ok(other) => map_error_response(other, json),
        Err(ExitCode::RuntimeUnavailable) => {
            // Offline doctor: report unreachable runtime + surface socket presence (best effort).
            let paths = resolve_paths();
            let mac_sock = lcu_platform_macos::client::default_socket_path();
            let chrome_sock = default_chrome_control_sock().ok();
            let mac_present = mac_sock.exists();
            let chrome_present = chrome_sock.as_ref().map(|p| p.exists()).unwrap_or(false);
            let report = DoctorReport {
                schema_version: PROTOCOL_SCHEMA_VERSION.to_string(),
                product: "local-computer-use".into(),
                platform: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                runtime_reachable: false,
                private_entry: PrivateEntryStatus {
                    kind: "unix_socket".into(),
                    listens_tcp: false,
                    path: paths.as_ref().map(|p| p.socket.display().to_string()),
                    directory_mode: None,
                    socket_mode: None,
                },
                permissions: vec![
                    PermissionCheck {
                        name: "screen_recording".into(),
                        state: "not_determined".into(),
                        required_for: vec!["observe".into()],
                    },
                    PermissionCheck {
                        name: "accessibility".into(),
                        state: "not_determined".into(),
                        required_for: vec!["semantic_action".into()],
                    },
                    PermissionCheck {
                        name: "mac_window_service".into(),
                        state: if mac_present {
                            "socket_present".into()
                        } else {
                            "disconnected".into()
                        },
                        required_for: vec!["mac_window_observe".into()],
                    },
                    PermissionCheck {
                        name: "chrome_control_host".into(),
                        state: if chrome_present {
                            "socket_present".into()
                        } else {
                            "disconnected".into()
                        },
                        required_for: vec!["chrome_tab_observe".into()],
                    },
                ],
                blockers: vec![
                    "lcu-desktop runtime not reachable; start apps/lcu-desktop".into(),
                ],
                notes: vec![
                    "CLI talks only to desktop-owned private socket (or LCU_EMBEDDED_RUNTIME=1 for debug)".into(),
                    format!(
                        "mac_window sock={} present={}",
                        mac_sock.display(),
                        mac_present
                    ),
                    format!(
                        "chrome_control sock={} present={}",
                        chrome_sock
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "(unresolved)".into()),
                        chrome_present
                    ),
                    "Skill and agents must call only `lcu` (no MCP/Playwright/direct sockets)".into(),
                ],
            };
            emit_doctor(report, json);
            Ok(ExitCode::RuntimeUnavailable)
        }
        Err(code) => Err(code),
    }
}

fn emit_doctor(report: DoctorReport, json: bool) {
    if json {
        print_json(&JsonEnvelope::ok(report));
    } else {
        println!(
            "Local Computer Use doctor ({}) reachable={}",
            report.schema_version, report.runtime_reachable
        );
        println!(
            "private_entry kind={} listens_tcp={}",
            report.private_entry.kind, report.private_entry.listens_tcp
        );
        for p in &report.permissions {
            println!("permission {}={}", p.name, p.state);
        }
        for b in &report.blockers {
            println!("blocker: {b}");
        }
        for n in &report.notes {
            println!("note: {n}");
        }
    }
}

fn handle_task_response(resp: InternalResponse, json: bool) -> Result<ExitCode, ExitCode> {
    match resp {
        InternalResponse::Task { task } => {
            if json {
                print_json(&JsonEnvelope::ok(task));
            } else {
                println!("task {} state={:?}", task.task_id.0, task.state);
            }
            Ok(ExitCode::Success)
        }
        other => map_error_response(other, json),
    }
}

/// Poll task status until terminal, waiting-user, or timeout (~10 min default).
fn wait_for_task(resp: InternalResponse, json: bool) -> Result<ExitCode, ExitCode> {
    use std::time::{Duration, Instant};

    let task_id = match resp {
        InternalResponse::Task { task } => {
            if !json {
                println!(
                    "task {} accepted state={:?}; waiting for product worker…",
                    task.task_id.0, task.state
                );
            }
            task.task_id.0
        }
        other => return map_error_response(other, json),
    };

    let timeout = std::env::var("LCU_WAIT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600u64);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut last_steps = u32::MAX;

    while Instant::now() < deadline {
        match call(InternalRequest::Status {
            task_id: task_id.clone(),
        })? {
            InternalResponse::Task { task } => {
                if task.step_count != last_steps {
                    if !json {
                        println!(
                            "task {} state={:?} steps={}",
                            task.task_id.0, task.state, task.step_count
                        );
                    }
                    last_steps = task.step_count;
                }
                if task.state.is_terminal() {
                    if json {
                        print_json(&JsonEnvelope::ok(&task));
                    } else {
                        println!(
                            "task {} finished state={:?} steps={} summary={:?} error={:?}",
                            task.task_id.0, task.state, task.step_count, task.summary, task.error
                        );
                    }
                    return match task.state {
                        lcu_core::task::TaskState::Succeeded => Ok(ExitCode::Success),
                        lcu_core::task::TaskState::Cancelled
                        | lcu_core::task::TaskState::Failed => Err(ExitCode::TaskFailed),
                        _ => Err(ExitCode::InternalError),
                    };
                }
                // Park states: still "running" from user POV but need interaction.
                if matches!(
                    task.state,
                    lcu_core::task::TaskState::WaitingActor
                        | lcu_core::task::TaskState::PausedByUser
                ) {
                    if json {
                        print_json(&JsonEnvelope::ok(&task));
                    } else {
                        println!(
                            "task {} parked state={:?} (decide/resume/cancel as needed)",
                            task.task_id.0, task.state
                        );
                    }
                    return Ok(ExitCode::WaitingUser);
                }
            }
            other => return map_error_response(other, json),
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    emit_error(
        json,
        ErrorCode::InternalError,
        format!("wait timed out after {timeout}s for task {task_id}"),
    );
    Err(ExitCode::InternalError)
}

/// Agent decision mode: fetch the observation the worker is waiting on.
/// With `--wait`, polls until a decision appears (or `LCU_WAIT_TIMEOUT_SECS`).
fn decide(task_id: String, json: bool, wait: bool) -> Result<ExitCode, ExitCode> {
    use std::time::{Duration, Instant};

    let deadline = if wait {
        let secs = std::env::var("LCU_WAIT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300u64);
        Instant::now() + Duration::from_secs(secs)
    } else {
        Instant::now() - Duration::from_secs(1)
    };

    loop {
        match call(InternalRequest::GetDecision {
            task_id: task_id.clone(),
        })? {
            InternalResponse::Decision {
                observation,
                context,
                image_path,
                expires_in_secs,
                ..
            } => {
                if json {
                    print_json(&JsonEnvelope::ok(serde_json::json!({
                        "task_id": task_id,
                        "observation_id": observation.observation_id,
                        "target": {
                            "app_id": observation.app_id,
                            "pid": observation.pid,
                            "window_id": observation.window_id,
                            "window_title": observation.window_title,
                        },
                        "transform": {
                            "id": observation.transform_id,
                            "frame": observation.window_frame,
                            "model_size": [observation.image_width, observation.image_height],
                            "display_scale": observation.display_scale,
                            "image_hash": observation.image_hash,
                        },
                        "ui_state": "captured",
                        "goal": context.goal,
                        "step": context.step,
                        "last_action_summary": context.last_action_summary,
                        "transition_result": context.transition_result,
                        "elements": observation.elements,
                        "image_path": image_path,
                        "expires_in_secs": expires_in_secs,
                    })));
                } else {
                    println!("observation_id: {}", observation.observation_id);
                    println!("goal: {}", context.goal);
                    println!("step: {}", context.step);
                    if let Some(result) = context.transition_result {
                        println!("transition_result: {result}");
                    }
                    println!("image_path: {}", image_path.unwrap_or_else(|| "(none)".into()));
                    println!("elements:");
                    for e in &observation.elements {
                        println!(
                            "  {} role={} label={}",
                            e.id,
                            e.role,
                            e.label.as_deref().unwrap_or("")
                        );
                    }
                    println!("expires_in_secs: {expires_in_secs}");
                    println!("submit with: lcu act {task_id} --observation-id {} --action '<json>'", observation.observation_id);
                }
                return Ok(ExitCode::Success);
            }
            InternalResponse::Error { code, message }
                if wait && Instant::now() < deadline =>
            {
                // Retry the normal intermediate states: the worker may still be
                // resolving/observing (no observation yet) or not have reached
                // the decision step. Only a genuine error should surface.
                let retriable = code == ErrorCode::TaskNotFound
                    || (code == ErrorCode::InvalidRequest && message.contains("no observation yet"));
                if retriable {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                return map_error_response(InternalResponse::Error { code, message }, json);
            }
            other => return map_error_response(other, json),
        }
    }
}

/// Agent decision mode: submit an action + closed-set effect for a pending observation.
fn act(
    task_id: String,
    observation_id: String,
    action: String,
    effect: Option<String>,
    json: bool,
) -> Result<ExitCode, ExitCode> {
    let value: serde_json::Value = serde_json::from_str(&action).map_err(|e| {
        emit_error(json, ErrorCode::UsageError, format!("action is not valid JSON: {e}"));
        ExitCode::UsageError
    })?;
    let effect_value: Option<serde_json::Value> = match effect {
        Some(raw) => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                emit_error(json, ErrorCode::UsageError, format!("effect is not valid JSON: {e}"));
                ExitCode::UsageError
            })?;
            Some(parsed)
        }
        None => None,
    };
    let resp = call(InternalRequest::SubmitDecision {
        task_id: task_id.clone(),
        observation_id,
        action: value,
        effect: effect_value,
        confidence: None,
    })?;
    match resp {
        InternalResponse::Submitted { action, .. } => {
            if json {
                print_json(&JsonEnvelope::ok(serde_json::json!({
                    "task_id": task_id,
                    "action": action,
                })));
            } else {
                println!("submitted action: {action:?}");
            }
            Ok(ExitCode::Success)
        }
        other => map_error_response(other, json),
    }
}

fn watch_task(task_id: String, seconds: u64, interval_ms: u64) -> Result<ExitCode, ExitCode> {
    use std::io::{stdout, Write};
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_state = String::new();
    let mut last_steps = u32::MAX;
    while Instant::now() < deadline {
        match call(InternalRequest::Status {
            task_id: task_id.clone(),
        })? {
            InternalResponse::Task { task } => {
                // Wire state name (waiting_actor / paused / ...), not the Rust
                // Debug variant (WaitingActor / PausedByUser) — the JSON
                // serde rename is the machine contract.
                let state = serde_json::to_value(&task.state)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{:?}", task.state));
                if state != last_state || task.step_count != last_steps {
                    let line = serde_json::json!({
                        "task_id": task.task_id.0,
                        "state": state,
                        "step_count": task.step_count,
                        "summary": task.summary,
                        "error": task.error,
                    });
                    println!("{line}");
                    let _ = stdout().flush();
                    last_state = state;
                    last_steps = task.step_count;
                }
                if task.state.is_terminal() {
                    return Ok(ExitCode::Success);
                }
            }
            other => return map_error_response(other, true),
        }
        std::thread::sleep(Duration::from_millis(interval_ms.max(50)));
    }
    Ok(ExitCode::Success)
}

fn map_error_response(resp: InternalResponse, json: bool) -> Result<ExitCode, ExitCode> {
    match resp {
        InternalResponse::Error { code, message } => {
            emit_error(json, code, message);
            Err(code.exit_code())
        }
        _ => {
            emit_error(
                json,
                ErrorCode::InternalError,
                "unexpected runtime response",
            );
            Err(ExitCode::InternalError)
        }
    }
}

fn call(request: InternalRequest) -> Result<InternalResponse, ExitCode> {
    if embedded_enabled() {
        return embedded_call(request);
    }
    let paths = resolve_paths().ok_or(ExitCode::RuntimeUnavailable)?;
    call_runtime_blocking(&paths.socket, request).map_err(|err| {
        emit_error(true, err.code(), err.to_string());
        err.code().exit_code()
    })
}

fn embedded_enabled() -> bool {
    // Sealed in release builds: product CLI must talk to desktop-owned Runtime only.
    if cfg!(not(debug_assertions)) {
        return false;
    }
    matches!(
        std::env::var("LCU_EMBEDDED_RUNTIME").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn embedded_call(request: InternalRequest) -> Result<InternalResponse, ExitCode> {
    // Process-local Runtime for unit/dev only — not the production architecture.
    use std::sync::OnceLock;
    static RT: OnceLock<Arc<Runtime>> = OnceLock::new();
    let runtime = RT.get_or_init(|| {
        let root = std::env::var_os("LCU_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("lcu-embedded-{}", std::process::id()))
            });
        let paths = RuntimePaths::from_root(root);
        let backend = build_product_backend();
        let rt = Arc::new(Runtime::new(paths, backend).expect("embedded runtime"));
        rt.start_scheduler();
        rt
    });
    Ok(runtime.handle_internal(request))
}

/// Same backend factory as `lcu-desktop` (mac window service + Chrome product backend).
fn build_product_backend() -> Arc<dyn lcu_platform::PlatformBackend> {
    if !cfg!(target_os = "macos") {
        return Arc::new(NullBackend);
    }
    let mac = MacosBackend::new();
    let _ = mac.ensure_service();
    match ProductBackend::with_defaults(Arc::new(mac)) {
        Ok(p) => Arc::new(p),
        Err(_) => Arc::new(MacosBackend::new()),
    }
}

fn resolve_paths() -> Option<RuntimePaths> {
    if let Some(dir) = std::env::var_os("LCU_RUNTIME_DIR") {
        return Some(RuntimePaths::from_root(dir));
    }
    RuntimePaths::default_user().ok()
}

fn print_json<T: Serialize>(value: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("json serialize")
    );
}

fn emit_error(json: bool, code: ErrorCode, message: impl Into<String>) {
    let message = message.into();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&JsonEnvelope::<()>::err(
                code.json_status(),
                code,
                message
            ))
            .expect("json")
        );
    } else {
        eprintln!("error[{code}]: {message}");
    }
}
