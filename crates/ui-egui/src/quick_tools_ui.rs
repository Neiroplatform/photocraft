//! Pixelle Quick Tools: one task per page over the accepted editor runtime.
//!
//! This module owns presentation state only. Documents, selections, local
//! processing, semantic inference and cloud replies continue through the same
//! engine and typed services as the complete editor.

use photocraft_doc::DocId;
use serde_json::{Value, json};

use crate::{ExportSettings, PhotocraftApp};

const LIME: egui::Color32 = egui::Color32::from_rgb(190, 244, 44);
const INK: egui::Color32 = egui::Color32::from_rgb(22, 24, 20);
const MUTED: egui::Color32 = egui::Color32::from_rgb(103, 105, 97);
const PAPER: egui::Color32 = egui::Color32::from_rgb(243, 241, 234);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(251, 250, 246);
const LINE: egui::Color32 = egui::Color32::from_rgb(216, 214, 204);
const DANGER: egui::Color32 = egui::Color32::from_rgb(180, 52, 42);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Catalog,
    Tool(Tool),
}

impl Route {
    pub fn browser_title(self) -> &'static str {
        match self {
            Self::Catalog => "Быстрые инструменты — Pixelle",
            Self::Tool(tool) => tool.spec().title,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    RemoveBackground,
    BlurBackground,
    ExtractObject,
    RemoveObject,
    AutoEnhance,
    Denoise,
    Sharpen,
    ResizeImage,
    ExpandCanvas,
    ReplaceBackground,
    Relight,
    Restyle,
    GenerativeFill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputKind {
    None,
    Dimensions,
    Prompt,
    Background,
    ClickObject,
    Rectangle,
}

#[derive(Clone, Copy)]
struct Spec {
    tool: Tool,
    slug: &'static str,
    title: &'static str,
    short: &'static str,
    description: &'static str,
    action: &'static str,
    local: bool,
    input: InputKind,
}

const SPECS: &[Spec] = &[
    Spec {
        tool: Tool::RemoveBackground,
        slug: "remove-background",
        title: "Убрать фон — Pixelle",
        short: "Убрать фон",
        description: "Получите аккуратный объект на прозрачном фоне.",
        action: "Убрать фон",
        local: true,
        input: InputKind::None,
    },
    Spec {
        tool: Tool::BlurBackground,
        slug: "blur-background",
        title: "Размыть фон — Pixelle",
        short: "Размыть фон",
        description: "Сохраните объект резким, а фон сделайте мягким.",
        action: "Размыть фон",
        local: true,
        input: InputKind::None,
    },
    Spec {
        tool: Tool::ExtractObject,
        slug: "extract-object",
        title: "Извлечь объект — Pixelle",
        short: "Извлечь объект",
        description: "Отделите главный объект и скачайте прозрачный PNG.",
        action: "Извлечь объект",
        local: true,
        input: InputKind::None,
    },
    Spec {
        tool: Tool::RemoveObject,
        slug: "remove-object",
        title: "Удалить объект — Pixelle",
        short: "Удалить объект",
        description: "Кликните по ненужному предмету — Pixelle заполнит освободившееся место.",
        action: "Удалить выбранное",
        local: true,
        input: InputKind::ClickObject,
    },
    Spec {
        tool: Tool::AutoEnhance,
        slug: "auto-enhance",
        title: "Улучшить фото — Pixelle",
        short: "Улучшить фото",
        description: "Автоматически выровняйте цвет и тон фотографии.",
        action: "Улучшить фото",
        local: true,
        input: InputKind::None,
    },
    Spec {
        tool: Tool::Denoise,
        slug: "denoise",
        title: "Убрать шум — Pixelle",
        short: "Убрать шум",
        description: "Сделайте зернистое изображение чище, сохранив детали.",
        action: "Убрать шум",
        local: true,
        input: InputKind::None,
    },
    Spec {
        tool: Tool::Sharpen,
        slug: "sharpen",
        title: "Добавить резкости — Pixelle",
        short: "Добавить резкости",
        description: "Подчеркните детали без сложных настроек фильтра.",
        action: "Добавить резкости",
        local: true,
        input: InputKind::None,
    },
    Spec {
        tool: Tool::ResizeImage,
        slug: "resize-image",
        title: "Изменить размер — Pixelle",
        short: "Изменить размер",
        description: "Укажите точную ширину и высоту результата.",
        action: "Изменить размер",
        local: true,
        input: InputKind::Dimensions,
    },
    Spec {
        tool: Tool::ExpandCanvas,
        slug: "expand-canvas",
        title: "Расширить холст — Pixelle",
        short: "Расширить холст",
        description: "Добавьте прозрачное пространство вокруг изображения.",
        action: "Расширить холст",
        local: true,
        input: InputKind::Dimensions,
    },
    Spec {
        tool: Tool::ReplaceBackground,
        slug: "replace-background",
        title: "Заменить фон — Pixelle",
        short: "Заменить фон",
        description: "Опишите новый фон и выберите, нужно ли сохранить объект без изменений.",
        action: "Заменить фон",
        local: false,
        input: InputKind::Background,
    },
    Spec {
        tool: Tool::Relight,
        slug: "relight",
        title: "Изменить освещение — Pixelle",
        short: "Изменить освещение",
        description: "Смените время суток, направление и характер света.",
        action: "Изменить освещение",
        local: false,
        input: InputKind::Prompt,
    },
    Spec {
        tool: Tool::Restyle,
        slug: "restyle",
        title: "Стилизовать — Pixelle",
        short: "Стилизовать",
        description: "Примените визуальный стиль, сохранив композицию кадра.",
        action: "Применить стиль",
        local: false,
        input: InputKind::Prompt,
    },
    Spec {
        tool: Tool::GenerativeFill,
        slug: "generative-fill",
        title: "Дорисовать область — Pixelle",
        short: "Дорисовать область",
        description: "Выделите прямоугольную область и опишите, что должно появиться.",
        action: "Дорисовать",
        local: false,
        input: InputKind::Rectangle,
    },
];

impl Tool {
    fn spec(self) -> &'static Spec {
        match self {
            Self::RemoveBackground => &SPECS[0],
            Self::BlurBackground => &SPECS[1],
            Self::ExtractObject => &SPECS[2],
            Self::RemoveObject => &SPECS[3],
            Self::AutoEnhance => &SPECS[4],
            Self::Denoise => &SPECS[5],
            Self::Sharpen => &SPECS[6],
            Self::ResizeImage => &SPECS[7],
            Self::ExpandCanvas => &SPECS[8],
            Self::ReplaceBackground => &SPECS[9],
            Self::Relight => &SPECS[10],
            Self::Restyle => &SPECS[11],
            Self::GenerativeFill => &SPECS[12],
        }
    }
}

pub fn route_from_path(path: &str) -> Option<Route> {
    let file = path.rsplit('/').next().unwrap_or(path).split('?').next().unwrap_or(path);
    if file == "quick-tools.html" {
        return Some(Route::Catalog);
    }
    let slug = file.strip_suffix(".html")?;
    SPECS.iter().find(|spec| spec.slug == slug).map(|spec| Route::Tool(spec.tool))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Empty,
    Ready,
    Selecting,
    Processing,
    Complete,
    Error,
}

pub struct State {
    route: Route,
    styled: bool,
    source_doc: Option<DocId>,
    observed_revision: u64,
    process_revision: u64,
    source_texture: Option<egui::TextureHandle>,
    result_texture: Option<egui::TextureHandle>,
    phase: Phase,
    prompt: String,
    width: String,
    height: String,
    cloud_consent: bool,
    full_image: bool,
    show_original: bool,
    drag_start: Option<egui::Pos2>,
    drag_current: Option<egui::Pos2>,
    corner_anchor: Option<egui::Pos2>,
    drag_moved: bool,
    error: Option<String>,
}

impl State {
    pub fn new(route: Route) -> Self {
        let prompt = match route {
            Route::Tool(Tool::ReplaceBackground) => "светлая современная фотостудия".into(),
            Route::Tool(Tool::Relight) => "мягкий тёплый вечерний свет".into(),
            Route::Tool(Tool::Restyle) => "кинематографическая журнальная фотография".into(),
            Route::Tool(Tool::GenerativeFill) => "естественно продолжи изображение".into(),
            _ => String::new(),
        };
        Self {
            route,
            styled: false,
            source_doc: None,
            observed_revision: 0,
            process_revision: 0,
            source_texture: None,
            result_texture: None,
            phase: Phase::Empty,
            prompt,
            width: String::new(),
            height: String::new(),
            cloud_consent: false,
            full_image: false,
            show_original: false,
            drag_start: None,
            drag_current: None,
            corner_anchor: None,
            drag_moved: false,
            error: None,
        }
    }
}

fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut style = (*ctx.style_of(egui::Theme::Light)).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = PAPER;
    style.visuals.window_fill = SURFACE;
    style.visuals.extreme_bg_color = egui::Color32::WHITE;
    style.visuals.faint_bg_color = egui::Color32::from_rgb(237, 235, 227);
    style.visuals.widgets.inactive.bg_fill = SURFACE;
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
    style.visuals.widgets.hovered.bg_fill = egui::Color32::WHITE;
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, INK);
    style.visuals.widgets.active.bg_fill = LIME;
    style.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5, INK);
    style.visuals.selection.bg_fill = LIME;
    style.visuals.selection.stroke = egui::Stroke::new(1.0, INK);
    style.visuals.hyperlink_color = INK;
    style.spacing.item_spacing = egui::vec2(12.0, 12.0);
    style.spacing.button_padding = egui::vec2(18.0, 11.0);
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);
    ctx.set_style_of(egui::Theme::Light, style);
}

