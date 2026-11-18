//! local-agents-rs: Rust port of the Local Agents control plane.
//!   mcp                 serve the stdio MCP bridge (drop-in for mcp_server.py)
//!   supervise <job_id>  run one job's supervisor (spawned by the bridge)
//!   status              print local_agents_status as JSON
//!   resources           print a resource sample and the admission verdict
//!   reclaim-plan        show what memory reclaim would quit right now (quits nothing)
//!   reclaim             quit candidate apps until memory target is met (asks first)
//!   reclaim-status | reclaim-sleep [hours] | reclaim-wake | reclaim-off | reclaim-on   pause or switch off memory cleanup
//!   agent <role> <task> [--max-turns N] [--timeout S] [--allow-host H]... [--vision] [--run-dir D] [--workspace W]
//!   image <prompt> [--preset P] [--reference-image F] [--output O] ...   FLUX.2 klein render under the Rust guard
//!   video <prompt> [--preset P] [--output O] ...   Wan clip under the Rust guard
//!   model-smoke [--vision]  load Qwen on Metal, ask one short question, unload
//!   officiation <role>  print the identity block a syndicate member is given
//!   syndicate-apply <bundle.json> [--dry-run]  validate an editor export and write it
mod agent;
mod assist;
mod dispatch;
mod forge;
mod jobs;
mod mcp;
mod media;
mod reclaim;
mod reclaim_consent;
mod report;
mod reward;
mod runtime;
mod sqlite;
mod supervisor;
mod syndicate;
mod telemetry;
#[cfg(test)]
mod tests;
mod tools;

