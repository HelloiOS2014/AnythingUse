//! VisionActor that talks to the local Python Qwen3-VL worker over stdin/stdout.
//!
//! Every request has a hard timeout. On timeout the worker process is killed so
//! the product loop can fall back instead of hanging for 15 minutes.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::{ModelObservation, ModelTaskContext, VisionActor};
use lcu_core::action::{Action, ProposedAction};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::ObservationId;

/// Default wall-clock budget for one worker request (warmup or propose).
///
/// Cold start on MPS has been measured ~134–161s; default must clear that budget.
fn request_timeout() -> Duration {
    let secs = std::env::var("LCU_VLM_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        // Default 240s: covers cold load (~134–161s) plus first propose.
        .unwrap_or(240u64);
    Duration::from_secs(secs.max(5))
}

/// Coerce worker/model action JSON into `Action`.
///
/// Handles the common sloppy shape where the model emits:
/// `{"action":"semantic","type":"invoke","element_id":"el_0"}`
/// instead of nesting `kind` under `action`.
fn coerce_action_value(
    action_val: serde_json::Value,
    envelope: &serde_json::Value,
) -> Result<Action, String> {
    if let Ok(a) = serde_json::from_value::<Action>(action_val.clone()) {
        return Ok(a);
    }
    // Nested {"action": {...}}
    if let Some(inner) = action_val.as_object() {
        if let Ok(a) = serde_json::from_value::<Action>(serde_json::Value::Object(inner.clone())) {
            return Ok(a);
        }
    }
    // Flat: action is a kind string; type/element_id may live on envelope.
    if let Some(kind) = action_val.as_str() {
        let mut rebuilt = serde_json::Map::new();
        rebuilt.insert("kind".into(), json!(kind));
        if let Some(obj) = envelope.as_object() {
            for key in [
                "type",
                "element_id",
                "url",
                "value",
                "delta_x",
                "delta_y",
                "milliseconds",
                "summary",
                "reason",
                "text",
                "x",
                "y",
                "button",
                "keys",
            ] {
                if let Some(v) = obj.get(key) {
                    rebuilt.insert(key.to_string(), v.clone());
                }
            }
            // Also accept fields under envelope.action if it was an object partially.
            if let Some(serde_json::Value::Object(inner)) = obj.get("action") {
                for (k, v) in inner {
                    if k != "kind" {
                        rebuilt.entry(k.clone()).or_insert(v.clone());
                    }
                }
            }
        }
        return serde_json::from_value::<Action>(serde_json::Value::Object(rebuilt))
            .map_err(|e| format!("flat kind coerce failed: {e}"));
    }
    // Last resort: wrap as {"action": ...}
    #[derive(Deserialize)]
    struct Wrap {
        action: Action,
    }
    serde_json::from_value::<Wrap>(json!({ "action": action_val.clone() }))
        .map(|w| w.action)
        .map_err(|e| format!("direct={e}; value={action_val}"))
}

/// Write screenshot bytes with owner-only permissions (0600 on Unix).
pub(crate) fn write_private_temp_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

pub struct SubprocessVisionActor {
    python: PathBuf,
    worker_script: PathBuf,
    model_dir: PathBuf,
    child: Mutex<Option<Worker>>,
    /// Serializes warm-up and propose requests. The worker protocol is one
    /// request/one response over stdin/stdout: two concurrent callers (the
    /// desktop warm thread and the task worker) would each spawn a second
    /// python process (double ~4GB model load), clobber the slot, and drop a
    /// live Child without kill/wait (orphan). Holding this lock for the whole
    /// request makes the child slot single-consumer by construction.
    busy: Mutex<()>,
    last_load_ms: Mutex<Option<u64>>,
    last_latency_ms: Mutex<Option<u64>>,
}

impl std::fmt::Debug for SubprocessVisionActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessVisionActor")
            .field("python", &self.python)
            .field("worker_script", &self.worker_script)
            .field("model_dir", &self.model_dir)
            .finish()
    }
}

struct Worker {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl SubprocessVisionActor {
    pub fn new(
        python: impl Into<PathBuf>,
        worker_script: impl Into<PathBuf>,
        model_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            python: python.into(),
            worker_script: worker_script.into(),
            model_dir: model_dir.into(),
            child: Mutex::new(None),
            busy: Mutex::new(()),
            last_load_ms: Mutex::new(None),
            last_latency_ms: Mutex::new(None),
        }
    }

    pub fn from_repo_root(repo: &Path) -> Self {
        let python = repo.join(".venv/bin/python");
        let python = if python.exists() {
            python
        } else {
            PathBuf::from("python3")
        };
        Self::new(
            python,
            repo.join("scripts/qwen3_vl_worker.py"),
            repo.join("models/Qwen3-VL-4B-Instruct"),
        )
    }

