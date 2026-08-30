// Copyright 2021 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::inline_box::InlineBox;
use crate::layout::{ContentWidths, LineMetrics, Style};
use crate::resolve::ResolvedStyle;
use crate::style::{Brush, EndOfLineWhitespace, WhiteSpaceCollapse};
use crate::util::nearly_zero;
use crate::{IndentOptions, InlineBoxKind, LineHeight, OverflowWrap, TextWrapMode};
use core::ops::Range;

use alloc::vec::Vec;
use parlance::BidiLevel;
use parley_engine::shape::Whitespace;
use parley_engine::{Boundary, ShapedSlice, ShapedText};

/// `HarfRust`-based run data
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RunData {
    /// Font attributes, needed for accessibility.
    pub(crate) font_attrs: fontique::Attributes,
    /// Synthesis for rendering (contains variation settings)
    pub(crate) synthesis: fontique::Synthesis,
    /// The line height
    pub line_height: f32,
    /// Additional word spacing.
    pub(crate) word_spacing: f32,
    /// Additional letter spacing.
    pub(crate) letter_spacing: f32,
}

#[derive(Copy, Clone, Default, PartialEq, Debug)]
pub enum BreakReason {
    #[default]
    None,
    Regular,
    Explicit,
    Emergency,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) struct LineData {
    /// Range of the source text.
    pub(crate) text_range: Range<usize>,
    /// Range of line items.
    pub(crate) item_range: Range<usize>,
    /// Metrics for the line.
    pub(crate) metrics: LineMetrics,
    /// The cause of the line break.
    pub(crate) break_reason: BreakReason,
    /// Maximum advance for the line.
    pub(crate) max_advance: f32,
    /// Number of justified clusters on the line.
    pub(crate) num_spaces: usize,
    /// Text indent applied to this line.
    pub(crate) indent: f32,
}

