//! The browser shell: web `Services`, drag-and-drop, and the eframe web runner.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_engine::Session;
use photocraft_ui_egui::served_fonts;
use photocraft_ui_egui::theme::ThemeKind;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogRequest, PhotocraftApp, Services};
use sha2::{Digest as _, Sha256};
use wasm_bindgen::JsCast as _;

type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

type ClipboardImage = (u32, u32, Vec<u8>);

/// Browser paste events expose image files only while the event is being handled. The bytes are
/// read asynchronously, decoded on the next frame, and then exposed through the synchronous
/// `Services::clipboard_get_image` boundary used by the editor.
#[derive(Default)]
struct BrowserClipboard {
    request: u64,
    arrived: Vec<(u64, Result<(String, Vec<u8>), String>)>,
    image: Option<ClipboardImage>,
    paste_requested: bool,
}

type Clipboard = Arc<Mutex<BrowserClipboard>>;

/// Everything File › Open reads: PhotoCraft, Photoshop and Affinity documents, flat images, and
/// Photoshop brushes (.abr), gradients (.grd) and swatches (.aco, .ase), which go to the preset libraries.
const OPEN_EXTS: &[&str] = &[
    "pcraft", "psd", "psb", "psdt", "png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp", "tga", "ico", "qoi", "exr", "hdr", "pbm", "pgm", "ppm", "pam",
    "pfm", "heic", "heif", "hif", "dng", "cr2", "cr3", "nef", "nrw", "arw", "pef", "orf", "rw2", "raf", "abr", "grd", "svg", "svgz", "aco", "ase", "af",
    "afdesign", "afphoto", "afpub",
];
const PRESET_LIBRARY_EXTS: &[&str] = &["abr", "grd", "aco", "ase"];
const CANVAS_ID: &str = "photocraft_canvas";

/// Fonts the host serves next to the page (`photocraft_ui_egui::served_fonts`): craft-fonts' manifest
/// format, each file relative to the site root. No manifest (a 404) means no served fonts.
const FONTS_MANIFEST: &str = "fonts/manifest.txt";

/// The served fonts: the manifest once read, and what the network has delivered.
#[derive(Default)]
struct ServedFonts {
    /// Font files by family.
    files: HashMap<String, Vec<String>>,
    /// Families fetched or being fetched: each one once per page load.
    fetched: HashSet<String>,
    /// Downloaded files waiting for the next frame: (family, bytes).
    arrived: Vec<(String, Vec<u8>)>,
}

type Served = Arc<Mutex<ServedFonts>>;

pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let q = query();
        let force_cpu = crate::query_flag(&q, "cpu");
        let user_agent = web_sys::window().and_then(|window| window.navigator().user_agent().ok()).unwrap_or_default();
        let mut options = eframe::WebOptions::default();
        photocraft_ui_egui::gpu_canvas::use_adapter_limits(&mut options.wgpu_options.wgpu_setup);
        if crate::use_webgl(&q, &user_agent)
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        let pen_target = canvas.clone();
        let paste_target = document.clone();
        let pathname = document.location().and_then(|location| location.pathname().ok()).unwrap_or_default();
        let quick_route = photocraft_ui_egui::quick_tools_ui::route_from_path(&pathname);
        if let Some(route) = quick_route {
            document.set_title(route.browser_title());
        }
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::Pro);
                    let inbox: Inbox = Arc::default();
                    let clipboard: Clipboard = Arc::default();
                    let served: Served = Arc::default();
                    load_font_manifest(served.clone(), cc.egui_ctx.clone());
                    let semantic = preload_semantic_model(cc.egui_ctx.clone());
                    let cloud_ai = cloud_ai_service(cc.egui_ctx.clone());
                    let mut app = PhotocraftApp::new(Session::new(), services(inbox.clone(), clipboard.clone(), semantic, cloud_ai));
                    if let Some(route) = quick_route {
                        app.quick_tool = Some(photocraft_ui_egui::quick_tools_ui::State::new(route));
                    }
                    listen_pen(&pen_target, app.stylus.feed.clone());
                    listen_image_paste(&paste_target, clipboard.clone(), cc.egui_ctx.clone());
                    app.set_theme(&cc.egui_ctx, ThemeKind::Pro);
                    if let Some(rs) = cc.wgpu_render_state.clone()
                        && !force_cpu
                    {
                        log::info!("photocraft-web: wgpu backend {:?}", rs.adapter.get_info().backend);
                        app.set_wgpu(rs);
                    }
                    let unsaved = Arc::new(AtomicBool::new(false));
                    guard_unload(unsaved.clone());
                    Ok(Box::new(WebShell { app, inbox, clipboard, unsaved, served }))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id("photocraft_loading") {
            match result {
                Ok(()) => el.remove(),
                Err(e) => el.set_inner_html(&format!("<p>Pixelle failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required.</p>")),
            }
        }
    });
}

