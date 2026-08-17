// Copyright 2021 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Context for layout.

use super::FontContext;
use super::context::LayoutContext;
use super::style::{Brush, StyleProperty, TextStyle, WhiteSpaceCollapse};

use super::layout::Layout;

use alloc::{string::String, vec::Vec};
use core::ops::{Bound, Range, RangeBounds};

use crate::InlineBoxKind;
use crate::break_overrides::LineBreakOverrideFn;
use crate::inline_box::{InlineBox, InlineBoxInput};
use crate::resolve::{ResolvedStyle, StyleRun, tree::ItemKind};

/// Builder for constructing a text layout with ranged attributes.
#[must_use]
pub struct RangedBuilder<'a, B: Brush> {
    pub(crate) scale: f32,
    pub(crate) quantize: bool,
    pub(crate) lcx: &'a mut LayoutContext<B>,
    pub(crate) fcx: &'a mut FontContext,
    pub(crate) line_break_override: Option<&'a LineBreakOverrideFn>,
}

impl<'b, B: Brush> RangedBuilder<'b, B> {
    pub fn push_default<'a>(&mut self, property: impl Into<StyleProperty<'a, B>>) {
        let resolved = self
            .lcx
            .rcx
            .resolve_property(self.fcx, &property.into(), self.scale);
        self.lcx.ranged_style_builder.push_default(resolved);
    }

    pub fn push<'a>(
        &mut self,
        property: impl Into<StyleProperty<'a, B>>,
        range: impl RangeBounds<usize>,
    ) {
        let resolved = self
            .lcx
            .rcx
            .resolve_property(self.fcx, &property.into(), self.scale);
        self.lcx.ranged_style_builder.push(resolved, range);
    }

    pub fn push_inline_box(&mut self, inline_box: InlineBox) {
        push_inline_box_input(self.lcx, inline_box, None);
    }

    /// Set the callback which will be called as a first provider of line breaking decisions.
    ///
    /// See [`LineBreakOverrideFn`] for more details.
    pub fn set_line_break_override(&mut self, overrides: Option<&'b LineBreakOverrideFn>) {
        self.line_break_override = overrides;
    }

    pub fn build_into(self, layout: &mut Layout<B>, text: impl AsRef<str>) {
        let root_style = self.lcx.ranged_style_builder.root_style().clone();
        // Apply RangedStyleBuilder styles directly to style-table/style-run state.
        self.lcx
            .ranged_style_builder
            .finish(&mut self.lcx.style_table, &mut self.lcx.style_runs);
        let root_style_index = establish_root_style(&mut self.lcx.style_table, root_style);

        // Call generic layout builder method
        build_into_layout(
            layout,
            self.scale,
            self.quantize,
            text.as_ref(),
            self.lcx,
            self.fcx,
            self.line_break_override,
            Some(root_style_index),
        );
    }

    pub fn build(self, text: impl AsRef<str>) -> Layout<B> {
        let mut layout = Layout::default();
        self.build_into(&mut layout, text);
        layout
    }
}

/// Builder for constructing a text layout from a style table and
/// indexed style runs.
#[must_use]
pub struct StyleRunBuilder<'a, B: Brush> {
    pub(crate) scale: f32,
    pub(crate) quantize: bool,
    pub(crate) len: usize,
    pub(crate) lcx: &'a mut LayoutContext<B>,
    pub(crate) fcx: &'a mut FontContext,
    pub(crate) cursor: usize,
    pub(crate) line_break_override: Option<&'a LineBreakOverrideFn>,
    pub(crate) root_style_index: Option<u16>,
}

impl<'b, B: Brush> StyleRunBuilder<'b, B> {
    /// Reserves additional capacity for styles and runs.
    ///
    /// This is an optional optimization for callers that know counts
    /// up front; call it before pushing styles and runs to reduce
    /// reallocations.
    pub fn reserve(&mut self, additional_styles: usize, additional_runs: usize) {
        self.lcx.style_table.reserve(additional_styles);
        self.lcx.style_runs.reserve(additional_runs);
    }

    /// Adds a fully-specified style to the shared style table and
    /// returns its index.
    pub fn push_style<'family, 'settings>(
        &mut self,
        style: TextStyle<'family, 'settings, B>,
    ) -> u16 {
        let resolved = self
            .lcx
            .rcx
            .resolve_entire_style_set(self.fcx, &style, self.scale);
        let style_index = self.lcx.style_table.len();
        assert!(style_index <= u16::MAX as usize, "too many styles");
        self.lcx.style_table.push(resolved);
        style_index as u16
    }

