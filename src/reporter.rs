use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use chrono::Local;
use slint::Weak;
use tracing::{error, warn};

use crate::{
    MainWindow, Report, ReportSeverity,
    app::{ReportSink, Severity},
    notification,
};

/// How long a warning stays on screen before it dismisses itself.
const WARNING_DISPLAY: Duration = Duration::from_secs(5);

/// A single report together with the id used to correlate dismissals.
struct Entry {
    id: i32,
    report: Report,
    timer_started: bool,
}

#[derive(Default)]
struct State {
    next_id: i32,
    active: Option<Entry>,
    queue: VecDeque<Entry>,
}

/// Reports warnings, errors, and fatalities to the user through the UI.
///
/// Reports are shown one at a time. Non-fatal reports are queued in the order
/// they arrive; a fatal report takes over immediately and discards the queue.
/// Cloning a `Reporter` shares the same queue, so it can be moved into any
/// Slint callback. Delivery to the UI always happens on the event loop.
#[derive(Clone)]
pub struct Reporter {
    ui: Weak<MainWindow>,
    state: Arc<Mutex<State>>,
}

impl Reporter {
    pub fn new(ui: Weak<MainWindow>) -> Self {
        Self {
            ui,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn warning(
        &self,
        title: impl Into<String>,
        message: impl Into<String>,
        details: impl Into<String>,
    ) {
        self.submit(
            ReportSeverity::Warning,
            title.into(),
            message.into(),
            details.into(),
        );
    }

    pub fn error(
        &self,
        title: impl Into<String>,
        message: impl Into<String>,
        details: impl Into<String>,
    ) {
        self.submit(
            ReportSeverity::Error,
            title.into(),
            message.into(),
            details.into(),
        );
    }

    pub fn fatal(
        &self,
        title: impl Into<String>,
        message: impl Into<String>,
        details: impl Into<String>,
    ) {
        self.submit(
            ReportSeverity::Fatal,
            title.into(),
            message.into(),
            details.into(),
        );
    }

    /// Called by the UI when a report is dismissed. The id guards against a
    /// late dismissal (e.g. a warning's timeout) dismissing a newer report.
    pub fn dismiss(&self, id: i32) {
        let mut state = self.state.lock().unwrap();
        if state.active.as_ref().is_some_and(|entry| entry.id == id) {
            state.active = state.queue.pop_front();
        }
        drop(state);

        self.present();
    }

    /// Terminates the application after the user acknowledges a fatal report.
    pub fn quit() {
        std::process::exit(1);
    }

    fn submit(&self, severity: ReportSeverity, title: String, message: String, details: String) {
        let is_fatal = matches!(severity, ReportSeverity::Fatal);

        match severity {
            ReportSeverity::Warning => {
                warn!("{title}: {message}");
                if !details.is_empty() {
                    warn!("Report details: {details}");
                }
            }
            ReportSeverity::Error | ReportSeverity::Fatal => {
                error!("{title}: {message}");
                if !details.is_empty() {
                    error!("Report details: {details}");
                }
            }
        }

        let notification_body = is_fatal.then(|| message.clone());
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut state = self.state.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;

        let entry = Entry {
            id,
            report: Report {
                severity,
                title: title.into(),
                message: message.into(),
                timestamp: timestamp.into(),
                details: details.into(),
            },
            timer_started: false,
        };

        if is_fatal {
            state.queue.clear();
            state.active = Some(entry);
        } else if state.active.is_none() {
            state.active = Some(entry);
        } else {
            state.queue.push_back(entry);
        }
        drop(state);

        if let Some(body) = notification_body
            && let Err(e) = notification::notify("Rhythm error", &body)
        {
            error!("Failed to show fatal notification: {e}");
        }

        self.present();
    }

    /// Pushes the active report (or hides the overlay when there is none).
    fn present(&self) {
        let active = self
            .state
            .lock()
            .unwrap()
            .active
            .as_ref()
            .map(|entry| (entry.id, entry.report.clone()));

        let ui = self.ui.clone();
        let reporter = self.clone();
        let _ = ui.upgrade_in_event_loop(move |ui| match active {
            Some((id, report)) => {
                let is_warning = matches!(report.severity, ReportSeverity::Warning);
                ui.set_active_report(report);
                ui.set_active_report_id(id);
                ui.set_report_visible(true);
                if is_warning {
                    reporter.start_warning_timer(id);
                }
            }
            None => ui.set_report_visible(false),
        });
    }

    /// Starts the auto-dismiss timer when a warning actually becomes visible.
    fn start_warning_timer(&self, id: i32) {
        let should_start = {
            let mut state = self.state.lock().unwrap();
            match state.active.as_mut() {
                Some(entry)
                    if entry.id == id
                        && matches!(entry.report.severity, ReportSeverity::Warning)
                        && !entry.timer_started =>
                {
                    entry.timer_started = true;
                    true
                }
                _ => false,
            }
        };

        if should_start {
            let reporter = self.clone();
            thread::spawn(move || {
                thread::sleep(WARNING_DISPLAY);
                reporter.dismiss(id);
            });
        }
    }
}

impl ReportSink for Reporter {
    fn report(&self, severity: Severity, title: String, message: String, details: String) {
        match severity {
            Severity::Warning => self.warning(title, message, details),
            Severity::Error => self.error(title, message, details),
        }
    }
}

/// Reports a fatal failure when no UI is available (e.g. after the event loop
/// has ended). Logs at error level and raises an OS notification.
pub fn report_fatal(title: &str, message: &str, details: &str) {
    error!("{title}: {message}");
    if !details.is_empty() {
        error!("Report details: {details}");
    }

    if let Err(e) = notification::notify(title, message) {
        error!("Failed to show fatal notification: {e}");
    }
}
