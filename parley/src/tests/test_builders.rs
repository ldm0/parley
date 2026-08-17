// Copyright 2025 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Test that the various builders produce the same results.

use std::{borrow::Cow, path::PathBuf, sync::Arc, vec::Vec};

use fontique::{Collection, CollectionOptions, FontStyle, FontWeight, FontWidth, SourceCache};
use parlance::FontFamilyName;
use peniko::{Blob, color::palette};

use super::utils::{ColorBrush, asserts::assert_eq_layout_data};
use crate::{
    FontContext, FontFamily, FontFeatures, FontVariations, InlineBox, InlineBoxKind, Layout,
    LayoutContext, LineHeight, OverflowWrap, RangedBuilder, StyleProperty, StyleRunBuilder,
    TextStyle, TextWrapMode, TreeBuilder, WordBreak,
};

// TODO: `FONT_FAMILY_LIST`, `load_fonts`, and `create_font_context` are
// duplicated between this crate and `parley_test`. We can't move the builder
// tests into `parley_test` because they use private APIs, but should eventually
// figure out some way to reduce the duplication.
const FONT_FAMILY_LIST: &[FontFamilyName<'_>] = &[
    FontFamilyName::Named(Cow::Borrowed("Roboto")),
    FontFamilyName::Named(Cow::Borrowed("Noto Kufi Arabic")),
];

pub(crate) fn load_fonts(
    collection: &mut Collection,
    font_dirs: impl Iterator<Item = PathBuf>,
) -> std::io::Result<()> {
    for dir in font_dirs {
        let paths = std::fs::read_dir(dir)?;
        for entry in paths {
            let entry = entry?;
            if !entry.metadata()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| !["ttf", "otf", "ttc", "otc"].contains(&ext))
            {
                continue;
            }
            let font_data = std::fs::read(&path)?;
            collection.register_fonts(Blob::new(Arc::new(font_data)), None);
        }
    }
    Ok(())
}

fn create_font_context() -> FontContext {
    let mut collection = Collection::new(CollectionOptions {
        shared: false,
        system_fonts: false,
    });
    load_fonts(&mut collection, parley_dev::font_dirs()).unwrap();
    for font in FONT_FAMILY_LIST {
        if let FontFamilyName::Named(font_name) = font {
            collection
                .family_id(font_name)
                .unwrap_or_else(|| panic!("{font_name} font not found"));
        }
    }
    FontContext {
        collection,
        source_cache: SourceCache::default(),
    }
}

/// Set of options for [`build_layout_with_ranged`].
struct RangedOptions<'a> {
    scale: f32,
    quantize: bool,
    max_advance: Option<f32>,
    text: &'a str,
}

/// Set of options for [`build_layout_with_tree`].
struct TreeOptions<'a, 'b> {
    scale: f32,
    quantize: bool,
    max_advance: Option<f32>,
    root_style: &'a TextStyle<'b, 'b, ColorBrush>,
}

