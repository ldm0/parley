// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{ColorBrush, FONT_FAMILY_LIST, create_font_context};
use crate::{
    BaseDirection, FontFamily, InlineBox, InlineBoxKind, Layout, LayoutContext, LineEdgeWhitespace,
    StyleProperty,
};

fn shape(
    text: &str,
    mode: LineEdgeWhitespace,
    direction: BaseDirection,
    boxes: &[(usize, InlineBoxKind)],
) -> Layout<ColorBrush> {
    let mut fonts = create_font_context();
    let mut context = LayoutContext::new();
    let mut builder = context.ranged_builder(&mut fonts, text, 1.0, false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.push_default(StyleProperty::FontSize(20.0));
    builder.push_default(StyleProperty::LineEdgeWhitespace(mode));
    builder.set_base_direction(direction);
    for (id, &(index, kind)) in boxes.iter().enumerate() {
        builder.push_inline_box(InlineBox {
            id: id as u64,
            index,
            kind,
            width: 0.0,
            height: 0.0,
        });
    }
    builder.build(text)
}

#[test]
fn collapsed_trailing_spaces_have_source_positions_but_no_advance_or_glyphs() {
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        let mut actual = shape("WW WW", LineEdgeWhitespace::Collapse, direction, &[]);
        let reference = shape("WW", LineEdgeWhitespace::Preserve, direction, &[])
            .calculate_content_widths()
            .max;
        actual.break_all_lines(Some(reference + 10.0));
        let line = actual.get(0).unwrap();
        assert_eq!(line.text_range(), 0..3);
        assert_eq!(line.metrics().advance, reference);
        assert_eq!(line.metrics().trailing_whitespace, 0.0);
        let space = crate::Cluster::from_byte_index(&actual, 2).unwrap();
        assert_eq!(space.text_range(), 2..3);
        assert_eq!(space.advance(), 0.0);
        assert_eq!(space.glyphs().count(), 0);
        assert_eq!(space.previous_logical().unwrap().text_range(), 1..2);
        assert_eq!(space.next_logical().unwrap().text_range(), 3..4);
    }
}

#[test]
fn collapsed_line_edges_are_local_to_each_width_probe() {
    let mut layout = shape(
        "WW WW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Ltr,
        &[],
    );
    let original_clusters = layout.data.clusters.clone();
    let natural = layout.calculate_content_widths().max;
    layout.break_all_lines(Some(natural * 0.6));
    assert_eq!(
        crate::Cluster::from_byte_index(&layout, 2)
            .unwrap()
            .advance(),
        0.0
    );
    layout.break_all_lines(Some(natural * 2.0));
    assert_eq!(layout.get(0).unwrap().metrics().advance, natural);
    assert!(
        crate::Cluster::from_byte_index(&layout, 2)
            .unwrap()
            .advance()
            > 0.0
    );
    assert_eq!(layout.data.clusters, original_clusters);
}

#[test]
fn inline_boundaries_and_positioned_boxes_do_not_stop_trimming() {
    let mut layout = shape(
        "WW \nWW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Ltr,
        &[
            (3, InlineBoxKind::StartBoundary),
            (3, InlineBoxKind::OutOfFlow),
            (6, InlineBoxKind::EndBoundary),
        ],
    );
    let reference = shape("WW", LineEdgeWhitespace::Preserve, BaseDirection::Ltr, &[])
        .calculate_content_widths()
        .max;
    layout.break_all_lines(Some(200.0));
    assert_eq!(layout.get(0).unwrap().metrics().advance, reference);
    for item in layout.get(0).unwrap().items() {
        if let crate::PositionedLayoutItem::InlineBox(item) = item {
            assert_eq!(item.x, reference);
        }
    }
}

#[test]
fn preserved_spaces_and_no_break_spaces_keep_their_geometry() {
    for (text, mode) in [
        ("WW ", LineEdgeWhitespace::Preserve),
        ("WW\u{a0}", LineEdgeWhitespace::Collapse),
    ] {
        let mut layout = shape(text, mode, BaseDirection::Ltr, &[]);
        let original = layout.data.clusters.iter().map(|c| c.advance).sum::<f32>();
        layout.break_all_lines(Some(200.0));
        assert_eq!(layout.get(0).unwrap().metrics().advance, original);
        assert!(
            crate::Cluster::from_byte_index(&layout, 2)
                .unwrap()
                .advance()
                > 0.0
        );
    }
}

#[test]
fn leading_spaces_do_not_consume_the_line_breaking_width() {
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        let reference = shape("WW WW", LineEdgeWhitespace::Collapse, direction, &[])
            .calculate_content_widths()
            .max;
        let mut layout = shape(
            "  WW WW",
            LineEdgeWhitespace::Collapse,
            direction,
            &[(1, InlineBoxKind::OutOfFlow)],
        );
        assert_eq!(layout.calculate_content_widths().max, reference);
        layout.break_all_lines(Some(reference));
        assert_eq!(layout.len(), 1);
        assert_eq!(layout.get(0).unwrap().metrics().advance, reference);
        for index in [0, 1] {
            assert!(
                crate::Cluster::from_byte_index(&layout, index)
                    .unwrap()
                    .is_collapsed()
            );
        }
    }
}

