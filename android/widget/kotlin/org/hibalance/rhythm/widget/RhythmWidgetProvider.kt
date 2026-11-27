package org.hibalance.rhythm.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.util.Log
import android.view.View
import android.widget.RemoteViews
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import org.HiBalance.rhythm.R
import org.json.JSONObject
import kotlin.math.roundToInt

class RhythmWidgetProvider : AppWidgetProvider() {
    private data class WidgetPalette(
        val background: Int,
        val foreground: Int,
        val alternate: Int,
        val control: Int,
        val controlForeground: Int,
    )

    private fun palette(context: Context, state: WidgetState): WidgetPalette {
        val dark = when (state.theme) {
            "Dark" -> true
            "Light" -> false
            else -> context.resources.configuration.uiMode and
                Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
        }
        return if (dark) {
            WidgetPalette(
                background = Color.parseColor("#1B1B1B"),
                foreground = Color.parseColor("#C4C4C4"),
                alternate = Color.parseColor("#2E2E2E"),
                control = Color.parseColor("#262626"),
                controlForeground = Color.parseColor("#C4C4C4"),
            )
        } else {
            WidgetPalette(
                background = Color.parseColor("#D7D7D7"),
                foreground = Color.parseColor("#292929"),
                alternate = Color.parseColor("#F2F2F2"),
                control = Color.parseColor("#C7C7C7"),
                controlForeground = Color.parseColor("#3D3D3D"),
            )
        }
    }

