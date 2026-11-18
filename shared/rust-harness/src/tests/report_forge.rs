// ---------- periodic HTML reports ----------
#[test]
fn report_renders_sections_counts_uncertainties_and_leaks_no_task_text() {
    let root = scratch("rep-basic");
    let dir = root.join("runs/r1");
    std::fs::create_dir_all(&dir).unwrap();
    crate::telemetry::record_run(
        &root,
        &root,
        &dir,
        &tele_record("cyber-lab", "model_finished", &[true]),
        "qwen",
        Some(false),
        crate::jobs::now(),
    );
    crate::jobs::JobStore::open(root.join("shared/runtime/mcp-jobs")).unwrap(); // creates the db so generate() can open it
    let path = crate::report::generate(&root, None, None).unwrap();
    let html = std::fs::read_to_string(&path).unwrap();
    for needle in [
        "Status report",
        "Uncertainties",
        "Agents",
        "Mission completion claims",
        "Delegation traffic",
        "Measured",
        "<table>",
    ] {
        assert!(html.contains(needle), "missing {needle}");
    }
    for secret in [
        "SECRET_OBJECTIVE",
        "SECRET_ANSWER",
        "SECRET_PATH",
        "SECRET_CMD",
        "SECRET_ERR",
    ] {
        assert!(!html.contains(secret), "report leaked {secret}");
    }
    assert!(html.contains("<b>1</b>"), "runs count should show");
}

#[test]
fn report_flags_self_reported_risk_and_distinguishes_measured_from_self_reported() {
    let root = scratch("rep-selfreport");
    let dir = root.join("runs/r2");
    std::fs::create_dir_all(&dir).unwrap();
    let t = crate::jobs::now();
    std::fs::write(dir.join("status-reports.jsonl"), format!("{}\n", json!({"at_unix": t, "progress": "halfway", "uncertainty": "SECRET_RISK_DETAIL unclear scope", "risk": "high"}))).unwrap();
    let mut rec = tele_record("cyber-lab", "model_finished", &[true]);
    rec["status_reports"] = json!([{"at_unix": t, "progress": "halfway", "uncertainty": "unclear scope", "risk": "high"}]);
    rec["reward"]["auto_reward"] = json!(false); // below the gate -> self_reported, not measured
    crate::telemetry::record_run(&root, &root, &dir, &rec, "qwen", Some(false), t + 0.05);
    let html =
        std::fs::read_to_string(crate::report::generate(&root, None, None).unwrap()).unwrap();
    assert!(
        html.contains("high risk") || html.contains(">high<"),
        "{html}"
    );
    assert!(html.contains("Self-reported") || html.contains("Unverified"));
    assert!(
        !html.contains("SECRET_RISK_DETAIL"),
        "free-text uncertainty content must not leak into telemetry/report"
    );
}

#[test]
fn report_cli_writes_both_timestamped_and_latest_files() {
    let root = scratch("rep-cli");
    std::fs::create_dir_all(root.join("runs")).unwrap();
    let p1 = crate::report::generate(&root, None, None).unwrap();
    assert!(p1.ends_with("fleet-latest.html"));
    assert!(root
        .join("shared/runtime/reports")
        .read_dir()
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("fleet-")
            && !e.file_name().to_string_lossy().ends_with("latest.html")));
}

#[test]
fn report_drills_down_into_response_cycles_tool_calls_and_reward_timing() {
    let root = scratch("rep-drill");
    let dir = root.join("runs/r3");
    std::fs::create_dir_all(&dir).unwrap();
    crate::telemetry::record_run(
        &root,
        &root,
        &dir,
        &tele_record("cyber-lab", "model_finished", &[true, false]),
        "qwen",
        Some(false),
        crate::jobs::now(),
    );
    let html =
        std::fs::read_to_string(crate::report::generate(&root, None, None).unwrap()).unwrap();
    assert!(
        html.contains("Response cycles") && html.contains("Tool calls"),
        "{html}"
    );
    assert!(html.contains("tok/s"), "decode speed should show per cycle");
    assert!(
        html.contains("prompt.json") && html.contains("result.json"),
        "drilldown should point at the local files, not embed them"
    );
    assert!(
        html.contains("Auto-granted") || html.contains("Held"),
        "reward timing/reasoning should be stated"
    );
    assert!(
        !html.contains("SECRET_OBJECTIVE")
            && !html.contains("SECRET_ANSWER")
            && !html.contains("SECRET_CMD"),
        "drilldown must stay secret-free like the rest of the report"
    );
}