#[test]
fn forced_breaks_trim_spaces_without_collapsing_controls_or_source_ranges() {
    let mut layout = shape(
        "WW \u{202c}\nWW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Ltr,
        &[],
    );
    let reference = shape("WW", LineEdgeWhitespace::Collapse, BaseDirection::Ltr, &[])
        .calculate_content_widths()
        .max;
    assert_eq!(layout.calculate_content_widths().max, reference);
    layout.break_all_lines(Some(200.0));
    assert_eq!(layout.get(0).unwrap().metrics().advance, reference);
    assert!(
        crate::Cluster::from_byte_index(&layout, 2)
            .unwrap()
            .is_collapsed()
    );
    assert!(
        !crate::Cluster::from_byte_index(&layout, 3)
            .unwrap()
            .is_collapsed()
    );
    assert!(
        !crate::Cluster::from_byte_index(&layout, 6)
            .unwrap()
            .is_collapsed()
    );
}

#[test]
fn checkpoints_preserve_the_split_line_views() {
    let mut layout = shape(
        "WW WW WW",
        LineEdgeWhitespace::Collapse,
        BaseDirection::Rtl,
        &[],
    );
    let word = shape("WW", LineEdgeWhitespace::Collapse, BaseDirection::Ltr, &[])
        .calculate_content_widths()
        .max;
    {
        let mut breaker = layout.break_lines();
        breaker.state_mut().set_layout_max_advance(200.0);
        breaker.state_mut().set_line_max_advance(word + 10.0);
        assert!(matches!(
            breaker.break_next(),
            Some(crate::YieldData::LineBreak(_))
        ));
        let first_line = breaker.state().clone();
        assert!(matches!(
            breaker.break_next(),
            Some(crate::YieldData::LineBreak(_))
        ));
        breaker.revert_to(first_line);
        breaker.state_mut().set_line_max_advance(200.0);
        while breaker.break_next().is_some() {}
    }
    assert_eq!(layout.len(), 2);
    assert_eq!(layout.get(0).unwrap().text_range(), 0..3);
    assert_eq!(layout.get(1).unwrap().text_range(), 3..8);
    assert!(
        crate::Cluster::from_byte_index(&layout, 2)
            .unwrap()
            .is_collapsed()
    );
    assert!(
        !crate::Cluster::from_byte_index(&layout, 5)
            .unwrap()
            .is_collapsed()
    );
}

#[test]
fn justification_counts_only_surviving_spaces() {
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        let mut layout = shape(" WW WW  WW", LineEdgeWhitespace::Collapse, direction, &[]);
        let pair = shape("WW WW", LineEdgeWhitespace::Collapse, direction, &[])
            .calculate_content_widths()
            .max;
        let width = pair + 12.0;
        layout.break_all_lines(Some(width));
        layout.align(
            crate::Alignment::Justify,
            crate::AlignmentOptions::default(),
        );
        let line = layout.get(0).unwrap();
        assert_eq!(line.text_range(), 0..8);
        let advance: f32 = line
            .runs()
            .map(|run| run.visual_clusters().map(|c| c.advance()).sum::<f32>())
            .sum();
        assert_eq!(advance, width);
        for index in [0, 6, 7] {
            let cluster = crate::Cluster::from_byte_index(&layout, index).unwrap();
            assert!(cluster.is_collapsed());
            assert_eq!(cluster.advance(), 0.0);
        }
    }
}

#[test]
fn preserved_style_ranges_stop_line_edge_collapse() {
    let text = "  WW  ";
    let mut fonts = create_font_context();
    let mut context = LayoutContext::<ColorBrush>::new();
    let mut builder = context.ranged_builder(&mut fonts, text, 1.0, false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.push_default(StyleProperty::FontSize(20.0));
    builder.push_default(StyleProperty::LineEdgeWhitespace(
        LineEdgeWhitespace::Collapse,
    ));
    for range in [0..1, 5..6] {
        builder.push(
            StyleProperty::LineEdgeWhitespace(LineEdgeWhitespace::Preserve),
            range,
        );
    }
    let mut layout = builder.build(text);
    let source_advance: f32 = layout.data.clusters.iter().map(|c| c.advance).sum();
    layout.break_all_lines(Some(200.0));
    assert_eq!(layout.get(0).unwrap().metrics().advance, source_advance);
    for index in [0, 1, 4, 5] {
        let cluster = crate::Cluster::from_byte_index(&layout, index).unwrap();
        assert!(!cluster.is_collapsed());
        assert!(cluster.advance() > 0.0);
    }
}
