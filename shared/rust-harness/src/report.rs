//! Periodic HTML status reports: one self-contained page per syndicate (or the whole fleet),
//! built only from telemetry spans, reward episodes and the job store — never generated prose
//! about "how things are going". Every number here traces to `telemetry::summarize`.
//!
//! An "uncertainty" is anything the harness cannot itself verify: a held-for-review reward (below
//! the 95% confidence gate), an overclaimed success, a stale job heartbeat, memory pressure during
//! a run, or a signal from `telemetry::summarize`'s own rules. Mission completion is reported as
//! `measured` (evidence-backed) or `self_reported` (the agent said so, unverified) — never blurred
//! into one word.
use crate::jobs::now;
use crate::telemetry;
use serde_json::{json, Value as Json};
use std::fs;
use std::path::{Path, PathBuf};

pub fn dir(root: &Path) -> PathBuf {
    root.join("shared/runtime/reports")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn pct(x: Option<f64>) -> String {
    x.map(|v| format!("{:.0}%", v * 100.0))
        .unwrap_or_else(|| "—".into())
}
fn ago(t: Option<f64>) -> String {
    match t {
        None => "never".into(),
        Some(t) => {
            let s = (now() - t).max(0.0);
            if s < 90.0 {
                format!("{:.0}s ago", s)
            } else if s < 5400.0 {
                format!("{:.0}m ago", s / 60.0)
            } else {
                format!("{:.1}h ago", s / 3600.0)
            }
        }
    }
}
/// Jobs whose supervisor heartbeat has gone stale: the clearest "something might be wrong" signal
/// the job store itself can give, independent of telemetry.
fn stale_jobs(store: &crate::jobs::JobStore) -> Vec<Json> {
    store
        .recent_ids(50)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| store.public(&id).ok())
        .filter(|j| j["heartbeat_stale"] == json!(true))
        .collect()
}

/// One row of uncertainty, with a severity so the report can sort and color it, never hide it.
struct Unc {
    severity: &'static str,
    text: String,
}

fn gather_uncertainties(summary: &Json, stale: &[Json]) -> Vec<Unc> {
    let mut u = vec![];
    for s in summary["signals"].as_array().into_iter().flatten() {
        let sev = match s["severity"].as_str() {
            Some("bad") => "high",
            Some("warn") => "medium",
            _ => "low",
        };
        let who = match (s["syndicate"].as_str(), s["agent"].as_str()) {
            (Some(sy), Some(a)) => format!(" ({sy}/{a})"),
            _ => String::new(),
        };
        u.push(Unc {
            severity: sev,
            text: format!("{}{who}", s["text"].as_str().unwrap_or_default()),
        });
    }
    for s in summary["recent_runs"].as_array().into_iter().flatten() {
        let fr = s["attrs"]["harness.self_report.flagged_uncertainty"] == json!(true);
        let risk = s["attrs"]["harness.self_report.max_risk"]
            .as_str()
            .unwrap_or("none");
        if fr || risk == "medium" || risk == "high" {
            let sev = if risk == "high" {
                "high"
            } else if risk == "medium" || fr {
                "medium"
            } else {
                "low"
            };
            u.push(Unc {
                severity: sev,
                text: format!(
                    "{}/{} filed a self-report with {} risk{}.",
                    s["syndicate"].as_str().unwrap_or("?"),
                    s["agent"].as_str().unwrap_or("?"),
                    risk,
                    if fr {
                        " and flagged an open uncertainty"
                    } else {
                        ""
                    }
                ),
            });
        }
    }
    for j in stale {
        u.push(Unc { severity: "high", text: format!("Job {} has a stale supervisor heartbeat — its last known state cannot currently be trusted; inspect before assuming it finished.", j["id"].as_str().unwrap_or("?")) });
    }
    if summary["fleet"]["runs"].as_u64().unwrap_or(0) == 0 {
        u.push(Unc { severity: "low", text: "No runs in this reporting window at all — either nothing was asked of the fleet, or something upstream is not dispatching work.".into() });
    }
    let order = |s: &str| match s {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    };
    u.sort_by_key(|x| order(x.severity));
    u
}