/// Generates a `Layout` with a ranged builder.
fn build_layout_with_ranged(
    fcx: &mut FontContext,
    lcx: &mut LayoutContext<ColorBrush>,
    opts: &RangedOptions<'_>,
    with_builder: impl Fn(&mut RangedBuilder<'_, ColorBrush>),
) -> Layout<ColorBrush> {
    let mut rb = lcx.ranged_builder(fcx, opts.text, opts.scale, opts.quantize);
    with_builder(&mut rb);
    let mut layout = rb.build(opts.text);
    layout.break_all_lines(opts.max_advance);
    layout
}

/// Generates a `Layout` with a tree builder.
fn build_layout_with_tree(
    fcx: &mut FontContext,
    lcx: &mut LayoutContext<ColorBrush>,
    opts: &TreeOptions<'_, '_>,
    with_builder: impl Fn(&mut TreeBuilder<'_, ColorBrush>),
) -> Layout<ColorBrush> {
    let mut tb = lcx.tree_builder(fcx, opts.scale, opts.quantize, opts.root_style);
    with_builder(&mut tb);
    let (mut layout, _) = tb.build();
    layout.break_all_lines(opts.max_advance);
    layout
}

/// Generates a `Layout` with a style run builder.
fn build_layout_with_style_runs(
    fcx: &mut FontContext,
    lcx: &mut LayoutContext<ColorBrush>,
    opts: &RangedOptions<'_>,
    with_builder: impl Fn(&mut StyleRunBuilder<'_, ColorBrush>),
) -> Layout<ColorBrush> {
    let mut rb = lcx.style_run_builder(fcx, opts.text, opts.scale, opts.quantize);
    with_builder(&mut rb);
    let mut layout = rb.build(opts.text);
    layout.break_all_lines(opts.max_advance);
    layout
}

/// Computes layout in various ways to ensure they all produce the same result.
///
/// ```text
/// LayoutContext A - Ranged
/// LayoutContext A - Ranged for idempotency
///
/// LayoutContext B - Tree
/// LayoutContext B - Tree for idempotency
///
/// LayoutContext C - Ranged for dirt
/// LayoutContext C - Tree from dirty
///
/// LayoutContext D - Tree for dirt
/// LayoutContext D - Ranged from dirty
/// ```
fn assert_builders_produce_same_result<'b>(
    text: &str,
    scale: f32,
    quantize: bool,
    max_advance: Option<f32>,
    root_style: &TextStyle<'b, 'b, ColorBrush>,
    with_ranged_builder: impl Fn(&mut RangedBuilder<'_, ColorBrush>),
    with_tree_builder: impl Fn(&mut TreeBuilder<'_, ColorBrush>),
    expect_empty: bool,
) {
    let mut fcx = create_font_context();

    let mut lcx_a: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut lcx_b: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut lcx_c: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut lcx_d: LayoutContext<ColorBrush> = LayoutContext::new();

    let ropts = RangedOptions {
        scale,
        quantize,
        max_advance,
        text,
    };
    let topts = TreeOptions {
        scale,
        quantize,
        max_advance,
        root_style,
    };

    // Source of truth - ranged builder from a clean layout context
    let layout_truth = build_layout_with_ranged(&mut fcx, &mut lcx_a, &ropts, &with_ranged_builder);
    assert!(
        layout_truth.data.runs.is_empty() == expect_empty,
        "expected runs to exist for lcx_a_rb_one"
    );

    // Testing idempotence of ranged builder creation
    let layout = build_layout_with_ranged(&mut fcx, &mut lcx_a, &ropts, &with_ranged_builder);
    assert!(
        layout.data.runs.is_empty() == expect_empty,
        "expected runs to exist for lcx_a_rb_two"
    );
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_a_rb_two");

    // Basic builder compatibility - tree builder from a clean layout context
    let layout = build_layout_with_tree(&mut fcx, &mut lcx_b, &topts, &with_tree_builder);
    assert!(
        layout.data.runs.is_empty() == expect_empty,
        "expected runs to exist for lcx_b_tb_one"
    );
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_b_tb_one");

    // Testing idempotence of tree builder creation
    let layout = build_layout_with_tree(&mut fcx, &mut lcx_b, &topts, &with_tree_builder);
    assert!(
        layout.data.runs.is_empty() == expect_empty,
        "expected runs to exist for lcx_b_tb_two"
    );
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_b_tb_two");

    // Priming a fresh layout context with ranged builder creation
    let _ = build_layout_with_ranged(&mut fcx, &mut lcx_c, &ropts, &with_ranged_builder);

    // Testing tree builder creation with a dirty layout context
    let layout = build_layout_with_tree(&mut fcx, &mut lcx_c, &topts, &with_tree_builder);
    assert!(
        layout.data.runs.is_empty() == expect_empty,
        "expected runs to exist for lcx_c_tb_one"
    );
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_c_tb_one");

    // Priming a fresh layout context with tree builder creation
    let _ = build_layout_with_tree(&mut fcx, &mut lcx_d, &topts, &with_tree_builder);

    // Testing ranged builder creation with a dirty layout context
    let layout = build_layout_with_ranged(&mut fcx, &mut lcx_d, &ropts, &with_ranged_builder);
    assert!(
        layout.data.runs.is_empty() == expect_empty,
        "expected runs to exist for lcx_d_rb_one"
    );
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_d_rb_one");
}

/// Returns a root style that uses non-default values.
///
/// The [`TreeBuilder`] version of [`set_root_style`].
fn create_root_style() -> TextStyle<'static, 'static, ColorBrush> {
    TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        font_size: 20.,
        font_width: FontWidth::CONDENSED,
        font_style: FontStyle::Italic,
        font_weight: FontWeight::BOLD,
        font_variations: FontVariations::empty(), // TODO: Set a non-default value
        font_features: FontFeatures::empty(),     // TODO: Set a non-default value
        locale: Some("en-US".parse().unwrap()),
        brush: ColorBrush::new(palette::css::GREEN),
        has_underline: true,
        underline_offset: Some(2.),
        underline_size: Some(3.5),
        underline_brush: Some(ColorBrush::new(palette::css::CYAN)),
        has_strikethrough: true,
        strikethrough_offset: Some(1.3),
        strikethrough_size: Some(1.7),
        strikethrough_brush: Some(ColorBrush::new(palette::css::BEIGE)),
        line_height: LineHeight::Absolute(30.),
        word_spacing: 2.,
        letter_spacing: 1.5,
        word_break: WordBreak::BreakAll,
        overflow_wrap: OverflowWrap::Anywhere,
        text_wrap_mode: TextWrapMode::Wrap,
    }
}

/// Sets a root style with non-default values.
///
/// The [`RangedBuilder`] version of [`create_root_style`].
fn set_root_style(rb: &mut RangedBuilder<'_, ColorBrush>) {
    rb.push_default(FontFamily::from(FONT_FAMILY_LIST));
    rb.push_default(StyleProperty::FontSize(20.));
    rb.push_default(StyleProperty::FontWidth(FontWidth::CONDENSED));
    rb.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    rb.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
    rb.push_default(FontVariations::empty());
    rb.push_default(FontFeatures::empty());
    rb.push_default(StyleProperty::Locale(Some("en-US".parse().unwrap())));
    rb.push_default(StyleProperty::Brush(ColorBrush::new(palette::css::GREEN)));
    rb.push_default(StyleProperty::Underline(true));
    rb.push_default(StyleProperty::UnderlineOffset(Some(2.)));
    rb.push_default(StyleProperty::UnderlineSize(Some(3.5)));
    rb.push_default(StyleProperty::UnderlineBrush(Some(ColorBrush::new(
        palette::css::CYAN,
    ))));
    rb.push_default(StyleProperty::Strikethrough(true));
    rb.push_default(StyleProperty::StrikethroughOffset(Some(1.3)));
    rb.push_default(StyleProperty::StrikethroughSize(Some(1.7)));
    rb.push_default(StyleProperty::StrikethroughBrush(Some(ColorBrush::new(
        palette::css::BEIGE,
    ))));
    rb.push_default(LineHeight::Absolute(30.));
    rb.push_default(StyleProperty::WordSpacing(2.));
    rb.push_default(StyleProperty::LetterSpacing(1.5));
    rb.push_default(StyleProperty::WordBreak(WordBreak::BreakAll));
    rb.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
}

/// Test that all the builders have the same default behavior.
#[test]
fn builders_default() {
    let text = "Builders often wear hard hats for safety while working on construction sites.";
    let scale = 2.;
    let quantize = false;
    let max_advance = Some(50.);
    let root_style = TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        ..TextStyle::default()
    };

    let with_ranged_builder = |rb: &mut RangedBuilder<'_, ColorBrush>| {
        rb.push_default(FontFamily::from(FONT_FAMILY_LIST));
    };
    let with_tree_builder = |tb: &mut TreeBuilder<'_, ColorBrush>| {
        tb.push_text(text);
    };

    assert_builders_produce_same_result(
        text,
        scale,
        quantize,
        max_advance,
        &root_style,
        with_ranged_builder,
        with_tree_builder,
        false,
    );
}

