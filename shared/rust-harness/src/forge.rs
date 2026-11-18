//! The Forge server: a local control plane for the Syndicate Forge console.
//!
//! Binds 127.0.0.1 ONLY — never 0.0.0.0, never a public interface — and requires a random bearer
//! token (written once, 0600, to `shared/runtime/forge/token`) on every request but the static
//! console shell. This machine's user is the only intended caller. There is no remote, cloud or
//! SSH dispatch anywhere in this file or reachable from it: `spawn_mission` always execs the local
//! `local-agents-rs` binary as a child *of this process, on this machine*, exactly like the `agent`
//! CLI command already does.
//!
//! No framework: a hand-rolled HTTP/1.1 parser and router, matching the style of the loopback
//! client in runtime.rs. One thread per connection; fine for an operator console on localhost.
use crate::jobs::now;
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Missions THIS server session started: job_id -> (pid, run dir relative to root). Only these can be
/// stopped through the API, so the endpoint can never be pointed at an arbitrary process.
fn missions() -> &'static Mutex<HashMap<String, (i32, String)>> {
    static M: OnceLock<Mutex<HashMap<String, (i32, String)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn dir(root: &Path) -> PathBuf {
    root.join("shared/runtime/forge")
}
fn token_path(root: &Path) -> PathBuf {
    dir(root).join("token")
}

/// The console's own bearer token. Created on first run (random, 0600); reused after that.
pub fn token(root: &Path) -> Result<String, String> {
    let p = token_path(root);
    if let Ok(t) = std::fs::read_to_string(&p) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    std::fs::create_dir_all(dir(root)).map_err(|e| e.to_string())?;
    let bytes: [u8; 24] = std::array::from_fn(|i| {
        // Not cryptographically audited, but local-only and rotated by deleting the file; good enough
        // for a loopback console whose worst case is another local process reading its own user's file.
        let seed = now()
            .to_bits()
            .wrapping_add(i as u64 * 2_654_435_761)
            .wrapping_add(std::process::id() as u64);
        (seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1) >> 33) as u8
    });
    let t = hex::encode(bytes);
    std::fs::write(&p, &t).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(t)
}

// ---------------------------------------------------------------- minimal HTTP/1.1
struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            Some((urldecode(k), urldecode(v)))
        })
        .collect()
}
fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(
                    u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00"), 16)
                        .unwrap_or(b'?'),
                );
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_request(stream: &mut TcpStream, max_body: usize) -> Result<Req, String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 32 * 1024 {
            return Err("headers too large".into());
        }
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = target
        .split_once('?')
        .map(|(p, q)| (p.to_string(), parse_query(q)))
        .unwrap_or((target, HashMap::new()));
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let content_len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .min(max_body);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_len);
    Ok(Req {
        method,
        path,
        query,
        headers,
        body,
    })
}

struct Res {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}
fn json_res(status: u16, v: &Json) -> Res {
    Res {
        status,
        content_type: "application/json; charset=utf-8",
        body: serde_json::to_vec(v).unwrap_or_default(),
    }
}
fn html_res(body: String) -> Res {
    Res {
        status: 200,
        content_type: "text/html; charset=utf-8",
        body: body.into_bytes(),
    }
}
fn err_res(status: u16, msg: &str) -> Res {
    json_res(status, &json!({"ok": false, "error": msg}))
}

fn write_response(stream: &mut TcpStream, res: Res) {
    let reason = match res.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    };
    let head = format!("HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        res.status, res.content_type, res.body.len());
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&res.body);
}

// ---------------------------------------------------------------- mission spawning (THIS MACHINE ONLY)
/// Spawn `local-agents-rs agent <role> <objective> [...]` as a detached child of THIS process, on
/// THIS machine. This is the one and only way the Forge server starts work; it never opens a
/// network connection to do it. A role must resolve to a real base role or syndicate member —
/// anything else is refused before any process is spawned.
/// All the validation and local bookkeeping that does NOT require actually spawning a process:
/// checks the role exists, bounds the objective, makes the run directory and writes acceptance
/// criteria. Pure enough to unit-test without a compiled `local-agents-rs` binary on hand.
fn mission_precheck(
    root: &Path,
    role: &str,
    objective: &str,
    criteria: &Json,
) -> Result<(String, PathBuf), String> {
    if crate::agent::role_dir(root, role).is_none() {
        return Err(format!("Unknown role or syndicate member: {role}"));
    }
    if objective.trim().is_empty() || objective.len() > 16_000 {
        return Err("objective must be 1-16000 characters".into());
    }
    let run_id = crate::jobs::new_job_id()?;
    let run_dir = root
        .join("runs")
        .join(format!("forge-{}-{run_id}", now() as u64));
    std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    if criteria.is_array() {
        std::fs::write(run_dir.join("acceptance.json"), criteria.to_string())
            .map_err(|e| e.to_string())?;
    }
    Ok((run_id, run_dir))
}

