// Copyright 2021 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Greedy line breaking.

use alloc::vec::Vec;

#[cfg(feature = "libm")]
#[allow(unused_imports)]
use core_maths::CoreFloat;

use crate::analysis::Boundary;
use crate::analysis::cluster::Whitespace;
use crate::data::ClusterData;
use crate::layout::{
    BreakReason, Layout, LayoutData, LayoutItem, LayoutItemKind, LineData, LineItemData,
    LineMetrics, Run,
};
use crate::style::WordBreak;
use crate::style::{Brush, EndOfLineWhitespace, WhiteSpaceCollapse};
use crate::{InlineBoxKind, OverflowWrap, TextWrapMode};

use core::ops::Range;

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
    clusters: Range<usize>,
    /// Of the line currently being built, the maximum line height seen so far.
    /// This represents a lower-bound on the eventual line height of the line.
    running_line_height: f32,
    /// This is set to true if we encounter something on the line (either a glyph or an inline box)
    /// that is taller than the `line_max_height`. When in this state `break_next` should yield control
    /// flow to the caller to handle the constraint violation.
    ///
    /// This never happens when calling `break_all_lines` as it never sets `line_max_height`, and it defaults to `f32::MAX`.
    max_height_exceeded: bool,

    /// Whether this line has consumed text or an atomic inline box.
    ///
    /// Item ranges alone cannot answer this because a text run can be carried
    /// across a line boundary with an empty cluster range.
    has_content: bool,

    /// We lag the text-wrap-mode by one cluster due to line-breaking boundaries only
    /// being triggered on the cluster after the linebreak.
    text_wrap_mode: TextWrapMode,
}

#[derive(Clone, Default)]
struct PrevBoundaryState {
    item_idx: usize,
    run_idx: usize,
    cluster_idx: usize,
    /// Whether the cluster just before this boundary is a preserved white space character in
    /// `break-spaces` mode (see the `break-spaces` handling in `break_next`).
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
    /// Iteration state: the current cluster (within the layout)
    cluster_idx: usize,

    /// The x coordinate of the left/start of the current line
    line_x: f32,
    /// The y coordinate of the top/start of the current line
    /// Use of f64 here is important. f32 causes test failures due to accumulated error
    line_y: f64,

    /// The max advance of the entire layout.
    layout_max_advance: f32,
    /// Tolerance by which content may overflow the max advance and still be treated as fitting
    /// (see [`BreakerState::set_max_advance_fit_tolerance`]).
    max_advance_fit_tolerance: f32,
    /// The max advance (max width) of the current line. This must be <= the `layout_max_advance`.
    line_max_advance: f32,
    /// The max height available to the current line.
    line_max_height: f32,

    /// The state of the current line
    line: LineState,

    /// Whether the most recently processed cluster was a preserved white space character in
    /// `break-spaces` mode (used to classify soft-wrap opportunities; see `break_next`).
    prev_cluster_was_break_spaces_space: bool,

    // Saved breaker states for reverting to a previously encountered line-breaking opportunity
    /// Saved breaker state for the last non-emergency line-breaking opportunity
    prev_boundary: Option<PrevBoundaryState>,
    /// Saved breaker state for the last emergency line-breaking opportunity
    emergency_boundary: Option<PrevBoundaryState>,
    /// State before a trailing sequence of inline-start edges. A break before
    /// the first content in the inline must move before these edges so border
    /// and padding cannot be stranded on the preceding line.
    pending_inline_start: Option<PrevBoundaryState>,
    /// Wrapping mode of the most recently consumed text or atomic content.
    /// Inline edges do not replace this state: a boundary next to an atomic
    /// inline is breakable when either side permits wrapping.
    last_content_text_wrap_mode: Option<TextWrapMode>,
    /// Whether the most recently consumed content established a break
    /// opportunity immediately after itself. Inline-end edges propagate this
    /// opportunity to their far side.
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
            max_advance_fit_tolerance: 0.0,
            line_max_advance: 0.0,
            line_max_height: f32::MAX,
            line: LineState::default(),
            prev_cluster_was_break_spaces_space: false,
            prev_boundary: None,
            emergency_boundary: None,
            pending_inline_start: None,
            last_content_text_wrap_mode: None,
            propagating_break_after: false,
        }
    }
}

impl BreakerState {
    /// Add the cluster(s) currently being evaluated to the current line
    pub fn append_cluster_to_line(&mut self, next_x: f32, clusters_height: f32) {
        self.line.items.end = self.item_idx + 1;
        self.line.clusters.end = self.cluster_idx + 1;
        self.line.x = next_x;
        self.add_line_height(clusters_height);
        // Would like to add:
        // self.cluster_idx += 1;
    }