/// Capture an image placed on the browser clipboard by a screenshot tool or another app. Text
/// paste is left to egui and focused text fields. Image paste is prevented from becoming a text
/// event, then completed after the browser has yielded the file bytes.
fn listen_image_paste(target: &web_sys::Document, clipboard: Clipboard, ctx: egui::Context) {
    use wasm_bindgen::closure::Closure;
    let cb = Closure::<dyn FnMut(web_sys::ClipboardEvent)>::new(move |event: web_sys::ClipboardEvent| {
        let Some(data) = event.clipboard_data() else {
            return;
        };
        let from_files =
            data.files().and_then(|files| (0..files.length()).filter_map(|index| files.item(index)).find(|file| file.type_().starts_with("image/")));
        let items = data.items();
        let from_items = || {
            (0..items.length())
                .filter_map(|index| items.get(index))
                .find(|item| item.kind() == "file" && item.type_().starts_with("image/"))
                .and_then(|item| item.get_as_file().ok().flatten())
        };
        let Some(file) = from_files.or_else(from_items) else { return };
        event.prevent_default();
        let name = clipboard_file_name(&file);
        let request = {
            let mut state = clipboard.lock().unwrap_or_else(|error| error.into_inner());
            state.request = state.request.saturating_add(1);
            state.image = None;
            state.paste_requested = true;
            state.request
        };
        let clipboard = clipboard.clone();
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let bytes = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
                .await
                .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec())
                .map_err(|error| format!("Could not read the pasted image: {error:?}"));
            let bytes = match bytes {
                Ok(bytes) => authorize_opened_project(&name, &bytes).await.map(|()| bytes),
                Err(error) => Err(error),
            };
            clipboard.lock().unwrap_or_else(|error| error.into_inner()).arrived.push((request, bytes.map(|bytes| (name, bytes))));
            ctx.request_repaint();
        });
    });
    if target.add_event_listener_with_callback("paste", cb.as_ref().unchecked_ref()).is_ok() {
        cb.forget();
    }
}

fn clipboard_file_name(file: &web_sys::File) -> String {
    let name = file.name();
    if name.contains('.') {
        return name;
    }
    let ext = match file.type_().as_str() {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "png",
    };
    format!("clipboard.{ext}")
}

/// Pen pressure, tilt, twist and the eraser button from Pointer Events (eframe forwards none of them for pens) into
/// the app's stylus feed. The sample is kept through `pointerup` so the stroke's last points keep
/// their pressure; hovering, a mouse, or leaving the canvas clears it.
fn listen_pen(target: &web_sys::HtmlCanvasElement, feed: photocraft_ui_egui::stylus::StylusFeed) {
    use photocraft_ui_egui::stylus::PenSample;
    use wasm_bindgen::closure::Closure;
    for kind in ["pointerdown", "pointermove", "pointerup", "pointercancel", "pointerleave"] {
        let feed = feed.clone();
        let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
            let ty = e.type_();
            if ty == "pointerup" && e.pointer_type() == "pen" {
                return;
            }
            let pen = e.pointer_type() == "pen" && e.buttons() != 0 && ty != "pointercancel" && ty != "pointerleave";
            // W3C Pointer Events: `buttons` bit 5 (32) is the pen's eraser.
            let eraser = e.buttons() & 32 != 0;
            feed.set(pen.then(|| PenSample {
                pressure: e.pressure(),
                tilt_x: e.tilt_x() as f32,
                tilt_y: e.tilt_y() as f32,
                rotation: e.twist() as f32,
                eraser,
            }));
        });
        if target.add_event_listener_with_callback(kind, cb.as_ref().unchecked_ref()).is_ok() {
            cb.forget();
        }
    }
}

