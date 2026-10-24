//! Asynchronous, revision-bound cloud image edits.
//!
//! The UI owns neither credentials nor HTTP. A platform shell receives bounded
//! PNG inputs through [`Service`] and returns one PNG. The reply is applied only
//! when the exact source document revision is still active.

use std::sync::mpsc::{Receiver, TryRecvError};

use photocraft_color::PixelFormat;
use photocraft_doc::LayerId;
use photocraft_engine::ai_action_plan::{ActionPlan, CloudEditKind};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde::{Deserialize, Serialize};

use crate::PhotocraftApp;

const MAX_EDGE: u32 = 3840;
const MAX_PIXELS: u64 = 8_294_400;
// The current provider may return a square canvas even when an aspect-aware
// size is requested. Background replacement therefore uses a registered
// square transport canvas and crops it back to the untouched document bounds.
// Keeping the transport edge at or below 2048 also bounds the uncompressed
// RGBA request below the gateway's 20 MiB image cap.
const MAX_PROVIDER_CANVAS_EDGE: u32 = 2048;
// A small local neighbourhood is enough for antialiased / refined selection
// fringes. The matting implementation automatically widens its search when a
// fully owned colour is not present in the first neighbourhood.
const EDGE_MATTE_RADIUS: f32 = 4.0;

/// How a background replacement is returned to the editable document.
///
/// The layered mode keeps the foreground local and authoritative. The full
/// image mode intentionally lets the provider regenerate the complete frame,
/// but still registers the transport and fits the reply back to the exact
/// source document bounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundOutputMode {
    #[default]
    SeparateLayers,
    FullImage,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub plan: ActionPlan,
    pub kind: CloudEditKind,
    pub prompt: String,
    pub image_png: Vec<u8>,
    pub mask_png: Option<Vec<u8>>,
    pub background_output: Option<BackgroundOutputMode>,
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
    prepared: Prepared,
    coalesce: String,
    rollback_selection: bool,
    receiver: Receiver<Result<Response, String>>,
}

#[derive(Clone)]
struct Prepared {
    image_png: Vec<u8>,
    mask_png: Option<Vec<u8>>,
    foreground: Option<photocraft_engine::edit_cmds::Clip>,
    subject_mask: Option<Surface>,
    provider_canvas: Option<photocraft_geom::Size>,
    source_layers: Vec<LayerId>,
    background_output: Option<BackgroundOutputMode>,
}

fn encode_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, String> {
    let image = photocraft_codecs::Image::from_u8(width, height, photocraft_codecs::ChannelLayout::Rgba, rgba).map_err(|error| error.to_string())?;
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|error| error.to_string())
}

