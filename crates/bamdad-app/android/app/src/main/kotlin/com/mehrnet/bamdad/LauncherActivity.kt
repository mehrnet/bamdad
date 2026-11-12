package com.mehrnet.bamdad

import android.content.Intent
import android.os.Bundle
import android.app.Activity

/**
 * Tiny launcher shim used by the accent activity-aliases.
 *
 * Keeping the aliases on a short-lived normal activity means the alias is no
 * longer running when the real NativeActivity switches icons, so changing the
 * enabled alias can't tear down the running task.
 */
class LauncherActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val intent = Intent(this, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        }
        startActivity(intent)
        // Never call finishAndRemoveTask here: MainActivity usually lands in
        // the same task as this shim, and removing the task would also remove
        // the app that was just launched.
        finish()
    }
}
