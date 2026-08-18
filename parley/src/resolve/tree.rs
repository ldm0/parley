// Copyright 2021 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hierarchical tree based style application.
use alloc::{string::String, vec::Vec};

use crate::style::WhiteSpaceCollapse;

use super::{Brush, ResolvedProperty, ResolvedStyle, StyleRun};

#[derive(Debug, Clone)]
struct StyleTreeNode<B: Brush> {
    parent: Option<usize>,
    style: ResolvedStyle<B>,
    style_id: Option<u16>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ItemKind {
    None,
    InlineBox,
    TextRun,
}

/// Builder for constructing a tree of styles
#[derive(Clone)]
pub(crate) struct TreeStyleBuilder<B: Brush> {
    tree: Vec<StyleTreeNode<B>>,
    style_table: Vec<ResolvedStyle<B>>,
    style_runs: Vec<StyleRun>,
    text: String,
    uncommitted_text: String,
    current_span: usize,
    last_item_kind: ItemKind,
}

impl<B: Brush> TreeStyleBuilder<B> {
    fn current_style(&self) -> ResolvedStyle<B> {
        self.tree[self.current_span].style.clone()
    }

    pub(crate) fn root_style(&self) -> &ResolvedStyle<B> {
        &self.tree[0].style
    }
}

impl<B: Brush> Default for TreeStyleBuilder<B> {
    fn default() -> Self {
        Self {
            tree: Vec::new(),
            style_table: Vec::new(),
            style_runs: Vec::new(),
            text: String::new(),
            uncommitted_text: String::new(),
            current_span: usize::MAX,
            last_item_kind: ItemKind::None,
        }
    }
}

impl<B: Brush> TreeStyleBuilder<B> {
    /// Prepares the builder for accepting a tree of styles and text.
    ///
    /// The provided `root_style` is the default style applied to all text unless overridden.
    pub(crate) fn begin(&mut self, root_style: ResolvedStyle<B>) {
        self.tree.clear();
        self.style_table.clear();
        self.style_runs.clear();
        self.text.clear();
        self.uncommitted_text.clear();
        self.last_item_kind = ItemKind::None;

        self.tree.push(StyleTreeNode {
            parent: None,
            style: root_style,
            style_id: None,
        });
        self.current_span = 0;
    }

    pub(crate) fn set_white_space_mode(&mut self, white_space_collapse: WhiteSpaceCollapse) {
        self.push_uncommitted_text(false);
        let current = &mut self.tree[self.current_span];
        current.style.white_space_collapse = white_space_collapse;
        current.style_id = None;
    }

    pub(crate) fn set_last_item_kind(&mut self, item_kind: ItemKind) {
        self.last_item_kind = item_kind;
    }

    pub(crate) fn push_uncommitted_text(&mut self, is_text_end: bool) {
        let white_space_collapse = self.tree[self.current_span].style.white_space_collapse;
        match white_space_collapse {
            // `break-spaces` differs from `preserve` only during phase-II line
            // breaking, so both keep the input text verbatim here.
            WhiteSpaceCollapse::Preserve | WhiteSpaceCollapse::BreakSpaces => {}
            WhiteSpaceCollapse::Collapse | WhiteSpaceCollapse::PreserveBreaks => {
                let trim_start = match self.last_item_kind {
                    ItemKind::None => true,
                    ItemKind::InlineBox => false,
                    ItemKind::TextRun => self
                        .text
                        .chars()
                        .last()
                        .is_some_and(|c| c.is_ascii_whitespace()),
                };
                collapse_white_space(
                    &mut self.uncommitted_text,
                    white_space_collapse == WhiteSpaceCollapse::PreserveBreaks,
                    trim_start,
                    is_text_end,
                );
            }
            WhiteSpaceCollapse::PreserveSpaces => {
                convert_white_space_to_spaces(&mut self.uncommitted_text);
            }
        }

        // Nothing to do if there is no uncommitted text.
        if self.uncommitted_text.is_empty() {
            return;
        }

        let range = self.text.len()..(self.text.len() + self.uncommitted_text.len());
        let style_index = self.resolve_current_style_id();
        self.style_runs.push(StyleRun { style_index, range });
        self.text.push_str(&self.uncommitted_text);
        self.uncommitted_text.clear();
        self.last_item_kind = ItemKind::TextRun;
    }

