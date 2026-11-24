use std::{
    io::{self, BufWriter},
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use chrono::{Local, Timelike, Weekday};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

/// The first day of the week used by the dashboard and weekly totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeekStart {
    #[default]
    Monday,
    Saturday,
    Sunday,
}

impl WeekStart {
    pub fn label(self) -> &'static str {
        match self {
            Self::Monday => "Monday",
            Self::Saturday => "Saturday",
            Self::Sunday => "Sunday",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Monday" => Some(Self::Monday),
            "Saturday" => Some(Self::Saturday),
            "Sunday" => Some(Self::Sunday),
            _ => None,
        }
    }

    pub fn weekday(self) -> Weekday {
        match self {
            Self::Monday => Weekday::Mon,
            Self::Saturday => Weekday::Sat,
            Self::Sunday => Weekday::Sun,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct AppearanceJson {
    selected_theme: String,
}

#[derive(Serialize, Deserialize)]
struct NotificationsJson {
    enabled: bool,
    quiet_start: u16,
    quiet_end: u16,
}

impl Default for NotificationsJson {
    fn default() -> Self {
        Self {
            enabled: true,
            quiet_start: 22 * 60,
            quiet_end: 7 * 60,
        }
    }
}

fn default_usual_work_minutes() -> u16 {
    7 * 60 + 30
}

#[derive(Serialize, Deserialize)]
struct WorkJson {
    #[serde(default = "default_usual_work_minutes")]
    usual_minutes: u16,
}

impl Default for WorkJson {
    fn default() -> Self {
        Self {
            usual_minutes: default_usual_work_minutes(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SettingsJson {
    // Keep reading the old misspelled key from existing config files.
    #[serde(alias = "appearence")]
    appearance: AppearanceJson,
    #[serde(default)]
    notifications: NotificationsJson,
    #[serde(default)]
    work: WorkJson,
    #[serde(default = "default_week_start")]
    week_start: String,
}

fn default_week_start() -> String {
    WeekStart::default().label().to_string()
}

impl Default for SettingsJson {
    fn default() -> Self {
        Self {
            appearance: AppearanceJson {
                selected_theme: "Dark".to_string(),
            },
            notifications: NotificationsJson::default(),
            work: WorkJson::default(),
            week_start: default_week_start(),
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    IoError(#[from] io::Error),

    #[error("{0}")]
    ParsingError(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Settings {
    file: PathBuf,
    data: SettingsJson,
}

impl Settings {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            file: config_dir.join("settings.json"),
            data: SettingsJson::default(),
        }
    }

    pub fn parse(filepath: &Path) -> Result<Settings> {
        info!("Retrieving settings from `{}`.", filepath.display());

        let data = match std::fs::read_to_string(filepath) {
            Ok(data) => serde_json::from_str(&data)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => SettingsJson::default(),
            Err(e) => return Err(Error::IoError(e)),
        };

        Ok(Settings {
            file: filepath.to_path_buf(),
            data: normalize(data),
        })
    }

    pub fn save_settings(&self) -> Result<()> {
        let mut writer = BufWriter::new(AtomicWriteFile::options().open(&self.file)?);
        serde_json::to_writer_pretty(&mut writer, &self.data)?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .commit()
            .map_err(Error::IoError)
    }

    pub fn selected_theme(&self) -> &str {
        &self.data.appearance.selected_theme
    }

    pub fn set_selected_theme(&mut self, selected: String) {
        self.data.appearance.selected_theme = selected;
    }

    pub fn notifications_enabled(&self) -> bool {
        self.data.notifications.enabled
    }

    pub fn set_notifications_enabled(&mut self, enabled: bool) {
        self.data.notifications.enabled = enabled;
    }

    pub fn quiet_hours(&self) -> (u16, u16) {
        (
            self.data.notifications.quiet_start,
            self.data.notifications.quiet_end,
        )
    }

    pub fn set_quiet_hours(&mut self, start: u16, end: u16) {
        self.data.notifications.quiet_start = start;
        self.data.notifications.quiet_end = end;
    }

    pub fn usual_work_minutes(&self) -> u16 {
        self.data.work.usual_minutes
    }

    pub fn set_usual_work_minutes(&mut self, minutes: u16) {
        self.data.work.usual_minutes = minutes;
    }

    pub fn usual_work_seconds(&self) -> u64 {
        u64::from(self.usual_work_minutes()) * 60
    }

    pub fn week_start(&self) -> WeekStart {
        WeekStart::parse(&self.data.week_start).unwrap_or_default()
    }

    pub fn set_week_start(&mut self, week_start: WeekStart) {
        self.data.week_start = week_start.label().to_string();
    }

    /// Whether rule reports should be shown right now.
    pub fn warnings_enabled(&self) -> bool {
        if !self.notifications_enabled() {
            return false;
        }
        let (start, end) = self.quiet_hours();
        if start == end {
            return true;
        }
        let time = Local::now().time();
        let minutes = time.hour() as u16 * 60 + time.minute() as u16;
        !is_quiet_hours(start, end, minutes)
    }
}

/// Quiet hours may wrap around midnight. Equal start/end means no quiet window.
pub fn is_quiet_hours(start: u16, end: u16, minutes: u16) -> bool {
    if start == end {
        return false;
    }
    if start < end {
        minutes >= start && minutes < end
    } else {
        minutes >= start || minutes < end
    }
}

/// Replaces values that the UI cannot represent with safe defaults.
fn normalize(mut data: SettingsJson) -> SettingsJson {
    if !matches!(
        data.appearance.selected_theme.as_str(),
        "System" | "Dark" | "Light"
    ) {
        data.appearance.selected_theme = "System".to_string();
    }
    if data.notifications.quiet_start >= 24 * 60 || data.notifications.quiet_end >= 24 * 60 {
        let defaults = NotificationsJson::default();
        data.notifications.quiet_start = defaults.quiet_start;
        data.notifications.quiet_end = defaults.quiet_end;
    }
    if data.work.usual_minutes == 0 || data.work.usual_minutes >= 24 * 60 {
        data.work.usual_minutes = default_usual_work_minutes();
    }
    if WeekStart::parse(&data.week_start).is_none() {
        data.week_start = default_week_start();
    }
    data
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn loads_legacy_key_and_round_trips() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-settings-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, r#"{"appearence":{"selected_theme":"Light"}}"#).unwrap();

        let mut settings = Settings::parse(&path).unwrap();
        assert_eq!(settings.selected_theme(), "Light");
        assert_eq!(settings.quiet_hours(), (22 * 60, 7 * 60));
        assert_eq!(settings.week_start(), WeekStart::Monday);
        assert_eq!(settings.usual_work_minutes(), 7 * 60 + 30);

        settings.set_notifications_enabled(false);
        settings.set_quiet_hours(21 * 60, 6 * 60);
        settings.set_week_start(WeekStart::Saturday);
        settings.set_usual_work_minutes(8 * 60);
        settings.save_settings().unwrap();

        let reloaded = Settings::parse(&path).unwrap();
        assert_eq!(reloaded.selected_theme(), "Light");
        assert!(!reloaded.notifications_enabled());
        assert_eq!(reloaded.quiet_hours(), (21 * 60, 6 * 60));
        assert_eq!(reloaded.week_start(), WeekStart::Saturday);
        assert_eq!(reloaded.usual_work_minutes(), 8 * 60);
    }

    #[test]
    fn invalid_values_fall_back_to_defaults() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-settings-invalid-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(
            &path,
            r#"{"appearance":{"selected_theme":"Neon"},"notifications":{"enabled":true,"quiet_start":9999,"quiet_end":8888},"work":{"usual_minutes":9999},"week_start":"Caturday"}"#,
        )
        .unwrap();

        let settings = Settings::parse(&path).unwrap();
        assert_eq!(settings.selected_theme(), "System");
        assert_eq!(settings.quiet_hours(), (22 * 60, 7 * 60));
        assert_eq!(settings.week_start(), WeekStart::Monday);
        assert_eq!(settings.usual_work_minutes(), 7 * 60 + 30);
    }
}
