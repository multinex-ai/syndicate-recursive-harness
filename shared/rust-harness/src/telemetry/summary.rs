use super::*;

// ---------- roster (the structure, independent of activity) ----------
pub fn roster(src_root: &Path) -> Json {
    let rd = |p: PathBuf| -> Json {
        fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Json::Null)
    };
    let mut out = vec![];
    let mut dirs: Vec<PathBuf> = fs::read_dir(src_root.join("syndicates"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let m = rd(d.join("syndicate.json"));
        if m.is_null() {
            continue;
        }
        let name = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let members: Vec<Json> = m["agent_ids"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).map(|id| {
            let adir = d.join("agents").join(id);
            let c = rd(adir.join("config.json"));
            json!({"agent_id": id, "role_title": c["role_title"], "persona_name": c["persona"]["name"], "persona_title": c["persona"]["title"],
                   "officiated": crate::syndicate::officiation(&adir).is_some(), "officiation": crate::syndicate::officiation(&adir),
                   "max_turns": c["max_turns"], "timeout_seconds": c["timeout_seconds"],
                   "allowed_tools": c["allowed_tools"], "allowed_hosts": c["allowed_hosts"]})
        }).collect();
        out.push(json!({"id": name, "syndicate_id": m["syndicate_id"], "display_name": m["display_name"], "mission": m["mission"], "purpose": m["purpose"],
                        "coordinator": m["coordinator_agent_id"], "officiation_enabled": m.pointer("/officiation/enabled") != Some(&json!(false)),
                        "state": m["state"], "max_dispatch_rounds": m["max_dispatch_rounds"], "members": members, "edges": m["edges"], "loops": m["loops"]}));
    }
    out.push(json!({"id": "base", "syndicate_id": "base", "display_name": "Base roles", "mission": "The built-in local workers.", "coordinator": "coordinator",
                    "officiation_enabled": false, "members": ROLES.iter().map(|r| json!({"agent_id": r, "officiated": false})).collect::<Vec<_>>(), "edges": [], "loops": []}));
    Json::Array(out)
}

// ---------- rollups ----------
fn pct(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    Some(v[rank.min(v.len()) - 1])
}
fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
fn opt(x: Option<f64>) -> Json {
    x.map(|v| json!(round(v))).unwrap_or(Json::Null)
}

#[derive(Default)]
struct Run {
    t: f64,
    dur: f64,
    status: String,
    turns: u64,
    tools: u64,
    tool_errors: u64,
    reward: String,
    value: f64,
    conf: f64,
    auto: bool,
    overclaim: bool,
    memq: String,
    officiated: Option<bool>,
}
#[derive(Default)]
struct Acc {
    runs: Vec<Run>,
    tools: BTreeMap<String, (u64, u64, Vec<f64>)>,
    tin: u64,
    tout: u64,
    tps: Vec<f64>,
    rss_peak: f64,
    avail_min: Option<f64>,
    warn_runs: u64,
    reclaim_runs: u64,
    missions: u64,
    missions_ok: u64,
}

