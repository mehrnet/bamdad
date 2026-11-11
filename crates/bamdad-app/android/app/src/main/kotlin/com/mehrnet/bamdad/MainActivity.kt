package com.mehrnet.bamdad

import android.Manifest
import android.app.NativeActivity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
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
        }
        super.onActivityResult(requestCode, resultCode, data)
    }

    companion object {
        private const val TAG = "Bamdad"
        private const val VPN_REQUEST = 1001
        private const val NOTIFICATION_REQUEST = 1002

        @Volatile
        private var instance: MainActivity? = null

        @Volatile
        private var pendingVpnPort: Int = 10808

        @JvmStatic
        fun nativeLibraryDir(): String =
            instance?.applicationInfo?.nativeLibraryDir ?: ""

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

        /** Forwards Rust tunnel output to logcat. */
        @JvmStatic
        fun logLine(line: String) {
            Log.i("BamdadTunnel", line)
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
