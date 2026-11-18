//! Durable job store. Same schema and file as shared/harness/jobs.py, so the
//! Python and Rust bridges can run against one jobs.sqlite during cut-over.
use crate::sqlite::{col, Db, Row, Value};
use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ROLES: [&str; 6] = [
    "coordinator",
    "cyber-lab",
    "browser",
    "desktop",
    "image-studio",
    "video-studio",
];
pub const ACTIVE: [&str; 3] = ["starting", "running", "cancelling"];
use crate::runtime::{MODEL_REL, PROJECTOR_REL};

/// Package root (the Agents folder). LOCAL_AGENTS_ROOT overrides; otherwise two levels above this crate.
pub fn root() -> PathBuf {
    if let Ok(p) = std::env::var("LOCAL_AGENTS_ROOT") {
        return PathBuf::from(p);
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    p.canonicalize().unwrap_or(p)
}

/// Where run-derived data (reward ledger, telemetry) is written. Identical to `root()` in real use; under
/// `cargo test` it is a throwaway temp dir, so unit tests can never leave fake episodes or spans in the
/// real ledgers that the console reads.
pub fn data_root() -> PathBuf {
    if cfg!(test) {
        let d = std::env::temp_dir().join("local-agents-rs-test-data");
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    root()
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn prerequisites(root: &Path, role: &str) -> Vec<String> {
    let required: Vec<String> = match role {
        "image-studio" => vec!["IMAGE_RUNTIME_BLOCKED_UNVERIFIED_ENCODER_ASSEMBLY".into()],
        "video-studio" => vec![
            "shared/models/wan-uncensored/wan_1.3B_exp_e14.safetensors".into(),
            "shared/models/wan-uncensored/Wan2.1_VAE.pth".into(),
            "shared/models/wan-uncensored/tokenizer.json".into(),
            "shared/models/wan-uncensored/t5-q8.safetensors".into(),
            "shared/models/wan-uncensored/t5-config.json".into(),
            "shared/vendor/mlx-wan/wan/pipeline.py".into(),
        ],
        _ => {
            let mut v = vec![MODEL_REL.to_string()];
            if role == "desktop" {
                v.push(PROJECTOR_REL.to_string());
            }
            v
        }
    };
    required
        .into_iter()
        .filter(|p| !root.join(p).is_file())
        .collect()
}

/// Validated StartRequest. Mirrors the strict pydantic model (extra=forbid, strict types).
pub fn validate_request(raw: &Json) -> Result<Map<String, Json>, String> {
    let obj = raw.as_object().ok_or("request must be an object")?;
    const KEYS: [&str; 9] = [
        "role",
        "task",
        "request_id",
        "timeout_seconds",
        "max_turns",
        "allowed_hosts",
        "dispatch_limit",
        "media_preset",
        "reference_image",
    ];
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("Extra inputs are not permitted: {k}"));
    }
    let s = |k: &str| -> Result<Option<&str>, String> {
        match obj.get(k) {
            None => Ok(None),
            Some(Json::String(v)) => Ok(Some(v)),
            Some(_) => Err(format!("{k} must be a string")),
        }
    };
    let int = |k: &str, def: i64, lo: i64, hi: i64| -> Result<i64, String> {
        match obj.get(k) {
            None => Ok(def),
            Some(Json::Number(n)) if n.is_i64() => {
                let v = n.as_i64().unwrap();
                if v < lo || v > hi {
                    Err(format!("{k} must be between {lo} and {hi}"))
                } else {
                    Ok(v)
                }
            }
            Some(_) => Err(format!("{k} must be an integer")),
        }
    };
    let role = s("role")?.ok_or("role is required")?;
    if !ROLES.contains(&role) {
        return Err(format!("Unknown role: {role}"));
    }
    let task = s("task")?.ok_or("task is required")?;
    let n = task.chars().count();
    if n == 0 || n > 16000 {
        return Err("task must be 1-16000 characters".into());
    }
    let rid = s("request_id")?.ok_or("request_id is required")?;
    if !(8..=128).contains(&rid.len())
        || !rid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("request_id must be 8-128 characters of [a-zA-Z0-9_-]".into());
    }
    let timeout = int("timeout_seconds", 900, 30, 3600)?;
    let turns = int("max_turns", 16, 1, 32)?;
    let dispatch = int("dispatch_limit", 3, 0, 10)?;
    let hosts: Vec<String> = match obj.get("allowed_hosts") {
        None => vec![],
        Some(Json::Array(a)) => {
            if a.len() > 20 {
                return Err("allowed_hosts allows at most 20 entries".into());
            }
            let mut out = vec![];
            for h in a {
                let h = h.as_str().ok_or("allowed_hosts entries must be strings")?;
                let ok = h == "::1"
                    || (!h.is_empty()
                        && h.len() <= 253
                        && h.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'));
                if !ok {
                    return Err("Use exact hostnames, without URLs, ports or wildcards".into());
                }
                out.push(h.to_string());
            }
            out
        }
        Some(_) => return Err("allowed_hosts must be a list".into()),
    };
    let preset = s("media_preset")?.unwrap_or("draft");
    if !["draft", "quality", "detail-short"].contains(&preset) {
        return Err(format!("Unsupported media_preset: {preset}"));
    }
    let reference = match obj.get("reference_image") {
        None | Some(Json::Null) => None,
        Some(Json::String(v)) if v.chars().count() <= 4096 => Some(v.clone()),
        Some(_) => return Err("reference_image must be a path of at most 4096 characters".into()),
    };
    media_arguments(role, preset, reference.as_deref())?;
    let mut m = Map::new();
    m.insert("allowed_hosts".into(), json!(hosts));
    m.insert("dispatch_limit".into(), json!(dispatch));
    m.insert("max_turns".into(), json!(turns));
    m.insert("media_preset".into(), json!(preset));
    m.insert("reference_image".into(), json!(reference));
    m.insert("request_id".into(), json!(rid));
    m.insert("role".into(), json!(role));
    m.insert("task".into(), json!(task));
    m.insert("timeout_seconds".into(), json!(timeout));
    Ok(m)
}

/// Port of media_policy.media_arguments (validation + CLI args).
pub fn media_arguments(
    role: &str,
    preset: &str,
    reference: Option<&str>,
) -> Result<Vec<String>, String> {
    match role {
        "image-studio" | "video-studio" => {
            let ok = match role {
                "image-studio" => ["draft", "quality"].contains(&preset),
                _ => ["draft", "quality", "detail-short"].contains(&preset),
            };
            if !ok {
                return Err("Unsupported media role/preset".into());
            }
            let mut args = vec!["--preset".to_string(), preset.to_string()];
            if let Some(r) = reference.filter(|r| !r.is_empty()) {
                if role != "image-studio" {
                    return Err("Reference editing is supported only by the image worker".into());
                }
                if !Path::new(r).is_absolute() {
                    return Err("Reference image must be an absolute local path".into());
                }
                args.extend(["--reference-image".to_string(), r.to_string()]);
            }
            Ok(args)
        }
        _ if preset != "draft" || reference.is_some() => {
            Err("Media options require a media role".into())
        }
        _ => Ok(vec![]),
    }
}

/// Byte-identical to Python json.dumps(obj, sort_keys=True) (default separators, ensure_ascii),
/// so request digests match across the Python and Rust bridges.
pub fn python_dumps(v: &Json) -> String {
    let mut out = String::new();
    write_py(v, &mut out);
    out
}

fn write_py(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(&n.to_string()),
        Json::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{8}' => out.push_str("\\b"),
                    '\u{c}' => out.push_str("\\f"),
                    ' '..='~' => out.push(c),
                    _ => {
                        let mut buf = [0u16; 2];
                        for u in c.encode_utf16(&mut buf) {
                            out.push_str(&format!("\\u{:04x}", u));
                        }
                    }
                }
            }
            out.push('"');
        }
        Json::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py(x, out);
            }
            out.push(']');
        }
        Json::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py(&Json::String((*k).clone()), out);
                out.push_str(": ");
                write_py(&m[*k], out);
            }
            out.push('}');
        }
    }
}

