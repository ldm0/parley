// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::vec::Vec;

use super::{ColorBrush, FONT_FAMILY_LIST, create_font_context};
use crate::{
    Alignment, AlignmentOptions, BaseDirection, Cluster, FontContext, FontFamily, InlineBox,
    InlineBoxKind, Layout, LayoutContext, LineHeight, PositionedLayoutItem, StyleProperty,
    YieldData,
};

struct Fixture {
    fonts: FontContext,
    layouts: LayoutContext<ColorBrush>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            fonts: create_font_context(),
            layouts: LayoutContext::new(),
        }
    }

    fn shape(
        &mut self,
        text: &str,
        indices: &[usize],
        kind: InlineBoxKind,
        direction: BaseDirection,
    ) -> Layout<ColorBrush> {
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, text, 1.0, false);
        builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
        builder.push_default(StyleProperty::FontSize(20.0));
        builder.push_default(LineHeight::Absolute(24.0));
        builder.set_base_direction(direction);
        for (id, index) in indices.iter().copied().enumerate() {
            builder.push_inline_box(InlineBox {
                id: u64::try_from(id).unwrap(),
                kind,
                index,
                width: 9999.0,
                height: 9999.0,
            });
        }
        builder.build(text)
    }
}

fn assert_lines_match(
    reference: &Layout<ColorBrush>,
    actual: &Layout<ColorBrush>,
    context: impl core::fmt::Debug,
) {
    let lines = |layout: &Layout<ColorBrush>| {
        layout
            .lines()
            .map(|line| (line.text_range(), line.break_reason(), *line.metrics()))
            .collect::<Vec<_>>()
    };
    assert_eq!(lines(reference), lines(actual), "{context:?}");
    let glyphs = |layout: &Layout<ColorBrush>| {
        layout
            .lines()
            .flat_map(|line| line.items())
            .flat_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => run
                    .positioned_glyphs()
                    .map(|glyph| (glyph.id, glyph.x, glyph.y))
                    .collect::<Vec<_>>(),
                PositionedLayoutItem::InlineBox(_) => Vec::new(),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(glyphs(reference), glyphs(actual), "{context:?}");
}

#[test]
fn placeholders_do_not_break_unbreakable_text_or_overflowing_lines() {
    let mut fixture = Fixture::new();
    let text = "WWWWWW";
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        for width in [0.0, 1.0, 50.0, 200.0] {
            let mut reference = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, direction);
            reference.break_all_lines(Some(width));
            for index in 0..=text.len() {
                let mut actual = fixture.shape(text, &[index], InlineBoxKind::OutOfFlow, direction);
                actual.break_all_lines(Some(width));
                assert_lines_match(&reference, &actual, (index, width, direction));
                let boxes = actual
                    .lines()
                    .flat_map(|line| line.items())
                    .filter(|item| matches!(item, PositionedLayoutItem::InlineBox(_)))
                    .count();
                assert_eq!(boxes, 1, "the placeholder must survive layout");
            }
        }
    }
}