/// Summarize spans in `[since, until]`. `syndicate` filters to one syndicate id when given.
pub fn summarize(
    data_root: &Path,
    src_root: &Path,
    since: f64,
    until: f64,
    syndicate: Option<&str>,
) -> Json {
    let all: Vec<Json> = read_spans(data_root, since, until)
        .into_iter()
        .filter(|s| syndicate.map(|x| s["syndicate"] == x).unwrap_or(true))
        .collect();
    let mut accs: BTreeMap<(String, String), Acc> = BTreeMap::new();
    let mut per_trace: BTreeMap<String, usize> = BTreeMap::new();
    let mut edges: BTreeMap<(String, String), (u64, u64, Vec<f64>)> = BTreeMap::new();
    let (mut dispatches, mut dispatches_ok) = (0u64, 0u64);
    for s in &all {
        *per_trace
            .entry(s["trace_id"].as_str().unwrap_or("").to_string())
            .or_default() += 1;
        let key = (
            s["syndicate"].as_str().unwrap_or("base").to_string(),
            s["agent"].as_str().unwrap_or("unknown").to_string(),
        );
        let a = &s["attrs"];
        let acc = accs.entry(key).or_default();
        match s["kind"].as_str().unwrap_or("") {
            "invoke_agent" => {
                let g = |k: &str| a[k].as_f64().unwrap_or(0.0);
                if g("harness.resource.pressure_warn_samples") > 0.0 {
                    acc.warn_runs += 1;
                }
                if g("harness.reclaim.quit_count") > 0.0 {
                    acc.reclaim_runs += 1;
                }
                if let Some(v) = a["harness.resource.rss_peak_bytes"].as_f64() {
                    acc.rss_peak = acc.rss_peak.max(v);
                }
                if let Some(v) = a["harness.resource.available_min_bytes"].as_f64() {
                    acc.avail_min = Some(acc.avail_min.map(|m| m.min(v)).unwrap_or(v));
                }
                acc.runs.push(Run {
                    t: s["start_unix"].as_f64().unwrap_or(0.0),
                    dur: s["duration_ms"].as_f64().unwrap_or(0.0),
                    status: s["status"].as_str().unwrap_or("").into(),
                    turns: a["harness.run.turns"].as_u64().unwrap_or(0),
                    tools: a["harness.run.tool_calls"].as_u64().unwrap_or(0),
                    tool_errors: a["harness.run.tool_errors"].as_u64().unwrap_or(0),
                    reward: a["harness.reward.kind"].as_str().unwrap_or("").into(),
                    value: g("harness.reward.value"),
                    conf: g("harness.reward.confidence"),
                    auto: a["harness.reward.auto"] == json!(true),
                    overclaim: a["harness.reward.overclaim"] == json!(true),
                    memq: a["harness.memq.state"].as_str().unwrap_or("").into(),
                    officiated: a["harness.officiated"].as_bool(),
                });
            }
            "chat" => {
                acc.tin += a["gen_ai.usage.input_tokens"].as_u64().unwrap_or(0);
                acc.tout += a["gen_ai.usage.output_tokens"].as_u64().unwrap_or(0);
                if let Some(v) = a["harness.decode_tps"].as_f64().filter(|v| *v > 0.0) {
                    acc.tps.push(v);
                }
            }
            "execute_tool" => {
                let e = acc
                    .tools
                    .entry(
                        a["gen_ai.tool.name"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string(),
                    )
                    .or_default();
                e.0 += 1;
                e.1 += u64::from(s["status"] != "ok");
                e.2.push(s["duration_ms"].as_f64().unwrap_or(0.0));
            }
            "dispatch" => {
                dispatches += 1;
                dispatches_ok += u64::from(s["status"] == "ok");
                let e = edges
                    .entry((
                        a["harness.dispatch.from"].as_str().unwrap_or("").into(),
                        a["harness.dispatch.to"].as_str().unwrap_or("").into(),
                    ))
                    .or_default();
                e.0 += 1;
                e.1 += u64::from(s["status"] == "ok");
                e.2.push(s["duration_ms"].as_f64().unwrap_or(0.0));
            }
            "mission" => {
                acc.missions += 1;
                acc.missions_ok += u64::from(s["status"] == "ok");
            }
            _ => {}
        }
    }
    let roster = roster(src_root);
    // every rostered agent appears, even with no runs
    for syn in roster.as_array().into_iter().flatten() {
        if syndicate.map(|x| syn["id"] != x).unwrap_or(false) {
            continue;
        }
        for m in syn["members"].as_array().into_iter().flatten() {
            accs.entry((
                syn["id"].as_str().unwrap_or("").into(),
                m["agent_id"].as_str().unwrap_or("").into(),
            ))
            .or_default();
        }
    }
    let (mut agents, mut signals) = (vec![], vec![]);
    let mut fleet = json!({"runs": 0, "ok": 0, "error": 0, "budget": 0, "cancelled": 0, "tool_calls": 0, "tool_errors": 0, "tokens_in": 0, "tokens_out": 0,
                           "reward": {"auto": 0, "held": 0, "penalty": 0, "provisional": 0, "none": 0}, "overclaims": 0,
                           "memq": {"written": 0, "failed": 0, "off": 0, "unavailable": 0}});
    let mut all_tps: Vec<f64> = vec![];
    let bump = |f: &mut Json, path: &str, by: u64| {
        let c = f.pointer(path).and_then(Json::as_u64).unwrap_or(0);
        *f.pointer_mut(path).unwrap() = json!(c + by);
    };
    for ((syn, agent), acc) in accs.iter_mut() {
        acc.runs
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
        let n = acc.runs.len() as u64;
        let count = |st: &str| acc.runs.iter().filter(|r| r.status == st).count() as u64;
        let (ok, err, bud, can) = (
            count("ok"),
            count("error"),
            count("budget"),
            count("cancelled"),
        );
        let calls: u64 = acc.runs.iter().map(|r| r.tools).sum();
        let terr: u64 = acc.runs.iter().map(|r| r.tool_errors).sum();
        let rw = |k: &str| acc.runs.iter().filter(|r| r.reward == k).count() as u64;
        let (auto, held, pen, prov) = (
            acc.runs.iter().filter(|r| r.auto).count() as u64,
            rw("held_for_review"),
            rw("penalty"),
            rw("provisional"),
        );
        let overclaims = acc.runs.iter().filter(|r| r.overclaim).count() as u64;
        let mut durs: Vec<f64> = acc.runs.iter().map(|r| r.dur).collect();
        let rated: Vec<&Run> = acc.runs.iter().filter(|r| !r.reward.is_empty()).collect();
        let last3_all_error = n >= 3 && acc.runs.iter().rev().take(3).all(|r| r.status == "error");
        let rate_bad = calls >= MIN_CALLS_FOR_RATE && (terr as f64 / calls as f64) > TOOL_ERR_RATE;
        let status = if n == 0 {
            "never_run"
        } else if last3_all_error || (n >= 3 && (ok as f64 / n as f64) < 0.5) {
            "failing"
        } else if err > 0 || bud > 0 || overclaims > 0 || pen > 0 || rate_bad {
            "degraded"
        } else {
            "healthy"
        };
        let mut push = |sev: &str, code: &str, text: String| {
            signals.push(json!({"severity": sev, "syndicate": syn, "agent": agent, "code": code, "text": text}))
        };
        if overclaims > 0 {
            push(
                "warn",
                "overclaim",
                format!("{overclaims} run(s) claimed success that the evidence did not back"),
            );
        }
        if held > 0 {
            push(
                "info",
                "held_for_review",
                format!(
                    "{held} reward episode(s) held below the 95% bar, waiting for a grader or you"
                ),
            );
        }
        if pen > 0 {
            push(
                "bad",
                "penalty",
                format!("{pen} run(s) penalised; lessons recorded"),
            );
        }
        if rate_bad {
            push(
                "warn",
                "tool_error_rate",
                format!(
                    "tool error rate {:.0}% over {calls} calls",
                    terr as f64 / calls as f64 * 100.0
                ),
            );
        }
        if bud > 0 {
            push(
                "warn",
                "budget_exhausted",
                format!("{bud} run(s) ran out of turn/output budget"),
            );
        }
        if acc.warn_runs > 0 {
            push(
                "info",
                "memory_pressure",
                format!("memory pressure warned during {} run(s)", acc.warn_runs),
            );
        }
        let tools_json: BTreeMap<String, Json> = acc
            .tools
            .iter_mut()
            .map(|(k, (c, e, ms))| {
                (
                    k.clone(),
                    json!({"calls": *c, "errors": *e, "p95_ms": opt(pct(ms, 95.0))}),
                )
            })
            .collect();
        agents.push(json!({"syndicate": syn, "agent": agent, "status": status, "runs": n, "ok": ok, "error": err, "budget": bud, "cancelled": can,
            "success_rate": if n > 0 { json!(round(ok as f64 / n as f64)) } else { Json::Null },
            "latency_ms": {"p50": opt(pct(&mut durs, 50.0)), "p95": opt(pct(&mut durs, 95.0)), "max": opt(durs.iter().cloned().fold(None, |m: Option<f64>, v| Some(m.map(|x| x.max(v)).unwrap_or(v))))},
            "turns_avg": if n > 0 { json!(round(acc.runs.iter().map(|r| r.turns).sum::<u64>() as f64 / n as f64)) } else { Json::Null },
            "tool_calls": calls, "tool_errors": terr, "tools": tools_json,
            "tokens": {"in": acc.tin, "out": acc.tout}, "decode_tps_avg": if acc.tps.is_empty() { Json::Null } else { json!(round(acc.tps.iter().sum::<f64>() / acc.tps.len() as f64)) },
            "reward": {"rated": rated.len(), "auto": auto, "held": held, "penalty": pen, "provisional": prov, "overclaims": overclaims,
                       "avg_value": if rated.is_empty() { Json::Null } else { json!(round(rated.iter().map(|r| r.value).sum::<f64>() / rated.len() as f64)) },
                       "avg_confidence": if rated.is_empty() { Json::Null } else { json!(round(rated.iter().map(|r| r.conf).sum::<f64>() / rated.len() as f64)) }},
            "resource": {"rss_peak_bytes": if acc.rss_peak > 0.0 { json!(acc.rss_peak) } else { Json::Null }, "available_min_bytes": acc.avail_min, "pressure_warn_runs": acc.warn_runs, "reclaim_runs": acc.reclaim_runs},
            "missions": acc.missions, "missions_ok": acc.missions_ok,
            "last_run_unix": acc.runs.last().map(|r| r.t), "officiated": acc.runs.last().and_then(|r| r.officiated),
            "runs_series": series(&acc.runs, since, until)}));
        for (k, v) in [
            ("runs", n),
            ("ok", ok),
            ("error", err),
            ("budget", bud),
            ("cancelled", can),
            ("tool_calls", calls),
            ("tool_errors", terr),
            ("tokens_in", acc.tin),
            ("tokens_out", acc.tout),
            ("overclaims", overclaims),
        ] {
            bump(&mut fleet, &format!("/{k}"), v);
        }
        for (k, v) in [
            ("auto", auto),
            ("held", held),
            ("penalty", pen),
            ("provisional", prov),
        ] {
            bump(&mut fleet, &format!("/reward/{k}"), v);
        }
        bump(&mut fleet, "/reward/none", n - rated.len() as u64);
        for r in &acc.runs {
            if !r.memq.is_empty() {
                bump(&mut fleet, &format!("/memq/{}", r.memq), 1);
            }
        }
        all_tps.extend(acc.tps.iter());
    }
    fleet["success_rate"] = if fleet["runs"].as_u64().unwrap_or(0) > 0 {
        json!(round(
            fleet["ok"].as_f64().unwrap_or(0.0) / fleet["runs"].as_f64().unwrap_or(1.0)
        ))
    } else {
        Json::Null
    };
    fleet["decode_tps_avg"] = if all_tps.is_empty() {
        Json::Null
    } else {
        json!(round(all_tps.iter().sum::<f64>() / all_tps.len() as f64))
    };
    fleet["dispatches"] = json!({"total": dispatches, "ok": dispatches_ok});
    for (trace, n) in &per_trace {
        if *n > RUNAWAY_SPANS {
            signals.push(json!({"severity": "warn", "code": "runaway_trace", "agent": null, "syndicate": null, "text": format!("trace {trace} has {n} spans (limit {RUNAWAY_SPANS}); possible runaway loop")}));
        }
    }
    let edge_list: Vec<Json> = edges.iter_mut().map(|((f, t), (c, ok, ms))| json!({"from": f, "to": t, "count": *c, "ok": *ok, "p50_ms": opt(pct(ms, 50.0))})).collect();
    // recent runs + their spans, for the trace view
    let mut recent: Vec<&Json> = all.iter().filter(|s| s["kind"] == "invoke_agent").collect();
    recent.sort_by(|a, b| {
        b["start_unix"]
            .as_f64()
            .partial_cmp(&a["start_unix"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    recent.truncate(50);
    let recent_ids: Vec<&str> = recent
        .iter()
        .map(|s| s["trace_id"].as_str().unwrap_or(""))
        .collect();
    let mut traces: BTreeMap<String, Vec<Json>> = BTreeMap::new();
    for s in &all {
        let t = s["trace_id"].as_str().unwrap_or("");
        if recent_ids.contains(&t) {
            let v = traces.entry(t.to_string()).or_default();
            if v.len() < 200 {
                v.push(s.clone());
            }
        }
    }
    let sev = |s: &Json| match s["severity"].as_str() {
        Some("bad") => 0,
        Some("warn") => 1,
        _ => 2,
    };
    signals.sort_by_key(sev);
    json!({"schema_version": SCHEMA_VERSION, "generated_unix": now(), "window": {"since": since, "until": until},
           "fleet": fleet, "agents": agents, "edges": edge_list, "signals": signals, "recent_runs": recent, "traces": traces, "roster": roster,
           "rules": {"status": "never_run: no runs in the window. failing: last 3 runs all errored, or under half succeeded (3+ runs). degraded: any error, budget stop, overclaim, penalty, or tool error rate above 25% (4+ calls). Otherwise healthy.",
                     "reward": "auto = both confidence scores at or above the bar, every non-negotiable verified, nothing overclaimed, intent declared. Below the bar = held for review.",
                     "tokens": "Summed from model-turn spans only, to avoid double counting.",
                     "timing": "Durations are measured. Start offsets of turn and tool spans inside a run are reconstructed from those durations.",
                     "runaway": format!("A trace with more than {RUNAWAY_SPANS} spans is flagged.")}})
}

/// 24 equal buckets across the window: `{runs, ok}` per bucket (counts only — no smoothing, no invention).
fn series(runs: &[Run], since: f64, until: f64) -> Json {
    let lo = if since > 0.0 {
        since
    } else {
        runs.first().map(|r| r.t).unwrap_or(0.0)
    };
    let hi = until.min(now()).max(lo + 1.0);
    let w = (hi - lo) / 24.0;
    let mut b = vec![(0u64, 0u64); 24];
    for r in runs {
        let i = (((r.t - lo) / w).floor() as i64).clamp(0, 23) as usize;
        b[i].0 += 1;
        b[i].1 += u64::from(r.status == "ok");
    }
    json!({"start": lo, "end": hi, "runs": b.iter().map(|x| x.0).collect::<Vec<_>>(), "ok": b.iter().map(|x| x.1).collect::<Vec<_>>()})
}

/// `24h`, `7d`, `90m`, or `all` -> unix seconds to subtract from now (None = all time).
pub fn parse_window(s: &str) -> Option<f64> {
    let s = s.trim();
    if s == "all" || s.is_empty() {
        return None;
    }
    let (n, unit) = s.split_at(s.len() - 1);
    let n: f64 = n.parse().ok()?;
    Some(
        n * match unit {
            "m" => 60.0,
            "h" => 3600.0,
            "d" => 86_400.0,
            _ => return None,
        },
    )
}
