//! Deterministic cleanup for point-prompt segmentation masks.

use std::collections::VecDeque;

use crate::{PointKind, PromptPoint};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MaskMetrics {
    pub area: usize,
    pub area_ratio: f32,
    pub border_ratio: f32,
    pub edges_touched: u8,
    pub prompt_selected: bool,
    pub bounds: Option<(usize, usize, usize, usize)>,
}

impl MaskMetrics {
    pub(crate) fn viable(self) -> bool {
        self.prompt_selected && self.area >= 4 && self.area_ratio <= 0.82 && self.border_ratio <= 0.28 && self.edges_touched <= 2
    }

    pub(crate) fn score(self) -> f32 {
        self.area_ratio - self.border_ratio * 2.0 - f32::from(self.edges_touched) * 0.04
    }
}

/// Measure whether a point-prompt proposal looks like foreground rather than
/// the surrounding canvas. Automatic subject discovery uses this to reject a
/// large mask attached to several image edges before it can erase the object.
pub(crate) fn automatic_mask_metrics(width: u32, height: u32, alpha: &[u8], prompt: PromptPoint) -> MaskMetrics {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || alpha.len() != w.saturating_mul(h) {
        return MaskMetrics { area: 0, area_ratio: 0.0, border_ratio: 0.0, edges_touched: 0, prompt_selected: false, bounds: None };
    }
    let (mut area, mut border) = (0usize, 0usize);
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0usize, 0usize);
    let mut edges = [false; 4];
    for y in 0..h {
        for x in 0..w {
            if alpha[y * w + x] < 128 {
                continue;
            }
            area += 1;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            if x == 0 {
                border += 1;
                edges[0] = true;
            }
            if x + 1 == w {
                border += 1;
                edges[1] = true;
            }
            if y == 0 {
                border += 1;
                edges[2] = true;
            }
            if y + 1 == h {
                border += 1;
                edges[3] = true;
            }
        }
    }
    let px = (prompt.x.clamp(0.0, 1.0) * width.saturating_sub(1) as f32).round() as usize;
    let py = (prompt.y.clamp(0.0, 1.0) * height.saturating_sub(1) as f32).round() as usize;
    MaskMetrics {
        area,
        area_ratio: area as f32 / alpha.len() as f32,
        border_ratio: border as f32 / (2 * w + 2 * h).max(1) as f32,
        edges_touched: edges.into_iter().filter(|edge| *edge).count() as u8,
        prompt_selected: alpha[py.min(h - 1) * w + px.min(w - 1)] >= 128,
        bounds: (area > 0).then_some((min_x, min_y, max_x, max_y)),
    }
}

pub(crate) fn related_masks(
    _width: u32,
    height: u32,
    left: &[u8],
    left_metrics: MaskMetrics,
    right: &[u8],
    right_metrics: MaskMetrics,
    allow_seam: bool,
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let (mut left_area, mut right_area, mut overlap) = (0usize, 0usize, 0usize);
    for (a, b) in left.iter().zip(right) {
        let a = *a >= 128;
        let b = *b >= 128;
        left_area += usize::from(a);
        right_area += usize::from(b);
        overlap += usize::from(a && b);
    }
    let pixel_overlap = overlap as f32 / left_area.min(right_area).max(1) as f32;
    let larger_ratio = left_metrics.area_ratio.max(right_metrics.area_ratio);
    let smaller_ratio = left_metrics.area_ratio.min(right_metrics.area_ratio);
    if larger_ratio > 0.2 && larger_ratio > smaller_ratio * 2.5 {
        return false;
    }
    if right_metrics.area_ratio > left_metrics.area_ratio * 1.15 {
        return false;
    }
    if right_metrics.edges_touched > left_metrics.edges_touched {
        return false;
    }
    let bounds_area = |bounds: Option<(usize, usize, usize, usize)>| bounds.map_or(0, |(x0, y0, x1, y1)| (x1 - x0 + 1).saturating_mul(y1 - y0 + 1));
    if bounds_area(right_metrics.bounds) > bounds_area(left_metrics.bounds).saturating_mul(3) / 2 {
        return false;
    }
    if pixel_overlap >= 0.08 {
        return true;
    }
    // Some legitimate compound subjects have a narrow unselected seam (for
    // example foliage immediately above its pot). Join only small, vertically
    // adjacent masks with meaningful horizontal overlap. This is deliberately
    // stricter than generic bounds overlap, which once joined a containing wall
    // to a correctly detected person.
    if !allow_seam {
        return false;
    }
    match (left_metrics.bounds, right_metrics.bounds) {
        (Some((lx0, ly0, lx1, ly1)), Some((rx0, ry0, rx1, ry1))) if left_metrics.area_ratio <= 0.25 && right_metrics.area_ratio <= 0.25 => {
            let overlap_width = lx1.min(rx1).checked_sub(lx0.max(rx0)).map_or(0, |value| value + 1);
            let min_width = (lx1 - lx0 + 1).min(rx1 - rx0 + 1).max(1);
            let vertical_gap = if ly1 < ry0 {
                ry0 - ly1 - 1
            } else if ry1 < ly0 {
                ly0 - ry1 - 1
            } else {
                0
            };
            overlap_width as f32 / min_width as f32 >= 0.2 && vertical_gap <= (height as usize / 30).max(2)
        }
        _ => false,
    }
}

