// Copyright 2026 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Resolve one paragraph including inline objects, without changing the text
//! or byte offsets consumed by shaping, line breaking, and editing.

use core::slice;

use crate::inline_box::InlineBoxInput;
use crate::{BaseDirection, Brush, InlineBoxKind, LayoutContext};

pub(super) fn resolve<B: Brush>(
    lcx: &mut LayoutContext<B>,
    text: &str,
    base_direction: BaseDirection,
) {
    let object = '\u{fffc}';
    let object_properties = lcx.analysis_data_sources.properties(object);
    let object_info = (
        object_properties.bidi_class(),
        lcx.analysis_data_sources.brackets().get(object),
    );
    let mut characters = text.char_indices().zip(&lcx.info).peekable();
    let mut objects = lcx
        .inline_boxes
        .iter()
        .filter(|input| !input.inline_box.kind.is_boundary())
        .peekable();
    let input = core::iter::from_fn(|| {
        if objects.peek().is_some_and(|input| {
            characters
                .peek()
                .is_none_or(|((offset, _), _)| input.inline_box.index <= *offset)
        }) {
            objects.next();
            return Some((object, object_info));
        }
        characters
            .next()
            .map(|((_, character), (info, _))| (character, (info.bidi_class, info.bracket)))
    });
    lcx.bidi.resolve(
        input,
        match base_direction {
            BaseDirection::Auto => None,
            BaseDirection::Ltr => Some(0),
            BaseDirection::Rtl => Some(1),
        },
    );

    // Project the combined analysis back onto the original items. Opening
    // boundaries follow subsequent content; closing boundaries follow preceding
    // content. Neither introduces a neutral character into the paragraph.
    let mut levels = lcx.bidi.levels().iter();
    let mut previous = lcx.bidi.base_level();
    let mut boxes = lcx.inline_boxes.iter_mut().peekable();
    for ((offset, _), (info, _)) in text.char_indices().zip(&mut lcx.info) {
        while boxes
            .peek()
            .is_some_and(|input| input.inline_box.index <= offset)
        {
            assign_box_level(boxes.next().unwrap(), &mut levels, &mut previous);
        }
        previous = *levels
            .next()
            .expect("every character has a resolved bidi level");
        info.bidi_level = previous;
    }
    for input in boxes {
        assign_box_level(input, &mut levels, &mut previous);
    }
    debug_assert!(levels.as_slice().is_empty());
}

