// Copyright 2025 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Test that the various builders produce the same results.

use std::{borrow::Cow, path::PathBuf, sync::Arc, vec, vec::Vec};

use fontique::{Collection, CollectionOptions, FontStyle, FontWeight, FontWidth, SourceCache};
use parlance::FontFamilyName;
use peniko::{Blob, color::palette};

use super::utils::{ColorBrush, asserts::assert_eq_layout_data};
use crate::{
    BaseDirection, FontContext, FontFamily, FontFeatures, FontVariations, InlineBox, InlineBoxKind,
    Layout, LayoutContext, LineHeight, OverflowWrap, PositionedLayoutItem, RangedBuilder,
    StyleProperty, StyleRunBuilder, TextStyle, TextWrapMode, TreeBuilder, WhiteSpaceCollapse,
    WordBreak,
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

#[test]
fn builders_apply_base_direction() {
    let text = "123 / 456";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();

    let mut ranged = lcx.ranged_builder(&mut fcx, text, 1.0, true);
    ranged.push_default(FontFamily::from(FONT_FAMILY_LIST));
    ranged.set_base_direction(BaseDirection::Rtl);
    let mut ranged_layout = ranged.build(text);
    ranged_layout.break_all_lines(None);
    assert!(ranged_layout.is_rtl());
    assert_eq!(
        ranged_layout
            .lines()
            .flat_map(|line| line.runs())
            .map(|run| run.text_range())
            .collect::<Vec<_>>(),
        [6..9, 3..6, 0..3]
    );

    let root_style = TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        ..TextStyle::default()
    };
    let mut tree = lcx.tree_builder(&mut fcx, 1.0, true, &root_style);
    tree.set_base_direction(BaseDirection::Rtl);
    tree.push_text(text);
    let (mut tree_layout, _) = tree.build();
    tree_layout.break_all_lines(None);
    assert!(tree_layout.is_rtl());
    assert_eq_layout_data(
        &ranged_layout.data,
        &tree_layout.data,
        "tree base direction",
    );

    let mut style_runs = lcx.style_run_builder(&mut fcx, text, 1.0, true);
    style_runs.set_base_direction(BaseDirection::Rtl);
    let style = style_runs.push_style(root_style);
    style_runs.push_style_run(style, ..);
    let mut style_run_layout = style_runs.build(text);
    style_run_layout.break_all_lines(None);
    assert!(style_run_layout.is_rtl());
    assert_eq_layout_data(
        &ranged_layout.data,
        &style_run_layout.data,
        "style-run base direction",
    );
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

    // Testing idempotence of ranged builder creation
    let layout = build_layout_with_ranged(&mut fcx, &mut lcx_a, &ropts, &with_ranged_builder);
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_a_rb_two");

    // Basic builder compatibility - tree builder from a clean layout context
    let layout = build_layout_with_tree(&mut fcx, &mut lcx_b, &topts, &with_tree_builder);
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_b_tb_one");

    // Testing idempotence of tree builder creation
    let layout = build_layout_with_tree(&mut fcx, &mut lcx_b, &topts, &with_tree_builder);
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_b_tb_two");

    // Priming a fresh layout context with ranged builder creation
    let _ = build_layout_with_ranged(&mut fcx, &mut lcx_c, &ropts, &with_ranged_builder);

    // Testing tree builder creation with a dirty layout context
    let layout = build_layout_with_tree(&mut fcx, &mut lcx_c, &topts, &with_tree_builder);
    assert_eq_layout_data(&layout_truth.data, &layout.data, "lcx_c_tb_one");

    // Priming a fresh layout context with tree builder creation
    let _ = build_layout_with_tree(&mut fcx, &mut lcx_d, &topts, &with_tree_builder);

    // Testing ranged builder creation with a dirty layout context
    let layout = build_layout_with_ranged(&mut fcx, &mut lcx_d, &ropts, &with_ranged_builder);
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
        white_space_collapse: WhiteSpaceCollapse::Preserve,
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

fn build_white_space_layout(
    fcx: &mut FontContext,
    text: &str,
    white_space_collapse: WhiteSpaceCollapse,
    text_wrap_mode: TextWrapMode,
) -> Layout<ColorBrush> {
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(fcx, text, 1.0, false);
    let style = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        white_space_collapse,
        text_wrap_mode,
        ..TextStyle::default()
    });
    builder.set_root_style(style);
    builder.push_style_run(style, ..);
    builder.build(text)
}