#[test]
fn placeholders_preserve_real_break_opportunities_and_alignment() {
    let mut fixture = Fixture::new();
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        for text in ["WW WW WW", "WW WW  ", "WW\nWW", "WW\n"] {
            for width in [30.0, 70.0, 200.0] {
                for alignment in [
                    Alignment::Start,
                    Alignment::End,
                    Alignment::Center,
                    Alignment::Justify,
                ] {
                    let mut reference =
                        fixture.shape(text, &[], InlineBoxKind::OutOfFlow, direction);
                    reference.break_all_lines(Some(width));
                    reference.align(alignment, AlignmentOptions::default());
                    for index in 0..=text.len() {
                        let mut actual =
                            fixture.shape(text, &[index], InlineBoxKind::OutOfFlow, direction);
                        actual.break_all_lines(Some(width));
                        actual.align(alignment, AlignmentOptions::default());
                        assert_lines_match(
                            &reference,
                            &actual,
                            (text, index, width, alignment, direction),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn placeholder_content_widths_preserve_adjacent_whitespace() {
    let mut fixture = Fixture::new();
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        for text in ["WW ", "WW   ", "WW WW ", "WW \nWW", "WW \n"] {
            let reference = fixture
                .shape(text, &[], InlineBoxKind::OutOfFlow, direction)
                .calculate_content_widths();
            for kind in [InlineBoxKind::OutOfFlow, InlineBoxKind::CustomOutOfFlow] {
                for index in 0..=text.len() {
                    let actual = fixture
                        .shape(text, &[index], kind, direction)
                        .calculate_content_widths();
                    assert_eq!(actual.min, reference.min, "{kind:?} at {index} in {text:?}");
                    assert_eq!(actual.max, reference.max, "{kind:?} at {index} in {text:?}");
                }
            }
        }
    }
}

#[test]
fn placeholder_items_do_not_contribute_advance_to_cluster_geometry() {
    let mut fixture = Fixture::new();
    let text = "WW WW";
    let mut reference = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, BaseDirection::Ltr);
    reference.break_all_lines(None);
    let offsets = |layout: &Layout<ColorBrush>| {
        layout
            .lines()
            .flat_map(|line| line.runs())
            .flat_map(|run| {
                run.visual_clusters()
                    .map(|cluster| cluster.visual_offset())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    for index in 0..=text.len() {
        let mut actual =
            fixture.shape(text, &[index], InlineBoxKind::OutOfFlow, BaseDirection::Ltr);
        actual.break_all_lines(None);
        assert_eq!(
            offsets(&reference),
            offsets(&actual),
            "placeholder at {index}"
        );
    }
}

#[test]
fn trailing_whitespace_crosses_multiple_placeholder_items() {
    let mut fixture = Fixture::new();
    let text = "WW   ";
    let mut reference = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, BaseDirection::Ltr);
    let mut actual = fixture.shape(
        text,
        &[3, 4, 5],
        InlineBoxKind::OutOfFlow,
        BaseDirection::Ltr,
    );
    reference.break_all_lines(Some(200.0));
    actual.break_all_lines(Some(200.0));
    reference.align(Alignment::End, AlignmentOptions::default());
    actual.align(Alignment::End, AlignmentOptions::default());
    assert_lines_match(&reference, &actual, text);
}

#[test]
fn placeholder_hit_testing_uses_flow_advance() {
    let mut fixture = Fixture::new();
    let text = "WW WW";
    let mut reference = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, BaseDirection::Ltr);
    reference.break_all_lines(None);
    for index in 0..=text.len() {
        let mut actual =
            fixture.shape(text, &[index], InlineBoxKind::OutOfFlow, BaseDirection::Ltr);
        actual.break_all_lines(None);
        for x in [1.0, 20.0, 38.0, 50.0, 70.0] {
            let hit = |layout: &Layout<ColorBrush>| {
                Cluster::from_point(layout, x, 12.0)
                    .map(|(cluster, side)| (cluster.text_range(), side))
            };
            assert_eq!(
                hit(&reference),
                hit(&actual),
                "placeholder at {index}, x={x}"
            );
        }
    }
}

#[test]
fn empty_line_after_newline_does_not_inherit_previous_advance() {
    let mut fixture = Fixture::new();
    for indices in [&[][..], &[3][..], &[3, 3][..]] {
        let mut layout = fixture.shape(
            "WW\n",
            indices,
            InlineBoxKind::OutOfFlow,
            BaseDirection::Ltr,
        );
        layout.break_all_lines(Some(100.0));
        let lines = layout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text_range(), 3..3);
        assert_eq!(lines[1].metrics().advance, 0.0);
        assert_eq!(lines[1].metrics().trailing_whitespace, 0.0);
        assert_eq!(
            lines[1].metrics().line_height,
            lines[0].metrics().line_height
        );
        assert_eq!(lines[1].metrics().ascent, lines[0].metrics().ascent);
    }
}

#[test]
fn custom_placeholders_yield_without_changing_wrapping_or_alignment() {
    let mut fixture = Fixture::new();
    let text = "WW WW   ";
    let mut reference = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, BaseDirection::Ltr);
    reference.break_all_lines(Some(30.0));
    reference.align(Alignment::End, AlignmentOptions::default());
    for index in 0..=text.len() {
        let mut actual = fixture.shape(
            text,
            &[index],
            InlineBoxKind::CustomOutOfFlow,
            BaseDirection::Ltr,
        );
        let mut breaker = actual.break_lines();
        breaker.state_mut().set_layout_max_advance(30.0);
        breaker.state_mut().set_line_max_advance(30.0);
        let mut yields = 0;
        while let Some(data) = breaker.break_next() {
            if let YieldData::InlineBoxBreak(data) = data {
                yields += 1;
                assert_eq!(data.inline_box_id, 0);
                breaker
                    .state_mut()
                    .append_inline_box_to_line(data.advance, 0.0);
            }
        }
        breaker.finish();
        assert_eq!(yields, 1);
        actual.align(Alignment::End, AlignmentOptions::default());
        assert_lines_match(&reference, &actual, index);
    }
}

#[test]
fn in_flow_boxes_still_contribute_size_and_break_opportunities() {
    let mut fixture = Fixture::new();
    let mut layout = fixture.shape("WWWW", &[2], InlineBoxKind::InFlow, BaseDirection::Ltr);
    assert!(layout.calculate_content_widths().min >= 9999.0);
    layout.break_all_lines(Some(40.0));
    assert_eq!(layout.len(), 3);
    let lines = layout.lines().collect::<Vec<_>>();
    assert_eq!(lines[1].metrics().advance, 9999.0);
    assert_eq!(lines[1].metrics().ascent, 9999.0);
}

#[test]
fn wrapped_rtl_lines_reset_only_their_trailing_whitespace_direction() {
    let mut fixture = Fixture::new();
    let mut layout = fixture.shape("WW WW", &[], InlineBoxKind::OutOfFlow, BaseDirection::Rtl);
    layout.break_all_lines(Some(50.0));
    let line = layout.get(0).unwrap();
    let runs = line.runs().collect::<Vec<_>>();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].text_range(), 2..3);
    assert!(runs[0].is_rtl(), "line-end space follows the RTL paragraph");
    assert_eq!(runs[1].text_range(), 0..2);
    assert!(
        !runs[1].is_rtl(),
        "Latin glyphs retain their shaping direction"
    );
    assert_eq!(line.metrics().trailing_whitespace, runs[0].advance());

    let mut rtl = fixture.shape(
        "مرحبا مرحبا",
        &[],
        InlineBoxKind::OutOfFlow,
        BaseDirection::Rtl,
    );
    rtl.break_all_lines(Some(1.0));
    let first = rtl.get(0).unwrap();
    let space = first
        .runs()
        .flat_map(|run| {
            run.visual_clusters()
                .filter(|cluster| cluster.source_char() == ' ')
                .map(|cluster| cluster.advance())
                .collect::<Vec<_>>()
        })
        .sum::<f32>();
    assert!(space > 0.0);
    assert_eq!(first.metrics().trailing_whitespace, space);
}

#[test]
fn breaker_checkpoint_retains_line_local_bidi_splits() {
    let mut fixture = Fixture::new();
    let text = "WW WW WW";
    let mut reference = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, BaseDirection::Rtl);
    reference.break_all_lines(Some(50.0));
    let mut actual = fixture.shape(text, &[], InlineBoxKind::OutOfFlow, BaseDirection::Rtl);
    let mut breaker = actual.break_lines();
    breaker.state_mut().set_layout_max_advance(50.0);
    breaker.state_mut().set_line_max_advance(50.0);
    assert!(matches!(
        breaker.break_next(),
        Some(YieldData::LineBreak(_))
    ));
    let checkpoint = breaker.state().clone();
    assert!(matches!(
        breaker.break_next(),
        Some(YieldData::LineBreak(_))
    ));
    breaker.revert_to(checkpoint);
    breaker.break_remaining(50.0);
    assert_lines_match(&reference, &actual, text);
}
