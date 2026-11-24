package org.hibalance.rhythm.widget

import android.content.Context

object RhythmNativeBridge {
    external fun applyCommand(json: String)
    external fun evaluateRules(configDir: String, context: Context)

    /** Loads the native library when the process was not started by the UI. */
    fun ensureLoaded(): Boolean =
        try {
            System.loadLibrary("rhythm_lib")
            true
        } catch (_: UnsatisfiedLinkError) {
            false
        }
}
