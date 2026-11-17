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
│  Profiles · Settings (logs) · responsive nav + connect orb  │
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
│  probe         protocol-aware latency                        │
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

The default screen is **Profiles**. A circular connect control sits in the
center of the bottom navigation on phones and at the bottom of the sidebar on
desktop/tablet. Session download/upload totals replace the old
connected/disconnected badge. Logs are part of Settings. Navigation uses SVG
icons (including the moon app mark), and profile names/emoji use the system font
fallback.

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

**Ping** and **Ping all** work for every saved profile, and what gets measured
depends on the transport:

- **Proxy profiles** are measured end to end. A temporary core is started from
  the profile's own config, a pooled HTTPS request goes through its HTTP
  inbound, and the best of three requests is reported. That proves the
  protocol, credentials, and transport, not just a reachable port.
- **WireGuard, AmneziaWG, and UDP OpenVPN** cannot answer a TCP handshake, so
  they are measured with an ICMP echo to the endpoint address: exact round
  trip, nothing sent to the VPN port.
- **TCP OpenVPN** falls back to a handshake on its own port when ICMP is
  filtered.
- When the host answers neither, the row shows a `≈` estimate measured against
  the same host's web ports, so an ICMP-filtered server is no longer reported as
  down.

While a native tunnel owns the VPN slot, proxy rows use the endpoint path
measurement instead of starting a probe core that would be routed through the
tunnel. Results are shown per row (green/amber/red) and update asynchronously.

## Failover

With **Auto-switch on failure** enabled (Settings → Privacy, on by default), a
hard failure moves the tunnel to the next profile in the same group:

- The walk follows the group's display order, wraps once, and skips profiles
  that cannot be dialled (no endpoint, empty native config) or that would need
  a password prompt mid-switch.
- An attempt ledger stops the rotation once every profile in the group has been
  tried, so a group of dead servers cannot keep restarting cores.
- The ledger resets as soon as the tunnel is proven: a successful proxied
  request, real traffic counters, or a fresh manual connect. Starting a core is
  not proof — a bad config exits right after "running".
- The switch is logged (`failover: <name> failed (<reason>); trying <name>`) and
  the selected profile, accent, and UI selection move with it.

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
    src/probe.rs              protocol-aware latency probes
    src/stats.rs              Xray gRPC + sing-box Clash HTTP
    src/system_proxy.rs       OS proxy toggles
  bamdad-app/
    ui/app.slint              responsive shell
    ui/pages.slint            profiles / settings (with logs)
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
git tag v0.9.6
git push origin v0.9.6
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

> **Never test the GUI in your live session.** Do not run the app, `grim`,
> `wtype`, or `ydotool` against your working Wayland/niri desktop. Use the
> caged stack below for every run, screenshot, and interaction test. See
> `AGENTS.md` for the full rule.

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
- `BAMDAD_PREVIEW_PAGE=0..1` — open Profiles or Settings.
- `BAMDAD_PREVIEW_PING=1` — run "Ping all" at startup.
- `BAMDAD_PREVIEW_LOGS=1` — open the fullscreen log view at startup.
- `BAMDAD_PREVIEW_LOG_FILE=1` — mirror app log lines to `preview.log` in the
  data directory (caged runs cannot scroll the log view).
- `BAMDAD_PREVIEW_LICENSES=1` — open the open-source licenses screen.
- `BAMDAD_PREVIEW_CONNECT=1` — connect the selected profile at startup.

The preview hooks are development-only environment variables.

## Tests

```bash
cargo test -p bamdad-core
```

The suite includes:

- share-link parsing and both native renderers,
- profile migration and telemetry injection for both cores,
- protocol-aware latency,
- a real Xray smoke test (stdin config, SOCKS listener, gRPC stats, clean stop),
- a real sing-box smoke test (file config, SOCKS listener, Clash API, clean stop).

Both smoke tests skip automatically when the corresponding core is not
installed.

## Supported profiles

| Input | Status |
|---|---|
| VLESS over TCP/Reality, WS, gRPC, HTTP/2, HTTPUpgrade, XHTTP/SpliHTTP | supported |
| VLESS TLS/Reality, `flow`, WS early data (`ed`/`eh`), gRPC `mode=multi`, XHTTP `mode`/`extra` | supported |
| VMess (TCP/WS/gRPC/HTTP) | supported |
| Trojan (TLS/WS/gRPC) | supported |
| Shadowsocks SIP002 / legacy links | supported |
| Custom Xray or sing-box JSON | paste it into the Import box (must contain an `outbounds` array) |
| Subscriptions | supported (import, refresh, reorder) |
| Libre `node.conf` | imported from a file or ZIP |
| `.ovpn` (inline or external CA/cert/key/tls-auth/tls-crypt/dh/pkcs12) | supported on Android |
| WireGuard / AmneziaWG `.conf` | supported on Android |
| SSR, SIP003 plugins (`obfs`, `v2ray-plugin`) | not yet |
| Hysteria2, TUIC | not yet |
| IPSec/IKEv2 `.sswan` | supported on Android for certificate-based profiles (PSK, EAP-TLS pending) |

