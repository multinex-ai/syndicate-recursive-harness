// ---------- telemetry ----------
fn tele_record(role: &str, status: &str, tools_ok: &[bool]) -> Json {
    let tools: Vec<Json> = tools_ok.iter().map(|ok| if *ok { json!({"tool":"write_file","seconds":0.4,"arguments":{"path":"SECRET_PATH.txt","content":"SECRET_BODY"},"response":{"ok":true,"result":{}}}) }
        else { json!({"tool":"run_command","seconds":0.2,"arguments":{"cmd":"SECRET_CMD"},"response":{"ok":false,"error":"PermissionError: SECRET_ERR details"}}) }).collect();
    json!({"role": role, "objective": "SECRET_OBJECTIVE do the thing", "status": status, "seconds": 12.5, "answer": "SECRET_ANSWER done", "controller": "rust",
           "turns": [{"turn":1,"finish_reason":"tool_calls","usage":{"prompt_tokens":900,"completion_tokens":40},"timings":{"prompt_ms":800.0,"predicted_ms":1200.0,"predicted_per_second":33.3,"prompt_per_second":1100.0},"message":{"tool_calls":tools.iter().map(|_| json!({"id":"x"})).collect::<Vec<_>>()}},
                     {"turn":2,"finish_reason":"stop","usage":{"prompt_tokens":1000,"completion_tokens":20},"timings":{"prompt_ms":500.0,"predicted_ms":700.0,"predicted_per_second":28.0,"prompt_per_second":1500.0},"message":{}}],
           "tools": tools,
           "reward": {"kind":"reward","value":1.0,"confidence":0.99,"factual_confidence":0.99,"alignment_confidence":1.0,"auto_reward":true,"overclaim_flagged":false,"criteria_declared":true,"non_negotiables":{"all_met":true},"memq":{"written":false,"reason":"memq_write_enabled is off in shared/config/reward.json"}}})
}

#[test]
fn telemetry_spans_have_structure_and_never_carry_task_text_secrets_or_errors() {
    let root = scratch("tel-priv");
    let run_dir = root.join("runs/run-a");
    std::fs::create_dir_all(&run_dir).unwrap();
    let n = crate::telemetry::record_run(
        &root,
        &root,
        &run_dir,
        &tele_record("cyber-lab", "model_finished", &[true, false]),
        "qwen",
        Some(false),
        1_800_000_000.0,
    );
    assert_eq!(n, 1 + 2 + 2, "1 invoke_agent + 2 chat + 2 tool spans");
    let spans = crate::telemetry::read_spans(&root, 0.0, f64::MAX);
    let all = serde_json::to_string(&spans).unwrap();
    for secret in [
        "SECRET_OBJECTIVE",
        "SECRET_ANSWER",
        "SECRET_PATH",
        "SECRET_BODY",
        "SECRET_CMD",
        "SECRET_ERR",
    ] {
        assert!(!all.contains(secret), "telemetry leaked {secret}");
    }
    assert!(
        all.contains("PermissionError"),
        "error CLASS is kept (not the message)"
    );
    let inv = spans.iter().find(|s| s["kind"] == "invoke_agent").unwrap();
    assert_eq!(inv["attrs"]["gen_ai.usage.input_tokens"], json!(1900));
    assert_eq!(inv["attrs"]["harness.run.tool_errors"], json!(1));
    assert_eq!(inv["attrs"]["harness.reward.auto"], json!(true));
    assert_eq!(inv["attrs"]["harness.memq.state"], "off");
    assert!(
        spans
            .iter()
            .filter(|s| s["kind"] != "invoke_agent")
            .all(|s| s["parent_id"] == inv["span_id"]),
        "children link to the run"
    );
}