/// Closing or reloading the tab while a document has unsaved changes asks first, as closing the
/// desktop window does: the browser shows its own "Leave site?" prompt (#1380). `unsaved` is
/// refreshed every frame by [`WebShell`].
fn guard_unload(unsaved: Arc<AtomicBool>) {
    use wasm_bindgen::closure::Closure;
    let Some(window) = web_sys::window() else { return };
    let cb = Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(move |e: web_sys::BeforeUnloadEvent| {
        if unsaved.load(Ordering::Relaxed) {
            e.prevent_default();
            // Older browsers show the prompt only when a return value is set.
            e.set_return_value("");
        }
    });
    if window.add_event_listener_with_callback("beforeunload", cb.as_ref().unchecked_ref()).is_ok() {
        cb.forget();
    }
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

/// Reads the served font manifest, if the host has one, and lists its families in the font menus.
fn load_font_manifest(served: Served, ctx: egui::Context) {
    wasm_bindgen_futures::spawn_local(async move {
        // Most hosts serve no fonts: a missing manifest is the normal case, not an error.
        let Ok(bytes) = fetch_bytes(FONTS_MANIFEST).await else { return };
        let text = String::from_utf8_lossy(&bytes);
        // A host that answers unknown paths with its HTML page (single-page app fallback).
        if text.trim_start().starts_with('<') {
            return;
        }
        let (fonts, skipped) = served_fonts::parse_manifest(&text);
        for s in skipped {
            log::warn!("{FONTS_MANIFEST}: skipped {s}");
        }
        let mut files: HashMap<String, Vec<String>> = HashMap::new();
        for f in fonts {
            files.entry(f.family).or_default().push(f.file);
        }
        log::info!("photocraft-web: {} served font families", files.len());
        let families: Vec<String> = files.keys().cloned().collect();
        served.lock().unwrap_or_else(|e| e.into_inner()).files = files;
        served_fonts::add_families(families);
        ctx.request_repaint();
    });
}

/// GET `url` (relative to the page) and read the whole body.
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    use wasm_bindgen_futures::JsFuture;
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_str(url)).await.map_err(js)?.dyn_into().map_err(|_| "not a Response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = JsFuture::from(resp.array_buffer().map_err(js)?).await.map_err(js)?;
    Ok(js_sys::Uint8Array::new(&body).to_vec())
}

/// Start the dedicated inference worker when the editor starts. Trunk creates
/// this stable loader from wasm-bindgen output; no handwritten JavaScript or
/// model code runs in the document/UI realm.
fn preload_semantic_model(ctx: egui::Context) -> Option<photocraft_ui_egui::semantic_select_ui::Service> {
    use std::sync::mpsc;
    use wasm_bindgen::closure::Closure;

    let worker = match web_sys::Worker::new("./photocraft-ml-worker_loader.js") {
        Ok(worker) => worker,
        Err(error) => {
            log::error!("photocraft-web: could not start local object worker: {error:?}");
            return None;
        }
    };
    let pending = Arc::new(Mutex::new(HashMap::<u64, mpsc::Sender<Result<photocraft_ml::SegmentationMask, String>>>::new()));
    let replies = pending.clone();
    let wake = ctx.clone();
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        let parsed = serde_wasm_bindgen::from_value::<photocraft_ml::WorkerResponse>(event.data());
        match parsed {
            Ok(response) if response.version == photocraft_ml::WORKER_PROTOCOL_VERSION => {
                if let Some(sender) = replies.lock().unwrap_or_else(|error| error.into_inner()).remove(&response.id) {
                    let _ = sender.send(response.result);
                }
            }
            Ok(response) => log::error!("photocraft-web: worker protocol {} is unsupported", response.version),
            Err(error) => log::error!("photocraft-web: invalid worker response: {error}"),
        }
        wake.request_repaint();
    });
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let failures = pending.clone();
    let wake = ctx;
    let on_error = Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(move |event: web_sys::ErrorEvent| {
        let message = format!("The local object worker failed: {}", event.message());
        for (_, sender) in failures.lock().unwrap_or_else(|error| error.into_inner()).drain() {
            let _ = sender.send(Err(message.clone()));
        }
        wake.request_repaint();
    });
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    let next = AtomicU64::new(0);
    Some(Box::new(move |input| {
        input.borrowed().validate().map_err(|error| error.to_string())?;
        let id = next.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let request = photocraft_ml::WorkerRequest { version: photocraft_ml::WORKER_PROTOCOL_VERSION, id, input };
        let value = serde_wasm_bindgen::to_value(&request).map_err(|error| error.to_string())?;
        let (sender, receiver) = mpsc::channel();
        pending.lock().unwrap_or_else(|error| error.into_inner()).insert(id, sender);
        if let Err(error) = worker.post_message(&value) {
            pending.lock().unwrap_or_else(|poison| poison.into_inner()).remove(&id);
            return Err(format!("Could not send object selection to the worker: {error:?}"));
        }
        Ok(receiver)
    }))
}