#[cfg(feature = "mobile-sam")]
pub(crate) fn union_metrics(left: MaskMetrics, right: MaskMetrics, area: usize, total: usize) -> MaskMetrics {
    let bounds = match (left.bounds, right.bounds) {
        (Some((lx0, ly0, lx1, ly1)), Some((rx0, ry0, rx1, ry1))) => Some((lx0.min(rx0), ly0.min(ry0), lx1.max(rx1), ly1.max(ry1))),
        (bounds @ Some(_), None) | (None, bounds @ Some(_)) => bounds,
        (None, None) => None,
    };
    MaskMetrics {
        area,
        area_ratio: area as f32 / total.max(1) as f32,
        border_ratio: left.border_ratio.max(right.border_ratio),
        edges_touched: left.edges_touched.max(right.edges_touched),
        prompt_selected: left.prompt_selected || right.prompt_selected,
        bounds,
    }
}

/// Keep the connected object nearest the positive click, discard unrelated
/// islands and fill only tiny enclosed holes. Large semantic openings (for
/// example a mug handle) remain transparent.
pub(crate) fn clean_clicked_object(width: u32, height: u32, alpha: &mut [u8], points: &[PromptPoint]) {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || alpha.len() != w.saturating_mul(h) {
        return;
    }
    let mut labels = vec![usize::MAX; alpha.len()];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for start in 0..alpha.len() {
        if alpha[start] == 0 || labels[start] != usize::MAX {
            continue;
        }
        let label = components.len();
        let mut pixels = Vec::new();
        let mut queue = VecDeque::from([start]);
        labels[start] = label;
        while let Some(index) = queue.pop_front() {
            pixels.push(index);
            let (x, y) = (index % w, index / w);
            for (nx, ny) in neighbours(x, y, w, h) {
                let next = ny * w + nx;
                if alpha[next] != 0 && labels[next] == usize::MAX {
                    labels[next] = label;
                    queue.push_back(next);
                }
            }
        }
        components.push(pixels);
    }
    if components.is_empty() {
        return;
    }
    let positives: Vec<(i64, i64)> = points
        .iter()
        .filter(|point| point.kind == PointKind::Foreground)
        .map(|point| {
            let x = (point.x.clamp(0.0, 1.0) * (width.saturating_sub(1)) as f32).round() as i64;
            let y = (point.y.clamp(0.0, 1.0) * (height.saturating_sub(1)) as f32).round() as i64;
            (x, y)
        })
        .collect();
    let keep = components
        .iter()
        .enumerate()
        .min_by_key(|(_, component)| {
            component
                .iter()
                .flat_map(|index| {
                    let (x, y) = ((*index % w) as i64, (*index / w) as i64);
                    positives.iter().map(move |(px, py)| (x - px).pow(2) + (y - py).pow(2))
                })
                .min()
                .unwrap_or(i64::MAX)
        })
        .map(|(label, _)| label)
        .unwrap_or(0);
    for (index, value) in alpha.iter_mut().enumerate() {
        if *value != 0 && labels[index] != keep {
            *value = 0;
        }
    }
    fill_tiny_holes(w, h, alpha);
}

