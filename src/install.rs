use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

// Installing pulls several GB (platform-tools, a platform image, build-tools,
// an emulator system image) — default timeouts are generous and every one is
// independently overridable for slow links.
const DOWNLOAD_TIMEOUT_MS: u64 = 180_000;
const UNZIP_TIMEOUT_MS: u64 = 60_000;
const SDKMANAGER_TIMEOUT_MS: u64 = 1_800_000;
const SDKMANAGER_LIST_TIMEOUT_MS: u64 = 60_000;

fn env_timeout_ms(var: &str, default_ms: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|secs| secs.saturating_mul(1000))
        .unwrap_or(default_ms)
}

fn run_with_timeout(mut cmd: Command, ms: u64, label: &str, env_var: &str) -> Result<()> {
    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {label}"))?;
    wait_with_timeout(child, ms, label, env_var)
}

fn wait_with_timeout(
    mut child: std::process::Child,
    ms: u64,
    label: &str,
    env_var: &str,
) -> Result<()> {
    let status = child
        .wait_timeout(Duration::from_millis(ms))
        .with_context(|| format!("failed while waiting for {label}"))?;
    let status = match status {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "{label} timed out after {}s — set {env_var}=<seconds> to extend",
                ms / 1000
            ));
        }
    };
    if !status.success() {
        return Err(anyhow!("{label} failed: {status}"));
    }
    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))
}

pub fn tools_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".avm").join("tools").join("android"))
}

pub fn sdk_dir(version: &str) -> Result<PathBuf> {
    Ok(tools_root()?.join(version).join("sdk"))
}

pub fn install_android(version: &str) -> Result<()> {
    let target = tools_root()?.join(version);
    let sdk = sdk_dir(version)?;

    // The platform package only exists once `install_packages` below has
    // actually succeeded — unlike the presence of `sdkmanager` itself, which
    // a previous *failed* attempt can already have left behind (cmdline-tools
    // extracts fine, then a package fails to resolve). Checking for the
    // platform, not just the tool that installs it, keeps a failed install
    // from being mistaken for a completed one on retry.
    if sdk.join("platforms").join(format!("android-{version}")).exists() {
        write_wrappers(&target, &sdk)?;
        return Ok(());
    }

    let java_home = require_jdk()?;

    let sdkmanager = sdk
        .join("cmdline-tools")
        .join("latest")
        .join("bin")
        .join("sdkmanager");
    if !sdkmanager.exists() {
        let tmp = home_dir()?.join(".avm").join("tmp").join("android").join(version);
        fs::create_dir_all(&tmp).context("failed to create android install temp dir")?;
        fs::create_dir_all(sdk.join("cmdline-tools")).context("failed to create android sdk dir")?;

        let zip_path = tmp.join("cmdline-tools.zip");
        download_cmdline_tools(&zip_path)?;
        extract_cmdline_tools(&zip_path, &tmp, &sdk)?;
        let _ = fs::remove_file(&zip_path);
    }

    accept_licenses(&sdkmanager, &sdk, java_home.as_deref())?;
    install_packages(&sdkmanager, &sdk, java_home.as_deref(), version)?;

    write_wrappers(&target, &sdk)?;
    Ok(())
}

fn download_cmdline_tools(destination: &Path) -> Result<()> {
    let url = cmdline_tools_url()?;
    let mut cmd = Command::new("curl");
    cmd.arg("-fL")
        .arg("--connect-timeout")
        .arg("10")
        .arg("--max-time")
        .arg((env_timeout_ms("AVM_ANDROID_CURL_TIMEOUT", DOWNLOAD_TIMEOUT_MS) / 1000).to_string())
        .arg(&url)
        .arg("-o")
        .arg(destination)
        .stdout(Stdio::null());
    run_with_timeout(
        cmd,
        env_timeout_ms("AVM_ANDROID_CURL_TIMEOUT", DOWNLOAD_TIMEOUT_MS),
        "Android command-line tools download",
        "AVM_ANDROID_CURL_TIMEOUT",
    )
    .with_context(|| format!("failed to download {url}"))
}