fn main() {
    unsafe { libc::umask(0o077) };
    let args: Vec<String> = std::env::args().collect();
    let store = match jobs::JobStore::default_store() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("job store unavailable: {e}");
            std::process::exit(2);
        }
    };
    let result = match args.get(1).map(String::as_str) {
        Some("mcp") => mcp::serve(&store),
        Some("supervise") => match args.get(2) {
            Some(id) => supervisor::supervise(&store, id, supervisor::command),
            None => Err("usage: local-agents-rs supervise <job_id>".into()),
        },
        Some("status") => mcp::call_tool(&store, "local_agents_status", &serde_json::json!({}))
            .map(|v| println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default())),
        Some("resources") => runtime::Sample::take(None).map(|s| {
            let mut v = s.to_json();
            v["admission_blocked_reason"] = serde_json::json!(runtime::admission_reason(&s));
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        }),
        Some("reclaim-plan") => reclaim_plan(),
        Some("reclaim") => reclaim_cli(),
        Some(c @ ("reclaim-status" | "reclaim-sleep" | "reclaim-wake" | "reclaim-off" | "reclaim-on")) => reclaim_control(c, &args[2..]),
        Some("reward") => reward_cli(&args[2..]),
        Some("telemetry") => telemetry_cli(&args[2..]),
        Some("report") => report_cli(&args[2..]),
        Some("forge") => forge_cli(&args[2..]),
        Some("assist") => assist_cli(&args[2..]),
        Some("agent") => agent_cli(&args[2..]),
        Some("officiation") => match args.get(2).and_then(|r| agent::role_dir(&jobs::root(), r)) {
            Some(d) => syndicate::officiation(&d).map(|t| print!("{t}")).ok_or_else(|| "no persona for this role, or officiation is off".to_string()),
            None => Err("usage: local-agents-rs officiation <syndicate member id>".into()),
        },
        Some("syndicate-apply") => match args.get(2) {
            Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))
                .and_then(|t| serde_json::from_str(&t).map_err(|e| format!("{p}: {e}")))
                .and_then(|b| syndicate::apply(&jobs::root(), &b, args.iter().any(|a| a == "--dry-run")))
                .map(|r| println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default())),
            None => Err("usage: local-agents-rs syndicate-apply <bundle.json> [--dry-run]".into()),
        },
        Some(kind @ ("image" | "video")) => {
            agent::install_cancel_handlers();
            let runner = media::Runner::standard();
            let r = if kind == "image" { media::run_image(&args[2..], &runner) } else { media::run_video(&args[2..], &runner) };
            r.map(|v| println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        Some("model-smoke") => model_smoke(args.iter().any(|a| a == "--vision")),
        Some("-h" | "--help" | "help") => {
            print_help();
            Ok(())
        }
        _ => Err("usage: local-agents-rs <mcp|supervise|status|resources|reclaim-*|agent|image|video|model-smoke|assist|forge|report|telemetry|reward|officiation|syndicate-apply>; run with --help for details".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "Local Syndicate Harness² control plane (local-agents-rs)\n\n\
         Commands:\n  \
           agent <role> <task> [--dispatch-limit N] [--timeout S]   Run a bounded agent or recursive relay\n  \
           model-smoke [--vision]                                   Verify Metal Qwen load, reply, and teardown\n  \
           video <prompt> [--preset draft|quality] [--output FILE]  Generate a short local Wan 2.1 video clip\n  \
           image <prompt> [--preset P] [--output FILE]              Run FLUX.2 image worker (gated until licensed)\n  \
           forge [--port 4287]                                      Start the loopback Syndicate Forge web console\n  \
           report [--syndicate ID] [--since WINDOW]                 Generate a self-contained HTML run report\n  \
           status | resources                                       Print worker readiness or memory admission JSON\n  \
           reclaim-plan | reclaim | reclaim-status                  Inspect or run memory reclamation\n  \
           mcp                                                      Serve the stdio MCP worker bridge"
    );
}

/// Live gate: heavy lock -> admission -> llama.cpp on Metal -> one completion -> teardown.
fn model_smoke(vision: bool) -> Result<(), String> {
    let run_dir = jobs::root().join(format!("runs/rust-model-smoke-{}", jobs::now() as u64));
    let started = std::time::Instant::now();
    let server = runtime::ModelServer::start(&runtime::ServerConfig::qwen(vision), &run_dir)?;
    let load_s = started.elapsed().as_secs_f64();
    let reply = server.request(
        "/v1/chat/completions",
        Some(&serde_json::json!({
        "model": "local-agent", "temperature": 0, "max_tokens": 16,
        "messages": [{"role": "user", "content": "Reply with exactly: RUST-OK"}]})),
        std::time::Duration::from_secs(120),
    )?;
    let text = reply
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let pid = server.pid();
    drop(server);
    let result = serde_json::json!({"run_dir": run_dir, "server_pid": pid, "load_seconds": load_s,
        "total_seconds": started.elapsed().as_secs_f64(), "reply": text, "reply_matches": text.contains("RUST-OK"),
        "server_stopped": runtime::resident_bytes(pid as i32) == 0});
    std::fs::write(
        run_dir.join("smoke.json"),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).unwrap_or_default()
    );
    Ok(())
}

