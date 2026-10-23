//! UI bridge from Object Selection clicks to the local segmentation service.
//!
//! The shell owns model loading and inference. This module renders a bounded
//! preview, binds the returned proposal to the exact engine revision and asks
//! the engine to apply it through an ordinary selection command.

use std::sync::mpsc::{Receiver, TryRecvError};

use photocraft_doc::DocId;
use photocraft_ml::{PointKind, PromptPoint, SegmentationInput, SegmentationMask, SegmentationMode};

use crate::PhotocraftApp;

/// Owned request passed across the shell boundary.
pub type Request = SegmentationInput;

/// Asynchronous service boundary. The browser implementation posts requests to
/// a dedicated Rust/WASM worker; native shells and tests may reply immediately.
pub type Service = Box<dyn FnMut(Request) -> Result<Receiver<Result<SegmentationMask, String>>, String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AfterApply {
    Nothing,
    RemoveBackground(String),
    BlurBackground(String),
    CopySelection(String),
    ReplaceCloudBackground { key: String, prompt: String },
}

impl AfterApply {
    fn coalesce(&self) -> Option<&str> {
        match self {
            Self::Nothing => None,
            Self::RemoveBackground(key) | Self::BlurBackground(key) | Self::CopySelection(key) | Self::ReplaceCloudBackground { key, .. } => Some(key),
        }
    }
}

pub(crate) struct Pending {
    document: DocId,
    revision: u64,
    mode: &'static str,
    after: AfterApply,
    receiver: Receiver<Result<SegmentationMask, String>>,
}

pub(crate) fn is_pending(app: &PhotocraftApp) -> bool {
    app.semantic_select_pending.is_some()
}

/// Cancel inference without ever committing a late worker reply. Automatic
/// subject discovery is proposal-only until the worker answers, so cancellation
/// restores the exact pre-action document without an undo mutation.
pub(crate) fn cancel(app: &mut PhotocraftApp) -> bool {
    let Some(_pending) = app.semantic_select_pending.take() else { return false };
    app.ui.status = "Object selection cancelled".into();
    app.ui.status_error = false;
    true
}

fn mode(mods: egui::Modifiers) -> &'static str {
    match (mods.shift, mods.alt) {
        (true, true) => "intersect",
        (true, false) => "add",
        (false, true) => "subtract",
        (false, false) => "replace",
    }
}

/// Segment the visible object at a document-space click and immediately turn
/// the exact-state proposal into the canonical selection.
pub fn select_at(app: &mut PhotocraftApp, point: [f64; 2], mods: egui::Modifiers) -> bool {
    select_at_then(app, point, mods, AfterApply::Nothing)
}

/// Start a state-bound inference request. The result is polled from the app's
/// frame loop, so model loading and inference never block the UI thread.
pub(crate) fn select_at_then(app: &mut PhotocraftApp, point: [f64; 2], mods: egui::Modifiers, after: AfterApply) -> bool {
    select_points_then(app, &[point], mods, after)
}

/// Start ordinary SAM inference from one or more joint prompt points.
pub(crate) fn select_points_then(app: &mut PhotocraftApp, points: &[[f64; 2]], mods: egui::Modifiers, after: AfterApply) -> bool {
    select_points_with_mode(app, points, mods, after, SegmentationMode::PromptPoints)
}

/// Start class-agnostic automatic subject discovery. Unlike joint point
/// refinement, the backend evaluates these probes independently.
pub(crate) fn select_automatic_subject_then(app: &mut PhotocraftApp, points: &[[f64; 2]], mods: egui::Modifiers, after: AfterApply) -> bool {
    select_points_with_mode(app, points, mods, after, SegmentationMode::AutomaticSubject)
}

