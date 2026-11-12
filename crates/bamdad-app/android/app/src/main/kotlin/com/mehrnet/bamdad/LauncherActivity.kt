package com.mehrnet.bamdad

import android.app.Activity
import android.content.Intent
import android.os.Bundle

/**
 * Tiny launcher shim used by the accent activity-aliases.
 *
 * The aliases target this no-display activity so the alias is no longer
 * running when the real NativeActivity switches icons. It also means the
 * launcher click never creates a visible trampoline window.
 */
class LauncherActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // NEW_TASK (without CLEAR_TOP) brings an existing MainActivity task
        // forward with its snapshot and state intact instead of recreating it.
        // Using ActivityManager.moveTaskToFront here instead bypasses the task
        // snapshot and flashes a black frame until the native surface redraws.
        val intent = Intent(this, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        startActivity(intent)
        // Theme.NoDisplay requires the activity to finish before onResume.
        finish()
    }
}
