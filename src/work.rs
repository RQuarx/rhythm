use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{Duration as ChronoDuration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::storage;

const FILE_NAME: &str = "work.json";
const MAX_DAYS: i64 = 60;

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),

    #[error("{0}")]
    Parsing(#[from] serde_json::Error),
}

#[derive(Clone, Serialize, Deserialize)]
struct WorkInterval {
    start: i64,
    end: i64,
    active: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct WorkJson {
    /// Legacy all-time active total, preserved from older config files.
    accumulated: u64,
    running: bool,
    paused: bool,
    since: Option<i64>,
    paused_since: Option<i64>,
    #[serde(default)]
    intervals: Vec<WorkInterval>,
}

enum WorkState {
    Stopped,
    Running { since: i64 },
    Paused { since: i64 },
}

/// Accumulates active working time and records both active and paused
/// intervals so breaks are visible in the usage graph.
pub struct WorkTimer {
    file: PathBuf,
    accumulated: u64,
    state: WorkState,
    intervals: Vec<WorkInterval>,
}

impl WorkTimer {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            file: config_dir.join(FILE_NAME),
            accumulated: 0,
            state: WorkState::Stopped,
            intervals: Vec::new(),
        }
    }

    pub fn load(config_dir: &Path) -> Result<Self, Error> {
        let file = config_dir.join(FILE_NAME);
        let data: WorkJson = match fs::read_to_string(&file) {
            Ok(data) => serde_json::from_str(&data)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => WorkJson::default(),
            Err(e) => return Err(e.into()),
        };

        let state = match (data.running, data.paused, data.since, data.paused_since) {
            (true, _, Some(since), _) => WorkState::Running { since },
            (false, true, _, Some(since)) => WorkState::Paused { since },
            // Older files recorded a pause without a timestamp.
            (false, true, _, None) => WorkState::Paused { since: now() },
            _ => WorkState::Stopped,
        };

        Ok(Self {
            file,
            accumulated: data.accumulated,
            state,
            intervals: data.intervals,
        })
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, WorkState::Stopped)
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.state, WorkState::Paused { .. })
    }

    pub fn elapsed(&self) -> u64 {
        let recorded: u64 = self
            .intervals
            .iter()
            .filter(|interval| interval.active)
            .map(|interval| interval.end.saturating_sub(interval.start) as u64)
            .sum();
        let current = match self.state {
            WorkState::Running { since } => now().saturating_sub(since) as u64,
            WorkState::Stopped | WorkState::Paused { .. } => 0,
        };
        self.accumulated + recorded + current
    }

    /// Active and paused seconds for the last `days` local dates, oldest first.
    pub fn daily_series(&self, days: usize) -> Vec<(NaiveDate, u64, u64)> {
        let mut totals: HashMap<NaiveDate, (u64, u64)> = HashMap::new();
        for interval in &self.intervals {
            add_interval(&mut totals, interval.start, interval.end, interval.active);
        }
        match self.state {
            WorkState::Running { since } => add_interval(&mut totals, since, now(), true),
            WorkState::Paused { since } => add_interval(&mut totals, since, now(), false),
            WorkState::Stopped => {}
        }

        let today = Local::now().date_naive();
        (0..days)
            .map(|offset| {
                let date = today - ChronoDuration::days((days - 1 - offset) as i64);
                let (active, paused) = totals.get(&date).copied().unwrap_or((0, 0));
                (date, active, paused)
            })
            .collect()
    }

    pub fn start(&mut self) -> Result<(), Error> {
        if !matches!(self.state, WorkState::Stopped) {
            return Ok(());
        }
        self.state = WorkState::Running { since: now() };
        self.save()
    }

    pub fn pause(&mut self) -> Result<(), Error> {
        let WorkState::Running { since } = self.state else {
            return Ok(());
        };
        let now = now();
        self.intervals.push(WorkInterval {
            start: since,
            end: now,
            active: true,
        });
        self.state = WorkState::Paused { since: now };
        self.save()
    }

    pub fn unpause(&mut self) -> Result<(), Error> {
        let WorkState::Paused { since } = self.state else {
            return Ok(());
        };
        let now = now();
        self.intervals.push(WorkInterval {
            start: since,
            end: now,
            active: false,
        });
        self.state = WorkState::Running { since: now };
        self.save()
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        let now = now();
        match self.state {
            WorkState::Running { since } => self.intervals.push(WorkInterval {
                start: since,
                end: now,
                active: true,
            }),
            WorkState::Paused { since } => self.intervals.push(WorkInterval {
                start: since,
                end: now,
                active: false,
            }),
            WorkState::Stopped => {}
        }
        self.state = WorkState::Stopped;
        self.save()
    }

    fn save(&self) -> Result<(), Error> {
        let (running, paused, since, paused_since) = match self.state {
            WorkState::Running { since } => (true, false, Some(since), None),
            WorkState::Paused { since } => (false, true, None, Some(since)),
            WorkState::Stopped => (false, false, None, None),
        };
        let cutoff = now() - MAX_DAYS * 24 * 60 * 60;
        let intervals = self
            .intervals
            .iter()
            .filter(|interval| interval.end >= cutoff)
            .cloned()
            .collect();
        let data = WorkJson {
            accumulated: self.accumulated,
            running,
            paused,
            since,
            paused_since,
            intervals,
        };
        Ok(storage::write_json(&self.file, &data)?)
    }
}

