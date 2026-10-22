//! Asynchronous, revision-bound cloud image edits.
//!
//! The UI owns neither credentials nor HTTP. A platform shell receives bounded
//! PNG inputs through [`Service`] and returns one PNG. The reply is applied only
//! when the exact source document revision is still active.

use std::sync::mpsc::{Receiver, TryRecvError};

use photocraft_color::PixelFormat;
use photocraft_engine::ai_action_plan::{ActionPlan, CloudEditKind};
use photocraft_geom::Rect;

use crate::PhotocraftApp;

const MAX_EDGE: u32 = 3840;
const MAX_PIXELS: u64 = 8_294_400;

#[derive(Clone, Debug)]
pub struct Request {
    pub plan: ActionPlan,
    pub kind: CloudEditKind,
    pub prompt: String,
    pub image_png: Vec<u8>,
    pub mask_png: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub image_png: Vec<u8>,
    pub model: String,
}

pub type Service = Box<dyn FnMut(Request) -> Result<Receiver<Result<Response, String>>, String>>;

pub(crate) struct Pending {
    plan: ActionPlan,
    previous_clipboard: Option<photocraft_engine::edit_cmds::Clip>,
    receiver: Receiver<Result<Response, String>>,
}

fn encode_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let image = photocraft_codecs::Image::from_u8(width, height, photocraft_codecs::ChannelLayout::Rgba, rgba).map_err(|error| error.to_string())?;
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|error| error.to_string())
}

fn inputs(app: &PhotocraftApp, kind: CloudEditKind) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    let state = app.session.active().ok_or_else(|| "No document is open".to_string())?;
    let bounds = state.doc.bounds();
    let width = bounds.width();
    let height = bounds.height();
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE || pixels > MAX_PIXELS {
        return Err(format!("Cloud edits support images up to {MAX_EDGE}px per edge and {MAX_PIXELS} pixels"));
    }

    let surface = photocraft_compose::flatten_to_surface(&state.doc, PixelFormat::RGBA8, None);
    let mut rgba = vec![[0u8; 4]; usize::try_from(pixels).map_err(|_| "Document is too large".to_string())?];
    surface.read_rgba8_into(bounds, &mut rgba);
    let image_png = encode_png(width, height, rgba.into_iter().flatten().collect())?;

    let mask_png = if kind == CloudEditKind::GenerativeFill {
        let selection = state.doc.selection.as_ref().ok_or_else(|| "Select the area to fill, then run the prompt again.".to_string())?;
        let mut mask = Vec::with_capacity(usize::try_from(pixels).map_err(|_| "Document is too large".to_string())? * 4);
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                let selected = selection.sample_channel(x, y, 0).clamp(0.0, 1.0);
                mask.extend_from_slice(&[0, 0, 0, ((1.0 - selected) * 255.0).round() as u8]);
            }
        }
        Some(encode_png(width, height, mask)?)
    } else {
        None
    };
    Ok((image_png, mask_png))
}

pub fn start(app: &mut PhotocraftApp, plan: ActionPlan, kind: CloudEditKind, prompt: String) -> Result<(), String> {
    if app.cloud_ai_edit_pending.is_some() {
        return Err("A cloud edit is already running".into());
    }
    app.session.validate_ai_action(&plan).map_err(|error| error.to_string())?;
    let (image_png, mask_png) = inputs(app, kind)?;
    let service = app.services.cloud_ai_edit.as_mut().ok_or_else(|| "Cloud AI editing is unavailable".to_string())?;
    let receiver = service(Request { plan: plan.clone(), kind, prompt, image_png, mask_png })?;
    app.cloud_ai_edit_pending = Some(Pending { plan, previous_clipboard: app.session.clipboard.clone(), receiver });
    app.ui.status = "Cloud AI edit is running; image pixels are being processed by the configured provider…".into();
    app.ui.status_error = false;
    Ok(())
}

pub fn is_pending(app: &PhotocraftApp) -> bool {
    app.cloud_ai_edit_pending.is_some()
}

