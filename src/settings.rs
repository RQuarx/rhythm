use std::{
    io::{self, BufWriter, Write},
    path::PathBuf,
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
struct SettingsJson {
    appearence: AppearanceJson,
}

impl Default for SettingsJson {
    fn default() -> Self {
        Self {
            appearence: AppearanceJson {
                selected_theme: "Dark".to_string(),
            },
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
    pub fn parse(filepath: &PathBuf) -> Result<Settings> {
        info!("Retrieving settings from `{}`.", filepath.display());

        match std::fs::read_to_string(filepath) {
            Ok(data) => Ok(Settings {
                file: filepath.clone(),
                data: serde_json::from_str(&data)?,
            }),
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound => Ok(Settings {
                    file: filepath.clone(),
                    data: SettingsJson::default(),
                }),
                _ => Err(Error::IoError(e)),
            },
        }
    }

    pub fn save_settings(&self) -> Result<()> {
        let mut writer = BufWriter::new(AtomicWriteFile::options().open(&self.file)?);
        serde_json::to_writer_pretty(&mut writer, &self.data)?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .commit()
            .map_err(|e| Error::IoError(e))
    }

    pub fn selected_theme(&self) -> &str {
        &self.data.appearence.selected_theme
    }

    pub fn set_selected_theme(&mut self, selected: String) {
        self.data.appearence.selected_theme = selected;
    }
}
