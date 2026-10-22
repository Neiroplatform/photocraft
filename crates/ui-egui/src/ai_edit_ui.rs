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

/// Deterministic class-agnostic probes for automatic subject discovery. The ML
/// backend evaluates them independently, rejects border-attached background
/// proposals and joins related foreground pieces. They are intentionally not
/// derived from the classical saliency selector: treating its rough mask as
/// truth made a bad heuristic seed erase the actual person or product.
fn subject_seed_points(app: &PhotocraftApp) -> Vec<[f64; 2]> {
    let Some(state) = app.session.active() else { return Vec::new() };
    let bounds = state.doc.bounds();
    if bounds.is_empty() {
        return Vec::new();
    }
    let point = |x: f64, y: f64| [f64::from(bounds.x0) + x * f64::from(bounds.width()), f64::from(bounds.y0) + y * f64::from(bounds.height())];
    vec![point(0.5, 0.5), point(0.32, 0.38), point(0.32, 0.55), point(0.68, 0.38), point(0.68, 0.55)]
}

/// Obtain a subject selection without assuming that the subject is a person.
/// MobileSAM evaluates bounded class-agnostic probes and returns one foreground
/// proposal. The classical selector is used only when the semantic service is
/// unavailable, so a rough heuristic mask never biases model inference.
fn start_subject_selection(app: &mut PhotocraftApp, after: crate::semantic_select_ui::AfterApply) -> Result<bool, String> {
    let coalesce = match &after {
        crate::semantic_select_ui::AfterApply::Nothing => None,
        crate::semantic_select_ui::AfterApply::RemoveBackground(key)
        | crate::semantic_select_ui::AfterApply::BlurBackground(key)
        | crate::semantic_select_ui::AfterApply::CopySelection(key) => Some(key.clone()),
    };
    let seeds = subject_seed_points(app);
    if seeds.is_empty() {
        return Err("Could not find a main object. Click the object once, then run the prompt again.".into());
    }
    let refined = crate::semantic_select_ui::select_automatic_subject_then(app, &seeds, egui::Modifiers::NONE, after);
    if !refined {
        app.run("select.subject", serde_json::json!({"coalesce": coalesce}))?;
        if !has_selection(app) {
            return Err("Object selection produced no usable mask".into());
        }
    }
    Ok(refined)
}

fn remove_background(app: &mut PhotocraftApp, refined: bool, coalesce: Option<&str>) -> Result<String, String> {
    app.run("layer.removeBackground", serde_json::json!({"useSelection": true, "refineSelection": refined, "coalesce": coalesce}))?;
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

fn remove_object(app: &mut PhotocraftApp) -> Result<String, String> {
    if !has_selection(app) {
        return Err("Select the unwanted object first, then run the prompt again.".into());
    }
    app.run("edit.deleteAndFillSelection", serde_json::json!({}))?;
    Ok("Selected object removed with local content-aware fill".into())
}

fn run_local_command(app: &mut PhotocraftApp, command: &str, params: serde_json::Value, message: &str) -> Result<String, String> {
    app.run(command, params)?;
    Ok(message.into())
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
        Intent::RemoveObject => remove_object(app),
        Intent::AutoEnhance => run_local_command(app, "image.autoColor", serde_json::json!({}), "Image enhanced locally"),
        Intent::Denoise => run_local_command(
            app,
            "filter.noise.reduceNoise",
            serde_json::json!({"strength": 6, "preserveDetails": 60, "reduceColorNoise": 45, "sharpenDetails": 25}),
            "Noise reduced locally",
        ),
        Intent::Sharpen => {
            run_local_command(app, "filter.sharpen.smartSharpen", serde_json::json!({"amount": 100, "radius": 1, "reduceNoise": 10}), "Image sharpened locally")
        }
        Intent::Resize { width, height } => run_local_command(
            app,
            "image.imageSize",
            serde_json::json!({"width": width, "height": height, "resample": "preserveDetails"}),
            &format!("Image resized locally to {width}×{height}"),
        ),
        Intent::ExpandCanvas { width, height } => run_local_command(
            app,
            "image.canvasSize",
            serde_json::json!({"width": width, "height": height, "anchor": "center", "extensionColor": "transparent"}),
            &format!("Canvas expanded locally to {width}×{height}"),
        ),
    }
}

