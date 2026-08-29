// Copyright 2021 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Greedy line breaking.

use alloc::vec::Vec;

#[cfg(feature = "libm")]
#[allow(unused_imports)]
use core_maths::CoreFloat;
use parlance::BidiLevel;

use crate::layout::data::count_graphemes;
use crate::layout::{
    BreakReason, Layout, LayoutData, LayoutItem, LayoutItemKind, LineData, LineItemData,
    LineMetrics, Run,
};
use crate::style::{Brush, EndOfLineWhitespace, WhiteSpaceCollapse, WordBreak};
use crate::{InlineBoxKind, OverflowWrap, TextWrapMode};

use core::ops::Range;
use parley_engine::shape::{Character, ShapedCluster, Whitespace};
use parley_engine::{Atom, Boundary, FontMetrics};

#[derive(Default)]
struct LineLayout {
    lines: Vec<LineData>,
    line_items: Vec<LineItemData>,
}

impl LineLayout {
    fn swap<B: Brush>(&mut self, layout: &mut LayoutData<B>) {
        core::mem::swap(&mut self.lines, &mut layout.lines);
        core::mem::swap(&mut self.line_items, &mut layout.line_items);
    }
}

#[derive(Clone, Default)]
struct LineState {
    x: f32,
    items: Range<usize>,
    /// The line's shaped clusters, as a range into [`parley_engine::ShapedText::shaped_clusters`].
    /// The bounds are atom-aligned.
    clusters: Range<u32>,
    box_metrics: LineBoxMetrics,
    /// This is set to true if we encounter something on the line (either a glyph or an inline box)
    /// that is taller than the `line_max_height`. When in this state `break_next` should yield control
    /// flow to the caller to handle the constraint violation.
    ///
    /// This never happens when calling `break_all_lines` as it never sets `line_max_height`, and it defaults to `f32::MAX`.
    max_height_exceeded: bool,

    /// Whether this line has consumed text or an atomic inline box.
    ///
    /// Item ranges alone cannot answer this because a text run can continue
    /// across a line boundary with an empty cluster range.
    has_content: bool,

    /// We lag the text-wrap-mode by one cluster due to line-breaking boundaries only
    /// being triggered on the cluster after the linebreak.
    text_wrap_mode: TextWrapMode,
}

impl LineState {
    /// Reset the per-line running state in preparation for building a new line.
    fn reset(&mut self) {
        self.x = 0.0;
        self.box_metrics = LineBoxMetrics::default();
        self.has_content = false;
    }
}

/// The metrics of a line box.
///
/// Note a line box is distinct from an *inline* box. A line box is a single line within the layout,
/// and can hold multiple text runs and inline boxes.
///
/// Following CSS 2.2 § 10.8 (line height calculations in "Visual formatting model details"), line
/// boxes are sized to fit the line's inline content. Inline content is first aligned to each other
/// (we currently only align content by their baselines). We model this as the inline content being
/// aligned to the line box's own "baseline," and carry the line box's height over and under that
/// baseline. See <https://www.w3.org/TR/CSS22/visudet.html#line-height>.
#[derive(Clone, Copy, Debug, Default)]
struct LineBoxMetrics {
    /// The extents from the line box's baseline.
    ///
    /// The extents are in block flow direction; i.e., for horizontal text, these are vertical, and
    /// for vertical text, these are horizontal.
    line_box: Extents,
    /// The content extents from the line box's baseline.
    ///
    /// This covers, roughly, the glyphs and inline boxes. This does not take into account
    /// typographic leading, but only the typographic ascent and descent. In case of negative
    /// leading, this can be larger than [`Self::line_box`].
    ///
    /// Like [`Self::line_box`], these are in block flow direction.
    content_box: Extents,
}

#[derive(Clone, Copy, Debug)]
struct Extents {
    /// The space over the line box's baseline.
    over: f32,
    /// The space under the line box's baseline.
    under: f32,
}

impl Default for Extents {
    fn default() -> Self {
        Self {
            // NOTE: these should be `f32::NEG_INFINITY`, but that's currently causing
            // `tests::test_builders::builders_empty` to fail with a `NaN`.
            //
            // And even more ideally, a line's initial extents should be sourced from the primary
            // font.
            over: 0.,
            under: 0.,
        }
    }
}

impl LineBoxMetrics {
    /// The line height seen so far.
    #[inline(always)]
    fn line_height(self) -> f32 {
        self.line_box.over + self.line_box.under
    }

    fn add_text(&mut self, metrics: &FontMetrics, line_height: f32, quantize: bool) {
        // TODO: perhaps precompute these run metrics and store in `RunMetrics`.
        let (ascent, descent) = if quantize {
            (metrics.ascent.round(), metrics.descent.round())
        } else {
            (metrics.ascent, metrics.descent)
        };
        let half_leading = (line_height - (ascent + descent)) / 2.;
        let over = if quantize {
            ascent + half_leading.floor()
        } else {
            ascent + half_leading
        };
        // Note the `under` part is *not* quantized. This is such that the exact line height is
        // reached. For determining the line box block, add this to the baseline and then quantize
        // by rounding.
        let under = line_height - over;

        self.line_box.over = self.line_box.over.max(over);
        self.line_box.under = self.line_box.under.max(under);
        self.content_box.over = self.content_box.over.max(ascent);
        self.content_box.under = self.content_box.under.max(descent);
    }

    fn add_inline_box(&mut self, ascent: f32, descent: f32, quantize: bool) {
        if quantize {
            self.line_box.over = self.line_box.over.max(ascent.round());
            self.line_box.under = self.line_box.under.max(descent.round());
            self.content_box.over = self.content_box.over.max(ascent.round());
            self.content_box.under = self.content_box.under.max(descent.round());
        } else {
            self.line_box.over = self.line_box.over.max(ascent);
            self.line_box.under = self.line_box.under.max(descent);
            self.content_box.over = self.content_box.over.max(ascent);
            self.content_box.under = self.content_box.under.max(descent);
        }
    }
}

#[derive(Clone, Default)]
struct PrevBoundaryState {
    item_idx: usize,
    run_idx: usize,
    cluster_idx: u32,
    /// The boundary follows a preserved white-space atom in `break-spaces`.
    after_break_spaces_space: bool,
    state: LineState,
}

impl PrevBoundaryState {
    fn has_content(&self) -> bool {
        self.state.has_content
    }
}

/// Reason that the line breaker has yielded control flow
#[derive(Clone, Debug)]
pub enum YieldData {
    /// Control flow was yielded because a line break occurred.
    /// The `reason` field of the [`LineBreakData`] contains a specific reason about what caused a
    /// line break at this location.
    LineBreak(LineBreakData),
    /// Control flow was yielded because content on the line caused the line to exceed the max height
    ///
    /// The caller is responsible for finding a new location for the line with a greater available height
    /// adjusting the line geometry to the new position and resuming iteration.
    ///
    /// Note: that by default no max height is set (and one is not required for laying out text into
    /// rectangular regions), so you will only encounter this if you explicitly set a max height
    /// using `BreakLine::set_line_max_height`.
    MaxHeightExceeded(MaxHeightBreakData),
    /// Control flow was yielded because an inline box with kind [`InlineBoxKind::CustomOutOfFlow`]
    /// was encountered.
    ///
    /// Parley does not position these boxes itself. The caller is responsible for
    /// placing the box (e.g. via a caller-owned algorithm), adjusting the line geometry through
    /// [`BreakerState`], and then resuming iteration.
    InlineBoxBreak(BoxBreakData),
}

#[derive(Clone, Debug)]
/// Information about a line break
pub struct LineBreakData {
    /// The reason for the line break (see [`BreakReason`] for details)
    pub reason: BreakReason,
    /// The computed advance (width) of the line
    pub advance: f32,
    /// The computed height of the line
    pub line_height: f32,
    /// The position of the top of the line
    pub line_y_start: f64,
    /// The position of the bottom of the line
    pub line_y_end: f64,
}

#[derive(Clone, Debug)]
/// Information about a "max height break" (where control flow has been yielded due to the
/// line's configured max height being exceeded by content by laid out into the line).
pub struct MaxHeightBreakData {
    /// The current advance of the in-progress line
    pub advance: f32,
    /// The current line height of the in-progress line
    pub line_height: f32,
}

#[derive(Clone, Debug)]
/// Information about a "box break" (where control flow has been yielded due to an inline box
/// with kind [`InlineBoxKind::CustomOutOfFlow`] being encountered during layout.
pub struct BoxBreakData {
    /// The user-supplied ID for the inline box
    pub inline_box_id: u64,
    /// The index of the inline box within `Layout::inline_boxes()`
    pub inline_box_index: usize,
    /// The current advance of the line (up to but *not* including the `CustomOutOfFlow` box)
    pub advance: f32,
}

