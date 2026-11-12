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
- [ ] Move repository license to GPL-3.0 and update notices.
- [ ] Add `Transport`, `TunnelProfile`, and `TunnelStatus` to `bamdad-core`.
- [ ] Add `Profile.transport` and `Profile.raw_config` with serde defaults so
      existing stores keep loading.
- [ ] Update profile rows/labels for WireGuard/AmneziaWG.
- [ ] Add `.conf` detection and import:
      - `[Interface]` + `PrivateKey` -> WireGuard
      - AmneziaWG keys (`Jc`, `Jmin`, `Jmax`, `S1`..`S4`, `H1`..`H4`,
        `I1`..`I5`) -> AmneziaWG
      - endpoint/address/port parsed for display and ping rows.
- [ ] Vendor `amneziawg-android` (pinned) as an Android library module.
- [ ] Add Kotlin `TunnelBackend`, `TunnelController`, `ProxyBackend`, and
      `AmneziaWgBackend`.
- [ ] Route `Transport::AmneziaWg` from Rust through the controller.
- [ ] Normalize native tunnel state/traffic/log events into Rust events.
- [ ] Make the import dialog accept multiline `.conf` text.
- [ ] Build in CI with recursive submodules and Go available.
- [ ] Smoke-test WireGuard and AmneziaWG profiles.

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
