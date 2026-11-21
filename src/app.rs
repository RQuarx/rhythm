use std::{fs, path::{Path, PathBuf}};

use directories::ProjectDirs;
use thiserror::Error;

use crate::settings;

pub struct Application {
    config_path: PathBuf,
    settings: settings::Settings,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to retrieve application directories")]
    AppDirsError,

    #[error("Failed to create application directories")]
    Io(#[from] std::io::Error),

    #[error("Failed to parse settings file")]
    SettingsError(#[from] settings::Error),
}

impl Application {
    pub fn new() -> Result<Application, Error> {
        let config_path: PathBuf = match Application::get_dirs() {
            Some(d) => Ok(d.config_dir().to_path_buf()),
            None => Err(Error::AppDirsError),
        }?;

        fs::create_dir_all(&config_path)?;

        let settings = settings::Settings::parse(&Path::new(&config_path).join("settings.json"))?;

        Ok(Application {
            config_path,
            settings,
        })
    }

    fn get_dirs() -> Option<ProjectDirs> {
        ProjectDirs::from("org", "HiBalance", "rhythm")
    }
}
