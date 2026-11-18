// ---------- Qwen-assisted authoring (fake model server; no GPU needed) ----------
fn fake_json_reply(json_text: &str) -> String {
    format!(
        r#"
import http.server, json, time
time.sleep(0.3)
REPLY = {reply}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def reply(self, obj):
        b = json.dumps(obj).encode()
        self.send_response(200); self.send_header('Content-Type','application/json')
        self.send_header('Content-Length', str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self): self.reply({{'status':'ok'}})
    def do_POST(self):
        n = int(self.headers['Content-Length']); self.rfile.read(n)
        self.reply({{'choices':[{{'message':{{'content': REPLY}}}}]}})
http.server.HTTPServer(('127.0.0.1', PORT), H).serve_forever()
"#,
        reply = json_text
    )
}

#[test]
fn assist_generate_syndicate_parses_validates_and_never_applies() {
    let root = dispatch_root("assist-gen");
    let draft = json!({"dir": "night-watch", "syndicate": {"syndicate_id": "syndicate-night-watch-v1", "display_name": "Night Watch", "mission": "Keep watch.",
        "coordinator_agent_id": "warden", "edges": [{"from": "warden", "to": "scout", "relation": "commands"}]},
        "members": [{"agent_id": "warden", "persona": {"name": "Aldric Vane", "backstory": "A steady veteran of the watch."}},
                    {"agent_id": "scout", "persona": {"name": "Brin Ashwood", "backstory": "Quick-footed and quiet."}}]});
    let port = free_port();
    let dir = scratch("assist-gen-srv");
    let cfg = fake_cfg(
        &dir,
        port,
        &fake_json_reply(&serde_json::to_string(&draft.to_string()).unwrap()),
    );
    let out = crate::assist::generate_syndicate(
        &root,
        &cfg,
        &dir.join("run"),
        "a small night-watch team with a warden and a scout",
    )
    .unwrap();
    assert_eq!(out["valid"], json!(true), "{out}");
    assert_eq!(out["bundle"]["dir"], "night-watch");
    assert!(
        !root.join("syndicates/night-watch").exists(),
        "generate must never write to disk by itself"
    );
}

#[test]
fn assist_generate_syndicate_rejects_unparseable_or_malformed_output() {
    let root = dispatch_root("assist-badgen");
    let port = free_port();
    let dir = scratch("assist-badgen-srv");
    let cfg = fake_cfg(&dir, port, &fake_json_reply("'not valid json at all'"));
    let e =
        crate::assist::generate_syndicate(&root, &cfg, &dir.join("run"), "anything").unwrap_err();
    assert!(e.contains("not parseable") || e.contains("shape"), "{e}");
}

#[test]
fn assist_explain_answers_a_relation_instantly_with_no_model_call() {
    let root = dispatch_root("assist-explain-rel");
    // port 0 / unreachable program: if explain() tried to start a model for a known relation id, this would fail
    let mut cfg = ServerConfig::qwen(false);
    cfg.program = "/no/such/binary".into();
    let out = crate::assist::explain(&root, &cfg, &root.join("run"), "commands").unwrap();
    assert_eq!(out["mode"], "relation");
    assert_eq!(out["answer"]["id"], "commands");
}

#[test]
fn assist_explain_free_form_uses_the_model_and_is_grounded() {
    let root = dispatch_root("assist-explain-free");
    let port = free_port();
    let dir = scratch("assist-explain-free-srv");
    let cfg = fake_cfg(
        &dir,
        port,
        &fake_json_reply("\"Tools are granted per member, never above the syndicate ceiling.\""),
    );
    let out = crate::assist::explain(
        &root,
        &cfg,
        &dir.join("run"),
        "how do tools get granted to a member?",
    )
    .unwrap();
    assert_eq!(out["mode"], "free");
    assert!(out["answer"].as_str().unwrap().contains("ceiling"));
}

#[test]
fn assist_tutorial_is_fixed_ordered_and_covers_the_whole_flow() {
    let steps = crate::assist::tutorial();
    let arr = steps.as_array().unwrap();
    assert!(arr.len() >= 8);
    for (i, s) in arr.iter().enumerate() {
        assert_eq!(s["step"], json!(i + 1));
        assert!(
            !s["title"].as_str().unwrap().is_empty() && !s["body"].as_str().unwrap().is_empty()
        );
    }
    let titles: Vec<&str> = arr.iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert!(
        titles.contains(&"Validate")
            && titles.contains(&"Apply")
            && titles.contains(&"Run a mission")
    );
}