    /// Adds a style run referencing an entry from the style table.
    ///
    /// Runs must be contiguous and non-overlapping, and must cover
    /// `0..text.len()` once all runs have been added.
    pub fn push_style_run(&mut self, style_index: u16, range: impl RangeBounds<usize>) {
        let range = resolve_range(range, self.len);
        assert!(
            range.start == self.cursor,
            "StyleRunBuilder expects contiguous non-overlapping runs"
        );
        assert!(
            range.start <= range.end,
            "StyleRunBuilder expects ordered ranges"
        );
        assert!(
            (style_index as usize) < self.lcx.style_table.len(),
            "StyleRunBuilder expects style indices that were previously added via push_style"
        );
        self.lcx.style_runs.push(StyleRun {
            style_index,
            range: range.clone(),
        });
        self.cursor = range.end;
    }

    pub fn push_inline_box(&mut self, inline_box: InlineBox) {
        push_inline_box_input(self.lcx, inline_box, None);
    }

    /// Sets the style of the inline formatting-context root.
    ///
    /// Callers that construct indexed style runs should provide this even when
    /// it differs from the first text run or the paragraph contains only
    /// inline boxes.
    pub fn set_root_style(&mut self, style_index: u16) {
        assert!(
            usize::from(style_index) < self.lcx.style_table.len(),
            "StyleRunBuilder expects a root style previously added via push_style"
        );
        self.root_style_index = Some(style_index);
    }

    /// Adds an inline box whose completion changes the current inline style.
    ///
    /// This models open/close inline items without reducing their state to one
    /// property. It is normally used with
    /// [`InlineStart`](crate::InlineBoxKind::InlineStart) and
    /// [`InlineEnd`](crate::InlineBoxKind::InlineEnd).
    pub fn push_inline_box_with_style_transition(
        &mut self,
        inline_box: InlineBox,
        style_after: u16,
    ) {
        assert!(
            usize::from(style_after) < self.lcx.style_table.len(),
            "StyleRunBuilder expects transition styles previously added via push_style"
        );
        push_inline_box_input(self.lcx, inline_box, Some(style_after));
    }

    /// Set the callback which will be called as a first provider of line breaking decisions.
    ///
    /// See [`LineBreakOverrideFn`] for more details.
    pub fn set_line_break_override(&mut self, overrides: Option<&'b LineBreakOverrideFn>) {
        self.line_break_override = overrides;
    }

    pub fn build_into(self, layout: &mut Layout<B>, text: impl AsRef<str>) {
        assert!(
            self.cursor == self.len,
            "StyleRunBuilder requires runs that cover the full text"
        );
        build_into_layout(
            layout,
            self.scale,
            self.quantize,
            text.as_ref(),
            self.lcx,
            self.fcx,
            self.line_break_override,
            self.root_style_index,
        );
    }

    pub fn build(self, text: impl AsRef<str>) -> Layout<B> {
        let mut layout = Layout::default();
        self.build_into(&mut layout, text);
        layout
    }
}

/// Builder for constructing a text layout with a tree of attributes.
#[must_use]
pub struct TreeBuilder<'a, B: Brush> {
    pub(crate) scale: f32,
    pub(crate) quantize: bool,
    pub(crate) lcx: &'a mut LayoutContext<B>,
    pub(crate) fcx: &'a mut FontContext,
    pub(crate) line_break_override: Option<&'a LineBreakOverrideFn>,
}

impl<'b, B: Brush> TreeBuilder<'b, B> {
    pub fn push_style_span(&mut self, style: TextStyle<'_, '_, B>) {
        let resolved = self
            .lcx
            .rcx
            .resolve_entire_style_set(self.fcx, &style, self.scale);
        self.lcx.tree_style_builder.push_style_span(resolved);
    }

    pub fn push_style_modification_span<'s, 'iter>(
        &mut self,
        properties: impl IntoIterator<Item = &'iter StyleProperty<'s, B>>,
    ) where
        's: 'iter,
        B: 'iter,
    {
        self.lcx.tree_style_builder.push_style_modification_span(
            properties
                .into_iter()
                .map(|p| self.lcx.rcx.resolve_property(self.fcx, p, self.scale)),
        );
    }

    pub fn pop_style_span(&mut self) {
        self.lcx.tree_style_builder.pop_style_span();
    }

    pub fn push_text(&mut self, text: &str) {
        self.lcx.tree_style_builder.push_text(text);
    }

    fn push_inline_box_input(&mut self, mut inline_box: InlineBox, style_after: Option<u16>) {
        if inline_box.kind == InlineBoxKind::InFlow {
            self.lcx.tree_style_builder.push_uncommitted_text(false);
            self.lcx.tree_style_builder.set_is_span_first(false);
            self.lcx
                .tree_style_builder
                .set_last_item_kind(ItemKind::InlineBox);
        }

        // TODO: arrange type better here to factor out the index
        inline_box.index = self.lcx.tree_style_builder.current_text_len();
        push_inline_box_input(self.lcx, inline_box, style_after);
    }

    pub fn push_inline_box(&mut self, inline_box: InlineBox) {
        self.push_inline_box_input(inline_box, None);
    }

    pub fn set_white_space_mode(&mut self, white_space_collapse: WhiteSpaceCollapse) {
        self.lcx
            .tree_style_builder
            .set_white_space_mode(white_space_collapse);
    }

