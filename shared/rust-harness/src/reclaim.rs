//! Memory reclaim before a heavy model load. Only when available RAM is below the
//! target, it asks apps to quit normally, one at a time, until the target is met:
//! first the apps named in shared/config/memory-reclaim.json, then (auto mode) the
//! largest remaining app in /Applications that the harness doesn't need.
//! Afterwards it reopens them in the background.
//!
//! It never force-kills: apps get a normal Quit, so they can show their own save
//! prompts. An app that doesn't quit in time is recorded, not killed.
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

const GIB: f64 = (1u64 << 30) as f64;
/// Never touched, even if someone lists them: the agent host, security, the OS.
const PROTECTED: [&str; 19] = [
    "Claude",
    "claude",
    "Codex",
    "Codex Computer Use",
    "Antigravity",
    "antigravity",
    "Terminal",
    "iTerm",
    "Ghostty",
    "Finder",
    "WindowServer",
    "loginwindow",
    "Dock",
    "System Settings",
    "Microsoft Defender",
    "Microsoft Defender Shim",
    "Company Portal",
    "Microsoft AutoUpdate",
    "Self Service",
];

pub fn is_protected(name: &str, never: &[String]) -> bool {
    PROTECTED.iter().any(|p| p.eq_ignore_ascii_case(name))
        || never.iter().any(|p| p.eq_ignore_ascii_case(name))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub target_available_bytes: u64,
    pub quit_timeout: Duration,
    pub restore_after: bool,
    pub apps: Vec<String>,
    /// After the named apps, quit the largest eligible app, repeatedly.
    pub auto_largest: bool,
    /// Auto mode ignores apps smaller than this.
    pub min_app_bytes: u64,
    /// Extra apps that must never be quit.
    pub never: Vec<String>,
    pub tolerate_warning_gib: Option<f64>,
}

impl Policy {
    pub fn path(root: &Path) -> PathBuf {
        root.join("shared/config/memory-reclaim.json")
    }

    pub fn load(root: &Path) -> Result<Option<Policy>, String> {
        let p = Self::path(root);
        if !p.is_file() {
            return Ok(None);
        }
        let v: Json =
            serde_json::from_str(&std::fs::read_to_string(&p).map_err(|e| e.to_string())?)
                .map_err(|e| format!("{}: {e}", p.display()))?;
        Self::from_json(&v).map(Some)
    }

    pub fn from_json(v: &Json) -> Result<Policy, String> {
        if v.get("enabled").and_then(Json::as_bool) == Some(false) {
            return Ok(Policy {
                target_available_bytes: 0,
                quit_timeout: Duration::ZERO,
                restore_after: false,
                apps: vec![],
                auto_largest: false,
                min_app_bytes: 0,
                never: vec![],
                tolerate_warning_gib: None,
            });
        }
        let gib = v
            .get("target_available_gib")
            .and_then(Json::as_f64)
            .unwrap_or(4.0);
        if !(1.0..=16.0).contains(&gib) {
            return Err("target_available_gib must be between 1 and 16".into());
        }
        let tolerate_warning_gib = v.get("tolerate_warning_gib").and_then(Json::as_f64);
        let names = |key: &str| -> Result<Vec<String>, String> {
            let mut out = vec![];
            for a in v.get(key).and_then(Json::as_array).into_iter().flatten() {
                let name = a
                    .as_str()
                    .ok_or(format!("{key} must be a list of application names"))?;
                if !valid_name(name) {
                    return Err(format!("Unsupported application name: {name:?}"));
                }
                out.push(name.to_string());
            }
            Ok(out)
        };
        let never = names("never")?;
        let apps = names("apps")?;
        if let Some(p) = apps.iter().find(|a| is_protected(a, &never)) {
            return Err(format!("{p} is protected and can't be reclaimed"));
        }
        Ok(Policy {
            target_available_bytes: (gib * GIB) as u64,
            quit_timeout: Duration::from_secs(
                v.get("quit_timeout_seconds")
                    .and_then(Json::as_u64)
                    .unwrap_or(20)
                    .clamp(3, 120),
            ),
            restore_after: v
                .get("restore_after")
                .and_then(Json::as_bool)
                .unwrap_or(true),
            apps,
            auto_largest: v
                .get("auto_largest")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            min_app_bytes: v.get("min_app_mb").and_then(Json::as_u64).unwrap_or(200) << 20,
            never,
            tolerate_warning_gib,
        })
    }
}