fn select_points_with_mode(
    app: &mut PhotocraftApp,
    points: &[[f64; 2]],
    mods: egui::Modifiers,
    after: AfterApply,
    segmentation_mode: SegmentationMode,
) -> bool {
    if app.semantic_select_pending.is_some() {
        app.ui.status = "The local object model is already processing a selection".into();
        app.ui.status_error = false;
        return false;
    }
    let Some(state) = app.session.active() else { return false };
    let bounds = state.doc.bounds();
    if bounds.is_empty()
        || points.is_empty()
        || points.iter().any(|point| {
            point[0] < f64::from(bounds.x0) || point[1] < f64::from(bounds.y0) || point[0] >= f64::from(bounds.x1) || point[1] >= f64::from(bounds.y1)
        })
    {
        return false;
    }
    let preview = photocraft_compose::thumbnail(&state.doc, photocraft_ml::MAX_PREVIEW_SIDE);
    let mut rgb = Vec::with_capacity(preview.width as usize * preview.height as usize * 3);
    for pixel in preview.pixels.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    let points = points
        .iter()
        .map(|point| PromptPoint {
            x: ((point[0] - f64::from(bounds.x0)) / f64::from(bounds.width())).clamp(0.0, 1.0) as f32,
            y: ((point[1] - f64::from(bounds.y0)) / f64::from(bounds.height())).clamp(0.0, 1.0) as f32,
            kind: PointKind::Foreground,
        })
        .collect();
    let document = state.doc.id;
    let revision = state.revision;
    let request = Request { width: preview.width, height: preview.height, rgb, points, mode: segmentation_mode };
    let Some(service) = app.services.semantic_select.as_mut() else {
        app.ui.status = "Local object model is unavailable; drag a box to use classical Object Selection".into();
        app.ui.status_error = true;
        return false;
    };
    app.ui.status = "Selecting object with the local model…".into();
    app.ui.status_error = false;
    let receiver = match service(request) {
        Ok(receiver) => receiver,
        Err(error) => {
            app.ui.status = error;
            app.ui.status_error = true;
            return false;
        }
    };
    app.semantic_select_pending = Some(Pending { document, revision, mode: mode(mods), after, receiver });
    // Immediate test/native services complete without waiting for another frame.
    let _ = poll(app);
    true
}