    pub fn last_load_ms(&self) -> Option<u64> {
        *self.last_load_ms.lock().expect("lock")
    }

    pub fn last_latency_ms(&self) -> Option<u64> {
        *self.last_latency_ms.lock().expect("lock")
    }

    fn kill_worker_locked(guard: &mut Option<Worker>) {
        if let Some(mut w) = guard.take() {
            let _ = w.child.kill();
            let _ = w.child.wait();
        }
    }

    fn ensure_worker(&self) -> LcuResult<()> {
        let mut guard = self.child.lock().expect("lock");
        if let Some(w) = guard.as_mut() {
            match w.child.try_wait() {
                Ok(None) => return Ok(()),
                Ok(Some(status)) => {
                    *guard = None;
                    return Err(LcuError::coded(
                        ErrorCode::InternalError,
                        format!("qwen worker exited: {status}"),
                    ));
                }
                Err(e) => {
                    return Err(LcuError::coded(
                        ErrorCode::InternalError,
                        format!("qwen worker status: {e}"),
                    ));
                }
            }
        }

        if !self.worker_script.exists() {
            return Err(LcuError::coded(
                ErrorCode::NotImplemented,
                format!("worker script missing: {}", self.worker_script.display()),
            ));
        }
        if !self.model_dir.exists() {
            return Err(LcuError::coded(
                ErrorCode::NotImplemented,
                format!("model dir missing: {}", self.model_dir.display()),
            ));
        }

        // Release must never inherit LCU_VLM_NO_IMAGE (would degrade to text-only).
        // Debug may still pass no_image via the JSON request field only.
        let mut cmd = Command::new(&self.python);
        cmd.arg("-u")
            .arg(&self.worker_script)
            .env("LCU_MODEL_DIR", &self.model_dir)
            // Default max_time aligns with propose budget; per-request max_time overrides.
            .env(
                "LCU_VLM_MAX_TIME",
                std::env::var("LCU_VLM_MAX_TIME")
                    .or_else(|_| std::env::var("LCU_VLM_PROPOSE_SECS"))
                    .unwrap_or_else(|_| "120".into()),
            )
            .env(
                "LCU_VLM_MAX_IMAGE",
                std::env::var("LCU_VLM_MAX_IMAGE").unwrap_or_else(|_| "640".into()),
            )
            .env(
                "LCU_VLM_MAX_NEW",
                std::env::var("LCU_VLM_MAX_NEW").unwrap_or_else(|_| "512".into()),
            )
            .env_remove("LCU_VLM_NO_IMAGE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("spawn qwen worker: {e}"))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LcuError::coded(ErrorCode::InternalError, "worker stdin missing"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LcuError::coded(ErrorCode::InternalError, "worker stdout missing"))?;
        *guard = Some(Worker {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        });
        Ok(())
    }

    /// Send one JSON line and wait for one response line with a hard wall-clock timeout.
    fn request_json(&self, req: serde_json::Value) -> LcuResult<serde_json::Value> {
        let timeout = request_timeout();
        // Serialize the whole request against warm_up/propose concurrency: see
        // `busy` doc on the struct. Lock order is busy → child, never reversed.
        let _busy = self.busy.lock().expect("busy lock");
        self.ensure_worker()?;

        let mut guard = self.child.lock().expect("lock");
        let worker = match guard.take() {
            Some(w) => w,
            None => {
                return Err(LcuError::coded(
                    ErrorCode::InternalError,
                    "worker not started",
                ));
            }
        };

        let line = serde_json::to_string(&req).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("serialize req: {e}"))
        })?;

        // Write while we own the worker.
        let mut worker = worker;
        if let Err(e) = writeln!(worker.stdin, "{line}").and_then(|_| worker.stdin.flush()) {
            Self::kill_worker_locked(&mut Some(worker));
            return Err(LcuError::coded(
                ErrorCode::InternalError,
                format!("write worker: {e}"),
            ));
        }

        let child_pid = worker.child.id();
        let (tx, rx) = mpsc::channel();
        // Reader thread owns worker until response or kill.
        std::thread::spawn(move || {
            let mut resp_line = String::new();
            let read = worker.stdout.read_line(&mut resp_line);
            let result = match read {
                Ok(0) => Err("worker closed stdout".to_string()),
                Ok(_) => Ok(resp_line),
                Err(e) => Err(format!("read worker: {e}")),
            };
            let _ = tx.send((result, worker));
        });

        match rx.recv_timeout(timeout) {
            Ok((Ok(resp_line), worker)) => {
                *guard = Some(worker);
                serde_json::from_str(resp_line.trim()).map_err(|e| {
                    LcuError::coded(
                        ErrorCode::InternalError,
                        format!("worker json parse: {e}; line={resp_line}"),
                    )
                })
            }
            Ok((Err(msg), mut worker)) => {
                let _ = worker.child.kill();
                let _ = worker.child.wait();
                Err(LcuError::coded(ErrorCode::InternalError, msg))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!(
                    "qwen worker request timed out after {}s pid={child_pid}; killing worker",
                    timeout.as_secs()
                );
                // Kill by pid first (Child is inside the reader thread).
                let _ = Command::new("kill")
                    .args(["-9", &child_pid.to_string()])
                    .status();
                // Reap reader thread so we don't leak Worker/Child.
                if let Ok((_res, mut worker)) = rx.recv_timeout(Duration::from_secs(3)) {
                    let _ = worker.child.kill();
                    let _ = worker.child.wait();
                }
                *guard = None;
                Err(LcuError::coded(
                    ErrorCode::InternalError,
                    format!(
                        "qwen worker timed out after {}s (set LCU_VLM_TIMEOUT_SECS)",
                        timeout.as_secs()
                    ),
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                *guard = None;
                Err(LcuError::coded(
                    ErrorCode::InternalError,
                    "qwen worker reader thread died",
                ))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct WorkerProposeResp {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    action: Option<serde_json::Value>,
    #[serde(default)]
    effect_claim: Option<String>,
    #[serde(default)]
    expected_effect: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    latency_ms: Option<u64>,
    #[serde(default)]
    load_ms: Option<u64>,
}

impl VisionActor for SubprocessVisionActor {
    fn name(&self) -> &str {
        "qwen3-vl-subprocess"
    }

    fn warm_up(&self) -> LcuResult<()> {
        let t0 = Instant::now();
        let resp = self.request_json(json!({"op": "warmup"}))?;
        if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err(LcuError::coded(
                ErrorCode::InternalError,
                format!("warmup failed: {resp}"),
            ));
        }
        if let Some(ms) = resp.get("load_ms").and_then(|v| v.as_u64()) {
            *self.last_load_ms.lock().expect("lock") = Some(ms);
        } else {
            *self.last_load_ms.lock().expect("lock") = Some(t0.elapsed().as_millis() as u64);
        }
        Ok(())
    }

    fn propose_action(
        &self,
        observation: &ModelObservation,
        context: &ModelTaskContext,
    ) -> LcuResult<ProposedAction> {
        // Strip image bytes from JSON payload; pass via temp path only.
        let mut obs_for_json = observation.clone();
        let png = obs_for_json.image_png.take();
        let obs_json = serde_json::to_value(&obs_for_json)
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("obs json: {e}")))?;
        // Enough tokens for a full action object; 256 was truncating mid-JSON on MPS.
        let max_new: u32 = std::env::var("LCU_VLM_MAX_NEW")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512);
        // One total wall budget for the warm propose (including any JSON retry).
        // Do not stack parent 240s + child 90s + retry 180s.
        // 180s: the first propose after a model load pays a one-time MPS
        // prefill compile that measured ~160s; 120s cut it mid-generation.
        let propose_budget_secs: u64 = std::env::var("LCU_VLM_PROPOSE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(180);
        let mut req = json!({
            "op": "propose",
            "goal": context.goal,
            "observation": obs_json,
            "max_new_tokens": max_new,
            "max_time": propose_budget_secs,
            "step": context.step,
            "last_action_summary": context.last_action_summary,
        });
        // Product default: send target window screenshot. LCU_VLM_NO_IMAGE=1 is
        // debug-only; release builds always require image when PNG is present.
        let force_no_image = cfg!(debug_assertions)
            && matches!(
                std::env::var("LCU_VLM_NO_IMAGE").as_deref(),
                Ok("1") | Ok("true") | Ok("TRUE")
            );
        let mut temp_path: Option<PathBuf> = None;
        if force_no_image {
            req["no_image"] = json!(true);
        } else if let Some(bytes) = png.as_ref() {
            let path = std::env::temp_dir().join(format!(
                "lcu-vlm-{}-{}.png",
                std::process::id(),
                observation.observation_id
            ));
            write_private_temp_file(&path, bytes).map_err(|e| {
                LcuError::coded(ErrorCode::InternalError, format!("write temp png: {e}"))
            })?;
            req["image_path"] = json!(path.display().to_string());
            req["no_image"] = json!(false);
            temp_path = Some(path);
        } else if let Some(summary) = &context.last_action_summary {
            if let Some(path) = summary.strip_prefix("image_path=") {
                req["image_path"] = json!(path);
                req["no_image"] = json!(false);
            } else {
                return Err(LcuError::coded(
                    ErrorCode::TaskFailed,
                    "product VLM requires target window screenshot; observation has no image_png",
                ));
            }
        } else {
            return Err(LcuError::coded(
                ErrorCode::TaskFailed,
                "product VLM requires target window screenshot; observation has no image_png",
            ));
        }

        let resp_val = self.request_json(req);
        // Always clean raw PNG and predictable derived JPEG (even on timeout/error).
        if let Some(path) = temp_path {
            let derived = path.with_extension("vlm.jpg");
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&derived);
        }
        let resp_val = resp_val?;
        let resp: WorkerProposeResp = serde_json::from_value(resp_val.clone()).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("worker resp shape: {e}; {resp_val}"),
            )
        })?;
        if !resp.ok {
            return Err(LcuError::coded(
                ErrorCode::TaskFailed,
                resp.error.unwrap_or_else(|| "worker propose failed".into()),
            ));
        }
        if let Some(ms) = resp.latency_ms {
            *self.last_latency_ms.lock().expect("lock") = Some(ms);
        }
        if let Some(ms) = resp.load_ms {
            *self.last_load_ms.lock().expect("lock") = Some(ms);
        }

        let action_val = resp.action.ok_or_else(|| {
            LcuError::coded(ErrorCode::InvalidRequest, "worker returned no action")
        })?;

        // Prefer full worker envelope (may carry flat fields for sloppy model output).
        let action = coerce_action_value(action_val.clone(), &resp_val).map_err(|e| {
            LcuError::coded(
                ErrorCode::InvalidRequest,
                format!("action parse failed: {e}; value={action_val}"),
            )
        })?;

        Ok(ProposedAction {
            observation_id: ObservationId(observation.observation_id.clone()),
            action,
            effect_claim: resp.effect_claim,
            expected_effect: resp.expected_effect,
            model_claimed_risk: None,
            confidence: resp.confidence.unwrap_or(0.5),
        })
    }
}

