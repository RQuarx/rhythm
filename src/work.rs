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

#[derive(Clone, Copy)]
enum WorkState {
    Stopped,
    Running { since: i64 },
    Paused { since: i64 },
}

/// A work or break interval clipped to one local date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkSegment {
    pub date: NaiveDate,
    /// Seconds after local midnight.
    pub from_secs: u32,
    pub to_secs: u32,
    pub active: bool,
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

    /// Seconds spent in the current pause, or zero when not paused.
    pub fn current_pause_elapsed(&self) -> u64 {
        match self.state {
            WorkState::Paused { since } => now().saturating_sub(since).max(0) as u64,
            WorkState::Stopped | WorkState::Running { .. } => 0,
        }
    }

    #[cfg(test)]
    pub fn elapsed(&self) -> u64 {
        let recorded: u64 = self
            .intervals
            .iter()
            .filter(|interval| interval.active)
            .map(|interval| interval.end.saturating_sub(interval.start).max(0) as u64)
            .sum();
        let current = match self.state {
            WorkState::Running { since } => now().saturating_sub(since).max(0) as u64,
            WorkState::Stopped | WorkState::Paused { .. } => 0,
        };
        self.accumulated + recorded + current
    }

    /// Active seconds recorded for today's local date.
    pub fn today_active(&self) -> u64 {
        self.daily_series(1)
            .last()
            .map(|(_, active, _)| *active)
            .unwrap_or(0)
    }

    /// Active seconds recorded between two local dates, inclusive.
    pub fn active_between(&self, first: NaiveDate, last: NaiveDate) -> u64 {
        self.series_between(first, last)
            .iter()
            .map(|(_, active, _)| *active)
            .sum()
    }

    /// Active and paused seconds for the last `days` local dates, oldest first.
    pub fn daily_series(&self, days: usize) -> Vec<(NaiveDate, u64, u64)> {
        let today = Local::now().date_naive();
        let Some(first) = first_date(today, days) else {
            return Vec::new();
        };
        self.series_between(first, today)
    }

    /// Active and paused seconds for every local date in an inclusive window.
    pub fn series_between(&self, first: NaiveDate, last: NaiveDate) -> Vec<(NaiveDate, u64, u64)> {
        if last < first {
            return Vec::new();
        }

        let mut totals: HashMap<NaiveDate, (u64, u64)> = HashMap::new();
        self.for_each_interval(|start, end, active| {
            add_interval(&mut totals, first, last, start, end, active);
        });

        let mut days = Vec::new();
        let mut date = first;
        loop {
            let (active, paused) = totals.get(&date).copied().unwrap_or((0, 0));
            days.push((date, active, paused));
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

    /// Work and break intervals clipped to an inclusive local-date window.
    pub fn segments_between(&self, first: NaiveDate, last: NaiveDate) -> Vec<WorkSegment> {
        if last < first {
            return Vec::new();
        }

        let mut segments = Vec::new();
        self.for_each_interval(|start, end, active| {
            add_segment(&mut segments, first, last, start, end, active);
        });
        segments.sort_by_key(|segment| (segment.date, segment.from_secs, !segment.active));
        segments
    }

    fn for_each_interval(&self, mut apply: impl FnMut(i64, i64, bool)) {
        for interval in &self.intervals {
            apply(interval.start, interval.end, interval.active);
        }
        match self.state {
            WorkState::Running { since } => apply(since, now(), true),
            WorkState::Paused { since } => apply(since, now(), false),
            WorkState::Stopped => {}
        }
    }

    /// Starts the timer at a timestamp captured by the caller. Used for
    /// widget commands that may be applied long after the button press.
    pub fn start_at(&mut self, at: i64) -> Result<(), Error> {
        if !matches!(self.state, WorkState::Stopped) {
            return Ok(());
        }
        let previous = self.state;
        self.state = WorkState::Running {
            since: event_time(at),
        };
        if let Err(e) = self.save() {
            self.state = previous;
            return Err(e);
        }
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), Error> {
        self.pause_at(now())
    }

    pub fn pause_at(&mut self, at: i64) -> Result<(), Error> {
        let WorkState::Running { since } = self.state else {
            return Ok(());
        };
        let at = event_time(at).max(since);
        let previous = self.state;
        self.intervals.push(WorkInterval {
            start: since,
            end: at,
            active: true,
        });
        self.state = WorkState::Paused { since: at };
        if let Err(e) = self.save() {
            self.intervals.pop();
            self.state = previous;
            return Err(e);
        }
        Ok(())
    }

    pub fn unpause_at(&mut self, at: i64) -> Result<(), Error> {
        let WorkState::Paused { since } = self.state else {
            return Ok(());
        };
        let at = event_time(at).max(since);
        let previous = self.state;
        self.intervals.push(WorkInterval {
            start: since,
            end: at,
            active: false,
        });
        self.state = WorkState::Running { since: at };
        if let Err(e) = self.save() {
            self.intervals.pop();
            self.state = previous;
            return Err(e);
        }
        Ok(())
    }

    pub fn stop_at(&mut self, at: i64) -> Result<(), Error> {
        let at = event_time(at);
        let previous = self.state;
        let pushed = match self.state {
            WorkState::Running { since } => {
                self.intervals.push(WorkInterval {
                    start: since,
                    end: at.max(since),
                    active: true,
                });
                true
            }
            WorkState::Paused { since } => {
                self.intervals.push(WorkInterval {
                    start: since,
                    end: at.max(since),
                    active: false,
                });
                true
            }
            WorkState::Stopped => false,
        };
        self.state = WorkState::Stopped;
        if let Err(e) = self.save() {
            if pushed {
                self.intervals.pop();
            }
            self.state = previous;
            return Err(e);
        }
        Ok(())
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

/// Clamps a queued event timestamp to a sane range: queued commands can be
/// old, and device clocks can move backwards.
fn event_time(at: i64) -> i64 {
    let now = now();
    if at <= 0 {
        now
    } else {
        at.clamp(now - MAX_DAYS * 24 * 60 * 60, now)
    }
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

fn add_interval(
    totals: &mut HashMap<NaiveDate, (u64, u64)>,
    first: NaiveDate,
    last: NaiveDate,
    start: i64,
    end: i64,
    active: bool,
) {
    if end <= start {
        return;
    }

    // Only split dates that are part of the requested window. This keeps the
    // work proportional to the graph period instead of the interval length,
    // without silently dropping totals from intervals spanning many days.
    let mut date = local_date(start).max(first);
    let final_date = local_date(end.saturating_sub(1)).min(last);
    while date <= final_date {
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
        let Some(next) = date.checked_add_signed(ChronoDuration::days(1)) else {
            break;
        };
        date = next;
    }
}

fn add_segment(
    segments: &mut Vec<WorkSegment>,
    first: NaiveDate,
    last: NaiveDate,
    start: i64,
    end: i64,
    active: bool,
) {
    if end <= start {
        return;
    }

    let mut date = local_date(start).max(first);
    let final_date = local_date(end.saturating_sub(1)).min(last);
    while date <= final_date {
        if let Some((day_start, day_end)) = day_bounds(date) {
            let from = start.max(day_start);
            let to = end.min(day_end);
            if to > from {
                let from_secs = (from - day_start).clamp(0, 24 * 3600) as u32;
                let to_secs = (to - day_start).clamp(0, 24 * 3600) as u32;
                if to_secs > from_secs {
                    segments.push(WorkSegment {
                        date,
                        from_secs,
                        to_secs,
                        active,
                    });
                }
            }
        }
        let Some(next) = date.checked_add_signed(ChronoDuration::days(1)) else {
            break;
        };
        date = next;
    }
}

fn first_date(today: NaiveDate, days: usize) -> Option<NaiveDate> {
    let span = i64::try_from(days.checked_sub(1)?).ok()?;
    today.checked_sub_signed(ChronoDuration::days(span))
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

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-work-{label}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pause_elapsed_only_counts_the_current_pause() {
        let mut timer = timer();
        timer.state = WorkState::Paused { since: now() - 120 };
        assert!(timer.current_pause_elapsed() >= 120);

        timer.state = WorkState::Stopped;
        assert_eq!(timer.current_pause_elapsed(), 0);
    }

    #[test]
    fn future_pause_start_does_not_wrap_to_a_huge_duration() {
        let mut timer = timer();
        timer.state = WorkState::Paused {
            since: now() + 60,
        };
        assert_eq!(timer.current_pause_elapsed(), 0);
    }

    #[test]
    fn queued_start_uses_the_original_timestamp() {
        let dir = temp_dir("start-at");
        let mut timer = WorkTimer::new(&dir);
        timer.start_at(now() - 120).unwrap();

        assert!(timer.is_active());
        assert!(!timer.is_paused());
        assert!(timer.elapsed() >= 120);
    }

    #[test]
    fn failed_save_does_not_leave_the_timer_active() {
        let dir = temp_dir("save-failure");
        let not_a_dir = dir.join("file");
        fs::write(&not_a_dir, "not a directory").unwrap();
        let mut timer = WorkTimer::new(&not_a_dir);

        assert!(timer.start_at(now()).is_err());
        assert!(!timer.is_active());
    }

    #[test]
    fn pauses_are_recorded_as_their_own_intervals() {
        let dir = temp_dir("pause-intervals");
        let mut timer = WorkTimer::new(&dir);
        let start = now() - 200;
        let pause = now() - 100;
        timer.intervals.push(WorkInterval {
            start,
            end: pause,
            active: true,
        });
        timer.state = WorkState::Paused { since: pause };

        assert_eq!(timer.elapsed(), 100);
        timer.unpause_at(now()).unwrap();
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
            yesterday,
            today,
            epoch(yesterday, 23, 0),
            epoch(today, 1, 0),
            true,
        );

        assert_eq!(totals.get(&yesterday).copied().unwrap().0, 3600);
        assert_eq!(totals.get(&today).copied().unwrap().0, 3600);
    }

    #[test]
    fn segments_keep_work_and_break_kinds_across_midnight() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut timer = timer();
        timer.intervals.push(WorkInterval {
            start: epoch(yesterday, 23, 0),
            end: epoch(today, 1, 0),
            active: true,
        });
        timer.intervals.push(WorkInterval {
            start: epoch(today, 1, 0),
            end: epoch(today, 1, 30),
            active: false,
        });

        let segments = timer.segments_between(yesterday, today);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].date, yesterday);
        assert!(segments[0].active);
        assert_eq!(segments[0].from_secs, 23 * 3600);
        assert_eq!(segments[1].date, today);
        assert!(segments[1].active);
        assert_eq!(segments[1].to_secs, 3600);
        assert_eq!(segments[2].date, today);
        assert!(!segments[2].active);
        assert_eq!(segments[2].to_secs, 5400);
    }

    #[test]
    fn long_intervals_are_counted_inside_the_window() {
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let mut totals = HashMap::new();
        add_interval(
            &mut totals,
            yesterday,
            today,
            epoch(today - ChronoDuration::days(401), 0, 0),
            epoch(today, 12, 0),
            true,
        );

        assert_eq!(totals.get(&yesterday).copied().unwrap().0, 24 * 3600);
        assert_eq!(totals.get(&today).copied().unwrap().0, 12 * 3600);
    }
}
