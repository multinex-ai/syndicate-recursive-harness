//! Detached job supervisor: runs one worker, enforces deadline, cancel, log budget
//! and heartbeat, then records an execution (not acceptance) result.
use crate::jobs::{media_arguments, root, JobStore};
use serde_json::{json, Map, Value as Json};
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const LOG_BUDGET: u64 = 16 * 1024 * 1024;

/// A process identity pinned by start time, so a reused PID is never signalled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Owned {
    pub pid: i32,
    pub start: (u64, u64),
}

pub fn start_time(pid: i32) -> Option<(u64, u64)> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    Some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

fn children(pid: i32) -> Vec<i32> {
    let mut buf = vec![0i32; 1024];
    let n = unsafe {
        libc::proc_listchildpids(
            pid,
            buf.as_mut_ptr() as *mut libc::c_void,
            (buf.len() * 4) as i32,
        )
    };
    if n <= 0 {
        return vec![];
    }
    buf.truncate((n as usize).min(1024));
    buf.into_iter().filter(|p| *p > 0).collect()
}

/// All live descendants of `pid` (breadth-first), each pinned by start time.
pub fn descendants(pid: i32) -> Vec<Owned> {
    let mut out = vec![];
    let mut queue = vec![pid];
    while let Some(p) = queue.pop() {
        for c in children(p) {
            if let Some(start) = start_time(c) {
                if !out.iter().any(|o: &Owned| o.pid == c) {
                    out.push(Owned { pid: c, start });
                    queue.push(c);
                }
            }
        }
    }
    out
}

fn merge(known: &mut Vec<Owned>, fresh: Vec<Owned>) {
    for f in fresh {
        if !known.contains(&f) {
            known.push(f);
        }
    }
}

/// SIGINT the worker, escalate to SIGKILL after 12 s, then kill any recorded
/// descendant that is still the same process (start time matches).
pub fn stop_owned(child: &mut Child, known: &[Owned]) {
    if matches!(child.try_wait(), Ok(None)) {
        unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
        let deadline = Instant::now() + Duration::from_secs(12);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            sleep(Duration::from_millis(100));
        }
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    for o in known.iter().rev() {
        if start_time(o.pid) == Some(o.start) {
            unsafe { libc::kill(o.pid, libc::SIGKILL) };
        }
    }
}

/// Worker argv: this binary's `agent`, `image` or `video` subcommand.
pub fn command(request: &Json, dir: &Path) -> Result<Vec<String>, String> {
    let r = root();
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            r.join("shared/rust-harness/target/release/local-agents-rs")
                .to_string_lossy()
                .into_owned()
        });
    let s = |k: &str| {
        request
            .get(k)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let i = |k: &str| {
        request
            .get(k)
            .and_then(Json::as_i64)
            .unwrap_or_default()
            .to_string()
    };
    let role = s("role");
    if role == "image-studio" || role == "video-studio" {
        let kind = role.trim_end_matches("-studio");
        let ext = if kind == "image" { "png" } else { "mp4" };
        let mut args = vec![exe, kind.to_string(), s("task")];
        let preset = request
            .get("media_preset")
            .and_then(Json::as_str)
            .unwrap_or("draft");
        let reference = request.get("reference_image").and_then(Json::as_str);
        args.extend(media_arguments(&role, preset, reference)?);
        args.extend([
            "--output".into(),
            dir.join(format!("output.{ext}"))
                .to_string_lossy()
                .into_owned(),
        ]);
        return Ok(args);
    }
    // Agent jobs run on the native Rust loop and tools (this same binary).
    let mut args = vec![
        exe,
        "agent".into(),
        role,
        s("task"),
        "--run-dir".into(),
        dir.join("agent").to_string_lossy().into_owned(),
        "--max-turns".into(),
        i("max_turns"),
        "--timeout".into(),
        i("timeout_seconds"),
        "--dispatch-limit".into(),
        i("dispatch_limit"),
    ];
    for h in request
        .get("allowed_hosts")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(h) = h.as_str() {
            args.extend(["--allow-host".into(), h.into()]);
        }
    }
    Ok(args)
}

