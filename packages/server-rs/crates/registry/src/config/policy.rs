use std::sync::{Arc, RwLock};

use opencode_config::entry::Entry;
use opencode_config::policy::{Action, Effect, decision, statements};
use opencode_config::service::ConfigService;
use opencode_location::state::Registration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::skill::{SkillEditor, SkillService};

pub fn apply(entries: &[Entry], editor: &mut SkillEditor<'_>) {
    let policies = statements(entries);
    let ids: Vec<String> = editor.list().iter().map(|skill| skill.id.0.clone()).collect();
    for id in ids {
        if decision(&policies, Action::SkillUse, &id) == Effect::Deny {
            editor.remove(&id);
        }
    }
}

pub struct ConfigPolicyHandle {
    _registration: Registration,
    task: JoinHandle<()>,
}

impl Drop for ConfigPolicyHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn register(config: &ConfigService, skills: &SkillService) -> ConfigPolicyHandle {
    let entries = Arc::new(RwLock::new(config.entries()));
    let registration = skills
        .transform({
            let entries = entries.clone();
            move |editor| {
                let entries = entries.read().expect("the config policy lock is never poisoned");
                apply(&entries, editor);
            }
        })
        .await;
    let mut updates = config.subscribe_updated();
    let task = tokio::spawn({
        let entries = entries.clone();
        let config = config.clone();
        let skills = skills.clone();
        async move {
            loop {
                match updates.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        *entries.write().expect("the config policy lock is never poisoned") = config.entries();
                        skills.reload().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    ConfigPolicyHandle { _registration: registration, task }
}
