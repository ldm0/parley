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

/// Whether a box is in-flow (takes up space in the layout) or out-of-flow (e.g. absolutely positioned)
/// or custom-out-of-flow (line-breaking should yield control flow)
#[derive(PartialEq, Debug, Clone, Copy)]
pub enum InlineBoxKind {
    /// `InFlow` boxes take up space in the layout and flow in line with text
    ///
    /// They correspond to `display: inline-block` boxes in CSS.
    InFlow,
    /// The inline-start edge of a non-atomic inline box.
    ///
    /// This contributes its width to inline flow, but does not create an
    /// independent soft break opportunity. A break before the first content
    /// inside the box is moved before this edge.
    InlineStart,
    /// The inline-end edge of a non-atomic inline box.
    ///
    /// This contributes its width to inline flow, but does not create an
    /// independent soft break opportunity. A break after the last content
    /// inside the box is moved after this edge.
    InlineEnd,
    /// `OutOfFlow` boxes are assigned a position as if they were a zero-sized inline box, but
    /// do not take up space in the layout.
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

impl InlineBoxKind {
    pub(crate) const fn contributes_advance(self) -> bool {
        matches!(self, Self::InFlow | Self::InlineStart | Self::InlineEnd)
    }

    pub(crate) const fn is_inline_edge(self) -> bool {
        matches!(self, Self::InlineStart | Self::InlineEnd)
    }
}

/// Builder input retained alongside an inline box until its position in the
/// shaped item stream has been resolved.
#[derive(Debug, Clone)]
pub(crate) struct InlineBoxInput {
    pub(crate) inline_box: InlineBox,
    /// Style that becomes current after this box is consumed.
    ///
    /// Atomic and out-of-flow boxes normally leave the surrounding inline
    /// style unchanged. [`InlineBoxKind::InlineStart`] and
    /// [`InlineBoxKind::InlineEnd`] use this transition to model entering or
    /// leaving a styled inline span. Keeping the complete style index here
    /// avoids growing one parallel transition field per CSS property.
    pub(crate) style_after: Option<u16>,
}
