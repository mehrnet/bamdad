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
        // NEW_TASK alone reuses an existing MainActivity task and brings it
        // back to the front with its state intact. CLEAR_TOP would instead
        // destroy and recreate MainActivity, replaying the splash screen and
        // losing the already-rendered UI.
        val intent = Intent(this, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        startActivity(intent)
        // Do not call finish() here. The activity is noHistory, so Android
        // removes it once MainActivity is visible. Finishing immediately can
        // race the task switch and leave the launcher on top instead.
    }
}
