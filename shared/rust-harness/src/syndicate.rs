//! Syndicates: identity (officiation), policy (what a member may do) and configuration (validate / apply).
//!
//!   syndicate.json           mission, purpose, lore, officiation, ceilings, edges (relationships), loops
//!   agents/<id>/config.json  persona, role_title, allowed_tools, allowed_hosts, max_turns, timeout_seconds
//!
//! EVERYTHING HERE IS HONORED AT RUNTIME (that is the rule for the Forge UI: no decorative settings):
//!   * a relationship A -> B is the permission for A to message B (send_message is restricted to A's neighbours)
//!   * `constraints.max_rounds`, a loop's `max_iterations` and `max_dispatch_rounds` cap delegations (dispatch.rs)
//!   * `allowed_tools` is an allowlist for the member's tools; `allowed_hosts` narrows the browser hosts it may be given
//!   * `max_turns` / `timeout_seconds` bound its runs
//!   * relationship text, condition, order and caps are written into the member's identity block, so the agent is told
//!   * a removed member is archived, so it can no longer be run by name
//!
//! Identity text is stated as fact. It must not contain framing words such as "roleplay" or "pretend" (see `lint`).
use crate::jobs::now;
use serde_json::{json, Map, Value as Json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const RELATIONS: [&str; 7] = [
    "commands",
    "feeds",
    "verifies",
    "challenges",
    "escalates_to",
    "hands_off",
    "reports_to",
];
pub const BASE_ROLES: [&str; 6] = [
    "coordinator",
    "cyber-lab",
    "browser",
    "desktop",
    "image-studio",
    "video-studio",
];
const MAX_LOOP_ITERATIONS: u64 = 10;
/// Words that would frame the identity as an act. Checked case-insensitively on identity text.
pub const FRAMING: [&str; 14] = [
    "roleplay",
    "role-play",
    "role play",
    "pretend",
    "fictional",
    "fiction",
    "in character",
    "in-character",
    "make-believe",
    "make believe",
    "act as",
    "simulate being",
    "imagine you are",
    "persona",
];

fn s<'a>(v: &'a Json, k: &str) -> &'a str {
    v.get(k).and_then(Json::as_str).unwrap_or("")
}
fn strs(v: &Json, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(String::from))
        .collect()
}
fn read_obj(p: &Path) -> Map<String, Json> {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
fn read_json(p: &Path) -> Json {
    fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Json::Null)
}
fn write_pretty(p: &Path, v: &Json) -> Result<(), String> {
    fs::write(
        p,
        serde_json::to_string_pretty(v).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("{}: {e}", p.display()))
}

/// Framing words found in `text` (lowercased matches, deduplicated).
pub fn lint(text: &str) -> Vec<String> {
    let t = text.to_lowercase();
    FRAMING
        .iter()
        .filter(|w| t.contains(*w))
        .map(|w| w.to_string())
        .collect()
}

// ---------------------------------------------------------------- the guide (single source of truth for UI + AI prompts)
/// What each relationship means, what it enforces, and what the agent is told. Served to the console and the AI helpers.
/// The "told" lines are produced by `relation_line`, the same function that writes the agent's prompt, so they cannot drift.
pub fn guide() -> Json {
    // (id, label, short, meaning, use_when, example)
    let rows = [
        (
            "commands",
            "Commands",
            "A directs B",
            "A gives B its work and B takes direction from A. The classic manager-to-worker link.",
            "A decides what happens and B carries it out.",
            "The Watch Commander commands the Sentinel to review today's advisories.",
        ),
        (
            "feeds",
            "Feeds",
            "A's findings go to B",
            "A produces information B needs. Neither directs the other.",
            "One agent's output is another agent's input (intel to analysis).",
            "The Sentinel feeds the Cartographer new advisories so exposure can be matched.",
        ),
        (
            "verifies",
            "Verifies",
            "A checks B's work",
            "B's work is not finished until A has checked it.",
            "You want an independent check before something counts as done.",
            "The Auditor verifies the Cartographer's map by re-checking each exposure.",
        ),
        (
            "challenges",
            "Challenges",
            "A argues against B's plans",
            "A looks for the weak point in B's plan before B acts (a devil's advocate).",
            "A plan is risky or costly to get wrong.",
            "The Contrarian challenges the Regent's plan with its three strongest objections.",
        ),
        (
            "escalates_to",
            "Escalates to",
            "A hands B what is beyond A",
            "When A is out of its depth, blocked, or facing something irreversible, it goes to B.",
            "You need a safe way up the chain.",
            "The Auditor escalates any out-of-scope finding to the Commander.",
        ),
        (
            "hands_off",
            "Hands off to",
            "A passes finished work to B",
            "A completes its stage, then B continues. Builds an ordered pipeline.",
            "The work moves through stages in a fixed order.",
            "The Cartographer hands the prioritised exposure list off to the Auditor.",
        ),
        (
            "reports_to",
            "Reports to",
            "A reports to B",
            "A tells B what happened. The upward counterpart of 'commands'.",
            "A manager needs status or results from a worker.",
            "The Tender reports to the Keeper that the lamp is lit.",
        ),
    ];
    Json::Array(rows.iter().map(|(id, label, short, meaning, use_when, example)| json!({
        "id": id, "label": label, "short": short, "meaning": meaning, "use_when": use_when, "example": example,
        "enforces": "A may message B. B's result comes back to A as a receipt.",
        "told_to_a": relation_line(id, "B", false), "told_to_b": relation_line(id, "A", true)})).collect())
}