/// Test that `StyleRunBuilder` produces the same result as `RangedBuilder` when given equivalent
/// styles.
#[test]
fn builders_style_runs_match_ranged() {
    let text = "Builders often wear hard hats.";
    let scale = 2.;
    let quantize = false;
    let max_advance = Some(120.);

    let root_style: TextStyle<'static, 'static, ColorBrush> = TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        ..TextStyle::default()
    };

    let split = text.len() / 2;
    let mut modified_style = root_style.clone();
    modified_style.font_size = 40.;
    modified_style.letter_spacing = 1.25;

    let mut fcx = create_font_context();
    let mut lcx_a: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut lcx_b: LayoutContext<ColorBrush> = LayoutContext::new();

    let ropts = RangedOptions {
        scale,
        quantize,
        max_advance,
        text,
    };

    let ranged = build_layout_with_ranged(&mut fcx, &mut lcx_a, &ropts, |rb| {
        rb.push_default(FontFamily::from(FONT_FAMILY_LIST));
        rb.push(
            StyleProperty::FontSize(modified_style.font_size),
            split..text.len(),
        );
        rb.push(
            StyleProperty::LetterSpacing(modified_style.letter_spacing),
            split..text.len(),
        );
    });

    let runs = build_layout_with_style_runs(&mut fcx, &mut lcx_b, &ropts, |rb| {
        let family: FontFamily<'static> = root_style.font_family.clone().into_owned();
        let root_run: TextStyle<'static, 'static, ColorBrush> = TextStyle {
            font_family: family.clone(),
            ..root_style.clone()
        };

        let modified_run: TextStyle<'static, 'static, ColorBrush> = TextStyle {
            font_family: family,
            ..modified_style.clone()
        };

        let root_index = rb.push_style(root_run);
        let modified_index = rb.push_style(modified_run);
        rb.push_style_run(root_index, 0..split);
        rb.push_style_run(modified_index, split..text.len());
    });

    assert_eq_layout_data(&ranged.data, &runs.data, "style_runs_match_ranged");
}

