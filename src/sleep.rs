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

    /// Whether a completed night is waiting for the user's correction.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Discards a pending night without recording it.
    pub fn discard_pending(&mut self) -> Result<(), Error> {
        if self.pending.take().is_none() {
            return Ok(());
        }
        self.save()
    }

    /// Seconds since the recorded bedtime, without triggering any rules.
    pub fn current_elapsed(&self) -> u64 {
        self.bedtime
            .map(|bedtime| now().saturating_sub(bedtime) as u64)
            .unwrap_or(0)
    }

    pub fn go_to_bed(&mut self) -> Result<(), Error> {
        self.bedtime = Some(now());
        self.save()
    }

    /// Creates a pending night for a manually entered bedtime. `wake_time` is
    /// the instant the user pressed "Woke up", so the recorded duration does
    /// not include the time spent answering the bedtime prompt.
    pub fn record_manual_night(&mut self, minutes: u16, wake_time: i64) -> Result<u64, Error> {
        let wake = Local
            .timestamp_opt(wake_time, 0)
            .single()
            .ok_or(Error::InvalidTime)?;
        let bedtime = resolve_bedtime(minutes, wake)?;
        if wake_time <= bedtime {
            return Err(Error::InvalidTime);
        }
        let seconds = (wake_time - bedtime) as u64;
        self.pending = Some(PendingNight {
            bedtime,
            wake_time,
            seconds,
        });
        self.save()?;
        Ok(seconds)
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

    /// Effective sleep from the most recent night, if one has been recorded.
    pub fn last_night_value(&self) -> Option<u64> {
        self.nights.last().map(|night| night.seconds)
    }

    pub fn today_total(&self) -> u64 {
        let today = Local::now().date_naive();
        let completed: u64 = self
            .nights
            .iter()
            .filter(|night| local_date(night.wake_time) == today)
            .map(|night| night.seconds)
            .sum();
        completed + self.current_sleep_overlap(today)
    }

    pub fn week_total(&self) -> u64 {
        let today = Local::now().date_naive();
        let first = today - ChronoDuration::days(6);
        let completed: u64 = self
            .nights
            .iter()
            .filter(|night| {
                let date = local_date(night.wake_time);
                (0..7).any(|days| today - ChronoDuration::days(days) == date)
            })
            .map(|night| night.seconds)
            .sum();
        let Some(start) = day_start(first) else {
            return completed;
        };
        let Some(end) = day_start(today + ChronoDuration::days(1)) else {
            return completed;
        };
        completed + self.active_sleep_in(start, end)
    }

    /// Portion of the in-progress night that falls inside the given date.
    fn current_sleep_overlap(&self, date: chrono::NaiveDate) -> u64 {
        let Some(start) = day_start(date) else {
            return 0;
        };
        let Some(end) = day_start(date + ChronoDuration::days(1)) else {
            return 0;
        };
        self.active_sleep_in(start, end)
    }

    fn active_sleep_in(&self, start: i64, end: i64) -> u64 {
        let Some(bedtime) = self.bedtime else {
            return 0;
        };
        let from = bedtime.max(start);
        let to = now().min(end);
        to.saturating_sub(from) as u64
    }

    /// Effective sleep totals by wake-up date, for the usage graph.
    pub fn daily_totals(&self) -> HashMap<chrono::NaiveDate, u64> {
        let mut totals = HashMap::new();
        for night in &self.nights {
            *totals.entry(local_date(night.wake_time)).or_insert(0) += night.seconds;
        }
        let today = Local::now().date_naive();
        let active = self.current_sleep_overlap(today);
        if active > 0 {
            *totals.entry(today).or_insert(0) += active;
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

fn day_start(date: chrono::NaiveDate) -> Option<i64> {
    local_datetime(date.and_hms_opt(0, 0, 0)?).map(|value| value.timestamp())
}

/// Resolves an "HH:MM" bedtime relative to a reference instant. Times later
/// than the reference belong to the previous day.
fn resolve_bedtime(minutes: u16, reference: DateTime<Local>) -> Result<i64, Error> {
    let time = NaiveTime::from_hms_opt((minutes / 60) as u32, (minutes % 60) as u32, 0)
        .ok_or(Error::InvalidTime)?;
    let mut naive = reference.date_naive().and_time(time);
    let mut local = local_datetime(naive).ok_or(Error::InvalidTime)?;

    if local.timestamp() > reference.timestamp() {
        naive -= ChronoDuration::days(1);
        local = local_datetime(naive).ok_or(Error::InvalidTime)?;
    }

    Ok(local.timestamp())
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
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn manual_night_uses_the_wake_button_time() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-sleep-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let mut tracker = SleepTracker::new(&dir);

        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let wake = epoch(today, 7, 0);
        let expected = (wake - epoch(yesterday, 23, 0)) as u64;

        assert_eq!(
            tracker.record_manual_night(23 * 60, wake).unwrap(),
            expected
        );
        assert!(tracker.has_pending());
        assert_eq!(tracker.pending_seconds(), expected);
    }
}
