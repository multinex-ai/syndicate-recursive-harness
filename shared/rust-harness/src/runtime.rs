//! Port of shared/harness/runtime.py plus the server path of media_policy.MediaGuard:
//! the single-flight heavy-model lock, resource admission and watchdog, and the
//! loopback llama.cpp (Metal) server lifecycle.
use crate::jobs::root;
use serde_json::{json, Value as Json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

mod resources;

// Keep the historical `crate::runtime::*` surface stable while implementation
// details live in the cohesive resource-control module.
#[allow(unused_imports)]
pub use resources::{
    admission_reason, available_bytes, footprint_bytes, max_swap_growth, memory_warning,
    pressure_normal, resident_bytes, runtime_reason, save_resources, spawn_watchdog,
    terminate_group, warning_tolerance, RestoreGuard, Sample, Watchdog,
};

pub const MODEL_REL: &str =
    "shared/models/qwen35-9b-uncensored/Qwen3.5-9B-Uncensored-HauhauCS-Aggressive-Q4_K_M.gguf";
pub const PROJECTOR_REL: &str =
    "shared/models/qwen35-9b-uncensored/mmproj-Qwen3.5-9B-Uncensored-HauhauCS-Aggressive-BF16.gguf";
pub const PORT: u16 = 8087;
pub const LOCK_CONTRACT_VERSION: u32 = 1;

// ---------- heavy-model lock ----------

/// Exclusive machine-level flock; every checkout and every compatible Python
/// worker resolves the same path so two harness clones cannot load heavy models
/// concurrently.
pub struct HeavyLock {
    file: File,
}

impl HeavyLock {
    pub fn acquire(path: &Path, timeout: Duration) -> Result<HeavyLock, String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(HeavyLock { file });
            }
            if Instant::now() >= deadline {
                return Err("Another local heavy-model worker is active.".into());
            }
            sleep(Duration::from_millis(250));
        }
    }

    pub fn default_path() -> PathBuf {
        let configured = std::env::var("SYNDICATE_HEAVY_LOCK").ok();
        let state = std::env::var("XDG_STATE_HOME").ok();
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        resolve_heavy_lock_path(configured.as_deref(), state.as_deref(), home.as_deref())
    }
}

pub(crate) fn resolve_heavy_lock_path(
    configured: Option<&str>,
    state: Option<&str>,
    home: Option<&Path>,
) -> PathBuf {
    let absolute = |raw: &str| {
        let candidate = PathBuf::from(raw.trim());
        (!raw.trim().is_empty() && candidate.is_absolute()).then_some(candidate)
    };
    if let Some(candidate) = configured.and_then(absolute) {
        return candidate;
    }
    if let Some(candidate) = state.and_then(absolute) {
        return candidate.join("local-syndicate-harness/heavy-model.lock");
    }
    home.map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/local-syndicate-harness/heavy-model.lock")
}

impl Drop for HeavyLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

// ---------- loopback HTTP ----------

/// Minimal authenticated HTTP/1.1 JSON client for the loopback model server only.
pub(crate) fn request_json_authenticated(
    port: u16,
    path: &str,
    payload: Option<&Json>,
    timeout: Duration,
    api_key: Option<&str>,
) -> Result<Json, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, timeout.min(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let body = payload.map(|p| p.to_string()).unwrap_or_default();
    let authorization = api_key
        .map(|key| format!("Authorization: Bearer {key}\r\n"))
        .unwrap_or_default();
    let head = match payload {
        Some(_) => format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{authorization}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
        None => format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{authorization}Connection: close\r\n\r\n"),
    };
    s.write_all(head.as_bytes())
        .and_then(|_| s.write_all(body.as_bytes()))
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => format!(
            "TimeoutError: model server gave no answer within {} s",
            timeout.as_secs()
        ),
        _ => e.to_string(),
    })?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let headers = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut content = raw[split + 4..].to_vec();
    let status: u16 = headers
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or("missing HTTP status")?;
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        content = dechunk(&content)?;
    }
    if status >= 400 {
        let text: String = String::from_utf8_lossy(&content)
            .chars()
            .take(400)
            .collect();
        return Err(format!("HTTP {status}: {text}"));
    }
    serde_json::from_slice(&content).map_err(|e| format!("invalid JSON from model server: {e}"))
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n").ok_or("bad chunk")?;
        let size_text = String::from_utf8_lossy(&b[..eol]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

fn port_in_use(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
}

// ---------- model server ----------

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub program: PathBuf,
    pub model: PathBuf,
    pub projector: PathBuf,
    pub port: u16,
    pub vision: bool,
    pub context_size: u32,
    pub low_memory: bool,
    pub lock_path: PathBuf,
    pub lock_timeout: Duration,
    pub load_timeout: Duration,
    pub guard: bool,
    pub api_key: String,
    /// Opt-in memory reclaim (quit listed apps) before admission.
    pub reclaim: Option<crate::reclaim::Policy>,
    /// Test hook: replaces the llama argv entirely (program still from `program`).
    pub args_override: Option<Vec<String>>,
}