    override fun onUpdate(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetIds: IntArray,
    ) {
        val state = WidgetStore.readState(context)
        for (id in appWidgetIds) {
            val options = appWidgetManager.getAppWidgetOptions(id)
            val width = options.getInt(AppWidgetManager.OPTION_APPWIDGET_MIN_WIDTH, 110)
            appWidgetManager.updateAppWidget(id, buildViews(context, state, width >= 250))
        }
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == COMMAND_ACTION) {
            val action = intent.getStringExtra(EXTRA_COMMAND) ?: return
            val at = System.currentTimeMillis() / 1000
            try {
                // Queue first so a state-file failure cannot lose the action.
                val id = WidgetStore.appendCommand(context, action, at)
                val state = WidgetStore.readState(context)
                applyOptimistic(state, action)
                WidgetStore.writeState(context, state)

                val command = JSONObject()
                    .put("id", id)
                    .put("action", action)
                    .put("at", at)
                    .toString()
                try {
                    RhythmNativeBridge.applyCommand(command)
                } catch (_: UnsatisfiedLinkError) {
                    // The app process is not alive; the command file is picked
                    // up on launch and replayed with the original timestamp.
                }
            } catch (e: Exception) {
                Log.e(TAG, "Failed to queue widget command $action", e)
            }
            updateAll(context)
            return
        }
        super.onReceive(context, intent)
    }

    override fun onAppWidgetOptionsChanged(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetId: Int,
        newOptions: Bundle,
    ) {
        val state = WidgetStore.readState(context)
        val width = newOptions.getInt(AppWidgetManager.OPTION_APPWIDGET_MIN_WIDTH, 110)
        appWidgetManager.updateAppWidget(appWidgetId, buildViews(context, state, width >= 250))
    }

    private fun applyOptimistic(state: WidgetState, action: String) {
        // The state file is only rewritten while the app runs. Add the time
        // that passed since the last write so a button press after a long gap
        // does not roll the displayed duration back.
        val elapsed = elapsedSinceUpdate(state)
        if (state.workActive) {
            if (state.workPaused) {
                state.pausedSeconds += elapsed
            } else {
                state.workSeconds += elapsed
            }
        }
        if (state.sleepActive) {
            state.sleepSeconds += elapsed
        }

        when (action) {
            "work-start" -> if (!state.workActive) {
                state.workActive = true
                state.workPaused = false
                state.pausedSeconds = 0
            }
            "work-pause" -> if (state.workActive && !state.workPaused) {
                state.workPaused = true
                state.pausedSeconds = 0
            }
            "work-resume" -> if (state.workActive && state.workPaused) {
                state.workPaused = false
                state.pausedSeconds = 0
            }
            "work-stop" -> if (state.workActive) {
                state.workActive = false
                state.workPaused = false
                state.pausedSeconds = 0
                // workSeconds is today's total, not the current session, so
                // it must not be reset when the timer stops.
            }
            "sleep-start" -> if (!state.sleepActive) {
                state.sleepActive = true
                state.sleepSeconds = 0
            }
            "sleep-wake" -> if (state.sleepActive) {
                state.sleepActive = false
                // While sleeping, sleepSeconds tracks the current night. Once
                // awake, the widget displays lastNightSeconds.
                state.lastNightSeconds = state.sleepSeconds
            }
        }
        state.updatedAt = nowText()
    }

    private fun elapsedSinceUpdate(state: WidgetState): Long {
        if (state.updatedAt.isEmpty()) return 0
        return try {
            val parsed = SimpleDateFormat(STATE_TIME_FORMAT, Locale.US).parse(state.updatedAt)
                ?: return 0
            ((System.currentTimeMillis() - parsed.time) / 1000).coerceAtLeast(0)
        } catch (_: Exception) {
            0
        }
    }

    private fun nowText(): String =
        SimpleDateFormat(STATE_TIME_FORMAT, Locale.US).format(Date())

    private fun buildViews(context: Context, state: WidgetState, wide: Boolean): RemoteViews {
        val views = RemoteViews(
            context.packageName,
            if (wide) R.layout.widget_4x2 else R.layout.widget_2x2,
        )

        val colors = palette(context, state)
        views.setInt(R.id.open_button, "setBackgroundColor", colors.background)
        listOf(R.id.work_status, R.id.work_time, R.id.sleep_status, R.id.sleep_time).forEach {
            views.setInt(it, "setTextColor", colors.foreground)
        }
        listOf(R.id.work_progress, R.id.sleep_progress).forEach {
            views.setInt(it, "setBackgroundColor", colors.alternate)
        }
        listOf(
            R.id.work_start,
            R.id.work_pause,
            R.id.work_resume,
            R.id.work_stop,
            R.id.sleep_start,
            R.id.sleep_wake,
        ).forEach {
            views.setInt(it, "setBackgroundColor", colors.control)
            views.setInt(it, "setTextColor", colors.controlForeground)
        }

        val elapsed = elapsedSinceUpdate(state)
        val pausedSeconds = if (state.workActive && state.workPaused) {
            state.pausedSeconds + elapsed
        } else {
            0
        }
        // The state file is only written while the app runs. Add the time since
        // the last write to an active timer so a refresh after a long gap does
        // not restart the Chronometer from a stale value.
        val workSeconds = if (state.workActive && !state.workPaused) {
            state.workSeconds + elapsed
        } else {
            state.workSeconds
        }
        val sleepSeconds = if (state.sleepActive) {
            state.sleepSeconds + elapsed
        } else {
            state.lastNightSeconds
        }
        views.setTextViewText(
            R.id.work_status,
            when {
                state.workActive && state.workPaused ->
                    "Paused ${formatDuration(pausedSeconds)}"
                state.workActive -> "Working"
                else -> "Not working"
            },
        )
        // Chronometer keeps ticking in the launcher process while the app is
        // not running, so an active timer stays accurate between updates.
        setChronometer(views, R.id.work_time, workSeconds, state.workActive && !state.workPaused)
        views.setTextViewText(
            R.id.sleep_status,
            if (state.sleepActive) "Sleeping" else "Awake",
        )
        setChronometer(views, R.id.sleep_time, sleepSeconds, state.sleepActive)

        views.setProgressBar(R.id.work_progress, 100, percent(workSeconds, state.workTarget), false)
        views.setProgressBar(R.id.sleep_progress, 100, percent(sleepSeconds, state.sleepTarget), false)

        views.setViewVisibility(R.id.work_start, if (state.workActive) View.GONE else View.VISIBLE)
        views.setViewVisibility(
            R.id.work_pause,
            if (state.workActive && !state.workPaused) View.VISIBLE else View.GONE,
        )
        views.setViewVisibility(
            R.id.work_resume,
            if (state.workActive && state.workPaused) View.VISIBLE else View.GONE,
        )
        views.setViewVisibility(R.id.work_stop, if (state.workActive) View.VISIBLE else View.GONE)
        views.setViewVisibility(R.id.sleep_start, if (state.sleepActive) View.GONE else View.VISIBLE)
        views.setViewVisibility(R.id.sleep_wake, if (state.sleepActive) View.VISIBLE else View.GONE)

        views.setOnClickPendingIntent(R.id.work_start, commandIntent(context, "work-start", 1))
        views.setOnClickPendingIntent(R.id.work_pause, commandIntent(context, "work-pause", 2))
        views.setOnClickPendingIntent(R.id.work_resume, commandIntent(context, "work-resume", 3))
        views.setOnClickPendingIntent(R.id.work_stop, commandIntent(context, "work-stop", 4))
        views.setOnClickPendingIntent(R.id.sleep_start, commandIntent(context, "sleep-start", 5))
        views.setOnClickPendingIntent(R.id.sleep_wake, commandIntent(context, "sleep-wake", 6))
        views.setOnClickPendingIntent(R.id.open_button, openIntent(context))
        return views
    }

    private fun commandIntent(context: Context, action: String, requestCode: Int): PendingIntent {
        val intent = Intent(context, RhythmWidgetProvider::class.java).apply {
            this.action = COMMAND_ACTION
            putExtra(EXTRA_COMMAND, action)
        }
        val flags = PendingIntent.FLAG_UPDATE_CURRENT or immutableFlag()
        return PendingIntent.getBroadcast(context, requestCode, intent, flags)
    }

    private fun openIntent(context: Context): PendingIntent {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?: Intent.makeRestartActivityTask(
                ComponentName(context.packageName, "android.app.NativeActivity"),
            )
        return PendingIntent.getActivity(
            context,
            100,
            launch,
            PendingIntent.FLAG_UPDATE_CURRENT or immutableFlag(),
        )
    }

    private fun immutableFlag(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_IMMUTABLE else 0

    private fun setChronometer(views: RemoteViews, id: Int, seconds: Long, running: Boolean) {
        val safeSeconds = seconds.coerceIn(0, Long.MAX_VALUE / 1000)
        val base = SystemClock.elapsedRealtime() - safeSeconds * 1000
        views.setChronometer(id, base, "%s", running)
    }

    private fun formatDuration(seconds: Long): String {
        val hours = seconds / 3600
        val minutes = (seconds % 3600) / 60
        return "%02dh %02dm".format(hours, minutes)
    }

    private fun percent(value: Long, target: Long): Int {
        if (target <= 0) return 0
        return ((value.coerceAtMost(target).toDouble() / target) * 100).roundToInt()
    }

    companion object {
        private const val TAG = "RhythmWidget"
        private const val STATE_TIME_FORMAT = "yyyy-MM-dd HH:mm:ss"
        const val COMMAND_ACTION: String = "org.HiBalance.rhythm.WIDGET_COMMAND"
        const val EXTRA_COMMAND: String = "command"

        @JvmStatic
        fun updateAll(context: Context) {
            val manager = AppWidgetManager.getInstance(context) ?: return
            val ids = manager.getAppWidgetIds(ComponentName(context, RhythmWidgetProvider::class.java))
            if (ids.isEmpty()) return
            RhythmWidgetProvider().onUpdate(context, manager, ids)
        }
    }
}
