use crate::install;
use crate::versions;
use anyhow::{anyhow, Context, Result};
use avm_plugin_api::{tool_dir, ToolVersionQuery};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

pub fn run(args: &[String]) -> ExitCode {
    let result = match args.split_first() {
        Some((cmd, _)) if cmd == "list" || cmd == "ls" => cmd_list(),
        Some((cmd, rest)) if cmd == "create" || cmd == "new" => cmd_create(rest),
        Some((cmd, rest)) if cmd == "delete" || cmd == "remove" || cmd == "rm" => cmd_delete(rest),
        Some((cmd, rest)) if cmd == "start" || cmd == "run" => cmd_start(rest),
        Some((cmd, _)) if cmd == "--help" || cmd == "-h" || cmd == "help" => {
            print_help();
            Ok(())
        }
        None => {
            print_help();
            Ok(())
        }
        Some((cmd, _)) => Err(anyhow!("unknown `avd` command '{cmd}' — run `avm android avd --help`")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!("Usage: avm android avd [COMMAND]");
    println!();
    println!("Commands:");
    println!("  list                 Show AVDs (Android Virtual Devices)");
    println!("  create [name]        Create a new AVD (picks the API/system image and device for you)");
    println!("  start <name>         Launch the emulator with the given AVD");
    println!("  delete <name>        Remove an AVD");
}

/// The android version avm-cli resolved for this invocation (local pin, else
/// global) — passed via env rather than re-implementing avm-core's pin
/// resolution inside the plugin. Its SDK is what every avd subcommand here
/// operates against, matching what `avm android use <version>` already
/// governs for everything else.
struct ActiveSdk {
    version: String,
    sdk: PathBuf,
    /// avm's SDK-aware wrappers (they export ANDROID_HOME/ANDROID_SDK_ROOT).
    bin: PathBuf,
    java_home: Option<PathBuf>,
}

fn active_sdk() -> Result<ActiveSdk> {
    let version = std::env::var("AVM_RESOLVED_VERSION").map_err(|_| {
        anyhow!("no android version selected — run `avm android use <version>` first")
    })?;
    let bin = tool_dir("android")?.join(&version).join("bin");
    if !bin.join("avdmanager").exists() {
        return Err(anyhow!(
            "android {version} isn't installed — run `avm android install {version}` first"
        ));
    }
    Ok(ActiveSdk {
        sdk: install::sdk_dir(&version)?,
        version,
        bin,
        java_home: install::require_jdk()?,
    })
}

fn tool_command(active: &ActiveSdk, binary: &str) -> Command {
    let mut cmd = Command::new(active.bin.join(binary));
    if let Some(java_home) = &active.java_home {
        cmd.env("JAVA_HOME", java_home);
    }
    cmd
}

fn cmd_list() -> Result<()> {
    let active = active_sdk()?;
    println!("AVDs (android {}):", active.version);
    let status = tool_command(&active, "avdmanager")
        .arg("list")
        .arg("avd")
        .status()
        .context("failed to run avdmanager")?;
    if !status.success() {
        return Err(anyhow!("avdmanager list avd failed: {status}"));
    }
    Ok(())
}

fn cmd_delete(args: &[String]) -> Result<()> {
    let name = args
        .first()
        .ok_or_else(|| anyhow!("usage: avm android avd delete <name>"))?;
    let active = active_sdk()?;
    let status = tool_command(&active, "avdmanager")
        .arg("delete")
        .arg("avd")
        .arg("-n")
        .arg(name)
        .status()
        .context("failed to run avdmanager")?;
    if !status.success() {
        return Err(anyhow!("avdmanager delete avd failed: {status}"));
    }
    println!("✓ Removed AVD '{name}'");
    Ok(())
}

fn cmd_start(args: &[String]) -> Result<()> {
    let name = args
        .first()
        .ok_or_else(|| anyhow!("usage: avm android avd start <name> [-- extra emulator args]"))?;
    let active = active_sdk()?;
    let extra = &args[1..];
    let status = tool_command(&active, "emulator")
        .arg("-avd")
        .arg(name)
        .args(extra)
        .status()
        .context("failed to run emulator")?;
    if !status.success() {
        return Err(anyhow!("emulator exited with {status}"));
    }
    Ok(())
}

/// Interactive wizard: pick an API level from the full list (same data
/// `avm android versions` shows), auto-install the matching system image if
/// it isn't already present in the active SDK, optionally pick a device
/// profile, then create the AVD. Every step accepts a flag instead
/// (`--api`, `--device`) for scripted/non-interactive use.
fn cmd_create(args: &[String]) -> Result<()> {
    let active = active_sdk()?;
    let (name, api, device) = parse_create_args(args)?;

    let name = match name {
        Some(name) => name,
        None => prompt("AVD name: ")?,
    };
    if name.trim().is_empty() {
        return Err(anyhow!("AVD name can't be empty"));
    }

    let api = match api {
        Some(api) => api,
        None => pick_api_level()?,
    };

    let java_home = active.java_home.as_deref();
    let listing = install::list_packages(&active.sdk, java_home)?;
    let system_image = install::resolve_system_image_id(&listing, &api)?;
    let package = install::sysimg_package(&system_image);
    // `system.img` specifically, not just the package dir: avdmanager refuses a
    // package that an interrupted install left without one ("contains no system images").
    let image = active
        .sdk
        .join("system-images")
        .join(format!("android-{system_image}"))
        .join("google_apis")
        .join(install::sysimg_abi())
        .join("system.img");
    if !image.exists() {
        println!("Installing {package} (closest match for API {api})...");
        install::accept_licenses(&active.sdk, java_home);
        install::install_system_image(&active.sdk, java_home, &system_image, &[])?;
    }

    let device = match device {
        Some(device) => Some(device),
        None => pick_device(&active)?,
    };

    println!("Creating AVD '{name}' ({package})...");
    let mut cmd = tool_command(&active, "avdmanager");
    cmd.arg("create").arg("avd").arg("-n").arg(&name).arg("-k").arg(&package);
    if let Some(device) = &device {
        cmd.arg("-d").arg(device);
    }
    cmd.stdin(Stdio::piped());
    let mut child = cmd.spawn().context("failed to run avdmanager")?;
    if let Some(mut stdin) = child.stdin.take() {
        // avdmanager asks "Do you wish to create a custom hardware profile
        // [no]" when a device (-d) wasn't given; "no" keeps the wizard
        // non-interactive from here on.
        let _ = stdin.write_all(b"no\n");
    }
    let status = child.wait().context("failed to run avdmanager")?;
    if !status.success() {
        return Err(anyhow!("avdmanager create avd failed: {status}"));
    }

    fix_avd_target_api(&name, &system_image);

    println!("✓ Created AVD '{name}'");
    println!("  Start it with: avm android avd start {name}");
    Ok(())
}

/// avdmanager (a Java tool) parses a system image's declared API level with
/// `Integer.parseInt`, which throws on a decimal string like `"37.0"` and
/// silently falls back to API level `0` — writing `target=android-0` into
/// the AVD's `.ini` file. The emulator later reads that same `target=` line
/// to decide whether the platform is new enough (`>= 21`) to enable HVF
/// hardware acceleration; API `0` fails that check, so it silently drops to
/// software (TCG) emulation — which then hard-fails on Apple Silicon, since
/// TCG's JIT needs simultaneously write+execute memory pages and macOS's
/// W^X enforcement there refuses that outright (`mprotect: Permission
/// denied`). None of this shows up as an error at `avd create` time — only
/// later, when the AVD actually tries to start. Rewriting `target=` with the
/// real major API version (parsed the same tolerant way `atoi` would, not
/// `Integer.parseInt`'s all-or-nothing rule) fixes it at the source, for
/// every AVD created against a decimal-versioned system image (`37.0`,
/// `36.1`, ...), not just this one.
fn fix_avd_target_api(name: &str, system_image: &str) {
    let Some(major) = system_image.split('.').next() else {
        return;
    };
    let Ok(home) = install::home_dir() else { return };
    let ini_path = home.join(".android").join("avd").join(format!("{name}.ini"));
    let Ok(contents) = std::fs::read_to_string(&ini_path) else {
        return;
    };

    let correct_target = format!("target=android-{major}");
    let mut changed = false;
    let patched: Vec<String> = contents
        .lines()
        .map(|line| {
            if line.starts_with("target=android-") && line != correct_target {
                changed = true;
                correct_target.clone()
            } else {
                line.to_string()
            }
        })
        .collect();

    if changed {
        let _ = std::fs::write(&ini_path, patched.join("\n") + "\n");
    }
}

fn parse_create_args(args: &[String]) -> Result<(Option<String>, Option<String>, Option<String>)> {
    let mut name = None;
    let mut api = None;
    let mut device = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--api" => {
                i += 1;
                api = Some(args.get(i).ok_or_else(|| anyhow!("--api requires a value"))?.clone());
            }
            "--device" => {
                i += 1;
                device = Some(args.get(i).ok_or_else(|| anyhow!("--device requires a value"))?.clone());
            }
            value if !value.starts_with('-') && name.is_none() => {
                name = Some(value.to_string());
            }
            other => return Err(anyhow!("unknown argument '{other}'")),
        }
        i += 1;
    }
    Ok((name, api, device))
}

