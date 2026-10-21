//! The Pixelle brand mark at the left of the title bar. It is drawn from simple vector shapes so
//! the in-app mark and the SVG favicon share one crisp, first-party identity at every scale.

#[cfg(test)]
use egui::Context;
use egui::{Color32, Id, Rect, Ui, pos2, vec2};

const TILE: Color32 = Color32::from_rgb(17, 21, 26);
const PIXEL_LIME: Color32 = Color32::from_rgb(215, 255, 63);
const PIXEL_LIGHT: Color32 = Color32::from_rgb(244, 247, 237);

/// Where the mark was painted this frame (tests and screenshots).
fn rect_id() -> Id {
    Id::new("pixelle-brand-mark-rect")
}

/// Paint the mark into `r` (square).
pub fn paint_mark(ui: &Ui, r: Rect) {
    let side = r.width().min(r.height());
    let tile = Rect::from_center_size(r.center(), vec2(side, side));
    let unit = side / 16.0;
    let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(tile.left() + x * unit, tile.top() + y * unit), vec2(w * unit, h * unit));

    ui.painter().rect_filled(tile, 3.25 * unit, TILE);
    ui.painter().rect_filled(at(3.5, 3.0, 3.0, 10.0), 0.7 * unit, PIXEL_LIME);
    ui.painter().circle_filled(pos2(tile.left() + 8.5 * unit, tile.top() + 6.5 * unit), 3.5 * unit, PIXEL_LIME);
    ui.painter().circle_filled(pos2(tile.left() + 8.5 * unit, tile.top() + 6.5 * unit), 1.25 * unit, TILE);
    ui.painter().rect_filled(at(10.5, 10.5, 2.0, 2.0), 0.45 * unit, PIXEL_LIGHT);
    ui.ctx().data_mut(|d| d.insert_temp(rect_id(), tile));
}

/// The rect the mark was last painted into.
#[cfg(test)]
pub fn mark_rect(ctx: &Context) -> Option<Rect> {
    ctx.data(|d| d.get_temp(rect_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_bar_paints_the_vector_mark() {
        let ctx = Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| paint_mark(ui, Rect::from_min_size(pos2(4.0, 6.0), vec2(20.0, 20.0))));
        out.textures_delta.clear();
        let rect = mark_rect(&ctx).expect("mark painted");
        assert!(rect.is_positive());
        assert!((rect.width() - rect.height()).abs() < f32::EPSILON);
        assert!(out.shapes.len() >= 5, "tile plus four P-mark shapes: {:?}", out.shapes);
    }
}
