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
- **Verified downloads** — the one direct download, the cmdline-tools
  bootstrap zip, is checked against a pinned sha256 before extraction;
  everything after it comes through `sdkmanager`, which verifies its own
  packages.
- **Automatic `ANDROID_HOME` / `ANDROID_SDK_ROOT`** — computed directly
  from the selected version, in-process, every time you run a command.
- **Working `adb`, `sdkmanager`, `avdmanager`, `emulator`** — installing a
  version also drops small wrapper scripts for each on `PATH` so they see
  the right `ANDROID_HOME` without you exporting anything by hand.
- **`avm android avd` — guided AVD (emulator) management** — `list`,
  `create`, `start`, `delete`, without needing to know `avdmanager`'s
  package-name syntax or which system image a given API level maps to;
  `create` picks an API level and device profile interactively and installs
  the matching system image automatically if it isn't already there.

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
| `avm android avd list` | Show AVDs (emulators) for the active version's SDK |
| `avm android avd create [name]` | Interactive wizard: pick an API level and device, auto-installs the matching system image if needed, creates the AVD |
| `avm android avd create <name> --api <level> [--device <id>]` | Same, non-interactive — for scripting |
| `avm android avd start <name>` | Launch the emulator with that AVD |
| `avm android avd delete <name>` | Remove an AVD |

`<version>` is an API level (`36`, `36.1`, `34`, ...) or `latest`.

Build-tools and system-image packages are resolved against the real SDK
repository listing, not guessed from the API string — neither follows the
platform's `major.minor` numbering, and a preview/beta level (`37.2`) often
has no exact package yet; the closest real one is picked automatically.

Useful env overrides for `install`:

| Variable | Purpose |
| --- | --- |
| `ANDROID_BUILD_TOOLS_VERSION` | Force a specific build-tools version instead of the auto-resolved closest match |
| `ANDROID_SYSTEM_IMAGE_API` | Force a specific system-image API id (e.g. `36.1`) instead of the auto-resolved closest match |
| `ANDROID_CMDLINE_TOOLS_BUILD` | Pin a specific cmdline-tools build number if Google's default one 404s (also set `ANDROID_CMDLINE_TOOLS_SHA256`) |
| `ANDROID_CMDLINE_TOOLS_SHA256` | Expected sha256 of a custom cmdline-tools build's zip |
| `AVM_ALLOW_UNVERIFIED=1` | Install a cmdline-tools build with no known sha256 (not recommended) |
| `AVM_ANDROID_CURL_TIMEOUT` / `AVM_ANDROID_UNZIP_TIMEOUT` / `AVM_ANDROID_SDKMANAGER_TIMEOUT` | Seconds — extend for slow links (the SDK pull is several GB) |

## Emulators (AVDs)

The easy way — `avm android avd`:

```bash
avm android use 36                      # pick which SDK avd commands operate against

avm android avd create                  # interactive: pick API level + device, auto-installs
                                         # the system image if it isn't there yet
avm android avd create pixel-36 --api 36 --device pixel_6   # same, non-interactive

avm android avd list                    # see what you've got, and flag any with a missing image
avm android avd start pixel-36          # launch the emulator
avm android avd delete pixel-36         # remove it
```

`avd create` always operates against whichever android version `avm
android use` last selected (local pin, else global) — that's the SDK the
system image gets installed into and the AVD gets created against.

The raw tools are still there if you want them directly — `adb`,
`sdkmanager`, `avdmanager`, and `emulator` land on `PATH` (via avm's shims,
with `ANDROID_HOME` already pointed at the right SDK) once a version is
selected, same as any other Android SDK install:

```bash
avdmanager list avd
adb devices
adb install app.apk
```

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
`docs/plugins/CREATING_A_PLUGIN.md` for the full wire protocol this
executable speaks (`manifest`, `versions`, `is-installed`,
`installed-versions`, `executable-path`, `env-vars`, `install`,
`uninstall`).

```bash
cargo build --release
# binary at target/release/avm-plugin-android
```