#[derive(Clone)]
/// The mutable state of the line breaker.
///
/// This is exposed so that callers using [`BreakLines`] directly can inspect and
/// adjust line geometry between calls to [`BreakLines::break_next`].
///
/// A `BreakerState` can be cloned and later passed to [`BreakLines::revert_to`]
/// to retry layout from a saved checkpoint.
pub struct BreakerState {
    /// The number of items that have been processed (used to revert state)
    items: usize,
    /// The number of lines that have been processed (used to revert state)
    lines: usize,

    /// Iteration state: the current item (within the layout)
    item_idx: usize,
    /// Iteration state: the current run (within the layout)
    run_idx: usize,
    /// Iteration state: the current shaped cluster.
    ///
    /// This indexes into [`parley_engine::ShapedText::shaped_clusters`]. It's atom-aligned, pointing
    /// at the start of the next atom to consume.
    //
    // TODO: rename this `shaped_cluster_idx`
    cluster_idx: u32,

    /// The x coordinate of the left/start of the current line
    line_x: f32,
    /// The y coordinate of the top/start of the current line
    /// Use of f64 here is important. f32 causes test failures due to accumulated error
    line_y: f64,

    /// The max advance of the entire layout.
    layout_max_advance: f32,
    /// The max advance (max width) of the current line. This must be <= the `layout_max_advance`.
    line_max_advance: f32,
    /// The max height available to the current line.
    line_max_height: f32,

    /// The state of the current line
    line: LineState,

    /// Whether the most recently inspected atom was preserved white space in
    /// `break-spaces`; used to distinguish the opportunities that mode adds.
    prev_atom_was_break_spaces_space: bool,

    // Saved breaker states for reverting to a previously encountered line-breaking opportunity
    /// Saved breaker state for the last non-emergency line-breaking opportunity
    prev_boundary: Option<PrevBoundaryState>,
    /// Saved breaker state for the last emergency line-breaking opportunity
    emergency_boundary: Option<PrevBoundaryState>,
    /// State before trailing inline-start edges. A break before the first
    /// enclosed content must also move those edges to the next line.
    pending_inline_start: Option<PrevBoundaryState>,
    /// Wrapping mode of the most recently consumed text or atomic content.
    /// Inline edges do not replace this state.
    last_content_text_wrap_mode: Option<TextWrapMode>,
    /// Whether the most recently consumed content established a break after
    /// itself. Inline-end edges move that opportunity to their far side.
    propagating_break_after: bool,
}

impl Default for BreakerState {
    fn default() -> Self {
        Self {
            items: 0,
            lines: 0,
            item_idx: 0,
            run_idx: 0,
            cluster_idx: 0,
            line_x: 0.0,
            line_y: 0.0,
            layout_max_advance: 0.0,
            line_max_advance: 0.0,
            line_max_height: f32::MAX,
            line: LineState::default(),
            prev_atom_was_break_spaces_space: false,
            prev_boundary: None,
            emergency_boundary: None,
            pending_inline_start: None,
            last_content_text_wrap_mode: None,
            propagating_break_after: false,
        }
    }
}

impl BreakerState {
    /// Add the atom currently being evaluated to the current line.
    ///
    /// `font_metrics` provides the raw font ascent and descent of the atom (i.e. the distances
    /// it extends above and below the baseline, *not* including leading) as well as the intrinsic
    /// line height of the atom (i.e. including the full leading), which may be smaller than
    /// `ascent + descent` when the leading is negative.
    #[inline]
    fn append_atom_to_line(
        &mut self,
        atom: &Atom<'_>,
        next_x: f32,
        font_metrics: &FontMetrics,
        line_height: f32,
        quantize: bool,
    ) {
        self.line.items.end = self.item_idx + 1;
        self.line.clusters.end = atom.shaped_clusters_range().end;
        self.cluster_idx = atom.shaped_clusters_range().end;
        self.line.x = next_x;
        self.line
            .box_metrics
            .add_text(font_metrics, line_height, quantize);
        self.update_max_height_exceeded();
    }

    /// Add an inline box to the line.
    ///
    /// `ascent` and `descent` are the distances the box extends above and below the text baseline
    /// respectively. A box with its bottom aligned to the baseline is simply one with a zero
    /// descent. The box grows the line only insofar as it extends beyond the text.
    pub fn append_inline_box_to_line(
        &mut self,
        next_x: f32,
        ascent: f32,
        descent: f32,
        quantize: bool,
    ) {
        self.item_idx += 1;
        self.line.items.end += 1;
        self.line.x = next_x;
        self.line
            .box_metrics
            .add_inline_box(ascent, descent, quantize);
        self.update_max_height_exceeded();
    }

    /// Store the current iteration state so that we can revert to it if we later want to take
    /// the line breaking opportunity at this point.
    fn mark_line_break_opportunity(&mut self, after_break_spaces_space: bool) {
        let mut boundary = self.boundary_state();
        boundary.after_break_spaces_space = after_break_spaces_space;
        self.prev_boundary = Some(boundary);
        self.propagating_break_after = true;
    }

    fn mark_line_break_opportunity_before_content(&mut self, after_break_spaces_space: bool) {
        let mut boundary = self.break_before_content();
        boundary.after_break_spaces_space = after_break_spaces_space;
        if boundary.has_content() {
            self.prev_boundary = Some(boundary);
        }
        self.propagating_break_after = false;
    }

    fn boundary_state(&self) -> PrevBoundaryState {
        PrevBoundaryState {
            item_idx: self.item_idx,
            run_idx: self.run_idx,
            cluster_idx: self.cluster_idx,
            after_break_spaces_space: false,
            state: self.line.clone(),
        }
    }

    fn begin_inline_start(&mut self) {
        if self.pending_inline_start.is_none() {
            self.pending_inline_start = Some(self.boundary_state());
        }
        self.propagating_break_after = false;
    }

    fn break_before_content(&mut self) -> PrevBoundaryState {
        if let Some(mut boundary) = self.pending_inline_start.take() {
            // Structural start edges move with the first real content. White
            // space consumed after the edge may hang or be removed, so resume
            // the text stream after it while replaying the edge itself.
            boundary.cluster_idx = self.cluster_idx;
            boundary
        } else {
            self.boundary_state()
        }
    }

    fn finish_content(&mut self, text_wrap_mode: TextWrapMode) {
        self.pending_inline_start = None;
        self.last_content_text_wrap_mode = Some(text_wrap_mode);
        self.propagating_break_after = false;
        self.line.has_content = true;
    }

    /// Records hanging/removable white space without letting it detach a
    /// pending inline-start edge from the first non-white-space content.
    fn finish_hanging_whitespace(&mut self, text_wrap_mode: TextWrapMode) {
        self.last_content_text_wrap_mode = Some(text_wrap_mode);
        self.propagating_break_after = false;
        self.line.has_content = true;
    }

    fn propagate_break_after_inline_end(&mut self) {
        self.pending_inline_start = None;
        if self.propagating_break_after {
            let after_break_spaces_space = self
                .prev_boundary
                .as_ref()
                .is_some_and(|boundary| boundary.after_break_spaces_space);
            let mut boundary = self.boundary_state();
            boundary.after_break_spaces_space = after_break_spaces_space;
            self.prev_boundary = Some(boundary);
        }
    }

    fn mark_emergency_break_opportunity_before_content(&mut self) {
        let boundary = self.break_before_content();
        if boundary.has_content() {
            self.emergency_boundary = Some(boundary);
        }
        self.propagating_break_after = false;
    }

    /// Revert boundary state to prev state
    fn reset_to(&mut self, prev_state: PrevBoundaryState) {
        self.item_idx = prev_state.item_idx;
        self.run_idx = prev_state.run_idx;
        self.cluster_idx = prev_state.cluster_idx;
        self.prev_atom_was_break_spaces_space = prev_state.after_break_spaces_space;
        self.line = prev_state.state;
    }

    #[inline(always)]
    fn update_max_height_exceeded(&mut self) {
        self.line.max_height_exceeded = self.line.box_metrics.line_height() > self.line_max_height;
    }

    /// Get the max-advance of the entire layout
    #[inline(always)]
    pub fn layout_max_advance(&self) -> f32 {
        self.layout_max_advance
    }
    /// Set the max-advance of the entire layout
    #[inline(always)]
    pub fn set_layout_max_advance(&mut self, advance: f32) {
        self.layout_max_advance = advance;
    }

    /// Get the max-advance of the current line
    #[inline(always)]
    pub fn line_max_advance(&self) -> f32 {
        self.line_max_advance
    }
    /// Set the max-advance of the current line
    #[inline(always)]
    pub fn set_line_max_advance(&mut self, advance: f32) {
        self.line_max_advance = advance;
    }

    /// Get the max-height of the current line
    #[inline(always)]
    pub fn line_max_height(&self) -> f32 {
        self.line_max_height
    }
    /// Set the max-height of the current line.
    #[inline(always)]
    pub fn set_line_max_height(&mut self, height: f32) {
        self.line_max_height = height;
    }