#[test]
fn drilldown_does_not_mix_cycles_from_a_different_run_in_the_same_trace() {
    let root = scratch("rep-mix");
    let mission = root.join("runs/mission-x");
    let planner = mission.join("planner");
    let child = mission.join("message-abc123");
    std::fs::create_dir_all(&planner).unwrap();
    std::fs::create_dir_all(&child).unwrap();
    crate::telemetry::record_run(
        &root,
        &root,
        &planner,
        &tele_record("coordinator", "model_finished", &[true]),
        "qwen",
        None,
        crate::jobs::now(),
    );
    crate::telemetry::record_run(
        &root,
        &root,
        &child,
        &tele_record("cyber-lab", "model_finished", &[false]),
        "qwen",
        None,
        crate::jobs::now() + 1.0,
    );
    assert_eq!(crate::telemetry::trace_of(&planner), "mission-x");
    assert_eq!(
        crate::telemetry::trace_of(&child),
        "mission-x",
        "planner and child share one trace"
    );
    let html =
        std::fs::read_to_string(crate::report::generate(&root, None, None).unwrap()).unwrap();
    let blocks: Vec<&str> = html.split("<details class=\"run\">").skip(1).collect();
    assert_eq!(
        blocks.len(),
        2,
        "two runs should each get their own drilldown card"
    );
    for b in &blocks {
        let cycles_table = b
            .split("Response cycles")
            .nth(1)
            .and_then(|t| t.split("</table>").next())
            .unwrap_or("");
        let rows = cycles_table
            .split("<tbody>")
            .nth(1)
            .map(|t| t.matches("<tr>").count())
            .unwrap_or(0);
        assert_eq!(rows, 2, "each run's own cycles table must show exactly its 2 turns, not another run's turns mixed in:\n{cycles_table}");
    }
}

// ---------- the Forge server: real HTTP over a real socket ----------
fn http(
    port: u16,
    method: &str,
    path: &str,
    tok: Option<&str>,
    body: Option<&str>,
) -> (u16, std::collections::HashMap<String, String>, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let b = body.unwrap_or("");
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(t) = tok {
        head += &format!("Authorization: Bearer {t}\r\n");
    }
    if !b.is_empty() {
        head += &format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        );
    }
    head += "\r\n";
    s.write_all(head.as_bytes()).unwrap();
    s.write_all(b.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        })
        .collect();
    (status, headers, body.to_string())
}

fn start_forge(tag: &str) -> (PathBuf, u16, String) {
    start_forge_on(syn_root(tag))
}
fn start_forge_on(root: PathBuf) -> (PathBuf, u16, String) {
    let assets = root.join("console");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<html>forge shell</html>").unwrap();
    std::fs::write(assets.join("icon.svg"), "<svg/>").unwrap();
    let (port, _stop) = crate::forge::serve(root.clone(), assets, 0).unwrap();
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let tok = crate::forge::token(&root).unwrap();
    (root, port, tok)
}

