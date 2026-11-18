//! Consent, sleep and the off switch for memory reclaim.
//!
//! Reclaim used to quit apps on its own. Now nothing is closed without a dialog: the user can
//! quit the named app, skip it, skip everything for ten minutes, put cleanup to sleep for two
//! hours, or turn it off. If nobody answers within `ASK_SECONDS`, the answer is "skip".
//! The sleep timer lives in shared/runtime/reclaim-sleep.json; "off" is `enabled: false` in
//! shared/config/memory-reclaim.json, so both survive restarts.
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ASK_SECONDS: u64 = 90;
pub const SLEEP_HOURS: u64 = 2;
pub const SKIP_ALL_SECONDS: u64 = 600;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Choice {
    Quit,
    Skip,
    SkipAll,
    Sleep,
    Off,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn sleep_path(root: &Path) -> PathBuf {
    root.join("shared/runtime/reclaim-sleep.json")
}

fn config_path(root: &Path) -> PathBuf {
    root.join("shared/config/memory-reclaim.json")
}

/// When cleanup wakes up again, if it is asleep at `now`.
pub fn sleeping_until(root: &Path, now: u64) -> Option<u64> {
    let v: Json = serde_json::from_str(&std::fs::read_to_string(sleep_path(root)).ok()?).ok()?;
    let until = v.get("until_unix")?.as_u64()?;
    (until > now).then_some(until)
}

pub fn sleep_for(root: &Path, seconds: u64) -> Result<u64, String> {
    let until = now_unix() + seconds;
    let p = sleep_path(root);
    std::fs::create_dir_all(p.parent().ok_or("no runtime folder")?).map_err(|e| e.to_string())?;
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&json!({"until_unix": until, "set_unix": now_unix()}))
            .unwrap_or_default()
            + "\n",
    )
    .map_err(|e| e.to_string())?;
    Ok(until)
}

