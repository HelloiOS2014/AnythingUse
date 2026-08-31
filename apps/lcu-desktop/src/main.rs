//! `lcu-desktop` — sole owner of the single-instance Runtime.
//!
//! Headless Runtime host + menubar tray. Frontend/tray must never perform OS
//! computer-use actions; only Runtime may. Tray can complete **GUI approvals**
//! (human presence) via private Runtime methods — never through `lcu approve`.

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use lcu_chrome::ProductBackend;
use lcu_platform::NullBackend;
use lcu_platform_macos::MacosBackend;
use lcu_runtime::ipc::{bind_private_listener, serve_forever};
use lcu_runtime::paths::RuntimePaths;
use lcu_runtime::single_instance::SingleInstanceLock;
use lcu_runtime::Runtime;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tracing_subscriber::EnvFilter;
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

#[derive(Debug, Parser)]
#[command(
    name = "lcu-desktop",
    about = "Local Computer Use desktop runtime host"
)]
struct Args {
    /// Override runtime data directory (tests). Default: user Application Support.
    #[arg(long, env = "LCU_RUNTIME_DIR")]
    runtime_dir: Option<std::path::PathBuf>,
    /// Headless: no tray UI (still owns Runtime socket).
    #[arg(long, default_value_t = false)]
    headless: bool,
    /// Exit after this many idle seconds. Omit for a persistent desktop host.
    #[arg(long)]
    idle_exit_secs: Option<u64>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let args = Args::parse();
    let paths = match args.runtime_dir {
        Some(dir) => RuntimePaths::from_root(dir),
        None => RuntimePaths::default_user().context("resolve user runtime paths")?,
    };
    paths.ensure_layout().context("ensure runtime layout")?;

    let _lock = SingleInstanceLock::acquire(&paths.root)
        .context("acquire single-instance lock (is another lcu-desktop running?)")?;

    // Product backend (mac window service + Chrome control) — PlatformBackend only.
    let backend = build_product_backend(&paths);
    let runtime = Arc::new(Runtime::new(paths.clone(), backend).context("start runtime")?);
    // Product worker: observe → VLM → EffectGuard → act.
    runtime.start_scheduler();

    // Background VLM warmup, only when the local model is the default
    // decision maker (LCU_VISION_ACTOR=vlm). The agent is the default; its
    // decisions need no 8GB model load.
    if runtime.is_vlm_default() {
        let warm = Arc::clone(&runtime);
        thread::spawn(move || {
            if let Err(e) = warm.warm_vision_actor() {
                tracing::warn!(error = %e, "vision warmup skipped/failed");
            } else {
                tracing::info!("vision actor warmup complete");
            }
        });
    }

    // Remove placeholder socket created during prepare, then bind for real.
    let _ = std::fs::remove_file(&paths.socket);

    let ready = paths.root.join("runtime.ready");
    std::fs::write(
        &ready,
        serde_json::json!({
            "pid": std::process::id(),
            "socket": paths.socket.display().to_string(),
            "listens_tcp": false,
            "tray": !args.headless,
            "capture_backend": "macos_window_service",
            "chrome_control": "chrome-control.sock",
            "product_worker": true,
        })
        .to_string(),
    )
    .ok();

    // Runtime accept loop on a dedicated thread with its own tokio runtime.
    let runtime_bg = Arc::clone(&runtime);
    let socket = paths.socket.clone();
    let serve_handle = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio");
        rt.block_on(async move {
            let listener = bind_private_listener(&socket)
                .await
                .expect("bind private unix socket");
            tracing::info!(socket = %socket.display(), "lcu-desktop runtime listening (no TCP)");
            let _ = serve_forever(runtime_bg, listener).await;
        });
    });

    if args.headless {
        let _ = serve_handle.join();
        let _ = std::fs::remove_file(ready);
        return Ok(());
    }

    // Menubar tray on the main thread (macOS needs an event loop).
    run_tray(paths, ready, runtime, args.idle_exit_secs)?;
    Ok(())
}

