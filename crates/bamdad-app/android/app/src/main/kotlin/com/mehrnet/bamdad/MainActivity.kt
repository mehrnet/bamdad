package com.mehrnet.bamdad

import android.Manifest
import android.app.NativeActivity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.util.Log
import android.widget.Toast

class MainActivity : NativeActivity() {
    // Registered by Rust from android_main via RegisterNatives.
    private external fun startTunnelNative(fd: Int, socksPort: Int): Boolean
    private external fun stopTunnelNative()

    override fun onCreate(savedInstanceState: Bundle?) {
        instance = this
        Log.i(TAG, "onCreate")
        super.onCreate(savedInstanceState)
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == VPN_REQUEST) {
            if (resultCode == RESULT_OK) {
                startVpnService(this, pendingVpnPort)
            } else {
                Log.w(TAG, "VPN consent denied")
            }
        } else if (requestCode == IMPORT_FILE_REQUEST) {
            if (resultCode == RESULT_OK) {
                data?.data?.let { FileImporter.handle(this, it) }
            }
        } else if (requestCode == VPN_REQUEST_NATIVE) {
            val json = pendingNativeTunnel
            pendingNativeTunnel = null
            if (resultCode == RESULT_OK && json != null) {
                startNativeTunnelService(this, json)
            } else {
                Log.w(TAG, "native VPN consent denied")
                nativeTunnelEvent(
                    """{"state":"error","error":"VPN permission denied"}""",
                )
            }
        }
        super.onActivityResult(requestCode, resultCode, data)
    }

    companion object {
        private const val TAG = "Bamdad"
        private const val VPN_REQUEST = 1001
        private const val NOTIFICATION_REQUEST = 1002
        private const val VPN_REQUEST_NATIVE = 1003
        const val IMPORT_FILE_REQUEST = 1004

        @Volatile
        private var instance: MainActivity? = null

        @Volatile
        private var pendingVpnPort: Int = 10808

        @Volatile
        private var pendingNativeTunnel: String? = null

        /** Registered by Rust; forwards native backend state into the session. */
        @JvmStatic
        external fun nativeTunnelEvent(json: String)

        /** Registered by Rust; imports one or more config bodies. */
        @JvmStatic
        external fun nativeImportConfigs(group: String, filesJson: String)

        /** Registered by Rust; adds a native transport log line to the app log. */
        @JvmStatic
        external fun nativeLog(line: String)

        @JvmStatic
        fun nativeLibraryDir(): String =
            instance?.applicationInfo?.nativeLibraryDir ?: ""

        /**
         * Binds a probe socket to the physical network under the active VPN so
         * latency checks measure the raw path, not the tunnel.
         */
        @JvmStatic
        fun bindSocketToPhysicalNetwork(fd: Int): Boolean {
            val activity = instance ?: return false
            return try {
                val manager =
                    activity.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
                val active = manager.activeNetwork ?: return false
                val capabilities = manager.getNetworkCapabilities(active)
                val target = capabilities?.underlyingNetworks?.firstOrNull() ?: active
                val descriptor = ParcelFileDescriptor.fromFd(fd)
                try {
                    target.bindSocket(descriptor.fileDescriptor)
                    true
                } finally {
                    descriptor.close()
                }
            } catch (error: Throwable) {
                Log.w(TAG, "could not bind socket $fd to the physical network: ${error.message}")
                false
            }
        }

        /** Version of the bundled WireGuard/AmneziaWG userspace backend. */
        @JvmStatic
        fun wireGuardVersion(): String =
            try {
                instance?.let { org.amnezia.awg.backend.GoBackend(it).version } ?: "unavailable"
            } catch (error: Throwable) {
                Log.w(TAG, "could not read the WireGuard version", error)
                "unavailable"
            }

        /** Version of the bundled OpenVPN 3 core library. */
        @JvmStatic
        fun openVpnVersion(): String =
            try {
                OpenVpnNative.nativeVersion()
            } catch (error: Throwable) {
                Log.w(TAG, "could not read the OpenVPN version", error)
                "unavailable"
            }

        @JvmStatic
        fun contentReady() {
            instance?.reportFullyDrawn()
            Log.i(TAG, "content ready")
        }

        /**
         * Called from Rust when the core reaches Running. Handles the
         * VpnService consent flow on the main thread, then starts the tunnel.
         */
        @JvmStatic
        fun startVpn(socksPort: Int) {
            val activity = instance ?: return
            Handler(Looper.getMainLooper()).post {
                pendingVpnPort = socksPort
                requestNotificationPermission(activity)
                val consent = VpnService.prepare(activity)
                if (consent != null) {
                    activity.startActivityForResult(consent, VPN_REQUEST)
                } else {
                    startVpnService(activity, socksPort)
                }
            }
        }

        @JvmStatic
        fun stopVpn() {
            val activity = instance ?: return
            Log.i(TAG, "stopVpn requested")
            Handler(Looper.getMainLooper()).post {
                BamdadVpnService.stop(activity)
            }
        }

        /**
         * Starts a native WireGuard/AmneziaWG backend. The backend owns its
         * own VpnService; the existing proxy VpnService must already be
         * stopped by Rust.
         */
        @JvmStatic
        fun startNativeTunnel(json: String) {
            val activity = instance ?: return
            Handler(Looper.getMainLooper()).postDelayed({
                requestNotificationPermission(activity)
                val consent = VpnService.prepare(activity)
                if (consent != null) {
                    pendingNativeTunnel = json
                    activity.startActivityForResult(consent, VPN_REQUEST_NATIVE)
                } else {
                    startNativeTunnelService(activity, json)
                }
            }, 1200)
        }

        @JvmStatic
        fun openImportFile() {
            val activity = instance ?: return
            Handler(Looper.getMainLooper()).post {
                FileImporter.pick(activity)
            }
        }

        @JvmStatic
        fun stopNativeTunnel() {
            val activity = instance ?: return
            Handler(Looper.getMainLooper()).post {
                val intent = Intent(activity, TunnelService::class.java).apply {
                    action = TunnelService.ACTION_STOP
                }
                activity.startService(intent)
            }
        }

        /** Forwards Rust tunnel output to logcat. */
        @JvmStatic
        fun logLine(line: String) {
            Log.i("BamdadTunnel", line)
        }

        /** Enables the launcher alias for the active accent. Safe because the
         * aliases target the short-lived LauncherActivity shim, not this
         * NativeActivity. */
        @JvmStatic
        fun applyLauncherAccent(accent: String) {
            val activity = instance ?: return
            val pkg = activity.packageName
            val aliases = mapOf(
                "cyan" to "$pkg.LauncherCyan",
                "blue" to "$pkg.LauncherBlue",
                "red" to "$pkg.LauncherRed",
                "green" to "$pkg.LauncherGreen",
                "purple" to "$pkg.LauncherPurple",
                "orange" to "$pkg.LauncherOrange",
                "pink" to "$pkg.LauncherPink",
                "gray" to "$pkg.LauncherGray",
            )
            val chosen = aliases[accent] ?: aliases["cyan"]!!
            for ((_, component) in aliases) {
                val state = if (component == chosen) {
                    android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_ENABLED
                } else {
                    android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DISABLED
                }
                try {
                    activity.packageManager.setComponentEnabledSetting(
                        android.content.ComponentName(activity.packageName, component),
                        state,
                        android.content.pm.PackageManager.DONT_KILL_APP,
                    )
                } catch (error: Throwable) {
                    Log.w(TAG, "launcher icon update failed: ${error.message}")
                }
            }
        }

        /** Updates the foreground VPN notification with the active config and totals. */
        @JvmStatic
        fun updateVpnNotification(profile: String, down: String, up: String, accent: String) {
            BamdadVpnService.updateStats(profile, down, up, accent)
        }

        /** Resolves the current Android system theme for the appearance setting. */
        @JvmStatic
        fun systemTheme(): String {
            val uiMode = instance?.resources?.configuration?.uiMode ?: 0
            return if ((uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
                Configuration.UI_MODE_NIGHT_YES) "dark" else "light"
        }

        /** Reads plain text from the clipboard for the import dialog. */
        @JvmStatic
        fun readClipboard(): String {
            val activity = instance ?: return ""
            return try {
                val clipboard =
                    activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                val clip = clipboard.primaryClip ?: return ""
                if (clip.itemCount == 0) return ""
                clip.getItemAt(0).coerceToText(activity).toString()
            } catch (error: Throwable) {
                Log.w(TAG, "readClipboard failed", error)
                ""
            }
        }

        /** Copies the current Settings log to the clipboard. */
        @JvmStatic
        fun copyLogs(text: String) {
            val activity = instance ?: return
            Handler(Looper.getMainLooper()).post {
                val clipboard =
                    activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                clipboard.setPrimaryClip(ClipData.newPlainText("Bamdad logs", text))
                Toast.makeText(activity, "Bamdad logs copied", Toast.LENGTH_SHORT).show()
            }
        }

        /** Opens the Android share sheet so logs can be sent or saved. */
        @JvmStatic
        fun shareLogs(text: String) {
            val activity = instance ?: return
            Handler(Looper.getMainLooper()).post {
                val intent = Intent(Intent.ACTION_SEND).apply {
                    type = "text/plain"
                    putExtra(Intent.EXTRA_SUBJECT, "Bamdad logs")
                    putExtra(Intent.EXTRA_TEXT, text)
                }
                activity.startActivity(Intent.createChooser(intent, "Share Bamdad logs"))
            }
        }

        /** Starts tun2socks against the VpnService descriptor (Rust native). */
        @JvmStatic
        fun startTunnel(fd: Int, socksPort: Int): Boolean =
            try {
                instance?.startTunnelNative(fd, socksPort) ?: false
            } catch (error: Throwable) {
                Log.e(TAG, "native startTunnel failed", error)
                false
            }

        /** Stops the tun2socks child process (Rust native). */
        @JvmStatic
        fun stopTunnel() {
            try {
                instance?.stopTunnelNative()
            } catch (error: Throwable) {
                Log.e(TAG, "native stopTunnel failed", error)
            }
        }

        private fun startVpnService(context: Context, socksPort: Int) {
            BamdadVpnService.start(context, socksPort)
        }

        private fun startNativeTunnelService(context: Context, json: String) {
            val intent = Intent(context, TunnelService::class.java).apply {
                action = TunnelService.ACTION_START
                putExtra(TunnelService.EXTRA_PROFILE, json)
            }
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(intent)
                } else {
                    context.startService(intent)
                }
            } catch (error: Throwable) {
                Log.e(TAG, "could not start native tunnel service", error)
                nativeLog("BamdadTunnel: could not start service: ${error.message}")
                nativeTunnelEvent(
                    """{"state":"error","error":"Could not start the tunnel service: ${error.message}"}""",
                )
            }
        }

        private fun requestNotificationPermission(activity: MainActivity) {
            if (
                Build.VERSION.SDK_INT >= 33 &&
                activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED
            ) {
                activity.requestPermissions(
                    arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                    NOTIFICATION_REQUEST,
                )
            }
        }
    }
}