// reward <run_dir> [--mission M] [--criteria file.json] [--memq] [--workspace W]
// Scores a finished run's result.json against its intent and records a reward/penalty episode.
fn reward_cli(args: &[String]) -> Result<(), String> {
    let run_dir = args.iter().find(|a| !a.starts_with("--")).ok_or(
        "usage: local-agents-rs reward <run_dir> [--mission M] [--criteria f.json] [--memq]",
    )?;
    let run_dir = std::path::PathBuf::from(run_dir);
    let opt = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let result: serde_json::Value = std::fs::read_to_string(run_dir.join("result.json"))
        .map_err(|e| format!("{}: {e}", run_dir.join("result.json").display()))
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))?;
    let mission = opt("--mission")
        .or_else(|| {
            result
                .get("objective")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    let declared = opt("--criteria")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .or_else(|| {
            std::fs::read_to_string(run_dir.join("acceptance.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
        })
        .unwrap_or(serde_json::Value::Null);
    let workspace = opt("--workspace").map(std::path::PathBuf::from);
    let root = jobs::root();
    let pol = reward::policy(&root);
    let episode = reward::assess(
        &run_dir,
        workspace.as_deref(),
        &result,
        &mission,
        &declared,
        pol.auto_threshold,
    );
    let run_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("run");
    let file = reward::record_local(&root, run_id, &episode)?;
    let mut out = serde_json::json!({"episode": episode, "ledger": file});
    if args.iter().any(|a| a == "--memq") {
        let py = root.join("shared/runtime/venv/bin/python");
        let ws = workspace.unwrap_or(root.clone());
        out["memq"] = reward::to_memq(
            &root,
            &py,
            &ws,
            &mission,
            &reward::memq_payload(&episode, run_id),
        );
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(())
}

// telemetry summary|export <out.json>|backfill|roster  [--since 24h|7d|all] [--syndicate ID]
fn telemetry_cli(args: &[String]) -> Result<(), String> {
    let root = jobs::root();
    let opt = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let since = opt("--since")
        .as_deref()
        .and_then(telemetry::parse_window)
        .map(|s| jobs::now() - s)
        .unwrap_or(0.0);
    let summary = || {
        telemetry::summarize(
            &root,
            &root,
            since,
            jobs::now() + 1.0,
            opt("--syndicate").as_deref(),
        )
    };
    let pretty = |v: &serde_json::Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match args.first().map(String::as_str).unwrap_or("summary") {
        "backfill" => println!(
            "{}",
            pretty(&telemetry::backfill(&root, &root, &root.join("runs")))
        ),
        "roster" => println!("{}", pretty(&telemetry::roster(&root))),
        "export" => {
            let out = args
                .get(1)
                .filter(|a| !a.starts_with("--"))
                .ok_or("usage: local-agents-rs telemetry export <out.json> [--since 7d]")?;
            std::fs::write(out, pretty(&summary())).map_err(|e| format!("{out}: {e}"))?;
            println!("wrote {out}");
        }
        "summary" => println!("{}", pretty(&summary())),
        other => {
            return Err(format!(
                "unknown telemetry command {other}; use summary|export|backfill|roster"
            ))
        }
    }
    Ok(())
}

// report [<syndicate>] [--since 24h|7d|all] [--all]
fn assist_cli(args: &[String]) -> Result<(), String> {
    let usage = "usage: local-agents-rs assist generate <description> | explain <topic> | tutorial";
    let root = jobs::root();
    let run_dir = root
        .join("runs")
        .join(format!("assist-{}", jobs::now() as u64));
    let cfg = runtime::ServerConfig::qwen(false);
    let out = match args.first().map(String::as_str) {
        Some("generate") => assist::generate_syndicate(
            &root,
            &cfg,
            &run_dir,
            &args.get(1..).map(|a| a.join(" ")).unwrap_or_default(),
        )?,
        Some("explain") => assist::explain(
            &root,
            &cfg,
            &run_dir,
            &args.get(1..).map(|a| a.join(" ")).unwrap_or_default(),
        )?,
        Some("tutorial") => assist::tutorial(),
        _ => return Err(usage.into()),
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    Ok(())
}

fn forge_cli(args: &[String]) -> Result<(), String> {
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(4287);
    let root = jobs::root();
    let assets = root.join("shared/forge-console");
    let (bound, _stop) = forge::serve(root.clone(), assets, port)?;
    let tok = forge::token(&root)?;
    println!("Forge server on http://127.0.0.1:{bound}/?token={tok}");
    println!(
        "(token also at {})",
        forge::dir(&root).join("token").display()
    );
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn report_cli(args: &[String]) -> Result<(), String> {
    let root = jobs::root();
    let since = args
        .iter()
        .position(|a| a == "--since")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| telemetry::parse_window(s));
    if args.iter().any(|a| a == "--all") {
        for p in report::generate_all(&root, since)? {
            println!("{}", p.display());
        }
        return Ok(());
    }
    let syn = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str);
    println!("{}", report::generate(&root, syn, since)?.display());
    Ok(())
}

fn reclaim_plan() -> Result<(), String> {
    let policy =
        reclaim::Policy::load(&jobs::root())?.ok_or("no shared/config/memory-reclaim.json")?;
    let avail = runtime::available_bytes().unwrap_or(0);
    let order: Vec<serde_json::Value> = policy
        .apps
        .iter()
        .filter(|a| reclaim::app_running(a))
        .map(|a| serde_json::json!({"app": a, "bytes": reclaim::app_rss_bytes(a), "why": "named"}))
        .chain(
            reclaim::apps_by_memory()
                .into_iter()
                .filter(|(n, b)| {
                    policy.auto_largest
                        && *b >= policy.min_app_bytes
                        && !policy.apps.contains(n)
                        && !reclaim::is_protected(n, &policy.never)
                })
                .map(|(n, b)| serde_json::json!({"app": n, "bytes": b, "why": "largest"})),
        )
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
        "available_bytes": avail, "target_bytes": policy.target_available_bytes,
        "would_act": avail < policy.target_available_bytes, "candidate_order": order}))
        .unwrap_or_default()
    );
    Ok(())
}

