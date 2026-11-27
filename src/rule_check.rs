//! Periodic rule evaluation used by the Android background job.

use std::{error::Error, path::Path};

use chrono::{Local, Timelike};

use crate::{
    report_history, settings, sleep,
    user::{
        data::UserData,
        rules::{RuleEvaluator, TimerValues},
    },
    work,
};

/// Evaluates the configured rules using the same state files as the UI.
///
/// The caller supplies the notification function because the background job
/// has its own Android `Context` instead of the foreground activity.
pub fn run<F>(config_dir: &Path, mut notify: F) -> Result<(), Box<dyn Error>>
where
    F: FnMut(&str, &str) -> Result<(), Box<dyn Error>>,
{
    std::fs::create_dir_all(config_dir)?;
    let settings = settings::Settings::parse(&config_dir.join("settings.json"))?;
    if !settings.warnings_enabled() {
        return Ok(());
    }

    let user_data = UserData::parse(config_dir)?;
    let work = work::WorkTimer::load(config_dir)?;
    let sleep = sleep::SleepTracker::load(config_dir)?;
    // Match the foreground: a pending night already counts with its button
    // timestamps, so background checks should evaluate it too.
    let sleep_value = sleep.last_night_value();
    let time = Local::now().time();
    let values = TimerValues {
        working: work.today_active(),
        sleep: sleep_value,
        system: time.hour() as u64 * 3600 + time.minute() as u64 * 60 + time.second() as u64,
    };

    let mut evaluator = RuleEvaluator::load(config_dir);
    let triggered = evaluator.evaluate(user_data.rules(), &values);
    if triggered.is_empty() {
        return Ok(());
    }

    let mut history = report_history::ReportHistory::load(config_dir)?;
    for rule in triggered {
        if let Err(e) = history.record("warning", &rule.name, &rule.message) {
            tracing::error!("Failed to record background rule report: {e}");
        }
        if let Err(e) = notify(&rule.name, &rule.message) {
            tracing::error!("Failed to show background rule notification: {e}");
        }
    }
    Ok(())
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn Java_org_hibalance_rhythm_widget_RhythmNativeBridge_evaluateRules<'local>(
    mut env: jni::JNIEnv<'local>,
    _this: jni::objects::JObject<'local>,
    config_dir: jni::objects::JString<'local>,
    context: jni::objects::JObject<'local>,
) {
    let Ok(config_dir) = env.get_string(&config_dir) else {
        let _ = env.exception_clear();
        return;
    };
    let config_dir = std::path::PathBuf::from(String::from(config_dir));

    let result = run(&config_dir, |title, body| {
        crate::notification::notify_with_context(&mut env, &context, title, body)
    });
    if let Err(e) = result {
        tracing::error!("Background rule check failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn background_check_triggers_and_records_rules() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-rule-check-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"appearance":{"selected_theme":"Dark"},"notifications":{"enabled":true,"quiet_start":0,"quiet_end":0}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("user_data.json"),
            r#"{"version":1,"rules":[{"id":"idle","name":"Idle","message":"No work yet","which-timer":"working","trigger-time":10,"tolerance":0,"trigger-if":"less"}]}"#,
        )
        .unwrap();

        let mut notified = Vec::new();
        run(&dir, |title, body| {
            notified.push((title.to_string(), body.to_string()));
            Ok(())
        })
        .unwrap();

        assert_eq!(notified, [("Idle".to_string(), "No work yet".to_string())]);
        let history = report_history::ReportHistory::load(&dir).unwrap();
        assert_eq!(history.reports().len(), 1);

        // The persisted crossing state prevents the same warning from firing
        // again on every background run.
        run(&dir, |title, body| {
            notified.push((title.to_string(), body.to_string()));
            Ok(())
        })
        .unwrap();
        assert_eq!(notified.len(), 1);
    }

    #[test]
    fn background_check_uses_a_pending_night() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-rule-check-pending-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"appearance":{"selected_theme":"Dark"},"notifications":{"enabled":true,"quiet_start":0,"quiet_end":0}}"#,
        )
        .unwrap();
        fs::write(
            dir.join("user_data.json"),
            r#"{"version":1,"rules":[{"id":"short-sleep","name":"Short sleep","message":"Sleep more","which-timer":"sleep","trigger-time":25200,"tolerance":0,"trigger-if":"less"}]}"#,
        )
        .unwrap();
        let now = Local::now().timestamp();
        fs::write(
            dir.join("sleep.json"),
            format!(
                r#"{{"bedtime":null,"pending":{{"bedtime":{},"wake_time":{now},"seconds":3600}},"nights":[]}}"#,
                now - 3600
            ),
        )
        .unwrap();

        let mut notified = Vec::new();
        run(&dir, |title, body| {
            notified.push((title.to_string(), body.to_string()));
            Ok(())
        })
        .unwrap();

        assert_eq!(
            notified,
            [("Short sleep".to_string(), "Sleep more".to_string())]
        );
    }
}
