use crate::versions::nums;
use anyhow::{anyhow, Context, Result};
use avm_plugin_api::{env_timeout_ms, list_installed, run_timed, tool_dir, wait_deadline};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// Installing pulls several GB (platform-tools, a platform image, build-tools,
// an emulator system image) — default timeouts are generous and every one is
// independently overridable for slow links.
const DOWNLOAD_TIMEOUT_MS: u64 = 180_000;
const UNZIP_TIMEOUT_MS: u64 = 60_000;
const SDKMANAGER_TIMEOUT_MS: u64 = 1_800_000;
const SDKMANAGER_LIST_TIMEOUT_MS: u64 = 60_000;
const SDKMANAGER_ENV: &str = "AVM_ANDROID_SDKMANAGER_TIMEOUT";

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))
}

pub fn sdk_dir(version: &str) -> Result<PathBuf> {
    Ok(tool_dir("android")?.join(version).join("sdk"))
}

pub fn install_android(version: &str) -> Result<()> {
    let target = tool_dir("android")?.join(version);
    let sdk = sdk_dir(version)?;

    // Both the platform package AND a real (non-truncated) system image are
    // required before this counts as done: an interrupted multi-package
    // sdkmanager run can land `platforms/` while `system-images/` only has a
    // package.xml, which avdmanager then refuses to create an AVD from.
    if sdk.join("platforms").join(format!("android-{version}")).exists() && has_complete_system_image(&sdk) {
        return write_wrappers(&target, &sdk);
    }

    let java_home = require_jdk()?;

    if !sdk.join("cmdline-tools").join("latest").join("bin").join("sdkmanager").exists() {
        // Staged inside the tools dir: never counts as installed, same fs for rename.
        let tmp = tool_dir("android")?.join(format!(".tmp-{version}"));
        fs::create_dir_all(&tmp).context("failed to create android install temp dir")?;
        fs::create_dir_all(sdk.join("cmdline-tools")).context("failed to create android sdk dir")?;
        let zip_path = tmp.join("cmdline-tools.zip");
        download_cmdline_tools(&zip_path)?;
        extract_cmdline_tools(&zip_path, &tmp, &sdk)?;
        let _ = fs::remove_dir_all(&tmp);
    }

    accept_licenses(&sdk, java_home.as_deref());
    // Neither build-tools nor system-image package IDs follow the platform
    // API string (and preview levels like "37.2" often have none yet), so one
    // repository listing backs both resolvers to pick real packages.
    let listing = list_packages(&sdk, java_home.as_deref())?;
    let build_tools = resolve_build_tools_version(&listing, version)?;
    let system_image = resolve_system_image_id(&listing, version)?;
    install_system_image(
        &sdk,
        java_home.as_deref(),
        &system_image,
        &[
            "platform-tools".to_string(),
            format!("platforms;android-{version}"),
            format!("build-tools;{build_tools}"),
        ],
    )
    .with_context(|| format!("failed to install Android API {version} packages"))?;

    write_wrappers(&target, &sdk)
}

/// True if any `sdk/system-images/*/*/*/system.img` exists — a directory alone
/// isn't enough, since an interrupted install can leave only `package.xml`.
fn has_complete_system_image(sdk: &Path) -> bool {
    let subdirs = |p: PathBuf| fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
    subdirs(sdk.join("system-images"))
        .flat_map(subdirs)
        .flat_map(subdirs)
        .any(|abi| abi.join("system.img").exists())
}

fn download_cmdline_tools(destination: &Path) -> Result<()> {
    let url = cmdline_tools_url()?;
    let max_secs = env_timeout_ms("AVM_ANDROID_CURL_TIMEOUT", DOWNLOAD_TIMEOUT_MS) / 1000;
    let mut cmd = Command::new("curl");
    cmd.args(["-fL", "--connect-timeout", "10", "--max-time", &max_secs.to_string(), &url, "-o"])
        .arg(destination)
        .stdout(Stdio::null());
    run_timed(cmd, DOWNLOAD_TIMEOUT_MS, "Android command-line tools download", "AVM_ANDROID_CURL_TIMEOUT")
        .with_context(|| format!("failed to download {url}"))
}

fn extract_cmdline_tools(zip_path: &Path, tmp: &Path, sdk: &Path) -> Result<()> {
    let extracted = tmp.join("cmdline-tools");
    if extracted.exists() {
        fs::remove_dir_all(&extracted).context("failed to clean previous extraction")?;
    }
    let mut cmd = Command::new("unzip");
    cmd.arg("-q").arg(zip_path).arg("-d").arg(tmp);
    run_timed(cmd, UNZIP_TIMEOUT_MS, "Android command-line tools extraction", "AVM_ANDROID_UNZIP_TIMEOUT")?;

    let dest = sdk.join("cmdline-tools").join("latest");
    if dest.exists() {
        fs::remove_dir_all(&dest).context("failed to replace existing cmdline-tools")?;
    }
    fs::rename(&extracted, &dest).context("failed to move cmdline-tools into place")
}

