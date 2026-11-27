use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::Path,
    sync::Mutex,
};

use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::storage;

const STATE_FILE: &str = "widget_state.json";
const COMMAND_FILE: &str = "widget_commands.json";
const DRAIN_FILE: &str = "widget_commands.drain.json";
const LAST_ID_FILE: &str = "widget_last_command.json";
const STATE_VERSION: u32 = 1;

static NATIVE_COMMANDS: Mutex<VecDeque<Command>> = Mutex::new(VecDeque::new());
static LAST_COMMAND_ID: Mutex<u64> = Mutex::new(0);

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StateData {
    pub version: u32,
    pub work_active: bool,
    pub work_paused: bool,
    pub work_seconds: u64,
    pub paused_seconds: u64,
    pub sleep_active: bool,
    pub sleep_seconds: u64,
    pub last_night_seconds: u64,
    pub work_target: u64,
    pub sleep_target: u64,
    pub theme: String,
    pub updated_at: String,
}

impl Default for StateData {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            work_active: false,
            work_paused: false,
            work_seconds: 0,
            paused_seconds: 0,
            sleep_active: false,
            sleep_seconds: 0,
            last_night_seconds: 0,
            work_target: 0,
            sleep_target: 0,
            theme: "System".to_string(),
            updated_at: String::new(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Command {
    pub id: u64,
    pub action: String,
    /// Unix timestamp (seconds) of the widget press. Commands can sit in the
    /// queue for a long time when the app is not running; applying them at the
    /// time they were pressed keeps the recorded intervals accurate.
    #[serde(default)]
    pub at: i64,
}

#[derive(Default, Deserialize, Serialize)]
struct CommandFile {
    #[serde(default)]
    commands: Vec<Command>,
}

#[derive(Default, Deserialize, Serialize)]
struct LastCommandId {
    id: u64,
}

fn signature(data: &StateData) -> String {
    let mut data = data.clone();
    data.updated_at = String::new();
    // Durations are written with second precision, but only a change in the
    // displayed minute (or in another field) should trigger a new write and a
    // widget refresh. The Chronometer ticks between writes on its own.
    data.work_seconds = data.work_seconds / 60 * 60;
    data.paused_seconds = data.paused_seconds / 60 * 60;
    data.sleep_seconds = data.sleep_seconds / 60 * 60;
    serde_json::to_string(&data).unwrap_or_default()
}

/// Writes the authoritative state for the widget.
///
/// The widget writes optimistic state into the same file when the user presses
/// a button, so we compare against the file itself rather than an in-memory
/// cache. That way stale optimistic state is always overwritten on the next
/// update.
pub fn write_state(config_dir: &Path, data: &StateData) {
    let path = config_dir.join(STATE_FILE);
    let expected = signature(data);

    if let Ok(text) = fs::read_to_string(&path)
        && let Ok(current) = serde_json::from_str::<StateData>(&text)
        && signature(&current) == expected
    {
        return;
    }

    let mut data = data.clone();
    data.updated_at = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    if let Err(e) = storage::write_json(&path, &data) {
        tracing::error!("Failed to write widget state: {e}");
        return;
    }
    notify_widget();
}

/// Returns newly queued commands, ordered by id.
///
/// The drained batch stays on disk until every command has been applied and
/// [`mark_command_applied`] persisted its id, so a process death in the middle
/// of a batch does not lose button presses or apply the batch twice.
pub fn drain_commands(config_dir: &Path) -> Vec<Command> {
    let file_commands = read_command_file(config_dir);
    let mut commands: Vec<Command> = NATIVE_COMMANDS.lock().unwrap().drain(..).collect();
    commands.extend(file_commands);

    let mut last_id = LAST_COMMAND_ID.lock().unwrap();
    *last_id = (*last_id).max(read_last_id(config_dir));

    // Older callers may send a command without an id. Assign one before the
    // id filter below; otherwise the synthesized id would look like an
    // already-applied command and be dropped.
    let mut next_id = (*last_id).saturating_add(1);
    for command in &mut commands {
        if command.id == 0 {
            command.id = next_id;
            next_id = next_id.saturating_add(1);
        }
    }
    commands.sort_by_key(|command| command.id);

    let mut seen = HashSet::new();
    commands
        .into_iter()
        // The widget writes a command to the file AND sends it through JNI,
        // so the same id can arrive twice in one drain.
        .filter(|command| command.id > *last_id && seen.insert(command.id))
        .collect()
}

/// Persists that one command has been applied.
///
/// Persist before applying the next command: if the process dies in between,
/// the next drain filters this command out instead of replaying the whole
/// batch. Returns false when the high-water mark could not be saved, in which
/// case the caller should leave the rest of the batch for the next run.
pub fn mark_command_applied(config_dir: &Path, id: u64) -> bool {
    let mut last_id = LAST_COMMAND_ID.lock().unwrap();
    if id <= *last_id {
        return true;
    }
    if let Err(e) = storage::write_json(&config_dir.join(LAST_ID_FILE), &LastCommandId { id }) {
        tracing::error!("Failed to persist widget command id: {e}");
        return false;
    }
    *last_id = id;
    true
}

/// Removes the drained batch after every command in it was applied.
pub fn finish_commands(config_dir: &Path) {
    let _ = fs::remove_file(config_dir.join(DRAIN_FILE));
}

fn read_last_id(config_dir: &Path) -> u64 {
    fs::read_to_string(config_dir.join(LAST_ID_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<LastCommandId>(&text).ok())
        .map(|data| data.id)
        .unwrap_or(0)
}

fn read_command_file(config_dir: &Path) -> Vec<Command> {
    let path = config_dir.join(COMMAND_FILE);
    let drain_path = config_dir.join(DRAIN_FILE);

    // A drain file is a batch that was not fully applied yet, either because
    // the app died while applying it or because removing it failed. Keep it
    // until finish_commands runs. New commands wait one tick while it is
    // recovered.
    if drain_path.exists() {
        return parse_command_file(&drain_path);
    }

    // Renaming is atomic: the widget can append to a fresh command file while
    // we process what was already queued, without a read-clear race.
    if fs::rename(&path, &drain_path).is_ok() {
        return parse_command_file(&drain_path);
    }

    Vec::new()
}

fn parse_command_file(path: &Path) -> Vec<Command> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<CommandFile>(&text)
        .map(|file| file.commands)
        .or_else(|_| serde_json::from_str::<Vec<Command>>(&text))
        .unwrap_or_default()
}

/// Called by the JNI bridge when the widget sends a command while the app is alive.
#[cfg(target_os = "android")]
fn queue_native_command(json: &str) {
    #[derive(Deserialize)]
    struct Incoming {
        action: String,
        #[serde(default)]
        id: u64,
        #[serde(default)]
        at: i64,
    }
    let Ok(incoming) = serde_json::from_str::<Incoming>(json) else {
        return;
    };
    NATIVE_COMMANDS.lock().unwrap().push_back(Command {
        id: incoming.id,
        action: incoming.action,
        at: incoming.at,
    });
}

#[cfg(target_os = "android")]
fn load_app_class<'local>(
    env: &mut jni::JNIEnv<'local>,
    context: &jni::objects::JObject<'local>,
    name: &str,
) -> Option<jni::objects::JClass<'local>> {
    use jni::{objects::JClass, objects::JValue};

