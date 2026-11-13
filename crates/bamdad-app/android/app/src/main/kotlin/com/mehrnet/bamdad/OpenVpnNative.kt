package com.mehrnet.bamdad

import androidx.annotation.Keep

/**
 * JNI surface for the OpenVPN 3 core. The native library is built from
 * `openvpn3-tunnel` and keeps the core session on its own worker thread.
 */
@Keep
object OpenVpnNative {
    init {
        System.loadLibrary("bamdad_openvpn3")
    }

    /** Binds the VpnService instance that owns the tun device. */
    @JvmStatic
    external fun nativeAttach(service: OpenVpnService)

    /** Starts a session; returns immediately, events arrive on the service. */
    @JvmStatic
    external fun nativeConnect(
        profile: String,
        username: String,
        password: String,
        keyPassword: String,
    )

    /** Requests the running session to stop. */
    @JvmStatic
    external fun nativeStop()

    /** Returns `{"in":<rx>,"out":<tx>}` for the current session. */
    @JvmStatic
    external fun nativeStats(): String

    /** Parses a profile without connecting; returns a small JSON summary. */
    @JvmStatic
    external fun nativeEval(profile: String): String
}