impl ServerConfig {
    pub fn qwen(vision: bool) -> ServerConfig {
        let r = root();
        ServerConfig {
            program: std::env::var("LLAMA_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|_| "/opt/homebrew/bin/llama".into()),
            model: r.join(MODEL_REL),
            projector: r.join(PROJECTOR_REL),
            port: PORT,
            vision,
            // A 16K default leaves room for tool schemas, a bounded sliding window, and output reserve.
            context_size: std::env::var("LOCAL_AGENTS_CONTEXT_SIZE")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .map(|n| n.clamp(4096, 32768))
                .unwrap_or(16384),
            // Opt-in: no host-RAM prompt cache and smaller batches, for runs on a Mac already near its memory limit.
            low_memory: std::env::var("LOCAL_AGENTS_LOW_MEMORY")
                .map(|v| v == "1")
                .unwrap_or(false),
            lock_path: HeavyLock::default_path(),
            // Opt-in queueing: long pipelines (a syndicate that starts narration jobs between dispatches) set
            // LOCAL_AGENTS_LOCK_WAIT_SECONDS so the next agent waits its turn instead of failing. Default: no wait.
            lock_timeout: Duration::from_secs(
                std::env::var("LOCAL_AGENTS_LOCK_WAIT_SECONDS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    .min(3600),
            ),
            load_timeout: Duration::from_secs(180),
            guard: true,
            api_key: crate::jobs::new_job_id()
                .expect("the local model server requires OS random bytes for its loopback API key"),
            reclaim: crate::reclaim::Policy::load(&r).ok().flatten(),
            args_override: None,
        }
    }

    /// Resolve a role's declared model profile (`config.json["model_profile_id"]` or `MODEL_PROFILE.txt`
    /// -> `models/<profile_id>.json`). Fails closed when a profile declares a non-local provider that
    /// is not enabled in the local runtime.
    pub fn for_role(root: &Path, role: &str, vision: bool) -> Result<ServerConfig, String> {
        let mut cfg = Self::qwen(vision);
        let Some(dir) = crate::agent::role_dir(root, role) else {
            return Ok(cfg);
        };
        let config_profile = fs::read_to_string(dir.join("config.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Json>(&t).ok())
            .and_then(|v| {
                v.get("model_profile_id")
                    .and_then(Json::as_str)
                    .map(String::from)
            });
        let file_profile = fs::read_to_string(dir.join("MODEL_PROFILE.txt"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let profile_id = config_profile
            .or(file_profile)
            .unwrap_or_else(|| "local-qwen35-9b-uncensored".into());
        let profile_path = root.join("models").join(format!("{profile_id}.json"));
        if !profile_path.is_file() {
            if root.join("models").is_dir() {
                return Err(format!(
                    "Unknown model profile '{profile_id}' for role '{role}' ({} not found)",
                    profile_path.display()
                ));
            }
            return Ok(cfg);
        }
        let raw = fs::read_to_string(&profile_path)
            .map_err(|e| format!("{}: {e}", profile_path.display()))?;
        let profile: Json =
            serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", profile_path.display()))?;
        let provider = profile
            .get("provider_kind")
            .and_then(Json::as_str)
            .unwrap_or("local_gguf");
        if provider != "local_gguf" {
            return Err(format!(
                "Model profile '{profile_id}' for role '{role}' requires provider_kind '{provider}', which is disabled until a hosted provider adapter is configured"
            ));
        }
        if let Some(m) = profile
            .get("model_ref")
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty())
        {
            cfg.model = root.join(m);
        }
        if let Some(p) = profile
            .get("mmproj_ref")
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty())
        {
            cfg.projector = root.join(p);
        }
        if std::env::var("LOCAL_AGENTS_CONTEXT_SIZE").is_err() {
            if let Some(ctx) = profile.get("context_window").and_then(Json::as_u64) {
                cfg.context_size = (ctx as u32).clamp(4096, 32768);
            }
        }
        Ok(cfg)
    }

    /// Identical to the argv runtime.py builds.
    pub fn args(&self) -> Vec<String> {
        if let Some(a) = &self.args_override {
            return a.clone();
        }
        let mut a: Vec<String> = ["serve", "-m"].iter().map(|s| s.to_string()).collect();
        a.push(self.model.to_string_lossy().into_owned());
        for s in ["--alias", "local-agent", "--host", "127.0.0.1", "--port"] {
            a.push(s.into());
        }
        a.push(self.port.to_string());
        a.push("-c".into());
        a.push(self.context_size.to_string());
        for s in [
            "-np",
            "1",
            "-ngl",
            "99",
            "-fa",
            "on",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q8_0",
            "--jinja",
            "--reasoning",
            "off",
        ] {
            a.push(s.into());
        }
        if self.low_memory {
            // A bounded prompt cache, not zero: this model needs it to reuse the shared prefix between turns,
            // and without reuse every turn reprocesses the whole prompt (measured ~4x slower).
            for s in [
                "--batch-size",
                "256",
                "--ubatch-size",
                "64",
                "--cache-ram",
                "1024",
            ] {
                a.push(s.into());
            }
        }
        a.push("--api-key".into());
        a.push(self.api_key.clone());
        if self.vision {
            a.push("--mmproj".into());
            a.push(self.projector.to_string_lossy().into_owned());
        }
        a
    }
}

/// A running, healthy model server. Dropping it terminates the server, records
/// the final state and releases the heavy lock (in that order).
pub struct ModelServer {
    pub port: u16,
    child: Child,
    run_dir: PathBuf,
    record: Json,
    watchdog: Option<Watchdog>,
    resources_path: PathBuf,
    samples: Arc<Mutex<Vec<Json>>>,
    api_key: String,
    _restore: RestoreGuard,
    _lock: HeavyLock,
}

fn write_json(path: &Path, v: &Json) {
    let _ = fs::write(path, serde_json::to_string_pretty(v).unwrap_or_default());
}
impl ModelServer {
    pub fn start(cfg: &ServerConfig, run_dir: &Path) -> Result<ModelServer, String> {
        let lock = HeavyLock::acquire(&cfg.lock_path, cfg.lock_timeout)?;
        fs::create_dir_all(run_dir).map_err(|e| e.to_string())?;
        let resources_path = run_dir.join("model.resources.json");
        let policy = cfg
            .reclaim
            .clone()
            .filter(|p| p.target_available_bytes > 0 && (p.auto_largest || !p.apps.is_empty()));
        let restore = RestoreGuard::new(policy.as_ref(), run_dir.join("memory-reclaim.json"));
        if let Some(policy) = &policy {
            restore.reclaim_before(policy);
        }
        let samples = Arc::new(Mutex::new(Vec::new()));
        let initial = if cfg.guard {
            let s = Sample::take(None)?;
            samples.lock().unwrap().push(s.to_json());
            if let Some(reason) = admission_reason(&s) {
                save_resources(
                    &resources_path,
                    "admission_blocked",
                    Some(reason),
                    &samples.lock().unwrap(),
                );
                return Err(reason.into());
            }
            save_resources(&resources_path, "running", None, &samples.lock().unwrap());
            Some(s)
        } else {
            None
        };
        if cfg.args_override.is_none() {
            if !cfg.model.is_file() {
                return Err(format!(
                    "Model download is incomplete: {}",
                    cfg.model.display()
                ));
            }
            if cfg.vision && !cfg.projector.is_file() {
                return Err("Vision projector download is incomplete".into());
            }
        }
        if port_in_use(cfg.port) {
            return Err(format!(
                "Port {} is already occupied; refusing to reuse an unverified server.",
                cfg.port
            ));
        }
        let log = File::create(run_dir.join("server.log")).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&cfg.program);
        cmd.args(cfg.args())
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("could not start model server: {e}"))?;
        let record = json!({"pid": child.id(), "endpoint": format!("http://127.0.0.1:{}", cfg.port), "vision": cfg.vision,
                            "state": "starting", "context_size": cfg.context_size, "low_memory": cfg.low_memory, "controller": "rust"});
        let in_run = policy.map(|p| (p, restore.clone()));
        let mut server = ModelServer {
            port: cfg.port,
            child,
            run_dir: run_dir.to_path_buf(),
            record,
            watchdog: None,
            resources_path,
            samples,
            api_key: cfg.api_key.clone(),
            _restore: restore,
            _lock: lock,
        };
        server.save_record();
        if let Some(initial) = initial {
            server.watchdog = Some(server.spawn_watchdog(initial, in_run));
        }
        let deadline = Instant::now() + cfg.load_timeout;
        loop {
            if let Ok(Some(_)) = server.child.try_wait() {
                return Err(format!(
                    "Model server exited; inspect {}",
                    run_dir.join("server.log").display()
                ));
            }
            if let Some(r) = server.stop_reason() {
                return Err(r);
            }
            if let Ok(h) = request_json_authenticated(
                cfg.port,
                "/health",
                None,
                Duration::from_secs(2),
                Some(&cfg.api_key),
            ) {
                if h.get("status").and_then(Json::as_str) == Some("ok") {
                    break;
                }
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "Model loading exceeded {} seconds",
                    cfg.load_timeout.as_secs()
                ));
            }
            sleep(Duration::from_millis(250));
        }
        server.record["state"] = json!("healthy");
        server.save_record();
        Ok(server)
    }