fn thumbnail_texture(ctx: &egui::Context, app: &PhotocraftApp, name: &str) -> Option<egui::TextureHandle> {
    let state = app.session.active()?;
    let preview = photocraft_compose::thumbnail(&state.doc, 1600);
    let image = egui::ColorImage::from_rgba_unmultiplied([preview.width as usize, preview.height as usize], &preview.pixels);
    Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR))
}

fn friendly_error(error: &str) -> String {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("model") || normalized.contains("safetensors") {
        "Не удалось загрузить локальную модель. Проверьте соединение и повторите.".into()
    } else if normalized.contains("cloud") || normalized.contains("gateway") {
        "Облачная обработка сейчас недоступна. Попробуйте ещё раз немного позже.".into()
    } else {
        error.to_string()
    }
}

fn sync_state(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut state) = app.quick_tool.take() else { return };
    if !state.styled {
        install_style(ctx);
        state.styled = true;
    }
    if let Some(active) = app.session.active() {
        if state.source_doc != Some(active.doc.id) {
            state.source_doc = Some(active.doc.id);
            state.observed_revision = active.revision;
            state.source_texture = thumbnail_texture(ctx, app, "quick-tool-source");
            state.result_texture = state.source_texture.clone();
            state.width = active.doc.size.width.to_string();
            state.height = active.doc.size.height.to_string();
            state.phase = Phase::Ready;
            state.show_original = false;
            state.error = None;
        } else if state.observed_revision != active.revision {
            state.observed_revision = active.revision;
            state.result_texture = thumbnail_texture(ctx, app, "quick-tool-result");
        }
    } else if state.source_doc.is_some() {
        state.source_doc = None;
        state.source_texture = None;
        state.result_texture = None;
        state.phase = Phase::Empty;
    }
    let pending = crate::semantic_select_ui::is_pending(app) || crate::cloud_ai_edit_ui::is_pending(app);
    if matches!(state.phase, Phase::Processing) && !pending {
        if app.ui.status_error {
            state.error = Some(friendly_error(&app.ui.status));
            state.phase = Phase::Error;
        } else if app.session.active().is_some_and(|active| active.revision != state.process_revision) {
            state.phase = Phase::Complete;
            state.result_texture = thumbnail_texture(ctx, app, "quick-tool-complete");
        }
    } else if matches!(state.phase, Phase::Selecting) && !crate::semantic_select_ui::is_pending(app) {
        if app.ui.status_error {
            state.error = Some(friendly_error(&app.ui.status));
            state.phase = Phase::Error;
        } else if app.session.active().is_some_and(|active| active.doc.selection.is_some()) {
            state.phase = Phase::Ready;
        }
    }
    app.quick_tool = Some(state);
}