    fn resolve_current_style_id(&mut self) -> u16 {
        if let Some(style_id) = self.tree[self.current_span].style_id {
            return style_id;
        }
        let style_id = self.style_table.len() as u16;
        self.style_table.push(self.current_style());
        self.tree[self.current_span].style_id = Some(style_id);
        style_id
    }

    pub(crate) fn current_text_len(&self) -> usize {
        self.text.len()
    }

    pub(crate) fn push_style_span(&mut self, style: ResolvedStyle<B>) {
        self.push_uncommitted_text(false);

        self.tree.push(StyleTreeNode {
            parent: Some(self.current_span),
            style,
            style_id: None,
        });
        self.current_span = self.tree.len() - 1;
    }

    pub(crate) fn push_style_modification_span(
        &mut self,
        properties: impl Iterator<Item = ResolvedProperty<B>>,
    ) {
        let mut style = self.current_style();
        for prop in properties {
            style.apply(prop.clone());
        }
        self.push_style_span(style);
    }

    pub(crate) fn pop_style_span(&mut self) {
        self.push_uncommitted_text(false);

        self.current_span = self.tree[self.current_span]
            .parent
            .expect("Popped root style");
    }

    /// Pushes a property that covers the specified range of text.
    pub(crate) fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.uncommitted_text.push_str(text);
        }
    }

    /// Computes style table + style runs and returns the final text buffer.
    pub(crate) fn finish(
        &mut self,
        style_table: &mut Vec<ResolvedStyle<B>>,
        style_runs: &mut Vec<StyleRun>,
    ) -> String {
        while self.tree[self.current_span].parent.is_some() {
            self.pop_style_span();
        }

        self.push_uncommitted_text(true);

        style_table.clear();
        style_runs.clear();
        style_table.extend_from_slice(&self.style_table);
        style_runs.extend_from_slice(&self.style_runs);

        if style_runs.is_empty() {
            // If there's no text, the layout still needs the root style, e.g., to size a cursor.
            style_runs.push(StyleRun {
                style_index: style_table.len() as u16,
                range: 0..0,
            });
            style_table.push(self.current_style());
        }

        core::mem::take(&mut self.text)
    }
}

/// Collapses CSS white-space sequences in place while preserving UTF-8 data.
///
/// Segment breaks are either collapsed with their surrounding white space or,
/// for `preserve-breaks`, retained as normalized LF characters. A CRLF pair is
/// one segment break.
fn collapse_white_space(
    text: &mut String,
    preserve_breaks: bool,
    trim_start: bool,
    trim_end: bool,
) {
    const LINE_SEPARATOR: [u8; 3] = [0xE2, 0x80, 0xA8];
    const PARAGRAPH_SEPARATOR: [u8; 3] = [0xE2, 0x80, 0xA9];

    let is_collapsible = |c: char| c.is_ascii_whitespace() || matches!(c, '\u{2028}' | '\u{2029}');
    let mut trimmed = text.as_str();
    if trim_start {
        trimmed = trimmed.trim_start_matches(is_collapsible);
    }
    if trim_end {
        trimmed = trimmed.trim_end_matches(is_collapsible);
    }
    let start = if trim_start {
        text.len() - text.trim_start_matches(is_collapsible).len()
    } else {
        0
    };
    let end = start + trimmed.len();

    let mut bytes = core::mem::take(text).into_bytes();
    let mut read = start;
    let mut write = 0;
    while read < end {
        let byte = bytes[read];
        let is_unicode_break = bytes[read..end].starts_with(&LINE_SEPARATOR)
            || bytes[read..end].starts_with(&PARAGRAPH_SEPARATOR);
        if !byte.is_ascii_whitespace() && !is_unicode_break {
            bytes[write] = byte;
            write += 1;
            read += 1;
            continue;
        }

        let mut segment_breaks = 0;
        while read < end {
            if bytes[read..end].starts_with(&LINE_SEPARATOR)
                || bytes[read..end].starts_with(&PARAGRAPH_SEPARATOR)
            {
                segment_breaks += 1;
                read += LINE_SEPARATOR.len();
                continue;
            }
            if !bytes[read].is_ascii_whitespace() {
                break;
            }
            match bytes[read] {
                b'\r' => {
                    segment_breaks += 1;
                    read += 1;
                    if read < end && bytes[read] == b'\n' {
                        read += 1;
                    }
                }
                b'\n' => {
                    segment_breaks += 1;
                    read += 1;
                }
                _ => read += 1,
            }
        }

        if preserve_breaks && segment_breaks > 0 {
            for _ in 0..segment_breaks {
                bytes[write] = b'\n';
                write += 1;
            }
        } else {
            bytes[write] = b' ';
            write += 1;
        }
    }
    bytes.truncate(write);
    *text = String::from_utf8(bytes).expect("white-space collapsing preserves UTF-8");
}