#[test]
fn style_runs_apply_white_space_collapse_to_intrinsic_sizes() {
    let mut fcx = create_font_context();
    let collapse = build_white_space_layout(
        &mut fcx,
        "x ",
        WhiteSpaceCollapse::Collapse,
        TextWrapMode::Wrap,
    )
    .calculate_content_widths();
    let preserve = build_white_space_layout(
        &mut fcx,
        "x ",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    )
    .calculate_content_widths();
    let break_spaces = build_white_space_layout(
        &mut fcx,
        "x ",
        WhiteSpaceCollapse::BreakSpaces,
        TextWrapMode::Wrap,
    )
    .calculate_content_widths();

    assert_eq!(collapse.min, preserve.min);
    assert!(preserve.max > collapse.max);
    assert!(break_spaces.min > preserve.min);
    assert_eq!(break_spaces.max, preserve.max);
}

#[test]
fn break_spaces_breaks_after_each_preserved_space() {
    let mut fcx = create_font_context();
    let mut layout = build_white_space_layout(
        &mut fcx,
        "A   B",
        WhiteSpaceCollapse::BreakSpaces,
        TextWrapMode::Wrap,
    );
    layout.break_all_lines(Some(0.0));
    assert_eq!(
        layout
            .lines()
            .map(|line| line.text_range())
            .collect::<Vec<_>>(),
        [0..2, 2..3, 3..4, 4..5],
    );
}

#[test]
fn white_space_semantics_follow_each_style_run() {
    let text = "A   B";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);
    let collapse = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        white_space_collapse: WhiteSpaceCollapse::Collapse,
        ..TextStyle::default()
    });
    let break_spaces = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        white_space_collapse: WhiteSpaceCollapse::BreakSpaces,
        ..TextStyle::default()
    });
    builder.set_root_style(collapse);
    builder.push_style_run(collapse, 0..1);
    builder.push_style_run(break_spaces, 1..4);
    builder.push_style_run(collapse, 4..5);

    let mut layout = builder.build(text);
    layout.break_all_lines(Some(0.0));
    assert_eq!(
        layout
            .lines()
            .map(|line| line.text_range())
            .collect::<Vec<_>>(),
        [0..2, 2..3, 3..4, 4..5],
    );
}

#[test]
fn break_spaces_opportunity_propagates_through_inline_end() {
    let text = "A  B";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);
    let style = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        white_space_collapse: WhiteSpaceCollapse::BreakSpaces,
        ..TextStyle::default()
    });
    builder.set_root_style(style);
    builder.push_style_run(style, ..);
    push_test_inline_box_at(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        0,
        0.0,
        Some(style),
    );
    push_test_inline_box_at(
        &mut builder,
        1,
        InlineBoxKind::InlineEnd,
        2,
        0.0,
        Some(style),
    );

    let mut layout = builder.build(text);
    layout.break_all_lines(Some(0.0));
    assert_eq!(
        layout
            .lines()
            .map(|line| line.text_range())
            .collect::<Vec<_>>(),
        [0..2, 2..3, 3..4],
    );
}

