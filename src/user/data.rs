use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{storage, user::rules::UserRulesJson};

const FILE_NAME: &str = "user_data.json";
const DEFAULT_RULES: &str = include_str!("../../assets/config/user_data.json");

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),

    #[error("{0}")]
    Parsing(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
struct UserDataJson {
    version: u32,
    rules: Vec<UserRulesJson>,
}

impl Default for UserDataJson {
    fn default() -> Self {
        UserDataJson {
            version: 1,
            rules: vec![
                UserRulesJson {
                    id: "overwork".into(),
                    name: "Overwork detection".into(),
                    message: "You have worked too long, time to take a rest.".into(),
                    which_timer: "working".into(),
                    trigger_time: 27_000,
                    tolerance: 1_800,
                    trigger_if: "more".into(),
                },
                UserRulesJson {
                    id: "sleep".into(),
                    name: "Not enough sleep".into(),
                    message:
                        "You haven't gotten enough sleep, you should sleep earlier this night."
                            .into(),
                    which_timer: "sleep".into(),
                    trigger_time: 25_200,
                    tolerance: 0,
                    trigger_if: "less".into(),
                },
            ],
        }
    }
}

pub struct UserData {
    file: PathBuf,
    data: UserDataJson,
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            data: UserDataJson::default(),
        }
    }
}

impl UserData {
    /// Loads the config directory's rule file, falling back to the bundled rules.
    pub fn parse(config_dir: &Path) -> Result<Self, Error> {
        let file = config_dir.join(FILE_NAME);
        let mut data: UserDataJson = match fs::read_to_string(&file) {
            Ok(data) => serde_json::from_str(&data)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => serde_json::from_str(DEFAULT_RULES)?,
            Err(e) => return Err(e.into()),
        };

        // Rules saved before ids existed get a stable one based on their position.
        // Duplicate or empty ids are replaced so the evaluator always has a unique key.
        let mut seen = HashSet::new();
        for (index, rule) in data.rules.iter_mut().enumerate() {
            if rule.id.is_empty() || !seen.insert(rule.id.clone()) {
                let mut candidate = format!("rule-{index}");
                while !seen.insert(candidate.clone()) {
                    candidate.push('_');
                }
                rule.id = candidate;
            }
        }

        Ok(Self { file, data })
    }

    /// Bundled defaults associated with a config directory, for save/reset paths.
    pub fn default_at(config_dir: &Path) -> Self {
        Self {
            file: config_dir.join(FILE_NAME),
            data: UserDataJson::default(),
        }
    }

    pub fn save(&self) -> Result<(), Error> {
        Ok(storage::write_json(&self.file, &self.data)?)
    }

    pub fn rules(&self) -> &[UserRulesJson] {
        &self.data.rules
    }

    pub fn set_rules(&mut self, rules: Vec<UserRulesJson>) {
        self.data.rules = rules;
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn old_rules_get_stable_ids_and_edits_persist() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rhythm-data-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(FILE_NAME),
            r#"{"version":1,"rules":[{"name":"A","message":"B","which-timer":"working","trigger-time":100,"tolerance":0,"trigger-if":"more"}]}"#,
        )
        .unwrap();

        let mut data = UserData::parse(&dir).unwrap();
        assert_eq!(data.rules()[0].id, "rule-0");

        data.set_rules(vec![]);
        data.save().unwrap();
        assert!(UserData::parse(&dir).unwrap().rules().is_empty());
    }
}