    let loader = env
        .call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .ok()?
        .l()
        .ok()?;
    let name = env.new_string(name).ok()?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )
        .ok()?
        .l()
        .ok()?;
    Some(JClass::from(class))
}

#[cfg(target_os = "android")]
pub fn notify_widget() {
    use jni::{JavaVM, objects::JObject};

    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };

    // FindClass on a native thread uses the boot class loader, which cannot
    // see the app's classes. Resolve the provider through the activity's
    // class loader instead.
    let Some(class) = load_app_class(
        &mut env,
        &activity,
        "org.hibalance.rhythm.widget.RhythmWidgetProvider",
    ) else {
        let _ = env.exception_clear();
        return;
    };

    let result = env.call_static_method(
        class,
        "updateAll",
        "(Landroid/content/Context;)V",
        &[(&activity).into()],
    );
    if result.is_err() {
        let _ = env.exception_clear();
    }
}

#[cfg(not(target_os = "android"))]
pub fn notify_widget() {}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-widget-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Draining updates a process-wide high-water mark. Serialize the tests
    /// that use it and reset it so their ids cannot interfere.
    fn drain_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK.lock().unwrap();
        *LAST_COMMAND_ID.lock().unwrap() = 0;
        guard
    }

    #[test]
    fn authoritative_state_overwrites_optimistic_widget_state() {
        let dir = temp_dir("state");
        let state = StateData {
            work_active: false,
            theme: "Dark".into(),
            ..StateData::default()
        };
        write_state(&dir, &state);

        let mut optimistic = state.clone();
        optimistic.work_active = true;
        storage::write_json(&dir.join(STATE_FILE), &optimistic).unwrap();

        write_state(&dir, &state);
        let current: StateData =
            serde_json::from_str(&fs::read_to_string(dir.join(STATE_FILE)).unwrap()).unwrap();
        assert!(!current.work_active);
    }

    #[test]
    fn sub_minute_duration_changes_do_not_rewrite_state() {
        let dir = temp_dir("signature");
        let state = StateData {
            work_active: true,
            work_seconds: 3_600,
            ..StateData::default()
        };
        write_state(&dir, &state);
        let first = fs::read_to_string(dir.join(STATE_FILE)).unwrap();

        let mut later = state.clone();
        later.work_seconds = 3_659;
        write_state(&dir, &later);
        assert_eq!(fs::read_to_string(dir.join(STATE_FILE)).unwrap(), first);

        later.work_seconds = 3_660;
        write_state(&dir, &later);
        let updated: StateData =
            serde_json::from_str(&fs::read_to_string(dir.join(STATE_FILE)).unwrap()).unwrap();
        assert_eq!(updated.work_seconds, 3_660);
    }

    #[test]
    fn duplicate_command_ids_are_only_applied_once() {
        let _guard = drain_guard();
        let dir = temp_dir("commands");
        let command = Command {
            id: 42,
            action: "work-start".into(),
            at: 0,
        };
        storage::write_json(
            &dir.join(COMMAND_FILE),
            &CommandFile {
                commands: vec![command.clone(), command],
            },
        )
        .unwrap();

        let drained = drain_commands(&dir);
        assert_eq!(drained.len(), 1);
        assert!(mark_command_applied(&dir, drained[0].id));
        finish_commands(&dir);
        assert!(drain_commands(&dir).is_empty());
    }

    #[test]
    fn commands_without_ids_are_not_dropped() {
        let _guard = drain_guard();
        let dir = temp_dir("idless");
        storage::write_json(
            &dir.join(COMMAND_FILE),
            &CommandFile {
                commands: vec![Command {
                    id: 0,
                    action: "work-start".into(),
                    at: 1_700_000_000,
                }],
            },
        )
        .unwrap();

        let drained = drain_commands(&dir);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action, "work-start");
        assert_eq!(drained[0].at, 1_700_000_000);
        assert!(drained[0].id > 0);
        assert!(mark_command_applied(&dir, drained[0].id));
        finish_commands(&dir);
    }

    #[test]
    fn uncommitted_commands_are_retried() {
        let _guard = drain_guard();
        let dir = temp_dir("retry");
        storage::write_json(
            &dir.join(COMMAND_FILE),
            &CommandFile {
                commands: vec![Command {
                    id: 7,
                    action: "work-stop".into(),
                    at: 0,
                }],
            },
        )
        .unwrap();

        assert_eq!(drain_commands(&dir).len(), 1);
        // No mark yet, as if the process died while applying the command.
        let retried = drain_commands(&dir);
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].id, 7);
    }

    #[test]
    fn applied_commands_do_not_replay_from_a_leftover_drain_file() {
        let _guard = drain_guard();
        let dir = temp_dir("applied");
        let command = Command {
            id: 11,
            action: "sleep-wake".into(),
            at: 0,
        };
        storage::write_json(
            &dir.join(COMMAND_FILE),
            &CommandFile {
                commands: vec![command.clone()],
            },
        )
        .unwrap();

        let drained = drain_commands(&dir);
        assert_eq!(drained.len(), 1);
        assert!(mark_command_applied(&dir, drained[0].id));

        // Simulate a crash before the drain file could be removed: the id was
        // persisted, so the already-applied command must be filtered out.
        storage::write_json(
            &dir.join(DRAIN_FILE),
            &CommandFile {
                commands: vec![command],
            },
        )
        .unwrap();
        assert!(drain_commands(&dir).is_empty());
        finish_commands(&dir);
        assert!(!dir.join(DRAIN_FILE).exists());
    }

    #[test]
    fn a_partially_applied_batch_resumes_after_the_last_applied_id() {
        let _guard = drain_guard();
        let dir = temp_dir("partial");
        storage::write_json(
            &dir.join(COMMAND_FILE),
            &CommandFile {
                commands: vec![
                    Command {
                        id: 20,
                        action: "work-start".into(),
                        at: 0,
                    },
                    Command {
                        id: 21,
                        action: "work-stop".into(),
                        at: 0,
                    },
                ],
            },
        )
        .unwrap();

        let drained = drain_commands(&dir);
        assert_eq!(drained.len(), 2);
        assert!(mark_command_applied(&dir, drained[0].id));
        // The process dies before the second command is applied. Only the
        // remaining command is replayed.
        let retried = drain_commands(&dir);
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].id, 21);
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn Java_org_hibalance_rhythm_widget_RhythmNativeBridge_applyCommand(
    mut env: jni::JNIEnv,
    _this: jni::objects::JObject,
    command: jni::objects::JString<'_>,
) {
    let Ok(value) = env.get_string(&command) else {
        let _ = env.exception_clear();
        return;
    };
    queue_native_command(&String::from(value));
}