pub fn cancel(app: &mut PhotocraftApp) -> bool {
    let Some(pending) = app.cloud_ai_edit_pending.take() else { return false };
    app.session.clipboard = pending.previous_clipboard;
    app.ui.status = "Cloud AI edit cancelled; the document was not changed".into();
    app.ui.status_error = false;
    true
}

pub fn poll(app: &mut PhotocraftApp) {
    let reply = match app.cloud_ai_edit_pending.as_ref().map(|pending| pending.receiver.try_recv()) {
        None | Some(Err(TryRecvError::Empty)) => return,
        Some(Err(TryRecvError::Disconnected)) => Err("Cloud edit service stopped before returning an image".into()),
        Some(Ok(result)) => result,
    };
    let Some(pending) = app.cloud_ai_edit_pending.take() else { return };
    let result = reply.and_then(|response| apply(app, &pending.plan, response));
    app.session.clipboard = pending.previous_clipboard;
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

fn apply(app: &mut PhotocraftApp, plan: &ActionPlan, response: Response) -> Result<String, String> {
    app.session.validate_ai_action(plan).map_err(|_| "Cloud edit result was discarded because the document changed".to_string())?;
    let imported =
        photocraft_io::import("pixelle-cloud-edit.png", &response.image_png).map_err(|error| format!("Cloud edit returned an invalid image: {error}"))?;
    let state = app.session.active().ok_or_else(|| "No document is open".to_string())?;
    let target_size = state.doc.size;
    let mut result_session = photocraft_engine::Session::new();
    result_session.add_document(imported.document, None);
    if result_session.active().is_some_and(|result| result.doc.size != target_size) {
        result_session
            .execute("image.imageSize", serde_json::json!({"width": target_size.width, "height": target_size.height, "resample": "bicubic"}))
            .map_err(|error| format!("Cloud edit result could not be fitted to the document: {error}"))?;
    }
    let result = result_session.active().ok_or_else(|| "Cloud edit result could not be opened".to_string())?;
    let bounds = result.doc.bounds();
    let surface = photocraft_compose::flatten_to_surface(&result.doc, PixelFormat::RGBA8, None);
    app.session.clipboard = Some(photocraft_engine::edit_cmds::Clip { surface, bounds: Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1) });
    app.run("edit.pasteSpecial.pasteInPlace", serde_json::json!({}))?;
    Ok(format!("Cloud edit added as a new layer ({})", response.model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::Session;

    #[test]
    fn stale_cloud_reply_never_mutates_a_changed_document() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let service = move |_request: Request| receiver.take().ok_or_else(|| "fixture was already used".into());
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();
        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into()).unwrap();
        app.session.execute("layer.new.layer", serde_json::json!({})).unwrap();
        let image_png = encode_png(16, 12, vec![0; 16 * 12 * 4]).unwrap();
        sender.send(Ok(Response { image_png, model: "fixture".into() })).unwrap();
        poll(&mut app);
        assert!(app.ui.status_error);
        assert!(app.ui.status.contains("discarded because the document changed"));
        assert_eq!(app.session.active().unwrap().doc.layer_count(), 2);
    }

    #[test]
    fn generative_fill_requires_an_explicit_selection_before_network_dispatch() {
        let mut app = PhotocraftApp::new(Session::new(), crate::Services::default());
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        let plan = app.session.plan_ai_action("дорисуй выделенную область").unwrap();
        let error = start(&mut app, plan, CloudEditKind::GenerativeFill, "дорисуй выделенную область".into()).unwrap_err();
        assert!(error.contains("Select the area"));
    }

    #[test]
    fn successful_reply_is_fitted_and_added_as_one_editable_history_step() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let service = move |_request: Request| receiver.take().ok_or_else(|| "fixture was already used".into());
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();
        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into()).unwrap();
        let image_png = encode_png(1, 1, vec![10, 20, 30, 255]).unwrap();
        sender.send(Ok(Response { image_png, model: "fixture".into() })).unwrap();
        poll(&mut app);
        let state = app.session.active().unwrap();
        assert_eq!(state.doc.layer_count(), 2);
        assert_eq!(state.history.past_len(), history + 1);
        assert!(!app.ui.status_error, "{}", app.ui.status);
        assert!(app.ui.status.contains("fixture"));
    }
}
