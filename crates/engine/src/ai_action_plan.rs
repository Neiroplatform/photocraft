//! Closed, revision-bound plans for local natural-language editing.
//!
//! Planning never executes command identifiers supplied by text. It maps a
//! bounded RU/EN vocabulary to a versioned enum and binds the result to the
//! exact active document state. UI shells may gather a semantic selection
//! asynchronously, but must validate the plan before its first mutation.

use photocraft_doc::DocId;
use serde::{Deserialize, Serialize};

use crate::{EngineError, Result, Session};

pub const ACTION_PLAN_VERSION: u8 = 2;
const MAX_PROMPT_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionIntent {
    RemoveBackground,
    BlurBackground,
    CopySelection,
    RemoveObject,
    AutoEnhance,
    Denoise,
    Sharpen,
    Resize { width: u32, height: u32 },
    ExpandCanvas { width: u32, height: u32 },
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
        let intent = classify(prompt).map_err(EngineError::Other)?.ok_or_else(|| {
            EngineError::Other(
                "Supported local edits: remove or blur background, remove or copy a selected object, auto enhance, denoise, sharpen, resize, and expand canvas.".into(),
            )
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

fn dimensions(value: &str) -> Option<(u32, u32)> {
    let normalized = value.replace('×', "x").replace("х", "x").replace("на", "x");
    for (index, ch) in normalized.char_indices() {
        if ch != 'x' {
            continue;
        }
        let left = normalized[..index].trim_end();
        let right = normalized[index + 1..].trim_start();
        let width_text = left.rsplit(|ch: char| !ch.is_ascii_digit()).next().unwrap_or("");
        let height_text = right.split(|ch: char| !ch.is_ascii_digit()).next().unwrap_or("");
        if let (Ok(width), Ok(height)) = (width_text.parse::<u32>(), height_text.parse::<u32>())
            && width > 0
            && height > 0
        {
            return Some((width, height));
        }
    }
    None
}

fn classify(prompt: &str) -> std::result::Result<Option<ActionIntent>, String> {
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
        Ok(Some(ActionIntent::RemoveBackground))
    } else if background && (value.contains("разм") || value.contains("блюр") || value.contains("blur")) {
        Ok(Some(ActionIntent::BlurBackground))
    } else if value.contains("скопир") || value.contains("copy selection") || value.contains("copy selected") {
        Ok(Some(ActionIntent::CopySelection))
    } else if (value.contains("удал") || value.contains("убер") || value.contains("remove") || value.contains("delete"))
        && (value.contains("объект")
            || value.contains("предмет")
            || value.contains("выдел")
            || value.contains("object")
            || value.contains("item")
            || value.contains("selection"))
    {
        Ok(Some(ActionIntent::RemoveObject))
    } else if value.contains("расшир") || value.contains("expand canvas") || value.contains("canvas expand") {
        dimensions(&value)
            .map(|(width, height)| ActionIntent::ExpandCanvas { width, height })
            .map(Some)
            .ok_or_else(|| "Canvas expansion needs a target size, for example: «расширь холст до 1920x1080».".into())
    } else if value.contains("размер") || value.contains("resize") || value.contains("scale image") {
        dimensions(&value)
            .map(|(width, height)| ActionIntent::Resize { width, height })
            .map(Some)
            .ok_or_else(|| "Resize needs a target size, for example: «изменить размер на 1200x800».".into())
    } else if value.contains("шум") || value.contains("denoise") || value.contains("reduce noise") {
        Ok(Some(ActionIntent::Denoise))
    } else if value.contains("резк") || value.contains("четк") || value.contains("sharpen") || value.contains("unblur") {
        Ok(Some(ActionIntent::Sharpen))
    } else if value.contains("улуч")
        || value.contains("автокорр")
        || value.contains("enhance")
        || value.contains("auto color")
        || value.contains("auto correct")
    {
        Ok(Some(ActionIntent::AutoEnhance))
    } else {
        Ok(None)
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
        let cases = [
            ("убрать фон", ActionIntent::RemoveBackground),
            ("размой фон", ActionIntent::BlurBackground),
            ("copy selected object", ActionIntent::CopySelection),
            ("удали выделенный объект", ActionIntent::RemoveObject),
            ("улучши фото", ActionIntent::AutoEnhance),
            ("убери шум", ActionIntent::Denoise),
            ("sharpen image", ActionIntent::Sharpen),
            ("изменить размер на 1200×800", ActionIntent::Resize { width: 1200, height: 800 }),
            ("expand canvas to 1920x1080", ActionIntent::ExpandCanvas { width: 1920, height: 1080 }),
        ];
        for (prompt, expected) in cases {
            assert_eq!(session.plan_ai_action(prompt).unwrap().intent, expected, "{prompt}");
        }
        assert!(session.plan_ai_action("execute javascript").is_err());
    }

    #[test]
    fn dimensions_are_required_for_sizing_actions() {
        let session = session();
        assert!(session.plan_ai_action("измени размер").unwrap_err().to_string().contains("1200x800"));
        assert!(session.plan_ai_action("расширь холст").unwrap_err().to_string().contains("1920x1080"));
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