#[test]
fn preserved_trailing_space_hangs_only_when_wrapping_allows_it() {
    let mut fcx = create_font_context();
    let measure = |fcx: &mut FontContext, text: &str, mode, wrap, width| {
        let mut layout = build_white_space_layout(fcx, text, mode, wrap);
        layout.break_all_lines(width);
        layout
    };

    let text_width = measure(
        &mut fcx,
        "xx",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
        None,
    )
    .width();
    let full_width = measure(
        &mut fcx,
        "xx ",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::NoWrap,
        None,
    )
    .width();
    let constraint = (text_width + full_width) * 0.5;

    let wrap = measure(
        &mut fcx,
        "xx ",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
        Some(constraint),
    );
    assert_eq!(wrap.len(), 1);
    assert!((wrap.width() - constraint).abs() < 0.01);

    let nowrap = measure(
        &mut fcx,
        "xx ",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::NoWrap,
        Some(constraint),
    );
    assert_eq!(nowrap.len(), 1);
    assert!((nowrap.width() - full_width).abs() < 0.01);
    assert_eq!(nowrap.get(0).unwrap().metrics().trailing_whitespace, 0.0);

    let collapsed = measure(
        &mut fcx,
        "xx ",
        WhiteSpaceCollapse::Collapse,
        TextWrapMode::Wrap,
        None,
    );
    assert!((collapsed.width() - text_width).abs() < 0.01);
}

#[test]
fn phase_two_removal_is_scoped_to_a_line_break() {
    let mut fcx = create_font_context();
    let mut layout = build_white_space_layout(
        &mut fcx,
        "xx xx",
        WhiteSpaceCollapse::Collapse,
        TextWrapMode::Wrap,
    );
    let max_width = layout.calculate_content_widths().max;
    let min_width = layout.calculate_content_widths().min;

    layout.break_all_lines(Some(min_width));
    assert_eq!(layout.len(), 2);

    // Rebreaking must start from immutable shaping metrics; a space removed at
    // the first narrow line end becomes internal content at the wider width.
    layout.break_all_lines(None);
    assert_eq!(layout.len(), 1);
    assert!((layout.full_width() - max_width).abs() < 0.01);
}

#[test]
fn phase_two_removed_space_does_not_offset_a_trailing_inline_edge() {
    let text = "xx xx";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);
    let style = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        white_space_collapse: WhiteSpaceCollapse::Collapse,
        ..TextStyle::default()
    });
    builder.set_root_style(style);
    builder.push_style_run(style, ..);
    push_test_inline_box_at(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        0,
        8.0,
        Some(style),
    );
    push_test_inline_box_at(
        &mut builder,
        1,
        InlineBoxKind::InlineEnd,
        3,
        8.0,
        Some(style),
    );
    push_test_inline_box_at(
        &mut builder,
        2,
        InlineBoxKind::InlineStart,
        3,
        8.0,
        Some(style),
    );
    push_test_inline_box_at(
        &mut builder,
        3,
        InlineBoxKind::InlineEnd,
        text.len(),
        8.0,
        Some(style),
    );

    let mut layout = builder.build(text);
    let min_width = layout.calculate_content_widths().min;
    layout.break_all_lines(Some(min_width));
    assert_eq!(layout.len(), 2);
    let line = layout.get(0).expect("one line");
    let end = line
        .items()
        .find_map(|item| match item {
            PositionedLayoutItem::InlineBox(inline_box) if inline_box.id == 1 => Some(inline_box),
            PositionedLayoutItem::GlyphRun(_) | PositionedLayoutItem::InlineBox(_) => None,
        })
        .expect("trailing inline edge");

    assert!((end.x + end.width - line.metrics().advance).abs() < 0.01);
}

#[test]
fn no_break_space_is_visible_and_non_breaking_at_line_end() {
    let mut fcx = create_font_context();
    let mut text = build_white_space_layout(
        &mut fcx,
        "xx",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    );
    text.break_all_lines(None);
    let text_width = text.width();

    let mut full = build_white_space_layout(
        &mut fcx,
        "xx\u{00a0}",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    );
    full.break_all_lines(None);
    let full_width = full.width();
    assert!(full_width > text_width);

    let mut constrained = build_white_space_layout(
        &mut fcx,
        "xx\u{00a0}",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    );
    constrained.break_all_lines(Some((text_width + full_width) * 0.5));
    assert_eq!(constrained.len(), 1);
    assert!((constrained.width() - full_width).abs() < 0.01);
    assert_eq!(
        constrained.get(0).unwrap().metrics().trailing_whitespace,
        0.0
    );
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

        let root_index = rb.push_style(root_run);
        let modified_index = rb.push_style(modified_run);
        rb.set_root_style(root_index);
        rb.push_style_run(modified_index, 0..text.len());
    });

    assert_eq_layout_data(
        &ranged.data,
        &runs.data,
        "style_runs_first_run_can_use_nonzero_style_index",
    );
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

    let with_ranged_builder = |rb: &mut RangedBuilder<'_, ColorBrush>| set_root_style(rb);
    let with_tree_builder = |_tb: &mut TreeBuilder<'_, ColorBrush>| {};

    assert_builders_produce_same_result(
        text,
        scale,
        quantize,
        max_advance,
        &root_style,
        with_ranged_builder,
        with_tree_builder,
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
    );
}

