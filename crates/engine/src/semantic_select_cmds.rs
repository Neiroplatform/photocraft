//! State-bound semantic-selection proposals.
//!
//! ML runtimes are deliberately outside the engine. They may stage an
//! untrusted mask against the active document revision; only the commands in
//! this module can commit or cancel that proposal.

use std::collections::VecDeque;

use photocraft_algo::selection::{Region, SelectionMode};
use photocraft_doc::{DocId, LayerId};
use photocraft_geom::Rect;
use photocraft_ml::SegmentationMask;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const APPLY: &str = "select.semantic.apply";
const CANCEL: &str = "select.semantic.cancel";
const MAX_PROPOSALS: usize = 8;

#[derive(Clone, Debug)]
struct Proposal {
    token: u64,
    document: DocId,
    revision: u64,
    source_layer: Option<LayerId>,
    sample_all_layers: bool,
    canvas: Rect,
    mask: SegmentationMask,
}

#[derive(Default)]
pub(crate) struct ProposalStore {
    next: u64,
    proposals: VecDeque<Proposal>,
}

impl ProposalStore {
    pub(crate) fn remove_document(&mut self, document: DocId) {
        self.proposals.retain(|proposal| proposal.document != document);
    }

    fn take(&mut self, token: u64) -> Option<Proposal> {
        let index = self.proposals.iter().position(|proposal| proposal.token == token)?;
        self.proposals.remove(index)
    }

    fn discard(&mut self, token: u64) -> bool {
        self.take(token).is_some()
    }
}

impl std::fmt::Debug for ProposalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposalStore").field("pending", &self.proposals.len()).finish()
    }
}

impl Session {
    /// Stage an untrusted semantic mask against the exact active document state.
    /// This is revision-neutral; [`APPLY`] is the only path that can mutate the
    /// canonical selection.
    pub fn stage_selection_proposal(&mut self, mask: SegmentationMask, sample_all_layers: bool) -> Result<u64> {
        mask.validate().map_err(|error| EngineError::Other(error.to_string()))?;
        let state = self.active().ok_or(EngineError::NoDocument)?;
        let canvas = state.doc.bounds();
        if canvas.is_empty() {
            return Err(EngineError::Other("cannot stage a selection for an empty canvas".into()));
        }
        if !same_aspect(mask.width, mask.height, canvas.width(), canvas.height()) {
            return Err(EngineError::Other(format!(
                "semantic mask {}x{} does not match canvas aspect {}x{}",
                mask.width,
                mask.height,
                canvas.width(),
                canvas.height()
            )));
        }
        let document = state.doc.id;
        let revision = state.revision;
        let source_layer = (!sample_all_layers).then_some(state.active_layer).flatten();
        // A new preview for the same document supersedes the old one. This keeps
        // repeated inference bounded even when a UI disappears without Cancel.
        self.semantic_proposals.remove_document(document);
        self.semantic_proposals.next = self.semantic_proposals.next.saturating_add(1).max(1);
        let token = self.semantic_proposals.next;
        self.semantic_proposals.proposals.push_back(Proposal { token, document, revision, source_layer, sample_all_layers, canvas, mask });
        while self.semantic_proposals.proposals.len() > MAX_PROPOSALS {
            self.semantic_proposals.proposals.pop_front();
        }
        Ok(token)
    }
}

fn same_aspect(a_w: u32, a_h: u32, b_w: u32, b_h: u32) -> bool {
    let lhs = u64::from(a_w) * u64::from(b_h);
    let rhs = u64::from(a_h) * u64::from(b_w);
    let difference = lhs.abs_diff(rhs);
    // Reduced previews round each side independently. Accept that sub-pixel
    // rounding, but reject masks whose aspect is materially unrelated to the
    // document (which would silently stretch a model result over the canvas).
    difference.saturating_mul(100) <= lhs.max(rhs)
}

fn token(p: &Value, cmd: &str) -> Result<u64> {
    p.get("token")
        .and_then(Value::as_u64)
        .filter(|token| *token != 0)
        .ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: "needs a non-zero `token`".into() })
}

fn selection_mode(p: &Value) -> SelectionMode {
    SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("replace"))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let token = token(p, APPLY)?;
    let proposal = s.semantic_proposals.take(token).ok_or_else(|| EngineError::Other("semantic selection proposal is missing or already consumed".into()))?;
    let state = s.active().ok_or(EngineError::NoDocument)?;
    if state.doc.id != proposal.document || state.revision != proposal.revision {
        return Err(EngineError::Other("semantic selection proposal is stale; the document changed".into()));
    }
    if !proposal.sample_all_layers && state.active_layer != proposal.source_layer {
        return Err(EngineError::Other("semantic selection proposal is stale; the source layer changed".into()));
    }
    let model = proposal.mask.model.clone();
    let region = proposal_region(&proposal)?;
    let mut result = crate::smartselect_cmds::apply(s, "Semantic Object Selection", region, selection_mode(p))?;
    if let Value::Object(fields) = &mut result {
        fields.insert("model".into(), serde_json::to_value(model).map_err(|error| EngineError::Other(error.to_string()))?);
    }
    Ok(result)
}

fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    let token = token(p, CANCEL)?;
    Ok(json!({ "cancelled": s.semantic_proposals.discard(token) }))
}

fn proposal_region(proposal: &Proposal) -> Result<Option<Region>> {
    let (preview_w, preview_h) = (proposal.mask.width as usize, proposal.mask.height as usize);
    let (canvas_w, canvas_h) = (proposal.canvas.width() as usize, proposal.canvas.height() as usize);
    let mut full = vec![0u8; canvas_w.saturating_mul(canvas_h)];
    let horizontal: Vec<_> = (0..canvas_w).map(|x| sample_axis(x, preview_w, canvas_w)).collect();
    for y in 0..canvas_h {
        let (source_y0, source_y1, weight_y, denominator_y) = sample_axis(y, preview_h, canvas_h);
        for (x, &(source_x0, source_x1, weight_x, denominator_x)) in horizontal.iter().enumerate() {
            let sample = |source_x: usize, source_y: usize| {
                proposal.mask.alpha.get(source_y.saturating_mul(preview_w).saturating_add(source_x)).copied().unwrap_or(0) as u64
            };
            if let Some(target) = full.get_mut(y.saturating_mul(canvas_w).saturating_add(x)) {
                let wx0 = denominator_x.saturating_sub(weight_x);
                let wx1 = weight_x;
                let wy0 = denominator_y.saturating_sub(weight_y);
                let wy1 = weight_y;
                let denominator = denominator_x.saturating_mul(denominator_y).max(1);
                let coverage = sample(source_x0, source_y0) * wx0 * wy0
                    + sample(source_x1, source_y0) * wx1 * wy0
                    + sample(source_x0, source_y1) * wx0 * wy1
                    + sample(source_x1, source_y1) * wx1 * wy1;
                *target = ((coverage + denominator / 2) / denominator).min(u64::from(u8::MAX)) as u8;
            }
        }
    }
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for (index, _) in full.iter().enumerate().filter(|(_, alpha)| **alpha != 0) {
        let (x, y) = (index % canvas_w, index / canvas_w);
        bounds = Some(match bounds {
            None => (x, y, x + 1, y + 1),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
        });
    }
    let Some((x0, y0, x1, y1)) = bounds else { return Ok(None) };
    let width = x1 - x0;
    let mut mask = Vec::with_capacity(width.saturating_mul(y1 - y0));
    for y in y0..y1 {
        let start = y.saturating_mul(canvas_w).saturating_add(x0);
        let end = start.saturating_add(width);
        let row = full.get(start..end).ok_or_else(|| EngineError::Other("semantic mask bounds are invalid".into()))?;
        mask.extend_from_slice(row);
    }
    Ok(Some(Region {
        bbox: Rect::new(proposal.canvas.x0 + x0 as i32, proposal.canvas.y0 + y0 as i32, proposal.canvas.x0 + x1 as i32, proposal.canvas.y0 + y1 as i32),
        mask,
    }))
}