    /// Get the x-offset of the current line
    #[inline(always)]
    pub fn line_x(&self) -> f32 {
        self.line_x
    }
    /// Set the x-offset for the current line.
    #[inline(always)]
    pub fn set_line_x(&mut self, x: f32) {
        self.line_x = x;
    }

    /// Get the y-offset of the current line
    #[inline(always)]
    pub fn line_y(&self) -> f64 {
        self.line_y
    }
    /// Set the y-offset for the current line.
    #[inline(always)]
    pub fn set_line_y(&mut self, y: f64) {
        self.line_y = y;
    }
}

/// Line breaking support for a paragraph.
pub struct BreakLines<'a, B: Brush> {
    layout: &'a mut Layout<B>,
    lines: LineLayout,
    state: BreakerState,
    prev_state: Option<BreakerState>,
    done: bool,
}

impl<'a, B: Brush> BreakLines<'a, B> {
    pub(crate) fn new(layout: &'a mut Layout<B>) -> Self {
        layout.data.width = 0.;
        layout.data.height = 0.;
        let mut lines = LineLayout::default();
        lines.swap(&mut layout.data);
        lines.lines.clear();
        lines.line_items.clear();
        let mut state = BreakerState::default();
        if !layout.data.items.is_empty() {
            state.line.text_wrap_mode =
                layout.data.styles[usize::from(layout.data.root_style_index)].text_wrap_mode;
        }
        Self {
            layout,
            lines,
            state,
            prev_state: None,
            done: false,
        }
    }

    /// Reset state when a line has been committed
    fn start_new_line(
        &mut self,
        reason: BreakReason,
        max_advance: f32,
        line_indent: f32,
    ) -> Option<YieldData> {
        commit_line(
            self.layout,
            &mut self.lines,
            &mut self.state.line,
            max_advance,
            reason,
            line_indent,
        );

        let line_height = self.state.line.box_metrics.line_height();
        let line_y_start = self.state.line_y;

        self.state.items = self.lines.line_items.len();
        self.state.lines = self.lines.lines.len();
        // A line can replay structural start edges from an earlier item while
        // resuming text after discarded phase-II white space. The new line's
        // text range must start at that resumed cursor.
        self.state.line.clusters.start = self.state.cluster_idx;
        self.state.line.clusters.end = self.state.cluster_idx;
        self.state.prev_boundary = None;
        self.state.emergency_boundary = None;
        self.state.pending_inline_start = None;
        self.state.last_content_text_wrap_mode = None;
        self.state.propagating_break_after = false;
        self.state.prev_atom_was_break_spaces_space = false;

        // `finish_line` reads the line's accumulated vertical metrics from `self.state.line`, so
        // it must run before we reset the per-line running state.
        self.finish_line(self.lines.lines.len() - 1, line_height);
        self.state.line.reset();

        self.state.line_y += line_height as f64;

        Some(YieldData::LineBreak(
            self.last_line_data(reason, line_y_start),
        ))
    }

    #[inline(always)]
    fn last_line_data(&self, reason: BreakReason, line_y_start: f64) -> LineBreakData {
        let line = self.lines.lines.last().unwrap();
        LineBreakData {
            reason,
            advance: line.metrics.advance,
            line_height: line.size(),
            line_y_start,
            line_y_end: self.state.line_y,
        }
    }

    #[inline(always)]
    fn max_height_break_data(&self, line_height: f32) -> Option<YieldData> {
        Some(YieldData::MaxHeightExceeded(MaxHeightBreakData {
            advance: self.state.line.x,
            line_height,
        }))
    }

    #[inline(always)]
    pub fn state(&self) -> &BreakerState {
        &self.state
    }

    #[inline(always)]
    pub fn state_mut(&mut self) -> &mut BreakerState {
        &mut self.state
    }

    /// Set the max-advance of the previous line.
    ///
    /// This is an escape-hatch for allowing a custom width for
    /// alignment on each line, which is different to the breaking width.
    ///
    /// Should be used in combination with [`AlignmentOptions`](crate::AlignmentOptions::align_when_overflowing).
    ///
    /// This method changes a line's reported [`inline_max_coord`](LineMetrics::inline_max_coord), so
    /// if you use this method and read that value, you should be cautious.
    // This escape hatch has not been carefully evaluated for unexpected consequences.
    // It's current motivation is cases where Parley gives different line breaking results
    // than blink, for reasons which haven't been fully understood.
    #[doc(hidden)]
    pub fn set_prior_line_width(&mut self, advance: f32) {
        if let Some(line) = self.lines.lines.last_mut() {
            line.metrics.inline_max_coord = line.metrics.inline_min_coord + advance;
        }
    }

    /// Reverts the to an externally saved state.
    pub fn revert_to(&mut self, state: BreakerState) {
        self.state = state;
        self.lines.lines.truncate(self.state.lines);
        self.lines.line_items.truncate(self.state.items);
        self.done = false;
    }

    /// Reverts the last computed line, returning to the previous state.
    #[inline(always)]
    pub fn revert(&mut self) -> bool {
        if let Some(state) = self.prev_state.take() {
            self.revert_to(state);
            true
        } else {
            false
        }
    }

    /// Returns the y-coordinate of the top of the current line
    #[inline(always)]
    pub fn committed_y(&self) -> f64 {
        self.state.line_y
    }

    /// Returns true if all the text has been placed into lines.
    #[inline(always)]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Computes the next line in the paragraph. Returns the advance and size
    /// (width and height for horizontal layouts) of the line.
    #[inline(always)]
    pub fn break_next(&mut self) -> Option<YieldData> {
        self.break_next_line_or_box()
    }

