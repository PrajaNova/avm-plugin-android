mod avd;
mod install;
mod versions;

use anyhow::Result;
use avm_plugin_api::{runner, tool_dir, Manifest, ToolProvider, ToolVersion, ToolVersionQuery};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

struct AndroidProvider;

impl AndroidProvider {
    fn sdkmanager_wrapper(&self, version: &str) -> Result<Option<PathBuf>> {
        let candidate = tool_dir("android")?.join(version).join("bin").join("sdkmanager");
        Ok(candidate.exists().then_some(candidate))
    }
}

impl ToolProvider for AndroidProvider {
    fn name(&self) -> &str {
        "android"
    }

    fn is_installed(&self, version: &str) -> bool {
        self.sdkmanager_wrapper(version).ok().flatten().is_some()
    }

    fn installed_versions(&self) -> Result<Vec<String>> {
        avm_plugin_api::list_installed("android", |v| self.is_installed(v))
    }

    fn available_versions(&self, query: ToolVersionQuery) -> Result<Vec<ToolVersion>> {
        versions::available_versions(query)
    }

    fn executable_path(&self, version: &str) -> Result<Option<PathBuf>> {
        self.sdkmanager_wrapper(version)
    }

    /// Deterministic, in-process — can't be poisoned by whatever ANDROID_HOME
    /// the caller's environment already has.
    fn env_vars(&self, version: &str) -> Result<HashMap<String, String>> {
        let sdk = install::sdk_dir(version)?.to_string_lossy().to_string();
        Ok(HashMap::from([
            ("ANDROID_HOME".to_string(), sdk.clone()),
            ("ANDROID_SDK_ROOT".to_string(), sdk),
        ]))
    }

    fn install(&self, version: &str) -> Result<()> {
        install::install_android(version)
    }

    fn uninstall(&self, version: &str) -> Result<()> {
        avm_plugin_api::remove_version("android", version)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("avd") {
        return avd::run(&args[1..]);
    }
    let manifest = Manifest::new(
        "android",
        env!("CARGO_PKG_VERSION"),
        "Built-in Android SDK provider (cmdline-tools, platform-tools, sdkmanager)",
        "Android SDK",
    );
    runner::run(manifest, &AndroidProvider)
}
