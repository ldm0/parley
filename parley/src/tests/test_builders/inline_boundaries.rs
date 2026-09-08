// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::vec::Vec;

use super::{ColorBrush, FONT_FAMILY_LIST, create_font_context};
use crate::{
    BaseDirection, FontFamily, InlineBox, InlineBoxKind, Layout, LayoutContext, LineHeight,
    PositionedLayoutItem, StyleProperty,
};

fn shape(
    text: &str,
    boxes: &[(usize, InlineBoxKind, f32)],
    direction: BaseDirection,
) -> Layout<ColorBrush> {
    let mut fonts = create_font_context();
    let mut context = LayoutContext::new();
    let mut builder = context.ranged_builder(&mut fonts, text, 1.0, false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.push_default(StyleProperty::FontSize(20.0));
    builder.push_default(LineHeight::Absolute(24.0));
    builder.set_base_direction(direction);
    for (id, &(index, kind, width)) in boxes.iter().enumerate() {
        builder.push_inline_box(InlineBox {
            id: id as u64,
            kind,
            index,
            width,
            height: 0.0,
        });
    }
    builder.build(text)
}

#[test]
fn boundaries_do_not_split_unbreakable_words() {
    let text = "WWWWWW";
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        for (start, end) in [(0, 6), (1, 4), (2, 2)] {
            for width in [0.0, 1.0, 40.0, 200.0] {
                let mut reference = shape(text, &[], direction);
                let mut actual = shape(
                    text,
                    &[
                        (start, InlineBoxKind::StartBoundary, 0.0),
                        (end, InlineBoxKind::EndBoundary, 0.0),
                    ],
                    direction,
                );
                let actual_widths = actual.calculate_content_widths();
                let reference_widths = reference.calculate_content_widths();
                assert_eq!(actual_widths.min, reference_widths.min);
                assert_eq!(actual_widths.max, reference_widths.max);
                reference.break_all_lines(Some(width));
                actual.break_all_lines(Some(width));
                let lines = |layout: &Layout<ColorBrush>| {
                    layout
                        .lines()
                        .map(|line| (line.text_range(), *line.metrics()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    lines(&actual),
                    lines(&reference),
                    "{direction:?}, {start}..{end}, width {width}"
                );
            }
        }
    }
}

#[test]
fn opening_edges_stay_with_following_text() {
    let mut layout = shape(
        "WW WW",
        &[
            (3, InlineBoxKind::StartBoundary, 20.0),
            (5, InlineBoxKind::EndBoundary, 0.0),
        ],
        BaseDirection::Ltr,
    );
    layout.break_all_lines(Some(85.0));
    let lines = layout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text_range(), 0..3);
    assert_eq!(lines[1].text_range(), 3..5);
    assert!(
        lines[0]
            .items()
            .all(|item| !matches!(item, PositionedLayoutItem::InlineBox(_)))
    );
    assert_eq!(
        lines[1]
            .items()
            .filter(|item| matches!(item, PositionedLayoutItem::InlineBox(_)))
            .count(),
        2
    );
}

#[test]
fn closing_edge_overflow_rewinds_the_attached_word() {
    let mut layout = shape(
        "WW WW",
        &[
            (3, InlineBoxKind::StartBoundary, 0.0),
            (5, InlineBoxKind::EndBoundary, 20.0),
        ],
        BaseDirection::Ltr,
    );
    layout.break_all_lines(Some(85.0));
    let lines = layout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text_range(), 0..3);
    assert_eq!(lines[1].text_range(), 3..5);
    assert_eq!(
        lines[1]
            .items()
            .filter(|item| matches!(item, PositionedLayoutItem::InlineBox(_)))
            .count(),
        2
    );
}

#[test]
fn intrinsic_widths_include_edges_in_their_unbreakable_segment() {
    let text = shape("WW", &[], BaseDirection::Ltr).calculate_content_widths();
    let actual = shape(
        "WW WW",
        &[
            (3, InlineBoxKind::StartBoundary, 10.0),
            (5, InlineBoxKind::EndBoundary, 20.0),
        ],
        BaseDirection::Ltr,
    )
    .calculate_content_widths();
    assert_eq!(actual.min, text.min + 30.0);
    let reference = shape("WW WW", &[], BaseDirection::Ltr).calculate_content_widths();
    assert_eq!(actual.max, reference.max + 30.0);
}

