use std::{
    fs, io,
    path::{Path, PathBuf},
};

use chrono::Local;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage;

const FILE_NAME: &str = "reports.json";
const MAX_REPORTS: usize = 10;

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),

    #[error("{0}")]
    Parsing(#[from] serde_json::Error),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ReportRecord {
    pub severity: String,
    pub title: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Serialize, Deserialize, Default)]
struct HistoryJson {
    #[serde(default)]
    reports: Vec<ReportRecord>,
}

/// Persisted, bounded history of user-facing reports.
pub struct ReportHistory {
    file: PathBuf,
    reports: Vec<ReportRecord>,
}

impl ReportHistory {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            file: config_dir.join(FILE_NAME),
            reports: Vec::new(),
        }
    }

    pub fn load(config_dir: &Path) -> Result<Self, Error> {
        let file = config_dir.join(FILE_NAME);
        let data: HistoryJson = match fs::read_to_string(&file) {
            Ok(data) => serde_json::from_str(&data)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => HistoryJson::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            file,
            reports: data.reports,
        })
    }

    pub fn record(&mut self, severity: &str, title: &str, message: &str) -> Result<(), Error> {
        self.reports.insert(
            0,
            ReportRecord {
                severity: severity.to_string(),
                title: title.to_string(),
                message: message.to_string(),
                timestamp: Local::now().format("%Y-%m-%d %H:%M").to_string(),
            },
        );
        self.reports.truncate(MAX_REPORTS);
        storage::write_json(
            &self.file,
            &HistoryJson {
                reports: self.reports.clone(),
            },
        )?;
        Ok(())
    }

    pub fn reports(&self) -> &[ReportRecord] {
        &self.reports
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn records_and_persists_reports() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-reports-{nanos}"));
        fs::create_dir_all(&dir).unwrap();

        let mut history = ReportHistory::new(&dir);
        history.record("warning", "Title", "Message").unwrap();
        let loaded = ReportHistory::load(&dir).unwrap();

        assert_eq!(loaded.reports().len(), 1);
        assert_eq!(loaded.reports()[0].title, "Title");
        assert_eq!(loaded.reports()[0].severity, "warning");
    }
}
