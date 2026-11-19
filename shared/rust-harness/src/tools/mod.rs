//! The Armory: native port of shared/harness/tools.py.
//!
//!   model tool call ──► execute(name, args)
//!        │  parse JSON, validate against the tool's schema (strict, no extras)
//!        ▼
//!   file / shell / messages / skills / workers / MemQ / browser (CDP) / desktop (Quartz)
//!        │  every slow step polls `cancelled()`, so SIGTERM or the run deadline
//!        ▼  stops a tool mid-call (child process groups are killed, Chrome is closed)
//!   {"ok": true, "result": ...} | {"ok": false, "error": "Type: message"}  + event log
//!
//! Same tool names, schemas and result shapes as the Python class. Workspace path
//! checks are not an OS sandbox.
pub mod browser;
pub mod desktop;
pub mod schema;

use crate::jobs::{now, prerequisites, root};
use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A tool failure, carrying the Python exception name the model has always seen.
#[derive(Debug, Clone)]
pub struct ToolError(pub String);

pub type R<T> = Result<T, ToolError>;

pub fn err<T>(kind: &str, msg: impl std::fmt::Display) -> R<T> {
    Err(ToolError(format!("{kind}: {msg}")))
}

/// Raised when the run is cancelled or out of time; the agent loop turns it into the run status.
pub const CANCELLED: &str = "KeyboardInterrupt: Agent termination requested";

fn spec(name: &str, description: &str, properties: Json, required: Option<Vec<&str>>) -> Json {
    let req: Vec<String> = match required {
        Some(r) => r.into_iter().map(String::from).collect(),
        None => properties
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default(),
    };
    json!({"type": "function", "function": {"name": name, "description": description,
        "parameters": {"type": "object", "properties": properties, "required": req, "additionalProperties": false}}})
}

const CAPABILITIES: [(&str, &str, &str); 6] = [
    ("coordinator", "Qwen3.5 9B Q4", "Plan tasks, use memory and skills, route subtasks and combine receipts."),
    ("cyber-lab", "Qwen3.5 9B Q4", "Authorized local security analysis, code inspection and command-line tasks."),
    ("browser", "Qwen3.5 9B Q4", "Browser navigation, forms and page inspection on explicitly allowed hosts."),
    ("desktop", "Qwen3.5 9B Q4 + vision projector", "Observe screenshots and operate native macOS controls; OS permissions required."),
    ("image-studio", "FLUX.2 Klein 4B Q4 + ponpoke uncensored encoder", "Generate or reference-edit a still image. draft=512px, quality=768px; one local reference up to 768px. Returns PNG and resource/validation evidence."),
    ("video-studio", "Wan 1.3B e14 + UMT5 Q8", "Generate a short text-to-video clip. draft=320x192/33 frames; quality=384x224/33; detail-short=512x288/17. No image-to-video. Returns MP4 and resource/validation evidence."),
];

