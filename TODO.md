# Bamdad multi-transport roadmap

Goal: support proxy cores (Xray/sing-box), WireGuard, AmneziaWG, IPSec, and
OpenVPN in one cross-platform app while keeping Rust/Slint as the brain and
one Android VpnService slot as the transport boundary.

Licensing: the whole cross-platform implementation moves to GPL-3.0.

## Architecture decisions

- Rust owns profiles, selection, groups, UI, logs, and connection state.
- Android Kotlin owns the VPN slot and protocol backends.
- One `TunnelController` serializes connect/disconnect and switching.
- Only one backend may own the Android VPN slot at a time.
- Proxy cores keep the current path: VpnService TUN -> tun2socks -> SOCKS/HTTP.
- Native protocols own their own VpnService through their upstream libraries.
- Native status is normalized into the existing Rust connection events so the
  Slint UI does not need protocol-specific branches.

```
Rust/Slint
  profiles, UI, state
        | JNI startTunnel/stopTunnel/nativeTunnelEvent
        v
Android TunnelController
  mutex, one active backend
        |-- ProxyBackend      Xray/sing-box + tun2socks
        |-- AmneziaWgBackend  amneziawg-android GoBackend
        |-- IpSecBackend      strongSwan CharonVpnService
        `-- OpenVpnBackend    OpenVPN native core
