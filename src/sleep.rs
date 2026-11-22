use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{
    DateTime, Duration as ChronoDuration, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage;

const FILE_NAME: &str = "sleep.json";
const MAX_NIGHTS: usize = 60;

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),

    #[error("{0}")]
    Parsing(#[from] serde_json::Error),

    #[error("Unable to resolve the entered bedtime in the local timezone")]
    InvalidTime,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingNight {
    bedtime: i64,
    wake_time: i64,
    seconds: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct NightRecord {
    bedtime: i64,
    wake_time: i64,
    seconds: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct SleepJson {
    bedtime: Option<i64>,
    pending: Option<PendingNight>,
    #[serde(default)]
    nights: Vec<NightRecord>,
}

/// Tracks sleep as discrete bedtime → wake-up intervals.
pub struct SleepTracker {
    file: PathBuf,
    bedtime: Option<i64>,
    pending: Option<PendingNight>,
    nights: Vec<NightRecord>,
}

impl SleepTracker {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            file: config_dir.join(FILE_NAME),
            bedtime: None,
            pending: None,
            nights: Vec::new(),
        }
    }

    pub fn load(config_dir: &Path) -> Result<Self, Error> {
        let file = config_dir.join(FILE_NAME);
        let data: SleepJson = match fs::read_to_string(&file) {
            Ok(data) => serde_json::from_str(&data)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => SleepJson::default(),
            Err(e) => return Err(e.into()),
        };

        Ok(Self {
            file,
            bedtime: data.bedtime,
            pending: data.pending,
            nights: data.nights,
        })
    }

    pub fn is_sleeping(&self) -> bool {
        self.bedtime.is_some()
    }

    pub fn pending_seconds(&self) -> u64 {
        self.pending
            .as_ref()
            .map(|pending| pending.seconds)
            .unwrap_or(0)
    }

    /// Seconds since the recorded bedtime, without triggering any rules.
    pub fn current_elapsed(&self) -> u64 {
        self.bedtime
            .map(|bedtime| now().saturating_sub(bedtime) as u64)
            .unwrap_or(0)
    }

    pub fn go_to_bed(&mut self) -> Result<(), Error> {
        self.bedtime = Some(now());
        self.pending = None;
        self.save()
    }

    /// Records a bedtime from an "HH:MM" entry. Times in the future are
    /// interpreted as belonging to the previous night.
    pub fn set_bedtime_minutes(&mut self, minutes: u16) -> Result<(), Error> {
        let time = NaiveTime::from_hms_opt((minutes / 60) as u32, (minutes % 60) as u32, 0)
            .ok_or(Error::InvalidTime)?;
        let now = Local::now();
        let mut naive = now.date_naive().and_time(time);
        let mut local = local_datetime(naive).ok_or(Error::InvalidTime)?;

        if local.timestamp() > now.timestamp() {
            naive -= ChronoDuration::days(1);
            local = local_datetime(naive).ok_or(Error::InvalidTime)?;
        }

        self.bedtime = Some(local.timestamp());
        self.save()
    }

    /// Ends the current interval and returns its raw duration for correction.
    pub fn wake_up(&mut self) -> Result<Option<u64>, Error> {
        let Some(bedtime) = self.bedtime.take() else {
            return Ok(None);
        };
        let wake_time = now();
        let seconds = wake_time.saturating_sub(bedtime) as u64;
        self.pending = Some(PendingNight {
            bedtime,
            wake_time,
            seconds,
        });
        self.save()?;
        Ok(Some(seconds))
    }

    /// Stores the user-approved duration and returns it for rule evaluation.
    pub fn commit_pending(&mut self, seconds: u64) -> Result<u64, Error> {
        let Some(pending) = self.pending.take() else {
            return Ok(0);
        };
        self.nights.push(NightRecord {
            bedtime: pending.bedtime,
            wake_time: pending.wake_time,
            seconds,
        });
        if self.nights.len() > MAX_NIGHTS {
            self.nights.drain(..self.nights.len() - MAX_NIGHTS);
        }
        self.save()?;
        Ok(seconds)
    }

    /// Effective sleep from the most recently completed night.
    pub fn last_night(&self) -> u64 {
        self.nights.last().map(|night| night.seconds).unwrap_or(0)
    }

    pub fn today_total(&self) -> u64 {
        let today = Local::now().date_naive();
        self.nights
            .iter()
            .filter(|night| local_date(night.wake_time) == today)
            .map(|night| night.seconds)
            .sum()
    }

    pub fn week_total(&self) -> u64 {
        let today = Local::now().date_naive();
        self.nights
            .iter()
            .filter(|night| {
                let date = local_date(night.wake_time);
                (0..7).any(|days| today - ChronoDuration::days(days) == date)
            })
            .map(|night| night.seconds)
            .sum()
    }

    /// Effective sleep totals by wake-up date, for the usage graph.
    pub fn daily_totals(&self) -> HashMap<chrono::NaiveDate, u64> {
        let mut totals = HashMap::new();
        for night in &self.nights {
            *totals.entry(local_date(night.wake_time)).or_insert(0) += night.seconds;
        }
        totals
    }

    fn save(&self) -> Result<(), Error> {
        let data = SleepJson {
            bedtime: self.bedtime,
            pending: self.pending.clone(),
            nights: self.nights.clone(),
        };
        Ok(storage::write_json(&self.file, &data)?)
    }
}

fn now() -> i64 {
    Local::now().timestamp()
}

fn local_datetime(naive: NaiveDateTime) -> Option<DateTime<Local>> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(first, _) => Some(first),
        LocalResult::None => None,
    }
}

fn local_date(epoch: i64) -> chrono::NaiveDate {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|value| value.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone};

    use super::*;

    fn epoch(date: NaiveDate, hour: u32, minute: u32) -> i64 {
        let naive = date.and_hms_opt(hour, minute, 0).unwrap();
        Local
            .from_local_datetime(&naive)
            .single()
            .unwrap()
            .timestamp()
    }

    fn tracker() -> SleepTracker {
        SleepTracker::new(Path::new("/tmp"))
    }

    #[test]
    fn night_crossing_midnight_counts_for_wake_day() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut tracker = tracker();
        tracker.nights.push(NightRecord {
            bedtime: epoch(yesterday, 23, 0),
            wake_time: epoch(today, 6, 30),
            seconds: 27_000,
        });

        assert_eq!(tracker.today_total(), 27_000);
        assert_eq!(tracker.week_total(), 27_000);
    }

    #[test]
    fn older_nights_are_excluded_from_week_total() {
        let today = Local::now().date_naive();
        let mut tracker = tracker();
        tracker.nights.push(NightRecord {
            bedtime: epoch(today - ChronoDuration::days(9), 22, 0),
            wake_time: epoch(today - ChronoDuration::days(8), 6, 0),
            seconds: 28_800,
        });

        assert_eq!(tracker.week_total(), 0);
    }
}
