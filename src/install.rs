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
    if sdk.join("cmdline-tools").join("latest").join("bin").join("sdkmanager").exists() {
        write_wrappers(&target, &sdk)?;
        return Ok(());
    }

    let java_home = require_jdk()?;

    let tmp = home_dir()?.join(".avm").join("tmp").join("android").join(version);
    fs::create_dir_all(&tmp).context("failed to create android install temp dir")?;
    fs::create_dir_all(sdk.join("cmdline-tools")).context("failed to create android sdk dir")?;

    let zip_path = tmp.join("cmdline-tools.zip");
    download_cmdline_tools(&zip_path)?;
    extract_cmdline_tools(&zip_path, &tmp, &sdk)?;
    let _ = fs::remove_file(&zip_path);

    let sdkmanager = sdk
        .join("cmdline-tools")
        .join("latest")
        .join("bin")
        .join("sdkmanager");
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
    let build_tools = std::env::var("ANDROID_BUILD_TOOLS_VERSION")
        .unwrap_or_else(|_| format!("{api}.0.0"));

    let mut cmd = Command::new(sdkmanager);
    cmd.arg(format!("--sdk_root={}", sdk.display()))
        .arg("platform-tools")
        .arg(format!("platforms;android-{api}"))
        .arg(format!("build-tools;{build_tools}"))
        .arg("emulator")
        .arg(format!(
            "system-images;android-{api};google_apis;{}",
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