/// Same-origin cloud edit boundary. The browser never receives provider
/// credentials: it sends the user-authorized PNG payload to the product
/// gateway, which enforces model, size, origin, rate and concurrency policy.
fn cloud_ai_service(ctx: egui::Context) -> Option<photocraft_ui_egui::cloud_ai_edit_ui::Service> {
    use std::sync::mpsc;

    Some(Box::new(move |request| {
        let (sender, receiver) = mpsc::channel();
        let wake = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = request_cloud_ai(request).await;
            let _ = sender.send(result);
            wake.request_repaint();
        });
        Ok(receiver)
    }))
}

async fn request_cloud_ai(request: photocraft_ui_egui::cloud_ai_edit_ui::Request) -> Result<photocraft_ui_egui::cloud_ai_edit_ui::Response, String> {
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    let authorization_id = authorize_cloud_ai(cloud_operation(request.kind)).await?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&request.image_png);
    let mask = request.mask_png.as_ref().map(|value| base64::engine::general_purpose::STANDARD.encode(value));
    let body = serde_json::to_string(&serde_json::json!({
        "operation": request.kind,
        "prompt": request.prompt,
        "image_base64": encoded,
        "mask_base64": mask,
        "background_output": request.background_output,
        "authorization_id": authorization_id,
    }))
    .map_err(|error| error.to_string())?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));
    let web_request =
        web_sys::Request::new_with_str_and_init("/api/pixelle/v1/edit", &init).map_err(|error| format!("Could not prepare cloud edit: {error:?}"))?;
    web_request.headers().set("content-type", "application/json").map_err(|error| format!("Could not prepare cloud edit headers: {error:?}"))?;
    let window = web_sys::window().ok_or_else(|| "No browser window is available".to_string())?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&web_request))
        .await
        .map_err(|error| format!("Cloud edit request failed: {error:?}"))?
        .dyn_into()
        .map_err(|_| "Cloud edit returned an invalid response".to_string())?;
    let text = JsFuture::from(response.text().map_err(|error| format!("Could not read cloud edit response: {error:?}"))?)
        .await
        .map_err(|error| format!("Could not read cloud edit response: {error:?}"))?
        .as_string()
        .ok_or_else(|| "Cloud edit response was not text".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| format!("Cloud edit returned HTTP {} with an invalid response", response.status()))?;
    if !response.ok() {
        let message = value.get("error").and_then(serde_json::Value::as_str).unwrap_or("Cloud edit failed");
        return Err(format!("{message} (HTTP {})", response.status()));
    }
    let image = value.get("image_base64").and_then(serde_json::Value::as_str).ok_or_else(|| "Cloud edit response did not contain an image".to_string())?;
    let image_png = base64::engine::general_purpose::STANDARD.decode(image).map_err(|_| "Cloud edit response contained invalid image data".to_string())?;
    let model = value.get("model").and_then(serde_json::Value::as_str).unwrap_or("configured model").to_string();
    Ok(photocraft_ui_egui::cloud_ai_edit_ui::Response { image_png, model })
}

fn cloud_operation(kind: photocraft_engine::ai_action_plan::CloudEditKind) -> &'static str {
    use photocraft_engine::ai_action_plan::CloudEditKind;
    match kind {
        CloudEditKind::ReplaceBackground => "replace-background",
        CloudEditKind::Relight => "relight",
        CloudEditKind::Restyle => "restyle",
        CloudEditKind::GenerativeFill => "generative-fill",
    }
}

async fn authorize_cloud_ai(operation: &str) -> Result<String, String> {
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    let body = serde_json::to_string(&serde_json::json!({ "operation": operation })).map_err(|error| error.to_string())?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));
    init.set_credentials(web_sys::RequestCredentials::Include);
    let request = web_sys::Request::new_with_str_and_init("/api/v1/pixelle/cloud/authorize", &init)
        .map_err(|error| format!("Could not prepare Pixelle access check: {error:?}"))?;
    request.headers().set("content-type", "application/json").map_err(|error| format!("Could not prepare Pixelle access headers: {error:?}"))?;
    let window = web_sys::window().ok_or_else(|| "No browser window is available".to_string())?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| format!("Pixelle access check failed: {error:?}"))?
        .dyn_into()
        .map_err(|_| "Pixelle access check returned an invalid response".to_string())?;
    let value = JsFuture::from(response.json().map_err(|error| format!("Could not read Pixelle access response: {error:?}"))?)
        .await
        .map_err(|error| format!("Could not read Pixelle access response: {error:?}"))?;
    let value: serde_json::Value = serde_wasm_bindgen::from_value(value).map_err(|error| error.to_string())?;
    if !response.ok() {
        if matches!(response.status(), 401 | 403) {
            open_commerce_paywall();
        }
        return Err(match response.status() {
            401 => "Sign in to use cloud AI".to_string(),
            403 => value.get("message").and_then(serde_json::Value::as_str).unwrap_or("Pixelle cloud allowance is unavailable").to_string(),
            _ => "Pixelle access service is temporarily unavailable".to_string(),
        });
    }
    value
        .get("authorizationId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Pixelle access response did not contain an authorization".to_string())
}

