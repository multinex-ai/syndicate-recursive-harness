//! Qwen-assisted authoring for the Forge console: generate a draft syndicate, explain a relation or
//! a free-form question grounded in the real catalog, and a fixed guided tutorial.
//!
//! The model NEVER applies anything by itself. `generate_syndicate` returns a bundle plus the exact
//! same `validate`/`notes` a human edit would get; the operator (or the console, on their click)
//! still has to call `/api/apply`. Explanations are grounded: the real tool catalog, relation guide
//! and roster are put in the prompt, and the model is told to say it does not know rather than invent
//! a tool, host or agent that does not exist. The tutorial is NOT model-generated — fixed, instant,
//! and the same for everyone, so a guide to the console never depends on model quality or load time.
use crate::runtime::{ModelServer, ServerConfig};
use serde_json::{json, Value as Json};
use std::path::Path;
use std::time::Duration;

const MAX_INPUT: usize = 4000;

fn complete(
    server: &ModelServer,
    system: &str,
    user: &str,
    json_mode: bool,
    max_tokens: u64,
) -> Result<String, String> {
    let mut body = json!({"model": "local-agent", "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
                          "temperature": 0.2, "max_tokens": max_tokens});
    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
        body["temperature"] = json!(0.0);
    }
    let reply = server.request("/v1/chat/completions", Some(&body), Duration::from_secs(90))?;
    reply
        .pointer("/choices/0/message/content")
        .and_then(Json::as_str)
        .map(String::from)
        .ok_or_else(|| "model returned no content".into())
}

/// Conservative defaults the generator falls back to when the model omits a ceiling; `validate`
/// still catches anything unsafe, this just avoids an empty-ceiling bundle rejecting every tool.
fn default_ceilings(bundle: &mut Json) {
    let syn = &mut bundle["syndicate"];
    if !syn["allowed_tools_ceiling"].is_array()
        || syn["allowed_tools_ceiling"].as_array().unwrap().is_empty()
    {
        syn["allowed_tools_ceiling"] =
            json!(["read_file", "write_file", "list_files", "send_message"]);
    }
    if !syn["allowed_hosts_ceiling"].is_array() {
        syn["allowed_hosts_ceiling"] = json!(["localhost", "127.0.0.1"]);
    }
    if !syn["edges"].is_array() {
        syn["edges"] = json!([]);
    }
    if !syn["loops"].is_array() {
        syn["loops"] = json!([]);
    }
    if syn["max_dispatch_rounds"]
        .as_u64()
        .map(|n| n == 0 || n > 20)
        .unwrap_or(true)
    {
        syn["max_dispatch_rounds"] = json!(6);
    }
    if syn["officiation"].is_null() {
        syn["officiation"] = json!({"enabled": true});
    }
}

fn extract_json(text: &str) -> Option<Json> {
    if let Ok(v) = serde_json::from_str::<Json>(text.trim()) {
        return Some(v);
    }
    // model sometimes wraps the object in prose or a fenced code block; take the outermost {...}
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(&text[start..=end]).ok()
}

