use super::desktop::{Desktop, Window};
use super::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "la-tools-{tag}-{}",
        crate::jobs::new_job_id().unwrap()
    ));
    fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn tools(role: &str, tag: &str) -> (Tools, PathBuf) {
    let d = temp(tag);
    let mut t = Tools::new(role, &d.join("workspace"), &d, &[], "test task").unwrap();
    t.root = d.clone();
    (t, d)
}

#[test]
fn write_read_and_command_are_real() {
    let (mut t, d) = tools("cyber-lab", "basic");
    assert_eq!(
        t.execute(
            "write_file",
            &json!({"path": "note.txt", "content": "observed"})
        )["ok"],
        json!(true)
    );
    assert_eq!(
        fs::read_to_string(d.join("workspace/note.txt")).unwrap(),
        "observed"
    );
    assert_eq!(
        t.execute("read_file", &json!("{\"path\": \"note.txt\"}"))["result"],
        json!("observed")
    );
    assert_eq!(
        t.execute("list_files", &json!({}))["result"],
        json!(["note.txt"])
    );
    let r = t.execute(
        "run_command",
        &json!({"argv": ["/usr/bin/printf", "tool-executed"]}),
    );
    assert_eq!(r["result"]["stdout"], json!("tool-executed"), "{r}");
    assert_eq!(r["result"]["exit_code"], json!(0));
    assert_eq!(t.events.len(), 4);
    assert_eq!(t.events[1]["arguments"], json!({"path": "note.txt"}));
}

#[test]
fn malformed_arguments_never_execute() {
    let (mut t, d) = tools("cyber-lab", "malformed");
    for a in [
        json!("{bad"),
        json!({"path": "note.txt"}),
        json!({"path": "note.txt", "content": "x", "extra": true}),
        json!({"path": 4, "content": "x"}),
    ] {
        let r = t.execute("write_file", &a);
        assert_eq!(r["ok"], json!(false), "{a}");
    }
    assert!(t.execute("write_file", &json!("{bad"))["error"]
        .as_str()
        .unwrap()
        .starts_with("JSONDecodeError"));
    assert!(!d.join("workspace/note.txt").exists());
    assert_eq!(
        t.execute("run_command", &json!({"argv": []}))["ok"],
        json!(false)
    );
    // Role gating: browser tools are not offered to cyber-lab.
    assert_eq!(
        t.execute("browser_open", &json!({"url": "http://127.0.0.1/"}))["error"],
        json!("ValueError: Unknown tool: browser_open")
    );
}

#[test]
fn unknown_tool_traversal_and_symlink_escape_are_refused() {
    let (mut t, d) = tools("cyber-lab", "escape");
    assert_eq!(t.execute("fake_tool", &json!({}))["ok"], json!(false));
    assert_eq!(
        t.execute(
            "write_file",
            &json!({"path": "../outside.txt", "content": "x"})
        )["ok"],
        json!(false)
    );
    assert_eq!(
        t.execute(
            "write_file",
            &json!({"path": "/etc/outside.txt", "content": "x"})
        )["ok"],
        json!(false)
    );
    std::os::unix::fs::symlink(&d, d.join("workspace/link")).unwrap();
    assert_eq!(
        t.execute(
            "write_file",
            &json!({"path": "link/outside.txt", "content": "x"})
        )["ok"],
        json!(false)
    );
    assert!(!d.join("outside.txt").exists());
    assert_eq!(
        t.execute(
            "write_file",
            &json!({"path": "sub/../ok.txt", "content": "x"})
        )["ok"],
        json!(true)
    );
    assert!(d.join("workspace/ok.txt").is_file());
}

#[test]
fn browser_hosts_are_exact() {
    let (t, _) = tools("browser", "hosts");
    assert!(t.url_allowed("http://127.0.0.1:9000/"));
    assert!(t.url_allowed("http://[::1]:8000/x"));
    for u in [
        "file:///etc/passwd",
        "https://localhost.evil.test/",
        "https://example.com/",
        "http://user@example.com/",
        "javascript:alert(1)",
    ] {
        assert!(!t.url_allowed(u), "{u}");
    }
    assert!(
        t.url_allowed("http://evil@localhost/"),
        "userinfo is not the host"
    );
}