struct TrayApp {
    paths: RuntimePaths,
    ready_path: std::path::PathBuf,
    runtime: Arc<Runtime>,
    _tray: Option<tray_icon::TrayIcon>,
    quit_id: muda::MenuId,
    doctor_id: muda::MenuId,
    list_tasks_id: muda::MenuId,
    approvals_id: muda::MenuId,
    decide_id: muda::MenuId,
    revoke_access_id: muda::MenuId,
    idle_exit_after: Option<Duration>,
    idle_since: Option<Instant>,
    presented_app_access: HashSet<String>,
}

impl ApplicationHandler for TrayApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        _event: WindowEvent,
    ) {
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            tracing::debug!(?event, "tray event");
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                tracing::info!("quit requested from tray");
                self.cleanup();
                event_loop.exit();
            } else if event.id == self.doctor_id {
                let report = self.runtime.doctor_report();
                tracing::info!(
                    socket = %self.paths.socket.display(),
                    reachable = report.runtime_reachable,
                    notes = ?report.notes,
                    "doctor: runtime owned by lcu-desktop; use `lcu doctor --json`"
                );
            } else if event.id == self.list_tasks_id {
                let tasks = self.runtime.list_tasks();
                if tasks.is_empty() {
                    tracing::info!("queue empty");
                } else {
                    for t in tasks.iter().take(20) {
                        tracing::info!(
                            task_id = %t.task_id.0,
                            state = ?t.state,
                            steps = t.step_count,
                            goal = %t.goal.chars().take(80).collect::<String>(),
                            "queue task"
                        );
                    }
                }
            } else if event.id == self.approvals_id {
                let pending = self.runtime.list_pending_gates();
                if pending.is_empty() {
                    tracing::info!("no pending gates");
                } else {
                    for p in &pending {
                        tracing::info!(
                            grant_id = %p.grant_id,
                            kind = ?p.kind,
                            task_id = %p.task_id,
                            target_app = %p.target_app,
                            summary = %p.summary,
                            impact = %p.impact,
                            message = %p.message,
                            "pending gate (use tray Review & decide)"
                        );
                    }
                }
            } else if event.id == self.revoke_access_id {
                let keys = self
                    .runtime
                    .list_app_permissions()
                    .into_iter()
                    .map(|permission| permission.app_key)
                    .collect::<Vec<_>>();
                if let Some(app_key) = revoke_app_access_dialog(&keys) {
                    match self.runtime.revoke_app_permission(&app_key) {
                        Ok(true) => tracing::info!(app_key, "GUI: app access revoked"),
                        Ok(false) => tracing::info!(app_key, "GUI: app access was already absent"),
                        Err(e) => tracing::warn!(error = %e, "GUI: app access revoke failed"),
                    }
                }
            } else if event.id == self.decide_id {
                // AppAccess: Allow once / Always allow / Deny.
                // Consequence: Approve/Deny.
                // Takeover (R4): Start takeover → (user acts) → Done/Cancel.
                let pending = self.runtime.list_pending_gates();
                match pending.first() {
                    Some(p) => {
                        use lcu_core::approval::GateKind;
                        match p.kind {
                            GateKind::AppAccess => match app_access_dialog(p) {
                                Some(decision) => {
                                    match self.runtime.app_access_in_gui(&p.grant_id, decision) {
                                        Ok(()) => tracing::info!(
                                            grant_id = %p.grant_id,
                                            decision = ?decision,
                                            "GUI: app access decided"
                                        ),
                                        Err(e) => {
                                            tracing::warn!(error = %e, "GUI app access failed")
                                        }
                                    }
                                }
                                None => tracing::info!(
                                    grant_id = %p.grant_id,
                                    "GUI: app access dialog cancelled/failed"
                                ),
                            },
                            GateKind::Takeover => {
                                if !p.takeover_started {
                                    match takeover_start_dialog(p) {
                                        Some(true) => {
                                            match self.runtime.begin_takeover_in_gui(&p.grant_id) {
                                                Ok(()) => tracing::info!(
                                                    grant_id = %p.grant_id,
                                                    "GUI: R4 takeover started (user must finish action)"
                                                ),
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "begin takeover failed")
                                                }
                                            }
                                        }
                                        Some(false) => {
                                            match self.runtime.deny_pending_in_gui(&p.grant_id) {
                                                Ok(()) => tracing::info!(
                                                    grant_id = %p.grant_id,
                                                    "GUI: R4 takeover cancelled"
                                                ),
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "GUI deny failed")
                                                }
                                            }
                                        }
                                        None => tracing::info!(
                                            grant_id = %p.grant_id,
                                            "GUI: R4 start dialog cancelled/failed"
                                        ),
                                    }
                                } else {
                                    match takeover_done_dialog(p) {
                                        Some(true) => {
                                            match self.runtime.complete_takeover_in_gui(&p.grant_id)
                                            {
                                                Ok(()) => tracing::info!(
                                                    grant_id = %p.grant_id,
                                                    "GUI: R4 takeover marked complete; re-observe"
                                                ),
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "complete takeover failed")
                                                }
                                            }
                                        }
                                        Some(false) => {
                                            match self.runtime.deny_pending_in_gui(&p.grant_id) {
                                                Ok(()) => tracing::info!(
                                                    grant_id = %p.grant_id,
                                                    "GUI: R4 takeover cancelled after start"
                                                ),
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "GUI deny failed")
                                                }
                                            }
                                        }
                                        None => tracing::info!(
                                            grant_id = %p.grant_id,
                                            "GUI: R4 done dialog cancelled/failed"
                                        ),
                                    }
                                }
                            }
                            GateKind::Consequence => {
                                let decision = confirm_pending_dialog(p);
                                match decision {
                                    Some(true) => {
                                        match self.runtime.approve_pending_in_gui(&p.grant_id) {
                                            Ok(()) => tracing::info!(
                                                grant_id = %p.grant_id,
                                                "GUI decision: approved"
                                            ),
                                            Err(e) => {
                                                tracing::warn!(error = %e, "GUI approve failed")
                                            }
                                        }
                                    }
                                    Some(false) => {
                                        match self.runtime.deny_pending_in_gui(&p.grant_id) {
                                            Ok(()) => tracing::info!(
                                                grant_id = %p.grant_id,
                                                "GUI decision: denied"
                                            ),
                                            Err(e) => {
                                                tracing::warn!(error = %e, "GUI deny failed")
                                            }
                                        }
                                    }
                                    None => tracing::info!(
                                        grant_id = %p.grant_id,
                                        "GUI decision: cancelled / dialog failed"
                                    ),
                                }
                            }
                        }
                    }
                    None => tracing::info!("no pending gate to decide"),
                }
            }
        }

        if let Some(pending) = self
            .runtime
            .list_pending_gates()
            .into_iter()
            .find(|p| {
                p.kind == lcu_core::approval::GateKind::AppAccess
                    && !self.presented_app_access.contains(&p.grant_id)
            })
        {
            self.presented_app_access.insert(pending.grant_id.clone());
            if let Some(decision) = app_access_dialog(&pending) {
                if let Err(error) = self
                    .runtime
                    .app_access_in_gui(&pending.grant_id, decision)
                {
                    tracing::warn!(%error, "automatic app access dialog failed");
                }
            }
        }

        let Some(idle_exit_after) = self.idle_exit_after else {
            return;
        };
        let now = Instant::now();
        if runtime_busy(&self.runtime) {
            self.idle_since = None;
        } else {
            let idle_since = self.idle_since.get_or_insert(now);
            if now.duration_since(*idle_since) >= idle_exit_after {
                tracing::info!("auto-started runtime idle; exiting");
                self.idle_exit_after = None;
                self.cleanup();
                event_loop.exit();
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_secs(1)));
    }
}