/// Check that reusing a [`Layout`] works, by overwriting a layout with
/// [`RangedBuilder::build_into`].
#[test]
fn ranged_builder_reuse_layout() {
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();

    static FIRST_TEXT: &str = "Some first text.";
    static SECOND_TEXT: &str = "Some second text.";
    const MAX_ADVANCE: f32 = 50.;

    let mut layout = Layout::new();

    let mut builder = lcx.ranged_builder(&mut fcx, FIRST_TEXT, 1., false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.build_into(&mut layout, FIRST_TEXT);
    layout.break_all_lines(Some(MAX_ADVANCE));

    let mut builder = lcx.ranged_builder(&mut fcx, SECOND_TEXT, 1., false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    builder.build_into(&mut layout, SECOND_TEXT);
    layout.break_all_lines(Some(MAX_ADVANCE));

    let mut builder = lcx.ranged_builder(&mut fcx, SECOND_TEXT, 1., false);
    builder.push_default(FontFamily::from(FONT_FAMILY_LIST));
    let mut expected = builder.build(SECOND_TEXT);
    expected.break_all_lines(Some(MAX_ADVANCE));

    assert_eq_layout_data(
        &expected.data,
        &layout.data,
        "Expected reused layout to be identical to fresh layout",
    );
}

#[test]
fn builders_crlf_counts_as_single_line_break() {
    let mut fcx = create_font_context();
    let mut line_count = |text: &str| -> usize {
        let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
        let ropts = RangedOptions {
            scale: 1.0,
            quantize: false,
            max_advance: None,
            text,
        };
        let layout = build_layout_with_ranged(&mut fcx, &mut lcx, &ropts, |rb| {
            set_root_style(rb);
        });
        layout.lines().len()
    };

    let crlf = line_count("a\r\nb");
    let lf = line_count("a\nb");
    let cr = line_count("a\rb");

    assert_eq!(crlf, 2, "CRLF should produce exactly two lines");
    assert_eq!(lf, 2, "LF should produce exactly two lines");
    assert_eq!(crlf, lf, "CRLF should match LF line count");
    assert_eq!(crlf, cr, "CRLF, LF, and CR should match line count");

    let double_crlf = line_count("a\r\n\r\nb");
    let double_lf = line_count("a\n\nb");

    assert_eq!(
        double_crlf, 3,
        "two CRLF breaks should produce one blank line, not extra blanks"
    );
    assert_eq!(
        double_crlf, double_lf,
        "two CRLF breaks should match two LF breaks"
    );

    let trailing_cr = line_count("a\r");
    let trailing_lf = line_count("a\n");

    assert_eq!(
        trailing_cr, trailing_lf,
        "trailing CR should match trailing LF line count"
    );
}

/// A CRLF whose `\r` and `\n` land in different shaped runs (because a style
/// change starts at the `\n`) must still coalesce into a single hard break.
#[test]
fn builders_crlf_across_run_boundary_counts_as_single_line_break() {
    let mut fcx = create_font_context();
    let styled_line_count =
        |fcx: &mut FontContext, text: &str, style_range: std::ops::Range<usize>| -> usize {
            let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
            let ropts = RangedOptions {
                scale: 1.0,
                quantize: false,
                max_advance: None,
                text,
            };
            let layout = build_layout_with_ranged(fcx, &mut lcx, &ropts, |rb| {
                set_root_style(rb);
                // A style change starting at the `\n` forces shaping to split the
                // CRLF pair across two runs.
                rb.push(StyleProperty::FontSize(40.), style_range.clone());
            });
            layout.lines().len()
        };

    // The `\n` in "a\r\nb" is byte 2.
    let split_crlf = styled_line_count(&mut fcx, "a\r\nb", 2..3);
    let split_lf = styled_line_count(&mut fcx, "a\nb", 2..3);

    assert_eq!(
        split_crlf, 2,
        "a style boundary at the LF must not turn CRLF into two hard breaks"
    );
    assert_eq!(
        split_crlf, split_lf,
        "styled CRLF should match styled LF line count"
    );
}

fn push_test_inline_box(
    builder: &mut StyleRunBuilder<'_, ColorBrush>,
    id: u64,
    kind: InlineBoxKind,
    width: f32,
    style_after: Option<u16>,
) {
    push_test_inline_box_at(builder, id, kind, 0, width, style_after);
}

fn push_test_inline_box_at(
    builder: &mut StyleRunBuilder<'_, ColorBrush>,
    id: u64,
    kind: InlineBoxKind,
    index: usize,
    width: f32,
    style_after: Option<u16>,
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
        baseline: None,
    };
    if let Some(style_after) = style_after {
        builder.push_inline_box_with_style_transition(inline_box, style_after);
    } else {
        builder.push_inline_box(inline_box);
    }
}

fn line_advances(layout: &Layout<ColorBrush>) -> Vec<f32> {
    layout.lines().map(|line| line.metrics().advance).collect()
}

/// A nested nowrap span is a style transition in the item stream. Its atomic
/// children form one min-content unit, then wrapping resumes outside it.
#[test]
fn inline_style_boundaries_group_nested_nowrap_boxes() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let wrap = builder.push_style(TextStyle::default());
    let nowrap = builder.push_style(TextStyle {
        text_wrap_mode: TextWrapMode::NoWrap,
        ..TextStyle::default()
    });
    builder.set_root_style(wrap);
    builder.push_style_run(wrap, ..);
    push_test_inline_box(
        &mut builder,
        0,
        InlineBoxKind::InlineStart,
        8.0,
        Some(nowrap),
    );
    for id in 1..=2 {
        push_test_inline_box(&mut builder, id, InlineBoxKind::InFlow, 64.0, None);
    }
    push_test_inline_box(&mut builder, 3, InlineBoxKind::InlineEnd, 8.0, Some(wrap));
    push_test_inline_box(&mut builder, 4, InlineBoxKind::InFlow, 64.0, None);

    let mut layout = builder.build(text);
    assert_eq!(layout.calculate_content_widths().min, 144.0);
    assert_eq!(layout.calculate_content_widths().max, 208.0);

    layout.break_all_lines(Some(100.0));
    assert_eq!(line_advances(&layout), [144.0, 64.0]);
}

