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
- [x] Build in CI with recursive submodules and Go available.
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

- [ ] Decide the licensing path:
      - ics-openvpn is GPL-2.0-only and cannot be linked into a GPL-3.0 app.
      - preferred: separate OpenVPN native executable + our own management
        client, or a GPL-3.0-compatible core.
- [ ] Add `OpenVpnBackend`.
- [ ] Import `.ovpn` with inline CA/cert/key/tls-auth/tls-crypt.
- [ ] Add credential prompt for `auth-user-pass`.
- [ ] Test against an OpenVPN test server.

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

- [x] Test against a real WireGuard server (handshake + HTTP traffic).
- [ ] Test against a real AmneziaWG server.
- [ ] Add a file picker for `.conf` files in addition to paste.
- [ ] Unify the native notification with the existing accent/profile/totals
      notification.
- [ ] Move per-app/DNS/bypass-CIDR patching into `TunnelProfile` and apply it
      before `Config.parse`.
