use anyhow::{Context, Result};
use avm_plugin_api::{ToolVersion, ToolVersionQuery};
use std::collections::HashSet;

const DEFAULT_REPOSITORY_URL: &str = "https://dl.google.com/android/repository/repository2-3.xml";

pub fn available_versions(query: ToolVersionQuery) -> Result<Vec<ToolVersion>> {
    let url = std::env::var("AVM_ANDROID_REPOSITORY_URL").unwrap_or_else(|_| DEFAULT_REPOSITORY_URL.to_string());
    let xml = String::from_utf8(avm_plugin_api::fetch(&url, 30)?)
        .context("Android repository index was not valid UTF-8")?;
    let mut levels = platform_levels(&xml);
    // Newest first, matching how node's release index is ordered.
    levels.sort_unstable_by(|a, b| b.cmp(a));

    Ok(query
        .filter(levels, |level| level.major)
        .into_iter()
        .map(|level| ToolVersion {
            label: format!("API {}", level.raw),
            version: level.raw,
            channel: None,
            is_lts: false,
            is_security: false,
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ApiLevel {
    major: u64,
    minor: u64,
    raw: String,
}

/// `s` as exactly `len` dot-separated numbers (missing trailing parts are 0);
/// `None` for more parts or anything non-numeric (`CANARY`, `37.2-beta3`, ...).
pub fn nums(s: &str, len: usize) -> Option<Vec<u64>> {
    let mut parts: Vec<u64> = s.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if parts.len() > len {
        return None;
    }
    parts.resize(len, 0);
    Some(parts)
}

/// Every stable `platforms;android-<level>` package path in Google's SDK
/// repository index. A substring scan, not an XML parser — the index is 400KB+
/// and we need one attribute of one element type. Preview levels are skipped.
fn platform_levels(xml: &str) -> Vec<ApiLevel> {
    const NEEDLE: &str = "platforms;android-";
    let mut seen = HashSet::new();
    let mut levels = Vec::new();
    let mut rest = xml;
    while let Some(idx) = rest.find(NEEDLE) {
        let after = &rest[idx + NEEDLE.len()..];
        let Some(end) = after.find('"') else { break };
        let raw = &after[..end];
        rest = &after[end..];
        if seen.insert(raw) {
            if let Some(n) = nums(raw, 2) {
                levels.push(ApiLevel { major: n[0], minor: n[1], raw: raw.to_string() });
            }
        }
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stable_levels_only() {
        assert_eq!(nums("37", 2), Some(vec![37, 0]));
        assert_eq!(nums("36.0.1", 2), None);
        assert_eq!(nums("37.2-beta3", 2), None);
        let xml = r#"<p path="platforms;android-36"/><p path="platforms;android-37.2"/><p path="platforms;android-CANARY"/><p path="platforms;android-36"/>"#;
        let mut levels = platform_levels(xml);
        levels.sort_unstable_by(|a, b| b.cmp(a));
        let raws: Vec<_> = levels.iter().map(|l| l.raw.as_str()).collect();
        assert_eq!(raws, ["37.2", "36"]);
    }
}