fn open_image(app: &mut PhotocraftApp) {
    let result = app.pick_file_bytes(|app, name, bytes| {
        app.open_bytes(&name, &bytes)?;
        if let Some(state) = app.quick_tool.as_mut() {
            state.source_doc = None;
        }
        Ok(Value::Null)
    });
    if let Err(error) = result
        && error != crate::file_dialog::CANCELLED
    {
        app.ui.status = error;
        app.ui.status_error = true;
    }
}

fn prompt_for(state: &State, tool: Tool) -> Result<String, String> {
    let dimensions = || -> Result<(u32, u32), String> {
        let width = state.width.trim().parse::<u32>().map_err(|_| "Укажите корректную ширину".to_string())?;
        let height = state.height.trim().parse::<u32>().map_err(|_| "Укажите корректную высоту".to_string())?;
        if width == 0 || height == 0 {
            return Err("Размер должен быть больше нуля".into());
        }
        Ok((width, height))
    };
    Ok(match tool {
        Tool::RemoveBackground | Tool::ExtractObject => "убери фон".into(),
        Tool::BlurBackground => "размой фон".into(),
        Tool::RemoveObject => "удали выделенный объект".into(),
        Tool::AutoEnhance => "улучши фото".into(),
        Tool::Denoise => "убери шум".into(),
        Tool::Sharpen => "добавь резкости".into(),
        Tool::ResizeImage => {
            let (width, height) = dimensions()?;
            format!("измени размер на {width}x{height}")
        }
        Tool::ExpandCanvas => {
            let (width, height) = dimensions()?;
            format!("расширь холст до {width}x{height}")
        }
        Tool::ReplaceBackground => format!("замени фон на {}", required_prompt(state)?),
        Tool::Relight => format!("измени освещение: {}", required_prompt(state)?),
        Tool::Restyle => format!("сделай в стиле {}", required_prompt(state)?),
        Tool::GenerativeFill => format!("дорисуй выделенную область: {}", required_prompt(state)?),
    })
}