/// A break next to an atomic inline is moved outside its start/end
/// decorations, keeping the decorated fragment indivisible.
#[test]
fn inline_style_boundaries_keep_atomic_decorations_together() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let wrap = builder.push_style(TextStyle::default());
    builder.set_root_style(wrap);
    builder.push_style_run(wrap, ..);
    push_test_inline_box(&mut builder, 0, InlineBoxKind::InlineStart, 8.0, Some(wrap));
    push_test_inline_box(&mut builder, 1, InlineBoxKind::InFlow, 64.0, None);
    push_test_inline_box(&mut builder, 2, InlineBoxKind::InlineEnd, 8.0, Some(wrap));
    push_test_inline_box(&mut builder, 3, InlineBoxKind::InFlow, 64.0, None);

    let mut layout = builder.build(text);
    assert_eq!(layout.calculate_content_widths().min, 80.0);
    assert_eq!(layout.calculate_content_widths().max, 144.0);

    layout.break_all_lines(Some(64.0));
    assert_eq!(line_advances(&layout), [80.0, 64.0]);
}

/// A descendant can re-enable wrapping inside a nowrap root, while its close
/// item restores the root style for following atomic content.
#[test]
fn inline_style_boundaries_restore_parent_style() {
    let text = "";
    let mut fcx = FontContext::new();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);

    let nowrap = builder.push_style(TextStyle {
        text_wrap_mode: TextWrapMode::NoWrap,
        ..TextStyle::default()
    });
    let wrap = builder.push_style(TextStyle::default());
    builder.set_root_style(nowrap);
    builder.push_style_run(nowrap, ..);
    push_test_inline_box(&mut builder, 0, InlineBoxKind::InlineStart, 0.0, Some(wrap));
    for id in 1..=2 {
        push_test_inline_box(&mut builder, id, InlineBoxKind::InFlow, 64.0, None);
    }
    push_test_inline_box(&mut builder, 3, InlineBoxKind::InlineEnd, 0.0, Some(nowrap));
    push_test_inline_box(&mut builder, 4, InlineBoxKind::InFlow, 64.0, None);

    let mut layout = builder.build(text);
    assert_eq!(layout.calculate_content_widths().min, 64.0);
    assert_eq!(layout.calculate_content_widths().max, 192.0);

    layout.break_all_lines(Some(64.0));
    assert_eq!(line_advances(&layout), [64.0, 64.0, 64.0]);
}