    /// Set the callback which will be called as a first provider of line breaking decisions.
    ///
    /// See [`LineBreakOverrideFn`] for more details.
    pub fn set_line_break_override(&mut self, overrides: Option<&'b LineBreakOverrideFn>) {
        self.line_break_override = overrides;
    }

    #[inline]
    pub fn build_into(self, layout: &mut Layout<B>) -> String {
        let root_style = self.lcx.tree_style_builder.root_style().clone();
        let has_inline_boxes = !self.lcx.inline_boxes.is_empty();
        // Apply TreeStyleBuilder styles to LayoutContext.
        let text = self
            .lcx
            .tree_style_builder
            .finish(&mut self.lcx.style_table, &mut self.lcx.style_runs);
        let root_style_index = (has_inline_boxes || !text.is_empty())
            .then(|| establish_root_style(&mut self.lcx.style_table, root_style));

        // Call generic layout builder method
        build_into_layout(
            layout,
            self.scale,
            self.quantize,
            &text,
            self.lcx,
            self.fcx,
            self.line_break_override,
            root_style_index,
        );

        text
    }

    #[inline]
    pub fn build(self) -> (Layout<B>, String) {
        let mut layout = Layout::default();
        let text = self.build_into(&mut layout);
        (layout, text)
    }
}

fn push_inline_box_input<B: Brush>(
    lcx: &mut LayoutContext<B>,
    inline_box: InlineBox,
    style_after: Option<u16>,
) {
    lcx.inline_boxes.push(InlineBoxInput {
        inline_box,
        style_after,
    });
}

fn establish_root_style<B: Brush>(
    style_table: &mut Vec<ResolvedStyle<B>>,
    root_style: ResolvedStyle<B>,
) -> u16 {
    if let Some(index) = style_table.iter().position(|style| *style == root_style) {
        return u16::try_from(index).expect("too many styles");
    }
    let index = style_table.len();
    assert!(index <= usize::from(u16::MAX), "too many styles");
    style_table.push(root_style);
    index as u16
}

fn build_into_layout<B: Brush>(
    layout: &mut Layout<B>,
    scale: f32,
    quantize: bool,
    text: &str,
    lcx: &mut LayoutContext<B>,
    fcx: &mut FontContext,
    line_break_override: Option<&LineBreakOverrideFn>,
    root_style_index: Option<u16>,
) {
    if text.is_empty() && lcx.style_runs.is_empty() {
        lcx.style_table.push(ResolvedStyle::default());
        lcx.style_runs.push(StyleRun {
            style_index: 0,
            range: 0..0,
        });
    }
    assert!(
        !lcx.style_runs.is_empty(),
        "at least one style run is required"
    );

    crate::analysis::analyze_text(lcx, text, line_break_override);

    layout.data.clear();
    layout.data.scale = scale;
    layout.data.quantize = quantize;
    layout.data.base_level = lcx.bidi.base_level();
    layout.data.text_len = text.len();

    let mut char_index = 0;
    for style_run in &lcx.style_runs {
        for _ in text[style_run.range.clone()].chars() {
            lcx.info[char_index].1 = style_run.style_index;
            char_index += 1;
        }
    }

    // Copy the visual styles into the layout
    layout
        .data
        .styles
        .extend(lcx.style_table.iter().map(|s| s.as_layout_style()));
    layout.data.root_style_index =
        Some(root_style_index.unwrap_or_else(|| lcx.style_runs[0].style_index));

    // Sort the inline boxes as subsequent code assumes that they are in text index order.
    // Note: It's important that this is a stable sort to allow users to control the order of contiguous inline boxes
    lcx.inline_boxes.sort_by_key(|input| input.inline_box.index);

    {
        let query = fcx.collection.query(&mut fcx.source_cache);
        super::shape::shape_text(
            &lcx.rcx,
            query,
            &lcx.style_table,
            &lcx.inline_boxes,
            &lcx.info,
            lcx.bidi.levels(),
            &mut lcx.scx,
            text,
            layout,
            &lcx.analysis_data_sources,
        );
    }

    // Move inline boxes into the layout
    layout.data.inline_boxes.clear();
    layout.data.inline_box_style_after.clear();
    for input in lcx.inline_boxes.drain(..) {
        layout.data.inline_boxes.push(input.inline_box);
        layout.data.inline_box_style_after.push(input.style_after);
    }
    debug_assert_eq!(
        layout.data.inline_boxes.len(),
        layout.data.inline_box_style_after.len()
    );

    layout.data.finish();
}

fn resolve_range(range: impl RangeBounds<usize>, len: usize) -> Range<usize> {
    let start = match range.start_bound() {
        Bound::Unbounded => 0,
        Bound::Included(n) => *n,
        Bound::Excluded(n) => *n + 1,
    };
    let end = match range.end_bound() {
        Bound::Unbounded => len,
        Bound::Included(n) => *n + 1,
        Bound::Excluded(n) => *n,
    };
    start.min(len)..end.min(len)
}