#[test]
fn run_command_is_cancellable_mid_call_and_kills_the_group() {
    let (mut t, d) = tools("cyber-lab", "cancel");
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    t.cancelled = Box::new(move || f2.load(Ordering::SeqCst));
    let pidfile = d.join("workspace/grandchild.pid");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let r = t.execute(
        "run_command",
        &json!({"argv": ["/bin/sh", "-c", "sleep 30 & echo $! > grandchild.pid; wait"]}),
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "cancel must not wait for the command"
    );
    assert_eq!(r["error"], json!(CANCELLED));
    std::thread::sleep(Duration::from_millis(200));
    let pid: i32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "grandchild in the process group must be gone"
    );
}

#[test]
fn schema_subset_matches_draft_2020() {
    let s = json!({"type": "object", "properties": {"n": {"type": "integer", "minimum": 1}, "k": {"type": "string", "enum": ["a"]}}, "required": ["n"], "additionalProperties": false});
    assert!(schema::validate(&s, &json!({"n": 3})).is_ok());
    assert!(schema::validate(&s, &json!({"n": 3.0})).is_ok());
    for bad in [
        json!({"n": 3.5}),
        json!({"n": 0}),
        json!({"n": true}),
        json!({}),
        json!({"n": 1, "k": "b"}),
        json!({"n": 1, "z": 1}),
        json!([]),
    ] {
        assert!(schema::validate(&s, &bad).is_err(), "{bad}");
    }
}

