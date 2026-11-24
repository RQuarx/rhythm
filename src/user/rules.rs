use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::storage;

const STATE_FILE: &str = "rule_state.json";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UserRulesJson {
    /// Stable identity used by the evaluator to track notification crossings.
    #[serde(default)]
    pub id: String,

    pub name: String,
    pub message: String,

    /// A string telling the rule which timer to subscribe into.
    ///
    /// Available options are:
    /// - "working": Subscribes to the working timer.
    /// - "sleep": Subscribes to the sleep timer.
    /// - "system": Subscribes to the system time.
    ///
    /// A "system" timer might be used to notify working hour, rest hour, sleep hour, etc.
    /// A "working" timer might be used to notify when to stop working.
    /// A "sleep" timer might be used to notify a lack of sleep.
    pub which_timer: String,

    /// The time which triggers the notification (in seconds).
    pub trigger_time: u64,

    /// Tells when to trigger the rule.
    ///
    /// Available options:
    /// - "more": Trigger the rule when the timer is more than `trigger_time`.
    /// - "less": Trigger the rule when the timer is less than `trigger_time`.
    /// - "same": Trigger the rule when the timer is same with `trigger_time`.
    pub trigger_if: String,

    pub tolerance: u64,
}

/// Current values of the timers that rules can subscribe to.
pub struct TimerValues {
    /// Active working time for the current day, in seconds.
    pub working: u64,
    /// Effective sleep from the last completed night, if available.
    pub sleep: Option<u64>,
    /// Seconds since local midnight.
    pub system: u64,
}

pub struct TriggeredRule {
    pub name: String,
    pub message: String,
}

/// Evaluates user rules and reports each rule only on a false→true crossing.
pub struct RuleEvaluator {
    active: HashSet<String>,
    /// Shared with the Android background checker so both processes agree on
    /// which rules have already crossed.
    file: Option<PathBuf>,
}

#[derive(Default, Deserialize, Serialize)]
struct RuleStateJson {
    #[serde(default)]
    active: Vec<String>,
}

impl Default for RuleEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleEvaluator {
    pub fn new() -> Self {
        Self {
            active: HashSet::new(),
            file: None,
        }
    }

    /// Creates an evaluator whose crossing state is persisted in `config_dir`.
    pub fn load(config_dir: &Path) -> Self {
        let file = config_dir.join(STATE_FILE);
        let active = fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<RuleStateJson>(&text).ok())
            .map(|state| state.active.into_iter().collect())
            .unwrap_or_default();
        Self {
            active,
            file: Some(file),
        }
    }

    fn reload(&mut self) {
        let Some(file) = &self.file else {
            return;
        };
        let Ok(text) = fs::read_to_string(file) else {
            return;
        };
        let Ok(state) = serde_json::from_str::<RuleStateJson>(&text) else {
            return;
        };
        self.active = state.active.into_iter().collect();
    }

    fn persist(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut active: Vec<_> = self.active.iter().cloned().collect();
        active.sort();
        if let Err(e) = storage::write_json(file, &RuleStateJson { active }) {
            tracing::error!("Failed to save rule state: {e}");
        }
    }

    /// Clears all crossings, for example after the rule list was edited.
    pub fn reset(&mut self) {
        self.active.clear();
        self.persist();
    }

    pub fn evaluate(
        &mut self,
        rules: &[UserRulesJson],
        values: &TimerValues,
    ) -> Vec<TriggeredRule> {
        self.reload();

        let mut now_active = HashSet::new();
        let mut triggered = Vec::new();

        for rule in rules {
            let value = match rule.which_timer.as_str() {
                "working" => Some(values.working),
                "sleep" => values.sleep,
                "system" => Some(values.system),
                _ => None,
            };
            let Some(value) = value else { continue };

            let matches = match rule.trigger_if.as_str() {
                "more" => value > rule.trigger_time.saturating_add(rule.tolerance),
                "less" => value < rule.trigger_time.saturating_sub(rule.tolerance),
                "same" => same_matches(value, rule.trigger_time, rule.tolerance),
                _ => false,
            };
            if !matches {
                continue;
            }

            let key = if rule.id.is_empty() {
                rule.name.clone()
            } else {
                rule.id.clone()
            };
            now_active.insert(key.clone());
            if !self.active.contains(&key) {
                triggered.push(TriggeredRule {
                    name: rule.name.clone(),
                    message: rule.message.clone(),
                });
            }
        }

        if now_active != self.active {
            self.active = now_active;
            self.persist();
        }
        triggered
    }
}