/// `sdkmanager --sdk_root=<sdk>`, pointed at `java_home` when one is needed.
fn sdkmanager(sdk: &Path, java_home: Option<&Path>) -> Command {
    let mut cmd = Command::new(sdk.join("cmdline-tools").join("latest").join("bin").join("sdkmanager"));
    cmd.arg(format!("--sdk_root={}", sdk.display()));
    if let Some(java_home) = java_home {
        cmd.env("JAVA_HOME", java_home);
    }
    cmd
}

/// Best-effort: some sdkmanager versions exit non-zero here even after
/// accepting everything needed; the package install is the real check.
pub fn accept_licenses(sdk: &Path, java_home: Option<&Path>) {
    let mut cmd = sdkmanager(sdk, java_home);
    cmd.arg("--licenses").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else { return };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        // sdkmanager prompts once per license; "y\n" repeated satisfies all of them.
        let _ = stdin.write_all("y\n".repeat(64).as_bytes());
    }
    let _ = wait_deadline(&mut child, env_timeout_ms(SDKMANAGER_ENV, SDKMANAGER_TIMEOUT_MS));
}

pub fn sysimg_package(system_image: &str) -> String {
    format!("system-images;android-{system_image};google_apis;{}", sysimg_abi())
}

/// Install `extra` packages plus the emulator and `system_image`. sdkmanager
/// tracks "installed" by package.xml alone, so an interrupted earlier install
/// that left package.xml without system.img would make it a silent no-op —
/// clear such a partial image first to force a real re-fetch.
pub fn install_system_image(sdk: &Path, java_home: Option<&Path>, system_image: &str, extra: &[String]) -> Result<()> {
    let package = sysimg_package(system_image);
    let image_dir = sdk
        .join("system-images")
        .join(format!("android-{system_image}"))
        .join("google_apis")
        .join(sysimg_abi());
    if image_dir.exists() && !image_dir.join("system.img").exists() {
        println!("Found an incomplete {package} install — removing and re-fetching...");
        fs::remove_dir_all(&image_dir)
            .with_context(|| format!("failed to remove incomplete {}", image_dir.display()))?;
    }
    let mut cmd = sdkmanager(sdk, java_home);
    cmd.args(extra).arg("emulator").arg(&package).stdout(Stdio::null());
    run_timed(cmd, SDKMANAGER_TIMEOUT_MS, "sdkmanager package install", SDKMANAGER_ENV)
}

/// Build-tools packages are versioned independently of platform API levels
/// (`build-tools;37.0.0` exists but `build-tools;37.2.0.0` never will) — pick
/// the highest stable release matching the platform's major, else the
/// highest stable release overall.
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
        // Stable releases only ("-rc1"/"-preview" fail to parse).
        let Some(parsed) = nums(version_str, 3) else {
            continue;
        };
        if version_str.split('.').next() == Some(api_major) {
            same_major.push(parsed.clone());
        }
        all.push(parsed);
    }

    same_major
        .into_iter()
        .max()
        .or_else(|| all.into_iter().max())
        .map(|v| format!("{}.{}.{}", v[0], v[1], v[2]))
        .ok_or_else(|| anyhow!("no build-tools package found in the Android SDK repository"))
}

/// System-image IDs are published as either a bare major (`android-36`) or
/// with an explicit `.0` (`android-37.0`), and preview levels often have none,
/// so the winning candidate's *exact* id is returned. Prefers the highest
/// same-major image at or below the requested level, then the closest one
/// above, then the highest available for any major.
pub fn resolve_system_image_id(listing: &str, api: &str) -> Result<String> {
    if let Ok(id) = std::env::var("ANDROID_SYSTEM_IMAGE_API") {
        return Ok(id);
    }

    let abi = sysimg_abi();
    let suffix = format!(";google_apis;{abi}");
    let target = nums(api, 2).ok_or_else(|| anyhow!("cannot parse Android API level '{api}'"))?;

    let mut same_major_le = Vec::new();
    let mut same_major_gt = Vec::new();
    let mut any = Vec::new();

    for line in listing.lines() {
        let Some(rest) = line.trim().strip_prefix("system-images;android-") else {
            continue;
        };
        let Some(id_end) = rest.find(';') else { continue };
        let (id, after_id) = rest.split_at(id_end);
        // The suffix must end the column (not `google_apis_playstore`).
        let Some(tail) = after_id.strip_prefix(&suffix) else { continue };
        if !tail.starts_with(char::is_whitespace) {
            continue;
        }
        // Extension/preview ids ("36-ext18", "37-rc1") fail to parse.
        let Some(parsed) = nums(id, 2) else { continue };

        if parsed[0] == target[0] {
            if parsed <= target {
                same_major_le.push((parsed.clone(), id));
            } else {
                same_major_gt.push((parsed.clone(), id));
            }
        }
        any.push((parsed, id));
    }

    same_major_le
        .into_iter()
        .max_by_key(|(key, _)| key.clone())
        .or_else(|| same_major_gt.into_iter().min_by_key(|(key, _)| key.clone()))
        .or_else(|| any.into_iter().max_by_key(|(key, _)| key.clone()))
        .map(|(_, id)| id.to_string())
        .ok_or_else(|| anyhow!("no {abi} system image found for Android API {api} in the SDK repository"))
}

