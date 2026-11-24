package org.hibalance.rhythm.widget

import android.content.Context
import java.io.File
import java.util.concurrent.atomic.AtomicLong
import org.json.JSONArray
import org.json.JSONObject

data class WidgetState(
    var version: Int = 1,
    var workActive: Boolean = false,
    var workPaused: Boolean = false,
    var workSeconds: Long = 0,
    var pausedSeconds: Long = 0,
    var sleepActive: Boolean = false,
    var sleepSeconds: Long = 0,
    var lastNightSeconds: Long = 0,
    var workTarget: Long = 0,
    var sleepTarget: Long = 0,
    var theme: String = "System",
    var updatedAt: String = "",
)

object WidgetStore {
    private const val DIR = "rhythm"

    // Monotonic within the process and independent of wall-clock changes.
    private val commandCounter = AtomicLong(System.currentTimeMillis() * 1000)

    private fun stateFile(context: Context) = File(context.filesDir, "$DIR/widget_state.json")
    private fun commandsFile(context: Context) = File(context.filesDir, "$DIR/widget_commands.json")

    private fun ensureDir(context: Context) {
        File(context.filesDir, DIR).mkdirs()
    }

    private fun writeAtomic(file: File, text: String) {
        file.parentFile?.mkdirs()
        val temp = File(file.parentFile, "${file.name}.tmp")
        temp.writeText(text)
        if (!temp.renameTo(file)) {
            // Fall back to a direct write on filesystems that cannot replace
            // an existing file with renameTo.
            file.writeText(text)
            temp.delete()
        }
    }

    private fun readCommands(file: File): JSONArray {
        if (!file.exists()) return JSONArray()
        val text = file.readText()
        return try {
            JSONArray(text)
        } catch (_: Exception) {
            try {
                JSONObject(text).optJSONArray("commands") ?: JSONArray()
            } catch (_: Exception) {
                JSONArray()
            }
        }
    }

    @Synchronized
    fun readState(context: Context): WidgetState {
        val file = stateFile(context)
        if (!file.exists()) return WidgetState()
        return try {
            val json = JSONObject(file.readText())
            WidgetState(
                version = json.optInt("version", 1),
                workActive = json.optBoolean("work_active", false),
                workPaused = json.optBoolean("work_paused", false),
                workSeconds = json.optLong("work_seconds", 0),
                pausedSeconds = json.optLong("paused_seconds", 0),
                sleepActive = json.optBoolean("sleep_active", false),
                sleepSeconds = json.optLong("sleep_seconds", 0),
                lastNightSeconds = json.optLong("last_night_seconds", 0),
                workTarget = json.optLong("work_target", 0),
                sleepTarget = json.optLong("sleep_target", 0),
                theme = json.optString("theme", "System"),
                updatedAt = json.optString("updated_at", ""),
            )
        } catch (_: Exception) {
            WidgetState()
        }
    }

    @Synchronized
    fun writeState(context: Context, state: WidgetState) {
        ensureDir(context)
        val json = JSONObject()
            .put("version", state.version)
            .put("work_active", state.workActive)
            .put("work_paused", state.workPaused)
            .put("work_seconds", state.workSeconds)
            .put("paused_seconds", state.pausedSeconds)
            .put("sleep_active", state.sleepActive)
            .put("sleep_seconds", state.sleepSeconds)
            .put("last_night_seconds", state.lastNightSeconds)
            .put("work_target", state.workTarget)
            .put("sleep_target", state.sleepTarget)
            .put("theme", state.theme)
            .put("updated_at", state.updatedAt)
        writeAtomic(stateFile(context), json.toString())
    }

    @Synchronized
    fun appendCommand(context: Context, action: String): Long {
        ensureDir(context)
        val file = commandsFile(context)
        val commands = readCommands(file)

        val id = commandCounter.incrementAndGet()
        commands.put(JSONObject().put("id", id).put("action", action))
        writeAtomic(file, JSONObject().put("commands", commands).toString())
        return id
    }
}
