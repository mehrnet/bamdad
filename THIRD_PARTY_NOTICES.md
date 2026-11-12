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

## ics-openvpn

- Project: https://github.com/schwabe/ics-openvpn
- License: GPL-2.0-only with additional terms
- OpenVPN integration is pending a GPL-3.0-compatible licensing decision.

## zip4j

- Project: https://github.com/srikanth-lingala/zip4j
- License: Apache-2.0
- Used by the Android file/ZIP importer for password-protected archives.