#[test]
fn send_and_receive_messages_use_local_inboxes() {
    let (mut t, d) = tools("coordinator", "msg");
    for role in ["coordinator", "image-studio", "cyber-lab"] {
        fs::create_dir_all(d.join("agents").join(role)).unwrap();
    }
    assert_eq!(
        t.execute(
            "send_message",
            &json!({"recipient": "coordinator", "message": "x"})
        )["ok"],
        json!(false),
        "no self-delegation"
    );
    assert_eq!(
        t.execute(
            "send_message",
            &json!({"recipient": "cyber-lab", "message": "x", "media_preset": "quality"})
        )["ok"],
        json!(false)
    );
    assert_eq!(
        t.execute(
            "send_message",
            &json!({"recipient": "image-studio", "message": "x", "reference_image": "rel.png"})
        )["ok"],
        json!(false)
    );
    let r = t.execute(
        "send_message",
        &json!({"recipient": "image-studio", "message": "a red kite", "media_preset": "quality"}),
    );
    assert_eq!(r["ok"], json!(true), "{r}");
    let env: Json =
        serde_json::from_str(&fs::read_to_string(r["result"]["path"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(
        (
            env["status"].clone(),
            env["media_preset"].clone(),
            env["origin_task"].clone()
        ),
        (json!("queued"), json!("quality"), json!("test task"))
    );
    assert!(env["allowed_hosts"]
        .as_array()
        .unwrap()
        .contains(&json!("127.0.0.1")));
    // dispatch.rs reads exactly this path back.
    let mut rec = json!({"tools": t.events});
    assert_eq!(crate::dispatch::queued(&rec).len(), 1);
    rec["tools"] = json!([]);
    fs::create_dir_all(d.join("agents/coordinator/inbox")).unwrap();
    fs::write(
        d.join("agents/coordinator/inbox/r.json"),
        "{\"status\": \"receipt\"}",
    )
    .unwrap();
    assert_eq!(
        t.execute("receive_messages", &json!({}))["result"],
        json!([{"status": "receipt"}])
    );
}

#[test]
fn skills_are_hash_pinned_and_confined() {
    let (mut t, d) = tools("coordinator", "skills");
    let dir = d.join("shared/skills/demo");
    fs::create_dir_all(dir.join("refs")).unwrap();
    fs::write(dir.join("SKILL.md"), "# demo").unwrap();
    fs::write(dir.join("refs/more.md"), "more").unwrap();
    fs::write(d.join("secret.md"), "no").unwrap();
    let sha = hex::encode(Sha256::digest(b"# demo"));
    fs::create_dir_all(d.join("shared/config")).unwrap();
    fs::write(d.join("shared/config/skills.json"), json!({"skills": [{"name": "demo", "description": "d", "path": "shared/skills/demo/SKILL.md", "sha256": sha, "capability_note": "n"}]}).to_string()).unwrap();
    assert_eq!(
        t.execute("list_skills", &json!({}))["result"],
        json!([{"name": "demo", "description": "d", "capability_note": "n"}])
    );
    let r = t.execute(
        "read_skill",
        &json!({"name": "demo", "reference": "refs/more.md"}),
    );
    assert_eq!(r["result"]["text"], json!("more"), "{r}");
    assert_eq!(
        r["result"]["available_references"],
        json!(["SKILL.md", "refs/more.md"])
    );
    assert_eq!(
        t.execute(
            "read_skill",
            &json!({"name": "demo", "reference": "../../../secret.md"})
        )["ok"],
        json!(false)
    );
    assert_eq!(
        t.execute("read_skill", &json!({"name": "nope"}))["ok"],
        json!(false)
    );
    fs::write(dir.join("SKILL.md"), "# changed").unwrap();
    assert!(t.execute("read_skill", &json!({"name": "demo"}))["error"]
        .as_str()
        .unwrap()
        .contains("refresh its reviewed manifest"));
}

#[test]
fn memq_scope_blocks_excluded_root_and_reads_config_only() {
    let (mut t, d) = tools("coordinator", "memq");
    fs::create_dir_all(d.join("shared/config")).unwrap();
    fs::write(d.join("shared/config/memq.json"), json!({"enabled": true, "expected_account": "a", "expected_namespace_organization": "o",
        "excluded_project_roots": [d.join("workspace")], "binding": null, "allowed_tools": ["query_memory"]}).to_string()).unwrap();
    assert!(t.execute("memq_status", &json!({}))["error"]
        .as_str()
        .unwrap()
        .starts_with("PermissionError"));
    fs::write(d.join("shared/config/memq.json"), json!({"enabled": true, "expected_account": "a", "expected_namespace_organization": "o",
        "excluded_project_roots": ["/nonexistent-root"], "binding": null, "allowed_tools": ["query_memory"]}).to_string()).unwrap();
    let r = t.execute("memq_status", &json!({}));
    assert_eq!(
        (
            r["result"]["binding_configured"].clone(),
            r["result"]["live_identity_verified"].clone()
        ),
        (json!(false), json!(false)),
        "{r}"
    );
    assert_eq!(
        t.execute("memq_tools", &json!({}))["result"]["reason"],
        json!("Verified binding not yet configured")
    );
    t.task = "update restricted client notes".into();
    assert_eq!(
        t.execute(
            "memq_call",
            &json!({"name": "query_memory", "arguments": {}})
        )["ok"],
        json!(false)
    );
}

#[test]
fn list_workers_reports_all_six_without_loading_models() {
    let (mut t, _) = tools("coordinator", "workers");
    t.root = crate::jobs::root();
    let r = t.execute("list_workers", &json!({}));
    let w = r["result"]["workers"].as_array().unwrap();
    assert_eq!(w.len(), 6);
    assert!(
        r["result"]["resources"]["physical_memory_gib"]
            .as_f64()
            .unwrap()
            > 1.0
    );
    assert!(worker_prompt().starts_with("Worker routes: coordinator:"));
}

// ---------- desktop scope, with a fake OS ----------

#[derive(Default)]
struct Fake {
    windows: Vec<Window>,
    front: Option<i64>,
    posted: Rc<RefCell<Vec<String>>>,
}

impl Desktop for Fake {
    fn windows(&self) -> Vec<Window> {
        self.windows.clone()
    }
    fn accessibility(&self) -> bool {
        true
    }
    fn screen_recording(&self) -> bool {
        true
    }
    fn display_size(&self) -> (f64, f64) {
        (1000.0, 800.0)
    }
    fn click(&self, x: f64, y: f64) {
        self.posted.borrow_mut().push(format!("click {x} {y}"))
    }
    fn type_text(&self, t: &str) {
        self.posted.borrow_mut().push(format!("type {t}"))
    }
    fn key(&self, c: u16, _: bool) {
        self.posted.borrow_mut().push(format!("key {c}"))
    }
    fn frontmost_pid(&self) -> Option<i64> {
        self.front
    }
    fn activate(&self, _: i64) -> Result<bool, String> {
        Ok(true)
    }
}

fn win(name: &str, pid: i64, on: bool) -> Window {
    Window {
        owner: Some("App".into()),
        process_id: pid,
        name: Some(name.into()),
        on_screen: on,
        layer: 0,
        bounds: (100.0, 100.0, 200.0, 200.0),
    }
}

fn desk(windows: Vec<Window>, front: Option<i64>) -> (Tools, Rc<RefCell<Vec<String>>>) {
    let (mut t, _) = tools("desktop", "desk");
    let posted = Rc::new(RefCell::new(vec![]));
    t.desktop = Box::new(Fake {
        windows,
        front,
        posted: posted.clone(),
    });
    t.desktop_window_title = Some("Fixture".into());
    (t, posted)
}

#[test]
fn desktop_missing_hidden_or_ambiguous_target_rejects_input() {
    for ws in [
        vec![],
        vec![win("Fixture", 123, false)],
        vec![win("Fixture", 123, true), win("Fixture", 123, true)],
    ] {
        let (t, _) = desk(ws, None);
        assert!(t.native_target(true).is_err());
    }
    let (t, _) = desk(vec![win("Fixture", 123, false)], None);
    assert_eq!(t.native_target(false).unwrap().unwrap().process_id, 123);
}

#[test]
fn desktop_focus_requires_observed_and_matching_target() {
    let (mut t, _) = desk(vec![win("Fixture", 123, true)], None);
    assert!(
        t.execute("desktop_focus", &json!({"process_id": 456}))["error"]
            .as_str()
            .unwrap()
            .starts_with("ValueError")
    );
    t.observed_desktop_pids.insert(456);
    assert!(
        t.execute("desktop_focus", &json!({"process_id": 456}))["error"]
            .as_str()
            .unwrap()
            .starts_with("PermissionError")
    );
    t.observed_desktop_pids.insert(123);
    assert_eq!(
        t.execute("desktop_focus", &json!({"process_id": 123}))["ok"],
        json!(true)
    );
}

#[test]
fn desktop_outside_target_click_and_unfocused_typing_post_no_input() {
    let (mut t, posted) = desk(vec![win("Fixture", 123, true)], Some(999));
    assert!(
        t.execute("desktop_click", &json!({"x": 10, "y": 10}))["error"]
            .as_str()
            .unwrap()
            .starts_with("PermissionError")
    );
    assert!(t.execute("desktop_type", &json!({"text": "hi"}))["error"]
        .as_str()
        .unwrap()
        .starts_with("PermissionError"));
    assert_eq!(
        t.execute("desktop_click", &json!({"x": 150, "y": 150}))["ok"],
        json!(true)
    );
    // normalized_1000 maps onto the 1000x800 display: (150, 250) -> (150, 200).
    assert_eq!(
        t.execute(
            "desktop_click",
            &json!({"x": 150, "y": 250, "coordinate_space": "normalized_1000"})
        )["result"]["clicked"],
        json!([150.0, 200.0])
    );
    assert_eq!(*posted.borrow(), vec!["click 150 150", "click 150 200"]);
}

#[test]
fn desktop_observation_is_bounded_and_query_cannot_widen_target() {
    let mut ws: Vec<Window> = (0..30)
        .map(|i| win(&format!("Other {i}"), 1000 + i, true))
        .collect();
    ws.push(win("Fixture", 123, true));
    let (mut t, _) = desk(ws, None);
    assert_eq!(
        t.desktop_observe("")["windows"].as_array().unwrap().len(),
        1
    );
    assert_eq!(t.desktop_observe("Other")["windows"], json!([]));
    t.desktop_window_title = None;
    let r = t.desktop_observe("");
    assert_eq!(r["windows"].as_array().unwrap().len(), 12);
    assert_eq!(r["truncated"], json!(true));
    assert_eq!(
        t.desktop_observe("Fixture")["windows"],
        json!([win("Fixture", 123, true).to_json()])
    );
}

// ---------- browser, live against headless Chrome and a loopback page ----------

fn serve_fixture() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader, Write};
                let mut r = BufReader::new(&s);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) <= 2 {
                        break;
                    }
                }
                let body = if path.starts_with("/long") {
                    format!("<h1>Top</h1><p>{}</p><h2 id='later'>Later section</h2><p>Deep fact: splashdown at 18:07.</p>", "filler words ".repeat(1200))
                } else if path.starts_with("/done") {
                    format!("<p>Submitted {}</p>", path.split("q=").nth(1).unwrap_or(""))
                } else {
                    "<h1>Fixture</h1><img src='https://example.com/x.png'><form action='/done'><input id='q' name='q'><button id='go'>Go</button></form>".into()
                };
                let mut w = &s;
                let _ = write!(w, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            });
        }
    });
    port
}

