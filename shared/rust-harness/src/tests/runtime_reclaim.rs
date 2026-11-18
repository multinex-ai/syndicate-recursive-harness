// ---------- runtime (Metal server lifecycle) ----------
use crate::runtime::{
    admission_reason, max_swap_growth, resolve_heavy_lock_path, runtime_reason, HeavyLock,
    ModelServer, Sample, ServerConfig,
};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fake_cfg(dir: &Path, port: u16, script: &str) -> ServerConfig {
    let mut c = ServerConfig::qwen(false);
    c.program = "python3".into();
    c.args_override = Some(vec![
        "-I".into(),
        "-c".into(),
        script.replace("PORT", &port.to_string()),
    ]);
    c.port = port;
    c.lock_path = dir.join("heavy.lock");
    c.load_timeout = Duration::from_secs(15);
    c.guard = false;
    c.reclaim = None;
    c
}

const HEALTHY: &str = r#"
import http.server, json, time
time.sleep(0.5)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def reply(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self): self.reply({'status':'ok'})
    def do_POST(self):
        n = int(self.headers['Content-Length']); body = json.loads(self.rfile.read(n))
        self.reply({'choices':[{'message':{'content':'echo:'+body['messages'][0]['content']}}]})
http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
"#;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "la-rt-{tag}-{}",
        crate::jobs::new_job_id().unwrap()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn heavy_lock_is_exclusive_and_released() {
    let d = scratch("lock");
    let p = d.join("heavy.lock");
    let a = HeavyLock::acquire(&p, Duration::ZERO).unwrap();
    assert!(HeavyLock::acquire(&p, Duration::from_millis(400)).is_err());
    drop(a);
    assert!(HeavyLock::acquire(&p, Duration::ZERO).is_ok());
}

#[test]
fn heavy_lock_excludes_python_holder() {
    let d = scratch("pylock");
    let p = d.join("heavy.lock");
    let mut py = Command::new("python3").args(["-I", "-c", &format!(
        "import fcntl,time; f=open('{}','a+'); fcntl.flock(f,fcntl.LOCK_EX); print('held',flush=True); time.sleep(3)", p.display())])
        .stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(py.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(HeavyLock::acquire(&p, Duration::from_millis(300)).is_err());
    py.wait().unwrap();
    assert!(HeavyLock::acquire(&p, Duration::ZERO).is_ok());
}

#[test]
fn heavy_lock_path_matches_python_for_whitespace_and_relative_overrides() {
    let home = Path::new("/tmp/syndicate-home");
    let rust = resolve_heavy_lock_path(
        Some("  /tmp/shared-heavy.lock  "),
        Some(" /tmp/state "),
        Some(home),
    );
    let script = format!(
        "import os,sys;sys.path.insert(0,{});os.environ['SYNDICATE_HEAVY_LOCK']='  /tmp/shared-heavy.lock  ';os.environ['XDG_STATE_HOME']=' /tmp/state ';import runtime;print(runtime.heavy_lock_path())",
        serde_json::to_string(&crate::jobs::root().join("shared/harness").display().to_string()).unwrap()
    );
    let output = Command::new("python3")
        .args(["-I", "-c", &script])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), rust.display().to_string());

    assert_eq!(
        resolve_heavy_lock_path(Some("relative.lock"), Some(" /tmp/state "), Some(home)),
        PathBuf::from("/tmp/state/local-syndicate-harness/heavy-model.lock")
    );
}