fn reclaim_control(cmd: &str, rest: &[String]) -> Result<(), String> {
    use reclaim_consent as c;
    let root = jobs::root();
    match cmd {
        "reclaim-sleep" => {
            let hours: f64 = rest
                .first()
                .map(|h| h.parse().map_err(|_| "hours must be a number".to_string()))
                .transpose()?
                .unwrap_or(c::SLEEP_HOURS as f64);
            if !(0.1..=720.0).contains(&hours) {
                return Err("hours must be between 0.1 and 720".into());
            }
            c::sleep_for(&root, (hours * 3600.0) as u64)?;
        }
        "reclaim-wake" => c::wake(&root)?,
        "reclaim-off" => c::set_enabled(&root, false)?,
        "reclaim-on" => {
            c::set_enabled(&root, true)?;
            c::wake(&root)?;
        }
        _ => {}
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&c::status(&root)).unwrap_or_default()
    );
    Ok(())
}

fn reclaim_cli() -> Result<(), String> {
    let policy =
        reclaim::Policy::load(&jobs::root())?.ok_or("no shared/config/memory-reclaim.json")?;
    let avail = || runtime::available_bytes().unwrap_or(0);
    let receipt = reclaim::reclaim(
        &policy,
        &avail,
        &runtime::pressure_normal,
        &reclaim::app_running,
        &reclaim::quit_app,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&receipt.to_json()).unwrap_or_default()
    );
    Ok(())
}

