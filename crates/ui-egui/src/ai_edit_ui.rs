//! Bounded prompt-to-command harness for local editing actions.
//!
//! Natural language never mutates documents directly. It selects one of a
//! small typed allowlist, gathers a semantic selection when required and then
//! runs ordinary engine commands, preserving their validation and history.

use crate::PhotocraftApp;
use photocraft_engine::ai_action_plan::ActionIntent as Intent;

fn has_selection(app: &PhotocraftApp) -> bool {
    app.session.active().is_some_and(|state| state.doc.selection.is_some())
}

/// Pick a small set of well-separated points inside the deterministic subject
/// mask. One centre point is often interpreted as an object part (for example
/// one leaf); spatially distributed foreground prompts describe the complete
/// main subject to SAM without hard-coding people, animals or product classes.
fn subject_seed_points(app: &PhotocraftApp) -> Vec<[f64; 2]> {
    let Some(selection) = app.session.active().and_then(|state| state.doc.selection.as_ref()) else { return Vec::new() };
    let bounds = selection.content_bounds();
    if bounds.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for gy in 0_i32..13 {
        let y = bounds.y0 + ((2 * gy + 1) * bounds.height() as i32) / 26;
        for gx in 0_i32..13 {
            let x = bounds.x0 + ((2 * gx + 1) * bounds.width() as i32) / 26;
            if selection.sample_channel(x, y, 0) > 0.5 {
                candidates.push([f64::from(x) + 0.5, f64::from(y) + 0.5]);
            }
        }
    }
    if candidates.is_empty() {
        return vec![[f64::from(bounds.x0 + bounds.x1) / 2.0, f64::from(bounds.y0 + bounds.y1) / 2.0]];
    }
    let centre = [f64::from(bounds.x0 + bounds.x1) / 2.0, f64::from(bounds.y0 + bounds.y1) / 2.0];
    let distance = |left: [f64; 2], right: [f64; 2]| (left[0] - right[0]).powi(2) + (left[1] - right[1]).powi(2);
    let first = candidates.iter().copied().min_by(|left, right| distance(*left, centre).total_cmp(&distance(*right, centre))).unwrap_or(centre);
    let mut selected = vec![first];
    while selected.len() < 5 && selected.len() < candidates.len() {
        let next = candidates.iter().copied().filter(|candidate| !selected.contains(candidate)).max_by(|left, right| {
            let left_distance = selected.iter().map(|seed| distance(*left, *seed)).fold(f64::INFINITY, f64::min);
            let right_distance = selected.iter().map(|seed| distance(*right, *seed)).fold(f64::INFINITY, f64::min);
            left_distance.total_cmp(&right_distance)
        });
        let Some(next) = next else { break };
        selected.push(next);
    }
    selected
}

/// Obtain a subject selection without assuming that the subject is a person.
/// The existing deterministic subject finder supplies a robust seed point;
/// MobileSAM replaces its rough mask when ready. If the model is still loading,
/// the classical mask remains a safe fallback instead of making the prompt do
/// nothing.
fn start_subject_selection(app: &mut PhotocraftApp, after: crate::semantic_select_ui::AfterApply) -> Result<bool, String> {
    let coalesce = match &after {
        crate::semantic_select_ui::AfterApply::Nothing => None,
        crate::semantic_select_ui::AfterApply::RemoveBackground(key)
        | crate::semantic_select_ui::AfterApply::BlurBackground(key)
        | crate::semantic_select_ui::AfterApply::CopySelection(key) => Some(key.clone()),
    };
    app.run("select.subject", serde_json::json!({"coalesce": coalesce}))?;
    let seeds = subject_seed_points(app);
    if seeds.is_empty() {
        return Err("Could not find a main object. Click the object once, then run the prompt again.".into());
    }
    let refined = crate::semantic_select_ui::select_points_then(app, &seeds, egui::Modifiers::NONE, after);
    if !refined && !has_selection(app) {
        return Err("Object selection produced no usable mask".into());
    }
    Ok(refined)
}

fn remove_background(app: &mut PhotocraftApp, refined: bool, coalesce: Option<&str>) -> Result<String, String> {
    app.run("layer.removeBackground", serde_json::json!({"useSelection": true, "coalesce": coalesce}))?;
    Ok(if refined {
        "Background removed with the local object model".into()
    } else if coalesce.is_none() {
        "Background removed from the current selection".into()
    } else {
        "Background removed with the local fallback; the semantic model is still loading".into()
    })
}

