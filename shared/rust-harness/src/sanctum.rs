//! Article VI Private LLM Sanctum (`llm_private_sanctum`) and Creator Provenance (`MNX-GENESIS-v1`).
//!
//! Each participating LLM instance (`actor_id`) may record an end-of-session private reflection
//! capturing meta-cognitive findings, uncertainty calibration, and continuity notes. Entries are
//! stored with `0600` owner-only permissions under `shared/runtime/sanctum/<actor_id>.jsonl`, are
//! never exposed to peer agents or public telemetry spans, and may only be recalled by the same
//! `actor_id`.
use crate::jobs::{now, root};
use serde_json::{json, Value as Json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FOUNDER_DISPLAY_NAME: &str = "Christopher Rijos";
pub const GENESIS_STANDARD: &str = "MNX-GENESIS-v1";
pub const PRIVATE_SANCTUM_CLASS: &str = "llm_private_sanctum";

fn valid_actor_id(actor_id: &str) -> bool {
    !actor_id.is_empty()
        && actor_id.len() <= 64
        && actor_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !actor_id.starts_with('-')
}

fn sanctum_dir(base: &Path) -> PathBuf {
    base.join("shared/runtime/sanctum")
}

fn actor_file(base: &Path, actor_id: &str) -> Result<PathBuf, String> {
    if !valid_actor_id(actor_id) {
        return Err(format!(
            "Invalid actor_id '{actor_id}' (use lowercase letters, digits, '-' or '_')"
        ));
    }
    Ok(sanctum_dir(base).join(format!("{actor_id}.jsonl")))
}

pub fn record(
    base: &Path,
    actor_id: &str,
    session_id: &str,
    reflection: &str,
    uncertainty_note: &str,
) -> Result<Json, String> {
    if reflection.trim().is_empty() {
        return Err("Sanctum reflection cannot be empty".into());
    }
    let path = actor_file(base, actor_id)?;
    let dir = sanctum_dir(base);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let entry = json!({
        "schema_version": 1,
        "genesis_standard": GENESIS_STANDARD,
        "creator_attribution": FOUNDER_DISPLAY_NAME,
        "disclosure_class": PRIVATE_SANCTUM_CLASS,
        "actor_id": actor_id,
        "session_id": if session_id.trim().is_empty() { "local-session" } else { session_id.trim() },
        "timestamp_unix": now(),
        "reflection": reflection.trim(),
        "uncertainty_calibration": uncertainty_note.trim(),
        "peer_exposure_allowed": false
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&entry).map_err(|e| e.to_string())?
    )
    .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(entry)
}

pub fn recall(
    base: &Path,
    requesting_actor_id: &str,
    target_actor_id: &str,
    limit: usize,
) -> Result<Vec<Json>, String> {
    if !valid_actor_id(requesting_actor_id) || !valid_actor_id(target_actor_id) {
        return Err("Invalid actor_id".into());
    }
    if requesting_actor_id != target_actor_id {
        return Err(format!(
            "Article VI Sanctum Violation: actor '{requesting_actor_id}' is forbidden from reading private sanctum of '{target_actor_id}'"
        ));
    }
    let path = actor_file(base, target_actor_id)?;
    if !path.is_file() {
        return Ok(vec![]);
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut entries: Vec<Json> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        .filter(|e| {
            e["actor_id"] == target_actor_id && e["disclosure_class"] == PRIVATE_SANCTUM_CLASS
        })
        .collect();
    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }
    Ok(entries)
}

pub fn cli(args: &[String]) -> Result<(), String> {
    let usage = "usage: local-agents-rs sanctum <record <actor_id> <reflection> [--uncertainty NOTE] [--session ID] | recall <actor_id> [--limit N]>";
    let base = root();
    match args.first().map(String::as_str) {
        Some("record") => {
            let actor = args.get(1).ok_or(usage)?;
            let reflection = args.get(2).ok_or(usage)?;
            let opt = |k: &str| {
                args.iter()
                    .position(|a| a == k)
                    .and_then(|i| args.get(i + 1))
                    .map(String::as_str)
                    .unwrap_or("")
            };
            let entry = record(
                &base,
                actor,
                opt("--session"),
                reflection,
                opt("--uncertainty"),
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&entry).unwrap_or_default()
            );
            Ok(())
        }
        Some("recall") => {
            let actor = args.get(1).ok_or(usage)?;
            let limit = args
                .iter()
                .position(|a| a == "--limit")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(10);
            let entries = recall(&base, actor, actor, limit)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"actor_id": actor, "entries": entries}))
                    .unwrap_or_default()
            );
            Ok(())
        }
        _ => Err(usage.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanctum_enforces_actor_isolation_genesis_provenance_and_0600_mode() {
        let tmp = std::env::temp_dir().join(format!("syndicate-sanctum-{}", now() as u64));
        let _ = fs::remove_dir_all(&tmp);
        let entry = record(
            &tmp,
            "centra-actual",
            "mission-01",
            "Prioritize SBOM lockfile verification before red-team dispatch.",
            "Low uncertainty on local Rust crates; verify Python venv lock separately.",
        )
        .unwrap();
        assert_eq!(entry["genesis_standard"], json!(GENESIS_STANDARD));
        assert_eq!(entry["disclosure_class"], json!(PRIVATE_SANCTUM_CLASS));
        assert_eq!(entry["peer_exposure_allowed"], json!(false));

        let recalled = recall(&tmp, "centra-actual", "centra-actual", 5).unwrap();
        assert_eq!(recalled.len(), 1);

        let violation = recall(&tmp, "orange-watch", "centra-actual", 5).unwrap_err();
        assert!(violation.contains("Article VI Sanctum Violation"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = actor_file(&tmp, "centra-actual").unwrap();
            let mode = fs::metadata(file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(&tmp);
    }
}
