// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::vec::Vec;

use super::{ColorBrush, FONT_FAMILY_LIST, create_font_context};
use crate::{
    BaseDirection, BoxBreakData, FontFamily, IndentOptions, InlineBox, InlineBoxKind, Layout,
    LayoutContext, LineEdgeWhitespace, LineHeight, StyleProperty, YieldData,
};

pub(super) fn shape(
    text: &str,
    mode: LineEdgeWhitespace,
    direction: BaseDirection,
    boxes: &[(usize, InlineBoxKind, f32)],
) -> Layout<ColorBrush> {
    let mut fonts = create_font_context();
    let mut context = LayoutContext::new();
    let mut builder = context.ranged_builder(&mut fonts, text, 1.0, false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.push_default(StyleProperty::FontSize(20.0));
    builder.push_default(LineHeight::Absolute(24.0));
    builder.push_default(StyleProperty::LineEdgeWhitespace(mode));
    builder.set_base_direction(direction);
    for (id, &(index, kind, width)) in boxes.iter().enumerate() {
        builder.push_inline_box(InlineBox {
            id: id as u64,
            index,
            kind,
            width,
            height: 0.0,
        });
    }
    builder.build(text)
}

fn collect_breaks(layout: &mut Layout<ColorBrush>, width: f32) -> Vec<BoxBreakData> {
    let mut breaker = layout.break_lines();
    breaker.state_mut().set_layout_max_advance(width);
    breaker.state_mut().set_line_max_advance(width);
    let mut boxes = Vec::new();
    while let Some(data) = breaker.break_next() {
        if let YieldData::InlineBoxBreak(data) = data {
            breaker
                .state_mut()
                .append_inline_box_to_line(data.advance, 0.0);
            boxes.push(data);
        }
    }
    breaker.finish();
    boxes
}

#[test]
fn custom_box_fit_excludes_collapsible_space_without_changing_the_line() {
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        let mut reference = shape("WW WW", LineEdgeWhitespace::Collapse, direction, &[]);
        reference.break_all_lines(Some(200.0));
        let mut actual = shape(
            "WW WW",
            LineEdgeWhitespace::Collapse,
            direction,
            &[(3, InlineBoxKind::CustomOutOfFlow, 0.0)],
        );
        let boxes = collect_breaks(&mut actual, 200.0);
        assert_eq!(boxes.len(), 1);
        let prefix = shape("WW", LineEdgeWhitespace::Collapse, direction, &[])
            .calculate_content_widths()
            .max;
        assert_eq!(boxes[0].fit_advance, prefix);
        assert!(boxes[0].advance > boxes[0].fit_advance);
        assert!(!boxes[0].is_line_start);
        assert_eq!(
            actual.get(0).unwrap().metrics(),
            reference.get(0).unwrap().metrics()
        );
        assert!(
            crate::Cluster::from_byte_index(&actual, 2)
                .unwrap()
                .advance()
                > 0.0
        );
    }
}

#[test]
fn custom_box_fit_retains_preserved_spaces_and_nbsp() {
    for (text, mode) in [
        ("WW ", LineEdgeWhitespace::Preserve),
        ("WW\u{a0}", LineEdgeWhitespace::Collapse),
    ] {
        let mut layout = shape(
            text,
            mode,
            BaseDirection::Ltr,
            &[(text.len(), InlineBoxKind::CustomOutOfFlow, 0.0)],
        );
        let data = collect_breaks(&mut layout, 200.0).remove(0);
        assert_eq!(data.advance, data.fit_advance);
        assert!(!data.is_line_start);
    }
}

#[test]
fn custom_box_fit_distinguishes_leading_space_from_signed_inline_edges() {
    for (edge, leading) in [(0.0, true), (12.0, false), (-12.0, false)] {
        let text = " \u{202c} ";
        let mut layout = shape(
            text,
            LineEdgeWhitespace::Collapse,
            BaseDirection::Ltr,
            &[
                (0, InlineBoxKind::StartBoundary, edge),
                (text.len(), InlineBoxKind::CustomOutOfFlow, 0.0),
            ],
        );
        layout.set_text_indent(15.0, IndentOptions::default());
        let data = collect_breaks(&mut layout, 200.0).remove(0);
        assert_eq!(data.advance, edge);
        assert_eq!(data.fit_advance, edge + 15.0);
        assert_eq!(data.is_line_start, leading);
    }
}