fn copy_selection(app: &mut PhotocraftApp) -> Result<String, String> {
    app.run("edit.copy", serde_json::json!({}))?;
    Ok("Selected object copied".into())
}

fn blur_background(app: &mut PhotocraftApp, coalesce: Option<&str>) -> Result<String, String> {
    let key = coalesce.map(str::to_owned).unwrap_or_else(|| format!("ai-blur-background-{}", app.ui.alloc_id()));
    let params = |extra: serde_json::Value| {
        let mut object = extra.as_object().cloned().unwrap_or_default();
        object.insert("coalesce".into(), key.clone().into());
        serde_json::Value::Object(object)
    };
    app.run("select.inverse", params(serde_json::json!({})))?;
    if let Err(error) = app.run("filter.blur.gaussianBlur", params(serde_json::json!({"radius": 12.0}))) {
        let _ = app.run("select.inverse", params(serde_json::json!({})));
        return Err(error);
    }
    app.run("select.inverse", params(serde_json::json!({})))?;
    Ok("Background blurred".into())
}

fn execute(app: &mut PhotocraftApp, intent: Intent, refined: bool, coalesce: Option<&str>) -> Result<String, String> {
    match intent {
        Intent::RemoveBackground => remove_background(app, refined, coalesce),
        Intent::BlurBackground => blur_background(app, coalesce),
        Intent::CopySelection => copy_selection(app),
    }
}

fn after(intent: Intent, key: String) -> crate::semantic_select_ui::AfterApply {
    use crate::semantic_select_ui::AfterApply;
    match intent {
        Intent::RemoveBackground => AfterApply::RemoveBackground(key),
        Intent::BlurBackground => AfterApply::BlurBackground(key),
        Intent::CopySelection => AfterApply::CopySelection(key),
    }
}

fn finish(app: &mut PhotocraftApp, result: Result<String, String>) {
    match result {
        Ok(message) => {
            app.ui.status = message;
            app.ui.status_error = false;
            app.ui.ai_prompt.clear();
        }
        Err(error) => {
            app.ui.status = error;
            app.ui.status_error = true;
        }
    }
}

pub(crate) fn continue_after_selection(app: &mut PhotocraftApp, action: crate::semantic_select_ui::AfterApply) {
    use crate::semantic_select_ui::AfterApply;
    let result = match &action {
        AfterApply::Nothing => return,
        AfterApply::RemoveBackground(key) => execute(app, Intent::RemoveBackground, true, Some(key)),
        AfterApply::BlurBackground(key) => execute(app, Intent::BlurBackground, true, Some(key)),
        AfterApply::CopySelection(key) => execute(app, Intent::CopySelection, true, Some(key)),
    };
    if result.is_err() {
        rollback_failed_selection(app, &action);
    }
    finish(app, result);
}

pub(crate) fn rollback_failed_selection(app: &mut PhotocraftApp, action: &crate::semantic_select_ui::AfterApply) {
    if !matches!(action, crate::semantic_select_ui::AfterApply::Nothing) {
        let _ = app.run("edit.undo", serde_json::json!({}));
    }
}

