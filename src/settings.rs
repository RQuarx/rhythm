use std::{
    io::{self, BufWriter},
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

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

#[derive(Serialize, Deserialize)]
struct SettingsJson {
    // Keep reading the old misspelled key from existing config files.
    #[serde(alias = "appearence")]
    appearance: AppearanceJson,
    #[serde(default)]
    notifications: NotificationsJson,
}

impl Default for SettingsJson {
    fn default() -> Self {
        Self {
            appearance: AppearanceJson {
                selected_theme: "Dark".to_string(),
            },
            notifications: NotificationsJson::default(),
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
            data,
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

        settings.set_notifications_enabled(false);
        settings.set_quiet_hours(21 * 60, 6 * 60);
        settings.save_settings().unwrap();

        let reloaded = Settings::parse(&path).unwrap();
        assert_eq!(reloaded.selected_theme(), "Light");
        assert!(!reloaded.notifications_enabled());
        assert_eq!(reloaded.quiet_hours(), (21 * 60, 6 * 60));
    }
}