/// A "same" rule with no tolerance uses minute precision. Trigger times are
/// entered as HH:MM, while the underlying timer values are seconds, so an
/// exact-second comparison would almost never match.
fn same_matches(value: u64, trigger: u64, tolerance: u64) -> bool {
    if tolerance == 0 {
        value / 60 == trigger / 60
    } else {
        value.abs_diff(trigger) <= tolerance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(trigger_if: &str, trigger_time: u64, tolerance: u64) -> UserRulesJson {
        UserRulesJson {
            id: "test".into(),
            name: "test".into(),
            message: "message".into(),
            which_timer: "working".into(),
            trigger_time,
            tolerance,
            trigger_if: trigger_if.into(),
        }
    }

    fn values(working: u64) -> TimerValues {
        TimerValues {
            working,
            sleep: None,
            system: 0,
        }
    }

    #[test]
    fn tolerance_is_grace_before_warning() {
        let rules = [rule("more", 100, 10)];
        let mut evaluator = RuleEvaluator::new();
        assert!(evaluator.evaluate(&rules, &values(105)).is_empty());
        assert!(evaluator.evaluate(&rules, &values(110)).is_empty());
        assert_eq!(evaluator.evaluate(&rules, &values(111)).len(), 1);
    }

    #[test]
    fn notifies_only_on_crossing() {
        let rules = [rule("more", 100, 0)];
        let mut evaluator = RuleEvaluator::new();
        assert_eq!(evaluator.evaluate(&rules, &values(101)).len(), 1);
        assert!(evaluator.evaluate(&rules, &values(200)).is_empty());
        assert!(evaluator.evaluate(&rules, &values(99)).is_empty());
        assert_eq!(evaluator.evaluate(&rules, &values(101)).len(), 1);
    }

    #[test]
    fn same_uses_tolerance_band() {
        let rules = [rule("same", 100, 5)];
        let mut evaluator = RuleEvaluator::new();
        assert_eq!(evaluator.evaluate(&rules, &values(96)).len(), 1);
        assert!(evaluator.evaluate(&rules, &values(105)).is_empty());
        assert!(evaluator.evaluate(&rules, &values(106)).is_empty());
    }

    #[test]
    fn same_without_tolerance_uses_minute_precision() {
        let rules = [rule("same", 120, 0)];
        let mut evaluator = RuleEvaluator::new();
        assert_eq!(evaluator.evaluate(&rules, &values(125)).len(), 1);
        assert!(evaluator.evaluate(&rules, &values(179)).is_empty());
        assert!(evaluator.evaluate(&rules, &values(180)).is_empty());
        assert_eq!(evaluator.evaluate(&rules, &values(121)).len(), 1);
    }

    #[test]
    fn crossings_persist_between_evaluators() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-rule-state-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let rules = [rule("more", 100, 0)];

        let mut first = RuleEvaluator::load(&dir);
        assert_eq!(first.evaluate(&rules, &values(101)).len(), 1);

        let mut second = RuleEvaluator::load(&dir);
        assert!(second.evaluate(&rules, &values(102)).is_empty());
        assert!(second.evaluate(&rules, &values(99)).is_empty());
        assert_eq!(second.evaluate(&rules, &values(101)).len(), 1);
    }
}