#[test]
fn browser_live_open_fill_click_screenshot_and_block() {
    if !browser::chrome_binary().is_file() {
        eprintln!("skipped: Chrome not installed");
        return;
    }
    let port = serve_fixture();
    let (mut t, d) = tools("browser", "browser");
    let base = format!("http://127.0.0.1:{port}/");
    assert_eq!(
        t.execute("browser_observe", &json!({}))["error"],
        json!("ValueError: Open a page first")
    );
    assert_eq!(
        t.execute("browser_open", &json!({"url": "https://example.com/"}))["ok"],
        json!(false)
    );
    let r = t.execute("browser_open", &json!({"url": base}));
    assert_eq!(r["ok"], json!(true), "{r}");
    assert!(r["result"]["text"].as_str().unwrap().contains("Fixture"));
    assert!(r["result"]["controls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "q"));
    assert_eq!(
        t.execute("browser_click", &json!({"selector": "input, button"}))["error"]
            .as_str()
            .unwrap()
            .contains("strict mode"),
        true
    );
    assert_eq!(
        t.execute("browser_fill", &json!({"selector": "#q", "text": "hello"}))["ok"],
        json!(true)
    );
    let r = t.execute("browser_click", &json!({"selector": "#go"}));
    assert!(
        r["result"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Submitted hello"),
        "{r}"
    );
    let r = t.execute("browser_screenshot", &json!({}));
    if browser::chrome_binary()
        .to_string_lossy()
        .to_lowercase()
        .contains("lightpanda")
    {
        assert_eq!(r["ok"], json!(false), "Lightpanda has no screenshots");
    } else {
        let shot = PathBuf::from(r["result"]["image_path"].as_str().unwrap());
        assert_eq!(desktop::png_size(&shot), Some((1000, 700)));
    }
    // A #fragment moves the 7000-character text window to that section.
    let r = t.execute("browser_open", &json!({"url": format!("{base}long")}));
    assert!(
        !r["result"]["text"].as_str().unwrap().contains("Deep fact"),
        "section is past the window without a fragment"
    );
    let r = t.execute("browser_open", &json!({"url": format!("{base}long#later")}));
    let text = r["result"]["text"].as_str().unwrap();
    assert!(
        text.starts_with("[from #later] Later section") && text.contains("Deep fact"),
        "{text}"
    );
    let profile_dirs = || {
        fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".chrome-profile")
            })
            .count()
    };
    assert_eq!(profile_dirs(), 1);
    t.close();
    assert_eq!(
        profile_dirs(),
        0,
        "profile removed and Chrome stopped on close"
    );
}

