//! STDIO MCP bridge (newline-delimited JSON-RPC 2.0). Same five tools, names,
//! argument shapes and annotations as shared/harness/mcp_server.py.
use crate::jobs::{prerequisites, root, JobStore, ROLES};
use crate::supervisor::spawn_supervisor;
use serde_json::{json, Value as Json};
use std::fs;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};

const INSTRUCTIONS: &str = "Call local_agents_status first. Start only a user-authorized task, reuse request_id on retries, then poll local_agent_job. These local workers can modify files and control the computer. They run as the OS user, not inside an OS sandbox. Host approvals and rules still apply. Execution completion is not independent task acceptance. One heavy job at a time. Jobs survive client disconnection until their deadline; cancel explicitly when requested.";

const CAPABILITIES: [(&str, &str, &str); 6] = [
    ("coordinator", "Qwen3.5 9B Q4", "Plan tasks, use memory and skills, route subtasks and combine receipts."),
    ("cyber-lab", "Qwen3.5 9B Q4", "Authorized local security analysis, code inspection and command-line tasks."),
    ("browser", "Qwen3.5 9B Q4", "Browser navigation, forms and page inspection on explicitly allowed hosts."),
    ("desktop", "Qwen3.5 9B Q4 + vision projector", "Observe screenshots and operate native macOS controls; OS permissions required."),
    ("image-studio", "FLUX.2 Klein 4B Q4 + ponpoke uncensored encoder", "Generate or reference-edit a still image. draft=512px, quality=768px; one local reference up to 768px. Returns PNG and resource/validation evidence."),
    ("video-studio", "Wan 1.3B e14 + UMT5 Q8", "Generate a short text-to-video clip. draft=320x192/33 frames; quality=384x224/33; detail-short=512x288/17. No image-to-video. Returns MP4 and resource/validation evidence."),
];

pub fn tools() -> Json {
    let job_id = json!({"type":"object","properties":{"job_id":{"type":"string","title":"Job Id"}},"required":["job_id"]});
    let ro = json!({"readOnlyHint":true,"openWorldHint":false});
    json!([
        {"name":"local_agents_status","description":"Read installation readiness, current jobs and disk space; starts no model.",
         "inputSchema":{"type":"object","properties":{}},"annotations":ro},
        {"name":"start_local_agent","description":"Start a bounded local role job. Requires task authorization; may edit files or operate browser/desktop.\n\nUse a fresh request_id for each intentional new job and the same ID for retries. Tasks use the role's configured workspace. Exact browser hosts are opt-in. Image/video jobs accept media_preset; image jobs also accept an absolute local reference_image path. Resource admission can refuse or stop a job. Returns immediately; poll by job ID. Execution completion does not establish visual quality.",
         "inputSchema":{"type":"object","required":["request"],"properties":{"request":{"type":"object","additionalProperties":false,
            "required":["role","task","request_id"],"properties":{
              "role":{"type":"string","enum":ROLES},
              "task":{"type":"string","minLength":1,"maxLength":16000},
              "request_id":{"type":"string","minLength":8,"maxLength":128,"pattern":"^[a-zA-Z0-9_-]+$"},
              "timeout_seconds":{"type":"integer","minimum":30,"maximum":3600,"default":900},
              "max_turns":{"type":"integer","minimum":1,"maximum":32,"default":16},
              "allowed_hosts":{"type":"array","items":{"type":"string"},"maxItems":20,"default":[]},
              "dispatch_limit":{"type":"integer","minimum":0,"maximum":10,"default":3},
              "media_preset":{"type":"string","enum":["draft","quality","detail-short"],"default":"draft"},
              "reference_image":{"anyOf":[{"type":"string","maxLength":4096},{"type":"null"}],"default":null}}}}},
         "annotations":{"readOnlyHint":false,"destructiveHint":true,"idempotentHint":true,"openWorldHint":true}},
        {"name":"local_agent_job","description":"Read execution state, bounded answer and generated artifact paths for a job.","inputSchema":job_id,"annotations":ro},
        {"name":"cancel_local_agent","description":"Request cancellation. Poll until terminal state; cancelling is not stopped yet.","inputSchema":job_id,
         "annotations":{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}},
        {"name":"local_agent_log","description":"Read at most the final 8 KiB of this job's worker log; no arbitrary file access.","inputSchema":job_id,"annotations":ro}
    ])
}

fn free_gib(path: &std::path::Path) -> f64 {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return -1.0;
    }
    ((s.f_bavail as f64 * s.f_frsize as f64) / (1u64 << 30) as f64 * 100.0).round() / 100.0
}

fn physical_gib() -> f64 {
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut v as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (v as f64 / (1u64 << 30) as f64 * 100.0).round() / 100.0
}

fn read_json(p: &std::path::Path) -> Json {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(json!({}))
}

