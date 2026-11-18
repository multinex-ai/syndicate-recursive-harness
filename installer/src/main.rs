use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const USAGE: &str =
    "syndicate-installer <plan|doctor|install|models|uninstall> [--root PATH] [--models core|creative|all|none]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelSet {
    None,
    Core,
    Creative,
    All,
}

impl ModelSet {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "core" => Ok(Self::Core),
            "creative" => Ok(Self::Creative),
            "all" => Ok(Self::All),
            _ => Err("--models must be core, creative, all, or none".into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Core => "core",
            Self::Creative => "creative",
            Self::All => "all",
        }
    }
}

struct Options {
    command: String,
    root: PathBuf,
    models: ModelSet,
}

fn parse_args() -> Result<Options, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let default_root = env::current_dir().map_err(|error| error.to_string())?;
    parse_args_from(&args, default_root)
}

fn parse_args_from(args: &[String], default_root: PathBuf) -> Result<Options, String> {
    let command = args.first().cloned().ok_or(USAGE)?;
    let mut root = default_root;
    let mut models = ModelSet::Core;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => {
                index += 1;
                root = PathBuf::from(args.get(index).ok_or("--root requires a path")?);
            }
            "--models" => {
                index += 1;
                models = ModelSet::parse(args.get(index).ok_or("--models requires a value")?)?;
            }
            value => return Err(format!("unknown option: {value}\n{USAGE}")),
        }
        index += 1;
    }
    Ok(Options {
        command,
        root,
        models,
    })
}

fn available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn succeeds(root: &Path, command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .current_dir(root)
        .status()
        .is_ok_and(|status| status.success())
}

fn run(root: &Path, command: &str, args: &[&str]) -> Result<(), String> {
    let rendered = std::iter::once(command)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    println!("→ {rendered}");
    let status = Command::new(command)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|error| format!("could not start {command}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{rendered} exited with {status}"))
    }
}

fn run_with_retries(
    root: &Path,
    command: &str,
    args: &[&str],
    max_attempts: usize,
) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=max_attempts {
        match run(root, command, args) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = error;
                if attempt < max_attempts {
                    let delay = Duration::from_secs(attempt as u64 * 2);
                    eprintln!(
                        "network-backed install step failed (attempt {attempt}/{max_attempts}); retrying in {}s",
                        delay.as_secs()
                    );
                    thread::sleep(delay);
                }
            }
        }
    }
    Err(format!(
        "install step failed after {max_attempts} bounded attempts: {last_error}"
    ))
}

fn platform() -> String {
    format!("{}-{}", env::consts::OS, env::consts::ARCH)
}

fn supported_runtime() -> bool {
    env::consts::OS == "macos" && env::consts::ARCH == "aarch64"
}

fn print_plan(options: &Options) {
    println!("Local Syndicate Harness² install plan");
    println!("  platform: {}", platform());
    println!("  root: {}", options.root.display());
    println!("  models: {}", options.models.as_str());
    println!("  1. verify Homebrew, Node, Cargo, uv, llama.cpp");
    println!("  2. build the Rust control plane and installer");
    println!("  3. create the isolated Python/MLX runtime");
    println!("  4. install the pinned Codex CLI and isolated agents-chat config");
    println!("  5. download selected pinned model files and verify SHA-256");
    println!("  6. run the doctor; no model process remains resident");
    if !supported_runtime() {
        println!("  note: this installer compiles here, but the Metal/MLX inference runtime is currently qualified only on Apple Silicon macOS");
    }
}