#[test]
fn custom_box_fit_includes_only_immediately_following_ancestor_closing_edges() {
    use InlineBoxKind::{CustomOutOfFlow, EndBoundary, OutOfFlow, StartBoundary, TextBoundary};
    for (following, expected_edges) in [
        (
            alloc::vec![(2, EndBoundary, 25.0), (2, EndBoundary, -10.0)],
            15.0,
        ),
        (
            alloc::vec![
                (2, OutOfFlow, 0.0),
                (2, TextBoundary, 0.0),
                (2, EndBoundary, 25.0)
            ],
            25.0,
        ),
        (
            alloc::vec![
                (2, StartBoundary, 0.0),
                (2, EndBoundary, 0.0),
                (2, EndBoundary, 25.0)
            ],
            0.0,
        ),
        (alloc::vec![(3, EndBoundary, 25.0)], 0.0),
    ] {
        let mut boxes = alloc::vec![(2, CustomOutOfFlow, 0.0)];
        boxes.extend(following);
        let mut layout = shape(
            "WWW",
            LineEdgeWhitespace::Collapse,
            BaseDirection::Ltr,
            &boxes,
        );
        let data = collect_breaks(&mut layout, 200.0).remove(0);
        assert_eq!(data.fit_advance, data.advance + expected_edges);
    }
}

#[test]
fn custom_box_fit_trims_across_edges_and_placeholders_but_not_atomic_content() {
    for atomic in [false, true] {
        let mut layout = shape(
            "WW  ",
            LineEdgeWhitespace::Collapse,
            BaseDirection::Ltr,
            &[
                (
                    3,
                    if atomic {
                        InlineBoxKind::InFlow
                    } else {
                        InlineBoxKind::EndBoundary
                    },
                    10.0,
                ),
                (3, InlineBoxKind::OutOfFlow, 0.0),
                (4, InlineBoxKind::CustomOutOfFlow, 0.0),
            ],
        );
        let data = collect_breaks(&mut layout, 200.0).remove(0);
        let word = shape("WW", LineEdgeWhitespace::Collapse, BaseDirection::Ltr, &[])
            .calculate_content_widths()
            .max;
        let space = shape(" ", LineEdgeWhitespace::Preserve, BaseDirection::Ltr, &[])
            .data
            .clusters
            .iter()
            .map(|cluster| cluster.advance)
            .sum::<f32>();
        assert!(
            (data.fit_advance - (word + 10.0 + if atomic { space } else { 0.0 })).abs() < 0.0001,
            "atomic={atomic}, fit={}, word={word}, space={space}",
            data.fit_advance
        );
    }
}

#[test]
fn custom_box_fit_is_line_local_after_breaking_and_reverting() {
    let mut layout = shape(
        "WW WW WW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Ltr,
        &[
            (3, InlineBoxKind::CustomOutOfFlow, 0.0),
            (6, InlineBoxKind::CustomOutOfFlow, 0.0),
        ],
    );
    let word = shape("WW", LineEdgeWhitespace::Collapse, BaseDirection::Ltr, &[])
        .calculate_content_widths()
        .max;
    let mut breaker = layout.break_lines();
    breaker.state_mut().set_layout_max_advance(200.0);
    breaker.state_mut().set_line_max_advance(word + 10.0);
    let checkpoint = breaker.state().clone();
    for _ in 0..2 {
        let mut count = 0;
        while let Some(data) = breaker.break_next() {
            if let YieldData::InlineBoxBreak(data) = data {
                assert!((data.fit_advance - word).abs() < 0.0001);
                assert!(!data.is_line_start);
                breaker
                    .state_mut()
                    .append_inline_box_to_line(data.advance, 0.0);
                count += 1;
            }
        }
        assert_eq!(count, 2);
        breaker.revert_to(checkpoint.clone());
    }
    breaker.finish();
}
