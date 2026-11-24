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
import android.view.View
import android.widget.RemoteViews
import org.HiBalance.rhythm.R
import org.json.JSONObject

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
            val state = WidgetStore.readState(context)
            applyOptimistic(state, action)
            WidgetStore.writeState(context, state)

            val id = WidgetStore.appendCommand(context, action)
            val command = JSONObject().put("id", id).put("action", action).toString()
            try {
                RhythmNativeBridge.applyCommand(command)
            } catch (_: UnsatisfiedLinkError) {
                // The app process is not alive; the command file is picked up on launch.
            }
            updateAll(context)
            return
        }
        super.onReceive(context, intent)
    }

    private fun applyOptimistic(state: WidgetState, action: String) {
        when (action) {
            "work-start" -> {
                state.workActive = true
                state.workPaused = false
            }
            "work-pause" -> state.workPaused = true
            "work-resume" -> state.workPaused = false
            "work-stop" -> {
                state.workActive = false
                state.workPaused = false
                state.workSeconds = 0
            }
            "sleep-start" -> state.sleepActive = true
            "sleep-wake" -> state.sleepActive = false
        }
    }

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

        views.setTextViewText(
            R.id.work_status,
            when {
                state.workActive && state.workPaused ->
                    "Paused ${formatDuration(state.pausedSeconds)}"
                state.workActive -> "Working"
                else -> "Not working"
            },
        )
        views.setTextViewText(R.id.work_time, formatDuration(state.workSeconds))
        views.setTextViewText(
            R.id.sleep_status,
            if (state.sleepActive) "Sleeping" else "Awake",
        )
        val sleepSeconds = if (state.sleepActive) state.sleepSeconds else state.lastNightSeconds
        views.setTextViewText(R.id.sleep_time, formatDuration(sleepSeconds))

        views.setProgressBar(R.id.work_progress, 100, percent(state.workSeconds, state.workTarget), false)
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
        return PendingIntent.getActivity(context, 100, launch, immutableFlag())
    }

    private fun immutableFlag(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_IMMUTABLE else 0

    private fun formatDuration(seconds: Long): String {
        val hours = seconds / 3600
        val minutes = (seconds % 3600) / 60
        return "%02dh %02dm".format(hours, minutes)
    }

    private fun percent(value: Long, target: Long): Int {
        if (target <= 0) return 0
        return ((value.coerceAtMost(target).toDouble() / target) * 100).toInt()
    }

    companion object {
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