fn doctor(options: &Options) -> bool {
    let commands = ["node", "cargo", "uv", "llama"];
    let mut healthy = true;
    println!("Harness doctor ({})", platform());
    for command in commands {
        let present = available(command);
        healthy &= present;
        println!(
            "  {:<8} {}",
            command,
            if present { "ready" } else { "missing" }
        );
    }
    let control = options
        .root
        .join("target/release/local-agents-rs")
        .is_file();
    let chat = options.root.join("shared/runtime/bin/codex").is_file()
        && options.root.join("shared/chat/home/config.toml").is_file();
    let forge_ui = options
        .root
        .join("shared/forge-console/index.html")
        .is_file()
        && options.root.join("shared/forge-console/app.js").is_file();
    println!("  control  {}", if control { "ready" } else { "not built" });
    println!(
        "  chat     {}",
        if chat { "ready" } else { "not installed" }
    );
    println!("  forge    {}", if forge_ui { "ready" } else { "missing" });
    healthy &= control && chat && forge_ui;
    if matches!(options.models, ModelSet::Creative | ModelSet::All) {
        let ffmpeg = available("ffmpeg");
        println!("  ffmpeg   {}", if ffmpeg { "ready" } else { "missing" });
        healthy &= ffmpeg;
    }
    if !matches!(options.models, ModelSet::None) {
        let model_integrity = succeeds(
            &options.root,
            "node",
            &[
                "scripts/models/verify.mjs",
                "--set",
                options.models.as_str(),
            ],
        );
        println!(
            "  models   {}",
            if model_integrity {
                "hash verified"
            } else {
                "missing or invalid"
            }
        );
        healthy &= model_integrity;
    }
    println!("  image    unavailable until the FLUX encoder assembly is licensed and verified");
    println!("  lock     {} (contract v1)", shared_lock_path().display());
    println!(
        "  result   {}",
        if healthy { "READY" } else { "NEEDS ATTENTION" }
    );
    healthy
}

fn shared_lock_path() -> PathBuf {
    let configured = env::var("SYNDICATE_HEAVY_LOCK").ok();
    let state = env::var("XDG_STATE_HOME").ok();
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    resolve_lock_path(configured.as_deref(), state.as_deref(), home.as_deref())
}

fn resolve_lock_path(
    configured: Option<&str>,
    state: Option<&str>,
    home: Option<&Path>,
) -> PathBuf {
    let absolute = |raw: &str| {
        let candidate = PathBuf::from(raw.trim());
        (!raw.trim().is_empty() && candidate.is_absolute()).then_some(candidate)
    };
    if let Some(candidate) = configured.and_then(absolute) {
        return candidate;
    }
    if let Some(candidate) = state.and_then(absolute) {
        return candidate.join("local-syndicate-harness/heavy-model.lock");
    }
    home.map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join(".local/state/local-syndicate-harness/heavy-model.lock")
}

fn brew_install(root: &Path, formula: &str, command: &str) -> Result<(), String> {
    if available(command) {
        return Ok(());
    }
    if !available("brew") {
        return Err(format!("{command} is missing and Homebrew is unavailable; install Homebrew, then rerun the installer"));
    }
    run(root, "brew", &["install", formula])
}

fn install(options: &Options) -> Result<(), String> {
    if !supported_runtime() {
        return Err(format!(
            "full local inference is currently qualified only for macOS Apple Silicon; {} can run plan/doctor and compile the installer",
            platform()
        ));
    }
    if !options.root.join("Cargo.toml").is_file() {
        return Err(format!(
            "{} is not a harness checkout",
            options.root.display()
        ));
    }
    brew_install(&options.root, "node", "node")?;
    brew_install(&options.root, "uv", "uv")?;
    brew_install(&options.root, "llama.cpp", "llama")?;
    if matches!(options.models, ModelSet::Creative | ModelSet::All) {
        brew_install(&options.root, "ffmpeg", "ffmpeg")?;
    }
    if !available("cargo") {
        return Err("Cargo is required to build from source. Install Rust from https://rustup.rs and rerun.".into());
    }
    run(
        &options.root,
        "cargo",
        &["build", "--workspace", "--release"],
    )?;
    let runtime = options.root.join("shared/runtime/venv");
    if !runtime.join("bin/python").is_file() {
        run(
            &options.root,
            "uv",
            &["venv", "shared/runtime/venv", "--python", "3.12"],
        )?;
    }
    run_with_retries(
        &options.root,
        "uv",
        &[
            "pip",
            "sync",
            "shared/config/requirements.lock.txt",
            "--python",
            "shared/runtime/venv/bin/python",
        ],
        3,
    )?;
    run(&options.root, "node", &["shared/chat/install-chat.mjs"])?;
    install_models(options)?;
    if doctor(options) {
        println!(
            "\nNext step: export PATH=\"{}/bin:{}/target/release:$PATH\" && agents-chat",
            options.root.display(),
            options.root.display()
        );
        Ok(())
    } else {
        Err("installation finished, but the doctor found an incomplete component".into())
    }
}

