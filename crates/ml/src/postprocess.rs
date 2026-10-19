//! Deterministic cleanup for point-prompt segmentation masks.

use std::collections::VecDeque;

use crate::{PointKind, PromptPoint};

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
}
