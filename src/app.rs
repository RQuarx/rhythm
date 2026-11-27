use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, Timelike};
use thiserror::Error;
use tracing::{error, info};

use crate::{
    report_history, settings, sleep,
    user::{
        data::UserData,
        rules::{RuleEvaluator, TimerValues, TriggeredRule, UserRulesJson},
    },
    widget, work,
};

const DEFAULT_SLEEP_THRESHOLD: u64 = 25_200;
const MAX_EVENT_AGE: i64 = 60 * 24 * 60 * 60;
const THEMES: [&str; 3] = ["System", "Dark", "Light"];
const TIMERS: [&str; 3] = ["working", "sleep", "system"];
const CONDITIONS: [&str; 3] = ["more", "less", "same"];

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to retrieve application directories")]
    AppDirs,

    #[error("Failed to create application directories")]
    Io(#[from] std::io::Error),

    #[error("Failed to parse settings file")]
    Settings(#[from] settings::Error),
}

#[derive(Clone, Copy)]
pub enum Severity {
    Warning,
    Error,
}

/// Abstract output for user-facing reports, implemented by the UI layer.
pub trait ReportSink {
    fn report(&self, severity: Severity, title: String, message: String, details: String);
}

/// Editable representation of a rule. Time fields are strings so the UI can
/// bind them directly to text inputs; App validates and converts them.
#[derive(Clone, Default)]
pub struct RuleConfig {
    pub id: String,
    pub name: String,
    pub message: String,
    pub which_timer: String,
    pub trigger_if: String,
    pub trigger_hours: String,
    pub trigger_minutes: String,
    pub tolerance_hours: String,
    pub tolerance_minutes: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineKind {
    Work,
    Break,
    Sleep,
    ShortSleep,
}

impl TimelineKind {
    /// Integer value exposed to Slint.
    pub fn code(self) -> i32 {
        match self {
            Self::Work => 0,
            Self::Break => 1,
            Self::Sleep => 2,
            Self::ShortSleep => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::Break => "Break",
            Self::Sleep | Self::ShortSleep => "Sleep",
        }
    }

    fn draw_order(self) -> u8 {
        match self {
            Self::Sleep | Self::ShortSleep => 0,
            Self::Work => 1,
            Self::Break => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineBlock {
    pub kind: TimelineKind,
    /// Seconds after local midnight.
    pub from_secs: u32,
    pub to_secs: u32,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineDay {
    pub name: String,
    pub today: bool,
    pub work_label: String,
    pub sleep_label: Option<String>,
    /// Compact labels used when the columns are too narrow for the full text.
    pub work_compact: String,
    pub sleep_compact: Option<String>,
    pub sleep_low: bool,
    pub blocks: Vec<TimelineBlock>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineSnapshot {
    pub days: Vec<TimelineDay>,
    pub now_seconds: u32,
    pub week_label: String,
}

#[derive(Default)]
pub struct DashboardStats {
    pub average_work: String,
    pub average_sleep: String,
    pub streak: u32,
    pub work_goal_percent: i32,
    pub sleep_goal_percent: i32,
}

/// A UI-independent snapshot of everything the window needs to display.
#[derive(Default)]
pub struct AppSnapshot {
    pub today_working_hour: String,
    pub week_working_hour: String,
    pub max_working_hour: String,
    pub today_sleeping_hour: String,
    pub week_sleeping_hour: String,
    pub min_sleeping_hour: String,
    pub max_working_time: u64,
    pub current_working_time: u64,
    pub max_sleeping_time: u64,
    pub current_sleeping_time: u64,
    pub work_running: bool,
    pub work_paused: bool,
    pub sleeping: bool,
    pub need_bedtime: bool,
    pub pending_active: bool,
    pub pending_seconds: u64,
    pub pending_hours: String,
    pub pending_minutes: String,
    pub theme: String,
    pub notifications_enabled: bool,
    pub quiet_start: String,
    pub quiet_end: String,
    pub week_start: String,
    pub usual_work_hours: String,
}

/// Statistics and timeline shown on the dashboard, kept separate from the
/// per-second snapshot so the dashboard does not need to write widget state.
#[derive(Default)]
pub struct DashboardSnapshot {
    pub timeline: TimelineSnapshot,
    pub stats: DashboardStats,
}

/// Owns all application state and behaviour. Deliberately free of Slint types.
pub struct App {
    settings: settings::Settings,
    user_data: UserData,
    work: work::WorkTimer,
    sleep: sleep::SleepTracker,
    report_history: report_history::ReportHistory,
    evaluator: RuleEvaluator,
    reports: Box<dyn ReportSink>,
    widget_dir: PathBuf,
    need_bedtime: bool,
    manual_wake_time: Option<i64>,
}

impl App {
    pub fn new(reports: Box<dyn ReportSink>) -> Result<Self, Error> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();

        let config_path: PathBuf = match sysdirs::config_dir() {
            Some(d) => d.join("rhythm/").to_path_buf(),
            None => return Err(Error::AppDirs),
        };
        fs::create_dir_all(&config_path)?;
        info!("Using config directory {}", config_path.display());

        let settings = match settings::Settings::parse(&config_path.join("settings.json")) {
            Ok(settings) => settings,
            Err(e) => {
                reports.report(
                    Severity::Error,
                    "Failed to load settings".into(),
                    e.to_string(),
                    String::new(),
                );
                settings::Settings::new(&config_path)
            }
        };
        let user_data = match UserData::parse(&config_path) {
            Ok(data) => data,
            Err(e) => {
                reports.report(
                    Severity::Error,
                    "Failed to load rules".into(),
                    e.to_string(),
                    String::new(),
                );
                UserData::default_at(&config_path)
            }
        };
        let work = match work::WorkTimer::load(&config_path) {
            Ok(work) => work,
            Err(e) => {
                reports.report(
                    Severity::Error,
                    "Failed to load work timer".into(),
                    e.to_string(),
                    String::new(),
                );
                work::WorkTimer::new(&config_path)
            }
        };
        let sleep = match sleep::SleepTracker::load(&config_path) {
            Ok(sleep) => sleep,
            Err(e) => {
                reports.report(
                    Severity::Error,
                    "Failed to load sleep tracker".into(),
                    e.to_string(),
                    String::new(),
                );
                sleep::SleepTracker::new(&config_path)
            }
        };

        let report_history = match report_history::ReportHistory::load(&config_path) {
            Ok(history) => history,
            Err(e) => {
                reports.report(
                    Severity::Error,
                    "Failed to load report history".into(),
                    e.to_string(),
                    String::new(),
                );
                report_history::ReportHistory::new(&config_path)
            }
        };

        Ok(Self {
            settings,
            user_data,
            work,
            sleep,
            report_history,
            evaluator: RuleEvaluator::load(&config_path),
            reports,
            widget_dir: config_path,
            need_bedtime: false,
            manual_wake_time: None,
        })
    }

    pub fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    pub fn open_url(&mut self, url: &str) {
        info!("Opening URL \"{url}\"");

        if let Err(e) = webbrowser::open(url) {
            error!("Failed to open url \"{url}\": {e}");
            self.report(
                Severity::Error,
                "Failed to open link".into(),
                e.to_string(),
                error_details(&e),
            );
        }
    }

    pub fn tick(&mut self) {
        if let Err(e) = self.report_history.reload_if_changed() {
            error!("Failed to reload reports: {e}");
        }
        self.drain_widget_commands();
        // A pending night already counts with its button timestamps, so rules
        // should see it before the correction dialog is resolved.
        self.evaluate(self.sleep.last_night_value());
    }

    /// Applies commands queued by the homescreen widget.
    pub fn drain_widget_commands(&mut self) {
        let commands = widget::drain_commands(&self.widget_dir);
        for command in commands {
            let at = command.at;
            match command.action.as_str() {
                "work-start" => self.start_work_at(at),
                "work-pause" => self.pause_work_at(at),
                "work-resume" => self.unpause_work_at(at),
                "work-stop" => {
                    self.stop_work_at(at);
                }
                "sleep-start" => self.start_sleep_at(at),
                "sleep-wake" => self.wake_up_sleep_at(at),
                _ => {}
            }
            // Keep the batch on disk and record each applied id as we go, so
            // a crash cannot drop the remaining button presses or replay the
            // ones already applied.
            if !widget::mark_command_applied(&self.widget_dir, command.id) {
                return;
            }
        }
        widget::finish_commands(&self.widget_dir);
    }

    pub fn start_work(&mut self) {
        self.start_work_at(Local::now().timestamp());
    }

    fn start_work_at(&mut self, at: i64) {
        if !self.can_start_work() {
            return;
        }
        let result = self.work.start_at(at);
        self.report_result(result);
    }

    pub fn pause_work(&mut self) {
        let result = self.work.pause();
        self.report_result(result);
    }

    fn pause_work_at(&mut self, at: i64) {
        let result = self.work.pause_at(at);
        self.report_result(result);
    }

    pub fn unpause_work(&mut self) {
        self.unpause_work_at(Local::now().timestamp());
    }

    fn unpause_work_at(&mut self, at: i64) {
        if !self.can_start_work() {
            return;
        }
        let result = self.work.unpause_at(at);
        self.report_result(result);
    }

    pub fn stop_work(&mut self) {
        self.stop_work_at(Local::now().timestamp());
    }

    fn stop_work_at(&mut self, at: i64) -> bool {
        match self.work.stop_at(at) {
            Ok(()) => true,
            Err(e) => {
                self.report_result(Err::<(), _>(e));
                false
            }
        }
    }

    /// Work and sleep are mutually exclusive. Blocking the start here keeps a
    /// sleeping night from silently accumulating work time.
    fn can_start_work(&mut self) -> bool {
        if self.sleep.is_sleeping() {
            self.report(
                Severity::Warning,
                "Still sleeping".into(),
                "Wake up before starting work.".into(),
                String::new(),
            );
            return false;
        }
        true
    }

    pub fn start_sleep(&mut self) {
        self.start_sleep_at(Local::now().timestamp());
    }

    fn start_sleep_at(&mut self, at: i64) {
        if !self.can_start_sleep() {
            return;
        }
        // Going to bed ends the work interval at the same instant, so the two
        // timers never overlap.
        if self.work.is_active() && !self.stop_work_at(at) {
            return;
        }
        let result = self.sleep.go_to_bed_at(at);
        self.report_result(result);
    }

    fn can_start_sleep(&mut self) -> bool {
        if self.sleep.has_pending() {
            self.report(
                Severity::Warning,
                "Sleep correction pending".into(),
                "Confirm or discard the previous night before starting a new one.".into(),
                String::new(),
            );
            return false;
        }
        if self.need_bedtime {
            self.report(
                Severity::Warning,
                "Bedtime pending".into(),
                "Enter or cancel the previous bedtime before starting a new night.".into(),
                String::new(),
            );
            return false;
        }
        true
    }

    pub fn wake_up_sleep(&mut self) {
        if !self.can_wake_up_sleep() {
            return;
        }
        match self.sleep.wake_up() {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.need_bedtime = true;
                self.manual_wake_time = Some(Local::now().timestamp());
            }
            Err(e) => self.report_result(Err::<(), _>(e)),
        }
    }

    fn wake_up_sleep_at(&mut self, at: i64) {
        if !self.can_wake_up_sleep() {
            return;
        }
        match self.sleep.wake_up_at(at) {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.need_bedtime = true;
                // Preserve when the user actually pressed "Woke up" so the
                // manual bedtime prompt does not include the response delay.
                self.manual_wake_time = Some(event_time(at));
            }
            Err(e) => self.report_result(Err::<(), _>(e)),
        }
    }

    fn can_wake_up_sleep(&mut self) -> bool {
        if self.sleep.has_pending() {
            self.report(
                Severity::Warning,
                "Sleep correction pending".into(),
                "Confirm or discard the previous night before starting a new one.".into(),
                String::new(),
            );
            return false;
        }
        true
    }

    pub fn set_bedtime(&mut self, time: &str) {
        let Some(minutes) = parse_clock(time) else {
            self.report(
                Severity::Warning,
                "Invalid bedtime".into(),
                "Enter the bedtime as HH:MM.".into(),
                String::new(),
            );
            return;
        };
        if self.sleep.is_sleeping() {
            self.report(
                Severity::Warning,
                "Already sleeping".into(),
                "Wake up before entering a bedtime for the previous night.".into(),
                String::new(),
            );
            return;
        }
        if self.sleep.has_pending() {
            self.report(
                Severity::Warning,
                "Sleep correction pending".into(),
                "Confirm or discard the previous night before recording another one.".into(),
                String::new(),
            );
            return;
        }

        let wake_time = self
            .manual_wake_time
            .unwrap_or_else(|| Local::now().timestamp());
        if let Err(e) = self.sleep.record_manual_night(minutes, wake_time) {
            self.report_result(Err::<(), _>(e));
            return;
        }
        self.need_bedtime = false;
        self.manual_wake_time = None;
    }

    pub fn cancel_bedtime(&mut self) {
        self.need_bedtime = false;
        self.manual_wake_time = None;
    }

    /// Discards a pending night without recording it.
    pub fn discard_pending_sleep(&mut self) {
        if let Err(e) = self.sleep.discard_pending() {
            self.report_result(Err::<(), _>(e));
        }
    }

    /// Stores a corrected sleep duration. Blank fields keep the duration
    /// computed from the button presses; other invalid input is reported.
    pub fn correct_sleep(&mut self, hours: &str, minutes: &str) -> Result<(), String> {
        let computed = self.sleep.pending_seconds();
        let Some(seconds) = duration_or_computed(hours, minutes, computed) else {
            return Err("Enter hours and minutes (minutes must be below 60).".into());
        };

        match self.sleep.commit_pending(seconds) {
            Ok(effective) => {
                if effective > 0 {
                    self.evaluate(Some(effective));
                }
                Ok(())
            }
            // The caller shows the failure; reporting it here too would show
            // the same error twice.
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn rule_configs(&self) -> Vec<RuleConfig> {
        self.user_data.rules().iter().map(rule_to_config).collect()
    }

    pub fn default_rule_configs(&self) -> Vec<RuleConfig> {
        UserData::default()
            .rules()
            .iter()
            .map(rule_to_config)
            .collect()
    }

    /// Validates and persists a full rule list. Resets notification crossings.
    pub fn save_rules(&mut self, configs: Vec<RuleConfig>) -> Result<(), String> {
        let rules = build_rules(&configs)?;

        let previous = self.user_data.rules().to_vec();
        self.user_data.set_rules(rules);
        if let Err(e) = self.user_data.save() {
            self.user_data.set_rules(previous);
            return Err(e.to_string());
        }

        self.evaluator.reset();
        Ok(())
    }

    /// Validates and persists the settings chosen on the Settings page.
    pub fn save_settings(
        &mut self,
        theme: String,
        notifications_enabled: bool,
        quiet_start: String,
        quiet_end: String,
        week_start: String,
        usual_work_hours: String,
    ) -> Result<(), String> {
        if !THEMES.contains(&theme.as_str()) {
            return Err(format!("Unknown theme '{theme}'."));
        }
        let start = parse_clock(&quiet_start)
            .ok_or_else(|| "Quiet hours start must be HH:MM.".to_string())?;
        let end =
            parse_clock(&quiet_end).ok_or_else(|| "Quiet hours end must be HH:MM.".to_string())?;
        let week_start = settings::WeekStart::parse(&week_start)
            .ok_or_else(|| format!("Unknown week start '{week_start}'."))?;
        let usual_work_minutes = parse_clock(&usual_work_hours)
            .ok_or_else(|| "Usual working hours must be HH:MM.".to_string())?;
        if usual_work_minutes == 0 {
            return Err("Usual working hours must be greater than 00:00.".to_string());
        }

        let previous = (
            self.settings.selected_theme().to_string(),
            self.settings.notifications_enabled(),
            self.settings.quiet_hours(),
            self.settings.week_start(),
            self.settings.usual_work_minutes(),
        );
        self.settings.set_selected_theme(theme);
        self.settings
            .set_notifications_enabled(notifications_enabled);
        self.settings.set_quiet_hours(start, end);
        self.settings.set_week_start(week_start);
        self.settings.set_usual_work_minutes(usual_work_minutes);
        if let Err(e) = self.settings.save_settings() {
            self.settings.set_selected_theme(previous.0);
            self.settings.set_notifications_enabled(previous.1);
            self.settings.set_quiet_hours(previous.2.0, previous.2.1);
            self.settings.set_week_start(previous.3);
            self.settings.set_usual_work_minutes(previous.4);
            return Err(e.to_string());
        }
        Ok(())
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let today = Local::now().date_naive();
        let week_start = self.week_start_date(today);
        let week_end = week_start + ChronoDuration::days(6);
        let work_today = self.work.today_active();
        let work_week = self.work.active_between(week_start, week_end);
        let max_work = self.settings.usual_work_seconds();
        let min_sleep = self.threshold("sleep", "less", DEFAULT_SLEEP_THRESHOLD);
        let current_sleep = if self.sleep.is_sleeping() {
            self.sleep.current_elapsed()
        } else {
            self.sleep.last_night()
        };
        let pending_seconds = self.sleep.pending_seconds();
        let (quiet_start, quiet_end) = self.settings.quiet_hours();

        widget::write_state(
            &self.widget_dir,
            &widget::StateData {
                work_active: self.work.is_active(),
                work_paused: self.work.is_paused(),
                work_seconds: work_today,
                // The widget shows this next to "Paused", so it must be the
                // current pause, not the total paused time for the day.
                paused_seconds: self.work.current_pause_elapsed(),
                sleep_active: self.sleep.is_sleeping(),
                sleep_seconds: current_sleep,
                last_night_seconds: self.sleep.last_night(),
                work_target: max_work,
                sleep_target: min_sleep,
                theme: self.settings.selected_theme().to_string(),
                ..widget::StateData::default()
            },
        );

        AppSnapshot {
            today_working_hour: format_duration(work_today),
            week_working_hour: format_duration(work_week),
            max_working_hour: format_duration(max_work),
            today_sleeping_hour: format_duration(self.sleep.today_total()),
            week_sleeping_hour: format_duration(self.sleep.total_between(week_start, week_end)),
            min_sleeping_hour: format_duration(min_sleep),
            max_working_time: max_work,
            current_working_time: work_today,
            max_sleeping_time: min_sleep,
            current_sleeping_time: current_sleep,
            work_running: self.work.is_active(),
            work_paused: self.work.is_paused(),
            sleeping: self.sleep.is_sleeping(),
            need_bedtime: self.need_bedtime,
            pending_active: self.sleep.has_pending(),
            pending_seconds,
            pending_hours: hours_text(pending_seconds),
            pending_minutes: minutes_text(pending_seconds),
            theme: self.settings.selected_theme().to_string(),
            notifications_enabled: self.settings.notifications_enabled(),
            quiet_start: clock_text(quiet_start),
            quiet_end: clock_text(quiet_end),
            week_start: self.settings.week_start().label().to_string(),
            usual_work_hours: clock_text(self.settings.usual_work_minutes()),
        }
    }

    pub fn dashboard_snapshot(&self) -> DashboardSnapshot {
        let today = Local::now().date_naive();
        let week_start = self.week_start_date(today);
        let week_end = week_start + ChronoDuration::days(6);
        let work = self.work.series_between(week_start, week_end);
        let sleep = self.sleep.series_between(week_start, week_end);
        let min_sleep = self.threshold("sleep", "less", DEFAULT_SLEEP_THRESHOLD);

        DashboardSnapshot {
            timeline: self.build_timeline(today, week_start, &work, &sleep, min_sleep),
            stats: self.dashboard_stats(today, &work, &sleep, min_sleep),
        }
    }

    pub fn recent_reports(&self) -> Vec<report_history::ReportRecord> {
        self.report_history.reports().to_vec()
    }

    fn week_start_date(&self, today: NaiveDate) -> NaiveDate {
        week_start_for(today, self.settings.week_start())
    }

    fn build_timeline(
        &self,
        today: NaiveDate,
        week_start: NaiveDate,
        work: &[(NaiveDate, u64, u64)],
        sleep: &[(NaiveDate, u64)],
        sleep_target: u64,
    ) -> TimelineSnapshot {
        let week_end = week_start + ChronoDuration::days(6);
        let work_segments = self.work.segments_between(week_start, week_end);
        let sleep_segments = self.sleep.segments_between(week_start, week_end);

        let mut days = Vec::with_capacity(7);
        for offset in 0..7usize {
            let date = week_start + ChronoDuration::days(offset as i64);
            let active = work[offset].1;
            let sleep_seconds = sleep[offset].1;
            let mut blocks = Vec::new();

            for segment in sleep_segments.iter().filter(|segment| segment.date == date) {
                let kind = if !segment.active && segment.seconds < sleep_target {
                    TimelineKind::ShortSleep
                } else {
                    TimelineKind::Sleep
                };
                blocks.push(TimelineBlock {
                    kind,
                    from_secs: segment.from_secs,
                    to_secs: segment.to_secs,
                    detail: timeline_detail(kind, segment.from_secs, segment.to_secs),
                });
            }
            for segment in work_segments.iter().filter(|segment| segment.date == date) {
                let kind = if segment.active {
                    TimelineKind::Work
                } else {
                    TimelineKind::Break
                };
                blocks.push(TimelineBlock {
                    kind,
                    from_secs: segment.from_secs,
                    to_secs: segment.to_secs,
                    detail: timeline_detail(kind, segment.from_secs, segment.to_secs),
                });
            }
            blocks.sort_by_key(|block| (block.from_secs, block.kind.draw_order()));

            days.push(TimelineDay {
                name: date.format("%a").to_string(),
                today: date == today,
                work_label: format!("Work {}", duration_or_dash(active)),
                sleep_label: (sleep_seconds > 0)
                    .then(|| format!("Sleep {}", format_duration(sleep_seconds))),
                work_compact: compact_or_dash(active),
                sleep_compact: (sleep_seconds > 0).then(|| compact_duration(sleep_seconds)),
                sleep_low: sleep_seconds > 0 && sleep_seconds < sleep_target,
                blocks,
            });
        }

        TimelineSnapshot {
            days,
            now_seconds: Local::now().num_seconds_from_midnight(),
            week_label: format!(
                "{} – {}",
                week_start.format("%a %-d %b"),
                week_end.format("%a %-d %b")
            ),
        }
    }

    fn dashboard_stats(
        &self,
        today: NaiveDate,
        work: &[(NaiveDate, u64, u64)],
        sleep: &[(NaiveDate, u64)],
        sleep_target: u64,
    ) -> DashboardStats {
        let work_target = self.settings.usual_work_seconds();
        let current_work = self.work.today_active();
        let today_index = work
            .iter()
            .position(|(date, _, _)| *date == today)
            .unwrap_or(work.len().saturating_sub(1));
        let visible = today_index + 1;
        let first_observed = (0..visible)
            .find(|index| work[*index].1 > 0 || work[*index].2 > 0 || sleep[*index].1 > 0);
        let observed_days = first_observed
            .map(|first| visible - first)
            .unwrap_or(1)
            .max(1) as u64;
        let average_work = (0..visible).map(|index| work[index].1).sum::<u64>() / observed_days;
        let average_sleep = (0..visible).map(|index| sleep[index].1).sum::<u64>() / observed_days;

        let recent_work = self.work.daily_series(60);
        let recent_sleep: std::collections::HashMap<NaiveDate, u64> = self
            .sleep
            .series_between(today - ChronoDuration::days(59), today)
            .into_iter()
            .collect();
        let mut streak: u32 = 0;
        for (index, (date, active, paused)) in recent_work.iter().rev().enumerate() {
            let night = recent_sleep.get(date).copied().unwrap_or(0);
            if index == 0 && night == 0 {
                // Today's night may not be recorded yet; judge yesterday instead.
                continue;
            }
            if *active == 0 && *paused == 0 && night == 0 {
                break;
            }
            if *active <= work_target && night >= sleep_target && night > 0 {
                streak += 1;
            } else {
                break;
            }
        }

        DashboardStats {
            average_work: format_duration(average_work),
            average_sleep: format_duration(average_sleep),
            streak,
            work_goal_percent: percentage(current_work, work_target),
            sleep_goal_percent: percentage(self.sleep.last_night(), sleep_target),
        }
    }

    fn report(&mut self, severity: Severity, title: String, message: String, details: String) {
        let level = match severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        if let Err(e) = self.report_history.record(level, &title, &message) {
            error!("Failed to record report history: {e}");
        }
        self.reports.report(severity, title, message, details);
    }

    fn report_result<T, E: std::fmt::Display>(&mut self, result: Result<T, E>) {
        if let Err(e) = result {
            self.report(
                Severity::Error,
                "Failed to save application data".into(),
                e.to_string(),
                String::new(),
            );
        }
    }

    fn threshold(&self, timer: &str, condition: &str, fallback: u64) -> u64 {
        let rules = self.user_data.rules();
        rules
            .iter()
            .find(|rule| rule.which_timer == timer && rule.trigger_if == condition)
            .or_else(|| rules.iter().find(|rule| rule.which_timer == timer))
            .map(|rule| rule.trigger_time)
            .unwrap_or(fallback)
    }

    fn evaluate(&mut self, sleep_value: Option<u64>) {
        if !self.settings.warnings_enabled() {
            // Don't consume the crossing state while notifications are muted;
            // otherwise the warning is lost until the condition resets.
            return;
        }

        let time = Local::now().time();
        let system = time.hour() as u64 * 3600 + time.minute() as u64 * 60 + time.second() as u64;
        let values = TimerValues {
            working: self.work.today_active(),
            sleep: sleep_value,
            system,
        };

        for rule in self.evaluator.evaluate(self.user_data.rules(), &values) {
            let TriggeredRule { key, name, message } = rule;
            if let Err(e) = crate::notification::notify(&key, &name, &message) {
                error!("Failed to show rule notification: {e}");
            }
            self.report(Severity::Warning, name, message, String::new());
        }
    }
}

fn rule_to_config(rule: &UserRulesJson) -> RuleConfig {
    RuleConfig {
        id: rule.id.clone(),
        name: rule.name.clone(),
        message: rule.message.clone(),
        which_timer: rule.which_timer.clone(),
        trigger_if: rule.trigger_if.clone(),
        trigger_hours: (rule.trigger_time / 3600).to_string(),
        trigger_minutes: ((rule.trigger_time % 3600) / 60).to_string(),
        tolerance_hours: (rule.tolerance / 3600).to_string(),
        tolerance_minutes: ((rule.tolerance % 3600) / 60).to_string(),
    }
}

/// Collects the source chain of an error into a string suitable for `details`.
pub fn error_details(error: &(dyn std::error::Error + 'static)) -> String {
    let mut details = String::new();
    let mut source = error.source();
    while let Some(cause) = source {
        if !details.is_empty() {
            details.push('\n');
        }
        details.push_str(&cause.to_string());
        source = cause.source();
    }
    details
}

/// Clamps a widget command timestamp so an old queue entry or a backwards
/// clock change cannot create absurd intervals.
fn event_time(at: i64) -> i64 {
    let now = Local::now().timestamp();
    if at <= 0 {
        now
    } else {
        at.clamp(now - MAX_EVENT_AGE, now)
    }
}

fn format_duration(seconds: u64) -> String {
    format!("{:02}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
}

fn week_start_for(today: NaiveDate, week_start: settings::WeekStart) -> NaiveDate {
    let start = week_start.weekday();
    let offset = (today.weekday().num_days_from_monday() + 7 - start.num_days_from_monday()) % 7;
    today - ChronoDuration::days(offset as i64)
}

fn duration_or_dash(seconds: u64) -> String {
    if seconds == 0 {
        "–".to_string()
    } else {
        format_duration(seconds)
    }
}

/// Short duration for the narrow timeline columns, for example `7h30`.
fn compact_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    match (hours, minutes) {
        (0, minutes) => format!("{minutes}m"),
        (hours, 0) => format!("{hours}h"),
        (hours, minutes) => format!("{hours}h{minutes:02}"),
    }
}

fn compact_or_dash(seconds: u64) -> String {
    if seconds == 0 {
        "–".to_string()
    } else {
        compact_duration(seconds)
    }
}

fn timeline_detail(kind: TimelineKind, from_secs: u32, to_secs: u32) -> String {
    format!(
        "{} {}–{} ({})",
        kind.label(),
        clock_text((from_secs / 60) as u16),
        clock_text((to_secs / 60) as u16),
        format_duration(u64::from(to_secs.saturating_sub(from_secs)))
    )
}

/// The widget only displays minutes, so round second-based values to avoid
/// rewriting its state file and calling JNI on every tick.
fn percentage(value: u64, target: u64) -> i32 {
    if target == 0 {
        return 0;
    }
    ((value.min(target) as f64 / target as f64) * 100.0).round() as i32
}

fn hours_text(seconds: u64) -> String {
    (seconds / 3600).to_string()
}

fn minutes_text(seconds: u64) -> String {
    ((seconds % 3600) / 60).to_string()
}

fn clock_text(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

fn parse_clock(text: &str) -> Option<u16> {
    let (hours, minutes) = text.trim().split_once(':')?;
    let hours: u16 = hours.trim().parse().ok()?;
    let minutes: u16 = minutes.trim().parse().ok()?;
    if hours >= 24 || minutes >= 60 {
        return None;
    }
    Some(hours * 60 + minutes)
}

fn parse_duration(hours: &str, minutes: &str) -> Option<u64> {
    let hours: u64 = hours.trim().parse().ok()?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    if minutes >= 60 {
        return None;
    }
    hours
        .checked_mul(3600)?
        .checked_add(minutes.checked_mul(60)?)
}

/// An empty correction means the user accepted the timestamps captured by the
/// "Going to bed" and "Woke up" buttons.
fn duration_or_computed(hours: &str, minutes: &str, computed: u64) -> Option<u64> {
    if hours.trim().is_empty() && minutes.trim().is_empty() {
        Some(computed)
    } else {
        parse_duration(hours, minutes)
    }
}

fn parse_duration_fields(hours: &str, minutes: &str, label: &str) -> Result<u64, String> {
    parse_duration(hours, minutes)
        .ok_or_else(|| format!("Invalid {label}: enter hours and minutes (minutes < 60)."))
}

fn build_rules(configs: &[RuleConfig]) -> Result<Vec<UserRulesJson>, String> {
    let mut rules = Vec::with_capacity(configs.len());
    let mut ids = HashSet::new();
    for config in configs {
        let name = config.name.trim().to_string();
        let message = config.message.trim().to_string();
        if name.is_empty() || message.is_empty() {
            return Err("Every rule needs a name and a message.".into());
        }
        if !TIMERS.contains(&config.which_timer.as_str()) {
            return Err(format!("Unknown timer '{}'.", config.which_timer));
        }
        if !CONDITIONS.contains(&config.trigger_if.as_str()) {
            return Err(format!("Unknown condition '{}'.", config.trigger_if));
        }
        let trigger_time = parse_duration_fields(
            &config.trigger_hours,
            &config.trigger_minutes,
            "trigger time",
        )?;
        if config.which_timer == "system" && trigger_time >= 24 * 60 * 60 {
            return Err("System rules need a time of day between 00:00 and 23:59.".into());
        }
        let tolerance = parse_duration_fields(
            &config.tolerance_hours,
            &config.tolerance_minutes,
            "tolerance",
        )?;

        let id = if config.id.trim().is_empty() {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("rule-{nanos}-{}", rules.len())
        } else {
            config.id.trim().to_string()
        };
        if !ids.insert(id.clone()) {
            return Err(format!("Duplicate rule id '{id}'."));
        }

        rules.push(UserRulesJson {
            id,
            name,
            message,
            which_timer: config.which_timer.clone(),
            trigger_time,
            tolerance,
            trigger_if: config.trigger_if.clone(),
        });
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clock_entries() {
        assert_eq!(parse_clock("23:05"), Some(23 * 60 + 5));
        assert_eq!(parse_clock(" 7:0 "), Some(7 * 60));
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_clock("12:60"), None);
        assert_eq!(parse_clock("noon"), None);
    }

    #[test]
    fn parses_duration_entries() {
        assert_eq!(parse_duration("7", "30"), Some(27_000));
        assert_eq!(parse_duration("abc", "30"), None);
        assert_eq!(parse_duration("7", "60"), None);
        assert_eq!(parse_duration("18446744073709551615", "0"), None);
    }

    #[test]
    fn blank_correction_uses_the_button_times() {
        assert_eq!(duration_or_computed("", "", 27_000), Some(27_000));
        assert_eq!(duration_or_computed("  ", "\t", 27_000), Some(27_000));
        assert_eq!(duration_or_computed("7", "30", 1), Some(27_000));
        assert_eq!(duration_or_computed("", "30", 27_000), None);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0), "00h 00m");
        assert_eq!(format_duration(3720), "01h 02m");
    }

    #[test]
    fn formats_compact_durations() {
        assert_eq!(compact_duration(0), "0m");
        assert_eq!(compact_duration(45 * 60), "45m");
        assert_eq!(compact_duration(7 * 3600), "7h");
        assert_eq!(compact_duration(7 * 3600 + 30 * 60), "7h30");
        assert_eq!(compact_or_dash(0), "–");
        assert_eq!(compact_or_dash(8 * 3600), "8h");
    }

    #[test]
    fn weeks_start_on_the_configured_day() {
        let monday = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let tuesday = monday + ChronoDuration::days(1);
        let sunday = monday + ChronoDuration::days(6);

        assert_eq!(week_start_for(tuesday, settings::WeekStart::Monday), monday);
        assert_eq!(
            week_start_for(tuesday, settings::WeekStart::Sunday),
            monday - ChronoDuration::days(1)
        );
        assert_eq!(
            week_start_for(tuesday, settings::WeekStart::Saturday),
            monday - ChronoDuration::days(2)
        );
        assert_eq!(week_start_for(sunday, settings::WeekStart::Sunday), sunday);
        assert_eq!(
            week_start_for(monday, settings::WeekStart::Saturday),
            monday - ChronoDuration::days(2)
        );
    }

    #[test]
    fn formats_clock() {
        assert_eq!(clock_text(22 * 60), "22:00");
        assert_eq!(clock_text(7 * 60 + 5), "07:05");
    }

    #[test]
    fn quiet_hours_can_cross_midnight() {
        let start = 22 * 60;
        let end = 7 * 60;
        assert!(settings::is_quiet_hours(start, end, 23 * 60));
        assert!(settings::is_quiet_hours(start, end, 3 * 60));
        assert!(!settings::is_quiet_hours(start, end, 12 * 60));
        assert!(!settings::is_quiet_hours(start, start, 23 * 60));
    }

    #[test]
    fn build_rules_validates_and_converts_times() {
        let config = RuleConfig {
            id: "custom".into(),
            name: "Rule".into(),
            message: "Message".into(),
            which_timer: "working".into(),
            trigger_if: "more".into(),
            trigger_hours: "7".into(),
            trigger_minutes: "30".into(),
            tolerance_hours: "0".into(),
            tolerance_minutes: "15".into(),
        };
        let rules = build_rules(std::slice::from_ref(&config)).unwrap();
        assert_eq!(rules[0].id, "custom");
        assert_eq!(rules[0].trigger_time, 27_000);
        assert_eq!(rules[0].tolerance, 900);
        assert!(build_rules(&[config.clone(), config.clone()]).is_err());

        let invalid = RuleConfig {
            which_timer: "unknown".into(),
            ..config
        };
        assert!(build_rules(&[invalid]).is_err());
    }

    #[test]
    fn system_rules_use_a_clock_time() {
        let config = RuleConfig {
            id: "clock".into(),
            name: "Clock rule".into(),
            message: "Message".into(),
            which_timer: "system".into(),
            trigger_if: "more".into(),
            trigger_hours: "23".into(),
            trigger_minutes: "59".into(),
            tolerance_hours: "0".into(),
            tolerance_minutes: "5".into(),
        };
        let rules = build_rules(std::slice::from_ref(&config)).unwrap();
        assert_eq!(rules[0].trigger_time, 23 * 3600 + 59 * 60);

        for (hours, minutes) in [("24", "0"), ("25", "0"), ("23", "60")] {
            let invalid = RuleConfig {
                trigger_hours: hours.into(),
                trigger_minutes: minutes.into(),
                ..config.clone()
            };
            assert!(
                build_rules(&[invalid]).is_err(),
                "{hours}:{minutes} should be rejected for a system rule"
            );
        }

        let long_working = RuleConfig {
            which_timer: "working".into(),
            trigger_hours: "30".into(),
            trigger_minutes: "0".into(),
            ..config
        };
        assert!(build_rules(&[long_working]).is_ok());
    }

    struct NoReports;

    impl ReportSink for NoReports {
        fn report(&self, _: Severity, _: String, _: String, _: String) {}
    }

    fn app_at(dir: &std::path::Path) -> App {
        App {
            settings: settings::Settings::new(dir),
            user_data: UserData::default_at(dir),
            work: work::WorkTimer::new(dir),
            sleep: sleep::SleepTracker::new(dir),
            report_history: report_history::ReportHistory::new(dir),
            evaluator: RuleEvaluator::new(),
            reports: Box::new(NoReports),
            widget_dir: dir.to_path_buf(),
            need_bedtime: false,
            manual_wake_time: None,
        }
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-app-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn going_to_bed_stops_work() {
        let dir = temp_dir("bed-stops-work");
        let mut app = app_at(&dir);
        app.start_work();
        assert!(app.work.is_active());

        app.start_sleep();
        assert!(app.sleep.is_sleeping());
        assert!(!app.work.is_active());
    }

    #[test]
    fn work_cannot_start_while_sleeping() {
        let dir = temp_dir("no-work-while-sleeping");
        let mut app = app_at(&dir);
        app.start_sleep();
        assert!(app.sleep.is_sleeping());

        app.start_work();
        assert!(!app.work.is_active());
    }

    #[test]
    fn going_to_bed_keeps_work_when_a_correction_is_pending() {
        let dir = temp_dir("pending-blocks-bed");
        let mut app = app_at(&dir);
        app.start_work();
        let bedtime = Local::now().timestamp() - 3600;
        app.sleep.go_to_bed_at(bedtime).unwrap();
        app.sleep.wake_up_at(bedtime + 1800).unwrap();
        assert!(app.sleep.has_pending());

        app.start_sleep();
        assert!(!app.sleep.is_sleeping());
        assert!(app.work.is_active());
    }

    #[test]
    fn usual_work_hours_are_saved_and_drive_the_work_target() {
        let dir = temp_dir("work-hours");
        let mut app = app_at(&dir);
        app.save_settings(
            "Dark".into(),
            true,
            "22:00".into(),
            "07:00".into(),
            "Monday".into(),
            "08:15".into(),
        )
        .unwrap();

        let snapshot = app.snapshot();
        assert_eq!(snapshot.usual_work_hours, "08:15");
        assert_eq!(snapshot.max_working_hour, "08h 15m");
        assert_eq!(snapshot.max_working_time, 8 * 3600 + 15 * 60);

        let stored = settings::Settings::parse(&dir.join("settings.json")).unwrap();
        assert_eq!(stored.usual_work_minutes(), 8 * 60 + 15);
    }

    #[test]
    fn usual_work_hours_reject_zero_and_out_of_range_times() {
        let dir = temp_dir("work-hours-invalid");
        let mut app = app_at(&dir);
        for value in ["00:00", "24:00", "noon"] {
            let result = app.save_settings(
                "Dark".into(),
                true,
                "22:00".into(),
                "07:00".into(),
                "Monday".into(),
                value.into(),
            );
            assert!(result.is_err(), "{value} should be rejected");
        }
    }
}