    /// Computes the next line in the paragraph. Returns the advance and size
    /// (width and height for horizontal layouts) of the line.
    fn break_next_line_or_box(&mut self) -> Option<YieldData> {
        assert!(
            self.state.layout_max_advance == f32::INFINITY
                || self.state.line_max_advance - self.state.layout_max_advance < 1.0
        );

        // Maintain iterator state
        if self.done {
            return None;
        }
        self.prev_state = Some(self.state.clone());

        // HACK: ignore max_advance for empty layouts
        // Prevents crash when width is too small (https://github.com/linebender/parley/issues/186)
        let max_advance =
            if self.layout.data.text_len == 0 && self.layout.data.inline_boxes.is_empty() {
                f32::MAX
            } else {
                self.state.line_max_advance
            };

        let line_indent = self.resolve_indent();

        let max_advance = max_advance - line_indent;

        // dbg!(&self.layout.items);

        // println!("\nBREAK NEXT");
        // dbg!(&self.state.line.items);

        // Iterate over remaining runs in the Layout
        let item_count = self.layout.data.items.len();
        while self.state.item_idx < item_count {
            let item = &self.layout.data.items[self.state.item_idx];

            // println!(
            //     "\nitem = {} {:?}. x: {}",
            //     self.state.item_idx, item.kind, self.state.line.x
            // );
            // dbg!(&self.state.line.items);

            match item.kind {
                LayoutItemKind::InlineBox => {
                    let inline_box = &self.layout.data.inline_boxes[item.index];
                    let style_after = item.style_after.map(|style_index| {
                        self.layout.data.styles[usize::from(style_index)].text_wrap_mode
                    });

                    match inline_box.kind {
                        InlineBoxKind::CustomOutOfFlow => {
                            return Some(YieldData::InlineBoxBreak(BoxBreakData {
                                inline_box_id: inline_box.id,
                                inline_box_index: item.index,
                                advance: self.state.line.x,
                            }));
                        }
                        InlineBoxKind::OutOfFlow => {
                            self.state.append_inline_box_to_line(
                                self.state.line.x,
                                0.0,
                                0.0,
                                self.layout.data.quantize,
                            );
                            if let Some(mode) = style_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                        }
                        InlineBoxKind::InlineStart | InlineBoxKind::InlineEnd => {
                            if inline_box.kind == InlineBoxKind::InlineStart {
                                self.state.begin_inline_start();
                            }
                            if inline_box.height > self.state.line_max_height {
                                return self.max_height_break_data(inline_box.height);
                            }
                            let baseline = inline_box.baseline.unwrap_or(inline_box.height);
                            self.state.append_inline_box_to_line(
                                self.state.line.x + inline_box.width,
                                baseline,
                                inline_box.height - baseline,
                                self.layout.data.quantize,
                            );
                            if let Some(mode) = style_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                            if inline_box.kind == InlineBoxKind::InlineEnd {
                                self.state.propagate_break_after_inline_end();
                            }
                        }
                        InlineBoxKind::InFlow => {
                            let text_wrap_mode = self.state.line.text_wrap_mode;
                            // An atomic inline is breakable when either adjacent
                            // inline style permits wrapping. A pending start edge
                            // is part of the atomic fragment and moves with it.
                            let can_break_before = text_wrap_mode == TextWrapMode::Wrap
                                || self.state.last_content_text_wrap_mode
                                    == Some(TextWrapMode::Wrap);
                            let break_before =
                                can_break_before.then(|| self.state.break_before_content());
                            let next_x = self.state.line.x + inline_box.width;
                            let breaks_before = next_x > max_advance
                                && break_before
                                    .as_ref()
                                    .is_some_and(PrevBoundaryState::has_content);

                            if inline_box.height > self.state.line_max_height && !breaks_before {
                                return self.max_height_break_data(inline_box.height);
                            }
                            if breaks_before {
                                self.state
                                    .reset_to(break_before.expect("checked break boundary"));
                                return self.start_new_line(
                                    BreakReason::Regular,
                                    max_advance,
                                    line_indent,
                                );
                            }

                            // An oversized first fragment has no usable break
                            // before it, so consume it as one overflowing unit.
                            let baseline = inline_box.baseline.unwrap_or(inline_box.height);
                            self.state.append_inline_box_to_line(
                                next_x,
                                baseline,
                                inline_box.height - baseline,
                                self.layout.data.quantize,
                            );
                            if let Some(mode) = style_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                            self.state.finish_content(text_wrap_mode);
                            self.state.prev_atom_was_break_spaces_space = false;
                            if text_wrap_mode == TextWrapMode::Wrap {
                                self.state.mark_line_break_opportunity(false);
                            }
                        }
                    }
                }
                LayoutItemKind::TextRun => {
                    let run_idx = item.index;
                    let shaped_run = &self.layout.data.shaped_text.runs()[run_idx];

                    let run = Run::new(self.layout, 0, 0, run_idx, None);
                    let slice = run.full_slice();
                    let cluster_end = shaped_run.shaped_clusters_range.end;

                    // println!("TextRun ({:?})", &run_data.text_range);

                    // Iterate over the remaining atoms in the Run
                    for atom in slice.atoms_from(self.state.cluster_idx) {
                        // Retrieve metadata about the atom
                        let first_character = &atom.characters()[0];
                        let whitespace = first_character.info.whitespace();
                        let is_newline = whitespace == Whitespace::Newline;
                        let boundary = first_character.info.boundary();
                        let metrics = run.font_metrics();
                        let line_height = run.data.line_height;
                        let max_height_exceeded = self.state.line.max_height_exceeded;
                        let style = &self.layout.data.styles[first_character.style_index as usize];

                        let is_break_spaces =
                            style.white_space_collapse == WhiteSpaceCollapse::BreakSpaces;
                        let is_ideographic_space = first_character.info.source_char() == '\u{3000}';
                        let is_break_spaces_space = is_break_spaces
                            && (matches!(whitespace, Whitespace::Space | Whitespace::Tab)
                                || is_ideographic_space);
                        let hangs_or_is_removed =
                            (matches!(whitespace, Whitespace::Space | Whitespace::Tab)
                                || is_ideographic_space)
                                && style
                                    .white_space_collapse
                                    .end_of_line_whitespace(style.text_wrap_mode)
                                    != EndOfLineWhitespace::TakesUpSpace;
                        let prev_was_break_spaces_space = core::mem::replace(
                            &mut self.state.prev_atom_was_break_spaces_space,
                            is_break_spaces_space,
                        );

                        // Lag text_wrap_mode style by one atom
                        let text_wrap_mode = self.state.line.text_wrap_mode;
                        self.state.line.text_wrap_mode = style.text_wrap_mode;

                        if boundary == Boundary::Line && text_wrap_mode == TextWrapMode::Wrap {
                            // We don't record boundaries when the advance is 0. As we do not want overflowing content to cause extra consecutive
                            // line breaks. We should accept the overflowing fragment in that scenario.
                            if self.state.line.x != 0.0 {
                                let usable_by_break_spaces = prev_was_break_spaces_space
                                    || style.word_break == WordBreak::BreakAll;
                                self.state.mark_line_break_opportunity_before_content(
                                    usable_by_break_spaces,
                                );
                                // break_opportunity = true;
                            }
                        } else if is_newline {
                            if max_height_exceeded {
                                return self.max_height_break_data(line_height);
                            }

                            // A CRLF sequence is a single grapheme cluster and must produce
                            // exactly one hard line break (UAX#14: CR × LF, do not break
                            // between). Normally, this will be a single atom. However, if
                            // itemization splits the CR and LF into separate runs (e.g. a
                            // style boundary at the LF), the characters each form an atom of
                            // their own. In that case, append the CR to the current line but
                            // suppress the break here and let the LF emit the single break, so CR
                            // and LF share one line. The lookahead reads the global character list.
                            // The LF must be item-adjacent to the CR: if it lands in a later run it
                            // only coalesces when the next item is that run (not an inline box
                            // sitting between the two), so an inline box at the LF offset keeps the
                            // CR's break. Lone CR, lone LF, LS, and PS are unaffected.
                            let atom_chars = atom.char_range();
                            let lf_is_item_adjacent = atom.shaped_clusters_range().end
                                < cluster_end
                                || self
                                    .layout
                                    .data
                                    .items
                                    .get(self.state.item_idx + 1)
                                    .is_some_and(|item| item.kind == LayoutItemKind::TextRun);
                            let characters = self.layout.data.shaped_text.characters();
                            let is_cr_before_lf = characters[atom_chars.end as usize - 1]
                                .info
                                .source_char()
                                == '\r'
                                && lf_is_item_adjacent
                                && characters.get(atom_chars.end as usize).is_some_and(|next| {
                                    next.info.whitespace() == Whitespace::Newline
                                        && next.info.source_char() == '\n'
                                });

                            self.state.finish_content(style.text_wrap_mode);
                            self.state.append_atom_to_line(
                                &atom,
                                self.state.line.x,
                                metrics,
                                line_height,
                                self.layout.data.quantize,
                            );

                            if is_cr_before_lf {
                                continue;
                            }

                            return self.start_new_line(
                                BreakReason::Explicit,
                                max_advance,
                                line_indent,
                            );
                        } else if
                        // This text can contribute "emergency" line breaks.
                        style.overflow_wrap != OverflowWrap::Normal
                        && text_wrap_mode == TextWrapMode::Wrap
                        // If we're at the start of the line, this particular atom will never fit, so it's not a valid emergency break opportunity.
                        && self.state.line.x != 0.0
                        {
                            self.state.mark_emergency_break_opportunity_before_content();
                        }

                        // Breaking an atom requires reshaping, which we don't do here, so it is
                        // consumed as a whole (this includes all clusters of a ligature).
                        let advance = atom.advance();

                        // Compute the x position of the content being currently processed
                        let next_x = self.state.line.x + advance;

                        // println!("Cluster {} next_x: {}", self.state.cluster_idx, next_x);

                        // If the content fits (the x position does NOT exceed max_advance)
                        //
                        // We simply append the atom to the current line
                        if next_x <= max_advance {
                            if max_height_exceeded {
                                return self.max_height_break_data(line_height);
                            }
                            if hangs_or_is_removed {
                                self.state.finish_hanging_whitespace(style.text_wrap_mode);
                            } else {
                                self.state.finish_content(style.text_wrap_mode);
                            }
                            self.state.append_atom_to_line(
                                &atom,
                                next_x,
                                metrics,
                                line_height,
                                self.layout.data.quantize,
                            );
                            if is_break_spaces_space && style.text_wrap_mode == TextWrapMode::Wrap {
                                self.state.mark_line_break_opportunity(true);
                            }
                        }
                        // Else we attempt to line break:
                        //
                        // This will only succeed if there is an available line-break opportunity that has been marked earlier
                        // in the line. If there is no such line-breaking opportunity (such as if wrapping is disabled), then
                        // we fall back to appending the content to the line anyway.
                        else {
                            // Hanging or removable white space is not considered
                            // for fit. Keep consuming the run; the next actual
                            // opportunity, forced break, or end commits it.
                            if hangs_or_is_removed && text_wrap_mode == TextWrapMode::Wrap {
                                if max_height_exceeded {
                                    return self.max_height_break_data(line_height);
                                }
                                self.state.finish_hanging_whitespace(style.text_wrap_mode);
                                self.state.append_atom_to_line(
                                    &atom,
                                    next_x,
                                    metrics,
                                    line_height,
                                    self.layout.data.quantize,
                                );
                                continue;
                            }
                            // `break-spaces` adds an opportunity *after* each
                            // preserved space. It cannot use an unrelated
                            // regular opportunity before the overflowing space.
                            else if is_break_spaces_space && text_wrap_mode == TextWrapMode::Wrap
                            {
                                if let Some(prev) = self
                                    .state
                                    .prev_boundary
                                    .take_if(|prev| prev.after_break_spaces_space)
                                {
                                    self.state.reset_to(prev);
                                    return self.start_new_line(
                                        BreakReason::Regular,
                                        max_advance,
                                        line_indent,
                                    );
                                }
                                if let Some(prev_emergency) = self.state.emergency_boundary.take() {
                                    self.state.reset_to(prev_emergency);
                                    return self.start_new_line(
                                        BreakReason::Emergency,
                                        max_advance,
                                        line_indent,
                                    );
                                }
                                if max_height_exceeded {
                                    return self.max_height_break_data(line_height);
                                }
                                self.state.finish_content(style.text_wrap_mode);
                                self.state.append_atom_to_line(
                                    &atom,
                                    next_x,
                                    metrics,
                                    line_height,
                                    self.layout.data.quantize,
                                );
                                self.state.mark_line_break_opportunity(true);
                                continue;
                            }
                            // Case: we have previously encountered a REGULAR line-breaking opportunity in the current line
                            //
                            // We "take" the line-breaking opportunity by starting a new line and resetting our
                            // item/run/cluster iteration state back to how it was when the line-breaking opportunity was encountered
                            else if let Some(prev) = self.state.prev_boundary.take() {
                                self.state.reset_to(prev);
                                return self.start_new_line(
                                    BreakReason::Regular,
                                    max_advance,
                                    line_indent,
                                );
                            }
                            // Case: we have previously encountered an EMERGENCY line-breaking opportunity in the current line
                            //
                            // We "take" the line-breaking opportunity by starting a new line and resetting our
                            // item/run/cluster iteration state back to how it was when the line-breaking opportunity was encountered
                            else if let Some(prev_emergency) =
                                self.state.emergency_boundary.take()
                            {
                                self.state.reset_to(prev_emergency);
                                return self.start_new_line(
                                    BreakReason::Emergency,
                                    max_advance,
                                    line_indent,
                                );
                            }
                            // Case: no line-breaking opportunities available
                            //
                            // This can happen when wrapping is disabled (TextWrapMode::NoWrap) or when no wrapping opportunities
                            // (according to our `OverflowWrap` and `WordBreak` styles) have yet been encountered.
                            //
                            // We fall back to appending the content to the line.
                            else {
                                if max_height_exceeded {
                                    return self.max_height_break_data(line_height);
                                }
                                self.state.finish_content(style.text_wrap_mode);
                                self.state.append_atom_to_line(
                                    &atom,
                                    next_x,
                                    metrics,
                                    line_height,
                                    self.layout.data.quantize,
                                );
                            }
                        }
                    }
                    self.state.run_idx += 1;
                    self.state.item_idx += 1;
                }
            }
        }

        if self.state.line.items.end == 0 {
            self.state.line.items.end = 1;
        }
        self.done = true;
        self.start_new_line(BreakReason::None, max_advance, line_indent)
    }