fn required_prompt(state: &State) -> Result<&str, String> {
    let value = state.prompt.trim();
    if value.is_empty() { Err("Опишите желаемый результат".into()) } else { Ok(value) }
}

fn run_tool(app: &mut PhotocraftApp, tool: Tool) {
    let Some(mut state) = app.quick_tool.take() else { return };
    state.process_revision = app.session.active().map_or(0, |active| active.revision);
    let result = (|| {
        if app.session.active().is_none() {
            return Err("Сначала загрузите изображение".into());
        }
        if tool == Tool::RemoveObject && !app.session.active().is_some_and(|active| active.doc.selection.is_some()) {
            return Err("Кликните по объекту, который нужно удалить".into());
        }
        if tool == Tool::GenerativeFill && !app.session.active().is_some_and(|active| active.doc.selection.is_some()) {
            return Err("Выделите область на изображении".into());
        }
        if !tool.spec().local && !state.cloud_consent {
            return Err("Подтвердите облачную обработку изображения".into());
        }
        let prompt = prompt_for(&state, tool)?;
        app.ui.ai_cloud_consent = state.cloud_consent;
        if tool == Tool::ReplaceBackground {
            app.ui.ai_background_output = if state.full_image {
                crate::cloud_ai_edit_ui::BackgroundOutputMode::FullImage
            } else {
                crate::cloud_ai_edit_ui::BackgroundOutputMode::SeparateLayers
            };
        }
        app.ui.ai_prompt = prompt;
        app.ui.status_error = false;
        crate::ai_edit_ui::run_prompt(app);
        if app.ui.status_error { Err(app.ui.status.clone()) } else { Ok(()) }
    })();
    match result {
        Ok(()) => {
            state.phase = Phase::Processing;
            state.error = None;
        }
        Err(error) => {
            state.error = Some(friendly_error(&error));
            state.phase = Phase::Error;
        }
    }
    app.quick_tool = Some(state);
}

fn download_png(app: &mut PhotocraftApp, tool: Tool) {
    let name = format!("pixelle-{}.png", tool.spec().slug);
    match app.write_document(name, &ExportSettings::default(), true) {
        Ok(_) => {
            app.ui.status = "PNG скачан".into();
            app.ui.status_error = false;
        }
        Err(error) => {
            app.ui.status = error;
            app.ui.status_error = true;
        }
    }
}

fn brand(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        egui::Frame::NONE.fill(LIME).corner_radius(5.0).inner_margin(egui::Margin::same(7)).show(ui, |ui| {
            ui.label(egui::RichText::new("P").strong().color(INK).size(16.0));
        });
        ui.label(egui::RichText::new("Pixelle").strong().color(INK).size(18.0));
    });
}