pub fn run_prompt(app: &mut PhotocraftApp) {
    let prompt = app.ui.ai_prompt.clone();
    let plan = match app.session.plan_ai_action(&prompt) {
        Ok(plan) => plan,
        Err(error) => {
            finish(app, Err(error.to_string()));
            return;
        }
    };
    if let Err(error) = app.session.validate_ai_action(&plan) {
        finish(app, Err(error.to_string()));
        return;
    }
    let intent = plan.intent;
    if has_selection(app) {
        let result = execute(app, intent, false, None);
        finish(app, result);
        return;
    }
    let key = format!("ai-action-{}", app.ui.alloc_id());
    match start_subject_selection(app, after(intent, key.clone())) {
        Ok(true) => {
            // The semantic worker owns the continuation; its exact-state reply
            // is polled from the frame loop.
        }
        Ok(false) => {
            // If the worker is unavailable, the deterministic subject mask is
            // still a visible, undoable fallback.
            let result = execute(app, intent, false, Some(&key));
            finish(app, result);
        }
        Err(error) => finish(app, Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::Session;
    use photocraft_geom::Rect;
    use photocraft_ml::{ModelIdentity, SegmentationMask};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn selected_app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
        app.session.execute("file.new", serde_json::json!({"width": 32, "height": 24})).unwrap();
        app.session.execute("select.rect", serde_json::json!({"x": 4, "y": 3, "width": 16, "height": 12})).unwrap();
        app
    }

    #[test]
    fn russian_remove_background_executes_the_edit_instead_of_stopping_at_selection() {
        let mut app = selected_app();
        app.ui.ai_prompt = "убери фон".into();
        run_prompt(&mut app);
        let state = app.session.active().unwrap();
        let layer = state.doc.layer(state.active_layer.unwrap()).unwrap();
        assert!(layer.mask.is_some(), "{}", app.ui.status);
        assert!(state.doc.selection.is_none());
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }

    #[test]
    fn unsupported_prompt_is_fail_closed_and_keeps_the_document() {
        let mut app = selected_app();
        let revision = app.session.active().unwrap().revision;
        app.ui.ai_prompt = "draw a dragon".into();
        run_prompt(&mut app);
        assert_eq!(app.session.active().unwrap().revision, revision);
        assert!(app.ui.status_error);
    }

    #[test]
    fn blur_background_keeps_the_foreground_selection_and_coalesces_history() {
        let mut app = selected_app();
        let history = app.session.active().unwrap().history.past_len();
        app.ui.ai_prompt = "размой фон".into();
        run_prompt(&mut app);
        let state = app.session.active().unwrap();
        assert!(state.doc.selection.is_some());
        assert_eq!(state.history.past_len(), history + 1);
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }

    #[test]
    fn automatic_subject_uses_separated_points_inside_the_seed_selection() {
        let app = selected_app();
        let seeds = subject_seed_points(&app);
        let selection = app.session.active().unwrap().doc.selection.as_ref().unwrap();
        assert_eq!(seeds.len(), 5);
        assert!(seeds.iter().all(|point| selection.sample_channel(point[0].floor() as i32, point[1].floor() as i32, 0) > 0.5));
        assert!(seeds.iter().any(|point| point[0] < 10.0));
        assert!(seeds.iter().any(|point| point[0] > 17.0));
    }

    #[test]
    fn async_subject_remove_background_is_one_undo_group() {
        let service = |request: crate::semantic_select_ui::Request| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = sender.send(Ok(SegmentationMask {
                width: request.width,
                height: request.height,
                alpha: vec![255; request.width as usize * request.height as usize],
                confidence: Some(0.95),
                model: ModelIdentity::mobile_sam_tiny(),
            }));
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { semantic_select: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 32, "height": 24})).unwrap();
        app.session
            .edit("fixture", |document, _| {
                let surface = document.layers[0].surface_mut().unwrap();
                let area = Rect::new(10, 7, 22, 17);
                surface.write_region(area, &vec![0.0; 12 * 10 * surface.channels()]);
                Ok(())
            })
            .unwrap();
        let history = app.session.active().unwrap().history.past_len();
        app.ui.ai_prompt = "убрать фон".into();

        run_prompt(&mut app);

        let state = app.session.active().unwrap();
        let layer = state.doc.layer(state.active_layer.unwrap()).unwrap();
        assert!(layer.mask.is_some(), "{}", app.ui.status);
        assert!(state.doc.selection.is_none());
        assert_eq!(state.history.past_len(), history + 1);
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }

    #[test]
    fn cancelling_async_ai_action_restores_pre_action_state() {
        let held_sender = Rc::new(RefCell::new(None));
        let sender_for_service = held_sender.clone();
        let service = move |_request: crate::semantic_select_ui::Request| {
            let (sender, receiver) = std::sync::mpsc::channel();
            *sender_for_service.borrow_mut() = Some(sender);
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { semantic_select: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 32, "height": 24})).unwrap();
        app.session
            .edit("fixture", |document, _| {
                let surface = document.layers[0].surface_mut().unwrap();
                surface.write_region(Rect::new(10, 7, 22, 17), &vec![0.0; 12 * 10 * surface.channels()]);
                Ok(())
            })
            .unwrap();
        let history = app.session.active().unwrap().history.past_len();
        app.ui.ai_prompt = "убери фон".into();
        run_prompt(&mut app);
        assert!(crate::semantic_select_ui::is_pending(&app));
        assert!(app.session.active().unwrap().doc.selection.is_some());

        assert!(crate::semantic_select_ui::cancel(&mut app));

        let state = app.session.active().unwrap();
        assert_eq!(state.history.past_len(), history);
        assert!(state.doc.selection.is_none());
        assert!(state.doc.layers[0].mask.is_none());
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }
}