    /// Computes the next line in the paragraph by character count.
    ///
    /// This method breaks lines based on the number of characters rather than advance width.
    /// Each character of text (including whitespace and newlines) counts as 1.
    /// Each atomic inline box also counts as 1 character. Structural inline
    /// start/end edges do not count independently.
    ///
    /// Unlike `break_next`, this method does not respect normal line break opportunities and
    /// will break when the character limit is reached. It does not break on newlines, for example.
    ///
    /// Breaks do fall on atom boundaries, however: when the limit is reached inside an atom (e.g. a
    /// ligature or a multi-character grapheme), the whole atom is placed on the line before
    /// breaking.
    pub fn break_next_with_length(&mut self, max_chars: u32) -> Option<()> {
        if self.done {
            return None;
        }

        let line_indent = self.resolve_indent();

        let mut char_count: u32 = 0;
        let char_limit = max_chars.max(1);
        let mut pending_break_reason = BreakReason::Regular;

        let item_count = self.layout.data.items.len();
        while self.state.item_idx < item_count {
            let item = &self.layout.data.items[self.state.item_idx];
            let can_follow_limited_content = match item.kind {
                LayoutItemKind::InlineBox => {
                    let kind = self.layout.data.inline_boxes[item.index].kind;
                    kind == InlineBoxKind::InlineEnd || !kind.contributes_advance()
                }
                LayoutItemKind::TextRun => false,
            };
            if char_count >= char_limit && !can_follow_limited_content {
                self.start_new_line(pending_break_reason, f32::MAX, line_indent);
                return Some(());
            }

            match item.kind {
                LayoutItemKind::InlineBox => {
                    let inline_box = &self.layout.data.inline_boxes[item.index];
                    let style_after = item.style_after.map(|style_index| {
                        self.layout.data.styles[usize::from(style_index)].text_wrap_mode
                    });

                    if !inline_box.kind.contributes_advance() {
                        self.state.append_inline_box_to_line(
                            self.state.line.x,
                            0.0,
                            0.0,
                            self.layout.data.quantize,
                        );
                        if let Some(mode) = style_after {
                            self.state.line.text_wrap_mode = mode;
                        }
                        continue;
                    }

                    let baseline = inline_box.baseline.unwrap_or(inline_box.height);
                    if inline_box.kind.is_inline_edge() {
                        if inline_box.kind == InlineBoxKind::InlineStart {
                            self.state.begin_inline_start();
                        }
                        self.state.append_inline_box_to_line(
                            self.state.line.x + inline_box.width,
                            baseline,
                            inline_box.height - baseline,
                            self.layout.data.quantize,
                        );
                        if let Some(mode) = style_after {
                            self.state.line.text_wrap_mode = mode;
                        }
                        if inline_box.kind == InlineBoxKind::InlineEnd {
                            self.state.propagate_break_after_inline_end();
                        }
                        continue;
                    }

                    let text_wrap_mode = self.state.line.text_wrap_mode;
                    let next_x = self.state.line.x + inline_box.width;
                    self.state.append_inline_box_to_line(
                        next_x,
                        baseline,
                        inline_box.height - baseline,
                        self.layout.data.quantize,
                    );
                    if let Some(mode) = style_after {
                        self.state.line.text_wrap_mode = mode;
                    }
                    self.state.finish_content(text_wrap_mode);
                    self.state.prev_atom_was_break_spaces_space = false;
                    char_count += 1;
                    pending_break_reason = BreakReason::Regular;
                }
                LayoutItemKind::TextRun => {
                    let run_idx = item.index;
                    let run = Run::new(self.layout, 0, 0, run_idx, None);
                    let slice = run.full_slice();

                    for atom in slice.atoms_from(self.state.cluster_idx) {
                        if char_count >= char_limit {
                            self.start_new_line(pending_break_reason, f32::MAX, line_indent);
                            return Some(());
                        }

                        let first_character = &atom.characters()[0];
                        let whitespace = first_character.info.whitespace();
                        let is_newline = whitespace == Whitespace::Newline;
                        let style = &self.layout.data.styles[first_character.style_index as usize];
                        let advance = atom.advance();

                        // Compute the x position.
                        // Newlines don't contribute to line width (matching break_next behavior).
                        let next_x = if is_newline {
                            self.state.line.x
                        } else {
                            self.state.line.x + advance
                        };
                        let metrics = run.font_metrics();
                        self.state.finish_content(style.text_wrap_mode);
                        self.state.append_atom_to_line(
                            &atom,
                            next_x,
                            metrics,
                            run.data.line_height,
                            self.layout.data.quantize,
                        );
                        self.state.prev_atom_was_break_spaces_space = style.white_space_collapse
                            == WhiteSpaceCollapse::BreakSpaces
                            && (matches!(whitespace, Whitespace::Space | Whitespace::Tab)
                                || first_character.info.source_char() == '\u{3000}');
                        char_count += atom.char_range().len() as u32;
                        pending_break_reason = if is_newline {
                            BreakReason::Explicit
                        } else {
                            BreakReason::Regular
                        };
                    }
                    self.state.run_idx += 1;
                    self.state.item_idx += 1;
                }
            }
        }

        // Commit the final line (only reached if content remains after all break_next_with_length calls)
        if self.state.line.items.end == 0 {
            self.state.line.items.end = 1;
        }
        self.done = true;
        self.start_new_line(BreakReason::None, f32::MAX, line_indent);
        Some(())
    }

    /// Breaks all remaining lines with the specified maximum advance. This
    /// consumes the line breaker.
    pub fn break_remaining(mut self, max_advance: f32) {
        // println!("\nDEBUG ITEMS");
        // for item in &self.layout.items {
        //     match item.kind {
        //         LayoutItemKind::InlineBox => println!("{:?}", item.kind),
        //         LayoutItemKind::TextRun => {
        //             let run_data = &self.layout.runs[item.index];
        //             println!("{:?} ({:?})", item.kind, &run_data.text_range);
        //         }
        //     }
        // }

        // println!("\nBREAK ALL");
        self.state.layout_max_advance = max_advance;
        self.state.line_max_advance = max_advance;
        while let Some(yield_data) = self.break_next() {
            // When `break_next` encounters a `CustomOutOfFlow`, it yields a `YieldData::InlineBoxBreak`
            // without advancing the item iteration past the box. This allows embedders to implement custom
            // placement logic to place the box. However, it means that we must also handle placing the box
            // and advancing the iteration here to avoid an infinite loop.
            //
            // So we place the box as a zero-sized out-of-flow box to guarantee progress.
            if let YieldData::InlineBoxBreak(_) = yield_data {
                self.state.append_inline_box_to_line(
                    self.state.line.x,
                    0.0,
                    0.0,
                    self.layout.data.quantize,
                );
            }
        }
        self.finish();
    }