    /// Add inline box to line
    pub fn append_inline_box_to_line(&mut self, next_x: f32, box_height: f32) {
        // self.item_idx += 1;
        self.line.items.end += 1;
        self.line.x = next_x;
        self.add_line_height(box_height);
        // Would like to add:
        // self.item_idx += 1;
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
            // Hanging/removable white space consumed after this open edge is
            // discarded by CSS phase-II processing when the edge and the next
            // real content move to a new line. Re-enter at the open edge but
            // resume the text stream after that white-space run.
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

    /// Records hanging/removable white space without consuming the saved
    /// state before an inline-start edge. CSS open/close items are transparent
    /// to white-space processing, so a subsequent wrap must still be able to
    /// move the edge together with the first non-white-space content.
    fn finish_hanging_whitespace(&mut self, text_wrap_mode: TextWrapMode) {
        self.last_content_text_wrap_mode = Some(text_wrap_mode);
        self.propagating_break_after = false;
        self.line.has_content = true;
    }

    fn propagate_break_after_inline_end(&mut self) {
        self.pending_inline_start = None;
        if self.propagating_break_after {
            self.prev_boundary = Some(self.boundary_state());
        }
    }

    fn mark_emergency_break_opportunity_before_content(&mut self) {
        let boundary = self.break_before_content();
        if boundary.has_content() {
            self.emergency_boundary = Some(boundary);
        }
        self.propagating_break_after = false;
    }

    #[inline(always)]
    fn add_line_height(&mut self, height: f32) {
        self.line.running_line_height = self.line.running_line_height.max(height);
        self.line.max_height_exceeded = self.line.running_line_height > self.line_max_height;
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

    /// Get the max-advance fit tolerance
    #[inline(always)]
    pub fn max_advance_fit_tolerance(&self) -> f32 {
        self.max_advance_fit_tolerance
    }
    /// Set the tolerance by which content may overflow the max advance and still be treated as
    /// fitting when making line-breaking decisions (the default is zero).
    ///
    /// This compensates for floating-point rounding error in a max advance derived from font
    /// metrics (e.g. a CSS `ch`-based width that is intended to exactly fit a whole number of
    /// characters), so that an exactly-fitting line does not wrap. It only affects fit decisions:
    /// line metrics (such as the extent of hanging trailing white space) are still computed
    /// against the exact max advance.
    #[inline(always)]
    pub fn set_max_advance_fit_tolerance(&mut self, tolerance: f32) {
        self.max_advance_fit_tolerance = tolerance;
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
        state.line.text_wrap_mode = layout.data.initial_text_wrap_mode;
        Self {
            layout,
            lines,
            state,
            prev_state: None,
            done: false,
        }
    }

    /// Reset state when a line has been committed
    fn start_new_line(&mut self, reason: BreakReason) -> Option<YieldData> {
        let line_height = self.state.line.running_line_height;
        let line_y_start = self.state.line_y;

        self.state.items = self.lines.line_items.len();
        self.state.lines = self.lines.lines.len();
        self.state.line.x = 0.;
        // A break may resume after phase-II discarded white space while
        // restarting structural inline edges from an earlier item boundary.
        // The text range of the new line must always begin at the current
        // cluster cursor, not at the cluster endpoint of the committed line.
        self.state.line.clusters.start = self.state.cluster_idx;
        self.state.line.clusters.end = self.state.cluster_idx;
        self.state.line.running_line_height = 0.;
        self.state.line.has_content = false;
        self.state.prev_boundary = None;
        self.state.emergency_boundary = None;
        self.state.pending_inline_start = None;
        self.state.last_content_text_wrap_mode = None;
        self.state.propagating_break_after = false;

        self.finish_line(self.lines.lines.len() - 1, line_height);

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

        // This macro simply calls the `commit_line` with the provided arguments and some parts of self.
        // It exists solely to cut down on the boilerplate for accessing the self variables while
        // keeping the borrow checker happy
        macro_rules! try_commit_line {
            ($break_reason:expr) => {
                try_commit_line(
                    self.layout,
                    &mut self.lines,
                    &mut self.state.line,
                    max_advance,
                    $break_reason,
                    line_indent,
                )
            };
        }

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
                    let wrap_mode_after =
                        self.layout.data.inline_box_text_wrap_mode_after[item.index];

                    match inline_box.kind {
                        // If the box is a `CustomOutOfFlow` box then we yield control flow back to the caller.
                        // It is then the caller's responsibility to handle placement of the box.
                        InlineBoxKind::CustomOutOfFlow => {
                            self.state.item_idx += 1;
                            return Some(YieldData::InlineBoxBreak(BoxBreakData {
                                inline_box_id: inline_box.id,
                                inline_box_index: item.index,
                                advance: self.state.line.x,
                            }));
                        }
                        InlineBoxKind::OutOfFlow => {
                            self.state.item_idx += 1;
                            self.state.append_inline_box_to_line(self.state.line.x, 0.0);
                            if let Some(mode) = wrap_mode_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                        }
                        InlineBoxKind::InlineStart => {
                            self.state.begin_inline_start();
                            let next_x = self.state.line.x + inline_box.width;
                            if inline_box.height > self.state.line_max_height {
                                return self.max_height_break_data(inline_box.height);
                            }
                            self.state.item_idx += 1;
                            self.state
                                .append_inline_box_to_line(next_x, inline_box.height);
                            if let Some(mode) = wrap_mode_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                        }
                        InlineBoxKind::InlineEnd => {
                            let next_x = self.state.line.x + inline_box.width;
                            if inline_box.height > self.state.line_max_height {
                                return self.max_height_break_data(inline_box.height);
                            }
                            self.state.item_idx += 1;
                            self.state
                                .append_inline_box_to_line(next_x, inline_box.height);
                            if let Some(mode) = wrap_mode_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                            self.state.propagate_break_after_inline_end();
                        }
                        InlineBoxKind::InFlow => {
                            let text_wrap_mode = self.state.line.text_wrap_mode;
                            // Atomic inlines admit a boundary when either
                            // adjacent content style permits wrapping. Start
                            // edges remain attached to the atomic by restoring
                            // the state saved before the first such edge.
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
                                let boundary = break_before.expect("checked break boundary");
                                self.state.item_idx = boundary.item_idx;
                                self.state.run_idx = boundary.run_idx;
                                self.state.cluster_idx = boundary.cluster_idx;
                                self.state.line = boundary.state;
                                if try_commit_line!(BreakReason::Regular) {
                                    return self.start_new_line(BreakReason::Regular);
                                }
                            }

                            // No usable break precedes an oversized first
                            // fragment, so it must overflow as one unit.
                            self.state.item_idx += 1;
                            self.state
                                .append_inline_box_to_line(next_x, inline_box.height);
                            if let Some(mode) = wrap_mode_after {
                                self.state.line.text_wrap_mode = mode;
                            }
                            self.state.finish_content(text_wrap_mode);
                            if text_wrap_mode == TextWrapMode::Wrap {
                                self.state.mark_line_break_opportunity(false);
                            }
                        }
                    }
                }
                LayoutItemKind::TextRun => {
                    let run_idx = item.index;
                    let run_data = &self.layout.data.runs[run_idx];

                    let run = Run::new(self.layout, 0, 0, run_data, None);
                    let cluster_start = run_data.cluster_range.start;
                    let cluster_end = run_data.cluster_range.end;

                    // println!("TextRun ({:?})", &run_data.text_range);

                    // Iterate over remaining clusters in the Run
                    while self.state.cluster_idx < cluster_end {
                        let cluster = run.get(self.state.cluster_idx - cluster_start).unwrap();

                        // Retrieve metadata about the cluster
                        let is_ligature_continuation = cluster.is_ligature_continuation();
                        let whitespace = cluster.info().whitespace();
                        let is_newline = whitespace == Whitespace::Newline;
                        let boundary = cluster.info().boundary();
                        let line_height = run.metrics().line_height;
                        let max_height_exceeded = self.state.line.max_height_exceeded;
                        let style = &self.layout.data.styles[cluster.data.style_index as usize];

                        // In `break-spaces` mode, preserved white space gets a soft-wrap
                        // opportunity after each character and does not "hang" at the end of a
                        // line (see the handling below).
                        let is_break_spaces =
                            style.white_space_collapse == WhiteSpaceCollapse::BreakSpaces;

                        // An ideographic space (U+3000) is preserved even when white space is
                        // collapsed, but hangs unconditionally at the end of a line (except in
                        // `break-spaces` mode, where it takes up space and wraps like other
                        // preserved white space).
                        let is_ideographic_space = cluster.info().source_char() == '\u{3000}';

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
                        // Whether the *previous* cluster was a preserved white space character in
                        // `break-spaces` mode: a soft-wrap opportunity coinciding with the end of
                        // such a cluster is one "after a preserved space" (see the `break-spaces`
                        // overflow handling below).
                        let prev_was_break_spaces_space = core::mem::replace(
                            &mut self.state.prev_cluster_was_break_spaces_space,
                            is_break_spaces_space,
                        );

                        // Lag text_wrap_mode style by one cluster
                        let text_wrap_mode = self.state.line.text_wrap_mode;
                        self.state.line.text_wrap_mode = style.text_wrap_mode;

                        if boundary == Boundary::Line && text_wrap_mode == TextWrapMode::Wrap {
                            // We do not currently handle breaking within a ligature, so we ignore boundaries in such a position.
                            //
                            // We also don't record boundaries when the advance is 0. As we do not want overflowing content to cause extra consecutive
                            // line breaks. We should accept the overflowing fragment in that scenario.
                            if !is_ligature_continuation && self.state.line.x != 0.0 {
                                // With `word-break: break-all`, every opportunity is one an
                                // overflowing preserved space may wrap at (letters break freely,
                                // so wrapping here honors `word-break` rather than breaking
                                // before the space).
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
                            self.state.finish_content(style.text_wrap_mode);
                            self.state
                                .append_cluster_to_line(self.state.line.x, line_height);
                            if try_commit_line!(BreakReason::Explicit) {
                                // TODO: can this be hoisted out of the conditional?
                                self.state.cluster_idx += 1;
                                return self.start_new_line(BreakReason::Explicit);
                            }
                        } else if
                        // This text can contribute "emergency" line breaks.
                        style.overflow_wrap != OverflowWrap::Normal && !is_ligature_continuation
                        && text_wrap_mode == TextWrapMode::Wrap
                        // If we're at the start of the line, this particular cluster will never fit, so it's not a valid emergency break opportunity.
                        && self.state.line.x != 0.0
                        {
                            self.state.mark_emergency_break_opportunity_before_content();
                        }

                        // If current cluster is the start of a ligature, then advance state to include
                        // the remaining clusters that make up the ligature
                        let mut advance = cluster.advance();
                        if cluster.is_ligature_start() {
                            while let Some(cluster) = run.get(self.state.cluster_idx + 1) {
                                if !cluster.is_ligature_continuation() {
                                    break;
                                } else {
                                    advance += cluster.advance();
                                    self.state.cluster_idx += 1;
                                }
                            }
                        }

                        // Compute the x position of the content being currently processed
                        let next_x = self.state.line.x + advance;

                        // println!("Cluster {} next_x: {}", self.state.cluster_idx, next_x);

                        // If the content fits (the x position does NOT exceed max_advance,
                        // within the configured fit tolerance)
                        //
                        // We simply append the cluster(s) to the current line
                        if next_x <= max_advance + self.state.max_advance_fit_tolerance {
                            if max_height_exceeded {
                                return self.max_height_break_data(line_height);
                            }
                            if hangs_or_is_removed {
                                self.state.finish_hanging_whitespace(style.text_wrap_mode);
                            } else {
                                self.state.finish_content(style.text_wrap_mode);
                            }
                            self.state.append_cluster_to_line(next_x, line_height);
                            self.state.cluster_idx += 1;
                            // `break-spaces`: a soft-wrap opportunity exists after every preserved
                            // white space character (including between consecutive spaces). Record
                            // it here, *after* appending, so the white space stays on the current
                            // line and the break happens before the following content.
                            if is_break_spaces_space && text_wrap_mode == TextWrapMode::Wrap {
                                self.state.mark_line_break_opportunity(true);
                            }
                        }
                        // Else we attempt to line break:
                        //
                        // This will only succeed if there is an available line-break opportunity that has been marked earlier
                        // in the line. If there is no such line-breaking opportunity (such as if wrapping is disabled), then
                        // we fall back to appending the content to the line anyway.
                        else {
                            // Case: cluster is a space character (and wrapping is enabled)
                            //
                            // Hanging white space is not considered when measuring the line's
                            // contents for fit, so an overflowing space must not cause a break by
                            // itself. We append it to the line (where it will hang) and keep
                            // consuming the rest of the white space run: the line then breaks at
                            // the next soft-wrap opportunity (just after the run, before the
                            // following content), at a forced break — where the white space
                            // *conditionally* hangs — or at the end of the text.
                            //
                            // A no-break space is not hangable white space: it is treated like any
                            // other visible character (and provides no soft-wrap opportunity), so
                            // it falls through to the regular handling below.
                            if (whitespace == Whitespace::Space || is_ideographic_space)
                                && !is_break_spaces
                                && text_wrap_mode == TextWrapMode::Wrap
                            {
                                if max_height_exceeded {
                                    return self.max_height_break_data(line_height);
                                }
                                self.state.finish_hanging_whitespace(style.text_wrap_mode);
                                self.state.append_cluster_to_line(next_x, line_height);
                                self.state.cluster_idx += 1;
                                continue;
                            }
                            // Case: cluster is preserved white space in `break-spaces` mode (and
                            // wrapping is enabled)
                            //
                            // In `break-spaces` mode, preserved white space does not hang;
                            // instead it takes up space. An overflowing space wraps the line at
                            // the most recent soft-wrap opportunity *after another preserved
                            // space*, or at an emergency (`overflow-wrap`) opportunity. It may
                            // not wrap at a regular opportunity within the preceding content
                            // (there is no soft-wrap opportunity before a preserved space);
                            // absent a usable opportunity, the space overflows the line and — as
                            // a soft-wrap opportunity exists after every preserved white space
                            // character — the break happens at the opportunity recorded after it.
                            else if is_break_spaces_space && text_wrap_mode == TextWrapMode::Wrap
                            {
                                if let Some(prev) = self
                                    .state
                                    .prev_boundary
                                    .take_if(|prev| prev.after_break_spaces_space)
                                {
                                    self.state.line = prev.state;
                                    if try_commit_line!(BreakReason::Regular) {
                                        self.state.item_idx = prev.item_idx;
                                        self.state.run_idx = prev.run_idx;
                                        self.state.cluster_idx = prev.cluster_idx;
                                        return self.start_new_line(BreakReason::Regular);
                                    }
                                }
                                if let Some(prev_emergency) = self.state.emergency_boundary.take() {
                                    self.state.line = prev_emergency.state;
                                    if try_commit_line!(BreakReason::Emergency) {
                                        self.state.item_idx = prev_emergency.item_idx;
                                        self.state.run_idx = prev_emergency.run_idx;
                                        self.state.cluster_idx = prev_emergency.cluster_idx;
                                        return self.start_new_line(BreakReason::Emergency);
                                    }
                                }
                                if max_height_exceeded {
                                    return self.max_height_break_data(line_height);
                                }
                                self.state.finish_content(style.text_wrap_mode);
                                self.state.append_cluster_to_line(next_x, line_height);
                                self.state.cluster_idx += 1;
                                self.state.mark_line_break_opportunity(true);
                                continue;
                            }
                            // Case: we have previously encountered a REGULAR line-breaking opportunity in the current line
                            //
                            // We "take" the line-breaking opportunity by starting a new line and resetting our
                            // item/run/cluster iteration state back to how it was when the line-breaking opportunity was encountered
                            else if let Some(prev) = self.state.prev_boundary.take() {
                                // println!("REVERT");
                                // debug_assert!(prev.state.x != 0.0);

                                // Q: Why do we revert the line state here, but only revert the indexes if the commit succeeds?
                                self.state.line = prev.state;
                                if try_commit_line!(BreakReason::Regular) {
                                    // Revert boundary state to prev state
                                    self.state.item_idx = prev.item_idx;
                                    self.state.run_idx = prev.run_idx;
                                    self.state.cluster_idx = prev.cluster_idx;

                                    return self.start_new_line(BreakReason::Regular);
                                }
                            }
                            // Case: we have previously encountered an EMERGENCY line-breaking opportunity in the current line
                            //
                            // We "take" the line-breaking opportunity by starting a new line and resetting our
                            // item/run/cluster iteration state back to how it was when the line-breaking opportunity was encountered
                            else if let Some(prev_emergency) =
                                self.state.emergency_boundary.take()
                            {
                                self.state.line = prev_emergency.state;
                                if try_commit_line!(BreakReason::Emergency) {
                                    // Revert boundary state to prev state
                                    self.state.item_idx = prev_emergency.item_idx;
                                    self.state.run_idx = prev_emergency.run_idx;
                                    self.state.cluster_idx = prev_emergency.cluster_idx;

                                    return self.start_new_line(BreakReason::Emergency);
                                }
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
                                self.state.append_cluster_to_line(next_x, line_height);
                                self.state.cluster_idx += 1;
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
        if try_commit_line!(BreakReason::None) {
            self.done = true;
            return self.start_new_line(BreakReason::None);
        }

        None
    }

    /// Computes the next line in the paragraph by character count.
    ///
    /// This method breaks lines based on the number of characters rather than advance width.
    /// Each text cluster (including whitespace and newlines) counts as 1 character.
    /// Each atomic inline box also counts as 1 character. Structural
    /// inline-start and inline-end edges do not count independently.
    /// Ligature components each count separately (matching character count).
    ///
    /// Unlike `break_next`, this method does not respect normal line break opportunities and
    /// will break exactly when the character limit is reached. It does not break on newlines, for example.
    ///
    /// Atomic inline boxes are supported and each contributes as 1 character.
    /// A requested break remains outside any adjacent structural inline edges.
    pub fn break_next_with_length(&mut self, max_chars: u32) -> Option<()> {
        if self.done {
            return None;
        }

        let line_indent = self.resolve_indent();

        // Track cluster count for this line
        let mut char_count: u32 = 0;
        let char_limit = max_chars.max(1);
        let mut pending_break_reason = BreakReason::Regular;

        // This macro simply calls the `commit_line` with the provided arguments and some parts of self.
        macro_rules! try_commit_line {
            ($break_reason:expr) => {
                try_commit_line(
                    self.layout,
                    &mut self.lines,
                    &mut self.state.line,
                    f32::MAX, // No advance limit
                    $break_reason,
                    line_indent,
                )
            };
        }

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
            if char_count >= char_limit
                && !can_follow_limited_content
                && try_commit_line!(pending_break_reason)
            {
                self.start_new_line(pending_break_reason);
                return Some(());
            }

            match item.kind {
                LayoutItemKind::InlineBox => {
                    let inline_box = &self.layout.data.inline_boxes[item.index];

                    if !inline_box.kind.contributes_advance() {
                        self.state.item_idx += 1;
                        self.state.append_inline_box_to_line(self.state.line.x, 0.0);
                        if let Some(mode) =
                            self.layout.data.inline_box_text_wrap_mode_after[item.index]
                        {
                            self.state.line.text_wrap_mode = mode;
                        }
                        continue;
                    }

                    if inline_box.kind.is_inline_edge() {
                        if inline_box.kind == InlineBoxKind::InlineStart {
                            self.state.begin_inline_start();
                        }
                        let next_x = self.state.line.x + inline_box.width;
                        self.state.item_idx += 1;
                        self.state
                            .append_inline_box_to_line(next_x, inline_box.height);
                        if let Some(mode) =
                            self.layout.data.inline_box_text_wrap_mode_after[item.index]
                        {
                            self.state.line.text_wrap_mode = mode;
                        }
                        if inline_box.kind == InlineBoxKind::InlineEnd {
                            self.state.propagate_break_after_inline_end();
                        }
                        continue;
                    }

                    // Compute the x position for the line width tracking
                    let text_wrap_mode = self.state.line.text_wrap_mode;
                    let next_x = self.state.line.x + inline_box.width;
                    self.state.item_idx += 1;
                    self.state
                        .append_inline_box_to_line(next_x, inline_box.height);
                    self.state.finish_content(text_wrap_mode);
                    char_count += 1;
                    pending_break_reason = BreakReason::Regular;
                }
                LayoutItemKind::TextRun => {
                    let run_idx = item.index;
                    let run_data = &self.layout.data.runs[run_idx];
                    let run = Run::new(self.layout, 0, 0, run_data, None);
                    let cluster_start = run_data.cluster_range.start;
                    let cluster_end = run_data.cluster_range.end;

                    while self.state.cluster_idx < cluster_end {
                        let cluster = run.get(self.state.cluster_idx - cluster_start).unwrap();

                        // Check if we should break before this cluster
                        if char_count >= char_limit && try_commit_line!(pending_break_reason) {
                            self.start_new_line(pending_break_reason);
                            return Some(());
                        }

                        let whitespace = cluster.info().whitespace();
                        let is_newline = whitespace == Whitespace::Newline;
                        let advance = cluster.advance();
                        let style = &self.layout.data.styles[cluster.data.style_index as usize];

                        // Compute the x position.
                        // Newlines don't contribute to line width (matching break_next behavior).
                        let next_x = if is_newline {
                            self.state.line.x
                        } else {
                            self.state.line.x + advance
                        };
                        let line_height = run.metrics().line_height;
                        self.state.finish_content(style.text_wrap_mode);
                        self.state.append_cluster_to_line(next_x, line_height);
                        self.state.cluster_idx += 1;
                        char_count += 1;
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
        if try_commit_line!(BreakReason::None) {
            self.done = true;
            self.start_new_line(BreakReason::None);
            return Some(());
        }

        None
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
        while self.break_next().is_some() {}
        self.finish();
    }

    /// Consumes the line breaker and finalizes all line computations.
    pub fn finish(mut self) {
        if self.layout.data.text_len == 0 {
            if let Some(line) = self.lines.line_items.first_mut() {
                line.text_range = 0..0;
                line.cluster_range = 0..0;
            }
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
        line.metrics.ascent = 0.;
        line.metrics.descent = 0.;
        line.metrics.leading = 0.;
        line.metrics.offset = 0.;
        line.text_range.start = usize::MAX;

        line.metrics.line_height = line_height;

        if line.item_range.is_empty() {
            line.text_range = self.layout.data.text_len..self.layout.data.text_len;
        }
        // Compute metrics for the line, but ignore trailing whitespace.
        let mut have_metrics = false;
        let mut needs_reorder = false;
        for line_item in self.lines.line_items[line.item_range.clone()]
            .iter_mut()
            .rev()
        {
            match line_item.kind {
                LayoutItemKind::InlineBox => {
                    let item = &self.layout.data.inline_boxes[line_item.index];

                    // Advance is already computed in "commit line" for items
                    if item.kind == InlineBoxKind::InFlow {
                        // Default vertical alignment is to align the bottom of boxes with the text baseline.
                        // This is equivalent to the entire height of the box being "ascent"
                        line.metrics.ascent = line.metrics.ascent.max(item.height);

                        // Mark us as having seen non-whitespace content on this line
                        have_metrics = true;
                    }
                }
                LayoutItemKind::TextRun => {
                    line_item.compute_whitespace_properties(&self.layout.data);

                    // Compute the text range for the line
                    // Q: Can we not simplify this computation by assuming that items are in order?
                    line.text_range.end = line.text_range.end.max(line_item.text_range.end);
                    line.text_range.start = line.text_range.start.min(line_item.text_range.start);

                    // Mark line as needing bidi re-ordering if it contains any runs with non-zero bidi level
                    // (zero is the default level, so this is equivalent to marking lines that have multiple levels)
                    if line_item.bidi_level != 0 {
                        needs_reorder = true;
                    }

                    // Compute the run's advance by summing the advances of its constituent clusters
                    line_item.advance = self.layout.data.clusters[line_item.cluster_range.clone()]
                        .iter()
                        .map(|c| c.advance)
                        .sum();

                    // Ignore trailing whitespace for metrics computation
                    // (we are iterating backwards so trailing whitespace comes first)
                    if !have_metrics && line_item.is_whitespace {
                        continue;
                    }

                    // Compute the run's vertical metrics
                    let run = &self.layout.data.runs[line_item.index];
                    line.metrics.ascent = line.metrics.ascent.max(run.metrics.ascent);
                    line.metrics.descent = line.metrics.descent.max(run.metrics.descent);

                    // Mark us as having seen non-whitespace content on this line
                    have_metrics = true;
                }
            }
        }

        // UAX#9 rule L1: white space at the end of the line (in logical order) takes the
        // *paragraph* embedding level rather than the level of its run, so that it is placed at
        // the line's end edge in the paragraph direction (where any hanging happens). Reset the
        // bidi level of trailing white space items to the paragraph level, splitting the
        // logically-final run into a separate line item when only part of it is trailing white
        // space. (No-break spaces are not reset: their bidi class is not one L1 applies to.)
        let base_level = self.layout.data.base_level;
        let is_l1_whitespace = |cluster: &ClusterData| {
            matches!(
                cluster.info.whitespace(),
                Whitespace::Space | Whitespace::Tab | Whitespace::Newline
            ) || cluster.info.source_char() == '\u{3000}'
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
                // Open/close inline edges and out-of-flow boxes are opaque to
                // bidi and white-space collapsing. Continue to the preceding
                // text item, matching UAX#9 L1 over Blink's structural items.
                continue;
            }
            if item.bidi_level == base_level {
                // Already at the paragraph level. If the item is entirely white space, the
                // trailing white space run may extend into the previous item.
                if item.is_whitespace {
                    continue;
                }
                break;
            }
            let clusters = &self.layout.data.clusters[item.cluster_range.clone()];
            let ws_count = clusters
                .iter()
                .rev()
                .take_while(|cluster| is_l1_whitespace(cluster))
                .count();
            if ws_count == 0 {
                break;
            }
            needs_reorder = true;
            if ws_count == clusters.len() {
                self.lines.line_items[item_idx].bidi_level = base_level;
                continue;
            }
            // Split the run's trailing white space off into its own line item.
            let item = &self.lines.line_items[item_idx];
            let split_cluster_idx = item.cluster_range.end - ws_count;
            let run_data = &self.layout.data.runs[item.index];
            let split_text_idx = self.layout.data.clusters[split_cluster_idx]
                .text_range(run_data)
                .start;
            let ws_advance: f32 = self.layout.data.clusters
                [split_cluster_idx..item.cluster_range.end]
                .iter()
                .map(|cluster| cluster.advance)
                .sum();
            let mut ws_item = item.clone();
            ws_item.bidi_level = base_level;
            ws_item.cluster_range = split_cluster_idx..item.cluster_range.end;
            ws_item.text_range = split_text_idx..item.text_range.end;
            ws_item.advance = ws_advance;
            ws_item.compute_whitespace_properties(&self.layout.data);
            let item = &mut self.lines.line_items[item_idx];
            item.cluster_range.end = split_cluster_idx;
            item.text_range.end = split_text_idx;
            item.advance -= ws_advance;
            item.compute_whitespace_properties(&self.layout.data);
            self.lines.line_items.insert(item_idx + 1, ws_item);
            line.item_range.end += 1;
            break;
        }

        // Reorder the items within the line (if required). Reordering is required if the line contains
        // a mix of bidi levels (a mix of LTR and RTL text)
        let item_count = line.item_range.end - line.item_range.start;
        if needs_reorder && item_count > 1 {
            reorder_line_items(&mut self.lines.line_items[line.item_range.clone()]);
        }

        // Justification opportunities are a property of the committed line, after phase-II
        // white-space processing and bidi line construction. Deriving the count here avoids the
        // old line-breaker heuristic of incrementally counting source spaces and subtracting one
        // presumed trailing space, which cannot represent preserved runs of multiple trailing
        // spaces or structural inline edges.
        let total_justification_spaces = self.lines.line_items[line.item_range.clone()]
            .iter()
            .filter(|item| item.is_text_run())
            .map(|item| {
                self.layout.data.clusters[item.cluster_range.clone()]
                    .iter()
                    .filter(|cluster| cluster.info.whitespace().is_space_or_nbsp())
                    .count()
            })
            .sum::<usize>();

        // Compute size of line's trailing whitespace. "Trailing" is considered the right edge
        // for LTR text and the left edge for RTL text (i.e. the line's end edge in the paragraph
        // direction, which is where the trailing white space sits after the L1 reset above).
        //
        // How much of the trailing white space "hangs" (i.e. is excluded from the line's used
        // width and alignment) depends on the white-space-collapse mode (see
        // `EndOfLineWhitespace`). Removed white space and ideographic spaces hang
        // unconditionally; preserved white space at a forced break or the last line only
        // *conditionally* hangs, i.e. only insofar as it overflows the line. An unconditionally
        // hanging cluster inward of the conditional run at the edge can only hang (reach the
        // edge) if that conditional run fully hangs.
        let is_rtl = self.layout.is_rtl();
        // Clusters of *removed* white space (collapsible white space at the end of the line),
        // recorded as (item index, cluster index) pairs to be zeroed out below.
        let mut removed_clusters: Vec<(usize, usize)> = Vec::new();
        let (unconditional, conditional, trailing_justification_spaces) = {
            let styles = &self.layout.data.styles;
            let clusters = &self.layout.data.clusters;
            let items = &self.lines.line_items[line.item_range.clone()];
            let mut unconditional = 0.0;
            let mut conditional = 0.0;
            let mut past_conditional_edge = false;
            let mut seen_hanging = false;
            let mut trailing_justification_spaces = 0;
            // Iterate items from the line's trailing edge inward. Structural
            // inline edges and out-of-flow boxes are transparent; an in-flow
            // atomic inline is real content and terminates the run.
            let mut item_iter_fwd;
            let mut item_iter_rev;
            let item_iter: &mut dyn Iterator<Item = (usize, &LineItemData)> = if is_rtl {
                item_iter_fwd = items.iter().enumerate();
                &mut item_iter_fwd
            } else {
                item_iter_rev = items.iter().enumerate().rev();
                &mut item_iter_rev
            };
            'items: for (item_offset, item) in item_iter {
                if !item.is_text_run() {
                    let inline_box = &self.layout.data.inline_boxes[item.index];
                    if inline_box.kind == InlineBoxKind::InFlow {
                        break;
                    }
                    continue;
                }
                // Iterate the item's clusters from the line's trailing edge inward. Clusters are
                // stored in logical order, so this is a reverse iteration exactly when the item's
                // direction matches the paragraph direction.
                let mut cluster_iter_fwd;
                let mut cluster_iter_rev;
                let cluster_iter: &mut dyn Iterator<Item = usize> = if item.is_rtl() == is_rtl {
                    cluster_iter_rev = item.cluster_range.clone().rev();
                    &mut cluster_iter_rev
                } else {
                    cluster_iter_fwd = item.cluster_range.clone();
                    &mut cluster_iter_fwd
                };
                for cluster_idx in cluster_iter {
                    let cluster = &clusters[cluster_idx];
                    let hang = trailing_whitespace_hang(cluster, styles);
                    if !matches!(
                        hang,
                        TrailingWhitespaceHang::Skip | TrailingWhitespaceHang::End
                    ) && cluster.info.whitespace().is_space_or_nbsp()
                    {
                        trailing_justification_spaces += 1;
                    }
                    match hang {
                        TrailingWhitespaceHang::Skip => {}
                        TrailingWhitespaceHang::End => break 'items,
                        // Collapsible white space at the very end of the line is removed
                        // entirely: it takes up no space at all, rather than hanging. Inward of
                        // hanging white space (e.g. between hanging ideographic spaces) it is
                        // not at the end of the line, and hangs unconditionally instead.
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
        // Zero out the advances of removed white space clusters, and update the advances of the
        // line items (and the line itself) that contained them.
        let mut removed_advance = 0.0;
        for &(item_idx, cluster_idx) in &removed_clusters {
            let advance = core::mem::take(&mut self.layout.data.clusters[cluster_idx].advance);
            self.lines.line_items[item_idx].advance -= advance;
            removed_advance += advance;
        }
        let line = &mut self.lines.lines[line_idx];
        line.num_spaces = total_justification_spaces.saturating_sub(trailing_justification_spaces);
        line.metrics.advance -= removed_advance;
        let break_reason = line.break_reason;
        let line_max_advance = line.max_advance;
        let line_advance = line.metrics.advance;
        let conditional_hang = match break_reason {
            // Soft wrap: the preserved white space hangs unconditionally.
            BreakReason::Regular | BreakReason::Emergency => conditional,
            // Forced break or last line (end of block): only the overflowing part hangs.
            BreakReason::Explicit | BreakReason::None => {
                (line_advance - line_max_advance).clamp(0.0, conditional)
            }
        };
        line.metrics.trailing_whitespace = if conditional_hang >= conditional {
            conditional_hang + unconditional
        } else {
            conditional_hang
        };

        if !have_metrics {
            // Line consisting entirely of whitespace?
            if !line.item_range.is_empty() {
                let line_item = &self.lines.line_items[line.item_range.start];
                if line_item.is_text_run() {
                    let run = &self.layout.data.runs[line_item.index];
                    line.metrics.ascent = run.metrics.ascent;
                    line.metrics.descent = run.metrics.descent;
                }
            } else if let Some(metrics) = prev_line_metrics {
                // HACK: copy metrics from previous line if we don't have
                // any; this should only occur for an empty line following
                // a newline at the end of a layout
                line.metrics = metrics;
                // If we have no items on this line, it must be the last (empty)
                // line in a layout following a newline. Commit an empty run so
                // that AccessKit has a node with which to identify the visual
                // cursor position
                if let Some((index, run)) = self
                    .layout
                    .data
                    .runs
                    .iter()
                    .enumerate()
                    .rfind(|(_, run)| !run.text_range.is_empty())
                {
                    let run_index = self.lines.line_items.len();
                    let cluster = run.cluster_range.end;
                    let text = run.text_range.end;
                    self.lines.line_items.push(LineItemData {
                        kind: LayoutItemKind::TextRun,
                        index,
                        bidi_level: 0,
                        advance: 0.,
                        is_whitespace: false,
                        has_trailing_whitespace: false,
                        cluster_range: cluster..cluster,
                        text_range: text..text,
                    });
                    line.item_range = run_index..run_index + 1;
                }
            }
        }

        line.metrics.leading =
            line.metrics.line_height - (line.metrics.ascent + line.metrics.descent);

        // Whether metrics should be quantized to pixel boundaries
        let quantize = self.layout.data.quantize;

        let (ascent, descent) = if quantize {
            // We mimic Chrome in rounding ascent and descent separately,
            // before calculating the rest.
            // See lines_integral_line_height_ascent_descent_rounding() for more details.
            (line.metrics.ascent.round(), line.metrics.descent.round())
        } else {
            (line.metrics.ascent, line.metrics.descent)
        };

        let (leading_above, leading_below) = if quantize {
            // Calculate leading using the rounded ascent and descent.
            let leading = line.metrics.line_height - (ascent + descent);
            // We mimic Chrome in giving 'below' the larger leading half.
            // Although the comment in Chromium's NGLineHeightMetrics::AddLeading function
            // in ng_line_height_metrics.cc claims it's for legacy test compatibility.
            // So we might want to think about giving 'above' the larger half instead.
            let above = (leading * 0.5).floor();
            let below = leading.round() - above;
            (above, below)
        } else {
            (line.metrics.leading * 0.5, line.metrics.leading * 0.5)
        };

        let y = self.state.line_y;
        line.metrics.baseline =
            ascent + leading_above + if quantize { y.round() as f32 } else { y as f32 };

        // Small line heights will cause leading to be negative.
        // Negative leadings are correct for baseline calculation, but not for min/max coords.
        // We clamp leading to zero for the purposes of min/max coords,
        // which in turn clamps the selection box minimum height to ascent + descent.
        line.metrics.block_min_coord = line.metrics.baseline - ascent - leading_above.max(0.);
        line.metrics.block_max_coord = line.metrics.baseline + descent + leading_below.max(0.);

        // let max_advance = if self.state.line_max_advance < f32::MAX {
        //     self.state.line_max_advance
        // } else {
        //     line.metrics.advance - line.metrics.trailing_whitespace
        // };

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
        if let Some(last_line) = self.lines.lines.last() {
            if last_line.item_range.is_empty() {
                height -= last_line.metrics.line_height as f64;
            }
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

/// How a cluster within the run of trailing white space at the end of a line hangs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrailingWhitespaceHang {
    /// Removed (collapsible) white space and ideographic spaces (U+3000) hang unconditionally.
    Unconditional,
    /// Collapsible white space at the end of a line is removed entirely: it takes up no space
    /// at all.
    Removed,
    /// Preserved white space hangs conditionally: at a forced break or the end of the text it
    /// only hangs insofar as it overflows the line.
    Conditional,
    /// Zero-advance cluster (a newline) that does not terminate the trailing white space run.
    Skip,
    /// Not hangable white space: a no-break space, any other visible character, or white space
    /// that takes up space at the end of a line (`break-spaces`, or preserved white space under
    /// `nowrap`). Terminates the trailing white space run.
    End,
}

fn trailing_whitespace_hang<B: Brush>(
    cluster: &ClusterData,
    styles: &[crate::layout::Style<B>],
) -> TrailingWhitespaceHang {
    let style = &styles[cluster.style_index as usize];
    let end_of_line = style
        .white_space_collapse
        .end_of_line_whitespace(style.text_wrap_mode);
    match cluster.info.whitespace() {
        Whitespace::Newline => TrailingWhitespaceHang::Skip,
        Whitespace::NoBreakSpace => TrailingWhitespaceHang::End,
        Whitespace::Space | Whitespace::Tab => match end_of_line {
            EndOfLineWhitespace::TakesUpSpace => TrailingWhitespaceHang::End,
            EndOfLineWhitespace::Remove => TrailingWhitespaceHang::Removed,
            EndOfLineWhitespace::Hang => TrailingWhitespaceHang::Conditional,
        },
        Whitespace::None => {
            // An ideographic space (U+3000) is preserved even when white space is collapsed,
            // but hangs unconditionally at the end of a line (except in `break-spaces` mode,
            // where it takes up space).
            if cluster.info.source_char() == '\u{3000}'
                && end_of_line != EndOfLineWhitespace::TakesUpSpace
            {
                TrailingWhitespaceHang::Unconditional
            } else {
                TrailingWhitespaceHang::End
            }
        }
    }
}

// fn cluster_range_is_valid(
//     mut cluster_range: Range<usize>,
//     state_cluster_range: Range<usize>,
//     is_first: bool,
//     is_last: bool,
//     is_empty: bool,
// ) -> bool {
//     // Compute cluster range
//     if is_first {
//         cluster_range.start = state_cluster_range.start;
//     }
//     if is_last {
//         cluster_range.end = state_cluster_range.end;
//     }

//     // Return true if cluster is valid. Else false.
//     cluster_range.start < cluster_range.end
//         || (cluster_range.start == cluster_range.end && is_empty)
// }

// fn should_commit_line<B: Brush>(
//     layout: &LayoutData<B>,
//     state: &mut LineState,
//     is_last: bool,
// ) -> bool {
//     // Compute end cluster
//     state.clusters.end = state.clusters.end.min(layout.clusters.len());
//     if state.runs.end == 0 && is_last {
//         state.runs.end = 1;
//     }

//     let last_run = state.runs.len() - 1;
//     let is_empty = layout.text_len == 0;

//     // Iterate over runs. Checking if any have a valid cluster range.
//     let runs = &layout.runs[state.runs.clone()];
//     runs.iter().enumerate().any(|(i, run_data)| {
//         cluster_range_is_valid(
//             run_data.cluster_range.clone(),
//             state.clusters.clone(),
//             i == 0,
//             i == last_run,
//             is_empty,
//         )
//     })
// }

fn try_commit_line<B: Brush>(
    layout: &Layout<B>,
    lines: &mut LineLayout,
    state: &mut LineState,
    max_advance: f32,
    break_reason: BreakReason,
    line_indent: f32,
) -> bool {
    // Ensure that the cluster and item endpoints are within range
    state.clusters.end = state.clusters.end.min(layout.data.clusters.len());
    state.items.end = state.items.end.min(layout.data.items.len());

    let start_item_idx = lines.line_items.len();
    // let start_run_idx = lines.line_items.last().map(|item| item.index).unwrap_or(0);

    let items_to_commit = &layout.data.items[state.items.clone()];

    // Compute first and last run index
    let is_text_run = |item: &LayoutItem| item.kind == LayoutItemKind::TextRun;
    let first_run_pos = items_to_commit.iter().position(is_text_run).unwrap_or(0);
    let last_run_pos = items_to_commit.iter().rposition(is_text_run).unwrap_or(0);

    // // Return if line contains no runs
    // let (Some(first_run_pos), Some(last_run_pos)) = (first_run_pos, last_run_pos) else {
    //     return false;
    // };

    //let runs = &layout.runs[state.runs.clone()];
    // let start_run_idx = items_to_commit[first_run_pos].index;
    // let end_run_idx = items_to_commit[last_run_pos].index;

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
                    is_whitespace: false,
                    has_trailing_whitespace: false,
                    cluster_range: 0..0,
                    text_range: 0..0,
                });

                last_item_kind = item.kind;
            }
            LayoutItemKind::TextRun => {
                let run_data = &layout.data.runs[item.index];

                // Compute cluster range
                // The first and last ranges have overrides to account for line-breaks within runs
                let mut cluster_range = run_data.cluster_range.clone();
                if i == first_run_pos {
                    cluster_range.start = state.clusters.start;
                }
                if i == last_run_pos {
                    cluster_range.end = state.clusters.end;
                }

                if cluster_range.start >= run_data.cluster_range.end {
                    // println!("INVALID CLUSTER");
                    // dbg!(&run_data.text_range);
                    // dbg!(cluster_range);
                    continue;
                }

                last_item_kind = item.kind;
                committed_text_run = true;

                // Push run to line
                let run = Run::new(layout, 0, 0, run_data, None);
                let text_range = if run_data.cluster_range.is_empty() {
                    0..0
                } else {
                    let first_cluster = run
                        .get(cluster_range.start - run_data.cluster_range.start)
                        .unwrap();
                    let last_cluster = run
                        .get((cluster_range.end - run_data.cluster_range.start).saturating_sub(1))
                        .unwrap();
                    first_cluster.text_range().start..last_cluster.text_range().end
                };

                lines.line_items.push(LineItemData {
                    kind: LayoutItemKind::TextRun,
                    index: item.index,
                    bidi_level: run_data.bidi_level,
                    advance: 0.,
                    is_whitespace: false,
                    has_trailing_whitespace: false,
                    cluster_range,
                    text_range,
                });
            }
        }
    }
    // let end_run_idx = lines.line_items.last().map(|item| item.index).unwrap_or(0);
    let end_item_idx = lines.line_items.len();

    // Return false and don't commit line if there were no items to process
    // FIXME: support lines with only inlines boxes
    // if start_item_idx == end_item_idx {
    //     // } || first_run_pos == last_run_pos {
    //     return false;
    // }

    lines.lines.push(LineData {
        item_range: start_item_idx..end_item_idx,
        max_advance,
        break_reason,
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
        let is_odd = level & 1 != 0;

        // Update max level
        if level > max_level {
            max_level = level;
        }

        // Update min odd level
        if is_odd && level < lowest_odd_level {
            lowest_odd_level = level;
        }
    }

    // Iterate over bidi levels
    for level in (lowest_odd_level..=max_level).rev() {
        // Iterate over text runs
        let mut i = 0;
        while i < run_count {
            if runs[i].bidi_level >= level {
                let mut end = i + 1;
                while end < run_count && runs[end].bidi_level >= level {
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