// ---------------------------------------------------------------- tools
/// (name, category, note). Categories: core (always on), workspace, exec, browser, desktop.
const TOOLS: [(&str, &str); 24] = [
    ("send_message", "core"),
    ("list_workers", "core"),
    ("receive_messages", "core"),
    ("list_skills", "core"),
    ("read_skill", "core"),
    ("memq_status", "core"),
    ("memq_tools", "core"),
    ("memq_call", "core"),
    ("read_file", "workspace"),
    ("write_file", "workspace"),
    ("list_files", "workspace"),
    ("audit_ot_exposure", "workspace"),
    ("nexus_topology", "workspace"),
    ("run_command", "exec"),
    ("browser_open", "browser"),
    ("browser_observe", "browser"),
    ("browser_click", "browser"),
    ("browser_fill", "browser"),
    ("browser_screenshot", "browser"),
    ("desktop_observe", "desktop"),
    ("desktop_screenshot", "desktop"),
    ("desktop_click", "desktop"),
    ("desktop_type", "desktop"),
    ("desktop_key", "desktop"),
];

/// `list_dir` is the older spelling of `list_files`.
pub fn canonical_tool(name: &str) -> &str {
    if name == "list_dir" {
        "list_files"
    } else {
        name
    }
}
pub fn tool_category(name: &str) -> &'static str {
    let n = canonical_tool(name);
    if n == "desktop_focus" {
        return "desktop";
    }
    TOOLS
        .iter()
        .find(|(t, _)| *t == n)
        .map(|(_, c)| *c)
        .unwrap_or("unknown")
}

/// The tool catalog the console shows, with what each category needs in order to run.
pub fn tool_catalog() -> Json {
    let mut v: Vec<Json> = TOOLS.iter().map(|(t, c)| json!({"name": t, "category": c, "grantable": *c != "desktop" && *c != "core",
        "needs": match *c { "core" => "always available", "workspace" => "in the member's allowlist and the syndicate ceiling",
            "exec" => "allowlist + ceiling + the member's explicit exec acknowledgement (runs programs in its workspace; not network-sandboxed)",
            "browser" => "allowlist + ceiling, and hosts supplied for the task (hosts must be in the member's allowed hosts)",
            _ => "only the built-in desktop role" }})).collect();
    v.push(json!({"name": "desktop_focus", "category": "desktop", "grantable": false, "needs": "only the built-in desktop role"}));
    Json::Array(v)
}

/// What a member is actually given, decided from its config and its syndicate's ceiling.
#[derive(Debug, Default, Clone)]
pub struct Grants {
    pub workspace: Vec<String>,
    pub exec: bool,
    pub browser: Vec<String>,
    /// (tool, reason) for tools the config names that the runtime will not hand over.
    pub declared_only: Vec<(String, String)>,
}