fn regular_file(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

fn collect_result(dir: &Path, code: Option<i32>) -> Json {
    let mut artifacts = vec![];
    for name in [
        "output.png",
        "output.mp4",
        "output.evidence.json",
        "output.resources.json",
    ] {
        let p = dir.join(name);
        if regular_file(&p) {
            artifacts.push(json!({"name": name, "path": p.to_string_lossy(), "bytes": fs::metadata(&p).map(|m| m.len()).unwrap_or(0)}));
        }
    }
    let mut result = json!({"returncode": code, "artifacts": []});
    let record = dir.join("agent/result.json");
    if regular_file(&record) {
        if let Ok(data) = fs::read_to_string(&record)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Json>(&t).map_err(|e| e.to_string()))
        {
            let mut agent = Map::new();
            for k in [
                "status",
                "answer",
                "error",
                "seconds",
                "child_results",
                "remaining_queued",
            ] {
                agent.insert(k.into(), data.get(k).cloned().unwrap_or(Json::Null));
            }
            let canon_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            for c in data
                .get("child_results")
                .and_then(Json::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(out) = c.pointer("/result/output").and_then(Json::as_str) {
                    if let Ok(p) = PathBuf::from(out).canonicalize() {
                        if p.starts_with(&canon_dir) && p.is_file() {
                            artifacts.push(json!({"name": p.file_name().map(|n| n.to_string_lossy().into_owned()), "path": p.to_string_lossy(), "bytes": fs::metadata(&p).map(|m| m.len()).unwrap_or(0)}));
                        }
                    }
                }
            }
            result["agent"] = Json::Object(agent);
        }
    }
    result["artifacts"] = Json::Array(artifacts);
    result
}

pub type Builder = fn(&Json, &Path) -> Result<Vec<String>, String>;

pub fn supervise(store: &JobStore, job_id: &str, builder: Builder) -> Result<(), String> {
    let job = store.get(job_id)?;
    let dir = store.job_dir(job_id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if job.cancel {
        return store.update(
            job_id,
            Some("cancelled"),
            Some(&json!({"worker_started": false})),
        );
    }
    let mut known: Vec<Owned> = vec![];
    let mut child: Option<Child> = None;
    let outcome = (|| -> Result<(), String> {
        let argv = builder(&job.request, &dir)?;
        let log_path = dir.join("worker.log");
        let log = File::create(&log_path).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(root())
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let c = child.insert(cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?);
        store.update(job_id, Some("running"), None)?;
        let timeout = job
            .request
            .get("timeout_seconds")
            .and_then(Json::as_u64)
            .unwrap_or(900);
        let deadline = Instant::now() + Duration::from_secs(timeout);
        let mut beat = Instant::now();
        let pid = c.id() as i32;
        let code = loop {
            if let Some(status) = c.try_wait().map_err(|e| e.to_string())? {
                break status.code();
            }
            merge(&mut known, descendants(pid));
            if fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0) > LOG_BUDGET {
                return Err("Worker log exceeded the 16 MiB job budget".into());
            }
            let current = store.get(job_id)?;
            if current.cancel || Instant::now() >= deadline {
                stop_owned(c, &known);
                let code = c.try_wait().ok().flatten().and_then(|s| s.code());
                let state = if current.cancel {
                    "cancelled"
                } else {
                    "timed_out"
                };
                return store.update(job_id, Some(state), Some(&json!({"returncode": code})));
            }
            if beat.elapsed() >= Duration::from_secs(2) {
                store.update(job_id, None, None)?;
                beat = Instant::now();
            }
            sleep(Duration::from_millis(300));
        };
        // Process exit is execution evidence, not task acceptance or media review.
        let state = if code == Some(0) {
            "execution_completed"
        } else {
            "failed"
        };
        store.update(job_id, Some(state), Some(&collect_result(&dir, code)))
    })();
    if let Some(c) = child.as_mut() {
        stop_owned(c, &known);
    }
    if let Err(e) = &outcome {
        let current = store.get(job_id).map(|j| j.state).unwrap_or_default();
        if crate::jobs::ACTIVE.contains(&current.as_str()) {
            store.update(job_id, Some("failed"), Some(&json!({"error": e})))?;
        }
    }
    outcome
}

/// Spawn `<this exe> supervise <job_id>` detached in its own session.
pub fn spawn_supervisor(store: &JobStore, job_id: &str) -> Result<(), String> {
    let dir = store.job_dir(job_id);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .or_else(|e| if dir.is_dir() { Ok(()) } else { Err(e) })
        .map_err(|e| e.to_string())?;
    let log = File::create(dir.join("supervisor.log")).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("supervise")
        .arg(job_id)
        .env("LOCAL_AGENTS_ROOT", root())
        .env("LOCAL_AGENTS_JOBS", &store.base)
        .current_dir(root())
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Reap the detached supervisor so a long-lived bridge never accumulates zombies.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

use std::os::unix::fs::DirBuilderExt;
