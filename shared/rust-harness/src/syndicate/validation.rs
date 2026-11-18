use super::*;

// ---------------------------------------------------------------- validation
pub(super) fn slug_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}
/// A hostname (letters, digits, dashes, dots; no scheme, path, port or wildcard) or an IP literal.
pub(super) fn host_ok(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    if h == "::1" || h.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    h.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}
fn tool_name_ok(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The manifest as it will be after apply: the file on disk overlaid with the bundle's syndicate object.
pub(super) fn merged_manifest(root: &Path, bundle: &Json) -> Json {
    let dir = s(bundle, "dir");
    let mut merged = if slug_ok(dir) {
        read_obj(&root.join("syndicates").join(dir).join("syndicate.json"))
    } else {
        Map::new()
    };
    for (k, v) in bundle["syndicate"].as_object().cloned().unwrap_or_default() {
        merged.insert(k, v);
    }
    Json::Object(merged)
}

/// Every problem with a bundle; empty means it can be applied.
pub fn validate(root: &Path, bundle: &Json) -> Vec<String> {
    let mut errs = vec![];
    let dir = s(bundle, "dir");
    let merged = merged_manifest(root, bundle);
    let syn = &merged;
    if !slug_ok(dir) {
        errs.push(format!(
            "dir \"{dir}\" must be lowercase letters, digits and dashes"
        ));
    }
    if s(syn, "syndicate_id").is_empty() {
        errs.push("syndicate.syndicate_id is required".into());
    }
    if s(syn, "mission").trim().is_empty() {
        errs.push("syndicate.mission is required".into());
    }
    let members: Vec<&Json> = bundle["members"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if members.is_empty() {
        errs.push("at least one member is required".into());
    }
    let ids: Vec<String> = members
        .iter()
        .map(|m| s(m, "agent_id").to_string())
        .collect();
    let tool_ceiling = strs(syn, "allowed_tools_ceiling");
    let host_ceiling = strs(syn, "allowed_hosts_ceiling");
    for t in &tool_ceiling {
        if !tool_name_ok(t) {
            errs.push(format!("tool ceiling: \"{t}\" is not a valid tool name (lowercase letters, digits, underscores)"));
        }
    }
    for h in &host_ceiling {
        if !host_ok(h) {
            errs.push(format!("host ceiling: \"{h}\" is not a valid host (a bare hostname or IP, no scheme, port or wildcard)"));
        }
    }
    for (i, id) in ids.iter().enumerate() {
        if !slug_ok(id) {
            errs.push(format!(
                "member \"{id}\": id must be lowercase letters, digits and dashes"
            ));
        }
        if ids[..i].contains(id) {
            errs.push(format!("member \"{id}\" appears twice"));
        }
        if BASE_ROLES.contains(&id.as_str()) || root.join("agents").join(id).is_dir() {
            errs.push(format!("member \"{id}\" collides with a base role; base roles always win, so this member would never run"));
        }
        // an id may live in only one syndicate, or role_dir would pick one arbitrarily
        if let Ok(rd) = fs::read_dir(root.join("syndicates")) {
            for e in rd.flatten() {
                let other = e.file_name().to_string_lossy().into_owned();
                if other != dir && e.path().join("agents").join(id).is_dir() {
                    errs.push(format!("member \"{id}\" already belongs to syndicate \"{other}\"; a member id must be unique across syndicates"));
                }
            }
        }
    }
    let coord = s(syn, "coordinator_agent_id");
    if !coord.is_empty() && !ids.iter().any(|i| i == coord) {
        errs.push(format!("coordinator \"{coord}\" is not a member"));
    }
    for m in &members {
        let id = s(m, "agent_id");
        for t in strs(m, "allowed_tools") {
            if !tool_name_ok(&t) {
                errs.push(format!("{id}: \"{t}\" is not a valid tool name"));
            } else if !tool_ceiling
                .iter()
                .any(|c| canonical_tool(c) == canonical_tool(&t))
            {
                errs.push(format!("{id}: tool \"{t}\" is above the syndicate ceiling"));
            }
        }
        for h in strs(m, "allowed_hosts") {
            if !host_ok(&h) {
                errs.push(format!("{id}: \"{h}\" is not a valid host (a bare hostname or IP, no scheme, port or wildcard)"));
            } else if !host_ceiling.contains(&h) {
                errs.push(format!("{id}: host \"{h}\" is above the syndicate ceiling"));
            }
        }
        if let Some(t) = m.get("max_turns") {
            if !t.as_u64().map(|n| (1..=32).contains(&n)).unwrap_or(false) {
                errs.push(format!(
                    "{id}: max_turns must be a whole number from 1 to 32"
                ));
            }
        }
        if let Some(t) = m.get("timeout_seconds") {
            if !t
                .as_u64()
                .map(|n| (30..=3600).contains(&n))
                .unwrap_or(false)
            {
                errs.push(format!(
                    "{id}: timeout_seconds must be a whole number from 30 to 3600"
                ));
            }
        }
        let p = &m["persona"];
        if !p.is_object() || s(p, "name").trim().is_empty() {
            errs.push(format!("{id}: persona.name is required"));
        }
        for (field, text) in identity_texts(syn, p) {
            let hits = lint(&text);
            if !hits.is_empty() {
                errs.push(format!(
                    "{id}: {field} contains framing words {hits:?}; state the identity as fact"
                ));
            }
        }
    }
    for (i, e) in syn["edges"].as_array().into_iter().flatten().enumerate() {
        if !ids.iter().any(|x| x == s(e, "from")) {
            errs.push(format!(
                "edge {i}: from \"{}\" is not a member",
                s(e, "from")
            ));
        }
        if !ids.iter().any(|x| x == s(e, "to")) && !BASE_ROLES.contains(&s(e, "to")) {
            errs.push(format!(
                "edge {i}: to \"{}\" is neither a member nor a packaged worker",
                s(e, "to")
            ));
        }
        if s(e, "from") == s(e, "to") {
            errs.push(format!(
                "edge {i}: a member cannot point at itself (use a loop)"
            ));
        }
        let rel = e
            .get("relation")
            .and_then(Json::as_str)
            .unwrap_or("commands");
        if !RELATIONS.contains(&rel) {
            errs.push(format!("edge {i}: unknown relation \"{rel}\""));
        }
        if let Some(r) = e.pointer("/constraints/max_rounds").and_then(Json::as_u64) {
            if r == 0 || r > 20 {
                errs.push(format!("edge {i}: max_rounds must be 1-20"));
            }
        }
        if s(e, "condition").len() > 240 {
            errs.push(format!("edge {i}: condition is longer than 240 characters"));
        }
        let hits = lint(s(e, "condition"));
        if !hits.is_empty() {
            errs.push(format!(
                "edge {i}: condition contains framing words {hits:?}"
            ));
        }
        for t in strs(&e["constraints"], "tools") {
            if !tool_ceiling.contains(&t) {
                errs.push(format!(
                    "edge {i}: tool \"{t}\" is above the syndicate ceiling"
                ));
            }
        }
    }
    for (i, l) in syn["loops"].as_array().into_iter().flatten().enumerate() {
        let path = strs(l, "path");
        if path.len() < 2 {
            errs.push(format!("loop {i}: needs at least two members"));
        }
        for p in &path {
            if !ids.contains(p) {
                errs.push(format!("loop {i}: \"{p}\" is not a member"));
            }
        }
        let n = l.get("max_iterations").and_then(Json::as_u64).unwrap_or(0);
        if n == 0 || n > MAX_LOOP_ITERATIONS {
            errs.push(format!(
                "loop {i}: max_iterations must be 1-{MAX_LOOP_ITERATIONS}"
            ));
        }
        if s(l, "exit_when").trim().is_empty() {
            errs.push(format!(
                "loop {i}: exit_when is required so the loop can end"
            ));
        }
    }
    let rounds = syn
        .get("max_dispatch_rounds")
        .and_then(Json::as_u64)
        .unwrap_or(0);
    if rounds == 0 || rounds > 20 {
        errs.push("syndicate.max_dispatch_rounds must be 1-20".into());
    }
    errs
}

/// Things that are valid but will not behave the way the config suggests. Shown as warnings, never blocks apply.
pub fn notes(root: &Path, bundle: &Json) -> Vec<Json> {
    let mut out = vec![];
    let syn = merged_manifest(root, bundle);
    let members: Vec<&Json> = bundle["members"]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let coord = s(&syn, "coordinator_agent_id").to_string();
    let edges = edges_of(&syn);
    for m in &members {
        let id = s(m, "agent_id");
        let g = grants(m, &syn);
        for (t, why) in &g.declared_only {
            out.push(json!({"member": id, "code": "declared_only", "text": format!("{id}: \"{t}\" is listed but will not be granted: {why}.")}));
        }
        if m.get("allowed_tools")
            .and_then(Json::as_array)
            .map(|a| a.iter().any(|t| t == "list_dir"))
            .unwrap_or(false)
        {
            out.push(json!({"member": id, "code": "alias", "text": format!("{id}: \"list_dir\" is the older name for \"list_files\"; both are treated as list_files.")}));
        }
        if !g.browser.is_empty() && strs(m, "allowed_hosts").is_empty() {
            out.push(json!({"member": id, "code": "browser_no_hosts", "text": format!("{id}: has browser tools but no allowed hosts, so it can open nothing.")}));
        }
        let outgoing = edges.iter().any(|(_, e)| s(e, "from") == id);
        let incoming = edges.iter().any(|(_, e)| s(e, "to") == id);
        if !outgoing && id != coord {
            out.push(json!({"member": id, "code": "cannot_delegate", "text": format!("{id}: has no outgoing relationship, so it cannot message anyone (send_message is removed).")}));
        }
        if !incoming && id != coord {
            out.push(json!({"member": id, "code": "unreachable", "text": format!("{id}: has no incoming relationship, so nobody can send it work.")}));
        }
    }
    if coord.is_empty() {
        out.push(json!({"member": null, "code": "no_coordinator", "text": "No coordinator is set, so a mission has no entry point."}));
    }
    out
}

/// Every identity string that ends up in a member's prompt.
fn identity_texts(syndicate: &Json, persona: &Json) -> Vec<(String, String)> {
    let mut out = vec![];
    for k in ["display_name", "mission", "purpose", "lore"] {
        out.push((format!("syndicate.{k}"), s(syndicate, k).to_string()));
    }
    for k in ["name", "title", "backstory", "voice"] {
        out.push((format!("persona.{k}"), s(persona, k).to_string()));
    }
    for k in ["creed", "never"] {
        for (i, x) in strs(persona, k).into_iter().enumerate() {
            out.push((format!("persona.{k}[{i}]"), x));
        }
    }
    out
}