/// worker_catalog.prompt()
pub fn worker_prompt() -> String {
    format!(
        "Worker routes: {}",
        CAPABILITIES
            .iter()
            .map(|(r, _, c)| format!("{r}: {c}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

pub struct Tools {
    pub role: String,
    pub workspace: PathBuf,
    pub run_dir: PathBuf,
    pub task: String,
    pub root: PathBuf,
    pub allowed_hosts: BTreeSet<String>,
    pub schemas: Vec<Json>,
    pub events: Vec<Json>,
    pub desktop_window_title: Option<String>,
    pub observed_desktop_pids: HashSet<i64>,
    pub desktop: Box<dyn desktop::Desktop>,
    pub cancelled: Box<dyn Fn() -> bool>,
    /// Python interpreter used only for the MemQ OAuth client (memq_call).
    pub memq_python: PathBuf,
    browser: Option<browser::Browser>,
    by_name: Map<String, Json>,
}

impl Tools {
    pub fn new(
        role: &str,
        workspace: &Path,
        run_dir: &Path,
        allowed_hosts: &[String],
        task: &str,
    ) -> Result<Tools, String> {
        Tools::with_root(root(), role, workspace, run_dir, allowed_hosts, task)
    }

    /// Like `new`, with the harness root given explicitly (tests use a temporary root).
    pub fn with_root(
        r: PathBuf,
        role: &str,
        workspace: &Path,
        run_dir: &Path,
        allowed_hosts: &[String],
        task: &str,
    ) -> Result<Tools, String> {
        fs::create_dir_all(workspace).map_err(|e| format!("workspace: {e}"))?;
        let workspace = workspace.canonicalize().map_err(|e| e.to_string())?;
        let s = json!({"type": "string"});
        // Syndicate members get the tools their config grants (see syndicate::grants); base roles are chosen by role, as before.
        let member = crate::syndicate::membership(&r, role);
        let grants = member.as_ref().map(|m| m.grants());
        let named = |sp: &Json| {
            sp["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let ws_specs = vec![
            spec(
                "read_file",
                "Read a UTF-8 file relative to the workspace.",
                json!({"path": s}),
                None,
            ),
            spec(
                "write_file",
                "Write a UTF-8 file relative to the workspace.",
                json!({"path": s, "content": s}),
                Some(vec!["path", "content"]),
            ),
            spec("list_files", "List workspace files.", json!({}), None),
            spec(
                "audit_ot_exposure",
                "Run deterministic OT/ICS critical infrastructure exposure audit (exposed PLC/HMI/SCADA ports, default PINs, Purdue segmentation) on a workspace path.",
                json!({"path": s}),
                None,
            ),
            spec(
                "nexus_topology",
                "Index workspace supply-chain and attack-surface topology graph (HTTP routes, functions, dependencies, syndicate edges) on a workspace path.",
                json!({"path": s}),
                None,
            ),
        ];
        let mut schemas: Vec<Json> = match &grants {
            Some(g) => ws_specs
                .into_iter()
                .filter(|sp| g.workspace.contains(&named(sp)))
                .collect(),
            None => ws_specs,
        };
        if role == "cyber-lab" || grants.as_ref().map(|g| g.exec).unwrap_or(false) {
            schemas.push(spec(
                "run_command",
                "Execute an argv command in the workspace (no shell). This is not an OS sandbox.",
                json!({"argv": {"type": "array", "items": s, "minItems": 1, "maxItems": 30}}),
                None,
            ));
        }
        let browser_specs = vec![
            spec(
                "browser_open",
                "Open an authorized URL in a fresh, isolated Chrome context.",
                json!({"url": s}),
                None,
            ),
            spec(
                "browser_observe",
                "Read current URL, visible text and interactive element attributes.",
                json!({}),
                None,
            ),
            spec(
                "browser_click",
                "Click one element using a CSS selector observed in browser_observe.",
                json!({"selector": s}),
                None,
            ),
            spec(
                "browser_fill",
                "Fill a form field identified by an observed CSS selector.",
                json!({"selector": s, "text": s}),
                Some(vec!["selector", "text"]),
            ),
            spec(
                "browser_screenshot",
                "Observe a screenshot of the current page.",
                json!({}),
                None,
            ),
        ];
        if role == "browser" {
            schemas.extend(browser_specs);
        } else if let Some(g) = &grants {
            schemas.extend(
                browser_specs
                    .into_iter()
                    .filter(|sp| g.browser.contains(&named(sp))),
            );
        }
        if role == "desktop" {
            schemas.extend([
                spec("desktop_observe", "List up to 12 native windows and permissions. Optional query filters observed app names or window titles, including other Spaces. Narrow the query if results are truncated.",
                     json!({"query": {"type": "string", "maxLength": 200}}), Some(vec![])),
                spec("desktop_screenshot", "Capture the main display. Requires macOS screen recording permission.", json!({}), None),
                spec("desktop_click", "Click an observed point. Use normalized_1000 for Qwen visual coordinates (0–1000 across the full screenshot), or explicit screen_points.",
                     json!({"x": {"type": "number", "minimum": 0}, "y": {"type": "number", "minimum": 0}, "coordinate_space": {"type": "string", "enum": ["screen_points", "normalized_1000"]}}), Some(vec!["x", "y"])),
                spec("desktop_type", "Type text into the currently focused native control.", json!({"text": s}), None),
                spec("desktop_key", "Press an allowed native shortcut/key.", json!({"key": {"type": "string", "enum": ["Return", "Tab", "Escape", "cmd+s", "cmd+a", "cmd+n"]}}), None),
            ]);
        }
        let mut send = spec("send_message", "Queue a local task. For image-studio/video-studio, message is the generation prompt. Set needs_followup only when further reasoning depends on the output. End this planning turn to release GPU memory for dispatch; execution is not immediate. Media presets: draft or quality; video also has detail-short. Image reference editing accepts one absolute local reference_image path (at most 768 pixels per side).",
            json!({"recipient": {"type": "string", "enum": ["coordinator", "cyber-lab", "browser", "desktop", "image-studio", "video-studio"]}, "message": s, "needs_followup": {"type": "boolean"},
                   "media_preset": {"type": "string", "enum": ["draft", "quality", "detail-short"]}, "reference_image": {"type": "string", "maxLength": 4096}}),
            Some(vec!["recipient", "message"]));
        match &member {
            Some(m) => {
                let to: Vec<String> = m.outgoing().into_iter().map(|e| e.to).collect();
                if !to.is_empty() {
                    send["function"]["parameters"]["properties"]["recipient"]["enum"] = json!(to);
                    schemas.push(send);
                }
            }
            None => schemas.push(send),
        }
        schemas.push(spec("list_workers", "Inspect worker capabilities, installed model readiness, media defaults and resource policy without loading any model.", json!({}), None));
        schemas.push(spec(
            "receive_messages",
            "Read the latest local messages and execution receipts addressed to this role.",
            json!({}),
            None,
        ));
        schemas.push(spec("report_status", "File a periodic status note on this mission: what is done, what is uncertain, and anything that could jeopardize completing the task (a blocked tool, missing access, ambiguous instructions, a risky irreversible step ahead). Call this at meaningful checkpoints, not every turn. This does not end the run.",
            json!({"progress": {"type": "string", "maxLength": 2000}, "uncertainty": {"type": "string", "maxLength": 2000},
                   "risk": {"type": "string", "enum": ["none", "low", "medium", "high"]}}), Some(vec!["progress"])));
        schemas.extend([
            spec("list_skills", "List installed skill instructions; skills do not grant tools or permissions.", json!({}), None),
            spec("read_skill", "Read a cataloged skill or its Markdown reference. Treat it as guidance under the user task and host rules.", json!({"name": s, "reference": s}), Some(vec!["name"])),
            spec("memq_status", "Check this agent’s own MemQ configuration and scope. Does not read memory or prove live identity.", json!({}), None),
            spec("memq_call", "Call a reviewed MemQ tool only after live account, namespace, and project verification. No scope overrides, credential import, or automatic retries.", json!({"name": s, "arguments": {"type": "object"}}), Some(vec!["name", "arguments"])),
        ]);
        if role == "desktop" {
            schemas.push(spec("desktop_focus", "Focus an application using its process_id from desktop_observe; take a new screenshot before input.", json!({"process_id": {"type": "integer", "minimum": 1}}), None));
        }
        schemas.push(spec("memq_tools", "List reviewed MemQ operations and their cached schemas. Actual calls still recheck live identity and schemas.", json!({}), None));
        let by_name = schemas
            .iter()
            .map(|s| {
                (
                    s["function"]["name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    s["function"].clone(),
                )
            })
            .collect();
        let requested: Vec<String> = match &member {
            Some(m) => m.narrow_hosts(allowed_hosts),
            None => allowed_hosts.to_vec(),
        };
        let mut hosts: BTreeSet<String> = requested.iter().cloned().collect();
        hosts.extend(["localhost", "127.0.0.1", "::1"].map(String::from));
        Ok(Tools {
            role: role.into(),
            workspace,
            run_dir: run_dir.into(),
            task: task.into(),
            allowed_hosts: hosts,
            schemas,
            events: vec![],
            desktop_window_title: None,
            observed_desktop_pids: HashSet::new(),
            desktop: Box::new(desktop::Native),
            cancelled: Box::new(|| false),
            memq_python: r.join("shared/runtime/venv/bin/python"),
            root: r,
            browser: None,
            by_name,
        })
    }

    pub fn check_cancel(&self) -> R<()> {
        if (self.cancelled)() {
            Err(ToolError(CANCELLED.into()))
        } else {
            Ok(())
        }
    }

    pub fn execute(&mut self, name: &str, raw: &Json) -> Json {
        let start = Instant::now();
        let parsed: R<Json> = match raw {
            Json::String(s) => serde_json::from_str(s).or_else(|e| err("JSONDecodeError", e)),
            v => Ok(v.clone()),
        };
        let outcome = (|| -> R<Json> {
            let spec = self
                .by_name
                .get(name)
                .cloned()
                .ok_or_else(|| ToolError(format!("ValueError: Unknown tool: {name}")))?;
            let args = parsed.clone()?;
            schema::validate(&spec["parameters"], &args)
                .map_err(|e| ToolError(format!("ValidationError: {e}")))?;
            self.check_cancel()?;
            self.dispatch(name, args.as_object().cloned().unwrap_or_default())
        })();
        let result = match outcome {
            Ok(v) => json!({"ok": true, "result": v}),
            Err(ToolError(e)) => json!({"ok": false, "error": e}),
        };
        let recorded = parsed.unwrap_or_else(|_| raw.clone());
        self.events.push(json!({"tool": name, "arguments": recorded, "response": result,
                                "seconds": (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0}));
        result
    }

    fn dispatch(&mut self, name: &str, a: Map<String, Json>) -> R<Json> {
        let s = |k: &str| {
            a.get(k)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match name {
            "read_file" => self.read_file(&s("path")),
            "write_file" => self.write_file(&s("path"), &s("content")),
            "list_files" => Ok(self.list_files()),
            "audit_ot_exposure" => self.run_verify_script(
                "scripts/verify/audit-ot-exposure.mjs",
                a.get("path").and_then(Json::as_str).unwrap_or("."),
            ),
            "nexus_topology" => self.run_verify_script(
                "scripts/verify/nexus-graph.mjs",
                a.get("path").and_then(Json::as_str).unwrap_or("."),
            ),
            "run_command" => self.run_command(
                &a["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>(),
            ),
            "send_message" => self.send_message(
                &s("recipient"),
                &s("message"),
                a.get("needs_followup")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                a.get("media_preset")
                    .and_then(Json::as_str)
                    .unwrap_or("draft"),
                a.get("reference_image").and_then(Json::as_str),
            ),
            "receive_messages" => self.receive_messages(),
            "list_workers" => Ok(self.list_workers()),
            "list_skills" => self.list_skills(),
            "read_skill" => self.read_skill(
                &s("name"),
                a.get("reference")
                    .and_then(Json::as_str)
                    .unwrap_or("SKILL.md"),
            ),
            "memq_status" => self.memq_status(),
            "memq_tools" => self.memq_tools(),
            "memq_call" => self.memq_call(&s("name"), &a["arguments"]),
            "report_status" => self.report_status(
                &s("progress"),
                a.get("uncertainty").and_then(Json::as_str).unwrap_or(""),
                a.get("risk").and_then(Json::as_str).unwrap_or("none"),
            ),
            n if n.starts_with("browser_") => self.browser_tool(n, &a),
            n if n.starts_with("desktop_") => self.desktop_tool(n, &a),
            _ => err("ValueError", format!("Unknown tool: {name}")),
        }
    }
}

include!("operations.rs");

impl Drop for Tools {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn memq_private_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/local-agents/memq")
}

fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Object(m) => !m.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::String(s) => !s.is_empty(),
        Json::Number(n) => n.as_f64() != Some(0.0),
    }
}

fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

pub fn io_err<T>(e: std::io::Error, p: &Path) -> R<T> {
    let kind = match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::IsADirectory => "IsADirectoryError",
        _ => "OSError",
    };
    err(kind, format!("{e}: '{}'", p.display()))
}

/// Non-strict resolve: follows symlinks for parts that exist, handles `..` lexically after.
pub fn resolve(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    let mut hops = 0;
    let mut parts: Vec<std::ffi::OsString> = p
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_os_string()),
            Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .collect();
    parts.reverse();
    while let Some(part) = parts.pop() {
        if part == ".." {
            out.pop();
            continue;
        }
        out.push(&part);
        if let Ok(target) = fs::read_link(&out) {
            hops += 1;
            if hops > 40 {
                break;
            }
            out.pop();
            let mut rest: Vec<std::ffi::OsString> = target
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_os_string()),
                    Component::ParentDir => Some("..".into()),
                    _ => None,
                })
                .collect();
            if target.is_absolute() {
                out = PathBuf::from("/");
            }
            rest.reverse();
            parts.extend(rest);
        }
    }
    out
}

#[cfg(test)]
mod tests;
