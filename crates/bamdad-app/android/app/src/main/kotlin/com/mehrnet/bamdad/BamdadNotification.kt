package com.mehrnet.bamdad

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.PorterDuff
import android.graphics.drawable.Icon
import android.os.Build
import android.util.Log

/**
 * One notification for every transport. The proxy VpnService, the native
 * tunnel service, and the kill-switch sinkhole all publish through this
 * object, so the shade always shows the same channel, icon, accent color, and
 * profile title no matter which backend owns the VPN slot.
 */
object BamdadNotification {
    const val CHANNEL_ID = "bamdad_vpn"
    const val TUNNEL_ID = 2001
    const val KILL_SWITCH_ID = 2002

    private const val TAG = "BamdadNotify"

    fun ensureChannel(context: Context) {
        val manager = manager(context)
        val channel = NotificationChannel(
            CHANNEL_ID,
            "VPN",
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Tunnel status, profile, and traffic totals"
            setShowBadge(false)
        }
        manager.createNotificationChannel(channel)
        // Channels used by earlier builds; one channel replaces them all.
        runCatching { manager.deleteNotificationChannel("bamdad_tunnel") }
        runCatching { manager.deleteNotificationChannel("bamdad_killswitch") }
    }

    fun update(
        context: Context,
        id: Int,
        title: String,
        text: String,
        transport: String,
        accent: String,
    ) {
        ensureChannel(context)
        try {
            manager(context).notify(id, build(context, title, text, transport, accent))
        } catch (error: Throwable) {
            Log.w(TAG, "notification update failed: ${error.message}")
        }
    }

    fun startForeground(
        service: Service,
        id: Int,
        title: String,
        text: String,
        transport: String,
        accent: String,
    ) {
        ensureChannel(service)
        val notification = build(service, title, text, transport, accent)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            service.startForeground(
                id,
                notification,
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            )
        } else {
            service.startForeground(id, notification)
        }
    }

    fun cancel(context: Context, id: Int) {
        runCatching { manager(context).cancel(id) }
    }

    fun accentColor(name: String): Int = when (name.lowercase()) {
        "blue" -> 0xFF60A5FA.toInt()
        "red" -> 0xFFF87171.toInt()
        "green" -> 0xFF4ADE80.toInt()
        "purple" -> 0xFFA78BFA.toInt()
        "orange" -> 0xFFFB923C.toInt()
        "pink" -> 0xFFF472B6.toInt()
        "gray" -> 0xFF94A3B8.toInt()
        else -> 0xFF38BDF8.toInt()
    }

    private fun build(
        context: Context,
        title: String,
        text: String,
        transport: String,
        accent: String,
    ): Notification {
        val pending = PendingIntent.getActivity(
            context,
            0,
            Intent(context, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val color = accentColor(accent)
        return Notification.Builder(context, CHANNEL_ID)
            .setSmallIcon(Icon.createWithBitmap(accentIcon(context, color)))
            .setColor(color)
            .setContentTitle(title.ifBlank { "Bamdad" })
            .setContentText(text)
            .setSubText(transport)
            .setContentIntent(pending)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
    }

    private fun manager(context: Context): NotificationManager =
        context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    private fun accentIcon(context: Context, color: Int): Bitmap {
        val density = context.resources.displayMetrics.density
        val size = (24 * density).toInt().coerceAtLeast(24)
        val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        val drawable = context.getDrawable(R.drawable.ic_moon)
        if (drawable != null) {
            drawable.setBounds(0, 0, size, size)
            drawable.setColorFilter(color, PorterDuff.Mode.SRC_IN)
            drawable.draw(canvas)
            drawable.setColorFilter(null)
        }
        return bitmap
    }
}
