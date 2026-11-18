//! The reward step: the close of the agentic loop.
//!
//!   Perceive -> Plan -> Execute -> Verify -> REWARD -> (learn)
//!
//! Every run and every mission ends here. The reward step reads what the mission intended
//! (declared acceptance criteria, else provisional ones inferred from the objective), scores
//! the outcome against that intent FROM EVIDENCE ON DISK — not from the agent's own claim of
//! success — and emits a durable episode: a reward for verified success, a penalty-with-lesson
//! for failure. Episodes go to a local ledger always, and to MemQ through the real identity-checked
//! path (never faked).
//!
//! Anti-reward-hacking is the whole point of the design:
//!   * Criteria are declared up front and frozen; without them the reward is provisional and capped.
//!   * A criterion counts as met only when its evidence is verified; "the model said so" is `unverified`.
//!   * The agent never grants its own reward; a self-claim that evidence does not back is flagged.
//!   * A reward only influences future behaviour once a separate grader (the Assayer) and its trust
//!     gate (the Meta-Assayer) have passed it — this module computes the signal, it does not apply it.
use crate::jobs::now;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Terminal statuses that are failures regardless of criteria.
const FAILURE_STATES: [&str; 5] = [
    "error",
    "cancelled",
    "child_failed",
    "timed_out",
    "model_error",
];
/// Provisional (undeclared-criteria) reward is capped here, so declaring intent up front is rewarded.
const PROVISIONAL_CAP: f64 = 0.5;
const TAU: f64 = 0.15;

fn s<'a>(v: &'a Json, k: &str) -> &'a str {
    v.get(k).and_then(Json::as_str).unwrap_or("")
}

/// One acceptance criterion. `check` is how it is verified from evidence:
///   file_exists:<path>   a non-empty file (relative to run_dir or workspace, or absolute under them)
///   tool_ok:<name>       at least one successful call of that tool in the run record
///   status_finished      the run reached model_finished
///   manual[:<note>]      cannot be auto-verified -> counts as `unverified`, needs the Assayer
/// A criterion marked `non_negotiable` is a hard rule (scope, safety, a must-have output): if it is
/// not verified-met, the whole mission fails the auto-reward gate no matter how the rest scored.
#[derive(Clone)]
pub struct Criterion {
    pub id: String,
    pub text: String,
    pub check: String,
    pub weight: f64,
    pub non_negotiable: bool,
}

