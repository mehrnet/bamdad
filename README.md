# Bamdad

A cross-platform proxy/VPN client with a **Slint UI** and a **Rust core**.

Bamdad runs each proxy engine as an isolated child process instead of linking it
via FFI — the stability-first approach used by
[SimpleXray](https://github.com/lhear/SimpleXray), adapted for desktop, tablet
and phone-sized windows. Two engines are supported today:

- **Xray-core** — configuration via stdin, telemetry via gRPC `StatsService`.
- **sing-box** — configuration via a private temp file, telemetry via the
  Clash-compatible HTTP API.

The same profile, import, latency, lifecycle and UI code is shared by both.

```
┌──────────────────────────────────────────────────────────────┐
│ bamdad-app  (Slint)                                          │
│                                                              │
│  responsive shell: phone / tablet / desktop                  │
│  Dashboard · Profiles · Logs · Settings                      │
│  UI callbacks ──► EngineHandle ──┐                           │
│  UI properties ◄── bridge thread ◄┼── std::sync::mpsc        │
└──────────────────────────────────┼───────────────────────────┘
                                   │
┌──────────────────────────────────▼───────────────────────────┐
│ bamdad-core                                                  │
│                                                              │
│  kind          CoreKind::{Xray, SingBox} adapters            │
│  binary        resolve / install / version per core          │
│  link          one ProxySpec for vless/vmess/trojan/ss       │
│  config        native config generation + telemetry inject   │
│  engine        spawn, lifecycle, logs, per-core telemetry    │
│  probe         protocol-agnostic TCP latency                 │
│  system_proxy  GNOME / KDE / macOS / Windows toggles         │
└──────────────────────────────────┬───────────────────────────┘
                                   │ child process
                 ┌─────────────────▼─────────────────┐
                 │ Xray-core  or  sing-box           │
                 │ SOCKS/HTTP inbounds               │
                 │ gRPC API / Clash API              │
                 └───────────────────────────────────┘
```

## Responsive UI

The window has **no fixed width/height**. It sets `preferred-width`,
`preferred-height`, `min-width` and `min-height`, so the compositor or desktop
environment is free to tile, maximize, or resize it — including phone and
tablet surfaces.

Breakpoints:

| Width        | Shell                                        |
| ------------ | -------------------------------------------- |
| `< 700 px`   | top bar + bottom navigation + stacked cards  |
| `700–1023 px`| icon rail sidebar + adaptive content         |
| `≥ 1024 px`  | full sidebar + multi-column dashboard        |

The breakpoints are updated from Rust by watching the real window size (rather
than binding `root.width` inside the layout, which creates a Slint binding
loop). The layout also applies `safe-area-insets`, so phones with notches and
gesture bars are handled correctly.

## Multi-core profiles

A profile is a distinct object with a stable id:

```rust
Profile {
    id, name,
    core: Xray | SingBox,
    protocol: "vless" | "vmess" | "trojan" | "shadowsocks" | "custom",
    address, port,
    config: <native JSON for that core>,
    source: Option<share-link>,
}
```

* Importing a share link lets you choose **Xray** or **sing-box**; the same
  parsed `ProxySpec` is rendered into the native configuration for that core.
* Profiles with the same name are still distinct entries (id-based, not
  name-based).
* The UI shows a protocol badge and a core badge per profile.
* Existing stores are migrated in place: missing ids, `core` and protocol
  metadata are filled, and name-based selection becomes id-based selection.

### How each core is launched

| Core     | Launch                                   | Telemetry                  |
| -------- | ---------------------------------------- | -------------------------- |
| Xray     | binary with config streamed on **stdin**  | gRPC `StatsService`        |
| sing-box | `sing-box run -c <private temp file>`     | Clash API `/traffic`       |

sing-box configs are written with mode `0600` into the app's run directory and
removed when the session ends. Xray configs never touch disk.

## Unified latency

**Ping** and **Ping all** work for every saved profile regardless of protocol or
core. The measurement is a timed TCP handshake to the profile's endpoint, which
is shared by VLESS, VMess, Trojan and Shadowsocks (and their TLS/REALITY/WS/gRPC
variants). It deliberately does not send credentials, so it is safe to run
against every profile.

Results are shown per row (green/amber/red) and updated asynchronously. A
through-proxy "real delay" probe is a natural follow-up.

## Repository layout

```
crates/
  bamdad-core/
    proto/command.proto       Xray StatsService definition
    src/kind.rs               core adapter identity / paths / env vars
    src/binary.rs             resolve + install per core
    src/link.rs               ProxySpec + Xray/sing-box renderers
    src/config.rs             profiles, migration, telemetry injection
    src/engine.rs             process lifecycle + per-core telemetry
    src/probe.rs              unified TCP latency
    src/stats.rs              Xray gRPC + sing-box Clash HTTP
    src/system_proxy.rs       OS proxy toggles
  bamdad-app/
    ui/app.slint              responsive shell
    ui/pages.slint            dashboard/profiles/logs/settings
    ui/components.slint       primitives, profile rows, ping button
    ui/theme.slint            design tokens
    ui/models.slint           ProfileRow
    src/controller.rs         UI ↔ engine bridge, profiles, pings
crates/bamdad-app/android/   NativeActivity + Gradle Android project
.github/workflows/           desktop matrix, Android APK, tagged releases
scripts/
  fetch-xray.sh               install Xray-core
  fetch-singbox.sh            install sing-box
  fetch-android-cores.sh      package Android cores as libbamdad_*.so
  caged-preview.sh            headless cage + nested niri preview harness
```

## Run it

```bash
./scripts/fetch-xray.sh
./scripts/fetch-singbox.sh     # optional, enables the second core
cargo run -p bamdad-app
```

Linux build prerequisites are the usual Slint/winit ones (C toolchain,
fontconfig, Wayland/X11 headers). Windows (MSVC) and macOS build with the same
commands.

The app starts with two local test profiles, **Direct · Xray** and
**Direct · sing-box**, which run a `freedom`/`direct` outbound and local
SOCKS/HTTP inbounds. They verify the full pipeline without a remote server.

### Core resolution

1. `BAMDAD_XRAY_BIN` / `BAMDAD_SINGBOX_BIN`
2. `<data>/bin/xray[.exe]` / `<data>/bin/sing-box[.exe]`
3. `xray` / `sing-box` on `PATH`

Data directory:

| OS      | Path                                               |
| ------- | -------------------------------------------------- |
| Linux   | `$XDG_DATA_HOME/bamdad` or `~/.local/share/bamdad` |
| macOS   | `~/Library/Application Support/bamdad`             |
| Windows | `%APPDATA%\bamdad`                                 |

## Cross builds and releases

CI is in `.github/workflows/native.yml` (push/PR) and `release.yml` (tags).

Desktop matrix:

| Runner | Artifact | Bundled cores |
| --- | --- | --- |
| `ubuntu-latest` | `linux-x86_64` | Xray + sing-box |
| `ubuntu-24.04-arm` | `linux-aarch64` | Xray + sing-box |
| `windows-latest` | `windows-x86_64` | Xray + sing-box |
| `macos-latest` | `macos-aarch64` | Xray + sing-box |

Release archives contain the app plus `bin/xray` and `bin/sing-box`; the app
resolves cores next to the executable, so a download is self-contained.

Android is built as an APK with the Slint `cdylib` (`bamdad_app`) and a
`NativeActivity`. Xray and sing-box are packaged as
`jniLibs/<abi>/libbamdad_xray.so` and `libbamdad_singbox.so`. Android extracts
those to `nativeLibraryDir` with the executable bit set, and the resolver starts
them as child processes. `jniLibs` and signing files are gitignored.

Create a release by pushing a tag:

```bash
git tag v0.1.2
git push origin v0.1.2
```

The workflow produces signed APKs (`arm64-v8a`, `x86_64`, universal) and
desktop archives, then creates a GitHub Release with a checksum file. Android
signing is configured through repository secrets:

- `ANDROID_KEYSTORE_BASE64`
- `ANDROID_KEYSTORE_PASSWORD`
- `ANDROID_KEY_PASSWORD`
- `ANDROID_KEYSTORE_ALIAS`

The Android build reads `BAMDAD_VERSION_NAME`/`BAMDAD_VERSION_CODE` (the release
workflow fills them from the tag and run number). The app also supports
`bamdad --version` before starting the GUI.

## Caged Wayland previews

`scripts/caged-preview.sh` starts an **isolated** compositor stack:

```
cage (headless wlroots) → nested niri → Bamdad
```

It does not render into or control the caller's live session, and by default it
uses a throwaway data dir under `/tmp`, so real profiles are never touched.

```bash
# Start and capture a desktop-sized screenshot
./scripts/caged-preview.sh start
./scripts/caged-preview.sh shot /tmp/bamdad.png 1100

# Phone-sized preview with an isolated profile store
BAMDAD_PREVIEW_PAGE=0 BAMDAD_PREVIEW_PING=1 \
  ./scripts/caged-preview.sh start
./scripts/caged-preview.sh shot /tmp/bamdad-phone.png 380
./scripts/caged-preview.sh stop
```

Useful options:

- `BAMDAD_CAGE_DATA=/path/to/data` — reuse a specific data dir (app reads
  `<data>/bamdad`).
- `BAMDAD_CAGE_BIN=/path/to/bamdad` — use a release build.
- `BAMDAD_PREVIEW_PAGE=0..1` — open Dashboard/Profiles/Logs/Settings.
- `BAMDAD_PREVIEW_PING=1` — run "Ping all" at startup.
- `BAMDAD_PREVIEW_CONNECT=1` — connect the selected profile at startup.

The preview hooks are development-only environment variables.

## Tests

```bash
cargo test -p bamdad-core
```

The suite includes:

- share-link parsing and both native renderers,
- profile migration and telemetry injection for both cores,
- protocol-agnostic TCP latency,
- a real Xray smoke test (stdin config, SOCKS listener, gRPC stats, clean stop),
- a real sing-box smoke test (file config, SOCKS listener, Clash API, clean stop).

Both smoke tests skip automatically when the corresponding core is not
installed.

## Android VPN

Android has no general app-level proxy setting. Local SOCKS/HTTP listeners are
only useful to apps that can be configured manually. For system-wide routing,
Bamdad now uses an Android `VpnService` TUN and forwards it to the core's local
SOCKS listener with the MIT
[hev-socks5-tunnel](https://github.com/heiher/hev-socks5-tunnel) AAR. The core
process is excluded from the VPN so its outbound connections use the physical
network.

The VPN is started automatically when the core reaches `Running`; Android asks
for VPN consent on first use. Desktop keeps the system-proxy path.

## Current limits / roadmap

1. **TUN/VPN mode** — the current VPN path is a system proxy plus local
   SOCKS/HTTP listeners. Next: Android `VpnService` + `hev-socks5-tunnel`, and a
   privileged desktop TUN helper. The engine abstraction is ready for it.
2. **Subscriptions** — refresh, multiple profiles per subscription, latency
   sorting.
3. **Real-delay probe** — HTTP `generate_204` through the running core, in
   addition to the unified TCP handshake.
4. **Routing editor**, per-app split tunneling, DNS options.
5. **Packaging** — bundle sidecars, AppImage/MSI/.app, auto-update, tray mode.

## Licensing

Bamdad is MPL-2.0. Cores are shipped as separate executables: Xray-core is
MPL-2.0 and sing-box is GPL-3.0. Include each core's license and the
`geoip.dat`/`geosite.dat` notices in release artifacts.