fn catalog() -> Json {
    let r = root();
    let ev = r.join("evidence/setup-20261102");
    let workers: Vec<Json> = CAPABILITIES.iter().map(|(role, model, cap)| {
        let missing = prerequisites(&r, role);
        let tests: &[&str] = match *role {
            "coordinator" => &["communication", "lifecycle", "memory-agent", "media-routing"],
            "cyber-lab" => &["cyber"], "browser" => &["browser"], "desktop" => &["desktop", "vision"], _ => &[],
        };
        let mut q: Vec<Json> = tests.iter().map(|t| json!({"test": t, "passed": read_json(&ev.join(format!("{t}-acceptance.json")))["passed"] == json!(true)})).collect();
        if role.ends_with("-studio") {
            let d = read_json(&r.join(format!("agents/{role}/outputs/local-smoke.evidence.json")));
            q.push(json!({"test": "generation_and_visual_review", "passed": d["visual_inspection"] == json!("passed")}));
        }
        let cfg = read_json(&r.join(format!("agents/{role}/config.json")));
        let defaults: serde_json::Map<String, Json> = ["width", "height", "frames", "fps", "steps", "vision"].iter()
            .filter_map(|k| cfg.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        json!({"role": role, "model": model, "capability": cap, "installation_ready": missing.is_empty(), "missing": missing, "last_recorded_qualification": q, "defaults": defaults})
    }).collect();
    json!({"workers": workers, "resources": {"physical_memory_gib": physical_gib()},
           "resource_policy": "One package-managed heavy model process at a time. Release the parent before dispatch. No idle model residency. Other applications and model servers are not controlled.",
           "readiness_is_not_task_acceptance": true})
}

pub fn call_tool(store: &JobStore, name: &str, args: &Json) -> Result<Json, String> {
    let job_id = || {
        args.get("job_id")
            .and_then(Json::as_str)
            .ok_or_else(|| "job_id is required".to_string())
    };
    match name {
        "local_agents_status" => {
            let r = root();
            let jobs: Vec<Json> = store
                .recent_ids(10)?
                .iter()
                .filter_map(|i| store.public(i).ok())
                .collect();
            let roles: serde_json::Map<String, Json> = ROLES
                .iter()
                .map(|role| {
                    let m = prerequisites(&r, role);
                    (
                        role.to_string(),
                        json!({"installation_ready": m.is_empty(), "missing": m}),
                    )
                })
                .collect();
            Ok(
                json!({"worker_catalog": catalog(), "roles": roles, "free_gib": free_gib(&r), "jobs": jobs,
                      "transport": "local stdio", "bridge": "rust",
                      "model_and_media_acceptance": "See reports/local-agent-models.html; installation readiness is not qualification."}),
            )
        }
        "start_local_agent" => {
            let req =
                crate::jobs::validate_request(args.get("request").ok_or("request is required")?)?;
            let role = req["role"].as_str().unwrap_or_default().to_string();
            let missing = prerequisites(&root(), &role);
            if !missing.is_empty() {
                return Ok(json!({"state": "not_ready", "missing": missing, "job_started": false}));
            }
            let (id, created) = store.create(&req)?;
            if created {
                if let Err(e) = spawn_supervisor(store, &id) {
                    store.update(&id, Some("failed"), Some(&json!({"error": e})))?;
                }
            }
            store.public(&id)
        }
        "local_agent_job" => {
            let mut out = store.public(job_id()?)?;
            if let Some(Json::String(a)) = out.pointer_mut("/result/agent/answer") {
                if a.chars().count() > 32000 {
                    *a = a.chars().take(32000).collect::<String>()
                        + " [truncated; full result is in run_directory]";
                }
            }
            Ok(out)
        }
        "cancel_local_agent" => store.cancel(job_id()?),
        "local_agent_log" => {
            let id = job_id()?;
            store.get(id)?;
            let path = store.job_dir(id).join("worker.log");
            let meta = match fs::symlink_metadata(&path) {
                Err(_) => return Ok(json!({"text": "Worker log not created yet."})),
                Ok(m) => m,
            };
            if meta.file_type().is_symlink() {
                return Err("Symlink logs are not supported".into());
            }
            let mut f = fs::File::open(&path).map_err(|e| e.to_string())?;
            f.seek(SeekFrom::Start(meta.len().saturating_sub(8192)))
                .map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            f.take(8192)
                .read_to_end(&mut buf)
                .map_err(|e| e.to_string())?;
            Ok(json!({"text": String::from_utf8_lossy(&buf), "untrusted_observation": true}))
        }
        _ => Err(format!("Unknown tool: {name}")),
    }
}

/// Handle one JSON-RPC message; None for notifications.
pub fn handle(store: &JobStore, msg: &Json) -> Option<Json> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Json::as_str).unwrap_or_default();
    let id = id?; // notifications (initialized, cancelled) need no reply
    let result = match method {
        "initialize" => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Json::as_str)
                .unwrap_or("2025-06-18");
            Ok(
                json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": false}},
                      "serverInfo": {"name": "local-agent-workers", "version": env!("CARGO_PKG_VERSION")},
                      "instructions": INSTRUCTIONS}),
            )
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            Ok(match call_tool(store, name, &args) {
                Ok(v) => {
                    json!({"content": [{"type": "text", "text": v.to_string()}], "structuredContent": v, "isError": false})
                }
                Err(e) => {
                    json!({"content": [{"type": "text", "text": format!("Error executing tool {name}: {e}")}], "isError": true})
                }
            })
        }
        _ => Err(json!({"code": -32601, "message": format!("Method not found: {method}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

pub fn serve(store: &JobStore) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Json>(&line) {
            Ok(msg) => handle(store, &msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("Parse error: {e}")}}),
            ),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}").map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
