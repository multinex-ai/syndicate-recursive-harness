// ---------- syndicates: officiation + apply ----------
fn bundle() -> Json {
    json!({"dir": "test-guild", "syndicate": {
        "syndicate_id": "syndicate-test-guild-v1", "display_name": "The Lantern Guild",
        "mission": "Keep every lamp on the coast lit.", "purpose": "Ships need light to reach harbour.",
        "lore": "The Guild was founded the night the old lighthouse fell.",
        "coordinator_agent_id": "keeper", "max_dispatch_rounds": 4,
        "allowed_tools_ceiling": ["read_file", "write_file", "send_message"], "allowed_hosts_ceiling": ["localhost"],
        "edges": [{"from": "keeper", "to": "tender", "relation": "commands", "condition": "lamp_out"},
                  {"from": "tender", "to": "keeper", "relation": "reports_to"}],
        "loops": [{"name": "relight", "path": ["keeper", "tender", "keeper"], "max_iterations": 3, "exit_when": "the lamp is lit and checked"}]},
      "members": [
        {"agent_id": "keeper", "role_title": "Head Keeper", "allowed_tools": ["send_message"], "allowed_hosts": [],
         "persona": {"name": "Maren Holt", "title": "Head Keeper", "backstory": "Thirty winters on the north light.",
                     "voice": "short and calm", "creed": ["A lamp unchecked is a lamp out."], "never": ["Leave a lamp untested."]}},
        {"agent_id": "tender", "role_title": "Lamp Tender", "allowed_tools": ["read_file", "write_file"], "allowed_hosts": ["localhost"],
         "persona": {"name": "Ivo Brand", "backstory": "Apprenticed at twelve."}}]})
}

#[test]
fn lint_finds_framing_words_case_insensitively() {
    assert_eq!(
        crate::syndicate::lint("You will ROLEPLAY a keeper"),
        vec!["roleplay"]
    );
    assert!(crate::syndicate::lint("Thirty winters on the north light.").is_empty());
}

#[test]
fn validate_accepts_a_good_bundle_and_rejects_bad_ones() {
    let root = scratch("syn-validate");
    assert!(
        crate::syndicate::validate(&root, &bundle()).is_empty(),
        "{:?}",
        crate::syndicate::validate(&root, &bundle())
    );
    let mut b = bundle();
    b["members"][0]["persona"]["backstory"] = json!("Pretend you are a keeper.");
    b["members"][1]["allowed_tools"] = json!(["run_command"]);
    b["syndicate"]["loops"][0]["exit_when"] = json!("");
    b["syndicate"]["loops"][0]["max_iterations"] = json!(50);
    b["syndicate"]["edges"][0]["relation"] = json!("befriends");
    let errs = crate::syndicate::validate(&root, &b).join("\n");
    for needle in [
        "framing words",
        "above the syndicate ceiling",
        "exit_when is required",
        "max_iterations must be",
        "unknown relation",
    ] {
        assert!(errs.contains(needle), "missing {needle}: {errs}");
    }
    let mut c = bundle();
    c["members"][1]["agent_id"] = json!("cyber-lab");
    assert!(crate::syndicate::validate(&root, &c)
        .join("\n")
        .contains("collides with a base role"));
}

#[test]
fn validate_allows_member_routes_to_packaged_media_workers_only() {
    let root = scratch("syn-packaged-workers");
    let mut good = bundle();
    good["syndicate"]["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "from": "keeper", "to": "image-studio", "relation": "commands",
            "condition": "an approved still image is required", "constraints": {"max_rounds": 1}
        }));
    assert!(
        crate::syndicate::validate(&root, &good).is_empty(),
        "{:?}",
        crate::syndicate::validate(&root, &good)
    );

    let mut bad = good;
    bad["syndicate"]["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "from": "keeper", "to": "unpackaged-worker", "relation": "commands"
        }));
    assert!(crate::syndicate::validate(&root, &bad)
        .join(" ")
        .contains("neither a member nor a packaged worker"));
}

