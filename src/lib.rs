use std::{cell::RefCell, rc::Rc};

use slint::{Model, ModelRc, SharedString, VecModel};
use thiserror::Error;

mod app;
mod notification;
mod report_history;
mod reporter;
#[cfg(any(target_os = "android", test))]
mod rule_check;
mod settings;
mod sleep;
mod storage;
mod user;
mod widget;
mod work;

pub use app::error_details;
pub use reporter::report_fatal;

slint::include_modules!();

use app::{RuleConfig, TimelineBlock as AppTimelineBlock, TimelineDay as AppTimelineDay};

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    PlatformError(#[from] slint::PlatformError),

    #[error("{0}")]
    ApplicationError(#[from] app::Error),

    #[error("{0}")]
    NotificationError(#[from] Box<dyn std::error::Error>),
}

pub fn run(mobile: bool) -> Result<(), Error> {
    let ui = MainWindow::new()?;
    let reporter = reporter::Reporter::new(ui.as_weak());

    ui.set_is_mobile(mobile);
    ui.on_report_dismissed({
        let reporter = reporter.clone();
        move |id| reporter.dismiss(id)
    });
    ui.on_fatal_quit(reporter::Reporter::quit);

    match app::App::new(Box::new(reporter.clone())) {
        Ok(app) => {
            ui.set_app_version(app::App::version().into());

            let app = Rc::new(RefCell::new(app));
            app.borrow_mut().drain_widget_commands();

            let refresh = {
                let app = app.clone();
                let weak = ui.as_weak();
                move || {
                    if let Some(ui) = weak.upgrade() {
                        let snapshot = app.borrow().snapshot();
                        ui.set_today_working_hour(snapshot.today_working_hour.into());
                        ui.set_week_working_hour(snapshot.week_working_hour.into());
                        ui.set_max_working_hour(snapshot.max_working_hour.into());

                        ui.set_today_sleeping_hour(snapshot.today_sleeping_hour.into());
                        ui.set_week_sleeping_hour(snapshot.week_sleeping_hour.into());
                        ui.set_min_sleeping_hour(snapshot.min_sleeping_hour.into());

                        ui.set_max_working_time(to_i32(snapshot.max_working_time));
                        ui.set_current_working_time(to_i32(snapshot.current_working_time));
                        ui.set_max_sleeping_time(to_i32(snapshot.max_sleeping_time));
                        ui.set_current_sleeping_time(to_i32(snapshot.current_sleeping_time));

                        ui.set_work_running(snapshot.work_running);
                        ui.set_work_paused(snapshot.work_paused);
                        ui.set_sleeping(snapshot.sleeping);

                        ui.set_sleep_need_bedtime(snapshot.need_bedtime);
                        ui.set_sleep_pending(snapshot.pending_active);
                        ui.set_sleep_pending_seconds(to_i32(snapshot.pending_seconds));
                        ui.set_sleep_pending_hours(snapshot.pending_hours.into());
                        ui.set_sleep_pending_minutes(snapshot.pending_minutes.into());
                    }
                }
            };

            let refresh_settings = {
                let app = app.clone();
                let weak = ui.as_weak();
                move || {
                    if let Some(ui) = weak.upgrade() {
                        let snapshot = app.borrow().snapshot();
                        ui.set_settings_theme(snapshot.theme.into());
                        ui.set_settings_notifications(snapshot.notifications_enabled);
                        ui.set_settings_quiet_start(snapshot.quiet_start.into());
                        ui.set_settings_quiet_end(snapshot.quiet_end.into());
                        ui.set_settings_week_start(snapshot.week_start.into());
                        ui.set_settings_usual_work_hours(snapshot.usual_work_hours.into());
                    }
                }
            };

            let rule_model = Rc::new(VecModel::from(
                app.borrow()
                    .rule_configs()
                    .iter()
                    .map(config_to_edit)
                    .collect::<Vec<_>>(),
            ));
            ui.set_rule_edits(ModelRc::new(rule_model.clone()));

            let timeline_model = Rc::new(VecModel::from(Vec::<TimelineDay>::new()));
            let report_model = Rc::new(VecModel::from(Vec::<ReportItem>::new()));
            ui.set_timeline_days(ModelRc::new(timeline_model.clone()));
            ui.set_recent_reports(ModelRc::new(report_model.clone()));

            let refresh_dashboard = {
                let app = app.clone();
                let weak = ui.as_weak();
                let timeline_model = timeline_model.clone();
                let report_model = report_model.clone();
                move || {
                    if let Some(ui) = weak.upgrade() {
                        let (timeline, reports, stats) = {
                            let app = app.borrow();
                            let dashboard = app.dashboard_snapshot();
                            (dashboard.timeline, app.recent_reports(), dashboard.stats)
                        };
                        timeline_model.set_vec(
                            timeline
                                .days
                                .iter()
                                .map(timeline_day_to_slint)
                                .collect::<Vec<_>>(),
                        );
                        report_model
                            .set_vec(reports.iter().map(report_to_slint).collect::<Vec<_>>());
                        ui.set_week_label(timeline.week_label.into());
                        ui.set_timeline_now_seconds(to_i32(u64::from(timeline.now_seconds)));
                        ui.set_average_work(stats.average_work.into());
                        ui.set_average_sleep(stats.average_sleep.into());
                        ui.set_streak(to_i32(u64::from(stats.streak)));
                        ui.set_work_goal_percent(stats.work_goal_percent);
                        ui.set_sleep_goal_percent(stats.sleep_goal_percent);
                    }
                }
            };

            ui.on_tick({
                let app = app.clone();
                let refresh = refresh.clone();
                let refresh_dashboard = refresh_dashboard.clone();
                move || {
                    app.borrow_mut().tick();
                    refresh();
                    refresh_dashboard();
                }
            });

            ui.on_on_work_start({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().start_work();
                    refresh();
                }
            });
            ui.on_on_work_pause({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().pause_work();
                    refresh();
                }
            });
            ui.on_on_work_unpause({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().unpause_work();
                    refresh();
                }
            });
            ui.on_on_work_stop({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().stop_work();
                    refresh();
                }
            });

            ui.on_on_sleep_start({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().start_sleep();
                    refresh();
                }
            });
            ui.on_on_sleep_wake_up({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().wake_up_sleep();
                    refresh();
                }
            });
            ui.on_on_sleep_set_bedtime({
                let app = app.clone();
                let refresh = refresh.clone();
                move |time: SharedString| {
                    app.borrow_mut().set_bedtime(time.as_str());
                    refresh();
                }
            });
            ui.on_on_sleep_cancel_bedtime({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().cancel_bedtime();
                    refresh();
                }
            });
            ui.on_on_sleep_correct({
                let app = app.clone();
                let reporter = reporter.clone();
                let refresh = refresh.clone();
                move |hours: SharedString, minutes: SharedString| {
                    let result = app
                        .borrow_mut()
                        .correct_sleep(hours.as_str(), minutes.as_str());
                    if let Err(message) = result {
                        reporter.warning("Could not save sleep", message, "");
                    }
                    refresh();
                }
            });
            ui.on_on_sleep_discard({
                let app = app.clone();
                let refresh = refresh.clone();
                move || {
                    app.borrow_mut().discard_pending_sleep();
                    refresh();
                }
            });

            ui.on_open_url({
                let app = app.clone();
                move |url: SharedString| app.borrow_mut().open_url(url.as_str())
            });

            ui.on_rule_field_changed({
                let model = rule_model.clone();
                move |index: i32, field: SharedString, value: SharedString| {
                    let index = index as usize;
                    let Some(mut rule) = model.row_data(index) else {
                        return;
                    };
                    match field.as_str() {
                        "name" => rule.name = value,
                        "message" => rule.message = value,
                        "which-timer" => rule.which_timer = value,
                        "trigger-if" => rule.trigger_if = value,
                        "trigger-hours" => rule.trigger_hours = value,
                        "trigger-minutes" => rule.trigger_minutes = value,
                        "tolerance-hours" => rule.tolerance_hours = value,
                        "tolerance-minutes" => rule.tolerance_minutes = value,
                        _ => return,
                    }
                    model.set_row_data(index, rule);
                }
            });
            ui.on_rule_add({
                let model = rule_model.clone();
                move || model.push(blank_rule())
            });
            ui.on_rule_remove({
                let model = rule_model.clone();
                move |index: i32| {
                    if index >= 0 {
                        model.remove(index as usize);
                    }
                }
            });
            ui.on_rules_reset({
                let app = app.clone();
                let model = rule_model.clone();
                move || {
                    let configs = app.borrow().default_rule_configs();
                    set_rule_rows(&model, configs);
                }
            });
            ui.on_rules_save({
                let app = app.clone();
                let reporter = reporter.clone();
                let model = rule_model.clone();
                move || {
                    let configs = read_rule_rows(&model);
                    let result = app.borrow_mut().save_rules(configs);
                    match result {
                        Ok(()) => {
                            let configs = app.borrow().rule_configs();
                            set_rule_rows(&model, configs);
                        }
                        Err(message) => reporter.warning("Could not save rules", message, ""),
                    }
                }
            });
            ui.on_save_settings({
                let app = app.clone();
                let reporter = reporter.clone();
                let weak = ui.as_weak();
                let refresh_settings = refresh_settings.clone();
                let refresh_dashboard = refresh_dashboard.clone();
                move || {
                    let Some(ui) = weak.upgrade() else {
                        return;
                    };
                    let result = app.borrow_mut().save_settings(
                        ui.get_settings_theme().to_string(),
                        ui.get_settings_notifications(),
                        ui.get_settings_quiet_start().to_string(),
                        ui.get_settings_quiet_end().to_string(),
                        ui.get_settings_week_start().to_string(),
                        ui.get_settings_usual_work_hours().to_string(),
                    );
                    match result {
                        Ok(()) => {
                            refresh_settings();
                            refresh_dashboard();
                        }
                        Err(message) => {
                            reporter.warning("Could not save settings", message, "");
                            refresh_settings();
                        }
                    }
                }
            });

            refresh();
            refresh_settings();
            refresh_dashboard();
        }
        Err(e) => {
            reporter.fatal("Failed to start Rhythm", format!("{e}"), error_details(&e));
        }
    }

    ui.run().map_err(Error::PlatformError)
}