fn extract_cmdline_tools(zip_path: &Path, tmp: &Path, sdk: &Path) -> Result<()> {
    let extracted = tmp.join("cmdline-tools");
    if extracted.exists() {
        fs::remove_dir_all(&extracted).context("failed to clean previous extraction")?;
    }
    let mut cmd = Command::new("unzip");
    cmd.arg("-q").arg(zip_path).arg("-d").arg(tmp);
    run_with_timeout(
        cmd,
        env_timeout_ms("AVM_ANDROID_UNZIP_TIMEOUT", UNZIP_TIMEOUT_MS),
        "Android command-line tools extraction",
        "AVM_ANDROID_UNZIP_TIMEOUT",
    )?;

    let dest = sdk.join("cmdline-tools").join("latest");
    if dest.exists() {
        fs::remove_dir_all(&dest).context("failed to replace existing cmdline-tools")?;
    }
    fs::rename(&extracted, &dest).context("failed to move cmdline-tools into place")?;
    Ok(())
}

fn accept_licenses(sdkmanager: &Path, sdk: &Path, java_home: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new(sdkmanager);
    cmd.arg(format!("--sdk_root={}", sdk.display())).arg("--licenses");
    if let Some(java_home) = java_home {
        cmd.env("JAVA_HOME", java_home);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().context("failed to spawn sdkmanager --licenses")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        // sdkmanager prompts once per license; "y\n" repeated satisfies all of them.
        let _ = stdin.write_all("y\n".repeat(64).as_bytes());
    }
    // Best-effort: some sdkmanager versions exit non-zero here even after
    // accepting everything it actually needed; install_packages below is the
    // real correctness check.
    let _ = wait_with_timeout(
        child,
        env_timeout_ms("AVM_ANDROID_SDKMANAGER_TIMEOUT", SDKMANAGER_TIMEOUT_MS),
        "sdkmanager --licenses",
        "AVM_ANDROID_SDKMANAGER_TIMEOUT",
    );
    Ok(())
}

fn install_packages(sdkmanager: &Path, sdk: &Path, java_home: Option<&Path>, api: &str) -> Result<()> {
    // Neither build-tools nor system-image package IDs follow the platform
    // API string, and preview/beta platform levels (like "37.2") routinely
    // have no matching package at all yet — one repository listing backs
    // both resolvers below so they can pick a real, installable package
    // instead of guessing a name that may not exist.
    let listing = list_packages(sdkmanager, sdk, java_home)?;
    let build_tools = resolve_build_tools_version(&listing, api)?;
    let system_image = resolve_system_image_id(&listing, api)?;

    let mut cmd = Command::new(sdkmanager);
    cmd.arg(format!("--sdk_root={}", sdk.display()))
        .arg("platform-tools")
        .arg(format!("platforms;android-{api}"))
        .arg(format!("build-tools;{build_tools}"))
        .arg("emulator")
        .arg(format!(
            "system-images;android-{system_image};google_apis;{}",
            sysimg_abi()
        ))
        .stdout(Stdio::null());
    if let Some(java_home) = java_home {
        cmd.env("JAVA_HOME", java_home);
    }
    run_with_timeout(
        cmd,
        env_timeout_ms("AVM_ANDROID_SDKMANAGER_TIMEOUT", SDKMANAGER_TIMEOUT_MS),
        "sdkmanager package install",
        "AVM_ANDROID_SDKMANAGER_TIMEOUT",
    )
    .with_context(|| format!("failed to install Android API {api} packages"))
}