/// The commerce shell is ordinary same-origin browser chrome around the Rust
/// editor. Keeping this call typed and one-way prevents payment UI state from
/// becoming part of the canonical document/session state.
fn open_commerce_paywall() {
    let Some(window) = web_sys::window() else { return };
    let Ok(commerce) = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("PixelleCommerce")) else { return };
    let Ok(show) = js_sys::Reflect::get(&commerce, &wasm_bindgen::JsValue::from_str("showPaywall")) else { return };
    if let Ok(function) = show.dyn_into::<js_sys::Function>() {
        let _ = function.call0(&commerce);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ProjectAllowanceError {
    Denied(String),
    Unavailable(String),
}

fn project_allowance_error(status: u16) -> ProjectAllowanceError {
    match status {
        401 => ProjectAllowanceError::Denied("Sign in and activate Pixelle Pro to open an image project".to_string()),
        403 => ProjectAllowanceError::Denied("Pixelle project allowance is unavailable".to_string()),
        _ => ProjectAllowanceError::Unavailable(format!("Pixelle project allowance service returned HTTP {status}")),
    }
}

async fn meter_local_project(bytes: &[u8]) -> Result<(), ProjectAllowanceError> {
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;

    let project_key = format!("{:x}", Sha256::digest(bytes));
    let body =
        serde_json::to_string(&serde_json::json!({ "projectKey": project_key })).map_err(|error| ProjectAllowanceError::Unavailable(error.to_string()))?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));
    init.set_credentials(web_sys::RequestCredentials::Include);
    let request = web_sys::Request::new_with_str_and_init("/api/v1/pixelle/usage/project", &init)
        .map_err(|error| ProjectAllowanceError::Unavailable(format!("Could not prepare Pixelle project check: {error:?}")))?;
    request
        .headers()
        .set("content-type", "application/json")
        .map_err(|error| ProjectAllowanceError::Unavailable(format!("Could not prepare Pixelle project headers: {error:?}")))?;
    let window = web_sys::window().ok_or_else(|| ProjectAllowanceError::Unavailable("No browser window is available".to_string()))?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| ProjectAllowanceError::Unavailable(format!("Pixelle project check failed: {error:?}")))?
        .dyn_into()
        .map_err(|_| ProjectAllowanceError::Unavailable("Pixelle project check returned an invalid response".to_string()))?;
    if response.ok() {
        return Ok(());
    }
    let error = project_allowance_error(response.status());
    if matches!(error, ProjectAllowanceError::Denied(_)) {
        open_commerce_paywall();
    }
    Err(error)
}

fn is_preset_library(name: &str) -> bool {
    let extension = std::path::Path::new(name).extension().and_then(|value| value.to_str()).unwrap_or_default().to_ascii_lowercase();
    PRESET_LIBRARY_EXTS.contains(&extension.as_str())
}

/// Validate through the canonical importer before consuming a project slot.
/// Preset-library imports are editor resources, not image projects, and never
/// count against the period allowance.
async fn authorize_opened_project(name: &str, bytes: &[u8]) -> Result<(), String> {
    if is_preset_library(name) {
        return Ok(());
    }
    photocraft_io::import(name, bytes).map_err(|error| format!("Could not open {name}: {error}"))?;
    match meter_local_project(bytes).await {
        Ok(()) => Ok(()),
        Err(ProjectAllowanceError::Denied(message)) => Err(message),
        Err(ProjectAllowanceError::Unavailable(message)) => {
            // Local editing must remain available during a billing/control-plane outage. Cloud AI
            // remains fail-closed at its own authorization boundary before any pixels leave the browser.
            log::warn!("{message}; opening the local project without metering");
            Ok(())
        }
    }
}