fn assign_box_level(
    input: &mut InlineBoxInput,
    levels: &mut slice::Iter<'_, u8>,
    previous: &mut u8,
) {
    input.bidi_level = match input.inline_box.kind {
        InlineBoxKind::InFlow | InlineBoxKind::OutOfFlow | InlineBoxKind::CustomOutOfFlow => {
            *previous = *levels
                .next()
                .expect("every neutral object has a resolved bidi level");
            *previous
        }
        InlineBoxKind::StartBoundary | InlineBoxKind::TextBoundary => {
            levels.as_slice().first().copied().unwrap_or(*previous)
        }
        InlineBoxKind::EndBoundary => *previous,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::{ResolvedStyle, StyleRun};
    use crate::{InlineBox, InlineBoxKind};
    use alloc::{vec, vec::Vec};

    fn analyze(
        text: &str,
        direction: BaseDirection,
        boxes: &[(usize, InlineBoxKind)],
    ) -> LayoutContext {
        let mut context = LayoutContext::new();
        context.style_table.push(ResolvedStyle::default());
        context.style_runs.push(StyleRun {
            style_index: 0,
            range: 0..text.len(),
        });
        context
            .inline_boxes
            .extend(boxes.iter().map(|&(index, kind)| {
                InlineBoxInput::new(InlineBox {
                    id: 0,
                    kind,
                    index,
                    width: 10.0,
                    height: 10.0,
                })
            }));
        crate::analysis::analyze_text(&mut context, text, None, direction);
        context
    }

    #[test]
    fn explicit_direction_controls_empty_neutral_numeric_and_mixed_paragraphs() {
        for (text, direction, base, levels) in [
            ("", BaseDirection::Rtl, 1, vec![1]),
            (".", BaseDirection::Rtl, 1, vec![1]),
            ("12", BaseDirection::Rtl, 1, vec![2, 2]),
            ("abc", BaseDirection::Rtl, 1, vec![2, 2, 2]),
            ("א a", BaseDirection::Ltr, 0, vec![1, 0, 0]),
            ("א a", BaseDirection::Auto, 1, vec![1, 1, 2]),
            ("abc", BaseDirection::Auto, 0, vec![0, 0, 0]),
        ] {
            let context = analyze(text, direction, &[]);
            assert_eq!(context.bidi.base_level(), base, "{text:?}, {direction:?}");
            assert_eq!(
                context
                    .info
                    .iter()
                    .map(|(info, _)| info.bidi_level)
                    .collect::<Vec<_>>(),
                levels,
                "{text:?}, {direction:?}"
            );
        }
    }

    #[test]
    fn boxes_resolve_as_neutrals_not_the_previous_run() {
        for (text, direction, offsets, expected) in [
            ("", BaseDirection::Rtl, vec![0, 0], vec![1, 1]),
            ("אב", BaseDirection::Ltr, vec![0, 2, 4], vec![0, 1, 0]),
            ("ab", BaseDirection::Rtl, vec![0, 1, 2], vec![1, 2, 1]),
            ("12", BaseDirection::Rtl, vec![1], vec![1]),
            ("אב", BaseDirection::Auto, vec![0, 0, 4], vec![1, 1, 1]),
        ] {
            let boxes = offsets
                .into_iter()
                .map(|index| (index, InlineBoxKind::InFlow))
                .collect::<Vec<_>>();
            let context = analyze(text, direction, &boxes);
            assert_eq!(
                context
                    .inline_boxes
                    .iter()
                    .map(|input| input.bidi_level)
                    .collect::<Vec<_>>(),
                expected,
                "{text:?}, {direction:?}"
            );
        }
    }

    #[test]
    fn transparent_boundaries_follow_their_content_without_changing_analysis() {
        let context = analyze(
            "אa",
            BaseDirection::Ltr,
            &[
                (0, InlineBoxKind::StartBoundary),
                (2, InlineBoxKind::EndBoundary),
                (2, InlineBoxKind::StartBoundary),
                (3, InlineBoxKind::EndBoundary),
            ],
        );
        assert_eq!(
            context
                .info
                .iter()
                .map(|(info, _)| info.bidi_level)
                .collect::<Vec<_>>(),
            [1, 0]
        );
        assert_eq!(
            context
                .inline_boxes
                .iter()
                .map(|input| input.bidi_level)
                .collect::<Vec<_>>(),
            [1, 1, 0, 0]
        );
    }

    #[test]
    fn boxes_and_multibyte_text_share_one_analysis_inside_isolates() {
        // Compare projection to literal replacement characters, including an
        // isolate and multiple objects at the same byte offset.
        let boxes = [
            (0, InlineBoxKind::InFlow),
            (5, InlineBoxKind::InFlow),
            (5, InlineBoxKind::InFlow),
            (6, InlineBoxKind::InFlow),
            (12, InlineBoxKind::InFlow),
        ];
        let projected = analyze("א\u{2066}ab\u{2069}ב", BaseDirection::Rtl, &boxes);
        let literal = analyze(
            "\u{fffc}א\u{2066}\u{fffc}\u{fffc}a\u{fffc}b\u{2069}ב\u{fffc}",
            BaseDirection::Rtl,
            &[],
        );
        let levels = literal
            .info
            .iter()
            .map(|(info, _)| info.bidi_level)
            .collect::<Vec<_>>();
        assert_eq!(
            projected
                .inline_boxes
                .iter()
                .map(|input| input.bidi_level)
                .collect::<Vec<_>>(),
            [levels[0], levels[3], levels[4], levels[6], levels[10]]
        );
        assert_eq!(
            projected
                .info
                .iter()
                .map(|(info, _)| info.bidi_level)
                .collect::<Vec<_>>(),
            [
                levels[1], levels[2], levels[5], levels[7], levels[8], levels[9]
            ]
        );
    }
}
