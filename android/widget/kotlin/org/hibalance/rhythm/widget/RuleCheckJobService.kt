package org.hibalance.rhythm.widget

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.util.Log
import java.io.File

/** Periodically evaluates rules while the UI is not running. */
class RuleCheckJobService : JobService() {
    override fun onStartJob(params: JobParameters): Boolean {
        val configDir = File(filesDir, "rhythm").absolutePath
        val context = applicationContext
        Thread {
            try {
                if (RhythmNativeBridge.ensureLoaded()) {
                    RhythmNativeBridge.evaluateRules(configDir, context)
                } else {
                    Log.w(TAG, "Native library is unavailable")
                }
            } catch (e: Throwable) {
                // A background check must never take down the app process.
                Log.e(TAG, "Rule check failed", e)
            } finally {
                jobFinished(params, false)
            }
        }.start()
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean = true

    companion object {
        private const val TAG = "RhythmRuleCheck"
    }
}

object RuleCheckScheduler {
    private const val JOB_ID = 0x5248544D

    @JvmStatic
    fun schedule(context: Context) {
        val scheduler = context.getSystemService(JobScheduler::class.java) ?: return
        if (scheduler.getPendingJob(JOB_ID) != null) return

        val component = ComponentName(context, RuleCheckJobService::class.java)
        val job = JobInfo.Builder(JOB_ID, component)
            .setPeriodic(15 * 60 * 1000L)
            .setPersisted(true)
            .build()
        try {
            scheduler.schedule(job)
        } catch (_: Exception) {
            // Persisted jobs require RECEIVE_BOOT_COMPLETED; the manifest has it.
        }
    }
}