#[test]
fn forge_token_is_created_once_random_and_file_permissioned() {
    let root = scratch("forge-tok");
    let t1 = crate::forge::token(&root).unwrap();
    let t2 = crate::forge::token(&root).unwrap();
    assert_eq!(t1, t2, "token is stable across calls");
    assert!(t1.len() >= 32);
    let root2 = scratch("forge-tok2");
    assert_ne!(
        t1,
        crate::forge::token(&root2).unwrap(),
        "two installs get different tokens"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(crate::forge::dir(&root).join("token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "token file must not be group/world readable");
    }
}

#[test]
fn forge_requires_the_bearer_token_on_every_api_route_but_serves_the_shell_and_health_openly() {
    let (_root, port, tok) = start_forge("forge-auth");
    let (s1, _, b1) = http(port, "GET", "/api/summary", None, None);
    assert_eq!(s1, 401, "{b1}");
    let (s2, _, _) = http(
        port,
        "GET",
        "/api/summary",
        Some("wrong-token-entirely-00000000000000"),
        None,
    );
    assert_eq!(s2, 401);
    let (s3, _, b3) = http(port, "GET", "/api/summary", Some(&tok), None);
    assert_eq!(s3, 200, "{b3}");
    let (s4, _, b4) = http(port, "GET", "/", None, None);
    assert_eq!(s4, 200, "{b4}");
    assert!(b4.contains("forge shell"));
    let (s5, _, _) = http(port, "GET", "/api/health", None, None);
    assert_eq!(
        s5, 200,
        "health check needs no token so the console can probe liveness before it has one"
    );
}

#[test]
fn forge_summary_roster_and_guide_routes_return_real_structured_data() {
    let (_root, port, tok) = start_forge("forge-data");
    let (s, _, b) = http(port, "GET", "/api/summary?since=all", Some(&tok), None);
    assert_eq!(s, 200);
    let j: Json = serde_json::from_str(&b).unwrap();
    assert!(j["fleet"].is_object() && j["agents"].is_array());
    let (s2, _, b2) = http(port, "GET", "/api/roster", Some(&tok), None);
    assert_eq!(s2, 200);
    let j2: Json = serde_json::from_str(&b2).unwrap();
    assert!(j2["roster"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == "guild"));
    assert_eq!(j2["relation_ids"].as_array().unwrap().len(), 7);
    let (s3, _, b3) = http(port, "GET", "/api/guide", Some(&tok), None);
    assert_eq!(s3, 200);
    assert!(b3.contains("\"commands\""));
}

#[test]
fn forge_validate_and_apply_routes_reject_bad_bundles_and_dry_run_by_default() {
    // a plain root with no pre-existing syndicate, so the Lantern Guild fixture's member ids
    // (keeper, tender) don't collide with a syndicate start_forge("...") would have pre-populated
    let (root, port, tok) = start_forge_on(dispatch_root("forge-apply"));
    let (s, _, b) = http(port, "POST", "/api/validate", Some(&tok), Some("{}"));
    assert_eq!(s, 200);
    let j: Json = serde_json::from_str(&b).unwrap();
    assert_eq!(j["ok"], json!(false));
    assert!(!j["errors"].as_array().unwrap().is_empty());
    let good = bundle(); // the Lantern Guild fixture from the syndicate tests above
    let body = good.to_string();
    let (s2, _, b2) = http(
        port,
        "POST",
        &format!("/api/validate"),
        Some(&tok),
        Some(&body),
    );
    assert_eq!(s2, 200);
    let j2: Json = serde_json::from_str(&b2).unwrap();
    assert_eq!(j2["ok"], json!(true), "{j2}");
    let (_s3, _, b3) = http(port, "POST", "/api/apply", Some(&tok), Some(&body)); // no ?dry_run=false
    let j3: Json = serde_json::from_str(&b3).unwrap();
    assert_eq!(
        j3["result"]["dry_run"],
        json!(true),
        "apply defaults to a dry run"
    );
    assert!(
        !root.join("syndicates/test-guild").exists(),
        "a dry run must not write anything"
    );
    let (s4, _, b4) = http(
        port,
        "POST",
        "/api/apply?dry_run=false",
        Some(&tok),
        Some(&body),
    );
    assert_eq!(s4, 201.min(s4).max(200)); // 200 from json_res
    let j4: Json = serde_json::from_str(&b4).unwrap();
    assert_eq!(j4["result"]["applied"], json!(true), "{j4}");
    assert!(root.join("syndicates/test-guild/syndicate.json").exists());
}

#[test]
fn forge_mission_route_refuses_an_unknown_role_before_anything_runs() {
    let (root, port, tok) = start_forge("forge-mission");
    let (s, _, b) = http(
        port,
        "POST",
        "/api/mission",
        Some(&tok),
        Some(&json!({"role": "not-a-real-role", "objective": "do a thing"}).to_string()),
    );
    assert_eq!(s, 400, "{b}");
    assert!(
        !root.join("runs").exists() || root.join("runs").read_dir().unwrap().next().is_none(),
        "no run directory should be created for a refused mission"
    );
}

#[test]
fn mission_precheck_bounds_the_objective_and_writes_acceptance_criteria() {
    let root = syn_root("forge-precheck");
    let e1 = crate::forge::test_precheck(&root, "keeper", "", &Json::Null).unwrap_err();
    assert!(e1.contains("1-16000"));
    let e2 = crate::forge::test_precheck(&root, "nope", "go", &Json::Null).unwrap_err();
    assert!(e2.contains("Unknown role"));
    let crit = json!([{"id": "c", "text": "x", "check": "status_finished"}]);
    let (id, dir) = crate::forge::test_precheck(&root, "keeper", "light the lamp", &crit).unwrap();
    assert!(!id.is_empty());
    assert_eq!(
        serde_json::from_str::<Json>(
            &std::fs::read_to_string(dir.join("acceptance.json")).unwrap()
        )
        .unwrap(),
        crit
    );
}

#[test]
fn unknown_routes_and_non_json_bodies_fail_closed() {
    let (_root, port, tok) = start_forge("forge-404");
    let (s, _, _) = http(port, "GET", "/api/not-a-route", Some(&tok), None);
    assert_eq!(s, 404);
    let (s2, _, b2) = http(
        port,
        "POST",
        "/api/validate",
        Some(&tok),
        Some("not json at all"),
    );
    assert_eq!(s2, 400, "{b2}");
}