    fn save_record(&self) {
        write_json(&self.run_dir.join("server-process.json"), &self.record);
    }

    /// Samples every 2 s. On a memory warning it first reclaims (quits the next
    /// eligible app) when a reclaim policy is set; if the warning lasts 6 s with
    /// nothing left to reclaim, or a hard limit is breached, it stops the server's
    /// process group.
    fn spawn_watchdog(
        &self,
        initial: Sample,
        in_run: Option<(crate::reclaim::Policy, RestoreGuard)>,
    ) -> Watchdog {
        spawn_watchdog(
            self.child.id() as i32,
            initial,
            in_run,
            self.samples.clone(),
            self.resources_path.clone(),
        )
    }

    /// Why the watchdog stopped the server, if it did.
    pub fn stop_reason(&self) -> Option<String> {
        self.watchdog.as_ref().and_then(Watchdog::reason)
    }

    pub fn request(
        &self,
        path: &str,
        payload: Option<&Json>,
        timeout: Duration,
    ) -> Result<Json, String> {
        if let Some(r) = self.stop_reason() {
            return Err(r);
        }
        request_json_authenticated(self.port, path, payload, timeout, Some(&self.api_key))
    }

    pub(crate) fn request_api_key(&self) -> String {
        self.api_key.clone()
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        let reason = self.stop_reason();
        if let Some(w) = self.watchdog.as_mut() {
            w.finish();
        }
        terminate_group(&mut self.child);
        self.record["state"] = json!("terminated");
        self.record["returncode"] =
            json!(self.child.try_wait().ok().flatten().and_then(|s| s.code()));
        self.save_record();
        if self.watchdog.is_some() {
            if let Ok(s) = Sample::take(None) {
                self.samples.lock().unwrap().push(s.to_json());
            }
            let status = if reason.is_some() {
                "resource_stopped"
            } else {
                "completed"
            };
            save_resources(
                &self.resources_path,
                status,
                reason.as_deref(),
                &self.samples.lock().unwrap(),
            );
        }
        // _lock is released after this body, i.e. after the server is gone.
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn resolver_loads_local_gguf_profile_and_rejects_unconfigured_hosted_provider() {
        let root = crate::jobs::root();
        let cfg = ServerConfig::for_role(&root, "coordinator", false).unwrap();
        assert!(cfg
            .model
            .to_string_lossy()
            .ends_with("Qwen3.5-9B-Uncensored-HauhauCS-Aggressive-Q4_K_M.gguf"));
        assert_eq!(cfg.context_size, 16384);

        let tmp = std::env::temp_dir().join(format!(
            "syndicate-profile-test-{}",
            crate::jobs::now() as u64
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("agents/cloud-worker")).unwrap();
        fs::create_dir_all(tmp.join("models")).unwrap();
        fs::write(
            tmp.join("agents/cloud-worker/MODEL_PROFILE.txt"),
            "cloudflare-qwen-orchestrator\n",
        )
        .unwrap();
        fs::copy(
            root.join("models/cloudflare-qwen-orchestrator.json"),
            tmp.join("models/cloudflare-qwen-orchestrator.json"),
        )
        .unwrap();
        let err = ServerConfig::for_role(&tmp, "cloud-worker", false).unwrap_err();
        assert!(err.contains("cloudflare_workers_ai"));
        let _ = fs::remove_dir_all(&tmp);
    }
}