/// Converts tabs and segment breaks to spaces in place. CRLF is one break;
/// Unicode line and paragraph separators are normalized as segment breaks too.
fn convert_white_space_to_spaces(text: &mut String) {
    const LINE_SEPARATOR: [u8; 3] = [0xE2, 0x80, 0xA8];
    const PARAGRAPH_SEPARATOR: [u8; 3] = [0xE2, 0x80, 0xA9];

    let mut bytes = core::mem::take(text).into_bytes();
    let len = bytes.len();
    let mut read = 0;
    let mut write = 0;
    while read < len {
        match bytes[read] {
            b'\t' | b'\n' => {
                bytes[write] = b' ';
                write += 1;
                read += 1;
            }
            b'\r' => {
                read += 1;
                if read < len && bytes[read] == b'\n' {
                    read += 1;
                }
                bytes[write] = b' ';
                write += 1;
            }
            0xE2 if bytes[read..].starts_with(&LINE_SEPARATOR)
                || bytes[read..].starts_with(&PARAGRAPH_SEPARATOR) =>
            {
                bytes[write] = b' ';
                write += 1;
                read += LINE_SEPARATOR.len();
            }
            byte => {
                bytes[write] = byte;
                write += 1;
                read += 1;
            }
        }
    }
    bytes.truncate(write);
    *text = String::from_utf8(bytes).expect("white-space conversion preserves UTF-8");
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::ops::Range;

    #[test]
    fn collapses_ascii_whitespace_without_trimming_non_ascii_whitespace() {
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(ResolvedStyle::default());
        builder.set_white_space_mode(WhiteSpaceCollapse::Collapse);
        builder.push_text(" \u{00a0}text\u{00a0} ");

        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        let text = builder.finish(&mut style_table, &mut style_runs);

        assert_eq!(text, "\u{00a0}text\u{00a0}");
    }

    #[test]
    fn reuses_style_id_when_returning_to_parent_span() {
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(ResolvedStyle::default());
        builder.push_text("A");
        builder.push_style_modification_span([ResolvedProperty::FontSize(20.)].into_iter());
        builder.push_text("B");
        builder.pop_style_span();
        builder.push_text("C");

        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        let text = builder.finish(&mut style_table, &mut style_runs);

        assert_eq!(text, "ABC");
        assert_eq!(style_table.len(), 2);
        assert_eq!(style_runs.len(), 3);
        assert_eq!(style_runs[0].style_index, 0);
        assert_eq!(style_runs[1].style_index, 1);
        assert_eq!(style_runs[2].style_index, 0);
        assert_eq!(style_runs[0].range, Range { start: 0, end: 1 });
        assert_eq!(style_runs[1].range, Range { start: 1, end: 2 });
        assert_eq!(style_runs[2].range, Range { start: 2, end: 3 });
    }

    #[test]
    fn reuses_root_style_id_across_multiple_pop_return_cycles() {
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(ResolvedStyle::default());
        builder.push_text("A");
        builder.push_style_modification_span([ResolvedProperty::FontSize(20.)].into_iter());
        builder.push_text("B");
        builder.pop_style_span();
        builder.push_text("C");
        builder.push_style_modification_span([ResolvedProperty::LetterSpacing(1.)].into_iter());
        builder.push_text("D");
        builder.pop_style_span();
        builder.push_text("E");

        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        let text = builder.finish(&mut style_table, &mut style_runs);

        assert_eq!(text, "ABCDE");
        assert_eq!(style_table.len(), 3);
        assert_eq!(style_runs.len(), 5);
        assert_eq!(style_runs[0].style_index, 0);
        assert_eq!(style_runs[1].style_index, 1);
        assert_eq!(style_runs[2].style_index, 0);
        assert_eq!(style_runs[3].style_index, 2);
        assert_eq!(style_runs[4].style_index, 0);
    }

    #[test]
    fn reuses_parent_and_root_style_ids_after_nested_pop() {
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(ResolvedStyle::default());
        builder.push_text("R");
        builder.push_style_modification_span([ResolvedProperty::FontSize(20.)].into_iter());
        builder.push_text("A");
        builder.push_style_modification_span([ResolvedProperty::LetterSpacing(1.)].into_iter());
        builder.push_text("B");
        builder.pop_style_span();
        builder.push_text("C");
        builder.pop_style_span();
        builder.push_text("D");

        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        let text = builder.finish(&mut style_table, &mut style_runs);

        assert_eq!(text, "RABCD");
        assert_eq!(style_table.len(), 3);
        assert_eq!(style_runs.len(), 5);
        assert_eq!(style_runs[0].style_index, 0);
        assert_eq!(style_runs[1].style_index, 1);
        assert_eq!(style_runs[2].style_index, 2);
        assert_eq!(style_runs[3].style_index, 1);
        assert_eq!(style_runs[4].style_index, 0);
    }

    fn normalized_text(mode: WhiteSpaceCollapse, text: &str) -> String {
        let style = ResolvedStyle {
            white_space_collapse: mode,
            ..ResolvedStyle::default()
        };
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(style);
        builder.push_text(text);
        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        builder.finish(&mut style_table, &mut style_runs)
    }

    #[test]
    fn normalizes_each_white_space_collapse_mode() {
        use WhiteSpaceCollapse::*;

        let input = "a  b \t c\n\n d\r\ne  ";
        assert_eq!(normalized_text(Preserve, input), input);
        assert_eq!(normalized_text(BreakSpaces, input), input);
        assert_eq!(normalized_text(Collapse, input), "a b c d e");
        assert_eq!(normalized_text(PreserveBreaks, input), "a b c\n\nd\ne");
        assert_eq!(normalized_text(PreserveSpaces, input), "a  b   c   d e  ");
    }

    #[test]
    fn preserve_spaces_normalizes_unicode_segment_breaks() {
        assert_eq!(
            normalized_text(WhiteSpaceCollapse::PreserveSpaces, "a\u{2028}b\u{2029}c"),
            "a b c"
        );
        assert_eq!(
            normalized_text(WhiteSpaceCollapse::Collapse, "a\u{2028}\u{2029}b"),
            "a b"
        );
        assert_eq!(
            normalized_text(WhiteSpaceCollapse::PreserveBreaks, "a\u{2028}\u{2029}b"),
            "a\n\nb"
        );
    }

    #[test]
    fn collapse_crosses_style_boundaries_without_erasing_inline_spacing() {
        let root = ResolvedStyle {
            white_space_collapse: WhiteSpaceCollapse::Collapse,
            ..ResolvedStyle::default()
        };
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(root);
        builder.push_text("a ");
        builder.push_style_modification_span([ResolvedProperty::FontSize(20.)].into_iter());
        builder.push_text("  b");
        builder.pop_style_span();
        builder.push_uncommitted_text(false);
        builder.set_last_item_kind(ItemKind::InlineBox);
        builder.push_text(" c");

        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        let text = builder.finish(&mut style_table, &mut style_runs);
        assert_eq!(text, "a b c");
    }

    #[test]
    fn white_space_mode_is_part_of_the_resolved_style() {
        let mut builder = TreeStyleBuilder::<u32>::default();
        builder.begin(ResolvedStyle::default());
        builder.set_white_space_mode(WhiteSpaceCollapse::BreakSpaces);
        builder.push_text("a");
        builder.push_style_modification_span(
            [ResolvedProperty::WhiteSpaceCollapse(
                WhiteSpaceCollapse::Collapse,
            )]
            .into_iter(),
        );
        builder.push_text("b");
        builder.pop_style_span();

        let mut style_table = Vec::new();
        let mut style_runs = Vec::new();
        assert_eq!(builder.finish(&mut style_table, &mut style_runs), "ab");
        assert_eq!(style_runs.len(), 2);
        assert_eq!(
            style_table[style_runs[0].style_index as usize].white_space_collapse,
            WhiteSpaceCollapse::BreakSpaces
        );
        assert_eq!(
            style_table[style_runs[1].style_index as usize].white_space_collapse,
            WhiteSpaceCollapse::Collapse
        );
    }
}