/// Draft a syndicate from a short description. Never applies; returns the bundle plus its
/// validation, exactly as the manual editor would show, so a human reviews before anything is written.
pub fn generate_syndicate(
    root: &Path,
    cfg: &ServerConfig,
    run_dir: &Path,
    description: &str,
) -> Result<Json, String> {
    if description.trim().is_empty() || description.len() > MAX_INPUT {
        return Err(format!("description must be 1-{MAX_INPUT} characters"));
    }
    let tools = crate::syndicate::tool_catalog();
    let relations = crate::syndicate::guide();
    let system = format!(
"You design a bounded team of local AI agents (a \"syndicate\") from the operator's description. Output ONLY a single JSON object, no prose, no markdown fence, matching exactly this shape:
{{\"dir\": \"lowercase-with-dashes\", \"syndicate\": {{\"syndicate_id\": \"syndicate-<dir>-v1\", \"display_name\": \"...\", \"mission\": \"...\", \"purpose\": \"...\", \"lore\": \"...\",
 \"coordinator_agent_id\": \"<one member's agent_id>\", \"max_dispatch_rounds\": 1-20, \"officiation\": {{\"enabled\": true}},
 \"allowed_tools_ceiling\": [...], \"allowed_hosts_ceiling\": [...],
 \"edges\": [{{\"from\": \"<agent_id>\", \"to\": \"<agent_id>\", \"relation\": \"<one of the relation ids below>\", \"condition\": \"short phrase\", \"order\": 1}}],
 \"loops\": [{{\"name\": \"...\", \"path\": [\"<agent_id>\", \"<agent_id>\", \"<agent_id>\"], \"max_iterations\": 1-10, \"exit_when\": \"a condition that can actually become true\"}}]}},
 \"members\": [{{\"agent_id\": \"lowercase-with-dashes\", \"role_title\": \"...\", \"allowed_tools\": [...from the catalog only...], \"allowed_hosts\": [\"localhost\"],
 \"max_turns\": 8-20, \"timeout_seconds\": 180-900,
 \"persona\": {{\"name\": \"a believable full name, never a real public figure\", \"title\": \"...\", \"backstory\": \"2-4 sentences, set inside the machine, no fourth-wall break\",
 \"voice\": \"how it speaks\", \"creed\": [\"1-3 short rules it holds to\"], \"never\": [\"1-3 hard limits\"]}}}}]}}
Rules: 3 to 6 members. Every member must have at least one edge touching it (something it can message, or something that can message it), except the coordinator may be purely a hub.
State every persona as FACT — never write \"pretend\", \"roleplay\", \"act as\", \"in character\" or similar; the agent believes it is who it is, the same way a job title is a fact.
Use ONLY these tool names in allowed_tools and the ceiling: {}.
Use ONLY these relation ids: {}.
Do not invent a tool, host or relation outside these lists.",
        tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect::<Vec<_>>().join(", "),
        relations.as_array().unwrap().iter().filter_map(|r| r["id"].as_str()).collect::<Vec<_>>().join(", "));
    let server = ModelServer::start(cfg, run_dir)?;
    let text = complete(&server, &system, description, true, 1800)?;
    let mut bundle = extract_json(&text).ok_or_else(|| {
        format!(
            "the model's reply was not parseable JSON (first 200 chars): {}",
            text.chars().take(200).collect::<String>()
        )
    })?;
    if !bundle.is_object() || !bundle.get("members").map(Json::is_array).unwrap_or(false) {
        return Err(
            "the model's JSON did not match the expected bundle shape (missing members array)"
                .into(),
        );
    }
    default_ceilings(&mut bundle);
    let errors = crate::syndicate::validate(root, &bundle);
    let notes = crate::syndicate::notes(root, &bundle);
    Ok(
        json!({"bundle": bundle, "errors": errors, "notes": notes, "valid": errors.is_empty(), "generated_by": "qwen-assist", "note": "Draft only. Nothing is applied until you review and click Apply."}),
    )
}

/// Explain a relation id (deterministic, instant, no model call) or a free-form question grounded
/// in the real roster/catalog (one model call; told to say it does not know rather than invent).
pub fn explain(
    root: &Path,
    cfg: &ServerConfig,
    run_dir: &Path,
    topic: &str,
) -> Result<Json, String> {
    let g = crate::syndicate::guide();
    if let Some(row) = g.as_array().unwrap().iter().find(|r| {
        r["id"] == topic
            || r["label"]
                .as_str()
                .map(|l| l.eq_ignore_ascii_case(topic))
                .unwrap_or(false)
    }) {
        return Ok(json!({"mode": "relation", "answer": row, "generated_by": "guide"}));
    }
    if topic.trim().is_empty() || topic.len() > MAX_INPUT {
        return Err(format!("question must be 1-{MAX_INPUT} characters"));
    }
    let roster = crate::telemetry::roster(root);
    let system = format!("You explain how this local agent harness actually works, to its operator. Ground every claim in the facts given below; \
if the question is about something not in these facts, say plainly that you don't know rather than invent a tool, command, agent or behaviour. \
Answer in 2-6 short sentences, plain language, no markdown headings.\n\nRelation types: {}\nTool catalog: {}\nCurrent roster: {}",
        serde_json::to_string(&g).unwrap_or_default(), serde_json::to_string(&crate::syndicate::tool_catalog()).unwrap_or_default(),
        serde_json::to_string(&roster).unwrap_or_default().chars().take(3000).collect::<String>());
    let server = ModelServer::start(cfg, run_dir)?;
    let answer = complete(&server, &system, topic, false, 400)?;
    Ok(json!({"mode": "free", "answer": answer.trim(), "generated_by": "qwen-assist"}))
}

/// Draft one persona (backstory, voice, creed, limits) for an existing member. Never applied: the console
/// puts it in the editor for the operator to read and change. Stated as fact, no framing words.
pub fn persona(cfg: &ServerConfig, run_dir: &Path, input: &Json) -> Result<Json, String> {
    let g = |k: &str| {
        input[k]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(600)
            .collect::<String>()
    };
    let system = "You write the identity of one member of a team of local AI agents. Output ONLY one JSON object: \
{\"name\": \"a believable full name, never a real public figure\", \"title\": \"...\", \"backstory\": \"2-4 sentences, set inside the machine, no fourth-wall break\", \
\"voice\": \"how it speaks\", \"creed\": [\"1-3 short rules\"], \"never\": [\"1-3 hard limits\"]}. \
State everything as FACT. Never write pretend, roleplay, act as, in character or similar.";
    let user = format!(
        "Team: {}. Mission: {}. This member's role: {}. Direction from the operator: {}.",
        g("display_name"),
        g("mission"),
        g("role_title"),
        g("direction")
    );
    let server = ModelServer::start(cfg, run_dir)?;
    let text = complete(&server, system, &user, true, 700)?;
    let p = extract_json(&text).ok_or("the model's reply was not parseable JSON")?;
    let strs = |k: &str| {
        p[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .take(4)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
    if s("name").is_empty() || s("backstory").is_empty() {
        return Err("the model's persona was missing a name or backstory".into());
    }
    let persona = json!({"name": s("name"), "title": s("title"), "backstory": s("backstory"), "voice": s("voice"), "creed": strs("creed"), "never": strs("never")});
    Ok(
        json!({"persona": persona, "generated_by": "qwen-assist", "note": "Draft only. Edit freely, then Apply."}),
    )
}

/// The fixed, instant guided tutorial. Not model-generated on purpose (see module docs).
pub fn tutorial() -> Json {
    let steps = [
        ("Welcome", "This is the Syndicate Forge: where you build and run teams of local agents. Nothing here leaves this machine unless you explicitly send it.", "overview"),
        ("The syndicate", "A syndicate is one team: a mission, a purpose, and ceilings that cap what any of its members may ever be given.", "syndicate-panel"),
        ("Members & personas", "Each member has a persona — a believable identity stated as fact, not a performance. It shapes how that agent behaves.", "members-panel"),
        ("Relationships", "Draw an arrow from one member to another to let it message that member. The relation type (commands, feeds, verifies...) is written into both agents' identity.", "graph"),
        ("Loops & caps", "A loop is a cycle with a hard iteration cap and an exit condition, so back-and-forth work always ends.", "graph-loops"),
        ("Tools, hosts, turns", "Open a member to grant tools and hosts (never above the syndicate's own ceiling) and set its turn and timeout budget.", "member-drawer"),
        ("Validate", "Validate checks everything — framing words, ceilings, loop bounds, unreachable members — before anything is written.", "validate-panel"),
        ("Apply", "Apply writes the syndicate to disk. A dry run shows exactly what would change; nothing is touched until you confirm.", "apply-button"),
        ("Run a mission", "Pick a role, write the objective, optionally declare acceptance criteria, and run it — right here, on this machine.", "mission-panel"),
        ("Reports & telemetry", "Every run is measured: response cycles, tool calls, and when and why its reward was decided. Nothing is taken on faith.", "dashboard"),
    ];
    json!(steps.iter().enumerate().map(|(i, (title, body, anchor))| json!({"step": i + 1, "of": steps.len(), "title": title, "body": body, "anchor": anchor})).collect::<Vec<_>>())
}