/// Test that `StyleRunBuilder` handles a first run whose style table index is not zero.
#[test]
fn style_runs_first_run_can_use_nonzero_style_index() {
    let text = "Builders often wear hard hats.";
    let scale = 2.;
    let quantize = false;
    let max_advance = Some(50.);
    let root_style = create_root_style();
    let mut modified_style = root_style.clone();
    modified_style.font_size = 40.;

    let mut fcx = create_font_context();
    let mut lcx_a: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut lcx_b: LayoutContext<ColorBrush> = LayoutContext::new();

    let ropts = RangedOptions {
        scale,
        quantize,
        max_advance,
        text,
    };

    let ranged = build_layout_with_ranged(&mut fcx, &mut lcx_a, &ropts, |rb| {
        set_root_style(rb);
        rb.push(
            StyleProperty::FontSize(modified_style.font_size),
            0..text.len(),
        );
    });

    let runs = build_layout_with_style_runs(&mut fcx, &mut lcx_b, &ropts, |rb| {
        let family: FontFamily<'static> = root_style.font_family.clone().into_owned();
        let root_run: TextStyle<'static, 'static, ColorBrush> = TextStyle {
            font_family: family.clone(),
            ..root_style.clone()
        };

        let modified_run: TextStyle<'static, 'static, ColorBrush> = TextStyle {
            font_family: family,
            ..modified_style.clone()
        };

        let _root_index = rb.push_style(root_run);
        let modified_index = rb.push_style(modified_run);
        rb.push_style_run(modified_index, 0..text.len());
    });

    assert_eq_layout_data(
        &ranged.data,
        &runs.data,
        "style_runs_first_run_can_use_nonzero_style_index",
    );
}

#[test]
fn ranged_root_wrap_mode_precedes_the_first_ranged_run() {
    let text = "x";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.ranged_builder(&mut fcx, text, 1.0, false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.push_default(StyleProperty::TextWrapMode(TextWrapMode::NoWrap));
    builder.push(
        StyleProperty::TextWrapMode(TextWrapMode::Wrap),
        0..text.len(),
    );
    for id in 0..2 {
        builder.push_inline_box(InlineBox {
            id,
            kind: InlineBoxKind::InFlow,
            index: 0,
            width: 64.0,
            height: 20.0,
        });
    }

    let mut layout = builder.build(text);
    layout.break_all_lines(Some(64.0));
    assert_eq!(layout.lines().count(), 1);
}

#[test]
fn tree_inline_edges_are_zero_length_items() {
    let text = "";
    let root_style = TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        ..TextStyle::default()
    };
    let push_ranged_edges = |builder: &mut RangedBuilder<'_, ColorBrush>| {
        for (id, kind) in [
            (0, InlineBoxKind::InlineStart),
            (1, InlineBoxKind::InlineEnd),
        ] {
            builder.push_inline_box_with_text_wrap_mode(
                InlineBox {
                    id,
                    kind,
                    index: 0,
                    width: 8.0,
                    height: 0.0,
                },
                TextWrapMode::Wrap,
            );
        }
    };
    let push_tree_edges = |builder: &mut TreeBuilder<'_, ColorBrush>| {
        for (id, kind) in [
            (0, InlineBoxKind::InlineStart),
            (1, InlineBoxKind::InlineEnd),
        ] {
            builder.push_inline_box_with_text_wrap_mode(
                InlineBox {
                    id,
                    kind,
                    index: 0,
                    width: 8.0,
                    height: 0.0,
                },
                TextWrapMode::Wrap,
            );
        }
    };

    assert_builders_produce_same_result(
        text,
        1.0,
        false,
        None,
        &root_style,
        push_ranged_edges,
        push_tree_edges,
        true,
    );
}