impl TrayApp {
    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.ready_path);
        let _ = std::fs::remove_file(&self.paths.socket);
        let mac_socket = std::env::var_os("LCU_MACOS_WINDOW_SOCK")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.paths.root.join("macos-window.sock"));
        let mut mac_pid = mac_socket.clone();
        mac_pid.set_extension("pid");
        let _ = std::fs::remove_file(mac_socket);
        let _ = std::fs::remove_file(mac_pid);
    }
}

fn runtime_busy(runtime: &Runtime) -> bool {
    runtime
        .list_tasks()
        .into_iter()
        .any(|task| task_keeps_runtime_alive(task.state))
        || !runtime.list_pending_gates().is_empty()
}

fn task_keeps_runtime_alive(state: lcu_core::task::TaskState) -> bool {
    !state.is_terminal() && state != lcu_core::task::TaskState::PausedByUser
}

fn run_tray(
    paths: RuntimePaths,
    ready_path: std::path::PathBuf,
    runtime: Arc<Runtime>,
    idle_exit_secs: Option<u64>,
) -> Result<()> {
    let icon = default_icon();
    let menu = Menu::new();
    let doctor = MenuItem::new("Runtime status", true, None);
    let list_tasks = MenuItem::new("List task queue", true, None);
    let approvals = MenuItem::new("List pending approvals", true, None);
    let decide = MenuItem::new("Review & decide pending…", true, None);
    let revoke_access = MenuItem::new("Revoke app access…", true, None);
    let quit = MenuItem::new("Quit Local Computer Use", true, None);
    menu.append(&doctor)?;
    menu.append(&list_tasks)?;
    menu.append(&approvals)?;
    menu.append(&decide)?;
    menu.append(&revoke_access)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Local Computer Use")
        .with_icon(icon)
        .with_title("LCU")
        .build()
        .context("create tray icon")?;

    let mut app = TrayApp {
        paths,
        ready_path,
        runtime,
        _tray: Some(tray),
        quit_id: quit.id().clone(),
        doctor_id: doctor.id().clone(),
        list_tasks_id: list_tasks.id().clone(),
        approvals_id: approvals.id().clone(),
        decide_id: decide.id().clone(),
        revoke_access_id: revoke_access.id().clone(),
        idle_exit_after: idle_exit_secs
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs),
        idle_since: None,
        presented_app_access: HashSet::new(),
    };

    let event_loop = EventLoop::new().context("create event loop")?;
    event_loop
        .run_app(&mut app)
        .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcu_core::task::TaskState;

    #[test]
    fn auto_host_waits_only_for_live_work() {
        assert!(task_keeps_runtime_alive(TaskState::Queued));
        assert!(task_keeps_runtime_alive(TaskState::WaitingActor));
        assert!(!task_keeps_runtime_alive(TaskState::PausedByUser));
        assert!(!task_keeps_runtime_alive(TaskState::Succeeded));
    }
}

