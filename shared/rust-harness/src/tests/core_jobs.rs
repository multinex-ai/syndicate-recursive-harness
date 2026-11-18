use crate::jobs::{python_dumps, validate_request, JobStore};
use crate::mcp::{call_tool, handle};
use crate::supervisor::{descendants, start_time, supervise};
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn temp_store(tag: &str) -> JobStore {
    let dir = std::env::temp_dir().join(format!(
        "la-rs-{tag}-{}",
        crate::jobs::new_job_id().unwrap()
    ));
    JobStore::open(dir).unwrap()
}

fn req(rid: &str, task: &str) -> serde_json::Map<String, Json> {
    validate_request(&json!({"role": "cyber-lab", "task": task, "request_id": rid})).unwrap()
}

#[test]
fn digest_body_matches_python_json_dumps() {
    let r = req(
        "req-unicode-01",
        "Résumé \"quoted\" \\ tab\t line\n emoji 🛡 del\u{7f}",
    );
    let ours = python_dumps(&Json::Object(r.clone()));
    let py = Command::new("python3").args(["-I", "-c", "import json,sys; print(json.dumps(json.loads(sys.stdin.read()), sort_keys=True), end='')"])
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().map(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(Json::Object(r).to_string().as_bytes()).unwrap();
            String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap()
        });
    assert_eq!(ours, py.expect("python3 is required for the parity check"));
}

#[test]
fn request_validation_is_strict() {
    let bad = [
        json!({"role": "cyber-lab", "task": "x", "request_id": "short"}),
        json!({"role": "nope", "task": "x", "request_id": "abcdefgh"}),
        json!({"role": "cyber-lab", "task": "x", "request_id": "abcdefgh", "extra": 1}),
        json!({"role": "cyber-lab", "task": "x", "request_id": "abcdefgh", "timeout_seconds": 900.0}),
        json!({"role": "cyber-lab", "task": "x", "request_id": "abcdefgh", "allowed_hosts": ["https://a.test"]}),
        json!({"role": "cyber-lab", "task": "x", "request_id": "abcdefgh", "media_preset": "quality"}),
        json!({"role": "video-studio", "task": "x", "request_id": "abcdefgh", "reference_image": "/tmp/a.png"}),
        json!({"role": "image-studio", "task": "x", "request_id": "abcdefgh", "reference_image": "rel.png"}),
        json!({"role": "cyber-lab", "task": "", "request_id": "abcdefgh"}),
    ];
    for b in bad {
        assert!(validate_request(&b).is_err(), "should reject {b}");
    }
    let ok = validate_request(&json!({"role": "image-studio", "task": "x", "request_id": "abcdefgh", "media_preset": "quality", "reference_image": "/tmp/a.png"})).unwrap();
    assert_eq!(ok["timeout_seconds"], json!(900));
}

#[test]
fn create_is_idempotent_and_single_flight() {
    let s = temp_store("idem");
    let (a, created) = s.create(&req("request-0001", "one")).unwrap();
    assert!(created);
    let (b, created) = s.create(&req("request-0001", "one")).unwrap();
    assert!(!created && a == b);
    assert!(s
        .create(&req("request-0001", "different"))
        .unwrap_err()
        .contains("different arguments"));
    assert!(s
        .create(&req("request-0002", "two"))
        .unwrap_err()
        .contains("Another MCP job is active"));
    let c = s.cancel(&a).unwrap();
    assert_eq!(c["state"], json!("cancelling"));
    assert_eq!(c["completion_is_independently_verified"], json!(false));
}

fn sh_ok(_: &Json, _: &Path) -> Result<Vec<String>, String> {
    Ok(vec![
        "/bin/sh".into(),
        "-c".into(),
        "echo worker-ran".into(),
    ])
}

fn sh_tree(_: &Json, dir: &Path) -> Result<Vec<String>, String> {
    // Worker plus a grandchild that ignores SIGINT; both must be stopped on cancel.
    let marker = dir.join("grandchild.pid");
    Ok(vec![
        "/bin/sh".into(),
        "-c".into(),
        format!(
            "(trap '' INT; sleep 60) & echo $! > {}; sleep 60",
            marker.display()
        ),
    ])
}

#[test]
fn supervise_records_execution_result() {
    let s = temp_store("run");
    let (id, _) = s.create(&req("request-run-01", "go")).unwrap();
    supervise(&s, &id, sh_ok).unwrap();
    let j = s.public(&id).unwrap();
    assert_eq!(j["state"], json!("execution_completed"));
    assert_eq!(j["result"]["returncode"], json!(0));
    let log = call_tool(&s, "local_agent_log", &json!({"job_id": id})).unwrap();
    assert!(log["text"].as_str().unwrap().contains("worker-ran"));
}

#[test]
fn cancel_stops_worker_and_pinned_descendants() {
    let s = temp_store("cancel");
    let (id, _) = s.create(&req("request-cancel", "go")).unwrap();
    let dir: PathBuf = s.job_dir(&id);
    let s2 = JobStore::open(s.base.clone()).unwrap();
    let id2 = id.clone();
    let t = std::thread::spawn(move || supervise(&s2, &id2, sh_tree));
    let marker = dir.join("grandchild.pid");
    for _ in 0..50 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(800)); // let the supervisor pin descendants
    let gc: i32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(start_time(gc).is_some());
    s.cancel(&id).unwrap();
    t.join().unwrap().unwrap();
    assert_eq!(s.public(&id).unwrap()["state"], json!("cancelled"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        start_time(gc).is_none(),
        "grandchild {gc} survived cancellation"
    );
}

#[test]
fn descendant_walk_finds_nested_children() {
    let mut c = Command::new("/bin/sh")
        .args(["-c", "sleep 5 & sleep 5 & wait"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let d = descendants(c.id() as i32);
    let _ = c.kill();
    for o in &d {
        unsafe { libc::kill(o.pid, libc::SIGKILL) };
    }
    let _ = c.wait();
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn mcp_protocol_basics() {
    let s = temp_store("mcp");
    let init = handle(&s, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        json!("local-agent-workers")
    );
    assert!(handle(
        &s,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .is_none());
    let list = handle(
        &s,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 5);
    let bad = handle(&s, &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "local_agent_job", "arguments": {"job_id": "../etc"}}})).unwrap();
    assert_eq!(bad["result"]["isError"], json!(true));
}