```

## Phase 1: controller + AmneziaWG

- [x] Write this roadmap.
- [x] Move repository license to GPL-3.0 and update notices.
- [x] Add `Transport`, `TunnelProfile`, and `TunnelStatus` to `bamdad-core`.
- [x] Add `Profile.transport` and `Profile.raw_config` with serde defaults so
      existing stores keep loading.
- [x] Update profile rows/labels for WireGuard/AmneziaWG.
- [x] Remove the Xray/sing-box picker from the import modal and auto-detect:
      native `.conf`, proxy JSON shape, and link imports (Xray default).
- [x] Add `.conf` detection and import:
      - `[Interface]` + `PrivateKey` -> WireGuard
      - AmneziaWG keys (`Jc`, `Jmin`, `Jmax`, `S1`..`S4`, `H1`..`H4`,
        `I1`..`I5`) -> AmneziaWG
      - endpoint/address/port parsed for display and ping rows.
- [x] Vendor `amneziawg-android` (pinned) as an Android library module
      (`amneziawg-tunnel` local source module).
- [x] Add Kotlin `TunnelBackend`, `TunnelController`, and
      `AmneziaWgBackend`.
- [ ] Add `ProxyBackend` to the controller so proxy and native transports
      share the same switch path.
- [x] Route `Transport::AmneziaWg` from Rust through the controller.
- [x] Normalize native tunnel state and traffic into Rust events.
- [ ] Forward native backend logs into the Rust log buffer.
- [x] Make the import dialog accept multiline `.conf` text.
- [x] Import from a file or ZIP, with password-protected ZIP support and an
      in-app password prompt.
- [x] Trim UTF-8 BOM whitespace before native config parsing.
- [x] Resolve WireGuard/AmneziaWG hostname endpoints before connecting, add
      native transport logs, and add a handshake-timeout watchdog.
- [x] Exclude the WireGuard endpoint IP from both the VPN routes and the
      `AllowedIPs` set so the first handshake cannot loop into the tunnel
      before the socket is protected (works on every Android API).
- [x] Fix the CIDR complement: the previous split still included the endpoint
      in one generated sibling route, so the handshake looped despite the log
      claiming the endpoint was excluded.
- [x] Recognize `.ovpn` files during import and show them as OpenVPN profiles
      (backend still pending).
- [x] Build in CI with recursive submodules and Go available.
- [x] Use the official `com.wireguard.android:tunnel` backend for vanilla
      WireGuard; keep `amneziawg-android` for AmneziaWG.
- [x] Rename the AmneziaWG native library so both backends can coexist.
- [x] Smoke-test WireGuard end to end against a real WireGuard server:
      handshake succeeds, HTTP traffic passes through the native TUN, and the
      TUN is released on disconnect.
- [ ] Smoke-test an explicit AmneziaWG config. The backend is the same
      `amneziawg-android` implementation, but the test server needs AWG
      obfuscation options.

## Phase 2: IPSec

- [ ] Vendor strongSwan and build `libcharon.so` per ABI.
- [ ] Add `IpSecBackend` around `CharonVpnService` / `VpnStateService`.
- [ ] Import `.sswan` and/or expose manual IKEv2 fields.
- [ ] Normalize state and byte counters.
- [ ] Test against a strongSwan test server.

## Phase 3: OpenVPN

- [x] Licensing: use OpenVPN 3 Core under its MPL-2.0 option, which is
      compatible with GPL-3.0. No AGPL change is needed.
- [x] Pin upstream `OpenVPN/openvpn3` at `release/3.11.8` (MPL-2.0 submodule).
- [x] Build the OpenVPN 3 native dependencies (asio, lz4, mbedTLS) for
      arm64-v8a and x86_64 (`scripts/build-openvpn3-deps.sh`).
- [x] Add the `AndroidTunBuilder` JNI adapter and `OpenVpnBackend` that supplies
      the VpnService TUN directly.
- [ ] Import `.ovpn` with inline CA/cert/key/tls-auth/tls-crypt. (recognition
      and inline profiles work; external file references are not merged yet)
- [ ] Add credential prompt for `auth-user-pass`.
- [ ] Test against an OpenVPN test server.

## Phase 6: userspace native hops

- [x] Vendor wireproxy `v1.1.3` (ISC) and build `bamdad-wg` for host + Android
      (`scripts/build-wireguard-proxy.sh`, `libbamdad_wg.so`).
- [x] Host smoke: real WireGuard server -> userspace helper -> SOCKS -> HTTPS.
- [x] App integration on desktop: a vanilla WireGuard profile runs through
      `bamdad-wg` and feeds the proxy/system-proxy path.
- [ ] Android integration: blocked by Android's app seccomp policy killing the
      Go helper with SIGSYS (Go runtime/landlock syscalls). Android keeps the
      VpnService backend until this is solved.
- [ ] Use `bamdad-wg` as a chain hop in the hop supervisor.
- [ ] AmneziaWG variant (wireproxy-awg / amneziawg-go) and license check.

## Phase 5: client-side chains

- [x] `Profile.chain`: an ordered list of proxy hops (entry first, exit last).
- [x] Xray chain config via `streamSettings.sockopt.dialerProxy` (26.x syntax).
- [x] sing-box chain config via outbound `detour`.
- [x] Chain editor dialog: add/remove/reorder hops, same-core candidates only.
- [x] Connect path validates hops (proxy, same core, unchained) and logs the plan.
- [ ] Mixed-core chains (Xray ↔ sing-box) via local SOCKS bridges.
- [ ] Chain-aware latency probes (probe the full path, not just the exit).
- [ ] Routing rules: send domains/apps through different chains.
- [ ] Native hops (`bamdad-wg` userspace SOCKS helper, then OpenVPN).

## Phase 4: unified UX

- [ ] One notification across transports (profile name, totals, accent icon).
- [ ] Kill-switch sinkhole when no backend owns the VPN slot.
- [ ] Per-app/DNS/bypass-CIDR patching for every native backend.
- [ ] Protocol-aware latency display.
- [ ] Failover/pool rotation across transports.
- [ ] In-app open-source licenses screen for GPL/Apache/MPL components.

## Notes

- `amneziawg-android` GoBackend owns its own VpnService and handles both
  vanilla WireGuard and AmneziaWG configs.
- strongSwan owns its own VpnService via `CharonVpnService`.
- Do not try to share one TUN fd across the native libraries; the unified
  layer is the controller/backend interface.
- Android only allows one active VPN. Backend switches must stop the old
  backend and wait for its service to release the slot before starting the
  next one.

## Phase 1 follow-ups

- [x] Test against a real WireGuard server (handshake + HTTP traffic), using
      a quick-style config with `MTU = 1300` and a split `AllowedIPs` list.
- [ ] Test against a real AmneziaWG server.
- [ ] OpenVPN/strongSwan backends; `.ovpn`/`.sswan` are recognized during
      import but cannot connect yet.
- [ ] Unify the native notification with the existing accent/profile/totals
      notification.
- [ ] Move per-app/DNS/bypass-CIDR patching into `TunnelProfile` and apply it
      before `Config.parse`.

## Import notes

- The import modal no longer asks for Xray/sing-box. Native `.conf` files are
  detected first; JSON is classified by shape (`type` = sing-box,
  `protocol` = Xray); links default to Xray.
- File/ZIP import accepts `.conf`, `.ovpn`, `.sswan`, `.json`, and `.txt`.
  ZIP extraction uses zip4j and prompts for a password in-app.