    /// Consumes the line breaker and finalizes all line computations.
    pub fn finish(mut self) {
        if self.layout.data.text_len == 0
            && let Some(line) = self.lines.line_items.first_mut()
        {
            line.text_range = 0..0;
            line.shaped_cluster_range = 0..0;
            line.grapheme_range = 0..0;
        }
    }

    #[inline]
    fn resolve_indent(&self) -> f32 {
        let should_indent = {
            let is_scope_line = if self.layout.data.indent_options.each_line {
                self.lines.lines.is_empty()
                    || self.lines.lines.last().map(|l| l.break_reason)
                        == Some(BreakReason::Explicit)
            } else {
                self.lines.lines.is_empty()
            };
            is_scope_line ^ self.layout.data.indent_options.hanging
        };

        if should_indent {
            self.layout.data.indent_amount
        } else {
            0.0
        }
    }

    fn finish_line(&mut self, line_idx: usize, line_height: f32) {
        let prev_line_metrics = match line_idx {
            0 => None,
            idx => Some(self.lines.lines[idx - 1].metrics),
        };
        let line = &mut self.lines.lines[line_idx];

        // Reset metrics for line
        line.metrics.offset = 0.;
        line.text_range.start = usize::MAX;

        line.metrics.line_height = line_height;

        if line.item_range.is_empty() {
            line.text_range = self.layout.data.text_len..self.layout.data.text_len;
        }
        // Walk the line's items to compute text ranges, per-run advances and bidi ordering. The
        // vertical metrics (ascent/descent/line-height and inline box extents) are *not* computed
        // here: they were already accumulated into `self.state.line` as the line was built and are
        // read from there below. `have_metrics` records whether the line has any non-whitespace
        // content (text or an in-flow box), which distinguishes a genuinely empty line from a
        // whitespace-only one.
        let mut have_metrics = false;
        let mut needs_reorder = false;
        for line_item in self.lines.line_items[line.item_range.clone()]
            .iter_mut()
            .rev()
        {
            // Inline boxes carry bidi levels too. An object-only RTL line has
            // no text run to trigger reordering, but must still apply UAX #9
            // L2 to its object replacement characters.
            needs_reorder |= line_item.bidi_level != BidiLevel::new(0);
            match line_item.kind {
                LayoutItemKind::InlineBox => {
                    let item = &self.layout.data.inline_boxes[line_item.index];

                    // Advance is already computed in "commit line" for items
                    if item.kind.contributes_advance() {
                        // Mark us as having seen non-whitespace content on this line
                        have_metrics = true;
                    }
                }
                LayoutItemKind::TextRun => {
                    line_item.compute_ignorable_whitespace(&self.layout.data);

                    // Compute the text range for the line
                    // Q: Can we not simplify this computation by assuming that items are in order?
                    line.text_range.end = line.text_range.end.max(line_item.text_range.end);
                    line.text_range.start = line.text_range.start.min(line_item.text_range.start);

                    // Compute the run's advance by summing the advances of its constituent clusters
                    line_item.advance = {
                        let range = line_item.shaped_cluster_range.start as usize
                            ..line_item.shaped_cluster_range.end as usize;
                        self.layout.data.shaped_text.shaped_clusters()[range]
                            .iter()
                            .map(|c| c.advance)
                            .sum()
                    };

                    // Ignore trailing whitespace when deciding whether the line has content
                    // (we are iterating backwards so trailing whitespace comes first)
                    if !have_metrics && line_item.is_ignorable_whitespace {
                        continue;
                    }

                    // Mark us as having seen non-whitespace content on this line
                    have_metrics = true;
                }
            }
        }

        // UAX #9 rule L1 gives trailing segment/white-space characters the
        // paragraph embedding level. Split the logically-final run if only a
        // suffix needs the reset, then normal bidi reordering places the suffix
        // at the paragraph's visual end edge.
        let base_level = self.layout.data.base_level;
        let characters = self.layout.data.shaped_text.characters();
        let clusters = self.layout.data.shaped_text.shaped_clusters();
        let is_l1_whitespace = |cluster: &ShapedCluster| {
            let character = &characters[cluster.chars_range().start as usize];
            matches!(
                character.info.whitespace(),
                Whitespace::Space | Whitespace::Tab | Whitespace::Newline
            ) || character.info.source_char() == '\u{3000}'
        };
        let mut item_idx = line.item_range.end;
        while item_idx > line.item_range.start {
            item_idx -= 1;
            let item = &self.lines.line_items[item_idx];
            if !item.is_text_run() {
                let inline_box = &self.layout.data.inline_boxes[item.index];
                if inline_box.kind == InlineBoxKind::InFlow {
                    break;
                }
                continue;
            }
            let item_clusters = &clusters
                [item.shaped_cluster_range.start as usize..item.shaped_cluster_range.end as usize];
            let whitespace_count = item_clusters
                .iter()
                .rev()
                .take_while(|cluster| is_l1_whitespace(cluster))
                .count();
            if whitespace_count == 0 {
                break;
            }
            if item.bidi_level == base_level && whitespace_count == item_clusters.len() {
                continue;
            }
            needs_reorder = true;
            if whitespace_count == item_clusters.len() {
                self.lines.line_items[item_idx].bidi_level = base_level;
                continue;
            }

            let item = &self.lines.line_items[item_idx];
            let split_cluster = item.shaped_cluster_range.end - whitespace_count as u32;
            let run_slice = self.layout.data.shaped_text.run_slice(item.index as u32);
            let split_character = clusters[split_cluster as usize].chars_range().start;
            let split_text = run_slice.text_byte_at(split_character);
            let whitespace_advance: f32 = clusters
                [split_cluster as usize..item.shaped_cluster_range.end as usize]
                .iter()
                .map(|cluster| cluster.advance)
                .sum();
            let whitespace_graphemes =
                count_graphemes(run_slice.narrow(split_cluster..item.shaped_cluster_range.end));

            let mut whitespace_item = item.clone();
            whitespace_item.bidi_level = base_level;
            whitespace_item.advance = whitespace_advance;
            whitespace_item.shaped_cluster_range = split_cluster..item.shaped_cluster_range.end;
            whitespace_item.grapheme_range =
                item.grapheme_range.end - whitespace_graphemes..item.grapheme_range.end;
            whitespace_item.text_range = split_text..item.text_range.end;
            whitespace_item.compute_ignorable_whitespace(&self.layout.data);

            let item = &mut self.lines.line_items[item_idx];
            item.shaped_cluster_range.end = split_cluster;
            item.grapheme_range.end -= whitespace_graphemes;
            item.text_range.end = split_text;
            item.advance -= whitespace_advance;
            item.compute_ignorable_whitespace(&self.layout.data);
            self.lines.line_items.insert(item_idx + 1, whitespace_item);
            line.item_range.end += 1;
            break;
        }

        // Reorder the items within the line after the L1 reset.
        let item_count = line.item_range.end - line.item_range.start;
        if needs_reorder && item_count > 1 {
            reorder_line_items(&mut self.lines.line_items[line.item_range.clone()]);
        }

        // Count justification opportunities from the committed line rather
        // than maintaining a speculative count while the breaker backtracks.
        let characters = self.layout.data.shaped_text.characters();
        let clusters = self.layout.data.shaped_text.shaped_clusters();
        let total_justification_spaces = self.lines.line_items[line.item_range.clone()]
            .iter()
            .filter(|item| item.is_text_run())
            .map(|item| {
                clusters[item.shaped_cluster_range.start as usize
                    ..item.shaped_cluster_range.end as usize]
                    .iter()
                    .filter(|cluster| {
                        cluster.is_grapheme_start()
                            && characters[cluster.chars_range().start as usize]
                                .info
                                .whitespace()
                                .is_space_or_nbsp()
                    })
                    .count()
            })
            .sum::<usize>();

        // Walk inward from the paragraph's visual end edge. Structural inline
        // edges are transparent; an atomic in-flow box terminates the run.
        let is_rtl = self.layout.is_rtl();
        let mut removed_clusters = Vec::new();
        let (unconditional, conditional, trailing_justification_spaces) = {
            let styles = &self.layout.data.styles;
            let items = &self.lines.line_items[line.item_range.clone()];
            let mut unconditional = 0.0;
            let mut conditional = 0.0;
            let mut past_conditional_edge = false;
            let mut seen_hanging = false;
            let mut trailing_justification_spaces = 0;
            let mut items_forward;
            let mut items_reverse;
            let item_iter: &mut dyn Iterator<Item = (usize, &LineItemData)> = if is_rtl {
                items_forward = items.iter().enumerate();
                &mut items_forward
            } else {
                items_reverse = items.iter().enumerate().rev();
                &mut items_reverse
            };

            'items: for (item_offset, item) in item_iter {
                if !item.is_text_run() {
                    if self.layout.data.inline_boxes[item.index].kind == InlineBoxKind::InFlow {
                        break;
                    }
                    continue;
                }
                let mut clusters_forward;
                let mut clusters_reverse;
                let cluster_iter: &mut dyn Iterator<Item = u32> = if item.is_rtl() == is_rtl {
                    clusters_reverse = item.shaped_cluster_range.clone().rev();
                    &mut clusters_reverse
                } else {
                    clusters_forward = item.shaped_cluster_range.clone();
                    &mut clusters_forward
                };
                for cluster_idx in cluster_iter {
                    let cluster = &clusters[cluster_idx as usize];
                    let hang = trailing_whitespace_hang(cluster, characters, styles);
                    if !matches!(
                        hang,
                        TrailingWhitespaceHang::Skip | TrailingWhitespaceHang::End
                    ) && cluster.is_grapheme_start()
                        && characters[cluster.chars_range().start as usize]
                            .info
                            .whitespace()
                            .is_space_or_nbsp()
                    {
                        trailing_justification_spaces += 1;
                    }
                    match hang {
                        TrailingWhitespaceHang::Skip => {}
                        TrailingWhitespaceHang::End => break 'items,
                        TrailingWhitespaceHang::Removed if !seen_hanging => {
                            removed_clusters
                                .push((line.item_range.start + item_offset, cluster_idx));
                        }
                        TrailingWhitespaceHang::Conditional if !past_conditional_edge => {
                            seen_hanging = true;
                            conditional += cluster.advance;
                        }
                        TrailingWhitespaceHang::Conditional
                        | TrailingWhitespaceHang::Unconditional
                        | TrailingWhitespaceHang::Removed => {
                            seen_hanging = true;
                            past_conditional_edge = true;
                            unconditional += cluster.advance;
                        }
                    }
                }
            }
            (unconditional, conditional, trailing_justification_spaces)
        };