/// Spawn `local-agents-rs agent <role> <objective> ...` as a detached child of THIS process, on
/// THIS machine. This is the one and only way the Forge server starts work; it never opens a
/// network connection to do it. `exe` is the `local-agents-rs` binary to run (the running
/// process's own binary in production; a test passes a known path so it never execs the test
/// harness, which does not understand the `agent` subcommand).
fn spawn_mission_with(
    exe: &Path,
    root: &Path,
    role: &str,
    objective: &str,
    criteria: &Json,
) -> Result<Json, String> {
    let (run_id, run_dir) = mission_precheck(root, role, objective, criteria)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["agent", role, objective, "--run-dir"])
        .arg(&run_dir)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start local-agents-rs: {e}"))?;
    let pid = child.id();
    let rel = run_dir
        .strip_prefix(root)
        .unwrap_or(&run_dir)
        .to_string_lossy()
        .into_owned();
    missions()
        .lock()
        .unwrap()
        .insert(run_id.clone(), (pid as i32, rel.clone()));
    // Reaper: wait for the child so it never lingers as a zombie, then forget it (it can no longer be stopped).
    let reap_id = run_id.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        missions().lock().unwrap().remove(&reap_id);
    });
    Ok(json!({"job_id": run_id, "pid": pid, "run_dir": rel, "role": role, "started_unix": now()}))
}

/// Ask a mission this server started to stop (SIGTERM to its process group; the agent loop already
/// treats SIGTERM as a clean cancellation). Unknown job ids are refused.
fn stop_mission(job_id: &str) -> Result<Json, String> {
    let (pid, rel) = missions().lock().unwrap().get(job_id).cloned().ok_or(
        "unknown mission: it was not started by this server session, or it already finished",
    )?;
    #[cfg(unix)]
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    Ok(json!({"stopping": true, "job_id": job_id, "pid": pid, "run_dir": rel}))
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!(
            "{}… [+{} chars]",
            s.chars().take(n).collect::<String>(),
            s.chars().count() - n
        )
    }
}