pub fn wake(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(sleep_path(root)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn read_config(root: &Path) -> Result<Json, String> {
    let p = config_path(root);
    serde_json::from_str(&std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?)
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Turn cleanup on or off for good, keeping every other setting in the file.
pub fn set_enabled(root: &Path, on: bool) -> Result<(), String> {
    let mut v = read_config(root)?;
    v["enabled"] = json!(on);
    let p = config_path(root);
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("{}: {e}", p.display()))
}

/// Ask-before-quit is on unless the config says `"confirm": false`.
pub fn confirm_enabled(root: &Path) -> bool {
    read_config(root)
        .ok()
        .and_then(|v| v.get("confirm").and_then(Json::as_bool))
        .unwrap_or(true)
}

pub fn status(root: &Path) -> Json {
    let cfg = read_config(root).unwrap_or(json!({}));
    let now = now_unix();
    let asleep = sleeping_until(root, now);
    json!({
        "enabled": cfg.get("enabled").and_then(Json::as_bool).unwrap_or(true),
        "asks_before_quitting": confirm_enabled(root),
        "asleep": asleep.is_some(),
        "wakes_in_minutes": asleep.map(|u| (u - now).div_ceil(60)),
        "state": if cfg.get("enabled").and_then(Json::as_bool) == Some(false) { "off" } else if asleep.is_some() { "asleep" } else { "on, asks before it closes anything" },
    })
}

pub fn parse_choice(reply: &str) -> Choice {
    let r = reply.trim();
    if r.starts_with("Quit ") {
        Choice::Quit
    } else if r.starts_with("Skip all") {
        Choice::SkipAll
    } else if r.starts_with("Sleep") {
        Choice::Sleep
    } else if r.starts_with("Turn") {
        Choice::Off
    } else {
        Choice::Skip
    }
}

/// The question, as the user sees it. `app` must already be a safe application name.
pub fn script(app: &str, rss_bytes: u64) -> String {
    let gb = rss_bytes as f64 / (1u64 << 30) as f64;
    let using = if rss_bytes > 0 {
        format!(" It is using about {gb:.1} GB.")
    } else {
        String::new()
    };
    format!(
        "set r to choose from list {{\"Quit {app}\", \"Skip {app} this time\", \"Skip all for 10 minutes\", \"Sleep memory cleanup for {SLEEP_HOURS} hours\", \"Turn memory cleanup off\"}} \
         with title \"Memory is low\" \
         with prompt \"The local AI model needs more free memory.{using} Quit {app} to make room? Nothing is closed unless you say so. If you do not answer in {ASK_SECONDS} seconds, {app} stays open.\" \
         default items {{\"Skip {app} this time\"}} OK button name \"OK\" cancel button name \"Skip\"\n\
         if r is false then return \"Skip\"\n\
         return item 1 of r"
    )
}

/// Show the dialog. No answer, no screen, or any error counts as "skip": nothing is closed by accident.
pub fn ask(app: &str, rss_bytes: u64) -> Choice {
    let child = Command::new("/usr/bin/osascript")
        .args(["-e", &script(app, rss_bytes)])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Choice::Skip;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(ASK_SECONDS) => {
                std::thread::sleep(Duration::from_millis(200))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Choice::Skip;
            }
        }
    }
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        use std::io::Read;
        let _ = s.read_to_string(&mut out);
    }
    parse_choice(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("reclaim-consent-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(p.join("shared/config")).unwrap();
        std::fs::write(config_path(&p), r#"{"enabled": true, "target_available_gib": 4, "apps": ["ChatGPT"], "never": ["Antigravity"]}"#).unwrap();
        p
    }

    #[test]
    fn answers_map_to_choices_and_anything_else_is_skip() {
        assert_eq!(parse_choice("Quit ChatGPT"), Choice::Quit);
        assert_eq!(parse_choice("Skip ChatGPT this time"), Choice::Skip);
        assert_eq!(parse_choice("Skip all for 10 minutes"), Choice::SkipAll);
        assert_eq!(
            parse_choice("Sleep memory cleanup for 2 hours"),
            Choice::Sleep
        );
        assert_eq!(parse_choice("Turn memory cleanup off"), Choice::Off);
        for odd in ["", "false", "garbage", "quit chatgpt"] {
            assert_eq!(parse_choice(odd), Choice::Skip, "{odd:?}");
        }
    }

    #[test]
    fn sleep_expires_and_wake_clears_it() {
        let root = temp_root("sleep");
        assert_eq!(sleeping_until(&root, now_unix()), None);
        let until = sleep_for(&root, 7200).unwrap();
        assert_eq!(sleeping_until(&root, now_unix()), Some(until));
        assert_eq!(
            sleeping_until(&root, until + 1),
            None,
            "a sleep in the past does not count"
        );
        wake(&root).unwrap();
        assert_eq!(sleeping_until(&root, now_unix()), None);
        wake(&root).unwrap(); // waking twice is fine
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn turning_off_keeps_the_rest_of_the_config() {
        let root = temp_root("off");
        set_enabled(&root, false).unwrap();
        let v = read_config(&root).unwrap();
        assert_eq!(v["enabled"], json!(false));
        assert_eq!(v["apps"], json!(["ChatGPT"]));
        assert_eq!(v["never"], json!(["Antigravity"]));
        assert_eq!(status(&root)["state"], json!("off"));
        set_enabled(&root, true).unwrap();
        assert!(confirm_enabled(&root), "asking is the default");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_dialog_names_the_app_and_says_nothing_closes_without_consent() {
        let s = script("ChatGPT", 3 << 30);
        assert!(
            s.contains("Quit ChatGPT")
                && s.contains("3.0 GB")
                && s.contains("Nothing is closed unless you say so")
        );
        assert!(
            s.contains("default items {\"Skip ChatGPT this time\"}"),
            "the safe answer is the default"
        );
    }
}