fn now() -> i64 {
    Local::now().timestamp()
}

fn local_datetime(naive: NaiveDateTime) -> Option<chrono::DateTime<Local>> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(first, _) => Some(first),
        LocalResult::None => None,
    }
}

fn local_date(epoch: i64) -> NaiveDate {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|value| value.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

fn day_bounds(date: NaiveDate) -> Option<(i64, i64)> {
    let start = local_datetime(date.and_hms_opt(0, 0, 0)?)?.timestamp();
    let end = local_datetime((date + ChronoDuration::days(1)).and_hms_opt(0, 0, 0)?)?.timestamp();
    Some((start, end))
}

fn add_interval(totals: &mut HashMap<NaiveDate, (u64, u64)>, start: i64, end: i64, active: bool) {
    if end <= start {
        return;
    }
    let mut date = local_date(start);
    let last = local_date(end - 1);
    let mut guard = 0;
    while date <= last && guard < 400 {
        if let Some((day_start, day_end)) = day_bounds(date) {
            let from = start.max(day_start);
            let to = end.min(day_end);
            if to > from {
                let entry = totals.entry(date).or_insert((0, 0));
                if active {
                    entry.0 += (to - from) as u64;
                } else {
                    entry.1 += (to - from) as u64;
                }
            }
        }
        date += ChronoDuration::days(1);
        guard += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch(date: NaiveDate, hour: u32, minute: u32) -> i64 {
        local_datetime(date.and_hms_opt(hour, minute, 0).unwrap())
            .unwrap()
            .timestamp()
    }

    fn timer() -> WorkTimer {
        WorkTimer::new(Path::new("/tmp"))
    }

    #[test]
    fn pauses_are_recorded_as_their_own_intervals() {
        let mut timer = timer();
        timer.intervals.push(WorkInterval {
            start: 100,
            end: 200,
            active: true,
        });
        timer.state = WorkState::Paused { since: 200 };

        assert_eq!(timer.elapsed(), 100);
        timer.unpause().unwrap();
        assert_eq!(timer.intervals.len(), 2);
        assert!(!timer.intervals[1].active);
        assert_eq!(timer.elapsed(), 100);
    }

    #[test]
    fn intervals_are_split_at_midnight() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut totals = HashMap::new();
        add_interval(
            &mut totals,
            epoch(yesterday, 23, 0),
            epoch(today, 1, 0),
            true,
        );

        assert_eq!(totals.get(&yesterday).copied().unwrap().0, 3600);
        assert_eq!(totals.get(&today).copied().unwrap().0, 3600);
    }
}
