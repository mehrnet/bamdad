# Third-party notices

## tun2socks

- Project: https://github.com/xjasonlyu/tun2socks
- License: MIT
- Built for Android by `scripts/fetch-android-tun2socks.sh` and shipped as
  `libbamdad_tun2socks.so` in the APK.
- Copyright (c) 2019 Jason Lyu

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## MehrNet BGP country database

- Project: https://github.com/mehrnet/bgp-api
- File: `crates/bamdad-app/assets/country.mncc` (MNCC format)
- Derived from RIR allocations and geofeeds; used for offline IP-to-country lookup.
- Refresh with `scripts/fetch-country-db.sh`.

## Twemoji

- Project: https://github.com/jdecked/twemoji
- License: CC-BY 4.0
- Emoji SVGs are embedded in `crates/bamdad-app/assets/twemoji.zip` and rendered
  as color images in profile names.

## Noto Emoji

- Project: https://github.com/googlefonts/noto-emoji
- License: SIL Open Font License 1.1
- Embedded at `crates/bamdad-app/assets/NotoEmoji.ttf` for emoji fallback.

## Proxy cores

Xray-core is MPL-2.0 and sing-box is GPL-3.0. They are shipped as separate
executables; include their licenses and data notices in release artifacts.

## amneziawg-android

- Project: https://github.com/amnezia-vpn/amneziawg-android
- License: Apache-2.0
- Used on Android for the WireGuard and AmneziaWG userspace backend.

## strongSwan

- Project: https://github.com/strongswan/strongswan
- License: GPL-2.0-or-later
- Will be used on Android for the IPSec/IKEv2 backend.

## OpenVPN 3 Core Library

- Project: https://github.com/OpenVPN/openvpn3
- Version: `release/3.11.8` (git submodule at `crates/bamdad-app/android/vendor/openvpn3`)
- License: MPL-2.0 (the library is dual-licensed AGPL-3.0-only /
  MPL-2.0; Bamdad uses it under MPL-2.0, which is compatible with GPL-3.0)
- Used on Android by the `openvpn3-tunnel` module and the `OpenVpnService`
  VpnService backend. The tun device and socket protection are implemented by
  Bamdad; no ics-openvpn code is used.

## OpenVPN 3 native dependencies

Built by `scripts/build-openvpn3-deps.sh` and linked statically into
`libbamdad_openvpn3.so`:

- asio 1.24.0 — Boost Software License 1.0, with the OpenVPN patches.
- mbedTLS 2.28.4 — Apache-2.0.
- LZ4 1.8.3 — BSD 2-Clause.

## ics-openvpn

- Project: https://github.com/schwabe/ics-openvpn
- License: GPL-2.0-only with additional terms
- Not used. Its OpenVPN 3 fork is AGPL-3.0-only; Bamdad uses upstream OpenVPN 3
  under MPL-2.0 instead.

## wireproxy

- Project: https://github.com/whyvl/wireproxy (module `github.com/windtf/wireproxy`)
- Version: `v1.1.3` (git submodule at `vendor/wireproxy`)
- License: ISC
- Built by `scripts/build-wireguard-proxy.sh` as `bamdad-wg` (desktop) and
  `libbamdad_wg.so` (Android). It runs a userspace WireGuard session
  (`wireguard-go` + gVisor netstack) and exposes it as a local SOCKS5 proxy, so
  WireGuard configs can be used as a chain hop without the Android VpnService
  slot.

## zip4j

- Project: https://github.com/srikanth-lingala/zip4j
- License: Apache-2.0
- Used by the Android file/ZIP importer for password-protected archives.