/// Inline boxes can be the only paragraph content. Their initial wrapping
/// behavior must come from the first style run, not from the first entry in
/// the caller-managed style table.
#[test]
fn nowrap_first_style_run_keeps_inline_boxes_on_one_line() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let _unused_wrap_style = builder.push_style(TextStyle::default());
    let nowrap_style = builder.push_style(TextStyle {
        text_wrap_mode: TextWrapMode::NoWrap,
        ..TextStyle::default()
    });
    builder.push_style_run(nowrap_style, ..);
    for id in 0..4 {
        builder.push_inline_box(InlineBox {
            id,
            kind: InlineBoxKind::InFlow,
            index: 0,
            width: 64.0,
            height: 96.0,
        });
    }

    let mut layout = builder.build(text);
    let content_widths = layout.calculate_content_widths();
    assert_eq!(content_widths.min, 256.0);
    assert_eq!(content_widths.max, 256.0);

    layout.break_all_lines(Some(192.0));
    assert_eq!(layout.lines().count(), 1);
    assert_eq!(layout.lines().next().unwrap().metrics().advance, 256.0);
}

/// Keep the complementary wrapping behavior covered while changing the
/// inline-box boundary logic.
#[test]
fn wrap_first_style_run_breaks_between_inline_boxes() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let wrap_style = builder.push_style(TextStyle::default());
    builder.push_style_run(wrap_style, ..);
    for id in 0..4 {
        builder.push_inline_box(InlineBox {
            id,
            kind: InlineBoxKind::InFlow,
            index: 0,
            width: 64.0,
            height: 96.0,
        });
    }

    let mut layout = builder.build(text);
    let content_widths = layout.calculate_content_widths();
    assert_eq!(content_widths.min, 64.0);
    assert_eq!(content_widths.max, 256.0);

    layout.break_all_lines(Some(192.0));
    assert_eq!(layout.lines().count(), 2);
}

fn push_test_inline_box(
    builder: &mut StyleRunBuilder<'_, ColorBrush>,
    id: u64,
    kind: InlineBoxKind,
    width: f32,
    text_wrap_mode_after: Option<TextWrapMode>,
) {
    push_test_inline_box_at(builder, id, kind, 0, width, text_wrap_mode_after);
}

fn push_test_inline_box_at(
    builder: &mut StyleRunBuilder<'_, ColorBrush>,
    id: u64,
    kind: InlineBoxKind,
    index: usize,
    width: f32,
    text_wrap_mode_after: Option<TextWrapMode>,
) {
    let inline_box = InlineBox {
        id,
        kind,
        index,
        width,
        height: if kind == InlineBoxKind::InFlow {
            20.0
        } else {
            0.0
        },
    };
    if let Some(mode) = text_wrap_mode_after {
        builder.push_inline_box_with_text_wrap_mode(inline_box, mode);
    } else {
        builder.push_inline_box(inline_box);
    }
}

fn line_advances(layout: &Layout<ColorBrush>) -> Vec<f32> {
    layout.lines().map(|line| line.metrics().advance).collect()
}

/// A nested nowrap span is a state transition in the inline item stream, not
/// a property of the atomic boxes it contains. Its boxes therefore form one
/// min-content unit and overflow together before wrapping resumes outside.
#[test]
fn inline_edges_group_nested_nowrap_boxes() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let wrap = builder.push_style(TextStyle::default());
    builder.push_style_run(wrap, ..);
    builder.set_initial_text_wrap_mode(TextWrapMode::Wrap);
    push_test_inline_box(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        8.0,
        Some(TextWrapMode::NoWrap),
    );
    for id in 1..=2 {
        push_test_inline_box(&mut builder, id, InlineBoxKind::InFlow, 64.0, None);
    }
    push_test_inline_box(
        &mut builder,
        3,
        InlineBoxKind::InlineEnd,
        8.0,
        Some(TextWrapMode::Wrap),
    );
    push_test_inline_box(&mut builder, 4, InlineBoxKind::InFlow, 64.0, None);

    let mut layout = builder.build(text);
    assert_eq!(layout.calculate_content_widths().min, 144.0);
    assert_eq!(layout.calculate_content_widths().max, 208.0);

    layout.break_all_lines(Some(100.0));
    assert_eq!(line_advances(&layout), [144.0, 64.0]);
}