fn neighbours(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = (usize, usize)> {
    let x0 = x.saturating_sub(1);
    let y0 = y.saturating_sub(1);
    let x1 = (x + 1).min(w - 1);
    let y1 = (y + 1).min(h - 1);
    (y0..=y1).flat_map(move |ny| (x0..=x1).filter_map(move |nx| (nx != x || ny != y).then_some((nx, ny))))
}

fn fill_tiny_holes(w: usize, h: usize, alpha: &mut [u8]) {
    let limit = (w.saturating_mul(h) / 500).max(16);
    let mut seen = vec![false; alpha.len()];
    for start in 0..alpha.len() {
        if alpha[start] != 0 || seen[start] {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        let mut pixels = Vec::new();
        let mut touches_edge = false;
        seen[start] = true;
        while let Some(index) = queue.pop_front() {
            pixels.push(index);
            let (x, y) = (index % w, index / w);
            touches_edge |= x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            for (nx, ny) in neighbours(x, y, w, h) {
                let next = ny * w + nx;
                if alpha[next] == 0 && !seen[next] {
                    seen[next] = true;
                    queue.push_back(next);
                }
            }
        }
        if !touches_edge && pixels.len() <= limit {
            for index in pixels {
                alpha[index] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> PromptPoint {
        PromptPoint { x, y, kind: PointKind::Foreground }
    }

    #[test]
    fn keeps_the_component_nearest_the_click_and_removes_islands() {
        let mut mask = vec![0; 12 * 8];
        for y in 1..7 {
            for x in 1..5 {
                mask[y * 12 + x] = 255;
            }
        }
        for y in 2..6 {
            for x in 8..11 {
                mask[y * 12 + x] = 255;
            }
        }
        clean_clicked_object(12, 8, &mut mask, &[point(0.8, 0.5)]);
        assert_eq!(mask[3 * 12 + 9], 255);
        assert_eq!(mask[3 * 12 + 3], 0);
    }

    #[test]
    fn fills_tiny_holes_but_preserves_large_semantic_openings() {
        let mut mask = vec![255; 40 * 40];
        mask[10 * 40 + 10] = 0;
        for y in 16..24 {
            for x in 16..24 {
                mask[y * 40 + x] = 0;
            }
        }
        clean_clicked_object(40, 40, &mut mask, &[point(0.1, 0.1)]);
        assert_eq!(mask[10 * 40 + 10], 255);
        assert_eq!(mask[20 * 40 + 20], 0);
    }

    #[test]
    fn automatic_metrics_reject_border_background_and_keep_subject() {
        let (w, h) = (20, 16);
        let mut background = vec![0; w * h];
        for y in 0..h {
            for x in 0..w {
                if x < 8 || !(3..=12).contains(&y) {
                    background[y * w + x] = 255;
                }
            }
        }
        let mut subject = vec![0; w * h];
        for y in 3..=12 {
            for x in 5..=13 {
                subject[y * w + x] = 255;
            }
        }
        let bg = automatic_mask_metrics(w as u32, h as u32, &background, point(0.25, 0.5));
        let fg = automatic_mask_metrics(w as u32, h as u32, &subject, point(0.4, 0.5));
        assert!(!bg.viable());
        assert!(fg.viable());
        assert!(fg.score() > bg.score());
    }

    #[test]
    fn related_masks_join_split_subject_but_not_distractor() {
        let (w, h) = (20, 16);
        let mut crown = vec![0; w * h];
        let mut base = vec![0; w * h];
        let mut distractor = vec![0; w * h];
        for y in 3..=9 {
            for x in 4..=12 {
                crown[y * w + x] = 255;
            }
        }
        for y in 8..=13 {
            for x in 7..=11 {
                base[y * w + x] = 255;
            }
        }
        for y in 3..=5 {
            for x in 16..=18 {
                distractor[y * w + x] = 255;
            }
        }
        let crown_metrics = automatic_mask_metrics(w as u32, h as u32, &crown, point(0.4, 0.4));
        let base_metrics = automatic_mask_metrics(w as u32, h as u32, &base, point(0.45, 0.7));
        let distractor_metrics = automatic_mask_metrics(w as u32, h as u32, &distractor, point(0.85, 0.25));
        assert!(related_masks(w as u32, h as u32, &crown, crown_metrics, &base, base_metrics, true));
        assert!(!related_masks(w as u32, h as u32, &crown, crown_metrics, &distractor, distractor_metrics, true));

        let mut wall = vec![0; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                wall[y * w + x] = 255;
            }
        }
        let wall_metrics = automatic_mask_metrics(w as u32, h as u32, &wall, point(0.5, 0.5));
        assert!(
            !related_masks(w as u32, h as u32, &crown, crown_metrics, &wall, wall_metrics, true),
            "a containing background must not be merged into the subject"
        );
    }
}
