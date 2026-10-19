//! Closed, revision-bound plans for local natural-language editing.
//!
//! Planning never executes command identifiers supplied by text. It maps a
//! bounded RU/EN vocabulary to a versioned enum and binds the result to the
//! exact active document state. UI shells may gather a semantic selection
//! asynchronously, but must validate the plan before its first mutation.

use photocraft_doc::DocId;
use serde::{Deserialize, Serialize};

use crate::{EngineError, Result, Session};

pub const ACTION_PLAN_VERSION: u8 = 1;
const MAX_PROMPT_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionIntent {
    RemoveBackground,
    BlurBackground,
    CopySelection,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPlan {
    pub version: u8,
    pub document: DocId,
    pub revision: u64,
    pub intent: ActionIntent,
}

impl Session {
    /// Parse a bounded prompt into the closed action allowlist and bind it to
    /// the exact active document state. This operation is revision-neutral.
    pub fn plan_ai_action(&self, prompt: &str) -> Result<ActionPlan> {
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(EngineError::Other(format!("AI edit prompt exceeds the {MAX_PROMPT_BYTES}-byte local limit")));
        }
        let state = self.active().ok_or(EngineError::NoDocument)?;
        let intent = classify(prompt).ok_or_else(|| {
            EngineError::Other("This local AI harness currently supports: remove background, blur background, and copy the selected object.".into())
        })?;
        Ok(ActionPlan { version: ACTION_PLAN_VERSION, document: state.doc.id, revision: state.revision, intent })
    }

    /// Fail closed if another document or revision replaced the state for
    /// which the plan was shown.
    pub fn validate_ai_action(&self, plan: &ActionPlan) -> Result<()> {
        if plan.version != ACTION_PLAN_VERSION {
            return Err(EngineError::Other(format!("AI action plan version {} is unsupported", plan.version)));
        }
        let state = self.active().ok_or(EngineError::NoDocument)?;
        if state.doc.id != plan.document || state.revision != plan.revision {
            return Err(EngineError::Other("AI action plan is stale; the document changed".into()));
        }
        Ok(())
    }
}

fn classify(prompt: &str) -> Option<ActionIntent> {
    let value = prompt.trim().to_lowercase().replace('ё', "е");
    let background = value.contains("фон") || value.contains("background");
    if background
        && (value.contains("убер")
            || value.contains("убрат")
            || value.contains("удал")
            || value.contains("remove")
            || value.contains("delete")
            || value.contains("cut out"))
    {
        Some(ActionIntent::RemoveBackground)
    } else if background && (value.contains("разм") || value.contains("блюр") || value.contains("blur")) {
        Some(ActionIntent::BlurBackground)
    } else if value.contains("скопир") || value.contains("copy selection") || value.contains("copy selected") {
        Some(ActionIntent::CopySelection)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        let mut session = Session::new();
        session.execute("file.new", json!({"width": 16, "height": 12})).unwrap();
        session
    }

    #[test]
    fn russian_and_english_prompts_map_only_to_the_closed_allowlist() {
        let session = session();
        assert_eq!(session.plan_ai_action("убрать фон").unwrap().intent, ActionIntent::RemoveBackground);
        assert_eq!(session.plan_ai_action("размой фон").unwrap().intent, ActionIntent::BlurBackground);
        assert_eq!(session.plan_ai_action("copy selected object").unwrap().intent, ActionIntent::CopySelection);
        assert!(session.plan_ai_action("execute javascript").is_err());
    }

    #[test]
    fn plan_is_revision_bound_and_planning_does_not_mutate() {
        let mut session = session();
        let revision = session.active().unwrap().revision;
        let plan = session.plan_ai_action("remove background").unwrap();
        assert_eq!(plan.revision, revision);
        assert!(session.validate_ai_action(&plan).is_ok());
        session.execute("layer.new.layer", json!({})).unwrap();
        assert!(session.validate_ai_action(&plan).is_err());
    }

    #[test]
    fn prompt_budget_fails_closed() {
        let session = session();
        assert!(session.plan_ai_action(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
    }
}