/// Commit a completed worker proposal only when the exact request document
/// state is still active. A late result is discarded without mutation.
pub(crate) fn poll(app: &mut PhotocraftApp) -> bool {
    let Some(pending) = app.semantic_select_pending.take() else { return false };
    let proposal = match pending.receiver.try_recv() {
        Ok(Ok(mask)) => mask,
        Ok(Err(error)) => {
            app.ui.status = error;
            app.ui.status_error = true;
            return true;
        }
        Err(TryRecvError::Empty) => {
            app.semantic_select_pending = Some(pending);
            return false;
        }
        Err(TryRecvError::Disconnected) => {
            app.ui.status = "The local object worker stopped before returning a selection".into();
            app.ui.status_error = true;
            return true;
        }
    };
    if !app.session.active().is_some_and(|state| state.doc.id == pending.document && state.revision == pending.revision) {
        app.ui.status = "Object selection was discarded because the document changed".into();
        app.ui.status_error = true;
        return true;
    }
    let confidence = proposal.confidence;
    let token = match app.session.stage_selection_proposal(proposal, true) {
        Ok(token) => token,
        Err(error) => {
            app.ui.status = error.to_string();
            app.ui.status_error = true;
            return false;
        }
    };
    let mut params = serde_json::json!({"token": token, "mode": pending.mode});
    if let (Some(key), Some(object)) = (pending.after.coalesce(), params.as_object_mut()) {
        object.insert("coalesce".into(), key.into());
    }
    match app.run("select.semantic.apply", params) {
        Ok(_) => {
            app.ui.status =
                confidence.map_or_else(|| "Object selected locally".into(), |value| format!("Object selected locally ({:.0}% confidence)", value * 100.0));
            app.ui.status_error = false;
            crate::ai_edit_ui::continue_after_selection(app, pending.after);
            true
        }
        Err(error) => {
            crate::ai_edit_ui::rollback_failed_selection(app, &pending.after);
            app.ui.status = error;
            app.ui.status_error = true;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::Session;
    use photocraft_ml::ModelIdentity;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn app(calls: Rc<Cell<usize>>) -> PhotocraftApp {
        let service = move |request: Request| {
            request.borrowed().validate().map_err(|error| error.to_string())?;
            calls.set(calls.get() + 1);
            let mut alpha = vec![0; request.width as usize * request.height as usize];
            let x = (request.points[0].x * request.width as f32).floor() as usize;
            let y = (request.points[0].y * request.height as f32).floor() as usize;
            if let Some(value) = alpha.get_mut(y.min(request.height as usize - 1) * request.width as usize + x.min(request.width as usize - 1)) {
                *value = 255;
            }
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = sender.send(Ok(SegmentationMask {
                width: request.width,
                height: request.height,
                alpha,
                confidence: Some(0.9),
                model: ModelIdentity::mobile_sam_tiny(),
            }));
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { semantic_select: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 80, "height": 40})).unwrap();
        app
    }

    #[test]
    fn click_runs_the_service_and_commits_one_selection_step() {
        let calls = Rc::new(Cell::new(0));
        let mut app = app(calls.clone());
        let steps = app.session.active().unwrap().history.past_len();
        assert!(select_at(&mut app, [40.0, 20.0], egui::Modifiers::NONE));
        assert_eq!(calls.get(), 1);
        assert_eq!(app.session.active().unwrap().history.past_len(), steps + 1);
        assert!(app.session.active().unwrap().doc.selection.is_some());
        assert!(!app.ui.status_error, "{}", app.ui.status);
    }

    #[test]
    fn shift_click_adds_and_alt_click_subtracts() {
        let calls = Rc::new(Cell::new(0));
        let mut app = app(calls.clone());
        assert!(select_at(&mut app, [10.0, 10.0], egui::Modifiers::NONE));
        assert!(select_at(&mut app, [60.0, 30.0], egui::Modifiers { shift: true, ..Default::default() }));
        let selection = app.session.active().unwrap().doc.selection.as_ref().unwrap();
        assert!(selection.sample_channel(10, 10, 0) > 0.5);
        assert!(selection.sample_channel(60, 30, 0) > 0.5);
        assert!(select_at(&mut app, [10.0, 10.0], egui::Modifiers { alt: true, ..Default::default() }));
        let selection = app.session.active().unwrap().doc.selection.as_ref().unwrap();
        assert!(selection.sample_channel(10, 10, 0) < 0.5);
        assert!(selection.sample_channel(60, 30, 0) > 0.5);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn manual_multi_point_request_keeps_joint_prompt_mode() {
        let observed = Rc::new(Cell::new(None));
        let observed_for_service = observed.clone();
        let service = move |request: Request| {
            request.borrowed().validate().map_err(|error| error.to_string())?;
            observed_for_service.set(Some(request.mode));
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = sender.send(Ok(SegmentationMask {
                width: request.width,
                height: request.height,
                alpha: vec![255; request.width as usize * request.height as usize],
                confidence: Some(0.9),
                model: ModelIdentity::mobile_sam_tiny(),
            }));
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { semantic_select: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 80, "height": 40})).unwrap();

        assert!(select_points_then(&mut app, &[[10.0, 10.0], [60.0, 30.0]], egui::Modifiers::NONE, AfterApply::Nothing,));

        assert_eq!(observed.get(), Some(SegmentationMode::PromptPoints));
    }

    #[test]
    fn late_worker_reply_is_discarded_after_document_revision_changes() {
        let sender = Rc::new(RefCell::new(None));
        let sender_for_service = sender.clone();
        let service = move |_request: Request| {
            let (tx, receiver) = std::sync::mpsc::channel();
            *sender_for_service.borrow_mut() = Some(tx);
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { semantic_select: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 20, "height": 12})).unwrap();
        assert!(select_at(&mut app, [5.0, 5.0], egui::Modifiers::NONE));
        assert!(is_pending(&app));
        app.session.execute("select.rect", serde_json::json!({"x": 1, "y": 1, "width": 3, "height": 3})).unwrap();
        let mask = SegmentationMask { width: 20, height: 12, alpha: vec![255; 20 * 12], confidence: Some(0.9), model: ModelIdentity::mobile_sam_tiny() };
        sender.borrow_mut().take().unwrap().send(Ok(mask)).unwrap();

        assert!(poll(&mut app));

        let selection = app.session.active().unwrap().doc.selection.as_ref().unwrap();
        assert_eq!(selection.content_bounds(), photocraft_geom::Rect::new(1, 1, 4, 4));
        assert!(app.ui.status_error);
        assert!(!is_pending(&app));
    }

    #[test]
    fn cancelling_manual_inference_never_mutates_document_history() {
        let held_sender = Rc::new(RefCell::new(None));
        let sender_for_service = held_sender.clone();
        let service = move |_request: Request| {
            let (sender, receiver) = std::sync::mpsc::channel();
            *sender_for_service.borrow_mut() = Some(sender);
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { semantic_select: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 20, "height": 12})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        assert!(select_at(&mut app, [5.0, 5.0], egui::Modifiers::NONE));

        assert!(cancel(&mut app));

        assert_eq!(app.session.active().unwrap().history.past_len(), history);
        assert!(app.session.active().unwrap().doc.selection.is_none());
        assert!(!is_pending(&app));
    }
}
