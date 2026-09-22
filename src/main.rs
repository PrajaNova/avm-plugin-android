use avm_plugin_android::AndroidProvider;
use avm_plugin_api::{runner, Manifest};
use std::process::ExitCode;

fn main() -> ExitCode {
    let manifest = Manifest {
        name: "android".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        api_version: Some(1),
        description: Some(
            "Built-in Android SDK provider (cmdline-tools, platform-tools, sdkmanager)"
                .to_string(),
        ),
        section_label: Some("Android SDK".to_string()),
        homepage: Some("https://github.com/prajanova/avm".to_string()),
    };
    runner::run(manifest, &AndroidProvider::new())
}