impl LineData {
    pub(crate) fn size(&self) -> f32 {
        self.metrics.line_height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LineItemData {
    /// Whether the item is a run or an inline box
    pub(crate) kind: LayoutItemKind,
    /// The index of the run or inline box in the runs or `inline_boxes` vec
    pub(crate) index: usize,
    /// Bidi level for the item (used for reordering)
    pub(crate) bidi_level: BidiLevel,
    /// Advance (size in direction of text flow) for the run.
    pub(crate) advance: f32,

    // Fields that only apply to text runs (Ignored for boxes)
    // TODO: factor this out?
    /// True if the item contains only white space that may be removed or hang.
    pub(crate) is_ignorable_whitespace: bool,
    /// Range of the source text.
    pub(crate) text_range: Range<usize>,
    /// Phase-II removed white space within this line item. The source range is
    /// retained for caret mapping, but its per-line advance is zero.
    pub(crate) removed_text_range: Range<usize>,
    /// This run's shaped clusters on this line, as a range into [`ShapedText::shaped_clusters`].
    ///
    /// The bounds are atom-aligned.
    pub(crate) shaped_cluster_range: Range<u32>,
    /// This run's grapheme clusters on this line, as a range of grapheme indices relative to the
    /// owning [`parley_engine::ShapedRun`].
    pub(crate) grapheme_range: Range<usize>,
}

impl LineItemData {
    pub(crate) fn is_text_run(&self) -> bool {
        self.kind == LayoutItemKind::TextRun
    }

    #[inline(always)]
    pub(crate) fn is_rtl(&self) -> bool {
        self.bidi_level.is_rtl()
    }

    /// Determine whether this item consists entirely of removable/hanging
    /// white space. White space that takes up space and no-break spaces count
    /// as real content here.
    pub(crate) fn compute_ignorable_whitespace<B: Brush>(&mut self, layout_data: &LayoutData<B>) {
        // Skip items which are not text runs
        if self.kind != LayoutItemKind::TextRun {
            return;
        }

        let clusters = layout_data.shaped_text.shaped_clusters();
        let range = self.shaped_cluster_range.clone();
        let char_range = if range.is_empty() {
            0..0
        } else {
            clusters[range.start as usize].chars_range().start as usize
                ..clusters[range.end as usize - 1].chars_range().end as usize
        };
        let characters = &layout_data.shaped_text.characters()[char_range];
        let is_trailing_whitespace = |character: &parley_engine::shape::Character| {
            let style = &layout_data.styles[character.style_index as usize];
            character.info.is_whitespace()
                && character.info.whitespace() != Whitespace::NoBreakSpace
                && style
                    .white_space_collapse
                    .end_of_line_whitespace(style.text_wrap_mode)
                    != EndOfLineWhitespace::TakesUpSpace
        };

        self.is_ignorable_whitespace =
            !characters.is_empty() && characters.iter().all(is_trailing_whitespace);
    }
}

/// The number of graphemes in `slice`.
///
/// This is `O(n)` in the slice's characters.
pub(crate) fn count_graphemes(slice: ShapedSlice<'_>) -> usize {
    slice
        .characters_in(slice.char_range())
        .iter()
        .filter(|character| character.grapheme_start)
        .count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayoutItemKind {
    TextRun,
    InlineBox,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayoutItem {
    /// Whether the item is a run or an inline box
    pub(crate) kind: LayoutItemKind,
    /// The index of the run or inline box in the runs or `inline_boxes` vec
    pub(crate) index: usize,
    /// Bidi level for the item (used for reordering)
    pub(crate) bidi_level: BidiLevel,
    /// Style that becomes current after this item is consumed.
    ///
    /// Text items derive their style from shaped characters. This transition
    /// is used by inline start/end items, including zero-width boundaries that
    /// cannot be represented by a text range.
    pub(crate) style_after: Option<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutData<B: Brush> {
    // General settings (directly from the "builder")
    /// The display scale factor
    pub(crate) scale: f32,
    /// Whether metrics should be quantized to pixel boundaries
    pub(crate) quantize: bool,
    /// The `BiDi` base level
    pub(crate) base_level: BidiLevel,
    /// The length of the text in the layout
    pub(crate) text_len: usize,
    /// Style of the inline formatting-context root.
    pub(crate) root_style_index: u16,

    // Output of style resolution (input to line breaking)
    pub(crate) styles: Vec<Style<B>>,
    pub(crate) inline_boxes: Vec<InlineBox>,

    // Output of shaping (input to line breaking)
    pub(crate) shaped_text: ShapedText,
    pub(crate) runs: Vec<RunData>,
    pub(crate) items: Vec<LayoutItem>,

    // Output of line breaking
    /// The lines in the
    pub(crate) lines: Vec<LineData>,
    /// Items within each line
    pub(crate) line_items: Vec<LineItemData>,
    /// The width constraint that was used to line break the layout
    pub(crate) layout_max_advance: f32,
    /// The computed width of the layout excluding trailing whitespace
    pub(crate) width: f32,
    /// The computed width of the layout including trailing whitespace
    pub(crate) full_width: f32,
    /// The computed height of the layout
    pub(crate) height: f32,

    // Output of alignment
    #[cfg(feature = "accesskit")]
    /// Directly store the alignment if accessibility is enabled so we can
    /// set the corresponding AccessKit property.
    pub(crate) alignment: Option<super::Alignment>,
    /// Whether the layout is aligned with [`crate::Alignment::Justify`].
    pub(crate) is_aligned_justified: bool,
    /// The text-indent amount in layout units.
    pub(crate) indent_amount: f32,
    /// Options controlling text-indent behavior (each-line, hanging).
    pub(crate) indent_options: IndentOptions,
}

impl<B: Brush> Default for LayoutData<B> {
    fn default() -> Self {
        Self {
            scale: 1.,
            quantize: true,
            base_level: BidiLevel::new(0),
            text_len: 0,
            root_style_index: 0,
            width: 0.,
            full_width: 0.,
            height: 0.,
            styles: Vec::new(),
            inline_boxes: Vec::new(),
            shaped_text: ShapedText::new(),
            runs: Vec::new(),
            items: Vec::new(),
            lines: Vec::new(),
            line_items: Vec::new(),
            #[cfg(feature = "accesskit")]
            alignment: None,
            is_aligned_justified: false,
            layout_max_advance: 0.0,
            indent_amount: 0.0,
            indent_options: IndentOptions::default(),
        }
    }
}

impl<B: Brush> LayoutData<B> {
    pub(crate) fn clear(&mut self) {
        self.scale = 1.;
        self.quantize = true;
        self.base_level = BidiLevel::new(0);
        self.text_len = 0;
        self.root_style_index = 0;
        self.width = 0.;
        self.full_width = 0.;
        self.height = 0.;
        self.styles.clear();
        self.inline_boxes.clear();
        self.shaped_text.clear();
        self.runs.clear();
        self.items.clear();
        self.lines.clear();
        self.line_items.clear();
    }

    /// Push an inline box to the list of items
    pub(crate) fn push_inline_box(
        &mut self,
        index: usize,
        bidi_level: BidiLevel,
        style_after: Option<u16>,
    ) {
        self.items.push(LayoutItem {
            kind: LayoutItemKind::InlineBox,
            index,
            bidi_level,
            style_after,
        });
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_shaped_run(
        &mut self,
        shaped_run_idx: usize,
        run_style: &ResolvedStyle<B>,
        word_spacing: f32,
        letter_spacing: f32,
    ) {
        let shaped_run = &self.shaped_text.runs()[shaped_run_idx];
        debug_assert!(
            !shaped_run.shaped_clusters_range.is_empty(),
            "Shaped runs returned by `parley_engine` must be non-empty"
        );
        let style_index =
            self.shaped_text.characters()[shaped_run.characters_range.start as usize].style_index;

        let line_height = {
            // Compute line height
            let style = &self.styles[style_index as usize];
            match style.line_height {
                LineHeight::Absolute(value) => value,
                LineHeight::FontSizeRelative(value) => value * shaped_run.font_size,
                LineHeight::MetricsRelative(value) => {
                    (shaped_run.font_metrics.ascent
                        + shaped_run.font_metrics.descent
                        + shaped_run.font_metrics.leading)
                        * value
                }
            }
        };

        let font = &self.shaped_text.fonts()[shaped_run.font_index];
        let run = RunData {
            font_attrs: fontique::Attributes {
                width: run_style.font_width,
                weight: run_style.font_weight,
                style: run_style.font_style,
            },
            synthesis: font.synthesis,
            line_height,
            word_spacing,
            letter_spacing,
        };

        self.runs.push(run);
        self.items.push(LayoutItem {
            kind: LayoutItemKind::TextRun,
            index: self.runs.len() - 1,
            bidi_level: shaped_run.bidi_level,
            style_after: None,
        });
    }

    pub(crate) fn finish(&mut self) {
        for (run_index, run_data) in self.runs.iter().enumerate() {
            let word = run_data.word_spacing;
            let letter = run_data.letter_spacing;
            if nearly_zero(word) && nearly_zero(letter) {
                continue;
            }
            let cluster_range = self.shaped_text.runs()[run_index]
                .shaped_clusters_range
                .clone();
            let (characters, clusters, glyphs) =
                self.shaped_text.characters_shaped_clusters_and_glyphs_mut();
            for cluster in &mut clusters[cluster_range.start as usize..cluster_range.end as usize] {
                let first_character = &characters[cluster.chars_range().start as usize];
                let mut spacing = letter;
                if !nearly_zero(word) && first_character.info.whitespace().is_space_or_nbsp() {
                    spacing += word;
                }
                if !nearly_zero(spacing) {
                    cluster.advance += spacing;
                    // An inline glyph's advance is the cluster's advance, so it needs no separate
                    // adjustment.
                    if !cluster.has_inline_glyph() && cluster.glyph_len() > 0 {
                        let start = cluster.glyph_offset as usize;
                        let end = start + cluster.glyph_len() as usize;
                        if let Some(last) = glyphs[start..end].last_mut() {
                            last.advance += spacing;
                        }
                    }
                }
            }
        }
    }

    // TODO: this method does not handle mixed direction text at all.
    #[expect(clippy::cast_possible_truncation, reason = "deferred")]
    pub(crate) fn calculate_content_widths(&self) -> ContentWidths {
        // `Layout::default()` is a valid empty layout. It has no root style,
        // and no content can contribute an intrinsic width.
        if self.items.is_empty() {
            return ContentWidths { min: 0.0, max: 0.0 };
        }

        let end_of_line = |character: &parley_engine::shape::Character| {
            let style = &self.styles[character.style_index as usize];
            style
                .white_space_collapse
                .end_of_line_whitespace(style.text_wrap_mode)
        };

        let mut min_width = 0.0_f32;
        let mut max_width = 0.0_f32;

        let mut running_min_width = 0.0;
        let mut running_max_width = 0.0;
        // Open/close inline items belong to the adjacent unbreakable content:
        // start decorations move with following content and end decorations
        // remain attached to preceding content.
        let mut pending_inline_start_width = 0.0;
        let mut break_after_pending = false;
        let mut text_wrap_mode = self.styles[usize::from(self.root_style_index)].text_wrap_mode;
        let mut last_content_text_wrap_mode = None;
        // White space excluded from min-content hangs or is removed. Only
        // removed white space is excluded from max-content; conditionally
        // hanging white space counts at the forced break ending that measure.
        let mut min_trailing_whitespace = 0.0_f32;
        let mut max_trailing_whitespace = 0.0_f32;
        for item in &self.items {
            match item.kind {
                LayoutItemKind::TextRun => {
                    let slice = self.shaped_text.run_slice(item.index as u32);
                    for atom in slice.atoms_start() {
                        if break_after_pending {
                            min_width = min_width.max(running_min_width - min_trailing_whitespace);
                            running_min_width = 0.0;
                            min_trailing_whitespace = 0.0;
                            break_after_pending = false;
                        }
                        let character = &atom.characters()[0];
                        let boundary = character.info.boundary();
                        let style = &self.styles[character.style_index as usize];
                        let prev_text_wrap_mode = text_wrap_mode;
                        text_wrap_mode = style.text_wrap_mode;
                        if prev_text_wrap_mode == TextWrapMode::Wrap
                            && (boundary == Boundary::Line
                                || style.overflow_wrap == OverflowWrap::Anywhere)
                        {
                            min_width = min_width.max(
                                running_min_width
                                    - pending_inline_start_width
                                    - min_trailing_whitespace,
                            );
                            running_min_width = pending_inline_start_width;
                            min_trailing_whitespace = 0.0;
                        }
                        let advance = atom.advance();
                        running_min_width += advance;
                        running_max_width += advance;

                        let whitespace = character.info.whitespace();
                        let is_ideographic_space = character.info.source_char() == '\u{3000}';
                        if style.text_wrap_mode == TextWrapMode::Wrap
                            && style.white_space_collapse == WhiteSpaceCollapse::BreakSpaces
                            && (matches!(whitespace, Whitespace::Space | Whitespace::Tab)
                                || is_ideographic_space)
                        {
                            min_width = min_width.max(running_min_width);
                            running_min_width = 0.0;
                            min_trailing_whitespace = 0.0;
                        }

                        let eol = end_of_line(character);
                        match whitespace {
                            Whitespace::Space | Whitespace::Tab => {
                                if eol == EndOfLineWhitespace::TakesUpSpace {
                                    min_trailing_whitespace = 0.0;
                                } else {
                                    min_trailing_whitespace += advance;
                                }
                                if eol == EndOfLineWhitespace::Remove {
                                    max_trailing_whitespace += advance;
                                } else {
                                    max_trailing_whitespace = 0.0;
                                }
                            }
                            // Segment breaks have zero advance and do not end a
                            // preceding trailing-white-space run.
                            Whitespace::Newline => {}
                            Whitespace::None
                                if is_ideographic_space
                                    && eol != EndOfLineWhitespace::TakesUpSpace =>
                            {
                                min_trailing_whitespace += advance;
                                max_trailing_whitespace += advance;
                            }
                            _ => {
                                min_trailing_whitespace = 0.0;
                                max_trailing_whitespace = 0.0;
                            }
                        }
                        let is_hanging_or_removed =
                            (matches!(whitespace, Whitespace::Space | Whitespace::Tab)
                                || is_ideographic_space)
                                && eol != EndOfLineWhitespace::TakesUpSpace;
                        if !is_hanging_or_removed {
                            pending_inline_start_width = 0.0;
                        }
                        last_content_text_wrap_mode = Some(style.text_wrap_mode);

                        // Unicode line-break analysis stores the mandatory boundary on the
                        // character following a segment break. That character may be separated
                        // from the newline by an inline-box item, or may not exist at all for a
                        // trailing newline. The line breaker consumes the newline itself, so
                        // intrinsic widths must end the measure at the same atom.
                        if whitespace == Whitespace::Newline {
                            min_width = min_width.max(running_min_width - min_trailing_whitespace);
                            running_min_width = 0.0;
                            min_trailing_whitespace = 0.0;
                            max_width = max_width.max(running_max_width - max_trailing_whitespace);
                            running_max_width = 0.0;
                            max_trailing_whitespace = 0.0;
                            break_after_pending = false;
                        }
                    }
                    min_width = min_width.max(running_min_width - min_trailing_whitespace);
                }
                LayoutItemKind::InlineBox => {
                    let inline_box = &self.inline_boxes[item.index];
                    match inline_box.kind {
                        InlineBoxKind::InFlow => {
                            if break_after_pending {
                                min_width =
                                    min_width.max(running_min_width - min_trailing_whitespace);
                                running_min_width = 0.0;
                                min_trailing_whitespace = 0.0;
                            }
                            running_max_width += inline_box.width;
                            let can_break_before = text_wrap_mode == TextWrapMode::Wrap
                                || last_content_text_wrap_mode == Some(TextWrapMode::Wrap);
                            if can_break_before {
                                min_width = min_width.max(
                                    running_min_width
                                        - pending_inline_start_width
                                        - min_trailing_whitespace,
                                );
                                running_min_width = pending_inline_start_width;
                            }
                            running_min_width += inline_box.width;
                            pending_inline_start_width = 0.0;
                            break_after_pending = text_wrap_mode == TextWrapMode::Wrap;
                            last_content_text_wrap_mode = Some(text_wrap_mode);
                            min_trailing_whitespace = 0.0;
                            max_trailing_whitespace = 0.0;
                        }
                        InlineBoxKind::InlineStart => {
                            if break_after_pending {
                                min_width =
                                    min_width.max(running_min_width - min_trailing_whitespace);
                                running_min_width = 0.0;
                                min_trailing_whitespace = 0.0;
                                break_after_pending = false;
                            }
                            running_min_width += inline_box.width;
                            running_max_width += inline_box.width;
                            pending_inline_start_width += inline_box.width;
                        }
                        InlineBoxKind::InlineEnd => {
                            running_min_width += inline_box.width;
                            running_max_width += inline_box.width;
                            pending_inline_start_width = 0.0;
                        }
                        InlineBoxKind::OutOfFlow | InlineBoxKind::CustomOutOfFlow => {}
                    }
                    if let Some(style_index) = item.style_after {
                        text_wrap_mode = self.styles[usize::from(style_index)].text_wrap_mode;
                    }
                }
            }
            max_width = max_width.max(running_max_width - max_trailing_whitespace);
        }

        min_width = min_width.max(running_min_width - min_trailing_whitespace);

        ContentWidths {
            min: min_width,
            max: max_width,
        }
    }
}