fn after(intent: Intent, key: String) -> crate::semantic_select_ui::AfterApply {
    use crate::semantic_select_ui::AfterApply;
    match intent {
        Intent::RemoveBackground => AfterApply::RemoveBackground(key),
        Intent::BlurBackground => AfterApply::BlurBackground(key),
        Intent::CopySelection => AfterApply::CopySelection(key),
        Intent::RemoveObject | Intent::AutoEnhance | Intent::Denoise | Intent::Sharpen | Intent::Resize { .. } | Intent::ExpandCanvas { .. } => {
            AfterApply::Nothing
        }
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
    if matches!(intent, Intent::AutoEnhance | Intent::Denoise | Intent::Sharpen | Intent::Resize { .. } | Intent::ExpandCanvas { .. }) {
        let result = execute(app, intent, false, None);
        finish(app, result);
        return;
    }
    if intent == Intent::RemoveObject && !has_selection(app) {
        finish(app, Err("Select the unwanted object first, then run the prompt again.".into()));
        return;
    }
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
    use photocraft_ml::{ModelIdentity, SegmentationMask, SegmentationMode};
    use std::cell::Cell;
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
    fn remove_object_requires_a_selection_and_never_selects_the_subject_automatically() {
        let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
        app.session.execute("file.new", serde_json::json!({"width": 32, "height": 24})).unwrap();
        let revision = app.session.active().unwrap().revision;
        app.ui.ai_prompt = "удали ненужный объект".into();
        run_prompt(&mut app);
        assert_eq!(app.session.active().unwrap().revision, revision);
        assert!(app.ui.status_error);
        assert!(app.ui.status.contains("Select the unwanted object"));
    }

    #[test]
    fn selected_object_is_removed_in_one_local_history_step() {
        let mut app = selected_app();
        let history = app.session.active().unwrap().history.past_len();
        app.ui.ai_prompt = "remove selected object".into();
        run_prompt(&mut app);
        assert_eq!(app.session.active().unwrap().history.past_len(), history + 1);
        assert!(!app.ui.status_error, "{}", app.ui.status);
        assert!(app.ui.status.contains("content-aware fill"));
    }

    #[test]
    fn document_wide_local_actions_skip_subject_selection() {
        let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
        app.session.execute("file.new", serde_json::json!({"width": 32, "height": 24})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        app.ui.ai_prompt = "улучши фото".into();
        run_prompt(&mut app);
        assert_eq!(app.session.active().unwrap().history.past_len(), history + 1);
        assert!(app.session.active().unwrap().doc.selection.is_none());
        assert!(!app.ui.status_error, "{}", app.ui.status);

        app.ui.ai_prompt = "изменить размер на 20x10".into();
        run_prompt(&mut app);
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_geom::Size::new(20, 10));
        assert!(!app.ui.status_error, "{}", app.ui.status);

        app.ui.ai_prompt = "расширь холст до 30x18".into();
        run_prompt(&mut app);
        assert_eq!(app.session.active().unwrap().doc.size, photocraft_geom::Size::new(30, 18));
        assert!(!app.ui.status_error, "{}", app.ui.status);
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
    fn automatic_subject_uses_bounded_class_agnostic_probes() {
        let app = selected_app();
        let seeds = subject_seed_points(&app);
        assert_eq!(seeds.len(), 5);
        assert_eq!(seeds[0], [16.0, 12.0]);
        assert!(seeds.iter().any(|point| point[0] < 12.0));
        assert!(seeds.iter().any(|point| point[0] > 20.0));
    }

    #[test]
    fn async_subject_remove_background_is_one_undo_group() {
        let observed = Rc::new(Cell::new(None));
        let observed_for_service = observed.clone();
        let service = move |request: crate::semantic_select_ui::Request| {
            observed_for_service.set(Some(request.mode));
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut alpha = vec![0; request.width as usize * request.height as usize];
            for y in 5..19 {
                for x in 8..24 {
                    alpha[y * request.width as usize + x] = 255;
                }
            }
            let _ = sender.send(Ok(SegmentationMask {
                width: request.width,
                height: request.height,
                alpha,
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
        let mask = &layer.mask.as_ref().unwrap().surface;
        let soft = (0..24).flat_map(|y| (0..32).map(move |x| mask.sample_channel(x, y, 0))).filter(|coverage| *coverage > 0.0 && *coverage < 1.0).count();
        assert!(soft > 0, "automatic background removal must refine the model selection at document resolution");
        assert!(state.doc.selection.is_none());
        assert_eq!(state.history.past_len(), history + 1);
        assert_eq!(observed.get(), Some(SegmentationMode::AutomaticSubject));
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
        assert!(app.session.active().unwrap().doc.selection.is_none());

        assert!(crate::semantic_select_ui::cancel(&mut app));

        let state = app.session.active().unwrap();
        assert_eq!(state.history.past_len(), history);
        assert!(state.doc.selection.is_none());
        assert!(state.doc.layers[0].mask.is_none());
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }
}