/// Wraps the app to read dropped files asynchronously (browsers can't read them synchronously,
/// so the app's own drop path can't handle them) and feed them through the inbox.
struct WebShell {
    app: PhotocraftApp,
    inbox: Inbox,
    clipboard: Clipboard,
    /// Read by the `beforeunload` listener ([`guard_unload`]).
    unsaved: Arc<AtomicBool>,
    served: Served,
}

impl WebShell {
    /// Decode only the newest pasted image. Older asynchronous reads cannot replace a later
    /// clipboard gesture when their promises finish out of order.
    fn receive_clipboard_image(&mut self) {
        let (request, arrived) = {
            let mut state = self.clipboard.lock().unwrap_or_else(|error| error.into_inner());
            (state.request, std::mem::take(&mut state.arrived))
        };
        let Some((_, result)) = arrived.into_iter().rev().find(|(id, _)| *id == request) else { return };
        let decoded = result.and_then(|(name, bytes)| decode_clipboard_image(&name, &bytes));
        let mut state = self.clipboard.lock().unwrap_or_else(|error| error.into_inner());
        if state.request != request {
            return;
        }
        match decoded {
            Ok(image) => state.image = Some(image),
            Err(error) => {
                state.paste_requested = false;
                self.app.ui.status = error;
                self.app.ui.status_error = true;
            }
        }
    }

    /// If the key shortcut ran before the asynchronous image read completed, finish that same
    /// paste now. If it ran after decoding, `clipboard_get_image` already cleared the flag and
    /// this remains a no-op, preventing a duplicate layer/document.
    fn finish_clipboard_paste(&mut self, ctx: &egui::Context) {
        let ready = {
            let state = self.clipboard.lock().unwrap_or_else(|error| error.into_inner());
            state.paste_requested && state.image.is_some()
        };
        if ready {
            match photocraft_ui_egui::menus::invoke(&mut self.app, ctx, "edit.paste", serde_json::json!({})) {
                Ok(result) => {
                    if let (Some(width), Some(height)) =
                        (result.get("width").and_then(serde_json::Value::as_u64), result.get("height").and_then(serde_json::Value::as_u64))
                    {
                        log::info!("photocraft-web: pasted clipboard image {width}x{height} as a new document");
                    } else {
                        log::info!("photocraft-web: pasted clipboard image as a layer");
                    }
                }
                Err(error) => {
                    self.clipboard.lock().unwrap_or_else(|poison| poison.into_inner()).paste_requested = false;
                    self.app.ui.status = error;
                    self.app.ui.status_error = true;
                }
            }
        }
    }

    /// Fetches the served families asked for (picked in a font menu, or needed by a layout) and
    /// installs the files that arrived.
    fn serve_fonts(&mut self, ctx: &egui::Context) {
        let requested = served_fonts::take_requests();
        let (fetch, arrived) = {
            let mut s = self.served.lock().unwrap_or_else(|e| e.into_inner());
            let mut fetch = Vec::new();
            for family in requested {
                if let Some(files) = s.files.get(&family).cloned()
                    && s.fetched.insert(family.clone())
                {
                    fetch.extend(files.into_iter().map(|file| (family.clone(), file)));
                }
            }
            (fetch, std::mem::take(&mut s.arrived))
        };
        for (family, file) in fetch {
            let served = self.served.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_bytes(&file).await {
                    Ok(bytes) => {
                        served.lock().unwrap_or_else(|e| e.into_inner()).arrived.push((family, bytes));
                        ctx.request_repaint();
                    }
                    Err(e) => log::error!("couldn't fetch served font {file}: {e}"),
                }
            });
        }
        served_fonts::install(&mut self.app, arrived);
    }
}

impl eframe::App for WebShell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        for f in dropped {
            let inbox = self.inbox.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dropped".into());
                match f.bytes_async().await {
                    Ok(bytes) => match authorize_opened_project(&name, &bytes).await {
                        Ok(()) => {
                            inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                            ctx.request_repaint();
                        }
                        Err(error) => log::warn!("couldn't authorize dropped project {name}: {error}"),
                    },
                    Err(e) => log::error!("couldn't read dropped file {name}: {e}"),
                }
            });
        }
        self.receive_clipboard_image();
        self.serve_fonts(ctx);
        self.app.logic(ctx, frame);
        self.finish_clipboard_paste(ctx);
        self.unsaved.store(self.app.has_unsaved_work(), Ordering::Relaxed);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }
}