#[test]
fn telemetry_summary_rolls_up_honestly_including_agents_that_never_ran() {
    let root = scratch("tel-sum");
    let dir = root.join("syndicates/guild/agents");
    for a in ["keeper", "tender"] {
        std::fs::create_dir_all(dir.join(a)).unwrap();
        std::fs::write(
            dir.join(a).join("config.json"),
            format!(r#"{{"agent_id":"{a}"}}"#),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("syndicates/guild/syndicate.json"),
        r#"{"syndicate_id":"g","agent_ids":["keeper","tender"],"edges":[]}"#,
    )
    .unwrap();
    for (i, st) in ["model_finished", "model_finished", "error"]
        .iter()
        .enumerate()
    {
        let rd = root.join(format!("runs/r{i}"));
        std::fs::create_dir_all(&rd).unwrap();
        crate::telemetry::record_run(
            &root,
            &root,
            &rd,
            &tele_record("keeper", st, &[true]),
            "qwen",
            Some(true),
            1_800_000_000.0 + i as f64 * 100.0,
        );
    }
    let s = crate::telemetry::summarize(&root, &root, 0.0, f64::MAX, None);
    let agent = |n: &str| {
        s["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["agent"] == n)
            .cloned()
            .unwrap()
    };
    let k = agent("keeper");
    assert_eq!(
        (k["runs"].as_u64(), k["ok"].as_u64(), k["error"].as_u64()),
        (Some(3), Some(2), Some(1))
    );
    assert_eq!(k["status"], "degraded");
    assert_eq!(
        k["tokens"]["in"],
        json!(3 * 1900),
        "tokens summed from chat spans only"
    );
    let t = agent("tender");
    assert_eq!(
        t["status"], "never_run",
        "a rostered agent with no runs is never_run"
    );
    assert!(
        t["success_rate"].is_null() && t["latency_ms"]["p50"].is_null(),
        "no invented numbers for an idle agent"
    );
    assert_eq!(s["fleet"]["runs"], json!(3));
    assert_eq!(s["fleet"]["reward"]["auto"], json!(3));
}

#[test]
fn telemetry_links_child_runs_to_their_delegation_and_trace() {
    let root = scratch("tel-link");
    let child = root.join("runs/mission-1/message-abc123");
    std::fs::create_dir_all(&child).unwrap();
    assert_eq!(crate::telemetry::trace_of(&child), "mission-1");
    crate::telemetry::record_run(
        &root,
        &root,
        &child,
        &tele_record("cyber-lab", "model_finished", &[true]),
        "qwen",
        None,
        1_800_000_000.0,
    );
    let env = json!({"id":"abc123","sender":"coordinator","recipient":"cyber-lab","status":"completed","result":{"seconds":5.0}});
    crate::telemetry::record_dispatch(&root, &root, &child, &env, 1_800_000_000.0);
    let spans = crate::telemetry::read_spans(&root, 0.0, f64::MAX);
    let inv = spans.iter().find(|s| s["kind"] == "invoke_agent").unwrap();
    assert_eq!(inv["parent_id"], "dispatch-abc123");
    let s = crate::telemetry::summarize(&root, &root, 0.0, f64::MAX, None);
    assert_eq!(s["edges"][0]["from"], "coordinator");
    assert_eq!(s["edges"][0]["count"], json!(1));
}

#[test]
fn telemetry_backfill_is_idempotent_and_flags_runaway_traces() {
    let root = scratch("tel-back");
    let rd = root.join("runs/old-run");
    std::fs::create_dir_all(&rd).unwrap();
    std::fs::write(
        rd.join("result.json"),
        tele_record("cyber-lab", "model_finished", &[true]).to_string(),
    )
    .unwrap();
    let first = crate::telemetry::backfill(&root, &root, &root.join("runs"));
    assert_eq!(first["ingested"]["runs"], json!(1));
    let again = crate::telemetry::backfill(&root, &root, &root.join("runs"));
    assert_eq!(
        again["ingested"]["runs"],
        json!(0),
        "second pass must not duplicate"
    );
    assert_eq!(
        crate::telemetry::read_spans(&root, 0.0, f64::MAX)
            .iter()
            .filter(|s| s["kind"] == "invoke_agent")
            .count(),
        1
    );
    // a trace with far too many spans is flagged
    let many = vec![true; crate::telemetry::RUNAWAY_SPANS + 5];
    let rd2 = root.join("runs/loopy");
    std::fs::create_dir_all(&rd2).unwrap();
    crate::telemetry::record_run(
        &root,
        &root,
        &rd2,
        &tele_record("cyber-lab", "turn_budget_exhausted", &many),
        "qwen",
        None,
        1_800_000_500.0,
    );
    let s = crate::telemetry::summarize(&root, &root, 0.0, f64::MAX, None);
    assert!(s["signals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["code"] == "runaway_trace"));
}

#[test]
fn telemetry_window_parsing() {
    assert_eq!(crate::telemetry::parse_window("24h"), Some(86_400.0));
    assert_eq!(crate::telemetry::parse_window("7d"), Some(604_800.0));
    assert_eq!(crate::telemetry::parse_window("all"), None);
    assert_eq!(crate::telemetry::parse_window("nonsense"), None);
}

// ---------- syndicates at runtime: the config is what happens ----------
fn put_json(p: &Path, v: &Json) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

fn syn_root(tag: &str) -> PathBuf {
    let r = dispatch_root(tag);
    let sd = r.join("syndicates/guild");
    put_json(
        &sd.join("syndicate.json"),
        &json!({
        "syndicate_id": "g", "display_name": "The Lantern Guild", "mission": "Keep lamps lit.", "coordinator_agent_id": "keeper", "max_dispatch_rounds": 4,
        "allowed_tools_ceiling": ["read_file", "write_file", "list_files", "run_command", "browser_open", "send_message"], "allowed_hosts_ceiling": ["localhost", "example.org"],
        "agent_ids": ["keeper", "tender", "scout"],
        "edges": [{"from": "keeper", "to": "tender", "relation": "commands", "condition": "lamp_out", "constraints": {"max_rounds": 2}, "order": 2},
                  {"from": "tender", "to": "keeper", "relation": "reports_to"}, {"from": "keeper", "to": "scout", "relation": "feeds", "order": 1}],
        "loops": [{"name": "relight", "path": ["keeper", "tender", "keeper"], "max_iterations": 3, "exit_when": "lit"}]}),
    );
    let member = |id: &str, tools: Json, hosts: Json, extra: Json| {
        let mut c = json!({"agent_id": id, "allowed_tools": tools, "allowed_hosts": hosts, "max_turns": 7, "timeout_seconds": 90,
                           "persona": {"name": format!("Name {id}"), "backstory": "A steady hand."},
                           "workspace_rel_path": format!("syndicates/guild/agents/{id}/workspace")});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            c[k] = v;
        }
        std::fs::create_dir_all(sd.join("agents").join(id).join("inbox")).unwrap();
        put_json(&sd.join("agents").join(id).join("config.json"), &c);
    };
    member(
        "keeper",
        json!(["read_file", "write_file", "send_message"]),
        json!([]),
        json!({}),
    );
    member(
        "tender",
        json!(["read_file", "run_command", "list_dir"]),
        json!([]),
        json!({}),
    );
    member(
        "scout",
        json!(["browser_open", "read_file"]),
        json!(["example.org"]),
        json!({}),
    );
    r
}

fn tool_names(t: &crate::tools::Tools) -> Vec<String> {
    t.schemas
        .iter()
        .filter_map(|s| s["function"]["name"].as_str().map(String::from))
        .collect()
}
fn tools_for(root: &Path, role: &str, hosts: &[String]) -> crate::tools::Tools {
    crate::tools::Tools::with_root(
        root.to_path_buf(),
        role,
        &root.join("ws").join(role),
        &root.join("run"),
        hosts,
        "t",
    )
    .unwrap()
}

#[test]
fn a_members_tools_follow_its_config_ceiling_and_acknowledgement() {
    let root = syn_root("syn-tools");
    let tender = tool_names(&tools_for(&root, "tender", &[]));
    assert!(
        tender.contains(&"read_file".to_string()) && tender.contains(&"list_files".to_string()),
        "list_dir is an alias of list_files: {tender:?}"
    );
    assert!(
        !tender.contains(&"write_file".to_string()),
        "not in its allowlist"
    );
    assert!(
        !tender.contains(&"run_command".to_string()),
        "run_command is declared but exec is not acknowledged"
    );
    // acknowledging exec makes the same declaration real
    let cfg_path = root.join("syndicates/guild/agents/tender/config.json");
    let mut cfg: Json = serde_json::from_str(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap();
    cfg["exec_acknowledged"] = json!(true);
    put_json(&cfg_path, &cfg);
    assert!(tool_names(&tools_for(&root, "tender", &[])).contains(&"run_command".to_string()));
    // a tool above the ceiling is never granted, even if acknowledged
    cfg["allowed_tools"] = json!(["read_file", "browser_click"]);
    put_json(&cfg_path, &cfg);
    assert!(!tool_names(&tools_for(&root, "tender", &[])).contains(&"browser_click".to_string()));
    // base roles are untouched: cyber-lab still has run_command by role
    assert!(tool_names(&tools_for(&root, "cyber-lab", &[])).contains(&"run_command".to_string()));
}

#[test]
fn relationships_are_the_messaging_permissions() {
    let root = syn_root("syn-msg");
    let recipients = |role: &str| -> Option<Vec<String>> {
        let t = tools_for(&root, role, &[]);
        t.schemas
            .iter()
            .find(|s| s["function"]["name"] == "send_message")
            .map(|s| {
                s["function"]["parameters"]["properties"]["recipient"]["enum"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect()
            })
    };
    let mut k = recipients("keeper").unwrap();
    k.sort();
    assert_eq!(k, vec!["scout", "tender"]);
    assert_eq!(recipients("tender").unwrap(), vec!["keeper"]);
    assert!(
        recipients("scout").is_none(),
        "scout has no outgoing relationship, so send_message is removed"
    );
    // and a direct attempt to message a non-neighbour is refused
    let mut t = tools_for(&root, "tender", &[]);
    let r = t.execute(
        "send_message",
        &json!({"recipient": "scout", "message": "hi"}),
    );
    assert_eq!(r["ok"], json!(false), "{r}");
}

#[test]
fn a_members_hosts_are_narrowed_by_its_allowed_hosts() {
    let root = syn_root("syn-hosts");
    let t = tools_for(
        &root,
        "scout",
        &["example.org".to_string(), "evil.test".to_string()],
    );
    assert!(t.allowed_hosts.contains("example.org") && !t.allowed_hosts.contains("evil.test"));
    assert!(tool_names(&t).contains(&"browser_open".to_string()));
}

fn queue_syn(root: &Path, sender: &str, recipient: &str) -> Json {
    let id = crate::jobs::new_job_id().unwrap();
    let path = crate::agent::inbox_dir(root, recipient).join(format!("{id}.json"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, json!({"id": id, "sender": sender, "recipient": recipient, "message": "m", "needs_followup": false, "status": "queued",
        "allowed_hosts": [], "origin_task": "t", "media_preset": "draft", "reference_image": null}).to_string()).unwrap();
    json!({"tool": "send_message", "response": {"ok": true, "result": {"path": path, "status": "queued"}}})
}

#[test]
fn dispatch_honours_relationships_caps_and_inbox_location() {
    let root = syn_root("syn-disp");
    let ran = std::cell::RefCell::new(vec![]);
    let run_child = |role: &str, _: &str, _: &Path, _: Duration, _: &[String], _: &str| {
        ran.borrow_mut().push(role.to_string());
        ok_run(vec![], "ok")
    };
    let ctx = Ctx {
        root: root.clone(),
        run_child: &run_child,
        media_bin: root.join("bin"),
        counts: Default::default(),
    };
    // keeper -> tender (allowed, cap 2) three times, plus tender -> scout (no relationship)
    let planner = |_: &str, _: &Path, _: Duration| {
        ok_run(
            vec![
                queue_syn(&root, "keeper", "tender"),
                queue_syn(&root, "keeper", "tender"),
                queue_syn(&root, "keeper", "tender"),
                queue_syn(&root, "tender", "scout"),
            ],
            "plan",
        )
    };
    let r = orchestrate(
        &ctx,
        "go",
        &root.join("runs/syn1"),
        Duration::from_secs(60),
        10,
        &planner,
    )
    .unwrap();
    let statuses: Vec<String> = r["child_results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        statuses,
        vec!["completed", "completed", "blocked", "blocked"],
        "{statuses:?}"
    );
    let errors: Vec<String> = r["child_results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["error"].as_str().map(String::from))
        .collect();
    assert!(
        errors[0].contains("cap for this relationship is 2"),
        "{errors:?}"
    );
    assert!(
        errors[1].contains("No relationship allows tender to message scout"),
        "{errors:?}"
    );
    assert_eq!(
        *ran.borrow(),
        vec!["tender", "tender"],
        "blocked messages never run an agent"
    );
    assert_eq!(r["status"], json!("child_failed"));
    // receipts went to the syndicate member's inbox, and no stray base folder was created
    let receipts = std::fs::read_dir(root.join("syndicates/guild/agents/keeper/inbox"))
        .unwrap()
        .flatten()
        .count();
    assert!(receipts >= 2, "receipts land in the member's real inbox");
    assert!(
        !root.join("agents/keeper").exists() && !root.join("agents/tender").exists(),
        "a stray agents/<member> folder would shadow the member"
    );
}

#[test]
fn loop_caps_and_the_syndicate_cap_bound_delegation() {
    let root = syn_root("syn-cap");
    let m = crate::syndicate::membership(&root, "keeper").unwrap();
    assert_eq!(m.cap_to("tender"), 2, "the relationship's own cap wins");
    assert_eq!(
        m.cap_to("scout"),
        4,
        "no relationship cap, no loop on this pair: the syndicate-wide cap"
    );
    let t = crate::syndicate::membership(&root, "tender").unwrap();
    assert_eq!(
        t.cap_to("keeper"),
        3,
        "tender -> keeper runs inside the 3-iteration loop"
    );
}

#[test]
fn identity_text_carries_conditions_order_caps_and_who_you_may_message() {
    let root = syn_root("syn-ident");
    let text = crate::syndicate::officiation(&root.join("syndicates/guild/agents/keeper")).unwrap();
    assert!(
        text.contains("You direct Name tender (tender). When: lamp out. At most 2 rounds."),
        "{text}"
    );
    assert!(
        text.contains("You may send messages only to: Name scout (scout), Name tender (tender)")
            || text.contains(
                "You may send messages only to: Name tender (tender), Name scout (scout)"
            ),
        "{text}"
    );
    assert!(crate::syndicate::lint(&text).is_empty());
    let scout = crate::syndicate::officiation(&root.join("syndicates/guild/agents/scout")).unwrap();
    assert!(scout.contains("You do not delegate to anyone"), "{scout}");
    // order is honoured: the feed (order 1) is described before the command (order 2 by position)
    assert!(
        text.find("Your findings feed").unwrap() < text.find("You direct").unwrap(),
        "{text}"
    );
}

#[test]
fn validation_checks_limits_hosts_and_unique_member_ids_and_notes_warn() {
    let root = syn_root("syn-valid");
    let mut b = crate::syndicate::load_bundle(&root, "guild").unwrap();
    assert!(
        crate::syndicate::validate(&root, &b).is_empty(),
        "{:?}",
        crate::syndicate::validate(&root, &b)
    );
    b["members"][0]["max_turns"] = json!(99);
    b["members"][1]["timeout_seconds"] = json!(5);
    b["members"][2]["allowed_hosts"] = json!(["https://example.org/x"]);
    let e = crate::syndicate::validate(&root, &b).join(" ");
    for n in [
        "max_turns must be",
        "timeout_seconds must be",
        "is not a valid host",
    ] {
        assert!(e.contains(n), "{n}: {e}");
    }
    // a member id may belong to only one syndicate
    let mut other = crate::syndicate::load_bundle(&root, "guild").unwrap();
    other["dir"] = json!("second");
    other["syndicate"]["syndicate_id"] = json!("s2");
    assert!(crate::syndicate::validate(&root, &other)
        .join(" ")
        .contains("already belongs to syndicate \"guild\""));
    // notes: declared-only tools, alias, unreachable members
    let good = crate::syndicate::load_bundle(&root, "guild").unwrap();
    let notes: Vec<String> = crate::syndicate::notes(&root, &good)
        .iter()
        .map(|n| n["text"].as_str().unwrap().to_string())
        .collect();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("run_command") && n.contains("not acknowledged")),
        "{notes:?}"
    );
    assert!(notes.iter().any(|n| n.contains("list_dir")), "{notes:?}");
    assert!(
        notes
            .iter()
            .any(|n| n.contains("scout") && n.contains("cannot message anyone")),
        "{notes:?}"
    );
}

#[test]
fn apply_archives_removed_members_so_they_can_no_longer_run() {
    let root = syn_root("syn-remove");
    let mut b = crate::syndicate::load_bundle(&root, "guild").unwrap();
    b["members"]
        .as_array_mut()
        .unwrap()
        .retain(|m| m["agent_id"] != "scout");
    b["syndicate"]["edges"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["to"] != "scout");
    let dry = crate::syndicate::apply(&root, &b, true).unwrap();
    assert!(dry["writes"]
        .to_string()
        .contains("archive (member removed)"));
    assert!(
        root.join("syndicates/guild/agents/scout").exists(),
        "a dry run changes nothing"
    );
    crate::syndicate::apply(&root, &b, false).unwrap();
    assert!(!root.join("syndicates/guild/agents/scout").exists());
    assert!(
        crate::agent::role_dir(&root, "scout").is_none(),
        "the removed member can no longer be resolved or run"
    );
    let archived = std::fs::read_dir(root.join("syndicates/guild/.history"))
        .unwrap()
        .flatten()
        .any(|e| e.path().join("removed/scout").exists());
    assert!(
        archived,
        "removal is reversible: the folder is archived, not deleted"
    );
}