/// CLI parity with `agent.py <role> <task>`, including coordinator dispatch.
fn agent_cli(args: &[String]) -> Result<(), String> {
    let usage = "usage: local-agents-rs agent <role> <task> [--max-turns N] [--timeout S] [--allow-host H]... [--vision] [--run-dir D] [--workspace W] [--dispatch-limit N] [--scope-task T] [--desktop-window-title T]";
    let (role, task) = match args {
        [r, t, ..] => (r.clone(), t.clone()),
        _ => return Err(usage.into()),
    };
    let r = jobs::root();
    let dir = agent::role_dir(&r, &role).ok_or(format!("Unknown role: {role}"))?;
    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("config.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let member = syndicate::membership(&r, &role);
    let mut workspace = match &member {
        Some(m) => m.workspace(&r),
        None => config
            .get("workspace")
            .and_then(|v| v.as_str())
            .map(|w| r.join(w))
            .unwrap_or_else(|| dir.join("workspace")),
    };
    let mut run_dir = r.join("runs").join(format!(
        "rust-agent-{}-{}",
        jobs::now() as u64,
        &jobs::new_job_id()?[..6]
    ));
    let mut max_turns = config
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .unwrap_or(16) as u32;
    let mut timeout = config
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(600);
    let mut vision = config
        .get("vision")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut hosts = vec![];
    let mut scope_task: Option<String> = None;
    let mut window_title: Option<String> = None;
    let mut dispatch_limit: usize = if role == "coordinator" {
        3
    } else if member
        .as_ref()
        .map(|m| m.coordinator() == role)
        .unwrap_or(false)
    {
        member
            .as_ref()
            .map(|m| m.max_dispatch_rounds().min(10) as usize)
            .unwrap_or(0)
    } else {
        0
    };
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--max-turns" => max_turns = val()?.parse().map_err(|_| "bad --max-turns")?,
            "--timeout" => timeout = val()?.parse().map_err(|_| "bad --timeout")?,
            "--allow-host" => hosts.push(val()?),
            "--run-dir" => run_dir = val()?.into(),
            "--workspace" => workspace = val()?.into(),
            "--vision" => vision = true,
            "--scope-task" => scope_task = Some(val()?),
            "--desktop-window-title" => window_title = Some(val()?),
            "--dispatch-limit" => {
                dispatch_limit = val()?.parse().map_err(|_| "bad --dispatch-limit")?
            }
            _ => return Err(usage.into()),
        }
    }
    if max_turns < 1 || timeout < 1 {
        return Err("Budgets must be positive".into());
    }
    if dispatch_limit > 10 {
        return Err("Dispatch limit must be 0-10".into());
    }
    agent::install_cancel_handlers();
    let server_cfg = runtime::ServerConfig::for_role(&r, &role, vision)?;
    let result = if dispatch_limit > 0 {
        let parent = |objective: &str, dir: &std::path::Path, t: std::time::Duration| {
            let mut cfg = agent::AgentConfig::new(
                &role,
                objective,
                workspace.clone(),
                dir.to_path_buf(),
                vision,
            );
            cfg.server = server_cfg.clone();
            cfg.max_turns = max_turns;
            cfg.timeout = t;
            cfg.allowed_hosts = hosts.clone();
            cfg.scope_task = Some(scope_task.clone().unwrap_or_else(|| task.clone()));
            cfg.desktop_window_title = window_title.clone();
            agent::run_agent(&cfg)
        };
        let ctx = dispatch::Ctx {
            root: r.clone(),
            run_child: &run_child,
            media_bin: r.join("shared/bin"),
            counts: Default::default(),
        };
        dispatch::orchestrate(
            &ctx,
            &task,
            &run_dir,
            std::time::Duration::from_secs(timeout),
            dispatch_limit,
            &parent,
        )?
    } else {
        let mut cfg = agent::AgentConfig::new(&role, &task, workspace, run_dir, vision);
        cfg.server = server_cfg;
        cfg.max_turns = max_turns;
        cfg.timeout = std::time::Duration::from_secs(timeout);
        cfg.allowed_hosts = hosts;
        cfg.scope_task = scope_task;
        cfg.desktop_window_title = window_title;
        agent::run_agent(&cfg)
    };
    let summary: serde_json::Map<String, serde_json::Value> = result
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.as_str() != "turns" && k.as_str() != "tools")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if result["status"] == "model_finished" {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// A dispatched child: the recipient role's own config, workspace, turn budget and vision setting.
fn run_child(
    role: &str,
    message: &str,
    dir: &std::path::Path,
    timeout: std::time::Duration,
    hosts: &[String],
    scope: &str,
) -> serde_json::Value {
    let r = jobs::root();
    let home = agent::role_dir(&r, role);
    let config: serde_json::Value = home
        .as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("config.json")).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let workspace = match syndicate::membership(&r, role) {
        Some(m) => m.workspace(&r),
        None => config
            .get("workspace")
            .and_then(|v| v.as_str())
            .map(|w| r.join(w))
            .unwrap_or_else(|| r.join("agents").join(role).join("workspace")),
    };
    let vision = config
        .get("vision")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut cfg = agent::AgentConfig::new(role, message, workspace, dir.to_path_buf(), vision);
    match runtime::ServerConfig::for_role(&r, role, vision) {
        Ok(server) => cfg.server = server,
        Err(e) => {
            return serde_json::json!({"role": role, "objective": message, "status": "error", "error": e, "turns": [], "tools": []})
        }
    }
    cfg.max_turns = config
        .get("max_turns")
        .and_then(|v| v.as_u64())
        .unwrap_or(16) as u32;
    cfg.timeout = timeout;
    cfg.allowed_hosts = hosts.to_vec();
    cfg.scope_task = Some(scope.to_string());
    agent::run_agent(&cfg)
}