fn services(
    inbox: Inbox,
    clipboard: Clipboard,
    semantic_select: Option<photocraft_ui_egui::semantic_select_ui::Service>,
    cloud_ai_edit: Option<photocraft_ui_egui::cloud_ai_edit_ui::Service>,
) -> Services {
    let clipboard_get_image = Some(Box::new(move || {
        let mut state = clipboard.lock().unwrap_or_else(|error| error.into_inner());
        let image = state.image.clone();
        if image.is_some() {
            state.paste_requested = false;
        }
        image
    }) as photocraft_ui_egui::ClipboardGetFn);
    Services {
        screen_pick: screen_color_service(),
        semantic_select,
        cloud_ai_edit,
        import: Some(Box::new(|name: &str, bytes: &[u8]| photocraft_io::import(name, bytes).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string()))),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let mut opts = photocraft_io::ExportOptions::default();
            if let Some(q) = settings.jpeg_quality {
                opts.encode.jpeg_quality = q;
            }
            opts.encode.webp_lossless = settings.webp_lossless;
            if let Some(q) = settings.webp_quality {
                opts.encode.webp_quality = q;
            }
            opts.tiff_layers = settings.tiff_layers;
            opts.xmp = if settings.xmp_all { photocraft_io::XmpEmbed::All } else { photocraft_io::XmpEmbed::None };
            photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        file_dialog: Some(Box::new(|request, _parent, reply| match request {
            // The browser's file picker hands over the file's contents, not a path.
            FileDialogRequest::Open { .. } => wasm_bindgen_futures::spawn_local(async move {
                let picked = rfd::AsyncFileDialog::new().add_filter("All Formats", OPEN_EXTS).pick_file().await;
                let answer = match picked {
                    Some(file) => {
                        let bytes = file.read().await;
                        match authorize_opened_project(&file.file_name(), &bytes).await {
                            Ok(()) => Some(FileDialogAnswer::Contents(file.file_name(), bytes)),
                            Err(error) => {
                                log::warn!("couldn't authorize opened project: {error}");
                                None
                            }
                        }
                    }
                    None => None,
                };
                reply.send(answer);
            }),
            // No save dialog on the web: the suggested name becomes the download name.
            FileDialogRequest::Save { suggested } => {
                let name = std::path::Path::new(&suggested).file_name().map_or_else(|| suggested.clone(), |n| n.to_string_lossy().to_string());
                reply.send(Some(FileDialogAnswer::SaveTo(name)));
            }
        })),
        write: Some(Box::new(|path: &str, bytes: &[u8]| download(path, bytes))),
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        inbox: Some(inbox),
        clipboard_get_image,
        // Preferences live in the browser's localStorage.
        load_prefs: Some(Box::new(|| local_storage()?.get_item(PREFS_KEY).ok().flatten())),
        save_prefs: Some(Box::new(|text: &str| local_storage().ok_or("no localStorage")?.set_item(PREFS_KEY, text).map_err(|e| format!("{e:?}")))),
        ..Default::default()
    }
}

/// Decode a browser clipboard image through the same image importer as File › Open, then flatten
/// it to the RGBA8 contract expected by the editor's platform clipboard boundary.
fn decode_clipboard_image(name: &str, bytes: &[u8]) -> Result<ClipboardImage, String> {
    let document = photocraft_io::import(name, bytes).map_err(|error| format!("Could not paste {name}: {error}"))?.document;
    let bounds = document.bounds();
    let (width, height) = (bounds.width(), bounds.height());
    let count = usize::try_from(width)
        .ok()
        .and_then(|width| usize::try_from(height).ok().and_then(|height| width.checked_mul(height)))
        .ok_or_else(|| "The pasted image is too large".to_string())?;
    let surface = photocraft_compose::flatten_to_surface(&document, document.pixel_format(), None);
    let mut pixels = vec![[0u8; 4]; count];
    surface.read_rgba8_into(bounds, &mut pixels);
    Ok((width, height, pixels.into_iter().flatten().collect()))
}

const PREFS_KEY: &str = "photocraft.preferences";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Trigger a browser download of `bytes` named after the last component of `path`.
fn download(path: &str, bytes: &[u8]) -> Result<(), String> {
    let js = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "photocraft".into());
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime_for(&name));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(js)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a").map_err(js)?.dyn_into().map_err(|_| "not an anchor")?;
    a.set_href(&url);
    a.set_download(&name);
    a.style().set_property("display", "none").map_err(js)?;
    let body = document.body().ok_or("no body")?;
    body.append_child(&a).map_err(js)?;
    a.click();
    a.remove();
    // Revoke after the click has been dispatched; the download keeps its own reference.
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        web_sys::Url::revoke_object_url(&url).ok();
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000).map_err(js)?;
    Ok(())
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("tif" | "tiff") => "image/tiff",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("psd" | "psb") => "image/vnd.adobe.photoshop",
        _ => "application/octet-stream",
    }
}