/// Build-tools packages are versioned independently of platform API levels
/// (`build-tools;37.0.0` exists but `build-tools;37.2.0.0` never will, even
/// for platform `android-37.2`) — so the right package can't be guessed from
/// the API string. Pick the highest stable build-tools release matching the
/// platform's major version, falling back to the highest stable release
/// overall if none matches yet.
fn resolve_build_tools_version(listing: &str, api: &str) -> Result<String> {
    if let Ok(version) = std::env::var("ANDROID_BUILD_TOOLS_VERSION") {
        return Ok(version);
    }

    let api_major = api.split('.').next().unwrap_or(api);

    let mut same_major = Vec::new();
    let mut all = Vec::new();
    for line in listing.lines() {
        let Some(rest) = line.trim().strip_prefix("build-tools;") else {
            continue;
        };
        let version_str = rest.split_whitespace().next().unwrap_or("");
        // Stable releases only — "-rc1"/"-preview" builds aren't what a
        // plain `avm android install <version>` should silently pull in.
        if version_str.contains('-') {
            continue;
        }
        let Some(parsed) = parse_semver(version_str) else {
            continue;
        };
        if version_str.split('.').next() == Some(api_major) {
            same_major.push(parsed);
        }
        all.push(parsed);
    }

    same_major
        .into_iter()
        .max()
        .or_else(|| all.into_iter().max())
        .map(|(major, minor, patch)| format!("{major}.{minor}.{patch}"))
        .ok_or_else(|| anyhow!("no build-tools package found in the Android SDK repository"))
}

/// System-image IDs are even less predictable than build-tools: Google
/// publishes some as a bare major (`android-36`) and others with an explicit
/// `.0` (`android-37.0`), and a preview/beta platform level frequently has no
/// system image at all yet. Reformatting a guessed ID (e.g. always adding
/// `.0`) would just trade one wrong guess for another, so the winning
/// candidate's *exact* published id string is returned as-is. Prefers the
/// highest same-major image at or below the requested level (the emulator
/// doesn't need a newer image than the platform being targeted), then the
/// closest one above, then the highest available for any major as a last
/// resort.
fn resolve_system_image_id(listing: &str, api: &str) -> Result<String> {
    if let Ok(id) = std::env::var("ANDROID_SYSTEM_IMAGE_API") {
        return Ok(id);
    }

    let abi = sysimg_abi();
    let suffix = format!(";google_apis;{abi}");
    let target = parse_two_part(api)
        .ok_or_else(|| anyhow!("cannot parse Android API level '{api}'"))?;

    let mut same_major_le: Vec<((u64, u64), &str)> = Vec::new();
    let mut same_major_gt: Vec<((u64, u64), &str)> = Vec::new();
    let mut any: Vec<((u64, u64), &str)> = Vec::new();

    for line in listing.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("system-images;android-") else {
            continue;
        };
        let Some(id_end) = rest.find(';') else { continue };
        let id = &rest[..id_end];
        let after_id = &rest[id_end..];
        if !after_id.starts_with(&suffix) {
            continue;
        }
        // Guard against a longer variant sharing this prefix
        // (`google_apis_playstore`) — the real column ends in whitespace.
        if !after_id[suffix.len()..].starts_with(char::is_whitespace) {
            continue;
        }
        // Skip extension/preview channel ids ("36-ext18", "37-rc1").
        if id.contains('-') {
            continue;
        }
        let Some(parsed) = parse_two_part(id) else {
            continue;
        };

        if parsed.0 == target.0 {
            if parsed <= target {
                same_major_le.push((parsed, id));
            } else {
                same_major_gt.push((parsed, id));
            }
        }
        any.push((parsed, id));
    }

    same_major_le
        .into_iter()
        .max_by_key(|(key, _)| *key)
        .or_else(|| same_major_gt.into_iter().min_by_key(|(key, _)| *key))
        .or_else(|| any.into_iter().max_by_key(|(key, _)| *key))
        .map(|(_, id)| id.to_string())
        .ok_or_else(|| anyhow!("no {abi} system image found for Android API {api} in the SDK repository"))
}

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

fn parse_two_part(s: &str) -> Option<(u64, u64)> {
    let mut parts = s.splitn(2, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

fn list_packages(sdkmanager: &Path, sdk: &Path, java_home: Option<&Path>) -> Result<String> {
    let mut cmd = Command::new(sdkmanager);
    cmd.arg(format!("--sdk_root={}", sdk.display())).arg("--list");
    if let Some(java_home) = java_home {
        cmd.env("JAVA_HOME", java_home);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("failed to spawn sdkmanager --list")?;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });

    let status = child
        .wait_timeout(Duration::from_millis(env_timeout_ms(
            "AVM_ANDROID_SDKMANAGER_TIMEOUT",
            SDKMANAGER_LIST_TIMEOUT_MS,
        )))
        .context("failed while waiting for sdkmanager --list")?;
    let output = reader.join().unwrap_or_default();

    match status {
        Some(status) if status.success() => Ok(output),
        Some(status) => Err(anyhow!("sdkmanager --list failed: {status}")),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(anyhow!("sdkmanager --list timed out"))
        }
    }
}