pub fn list_packages(sdk: &Path, java_home: Option<&Path>) -> Result<String> {
    let mut cmd = sdkmanager(sdk, java_home);
    cmd.arg("--list").stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("failed to spawn sdkmanager --list")?;
    // Drain stdout on a thread so a full pipe can't deadlock the wait below.
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_string(&mut buf);
        }
        buf
    });

    let status = wait_deadline(&mut child, env_timeout_ms(SDKMANAGER_ENV, SDKMANAGER_LIST_TIMEOUT_MS))
        .context("failed while waiting for sdkmanager --list")?;
    let output = reader.join().unwrap_or_default();
    match status {
        Some(status) if status.success() => Ok(output),
        Some(status) => Err(anyhow!("sdkmanager --list failed: {status}")),
        None => Err(anyhow!("sdkmanager --list timed out")),
    }
}

/// Write tiny wrapper scripts into `<version>/bin/` so avm's generic
/// `~/.avm/tools/<tool>/<version>/bin/<binary>` shim lookup finds SDK-aware
/// adb/sdkmanager/etc. without every shim invocation needing to know about
/// `sdk/cmdline-tools/latest/...` layout.
fn write_wrappers(target: &Path, sdk: &Path) -> Result<()> {
    let bin = target.join("bin");
    fs::create_dir_all(&bin).context("failed to create android bin dir")?;
    let cmdline_bin = sdk.join("cmdline-tools").join("latest").join("bin");
    wrapper(&bin, sdk, "adb", &sdk.join("platform-tools").join("adb"))?;
    wrapper(&bin, sdk, "sdkmanager", &cmdline_bin.join("sdkmanager"))?;
    wrapper(&bin, sdk, "avdmanager", &cmdline_bin.join("avdmanager"))?;
    wrapper(&bin, sdk, "emulator", &sdk.join("emulator").join("emulator"))?;
    // `android` satisfies avm's is_installed marker convention and is a
    // reasonable default entry point.
    wrapper(&bin, sdk, "android", &cmdline_bin.join("sdkmanager"))
}

fn wrapper(bin: &Path, sdk: &Path, name: &str, target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dest = bin.join(name);
    let contents = format!(
        "#!/usr/bin/env sh\nexport ANDROID_HOME=\"{sdk}\"\nexport ANDROID_SDK_ROOT=\"{sdk}\"\nexec \"{target}\" \"$@\"\n",
        sdk = sdk.display(),
        target = target.display(),
    );
    fs::write(&dest, contents).with_context(|| format!("failed to write wrapper {}", dest.display()))?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
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

pub fn sysimg_abi() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64-v8a",
        _ => "x86_64",
    }
}

/// A JDK is required to run `sdkmanager`. A system `java` already on PATH is
/// used as-is (`Ok(None)` — no JAVA_HOME override needed). Otherwise an
/// avm-managed JDK is used (the `~/.avm.json` `tools.java` pin if installed,
/// else the highest installed one) and its home returned.
pub fn require_jdk() -> Result<Option<PathBuf>> {
    let system_java_works = Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if system_java_works {
        return Ok(None);
    }

    find_avm_managed_jdk().map(Some).ok_or_else(|| {
        anyhow!("a JDK is required to install Android SDK components (sdkmanager needs Java) — install one, or run `avm java use <version>`")
    })
}

fn find_avm_managed_jdk() -> Option<PathBuf> {
    let java_tools = tool_dir("java").ok()?;
    let has_java = |v: &str| java_tools.join(v).join("bin").join("java").exists();
    let version = pinned_java_version()
        .filter(|v| has_java(v))
        .or_else(|| list_installed("java", has_java).ok()?.pop())?;
    Some(java_tools.join(version))
}

fn pinned_java_version() -> Option<String> {
    let raw = fs::read_to_string(home_dir().ok()?.join(".avm.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parsed.get("tools")?.get("java")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolvers_pick_real_packages() {
        let abi = sysimg_abi();
        let listing = format!(
            "  build-tools;36.0.0 | 36.0.0 | x\n  build-tools;37.0.0-rc1 | x\n  build-tools;35.0.1 | x\n\
             system-images;android-36;google_apis;{abi} | 1 | x\n\
             system-images;android-37.0;google_apis;{abi} | 1 | x\n\
             system-images;android-37.0;google_apis_playstore;{abi} | 1 | x\n\
             system-images;android-37-ext18;google_apis;{abi} | 1 | x\n"
        );
        assert_eq!(resolve_build_tools_version(&listing, "37.2").unwrap(), "36.0.0");
        assert_eq!(resolve_build_tools_version(&listing, "35").unwrap(), "35.0.1");
        assert_eq!(resolve_system_image_id(&listing, "37.2").unwrap(), "37.0");
        assert_eq!(resolve_system_image_id(&listing, "36").unwrap(), "36");
    }
}