impl Drop for SubprocessVisionActor {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock() {
            Self::kill_worker_locked(&mut guard);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelObservation, ModelTaskContext};
    use std::sync::Arc;

    const STUB: &str = r#"#!/usr/bin/env python3
import json, sys, os
with open(os.environ["LCU_SPAWN_MARK"], "a") as f:
    f.write("spawn\n")
for line in sys.stdin:
    req = json.loads(line)
    op = req.get("op")
    if op == "warmup":
        sys.stdout.write(json.dumps({"ok": True, "load_ms": 1, "device": "cpu", "model_dir": "x"}) + "\n")
    elif op == "propose":
        sys.stdout.write(json.dumps({"ok": True, "action": {"kind": "wait", "milliseconds": 1}, "latency_ms": 1}) + "\n")
    else:
        sys.stdout.write(json.dumps({"ok": True}) + "\n")
    sys.stdout.flush()
"#;

    fn sample_obs() -> ModelObservation {
        ModelObservation {
            observation_id: "obs_1".into(),
            app_id: "com.example.App".into(),
            window_title: "t".into(),
            elements: vec![],
            image_png: None,
            image_width: 10,
            image_height: 10,
        }
    }

    #[test]
    fn concurrent_warmups_spawn_single_worker() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub_worker.py");
        std::fs::write(&stub, STUB).unwrap();
        let mark = dir.path().join("spawns.txt");
        std::fs::write(&mark, "").unwrap();
        let model_dir = dir.path().join("model");
        std::fs::create_dir(&model_dir).unwrap();

