use std::collections::HashSet;

use serde::{Deserialize, Serialize};

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
    /// Accumulated active working time in seconds.
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
        }
    }

    pub fn evaluate(
        &mut self,
        rules: &[UserRulesJson],
        values: &TimerValues,
    ) -> Vec<TriggeredRule> {
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
                "same" => value.abs_diff(rule.trigger_time) <= rule.tolerance,
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

        self.active = now_active;
        triggered
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
}
