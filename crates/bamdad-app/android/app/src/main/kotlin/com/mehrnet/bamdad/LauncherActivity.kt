package com.mehrnet.bamdad

import android.app.Activity
import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.util.Log

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
        if (bringExistingTaskToFront()) {
            finish()
            return
        }
        // No existing task: create the real task. NEW_TASK alone (without
        // CLEAR_TOP) keeps later launches from destroying and recreating
        // MainActivity, which would replay the splash screen.
        val intent = Intent(this, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        startActivity(intent)
        // Do not call finish() here. The activity is noHistory, so Android
        // removes it once MainActivity is visible. Finishing immediately can
        // race the task switch and leave the launcher on top instead.
    }

    /** Brings the already-running app task forward without recreating it. */
    private fun bringExistingTaskToFront(): Boolean {
        return try {
            val manager = getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
            val main = MainActivity::class.java.name
            val task = manager.appTasks.firstOrNull { candidate ->
                val base = candidate.taskInfo.baseActivity?.className
                val top = candidate.taskInfo.topActivity?.className
                base == main || top == main
            } ?: return false
            task.moveToFront()
            Log.i("Bamdad", "launcher brought existing task to front")
            true
        } catch (error: Throwable) {
            Log.w("Bamdad", "launcher task restore failed: ${error.message}")
            false
        }
    }
}