        // Removed characters keep their source mapping but have zero advance
        // only in this line. Do not mutate shared shaping data: the layout can
        // be rebroken at another width later.
        let mut removed_advance = 0.0;
        for (item_idx, cluster_idx) in removed_clusters {
            let cluster = &clusters[cluster_idx as usize];
            let item = &mut self.lines.line_items[item_idx];
            let slice = self.layout.data.shaped_text.run_slice(item.index as u32);
            let text_range = slice.text_byte_range(cluster.chars_range());
            if item.removed_text_range.is_empty() {
                item.removed_text_range = text_range;
            } else {
                item.removed_text_range.start = item.removed_text_range.start.min(text_range.start);
                item.removed_text_range.end = item.removed_text_range.end.max(text_range.end);
            }
            item.advance -= cluster.advance;
            removed_advance += cluster.advance;
        }

        let line = &mut self.lines.lines[line_idx];
        line.num_spaces = total_justification_spaces.saturating_sub(trailing_justification_spaces);
        line.metrics.advance -= removed_advance;
        let conditional_hang = match line.break_reason {
            BreakReason::Regular | BreakReason::Emergency => conditional,
            BreakReason::Explicit | BreakReason::None => {
                (line.metrics.advance - line.max_advance).clamp(0.0, conditional)
            }
        };
        line.metrics.trailing_whitespace = if conditional_hang >= conditional {
            conditional_hang + unconditional
        } else {
            conditional_hang
        };

        // Whether metrics should be quantized to pixel boundaries
        let quantize = self.layout.data.quantize;

        let mut line_box_extents = self.state.line.box_metrics.line_box;
        let mut content_box_extents = self.state.line.box_metrics.content_box;
        if !have_metrics
            && line.item_range.is_empty()
            && let Some(metrics) = prev_line_metrics
        {
            // HACK: copy metrics from previous line if we don't have
            // any; this should only occur for an empty line following
            // a newline at the end of a layout
            line.metrics = metrics;
            line_box_extents = Extents {
                over: metrics.baseline - metrics.block_min_coord,
                under: metrics.block_max_coord - metrics.baseline,
            };
            content_box_extents = Extents {
                over: metrics.baseline - metrics.content_block_min_coord,
                under: metrics.content_block_max_coord - metrics.baseline,
            };
            // If we have no items on this line, it must be the last (empty)
            // line in a layout following a newline. Commit an empty run so
            // that AccessKit has a node with which to identify the visual
            // cursor position
            if let Some((index, run)) = self
                .layout
                .data
                .shaped_text
                .runs()
                .iter()
                .enumerate()
                .rfind(|(_, run)| !run.range.byte_range.is_empty())
            {
                let run_index = self.lines.line_items.len();
                let cluster = run.shaped_clusters_range.end;
                let grapheme =
                    count_graphemes(self.layout.data.shaped_text.run_slice(index as u32));
                let text = run.range.byte_range.end;
                self.lines.line_items.push(LineItemData {
                    kind: LayoutItemKind::TextRun,
                    index,
                    bidi_level: BidiLevel::new(0),
                    advance: 0.,
                    is_ignorable_whitespace: false,
                    shaped_cluster_range: cluster..cluster,
                    grapheme_range: grapheme..grapheme,
                    text_range: text..text,
                    removed_text_range: 0..0,
                });
                line.item_range = run_index..run_index + 1;
            }
        }

        let top = if quantize {
            self.state.line_y.round() as f32
        } else {
            self.state.line_y as f32
        };
        line.metrics.baseline = top + line_box_extents.over;
        line.metrics.block_min_coord = top;
        line.metrics.block_max_coord = if quantize {
            // TODO: perhaps this should be something like the following, to ensure quantized line
            // boxes tile exactly without gaps or overlap. However, that would cause some
            // line height asserts to fail in the tests.
            // ```
            // (self.state.line_y as f32 + self.state.line.box_metrics.line_height()).round()
            // ```
            (line.metrics.baseline + line_box_extents.under).round()
        } else {
            line.metrics.baseline + line_box_extents.under
        };
        line.metrics.content_block_min_coord = line.metrics.baseline - content_box_extents.over;
        line.metrics.content_block_max_coord = line.metrics.baseline + content_box_extents.under;

        line.metrics.inline_min_coord = self.state.line_x;
        line.metrics.inline_max_coord = self.state.line_x + self.state.line_max_advance;
    }
}