fn header(ui: &mut egui::Ui, task: Option<Tool>) {
    let compact = ui.available_width() < 560.0;
    ui.horizontal(|ui| {
        brand(ui);
        if compact {
            if task.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.link(egui::RichText::new("Инструменты").color(MUTED)).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::same_tab("quick-tools.html"));
                    }
                });
            }
        } else {
            ui.add_space(12.0);
            if task.is_some() && ui.link(egui::RichText::new("Все инструменты").color(MUTED)).clicked() {
                ui.ctx().open_url(egui::OpenUrl::same_tab("quick-tools.html"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link(egui::RichText::new("Открыть редактор ↗").color(INK)).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::same_tab("patchy.html"));
                }
            });
        }
    });
    ui.add_space(20.0);
    ui.separator();
}

fn catalog(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let available = ui.available_size();
    let width = available.x.min(1180.0);
    ui.allocate_ui_with_layout(egui::vec2(width, available.y), egui::Layout::top_down(egui::Align::Min), |ui| {
        header(ui, None);
        ui.add_space(34.0);
        ui.label(egui::RichText::new("Одна задача.\nОдин отличный результат.").strong().color(INK).size(if width < 700.0 { 38.0 } else { 58.0 }));
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new("Быстрые инструменты Pixelle работают без сложных панелей: загрузите изображение, получите результат и скачайте PNG.")
                .color(MUTED)
                .size(18.0),
        );
        ui.add_space(34.0);
        let columns = width >= 760.0;
        if columns {
            ui.columns(2, |columns| {
                tool_list(&mut columns[0], true);
                tool_list(&mut columns[1], false);
            });
        } else {
            tool_list(ui, true);
            ui.add_space(28.0);
            tool_list(ui, false);
        }
        ui.add_space(36.0);
        ui.label(
            egui::RichText::new("Локальные инструменты не отправляют изображение в облако. Облачные запускаются только после вашего подтверждения.")
                .color(MUTED)
                .size(13.0),
        );
    });
    let _ = app;
}

fn tool_list(ui: &mut egui::Ui, local: bool) {
    let label = if local { "НА УСТРОЙСТВЕ" } else { "ОБЛАЧНЫЙ ИИ" };
    ui.label(egui::RichText::new(label).strong().color(if local { MUTED } else { INK }).size(12.0));
    ui.add_space(6.0);
    for spec in SPECS.iter().filter(|spec| spec.local == local) {
        ui.separator();
        let response = ui
            .add_sized([ui.available_width(), 54.0], egui::Button::new(egui::RichText::new(format!("{}   →", spec.short)).color(INK).size(17.0)).frame(false));
        if response.clicked() {
            ui.ctx().open_url(egui::OpenUrl::same_tab(format!("{}.html", spec.slug)));
        }
    }
}

fn status_pill(ui: &mut egui::Ui, local: bool) {
    let (text, fill) = if local {
        ("На устройстве", egui::Color32::from_rgb(228, 238, 210))
    } else {
        ("Облачный ИИ", egui::Color32::from_rgb(229, 225, 247))
    };
    egui::Frame::NONE.fill(fill).corner_radius(999.0).inner_margin(egui::Margin::symmetric(11, 6)).show(ui, |ui| {
        ui.label(egui::RichText::new(text).strong().color(INK).size(12.0));
    });
}

fn preview(ui: &mut egui::Ui, app: &mut PhotocraftApp, tool: Tool) {
    let Some(mut state) = app.quick_tool.take() else { return };
    let texture = if state.show_original { state.source_texture.as_ref() } else { state.result_texture.as_ref().or(state.source_texture.as_ref()) };
    let Some(texture) = texture else {
        upload_empty(ui, app, &mut state);
        app.quick_tool = Some(state);
        return;
    };
    let available = ui.available_size();
    let source = texture.size_vec2();
    let scale = (available.x / source.x).min(available.y / source.y).min(1.0);
    let size = (source * scale).max(egui::vec2(1.0, 1.0));
    let response = ui.add(egui::Image::new(texture).fit_to_exact_size(size).sense(egui::Sense::click_and_drag()));
    if tool == Tool::RemoveObject
        && response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(active) = app.session.active()
    {
        let u = ((pointer.x - response.rect.left()) / response.rect.width()).clamp(0.0, 1.0);
        let v = ((pointer.y - response.rect.top()) / response.rect.height()).clamp(0.0, 1.0);
        let bounds = active.doc.bounds();
        let point = [f64::from(bounds.x0) + f64::from(u) * f64::from(bounds.width()), f64::from(bounds.y0) + f64::from(v) * f64::from(bounds.height())];
        if crate::semantic_select_ui::select_at(app, point, egui::Modifiers::NONE) {
            state.phase = Phase::Selecting;
            state.error = None;
        }
    }
    if tool == Tool::GenerativeFill {
        if response.drag_started() {
            state.drag_start = response.interact_pointer_pos();
            state.drag_current = state.drag_start;
            state.drag_moved = false;
        }
        if response.dragged() {
            state.drag_current = response.interact_pointer_pos();
            state.drag_moved = true;
        }
        if let (Some(a), Some(b)) = (state.drag_start, state.drag_current) {
            ui.painter().rect_stroke(egui::Rect::from_two_pos(a, b), 2.0, egui::Stroke::new(2.0, LIME), egui::StrokeKind::Inside);
        }
        if let Some(anchor) = state.corner_anchor {
            ui.painter().circle_stroke(anchor, 7.0, egui::Stroke::new(2.0, LIME));
        }
        if response.drag_stopped() {
            let a = state.drag_start.take();
            let b = state.drag_current.take();
            if state.drag_moved
                && let (Some(a), Some(b)) = (a, b)
            {
                state.corner_anchor = None;
                commit_rectangle(app, &mut state, response.rect, a, b);
            }
            state.drag_moved = false;
        }
        if response.clicked()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            if let Some(anchor) = state.corner_anchor.take() {
                commit_rectangle(app, &mut state, response.rect, anchor, pointer);
            } else {
                state.corner_anchor = Some(pointer);
            }
        }
    }
    app.quick_tool = Some(state);
}