#[test]
fn browser_navigation_is_cancellable_mid_call() {
    if !browser::chrome_binary().is_file() {
        return;
    }
    // A loopback server that accepts and never answers: navigation hangs until cancelled.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let _held: Vec<_> = l.incoming().take(8).collect();
        std::thread::sleep(Duration::from_secs(60));
    });
    let (mut t, _) = tools("browser", "bcancel");
    // Derive cancellation from the monotonic clock instead of a competing helper thread.
    // Under a fully parallel suite, the helper could be starved and make this test measure
    // scheduler delay rather than the browser's 100 ms cancellation polling contract.
    let cancel_at = Instant::now() + Duration::from_millis(1200);
    t.cancelled = Box::new(move || Instant::now() >= cancel_at);
    let started = Instant::now();
    let r = t.execute(
        "browser_open",
        &json!({"url": format!("http://127.0.0.1:{port}/")}),
    );
    assert_eq!(r["error"], json!(CANCELLED), "{r}");
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn every_installed_skill_and_reference_is_readable() {
    let (mut t, _) = tools("coordinator", "realskills");
    t.root = crate::jobs::root();
    let skills = t.execute("list_skills", &json!({}))["result"]
        .as_array()
        .cloned()
        .unwrap();
    assert!(skills.len() >= 4);
    for s in skills {
        let r = t.execute("read_skill", &json!({"name": s["name"]}));
        assert_eq!(r["ok"], json!(true), "{r}");
        for reference in r["result"]["available_references"].as_array().unwrap() {
            assert_eq!(
                t.execute(
                    "read_skill",
                    &json!({"name": s["name"], "reference": reference})
                )["ok"],
                json!(true)
            );
        }
    }
    for (name, reference) in [
        ("memq-operations", "../../config/memq.json"),
        ("memq-operations", "/etc/passwd"),
    ] {
        assert_eq!(
            t.execute("read_skill", &json!({"name": name, "reference": reference}))["ok"],
            json!(false)
        );
    }
    let ot = t.execute("audit_ot_exposure", &json!({"path": "."}));
    assert_eq!(ot["ok"], json!(true), "{ot}");
    assert_eq!(ot["result"]["passed"], json!(true));
    let nx = t.execute("nexus_topology", &json!({"path": "."}));
    assert_eq!(nx["ok"], json!(true), "{nx}");
    assert_eq!(
        t.execute("audit_ot_exposure", &json!({"path": "../outside"}))["ok"],
        json!(false)
    );
}