#[test]
fn server_lifecycle_health_request_teardown() {
    let d = scratch("srv");
    let port = free_port();
    let cfg = fake_cfg(&d, port, HEALTHY);
    let server = ModelServer::start(&cfg, &d).unwrap();
    let pid = server.pid() as i32;
    let rec: Json =
        serde_json::from_str(&std::fs::read_to_string(d.join("server-process.json")).unwrap())
            .unwrap();
    assert_eq!(rec["state"], json!("healthy"));
    let r = server
        .request(
            "/v1/chat/completions",
            Some(&json!({"messages":[{"role":"user","content":"hi"}]})),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(r["choices"][0]["message"]["content"], json!("echo:hi"));
    // Lock is held while the server lives.
    assert!(HeavyLock::acquire(&cfg.lock_path, Duration::ZERO).is_err());
    drop(server);
    assert!(start_time(pid).is_none(), "server survived teardown");
    let rec: Json =
        serde_json::from_str(&std::fs::read_to_string(d.join("server-process.json")).unwrap())
            .unwrap();
    assert_eq!(rec["state"], json!("terminated"));
    assert!(HeavyLock::acquire(&cfg.lock_path, Duration::ZERO).is_ok());
}

#[test]
fn server_refuses_occupied_port_and_reports_early_exit() {
    let d = scratch("port");
    let hold = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = hold.local_addr().unwrap().port();
    let e = ModelServer::start(&fake_cfg(&d, port, HEALTHY), &d)
        .err()
        .unwrap();
    assert!(e.contains("already occupied"), "{e}");
    drop(hold);
    let e = ModelServer::start(&fake_cfg(&d, free_port(), "import sys; sys.exit(3)"), &d)
        .err()
        .unwrap();
    assert!(e.contains("exited"), "{e}");
    // A failed start must not leave the lock held.
    assert!(HeavyLock::acquire(&d.join("heavy.lock"), Duration::ZERO).is_ok());
}

#[test]
fn server_load_timeout_kills_the_process_group() {
    let d = scratch("slow");
    let mut cfg = fake_cfg(
        &d,
        free_port(),
        "import time, subprocess; subprocess.Popen(['sleep','60']); time.sleep(60)",
    );
    cfg.load_timeout = Duration::from_secs(1);
    let before = std::time::Instant::now();
    let e = ModelServer::start(&cfg, &d).err().unwrap();
    assert!(e.contains("exceeded"), "{e}");
    assert!(before.elapsed() < Duration::from_secs(12));
    let rec: Json =
        serde_json::from_str(&std::fs::read_to_string(d.join("server-process.json")).unwrap())
            .unwrap();
    let pid = rec["pid"].as_i64().unwrap() as i32;
    std::thread::sleep(Duration::from_millis(200));
    assert!(start_time(pid).is_none());
}

#[test]
fn resource_sample_and_thresholds() {
    let s = Sample::take(None).unwrap();
    assert!(s.physical_bytes > 0 && s.available_bytes > 0 && s.available_bytes <= s.physical_bytes);
    let gib = 1u64 << 30;
    let ok = Sample {
        time: 0.0,
        physical_bytes: 18 * gib,
        available_bytes: 8 * gib,
        pressure_level: 1,
        swap_used_bytes: gib,
        disk_free_bytes: 50 * gib,
        worker_rss_bytes: gib,
        worker_footprint_bytes: gib,
    };
    assert_eq!(admission_reason(&ok), None);
    assert!(admission_reason(&Sample {
        available_bytes: 3 * gib,
        ..ok
    })
    .is_some());
    assert!(admission_reason(&Sample {
        pressure_level: 2,
        ..ok
    })
    .is_some());
    assert!(runtime_reason(
        &Sample {
            worker_rss_bytes: 9 * gib,
            ..ok
        },
        &ok
    )
    .is_some());
    // GPU memory shows only in the footprint; it must trip the same limit.
    assert!(runtime_reason(
        &Sample {
            worker_footprint_bytes: 9 * gib,
            ..ok
        },
        &ok
    )
    .is_some());
    assert!(runtime_reason(
        &Sample {
            swap_used_bytes: ok.swap_used_bytes + max_swap_growth() + 1,
            ..ok
        },
        &ok
    )
    .is_some());
    assert_eq!(runtime_reason(&ok, &ok), None);
}

// ---------- memory reclaim ----------
use crate::reclaim::{reclaim, Policy};
use std::cell::RefCell;

#[test]
fn reclaim_policy_rejects_protected_and_odd_names() {
    assert!(Policy::from_json(&json!({"apps": ["Claude"]})).is_err());
    assert!(Policy::from_json(&json!({"apps": ["Microsoft Defender"]})).is_err());
    assert!(Policy::from_json(&json!({"apps": ["Evil\" to do shell script \"x"]})).is_err());
    assert!(Policy::from_json(&json!({"target_available_gib": 40})).is_err());
    let p = Policy::from_json(&json!({"apps": ["ChatGPT", "Google Chrome"]})).unwrap();
    assert_eq!(p.target_available_bytes, 4 << 30);
    assert!(p.restore_after);
}

#[test]
fn reclaim_quits_in_order_and_stops_at_target() {
    let gib = 1u64 << 30;
    let policy = Policy {
        target_available_bytes: 4 * gib,
        quit_timeout: Duration::from_secs(3),
        restore_after: true,
        apps: vec!["A".into(), "NotRunning".into(), "B".into(), "C".into()],
        auto_largest: false,
        min_app_bytes: 0,
        never: vec![],
        tolerate_warning_gib: None,
    };
    let avail = RefCell::new(2 * gib);
    let up = RefCell::new(vec!["A".to_string(), "B".to_string(), "C".to_string()]);
    let quits = RefCell::new(vec![]);
    let r = reclaim(
        &policy,
        &|| *avail.borrow(),
        &|| true,
        &|a| up.borrow().iter().any(|x| x == a),
        &|a| {
            quits.borrow_mut().push(a.to_string());
            up.borrow_mut().retain(|x| x != a);
            *avail.borrow_mut() += gib + gib / 2;
            Ok(())
        },
    );
    assert_eq!(*quits.borrow(), vec!["A", "B"]); // C untouched: target met after B
    assert_eq!(r.quit_apps, vec!["A", "B"]);
    assert!(r.to_json()["target_met"].as_bool().unwrap());
}

#[test]
fn reclaim_never_forces_an_app_that_refuses() {
    let gib = 1u64 << 30;
    let policy = Policy {
        target_available_bytes: 4 * gib,
        quit_timeout: Duration::from_secs(3),
        restore_after: true,
        apps: vec!["Stubborn".into()],
        auto_largest: false,
        min_app_bytes: 0,
        never: vec![],
        tolerate_warning_gib: None,
    };
    let r = reclaim(&policy, &|| gib, &|| true, &|_| true, &|_| Ok(()));
    assert!(r.quit_apps.is_empty());
    assert_eq!(
        r.entries[0]["outcome"],
        json!("still_running_after_timeout")
    );
    assert!(!r.to_json()["target_met"].as_bool().unwrap());
}

#[test]
fn reclaim_auto_mode_named_first_then_largest_skipping_protected() {
    let gib = 1u64 << 30;
    let policy = Policy::from_json(&json!({"apps": ["ChatGPT"], "auto_largest": true, "min_app_mb": 200, "never": ["Slack"], "quit_timeout_seconds": 3})).unwrap();
    let avail = RefCell::new(gib);
    let up = RefCell::new(
        vec!["ChatGPT", "Claude", "Google Chrome", "Slack", "Tiny"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
    );
    let sizes = |n: &str| match n {
        "Claude" => 3 * gib,
        "ChatGPT" => 2 * gib,
        "Google Chrome" => gib,
        "Slack" => 900 << 20,
        _ => 50 << 20,
    };
    let quits = RefCell::new(vec![]);
    let by_mem = || {
        let mut v: Vec<(String, u64)> = up.borrow().iter().map(|n| (n.clone(), sizes(n))).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    };
    let r = crate::reclaim::reclaim_with(
        &policy,
        &|| *avail.borrow(),
        &|| true,
        &|a| up.borrow().iter().any(|x| x == a),
        &|a| {
            quits.borrow_mut().push(a.to_string());
            *avail.borrow_mut() += sizes(a);
            up.borrow_mut().retain(|x| x != a);
            Ok(())
        },
        &by_mem,
    );
    // ChatGPT (named) first, then the largest eligible (Chrome); Claude is protected, Slack is in "never".
    assert_eq!(*quits.borrow(), vec!["ChatGPT", "Google Chrome"]);
    assert!(r.to_json()["target_met"].as_bool().unwrap());
    // Nothing happens when RAM is already available.
    let none = crate::reclaim::reclaim_with(
        &policy,
        &|| 8 * gib,
        &|| true,
        &|_| true,
        &|_| panic!("must not quit"),
        &by_mem,
    );
    assert!(none.entries.is_empty());
}

#[test]
fn reclaim_acts_on_memory_pressure_even_with_enough_ram() {
    let gib = 1u64 << 30;
    let policy =
        Policy::from_json(&json!({"apps": ["ChatGPT"], "quit_timeout_seconds": 3})).unwrap();
    let up = RefCell::new(true);
    let r = crate::reclaim::reclaim_with(
        &policy,
        &|| 6 * gib,
        &|| !*up.borrow(),
        &|_| *up.borrow(),
        &|_| {
            *up.borrow_mut() = false;
            Ok(())
        },
        &|| vec![],
    );
    assert_eq!(r.quit_apps, vec!["ChatGPT"]);
    assert!(!r.pressure_normal_before && r.pressure_normal_after);
}