/// Expandable per-run detail: each turn (a "response cycle") and each tool call, in order, with the
/// reward decision and when it was made (the run's close time = its last span's end).
fn drilldown_html(summary: &Json) -> String {
    let traces = summary["traces"].as_object().cloned().unwrap_or_default();
    let mut recent: Vec<&Json> = summary["recent_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    recent.sort_by(|a, b| {
        b["start_unix"]
            .as_f64()
            .partial_cmp(&a["start_unix"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    recent.iter().take(15).map(|run| {
        let trace_id = run["trace_id"].as_str().unwrap_or("");
        let my_span = run["span_id"].as_str().unwrap_or("");
        // Only this run's own children: a trace can hold several runs (a planner plus its delegated
        // children), each with its own turns and tool calls, so filter by parent_id, not trace membership.
        let mut spans: Vec<&Json> = traces.get(trace_id).and_then(Json::as_array).into_iter().flatten()
            .filter(|s| s["parent_id"] == json!(my_span)).collect();
        spans.sort_by(|a, b| a["start_unix"].as_f64().partial_cmp(&b["start_unix"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
        let a = &run["attrs"];
        let end = run["start_unix"].as_f64().unwrap_or(0.0) + run["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
        let reward_kind = a["harness.reward.kind"].as_str().unwrap_or("—");
        let reward_line = if reward_kind == "—" { "No reward step recorded.".to_string() }
            else { format!("<b>{}</b> (confidence {}, factual {}, alignment {}) decided at run close, {}.{}",
                esc(reward_kind), pct(a["harness.reward.confidence"].as_f64()), pct(a["harness.reward.factual"].as_f64()), pct(a["harness.reward.alignment"].as_f64()), ago(Some(end)),
                if a["harness.reward.auto"] == json!(true) { " Auto-granted — both confidence scores cleared the bar." } else { " Held — below the auto-reward bar, or a non-negotiable / overclaim." }) };
        let cycles: String = spans.iter().filter(|s| s["kind"] == "chat").map(|s| {
            let sa = &s["attrs"];
            format!(r#"<tr><td class="num">{}</td><td class="num">{}/{}</td><td class="num">{}</td><td>{}</td></tr>"#,
                sa["harness.turn"].as_u64().unwrap_or(0), sa["gen_ai.usage.input_tokens"].as_u64().unwrap_or(0), sa["gen_ai.usage.output_tokens"].as_u64().unwrap_or(0),
                sa["harness.decode_tps"].as_f64().map(|v| format!("{v:.0} tok/s")).unwrap_or_else(|| "—".into()), esc(sa["harness.finish_reason"].as_str().unwrap_or("—")))
        }).collect();
        let tools: String = spans.iter().filter(|s| s["kind"] == "execute_tool").map(|s| {
            let name = s["name"].as_str().unwrap_or("").strip_prefix("execute_tool ").unwrap_or("").to_string();
            format!(r#"<tr><td class="mono">{}</td><td><span class="pill {}">{}</span></td><td class="num">{:.0}ms</td></tr>"#,
                esc(&name), if s["status"] == "ok" { "ok" } else { "bad" }, if s["status"] == "ok" { "ok" } else { "error" }, s["duration_ms"].as_f64().unwrap_or(0.0))
        }).collect();
        let run_dir = a["harness.run_dir"].as_str().unwrap_or("");
        format!(r##"<details class="run"><summary><span class="mono">{}</span> <span class="pill {}">{}</span> · {} · {}</summary>
            <div class="rundetail">
              <p class="reward-line">{reward_line}</p>
              {}
              {}
              <p class="mute mono" style="margin-top:8px">Prompt and raw record (local files, never embedded here): <span class="mono">{}/prompt.json</span>, <span class="mono">{}/result.json</span></p>
            </div></details>"##,
            esc(run["agent"].as_str().unwrap_or("")), if run["status"] == "ok" { "ok" } else if run["status"] == "error" { "bad" } else { "warn" }, esc(run["status"].as_str().unwrap_or("")),
            ago(run["start_unix"].as_f64()), esc(run["syndicate"].as_str().unwrap_or("")),
            if cycles.is_empty() { String::new() } else { format!(r#"<h3>Response cycles</h3><table><thead><tr><th>Turn</th><th>Tokens in/out</th><th>Decode</th><th>Finish reason</th></tr></thead><tbody>{cycles}</tbody></table>"#) },
            if tools.is_empty() { String::new() } else { format!(r#"<h3>Tool calls</h3><table><thead><tr><th>Tool</th><th>Result</th><th>Duration</th></tr></thead><tbody>{tools}</tbody></table>"#) },
            esc(run_dir), esc(run_dir))
    }).collect()
}

/// A mission's completion claim, kept honest: evidence-backed vs. only the agent's own word.
fn mission_rows(summary: &Json) -> Vec<Json> {
    summary["recent_runs"].as_array().into_iter().flatten().filter(|s| s["kind"] == "invoke_agent").take(25).map(|s| {
        let a = &s["attrs"];
        let declared = a["harness.reward.declared"] == json!(true);
        let auto = a["harness.reward.auto"] == json!(true);
        let overclaim = a["harness.reward.overclaim"] == json!(true);
        let status = s["status"].as_str().unwrap_or("?");
        let basis = if status != "ok" { "failed" } else if auto { "measured" } else if declared { "unverified" } else { "self_reported" };
        json!({"agent": s["agent"], "syndicate": s["syndicate"], "status": status, "basis": basis, "overclaim": overclaim,
               "when": s["start_unix"], "duration_ms": s["duration_ms"], "trace": s["trace_id"]})
    }).collect()
}

fn badge(basis: &str) -> (&'static str, &'static str) {
    match basis {
        "measured" => ("ok", "Measured"),
        "self_reported" => ("warn", "Self-reported"),
        "unverified" => ("warn", "Unverified"),
        _ => ("bad", "Failed"),
    }
}

/// Render one self-contained HTML report for `syndicate` (or the whole fleet when None).
pub fn render(
    root: &Path,
    summary: &Json,
    stale: &[Json],
    syndicate: Option<&str>,
    since: f64,
    until: f64,
) -> String {
    let f = &summary["fleet"];
    let unc = gather_uncertainties(summary, stale);
    let missions = mission_rows(summary);
    let title = syndicate
        .map(|s| format!("Status report · {s}"))
        .unwrap_or_else(|| "Status report · all syndicates".into());
    let window_txt = format!("{} → {}", chrono_like(since), chrono_like(until));
    let agents_rows: String = summary["agents"].as_array().into_iter().flatten().filter(|a| syndicate.map(|s| a["syndicate"] == s).unwrap_or(true)).map(|a| {
        let status = a["status"].as_str().unwrap_or("never_run");
        let (cls, label) = match status { "healthy" => ("ok", "Healthy"), "degraded" => ("warn", "Degraded"), "failing" => ("bad", "Failing"), _ => ("mute", "Never run") };
        format!(r#"<tr><td>{}</td><td class="mono">{}</td><td><span class="pill {cls}">{label}</span></td><td class="num">{}</td><td class="num">{}</td>
            <td class="num">{}</td><td class="num">{}</td><td>{}</td></tr>"#,
            esc(a["syndicate"].as_str().unwrap_or("")), esc(a["agent"].as_str().unwrap_or("")),
            a["runs"].as_u64().unwrap_or(0), pct(a["success_rate"].as_f64()),
            a["latency_ms"]["p50"].as_f64().map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "—".into()),
            a["tool_errors"].as_u64().unwrap_or(0), ago(a["last_run_unix"].as_f64()))
    }).collect();
    let unc_rows: String = if unc.is_empty() {
        r#"<tr><td colspan="2" class="mute">No open uncertainties — every signal the harness can check came back clean.</td></tr>"#.into()
    } else {
        unc.iter()
            .map(|u| {
                let cls = match u.severity {
                    "high" => "bad",
                    "medium" => "warn",
                    _ => "mute",
                };
                format!(
                    r#"<tr><td><span class="pill {cls}">{}</span></td><td>{}</td></tr>"#,
                    u.severity,
                    esc(&u.text)
                )
            })
            .collect()
    };
    let mission_rows_html: String = if missions.is_empty() {
        r#"<tr><td colspan="6" class="mute">No runs in this window.</td></tr>"#.into()
    } else {
        missions.iter().map(|m| { let (cls, label) = badge(m["basis"].as_str().unwrap_or(""));
            let overclaim = if m["overclaim"] == json!(true) { r#" <span class="pill bad">overclaim</span>"#.to_string() } else { String::new() };
            format!(r#"<tr><td class="mono">{}</td><td>{}</td><td><span class="pill {cls}">{label}</span>{overclaim}</td><td class="num">{}</td><td>{}</td><td class="mono mute">{}</td></tr>"#,
                esc(m["agent"].as_str().unwrap_or("")), esc(m["syndicate"].as_str().unwrap_or("")),
                m["duration_ms"].as_f64().map(|v| format!("{:.1}s", v / 1000.0)).unwrap_or_default(), ago(m["when"].as_f64()), esc(m["trace"].as_str().unwrap_or(""))) }).collect()
    };
    let hi = unc.iter().filter(|u| u.severity == "high").count();
    let med = unc.iter().filter(|u| u.severity == "medium").count();
    let drilldown = drilldown_html(summary);
    let edges_rows: String = summary["edges"].as_array().into_iter().flatten().map(|e| format!(
        r#"<tr><td class="mono">{} → {}</td><td class="num">{}</td><td class="num">{}</td></tr>"#,
        esc(e["from"].as_str().unwrap_or("")), esc(e["to"].as_str().unwrap_or("")), e["count"].as_u64().unwrap_or(0),
        e["p50_ms"].as_f64().map(|v| format!("{v:.0}ms")).unwrap_or_else(|| "—".into()))).collect();
    format!(
        r##"<!doctype html><html lang="en" data-theme="system"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="light dark"><title>{title}</title><style>
:root{{color-scheme:light dark;--bg:light-dark(#F7F8FA,#05070B);--card:light-dark(#fff,#0D1117);--t1:light-dark(#0F172A,#E8EEF2);--t2:light-dark(#475569,#9FB0BF);--t3:light-dark(#64748B,#6E7F8E);
--bd:light-dark(rgba(15,23,42,.1),rgba(255,255,255,.09));--ok:light-dark(#059669,#34D399);--okb:light-dark(rgba(5,150,105,.1),rgba(52,211,153,.1));
--warn:light-dark(#B45309,#F59E0B);--warnb:light-dark(rgba(217,119,6,.1),rgba(245,158,11,.1));--bad:light-dark(#DC2626,#EF4444);--badb:light-dark(rgba(220,38,38,.1),rgba(239,68,68,.1));
--br:light-dark(#4F46E5,#A78BFA);--mono:'JetBrains Mono',ui-monospace,Menlo,monospace;--sans:'Inter',-apple-system,system-ui,sans-serif}}
html[data-theme=dark]{{color-scheme:dark}}html[data-theme=light]{{color-scheme:light}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--t1);font-family:var(--sans);font-size:14px;line-height:1.55;padding:18px}}
main{{max-width:1040px;margin:0 auto}}h1{{font-size:1.3rem;margin:0 0 2px}}.sub{{color:var(--t2);font-size:.85rem;margin:0 0 16px}}
.kpis{{display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:10px;margin:14px 0}}
.kpi{{background:var(--card);border:1px solid var(--bd);border-radius:10px;padding:10px 12px}}.kpi b{{display:block;font-size:1.4rem;font-variant-numeric:tabular-nums}}.kpi span{{font-size:.72rem;color:var(--t3)}}
.card{{background:var(--card);border:1px solid var(--bd);border-radius:12px;padding:14px 16px;margin-bottom:14px}}.card h2{{font-size:.95rem;margin:0 0 8px}}
table{{border-collapse:collapse;width:100%;font-size:.85rem}}th,td{{text-align:left;padding:6px 8px;border-bottom:1px solid var(--bd)}}th{{font-size:.68rem;letter-spacing:.05em;text-transform:uppercase;color:var(--t3)}}
.num{{font-variant-numeric:tabular-nums;text-align:right}}.mono{{font-family:var(--mono);font-size:.8rem}}.mute{{color:var(--t3)}}
.pill{{display:inline-block;font-size:.7rem;font-weight:650;padding:1px 7px;border-radius:999px;border:1px solid}}
.pill.ok{{color:var(--ok);background:var(--okb);border-color:var(--ok)}}.pill.warn{{color:var(--warn);background:var(--warnb);border-color:var(--warn)}}
.pill.bad{{color:var(--bad);background:var(--badb);border-color:var(--bad)}}.pill.mute{{color:var(--t3);background:transparent;border-color:var(--bd)}}
.banner{{border-radius:10px;padding:10px 14px;margin-bottom:14px;border:1px solid}}.banner.ok{{background:var(--okb);border-color:var(--ok);color:var(--ok)}}
.banner.warn{{background:var(--warnb);border-color:var(--warn);color:var(--warn)}}footer{{color:var(--t3);font-size:.72rem;margin-top:18px}}
details.run{{border:1px solid var(--bd);border-radius:8px;margin-bottom:8px;padding:6px 10px}}details.run summary{{cursor:pointer;font-size:.85rem}}.rundetail{{margin-top:8px}}.rundetail h3{{font-size:.75rem;letter-spacing:.04em;text-transform:uppercase;color:var(--t3);margin:10px 0 4px}}.reward-line{{font-size:.85rem;margin:4px 0}}@media print{{body{{padding:0}}details.run{{break-inside:avoid}}}}</style></head><body><main>
<h1>{title}</h1><p class="sub">Window {window_txt} · generated {gen}</p>
{banner}
<div class="kpis">
<div class="kpi"><b>{runs}</b><span>runs</span></div>
<div class="kpi"><b>{sr}</b><span>success rate</span></div>
<div class="kpi"><b>{auto}</b><span>auto-rewarded</span></div>
<div class="kpi"><b>{held}</b><span>held for review</span></div>
<div class="kpi"><b>{pen}</b><span>penalties</span></div>
<div class="kpi"><b>{tok}</b><span>tokens (in/out)</span></div>
</div>
<div class="card"><h2>Uncertainties ({hi} high · {med} medium)</h2><table><thead><tr><th>Severity</th><th>What the harness cannot itself confirm</th></tr></thead><tbody>{unc_rows}</tbody></table></div>
<div class="card"><h2>Agents</h2><table><thead><tr><th>Syndicate</th><th>Agent</th><th>Status</th><th class="num">Runs</th><th class="num">Success</th><th class="num">p50</th><th class="num">Tool errs</th><th>Last run</th></tr></thead><tbody>{agents_rows}</tbody></table></div>
<div class="card"><h2>Mission completion claims</h2><p class="sub" style="margin:-4px 0 8px">“Measured” means the reward step verified the result from evidence; “Self-reported” or “Unverified” means only the agent said so.</p>
<table><thead><tr><th>Agent</th><th>Syndicate</th><th>Basis</th><th class="num">Duration</th><th>When</th><th>Trace</th></tr></thead><tbody>{mission_rows_html}</tbody></table></div>
<div class="card"><h2>Recent runs — response cycles, tool calls and reward timing</h2><p class="sub" style="margin:-4px 0 8px">Each run expands into its model turns (response cycles) and tool calls, in order, with when and why its reward was decided. Prompts and raw records stay on disk — only their path is shown.</p>{drilldown}</div>
<div class="card"><h2>Delegation traffic</h2><table><thead><tr><th>Relationship</th><th class="num">Count</th><th class="num">p50</th></tr></thead><tbody>{edges_rows}{edges_empty}</tbody></table></div>
<footer>Generated by local-agents-rs from telemetry spans and reward episodes on this machine only. Nothing here was sent anywhere. Root: {root}</footer>
</main></body></html>"##,
        title = esc(&title),
        window_txt = esc(&window_txt),
        gen = chrono_like(now()),
        banner = if hi > 0 {
            r#"<div class="banner warn">This report has high-severity uncertainties — see the table below before treating any mission as complete.</div>"#.to_string()
        } else {
            r#"<div class="banner ok">No high-severity uncertainties this window.</div>"#
                .to_string()
        },
        runs = f["runs"].as_u64().unwrap_or(0),
        sr = pct(f["success_rate"].as_f64()),
        auto = f["reward"]["auto"].as_u64().unwrap_or(0),
        held = f["reward"]["held"].as_u64().unwrap_or(0),
        pen = f["reward"]["penalty"].as_u64().unwrap_or(0),
        tok = format!(
            "{}/{}",
            f["tokens_in"].as_u64().unwrap_or(0),
            f["tokens_out"].as_u64().unwrap_or(0)
        ),
        hi = hi,
        med = med,
        unc_rows = unc_rows,
        agents_rows = agents_rows,
        mission_rows_html = mission_rows_html,
        drilldown = drilldown,
        edges_rows = edges_rows,
        edges_empty = if edges_rows.is_empty() {
            r#"<tr><td colspan="3" class="mute">No delegations this window.</td></tr>"#
        } else {
            ""
        },
        root = esc(&root.to_string_lossy())
    )
}

/// A crude but dependency-free "YYYY-MM-DD HH:MM UTC" for a unix timestamp (matches telemetry::civil-style math).
fn chrono_like(t: f64) -> String {
    let days = (t / 86_400.0).floor() as i64;
    let secs_of_day = t - (days as f64) * 86_400.0;
    let (h, m) = (
        (secs_of_day / 3600.0) as u64,
        ((secs_of_day % 3600.0) / 60.0) as u64,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mth <= 2);
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02} UTC")
}

/// Generate and save a report for one syndicate, returning its path. `window_secs` = None means all time.
pub fn generate(
    root: &Path,
    syndicate: Option<&str>,
    window_secs: Option<f64>,
) -> Result<PathBuf, String> {
    let until = now();
    let since = window_secs.map(|w| until - w).unwrap_or(0.0);
    let summary = telemetry::summarize(root, root, since, until + 1.0, syndicate);
    // Use this `root`'s own job store (not the process-wide default), so a test root never touches the real job DB.
    let store = crate::jobs::JobStore::open(root.join("shared/runtime/mcp-jobs"))?;
    let stale = stale_jobs(&store);
    let html = render(root, &summary, &stale, syndicate, since, until);
    let d = dir(root);
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let name = syndicate.unwrap_or("fleet");
    let path = d.join(format!("{name}-{}.html", now() as u64));
    fs::write(&path, &html).map_err(|e| e.to_string())?;
    let latest = d.join(format!("{name}-latest.html"));
    fs::write(&latest, &html).map_err(|e| e.to_string())?;
    Ok(latest)
}

/// Generate one report per syndicate plus the fleet-wide one. Returns the paths written.
pub fn generate_all(root: &Path, window_secs: Option<f64>) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![generate(root, None, window_secs)?];
    for s in crate::syndicate::list_dirs(root) {
        out.push(generate(root, Some(&s), window_secs)?);
    }
    Ok(out)
}