/// Rust bindings to the browser's user-activated EyeDropper API; unsupported browsers keep
/// the document picker and its normal canvas zoom. The browser owns its screen magnifier.
fn screen_color_service() -> Option<photocraft_ui_egui::screen_picker::Service> {
    use js_sys::{Function, Reflect};
    use photocraft_ui_egui::screen_picker::{Capture, Pending};
    use wasm_bindgen::{JsValue, closure::Closure};
    let window = web_sys::window()?;
    let constructor = Reflect::get(&window, &JsValue::from_str("EyeDropper")).ok()?.dyn_into::<Function>().ok()?;
    Some(Box::new(move |ctx| {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = (|| -> Result<_, String> {
            let eye = Reflect::construct(&constructor, &js_sys::Array::new()).map_err(|e| format!("Could not start browser eyedropper: {e:?}"))?;
            let controller = web_sys::AbortController::new().map_err(|e| format!("Could not create eyedropper cancellation: {e:?}"))?;
            let options = js_sys::Object::new();
            Reflect::set(&options, &JsValue::from_str("signal"), &controller.signal()).map_err(|e| format!("Could not set eyedropper options: {e:?}"))?;
            let open = Reflect::get(&eye, &JsValue::from_str("open"))
                .map_err(|e| format!("Browser eyedropper has no open method: {e:?}"))?
                .dyn_into::<Function>()
                .map_err(|_| "Invalid browser eyedropper method")?;
            let promise = open
                .call1(&eye, &options)
                .map_err(|e| format!("Could not open browser eyedropper: {e:?}"))?
                .dyn_into::<js_sys::Promise>()
                .map_err(|_| "Browser eyedropper returned no promise")?;
            let stop = cancelled.clone();
            let abort = controller.clone();
            let timer = Closure::<dyn FnMut()>::new(move || {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    abort.abort();
                }
            });
            let interval = window.set_interval_with_callback_and_timeout_and_arguments_0(timer.as_ref().unchecked_ref(), 50).map_err(|e| {
                controller.abort();
                format!("Could not watch eyedropper cancellation: {e:?}")
            })?;
            Ok((promise, timer, interval))
        })();
        match started {
            Err(e) => {
                let _ = tx.send(Err(e));
            }
            Ok((promise, timer, interval)) => {
                let wake = ctx.clone();
                let window = window.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = match wasm_bindgen_futures::JsFuture::from(promise).await {
                        Ok(value) => Reflect::get(&value, &JsValue::from_str("sRGBHex"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .and_then(|h| photocraft_ui_egui::color_picker_ui::parse_hex(&h))
                            .map(|rgb| Capture::Color(Some(rgb)))
                            .ok_or_else(|| "Browser eyedropper returned an invalid color".into()),
                        Err(e) if Reflect::get(&e, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).as_deref() == Some("AbortError") => {
                            Ok(Capture::Color(None))
                        }
                        Err(e) => Err(format!("Browser screen color selection failed: {e:?}")),
                    };
                    window.clear_interval_with_handle(interval);
                    drop(timer);
                    let _ = tx.send(result);
                    wake.request_repaint();
                });
            }
        }
        Pending { receiver: rx, cancelled }
    }))
}

#[cfg(test)]
mod tests {
    use super::{ProjectAllowanceError, is_preset_library, project_allowance_error};

    #[test]
    fn preset_libraries_do_not_consume_image_project_slots() {
        for name in ["brush.ABR", "gradient.grd", "swatches.aco", "palette.ase"] {
            assert!(is_preset_library(name), "{name}");
        }
        for name in ["photo.png", "design.psd", "document.pcraft", "vector.svg"] {
            assert!(!is_preset_library(name), "{name}");
        }
    }

    #[test]
    fn only_explicit_auth_and_allowance_denials_block_local_projects() {
        assert!(matches!(project_allowance_error(401), ProjectAllowanceError::Denied(_)));
        assert!(matches!(project_allowance_error(403), ProjectAllowanceError::Denied(_)));
        for status in [0, 404, 429, 500, 502, 503] {
            assert!(matches!(project_allowance_error(status), ProjectAllowanceError::Unavailable(_)), "HTTP {status}");
        }
    }
}