/// A run directory named by a span attribute, resolved ONLY if it really lives under `<root>/runs`.
fn safe_run_dir(root: &Path, rel: &str) -> Option<PathBuf> {
    let base = root.join("runs").canonicalize().ok()?;
    let dir = root.join(rel).canonicalize().ok()?;
    if dir.starts_with(&base) && dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

fn read_capped(p: &Path, max: u64) -> Option<Json> {
    if std::fs::metadata(p).ok()?.len() > max {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// The operator's own on-demand drill-down for one run: the exact prompt it was given and its response
/// cycles. Local-only and token-gated; kept out of telemetry spans and reports on purpose. Long text is truncated.
fn run_detail(root: &Path, rel: &str) -> Json {
    let Some(dir) = safe_run_dir(root, rel) else {
        return json!({"available": false, "reason": "run folder not found under runs/"});
    };
    let prompt = read_capped(&dir.join("prompt.json"), 256 * 1024).map(|p| json!({
        "system": trunc(p["system"].as_str().unwrap_or(""), 20_000), "objective": trunc(p["objective"].as_str().unwrap_or(""), 8_000),
        "officiated": p["officiated"], "system_sha256": p["system_sha256"]}));
    let rec = read_capped(&dir.join("result.json"), 2 * 1024 * 1024);
    let record = rec.as_ref().map(|r| json!({
        "status": r["status"], "seconds": r["seconds"], "answer": trunc(r["answer"].as_str().unwrap_or(""), 6_000),
        "reward": r["reward"], "status_reports": r["status_reports"],
        "turns": r["turns"].as_array().into_iter().flatten().map(|t| json!({
            "turn": t["turn"], "finish_reason": t["finish_reason"], "usage": t["usage"], "decode_tps": t["timings"]["predicted_per_second"],
            "content": trunc(t["message"]["content"].as_str().unwrap_or(""), 3_000),
            "tool_calls": t["message"]["tool_calls"].as_array().map(|c| c.iter().filter_map(|x| x["function"]["name"].as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default()
        })).collect::<Vec<_>>(),
        "tools": r["tools"].as_array().into_iter().flatten().map(|t| json!({
            "tool": t["tool"], "seconds": t["seconds"], "ok": t["response"]["ok"],
            "arguments": trunc(&t["arguments"].to_string(), 1_500), "error": t["response"]["error"].as_str().map(|e| trunc(e, 600))
        })).collect::<Vec<_>>()}));
    json!({"available": prompt.is_some() || record.is_some(), "prompt": prompt, "record": record,
           "note": if prompt.is_none() { "No prompt.json: this run predates prompt capture." } else { "" }})
}

fn machine() -> Json {
    let lock_free = crate::runtime::HeavyLock::acquire(
        &crate::runtime::HeavyLock::default_path(),
        std::time::Duration::ZERO,
    )
    .is_ok();
    match crate::runtime::Sample::take(None) {
        Ok(s) => {
            json!({"ok": true, "time": now(), "sample": s.to_json(), "admission_blocked_reason": crate::runtime::admission_reason(&s), "model_lock_held": !lock_free,
                   "model_lock_path": crate::runtime::HeavyLock::default_path(), "lock_contract_version": crate::runtime::LOCK_CONTRACT_VERSION})
        }
        Err(e) => json!({"ok": false, "error": e, "model_lock_held": !lock_free,
                         "model_lock_path": crate::runtime::HeavyLock::default_path(), "lock_contract_version": crate::runtime::LOCK_CONTRACT_VERSION}),
    }
}
fn spawn_mission(
    root: &Path,
    role: &str,
    objective: &str,
    criteria: &Json,
) -> Result<Json, String> {
    spawn_mission_with(
        &std::env::current_exe().map_err(|e| e.to_string())?,
        root,
        role,
        objective,
        criteria,
    )
}

// ---------------------------------------------------------------- routing
fn route(root: &Path, req: &Req) -> Res {
    let p = normalize(&req.path);
    match (req.method.as_str(), p) {
        ("GET", "/api/summary") => {
            let since = req
                .query
                .get("since")
                .and_then(|s| crate::telemetry::parse_window(s))
                .map(|w| now() - w)
                .unwrap_or(0.0);
            json_res(
                200,
                &crate::telemetry::summarize(
                    root,
                    root,
                    since,
                    now() + 1.0,
                    req.query.get("syndicate").map(String::as_str),
                ),
            )
        }
        ("GET", "/api/roster") => json_res(
            200,
            &json!({"roster": crate::telemetry::roster(root), "tools": crate::syndicate::tool_catalog(),
            "relations": crate::syndicate::guide(), "relation_ids": crate::syndicate::RELATIONS, "base_roles": crate::syndicate::BASE_ROLES}),
        ),
        ("GET", "/api/report") => {
            let since = req
                .query
                .get("since")
                .and_then(|s| crate::telemetry::parse_window(s))
                .map(|w| now() - w);
            match crate::report::generate(
                root,
                req.query.get("syndicate").map(String::as_str),
                since,
            ) {
                Ok(path) => std::fs::read_to_string(&path)
                    .map(html_res)
                    .unwrap_or_else(|e| err_res(500, &e.to_string())),
                Err(e) => err_res(500, &e),
            }
        }
        ("GET", "/api/run") => match req.query.get("trace") {
            Some(t)
                if t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
            {
                let spans = crate::telemetry::read_spans(root, 0.0, now() + 1.0)
                    .into_iter()
                    .filter(|s| s["trace_id"] == *t)
                    .collect::<Vec<_>>();
                let runs: Vec<Json> = spans.iter().filter(|s| s["kind"] == "invoke_agent").map(|s| {
                    let rel = s["attrs"]["harness.run_dir"].as_str().unwrap_or("");
                    json!({"span_id": s["span_id"], "agent": s["agent"], "run_dir": rel, "detail": run_detail(root, rel)})
                }).collect();
                json_res(200, &json!({"trace_id": t, "spans": spans, "runs": runs}))
            }
            _ => err_res(400, "query parameter 'trace' is required"),
        },
        ("GET", "/api/bundle") => match req.query.get("dir") {
            Some(d) => crate::syndicate::load_bundle(root, d)
                .map(|b| json_res(200, &b))
                .unwrap_or_else(|e| err_res(404, &e)),
            None => err_res(400, "query parameter 'dir' is required"),
        },
        ("GET", "/api/guide") => json_res(200, &crate::syndicate::guide()),
        ("GET", "/api/tutorial") => json_res(200, &crate::assist::tutorial()),
        ("POST", "/api/assist/generate") => {
            let v: Json = serde_json::from_slice(&req.body).unwrap_or(Json::Null);
            let run_dir = root.join("runs").join(format!("assist-{}", now() as u64));
            match crate::assist::generate_syndicate(
                root,
                &crate::runtime::ServerConfig::qwen(false),
                &run_dir,
                v["description"].as_str().unwrap_or(""),
            ) {
                Ok(r) => json_res(200, &r),
                Err(e) => err_res(400, &e),
            }
        }
        ("POST", "/api/assist/explain") => {
            let v: Json = serde_json::from_slice(&req.body).unwrap_or(Json::Null);
            let run_dir = root.join("runs").join(format!("assist-{}", now() as u64));
            match crate::assist::explain(
                root,
                &crate::runtime::ServerConfig::qwen(false),
                &run_dir,
                v["topic"].as_str().unwrap_or(""),
            ) {
                Ok(r) => json_res(200, &r),
                Err(e) => err_res(400, &e),
            }
        }
        ("POST", "/api/assist/persona") => {
            let v: Json = serde_json::from_slice(&req.body).unwrap_or(Json::Null);
            let run_dir = root.join("runs").join(format!("assist-{}", now() as u64));
            match crate::assist::persona(&crate::runtime::ServerConfig::qwen(false), &run_dir, &v) {
                Ok(r) => json_res(200, &r),
                Err(e) => err_res(400, &e),
            }
        }
        ("POST", "/api/validate") => with_bundle(&req.body, |b| {
            let errs = crate::syndicate::validate(root, b);
            let notes = crate::syndicate::notes(root, b);
            let preview = crate::syndicate::preview(root, b);
            json!({"ok": errs.is_empty(), "errors": errs, "notes": notes, "officiation_preview": preview})
        }),
        ("POST", "/api/apply") => with_bundle(&req.body, |b| {
            let dry_run = req
                .query
                .get("dry_run")
                .map(|v| v != "false")
                .unwrap_or(true);
            match crate::syndicate::apply(root, b, dry_run) {
                Ok(r) => json!({"ok": true, "result": r}),
                Err(e) => json!({"ok": false, "error": e}),
            }
        }),
        ("POST", "/api/mission") => {
            let v: Json = serde_json::from_slice(&req.body).unwrap_or(Json::Null);
            let role = v["role"].as_str().unwrap_or("");
            let objective = v["objective"].as_str().unwrap_or("");
            match spawn_mission(root, role, objective, &v["acceptance_criteria"]) {
                Ok(r) => json_res(201, &json!({"ok": true, "mission": r})),
                Err(e) => err_res(400, &e),
            }
        }
        ("GET", "/api/mission/status") => {
            let job = req.query.get("job_id").map(String::as_str).unwrap_or("");
            let running = missions()
                .lock()
                .map(|m| m.contains_key(job))
                .unwrap_or(false);
            let rel = req.query.get("run_dir").map(String::as_str).unwrap_or("");
            json_res(
                200,
                &json!({"job_id": job, "running": running, "detail": run_detail(root, rel)}),
            )
        }
        ("GET", "/api/machine") => json_res(200, &machine()),
        ("POST", "/api/mission/stop") => {
            let v: Json = serde_json::from_slice(&req.body).unwrap_or(Json::Null);
            match stop_mission(v["job_id"].as_str().unwrap_or("")) {
                Ok(r) => json_res(200, &json!({"ok": true, "result": r})),
                Err(e) => err_res(404, &e),
            }
        }
        ("GET", "/api/health") => json_res(200, &json!({"ok": true, "time": now()})),
        _ => err_res(404, "no such route"),
    }
}

fn with_bundle(body: &[u8], f: impl FnOnce(&Json) -> Json) -> Res {
    match serde_json::from_slice::<Json>(body) {
        Ok(b) if b.is_object() => json_res(200, &f(&b)),
        Ok(_) => err_res(400, "body must be a JSON object"),
        Err(e) => err_res(400, &format!("bad JSON: {e}")),
    }
}

/// Files the static handler may ever serve, and their content type. Deliberately NOT a general
/// directory listing: adding a route here is the only way a file becomes reachable.
const STATIC_FILES: [(&str, &str, &str); 5] = [
    ("/", "index.html", "text/html; charset=utf-8"),
    (
        "/manifest.json",
        "manifest.json",
        "application/manifest+json",
    ),
    ("/sw.js", "sw.js", "application/javascript"),
    ("/app.js", "app.js", "application/javascript"),
    ("/icon.svg", "icon.svg", "image/svg+xml"),
];

fn serve_static(assets: &Path, path: &str) -> Option<Res> {
    let (_, file, ct) = STATIC_FILES.iter().find(|(p, _, _)| *p == path)?;
    let body = std::fs::read(assets.join(file)).ok()?;
    Some(Res {
        status: 200,
        content_type: ct,
        body,
    })
}

/// true for a route the console shell itself needs before it has the token (the page and its static
/// assets, plus the unauthenticated health check); every `/api/*` route still requires the bearer token.
fn is_public(path: &str) -> bool {
    STATIC_FILES.iter().any(|(p, _, _)| *p == path) || path == "/api/health"
}

/// Collapse a trailing slash EXCEPT on the root itself, so "/" and "" are never two different keys.
fn normalize(path: &str) -> &str {
    let t = path.trim_end_matches('/');
    if t.is_empty() {
        "/"
    } else {
        t
    }
}

fn handle(root: &Path, assets: &Path, tok: &str, stream: &mut TcpStream) {
    let req = match read_request(stream, 2 * 1024 * 1024) {
        Ok(r) => r,
        Err(_) => return,
    };
    let path = normalize(&req.path);
    if !is_public(path) {
        let got = req
            .headers
            .get("authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .unwrap_or("");
        if got != tok {
            write_response(stream, err_res(401, "missing or wrong bearer token"));
            return;
        }
    }
    let res = if path.starts_with("/api/") {
        route(root, &req)
    } else {
        serve_static(assets, path).unwrap_or_else(|| err_res(404, "not found"))
    };
    write_response(stream, res);
}

#[cfg(test)]
pub fn test_precheck(
    root: &Path,
    role: &str,
    objective: &str,
    criteria: &Json,
) -> Result<(String, PathBuf), String> {
    mission_precheck(root, role, objective, criteria)
}

/// Serve forever on 127.0.0.1:`port` (0 = OS-assigned). Returns the bound port and a stop flag the
/// caller can set to make the accept loop exit after its next connection (used by tests).
pub fn serve(root: PathBuf, assets: PathBuf, port: u16) -> Result<(u16, Arc<AtomicBool>), String> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map_err(|e| e.to_string())?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?.port();
    let tok = token(&root)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).ok();
        loop {
            if stop2.load(Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok((mut s, _)) => {
                    let (root, assets, tok) = (root.clone(), assets.clone(), tok.clone());
                    std::thread::spawn(move || handle(&root, &assets, &tok, &mut s));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(40))
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
    });
    Ok((bound, stop))
}

#[cfg(test)]
pub fn test_run_detail(root: &Path, rel: &str) -> Json {
    run_detail(root, rel)
}
#[cfg(test)]
pub fn test_spawn_with(
    exe: &Path,
    root: &Path,
    role: &str,
    objective: &str,
) -> Result<Json, String> {
    spawn_mission_with(exe, root, role, objective, &Json::Null)
}
#[cfg(test)]
pub fn test_stop(job_id: &str) -> Result<Json, String> {
    stop_mission(job_id)
}
#[cfg(test)]
pub fn test_registered(job_id: &str) -> bool {
    missions().lock().unwrap().contains_key(job_id)
}