/// A break before an atomic inline moves before its inline-start decoration,
/// and a break after it moves after its inline-end decoration. The decorated
/// inline therefore remains one 80px min-content unit and one overflowing
/// line when the available width is only 64px.
#[test]
fn inline_edges_move_atomic_breaks_outside_decorations() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let wrap = builder.push_style(TextStyle::default());
    builder.push_style_run(wrap, ..);
    push_test_inline_box(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        8.0,
        Some(TextWrapMode::Wrap),
    );
    push_test_inline_box(&mut builder, 1, InlineBoxKind::InFlow, 64.0, None);
    push_test_inline_box(
        &mut builder,
        2,
        InlineBoxKind::InlineEnd,
        8.0,
        Some(TextWrapMode::Wrap),
    );
    push_test_inline_box(&mut builder, 3, InlineBoxKind::InFlow, 64.0, None);

    let mut layout = builder.build(text);
    assert_eq!(layout.calculate_content_widths().min, 80.0);
    assert_eq!(layout.calculate_content_widths().max, 144.0);

    layout.break_all_lines(Some(64.0));
    assert_eq!(line_advances(&layout), [80.0, 64.0]);
}

/// A descendant can re-enable wrapping inside an otherwise nowrap paragraph.
/// Leaving that descendant restores the paragraph mode for following boxes,
/// without discarding the break opportunity created at the descendant's end.
#[test]
fn inline_edges_restore_parent_wrap_mode() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let nowrap = builder.push_style(TextStyle {
        text_wrap_mode: TextWrapMode::NoWrap,
        ..TextStyle::default()
    });
    builder.push_style_run(nowrap, ..);
    builder.set_initial_text_wrap_mode(TextWrapMode::NoWrap);
    push_test_inline_box(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        0.0,
        Some(TextWrapMode::Wrap),
    );
    for id in 1..=2 {
        push_test_inline_box(&mut builder, id, InlineBoxKind::InFlow, 64.0, None);
    }
    push_test_inline_box(
        &mut builder,
        3,
        InlineBoxKind::InlineEnd,
        0.0,
        Some(TextWrapMode::NoWrap),
    );
    push_test_inline_box(&mut builder, 4, InlineBoxKind::InFlow, 64.0, None);

    let mut layout = builder.build(text);
    assert_eq!(layout.calculate_content_widths().min, 64.0);
    assert_eq!(layout.calculate_content_widths().max, 192.0);

    layout.break_all_lines(Some(64.0));
    assert_eq!(line_advances(&layout), [64.0, 64.0, 64.0]);
}

/// Breakability on either side of an atomic inline survives zero-length style
/// boundaries. Inline-end decoration stays with preceding text, while
/// inline-start decoration moves with the following atomic box.
#[test]
fn inline_edges_transfer_text_atomic_breaks() {
    let text = "x";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);
    let text_style = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        text_wrap_mode: TextWrapMode::Wrap,
        ..TextStyle::default()
    });
    builder.push_style_run(text_style, ..);
    builder.set_initial_text_wrap_mode(TextWrapMode::NoWrap);
    push_test_inline_box_at(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        0,
        8.0,
        Some(TextWrapMode::Wrap),
    );
    push_test_inline_box_at(
        &mut builder,
        1,
        InlineBoxKind::InlineEnd,
        1,
        8.0,
        Some(TextWrapMode::NoWrap),
    );
    push_test_inline_box_at(&mut builder, 2, InlineBoxKind::InFlow, 1, 64.0, None);
    let mut layout = builder.build(text);
    let content_widths = layout.calculate_content_widths();
    assert_eq!(content_widths.min, 64.0);
    assert!(content_widths.max > content_widths.min);
    layout.break_all_lines(Some(64.0));
    let advances = line_advances(&layout);
    assert_eq!(advances.len(), 2);
    assert!(advances[0] < 64.0);
    assert_eq!(advances[1], 64.0);

    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);
    let text_style = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        text_wrap_mode: TextWrapMode::Wrap,
        ..TextStyle::default()
    });
    builder.push_style_run(text_style, ..);
    builder.set_initial_text_wrap_mode(TextWrapMode::Wrap);
    push_test_inline_box_at(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        1,
        8.0,
        Some(TextWrapMode::NoWrap),
    );
    push_test_inline_box_at(&mut builder, 1, InlineBoxKind::InFlow, 1, 64.0, None);
    push_test_inline_box_at(
        &mut builder,
        2,
        InlineBoxKind::InlineEnd,
        1,
        8.0,
        Some(TextWrapMode::Wrap),
    );
    let mut layout = builder.build(text);
    let content_widths = layout.calculate_content_widths();
    assert_eq!(content_widths.min, 80.0);
    assert!(content_widths.max > content_widths.min);
    layout.break_all_lines(Some(64.0));
    let advances = line_advances(&layout);
    assert_eq!(advances.len(), 2);
    assert!(advances[0] < 64.0);
    assert_eq!(advances[1], 80.0);
}