fn commit_rectangle(app: &mut PhotocraftApp, state: &mut State, image_rect: egui::Rect, a: egui::Pos2, b: egui::Pos2) {
    let Some(bounds) = app.session.active().map(|active| active.doc.bounds()) else { return };
    let map = |point: egui::Pos2| {
        let u = ((point.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0);
        let v = ((point.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0);
        (bounds.x0 + (u * bounds.width() as f32).round() as i32, bounds.y0 + (v * bounds.height() as f32).round() as i32)
    };
    let (x0, y0) = map(a);
    let (x1, y1) = map(b);
    let rect = photocraft_geom::Rect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
    if rect.width() <= 1 || rect.height() <= 1 {
        state.error = Some("Выделите область большего размера".into());
        state.phase = Phase::Error;
    } else if let Err(error) = app.run("select.rect", json!({"x": rect.x0, "y": rect.y0, "width": rect.width(), "height": rect.height()})) {
        state.error = Some(error);
        state.phase = Phase::Error;
    }
}

fn upload_empty(ui: &mut egui::Ui, app: &mut PhotocraftApp, state: &mut State) {
    let frame = egui::Frame::NONE.fill(SURFACE).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(24.0).inner_margin(egui::Margin::same(34));
    frame.show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(egui::RichText::new("+").color(INK).size(34.0));
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Перетащите изображение сюда").strong().color(INK).size(21.0));
            ui.label(egui::RichText::new("PNG, JPEG, WebP и другие популярные форматы").color(MUTED).size(14.0));
            ui.add_space(18.0);
            if primary_button(ui, "Выбрать изображение", true).clicked() {
                open_image(app);
            }
            ui.add_space(60.0);
        });
    });
    state.phase = Phase::Empty;
}

fn controls(ui: &mut egui::Ui, state: &mut State, spec: &Spec, has_selection: bool) {
    match spec.input {
        InputKind::Dimensions => {
            ui.label(egui::RichText::new("Размер результата").strong().color(INK));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.width).hint_text("Ширина").desired_width(110.0));
                ui.label(egui::RichText::new("×").color(MUTED));
                ui.add(egui::TextEdit::singleline(&mut state.height).hint_text("Высота").desired_width(110.0));
                ui.label(egui::RichText::new("px").color(MUTED));
            });
        }
        InputKind::Prompt | InputKind::Rectangle => {
            ui.label(egui::RichText::new(if spec.input == InputKind::Rectangle { "Что дорисовать" } else { "Опишите результат" }).strong().color(INK));
            ui.add(egui::TextEdit::multiline(&mut state.prompt).desired_rows(3).desired_width(f32::INFINITY));
            if spec.input == InputKind::Rectangle {
                ui.label(
                    egui::RichText::new(if has_selection {
                        "Область выделена"
                    } else {
                        "Протяните прямоугольник или укажите два угла кликами"
                    })
                    .color(if has_selection { INK } else { MUTED })
                    .size(13.0),
                );
            }
        }
        InputKind::Background => {
            ui.label(egui::RichText::new("Новый фон").strong().color(INK));
            ui.add(egui::TextEdit::multiline(&mut state.prompt).desired_rows(3).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Результат").strong().color(INK));
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut state.full_image, false, "Сохранить объект");
                ui.selectable_value(&mut state.full_image, true, "Пересобрать весь кадр");
            });
        }
        InputKind::ClickObject => {
            ui.label(
                egui::RichText::new(if has_selection {
                    "Объект выбран"
                } else {
                    "Кликните по объекту на изображении"
                })
                .strong()
                .color(INK),
            );
            ui.label(
                egui::RichText::new(if has_selection {
                    "Можно удалить его или кликнуть по другому объекту."
                } else {
                    "Модель выделит предмет локально. После этого нажмите кнопку удаления."
                })
                .color(MUTED)
                .size(13.0),
            );
        }
        InputKind::None => {}
    }
    if !spec.local {
        ui.add_space(8.0);
        ui.checkbox(&mut state.cloud_consent, "Я согласен отправить изображение облачному ИИ");
    }
}

fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).strong().color(if enabled { egui::Color32::WHITE } else { MUTED }).size(15.0))
            .fill(if enabled { INK } else { LINE })
            .corner_radius(12.0)
            .min_size(egui::vec2(190.0, 46.0)),
    )
}

fn can_run(state: &State, tool: Tool, has_selection: bool) -> bool {
    state.source_doc.is_some()
        && (!matches!(tool, Tool::RemoveObject | Tool::GenerativeFill) || has_selection)
        && (tool.spec().local || state.cloud_consent)
        && prompt_for(state, tool).is_ok()
}

fn task_page(app: &mut PhotocraftApp, ui: &mut egui::Ui, tool: Tool) {
    let spec = tool.spec();
    let available = ui.available_size();
    let width = available.x.min(1240.0);
    ui.allocate_ui_with_layout(egui::vec2(width, available.y), egui::Layout::top_down(egui::Align::Min), |ui| {
        header(ui, Some(tool));
        ui.add_space(26.0);
        let status = |ui: &mut egui::Ui| {
            status_pill(ui, spec.local);
            if !spec.local {
                ui.label(egui::RichText::new("Изображение покинет устройство только после подтверждения").color(MUTED).size(12.0));
            }
        };
        if width < 560.0 {
            ui.vertical(status);
        } else {
            ui.horizontal(status);
        }
        ui.add_space(12.0);
        ui.label(egui::RichText::new(spec.short).strong().color(INK).size(if width < 700.0 { 34.0 } else { 48.0 }));
        ui.label(egui::RichText::new(spec.description).color(MUTED).size(17.0));
        ui.add_space(24.0);

        let wide = width >= 860.0;
        if wide {
            ui.columns(2, |columns| {
                egui::Frame::NONE.fill(egui::Color32::from_rgb(35, 37, 33)).corner_radius(24.0).inner_margin(egui::Margin::same(18)).show(
                    &mut columns[0],
                    |ui| {
                        ui.set_min_height(520.0);
                        preview(ui, app, tool);
                    },
                );
                side_panel(&mut columns[1], app, tool, spec);
            });
        } else {
            egui::Frame::NONE.fill(egui::Color32::from_rgb(35, 37, 33)).corner_radius(22.0).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                ui.set_min_height(330.0);
                preview(ui, app, tool);
            });
            ui.add_space(18.0);
            side_panel(ui, app, tool, spec);
        }
    });
}