#[test]
fn negative_edges_reduce_intrinsic_and_final_advances() {
    let text = "WWWW";
    let reference = shape(text, &[], BaseDirection::Ltr).calculate_content_widths();
    for (start, end) in [(20.0, -40.0), (-20.0, 0.0), (0.0, -20.0)] {
        let mut actual = shape(
            text,
            &[
                (0, InlineBoxKind::StartBoundary, start),
                (text.len(), InlineBoxKind::EndBoundary, end),
            ],
            BaseDirection::Ltr,
        );
        let widths = actual.calculate_content_widths();
        assert_eq!(widths.min, reference.min - 20.0);
        assert_eq!(widths.max, reference.max - 20.0);
        actual.break_all_lines(Some(1.0));
        assert_eq!(actual.len(), 1);
        assert_eq!(
            actual.get(0).unwrap().metrics().advance,
            reference.max - 20.0
        );
    }
}

#[test]
fn boundary_edges_wrap_with_atomic_contents() {
    let mut layout = shape(
        "WW ",
        &[
            (3, InlineBoxKind::StartBoundary, 10.0),
            (3, InlineBoxKind::InFlow, 50.0),
            (3, InlineBoxKind::EndBoundary, 10.0),
        ],
        BaseDirection::Ltr,
    );
    layout.break_all_lines(Some(85.0));
    let lines = layout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    let boxes = |line: &crate::Line<'_, ColorBrush>| {
        line.items()
            .filter_map(|item| match item {
                PositionedLayoutItem::InlineBox(item) => Some(item.id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(boxes(&lines[0]).is_empty());
    assert_eq!(boxes(&lines[1]), [0, 1, 2]);
}

#[test]
fn collapsed_text_retains_its_place_between_opening_and_positioned_items() {
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        for text in ["WW WW", "WWWW"] {
            let index = if text.contains(' ') { 3 } else { 2 };
            let mut layout = shape(
                text,
                &[
                    (index, InlineBoxKind::StartBoundary, 0.0),
                    (index, InlineBoxKind::TextBoundary, 0.0),
                    (index, InlineBoxKind::OutOfFlow, 0.0),
                    (text.len(), InlineBoxKind::EndBoundary, 0.0),
                ],
                direction,
            );
            let mut reference = shape(text, &[], direction);
            let actual_widths = layout.calculate_content_widths();
            let expected_widths = reference.calculate_content_widths();
            assert_eq!(actual_widths.min, expected_widths.min);
            assert_eq!(actual_widths.max, expected_widths.max);
            layout.break_all_lines(Some(60.0));
            reference.break_all_lines(Some(60.0));
            assert_eq!(layout.len(), reference.len());
            assert_eq!(
                layout.get(0).unwrap().text_range(),
                reference.get(0).unwrap().text_range()
            );
            let first_line_boxes = layout
                .get(0)
                .unwrap()
                .items()
                .filter_map(|item| match item {
                    PositionedLayoutItem::InlineBox(item) => Some(item.id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for id in [0, 1, 2] {
                assert!(
                    first_line_boxes.contains(&id),
                    "{direction:?}: {text:?}, {first_line_boxes:?}"
                );
            }
        }
    }
}

#[test]
fn closing_edge_overflow_does_not_enable_disabled_wrapping() {
    let mut layout = shape(
        "WW WW",
        &[
            (0, InlineBoxKind::StartBoundary, 0.0),
            (3, InlineBoxKind::InFlow, 10.0),
            (5, InlineBoxKind::EndBoundary, 20.0),
        ],
        BaseDirection::Ltr,
    );
    for style in &mut layout.data.styles {
        style.text_wrap_mode = crate::TextWrapMode::NoWrap;
    }
    let widths = layout.calculate_content_widths();
    assert_eq!(widths.min, widths.max);
    layout.break_all_lines(Some(100.0));
    assert_eq!(layout.len(), 1);
    assert_eq!(layout.get(0).unwrap().metrics().advance, widths.max);
}