/// Write tiny wrapper scripts into `<version>/bin/` so avm's generic
/// `~/.avm/tools/<tool>/<version>/bin/<binary>` shim lookup finds SDK-aware
/// adb/sdkmanager/etc. without every shim invocation needing to know about
/// `sdk/cmdline-tools/latest/...` layout.
fn write_wrappers(target: &Path, sdk: &Path) -> Result<()> {
    let bin = target.join("bin");
    fs::create_dir_all(&bin).context("failed to create android bin dir")?;
    wrapper(&bin, sdk, "adb", &sdk.join("platform-tools").join("adb"))?;
    let cmdline_bin = sdk.join("cmdline-tools").join("latest").join("bin");
    wrapper(&bin, sdk, "sdkmanager", &cmdline_bin.join("sdkmanager"))?;
    wrapper(&bin, sdk, "avdmanager", &cmdline_bin.join("avdmanager"))?;
    wrapper(&bin, sdk, "emulator", &sdk.join("emulator").join("emulator"))?;
    // `android` satisfies avm's is_installed marker convention and is a
    // reasonable default entry point.
    wrapper(&bin, sdk, "android", &cmdline_bin.join("sdkmanager"))?;
    Ok(())
}

fn wrapper(bin: &Path, sdk: &Path, name: &str, target: &Path) -> Result<()> {
    let dest = bin.join(name);
    let contents = format!(
        "#!/usr/bin/env sh\nexport ANDROID_HOME=\"{sdk}\"\nexport ANDROID_SDK_ROOT=\"{sdk}\"\nexec \"{target}\" \"$@\"\n",
        sdk = sdk.display(),
        target = target.display(),
    );
    fs::write(&dest, contents).with_context(|| format!("failed to write wrapper {}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms)?;
    }
    Ok(())
}

fn cmdline_tools_url() -> Result<String> {
    let build = std::env::var("ANDROID_CMDLINE_TOOLS_BUILD").unwrap_or_else(|_| "11076708".to_string());
    let host = match std::env::consts::OS {
        "macos" => "mac",
        "linux" => "linux",
        other => return Err(anyhow!("unsupported OS for Android cmdline-tools: {other}")),
    };
    Ok(format!(
        "https://dl.google.com/android/repository/commandlinetools-{host}-{build}_latest.zip"
    ))
}

fn sysimg_abi() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64-v8a",
        _ => "x86_64",
    }
}

/// A JDK is required to run `sdkmanager`. A system `java` already on PATH is
/// used as-is (`Ok(None)` — no JAVA_HOME override needed). Otherwise, an
/// avm-managed JDK is used instead (preferring the pinned version from
/// `~/.avm.json`'s `tools.java`, else the first one found), and its home
/// returned so the caller can point the child process at it.
fn require_jdk() -> Result<Option<PathBuf>> {
    let system_java_works = Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if system_java_works {
        return Ok(None);
    }

    find_avm_managed_jdk()
        .map(Some)
        .ok_or_else(|| anyhow!(
            "a JDK is required to install Android SDK components (sdkmanager needs Java) — install one, or run `avm java use <version>`"
        ))
}

fn find_avm_managed_jdk() -> Option<PathBuf> {
    let home = home_dir().ok()?;
    let java_tools = home.join(".avm").join("tools").join("java");

    if let Some(version) = pinned_java_version(&home) {
        let candidate = java_tools.join(&version);
        if candidate.join("bin").join("java").exists() {
            return Some(candidate);
        }
    }

    let entries = fs::read_dir(&java_tools).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("bin").join("java").exists() {
            return Some(path);
        }
    }
    None
}

fn pinned_java_version(home: &Path) -> Option<String> {
    let raw = fs::read_to_string(home.join(".avm.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parsed
        .get("tools")?
        .get("java")?
        .as_str()
        .map(str::to_string)
}