#[test]
fn forge_assist_routes_are_token_gated_and_reach_the_same_logic() {
    let (_root, port, tok) = start_forge("forge-assist");
    let (s, _, _) = http(
        port,
        "POST",
        "/api/assist/explain",
        None,
        Some(&json!({"topic": "commands"}).to_string()),
    );
    assert_eq!(s, 401);
    let (s2, _, b2) = http(
        port,
        "POST",
        "/api/assist/explain",
        Some(&tok),
        Some(&json!({"topic": "commands"}).to_string()),
    );
    assert_eq!(s2, 200, "{b2}");
    let j2: Json = serde_json::from_str(&b2).unwrap();
    assert_eq!(j2["mode"], "relation");
    let (s3, _, b3) = http(port, "GET", "/api/tutorial", Some(&tok), None);
    assert_eq!(s3, 200);
    assert!(
        serde_json::from_str::<Json>(&b3)
            .unwrap()
            .as_array()
            .unwrap()
            .len()
            >= 8
    );
}

#[test]
fn forge_serves_the_icon_and_only_the_allow_listed_static_files() {
    let (root, port, _) = start_forge("forge-icon");
    let (s, h, b) = http(port, "GET", "/icon.svg", None, None);
    assert_eq!(s, 200, "{b}");
    assert!(h
        .get("content-type")
        .map(|t| t.contains("svg"))
        .unwrap_or(false));
    let (s2, _, _) = http(port, "GET", "/does-not-exist.txt", None, None);
    assert_eq!(
        s2, 401,
        "an unknown path demands the token first, so nobody can probe which paths exist"
    );
    let tok = crate::forge::token(&root).unwrap();
    let (s3, _, _) = http(port, "GET", "/does-not-exist.txt", Some(&tok), None);
    assert_eq!(
        s3, 404,
        "with the token it is a plain not-found: only allow-listed static files are ever served"
    );
    let (s4, _, _) = http(port, "GET", "/../../etc/passwd", Some(&tok), None);
    assert_eq!(s4, 404, "path traversal reaches nothing");
}

// ---------- Forge: run detail, machine stats, safe stop ----------
#[test]
fn run_detail_returns_prompt_and_response_cycles_and_refuses_paths_outside_runs() {
    let root = scratch("forge-detail");
    let rd = root.join("runs/r-detail");
    std::fs::create_dir_all(&rd).unwrap();
    std::fs::write(rd.join("prompt.json"), json!({"system": "You are Maren Holt.", "objective": "light the lamp", "officiated": true, "system_sha256": "abc"}).to_string()).unwrap();
    std::fs::write(rd.join("result.json"), json!({"status": "model_finished", "seconds": 3.2, "answer": "done",
        "turns": [{"turn": 1, "finish_reason": "tool_calls", "usage": {"prompt_tokens": 10, "completion_tokens": 5}, "timings": {"predicted_per_second": 18.0},
                   "message": {"content": "thinking", "tool_calls": [{"function": {"name": "write_file"}}]}}],
        "tools": [{"tool": "write_file", "seconds": 0.01, "arguments": {"path": "a.txt"}, "response": {"ok": false, "error": "PermissionError: nope"}}]}).to_string()).unwrap();
    let d = crate::forge::test_run_detail(&root, "runs/r-detail");
    assert_eq!(d["available"], json!(true));
    assert_eq!(d["prompt"]["system"], "You are Maren Holt.");
    assert_eq!(d["record"]["turns"][0]["tool_calls"][0], "write_file");
    assert_eq!(d["record"]["tools"][0]["ok"], json!(false));
    // a stored path that escapes runs/ must resolve to nothing, even if the target exists
    std::fs::create_dir_all(root.join("secret")).unwrap();
    std::fs::write(
        root.join("secret/result.json"),
        r#"{"answer":"TOP SECRET"}"#,
    )
    .unwrap();
    for evil in ["runs/../secret", "../secret", "/etc", "secret"] {
        let e = crate::forge::test_run_detail(&root, evil);
        assert_eq!(e["available"], json!(false), "{evil}: {e}");
        assert!(!e.to_string().contains("TOP SECRET"));
    }
}