/// MacosBackend + optional Chrome ProductBackend.
fn build_product_backend(paths: &RuntimePaths) -> Arc<dyn lcu_platform::PlatformBackend> {
    if !cfg!(target_os = "macos") {
        return Arc::new(NullBackend);
    }

    let mac_socket = std::env::var_os("LCU_MACOS_WINDOW_SOCK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths.root.join("macos-window.sock"));
    let mac = MacosBackend::with_socket(mac_socket.clone(), true);
    if let Err(e) = mac.ensure_service() {
        tracing::warn!(error = %e, "macos-window-service not ready at desktop start");
    }

    match ProductBackend::with_defaults(Arc::new(mac)) {
        Ok(p) => {
            if let Err(e) = p.cleanup_stale_lease() {
                tracing::warn!(error = %e, "stale Chrome lease cleanup failed");
            }
            Arc::new(p) as Arc<dyn lcu_platform::PlatformBackend>
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "ProductBackend chrome path failed; falling back to MacosBackend only"
            );
            Arc::new(MacosBackend::with_socket(mac_socket, true))
                as Arc<dyn lcu_platform::PlatformBackend>
        }
    }
}

fn default_icon() -> Icon {
    // Simple 32x32 blue RGBA icon.
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let edge = x < 2 || y < 2 || x >= size - 2 || y >= size - 2;
            if edge {
                rgba.extend_from_slice(&[20, 20, 20, 255]);
            } else {
                rgba.extend_from_slice(&[40, 120, 255, 255]);
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("icon")
}

/// Local confirmation dialog: shows task, app, consequence summary, impact;
/// Approve / Deny. Returns Some(true)=Approve, Some(false)=Deny, None=cancelled/failed.
fn confirm_pending_dialog(p: &lcu_runtime::GateUiLaunch) -> Option<bool> {
    let body = format!(
        "Task: {}\nApp: {}\nConsequence: {}\nImpact: {}\n\n{}",
        p.task_id, p.target_app, p.summary, p.impact, p.message
    );
    run_osascript_choice(
        &body,
        "Local Computer Use — Confirm consequence",
        "Deny",
        "Approve",
        "approve",
        "deny",
    )
}

/// App access dialog: Allow once / Always allow / Deny (3-button choice).
fn app_access_dialog(p: &lcu_runtime::GateUiLaunch) -> Option<lcu_core::approval::AppAccessDecision> {
    let body = format!(
        "First control of this app needs your decision.\n\nTask: {}\nApp: {}\n\n{}",
        p.task_id, p.target_app, p.message
    );
    let script = format!(
        r#"try
  set r to display dialog "{escaped}" with title "Local Computer Use — App access" buttons {{"Deny", "Always allow", "Allow once"}} default button "Allow once" cancel button "Deny" with icon caution
  if button returned of r is "Allow once" then
    return "allow_once"
  else if button returned of r is "Always allow" then
    return "always_allow"
  else
    return "deny"
  end if
on error number -128
  return "deny"
end try"#,
        escaped = body
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() {
        return Some(lcu_core::approval::AppAccessDecision::Deny);
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
    if text.contains("allow_once") {
        Some(lcu_core::approval::AppAccessDecision::AllowOnce)
    } else if text.contains("always_allow") {
        Some(lcu_core::approval::AppAccessDecision::AlwaysAllow)
    } else {
        Some(lcu_core::approval::AppAccessDecision::Deny)
    }
}

fn revoke_app_access_dialog(keys: &[String]) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let script = r#"on run argv
set picked to choose from list argv with title "AnythingUse — Revoke app access" with prompt "Choose an application identity to revoke:" without multiple selections allowed and empty selection allowed
if picked is false then return ""
return item 1 of picked
end run"#;
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg("--")
        .args(keys)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let selected = String::from_utf8_lossy(&out.stdout).trim().to_string();
    keys.iter().find(|key| **key == selected).cloned()
}

/// R4 step 1: begin human takeover (does not complete the action).
fn takeover_start_dialog(p: &lcu_runtime::GateUiLaunch) -> Option<bool> {
    let body = format!(
        "R4 human takeover required.\n\nTask: {}\nApp: {}\nConsequence: {}\nImpact: {}\n\n{}\n\nYou must perform this action yourself. Click Start takeover, do the work, then return here and mark Done.",
        p.task_id, p.target_app, p.summary, p.impact, p.message
    );
    run_osascript_choice(
        &body,
        "Local Computer Use — Start takeover",
        "Cancel",
        "Start takeover",
        "start",
        "cancel",
    )
}

/// R4 step 2: human finished (or cancelled) after start.
fn takeover_done_dialog(p: &lcu_runtime::GateUiLaunch) -> Option<bool> {
    let body = format!(
        "R4 takeover in progress.\n\nTask: {}\nApp: {}\nConsequence: {}\n\nClick Done only after you finished the action yourself. The agent will re-observe and will not auto-execute this action.",
        p.task_id, p.target_app, p.summary
    );
    run_osascript_choice(
        &body,
        "Local Computer Use — Takeover done?",
        "Cancel",
        "Done",
        "done",
        "cancel",
    )
}

fn run_osascript_choice(
    body: &str,
    title: &str,
    deny_label: &str,
    ok_label: &str,
    ok_token: &str,
    deny_token: &str,
) -> Option<bool> {
    let escaped = body
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let script = format!(
        r#"try
  set r to display dialog "{escaped}" with title "{title}" buttons {{"{deny_label}", "{ok_label}"}} default button "{deny_label}" cancel button "{deny_label}" with icon caution
  if button returned of r is "{ok_label}" then
    return "{ok_token}"
  else
    return "{deny_token}"
  end if
on error number -128
  return "{deny_token}"
end try"#
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("-128") || stderr.is_empty() {
            return Some(false);
        }
        tracing::warn!(%stderr, "dialog failed");
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_lowercase();
    if text.contains(ok_token) {
        Some(true)
    } else if text.contains(deny_token) {
        Some(false)
    } else {
        None
    }
}