#[test]
fn inline_start_moves_with_content_past_hanging_whitespace() {
    let text = "x x";
    let mut fcx = create_font_context();
    let mut lcx: LayoutContext<ColorBrush> = LayoutContext::new();
    let mut builder = lcx.style_run_builder(&mut fcx, text, 1.0, false);
    let style = builder.push_style(TextStyle {
        font_family: FontFamily::from(FONT_FAMILY_LIST),
        white_space_collapse: WhiteSpaceCollapse::Collapse,
        ..TextStyle::default()
    });
    builder.set_root_style(style);
    builder.push_style_run(style, ..);
    for (id, kind, index) in [
        (0, InlineBoxKind::InlineStart, 0),
        (1, InlineBoxKind::InlineEnd, 1),
        (2, InlineBoxKind::InlineStart, 1),
        (3, InlineBoxKind::InlineEnd, 3),
    ] {
        push_test_inline_box_at(&mut builder, id, kind, index, 8.0, None);
    }

    let mut layout = builder.build(text);
    let line_width = layout.calculate_content_widths().min;
    layout.break_all_lines(Some(line_width));
    let inline_ids = layout
        .lines()
        .map(|line| {
            line.items()
                .filter_map(|item| match item {
                    PositionedLayoutItem::InlineBox(inline_box) => Some(inline_box.id),
                    PositionedLayoutItem::GlyphRun(_) => None,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(inline_ids, [vec![0, 1], vec![2, 3]]);
    assert_eq!(
        layout
            .lines()
            .map(|line| line.text_range())
            .collect::<Vec<_>>(),
        [0..1, 2..3]
    );
    let advances = line_advances(&layout);
    assert_eq!(advances.len(), 2);
    assert_eq!(advances[0], advances[1]);
}

#[test]
fn justification_counts_only_non_hanging_spaces() {
    let mut fcx = create_font_context();
    let mut visible = build_white_space_layout(
        &mut fcx,
        "aa aa",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    );
    visible.break_all_lines(None);
    let visible_width = visible.width();

    let mut full = build_white_space_layout(
        &mut fcx,
        "aa aa   ",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    );
    full.break_all_lines(None);
    let space_width = (full.full_width() - visible_width) / 3.0;

    let mut layout = build_white_space_layout(
        &mut fcx,
        "aa aa   aa",
        WhiteSpaceCollapse::Preserve,
        TextWrapMode::Wrap,
    );
    layout.break_all_lines(Some(visible_width + 1.5 * space_width));
    assert_eq!(layout.data.lines.len(), 2);
    assert_eq!(
        layout.data.lines[0].break_reason,
        crate::BreakReason::Regular
    );
    assert_eq!(layout.data.lines[0].num_spaces, 1);
}
