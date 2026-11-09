package com.mehrnet.bamdad

import android.app.NativeActivity
import android.os.Bundle
import android.util.Log

class MainActivity : NativeActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        instance = this
        Log.i(TAG, "onCreate")
        super.onCreate(savedInstanceState)
    }

    companion object {
        private const val TAG = "Bamdad"

        @Volatile
        private var instance: MainActivity? = null

        @JvmStatic
        fun nativeLibraryDir(): String =
            instance?.applicationInfo?.nativeLibraryDir ?: ""

        @JvmStatic
        fun contentReady() {
            instance?.reportFullyDrawn()
            Log.i(TAG, "content ready")
        }
    }
}