fn osascript(script: &str) -> Result<String, String> {
    let out = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn app_running(name: &str) -> bool {
    osascript(&format!("application \"{name}\" is running"))
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// Resident memory of every process whose executable lives inside <name>.app.
pub fn app_rss_bytes(name: &str) -> u64 {
    let marker = format!("/{name}.app/");
    let out = Command::new("/bin/ps")
        .args(["-axo", "rss=,comm="])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (rss, comm) = l.split_once(' ')?;
            comm.contains(&marker)
                .then(|| rss.trim().parse::<u64>().ok().map(|k| k * 1024))
                .flatten()
        })
        .sum()
}

/// Apps installed under /Applications or ~/Applications that are running now,
/// with their summed resident memory, largest first.
pub fn apps_by_memory() -> Vec<(String, u64)> {
    let out = Command::new("/bin/ps")
        .args(["-axo", "rss=,comm="])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let home_apps = format!(
        "{}/Applications/",
        std::env::var("HOME").unwrap_or_default()
    );
    let mut sums: std::collections::BTreeMap<String, u64> = Default::default();
    for l in String::from_utf8_lossy(&out).lines() {
        let Some((rss, comm)) = l.trim_start().split_once(' ') else {
            continue;
        };
        let rest = comm
            .strip_prefix("/Applications/")
            .or_else(|| comm.strip_prefix(home_apps.as_str()));
        let Some(name) = rest.and_then(|r| r.split_once(".app/")).map(|(n, _)| n) else {
            continue;
        };
        if name.contains('/') || !valid_name(name) {
            continue;
        }
        *sums.entry(name.to_string()).or_default() += rss.trim().parse::<u64>().unwrap_or(0) * 1024;
    }
    let mut v: Vec<(String, u64)> = sums.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

#[derive(Debug, Default)]
pub struct Receipt {
    pub entries: Vec<Json>,
    pub quit_apps: Vec<String>,
    pub before_available: u64,
    pub after_available: u64,
    pub target: u64,
    pub pressure_normal_before: bool,
    pub pressure_normal_after: bool,
}

impl Receipt {
    pub fn to_json(&self) -> Json {
        json!({"target_available_bytes": self.target, "before_available_bytes": self.before_available,
               "after_available_bytes": self.after_available,
               "pressure_normal_before": self.pressure_normal_before, "pressure_normal_after": self.pressure_normal_after,
               "target_met": self.after_available >= self.target && self.pressure_normal_after,
               "actions": self.entries, "quit_apps": self.quit_apps,
               "method": "Normal Quit via Apple Events; apps can show save prompts; nothing is force-killed."})
    }
}

/// The next app to try: named apps first (in order), then the largest eligible one.
fn next_candidate(
    policy: &Policy,
    tried: &[String],
    running: &dyn Fn(&str) -> bool,
    by_memory: &dyn Fn() -> Vec<(String, u64)>,
) -> Option<(String, u64)> {
    if let Some(a) = policy
        .apps
        .iter()
        .find(|a| !tried.contains(a) && !is_protected(a, &policy.never) && running(a))
    {
        return Some((a.clone(), app_rss_bytes(a)));
    }
    if !policy.auto_largest {
        return None;
    }
    by_memory().into_iter().find(|(name, bytes)| {
        *bytes >= policy.min_app_bytes
            && !tried.contains(name)
            && !is_protected(name, &policy.never)
    })
}

/// Quit apps one at a time until available RAM reaches the target and macOS
/// memory pressure is normal (both are admission conditions). Does nothing when
/// they already hold. The OS calls are injected so tests run without touching real apps.
pub fn reclaim(
    policy: &Policy,
    available: &dyn Fn() -> u64,
    pressure_ok: &dyn Fn() -> bool,
    running: &dyn Fn(&str) -> bool,
    quit: &dyn Fn(&str) -> Result<(), String>,
) -> Receipt {
    reclaim_with(
        policy,
        available,
        pressure_ok,
        running,
        quit,
        &apps_by_memory,
    )
}

/// macOS reports pressure with a lag; after freeing memory, wait up to 10 s for it to settle.
fn settle(pressure_ok: &dyn Fn() -> bool, max: Duration) -> bool {
    let start = Instant::now();
    loop {
        if pressure_ok() {
            return true;
        }
        if start.elapsed() >= max {
            return false;
        }
        sleep(Duration::from_millis(500));
    }
}

pub fn reclaim_with(
    policy: &Policy,
    available: &dyn Fn() -> u64,
    pressure_ok: &dyn Fn() -> bool,
    running: &dyn Fn(&str) -> bool,
    quit: &dyn Fn(&str) -> Result<(), String>,
    by_memory: &dyn Fn() -> Vec<(String, u64)>,
) -> Receipt {
    let mut r = Receipt {
        target: policy.target_available_bytes,
        before_available: available(),
        ..Default::default()
    };
    r.after_available = r.before_available;
    r.pressure_normal_before = pressure_ok();
    r.pressure_normal_after = r.pressure_normal_before;
    let mut tried: Vec<String> = vec![];
    while (r.after_available < policy.target_available_bytes || !r.pressure_normal_after)
        && tried.len() < 12
    {
        let Some((app, rss)) = next_candidate(policy, &tried, running, by_memory) else {
            break;
        };
        tried.push(app.clone());
        let app = &app;
        let started = Instant::now();
        let outcome = match quit(app) {
            Err(e) => format!("quit_refused: {e}"),
            Ok(()) => {
                while running(app) && started.elapsed() < policy.quit_timeout {
                    sleep(Duration::from_millis(250));
                }
                if running(app) {
                    "still_running_after_timeout".into()
                } else {
                    "quit".into()
                }
            }
        };
        if outcome == "quit" {
            r.quit_apps.push(app.clone());
            sleep(Duration::from_millis(500)); // let the kernel return pages
            r.pressure_normal_after = settle(pressure_ok, Duration::from_secs(10));
        } else {
            r.pressure_normal_after = pressure_ok();
        }
        r.after_available = available();
        r.entries.push(json!({"app": app, "resident_bytes_before": rss, "outcome": outcome,
                              "seconds": started.elapsed().as_secs_f64(), "available_after_bytes": r.after_available}));
    }
    r
}

/// Quit one app, but only with the user's say-so: asleep or declined means nothing is closed.
/// Set LOCAL_AGENTS_RECLAIM_ASSUME_YES=1 (or "confirm": false in the config) to skip the dialog for unattended runs.
pub fn quit_app(name: &str) -> Result<(), String> {
    use crate::reclaim_consent as consent;
    if !valid_name(name) {
        return Err(format!("unsupported application name {name:?}"));
    }
    let root = crate::jobs::root();
    if let Some(until) = consent::sleeping_until(&root, consent::now_unix()) {
        return Err(format!(
            "memory cleanup is asleep for another {} minutes; nothing was closed",
            (until - consent::now_unix()).div_ceil(60)
        ));
    }
    let assume_yes = std::env::var("LOCAL_AGENTS_RECLAIM_ASSUME_YES")
        .ok()
        .as_deref()
        == Some("1");
    if consent::confirm_enabled(&root) && !assume_yes {
        match consent::ask(name, app_rss_bytes(name)) {
            consent::Choice::Quit => {}
            consent::Choice::Skip => return Err(format!("you chose to keep {name} open")),
            consent::Choice::SkipAll => {
                consent::sleep_for(&root, consent::SKIP_ALL_SECONDS)?;
                return Err("you chose to skip memory cleanup for 10 minutes".into());
            }
            consent::Choice::Sleep => {
                consent::sleep_for(&root, consent::SLEEP_HOURS * 3600)?;
                return Err(format!(
                    "memory cleanup put to sleep for {} hours",
                    consent::SLEEP_HOURS
                ));
            }
            consent::Choice::Off => {
                consent::set_enabled(&root, false)?;
                return Err(
                    "memory cleanup turned off; turn it back on with: local-agents-rs reclaim-on"
                        .into(),
                );
            }
        }
    }
    osascript(&format!("tell application \"{name}\" to quit")).map(|_| ())
}

/// Reopen apps without bringing them to the front.
pub fn restore(apps: &[String]) -> Vec<Json> {
    apps.iter()
        .map(|a| {
            let ok = Command::new("/usr/bin/open")
                .args(["-g", "-a", a])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            json!({"app": a, "reopened": ok})
        })
        .collect()
}
