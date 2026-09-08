// Copyright 2024 the Parley Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// A box to be laid out inline with text
#[derive(PartialEq, Debug, Clone)]
pub struct InlineBox {
    /// User-specified identifier for the box, which can be used by the user to determine which box in
    /// parley's output corresponds to which box in its input.
    pub id: u64,
    /// Whether the box is in-flow (takes up space in the layout) or out-of-flow (e.g. absolutely positioned or floated)
    pub kind: InlineBoxKind,
    /// The byte offset into the underlying text string at which the box should be placed.
    /// This must not be within a Unicode code point.
    pub index: usize,
    /// The width of the box in pixels
    pub width: f32,
    /// The height of the box in pixels
    pub height: f32,
}

impl InlineBox {
    /// The box's contribution to inline flow, independently of its own size.
    pub(crate) const fn advance(&self) -> f32 {
        match self.kind {
            InlineBoxKind::InFlow => self.width,
            InlineBoxKind::OutOfFlow | InlineBoxKind::CustomOutOfFlow => 0.0,
        }
    }
}

/// Whether a box is in-flow (takes up space in the layout) or out-of-flow (e.g. absolutely positioned)
/// or custom-out-of-flow (line-breaking should yield control flow)
#[derive(PartialEq, Debug, Clone, Copy)]
pub enum InlineBoxKind {
    /// `InFlow` boxes take up space in the layout and flow in line with text
    ///
    /// They correspond to `display: inline-block` boxes in CSS.
    InFlow,
    /// `OutOfFlow` boxes are assigned a position without taking up space or
    /// introducing a line-break opportunity. They do not interrupt the text's
    /// whitespace, wrapping, or intrinsic-width state.
    ///
    /// They correspond to `position: absolute` boxes in CSS.
    OutOfFlow,
    /// `CustomOutOfFlow` boxes also do not take up space in the layout, but they are not assigned a position
    /// by Parley. When they are encountered, control flow is yielded back to the caller who is then responsible
    /// for laying out the box.
    ///
    /// They can be used to implement advanced layout modes such as CSS's `float`
    CustomOutOfFlow,
}

/// How an inline item participates in Unicode bidirectional analysis.
///
/// This is independent of whether the item occupies space in line layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlineBoxBidi {
    /// A neutral object, analyzed as U+FFFC OBJECT REPLACEMENT CHARACTER.
    #[default]
    Neutral,
    /// A transparent opening boundary, following the next content's level.
    StartBoundary,
    /// A transparent closing boundary, following the preceding content's level.
    EndBoundary,
}

/// Builder input and its resolved embedding level. Boundary items are not
/// inserted into the text used for shaping, breaking, or bidi analysis.
pub(crate) struct InlineBoxInput {
    pub(crate) inline_box: InlineBox,
    pub(crate) bidi: InlineBoxBidi,
    pub(crate) bidi_level: u8,
}

impl InlineBoxInput {
    pub(crate) fn new(inline_box: InlineBox, bidi: InlineBoxBidi) -> Self {
        Self {
            inline_box,
            bidi,
            bidi_level: 0,
        }
    }
}
