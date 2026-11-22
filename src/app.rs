use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{Local, Timelike};
use thiserror::Error;
use tracing::{error, info};

use crate::{
    report_history, settings, sleep,
    user::{
        data::UserData,
        rules::{RuleEvaluator, TimerValues, UserRulesJson},
    },
    work,
};

const DEFAULT_WORK_THRESHOLD: u64 = 27_000;
const DEFAULT_SLEEP_THRESHOLD: u64 = 25_200;
const THEMES: [&str; 3] = ["System", "Dark", "Light"];
const TIMERS: [&str; 3] = ["working", "sleep", "system"];
const CONDITIONS: [&str; 3] = ["more", "less", "same"];

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to retrieve application directories")]
    AppDirsError,

    #[error("Failed to create application directories")]
    Io(#[from] std::io::Error),

    #[error("Failed to parse settings file")]
    SettingsError(#[from] settings::Error),
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

#[derive(Clone, Copy)]
pub enum UsagePeriod {
    Week,
    Month,
}

impl UsagePeriod {
    pub fn days(self) -> usize {
        match self {
            UsagePeriod::Week => 7,
            UsagePeriod::Month => 30,
        }
    }
}

#[derive(Clone)]
pub struct DayUsage {
    pub label: String,
    pub work: u64,
    pub paused: u64,
    pub sleep: u64,
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
    pub pending_seconds: u64,
    pub pending_hours: String,
    pub pending_minutes: String,
    pub theme: String,
    pub notifications_enabled: bool,
    pub quiet_start: String,
    pub quiet_end: String,
    pub chart_max: u64,
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
    need_bedtime: bool,
    pending_seconds: u64,
}

impl App {
    pub fn new(reports: Box<dyn ReportSink>) -> Result<Self, Error> {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();

        let config_path: PathBuf = match sysdirs::config_dir() {
            Some(d) => d.join("rhythm/").to_path_buf(),
            None => return Err(Error::AppDirsError),
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

        let pending_seconds = sleep.pending_seconds();

        Ok(Self {
            settings,
            user_data,
            work,
            sleep,
            report_history,
            evaluator: RuleEvaluator::new(),
            reports,
            need_bedtime: false,
            pending_seconds,
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
        self.evaluate(None);
    }

    pub fn start_work(&mut self) {
        let result = self.work.start();
        self.report_result(result);
    }

    pub fn pause_work(&mut self) {
        let result = self.work.pause();
        self.report_result(result);
    }

    pub fn unpause_work(&mut self) {
        let result = self.work.unpause();
        self.report_result(result);
    }

    pub fn stop_work(&mut self) {
        let result = self.work.stop();
        self.report_result(result);
    }

    pub fn start_sleep(&mut self) {
        let result = self.sleep.go_to_bed();
        self.report_result(result);
    }

    pub fn wake_up_sleep(&mut self) {
        match self.sleep.wake_up() {
            Ok(Some(seconds)) => self.pending_seconds = seconds,
            Ok(None) => self.need_bedtime = true,
            Err(e) => {
                self.report_result(Err::<(), _>(e));
                self.pending_seconds = self.sleep.pending_seconds();
            }
        }
    }

    pub fn set_bedtime(&mut self, time: &str) {
        self.need_bedtime = false;
        let Some(minutes) = parse_clock(time) else {
            self.report(
                Severity::Warning,
                "Invalid bedtime".into(),
                "Enter the bedtime as HH:MM.".into(),
                String::new(),
            );
            return;
        };

        let result = self.sleep.set_bedtime_minutes(minutes);
        self.report_result(result);
        match self.sleep.wake_up() {
            Ok(Some(seconds)) => self.pending_seconds = seconds,
            Ok(None) => {}
            Err(e) => {
                self.report_result(Err::<(), _>(e));
                self.pending_seconds = self.sleep.pending_seconds();
            }
        }
    }

    pub fn cancel_bedtime(&mut self) {
        self.need_bedtime = false;
    }

    pub fn correct_sleep(&mut self, hours: &str, minutes: &str) {
        let fallback = self.sleep.pending_seconds();
        let seconds = parse_duration(hours, minutes).unwrap_or(fallback);

        match self.sleep.commit_pending(seconds) {
            Ok(effective) => {
                self.pending_seconds = 0;
                if effective > 0 {
                    self.evaluate(Some(effective));
                }
            }
            Err(e) => self.report_result(Err::<(), _>(e)),
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

        self.evaluator = RuleEvaluator::new();
        Ok(())
    }

    /// Validates and persists the settings chosen on the Settings page.
    pub fn save_settings(
        &mut self,
        theme: String,
        notifications_enabled: bool,
        quiet_start: String,
        quiet_end: String,
    ) -> Result<(), String> {
        if !THEMES.contains(&theme.as_str()) {
            return Err(format!("Unknown theme '{theme}'."));
        }
        let start = parse_clock(&quiet_start)
            .ok_or_else(|| "Quiet hours start must be HH:MM.".to_string())?;
        let end =
            parse_clock(&quiet_end).ok_or_else(|| "Quiet hours end must be HH:MM.".to_string())?;

        let previous = (
            self.settings.selected_theme().to_string(),
            self.settings.notifications_enabled(),
            self.settings.quiet_hours(),
        );
        self.settings.set_selected_theme(theme);
        self.settings
            .set_notifications_enabled(notifications_enabled);
        self.settings.set_quiet_hours(start, end);
        if let Err(e) = self.settings.save_settings() {
            self.settings.set_selected_theme(previous.0);
            self.settings.set_notifications_enabled(previous.1);
            self.settings.set_quiet_hours(previous.2.0, previous.2.1);
            return Err(e.to_string());
        }
        Ok(())
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let work = self.work.elapsed();
        let max_work = self.threshold("working", DEFAULT_WORK_THRESHOLD);
        let min_sleep = self.threshold("sleep", DEFAULT_SLEEP_THRESHOLD);
        let current_sleep = if self.sleep.is_sleeping() {
            self.sleep.current_elapsed()
        } else {
            self.sleep.last_night()
        };
        let (quiet_start, quiet_end) = self.settings.quiet_hours();

        AppSnapshot {
            today_working_hour: format_duration(work),
            week_working_hour: format_duration(work),
            max_working_hour: format_duration(max_work),
            today_sleeping_hour: format_duration(self.sleep.today_total()),
            week_sleeping_hour: format_duration(self.sleep.week_total()),
            min_sleeping_hour: format_duration(min_sleep),
            max_working_time: max_work,
            current_working_time: work,
            max_sleeping_time: min_sleep,
            current_sleeping_time: current_sleep,
            work_running: self.work.is_active(),
            work_paused: self.work.is_paused(),
            sleeping: self.sleep.is_sleeping(),
            need_bedtime: self.need_bedtime,
            pending_seconds: self.pending_seconds,
            pending_hours: hours_text(self.pending_seconds),
            pending_minutes: minutes_text(self.pending_seconds),
            theme: self.settings.selected_theme().to_string(),
            notifications_enabled: self.settings.notifications_enabled(),
            quiet_start: clock_text(quiet_start),
            quiet_end: clock_text(quiet_end),
            chart_max: self.chart_max(max_work, min_sleep),
            stats: self.dashboard_stats(7, work, min_sleep),
        }
    }

    pub fn usage_series(&self, period: UsagePeriod) -> Vec<DayUsage> {
        let sleep = self.sleep.daily_totals();
        self.work
            .daily_series(period.days())
            .into_iter()
            .map(|(date, work, paused)| DayUsage {
                label: date.format("%a %d").to_string(),
                work,
                paused,
                sleep: sleep.get(&date).copied().unwrap_or(0),
            })
            .collect()
    }

    pub fn recent_reports(&self) -> Vec<report_history::ReportRecord> {
        self.report_history.reports().to_vec()
    }

    fn chart_max(&self, work_target: u64, sleep_target: u64) -> u64 {
        self.usage_series(UsagePeriod::Month)
            .iter()
            .flat_map(|day| [day.work, day.paused, day.sleep])
            .max()
            .unwrap_or(0)
            .max(work_target)
            .max(sleep_target)
            .max(1)
    }

    fn dashboard_stats(&self, days: usize, current_work: u64, sleep_target: u64) -> DashboardStats {
        let work_target = self.threshold("working", DEFAULT_WORK_THRESHOLD);
        let series = self.usage_series(UsagePeriod::Week);
        let divisor = days.max(1) as u64;
        let average_work: u64 = series.iter().take(days).map(|day| day.work).sum::<u64>() / divisor;
        let average_sleep: u64 =
            series.iter().take(days).map(|day| day.sleep).sum::<u64>() / divisor;

        let mut streak = 0;
        for (index, day) in series.iter().rev().enumerate() {
            if day.work == 0 && day.sleep == 0 {
                // Today may simply not be over yet; don't break the streak.
                if index == 0 {
                    continue;
                }
                break;
            }
            if day.work <= work_target && day.sleep >= sleep_target && day.sleep > 0 {
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

    fn threshold(&self, timer: &str, fallback: u64) -> u64 {
        self.user_data
            .rules()
            .iter()
            .find(|rule| rule.which_timer == timer)
            .map(|rule| rule.trigger_time)
            .unwrap_or(fallback)
    }

    fn rule_warnings_enabled(&self) -> bool {
        if !self.settings.notifications_enabled() {
            return false;
        }
        let (start, end) = self.settings.quiet_hours();
        if start == end {
            return true;
        }
        let time = Local::now().time();
        let minutes = time.hour() as u16 * 60 + time.minute() as u16;
        !is_quiet_hours(start, end, minutes)
    }

    fn evaluate(&mut self, sleep_value: Option<u64>) {
        let time = Local::now().time();
        let system = time.hour() as u64 * 3600 + time.minute() as u64 * 60 + time.second() as u64;
        let values = TimerValues {
            working: self.work.elapsed(),
            sleep: sleep_value,
            system,
        };
        let warnings_enabled = self.rule_warnings_enabled();

        for rule in self.evaluator.evaluate(self.user_data.rules(), &values) {
            if warnings_enabled {
                self.report(Severity::Warning, rule.name, rule.message, String::new());
            }
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

fn format_duration(seconds: u64) -> String {
    format!("{:02}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
}

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
    Some(hours * 3600 + minutes * 60)
}

fn parse_duration_fields(hours: &str, minutes: &str, label: &str) -> Result<u64, String> {
    parse_duration(hours, minutes)
        .ok_or_else(|| format!("Invalid {label}: enter hours and minutes (minutes < 60)."))
}

fn build_rules(configs: &[RuleConfig]) -> Result<Vec<UserRulesJson>, String> {
    let mut rules = Vec::with_capacity(configs.len());
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

/// Quiet hours may wrap around midnight. Equal start/end means no quiet window.
fn is_quiet_hours(start: u16, end: u16, minutes: u16) -> bool {
    if start == end {
        return false;
    }
    if start < end {
        minutes >= start && minutes < end
    } else {
        minutes >= start || minutes < end
    }
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
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0), "00h 00m");
        assert_eq!(format_duration(3720), "01h 02m");
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
        assert!(is_quiet_hours(start, end, 23 * 60));
        assert!(is_quiet_hours(start, end, 3 * 60));
        assert!(!is_quiet_hours(start, end, 12 * 60));
        assert!(!is_quiet_hours(start, start, 23 * 60));
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
        let rules = build_rules(&[config.clone()]).unwrap();
        assert_eq!(rules[0].id, "custom");
        assert_eq!(rules[0].trigger_time, 27_000);
        assert_eq!(rules[0].tolerance, 900);

        let invalid = RuleConfig {
            which_timer: "unknown".into(),
            ..config
        };
        assert!(build_rules(&[invalid]).is_err());
    }
}
