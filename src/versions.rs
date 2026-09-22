use anyhow::{anyhow, Context, Result};
use avm_plugin_api::{ToolVersion, ToolVersionQuery};
use std::fs;
use std::path::Path;
use std::process::Command;

const DEFAULT_REPOSITORY_URL: &str =
    "https://dl.google.com/android/repository/repository2-3.xml";

pub fn available_versions(query: ToolVersionQuery) -> Result<Vec<ToolVersion>> {
    let mut levels = platform_levels(&repository_xml()?);
    // Descending: newest first, matching how node's release index is ordered.
    levels.sort_unstable_by(|a, b| b.cmp(a));

    let filtered: Vec<ApiLevel> = match query {
        // The interactive picker (avm-cli) pages 10 at a time with real
        // up/down scrolling — capping the source list at exactly one page
        // left nothing to scroll into. 30 gives ~3 pages (in practice all
        // stable levels Google has ever shipped fit well under this).
        ToolVersionQuery::Recent => levels.into_iter().take(30).collect(),
        ToolVersionQuery::Latest => levels.into_iter().take(1).collect(),
        ToolVersionQuery::Major(major) => {
            levels.into_iter().filter(|lvl| lvl.major == major).collect()
        }
    };

    Ok(filtered
        .into_iter()
        .map(|level| ToolVersion {
            version: level.raw.clone(),
            label: format!("API {}", level.raw),
            channel: None,
            is_lts: false,
            is_security: false,
        })
        .collect())
}

fn repository_xml() -> Result<String> {
    let url =
        std::env::var("AVM_ANDROID_REPOSITORY_URL").unwrap_or_else(|_| DEFAULT_REPOSITORY_URL.to_string());

    // Same local-override convenience node's provider has: a path on disk
    // (used by tests / offline mirrors) is read directly instead of curled.
    let local = Path::new(&url);
    if local.exists() {
        return fs::read_to_string(local)
            .with_context(|| format!("failed to read Android repository index from {}", local.display()));
    }

    let output = Command::new("curl")
        .arg("-fsSL")
        .arg("--connect-timeout")
        .arg("10")
        .arg("--max-time")
        .arg("30")
        .arg(&url)
        .output()
        .with_context(|| format!("failed to fetch Android repository index from {url}"))?;

    if !output.status.success() {
        return Err(anyhow!(
            "failed to fetch Android repository index from {url}: curl exited with {}",
            output.status
        ));
    }

    String::from_utf8(output.stdout).context("Android repository index was not valid UTF-8")
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ApiLevel {
    raw: String,
    major: u64,
    minor: u64,
}

impl Ord for ApiLevel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

impl PartialOrd for ApiLevel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Extract every `platforms;android-<level>` package path out of Google's SDK
/// repository index. Deliberately not a real XML parser — the index is huge
/// (400KB+) and we only ever need one attribute value out of one element
/// type, so a substring scan keeps this dependency-free and fast. Preview
/// levels (e.g. `CANARY`) don't parse as a version and are skipped.
fn platform_levels(xml: &str) -> Vec<ApiLevel> {
    const NEEDLE: &str = "platforms;android-";
    let mut seen = std::collections::HashSet::new();
    let mut levels = Vec::new();

    let mut rest = xml;
    while let Some(idx) = rest.find(NEEDLE) {
        let after = &rest[idx + NEEDLE.len()..];
        let Some(end) = after.find('"') else { break };
        let raw = &after[..end];
        rest = &after[end..];

        if seen.insert(raw.to_string()) {
            if let Some(level) = parse_api_level(raw) {
                levels.push(level);
            }
        }
    }

    levels
}

/// Only clean `N` or `N.M` levels are stable platform releases — anything
/// else (`CANARY`, `37.2-beta3`, ...) is a preview channel and is skipped so
/// `avm android versions` lists installable GA levels only.
fn parse_api_level(raw: &str) -> Option<ApiLevel> {
    let mut parts = raw.splitn(2, '.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = match parts.next() {
        Some(m) => m.parse::<u64>().ok()?,
        None => 0,
    };
    Some(ApiLevel {
        raw: raw.to_string(),
        major,
        minor,
    })
}