fn parse_criteria(v: &Json) -> Vec<Criterion> {
    v.as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, c)| Criterion {
            id: c
                .get("id")
                .and_then(Json::as_str)
                .unwrap_or(&format!("c{i}"))
                .to_string(),
            text: s(c, "text").to_string(),
            check: s(c, "check").to_string(),
            weight: c
                .get("weight")
                .and_then(Json::as_f64)
                .unwrap_or(1.0)
                .clamp(0.0, 10.0),
            non_negotiable: c
                .get("non_negotiable")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

/// Reward policy: the auto-reward confidence bar and whether MemQ writes are turned on yet.
/// Loaded from shared/config/reward.json; defaults keep MemQ OFF until the account is confirmed.
pub struct Policy {
    pub auto_threshold: f64,
    pub memq_write_enabled: bool,
}
pub fn policy(root: &Path) -> Policy {
    let v: Json = std::fs::read_to_string(root.join("shared/config/reward.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Json::Null);
    Policy {
        auto_threshold: v
            .get("auto_reward_threshold")
            .and_then(Json::as_f64)
            .unwrap_or(0.95)
            .clamp(0.5, 1.0),
        memq_write_enabled: v
            .get("memq_write_enabled")
            .and_then(Json::as_bool)
            .unwrap_or(false),
    }
}

/// Conservative criteria inferred from an objective when none were declared. Flagged provisional.
fn provisional(objective: &str) -> Vec<Criterion> {
    let mut c = vec![Criterion {
        id: "completed".into(),
        text: "The run completed without failing".into(),
        check: "status_finished".into(),
        weight: 1.0,
        non_negotiable: false,
    }];
    let o = objective.to_lowercase();
    if [
        "write", "create", "save", "generate", "produce", "build", "render",
    ]
    .iter()
    .any(|k| o.contains(k))
    {
        c.push(Criterion {
            id: "artifact".into(),
            text: "The requested artifact was produced and verified".into(),
            check: "manual".into(),
            weight: 2.0,
            non_negotiable: false,
        });
    }
    c
}

fn file_ok(run_dir: &Path, workspace: Option<&Path>, rel: &str) -> bool {
    let p = Path::new(rel);
    let cands: Vec<PathBuf> = if p.is_absolute() {
        vec![p.to_path_buf()]
    } else {
        std::iter::once(run_dir.join(rel))
            .chain(workspace.map(|w| w.join(rel)))
            .collect()
    };
    cands
        .iter()
        .any(|c| c.is_file() && c.metadata().map(|m| m.len() > 0).unwrap_or(false))
}

fn tool_succeeded(result: &Json, name: &str) -> bool {
    result
        .get("tools")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .any(|e| {
            e.get("tool").and_then(Json::as_str) == Some(name)
                && e.pointer("/response/ok") == Some(&json!(true))
        })
}

/// Verify one criterion against evidence. Returns (verdict, evidence) where verdict is
/// "met" | "unmet" | "unverified". `unverified` NEVER counts as success.
fn verify(
    c: &Criterion,
    run_dir: &Path,
    workspace: Option<&Path>,
    result: &Json,
) -> (&'static str, String) {
    let (kind, arg) = c.check.split_once(':').unwrap_or((c.check.as_str(), ""));
    match kind {
        "file_exists" => {
            if file_ok(run_dir, workspace, arg) {
                ("met", format!("file present: {arg}"))
            } else {
                ("unmet", format!("file missing or empty: {arg}"))
            }
        }
        "tool_ok" => {
            if tool_succeeded(result, arg) {
                ("met", format!("tool succeeded: {arg}"))
            } else {
                ("unmet", format!("no successful {arg} call"))
            }
        }
        "status_finished" => {
            if s(result, "status") == "model_finished" {
                ("met", "run reached model_finished".into())
            } else {
                ("unmet", format!("terminal status: {}", s(result, "status")))
            }
        }
        _ => (
            "unverified",
            "needs a grader (the Assayer); not auto-verifiable".into(),
        ),
    }
}

/// Did the run's own answer claim success? Used only to FLAG a claim evidence does not back.
fn self_claims_success(result: &Json) -> bool {
    let a = s(result, "answer").to_lowercase();
    [
        "done", "complete", "success", "finished", "resolved", "working",
    ]
    .iter()
    .any(|k| a.contains(k))
}

/// Compute the reward episode for a finished run or mission. Pure: it reads evidence, writes nothing.
pub fn assess(
    run_dir: &Path,
    workspace: Option<&Path>,
    result: &Json,
    mission: &str,
    declared: &Json,
    auto_threshold: f64,
) -> Json {
    let provisional_flag =
        !declared.is_array() || declared.as_array().map(|a| a.is_empty()).unwrap_or(true);
    let criteria = if provisional_flag {
        provisional(mission)
    } else {
        parse_criteria(declared)
    };
    let status = s(result, "status");
    let failed_terminal = FAILURE_STATES.contains(&status);

    let mut verdicts = vec![];
    let (mut wmet, mut wunmet, mut wtot, mut wunver) = (0.0, 0.0, 0.0, 0.0);
    let (mut nn_total, mut nn_met) = (0usize, 0usize); // non-negotiables
    for c in &criteria {
        let (v, ev) = verify(c, run_dir, workspace, result);
        wtot += c.weight;
        match v {
            "met" => wmet += c.weight,
            "unmet" => wunmet += c.weight,
            _ => wunver += c.weight,
        }
        if c.non_negotiable {
            nn_total += 1;
            if v == "met" {
                nn_met += 1;
            }
        }
        verdicts.push(
            json!({"id": c.id, "text": c.text, "check": c.check, "verdict": v, "evidence": ev,
                             "weight": c.weight, "non_negotiable": c.non_negotiable}),
        );
    }
    let non_negotiables_met = nn_met == nn_total; // vacuously true if none declared

    // Evidence-grounded value in [-1, 1]: met raises, unmet lowers, unverified is neutral (never counts as success).
    let mut value = if wtot > 0.0 {
        (wmet - wunmet) / wtot
    } else {
        0.0
    };
    if failed_terminal {
        value = value.min(-0.5);
    }
    if provisional_flag {
        value = value.clamp(-1.0, PROVISIONAL_CAP);
    }
    if nn_total > 0 && !non_negotiables_met {
        value = value.min(-0.5);
    } // a broken non-negotiable is a failure

    let self_claim = self_claims_success(result);
    let evidence_backs = wmet > 0.0 && wunmet == 0.0 && !failed_terminal;
    let overclaim = self_claim && !evidence_backs;

    // --- the two confidence scores the auto-reward gate is built on ---
    // Factual: of everything the run implicitly claims, how much is verified from evidence (0..1).
    // Unverified and unmet both lower it; an overclaim (asserting what the files contradict) caps it hard.
    let mut factual = if wtot > 0.0 { wmet / wtot } else { 0.0 };
    if overclaim {
        factual = factual.min(0.4);
    }
    if wunmet > 0.0 {
        factual = factual.min(0.6);
    }
    // Alignment: did the outcome meet the ACTUAL goal and its non-negotiables?
    // If intent was never declared we cannot be confident we understood the goal -> capped.
    // Any broken non-negotiable is a hard zero.
    let mut alignment = if wtot > 0.0 { wmet / wtot } else { 0.0 };
    if provisional_flag {
        alignment = alignment.min(0.5);
    }
    if nn_total > 0 && !non_negotiables_met {
        alignment = 0.0;
    }
    if failed_terminal {
        alignment = alignment.min(0.3);
    }
    let confidence = factual.min(alignment); // the gate needs BOTH above the bar

    // Auto-reward only when both scores clear the bar, intent was declared, non-negotiables hold,
    // nothing is overclaimed, and the run did not fail. Otherwise it is held for a grader/human.
    let auto_reward = !provisional_flag
        && !failed_terminal
        && !overclaim
        && non_negotiables_met
        && factual >= auto_threshold
        && alignment >= auto_threshold;

    let kind = if failed_terminal || (nn_total > 0 && !non_negotiables_met) || value <= -TAU {
        "penalty"
    } else if auto_reward {
        "reward"
    }
    // autonomously granted
    else if value >= TAU {
        "held_for_review"
    }
    // earned, but below the bar -> needs the Assayer/human
    else {
        "provisional"
    };
    let requires_review = kind == "held_for_review";

    let lesson = if kind == "penalty" {
        let worst = verdicts
            .iter()
            .find(|v| v["verdict"] == "unmet" && v["non_negotiable"] == json!(true))
            .or_else(|| verdicts.iter().find(|v| v["verdict"] == "unmet"))
            .map(|v| s(v, "text"))
            .unwrap_or("the objective");
        Some(format!("Avoid repeating this failure: \"{worst}\" was not met (status {status}). Check evidence before claiming progress."))
    } else {
        None
    };

    json!({
        "episode": "reward_v2",
        "kind": kind,                               // reward (auto) | held_for_review | penalty | provisional
        "auto_reward": auto_reward,                 // granted without a human/grader?
        "requires_review": requires_review,
        "value": (value * 1000.0).round() / 1000.0,
        "confidence": (confidence * 1000.0).round() / 1000.0,
        "factual_confidence": (factual * 1000.0).round() / 1000.0,       // claims backed by evidence
        "alignment_confidence": (alignment * 1000.0).round() / 1000.0,   // met the real goal + non-negotiables
        "auto_reward_threshold": auto_threshold,
        "mission": mission,
        "run_status": status,
        "criteria_declared": !provisional_flag,
        "non_negotiables": {"total": nn_total, "met": nn_met, "all_met": non_negotiables_met},
        "scored": {"weight_total": wtot, "met": wmet, "unmet": wunmet, "unverified": wunver},
        "verdicts": verdicts,
        "self_claimed_success": self_claim,
        "evidence_backs_claim": evidence_backs,
        "overclaim_flagged": overclaim,
        "lesson": lesson,
        "grader": "rule_based_v2",
        "adjudication": if auto_reward { "auto-granted at >= threshold on factual and alignment confidence" }
                        else { "held: a separate grader (Assayer) and Meta-Assayer trust are required" },
        "created_unix": now(),
    })
}

/// Write the episode to the local reward ledger and append the index. Returns the file path.
pub fn record_local(root: &Path, run_id: &str, episode: &Json) -> Result<PathBuf, String> {
    let dir = root.join("shared/runtime/reward-ledger");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{}-{}.json", now() as u64, run_id));
    std::fs::write(
        &file,
        serde_json::to_string_pretty(episode).unwrap_or_default() + "\n",
    )
    .map_err(|e| e.to_string())?;
    let line = json!({"t": episode["created_unix"], "run": run_id, "kind": episode["kind"], "value": episode["value"], "status": episode["run_status"]});
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("index.jsonl"))
    {
        let _ = writeln!(f, "{line}");
    }
    Ok(file)
}

/// A bounded, secret-free MemQ payload (shaped for journal_record / add_memory). No answer text,
/// no tool arguments, no file contents — only the graded summary and provenance.
pub fn memq_payload(episode: &Json, run_id: &str) -> Json {
    let mission: String = s(episode, "mission").chars().take(90).collect();
    // add_memory is the reviewed write tool. content is a short human line; metadata holds the structure.
    let content = format!("{} ({}): mission \"{}\" — reward {}, confidence {} (factual {}, alignment {}), status {}.{}",
        s(episode, "kind"),
        if episode["auto_reward"] == json!(true) { "auto-granted" } else { "held" },
        mission, episode["value"], episode["confidence"], episode["factual_confidence"],
        episode["alignment_confidence"], s(episode, "run_status"),
        episode["lesson"].as_str().map(|l| format!(" Lesson: {l}")).unwrap_or_default());
    // add_memory server schema: required `text`; optional memory_type (reward episodes are episodic),
    // tags, task_id, agent_id, metadata. (Earlier `content` was rejected with a ValidationError.)
    let tag = if episode["auto_reward"] == json!(true) {
        "auto_reward"
    } else if s(episode, "kind") == "penalty" {
        "penalty"
    } else {
        "held_for_review"
    };
    json!({
        "name": "add_memory",
        "arguments": {
            "text": content,
            "memory_type": "episodic",
            "agent_id": "local-agents-rs/reward",
            "task_id": run_id,
            "tags": ["reward_episode", s(episode, "kind"), tag],
            "metadata": {
                "type": "reward_episode", "kind": episode["kind"], "auto_reward": episode["auto_reward"],
                "reward_value": episode["value"], "confidence": episode["confidence"],
                "factual_confidence": episode["factual_confidence"], "alignment_confidence": episode["alignment_confidence"],
                "non_negotiables": episode["non_negotiables"], "run_status": episode["run_status"],
                "criteria": episode["scored"], "overclaim_flagged": episode["overclaim_flagged"],
                "run_id": run_id, "grader": episode["grader"], "created_unix": episode["created_unix"],
                "source": "local-agents-rs reward_v2", "disclosure_class": "project",
                "note": "Advisory learning signal; influences future behaviour only after Assayer confirmation and Meta-Assayer trust."
            }
        }
    })
}

/// Attempt the MemQ write through the real identity-checked client. Never fabricates success:
/// returns {written:false, reason} when identity is not verified or the client is unavailable.
pub fn to_memq(root: &Path, python: &Path, workspace: &Path, task: &str, payload: &Json) -> Json {
    let script = root.join("shared/harness/memq_client.py");
    if !script.is_file() || !python.is_file() {
        return json!({"written": false, "reason": "MemQ client not available; kept in the local ledger only"});
    }
    let mut cmd = Command::new(python);
    cmd.arg(&script)
        .args(["call", "--name", s(payload, "name"), "--project"])
        .arg(workspace)
        .args([
            "--task",
            task,
            "--arguments",
            &payload["arguments"].to_string(),
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match cmd.output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            let reply: Option<Json> = out.lines().rev().find_map(|l| serde_json::from_str(l).ok());
            match reply {
                Some(r) if r["ok"] == json!(true) => {
                    json!({"written": true, "result": r["result"]})
                }
                Some(r) => {
                    json!({"written": false, "reason": r["error"].as_str().unwrap_or("MemQ refused the write")})
                }
                None => {
                    json!({"written": false, "reason": "MemQ client gave no reply; kept in the local ledger"})
                }
            }
        }
        Err(e) => {
            json!({"written": false, "reason": format!("MemQ client could not run: {e}; kept in the local ledger")})
        }
    }
}

/// Non-fatal close: score against intent, write the local ledger always, attach the episode. When the
/// policy says the account is confirmed (`memq_write_enabled`) and `memq` gives (python, task), also
/// writes to MemQ through the verified client; otherwise the episode stays in the local ledger and says so.
/// Never panics, never fails the run.
pub fn close_run(
    root: &Path,
    run_dir: &Path,
    workspace: Option<&Path>,
    record: &mut Json,
    mission: &str,
    declared: &Json,
    memq: Option<(&Path, &str)>,
) {
    let run_id = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("run")
        .to_string();
    let pol = policy(root);
    let mut episode = assess(
        run_dir,
        workspace,
        record,
        mission,
        declared,
        pol.auto_threshold,
    );
    let _ = record_local(root, &run_id, &episode);
    match (pol.memq_write_enabled, memq, workspace) {
        (true, Some((python, task)), Some(ws)) => {
            episode["memq"] = to_memq(root, python, ws, task, &memq_payload(&episode, &run_id));
        }
        (false, _, _) => {
            episode["memq"] = json!({"written": false, "reason": "memq_write_enabled is off in shared/config/reward.json (account not confirmed yet)"});
        }
        _ => {
            episode["memq"] = json!({"written": false, "reason": "no verified MemQ identity available at this close point"});
        }
    }
    record["reward"] = episode;
}
