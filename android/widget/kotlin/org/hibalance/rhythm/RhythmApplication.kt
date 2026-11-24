package org.hibalance.rhythm

import android.app.Application
import org.hibalance.rhythm.widget.RuleCheckScheduler

class RhythmApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        RuleCheckScheduler.schedule(this)
    }
}