XHTTP is Xray-only; imports that select sing-box are switched to Xray
automatically. Link-based profiles are regenerated from their stored share link
on startup, so config fixes apply without re-importing.

## Android VPN

Android has no general app-level proxy setting. Local SOCKS/HTTP listeners are
only useful to apps that can be configured manually. For system-wide routing,
Bamdad now uses an Android `VpnService` TUN and forwards it to the core's local
SOCKS listener with the MIT
[tun2socks](https://github.com/xjasonlyu/tun2socks) executable. The tunnel
descriptor is duplicated into the child's stdin (`fd://0`), and the core and
tunnel processes are excluded from the VPN so their outbound connections use
the physical network.

The VPN is started automatically when the core reaches `Running`; Android asks
for VPN consent on first use. Desktop keeps the system-proxy path.

Native protocols take the same slot instead of tun2socks: WireGuard and
AmneziaWG through `amneziawg-android`, OpenVPN through the pinned OpenVPN 3
core with a Bamdad-owned `VpnService`. Global settings apply to all of them:
custom DNS, per-app split tunneling, bypass CIDRs, and the kill-switch sinkhole
that holds the slot whenever no tunnel is connected. An emulator smoke test
verifies that lifecycle: sinkhole → tunnel → sinkhole.

## IPSec/IKEv2 groundwork

strongSwan 6.0.4 is vendored as a submodule and its Android frontend is built by
`scripts/build-strongswan-android.sh`, which stages a copy of the tree,
generates the sources the autotools build normally produces (ASN.1 OIDs,
static proposal keywords, the settings parser/lexer), builds a static
`libcrypto` from OpenSSL 3.5.4, and runs strongSwan's own `ndk-build` project.

The result is one set of libraries per ABI — `libcharon.so`,
`libandroidbridge.so` (the JNI layer used by `CharonVpnService`),
`libstrongswan.so`, `libipsec.so`, and the plugins — uploaded from CI as
`bamdad-strongswan-android`.

APKs package those libraries (debug builds from the same workflow, release
builds from the last CI artifact) and the IpSec backend drives
`org.strongswan.android.logic.CharonVpnService`, the class name the JNI symbols
bind to.

The backend is now wired: `.sswan` profiles are detected on import, the Kotlin
`IpSecBackend` starts `CharonVpnService`, charon negotiates IKEv2 with a
generated connection config, and its state codes map onto Bamdad's tunnel
events. Certificate-based profiles can embed their CA, client certificate, and
PKCS#8 key directly in the XML, so a profile is self-contained; strongSwan's
Android frontend itself does not support PSK, so neither does Bamdad yet.

An emulator smoke runs a strongSwan responder with a throwaway CA, connects,
checks that the CHILD_SA is ESTABLISHED on both ends, that charon built the tun
device, and that disconnecting releases it again.

## Desktop TUN

Desktop traffic can leave through a real TUN device instead of the system
proxy (Settings → Local listeners → **Tunnel mode**). The chain is:

```
bamdad (user) -> pkexec -> bamdad-tun-helper (root) -> tun2socks (user) -> core SOCKS
```

- `bamdad-tun-helper` opens `/dev/net/tun`, assigns `10.255.0.1/24`, points the
  default route at it, keeps the profile endpoint on the physical link with
  host routes, and holds the descriptor open. It drops privileges for the
  sidecar, and on SIGINT/SIGTERM restores the previous default route and
  removes its routes. A state file lets `down --state FILE` recover after an
  interrupted run.
- `bamdad-tun2socks` is xjasonlyu/tun2socks built for the host by
  `scripts/fetch-desktop-tun2socks.sh`; the helper hands it the tun descriptor
  as fd 3 and it forwards into the core's SOCKS listener.
- DNS follows the tunnel: the helper points systemd-resolved at the tunnel
  device with a `~.` routing domain when that is available, otherwise it swaps
  `/etc/resolv.conf` for the configured servers and keeps the original next to
  it. Both are reverted when the tunnel stops.
- IPv6 cannot bypass the tunnel either: the helper adds `fd00::1/64` and points
  the v6 default at the device, so v6 flows go through tun2socks too (and fail
  closed if the core has no v6 egress).
- The app passes its process id (`--parent-pid`); if it dies, the helper tears
  the tunnel down and restores routing, resolver settings, and the state file
  instead of leaving the machine pointing at a dead device.
- Failures are visible: if the helper or sidecar is missing, enabling tunnel
  mode says so, and a tunnel that fails to start turns the status into
  `Connected · tunnel failed` instead of pretending to protect the session.
- The kill switch means the same thing as on Android: when the core dies the
  tunnel stays up and traffic is blocked (`Core error · traffic blocked`)
  instead of silently falling back to the physical route.
- The app starts both once the core's SOCKS port answers, and stops the helper
  on disconnect or error, which restores routing.

The DNS servers come from Settings → Custom DNS (falling back to 1.1.1.1 and
8.8.8.8). The binaries ship inside the Linux release tarballs.

**Tunnel routes** (Settings → Local listeners, desktop) turns the tunnel into a
split tunnel: when the field holds one or more CIDRs, only those prefixes are
routed into the device — the default route, DNS, and IPv6 stay untouched. Leave
it empty to route everything.

### macOS

`bamdad-tun-helper` also builds for macOS: it opens a `utun` device through
`PF_SYSTEM`, assigns the address/MTU, routes either a list of prefixes
(`--route`) or everything (`--default`, via the `0.0.0.0/1` + `128.0.0.0/1`
split so the original default is untouched) with the endpoint kept on the
physical gateway, and removes everything on stop or when the parent process
disappears. `scripts/test-macos-tun-helper.sh` runs that on a macOS runner
using only a TEST-NET prefix, so the runner's default route is never at risk.

utun frames carry a 4-byte address-family header that tun2socks does not
expect, so the helper interposes a socketpair: packets read from utun are
stripped to plain IP, forwarded to the sidecar on fd 3, and the header is
re-added on the way back. The helper also writes its pid into the state file so
a privileged `kill` can stop it after a crash.

DNS is applied with `networksetup -setdnsservers` for every active network
service and reset to `Empty` on the way down.

Not wired yet: the app still uses the proxy path on macOS, because the
`osascript ... with administrator privileges` prompt needs validation on a real
desktop session (CI runners have no GUI authorisation). The helper and sidecar
now ship inside the macOS tarball, and the frame adapter has unit tests.

### Windows

`bamdad-tun-helper` compiles for Windows and drives the chain in the same way:
the sidecar creates a Wintun adapter (`-device tun://bamdad0`), and the helper
waits for it, assigns the address/MTU, installs routes (`--route`, or the same
`/1` split for `--default` with endpoint host routes) and per-interface DNS, and
tears everything down on exit.

It is **not** shipped or wired yet: on GitHub's Windows runners the Wintun
adapter created by the sidecar never shows up in `netsh interface` or
`Get-NetAdapter`, so the routing step cannot be validated there (`scripts/test-windows-tun-helper.sh`
reproduces it). That needs a real Windows desktop before users get a Windows
tunnel.

`scripts/test-desktop-tunnel.sh` proves the whole chain in CI: a network
namespace with a veth "internet", a real sing-box SOCKS core, the helper and
tun2socks, then `curl` must reach a host HTTP server through the tunnel and the
original routing must be back after the helper stops.

## Current limits / roadmap

1. **Desktop TUN** — Linux only: tunnel mode, helper, sidecar, DNS, and an
   end-to-end CI test. macOS/Windows helpers are next.
2. ~~**Subscriptions**~~ — imports, refresh, reordering, and latency sorting
   are in place.
3. ~~**Real-delay probe**~~ — proxied `generate_204` requests now drive the
   proxy latency numbers.
4. **Routing editor** — per-app split tunneling, custom DNS, and bypass
   routes already apply to the proxy, WireGuard/AmneziaWG, and OpenVPN
   tunnels; a profile-aware routing editor is next.
5. **Packaging** — bundle sidecars, AppImage/MSI/.app, auto-update, tray mode.

## Licensing

Bamdad is GPL-3.0-only. Settings → **Open-source licenses** lists every
bundled component with its version and license. Proxy cores are shipped as
separate executables:
Xray-core is MPL-2.0 and sing-box is GPL-3.0. Android native transports use
Apache-2.0 `amneziawg-android`, MPL-2.0 OpenVPN 3, and GPL-2.0-or-later
strongSwan. Include each component's license and the
`geoip.dat`/`geosite.dat` notices in release artifacts.

## Versioning

Bamdad uses an odometer-style version: `major.minor.patch`, where `patch` is a
single digit and rolls over into `minor`:

```
0.3.8 -> 0.3.9 -> 0.4.0
```

There is no standard SemVer name for this; it is a base-10 (odometer) carry
scheme. Use `scripts/bump-version.sh` to advance it. The next release is
`v0.9.6`.
