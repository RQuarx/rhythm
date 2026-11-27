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

/// A night clipped to one local date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepSegment {
    pub date: chrono::NaiveDate,
    /// Seconds after local midnight.
    pub from_secs: u32,
    pub to_secs: u32,
    /// Full length of the night, used to decide whether it was short.
    pub seconds: u64,
    /// True while the night is still in progress.
    pub active: bool,
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
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if let Err(e) = self.save() {
            self.pending = Some(pending);
            return Err(e);
        }
        Ok(())
    }

    /// Seconds since the recorded bedtime, without triggering any rules.
    pub fn current_elapsed(&self) -> u64 {
        self.bedtime
            .map(|bedtime| now().saturating_sub(bedtime) as u64)
            .unwrap_or(0)
    }

    pub fn go_to_bed(&mut self) -> Result<(), Error> {
        self.go_to_bed_at(now())
    }

    /// Records bedtime at a timestamp captured by the caller. A second
    /// "going to bed" action is a no-op instead of resetting the start time.
    pub fn go_to_bed_at(&mut self, at: i64) -> Result<(), Error> {
        if self.bedtime.is_some() {
            return Ok(());
        }
        let at = event_time(at);
        self.bedtime = Some(at);
        if let Err(e) = self.save() {
            self.bedtime = None;
            return Err(e);
        }
        Ok(())
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
        let previous = self.pending.replace(PendingNight {
            bedtime,
            wake_time,
            seconds,
        });
        if let Err(e) = self.save() {
            self.pending = previous;
            return Err(e);
        }
        Ok(seconds)
    }

    /// Ends the current interval and returns its raw duration for correction.
    pub fn wake_up(&mut self) -> Result<Option<u64>, Error> {
        self.wake_up_at(now())
    }

    /// Ends the current interval at a timestamp captured by the caller. Used
    /// for widget commands that may be applied after the fact.
    pub fn wake_up_at(&mut self, at: i64) -> Result<Option<u64>, Error> {
        let Some(bedtime) = self.bedtime.take() else {
            return Ok(None);
        };
        let wake_time = event_time(at).max(bedtime.saturating_add(1));
        let seconds = wake_time.saturating_sub(bedtime) as u64;
        let previous = self.pending.replace(PendingNight {
            bedtime,
            wake_time,
            seconds,
        });
        if let Err(e) = self.save() {
            self.bedtime = Some(bedtime);
            self.pending = previous;
            return Err(e);
        }
        Ok(Some(seconds))
    }

    /// Stores the user-approved duration and returns it for rule evaluation.
    pub fn commit_pending(&mut self, seconds: u64) -> Result<u64, Error> {
        let Some(pending) = self.pending.take() else {
            return Ok(0);
        };
        let previous_nights = self.nights.clone();
        self.nights.push(NightRecord {
            bedtime: pending.bedtime,
            wake_time: pending.wake_time,
            seconds,
        });
        if self.nights.len() > MAX_NIGHTS {
            self.nights.drain(..self.nights.len() - MAX_NIGHTS);
        }
        if let Err(e) = self.save() {
            self.pending = Some(pending);
            self.nights = previous_nights;
            return Err(e);
        }
        Ok(seconds)
    }

    /// Effective sleep from the most recently completed night, including a
    /// night that is still waiting for correction.
    pub fn last_night(&self) -> u64 {
        self.last_night_value().unwrap_or(0)
    }

    /// Effective sleep from the most recent night, if one has been recorded.
    pub fn last_night_value(&self) -> Option<u64> {
        self.pending
            .as_ref()
            .map(|pending| pending.seconds)
            .or_else(|| self.nights.last().map(|night| night.seconds))
    }

    pub fn today_total(&self) -> u64 {
        let today = Local::now().date_naive();
        self.total_between(today, today)
    }

    /// Effective sleep by wake-up date for an inclusive local-date window.
    pub fn series_between(
        &self,
        first: chrono::NaiveDate,
        last: chrono::NaiveDate,
    ) -> Vec<(chrono::NaiveDate, u64)> {
        if last < first {
            return Vec::new();
        }

        let mut totals: HashMap<chrono::NaiveDate, u64> = HashMap::new();
        for night in &self.nights {
            let date = local_date(night.wake_time);
            if date >= first && date <= last {
                *totals.entry(date).or_insert(0) += night.seconds;
            }
        }
        if let Some(pending) = &self.pending {
            let date = local_date(pending.wake_time);
            if date >= first && date <= last {
                *totals.entry(date).or_insert(0) += pending.seconds;
            }
        }
        let mut date = first;
        loop {
            let active = self.current_sleep_overlap(date);
            if active > 0 {
                *totals.entry(date).or_insert(0) += active;
            }
            if date >= last {
                break;
            }
            let Some(next) = date.checked_add_signed(ChronoDuration::days(1)) else {
                break;
            };
            date = next;
        }

        let mut days = Vec::new();
        let mut date = first;
        loop {
            days.push((date, totals.get(&date).copied().unwrap_or(0)));
            if date >= last {
                break;
            }
            let Some(next) = date.checked_add_signed(ChronoDuration::days(1)) else {
                break;
            };
            date = next;
        }
        days
    }

    /// Effective sleep total for an inclusive local-date window.
    pub fn total_between(&self, first: chrono::NaiveDate, last: chrono::NaiveDate) -> u64 {
        self.series_between(first, last)
            .iter()
            .map(|(_, seconds)| *seconds)
            .sum()
    }

    /// Sleep intervals clipped to an inclusive local-date window.
    pub fn segments_between(
        &self,
        first: chrono::NaiveDate,
        last: chrono::NaiveDate,
    ) -> Vec<SleepSegment> {
        if last < first {
            return Vec::new();
        }

        let mut segments = Vec::new();
        for night in &self.nights {
            add_sleep_segment(
                &mut segments,
                first,
                last,
                night.bedtime,
                night.wake_time,
                night.seconds,
                false,
            );
        }
        if let Some(pending) = &self.pending {
            add_sleep_segment(
                &mut segments,
                first,
                last,
                pending.bedtime,
                pending.wake_time,
                pending.seconds,
                false,
            );
        }
        if let Some(bedtime) = self.bedtime {
            let wake_time = now();
            add_sleep_segment(
                &mut segments,
                first,
                last,
                bedtime,
                wake_time,
                wake_time.saturating_sub(bedtime) as u64,
                true,
            );
        }
        segments.sort_by_key(|segment| (segment.date, segment.from_secs));
        segments
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

/// Clamps a queued event timestamp to a sane range. Widget commands can sit
/// in the queue for days, and device clocks can move backwards.
fn event_time(at: i64) -> i64 {
    let now = now();
    if at <= 0 {
        now
    } else {
        at.clamp(now - 60 * 24 * 60 * 60, now)
    }
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

fn add_sleep_segment(
    segments: &mut Vec<SleepSegment>,
    first: chrono::NaiveDate,
    last: chrono::NaiveDate,
    bedtime: i64,
    wake_time: i64,
    seconds: u64,
    active: bool,
) {
    if wake_time <= bedtime {
        return;
    }

    let mut date = local_date(bedtime).max(first);
    let final_date = local_date(wake_time.saturating_sub(1)).min(last);
    while date <= final_date {
        let Some(start_of_day) = day_start(date) else {
            return;
        };
        let Some(end_of_day) = day_start(date + ChronoDuration::days(1)) else {
            return;
        };
        let from = bedtime.max(start_of_day);
        let to = wake_time.min(end_of_day);
        if to > from {
            let from_secs = (from - start_of_day).clamp(0, 24 * 3600) as u32;
            let to_secs = (to - start_of_day).clamp(0, 24 * 3600) as u32;
            if to_secs > from_secs {
                segments.push(SleepSegment {
                    date,
                    from_secs,
                    to_secs,
                    seconds,
                    active,
                });
            }
        }
        let Some(next) = date.checked_add_signed(ChronoDuration::days(1)) else {
            break;
        };
        date = next;
    }
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

    fn temp_tracker(label: &str) -> SleepTracker {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-sleep-{label}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        SleepTracker::new(&dir)
    }

    #[test]
    fn pending_night_counts_until_confirmed() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut tracker = temp_tracker("pending");
        let seconds = 8 * 3600;
        tracker.pending = Some(PendingNight {
            bedtime: epoch(yesterday, 23, 0),
            wake_time: epoch(today, 7, 0),
            seconds,
        });

        assert_eq!(tracker.last_night(), seconds);
        assert_eq!(tracker.today_total(), seconds);
        assert_eq!(tracker.total_between(today, today), seconds);

        let segments = tracker.segments_between(yesterday, today);
        assert_eq!(segments.len(), 2);
        assert!(!segments[0].active);
        assert!(!segments[1].active);

        tracker.commit_pending(seconds).unwrap();
        assert!(!tracker.has_pending());
        assert_eq!(tracker.today_total(), seconds);
        assert_eq!(tracker.last_night(), seconds);
    }

    #[test]
    fn discarded_pending_night_is_not_counted() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut tracker = temp_tracker("discard");
        let seconds = 8 * 3600;
        tracker.pending = Some(PendingNight {
            bedtime: epoch(yesterday, 23, 0),
            wake_time: epoch(today, 7, 0),
            seconds,
        });

        assert_eq!(tracker.today_total(), seconds);
        tracker.discard_pending().unwrap();
        assert_eq!(tracker.today_total(), 0);
        assert_eq!(tracker.last_night(), 0);
        assert!(tracker.segments_between(yesterday, today).is_empty());
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
        assert_eq!(tracker.total_between(today, today), 27_000);
    }

    #[test]
    fn older_nights_are_excluded_from_window_total() {
        let today = Local::now().date_naive();
        let mut tracker = tracker();
        tracker.nights.push(NightRecord {
            bedtime: epoch(today - ChronoDuration::days(9), 22, 0),
            wake_time: epoch(today - ChronoDuration::days(8), 6, 0),
            seconds: 28_800,
        });

        assert_eq!(
            tracker.total_between(today - ChronoDuration::days(6), today),
            0
        );
    }

    #[test]
    fn sleep_is_split_across_midnight_for_the_timeline() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut tracker = tracker();
        tracker.nights.push(NightRecord {
            bedtime: epoch(yesterday, 23, 0),
            wake_time: epoch(today, 7, 0),
            seconds: 28_800,
        });

        let segments = tracker.segments_between(yesterday, today);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].date, yesterday);
        assert_eq!(segments[0].from_secs, 23 * 3600);
        assert_eq!(segments[0].to_secs, 24 * 3600);
        assert_eq!(segments[1].date, today);
        assert_eq!(segments[1].from_secs, 0);
        assert_eq!(segments[1].to_secs, 7 * 3600);
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

    #[test]
    fn going_to_bed_twice_keeps_the_first_timestamp() {
        let mut tracker = temp_tracker("idempotent");
        tracker.go_to_bed_at(now() - 60).unwrap();
        let first = tracker.bedtime.unwrap();

        tracker.go_to_bed_at(now()).unwrap();
        assert_eq!(tracker.bedtime, Some(first));
    }

    #[test]
    fn queued_wake_uses_the_original_timestamp() {
        let mut tracker = temp_tracker("wake-at");
        let now = now();
        tracker.go_to_bed_at(now - 3600).unwrap();

        let seconds = tracker.wake_up_at(now).unwrap().unwrap();
        assert!((3600..=3601).contains(&seconds));
        assert_eq!(tracker.pending_seconds(), seconds);
    }

    #[test]
    fn failed_save_restores_the_pending_night() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-sleep-save-failure-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        let not_a_dir = dir.join("file");
        fs::write(&not_a_dir, "not a directory").unwrap();
        let mut tracker = SleepTracker::new(&not_a_dir);
        tracker.pending = Some(PendingNight {
            bedtime: 100,
            wake_time: 200,
            seconds: 100,
        });

        assert!(tracker.discard_pending().is_err());
        assert!(tracker.has_pending());
        assert_eq!(tracker.pending_seconds(), 100);

        assert!(tracker.commit_pending(90).is_err());
        assert!(tracker.has_pending());
        assert!(tracker.nights.is_empty());
    }
}