pub fn grants(config: &Json, manifest: &Json) -> Grants {
    let allowed: Vec<String> = match config.get("allowed_tools").and_then(Json::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(|t| canonical_tool(t).to_string())
            .collect(),
        None => vec!["read_file".into(), "write_file".into(), "list_files".into()],
    };
    let ceiling: BTreeSet<String> = strs(manifest, "allowed_tools_ceiling")
        .iter()
        .map(|t| canonical_tool(t).to_string())
        .collect();
    let acknowledged = config.get("exec_acknowledged") == Some(&json!(true));
    let mut g = Grants::default();
    for t in allowed {
        match tool_category(&t) {
            "core" => {}
            "workspace"
                if ceiling.contains(&t)
                    || ceiling.is_empty() && config.get("allowed_tools").is_none() =>
            {
                g.workspace.push(t)
            }
            "workspace" => g
                .declared_only
                .push((t, "above the syndicate ceiling".into())),
            "exec" if !ceiling.contains(&t) => g
                .declared_only
                .push((t, "above the syndicate ceiling".into())),
            "exec" if !acknowledged => g
                .declared_only
                .push((t, "execution is not acknowledged for this member".into())),
            "exec" => g.exec = true,
            "browser" if ceiling.contains(&t) => g.browser.push(t),
            "browser" => g
                .declared_only
                .push((t, "above the syndicate ceiling".into())),
            "desktop" => g.declared_only.push((
                t,
                "desktop control is only available to the built-in desktop role".into(),
            )),
            _ => g
                .declared_only
                .push((t, "the runtime has no such tool".into())),
        }
    }
    g.workspace.sort();
    g.workspace.dedup();
    g.browser.sort();
    g.browser.dedup();
    g
}

// ---------------------------------------------------------------- membership (runtime policy)
#[derive(Debug, Clone)]
pub struct Edge {
    pub to: String,
    #[allow(dead_code)]
    pub relation: String,
    #[allow(dead_code)]
    pub condition: String,
    pub max_rounds: Option<u64>,
    pub order: u64,
}

pub struct Member {
    pub syndicate_dir: PathBuf,
    pub agent_id: String,
    pub config: Json,
    pub manifest: Json,
}

/// The syndicate a role belongs to, if it is a syndicate member (base roles are not).
pub fn membership(root: &Path, role: &str) -> Option<Member> {
    let d = crate::agent::role_dir(root, role)?;
    let agents = d.parent()?;
    if agents.file_name()? != "agents" {
        return None;
    }
    let syn_dir = agents.parent()?;
    if syn_dir.parent()?.file_name()? != "syndicates" {
        return None;
    }
    let manifest = read_json(&syn_dir.join("syndicate.json"));
    if manifest.is_null() {
        return None;
    }
    Some(Member {
        syndicate_dir: syn_dir.to_path_buf(),
        agent_id: role.to_string(),
        config: read_json(&d.join("config.json")),
        manifest,
    })
}

fn edges_of(manifest: &Json) -> Vec<(usize, &Json)> {
    manifest
        .get("edges")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .collect()
}

impl Member {
    pub fn syndicate_name(&self) -> String {
        self.syndicate_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    fn edge(&self, i: usize, e: &Json) -> Edge {
        Edge {
            to: s(e, "to").into(),
            relation: e
                .get("relation")
                .and_then(Json::as_str)
                .unwrap_or("commands")
                .into(),
            condition: s(e, "condition").into(),
            max_rounds: e.pointer("/constraints/max_rounds").and_then(Json::as_u64),
            order: e
                .get("order")
                .and_then(Json::as_u64)
                .unwrap_or(i as u64 + 1),
        }
    }
    /// Outgoing relationships, in flow order.
    pub fn outgoing(&self) -> Vec<Edge> {
        let mut v: Vec<Edge> = edges_of(&self.manifest)
            .into_iter()
            .filter(|(_, e)| s(e, "from") == self.agent_id)
            .map(|(i, e)| self.edge(i, e))
            .collect();
        v.sort_by_key(|e| e.order);
        v
    }
    pub fn can_message(&self, to: &str) -> Option<Edge> {
        self.outgoing().into_iter().find(|e| e.to == to)
    }
    pub fn max_dispatch_rounds(&self) -> u64 {
        self.manifest
            .get("max_dispatch_rounds")
            .and_then(Json::as_u64)
            .unwrap_or(3)
            .clamp(1, 20)
    }
    /// How many times this member may delegate to `to` in one mission: the relationship's own cap, else the
    /// tightest loop cap that runs through this pair, else the syndicate-wide cap.
    pub fn cap_to(&self, to: &str) -> u64 {
        if let Some(r) = self.can_message(to).and_then(|e| e.max_rounds) {
            return r;
        }
        let mut cap = self.max_dispatch_rounds();
        for l in self
            .manifest
            .get("loops")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let path = strs(l, "path");
            let in_loop = path.windows(2).any(|w| w[0] == self.agent_id && w[1] == to);
            if in_loop {
                cap = cap.min(
                    l.get("max_iterations")
                        .and_then(Json::as_u64)
                        .unwrap_or(cap),
                );
            }
        }
        cap
    }
    pub fn coordinator(&self) -> String {
        s(&self.manifest, "coordinator_agent_id").to_string()
    }
    pub fn grants(&self) -> Grants {
        grants(&self.config, &self.manifest)
    }
    /// Hosts this member may be given: the request's hosts narrowed by the member's allowed_hosts (when it has any).
    pub fn narrow_hosts(&self, requested: &[String]) -> Vec<String> {
        let allowed = strs(&self.config, "allowed_hosts");
        if allowed.is_empty() {
            return requested.to_vec();
        }
        requested
            .iter()
            .filter(|h| allowed.contains(h))
            .cloned()
            .collect()
    }
    #[allow(dead_code)]
    pub fn max_turns(&self) -> Option<u64> {
        self.config.get("max_turns").and_then(Json::as_u64)
    }
    #[allow(dead_code)]
    pub fn timeout_seconds(&self) -> Option<u64> {
        self.config.get("timeout_seconds").and_then(Json::as_u64)
    }
    pub fn workspace(&self, root: &Path) -> PathBuf {
        match self.config.get("workspace_rel_path").and_then(Json::as_str) {
            Some(p) => root.join(p),
            None => self
                .syndicate_dir
                .join("agents")
                .join(&self.agent_id)
                .join("workspace"),
        }
    }
}