/// Test that all the builders behave the same when given the same root style.
#[test]
fn builders_root_only() {
    let text = "Builders often wear hard hats for safety while working on construction sites.";
    let scale = 2.;
    let quantize = false;
    let max_advance = Some(50.);
    let root_style = create_root_style();

    let with_ranged_builder = |rb: &mut RangedBuilder<'_, ColorBrush>| {
        set_root_style(rb);
    };
    let with_tree_builder = |tb: &mut TreeBuilder<'_, ColorBrush>| {
        tb.push_text(text);
    };

    assert_builders_produce_same_result(
        text,
        scale,
        quantize,
        max_advance,
        &root_style,
        with_ranged_builder,
        with_tree_builder,
        false,
    );
}

/// Test that an empty layout doesn't crash
#[test]
fn builders_empty() {
    let text = "";
    let scale = 1.;
    let quantize = false;
    let max_advance = Some(50.);
    let root_style = create_root_style();

    let with_ranged_builder = |_rb: &mut RangedBuilder<'_, ColorBrush>| {};
    let with_tree_builder = |_tb: &mut TreeBuilder<'_, ColorBrush>| {};

    assert_builders_produce_same_result(
        text,
        scale,
        quantize,
        max_advance,
        &root_style,
        with_ranged_builder,
        with_tree_builder,
        true,
    );
}

/// Test that all the builders behave the same with mixed styles.
#[test]
fn builders_mixed_styles() {
    let text = "Builders often wear hard hats for safety while working on construction sites.";
    let scale = 2.;
    let quantize = false;
    let max_advance = Some(50.);
    let root_style = create_root_style();

    let with_ranged_builder = |rb: &mut RangedBuilder<'_, ColorBrush>| {
        set_root_style(rb);

        // Make the first word bigger
        rb.push(StyleProperty::FontSize(68.), 0..8);
        // Push two modified styles for the same range
        rb.push(StyleProperty::LetterSpacing(4.), 12..17);
        rb.push(StyleProperty::WordSpacing(3.), 12..17);
        // Plus, change the line height for the last letter
        rb.push(StyleProperty::LineHeight(LineHeight::Absolute(40.)), 16..17);
    };
    let with_tree_builder = |tb: &mut TreeBuilder<'_, ColorBrush>| {
        // Make the first word bigger
        tb.push_style_modification_span(&[StyleProperty::FontSize(68.)]);
        tb.push_text(&text[..8]);
        tb.pop_style_span();

        tb.push_text(&text[8..12]);

        // Push two modified styles in batch
        tb.push_style_modification_span(&[
            StyleProperty::LetterSpacing(4.),
            StyleProperty::WordSpacing(3.),
        ]);
        tb.push_text(&text[12..16]);
        // Plus, change the line height for the last letter
        tb.push_style_modification_span(&[StyleProperty::LineHeight(LineHeight::Absolute(40.))]);
        tb.push_text(&text[16..17]);
        tb.pop_style_span();
        tb.pop_style_span();

        tb.push_text(&text[17..]);
    };

    assert_builders_produce_same_result(
        text,
        scale,
        quantize,
        max_advance,
        &root_style,
        with_ranged_builder,
        with_tree_builder,
        false,
    );
}