#[test]
fn run_detail_truncates_huge_text_and_says_when_a_run_predates_prompt_capture() {
    let root = scratch("forge-detail2");
    let rd = root.join("runs/old");
    std::fs::create_dir_all(&rd).unwrap();
    std::fs::write(
        rd.join("result.json"),
        json!({"status": "model_finished", "answer": "x".repeat(50_000)}).to_string(),
    )
    .unwrap();
    let d = crate::forge::test_run_detail(&root, "runs/old");
    assert!(d["prompt"].is_null() && d["note"].as_str().unwrap().contains("predates"));
    assert!(
        d["record"]["answer"].as_str().unwrap().len() < 7_000,
        "long answers are truncated"
    );
}

#[test]
fn forge_machine_route_reports_live_resources_and_lock_state() {
    let (_root, port, tok) = start_forge("forge-machine");
    let (s, _, b) = http(port, "GET", "/api/machine", Some(&tok), None);
    assert_eq!(s, 200, "{b}");
    let j: Json = serde_json::from_str(&b).unwrap();
    assert!(j["model_lock_held"].is_boolean());
    if j["ok"] == json!(true) {
        for k in [
            "available_bytes",
            "pressure_level",
            "swap_used_bytes",
            "disk_free_bytes",
            "physical_bytes",
        ] {
            assert!(j["sample"][k].is_number(), "{k}");
        }
    }
}

#[test]
fn a_started_mission_can_be_stopped_and_is_reaped_and_strangers_cannot() {
    let root = syn_root("forge-stop");
    let script = root.join("fake-agent.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let m = crate::forge::test_spawn_with(&script, &root, "keeper", "light the lamp").unwrap();
    let id = m["job_id"].as_str().unwrap().to_string();
    assert!(crate::forge::test_registered(&id));
    assert!(
        crate::forge::test_stop("not-a-job-this-server-started").is_err(),
        "an unknown id must be refused, never treated as a pid"
    );
    assert!(crate::forge::test_stop("1").is_err());
    crate::forge::test_stop(&id).unwrap();
    let mut gone = false;
    for _ in 0..60 {
        if !crate::forge::test_registered(&id) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        gone,
        "after SIGTERM the child exits, the reaper reaps it and removes it from the registry"
    );
    assert!(
        crate::forge::test_stop(&id).is_err(),
        "a finished mission can no longer be stopped"
    );
}

#[test]
fn a_ceiling_and_a_member_that_both_say_list_dir_agree() {
    let root = syn_root("syn-alias");
    let mut b = crate::syndicate::load_bundle(&root, "guild").unwrap();
    b["syndicate"]["allowed_tools_ceiling"] = json!(["read_file", "list_dir", "send_message"]);
    b["members"][0]["allowed_tools"] = json!(["read_file", "list_dir"]);
    let e = crate::syndicate::validate(&root, &b);
    assert!(
        !e.iter()
            .any(|x| x.starts_with("keeper:") && x.contains("above the syndicate ceiling")),
        "{e:?}"
    );
    b["members"][0]["allowed_tools"] = json!(["read_file", "write_file"]);
    assert!(crate::syndicate::validate(&root, &b)
        .iter()
        .any(|x| x.starts_with("keeper:") && x.contains("above the syndicate ceiling")));
}

#[test]
fn warning_tolerance_is_opt_in_and_never_covers_critical_or_low_memory() {
    use crate::runtime::memory_warning;
    let g = 1u64 << 30;
    // default: any non-normal pressure is a warning
    assert!(!memory_warning(1, 6 * g, None));
    assert!(memory_warning(2, 6 * g, None));
    // tolerance set to 2 GiB: warning pressure is tolerated only while >= 2 GiB is available
    assert!(!memory_warning(2, 3 * g, Some(2 * g)));
    assert!(memory_warning(2, g + g / 2 + 1, Some(2 * g)));
    // critical pressure and under 1.5 GiB always count
    assert!(memory_warning(4, 6 * g, Some(2 * g)));
    assert!(memory_warning(1, g, Some(g)));
}