// ---------------------------------------------------------------- identity
fn humanize(c: &str) -> String {
    c.replace('_', " ")
}

fn relation_line(rel: &str, other: &str, incoming: bool) -> String {
    match (rel, incoming) {
        ("commands", false) => format!("You direct {other}."),
        ("commands", true) => format!("You take direction from {other}."),
        ("feeds", false) => format!("Your findings feed {other}."),
        ("feeds", true) => format!("{other} feeds you its findings."),
        ("verifies", false) => format!(
            "You verify the work of {other}; nothing of theirs is done until you have checked it."
        ),
        ("verifies", true) => {
            format!("{other} verifies your work; it is not done until they have checked it.")
        }
        ("challenges", false) => format!("You challenge the plans of {other} before they act."),
        ("challenges", true) => {
            format!("{other} challenges your plans before you act; answer them with evidence.")
        }
        ("escalates_to", false) => {
            format!("When something is beyond you, you escalate to {other}.")
        }
        ("escalates_to", true) => {
            format!("{other} escalates to you when something is beyond them.")
        }
        ("hands_off", false) => format!("When your stage is complete you hand off to {other}."),
        ("hands_off", true) => format!("{other} hands off to you when their stage is complete."),
        ("reports_to", false) => format!("You report to {other}."),
        (_, _) => format!("{other} reports to you."),
    }
}