fn pick_api_level() -> Result<String> {
    let versions = versions::available_versions(ToolVersionQuery::Recent)?;
    if versions.is_empty() {
        return Err(anyhow!("no Android API levels available"));
    }
    let labels: Vec<&str> = versions.iter().map(|v| v.label.as_str()).collect();
    let idx = pick("API level for the new AVD's system image:", &labels)?;
    Ok(versions[idx].version.clone())
}

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        return Err(anyhow!("cancelled"));
    }
    Ok(line.trim().to_string())
}

/// Numbered list on stdout; returns the 0-based index picked.
fn pick(title: &str, labels: &[&str]) -> Result<usize> {
    println!("{title}");
    for (i, label) in labels.iter().enumerate() {
        println!("  {:>3}) {label}", i + 1);
    }
    prompt(&format!("Choose 1-{}: ", labels.len()))?
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=labels.len()).contains(n))
        .map(|n| n - 1)
        .ok_or_else(|| anyhow!("invalid choice"))
}

struct DeviceProfile {
    id: String,
    label: String,
}

fn pick_device(active: &ActiveSdk) -> Result<Option<String>> {
    let output = tool_command(active, "avdmanager")
        .arg("list")
        .arg("device")
        .output()
        .context("failed to run avdmanager")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let devices = parse_device_list(&text);
    if devices.is_empty() {
        return Ok(None);
    }

    let mut labels = vec!["(skip — use avdmanager's default profile)"];
    labels.extend(devices.iter().map(|d| d.label.as_str()));
    Ok(match pick("Device profile:", &labels)? {
        0 => None,
        i => Some(devices[i - 1].id.clone()),
    })
}

/// Parses `avdmanager list device`'s block format:
/// ```text
/// id: 0 or "automotive_1024p_landscape"
///     Name: Automotive (1024p landscape)
///     OEM : Google
///     Tag : android-automotive-playstore
/// ---------
/// ```
fn parse_device_list(text: &str) -> Vec<DeviceProfile> {
    let mut devices = Vec::new();
    for block in text.split("---------") {
        let mut id = None;
        let mut display_name = None;
        for line in block.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("id:") {
                // `N or "device-id"` — the quoted form is what -d expects.
                if let Some(start) = rest.find('"') {
                    if let Some(end) = rest[start + 1..].find('"') {
                        id = Some(rest[start + 1..start + 1 + end].to_string());
                    }
                }
            } else if let Some(rest) = line.strip_prefix("Name:") {
                display_name = Some(rest.trim().to_string());
            }
        }
        if let (Some(id), Some(name)) = (id, display_name) {
            devices.push(DeviceProfile {
                label: format!("{name} ({id})"),
                id,
            });
        }
    }
    devices
}