impl<B: Brush> Drop for BreakLines<'_, B> {
    fn drop(&mut self) {
        // Compute the overall width and height of the entire layout
        // The "width" excludes trailing whitespace. The "full_width" includes it.
        let mut layout_width = 0_f32;
        let mut layout_full_width = 0_f32;
        let mut height = 0_f64; // f32 causes test failures due to accumulated error
        for line in &mut self.lines.lines {
            let indent_extra = line.indent.max(0.0);
            let line_max = line.metrics.inline_min_coord + line.metrics.advance + indent_extra;
            layout_full_width = layout_full_width.max(line_max);
            layout_width = layout_width.max(line_max - line.metrics.trailing_whitespace);
            height += line.metrics.line_height as f64;
        }

        // If laying out with infinite width constraint, then set all lines' "max_width"
        // to the measured width of the longest line.
        if self.state.layout_max_advance >= f32::MAX {
            for line in &mut self.lines.lines {
                if line.metrics.inline_max_coord >= f32::MAX {
                    line.metrics.inline_max_coord = layout_width;
                }
            }
        }

        // Don't include the last line's line_height in the layout's height if the last line is empty
        if let Some(last_line) = self.lines.lines.last()
            && last_line.item_range.is_empty()
        {
            height -= last_line.metrics.line_height as f64;
        }

        // Save the computed widths/height to the layout
        self.layout.data.width = layout_width;
        self.layout.data.full_width = layout_full_width;
        self.layout.data.height = height as f32;
        self.layout.data.layout_max_advance = self.state.layout_max_advance;

        // for (i, line) in self.lines.lines.iter().enumerate() {
        //     println!("LINE {i} (h:{})", line.metrics.line_height);
        //     for item_idx in line.item_range.clone() {
        //         let item = &self.lines.line_items[item_idx];
        //         println!("  ITEM {:?} ({})", item.kind, item.advance);
        //     }
        // }

        // Save the computed lines to the layout
        self.lines.swap(&mut self.layout.data);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrailingWhitespaceHang {
    /// Hangs regardless of the kind of line break.
    Unconditional,
    /// Removed when it is at the actual line end.
    Removed,
    /// Hangs fully at a soft wrap and only as needed at a forced/end break.
    Conditional,
    /// Zero-advance segment break; continue scanning inward.
    Skip,
    /// Visible content or white space that always takes up space.
    End,
}

fn trailing_whitespace_hang<B: Brush>(
    cluster: &ShapedCluster,
    characters: &[Character],
    styles: &[crate::layout::Style<B>],
) -> TrailingWhitespaceHang {
    let character = &characters[cluster.chars_range().start as usize];
    let style = &styles[character.style_index as usize];
    let end_of_line = style
        .white_space_collapse
        .end_of_line_whitespace(style.text_wrap_mode);
    match character.info.whitespace() {
        Whitespace::Newline => TrailingWhitespaceHang::Skip,
        Whitespace::NoBreakSpace => TrailingWhitespaceHang::End,
        Whitespace::Space | Whitespace::Tab => match end_of_line {
            EndOfLineWhitespace::TakesUpSpace => TrailingWhitespaceHang::End,
            EndOfLineWhitespace::Remove => TrailingWhitespaceHang::Removed,
            EndOfLineWhitespace::Hang => TrailingWhitespaceHang::Conditional,
        },
        Whitespace::None => {
            if character.info.source_char() == '\u{3000}'
                && end_of_line != EndOfLineWhitespace::TakesUpSpace
            {
                TrailingWhitespaceHang::Unconditional
            } else {
                TrailingWhitespaceHang::End
            }
        }
    }
}

#[expect(clippy::cast_possible_truncation, reason = "deferred")]
fn commit_line<B: Brush>(
    layout: &Layout<B>,
    lines: &mut LineLayout,
    state: &mut LineState,
    max_advance: f32,
    break_reason: BreakReason,
    line_indent: f32,
) -> bool {
    let shaped_text = &layout.data.shaped_text;
    let shaped_clusters = shaped_text.shaped_clusters();

    // Ensure that the cluster and item endpoints are within range
    state.clusters.end = state.clusters.end.min(shaped_clusters.len() as u32);
    state.items.end = state.items.end.min(layout.data.items.len());

    let start_item_idx = lines.line_items.len();
    // let start_run_idx = lines.line_items.last().map(|item| item.index).unwrap_or(0);

    let items_to_commit = &layout.data.items[state.items.clone()];

    // Compute first and last run index
    let is_text_run = |item: &LayoutItem| item.kind == LayoutItemKind::TextRun;
    let first_run_pos = items_to_commit.iter().position(is_text_run).unwrap_or(0);
    let last_run_pos = items_to_commit.iter().rposition(is_text_run).unwrap_or(0);

    // Iterate over the items to commit
    // println!("\nCOMMIT LINE");
    let mut last_item_kind = LayoutItemKind::TextRun;
    let mut committed_text_run = false;
    for (i, item) in items_to_commit.iter().enumerate() {
        // println!("i = {} index = {} {:?}", i, item.index, item.kind);

        match item.kind {
            LayoutItemKind::InlineBox => {
                let inline_box = &layout.data.inline_boxes[item.index];

                lines.line_items.push(LineItemData {
                    kind: LayoutItemKind::InlineBox,
                    index: item.index,
                    bidi_level: item.bidi_level,
                    advance: inline_box.width,

                    // These properties are ignored for inline boxes. So we just put a dummy value.
                    is_ignorable_whitespace: false,
                    shaped_cluster_range: 0..0,
                    grapheme_range: 0..0,
                    text_range: 0..0,
                    removed_text_range: 0..0,
                });

                last_item_kind = item.kind;
            }
            LayoutItemKind::TextRun => {
                let shaped_run = &shaped_text.runs()[item.index];

                // Compute cluster range
                // The first and last ranges have overrides to account for line-breaks within runs
                let mut cluster_range = shaped_run.shaped_clusters_range.clone();
                if i == first_run_pos {
                    cluster_range.start = state.clusters.start;
                }
                if i == last_run_pos {
                    cluster_range.end = state.clusters.end;
                }

                if cluster_range.start >= shaped_run.shaped_clusters_range.end {
                    // println!("INVALID CLUSTER");
                    // dbg!(&run_data.text_range);
                    // dbg!(cluster_range);
                    continue;
                }

                last_item_kind = item.kind;
                committed_text_run = true;

                // Map the cluster range to source-text and grapheme ranges. Line boundaries are
                // always aligned to `Atom`s, i.e., line bounds are always grapheme bounds.
                //
                // Because counting graphemes is `O(n)`, and for runs that are split across lines we
                // would recount the prefix every time, we first check whether this line continues
                // the run committed to the previous line. In that case, use its grapheme range end
                // as our start.
                //
                // Perhaps this can be improved...
                let slice = shaped_text.run_slice(item.index as u32);
                let grapheme_start =
                    lines
                        .line_items
                        .iter()
                        .rev()
                        .find(|prev| prev.is_text_run())
                        .filter(|prev| {
                            prev.index == item.index
                                && prev.shaped_cluster_range.end == cluster_range.start
                        })
                        .map(|prev| prev.grapheme_range.end)
                        .unwrap_or_else(|| {
                            count_graphemes(slice.narrow(
                                shaped_run.shaped_clusters_range.start..cluster_range.start,
                            ))
                        });
                let (text_range, grapheme_range) = if cluster_range.is_empty() {
                    let char_pos = shaped_clusters[cluster_range.start as usize]
                        .chars_range()
                        .start;
                    let text_pos = slice.text_byte_at(char_pos);
                    (text_pos..text_pos, grapheme_start..grapheme_start)
                } else {
                    let char_range = shaped_clusters[cluster_range.start as usize]
                        .chars_range()
                        .start
                        ..shaped_clusters[cluster_range.end as usize - 1]
                            .chars_range()
                            .end;
                    let text_range = slice.text_byte_range(char_range.clone());
                    let grapheme_len = count_graphemes(slice.narrow(cluster_range.clone()));
                    (text_range, grapheme_start..grapheme_start + grapheme_len)
                };

                lines.line_items.push(LineItemData {
                    kind: LayoutItemKind::TextRun,
                    index: item.index,
                    bidi_level: shaped_run.bidi_level,
                    advance: 0.,
                    is_ignorable_whitespace: false,
                    shaped_cluster_range: cluster_range,
                    grapheme_range,
                    text_range,
                    removed_text_range: 0..0,
                });
            }
        }
    }
    // let end_run_idx = lines.line_items.last().map(|item| item.index).unwrap_or(0);
    let end_item_idx = lines.line_items.len();

    lines.lines.push(LineData {
        item_range: start_item_idx..end_item_idx,
        max_advance,
        break_reason,
        // Computed from committed line content in `finish_line`.
        num_spaces: 0,
        indent: line_indent,
        metrics: LineMetrics {
            advance: state.x,
            ..Default::default()
        },
        ..Default::default()
    });

    // Reset state for the new line
    if committed_text_run {
        state.clusters.start = state.clusters.end;
    }

    state.items.start = match last_item_kind {
        // For text runs, the first item of line N+1 needs to be the SAME as
        // the last item for line N. This is because the item (if it a text run
        // may be split across the two lines with some clusters in line N and some
        // in line N+1). The item is later filtered out (see `continue` in loop above)
        // if there are not actually any clusters in line N+1.
        LayoutItemKind::TextRun => state.items.end.saturating_sub(1),
        // Inline boxes cannot be spread across multiple lines, so we should set
        // the first item of line N+1 to be the item AFTER the last item in line N.
        LayoutItemKind::InlineBox => state.items.end,
    };

    true
}

/// Reorder items within line according to the bidi levels of the items
fn reorder_line_items(runs: &mut [LineItemData]) {
    let run_count = runs.len();

    // Find the max level and the min *odd* level
    let mut max_level = 0;
    let mut lowest_odd_level = 255;
    for run in runs.iter() {
        let level = run.bidi_level;
        let is_odd = level.to_u8() & 1 != 0;

        // Update max level
        if level.to_u8() > max_level {
            max_level = level.to_u8();
        }

        // Update min odd level
        if is_odd && level.to_u8() < lowest_odd_level {
            lowest_odd_level = level.to_u8();
        }
    }

    // Iterate over bidi levels
    for level in (lowest_odd_level..=max_level).rev() {
        // Iterate over text runs
        let mut i = 0;
        while i < run_count {
            if runs[i].bidi_level.to_u8() >= level {
                let mut end = i + 1;
                while end < run_count && runs[end].bidi_level.to_u8() >= level {
                    end += 1;
                }

                let mut j = i;
                let mut k = end - 1;
                while j < k {
                    runs.swap(j, k);
                    j += 1;
                    k -= 1;
                }

                i = end;
            }
            i += 1;
        }
    }
}