/// Render a member's identity block from in-memory pieces (used for the on-disk files and for UI previews).
/// `people` lists (agent_id, persona name) for naming counterparts.
fn render_identity(
    me: &str,
    persona: &Json,
    syn: &Json,
    people: &dyn Fn(&str) -> String,
) -> String {
    let mut out = String::new();
    let title = s(persona, "title");
    out += &format!(
        "You are {}{}, of {}.\n",
        s(persona, "name"),
        if title.is_empty() {
            String::new()
        } else {
            format!(", {title}")
        },
        s(syn, "display_name")
    );
    if !s(persona, "backstory").is_empty() {
        out += &format!("{}\n", s(persona, "backstory"));
    }
    out += &format!(
        "\nThe mission of {}: {}\n",
        s(syn, "display_name"),
        s(syn, "mission")
    );
    if !s(syn, "purpose").is_empty() {
        out += &format!("Why it exists: {}\n", s(syn, "purpose"));
    }
    if !s(syn, "lore").is_empty() {
        out += &format!("{}\n", s(syn, "lore"));
    }
    let creed = strs(persona, "creed");
    if !creed.is_empty() {
        out += "\nWhat you hold to:\n";
        for c in creed {
            out += &format!("- {c}\n");
        }
    }
    let never = strs(persona, "never");
    if !never.is_empty() {
        out += "\nWhat you never do:\n";
        for c in never {
            out += &format!("- {c}\n");
        }
    }
    if !s(persona, "voice").is_empty() {
        out += &format!("\nHow you speak: {}\n", s(persona, "voice"));
    }
    // Relationships in flow order, each with its condition and cap.
    let mut rels: Vec<(u64, String)> = vec![];
    let mut can_message: Vec<String> = vec![];
    for (i, e) in edges_of(syn) {
        let rel = e
            .get("relation")
            .and_then(Json::as_str)
            .unwrap_or("commands");
        let order = e
            .get("order")
            .and_then(Json::as_u64)
            .unwrap_or(i as u64 + 1);
        let cond = if s(e, "condition").is_empty() {
            String::new()
        } else {
            format!(" When: {}.", humanize(s(e, "condition")))
        };
        let cap = e
            .pointer("/constraints/max_rounds")
            .and_then(Json::as_u64)
            .map(|r| format!(" At most {r} rounds."))
            .unwrap_or_default();
        if s(e, "from") == me {
            rels.push((
                order,
                format!(
                    "{}{cond}{cap}",
                    relation_line(rel, &people(s(e, "to")), false)
                ),
            ));
            can_message.push(people(s(e, "to")));
        }
        if s(e, "to") == me {
            rels.push((
                order,
                format!(
                    "{}{cond}{cap}",
                    relation_line(rel, &people(s(e, "from")), true)
                ),
            ));
        }
    }
    rels.sort_by_key(|(o, _)| *o);
    let mut place: Vec<String> = rels.into_iter().map(|(_, t)| t).collect();
    for l in syn
        .get("loops")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let path = strs(l, "path");
        if path.iter().any(|p| p == me) {
            place.push(format!(
                "You take part in the {} cycle ({}), at most {} rounds, until {}.",
                s(l, "name"),
                path.iter()
                    .map(|p| people(p))
                    .collect::<Vec<_>>()
                    .join(" → "),
                l.get("max_iterations").and_then(Json::as_u64).unwrap_or(1),
                s(l, "exit_when")
            ));
        }
    }
    place.push(if can_message.is_empty() {
        "You do not delegate to anyone; return your result to whoever gave you the task.".into()
    } else {
        format!("You may send messages only to: {}.", can_message.join(", "))
    });
    out += "\nYour place in the syndicate:\n";
    for p in place {
        out += &format!("- {p}\n");
    }
    out
}

/// The identity block for a syndicate member on disk, or None if it has no persona or officiation is off.
pub fn officiation(role_dir: &Path) -> Option<String> {
    let cfg: Json =
        serde_json::from_str(&fs::read_to_string(role_dir.join("config.json")).ok()?).ok()?;
    let persona = cfg.get("persona")?;
    let syn_path = role_dir.parent()?.parent()?.join("syndicate.json");
    let syn: Json = serde_json::from_str(&fs::read_to_string(syn_path).ok()?).ok()?;
    if syn.pointer("/officiation/enabled") == Some(&json!(false)) {
        return None;
    }
    let me = s(&cfg, "agent_id").to_string();
    let name = |id: &str| -> String {
        role_dir
            .parent()
            .map(|a| read_json(&a.join(id).join("config.json")))
            .and_then(|c| {
                c.pointer("/persona/name")
                    .and_then(Json::as_str)
                    .map(|n| format!("{n} ({id})"))
            })
            .unwrap_or_else(|| id.to_string())
    };
    Some(render_identity(&me, persona, &syn, &name))
}

/// Identity blocks for every member of an unsaved bundle: `{agent_id: text}`.
pub fn preview(root: &Path, bundle: &Json) -> Json {
    let syn = merged_manifest(root, bundle);
    let members: Vec<&Json> = bundle["members"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if syn.pointer("/officiation/enabled") == Some(&json!(false)) {
        return json!({});
    }
    let name = |id: &str| -> String {
        members
            .iter()
            .find(|m| s(m, "agent_id") == id)
            .and_then(|m| {
                m.pointer("/persona/name")
                    .and_then(Json::as_str)
                    .filter(|n| !n.is_empty())
                    .map(|n| format!("{n} ({id})"))
            })
            .unwrap_or_else(|| id.to_string())
    };
    let mut out = Map::new();
    for m in &members {
        out.insert(
            s(m, "agent_id").to_string(),
            json!(render_identity(
                s(m, "agent_id"),
                &m["persona"],
                &syn,
                &name
            )),
        );
    }
    Json::Object(out)
}

#[path = "syndicate/storage.rs"]
mod storage;
#[path = "syndicate/validation.rs"]
mod validation;

pub use storage::{apply, list_dirs, load_bundle};
use validation::merged_manifest;
pub use validation::{notes, validate};