fn timeline_day_to_slint(day: &AppTimelineDay) -> TimelineDay {
    TimelineDay {
        name: day.name.clone().into(),
        today: day.today,
        work_label: day.work_label.clone().into(),
        sleep_label: day.sleep_label.clone().unwrap_or_default().into(),
        work_compact: day.work_compact.clone().into(),
        sleep_compact: day.sleep_compact.clone().unwrap_or_default().into(),
        sleep_low: day.sleep_low,
        blocks: ModelRc::new(VecModel::from(
            day.blocks
                .iter()
                .map(timeline_block_to_slint)
                .collect::<Vec<_>>(),
        )),
    }
}

fn timeline_block_to_slint(block: &AppTimelineBlock) -> TimelineBlock {
    TimelineBlock {
        kind: block.kind.code(),
        from_seconds: to_i32(u64::from(block.from_secs)),
        to_seconds: to_i32(u64::from(block.to_secs)),
        detail: block.detail.clone().into(),
    }
}

fn to_i32(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn report_to_slint(report: &report_history::ReportRecord) -> ReportItem {
    ReportItem {
        severity: report.severity.clone().into(),
        title: report.title.clone().into(),
        message: report.message.clone().into(),
        timestamp: report.timestamp.clone().into(),
    }
}

fn config_to_edit(config: &RuleConfig) -> RuleEdit {
    RuleEdit {
        id: config.id.clone().into(),
        name: config.name.clone().into(),
        message: config.message.clone().into(),
        which_timer: config.which_timer.clone().into(),
        trigger_if: config.trigger_if.clone().into(),
        trigger_hours: config.trigger_hours.clone().into(),
        trigger_minutes: config.trigger_minutes.clone().into(),
        tolerance_hours: config.tolerance_hours.clone().into(),
        tolerance_minutes: config.tolerance_minutes.clone().into(),
    }
}

fn edit_to_config(edit: &RuleEdit) -> RuleConfig {
    RuleConfig {
        id: edit.id.to_string(),
        name: edit.name.to_string(),
        message: edit.message.to_string(),
        which_timer: edit.which_timer.to_string(),
        trigger_if: edit.trigger_if.to_string(),
        trigger_hours: edit.trigger_hours.to_string(),
        trigger_minutes: edit.trigger_minutes.to_string(),
        tolerance_hours: edit.tolerance_hours.to_string(),
        tolerance_minutes: edit.tolerance_minutes.to_string(),
    }
}

fn blank_rule() -> RuleEdit {
    RuleEdit {
        id: "".into(),
        name: "New rule".into(),
        message: "".into(),
        which_timer: "working".into(),
        trigger_if: "more".into(),
        trigger_hours: "0".into(),
        trigger_minutes: "0".into(),
        tolerance_hours: "0".into(),
        tolerance_minutes: "0".into(),
    }
}

fn set_rule_rows(model: &VecModel<RuleEdit>, configs: Vec<RuleConfig>) {
    model.set_vec(configs.iter().map(config_to_edit).collect::<Vec<_>>());
}

fn read_rule_rows(model: &VecModel<RuleEdit>) -> Vec<RuleConfig> {
    (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .map(|edit| edit_to_config(&edit))
        .collect()
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    slint::android::init(app).unwrap();
    if let Err(e) = run(true) {
        report_fatal("Rhythm failed", &format!("{e}"), &error_details(&e));
    }
}