/// Pixel-centred source interval and exact integer interpolation weight.
/// Keeping the mapping rational makes native and WASM masks byte-identical.
fn sample_axis(output: usize, source_len: usize, output_len: usize) -> (usize, usize, u64, u64) {
    if source_len <= 1 || output_len == 0 {
        return (0, 0, 0, 1);
    }
    let denominator = (output_len as u64).saturating_mul(2).max(1);
    let centred = ((output as u64).saturating_mul(2).saturating_add(1)).saturating_mul(source_len as u64);
    let Some(position) = centred.checked_sub(output_len as u64) else {
        return (0, 0, 0, 1);
    };
    let maximum = (source_len.saturating_sub(1) as u64).saturating_mul(denominator);
    if position >= maximum {
        let last = source_len - 1;
        return (last, last, 0, 1);
    }
    let left = (position / denominator) as usize;
    let weight = position % denominator;
    (left, left + 1, weight, denominator)
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: APPLY,
            label: "Apply Semantic Object Selection",
            menu: &[],
            shortcut: None,
            params: r##"{"token":u64,"mode":"replace|add|subtract|intersect"="replace"}"##,
            enabled: has_doc,
            run: apply,
            journal: true,
        },
        CommandSpec {
            id: CANCEL,
            label: "Cancel Semantic Object Selection",
            menu: &[],
            shortcut: None,
            params: r##"{"token":u64}"##,
            enabled: |_| Ok(()),
            run: cancel,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_ml::{ModelIdentity, SegmentationMask};

    fn session() -> Session {
        let mut session = Session::new();
        let result = session.execute("file.new", json!({"width": 8, "height": 6}));
        assert!(result.is_ok());
        session
    }

    fn mask(alpha: Vec<u8>) -> SegmentationMask {
        SegmentationMask { width: 4, height: 3, alpha, confidence: Some(0.9), model: ModelIdentity::mobile_sam_tiny() }
    }

    fn selected(session: &Session, x: i32, y: i32) -> f32 {
        session.active().and_then(|state| state.doc.selection.as_ref()).map_or(0.0, |selection| selection.sample_channel(x, y, 0))
    }

    #[test]
    fn apply_commits_one_selection_step_and_consumes_proposal() {
        let mut session = session();
        let token = session.stage_selection_proposal(mask(vec![0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255, 0]), false);
        assert!(token.is_ok());
        let token = token.unwrap_or(0);
        let revision = session.active().map(|state| state.revision).unwrap_or(0);
        let history = session.active().map(|state| state.history.past_len()).unwrap_or(0);
        let result = session.execute(APPLY, json!({"token": token, "mode": "replace"}));
        assert!(result.is_ok());
        assert!(selected(&session, 3, 3) > 0.9);
        assert_eq!(session.active().map(|state| state.revision), Some(revision + 1));
        assert_eq!(session.active().map(|state| state.history.past_len()), Some(history + 1));
        assert!(session.execute(APPLY, json!({"token": token})).is_err());
    }

    #[test]
    fn cancel_is_revision_neutral_and_stale_apply_fails_closed() {
        let mut session = session();
        let token = session.stage_selection_proposal(mask(vec![255; 12]), false).unwrap_or(0);
        let revision = session.active().map(|state| state.revision).unwrap_or(0);
        let result = session.execute(CANCEL, json!({"token": token}));
        assert_eq!(result.ok().and_then(|value| value.get("cancelled").and_then(Value::as_bool)), Some(true));
        assert_eq!(session.active().map(|state| state.revision), Some(revision));
        assert!(session.execute(APPLY, json!({"token": token})).is_err());

        let token = session.stage_selection_proposal(mask(vec![255; 12]), false).unwrap_or(0);
        assert!(session.execute("layer.new.layer", json!({})).is_ok());
        assert!(session.execute(APPLY, json!({"token": token})).is_err());
        assert!(session.active().and_then(|state| state.doc.selection.as_ref()).is_none());
    }

    #[test]
    fn replacement_proposal_bounds_memory_and_add_mode_preserves_selection() {
        let mut session = session();
        let left = session.stage_selection_proposal(mask(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]), false).unwrap_or(0);
        let superseding = session.stage_selection_proposal(mask(vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]), false).unwrap_or(0);
        assert!(session.execute(APPLY, json!({"token": left})).is_err());
        assert!(session.execute(APPLY, json!({"token": superseding})).is_ok());
        assert!(selected(&session, 7, 3) > 0.9);

        let add = session.stage_selection_proposal(mask(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]), false).unwrap_or(0);
        assert!(session.execute(APPLY, json!({"token": add, "mode": "add"})).is_ok());
        assert!(selected(&session, 0, 3) > 0.9 && selected(&session, 7, 3) > 0.9);
    }

    #[test]
    fn preview_mask_expansion_preserves_soft_boundary_coverage() {
        let mut session = session();
        let alpha = vec![0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255];
        let token = session.stage_selection_proposal(mask(alpha), false).unwrap_or(0);

        assert!(session.execute(APPLY, json!({"token": token})).is_ok());

        let left_fringe = selected(&session, 3, 3);
        let right_fringe = selected(&session, 4, 3);
        assert!(left_fringe > 0.0 && left_fringe < 0.5, "left edge coverage was {left_fringe}");
        assert!(right_fringe > 0.5 && right_fringe < 1.0, "right edge coverage was {right_fringe}");
        assert!(selected(&session, 2, 3) < 0.01);
        assert!(selected(&session, 5, 3) > 0.99);
    }

    #[test]
    fn malformed_or_distorted_proposals_are_rejected() {
        let mut session = session();
        let malformed = SegmentationMask { width: 4, height: 3, alpha: vec![255], confidence: None, model: ModelIdentity::mobile_sam_tiny() };
        assert!(session.stage_selection_proposal(malformed, false).is_err());
        let distorted = SegmentationMask { width: 4, height: 4, alpha: vec![255; 16], confidence: None, model: ModelIdentity::mobile_sam_tiny() };
        assert!(session.stage_selection_proposal(distorted, false).is_err());
    }

    #[test]
    fn one_percent_preview_rounding_is_accepted_without_allowing_distortion() {
        let mut session = session();
        let rounded = SegmentationMask { width: 81, height: 61, alpha: vec![255; 81 * 61], confidence: None, model: ModelIdentity::mobile_sam_tiny() };
        assert!(session.stage_selection_proposal(rounded, false).is_ok());
    }
}
