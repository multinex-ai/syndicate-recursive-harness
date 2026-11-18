use super::validation::{slug_ok, validate};
use super::*;

// ---------------------------------------------------------------- load / list / apply
/// Syndicate folder names under `syndicates/`.
pub fn list_dirs(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root.join("syndicates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("syndicate.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// An editable bundle read back from disk: the manifest plus each member's config.
pub fn load_bundle(root: &Path, dir: &str) -> Result<Json, String> {
    if !slug_ok(dir) {
        return Err("bad syndicate folder name".into());
    }
    let sd = root.join("syndicates").join(dir);
    let manifest = read_json(&sd.join("syndicate.json"));
    if manifest.is_null() {
        return Err(format!("no syndicate named {dir}"));
    }
    let members: Vec<Json> = strs(&manifest, "agent_ids")
        .iter()
        .map(|id| {
            let c = read_json(&sd.join("agents").join(id).join("config.json"));
            let mut m = json!({"agent_id": id});
            for k in [
                "role_title",
                "persona",
                "allowed_tools",
                "allowed_hosts",
                "max_turns",
                "timeout_seconds",
                "exec_acknowledged",
                "model_profile_id",
            ] {
                if let Some(v) = c.get(k) {
                    m[k] = v.clone();
                }
            }
            m
        })
        .collect();
    Ok(json!({"dir": dir, "syndicate": manifest, "members": members}))
}

/// Validate, back up, and write. Unknown keys already in the files (mcp_servers, provenance…) are kept.
/// Members dropped from the bundle are archived under `.history/<ts>/removed/`, so they can no longer be run.
pub fn apply(root: &Path, bundle: &Json, dry_run: bool) -> Result<Json, String> {
    let errs = validate(root, bundle);
    if !errs.is_empty() {
        return Err(format!("bundle rejected:\n- {}", errs.join("\n- ")));
    }
    let dir = root.join("syndicates").join(s(bundle, "dir"));
    let syn_path = dir.join("syndicate.json");
    let mut writes: Vec<(PathBuf, Json, bool)> = vec![];
    let mut syn = read_obj(&syn_path);
    let old_ids = strs(&Json::Object(syn.clone()), "agent_ids");
    for (k, v) in bundle["syndicate"].as_object().cloned().unwrap_or_default() {
        syn.insert(k, v);
    }
    syn.entry("schema_version").or_insert(json!(1));
    let new_ids: Vec<String> = bundle["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| s(m, "agent_id").to_string())
        .collect();
    syn.insert("agent_ids".into(), json!(new_ids));
    writes.push((
        syn_path.clone(),
        Json::Object(syn.clone()),
        syn_path.exists(),
    ));
    for m in bundle["members"].as_array().unwrap() {
        let id = s(m, "agent_id");
        let p = dir.join("agents").join(id).join("config.json");
        let existed = p.exists();
        let mut cfg = read_obj(&p);
        if !existed {
            cfg.insert("schema_version".into(), json!(1));
            cfg.insert(
                "model_profile_id".into(),
                json!("local-qwen35-9b-uncensored"),
            );
            cfg.insert("prompt_file".into(), json!("PROMPT.md"));
            cfg.insert(
                "system_instructions_file".into(),
                json!("SYSTEM_INSTRUCTIONS.md"),
            );
            cfg.insert("vision".into(), json!(false));
            cfg.insert(
                "workspace_rel_path".into(),
                json!(format!(
                    "syndicates/{}/agents/{id}/workspace",
                    s(bundle, "dir")
                )),
            );
        }
        cfg.insert("agent_id".into(), json!(id));
        cfg.insert("syndicate_id".into(), syn["syndicate_id"].clone());
        for k in [
            "role_title",
            "persona",
            "allowed_tools",
            "allowed_hosts",
            "max_turns",
            "timeout_seconds",
            "model_profile_id",
            "exec_acknowledged",
        ] {
            if let Some(v) = m.get(k) {
                cfg.insert(k.into(), v.clone());
            }
        }
        writes.push((p, Json::Object(cfg), existed));
    }
    let removed: Vec<&String> = old_ids.iter().filter(|o| !new_ids.contains(o)).collect();
    let mut plan: Vec<Json> = writes.iter().map(|(p, _, e)| json!({"path": p.strip_prefix(root).unwrap_or(p), "action": if *e { "update" } else { "create" }})).collect();
    for r in &removed {
        plan.push(json!({"path": format!("syndicates/{}/agents/{r}", s(bundle, "dir")), "action": "archive (member removed)"}));
    }
    if dry_run {
        return Ok(json!({"dry_run": true, "writes": plan}));
    }
    let backup = dir.join(".history").join(format!("{}", now() as u64));
    for (p, v, existed) in &writes {
        if *existed {
            let rel = p.strip_prefix(&dir).unwrap_or(p);
            let b = backup.join(rel);
            fs::create_dir_all(b.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::copy(p, &b).map_err(|e| e.to_string())?;
        }
        let parent = p.parent().unwrap();
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        if parent.file_name().and_then(|n| n.to_str()) != Some(s(bundle, "dir")) {
            for sub in ["workspace", "inbox"] {
                fs::create_dir_all(parent.join(sub)).map_err(|e| e.to_string())?;
            }
            let prompt = parent.join("PROMPT.md");
            if !prompt.exists() {
                fs::write(&prompt, format!("Carry out your part of the {} mission with the tools you have, and report what actually happened.\n",
                    s(&bundle["syndicate"], "display_name"))).map_err(|e| e.to_string())?;
            }
        }
        write_pretty(p, v)?;
    }
    for r in &removed {
        let from = dir.join("agents").join(r);
        if from.is_dir() {
            let to = backup.join("removed").join(r);
            fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::rename(&from, &to).map_err(|e| format!("archive {r}: {e}"))?;
        }
    }
    Ok(
        json!({"applied": true, "writes": plan, "backup": if backup.exists() { json!(backup.strip_prefix(root).unwrap_or(&backup)) } else { Json::Null }}),
    )
}
