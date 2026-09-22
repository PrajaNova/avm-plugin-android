# avm-plugin-android

avm's native Android SDK provider: version install/switching and automatic
`ANDROID_HOME`. Works with [avm](https://github.com/PrajaNova/avm) via
avm's plugin marketplace — install with:

```bash
avm plugin add android
```

That fetches this repo's latest compiled release for your platform from
GitHub — no Rust toolchain or network access needed beyond the one
download. See [Release process](#release-process) below if you're
building/publishing this repo itself.

## Features

- **Live version index** — `avm android versions` reads Google's SDK
  repository index (`repository2-3.xml`) directly and lists every stable
  `platforms;android-<level>` package, including the newer decimal levels
  (`36.1`, `37.0`, ...) — preview/beta channels (`CANARY`, `-betaN`) are
  filtered out.
- **Version install & switching** — "a version" is an Android API level.
  Installing one pulls the SDK cmdline-tools, `platform-tools`,
  `platforms;android-<level>`, matching `build-tools`, the `emulator`, and
  a matching system image, all driven through `sdkmanager` (never a manual
  download).
- **Automatic `ANDROID_HOME` / `ANDROID_SDK_ROOT`** — computed directly
  from the selected version, in-process, every time you run a command.
- **Working `adb`, `sdkmanager`, `avdmanager`, `emulator`** — installing a
  version also drops small wrapper scripts for each on `PATH` so they see
  the right `ANDROID_HOME` without you exporting anything by hand.

## Requirements

Installing a version needs a JDK (`sdkmanager` itself needs Java). A
system `java` on `PATH` is used if present; otherwise an avm-managed one —
run `avm plugin add java && avm java install <version>` first if you don't
have either.

## Commands

Once installed (`avm plugin add android`), everything is under `avm android`:

| Command | What it does |
| --- | --- |
| `avm android` | Interactive menu (list / browse versions / install latest / uninstall / help) |
| `avm android list` | Show the selected and installed versions |
| `avm android versions` | Browse the 10 most recent API levels |
| `avm android <level> versions` | e.g. `avm android 34 versions` — check one specific level exists |
| `avm android latest versions` | Just the newest API level |
| `avm android use <version> [-g\|--global]` | Select an installed version, locally (default) or globally |
| `avm android set <version> [-g\|--global]` | Alias for `use` |
| `avm android install <version\|latest\|N>` | Install (if missing) + auto-pin: local, and global too if nothing's pinned globally yet |
| `avm android install <version> --global` | Install + pin globally only |
| `avm android install <version> --no-pin` | Install without touching any pin |
| `avm android uninstall <version>` | Remove a managed version |

`<version>` is an API level (`36`, `36.1`, `34`, ...) or `latest`.

Useful env overrides for `install`:

| Variable | Purpose |
| --- | --- |
| `ANDROID_BUILD_TOOLS_VERSION` | Override the build-tools version installed alongside the platform (defaults to `<level>.0.0`) |
| `ANDROID_CMDLINE_TOOLS_BUILD` | Pin a specific cmdline-tools build number if Google's default one 404s |
| `AVM_ANDROID_CURL_TIMEOUT` / `AVM_ANDROID_UNZIP_TIMEOUT` / `AVM_ANDROID_SDKMANAGER_TIMEOUT` | Seconds — extend for slow links (the SDK pull is several GB) |

## Environment

`ANDROID_HOME` and `ANDROID_SDK_ROOT` are exported automatically to the
selected version's SDK root whenever it's the active pin (local overriding
global, same as every other avm tool).

## Release process

Tag-triggered (`vX.Y.Z`) GitHub Actions workflow builds
`avm-plugin-android_<os>_<arch>.tar.gz` for `linux_amd64`, `linux_arm64`,
and `darwin_arm64`, and publishes them as a GitHub Release — that's what
`avm plugin add android` downloads. See
[`avm-marketplace`](https://github.com/PrajaNova/avm-marketplace) for the
registry entry that points at this repo, and the main
[avm repo](https://github.com/PrajaNova/avm)'s
`docs/migration/PLUGIN_PROTOCOL.md` for the full wire protocol this
executable speaks (`manifest`, `versions`, `is-installed`,
`installed-versions`, `executable-path`, `env-vars`, `install`,
`uninstall`).

```bash
cargo build --release
# binary at target/release/avm-plugin-android
```
