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
            InlineBoxKind::InFlow | InlineBoxKind::StartBoundary | InlineBoxKind::EndBoundary => {
                self.width
            }
            InlineBoxKind::TextBoundary
            | InlineBoxKind::OutOfFlow
            | InlineBoxKind::CustomOutOfFlow => 0.0,
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
    /// Opening edge of a non-atomic inline. Its signed width contributes to
    /// inline flow and stays attached to the following content when wrapping.
    /// It is transparent to text and bidi analysis, not a replacement character.
    StartBoundary,
    /// Closing edge of a non-atomic inline. Its signed width contributes to
    /// inline flow and stays attached to the preceding content when wrapping.
    /// Boundary heights do not supply atomic-box metrics.
    EndBoundary,
    /// An empty text item, for example a source text node whose whitespace
    /// collapsed into preceding text. It retains its position among inline
    /// edges without contributing a character, advance, or break opportunity.
    /// A following text break occurs after this item, not before an opening
    /// edge that precedes it.
    TextBoundary,
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

impl InlineBoxKind {
    pub(crate) fn is_boundary(self) -> bool {
        matches!(
            self,
            Self::StartBoundary | Self::EndBoundary | Self::TextBoundary
        )
    }
}

/// Breaks at a text position precede its opening inline edges, but follow
/// closing edges. Keep the outermost opening until content consumes it or all
/// intervening empty inlines close. Both intrinsic and final layout use this
/// affinity, with their own checkpoint representation.
#[derive(Clone)]
pub(crate) struct InlineBoundaryAffinity<T> {
    opening: Option<(T, usize)>,
}

impl<T> Default for InlineBoundaryAffinity<T> {
    fn default() -> Self {
        Self { opening: None }
    }
}

impl<T> InlineBoundaryAffinity<T> {
    pub(crate) fn open(&mut self, before: T) {
        if let Some((_, depth)) = &mut self.opening {
            *depth += 1;
        } else {
            self.opening = Some((before, 1));
        }
    }

    pub(crate) fn close(&mut self) {
        if let Some((_, depth)) = &mut self.opening {
            *depth -= 1;
            if *depth == 0 {
                self.opening = None;
            }
        }
    }

    pub(crate) fn before_opening(&self) -> Option<&T> {
        self.opening.as_ref().map(|(before, _)| before)
    }

    pub(crate) fn consume_content(&mut self) {
        self.opening = None;
    }
}

/// Builder input and its resolved embedding level. Boundary items are not
/// inserted into the text used for shaping, breaking, or bidi analysis.
pub(crate) struct InlineBoxInput {
    pub(crate) inline_box: InlineBox,
    pub(crate) bidi_level: u8,
}

impl InlineBoxInput {
    pub(crate) fn new(inline_box: InlineBox) -> Self {
        Self {
            inline_box,
            bidi_level: 0,
        }
    }
}