pub fn new_job_id() -> Result<String, String> {
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| e.to_string())?;
    b[6] = (b[6] & 0x0f) | 0x40; // uuid4 version
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    Ok(hex::encode(b))
}

pub fn valid_job_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub struct JobStore {
    pub base: PathBuf,
    pub db_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub created: f64,
    pub updated: f64,
    pub cancel: bool,
    pub request: Json,
    pub result: Json,
}

impl Job {
    pub fn heartbeat_stale(&self) -> bool {
        ACTIVE.contains(&self.state.as_str()) && now() - self.updated > 45.0
    }
}

impl JobStore {
    pub fn open(base: PathBuf) -> Result<JobStore, String> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&base)
            .map_err(|e| e.to_string())?;
        let store = JobStore {
            db_path: base.join("jobs.sqlite"),
            base,
        };
        store.connect()?.exec("CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, request_id TEXT UNIQUE, digest TEXT, request TEXT, state TEXT, created REAL, updated REAL, cancel INTEGER DEFAULT 0, result TEXT)")?;
        fs::set_permissions(&store.db_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        Ok(store)
    }

    pub fn default_store() -> Result<JobStore, String> {
        let base = std::env::var("LOCAL_AGENTS_JOBS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root().join("shared/runtime/mcp-jobs"));
        JobStore::open(base)
    }

    pub fn connect(&self) -> Result<Db, String> {
        let db = Db::open(&self.db_path, 10_000)?;
        db.exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(db)
    }

    fn row_to_job(row: &Row) -> Result<Job, String> {
        let parse = |name: &str| -> Json {
            col(row, name)
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Json::Null)
        };
        Ok(Job {
            id: col(row, "id").as_str().unwrap_or_default().to_string(),
            state: col(row, "state").as_str().unwrap_or_default().to_string(),
            created: col(row, "created").as_f64().unwrap_or(0.0),
            updated: col(row, "updated").as_f64().unwrap_or(0.0),
            cancel: col(row, "cancel").as_i64().unwrap_or(0) != 0,
            request: parse("request"),
            result: parse("result"),
        })
    }

    pub fn get(&self, job_id: &str) -> Result<Job, String> {
        if !valid_job_id(job_id) {
            return Err("Invalid job ID".into());
        }
        let rows = self.connect()?.query(
            "SELECT * FROM jobs WHERE id=?",
            &[Value::Text(job_id.into())],
        )?;
        rows.first()
            .map(Self::row_to_job)
            .unwrap_or_else(|| Err("Unknown job ID".into()))
    }

    pub fn recent_ids(&self, limit: i64) -> Result<Vec<String>, String> {
        let rows = self.connect()?.query(
            "SELECT id FROM jobs ORDER BY created DESC LIMIT ?",
            &[Value::Int(limit)],
        )?;
        Ok(rows
            .iter()
            .filter_map(|r| col(r, "id").as_str().map(String::from))
            .collect())
    }

    /// Admission + idempotency in one IMMEDIATE transaction. Returns (job_id, created).
    pub fn create(&self, request: &Map<String, Json>) -> Result<(String, bool), String> {
        let body = python_dumps(&Json::Object(request.clone()));
        let digest = hex::encode(Sha256::digest(body.as_bytes()));
        let rid = request["request_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let db = self.connect()?;
        db.immediate(|db| {
            let prior = db.query("SELECT id,digest FROM jobs WHERE request_id=?", &[Value::Text(rid.clone())])?;
            if let Some(p) = prior.first() {
                if col(p, "digest").as_str() != Some(digest.as_str()) {
                    return Err("request_id already belongs to different arguments".into());
                }
                return Ok((col(p, "id").as_str().unwrap_or_default().to_string(), false));
            }
            let busy = db.query("SELECT id FROM jobs WHERE state IN ('starting','running','cancelling')", &[])?;
            if let Some(b) = busy.first() {
                return Err(format!("Another MCP job is active: {}", col(b, "id").as_str().unwrap_or("?")));
            }
            let id = new_job_id()?;
            let t = now();
            db.query(
                "INSERT INTO jobs (id,request_id,digest,request,state,created,updated) VALUES (?,?,?,?,?,?,?)",
                &[Value::Text(id.clone()), Value::Text(rid.clone()), Value::Text(digest.clone()), Value::Text(body.clone()), Value::Text("starting".into()), Value::Real(t), Value::Real(t)],
            )?;
            Ok((id, true))
        })
    }

    pub fn update(
        &self,
        job_id: &str,
        state: Option<&str>,
        result: Option<&Json>,
    ) -> Result<(), String> {
        let s = state.map(|s| Value::Text(s.into())).unwrap_or(Value::Null);
        let r = result
            .map(|r| Value::Text(python_dumps(r)))
            .unwrap_or(Value::Null);
        self.connect()?.query(
            "UPDATE jobs SET updated=?,state=COALESCE(?,state),result=COALESCE(?,result) WHERE id=?",
            &[Value::Real(now()), s, r, Value::Text(job_id.into())],
        )?;
        Ok(())
    }

    pub fn cancel(&self, job_id: &str) -> Result<Json, String> {
        self.get(job_id)?;
        self.connect()?.query(
            "UPDATE jobs SET cancel=1,state='cancelling' WHERE id=? AND state IN ('starting','running','cancelling')",
            &[Value::Text(job_id.into())],
        )?;
        self.public(job_id)
    }

    pub fn public(&self, job_id: &str) -> Result<Json, String> {
        let job = self.get(job_id)?;
        let stale = job.heartbeat_stale();
        let mut out = json!({
            "id": job.id, "state": job.state, "created": job.created, "updated": job.updated,
            "heartbeat_stale": stale, "result": job.result,
            "role": job.request.get("role").cloned().unwrap_or(Json::Null),
            "run_directory": self.base.join(job_id).to_string_lossy(),
            "completion_is_independently_verified": false,
        });
        if stale {
            out["attention"] = json!("Supervisor heartbeat stopped. Inspect owned processes before recovery; this job will not be silently retried.");
        }
        Ok(out)
    }

    pub fn job_dir(&self, job_id: &str) -> PathBuf {
        self.base.join(job_id)
    }
}