fn mask_region(mask: &Surface, bounds: Rect) -> photocraft_algo::selection::Region {
    let mut coverage = Vec::with_capacity(bounds.width() as usize * bounds.height() as usize);
    for y in bounds.y0..bounds.y1 {
        for x in bounds.x0..bounds.x1 {
            coverage.push((mask.sample_channel(x, y, 0).clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    photocraft_algo::selection::Region { bbox: bounds, mask: coverage }
}

fn preserved_foreground(source: &Surface, mask: &Surface, bounds: Rect) -> Surface {
    let region = mask_region(mask, bounds);
    // A flattened photograph already contains the old background colour in
    // antialiased boundary pixels. Replace only that soft fringe from nearby
    // fully selected foreground before applying the authoritative alpha.
    let decontaminated = photocraft_algo::matting::decontaminate(source, &region, EDGE_MATTE_RADIUS, 100.0);
    photocraft_algo::matting::masked_copy(&decontaminated, &region)
}

fn background_provider_canvas(width: u32, height: u32, rgba: &[[u8; 4]]) -> Result<(photocraft_geom::Size, Vec<u8>), String> {
    let source_bounds = Rect::new(0, 0, width as i32, height as i32);
    let source = Surface::from_interleaved(PixelFormat::RGBA8, source_bounds, rgba.as_flattened());
    let source_edge = width.max(height);
    let edge = source_edge.min(MAX_PROVIDER_CANVAS_EDGE);
    let scale = f64::from(edge) / f64::from(source_edge.max(1));
    let canvas_width = (f64::from(width) * scale).round().clamp(1.0, f64::from(edge)) as u32;
    let canvas_height = (f64::from(height) * scale).round().clamp(1.0, f64::from(edge)) as u32;
    let scaled = if edge == source_edge {
        source
    } else {
        photocraft_algo::resample::resize_surface_in_canvas(&source, scale, scale, photocraft_algo::resample::Resample::Bicubic, source_bounds)
    };
    let scaled_bounds = Rect::new(0, 0, canvas_width as i32, canvas_height as i32);
    let mut content = vec![[0u8; 4]; canvas_width as usize * canvas_height as usize];
    scaled.read_rgba8_into(scaled_bounds, &mut content);

    let offset_x = (edge - canvas_width) / 2;
    let offset_y = (edge - canvas_height) / 2;
    let mut canvas = vec![[0u8; 4]; edge as usize * edge as usize];
    for y in 0..edge {
        let source_y = y.saturating_sub(offset_y).min(canvas_height - 1);
        for x in 0..edge {
            let source_x = x.saturating_sub(offset_x).min(canvas_width - 1);
            let mut pixel = content[source_y as usize * canvas_width as usize + source_x as usize];
            if x < offset_x || x >= offset_x + canvas_width || y < offset_y || y >= offset_y + canvas_height {
                // Padding is transport-only. Keep it opaque so it cannot be
                // confused with the transparent subject silhouette.
                pixel[3] = 255;
            }
            canvas[y as usize * edge as usize + x as usize] = pixel;
        }
    }
    Ok((photocraft_geom::Size::new(edge, edge), canvas.into_iter().flatten().collect()))
}

fn inputs(app: &PhotocraftApp, kind: CloudEditKind, background_output: Option<BackgroundOutputMode>) -> Result<Prepared, String> {
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
    let mut foreground = None;
    let mut subject_mask = None;
    let mut provider_canvas = None;
    let source_layers = state.doc.layers.iter().filter(|layer| layer.visible).map(|layer| layer.id).collect();

    if kind == CloudEditKind::ReplaceBackground && background_output == Some(BackgroundOutputMode::SeparateLayers) {
        let selection = state.doc.selection.as_ref().ok_or_else(|| "Select the main object, then replace the background again.".to_string())?;
        for (index, background_pixel) in rgba.iter_mut().enumerate() {
            let x = bounds.x0 + i32::try_from(index % width as usize).map_err(|_| "Document is too large".to_string())?;
            let y = bounds.y0 + i32::try_from(index / width as usize).map_err(|_| "Document is too large".to_string())?;
            let selected = selection.sample_channel(x, y, 0).clamp(0.0, 1.0);
            background_pixel[3] = (f32::from(background_pixel[3]) * (1.0 - selected)).round() as u8;
            // Transparent RGB still survives PNG encoding. Clear it as well so
            // foreground pixels are not disclosed to the cloud provider.
            if selected > 0.0 {
                background_pixel[..3].fill(0);
            }
        }
        foreground = Some(photocraft_engine::edit_cmds::Clip { surface: preserved_foreground(&surface, selection, bounds), bounds });
        subject_mask = Some(selection.clone());
    }
    let image_png = if kind == CloudEditKind::ReplaceBackground {
        let (size, bytes) = background_provider_canvas(width, height, &rgba)?;
        provider_canvas = Some(size);
        encode_png(size.width, size.height, bytes)?
    } else {
        encode_png(width, height, rgba.into_iter().flatten().collect())?
    };

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
    Ok(Prepared { image_png, mask_png, foreground, subject_mask, provider_canvas, source_layers, background_output })
}

pub fn start(
    app: &mut PhotocraftApp,
    plan: ActionPlan,
    kind: CloudEditKind,
    prompt: String,
    background_output: Option<BackgroundOutputMode>,
    coalesce: Option<String>,
    rollback_selection: bool,
) -> Result<(), String> {
    if app.cloud_ai_edit_pending.is_some() {
        return Err("A cloud edit is already running".into());
    }
    app.session.validate_ai_action(&plan).map_err(|error| error.to_string())?;
    if (kind == CloudEditKind::ReplaceBackground) != background_output.is_some() {
        return Err("Background output mode is required only for background replacement".into());
    }
    let prepared = inputs(app, kind, background_output)?;
    let service = app.services.cloud_ai_edit.as_mut().ok_or_else(|| "Cloud AI editing is unavailable".to_string())?;
    let receiver =
        service(Request { plan: plan.clone(), kind, prompt, image_png: prepared.image_png.clone(), mask_png: prepared.mask_png.clone(), background_output })?;
    let coalesce = coalesce.unwrap_or_else(|| format!("cloud-ai-edit-{}", app.ui.alloc_id()));
    app.cloud_ai_edit_pending = Some(Pending { plan, previous_clipboard: app.session.clipboard.clone(), prepared, coalesce, rollback_selection, receiver });
    app.ui.status = "Cloud AI edit is running; image pixels are being processed by the configured provider…".into();
    app.ui.status_error = false;
    Ok(())
}

pub fn is_pending(app: &PhotocraftApp) -> bool {
    app.cloud_ai_edit_pending.is_some()
}

pub fn cancel(app: &mut PhotocraftApp) -> bool {
    let Some(pending) = app.cloud_ai_edit_pending.take() else { return false };
    if pending.rollback_selection && app.session.validate_ai_action(&pending.plan).is_ok() {
        let _ = app.session.execute("edit.undo", serde_json::json!({}));
    }
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
    let result = reply.and_then(|response| apply(app, &pending.plan, &pending.prepared, &pending.coalesce, response));
    if result.is_err() && pending.rollback_selection && app.session.validate_ai_action(&pending.plan).is_ok() {
        let _ = app.session.execute("edit.undo", serde_json::json!({}));
    }
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

fn cover_size(source: photocraft_geom::Size, target: photocraft_geom::Size) -> Result<photocraft_geom::Size, String> {
    if source.width == 0 || source.height == 0 || target.width == 0 || target.height == 0 {
        return Err("Cloud edit returned an empty image".into());
    }
    let sw = u64::from(source.width);
    let sh = u64::from(source.height);
    let tw = u64::from(target.width);
    let th = u64::from(target.height);
    let (width, height) = if sw.saturating_mul(th) >= tw.saturating_mul(sh) {
        ((sw.saturating_mul(th).saturating_add(sh - 1)) / sh, th)
    } else {
        (tw, (sh.saturating_mul(tw).saturating_add(sw - 1)) / sw)
    };
    Ok(photocraft_geom::Size::new(
        u32::try_from(width).map_err(|_| "Cloud edit result is too large".to_string())?,
        u32::try_from(height).map_err(|_| "Cloud edit result is too large".to_string())?,
    ))
}

fn fitted_surface(image_png: &[u8], target_size: photocraft_geom::Size, expected_provider_canvas: Option<photocraft_geom::Size>) -> Result<Surface, String> {
    let imported = photocraft_io::import("pixelle-cloud-edit.png", image_png).map_err(|error| format!("Cloud edit returned an invalid image: {error}"))?;
    let source_size = imported.document.size;
    if let Some(expected) = expected_provider_canvas
        && u64::from(source_size.width) * u64::from(expected.height) != u64::from(expected.width) * u64::from(source_size.height)
    {
        return Err(format!(
            "Cloud background returned {}×{} instead of the registered {}×{} aspect; the original document was not changed",
            source_size.width, source_size.height, expected.width, expected.height
        ));
    }
    let fitted = cover_size(source_size, target_size)?;
    let mut result_session = photocraft_engine::Session::new();
    result_session.add_document(imported.document, None);
    if fitted != source_size {
        result_session
            .execute("image.imageSize", serde_json::json!({"width": fitted.width, "height": fitted.height, "resample": "bicubic"}))
            .map_err(|error| format!("Cloud edit result could not be fitted to the document: {error}"))?;
    }
    if fitted != target_size {
        result_session
            .execute(
                "image.canvasSize",
                serde_json::json!({"width": target_size.width, "height": target_size.height, "anchor": "center", "extensionColor": "transparent"}),
            )
            .map_err(|error| format!("Cloud edit result could not be cropped to the document: {error}"))?;
    }
    let result = result_session.active().ok_or_else(|| "Cloud edit result could not be opened".to_string())?;
    Ok(photocraft_compose::flatten_to_surface(&result.doc, PixelFormat::RGBA8, None))
}

fn key_subject_hole(mut surface: Surface, mask: &Surface, bounds: Rect) -> Surface {
    // The provider is asked to paint the subject silhouette chroma green. For
    // a soft local selection, however, simply making the background alpha
    // `1 - selection` creates an alpha hole when the foreground is composed
    // over it. Build a local edge matte whose fully owned samples are the real
    // generated background and whose soft samples are precisely the subject
    // transition. Decontamination then reconstructs transition RGB without
    // using provider chroma or returned subject pixels.
    let mut edge_matte = mask_region(mask, bounds);
    for value in &mut edge_matte.mask {
        *value = match *value {
            0 => 255,
            255 => 0,
            _ => 128,
        };
    }
    surface = photocraft_algo::matting::decontaminate(&surface, &edge_matte, EDGE_MATTE_RADIUS, 100.0);
    let mut pixels = vec![[0u8; 4]; bounds.width() as usize * bounds.height() as usize];
    surface.read_rgba8_into(bounds, &mut pixels);
    for (index, pixel) in pixels.iter_mut().enumerate() {
        let x = bounds.x0 + (index % bounds.width() as usize) as i32;
        let y = bounds.y0 + (index / bounds.width() as usize) as i32;
        let selected = (mask.sample_channel(x, y, 0).clamp(0.0, 1.0) * 255.0).round() as u8;
        if selected == 255 {
            pixel[3] = 0;
        } else if selected > 0 {
            // Opaque underlap beneath a partially transparent foreground keeps
            // source-over alpha at one across the entire soft transition.
            pixel[3] = 255;
        }
    }
    let bytes: Vec<u8> = pixels.into_iter().flatten().collect();
    surface = Surface::from_interleaved(PixelFormat::RGBA8, bounds, &bytes);
    surface.prune();
    surface
}

fn pasted_layer(value: serde_json::Value) -> Result<LayerId, String> {
    value.get("layer").and_then(serde_json::Value::as_u64).map(LayerId).ok_or_else(|| "Cloud edit could not create an editable layer".to_string())
}

fn paste_named(app: &mut PhotocraftApp, clip: photocraft_engine::edit_cmds::Clip, name: &str, coalesce: &str) -> Result<LayerId, String> {
    app.session.clipboard = Some(clip);
    let layer =
        pasted_layer(app.session.execute("edit.pasteSpecial.pasteInPlace", serde_json::json!({"coalesce": coalesce})).map_err(|error| error.to_string())?)?;
    app.session.execute("layer.renameLayer", serde_json::json!({"layer": layer.0, "name": name, "coalesce": coalesce})).map_err(|error| error.to_string())?;
    Ok(layer)
}

fn apply(app: &mut PhotocraftApp, plan: &ActionPlan, prepared: &Prepared, coalesce: &str, response: Response) -> Result<String, String> {
    app.session.validate_ai_action(plan).map_err(|_| "Cloud edit result was discarded because the document changed".to_string())?;
    let state = app.session.active().ok_or_else(|| "No document is open".to_string())?;
    let target_size = state.doc.size;
    let bounds = state.doc.bounds();
    let mut surface = fitted_surface(&response.image_png, target_size, prepared.provider_canvas)?;
    if let (Some(mask), Some(foreground)) = (&prepared.subject_mask, &prepared.foreground) {
        surface = key_subject_hole(surface, mask, bounds);
        let result = (|| {
            let anchor = prepared.source_layers.last().copied().ok_or_else(|| "The source document has no visible layer".to_string())?;
            app.session.select_layer(anchor).map_err(|error| error.to_string())?;
            let background = photocraft_engine::edit_cmds::Clip { surface, bounds };
            let _background_layer = paste_named(app, background, "AI Background", coalesce)?;
            let _foreground_layer = paste_named(app, foreground.clone(), "Preserved Foreground", coalesce)?;
            for layer in &prepared.source_layers {
                app.session.execute("layer.hideLayers", serde_json::json!({"layer": layer.0, "coalesce": coalesce})).map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            let _ = app.session.execute("edit.undo", serde_json::json!({}));
            return Err(format!("Cloud background could not be assembled: {error}"));
        }
        return Ok(format!("Background replaced on a separate layer; foreground pixels were preserved ({})", response.model));
    }
    if prepared.background_output == Some(BackgroundOutputMode::FullImage) {
        let result = (|| {
            let anchor = prepared.source_layers.last().copied().ok_or_else(|| "The source document has no visible layer".to_string())?;
            app.session.select_layer(anchor).map_err(|error| error.to_string())?;
            let replacement = photocraft_engine::edit_cmds::Clip { surface, bounds };
            let layer = paste_named(app, replacement, "AI Full Image", coalesce)?;
            for source in &prepared.source_layers {
                if *source != layer {
                    app.session.execute("layer.hideLayers", serde_json::json!({"layer": source.0, "coalesce": coalesce})).map_err(|error| error.to_string())?;
                }
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            let _ = app.session.execute("edit.undo", serde_json::json!({}));
            return Err(format!("Cloud full-image background could not be assembled: {error}"));
        }
        return Ok(format!("Background replaced in one full-image layer at the original dimensions ({})", response.model));
    }
    app.session.clipboard = Some(photocraft_engine::edit_cmds::Clip { surface, bounds });
    app.session.execute("edit.pasteSpecial.pasteInPlace", serde_json::json!({"coalesce": coalesce})).map_err(|error| error.to_string())?;
    Ok(format!("Cloud edit added as a new layer ({})", response.model))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::Session;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn rgba_from_png(bytes: &[u8]) -> (Rect, Vec<[u8; 4]>) {
        let imported = photocraft_io::import("fixture.png", bytes).unwrap();
        let bounds = imported.document.bounds();
        let surface = photocraft_compose::flatten_to_surface(&imported.document, PixelFormat::RGBA8, None);
        let mut pixels = vec![[0; 4]; bounds.width() as usize * bounds.height() as usize];
        surface.read_rgba8_into(bounds, &mut pixels);
        (bounds, pixels)
    }

    #[test]
    fn replacement_request_contains_background_but_no_foreground_pixels() {
        let observed = Rc::new(RefCell::new(None));
        let observed_for_service = observed.clone();
        let service = move |request: Request| {
            *observed_for_service.borrow_mut() = Some(request);
            let (_sender, receiver) = std::sync::mpsc::channel();
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 4, "height": 2})).unwrap();
        app.session
            .edit("fixture pixels", |document, _| {
                let bounds = document.bounds();
                document.layers[0].surface_mut().unwrap().fill_rect(bounds, &[0.8, 0.2, 0.1, 1.0]);
                Ok(())
            })
            .unwrap();
        app.session.execute("select.rect", serde_json::json!({"x": 1, "y": 0, "width": 2, "height": 2})).unwrap();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();

        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into(), Some(BackgroundOutputMode::SeparateLayers), None, false)
            .unwrap();

        let request = observed.borrow();
        let request = request.as_ref().unwrap();
        let (provider_bounds, pixels) = rgba_from_png(&request.image_png);
        assert_eq!(provider_bounds, Rect::new(0, 0, 4, 4));
        let at = |x: usize, y: usize| pixels[y * 4 + x];
        assert_eq!(at(0, 1), [204, 51, 26, 255]);
        assert_eq!(at(1, 1), [0, 0, 0, 0]);
        assert_eq!(at(2, 1), [0, 0, 0, 0]);
        assert_eq!(at(3, 1), [204, 51, 26, 255]);
        assert_eq!(at(1, 0)[3], 255, "transport padding must not become a second transparent subject hole");
        assert!(request.mask_png.is_none());
        assert_eq!(request.background_output, Some(BackgroundOutputMode::SeparateLayers));
    }

    #[test]
    fn full_image_replacement_sends_the_complete_frame_without_requiring_a_selection() {
        let observed = Rc::new(RefCell::new(None));
        let observed_for_service = observed.clone();
        let service = move |request: Request| {
            *observed_for_service.borrow_mut() = Some(request);
            let (_sender, receiver) = std::sync::mpsc::channel();
            Ok(receiver)
        };
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 4, "height": 2})).unwrap();
        app.session
            .edit("fixture pixels", |document, _| {
                let bounds = document.bounds();
                document.layers[0].surface_mut().unwrap().fill_rect(bounds, &[0.8, 0.2, 0.1, 1.0]);
                Ok(())
            })
            .unwrap();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();

        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into(), Some(BackgroundOutputMode::FullImage), None, false).unwrap();

        let request = observed.borrow();
        let request = request.as_ref().unwrap();
        let (provider_bounds, pixels) = rgba_from_png(&request.image_png);
        assert_eq!(provider_bounds, Rect::new(0, 0, 4, 4));
        assert!(pixels.iter().all(|pixel| pixel[3] == 255));
        assert!(pixels.contains(&[204, 51, 26, 255]));
        assert!(request.mask_png.is_none());
        assert_eq!(request.background_output, Some(BackgroundOutputMode::FullImage));
    }

    #[test]
    fn cover_fit_preserves_aspect_before_center_crop() {
        assert_eq!(cover_size(photocraft_geom::Size::new(1024, 1024), photocraft_geom::Size::new(1884, 1028)).unwrap(), photocraft_geom::Size::new(1884, 1884));
        assert_eq!(cover_size(photocraft_geom::Size::new(1536, 1024), photocraft_geom::Size::new(1884, 1028)).unwrap(), photocraft_geom::Size::new(1884, 1256));
    }

    #[test]
    fn provider_canvas_downscales_large_sources_without_changing_their_aspect() {
        let rgba = vec![[12, 34, 56, 255]; 3000 * 1000];
        let (size, bytes) = background_provider_canvas(3000, 1000, &rgba).unwrap();
        assert_eq!(size, photocraft_geom::Size::new(2048, 2048));
        assert_eq!(bytes.len(), 2048 * 2048 * 4);
    }

    #[test]
    fn chroma_green_is_removed_only_inside_the_local_subject_mask() {
        let bounds = Rect::new(0, 0, 2, 1);
        let green = Surface::from_interleaved(PixelFormat::RGBA8, bounds, &[0, 255, 0, 255, 0, 255, 0, 255]);
        let mask = Surface::from_interleaved(photocraft_color::PixelFormat::GRAY8, bounds, &[255, 0]);
        let keyed = key_subject_hole(green, &mask, bounds);
        let mut pixels = [[0; 4]; 2];
        keyed.read_rgba8_into(bounds, &mut pixels);
        assert_eq!(pixels[0][3], 0);
        assert_eq!(pixels[1], [0, 255, 0, 255], "real green background outside the subject must survive");
    }

    #[test]
    fn soft_subject_edge_gets_opaque_reconstructed_background_underlap() {
        let bounds = Rect::new(0, 0, 3, 1);
        let provider = Surface::from_interleaved(
            PixelFormat::RGBA8,
            bounds,
            &[
                20, 40, 200, 255, // real generated background
                0, 255, 0, 255, // provider chroma in the soft transition
                0, 255, 0, 255, // provider chroma in the subject core
            ],
        );
        let mask = Surface::from_interleaved(PixelFormat::GRAY8, bounds, &[0, 128, 255]);

        let keyed = key_subject_hole(provider, &mask, bounds);
        let mut pixels = [[0; 4]; 3];
        keyed.read_rgba8_into(bounds, &mut pixels);

        assert_eq!(pixels[0], [20, 40, 200, 255]);
        assert_eq!(pixels[1], [20, 40, 200, 255], "the transition must use reconstructed background, not provider chroma");
        assert_eq!(pixels[2][3], 0, "the fully selected subject core remains clear on the background layer");
        let foreground_alpha = 128.0 / 255.0;
        let composite_alpha = foreground_alpha + (f32::from(pixels[1][3]) / 255.0) * (1.0 - foreground_alpha);
        assert!((composite_alpha - 1.0).abs() < f32::EPSILON, "the two-layer soft edge must not contain an alpha seam");
    }

    #[test]
    fn preserved_foreground_decontaminates_only_its_soft_fringe() {
        let bounds = Rect::new(0, 0, 3, 1);
        let source = Surface::from_interleaved(
            PixelFormat::RGBA8,
            bounds,
            &[
                0, 0, 0, 255, // old background
                64, 0, 0, 255, // old-background-contaminated antialias pixel
                240, 20, 10, 255, // fully selected foreground
            ],
        );
        let mask = Surface::from_interleaved(PixelFormat::GRAY8, bounds, &[0, 128, 255]);

        let foreground = preserved_foreground(&source, &mask, bounds);
        let mut pixels = [[0; 4]; 3];
        foreground.read_rgba8_into(bounds, &mut pixels);

        assert_eq!(pixels[0][3], 0);
        assert_eq!(pixels[1][3], 128, "the authoritative soft alpha must remain exact");
        assert!(pixels[1][0] > 64, "the old-background fringe colour must move toward the owned foreground colour");
        assert_eq!(pixels[2], [240, 20, 10, 255], "fully selected foreground pixels must remain byte-exact");
    }

    #[test]
    fn stale_cloud_reply_never_mutates_a_changed_document() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let service = move |_request: Request| receiver.take().ok_or_else(|| "fixture was already used".into());
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        app.session
            .edit("fixture pixels", |document, _| {
                let bounds = document.bounds();
                document.layers[0].surface_mut().unwrap().fill_rect(bounds, &[0.2, 0.4, 0.6, 1.0]);
                Ok(())
            })
            .unwrap();
        app.session.execute("select.rect", serde_json::json!({"x": 4, "y": 3, "width": 8, "height": 6})).unwrap();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();
        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into(), Some(BackgroundOutputMode::SeparateLayers), None, false)
            .unwrap();
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
        let error = start(&mut app, plan, CloudEditKind::GenerativeFill, "дорисуй выделенную область".into(), None, None, false).unwrap_err();
        assert!(error.contains("Select the area"));
    }

    #[test]
    fn successful_reply_is_fitted_and_added_as_one_editable_history_step() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let service = move |_request: Request| receiver.take().ok_or_else(|| "fixture was already used".into());
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        app.session
            .edit("fixture pixels", |document, _| {
                let bounds = document.bounds();
                document.layers[0].surface_mut().unwrap().fill_rect(bounds, &[0.2, 0.4, 0.6, 1.0]);
                Ok(())
            })
            .unwrap();
        app.session.execute("select.rect", serde_json::json!({"x": 4, "y": 3, "width": 8, "height": 6})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();
        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into(), Some(BackgroundOutputMode::SeparateLayers), None, false)
            .unwrap();
        let image_png = encode_png(1, 1, vec![10, 20, 30, 255]).unwrap();
        sender.send(Ok(Response { image_png, model: "fixture".into() })).unwrap();
        poll(&mut app);
        let state = app.session.active().unwrap();
        assert_eq!(state.doc.size, photocraft_geom::Size::new(16, 12), "a provider reply must never resize the source document");
        assert_eq!(state.doc.layer_count(), 3);
        assert!(!state.doc.layers[0].visible);
        assert_eq!(state.doc.layers[1].name, "AI Background");
        assert_eq!(state.doc.layers[2].name, "Preserved Foreground");
        assert_eq!(state.doc.layers[1].surface().unwrap().content_bounds(), Rect::new(0, 0, 16, 12));
        assert_eq!(state.doc.layers[1].surface().unwrap().sample_channel(6, 5, 3), 0.0);
        assert_eq!(state.doc.layers[2].surface().unwrap().pixel(6, 5), vec![0.2, 0.4, 0.6, 1.0]);
        assert_eq!(state.history.past_len(), history + 1);
        assert!(!app.ui.status_error, "{}", app.ui.status);
        assert!(app.ui.status.contains("fixture"));

        app.session.execute("edit.undo", serde_json::json!({})).unwrap();
        let restored = app.session.active().unwrap();
        assert_eq!(restored.doc.layer_count(), 1);
        assert!(restored.doc.layers[0].visible);
        assert!(restored.doc.selection.is_some());
    }

    #[test]
    fn full_image_reply_is_one_layer_at_the_exact_original_dimensions() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let service = move |_request: Request| receiver.take().ok_or_else(|| "fixture was already used".into());
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        let history = app.session.active().unwrap().history.past_len();
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();
        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into(), Some(BackgroundOutputMode::FullImage), None, false).unwrap();
        let image_png = encode_png(1024, 1024, vec![10; 1024 * 1024 * 4]).unwrap();
        sender.send(Ok(Response { image_png, model: "fixture".into() })).unwrap();

        poll(&mut app);

        let state = app.session.active().unwrap();
        assert_eq!(state.doc.size, photocraft_geom::Size::new(16, 12));
        assert_eq!(state.doc.layer_count(), 2);
        assert!(!state.doc.layers[0].visible);
        assert_eq!(state.doc.layers[1].name, "AI Full Image");
        assert!(state.doc.layers[1].visible);
        assert_eq!(state.doc.layers[1].surface().unwrap().content_bounds(), Rect::new(0, 0, 16, 12));
        assert_eq!(state.history.past_len(), history + 1);
        assert!(app.ui.status.contains("original dimensions"));

        app.session.execute("edit.undo", serde_json::json!({})).unwrap();
        let restored = app.session.active().unwrap();
        assert_eq!(restored.doc.layer_count(), 1);
        assert!(restored.doc.layers[0].visible);
        assert_eq!(restored.doc.size, photocraft_geom::Size::new(16, 12));
    }

    #[test]
    fn mismatched_provider_aspect_fails_without_resizing_or_adding_layers() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut receiver = Some(receiver);
        let service = move |_request: Request| receiver.take().ok_or_else(|| "fixture was already used".into());
        let mut app = PhotocraftApp::new(Session::new(), crate::Services { cloud_ai_edit: Some(Box::new(service)), ..Default::default() });
        app.session.execute("file.new", serde_json::json!({"width": 16, "height": 12})).unwrap();
        app.session.execute("select.rect", serde_json::json!({"x": 4, "y": 3, "width": 8, "height": 6})).unwrap();
        let revision = app.session.active().unwrap().revision;
        let plan = app.session.plan_ai_action("замени фон на студийный").unwrap();
        start(&mut app, plan, CloudEditKind::ReplaceBackground, "замени фон на студийный".into(), Some(BackgroundOutputMode::SeparateLayers), None, false)
            .unwrap();
        let image_png = encode_png(3, 2, vec![10; 3 * 2 * 4]).unwrap();
        sender.send(Ok(Response { image_png, model: "fixture".into() })).unwrap();

        poll(&mut app);

        let state = app.session.active().unwrap();
        assert_eq!(state.doc.size, photocraft_geom::Size::new(16, 12));
        assert_eq!(state.doc.layer_count(), 1);
        assert_eq!(state.revision, revision);
        assert!(app.ui.status_error);
        assert!(app.ui.status.contains("registered"));
    }
}