        // The child inherits the parent env; LCU_SPAWN_MARK tells the stub where
        // to record spawns. Rust 2024 env APIs are set via unsafe; use the
        // simple pre-Rust-2024 set_var (edition 2021, safe).
        std::env::set_var("LCU_SPAWN_MARK", &mark);

        let actor = Arc::new(SubprocessVisionActor::new(
            "python3",
            stub,
            model_dir,
        ));
        let h1 = {
            let a = actor.clone();
            std::thread::spawn(move || a.warm_up())
        };
        let h2 = {
            let a = actor.clone();
            std::thread::spawn(move || a.warm_up())
        };
        h1.join().unwrap().unwrap();
        h2.join().unwrap().unwrap();

        let spawns = std::fs::read_to_string(&mark).unwrap().lines().count();
        assert_eq!(
            spawns, 1,
            "concurrent warmups must spawn exactly one worker process, got {spawns}"
        );

        // Worker survived (slot not clobbered): a follow-up propose succeeds.
        // The product path requires an image; the stub never reads the file, so
        // a fake image_path summary satisfies the check.
        let ctx = ModelTaskContext {
            goal: "g".into(),
            step: 0,
            last_action_summary: Some("image_path=/tmp/stub.png".into()),
        };
        let proposal = actor.propose_action(&sample_obs(), &ctx).unwrap();
        assert!(matches!(proposal.action, Action::Wait { .. }));
    }
}