fn side_panel(ui: &mut egui::Ui, app: &mut PhotocraftApp, tool: Tool, spec: &Spec) {
    let Some(mut state) = app.quick_tool.take() else { return };
    let mut run_requested = false;
    let mut download_requested = false;
    egui::Frame::NONE.fill(SURFACE).stroke(egui::Stroke::new(1.0, LINE)).corner_radius(24.0).inner_margin(egui::Margin::same(24)).show(ui, |ui| {
        if state.source_doc.is_none() {
            ui.label(egui::RichText::new("1 · Загрузите фото").strong().color(INK).size(18.0));
            ui.label(egui::RichText::new("Оригинал останется в памяти вкладки и не будет перезаписан.").color(MUTED));
            ui.add_space(18.0);
            if primary_button(ui, "Выбрать изображение", true).clicked() {
                open_image(app);
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Изображение загружено").strong().color(INK));
            if ui.link(egui::RichText::new("Заменить").color(MUTED)).clicked() {
                open_image(app);
            }
        });
        ui.add_space(18.0);
        let has_selection = app.session.active().is_some_and(|active| active.doc.selection.is_some());
        controls(ui, &mut state, spec, has_selection);
        ui.add_space(20.0);
        if matches!(state.phase, Phase::Processing | Phase::Selecting) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new(if state.phase == Phase::Selecting {
                        "Выделяем объект на устройстве…"
                    } else {
                        "Обрабатываем изображение…"
                    })
                    .color(INK),
                );
            });
        } else {
            if primary_button(ui, spec.action, can_run(&state, tool, has_selection)).clicked() {
                run_requested = true;
            }
        }
        if let Some(error) = &state.error {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(error).color(DANGER).size(13.0));
        }
        if state.phase == Phase::Complete {
            ui.add_space(20.0);
            ui.separator();
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Готово").strong().color(INK).size(22.0));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.show_original, true, "До");
                ui.selectable_value(&mut state.show_original, false, "После");
            });
            ui.add_space(12.0);
            if primary_button(ui, "Скачать PNG", true).clicked() {
                download_requested = true;
            }
        }
    });
    app.quick_tool = Some(state);
    if run_requested {
        run_tool(app, tool);
    }
    if download_requested {
        download_png(app, tool);
    }
}

pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    sync_state(app, ui.ctx());
    let route = app.quick_tool.as_ref().map(|state| state.route).unwrap_or(Route::Catalog);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(PAPER).inner_margin(egui::Margin::same(if ui.available_width() < 700.0 { 18 } else { 32 })))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| match route {
                    Route::Catalog => catalog(app, ui),
                    Route::Tool(tool) => task_page(app, ui, tool),
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_one_unique_standalone_route() {
        assert_eq!(SPECS.len(), 13);
        let mut slugs = SPECS.iter().map(|spec| spec.slug).collect::<Vec<_>>();
        slugs.sort_unstable();
        slugs.dedup();
        assert_eq!(slugs.len(), SPECS.len());
        for spec in SPECS {
            assert_eq!(route_from_path(&format!("/releases/current/{}.html", spec.slug)), Some(Route::Tool(spec.tool)));
        }
        assert_eq!(route_from_path("/quick-tools.html"), Some(Route::Catalog));
        assert_eq!(route_from_path("/patchy.html"), None);
    }

    #[test]
    fn prompts_remain_inside_the_existing_closed_action_language() {
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
        for spec in SPECS {
            let mut state = State::new(Route::Tool(spec.tool));
            state.width = "80".into();
            state.height = "60".into();
            state.prompt = "мягкий студийный свет".into();
            let prompt = prompt_for(&state, spec.tool).unwrap();
            assert!(session.plan_ai_action(&prompt).is_ok(), "{}: {prompt}", spec.slug);
        }
    }

    #[test]
    fn cloud_and_local_registry_is_explicit() {
        let cloud = SPECS.iter().filter(|spec| !spec.local).map(|spec| spec.tool).collect::<Vec<_>>();
        assert_eq!(cloud, [Tool::ReplaceBackground, Tool::Relight, Tool::Restyle, Tool::GenerativeFill]);
    }

    #[test]
    fn action_enablement_requires_every_task_input() {
        let mut state = State::new(Route::Tool(Tool::RemoveObject));
        state.source_doc = Some(DocId::fresh());
        assert!(!can_run(&state, Tool::RemoveObject, false));
        assert!(can_run(&state, Tool::RemoveObject, true));

        state.route = Route::Tool(Tool::ReplaceBackground);
        state.prompt = "светлая студия".into();
        assert!(!can_run(&state, Tool::ReplaceBackground, false));
        state.cloud_consent = true;
        assert!(can_run(&state, Tool::ReplaceBackground, false));

        state.route = Route::Tool(Tool::ResizeImage);
        state.width = "0".into();
        state.height = "600".into();
        assert!(!can_run(&state, Tool::ResizeImage, false));
        state.width = "800".into();
        assert!(can_run(&state, Tool::ResizeImage, false));
    }

    #[test]
    fn service_details_are_replaced_with_recoverable_copy() {
        assert_eq!(
            friendly_error("HTTP 404 loading models/mobile-sam-tiny-vitt.safetensors.part-aa"),
            "Не удалось загрузить локальную модель. Проверьте соединение и повторите."
        );
    }
}