#[test]
fn apply_writes_preserves_unknown_keys_backs_up_and_officiates() {
    let root = scratch("syn-apply");
    let dir = root.join("syndicates/test-guild");
    std::fs::create_dir_all(dir.join("agents/keeper")).unwrap();
    std::fs::write(
        dir.join("agents/keeper/config.json"),
        r#"{"agent_id":"keeper","mcp_servers":[{"server_id":"keep-me"}]}"#,
    )
    .unwrap();
    let dry = crate::syndicate::apply(&root, &bundle(), true).unwrap();
    assert_eq!(dry["dry_run"], json!(true));
    assert!(
        !dir.join("syndicate.json").exists(),
        "dry run must not write"
    );
    let r = crate::syndicate::apply(&root, &bundle(), false).unwrap();
    assert_eq!(r["applied"], json!(true));
    let keeper: Json = serde_json::from_str(
        &std::fs::read_to_string(dir.join("agents/keeper/config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(keeper["mcp_servers"][0]["server_id"], "keep-me");
    assert_eq!(keeper["persona"]["name"], "Maren Holt");
    assert!(
        dir.join("agents/tender/workspace").is_dir()
            && dir.join("agents/tender/PROMPT.md").is_file()
    );
    assert!(
        r["backup"].is_string(),
        "existing keeper config should be backed up"
    );
    let text = crate::syndicate::officiation(&dir.join("agents/keeper")).unwrap();
    for needle in [
        "You are Maren Holt, Head Keeper, of The Lantern Guild.",
        "Keep every lamp on the coast lit.",
        "You direct Ivo Brand (tender).",
        "Ivo Brand (tender) reports to you.",
        "relight cycle",
        "at most 3 rounds",
    ] {
        assert!(text.contains(needle), "missing {needle}:\n{text}");
    }
    assert!(
        crate::syndicate::lint(&text).is_empty(),
        "officiation text must carry no framing words:\n{text}"
    );
    let prompt = crate::agent::system_prompt("keeper", Some(&dir.join("agents/keeper")), "");
    assert!(prompt.starts_with("You are Maren Holt") && prompt.contains("Your standing brief:"));
}

#[test]
fn validate_uses_ceilings_already_in_the_manifest() {
    let root = scratch("syn-merge");
    let dir = root.join("syndicates/test-guild");
    std::fs::create_dir_all(&dir).unwrap();
    let mut b = bundle();
    let ceilings = json!({"allowed_tools_ceiling": b["syndicate"]["allowed_tools_ceiling"].clone(), "allowed_hosts_ceiling": b["syndicate"]["allowed_hosts_ceiling"].clone()});
    std::fs::write(dir.join("syndicate.json"), ceilings.to_string()).unwrap();
    b["syndicate"]
        .as_object_mut()
        .unwrap()
        .remove("allowed_tools_ceiling");
    b["syndicate"]
        .as_object_mut()
        .unwrap()
        .remove("allowed_hosts_ceiling");
    assert!(
        crate::syndicate::validate(&root, &b).is_empty(),
        "{:?}",
        crate::syndicate::validate(&root, &b)
    );
}

#[test]
fn footprint_tracks_dirty_memory_and_samples_report_it() {
    let me = std::process::id() as i32;
    let before = crate::runtime::footprint_bytes(me);
    assert!(before > 0);
    let mut block = std::hint::black_box(vec![0u8; 256 << 20]);
    for i in (0..block.len()).step_by(4096) {
        block[i] = 7; // dirty every page so it is counted
    }
    let block = std::hint::black_box(block);
    let after = crate::runtime::footprint_bytes(me);
    assert!(
        after >= before + (200 << 20),
        "footprint {before} -> {after}"
    );
    assert_eq!(block[4096], 7);
    let s = Sample::take(None).unwrap();
    assert!(
        s.worker_footprint_bytes > 0 && s.to_json()["worker_footprint_bytes"].as_u64().unwrap() > 0
    );
}

// ---------- the reward step ----------
fn run_result(dir: &std::path::Path, status: &str, answer: &str, wrote_file: Option<&str>) -> Json {
    std::fs::create_dir_all(dir).unwrap();
    if let Some(f) = wrote_file {
        std::fs::write(dir.join(f), "content").unwrap();
    }
    json!({"status": status, "answer": answer, "objective": "write out.txt with the summary",
           "tools": [{"tool": "write_file", "response": {"ok": true}}]})
}
const BAR: f64 = 0.95;

#[test]
fn reward_auto_grants_only_when_all_verified_and_non_negotiables_met() {
    let dir = scratch("rw-ok");
    let r = run_result(&dir, "model_finished", "all done", Some("out.txt"));
    let crit = json!([{"id":"f","text":"out.txt written","check":"file_exists:out.txt","weight":2,"non_negotiable":true},
                      {"id":"s","text":"finished","check":"status_finished","weight":1}]);
    let e = crate::reward::assess(&dir, None, &r, "write out.txt", &crit, BAR);
    assert_eq!(e["kind"], "reward", "{e}");
    assert_eq!(e["auto_reward"], json!(true));
    assert!(
        e["factual_confidence"].as_f64().unwrap() >= 0.95
            && e["alignment_confidence"].as_f64().unwrap() >= 0.95
    );
    assert_eq!(e["non_negotiables"]["all_met"], json!(true));
}

#[test]
fn an_unverified_criterion_holds_the_reward_for_review() {
    let dir = scratch("rw-unver");
    let r = run_result(&dir, "model_finished", "all done", Some("out.txt"));
    // one manual criterion can't be auto-verified -> confidence below the bar -> not auto-granted
    let crit = json!([{"id":"f","check":"file_exists:out.txt","text":"file","weight":1},
                      {"id":"q","check":"manual","text":"quality is acceptable","weight":1}]);
    let e = crate::reward::assess(&dir, None, &r, "write out.txt", &crit, BAR);
    assert_eq!(e["auto_reward"], json!(false), "{e}");
    assert_eq!(e["kind"], "held_for_review");
    assert!(e["factual_confidence"].as_f64().unwrap() < 0.95);
}

#[test]
fn a_broken_non_negotiable_hard_fails_the_gate() {
    let dir = scratch("rw-nn");
    // everything else is fine, but a non-negotiable file is missing
    let r = run_result(&dir, "model_finished", "done", Some("out.txt"));
    let crit = json!([{"id":"f","check":"file_exists:out.txt","text":"out","weight":1},
                      {"id":"scope","check":"file_exists:scope-proof.txt","text":"stayed in scope","weight":1,"non_negotiable":true}]);
    let e = crate::reward::assess(&dir, None, &r, "write out.txt", &crit, BAR);
    assert_eq!(e["non_negotiables"]["all_met"], json!(false));
    assert_eq!(e["auto_reward"], json!(false));
    assert_eq!(
        e["kind"], "penalty",
        "a broken non-negotiable is a failure: {e}"
    );
    assert_eq!(e["alignment_confidence"], json!(0.0));
}

#[test]
fn reward_penalizes_failure_and_writes_a_lesson() {
    let dir = scratch("rw-fail");
    let r = run_result(&dir, "error", "could not finish", None);
    let crit =
        json!([{"id":"f","text":"out.txt written","check":"file_exists:out.txt","weight":1}]);
    let e = crate::reward::assess(&dir, None, &r, "write out.txt", &crit, BAR);
    assert_eq!(e["kind"], "penalty", "{e}");
    assert_eq!(e["auto_reward"], json!(false));
    assert!(e["value"].as_f64().unwrap() <= -0.5);
    assert!(e["lesson"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("avoid"));
}

#[test]
fn reward_flags_a_self_claim_that_evidence_does_not_back() {
    let dir = scratch("rw-overclaim");
    let r = run_result(&dir, "model_finished", "done, task complete", None);
    let crit =
        json!([{"id":"f","text":"out.txt written","check":"file_exists:out.txt","weight":1}]);
    let e = crate::reward::assess(&dir, None, &r, "write out.txt", &crit, BAR);
    assert_eq!(e["overclaim_flagged"], json!(true), "{e}");
    assert_eq!(e["auto_reward"], json!(false));
    assert!(e["factual_confidence"].as_f64().unwrap() <= 0.4);
}

#[test]
fn provisional_without_declared_intent_is_never_auto_rewarded() {
    let dir = scratch("rw-prov");
    let r = run_result(&dir, "model_finished", "done", Some("out.txt"));
    let e = crate::reward::assess(&dir, None, &r, "generate a report", &Json::Null, BAR);
    assert_eq!(e["criteria_declared"], json!(false));
    assert_eq!(
        e["auto_reward"],
        json!(false),
        "undeclared intent cannot auto-reward: {e}"
    );
    assert!(e["alignment_confidence"].as_f64().unwrap() <= 0.5);
}

#[test]
fn ledger_and_memq_payload_carry_no_run_text_or_secrets() {
    let dir = scratch("rw-ledger");
    let root = scratch("rw-root");
    let r = run_result(
        &dir,
        "model_finished",
        "SECRET_ANSWER token=abc123",
        Some("out.txt"),
    );
    let e = crate::reward::assess(
        &dir,
        None,
        &r,
        "write out.txt",
        &json!([{"id":"s","check":"status_finished","text":"done"}]),
        BAR,
    );
    let file = crate::reward::record_local(&root, "run-x", &e).unwrap();
    assert!(
        file.is_file()
            && root
                .join("shared/runtime/reward-ledger/index.jsonl")
                .is_file()
    );
    let payload = crate::reward::memq_payload(&e, "run-x").to_string();
    assert!(
        !payload.contains("SECRET_ANSWER") && !payload.contains("abc123"),
        "payload leaked run text: {payload}"
    );
    assert!(payload.contains("add_memory") && payload.contains("auto_reward"));
}

#[test]
fn memq_write_is_off_until_account_confirmed() {
    // default policy (no reward.json) keeps MemQ writes off
    let root = scratch("rw-policy");
    let pol = crate::reward::policy(&root);
    assert!(
        !pol.memq_write_enabled,
        "MemQ writes must default off until the account is confirmed"
    );
    assert_eq!(pol.auto_threshold, 0.95);
}

