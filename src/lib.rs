mod install;
mod versions;

use anyhow::{Context, Result};
use avm_plugin_api::{ToolProvider, ToolVersion, ToolVersionQuery};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct AndroidProvider;

impl AndroidProvider {
    pub fn new() -> Self {
        Self
    }

    fn bin_path_for(&self, version: &str, binary: &str) -> Result<Option<PathBuf>> {
        let candidate = install::tools_root()?
            .join(version)
            .join("bin")
            .join(binary);
        Ok(candidate.exists().then_some(candidate))
    }
}

impl ToolProvider for AndroidProvider {
    fn name(&self) -> &str {
        "android"
    }

    fn is_installed(&self, version: &str) -> bool {
        self.bin_path_for(version, "sdkmanager")
            .ok()
            .flatten()
            .is_some()
    }

    fn installed_versions(&self) -> Result<Vec<String>> {
        let root = install::tools_root()?;
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).context("failed to read android tools dir"),
        };

        let mut versions: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|version| self.is_installed(version))
            .collect();
        versions.sort_unstable();
        Ok(versions)
    }

    fn available_versions(&self, query: ToolVersionQuery) -> Result<Vec<ToolVersion>> {
        versions::available_versions(query)
    }

    fn executable_path(&self, version: &str) -> Result<Option<PathBuf>> {
        self.bin_path_for(version, "sdkmanager")
    }

    /// Deterministic, in-process — no subprocess, so it can't be poisoned by
    /// whatever ANDROID_HOME already happens to be in the caller's environment
    /// (the failure mode of the old asdf `exec-env`-diffing approach).
    fn env_vars(&self, version: &str) -> Result<HashMap<String, String>> {
        let sdk = install::sdk_dir(version)?;
        let mut env = HashMap::new();
        env.insert("ANDROID_HOME".to_string(), sdk.to_string_lossy().to_string());
        env.insert("ANDROID_SDK_ROOT".to_string(), sdk.to_string_lossy().to_string());
        Ok(env)
    }

    fn install(&self, version: &str) -> Result<()> {
        install::install_android(version)
    }

    fn uninstall(&self, version: &str) -> Result<()> {
        let target = install::tools_root()?.join(version);
        if target.exists() {
            fs::remove_dir_all(&target).context("failed to remove managed android version")?;
        }
        Ok(())
    }
}