fn install_models(options: &Options) -> Result<(), String> {
    if matches!(options.models, ModelSet::None) {
        return Ok(());
    }
    run(
        &options.root,
        "node",
        &[
            "scripts/models/download.mjs",
            "--set",
            options.models.as_str(),
        ],
    )
}

fn uninstall(options: &Options) -> Result<(), String> {
    for rel in ["shared/runtime", "shared/chat/home"] {
        let target = options.root.join(rel);
        if target.exists() {
            std::fs::remove_dir_all(&target)
                .map_err(|e| format!("could not remove {}: {e}", target.display()))?;
            println!("✓ removed {}", target.display());
        }
    }
    println!("Uninstalled isolated runtime and chat config (source code and downloaded weights in shared/models/ were preserved).");
    Ok(())
}

fn main() {
    let result = (|| -> Result<(), String> {
        let options = parse_args()?;
        match options.command.as_str() {
            "plan" => {
                print_plan(&options);
                Ok(())
            }
            "doctor" => {
                print_plan(&options);
                if doctor(&options) {
                    Ok(())
                } else {
                    Err("doctor found missing components".into())
                }
            }
            "install" => install(&options),
            "models" => install_models(&options),
            "uninstall" => uninstall(&options),
            _ => Err(USAGE.into()),
        }
    })();
    if let Err(error) = result {
        eprintln!("installer error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_trims_absolute_overrides_and_rejects_relative_ones() {
        let home = Path::new("/tmp/example-home");
        assert_eq!(
            resolve_lock_path(Some("  /tmp/shared-heavy.lock  "), None, Some(home)),
            PathBuf::from("/tmp/shared-heavy.lock")
        );
        assert_eq!(
            resolve_lock_path(Some("relative.lock"), Some(" /tmp/state "), Some(home)),
            PathBuf::from("/tmp/state/local-syndicate-harness/heavy-model.lock")
        );
        assert_eq!(
            resolve_lock_path(None, None, Some(home)),
            PathBuf::from(
                "/tmp/example-home/.local/state/local-syndicate-harness/heavy-model.lock"
            )
        );
    }

    #[test]
    fn model_set_roundtrips_and_rejects_invalid_names() {
        for (name, expected) in [
            ("none", ModelSet::None),
            ("core", ModelSet::Core),
            ("creative", ModelSet::Creative),
            ("all", ModelSet::All),
        ] {
            let parsed = ModelSet::parse(name).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_str(), name);
        }
        assert!(ModelSet::parse("invalid").is_err());
    }

    #[test]
    fn parse_args_accepts_subcommands_and_flags() {
        let args = vec![
            "models".to_string(),
            "--root".to_string(),
            "/tmp/harness".to_string(),
            "--models".to_string(),
            "all".to_string(),
        ];
        let opts = parse_args_from(&args, PathBuf::from("/default")).unwrap();
        assert_eq!(opts.command, "models");
        assert_eq!(opts.root, PathBuf::from("/tmp/harness"));
        assert_eq!(opts.models, ModelSet::All);
    }

    #[test]
    fn parse_args_rejects_missing_or_unknown_flags() {
        assert!(parse_args_from(&[], PathBuf::from("/default")).is_err());
        let bad = vec!["plan".to_string(), "--unknown".to_string()];
        assert!(parse_args_from(&bad, PathBuf::from("/default")).is_err());
    }
}
