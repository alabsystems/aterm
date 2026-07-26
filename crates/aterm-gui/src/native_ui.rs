// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed semantic UI tree for first-party tab applications.
//!
//! A control is described once as [`UiContent`].  [`UiTree::compile`] derives the
//! paint list, pointer hit index, semantic nodes, focus order, and controller
//! serialization from that one description.  Apps therefore cannot independently
//! author five subtly different versions of a switch or button.

#![allow(
    dead_code,
    reason = "native tab-app migration foundation; consumers land with the tab host"
)]

use std::collections::HashSet;
use std::fmt;

use aterm_grapheme::GraphemeClusters;

use crate::type_scale::TypeStep;

/// Stable logical identity of a semantic node.  Keys survive layout, filtering,
/// and route changes; vector positions are deliberately not identities.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct UiKey(String);

impl UiKey {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UiKey").field(&self.0).finish()
    }
}

/// Stable reducer action identity emitted by an interactive semantic node.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ActionId(String);

impl ActionId {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ActionId").field(&self.0).finish()
    }
}

/// Logical-pixel rectangle, top-left origin and half-open edges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct LogicalRect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl LogicalRect {
    pub(crate) const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub(crate) fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 0.0
            && self.height >= 0.0
    }

    pub(crate) fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    pub(crate) fn right(self) -> f32 {
        self.x + self.width
    }

    pub(crate) fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub(crate) fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub(crate) fn intersect(self, other: Self) -> Option<Self> {
        const EDGE_EPSILON: f32 = 0.01;
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > x && bottom > y).then(|| {
            // Fractional-track layout arithmetic drifts by ulps when edges are
            // recomputed through a clip chain. A cut smaller than the
            // paint-audit edge epsilon is not real clipping (the exact
            // judgment `materially_clipped` makes), so an axis that is
            // effectively one input's own extent keeps that extent
            // bit-identical instead of a resummed `right - x` — compiled
            // paint and hit geometry must never disagree over an ulp.
            let snap_axis =
                |low: f32, high: f32, a_low: f32, a_len: f32, b_low: f32, b_len: f32| {
                    if (low - a_low).abs() < EDGE_EPSILON
                        && (high - (a_low + a_len)).abs() < EDGE_EPSILON
                    {
                        (a_low, a_len)
                    } else if (low - b_low).abs() < EDGE_EPSILON
                        && (high - (b_low + b_len)).abs() < EDGE_EPSILON
                    {
                        (b_low, b_len)
                    } else {
                        (low, high - low)
                    }
                };
            let (x, width) = snap_axis(x, right, other.x, other.width, self.x, self.width);
            let (y, height) = snap_axis(y, bottom, other.y, other.height, self.y, self.height);
            Self::new(x, y, width, height)
        })
    }

    fn inset(self, insets: Insets) -> Self {
        let horizontal = insets.left + insets.right;
        let vertical = insets.top + insets.bottom;
        Self::new(
            self.x + insets.left,
            self.y + insets.top,
            (self.width - horizontal).max(0.0),
            (self.height - vertical).max(0.0),
        )
    }
}

/// Whether an effective ancestor clip removes a perceptible part of a node.
///
/// Layout arithmetic can produce a few thousandths of a logical pixel of edge
/// drift when nested fractional tracks are intersected. That is neither
/// paint-visible nor actionable, so introspection must not report it as real
/// clipping. Keeping this decision in one helper also makes every paint audit
/// agree about the exact same compiled geometry.
const CLIP_EDGE_EPSILON: f32 = 0.01;

fn materially_clipped(rect: LogicalRect, clip: LogicalRect) -> bool {
    clip.is_empty()
        || clip.x > rect.x + CLIP_EDGE_EPSILON
        || clip.y > rect.y + CLIP_EDGE_EPSILON
        || clip.right() + CLIP_EDGE_EPSILON < rect.right()
        || clip.bottom() + CLIP_EDGE_EPSILON < rect.bottom()
}

/// Logical-pixel insets.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Insets {
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
    pub(crate) left: f32,
}

impl Insets {
    pub(crate) const fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub(crate) const fn symmetric(horizontal: f32, vertical: f32) -> Self {
        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }

    fn is_valid(self) -> bool {
        [self.top, self.right, self.bottom, self.left]
            .into_iter()
            .all(|v| v.is_finite() && v >= 0.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Flow {
    #[default]
    Overlay,
    Row,
    Column,
}

/// A dimension in a parent's flow axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum Length {
    /// Divide remaining space equally between all `Fill` siblings.
    #[default]
    Fill,
    /// Use the control's semantic intrinsic size.
    Intrinsic,
    /// A fixed logical-pixel length.
    Fixed(f32),
    /// A fraction of the parent's content extent, clamped to `0..=1`.
    Fraction(f32),
}

/// Small deterministic layout vocabulary.  It is intentionally sufficient for
/// app shells and form rows, while remaining independent of any OS widget kit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    pub(crate) flow: Flow,
    pub(crate) width: Length,
    pub(crate) height: Length,
    pub(crate) padding: Insets,
    pub(crate) gap: f32,
    pub(crate) clip: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            flow: Flow::Overlay,
            width: Length::Fill,
            height: Length::Fill,
            padding: Insets::default(),
            gap: 0.0,
            clip: false,
        }
    }
}

impl Layout {
    pub(crate) fn column() -> Self {
        Self {
            flow: Flow::Column,
            ..Self::default()
        }
    }

    pub(crate) fn row() -> Self {
        Self {
            flow: Flow::Row,
            ..Self::default()
        }
    }

    pub(crate) fn height(mut self, height: Length) -> Self {
        self.height = height;
        self
    }

    pub(crate) fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    pub(crate) fn padding(mut self, padding: Insets) -> Self {
        self.padding = padding;
        self
    }

    pub(crate) fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub(crate) fn clipped(mut self) -> Self {
        self.clip = true;
        self
    }

    fn is_valid(self) -> bool {
        self.padding.is_valid()
            && self.gap.is_finite()
            && self.gap >= 0.0
            && length_valid(self.width)
            && length_valid(self.height)
    }
}

fn length_valid(length: Length) -> bool {
    match length {
        Length::Fill | Length::Intrinsic => true,
        Length::Fixed(v) => v.is_finite() && v >= 0.0,
        Length::Fraction(v) => v.is_finite() && (0.0..=1.0).contains(&v),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticRole {
    Application,
    Group,
    Heading,
    Text,
    Button,
    Switch,
    Slider,
    TextField,
    RichText,
    TextViewport,
    Link,
    Navigation,
    Status,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum SemanticValue {
    #[default]
    None,
    Text(String),
    Bool(bool),
    Number {
        value: f64,
        minimum: f64,
        maximum: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ControlState {
    pub(crate) enabled: bool,
    pub(crate) focused: bool,
    pub(crate) focus_visible: bool,
    pub(crate) hovered: bool,
    pub(crate) pressed: bool,
    pub(crate) selected: bool,
    pub(crate) invalid: bool,
    pub(crate) busy: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            enabled: true,
            focused: false,
            focus_visible: false,
            hovered: false,
            pressed: false,
            selected: false,
            invalid: false,
            busy: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StyleRef {
    #[default]
    Plain,
    Hero,
    Primary,
    Secondary,
    Quiet,
    Accent,
    Success,
    Danger,
    Navigation,
    Setting,
    Code,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Control<T> {
    pub(crate) spec: T,
    pub(crate) value: SemanticValue,
    pub(crate) state: ControlState,
    pub(crate) action: ActionId,
    pub(crate) style: StyleRef,
}

impl<T> Control<T> {
    pub(crate) fn new(spec: T, action: ActionId) -> Self {
        Self {
            spec,
            value: SemanticValue::None,
            state: ControlState::default(),
            action,
            style: StyleRef::default(),
        }
    }

    pub(crate) fn value(mut self, value: SemanticValue) -> Self {
        self.value = value;
        self
    }

    pub(crate) fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub(crate) fn style(mut self, style: StyleRef) -> Self {
        self.style = style;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GroupSpec {
    pub(crate) label: Option<String>,
    pub(crate) role: SemanticRole,
    pub(crate) style: StyleRef,
}

impl GroupSpec {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            role: SemanticRole::Group,
            style: StyleRef::Plain,
        }
    }

    pub(crate) fn unlabeled(role: SemanticRole) -> Self {
        Self {
            label: None,
            role,
            style: StyleRef::Plain,
        }
    }

    pub(crate) fn style(mut self, style: StyleRef) -> Self {
        self.style = style;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextSpec {
    pub(crate) text: String,
    pub(crate) role: SemanticRole,
    pub(crate) style: StyleRef,
}

impl TextSpec {
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: SemanticRole::Text,
            style: StyleRef::Plain,
        }
    }

    pub(crate) fn heading(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: SemanticRole::Heading,
            style: StyleRef::Primary,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ButtonSpec {
    pub(crate) label: String,
    /// Optional deterministic paint label. Semantics always expose `label`, so
    /// compact icon rails can draw a terse mark without clipping or degrading
    /// the accessible/control name.
    pub(crate) visual_label: Option<String>,
    /// Optional renderer-native pictogram. Unlike a Unicode visual label this
    /// cannot disappear when the user's UI font lacks a symbol glyph.
    pub(crate) visual_icon: Option<ButtonIcon>,
    /// Optional renderer-native affordance after a normal text label. Select
    /// fields use this instead of appending a font-dependent `v` glyph.
    pub(crate) trailing_icon: Option<ButtonIcon>,
    pub(crate) description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonIcon {
    Back,
    Forward,
    Copy,
    External,
    Anchor,
    ChevronDown,
    Home,
    Modified,
    Appearance,
    Text,
    Cursor,
    Window,
    Keyboard,
    Terminal,
    Performance,
    Security,
    Diagnostics,
    Update,
    Packages,
    Info,
}

impl ButtonSpec {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            visual_label: None,
            visual_icon: None,
            trailing_icon: None,
            description: None,
        }
    }

    pub(crate) fn visual_label(mut self, label: impl Into<String>) -> Self {
        self.visual_label = Some(label.into());
        self
    }

    pub(crate) fn visual_icon(mut self, icon: ButtonIcon) -> Self {
        self.visual_icon = Some(icon);
        self.visual_label = None;
        self
    }

    pub(crate) fn trailing_icon(mut self, icon: ButtonIcon) -> Self {
        self.trailing_icon = Some(icon);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SwitchSpec {
    pub(crate) label: String,
    pub(crate) description: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SliderSpec {
    pub(crate) label: String,
    pub(crate) step: f64,
    pub(crate) display_value: String,
}

/// The `audit_id` naming the Tab Color HSV wheel — the ONE
/// [`UiContent::Custom`] node with a special paint lowering (an
/// [`crate::widget::DrawPrim::HsvDisk`] + the committed-color marker) and a
/// positional pointer mapping ([`CompiledUi::color_wheel_color_at`]).
pub(crate) const TAB_COLOR_WHEEL_AUDIT: &str = "settings.tab-color.wheel";

#[derive(Clone, Copy, Debug, PartialEq)]

/// The wheel's disk geometry within its painted node rect: centered, with a
/// small breathing margin so the marker ring never clips at full saturation.
pub(crate) struct ColorWheelGeometry {
    pub(crate) cx: f32,
    pub(crate) cy: f32,
    pub(crate) r: f32,
}

pub(crate) fn color_wheel_geometry(rect: LogicalRect) -> ColorWheelGeometry {
    ColorWheelGeometry {
        cx: rect.x + rect.width * 0.5,
        cy: rect.y + rect.height * 0.5,
        r: ((rect.width.min(rect.height)) * 0.5 - 8.0).max(10.0),
    }
}

/// The color under `(x, y)` on the disk painted in `rect`, or `None` outside
/// it. Hue/saturation use EXACTLY the raster's per-pixel convention (clockwise
/// turns from 12 o'clock; radius = saturation; full value).
pub(crate) fn color_wheel_rgb_at(rect: LogicalRect, x: f32, y: f32) -> Option<[u8; 3]> {
    let geometry = color_wheel_geometry(rect);
    let dx = x - geometry.cx;
    let dy = y - geometry.cy;
    let distance = (dx * dx + dy * dy).sqrt();
    if distance > geometry.r {
        return None;
    }
    let hue = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    let saturation = (distance / geometry.r).min(1.0);
    Some(crate::widget::hsv_to_rgb(hue, saturation, 1.0))
}

/// The marker position for a committed color on the wheel in `rect` — the
/// exact inverse of [`color_wheel_rgb_at`]'s polar mapping (value is projected
/// onto the full-value disk, so a dark pick still marks its hue/saturation).
pub(crate) fn color_wheel_marker_at(rect: LogicalRect, rgb: [u8; 3]) -> (f32, f32) {
    let geometry = color_wheel_geometry(rect);
    let (hue, saturation, _) = crate::widget::rgb_to_hsv(rgb);
    let theta = hue * std::f32::consts::TAU;
    (
        geometry.cx + geometry.r * saturation * theta.sin(),
        geometry.cy - geometry.r * saturation * theta.cos(),
    )
}

pub(crate) struct SliderGeometry {
    pub(crate) track_x: f32,
    pub(crate) track_right: f32,
    pub(crate) value_right: f32,
}

pub(crate) fn slider_geometry(rect: LogicalRect) -> SliderGeometry {
    let value_width = 62.0_f32.min((rect.width * 0.32).max(42.0));
    SliderGeometry {
        track_x: rect.x + 12.0,
        track_right: (rect.right() - value_width - 12.0).max(rect.x + 12.0),
        value_right: rect.right() - 10.0,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextFieldSpec {
    pub(crate) label: String,
    pub(crate) placeholder: Option<String>,
    pub(crate) secret: bool,
    /// Optional authored paint value when the semantic value intentionally
    /// reports the effective setting. Unset Settings fields paint this empty
    /// value as a quiet placeholder while accessibility and introspection read
    /// the non-empty value that is actually in effect.
    pub(crate) visual_value: Option<String>,
    /// Present only for a live editable field. Paint and accessibility consume
    /// this same immutable projection; unfocused display-only fields need none.
    pub(crate) input: Option<crate::native_text_input::TextInputProjection>,
    /// A parsed, truthful RGB cue for color values. Malformed values have no
    /// swatch, making invalid text visually distinct from a valid color.
    pub(crate) swatch: Option<[u8; 3]>,
}

fn text_field_visual_text(control: &Control<TextFieldSpec>) -> &str {
    control.spec.input.as_ref().map_or_else(
        || {
            control
                .spec
                .visual_value
                .as_deref()
                .unwrap_or_else(|| semantic_text(&control.value))
        },
        |input| input.text.as_str(),
    )
}

const TEXT_FIELD_HORIZONTAL_PADDING: f32 = 10.0;
const TEXT_FIELD_SWATCH_SIZE: f32 = 22.0;
const TEXT_FIELD_SWATCH_GAP: f32 = 8.0;
const TEXT_FIELD_GEOMETRY_BYTES: usize = 8 * 1024;

/// The horizontally windowed source interval and exact painted text band for a
/// field. The interval is grapheme aligned and bounded even for a pathological
/// configured string. Prefix widths use the same proportional-font measurer as
/// the rasterizer, so highlights, caret, and IME underline share one geometry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextFieldGeometry {
    pub(crate) text_x: f32,
    pub(crate) text_right: f32,
    pub(crate) visible: std::ops::Range<usize>,
}

pub(crate) fn text_field_geometry(
    control: &Control<TextFieldSpec>,
    rect: LogicalRect,
    text_px: f32,
) -> TextFieldGeometry {
    let text_x = rect.x + TEXT_FIELD_HORIZONTAL_PADDING;
    let swatch_space = if control.spec.swatch.is_some() {
        TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP
    } else {
        0.0
    };
    let text_right = (rect.right() - TEXT_FIELD_HORIZONTAL_PADDING - swatch_space).max(text_x);
    let available = (text_right - text_x).max(0.0);
    let text = text_field_visual_text(control);
    if text.is_empty() || available <= 0.0 {
        return TextFieldGeometry {
            text_x,
            text_right,
            visible: 0..0,
        };
    }

    let focused = control.state.focused && control.spec.input.is_some();
    let anchor = if focused {
        control
            .spec
            .input
            .as_ref()
            .map_or(0, |input| input.selection.head.min(text.len()))
    } else {
        0
    };
    let anchor = grapheme_boundary_at_or_before(text, anchor);
    let scan_start =
        grapheme_boundary_at_or_before(text, anchor.saturating_sub(TEXT_FIELD_GEOMETRY_BYTES / 2));
    let scan_end = grapheme_boundary_at_or_before(
        text,
        anchor
            .saturating_add(TEXT_FIELD_GEOMETRY_BYTES / 2)
            .min(text.len()),
    )
    .max(anchor);

    let mut start = anchor;
    if focused {
        let left_budget = available * 0.68;
        let boundaries = text[scan_start..anchor]
            .grapheme_indices()
            .map(|(boundary, _)| boundary)
            .collect::<Vec<_>>();
        for boundary in boundaries.into_iter().rev() {
            let candidate = scan_start + boundary;
            if crate::tray_raster::ui_text_width(&text[candidate..anchor], text_px) > left_budget {
                break;
            }
            start = candidate;
        }
    } else {
        start = scan_start;
    }

    let mut end = anchor;
    for (offset, grapheme) in text[anchor..scan_end].grapheme_indices() {
        let candidate = anchor + offset + grapheme.len();
        if crate::tray_raster::ui_text_width(&text[start..candidate], text_px) > available {
            break;
        }
        end = candidate;
    }
    // If there is little or no suffix, spend the remaining band on more prefix.
    if focused && end == scan_end {
        let boundaries = text[scan_start..start]
            .grapheme_indices()
            .map(|(boundary, _)| boundary)
            .collect::<Vec<_>>();
        for boundary in boundaries.into_iter().rev() {
            let candidate = scan_start + boundary;
            if crate::tray_raster::ui_text_width(&text[candidate..end], text_px) > available {
                break;
            }
            start = candidate;
        }
    }
    if end == start {
        end = text[start..]
            .graphemes()
            .next()
            .map_or(start, |grapheme| start + grapheme.len());
    }
    TextFieldGeometry {
        text_x,
        text_right,
        visible: start..end,
    }
}

pub(crate) fn text_field_x_for_byte(
    text: &str,
    geometry: &TextFieldGeometry,
    byte: usize,
    text_px: f32,
) -> f32 {
    let byte = grapheme_boundary_at_or_before(
        text,
        byte.clamp(geometry.visible.start, geometry.visible.end),
    );
    geometry.text_x
        + crate::tray_raster::ui_text_width(&text[geometry.visible.start..byte], text_px)
}

fn semantic_text(value: &SemanticValue) -> &str {
    match value {
        SemanticValue::Text(value) => value,
        SemanticValue::None | SemanticValue::Bool(_) | SemanticValue::Number { .. } => "",
    }
}

fn grapheme_boundary_at_or_before(text: &str, requested: usize) -> usize {
    let requested = requested.min(text.len());
    if requested == text.len() {
        return requested;
    }
    text.grapheme_indices()
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset <= requested)
        .last()
        .unwrap_or(0)
}

fn grapheme_boundary_at_or_after(text: &str, requested: usize) -> usize {
    let requested = requested.min(text.len());
    if requested == 0 || requested == text.len() {
        return requested;
    }
    text.grapheme_indices()
        .map(|(offset, _)| offset)
        .find(|offset| *offset >= requested)
        .unwrap_or(text.len())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RichTextSpec {
    /// Exact authored/projected source exposed to semantics. Paint may insert
    /// responsive line breaks, but accessibility and inspection never receive
    /// that whitespace-normalized visual projection in place of the source.
    pub(crate) semantic_text: String,
    pub(crate) text: String,
    pub(crate) selectable: bool,
}

/// Presentation vocabulary for the native Markdown reader. Keeping block kind
/// in the semantic tree lets paint, inspection, and accessibility agree that a
/// heading is a heading and code is code without introducing a web view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MarkdownBlockKind {
    Heading(u8),
    Paragraph,
    ListItem { depth: usize, ordinal: Option<u64> },
    Quote,
    Code { language: Option<String> },
    Table,
    Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MarkdownBlockSpec {
    pub(crate) text: String,
    pub(crate) kind: MarkdownBlockKind,
    /// A denser visual projection for exact source beside a rendered preview.
    /// Semantics retain the identical text; only the named type step changes.
    pub(crate) dense: bool,
    pub(crate) selectable: bool,
    /// Source-addressed activation (currently whole-block selection). Keeping
    /// this on the semantic node makes pointer, keyboard, accessibility, and
    /// control-socket activation share one reducer action.
    pub(crate) action: Option<ActionId>,
    /// True only when this entire visible block intersects the reader's exact
    /// source selection. Partial pointer selection is intentionally not
    /// synthesized from wrapped presentation text.
    pub(crate) selected: bool,
    pub(crate) source: std::ops::Range<usize>,
    /// Width-aware height estimated by the bounded Markdown layout engine.
    pub(crate) estimated_height: f32,
    /// First wrapped visual row painted from this block. This makes a tall
    /// single block genuinely scrollable without manufacturing pixel offsets
    /// or reparsing document bytes in the renderer.
    pub(crate) visual_row: usize,
    pub(crate) total_visual_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextViewportSpec {
    pub(crate) label: String,
    pub(crate) document_key: String,
    pub(crate) selectable: bool,
    /// Visible-only, source-addressable editor rows. `None` keeps the primitive
    /// useful for future non-editor text surfaces without inventing a second
    /// paint path.
    pub(crate) projection: Option<crate::native_editor::EditorViewportProjection>,
    pub(crate) preedit: String,
    /// Bounded paint string shown in the footer.
    pub(crate) status: Option<String>,
    /// Complete status value announced by accessibility. This is deliberately
    /// separate from `status`: fitting a narrow footer must never truncate a
    /// diagnostic before it reaches the semantic tree.
    pub(crate) semantic_status: Option<String>,
    pub(crate) minibuffer: Option<String>,
    pub(crate) cursor_label: Option<String>,
    pub(crate) dirty: bool,
    pub(crate) saving: bool,
    pub(crate) focused: bool,
    pub(crate) action: Option<ActionId>,
}

/// Canonical editor viewport geometry shared by paint and accessibility. The
/// values are logical native-app pixels; the AccessKit host applies the final
/// window scale/translation only at the semantic root.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TextViewportGeometry {
    pub(crate) header_h: f32,
    pub(crate) footer_h: f32,
    pub(crate) body_y: f32,
    pub(crate) body_h: f32,
    pub(crate) gutter_w: f32,
    pub(crate) text_x: f32,
    pub(crate) line_h: f32,
    pub(crate) cell_w: f32,
}

pub(crate) fn text_viewport_geometry(rect: LogicalRect) -> TextViewportGeometry {
    text_viewport_geometry_at_scale(rect, crate::native_appearance::text_scale())
}

pub(crate) fn text_viewport_geometry_at_scale(
    rect: LogicalRect,
    text_scale: f32,
) -> TextViewportGeometry {
    const HEADER_H: f32 = 44.0;
    const FOOTER_H: f32 = 30.0;
    const LINE_H: f32 = 20.0;
    const FALLBACK_CELL_W: f32 = 7.8;

    let text_scale = if text_scale.is_finite() && text_scale > 0.0 {
        text_scale.clamp(0.85, 2.0)
    } else {
        1.0
    };
    // Editor text is painted at the native Secondary step (13 px at 1×).
    // Measure the active mono stack with the same fixed-point metric as the
    // raster pen so custom terminal fonts cannot drift from hit-testing,
    // selections, carets, or accessibility bounds.
    let measured_cell_w = crate::tray_raster::measure_text(
        "M",
        13.0 * text_scale,
        crate::widget::TextWeight::Regular,
    );
    let cell_w = if measured_cell_w.is_finite() && measured_cell_w > 0.0 {
        measured_cell_w
    } else {
        FALLBACK_CELL_W * text_scale
    };

    // Header and footer are CHROME bands, not content: like the editor shell's
    // `chrome_scale`, they stop growing at 1.35× so a 2× Dynamic Type phone
    // keeps its line capacity for actual document text (the type inside them
    // is already clamped to the same chrome steps).
    let chrome_scale = text_scale.min(1.35);
    let header_h = (HEADER_H * chrome_scale).min(rect.height);
    let footer_h = (FOOTER_H * chrome_scale).min((rect.height - header_h).max(0.0));
    let body_y = rect.y + header_h;
    let body_h = (rect.height - header_h - footer_h).max(0.0);
    let gutter_w = if rect.width < 480.0 * text_scale {
        44.0 * text_scale
    } else {
        58.0 * text_scale
    };
    TextViewportGeometry {
        header_h,
        footer_h,
        body_y,
        body_h,
        gutter_w,
        text_x: rect.x + gutter_w + 12.0 * text_scale,
        line_h: LINE_H * text_scale,
        cell_w,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EditorShellMetrics {
    pub(crate) outer_padding: f32,
    pub(crate) compact_commands: bool,
    pub(crate) command_gap: f32,
    pub(crate) command_bar_height: f32,
    pub(crate) content_gap: f32,
    pub(crate) palette_padding: f32,
    pub(crate) palette_header_height: f32,
    pub(crate) palette_row_height: f32,
    pub(crate) palette_row_gap: f32,
    pub(crate) palette_visible_rows: usize,
    pub(crate) palette_height: f32,
}

pub(crate) fn editor_shell_metrics(
    viewport: LogicalRect,
    palette_candidates: usize,
) -> EditorShellMetrics {
    editor_shell_metrics_at_scale(
        viewport,
        palette_candidates,
        crate::native_appearance::text_scale(),
    )
}

pub(crate) fn editor_shell_metrics_at_scale(
    viewport: LogicalRect,
    palette_candidates: usize,
    text_scale: f32,
) -> EditorShellMetrics {
    let text_scale = if text_scale.is_finite() && text_scale > 0.0 {
        text_scale.clamp(0.85, 2.0)
    } else {
        1.0
    };
    let chrome_scale = text_scale.min(1.35);
    let compact_commands = viewport.width < 390.0 * text_scale.min(1.4);
    let short = viewport.height < 440.0 * text_scale.min(1.2);
    let outer_padding = if short {
        6.0 * chrome_scale
    } else if viewport.width < 560.0 * text_scale.min(1.4) {
        8.0 * chrome_scale
    } else {
        16.0 * chrome_scale
    };
    let command_gap = if compact_commands {
        4.0 * chrome_scale
    } else {
        6.0 * chrome_scale
    };
    let command_button_height = if compact_commands {
        (34.0 * text_scale).clamp(34.0, 48.0)
    } else {
        (36.0 * text_scale).clamp(36.0, 48.0)
    };
    let command_bar_height = if compact_commands {
        command_button_height * 2.0 + command_gap
    } else {
        command_button_height
    };
    let content_gap = 8.0 * chrome_scale;
    let palette_padding = 6.0 * chrome_scale;
    let palette_header_height = (28.0 * text_scale).clamp(28.0, 40.0);
    let palette_row_height = (34.0 * text_scale).clamp(34.0, 48.0);
    let palette_row_gap = 3.0 * chrome_scale;
    let minimum_editor_height = (78.0 * text_scale).clamp(78.0, 120.0);
    let available_palette_height = (viewport.height
        - outer_padding * 2.0
        - command_bar_height
        - content_gap * 2.0
        - minimum_editor_height)
        .max(0.0);
    let palette_chrome = palette_padding * 2.0 + palette_header_height;
    let palette_visible_rows = if palette_candidates == 0 {
        0
    } else {
        (((available_palette_height - palette_chrome).max(0.0)
            / (palette_row_height + palette_row_gap))
            .floor() as usize)
            .clamp(1, palette_candidates.min(COMMAND_COMPLETION_RENDER_LIMIT))
    };
    let palette_height = if palette_visible_rows == 0 {
        0.0
    } else {
        palette_chrome
            + palette_visible_rows as f32 * palette_row_height
            + palette_visible_rows as f32 * palette_row_gap
    };
    EditorShellMetrics {
        outer_padding,
        compact_commands,
        command_gap,
        command_bar_height,
        content_gap,
        palette_padding,
        palette_header_height,
        palette_row_height,
        palette_row_gap,
        palette_visible_rows,
        palette_height,
    }
}

const COMMAND_COMPLETION_RENDER_LIMIT: usize = 8;

/// Exact editor paint rectangle derived from the same responsive shell metrics
/// consumed by `EditorApp::view`. The host uses this before reducer input so
/// caret reveal and the renderer share one row-capacity truth.
pub(crate) fn editor_text_viewport_rect(viewport: LogicalRect) -> LogicalRect {
    editor_text_viewport_rect_with_palette(viewport, 0)
}

pub(crate) fn editor_text_viewport_rect_with_palette(
    viewport: LogicalRect,
    palette_candidates: usize,
) -> LogicalRect {
    editor_text_viewport_rect_at_scale(
        viewport,
        palette_candidates,
        crate::native_appearance::text_scale(),
    )
}

fn editor_text_viewport_rect_at_scale(
    viewport: LogicalRect,
    palette_candidates: usize,
    text_scale: f32,
) -> LogicalRect {
    let metrics = editor_shell_metrics_at_scale(viewport, palette_candidates, text_scale);
    let gaps = if metrics.palette_visible_rows == 0 {
        1.0
    } else {
        2.0
    };
    LogicalRect::new(
        0.0,
        0.0,
        (viewport.width - metrics.outer_padding * 2.0).max(0.0),
        (viewport.height
            - metrics.outer_padding * 2.0
            - metrics.command_bar_height
            - metrics.palette_height
            - metrics.content_gap * gaps)
            .max(0.0),
    )
}

pub(crate) fn editor_visible_line_capacity(viewport: LogicalRect) -> usize {
    editor_visible_line_capacity_at_scale(viewport, crate::native_appearance::text_scale())
}

pub(crate) fn editor_visible_line_capacity_with_palette(
    viewport: LogicalRect,
    palette_candidates: usize,
) -> usize {
    let geometry = text_viewport_geometry(editor_text_viewport_rect_with_palette(
        viewport,
        palette_candidates,
    ));
    ((geometry.body_h / geometry.line_h).ceil() as usize)
        .saturating_add(1)
        .clamp(1, 256)
}

pub(crate) fn editor_visible_line_capacity_at_scale(
    viewport: LogicalRect,
    text_scale: f32,
) -> usize {
    let geometry = text_viewport_geometry_at_scale(
        editor_text_viewport_rect_at_scale(viewport, 0, text_scale),
        text_scale,
    );
    ((geometry.body_h / geometry.line_h).ceil() as usize)
        .saturating_add(1)
        .clamp(1, 256)
}

/// Map a logical pointer location to a canonical document UTF-8 byte boundary
/// using the exact rows and geometry painted by [`paint_text_viewport`]. Header,
/// gutter, footer, clipping, and unmaterialized document text are deliberately
/// not addressable.
pub(crate) fn text_viewport_byte_at(
    spec: &TextViewportSpec,
    rect: LogicalRect,
    x: f32,
    y: f32,
) -> Option<usize> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let geometry = text_viewport_geometry(rect);
    if geometry.body_h <= 0.0
        || x < geometry.text_x
        || x >= rect.right()
        || y < geometry.body_y
        || y >= geometry.body_y + geometry.body_h
    {
        return None;
    }
    let row = ((y - geometry.body_y) / geometry.line_h).floor() as usize;
    let line = spec.projection.as_ref()?.lines.get(row)?;
    let target_column = line.column_start as f32 + (x - geometry.text_x) / geometry.cell_w;
    let mut column = line.column_start;
    for (byte, grapheme) in line.text.grapheme_indices() {
        let cells = crate::native_editor::editor_grapheme_columns(grapheme, column);
        let next = column.saturating_add(cells);
        if target_column < (column + next) as f32 * 0.5 {
            return Some(line.source.start.saturating_add(byte));
        }
        column = next;
    }
    Some(line.source.end)
}

/// Escape hatch for genuinely novel controls.  Its audit id is mandatory and
/// appears in introspection so parity gates can name the reviewed adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuditedCustomNode {
    pub(crate) audit_id: &'static str,
    pub(crate) role: SemanticRole,
    pub(crate) label: String,
    pub(crate) value: Option<String>,
    pub(crate) action: Option<ActionId>,
    pub(crate) focusable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiContent {
    Group(GroupSpec),
    Text(TextSpec),
    Button(Control<ButtonSpec>),
    Switch(Control<SwitchSpec>),
    Slider(Control<SliderSpec>),
    TextField(Control<TextFieldSpec>),
    RichText(RichTextSpec),
    MarkdownBlock(MarkdownBlockSpec),
    TextViewport(TextViewportSpec),
    /// A bounded renderer-native terminal/effect demonstration for Settings.
    /// Paint is lowered from the same semantic node as accessibility and
    /// introspection; no PTY, platform widget, or web surface is involved.
    SettingsPreview(Box<crate::settings_preview::SettingsPreviewSpec>),
    Custom(AuditedCustomNode),
}

impl UiContent {
    fn intrinsic_size(&self) -> (f32, f32) {
        let text_scale = crate::native_appearance::text_scale();
        match self {
            Self::Group(_) => (0.0, 0.0),
            Self::Text(TextSpec {
                role: SemanticRole::Heading,
                ..
            }) => (160.0 * text_scale, 32.0 * text_scale),
            Self::Text(_) => (120.0 * text_scale, 24.0 * text_scale),
            Self::Button(_) => (96.0 * text_scale, (32.0 * text_scale).max(32.0)),
            Self::Switch(_) | Self::Slider(_) | Self::TextField(_) => {
                (240.0 * text_scale, (40.0 * text_scale).max(40.0))
            }
            Self::RichText(_) => (320.0 * text_scale, 80.0 * text_scale),
            Self::MarkdownBlock(spec) => (
                640.0 * text_scale,
                if spec.estimated_height.is_finite() {
                    (spec.estimated_height * text_scale).max(1.0)
                } else {
                    1.0
                },
            ),
            Self::TextViewport(_) => (320.0 * text_scale, 200.0 * text_scale),
            Self::SettingsPreview(_) => (320.0 * text_scale, 176.0 * text_scale),
            Self::Custom(_) => (120.0 * text_scale, (40.0 * text_scale).max(40.0)),
        }
    }

    fn semantic(&self) -> SemanticProjection {
        match self {
            Self::Group(spec) => SemanticProjection {
                role: spec.role,
                label: spec.label.clone().unwrap_or_default(),
                value: SemanticValue::None,
                state: None,
                action: None,
                focusable: false,
                audit_id: None,
            },
            Self::Text(spec) => SemanticProjection {
                role: spec.role,
                label: spec.text.clone(),
                value: SemanticValue::None,
                state: None,
                action: None,
                focusable: false,
                audit_id: None,
            },
            Self::Button(control) => {
                control_projection(SemanticRole::Button, &control.spec.label, control)
            }
            Self::Switch(control) => {
                control_projection(SemanticRole::Switch, &control.spec.label, control)
            }
            Self::Slider(control) => {
                control_projection(SemanticRole::Slider, &control.spec.label, control)
            }
            Self::TextField(control) => {
                control_projection(SemanticRole::TextField, &control.spec.label, control)
            }
            Self::RichText(spec) => SemanticProjection {
                role: SemanticRole::RichText,
                label: spec.semantic_text.clone(),
                value: SemanticValue::Text(spec.semantic_text.clone()),
                state: None,
                action: None,
                focusable: spec.selectable,
                audit_id: None,
            },
            Self::MarkdownBlock(spec) => SemanticProjection {
                role: if matches!(&spec.kind, MarkdownBlockKind::Heading(_)) {
                    SemanticRole::Heading
                } else {
                    SemanticRole::RichText
                },
                label: spec.text.clone(),
                value: SemanticValue::Text(spec.text.clone()),
                state: Some(ControlState {
                    selected: spec.selected,
                    ..ControlState::default()
                }),
                action: spec.action.clone(),
                focusable: spec.selectable,
                audit_id: None,
            },
            Self::TextViewport(spec) => SemanticProjection {
                role: SemanticRole::TextViewport,
                label: spec.label.clone(),
                value: SemanticValue::Text(spec.document_key.clone()),
                state: None,
                action: spec.action.clone(),
                focusable: spec.selectable,
                audit_id: None,
            },
            Self::SettingsPreview(spec) => SemanticProjection {
                role: SemanticRole::Group,
                label: spec.semantic_label(),
                value: SemanticValue::Text(spec.semantic_value()),
                state: None,
                action: None,
                focusable: false,
                audit_id: None,
            },
            Self::Custom(spec) => SemanticProjection {
                role: spec.role,
                label: spec.label.clone(),
                value: spec
                    .value
                    .as_ref()
                    .map_or(SemanticValue::None, |v| SemanticValue::Text(v.clone())),
                state: None,
                action: spec.action.clone(),
                focusable: spec.focusable,
                audit_id: Some(spec.audit_id),
            },
        }
    }
}

fn control_projection<T>(
    role: SemanticRole,
    label: &str,
    control: &Control<T>,
) -> SemanticProjection {
    SemanticProjection {
        role,
        label: label.to_string(),
        value: control.value.clone(),
        state: Some(control.state),
        action: Some(control.action.clone()),
        focusable: control.state.enabled,
        audit_id: None,
    }
}

struct SemanticProjection {
    role: SemanticRole,
    label: String,
    value: SemanticValue,
    state: Option<ControlState>,
    action: Option<ActionId>,
    focusable: bool,
    audit_id: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UiNode {
    pub(crate) key: UiKey,
    pub(crate) layout: Layout,
    pub(crate) content: UiContent,
    pub(crate) children: Vec<UiNode>,
    /// Render this node without adding a second accessibility/focus/hit-test
    /// projection. This is reserved for responsive visual copies whose parent
    /// already carries the complete semantic label.
    pub(crate) paint_only: bool,
}

impl UiNode {
    pub(crate) fn new(key: impl Into<String>, content: UiContent) -> Self {
        Self {
            key: UiKey::new(key),
            layout: Layout::default(),
            content,
            children: Vec::new(),
            paint_only: false,
        }
    }

    pub(crate) fn layout(mut self, layout: Layout) -> Self {
        self.layout = layout;
        self
    }

    pub(crate) fn children(mut self, children: Vec<Self>) -> Self {
        self.children = children;
        self
    }

    pub(crate) fn paint_only(mut self) -> Self {
        self.paint_only = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UiTree {
    pub(crate) root: UiNode,
}

impl UiTree {
    pub(crate) fn new(root: UiNode) -> Self {
        Self { root }
    }

    /// Apply the host's one canonical keyboard-focus key to every standard
    /// control before any observer is compiled. Paint, hit testing,
    /// accessibility, and control inspection therefore see the same focus
    /// state even for controls authored by different native apps.
    pub(crate) fn apply_focus(mut self, focus: Option<&UiKey>) -> Self {
        self.apply_interaction(focus, None, None, true);
        self
    }

    /// Apply all transient host interaction state in the same pre-compile pass
    /// as keyboard focus. Hover/press never become parallel paint-only state.
    pub(crate) fn apply_interaction(
        &mut self,
        focus: Option<&UiKey>,
        hovered: Option<&UiKey>,
        pressed: Option<&UiKey>,
        focus_visible: bool,
    ) {
        fn visit(node: &mut UiNode, focus: Option<&UiKey>) {
            let focused = focus.is_some_and(|key| key == &node.key);
            match &mut node.content {
                UiContent::Button(control) => control.state.focused = focused,
                UiContent::Switch(control) => control.state.focused = focused,
                UiContent::Slider(control) => control.state.focused = focused,
                UiContent::TextField(control) => control.state.focused = focused,
                UiContent::Group(_)
                | UiContent::Text(_)
                | UiContent::RichText(_)
                | UiContent::MarkdownBlock(_)
                | UiContent::TextViewport(_)
                | UiContent::SettingsPreview(_)
                | UiContent::Custom(_) => {}
            }
            for child in &mut node.children {
                visit(child, focus);
            }
        }

        visit(&mut self.root, focus);
        fn transient(
            node: &mut UiNode,
            hovered: Option<&UiKey>,
            pressed: Option<&UiKey>,
            focus_visible: bool,
        ) {
            let is_hovered = hovered.is_some_and(|key| key == &node.key);
            let is_pressed = pressed.is_some_and(|key| key == &node.key);
            let set = |state: &mut ControlState| {
                state.hovered = is_hovered;
                state.pressed = is_pressed && is_hovered;
                state.focus_visible = state.focused && focus_visible;
            };
            match &mut node.content {
                UiContent::Button(control) => set(&mut control.state),
                UiContent::Switch(control) => set(&mut control.state),
                UiContent::Slider(control) => set(&mut control.state),
                UiContent::TextField(control) => set(&mut control.state),
                UiContent::Group(_)
                | UiContent::Text(_)
                | UiContent::RichText(_)
                | UiContent::MarkdownBlock(_)
                | UiContent::TextViewport(_)
                | UiContent::SettingsPreview(_)
                | UiContent::Custom(_) => {}
            }
            for child in &mut node.children {
                transient(child, hovered, pressed, focus_visible);
            }
        }
        transient(&mut self.root, hovered, pressed, focus_visible);
    }

    /// Compile all observable UI products from one typed tree.
    pub(crate) fn compile(&self, viewport: LogicalRect) -> Result<CompiledUi, CompileError> {
        if !viewport.is_valid() || viewport.is_empty() {
            return Err(CompileError::InvalidViewport);
        }
        let mut compiler = Compiler {
            seen: HashSet::new(),
            output: CompiledUi {
                bounds: viewport,
                ..CompiledUi::default()
            },
        };
        compiler.node(&self.root, viewport, viewport, None)?;
        // From the AUTHORED tree, not the clipped observers — see the field doc.
        fn find_default(node: &UiNode) -> Option<(UiKey, ActionId)> {
            if let UiContent::Button(control) = &node.content
                && control.style == StyleRef::Primary
                && control.state.enabled
            {
                return Some((node.key.clone(), control.action.clone()));
            }
            node.children.iter().find_map(find_default)
        }
        compiler.output.default_action = find_default(&self.root);
        Ok(compiler.output)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaintNode {
    pub(crate) key: UiKey,
    pub(crate) rect: LogicalRect,
    pub(crate) clip: LogicalRect,
    pub(crate) content: UiContent,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HitRegion {
    pub(crate) key: UiKey,
    pub(crate) rect: LogicalRect,
    pub(crate) action: ActionId,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticNode {
    pub(crate) key: UiKey,
    pub(crate) parent: Option<UiKey>,
    pub(crate) rect: LogicalRect,
    pub(crate) role: SemanticRole,
    pub(crate) label: String,
    pub(crate) value: SemanticValue,
    pub(crate) state: Option<ControlState>,
    pub(crate) action: Option<ActionId>,
    pub(crate) audit_id: Option<&'static str>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CompiledUi {
    pub(crate) bounds: LogicalRect,
    pub(crate) paint: Vec<PaintNode>,
    pub(crate) hits: Vec<HitRegion>,
    pub(crate) semantics: Vec<SemanticNode>,
    pub(crate) focus_order: Vec<UiKey>,
    /// The page's DEFAULT button — what a bare Return activates when NO
    /// control holds keyboard focus, mirroring the native "Return fires the
    /// highlighted default button" convention: the first enabled
    /// `StyleRef::Primary` button in authoring (= visual) order. Pages follow
    /// the platform rule of one primary-styled action per state (Software
    /// Update swaps Primary between "Check for Updates" and "Install &
    /// Relaunch" as a build stages), so "first" is the unique one. Recorded
    /// from the AUTHORED tree before viewport clipping — a route's default is
    /// semantic, not visual, so Return works even with the button scrolled out
    /// of view (clipped subtrees are absent from every other observer here).
    /// A focused control and a focused text field's Submit take precedence at
    /// the call site, and Space never falls back to this (Space activates only
    /// the focused control on macOS).
    pub(crate) default_action: Option<(UiKey, ActionId)>,
}

impl CompiledUi {
    /// Topmost enabled control at the point, matching paint order.
    pub(crate) fn hit_test(&self, x: f32, y: f32) -> Option<&HitRegion> {
        self.hits.iter().rev().find(|hit| hit.rect.contains(x, y))
    }

    pub(crate) fn semantic(&self, key: &UiKey) -> Option<&SemanticNode> {
        self.semantics.iter().find(|node| &node.key == key)
    }

    /// Snap one pointer position through the exact HSV disk painted for `key`
    /// (the Tab Color wheel — a [`UiContent::Custom`] node whose `audit_id` is
    /// [`TAB_COLOR_WHEEL_AUDIT`]). Returns the picked `[r, g, b]` only for a
    /// point INSIDE the disk, using the SAME polar-hue math the rasterizer
    /// paints (`tray_raster::hsv_disk`), so the committed color and the pixel
    /// under the pointer can never disagree. Anywhere else (including the
    /// node's corners outside the disk) is `None` — a miss, not a pick.
    pub(crate) fn color_wheel_color_at(&self, key: &UiKey, x: f32, y: f32) -> Option<[u8; 3]> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let paint = self.paint.iter().find(|node| &node.key == key)?;
        let UiContent::Custom(spec) = &paint.content else {
            return None;
        };
        if spec.audit_id != TAB_COLOR_WHEEL_AUDIT {
            return None;
        }
        color_wheel_rgb_at(paint.rect, x, y)
    }

    /// Snap one pointer x-coordinate through the exact slider track painted for
    /// `key`. The returned number is reducer-ready and honors authored min/max/
    /// step; callers never infer a range from labels or app-specific state.
    pub(crate) fn slider_value_at(&self, key: &UiKey, x: f32) -> Option<f64> {
        if !x.is_finite() {
            return None;
        }
        let paint = self.paint.iter().find(|node| &node.key == key)?;
        let UiContent::Slider(control) = &paint.content else {
            return None;
        };
        let SemanticValue::Number {
            minimum, maximum, ..
        } = control.value
        else {
            return None;
        };
        if !minimum.is_finite()
            || !maximum.is_finite()
            || maximum <= minimum
            || !control.spec.step.is_finite()
            || control.spec.step <= 0.0
        {
            return None;
        }
        let geometry = slider_geometry(paint.rect);
        let width = geometry.track_right - geometry.track_x;
        if width <= 0.0 {
            return Some(minimum);
        }
        if x < geometry.track_x || x > geometry.track_right {
            return None;
        }
        let fraction = ((x - geometry.track_x) / width).clamp(0.0, 1.0) as f64;
        let raw = minimum + (maximum - minimum) * fraction;
        let steps = ((raw - minimum) / control.spec.step).round();
        Some((minimum + steps * control.spec.step).clamp(minimum, maximum))
    }

    /// Resolve a pointer x-coordinate to the nearest UTF-8 grapheme boundary
    /// in the exact bounded text projection painted for `key`.
    ///
    /// The full control remains a useful click target: leading padding clamps
    /// to the first visible boundary, while trailing padding and a color swatch
    /// clamp to the last. Width comparisons use the same proportional text
    /// measurer as paint. The visible projection is capped by
    /// `TEXT_FIELD_GEOMETRY_BYTES`, so even an adversarial field cannot turn a
    /// pointer event into an unbounded full-value scan.
    pub(crate) fn text_field_byte_at(&self, key: &UiKey, x: f32) -> Option<usize> {
        if !x.is_finite() {
            return None;
        }
        let paint = self.paint.iter().find(|node| &node.key == key)?;
        let UiContent::TextField(control) = &paint.content else {
            return None;
        };
        if !control.state.enabled {
            return None;
        }
        let text = text_field_visual_text(control);
        if text.is_empty() {
            return Some(0);
        }

        let text_px = native_type_px(crate::type_scale::TypeStep::Secondary).get();
        let geometry = text_field_geometry(control, paint.rect, text_px);
        if x <= geometry.text_x {
            return Some(geometry.visible.start);
        }
        if x >= geometry.text_right {
            return Some(geometry.visible.end);
        }

        let visible = &text[geometry.visible.clone()];
        let mut boundaries = Vec::with_capacity(visible.len().min(TEXT_FIELD_GEOMETRY_BYTES) + 1);
        boundaries.push(geometry.visible.start);
        boundaries.extend(
            visible
                .grapheme_indices()
                .map(|(offset, grapheme)| geometry.visible.start + offset + grapheme.len()),
        );
        boundaries.dedup();

        let width_at = |byte: usize| {
            crate::tray_raster::ui_text_width(
                &text[geometry.visible.start..byte.min(geometry.visible.end)],
                text_px,
            )
        };
        let target = x - geometry.text_x;
        let total = width_at(geometry.visible.end);
        if target >= total {
            return Some(geometry.visible.end);
        }
        let right_index = boundaries.partition_point(|boundary| width_at(*boundary) < target);
        let right_index = right_index.min(boundaries.len().saturating_sub(1));
        let right = boundaries[right_index];
        let left = boundaries[right_index.saturating_sub(1)];
        let left_width = width_at(left);
        let right_width = width_at(right);
        Some(if target - left_width <= right_width - target {
            left
        } else {
            right
        })
    }

    /// Canonical control serialization generated from semantic nodes—not from
    /// app-specific side tables.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        self.semantics
            .iter()
            .map(|node| {
                let action = node.action.as_ref().map_or("-", ActionId::as_str);
                let value = match &node.value {
                    SemanticValue::None => "-".to_string(),
                    SemanticValue::Text(value) => format!("{value:?}"),
                    SemanticValue::Bool(value) => value.to_string(),
                    SemanticValue::Number {
                        value,
                        minimum,
                        maximum,
                    } => format!("{value} range={minimum}..{maximum}"),
                };
                let state = node.state.map_or_else(
                    || "-".to_string(),
                    |state| {
                        format!(
                            "enabled:{} focused:{} focus-visible:{} hovered:{} pressed:{} selected:{} invalid:{} busy:{}",
                            state.enabled,
                            state.focused,
                            state.focus_visible,
                            state.hovered,
                            state.pressed,
                            state.selected,
                            state.invalid,
                            state.busy,
                        )
                    },
                );
                format!(
                    "ui key={:?} role={:?} label={:?} value={} action={} state={} rect={:.1},{:.1},{:.1},{:.1}",
                    node.key.as_str(),
                    node.role,
                    node.label,
                    value,
                    action,
                    state,
                    node.rect.x,
                    node.rect.y,
                    node.rect.width,
                    node.rect.height,
                )
            })
            .collect()
    }

    /// Complete paint/semantic inspection for the exact compiled viewport.
    ///
    /// The first line and `paint-text` records remain backward compatible with
    /// the original text-fit audit. The additive `paint-node` and typed records
    /// make the compiled renderer artifact self-describing: authored geometry,
    /// effective ancestor clip, semantics, interaction state, focus order, and
    /// paint-only state are all bound to the same [`CompiledUi`].
    pub(crate) fn paint_audit_lines(&self) -> Vec<String> {
        let audits = self
            .paint
            .iter()
            .filter_map(text_fit_audit)
            .collect::<Vec<_>>();
        let overflow = audits.iter().filter(|audit| audit.overflow).count();
        let clipped = self
            .paint
            .iter()
            .filter(|node| materially_clipped(node.rect, node.clip))
            .count();
        let mut lines = vec![format!(
            "paint-audit text-nodes={} overflow={overflow} viewport={:.1},{:.1},{:.1},{:.1} nodes={} semantics={} hits={} focusable={} clipped={clipped} compiled-fingerprint={:016x}",
            audits.len(),
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            self.bounds.height,
            self.paint.len(),
            self.semantics.len(),
            self.hits.len(),
            self.focus_order.len(),
            self.fingerprint(),
        )];
        lines.extend(self.paint.iter().map(|paint| self.paint_node_audit(paint)));
        lines.extend(audits.into_iter().map(|audit| {
            format!(
                "paint-text key={:?} kind={} overflow={} clip-truncated={} required={:.1} available={:.1} authored-available={:.1} painted={:?}",
                audit.key.as_str(),
                audit.kind,
                audit.overflow,
                audit.clip_truncated,
                audit.required,
                audit.available,
                audit.authored_available,
                audit.painted,
            )
        }));
        for paint in &self.paint {
            match &paint.content {
                UiContent::SettingsPreview(spec) => {
                    let animation = match spec.animation() {
                        crate::settings_preview::PreviewAnimation::None => "none".to_string(),
                        crate::settings_preview::PreviewAnimation::BlinkEdge { after_ms } => {
                            format!("blink-edge:{after_ms}ms")
                        }
                        crate::settings_preview::PreviewAnimation::Continuous => {
                            "continuous".to_string()
                        }
                    };
                    lines.push(format!(
                        "paint-preview key={:?} scene={:?} animation={} audit-state={:?} paint-fingerprint={:016x}",
                        paint.key.as_str(),
                        spec.semantic_label(),
                        animation,
                        spec.audit_value(),
                        spec.paint_fingerprint(),
                    ));
                }
                UiContent::MarkdownBlock(spec) => {
                    lines.push(markdown_paint_audit(paint, spec));
                }
                UiContent::TextViewport(spec) => {
                    lines.push(text_viewport_paint_audit(paint, spec));
                }
                _ => {}
            }
        }
        lines
    }

    fn paint_node_audit(&self, paint: &PaintNode) -> String {
        let semantic = self.semantic(&paint.key);
        let role = semantic.map_or_else(|| "-".to_string(), |node| format!("{:?}", node.role));
        let label = semantic.map_or_else(String::new, |node| node.label.clone());
        let value =
            semantic.map_or_else(|| "-".to_string(), |node| audit_semantic_value(&node.value));
        let action = semantic
            .and_then(|node| node.action.as_ref())
            .map_or("-", ActionId::as_str);
        let state =
            semantic.map_or_else(|| "-".to_string(), |node| audit_control_state(node.state));
        let audit_id = semantic.and_then(|node| node.audit_id).unwrap_or("-");
        let focus_index = self
            .focus_order
            .iter()
            .position(|key| key == &paint.key)
            .map_or_else(|| "-".to_string(), |index| index.to_string());
        format!(
            "paint-node key={:?} kind={} rect={:.1},{:.1},{:.1},{:.1} clip={:.1},{:.1},{:.1},{:.1} visible={} clipped={} role={} label={:?} value={} action={} state={} focus-index={} audit-id={}",
            paint.key.as_str(),
            ui_content_kind(&paint.content),
            paint.rect.x,
            paint.rect.y,
            paint.rect.width,
            paint.rect.height,
            paint.clip.x,
            paint.clip.y,
            paint.clip.width,
            paint.clip.height,
            !paint.clip.is_empty(),
            materially_clipped(paint.rect, paint.clip),
            role,
            label,
            value,
            action,
            state,
            focus_index,
            audit_id,
        )
    }

    /// Stable fingerprint of every visible semantic value and paint projection
    /// plus exact compiled geometry. The host folds this into its repaint and
    /// raster-cache keys; responsive paint-only copy must therefore participate
    /// even though accessibility deliberately does not publish it.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.bounds.x.to_bits().hash(&mut hash);
        self.bounds.y.to_bits().hash(&mut hash);
        self.bounds.width.to_bits().hash(&mut hash);
        self.bounds.height.to_bits().hash(&mut hash);
        for line in self.controls_lines() {
            line.hash(&mut hash);
        }
        // Paint can intentionally differ from semantics (compact button labels,
        // responsive status copy, editor windows, preview animation). Hash the
        // typed paint product itself so none of those projections can stale-serve
        // an old retained raster. Large custom paint types keep their bounded
        // purpose-built hashes; ordinary controls use their complete Debug
        // projection, which is deterministic for these value-only structs.
        for node in &self.paint {
            node.key.hash(&mut hash);
            node.rect.x.to_bits().hash(&mut hash);
            node.rect.y.to_bits().hash(&mut hash);
            node.rect.width.to_bits().hash(&mut hash);
            node.rect.height.to_bits().hash(&mut hash);
            node.clip.x.to_bits().hash(&mut hash);
            node.clip.y.to_bits().hash(&mut hash);
            node.clip.width.to_bits().hash(&mut hash);
            node.clip.height.to_bits().hash(&mut hash);
            ui_content_kind(&node.content).hash(&mut hash);
            match &node.content {
                UiContent::SettingsPreview(spec) => spec.paint_fingerprint().hash(&mut hash),
                UiContent::MarkdownBlock(spec) => {
                    markdown_paint_fingerprint(node, spec).hash(&mut hash);
                }
                UiContent::TextViewport(spec) => text_viewport_fingerprint(spec).hash(&mut hash),
                content => format!("{content:?}").hash(&mut hash),
            }
        }
        hash.finish() | 1
    }

    /// Bootstrap native-app painter. Typed controls lower to the shared
    /// renderer-independent draw vocabulary, so CPU, GPU, and image capture
    /// consume identical pixels. Document apps may later lower large text runs
    /// directly to the retained scene without changing their semantic tree.
    pub(crate) fn tray(
        &self,
        theme: aterm_render::Theme,
        base_px: f32,
    ) -> crate::widget::TrayInput {
        let roles = crate::settings::Roles::from_theme(theme);
        let mut prims = Vec::with_capacity(self.paint.len() * 5);
        for node in &self.paint {
            prims.push(crate::widget::DrawPrim::ClipPush {
                x: node.clip.x,
                y: node.clip.y,
                w: node.clip.width,
                h: node.clip.height,
            });
            paint_compiled_node(&mut prims, node, roles, theme, base_px);
            prims.push(crate::widget::DrawPrim::ClipPop);
        }
        crate::widget::TrayInput {
            prims,
            card: (
                self.bounds.x,
                self.bounds.y,
                self.bounds.width,
                self.bounds.height,
            ),
        }
    }

    /// Assert the cross-observer invariant for every actionable semantic node.
    pub(crate) fn validate_parity(&self) -> Result<(), CompileError> {
        for semantic in self
            .semantics
            .iter()
            .filter(|node| node.action.is_some() && node.state.is_none_or(|state| state.enabled))
        {
            let painted = self
                .paint
                .iter()
                .filter(|node| node.key == semantic.key)
                .count();
            let hit = self
                .hits
                .iter()
                .filter(|node| node.key == semantic.key)
                .count();
            if painted != 1 || hit != 1 {
                return Err(CompileError::ObserverMismatch(semantic.key.clone()));
            }
            let hit_action = self
                .hits
                .iter()
                .find(|node| node.key == semantic.key)
                .map(|node| &node.action);
            if hit_action != semantic.action.as_ref() {
                return Err(CompileError::ObserverMismatch(semantic.key.clone()));
            }
        }
        Ok(())
    }
}

fn hash_text_viewport<H: std::hash::Hasher>(spec: &TextViewportSpec, hash: &mut H) {
    use std::hash::Hash;

    spec.label.hash(hash);
    spec.document_key.hash(hash);
    spec.selectable.hash(hash);
    spec.preedit.hash(hash);
    spec.status.hash(hash);
    spec.semantic_status.hash(hash);
    spec.minibuffer.hash(hash);
    spec.cursor_label.hash(hash);
    spec.dirty.hash(hash);
    spec.saving.hash(hash);
    spec.focused.hash(hash);
    spec.action.hash(hash);
    let Some(projection) = spec.projection.as_ref() else {
        0_u8.hash(hash);
        return;
    };
    1_u8.hash(hash);
    projection.first_line.hash(hash);
    projection.total_lines.hash(hash);
    projection.lines.len().hash(hash);
    for line in &projection.lines {
        line.number.hash(hash);
        line.source.start.hash(hash);
        line.source.end.hash(hash);
        line.column_start.hash(hash);
        line.text.hash(hash);
        line.selections.len().hash(hash);
        for selection in &line.selections {
            selection.bytes.start.hash(hash);
            selection.bytes.end.hash(hash);
            selection.continues.hash(hash);
            selection.primary.hash(hash);
        }
        line.carets.hash(hash);
        line.syntax.len().hash(hash);
        for syntax in &line.syntax {
            syntax.bytes.start.hash(hash);
            syntax.bytes.end.hash(hash);
            let class = match syntax.class {
                crate::native_editor::EditorSyntaxClass::Table => 0_u8,
                crate::native_editor::EditorSyntaxClass::Key => 1,
                crate::native_editor::EditorSyntaxClass::String => 2,
                crate::native_editor::EditorSyntaxClass::Number => 3,
                crate::native_editor::EditorSyntaxClass::Boolean => 4,
                crate::native_editor::EditorSyntaxClass::Comment => 5,
            };
            class.hash(hash);
        }
        line.diagnostics.len().hash(hash);
        for diagnostic in &line.diagnostics {
            diagnostic.bytes.start.hash(hash);
            diagnostic.bytes.end.hash(hash);
            diagnostic.error.hash(hash);
        }
    }
}

fn text_viewport_fingerprint(spec: &TextViewportSpec) -> u64 {
    use std::hash::Hasher;

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    hash_text_viewport(spec, &mut hash);
    hash.finish() | 1
}

fn audit_semantic_value(value: &SemanticValue) -> String {
    match value {
        SemanticValue::None => "-".to_string(),
        SemanticValue::Text(value) => format!("{value:?}"),
        SemanticValue::Bool(value) => value.to_string(),
        SemanticValue::Number {
            value,
            minimum,
            maximum,
        } => format!("{value} range={minimum}..{maximum}"),
    }
}

fn audit_control_state(state: Option<ControlState>) -> String {
    state.map_or_else(
        || "-".to_string(),
        |state| {
            format!(
                "enabled:{} focused:{} focus-visible:{} hovered:{} pressed:{} selected:{} invalid:{} busy:{}",
                state.enabled,
                state.focused,
                state.focus_visible,
                state.hovered,
                state.pressed,
                state.selected,
                state.invalid,
                state.busy,
            )
        },
    )
}

const fn ui_content_kind(content: &UiContent) -> &'static str {
    match content {
        UiContent::Group(_) => "group",
        UiContent::Text(_) => "text",
        UiContent::Button(_) => "button",
        UiContent::Switch(_) => "switch",
        UiContent::Slider(_) => "slider",
        UiContent::TextField(_) => "text-field",
        UiContent::RichText(_) => "rich-text",
        UiContent::MarkdownBlock(_) => "markdown-block",
        UiContent::TextViewport(_) => "text-viewport",
        UiContent::SettingsPreview(_) => "settings-preview",
        UiContent::Custom(_) => "custom",
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TextFitAudit {
    key: UiKey,
    kind: &'static str,
    overflow: bool,
    clip_truncated: bool,
    required: f32,
    available: f32,
    authored_available: f32,
    painted: String,
}

fn effective_horizontal_width(node: &PaintNode, left: f32, right: f32) -> f32 {
    let left = left.max(node.clip.x);
    let right = right.min(node.clip.right());
    (right - left).max(0.0)
}

fn finish_text_fit(
    node: &PaintNode,
    kind: &'static str,
    text: &str,
    painted: String,
    required: f32,
    authored_available: f32,
    available: f32,
) -> TextFitAudit {
    let clip_truncated = materially_clipped(node.rect, node.clip)
        || available + CLIP_EDGE_EPSILON < authored_available;
    TextFitAudit {
        key: node.key.clone(),
        kind,
        overflow: painted != text || required > available + 0.5 || clip_truncated,
        clip_truncated,
        required,
        available,
        authored_available,
        painted,
    }
}

fn text_fit_audit(node: &PaintNode) -> Option<TextFitAudit> {
    match &node.content {
        UiContent::Text(spec) => {
            let authored = text_visual_projection(spec, node.rect.width);
            let available = effective_horizontal_width(node, node.rect.x, node.rect.right());
            Some(finish_text_fit(
                node,
                "text",
                &spec.text,
                authored.painted,
                authored.required,
                authored.available,
                available,
            ))
        }
        UiContent::Button(control) => {
            let navigation = control.style == StyleRef::Navigation;
            let size = native_type_px(TypeStep::Secondary).get();
            let (label, text_x, text_right) = if control.spec.visual_icon.is_some() {
                if navigation && node.rect.width >= 96.0 {
                    (
                        control
                            .spec
                            .visual_label
                            .as_ref()
                            .unwrap_or(&control.spec.label),
                        node.rect.x + 40.0,
                        node.rect.right() - 12.0,
                    )
                } else {
                    return None;
                }
            } else {
                let label = control
                    .spec
                    .visual_label
                    .as_ref()
                    .unwrap_or(&control.spec.label);
                let text_x = if navigation && control.spec.visual_label.is_some() {
                    let width = crate::tray_raster::ui_text_width(label, size);
                    node.rect.x + ((node.rect.width - width) / 2.0).max(0.0)
                } else {
                    node.rect.x + if navigation { 12.0 } else { 10.0 }
                };
                let trailing_reserve = if control.spec.trailing_icon.is_some() {
                    38.0
                } else {
                    10.0
                };
                (label, text_x, node.rect.right() - trailing_reserve)
            };
            let authored_available = (text_right - text_x).max(0.0);
            let available = effective_horizontal_width(node, text_x, text_right);
            let painted = elide_ui_label(label, authored_available, size);
            Some(finish_text_fit(
                node,
                "button",
                label,
                painted,
                crate::tray_raster::ui_text_width(label, size),
                authored_available,
                available,
            ))
        }
        UiContent::Switch(control) => {
            let text = if matches!(control.value, SemanticValue::Bool(true)) {
                "On"
            } else {
                "Off"
            };
            let size = native_type_px(TypeStep::Secondary).get();
            let track_w = 42.0_f32.min((node.rect.width - 8.0).max(0.0));
            let text_x = node.rect.x + 10.0;
            let text_right = node.rect.right() - track_w - 12.0;
            let authored_available = (text_right - text_x).max(0.0);
            let available = effective_horizontal_width(node, text_x, text_right);
            Some(finish_text_fit(
                node,
                "switch",
                text,
                text.to_string(),
                crate::tray_raster::ui_text_width(text, size),
                authored_available,
                available,
            ))
        }
        UiContent::Slider(control) => {
            let size = native_type_px(TypeStep::Caption).get();
            let geometry = slider_geometry(node.rect);
            let required = crate::tray_raster::ui_text_width(&control.spec.display_value, size);
            let text_x = (geometry.value_right - required).max(geometry.track_right + 6.0);
            let authored_available = (geometry.value_right - text_x).max(0.0);
            let available = effective_horizontal_width(node, text_x, geometry.value_right);
            Some(finish_text_fit(
                node,
                "slider",
                &control.spec.display_value,
                control.spec.display_value.clone(),
                required,
                authored_available,
                available,
            ))
        }
        UiContent::TextField(control) => {
            let size = native_type_px(TypeStep::Secondary).get();
            let geometry = text_field_geometry(control, node.rect, size);
            let actual = text_field_visual_text(control);
            let source = if actual.is_empty() {
                control.spec.placeholder.clone().unwrap_or_default()
            } else {
                actual.to_string()
            };
            let painted = if actual.is_empty() {
                source.clone()
            } else {
                actual[geometry.visible.clone()].to_string()
            };
            let authored_available = (geometry.text_right - geometry.text_x).max(0.0);
            let available = effective_horizontal_width(node, geometry.text_x, geometry.text_right);
            let required = crate::tray_raster::ui_text_width(&source, size);
            Some(finish_text_fit(
                node,
                "text-field",
                &source,
                painted.clone(),
                required,
                authored_available,
                available,
            ))
        }
        UiContent::RichText(spec) => {
            let size = native_type_px(TypeStep::Body).get();
            let line_h = (size * 1.45).max(16.0);
            let required = spec
                .text
                .lines()
                .map(|line| crate::tray_raster::ui_text_width(line, size))
                .fold(0.0_f32, f32::max);
            let available = effective_horizontal_width(node, node.rect.x, node.rect.right());
            let required_height = spec.text.lines().count().max(1) as f32 * line_h;
            let visible_height = node.clip.height.min(node.rect.height);
            let mut audit = finish_text_fit(
                node,
                "rich-text",
                &spec.text,
                spec.text.clone(),
                required,
                node.rect.width,
                available,
            );
            audit.overflow |= required_height > visible_height + 0.5;
            Some(audit)
        }
        UiContent::Custom(spec) => {
            let size = native_type_px(TypeStep::Body).get();
            let available = effective_horizontal_width(node, node.rect.x, node.rect.right());
            Some(finish_text_fit(
                node,
                "custom",
                &spec.label,
                spec.label.clone(),
                crate::tray_raster::ui_text_width(&spec.label, size),
                node.rect.width,
                available,
            ))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
struct MarkdownAuditLayout {
    text_x: f32,
    text_y: f32,
    text_w: f32,
    px: f32,
    line_h: f32,
    max_columns: usize,
    max_lines: usize,
    preserve_lines: bool,
    face: crate::widget::TextFace,
}

fn markdown_audit_layout(
    rect: LogicalRect,
    spec: &MarkdownBlockSpec,
) -> Option<MarkdownAuditLayout> {
    use crate::widget::TextFace;

    let (mut text_x, mut text_y, mut text_w) = (rect.x, rect.y, rect.width);
    let (step, face, line_multiplier, preserve_lines) = match &spec.kind {
        MarkdownBlockKind::Heading(1) => (TypeStep::Display, TextFace::UiBold, 1.32, false),
        MarkdownBlockKind::Heading(2) => (TypeStep::Title, TextFace::UiBold, 1.35, false),
        MarkdownBlockKind::Heading(_) => (TypeStep::Body, TextFace::UiBold, 1.45, false),
        MarkdownBlockKind::Paragraph => (TypeStep::Body, TextFace::Ui, 1.72, false),
        MarkdownBlockKind::ListItem { depth, .. } => {
            let indent = (*depth as f32 * 16.0).min(64.0);
            text_x += 28.0 + indent;
            text_w = (text_w - 28.0 - indent).max(20.0);
            (TypeStep::Body, TextFace::Ui, 1.66, false)
        }
        MarkdownBlockKind::Quote => {
            text_x += 18.0;
            text_y += 8.0;
            text_w = (text_w - 30.0).max(20.0);
            (TypeStep::Body, TextFace::Ui, 1.66, false)
        }
        MarkdownBlockKind::Code { language } => {
            text_x += 12.0;
            text_y += if language.is_some() { 24.0 } else { 10.0 };
            text_w = (text_w - 24.0).max(20.0);
            (
                if spec.dense {
                    TypeStep::Caption
                } else {
                    TypeStep::Body
                },
                TextFace::Mono,
                1.55,
                true,
            )
        }
        MarkdownBlockKind::Table => {
            text_x += 12.0;
            text_y += 8.0;
            text_w = (text_w - 24.0).max(20.0);
            (TypeStep::Body, TextFace::Mono, 1.65, true)
        }
        MarkdownBlockKind::Rule => return None,
    };
    let px = native_type_px(step).get();
    let line_h = (px * line_multiplier).max(16.0);
    let max_lines = ((rect.bottom() - text_y).max(0.0) / line_h).ceil().max(1.0) as usize;
    let average_advance = if face == TextFace::Mono {
        px * 0.62
    } else {
        px * 0.55
    };
    let max_columns = (text_w / average_advance.max(1.0)).floor().max(4.0) as usize;
    Some(MarkdownAuditLayout {
        text_x,
        text_y,
        text_w,
        px,
        line_h,
        max_columns,
        max_lines,
        preserve_lines,
        face,
    })
}

fn markdown_kind_label(kind: &MarkdownBlockKind) -> String {
    match kind {
        MarkdownBlockKind::Heading(level) => format!("heading-{level}"),
        MarkdownBlockKind::Paragraph => "paragraph".to_string(),
        MarkdownBlockKind::ListItem { depth, ordinal } => ordinal.map_or_else(
            || format!("list-item:depth-{depth}:unordered"),
            |ordinal| format!("list-item:depth-{depth}:ordinal-{ordinal}"),
        ),
        MarkdownBlockKind::Quote => "quote".to_string(),
        MarkdownBlockKind::Code { language } => format!(
            "code:{}",
            language
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("plain")
        ),
        MarkdownBlockKind::Table => "table".to_string(),
        MarkdownBlockKind::Rule => "rule".to_string(),
    }
}

fn markdown_paint_fingerprint(node: &PaintNode, spec: &MarkdownBlockSpec) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    node.rect.x.to_bits().hash(&mut hash);
    node.rect.y.to_bits().hash(&mut hash);
    node.rect.width.to_bits().hash(&mut hash);
    node.rect.height.to_bits().hash(&mut hash);
    node.clip.x.to_bits().hash(&mut hash);
    node.clip.y.to_bits().hash(&mut hash);
    node.clip.width.to_bits().hash(&mut hash);
    node.clip.height.to_bits().hash(&mut hash);
    markdown_kind_label(&spec.kind).hash(&mut hash);
    spec.text.hash(&mut hash);
    spec.dense.hash(&mut hash);
    spec.selectable.hash(&mut hash);
    spec.action.hash(&mut hash);
    spec.selected.hash(&mut hash);
    spec.source.start.hash(&mut hash);
    spec.source.end.hash(&mut hash);
    spec.estimated_height.to_bits().hash(&mut hash);
    spec.visual_row.hash(&mut hash);
    spec.total_visual_rows.hash(&mut hash);
    hash.finish() | 1
}

fn markdown_paint_audit(node: &PaintNode, spec: &MarkdownBlockSpec) -> String {
    let fingerprint = markdown_paint_fingerprint(node, spec);
    let Some(layout) = markdown_audit_layout(node.rect, spec) else {
        return format!(
            "paint-markdown key={:?} block-kind=rule source={}..{} selected={} selectable={} dense={} visual-row={}/{} estimated-height={:.1} wrapped-lines=0 visible-lines=0 elided=false required-width=0.0 available=0.0 required-height=1.0 text={:?} paint-fingerprint={fingerprint:016x}",
            node.key.as_str(),
            spec.source.start,
            spec.source.end,
            spec.selected,
            spec.selectable,
            spec.dense,
            spec.visual_row,
            spec.total_visual_rows,
            spec.estimated_height,
            spec.text,
        );
    };
    let painted = wrap_markdown_text_window(
        &spec.text,
        layout.max_columns,
        spec.visual_row,
        layout.max_lines,
        layout.preserve_lines,
    );
    let visible_lines = painted
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            let top = layout.text_y + *index as f32 * layout.line_h;
            top < node.clip.bottom() && top + layout.line_h > node.clip.y
        })
        .count();
    let required_width = spec
        .text
        .lines()
        .map(|line| visual_text_width(line, layout.px, layout.face))
        .fold(0.0_f32, f32::max);
    let available = effective_horizontal_width(node, layout.text_x, layout.text_x + layout.text_w);
    let required_height = spec.total_visual_rows.max(1) as f32 * layout.line_h;
    let elided = painted.last().is_some_and(|line| line.ends_with('…'))
        || visible_lines < painted.len()
        || available + 0.5 < layout.text_w;
    format!(
        "paint-markdown key={:?} block-kind={} source={}..{} selected={} selectable={} dense={} visual-row={}/{} estimated-height={:.1} wrapped-lines={} visible-lines={} elided={} required-width={:.1} available={:.1} required-height={:.1} text={:?} paint-fingerprint={fingerprint:016x}",
        node.key.as_str(),
        markdown_kind_label(&spec.kind),
        spec.source.start,
        spec.source.end,
        spec.selected,
        spec.selectable,
        spec.dense,
        spec.visual_row,
        spec.total_visual_rows,
        spec.estimated_height,
        painted.len(),
        visible_lines,
        elided,
        required_width,
        available,
        required_height,
        spec.text,
    )
}

fn text_viewport_paint_audit(node: &PaintNode, spec: &TextViewportSpec) -> String {
    let geometry = text_viewport_geometry(node.rect);
    let projection = spec.projection.as_ref();
    let painted_rows = projection.map_or(0, |projection| {
        ((geometry.body_h / geometry.line_h).ceil() as usize)
            .saturating_add(1)
            .min(projection.lines.len())
    });
    let visible_rows = projection.map_or(0, |projection| {
        projection
            .lines
            .iter()
            .take(painted_rows)
            .enumerate()
            .filter(|(index, _)| {
                let top = geometry.body_y + *index as f32 * geometry.line_h;
                top < node.clip.bottom() && top + geometry.line_h > node.clip.y
            })
            .count()
    });
    let carets = projection.map_or(0, |projection| {
        projection
            .lines
            .iter()
            .take(painted_rows)
            .map(|line| line.carets.len())
            .sum()
    });
    let selections = projection.map_or(0, |projection| {
        projection
            .lines
            .iter()
            .take(painted_rows)
            .map(|line| line.selections.len())
            .sum()
    });
    let first = projection.map_or(0, |projection| projection.first_line.saturating_add(1));
    let total = projection.map_or(0, |projection| projection.total_lines);
    let cursor = spec.cursor_label.as_deref().unwrap_or("Unavailable");
    let status = spec.status.as_deref().unwrap_or("Ready");
    let minibuffer = spec.minibuffer.as_deref();
    let footer = minibuffer.unwrap_or(status);
    let modeline = format!("EDIT · {} · {cursor} | EMACS · {footer}", spec.label);
    let title = if spec.dirty {
        format!("{}  •", spec.label)
    } else {
        spec.label.clone()
    };
    let title_x = node.rect.x + 58.0;
    let mut title_right = node.rect.right();
    if let Some(cursor) = spec.cursor_label.as_ref() {
        let reserve = if spec.saving { 122.0 } else { 22.0 };
        let width = cursor.chars().count() as f32 * 6.6;
        let x = (node.rect.right() - reserve - width).max(title_x + 120.0);
        if x < node.rect.right() - 8.0 {
            title_right = title_right.min(x - 8.0);
        }
    }
    if spec.saving {
        title_right = title_right.min((node.rect.right() - 84.0).max(title_x) - 8.0);
    }
    let title_required = visual_text_width(
        &title,
        native_type_px(TypeStep::Body).get(),
        crate::widget::TextFace::UiBold,
    );
    let title_available = effective_horizontal_width(node, title_x, title_right);
    let footer_face = if minibuffer.is_some() {
        crate::widget::TextFace::Mono
    } else {
        crate::widget::TextFace::Ui
    };
    let footer_required =
        visual_text_width(footer, native_type_px(TypeStep::Caption).get(), footer_face);
    let footer_available = effective_horizontal_width(node, node.rect.x + 72.0, node.rect.right());
    let row_available = effective_horizontal_width(node, geometry.text_x, node.rect.right());
    let row_overflow = projection.map_or(0, |projection| {
        projection
            .lines
            .iter()
            .take(painted_rows)
            .filter(|line| {
                let columns = crate::native_editor::editor_display_column(
                    &line.text,
                    line.text.len(),
                    line.column_start,
                );
                columns as f32 * geometry.cell_w > row_available + 0.5
            })
            .count()
    });
    format!(
        "paint-editor key={:?} document={:?} dirty={} saving={} focused={} selectable={} first-row={} painted-rows={} visible-rows={} total-rows={} carets={} selections={} cursor={cursor:?} modeline={modeline:?} status={status:?} minibuffer-active={} minibuffer={:?} preedit={:?} title-required={title_required:.1} title-available={title_available:.1} footer-required={footer_required:.1} footer-available={footer_available:.1} row-available={row_available:.1} row-overflow={row_overflow} clip-truncated={} paint-fingerprint={:016x}",
        node.key.as_str(),
        spec.document_key,
        spec.dirty,
        spec.saving,
        spec.focused,
        spec.selectable,
        first,
        painted_rows,
        visible_rows,
        total,
        carets,
        selections,
        minibuffer.is_some(),
        minibuffer.unwrap_or("Inactive"),
        spec.preedit,
        materially_clipped(node.rect, node.clip) || visible_rows < painted_rows,
        text_viewport_fingerprint(spec),
    )
}

#[derive(Clone, Debug, PartialEq)]
struct TextVisualProjection {
    painted: String,
    required: f32,
    available: f32,
}

fn text_visual_projection(spec: &TextSpec, available: f32) -> TextVisualProjection {
    let (step, face) = text_typography(spec);
    let px = native_type_px(step).get();
    let available = available.max(0.0);
    TextVisualProjection {
        painted: elide_text_label(&spec.text, available, px, face),
        required: visual_text_width(&spec.text, px, face),
        available,
    }
}

fn text_typography(spec: &TextSpec) -> (TypeStep, crate::widget::TextFace) {
    use crate::widget::TextFace;

    match spec.role {
        SemanticRole::Heading if spec.style == StyleRef::Hero => {
            (TypeStep::Display, TextFace::UiBold)
        }
        SemanticRole::Heading if matches!(spec.style, StyleRef::Quiet | StyleRef::Accent) => {
            (TypeStep::Caption, TextFace::UiBold)
        }
        // Compact native pages use Plain for a large-type heading: Body-bold
        // remains visibly hierarchical at the platform maximum without
        // forcing route names to ellipsize in a 320-point host.
        SemanticRole::Heading if spec.style == StyleRef::Plain => {
            (TypeStep::Body, TextFace::UiBold)
        }
        SemanticRole::Heading => (TypeStep::Title, TextFace::UiBold),
        _ if spec.style == StyleRef::Code => (TypeStep::Body, TextFace::Mono),
        SemanticRole::Status if spec.style == StyleRef::Primary => (TypeStep::Body, TextFace::Ui),
        SemanticRole::Status => (TypeStep::Caption, TextFace::Ui),
        _ => (TypeStep::Body, TextFace::Ui),
    }
}

fn paint_compiled_node(
    prims: &mut Vec<crate::widget::DrawPrim>,
    node: &PaintNode,
    roles: crate::settings::Roles,
    theme: aterm_render::Theme,
    _base_px: f32,
) {
    use crate::tray_raster::row_baseline;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    let rect = node.rect;
    match &node.content {
        UiContent::Group(spec) => {
            if spec.role == SemanticRole::Application {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 0.0,
                    fill: rgba(roles.surface, 255),
                    blur: false,
                });
            } else if spec.role == SemanticRole::Navigation {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 0.0,
                    fill: rgba(roles.elevated, 245),
                    blur: false,
                });
                prims.push(DrawPrim::Stroke {
                    x: (rect.right() - 1.0).max(rect.x),
                    y: rect.y,
                    w: 1.0,
                    h: rect.height,
                    radius: 0.0,
                    width: 1.0,
                    color: rgba(roles.separator, 128),
                });
            } else if matches!(spec.style, StyleRef::Primary | StyleRef::Secondary) {
                // Settings cards are part of the canvas, not floating popovers:
                // one flat surface and one separator, with no fake drop shadow.
                let hero = spec.style == StyleRef::Primary;
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 12.0,
                    fill: rgba(
                        if hero {
                            mix_rgb(roles.elevated, roles.accent, 0.07)
                        } else {
                            roles.elevated
                        },
                        255,
                    ),
                    blur: false,
                });
                prims.push(DrawPrim::Stroke {
                    x: rect.x + 0.5,
                    y: rect.y + 0.5,
                    w: (rect.width - 1.0).max(0.0),
                    h: (rect.height - 1.0).max(0.0),
                    radius: 12.0,
                    width: 1.0,
                    color: rgba(if hero { roles.accent } else { roles.separator }, 142),
                });
            } else if spec.style == StyleRef::Code {
                // A terminal/editor canvas should read as a working surface
                // inside the surrounding card, not as another generic card.
                // Keep it theme-derived and deliberately a step quieter than
                // `surface`; the sample text supplies the visual hierarchy.
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 10.0,
                    fill: rgba(mix_rgb(roles.surface, roles.elevated, 0.12), 255),
                    blur: false,
                });
                prims.push(DrawPrim::Stroke {
                    x: rect.x + 0.5,
                    y: rect.y + 0.5,
                    w: (rect.width - 1.0).max(0.0),
                    h: (rect.height - 1.0).max(0.0),
                    radius: 10.0,
                    width: 1.0,
                    color: rgba(roles.separator, 180),
                });
            }
        }
        UiContent::Text(spec) => {
            let (step, face) = text_typography(spec);
            let color = match spec.role {
                SemanticRole::Heading if spec.style == StyleRef::Quiet => {
                    readable_secondary(&roles)
                }
                SemanticRole::Heading if spec.style == StyleRef::Accent => roles.accent,
                SemanticRole::Heading => roles.text_primary,
                _ if spec.style == StyleRef::Code => roles.text_primary,
                SemanticRole::Status if spec.style == StyleRef::Success => roles.success,
                SemanticRole::Status if spec.style == StyleRef::Primary => roles.text_primary,
                SemanticRole::Status => readable_secondary(&roles),
                _ => style_text_color(spec.style, &roles),
            };
            let size = native_type_px(step);
            let projection = text_visual_projection(spec, rect.width);
            prims.push(text_prim(
                rect.x,
                row_baseline(rect.y, rect.height, size.get()),
                projection.painted,
                size,
                TextWeight::Regular,
                face,
                rgba(color, 255),
            ));
        }
        UiContent::Button(control) => {
            let primary = control.style == StyleRef::Primary;
            let selected = control.state.selected;
            let navigation = control.style == StyleRef::Navigation;
            let quiet = control.style == StyleRef::Quiet;
            let fill = if primary {
                mix_rgb(roles.elevated, roles.accent, 0.20)
            } else if control.state.pressed {
                mix_rgb(roles.elevated, roles.accent, 0.16)
            } else if selected {
                mix_rgb(roles.elevated, roles.accent, 0.13)
            } else if control.state.hovered {
                mix_rgb(roles.elevated, roles.text_primary, 0.06)
            } else {
                roles.elevated
            };
            if !navigation || selected || control.state.hovered || control.state.pressed {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 8.0,
                    fill: rgba(fill, if quiet { 72 } else { 255 }),
                    blur: false,
                });
            }
            if primary
                || control.state.focus_visible
                || matches!(control.style, StyleRef::Secondary | StyleRef::Setting)
            {
                prims.push(DrawPrim::Stroke {
                    x: rect.x + 0.5,
                    y: rect.y + 0.5,
                    w: (rect.width - 1.0).max(0.0),
                    h: (rect.height - 1.0).max(0.0),
                    radius: 7.0,
                    width: if control.state.focus_visible {
                        2.0
                    } else {
                        1.0
                    },
                    color: rgba(
                        if primary || control.state.focus_visible {
                            roles.accent
                        } else {
                            roles.separator
                        },
                        if control.state.focus_visible {
                            255
                        } else {
                            176
                        },
                    ),
                });
            }
            if navigation && selected {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y + 7.0,
                    w: 3.0,
                    h: (rect.height - 14.0).max(0.0),
                    radius: 1.5,
                    fill: rgba(roles.accent, 255),
                    blur: false,
                });
            }
            let color = if navigation && selected {
                roles.accent
            } else {
                control_text_color(control.state, &roles)
            };
            if let Some(icon) = control.spec.visual_icon {
                if navigation && rect.width >= 96.0 {
                    // Wide navigation keeps the same renderer-native icon as
                    // the compact rail, then adds the full semantic label.
                    // The 32px icon slot is independent of the label width, so
                    // localization cannot move the pictogram.
                    let icon_rect = LogicalRect::new(rect.x + 6.0, rect.y, 28.0, rect.height);
                    paint_button_icon(prims, icon_rect, icon, color);
                    let size = native_type_px(TypeStep::Secondary);
                    let visual_label = control
                        .spec
                        .visual_label
                        .as_ref()
                        .unwrap_or(&control.spec.label);
                    let label =
                        elide_ui_label(visual_label, (rect.width - 52.0).max(0.0), size.get());
                    prims.push(text_prim(
                        rect.x + 40.0,
                        row_baseline(rect.y, rect.height, size.get()),
                        label,
                        size,
                        TextWeight::Regular,
                        TextFace::Ui,
                        rgba(color, 255),
                    ));
                } else {
                    paint_button_icon(prims, rect, icon, color);
                }
            } else {
                let size = native_type_px(TypeStep::Secondary);
                let label = control
                    .spec
                    .visual_label
                    .as_ref()
                    .unwrap_or(&control.spec.label);
                let text_x = if navigation && control.spec.visual_label.is_some() {
                    let width = crate::tray_raster::ui_text_width(label, size.get());
                    rect.x + ((rect.width - width) / 2.0).max(0.0)
                } else {
                    rect.x + if navigation { 12.0 } else { 10.0 }
                };
                let trailing_reserve = if control.spec.trailing_icon.is_some() {
                    38.0
                } else {
                    10.0
                };
                let painted_label = elide_ui_label(
                    label,
                    (rect.right() - trailing_reserve - text_x).max(0.0),
                    size.get(),
                );
                prims.push(text_prim(
                    text_x,
                    row_baseline(rect.y, rect.height, size.get()),
                    painted_label,
                    size,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(color, 255),
                ));
                if let Some(icon) = control.spec.trailing_icon {
                    let icon_rect = LogicalRect::new(
                        (rect.right() - 34.0).max(rect.x),
                        rect.y,
                        28.0_f32.min(rect.width),
                        rect.height,
                    );
                    paint_button_icon(prims, icon_rect, icon, color);
                }
            }
        }
        UiContent::Switch(control) => {
            paint_control_surface(prims, rect, control.state, roles);
            let on = matches!(control.value, SemanticValue::Bool(true));
            let track_w = 42.0_f32.min((rect.width - 8.0).max(0.0));
            let track_h = 22.0_f32.min((rect.height - 8.0).max(0.0));
            prims.push(DrawPrim::Capsule {
                x: rect.right() - track_w - 8.0,
                y: rect.y + (rect.height - track_h) / 2.0,
                w: track_w,
                h: track_h,
                frac: if on { 1.0 } else { 0.0 },
                fill: rgba(roles.accent, 255),
                track: rgba(roles.control_track, 180),
            });
            // A moving, high-contrast thumb is the primary state cue; color and
            // the adjacent On/Off text are redundant cues. This reads as a
            // switch in grayscale and at a glance, unlike the old fill-only bar.
            prims.push(DrawPrim::Dot {
                cx: if on {
                    rect.right() - 8.0 - track_h / 2.0
                } else {
                    rect.right() - 8.0 - track_w + track_h / 2.0
                },
                cy: rect.y + rect.height / 2.0,
                r: (track_h / 2.0 - 3.0).max(2.0),
                color: rgba(
                    if on {
                        roles.on_accent
                    } else {
                        roles.text_secondary
                    },
                    255,
                ),
                breathe: false,
            });
            let size = native_type_px(TypeStep::Secondary);
            prims.push(text_prim(
                rect.x + 10.0,
                row_baseline(rect.y, rect.height, size.get()),
                if on { "On" } else { "Off" }.to_string(),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(control_text_color(control.state, &roles), 255),
            ));
        }
        UiContent::Slider(control) => {
            paint_control_surface(prims, rect, control.state, roles);
            let fraction = match control.value {
                SemanticValue::Number {
                    value,
                    minimum,
                    maximum,
                } if maximum > minimum => {
                    ((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0) as f32
                }
                _ => 0.0,
            };
            let geometry = slider_geometry(rect);
            prims.push(DrawPrim::Capsule {
                x: geometry.track_x,
                y: rect.y + rect.height / 2.0 - 3.0,
                w: (geometry.track_right - geometry.track_x).max(0.0),
                h: 6.0,
                frac: fraction,
                fill: rgba(roles.accent, 255),
                track: rgba(roles.control_track, 180),
            });
            let size = native_type_px(TypeStep::Caption);
            let width = crate::tray_raster::ui_text_width(&control.spec.display_value, size.get());
            prims.push(text_prim(
                (geometry.value_right - width).max(geometry.track_right + 6.0),
                row_baseline(rect.y, rect.height, size.get()),
                control.spec.display_value.clone(),
                size,
                TextWeight::Regular,
                TextFace::Mono,
                rgba(control_text_color(control.state, &roles), 255),
            ));
        }
        UiContent::TextField(control) => {
            paint_control_surface(prims, rect, control.state, roles);
            let size = native_type_px(TypeStep::Secondary);
            let geometry = text_field_geometry(control, rect, size.get());
            let actual = text_field_visual_text(control);
            let (value, quiet) = if actual.is_empty() {
                (
                    control.spec.placeholder.as_deref().unwrap_or_default(),
                    true,
                )
            } else {
                (
                    &actual[geometry.visible.start.min(actual.len())
                        ..geometry.visible.end.min(actual.len())],
                    false,
                )
            };
            prims.push(DrawPrim::ClipPush {
                x: geometry.text_x,
                y: rect.y + 2.0,
                w: (geometry.text_right - geometry.text_x).max(0.0),
                h: (rect.height - 4.0).max(0.0),
            });
            if control.state.focused
                && let Some(input) = control.spec.input.as_ref()
                && !input.text.is_empty()
            {
                let selected = input.selection.range();
                let start = selected.start.max(geometry.visible.start);
                let end = selected.end.min(geometry.visible.end);
                if start < end {
                    let x0 = text_field_x_for_byte(&input.text, &geometry, start, size.get());
                    let x1 = text_field_x_for_byte(&input.text, &geometry, end, size.get());
                    prims.push(DrawPrim::Panel {
                        x: x0,
                        y: rect.y + 6.0,
                        w: (x1 - x0).max(1.0),
                        h: (rect.height - 12.0).max(1.0),
                        radius: 3.0,
                        fill: rgba(roles.accent, 82),
                        blur: false,
                    });
                }
            }
            prims.push(text_prim(
                geometry.text_x,
                row_baseline(rect.y, rect.height, size.get()),
                value.to_string(),
                size,
                TextWeight::Regular,
                if control.style == StyleRef::Code {
                    TextFace::Mono
                } else {
                    TextFace::Ui
                },
                rgba(
                    if !control.state.enabled {
                        roles.text_tertiary
                    } else if quiet {
                        quiet_text(&roles)
                    } else {
                        control_text_color(control.state, &roles)
                    },
                    255,
                ),
            ));
            if control.state.focused
                && let Some(input) = control.spec.input.as_ref()
            {
                if let Some(marked) = input.preedit.as_ref() {
                    let start = marked.start.max(geometry.visible.start);
                    let end = marked.end.min(geometry.visible.end);
                    if start < end {
                        let x0 = text_field_x_for_byte(&input.text, &geometry, start, size.get());
                        let x1 = text_field_x_for_byte(&input.text, &geometry, end, size.get());
                        prims.push(DrawPrim::Stroke {
                            x: x0,
                            y: rect.bottom() - 6.0,
                            w: (x1 - x0).max(1.0),
                            h: 1.0,
                            radius: 0.0,
                            width: 1.0,
                            color: rgba(roles.accent, 255),
                        });
                    }
                }
                let caret = input
                    .selection
                    .head
                    .clamp(geometry.visible.start, geometry.visible.end);
                let caret_x = if input.text.is_empty() {
                    geometry.text_x
                } else {
                    text_field_x_for_byte(&input.text, &geometry, caret, size.get())
                };
                prims.push(DrawPrim::Stroke {
                    x: caret_x,
                    y: rect.y + 7.0,
                    w: 1.0,
                    h: (rect.height - 14.0).max(1.0),
                    radius: 0.0,
                    width: 1.0,
                    color: rgba(roles.accent, 255),
                });
            }
            prims.push(DrawPrim::ClipPop);
            if let Some(color) = control.spec.swatch {
                let x = rect.right() - TEXT_FIELD_HORIZONTAL_PADDING - TEXT_FIELD_SWATCH_SIZE;
                let y = rect.y + (rect.height - TEXT_FIELD_SWATCH_SIZE) / 2.0;
                prims.push(DrawPrim::Panel {
                    x,
                    y,
                    w: TEXT_FIELD_SWATCH_SIZE,
                    h: TEXT_FIELD_SWATCH_SIZE,
                    radius: 6.0,
                    fill: rgba(color, 255),
                    blur: false,
                });
                prims.push(DrawPrim::Stroke {
                    x: x + 0.5,
                    y: y + 0.5,
                    w: TEXT_FIELD_SWATCH_SIZE - 1.0,
                    h: TEXT_FIELD_SWATCH_SIZE - 1.0,
                    radius: 5.5,
                    width: 1.0,
                    color: rgba(roles.separator, 220),
                });
            }
        }
        UiContent::RichText(spec) => {
            let size = native_type_px(TypeStep::Body);
            let line_h = (size.get() * 1.45).max(16.0);
            for (line_index, line) in spec.text.lines().enumerate() {
                let y = rect.y + line_index as f32 * line_h;
                if y >= rect.bottom() {
                    break;
                }
                prims.push(text_prim(
                    rect.x,
                    row_baseline(y, line_h, size.get()),
                    line.to_string(),
                    size,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(roles.text_primary, 255),
                ));
            }
        }
        UiContent::MarkdownBlock(spec) => {
            paint_markdown_block(prims, rect, spec, roles);
        }
        UiContent::TextViewport(spec) => {
            paint_text_viewport(prims, rect, spec, roles);
        }
        UiContent::SettingsPreview(spec) => {
            spec.paint(prims, rect, theme, roles);
        }
        UiContent::Custom(spec) => {
            // The Tab Color wheel is the ONE custom node with a raster lowering:
            // the shared HSV disk primitive plus a two-tone marker ring at the
            // committed color's polar position (spec.value = "#rrggbb"). Every
            // other custom node keeps the plain label projection below.
            if spec.audit_id == TAB_COLOR_WHEEL_AUDIT {
                let geometry = color_wheel_geometry(rect);
                prims.push(crate::widget::DrawPrim::HsvDisk {
                    cx: geometry.cx,
                    cy: geometry.cy,
                    r: geometry.r,
                    value: 1.0,
                });
                if let Some(rgb) = spec
                    .value
                    .as_deref()
                    .and_then(crate::app_config::parse_hex_color)
                {
                    let rgb = [rgb.r, rgb.g, rgb.b];
                    let (mx, my) = color_wheel_marker_at(rect, rgb);
                    // Contrast ring first, then the committed color's dot — the
                    // marker reads on every hue at every saturation.
                    let ring = if crate::tab_bar::bg_is_light(rgb) {
                        [0u8, 0, 0]
                    } else {
                        [255u8, 255, 255]
                    };
                    prims.push(crate::widget::DrawPrim::Dot {
                        cx: mx,
                        cy: my,
                        r: 7.0,
                        color: rgba(ring, 255),
                        breathe: false,
                    });
                    prims.push(crate::widget::DrawPrim::Dot {
                        cx: mx,
                        cy: my,
                        r: 5.0,
                        color: rgba(rgb, 255),
                        breathe: false,
                    });
                }
                return;
            }
            let size = native_type_px(TypeStep::Body);
            prims.push(text_prim(
                rect.x,
                row_baseline(rect.y, rect.height, size.get()),
                spec.label.clone(),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(roles.text_primary, 255),
            ));
        }
    }
}

/// Bounded, UTF-8-safe visual projection for renderer-native button labels.
/// Semantics retain the complete authored label; paint receives an ellipsis
/// that fits the exact UI-face width budget so adjacent picker cells and
/// trailing affordances can never overdraw one another.
fn elide_ui_label(value: &str, max_width: f32, px: f32) -> String {
    elide_text_label(value, max_width, px, crate::widget::TextFace::Ui)
}

/// Project a complete semantic button label into a bounded renderer label.
///
/// Callers keep the original text in [`ButtonSpec::label`] and put this value
/// in `visual_label`. This is intentionally the same typography and UTF-8-safe
/// elision path used by the painter, which lets responsive authoring avoid a
/// knowingly overflowing intermediate label while preserving full semantics.
pub(crate) fn fit_native_button_label(value: &str, max_width: f32) -> String {
    elide_ui_label(value, max_width, native_type_px(TypeStep::Secondary).get())
}

/// Bounded visual projection for a one-line native body label. The caller's
/// semantic wrapper remains responsible for the complete authored copy.
pub(crate) fn fit_native_text_label(value: &str, max_width: f32) -> String {
    elide_ui_label(value, max_width, native_type_px(TypeStep::Body).get())
}

/// Bounded visual projection for one-line native status copy. The owning
/// semantic Group remains responsible for the complete message.
pub(crate) fn fit_native_status_label(value: &str, max_width: f32) -> String {
    elide_text_label(
        value,
        max_width,
        native_type_px(TypeStep::Caption).get(),
        crate::widget::TextFace::Ui,
    )
}

fn visual_text_width(value: &str, px: f32, face: crate::widget::TextFace) -> f32 {
    if face == crate::widget::TextFace::Mono {
        crate::tray_raster::measure_text(value, px, crate::widget::TextWeight::Regular)
    } else {
        crate::tray_raster::ui_text_width_for(face, value, px)
    }
}

fn elide_text_label(value: &str, max_width: f32, px: f32, face: crate::widget::TextFace) -> String {
    const MAX_VISUAL_GRAPHEMES: usize = 256;

    if max_width <= 0.0 {
        return String::new();
    }
    let mut capped_end = value.len();
    let mut elided = false;
    for (index, (offset, _)) in value.grapheme_indices().enumerate() {
        if index == MAX_VISUAL_GRAPHEMES {
            capped_end = offset;
            elided = true;
            break;
        }
    }
    let mut output = value[..capped_end].to_string();
    if !elided && visual_text_width(&output, px, face) <= max_width {
        return output;
    }
    let ellipsis = "…";
    let ellipsis_width = visual_text_width(ellipsis, px, face);
    while !output.is_empty() && visual_text_width(&output, px, face) + ellipsis_width > max_width {
        let Some((offset, _)) = output.grapheme_indices().last() else {
            output.clear();
            break;
        };
        output.truncate(offset);
        elided = true;
    }
    if elided && ellipsis_width <= max_width {
        output.push('…');
    }
    output
}

/// Paint small navigation pictograms entirely from the shared draw IR. These
/// shapes deliberately use no font glyphs, so the medium Settings rail remains
/// stable under custom/fallback UI fonts on every backend.
fn paint_button_icon(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    icon: ButtonIcon,
    color: [u8; 3],
) {
    use crate::widget::{DrawPrim, rgba};

    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let line = |x: f32, y: f32, w: f32, h: f32| DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius: 0.0,
        width: 1.5,
        color: rgba(color, 235),
    };
    let outline = |x: f32, y: f32, w: f32, h: f32, radius: f32| DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius,
        width: 1.5,
        color: rgba(color, 235),
    };
    let block = |x: f32, y: f32, w: f32, h: f32, radius: f32| DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius,
        fill: rgba(color, 220),
        blur: false,
    };
    let segment = |x1: f32, y1: f32, x2: f32, y2: f32| DrawPrim::Line {
        x1,
        y1,
        x2,
        y2,
        width: 1.75,
        color: rgba(color, 235),
    };
    match icon {
        ButtonIcon::Back => {
            prims.push(segment(cx + 7.0, cy, cx - 7.0, cy));
            prims.push(segment(cx - 7.0, cy, cx - 1.0, cy - 6.0));
            prims.push(segment(cx - 7.0, cy, cx - 1.0, cy + 6.0));
        }
        ButtonIcon::Forward => {
            prims.push(segment(cx - 7.0, cy, cx + 7.0, cy));
            prims.push(segment(cx + 7.0, cy, cx + 1.0, cy - 6.0));
            prims.push(segment(cx + 7.0, cy, cx + 1.0, cy + 6.0));
        }
        ButtonIcon::Copy => {
            prims.push(outline(cx - 8.0, cy - 8.0, 11.0, 12.0, 2.0));
            prims.push(outline(cx - 3.0, cy - 3.0, 11.0, 12.0, 2.0));
        }
        ButtonIcon::External => {
            prims.push(outline(cx - 8.0, cy - 5.0, 13.0, 13.0, 2.0));
            prims.push(line(cx, cy - 8.0, 8.0, 1.0));
            prims.push(line(cx + 7.0, cy - 8.0, 1.0, 8.0));
            prims.push(line(cx - 1.0, cy - 1.0, 8.0, 1.0));
        }
        ButtonIcon::Anchor => {
            prims.push(outline(cx - 3.0, cy - 8.0, 6.0, 6.0, 3.0));
            prims.push(line(cx, cy - 2.0, 1.0, 10.0));
            prims.push(line(cx - 7.0, cy + 2.0, 1.0, 5.0));
            prims.push(line(cx + 7.0, cy + 2.0, 1.0, 5.0));
            prims.push(line(cx - 7.0, cy + 7.0, 14.0, 1.0));
        }
        ButtonIcon::ChevronDown => {
            // A compact filled disclosure triangle. The former disconnected
            // one-pixel strokes read as an ellipsis after device scaling; three
            // touching rows retain an unmistakable down affordance at 1×/2×.
            prims.push(block(cx - 5.0, cy - 3.0, 10.0, 2.0, 0.0));
            prims.push(block(cx - 3.0, cy - 1.0, 6.0, 2.0, 0.0));
            prims.push(block(cx - 1.0, cy + 1.0, 2.0, 2.0, 0.0));
        }
        ButtonIcon::Home => {
            prims.push(outline(cx - 7.0, cy - 4.0, 14.0, 11.0, 2.0));
            prims.push(line(cx - 4.0, cy - 7.0, 8.0, 1.0));
            prims.push(block(cx - 1.5, cy + 1.0, 3.0, 6.0, 1.0));
        }
        ButtonIcon::Modified => {
            prims.push(line(cx - 7.0, cy, 14.0, 1.0));
            prims.push(line(cx, cy - 7.0, 1.0, 14.0));
            prims.push(DrawPrim::Dot {
                cx,
                cy,
                r: 2.0,
                color: rgba(color, 240),
                breathe: false,
            });
        }
        ButtonIcon::Appearance => prims.push(DrawPrim::Ring {
            cx,
            cy,
            r_outer: 8.0,
            thickness: 3.0,
            track: rgba(color, 70),
            sys_frac: 0.5,
            sys_color: rgba(color, 240),
            tab_frac: None,
            tab_color: rgba(color, 0),
            dashed_tab: false,
        }),
        ButtonIcon::Text => {
            prims.push(line(cx - 7.0, cy - 6.0, 14.0, 1.0));
            prims.push(line(cx - 5.0, cy, 10.0, 1.0));
            prims.push(line(cx - 3.0, cy + 6.0, 6.0, 1.0));
        }
        ButtonIcon::Cursor => {
            prims.push(line(cx - 8.0, cy, 16.0, 1.0));
            prims.push(line(cx, cy - 8.0, 1.0, 16.0));
            prims.push(outline(cx - 3.0, cy - 3.0, 6.0, 6.0, 3.0));
        }
        ButtonIcon::Window => {
            prims.push(outline(cx - 8.0, cy - 7.0, 16.0, 14.0, 2.0));
            prims.push(line(cx - 8.0, cy - 3.0, 16.0, 1.0));
            prims.push(line(cx - 2.0, cy - 3.0, 1.0, 10.0));
        }
        ButtonIcon::Keyboard => {
            prims.push(outline(cx - 9.0, cy - 6.0, 18.0, 12.0, 2.5));
            for offset in [-5.0_f32, 0.0, 5.0] {
                prims.push(DrawPrim::Dot {
                    cx: cx + offset,
                    cy: cy - 2.0,
                    r: 1.1,
                    color: rgba(color, 230),
                    breathe: false,
                });
            }
            prims.push(line(cx - 5.0, cy + 3.0, 10.0, 1.0));
        }
        ButtonIcon::Terminal => {
            prims.push(outline(cx - 9.0, cy - 7.0, 18.0, 14.0, 2.0));
            prims.push(line(cx - 5.0, cy - 2.0, 4.0, 1.0));
            prims.push(line(cx + 1.0, cy + 3.0, 5.0, 1.0));
        }
        ButtonIcon::Performance => {
            prims.push(block(cx - 7.0, cy + 1.0, 3.0, 6.0, 1.0));
            prims.push(block(cx - 1.5, cy - 3.0, 3.0, 10.0, 1.0));
            prims.push(block(cx + 4.0, cy - 7.0, 3.0, 14.0, 1.0));
        }
        ButtonIcon::Security => {
            prims.push(outline(cx - 7.0, cy - 1.0, 14.0, 9.0, 2.0));
            prims.push(outline(cx - 4.5, cy - 7.0, 9.0, 9.0, 4.0));
            prims.push(block(cx - 1.0, cy + 2.0, 2.0, 3.0, 1.0));
        }
        ButtonIcon::Diagnostics => {
            for offset in [-6.0_f32, 0.0, 6.0] {
                prims.push(DrawPrim::Dot {
                    cx: cx + offset,
                    cy,
                    r: 2.0,
                    color: rgba(color, 230),
                    breathe: false,
                });
            }
        }
        ButtonIcon::Update => prims.push(DrawPrim::Ring {
            cx,
            cy,
            r_outer: 8.0,
            thickness: 2.0,
            track: rgba(color, 55),
            sys_frac: 0.78,
            sys_color: rgba(color, 240),
            tab_frac: None,
            tab_color: rgba(color, 0),
            dashed_tab: false,
        }),
        ButtonIcon::Packages => {
            // A parcel: box outline, middle tape band, and a short lid seam.
            prims.push(outline(cx - 8.0, cy - 7.0, 16.0, 14.0, 2.0));
            prims.push(line(cx - 8.0, cy, 16.0, 1.0));
            prims.push(line(cx, cy - 7.0, 1.0, 4.0));
        }
        ButtonIcon::Info => {
            prims.push(outline(cx - 8.0, cy - 8.0, 16.0, 16.0, 8.0));
            prims.push(DrawPrim::Dot {
                cx,
                cy: cy - 4.0,
                r: 1.2,
                color: rgba(color, 240),
                breathe: false,
            });
            prims.push(line(cx, cy, 1.0, 6.0));
        }
    }
}

fn paint_markdown_block(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    spec: &MarkdownBlockSpec,
    roles: crate::settings::Roles,
) {
    use crate::tray_raster::row_baseline;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};
    use MarkdownBlockKind::{Code, Heading, ListItem, Paragraph, Quote, Rule, Table};

    if spec.selected && !matches!(&spec.kind, Rule) {
        prims.push(DrawPrim::Panel {
            x: rect.x - 6.0,
            y: rect.y,
            w: rect.width + 12.0,
            h: rect.height,
            radius: 7.0,
            fill: rgba(mix_rgb(roles.surface, roles.accent, 0.18), 210),
            blur: false,
        });
    }

    if matches!(&spec.kind, Rule) {
        prims.push(DrawPrim::Stroke {
            x: rect.x,
            y: rect.y + rect.height.min(24.0) / 2.0,
            w: rect.width,
            h: 1.0,
            radius: 0.5,
            width: 1.0,
            color: rgba(roles.separator, 190),
        });
        return;
    }

    let (mut text_x, mut text_y, mut text_w) = (rect.x, rect.y, rect.width);
    let (step, face, weight, color, line_multiplier, preserve_lines) = match &spec.kind {
        Heading(1) => (
            TypeStep::Display,
            TextFace::UiBold,
            TextWeight::Regular,
            roles.text_primary,
            1.32,
            false,
        ),
        Heading(2) => (
            TypeStep::Title,
            TextFace::UiBold,
            TextWeight::Regular,
            roles.text_primary,
            1.35,
            false,
        ),
        Heading(_) => (
            TypeStep::Body,
            TextFace::UiBold,
            TextWeight::Regular,
            roles.text_primary,
            1.45,
            false,
        ),
        Paragraph => (
            TypeStep::Body,
            TextFace::Ui,
            TextWeight::Regular,
            roles.text_primary,
            1.72,
            false,
        ),
        ListItem { depth, .. } => {
            let indent = (*depth as f32 * 16.0).min(64.0);
            text_x += 28.0 + indent;
            text_w = (text_w - 28.0 - indent).max(20.0);
            (
                TypeStep::Body,
                TextFace::Ui,
                TextWeight::Regular,
                roles.text_primary,
                1.66,
                false,
            )
        }
        Quote => {
            prims.push(DrawPrim::Panel {
                x: rect.x,
                y: rect.y,
                w: rect.width,
                h: rect.height,
                radius: 8.0,
                fill: rgba(mix_rgb(roles.elevated, roles.accent, 0.06), 225),
                blur: false,
            });
            prims.push(DrawPrim::Panel {
                x: rect.x,
                y: rect.y + 2.0,
                w: 3.0,
                h: (rect.height - 4.0).max(0.0),
                radius: 1.5,
                fill: rgba(roles.accent, 220),
                blur: false,
            });
            text_x += 18.0;
            text_y += 8.0;
            text_w = (text_w - 30.0).max(20.0);
            (
                TypeStep::Body,
                TextFace::Ui,
                TextWeight::Regular,
                mix_rgb(roles.text_secondary, roles.text_primary, 0.55),
                1.66,
                false,
            )
        }
        Code { language } => {
            prims.push(DrawPrim::Panel {
                x: rect.x,
                y: rect.y,
                w: rect.width,
                h: rect.height,
                radius: 9.0,
                fill: rgba(mix_rgb(roles.elevated, roles.surface, 0.20), 255),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x: rect.x + 0.5,
                y: rect.y + 0.5,
                w: (rect.width - 1.0).max(0.0),
                h: (rect.height - 1.0).max(0.0),
                radius: 9.0,
                width: 1.0,
                color: rgba(roles.separator, 150),
            });
            if let Some(language) = language.as_deref().filter(|value| !value.is_empty()) {
                let caption = native_type_px(TypeStep::Caption);
                prims.push(text_prim(
                    rect.x + 12.0,
                    row_baseline(rect.y + 5.0, 16.0, caption.get()),
                    language.to_ascii_uppercase(),
                    caption,
                    TextWeight::Regular,
                    TextFace::UiBold,
                    rgba(readable_secondary(&roles), 235),
                ));
            }
            text_x += 12.0;
            text_y += if language.is_some() { 24.0 } else { 10.0 };
            text_w = (text_w - 24.0).max(20.0);
            (
                if spec.dense {
                    TypeStep::Caption
                } else {
                    TypeStep::Body
                },
                TextFace::Mono,
                TextWeight::Regular,
                roles.text_primary,
                1.55,
                true,
            )
        }
        Table => {
            prims.push(DrawPrim::Panel {
                x: rect.x,
                y: rect.y,
                w: rect.width,
                h: rect.height,
                radius: 8.0,
                fill: rgba(roles.elevated, 245),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x: rect.x + 0.5,
                y: rect.y + 0.5,
                w: (rect.width - 1.0).max(0.0),
                h: (rect.height - 1.0).max(0.0),
                radius: 8.0,
                width: 1.0,
                color: rgba(roles.separator, 150),
            });
            text_x += 12.0;
            text_y += 8.0;
            text_w = (text_w - 24.0).max(20.0);
            (
                TypeStep::Body,
                TextFace::Mono,
                TextWeight::Regular,
                roles.text_primary,
                1.65,
                true,
            )
        }
        Rule => unreachable!("rule returned above"),
    };

    let size = native_type_px(step);
    let line_h = (size.get() * line_multiplier).max(16.0);
    let max_lines = ((rect.bottom() - text_y).max(0.0) / line_h).ceil().max(1.0) as usize;
    let average_advance = if face == TextFace::Mono {
        size.get() * 0.62
    } else {
        size.get() * 0.55
    };
    let max_columns = (text_w / average_advance.max(1.0)).floor().max(4.0) as usize;
    let lines = wrap_markdown_text_window(
        &spec.text,
        max_columns,
        spec.visual_row,
        max_lines,
        preserve_lines,
    );

    if let ListItem { ordinal, .. } = &spec.kind {
        let marker = ordinal.map_or_else(|| "•".to_string(), |value| format!("{value}."));
        prims.push(text_prim(
            text_x - 20.0,
            row_baseline(text_y, line_h, size.get()),
            marker,
            size,
            TextWeight::Regular,
            TextFace::UiBold,
            rgba(roles.accent, 255),
        ));
    }
    for (line_index, line) in lines.into_iter().enumerate() {
        let y = text_y + line_index as f32 * line_h;
        if y >= rect.bottom() {
            break;
        }
        prims.push(text_prim(
            text_x,
            row_baseline(y, line_h, size.get()),
            line,
            size,
            if matches!(&spec.kind, Table) && line_index == 0 {
                TextWeight::Bold
            } else {
                weight
            },
            face,
            rgba(color, 255),
        ));
    }
}

/// Bounded, allocation-conscious wrapping for visible reader blocks. The app
/// already caps source copied into a block; this cap also bounds paint prims.
fn wrap_markdown_text(
    text: &str,
    max_columns: usize,
    max_lines: usize,
    preserve_lines: bool,
) -> Vec<String> {
    let max_columns = max_columns.max(1);
    // Upstream caps block text at 128 KiB. Even the adversarial one-byte-line
    // case is therefore bounded while still allowing a tail viewport far past
    // the former 512-row ceiling.
    let max_lines = max_lines.clamp(1, 131_072);
    let mut output = Vec::with_capacity(max_lines.min(16));
    for source_line in text.lines().chain(text.is_empty().then_some("")) {
        if output.len() >= max_lines {
            break;
        }
        if preserve_lines {
            let characters = source_line.chars().collect::<Vec<_>>();
            if characters.is_empty() {
                output.push(String::new());
            } else {
                let mut start = 0usize;
                while start < characters.len() {
                    if output.len() >= max_lines {
                        break;
                    }
                    let hard_end = (start + max_columns).min(characters.len());
                    if hard_end == characters.len() {
                        output.push(characters[start..hard_end].iter().collect());
                        break;
                    }
                    // Preserve authored line boundaries and interior spacing,
                    // but prefer the last word boundary that fits. Exact
                    // source remains in semantics; paint no longer strands a
                    // single letter at the edge of a split Markdown pane.
                    let soft_end = characters[start..=hard_end]
                        .iter()
                        .rposition(|character| character.is_whitespace())
                        .map(|offset| start + offset)
                        .filter(|end| *end > start);
                    let end = soft_end.unwrap_or(hard_end);
                    output.push(characters[start..end].iter().collect());
                    start = end;
                    while start < characters.len() && characters[start].is_whitespace() {
                        start += 1;
                    }
                }
            }
            continue;
        }
        let mut line = String::new();
        let mut line_columns = 0usize;
        for word in source_line.split_whitespace() {
            let separator = usize::from(!line.is_empty());
            let word_columns = word.chars().count();
            if line_columns + separator + word_columns <= max_columns {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
                line_columns += separator + word_columns;
                continue;
            }
            if !line.is_empty() {
                output.push(std::mem::take(&mut line));
                if output.len() >= max_lines {
                    break;
                }
            }
            let mut chunk = String::new();
            let mut chunk_columns = 0usize;
            for character in word.chars() {
                chunk.push(character);
                chunk_columns += 1;
                if chunk_columns == max_columns {
                    output.push(std::mem::take(&mut chunk));
                    chunk_columns = 0;
                    if output.len() >= max_lines {
                        break;
                    }
                }
            }
            line = chunk;
            line_columns = chunk_columns;
        }
        if output.len() >= max_lines {
            break;
        }
        if !line.is_empty() || source_line.is_empty() {
            output.push(line);
        }
    }
    if output.len() == max_lines
        && text.chars().count()
            > output
                .iter()
                .map(|line| line.chars().count())
                .sum::<usize>()
        && let Some(last) = output.last_mut()
    {
        let keep = max_columns.saturating_sub(1);
        *last = last.chars().take(keep).collect::<String>();
        last.push('…');
    }
    output
}

fn wrap_markdown_text_window(
    text: &str,
    max_columns: usize,
    visual_row: usize,
    max_lines: usize,
    preserve_lines: bool,
) -> Vec<String> {
    let take = max_lines.clamp(1, 512);
    let requested = visual_row.saturating_add(take).clamp(1, 131_072);
    let mut lines = wrap_markdown_text(text, max_columns, requested, preserve_lines);
    if visual_row >= lines.len() {
        return Vec::new();
    }
    lines.drain(..visual_row);
    lines.truncate(take);
    lines
}

/// Paint the editor from the same bounded, source-addressable projection used by
/// semantics and pointer mapping. Nothing here walks the full document: even a
/// multi-gigabyte buffer costs at most the visible line count prepared by the
/// controller.
fn paint_text_viewport(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    spec: &TextViewportSpec,
    roles: crate::settings::Roles,
) {
    use crate::tray_raster::row_baseline;
    use crate::type_scale::TypeStep;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height,
        radius: 10.0,
        fill: rgba(roles.surface, 255),
        blur: false,
    });
    if rect.width <= 1.0 || rect.height <= 1.0 {
        return;
    }

    let geometry = text_viewport_geometry(rect);
    let header_h = geometry.header_h;
    let footer_h = geometry.footer_h;
    let body_y = geometry.body_y;
    let body_h = geometry.body_h;
    let footer_y = body_y + body_h;
    let gutter_w = geometry.gutter_w;
    let text_x = geometry.text_x;
    let line_h = geometry.line_h;
    let cell_w = geometry.cell_w;

    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: header_h,
        radius: 10.0,
        fill: rgba(roles.elevated, 255),
        blur: false,
    });
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: body_y,
        w: gutter_w,
        h: body_h,
        radius: 0.0,
        fill: rgba(mix_rgb(roles.surface, roles.elevated, 0.52), 255),
        blur: false,
    });
    for y in [body_y, footer_y] {
        prims.push(DrawPrim::Stroke {
            x: rect.x,
            y,
            w: rect.width,
            h: 1.0,
            radius: 0.0,
            width: 1.0,
            color: rgba(roles.separator, 150),
        });
    }
    prims.push(DrawPrim::Stroke {
        x: rect.x + gutter_w,
        y: body_y,
        w: 1.0,
        h: body_h,
        radius: 0.0,
        width: 1.0,
        color: rgba(roles.separator, 110),
    });

    let caption = native_type_px(TypeStep::Caption);
    let body = native_type_px(TypeStep::Body);
    let secondary = native_type_px(TypeStep::Secondary);
    let header_baseline = row_baseline(rect.y, header_h, body.get());
    prims.push(text_prim(
        rect.x + 14.0,
        header_baseline,
        "EDIT".to_string(),
        caption,
        TextWeight::Bold,
        TextFace::UiBold,
        rgba(roles.accent, 255),
    ));
    let title_x = rect.x + 58.0;
    let title = if spec.dirty {
        format!("{}  •", spec.label)
    } else {
        spec.label.clone()
    };
    prims.push(text_prim(
        title_x,
        header_baseline,
        title,
        body,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(roles.text_primary, 255),
    ));
    if let Some(cursor) = spec.cursor_label.as_ref() {
        let reserve = if spec.saving { 122.0 } else { 22.0 };
        let width = cursor.chars().count() as f32 * 6.6;
        let x = (rect.right() - reserve - width).max(title_x + 120.0);
        if x < rect.right() - 8.0 {
            prims.push(text_prim(
                x,
                row_baseline(rect.y, header_h, caption.get()),
                cursor.clone(),
                caption,
                TextWeight::Regular,
                TextFace::Mono,
                rgba(readable_secondary(&roles), 255),
            ));
        }
    }
    if spec.saving {
        prims.push(text_prim(
            (rect.right() - 84.0).max(title_x),
            row_baseline(rect.y, header_h, caption.get()),
            "Saving…".to_string(),
            caption,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(roles.accent, 255),
        ));
    }

    prims.push(DrawPrim::ClipPush {
        x: rect.x,
        y: body_y,
        w: rect.width,
        h: body_h,
    });
    if let Some(projection) = spec.projection.as_ref() {
        let visible = ((body_h / line_h).ceil() as usize)
            .saturating_add(1)
            .min(projection.lines.len());
        for (row, line) in projection.lines.iter().take(visible).enumerate() {
            let y = body_y + row as f32 * line_h;
            if line.carets.iter().any(|(_, primary)| *primary) {
                prims.push(DrawPrim::Panel {
                    x: rect.x + gutter_w + 1.0,
                    y,
                    w: (rect.width - gutter_w - 1.0).max(0.0),
                    h: line_h,
                    radius: 0.0,
                    fill: rgba(roles.accent, 18),
                    blur: false,
                });
            }
            for selection in &line.selections {
                let visual_start = grapheme_boundary_at_or_before(
                    &line.text,
                    selection.bytes.start.min(line.text.len()),
                );
                let visual_end = grapheme_boundary_at_or_after(
                    &line.text,
                    selection.bytes.end.min(line.text.len()),
                )
                .max(visual_start);
                let start_col = crate::native_editor::editor_display_column(
                    &line.text,
                    visual_start,
                    line.column_start,
                );
                let end_col = crate::native_editor::editor_display_column(
                    &line.text,
                    visual_end,
                    line.column_start,
                );
                let cells = end_col
                    .saturating_sub(start_col)
                    .saturating_add(usize::from(selection.continues))
                    .max(1);
                prims.push(DrawPrim::Panel {
                    x: text_x + start_col as f32 * cell_w,
                    y: y + 1.0,
                    w: cells as f32 * cell_w,
                    h: line_h - 2.0,
                    radius: 2.0,
                    fill: rgba(roles.accent, if selection.primary { 86 } else { 54 }),
                    blur: false,
                });
            }

            let line_number = (line.number + 1).to_string();
            let number_w = line_number.len() as f32 * cell_w;
            prims.push(text_prim(
                (rect.x + gutter_w - 10.0 - number_w).max(rect.x + 4.0),
                row_baseline(y, line_h, caption.get()),
                line_number,
                caption,
                TextWeight::Regular,
                TextFace::Mono,
                rgba(mix_rgb(roles.text_tertiary, roles.text_secondary, 0.5), 255),
            ));
            paint_editor_line_text(
                prims,
                text_x,
                row_baseline(y, line_h, secondary.get()),
                line,
                cell_w,
                secondary,
                roles,
            );

            for (byte, primary) in &line.carets {
                let col = crate::native_editor::editor_display_column(
                    &line.text,
                    *byte,
                    line.column_start,
                );
                let x = text_x + col as f32 * cell_w;
                prims.push(DrawPrim::Stroke {
                    x,
                    y: y + 2.0,
                    w: 1.0,
                    h: line_h - 4.0,
                    radius: 0.0,
                    width: 1.0,
                    color: rgba(
                        if *primary {
                            roles.accent
                        } else {
                            roles.text_secondary
                        },
                        255,
                    ),
                });
                if *primary && !spec.preedit.is_empty() {
                    let preedit = spec.preedit.replace(['\r', '\n'], "↵");
                    let preedit_columns =
                        crate::native_editor::editor_display_column(&preedit, preedit.len(), 0)
                            .max(1);
                    prims.push(text_prim(
                        x + 2.0,
                        row_baseline(y, line_h, secondary.get()),
                        preedit.clone(),
                        secondary,
                        TextWeight::Regular,
                        TextFace::Mono,
                        rgba(roles.accent, 255),
                    ));
                    prims.push(DrawPrim::Stroke {
                        x: x + 2.0,
                        y: y + line_h - 2.0,
                        w: (preedit_columns as f32 * cell_w).max(2.0),
                        h: 1.0,
                        radius: 0.0,
                        width: 1.0,
                        color: rgba(roles.accent, 255),
                    });
                }
            }
        }
    }
    prims.push(DrawPrim::ClipPop);

    if footer_h > 0.0 {
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: footer_y,
            w: rect.width,
            h: footer_h,
            radius: 10.0,
            fill: rgba(roles.elevated, 255),
            blur: false,
        });
        prims.push(DrawPrim::Panel {
            x: rect.x + 10.0,
            y: footer_y + 7.0,
            w: 50.0,
            h: 17.0,
            radius: 4.0,
            fill: rgba(roles.accent, 38),
            blur: false,
        });
        prims.push(text_prim(
            rect.x + 17.0,
            row_baseline(footer_y + 5.0, 20.0, caption.get()),
            "EMACS".to_string(),
            caption,
            TextWeight::Bold,
            TextFace::UiBold,
            rgba(roles.accent, 255),
        ));
        let message = spec
            .minibuffer
            .as_ref()
            .or(spec.status.as_ref())
            .map(String::as_str)
            .unwrap_or("Ready");
        prims.push(text_prim(
            rect.x + 72.0,
            row_baseline(footer_y, footer_h, caption.get()),
            message.to_string(),
            caption,
            TextWeight::Regular,
            if spec.minibuffer.is_some() {
                TextFace::Mono
            } else {
                TextFace::Ui
            },
            rgba(readable_secondary(&roles), 255),
        ));
    }

    if spec.focused {
        prims.push(DrawPrim::Stroke {
            x: rect.x + 0.5,
            y: rect.y + 0.5,
            w: (rect.width - 1.0).max(0.0),
            h: (rect.height - 1.0).max(0.0),
            radius: 10.0,
            width: 1.0,
            color: rgba(roles.accent, 210),
        });
    }
}

/// Lower one projected editor row into positioned monospace runs. Tabs never
/// reach the font rasterizer: they become spaces at the canonical four-column
/// phase, while the following run is placed at its exact cell origin. Source
/// bytes remain untouched in the projection used by selection and hit-testing.
fn paint_editor_line_text(
    prims: &mut Vec<crate::widget::DrawPrim>,
    text_x: f32,
    baseline: f32,
    line: &crate::native_editor::EditorViewportLine,
    cell_w: f32,
    size: crate::type_scale::StepPx,
    roles: crate::settings::Roles,
) {
    use crate::native_editor::EditorSyntaxClass;

    paint_editor_text_range(
        prims,
        (text_x, baseline),
        line,
        0..line.text.len(),
        cell_w,
        size,
        crate::widget::rgba(roles.text_primary, 255),
    );
    for span in &line.syntax {
        let color = match span.class {
            EditorSyntaxClass::Table => crate::widget::rgba(roles.accent, 255),
            EditorSyntaxClass::Key => {
                crate::widget::rgba(mix_rgb(roles.text_primary, roles.accent, 0.62), 255)
            }
            EditorSyntaxClass::String => crate::widget::rgba(roles.success, 255),
            EditorSyntaxClass::Number => {
                crate::widget::rgba(mix_rgb(roles.accent, roles.success, 0.42), 255)
            }
            EditorSyntaxClass::Boolean => {
                crate::widget::rgba(mix_rgb(roles.accent, roles.danger, 0.28), 255)
            }
            EditorSyntaxClass::Comment => crate::widget::rgba(roles.text_tertiary, 255),
        };
        paint_editor_text_range(
            prims,
            (text_x, baseline),
            line,
            span.bytes.clone(),
            cell_w,
            size,
            color,
        );
    }
    for diagnostic in &line.diagnostics {
        let visual_start =
            grapheme_boundary_at_or_before(&line.text, diagnostic.bytes.start.min(line.text.len()));
        let visual_end =
            grapheme_boundary_at_or_after(&line.text, diagnostic.bytes.end.min(line.text.len()))
                .max(visual_start);
        let start = crate::native_editor::editor_display_column(
            &line.text,
            visual_start,
            line.column_start,
        );
        let end =
            crate::native_editor::editor_display_column(&line.text, visual_end, line.column_start);
        let cells = end.saturating_sub(start).max(1);
        prims.push(crate::widget::DrawPrim::Stroke {
            x: text_x + start as f32 * cell_w,
            y: baseline + 2.0,
            w: cells as f32 * cell_w,
            h: 1.0,
            radius: 0.0,
            width: 1.0,
            color: crate::widget::rgba(
                if diagnostic.error {
                    roles.danger
                } else {
                    roles.accent
                },
                255,
            ),
        });
    }
}

fn paint_editor_text_range(
    prims: &mut Vec<crate::widget::DrawPrim>,
    origin_px: (f32, f32),
    line: &crate::native_editor::EditorViewportLine,
    bytes: std::ops::Range<usize>,
    cell_w: f32,
    size: crate::type_scale::StepPx,
    color: crate::widget::Rgba,
) {
    use crate::widget::{TextFace, TextWeight, text_prim};

    let (text_x, baseline) = origin_px;

    let start = grapheme_boundary_at_or_before(&line.text, bytes.start.min(line.text.len()));
    let end = grapheme_boundary_at_or_after(&line.text, bytes.end.min(line.text.len())).max(start);
    if start == end {
        return;
    }

    let origin = line.column_start;
    let mut column = origin.saturating_add(crate::native_editor::editor_display_column(
        &line.text,
        start,
        line.column_start,
    ));
    let mut run_start = start;
    let mut run_column = column;

    for (relative, grapheme) in line.text[start..end].grapheme_indices() {
        let byte = start + relative;
        if grapheme != "\t" && !grapheme.chars().any(char::is_control) {
            column = column.saturating_add(crate::native_editor::editor_grapheme_columns(
                grapheme, column,
            ));
            continue;
        }

        if run_start < byte {
            prims.push(text_prim(
                text_x + column_offset(run_column, origin) as f32 * cell_w,
                baseline,
                line.text[run_start..byte].to_string(),
                size,
                TextWeight::Regular,
                TextFace::Mono,
                color,
            ));
        }

        if grapheme == "\t" {
            let cells = crate::native_editor::editor_grapheme_columns(grapheme, column);
            prims.push(text_prim(
                text_x + column_offset(column, origin) as f32 * cell_w,
                baseline,
                " ".repeat(cells),
                size,
                TextWeight::Regular,
                TextFace::Mono,
                color,
            ));
            column = column.saturating_add(cells);
        }

        run_start = byte + grapheme.len();
        run_column = column;
    }

    if run_start < end {
        prims.push(text_prim(
            text_x + column_offset(run_column, origin) as f32 * cell_w,
            baseline,
            line.text[run_start..end].to_string(),
            size,
            TextWeight::Regular,
            TextFace::Mono,
            color,
        ));
    }
}

fn column_offset(column: usize, origin: usize) -> usize {
    column.saturating_sub(origin)
}

fn paint_control_surface(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    state: ControlState,
    roles: crate::settings::Roles,
) {
    use crate::widget::{DrawPrim, rgba};

    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height,
        radius: 8.0,
        fill: rgba(
            if state.pressed {
                mix_rgb(roles.elevated, roles.accent, 0.16)
            } else if state.hovered {
                mix_rgb(roles.elevated, roles.text_primary, 0.06)
            } else {
                roles.elevated
            },
            255,
        ),
        blur: false,
    });
    prims.push(DrawPrim::Stroke {
        x: rect.x + 0.5,
        y: rect.y + 0.5,
        w: (rect.width - 1.0).max(0.0),
        h: (rect.height - 1.0).max(0.0),
        radius: 8.0,
        width: if state.focus_visible { 2.0 } else { 1.0 },
        color: rgba(
            if state.invalid {
                roles.danger
            } else if state.focus_visible {
                roles.accent
            } else {
                roles.separator
            },
            220,
        ),
    });
}

fn control_text_color(state: ControlState, roles: &crate::settings::Roles) -> [u8; 3] {
    if state.enabled {
        roles.text_primary
    } else {
        // Disabled remains visibly subordinate, but never disappears into the
        // surface—especially for icon-only document actions.
        mix_rgb(roles.text_tertiary, roles.text_secondary, 0.35)
    }
}

/// Native tab apps use an OS-like, zoom-independent type ladder. We still mint
/// every size through the repository's named type-scale proof token, but pick
/// the base that lands exactly on the app-shell contract rather than inheriting
/// the terminal's monospace zoom: 24 / 20 / 13 / 13 / 11 logical pixels.
fn native_type_px(step: crate::type_scale::TypeStep) -> crate::type_scale::StepPx {
    use crate::type_scale::TypeStep;

    let scale = crate::native_appearance::text_scale();
    match step {
        TypeStep::Display => TypeStep::Display.px(15.0),
        TypeStep::Title => TypeStep::Title.px(20.0 / TypeStep::Title.factor()),
        TypeStep::Body => TypeStep::Body.px(13.0),
        TypeStep::Secondary => TypeStep::Secondary.px(13.0 / TypeStep::Secondary.factor()),
        TypeStep::Caption => TypeStep::Caption.px(11.0 / TypeStep::Caption.factor()),
    }
    .scaled(scale)
}

fn mix_rgb(a: [u8; 3], b: [u8; 3], amount: f32) -> [u8; 3] {
    let amount = amount.clamp(0.0, 1.0);
    std::array::from_fn(|index| {
        (f32::from(a[index]) + (f32::from(b[index]) - f32::from(a[index])) * amount).round() as u8
    })
}

fn readable_secondary(roles: &crate::settings::Roles) -> [u8; 3] {
    // Native apps carry more low-density prose than terminal chrome. Lift the
    // conditioned secondary role enough to stay effortless at 1× while keeping
    // a visible step below primary text.
    mix_rgb(roles.text_secondary, roles.text_primary, 0.55)
}

fn quiet_text(roles: &crate::settings::Roles) -> [u8; 3] {
    // Quiet is hierarchy, never "disabled". Keep authored supporting copy on
    // the same contrast-conditioned secondary ramp; only disabled controls use
    // tertiary text.
    readable_secondary(roles)
}

fn style_text_color(style: StyleRef, roles: &crate::settings::Roles) -> [u8; 3] {
    match style {
        StyleRef::Hero | StyleRef::Primary => roles.text_primary,
        StyleRef::Accent => roles.accent,
        StyleRef::Success => roles.success,
        StyleRef::Danger => roles.danger,
        StyleRef::Quiet => quiet_text(roles),
        StyleRef::Plain | StyleRef::Secondary | StyleRef::Navigation | StyleRef::Setting => {
            readable_secondary(roles)
        }
        StyleRef::Code => roles.text_primary,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompileError {
    InvalidViewport,
    InvalidLayout(UiKey),
    EmptyKey,
    DuplicateKey(UiKey),
    CustomWithoutAudit(UiKey),
    PaintOnlyAction(UiKey),
    ObserverMismatch(UiKey),
}

struct Compiler {
    seen: HashSet<UiKey>,
    output: CompiledUi,
}

impl Compiler {
    fn node(
        &mut self,
        node: &UiNode,
        rect: LogicalRect,
        inherited_clip: LogicalRect,
        parent: Option<&UiKey>,
    ) -> Result<(), CompileError> {
        if node.key.as_str().trim().is_empty() {
            return Err(CompileError::EmptyKey);
        }
        if !node.layout.is_valid() || !rect.is_valid() {
            return Err(CompileError::InvalidLayout(node.key.clone()));
        }
        if !self.seen.insert(node.key.clone()) {
            return Err(CompileError::DuplicateKey(node.key.clone()));
        }
        if let UiContent::Custom(custom) = &node.content
            && custom.audit_id.trim().is_empty()
        {
            return Err(CompileError::CustomWithoutAudit(node.key.clone()));
        }

        let projection = node.content.semantic();
        if node.paint_only && (projection.action.is_some() || projection.focusable) {
            return Err(CompileError::PaintOnlyAction(node.key.clone()));
        }

        let own_clip = inherited_clip.intersect(rect);
        let Some(visible) = own_clip else {
            // A clipped subtree is not materialized in any observer.  Virtualized
            // views can compile requested semantic ranges separately.
            return Ok(());
        };
        self.output.paint.push(PaintNode {
            key: node.key.clone(),
            rect,
            clip: visible,
            content: node.content.clone(),
        });
        if !node.paint_only {
            self.output.semantics.push(SemanticNode {
                key: node.key.clone(),
                parent: parent.cloned(),
                rect: visible,
                role: projection.role,
                label: projection.label,
                value: projection.value,
                state: projection.state,
                action: projection.action.clone(),
                audit_id: projection.audit_id,
            });
            if projection.focusable {
                self.output.focus_order.push(node.key.clone());
            }
            if let Some(action) = projection.action
                && projection.state.is_none_or(|state| state.enabled)
            {
                self.output.hits.push(HitRegion {
                    key: node.key.clone(),
                    rect: visible,
                    action,
                });
            }
        }

        let content_rect = rect.inset(node.layout.padding);
        let child_clip = if node.layout.clip {
            inherited_clip.intersect(content_rect).unwrap_or_default()
        } else {
            inherited_clip
        };
        if child_clip.is_empty() {
            return Ok(());
        }
        let child_rects = layout_children(node, content_rect)?;
        let semantic_parent = if node.paint_only {
            parent
        } else {
            Some(&node.key)
        };
        for (child, child_rect) in node.children.iter().zip(child_rects) {
            self.node(child, child_rect, child_clip, semantic_parent)?;
        }
        Ok(())
    }
}

fn layout_children(node: &UiNode, content: LogicalRect) -> Result<Vec<LogicalRect>, CompileError> {
    if node.children.is_empty() {
        return Ok(Vec::new());
    }
    if node.layout.flow == Flow::Overlay {
        return node
            .children
            .iter()
            .map(|child| child_rect_overlay(child, content))
            .collect::<Result<Vec<_>, _>>();
    }

    let is_row = node.layout.flow == Flow::Row;
    let main_extent = if is_row {
        content.width
    } else {
        content.height
    };
    let cross_extent = if is_row {
        content.height
    } else {
        content.width
    };
    let total_gap = node.layout.gap * node.children.len().saturating_sub(1) as f32;
    let available = (main_extent - total_gap).max(0.0);
    let mut fixed = 0.0;
    let mut fills = 0usize;
    let mut main_lengths = Vec::with_capacity(node.children.len());
    for child in &node.children {
        let (intrinsic_w, intrinsic_h) = child.content.intrinsic_size();
        let main = if is_row {
            child.layout.width
        } else {
            child.layout.height
        };
        let intrinsic = if is_row { intrinsic_w } else { intrinsic_h };
        let value = match main {
            Length::Fill => {
                fills += 1;
                None
            }
            Length::Intrinsic => Some(intrinsic),
            Length::Fixed(v) => Some(v),
            Length::Fraction(v) => Some(available * v),
        };
        if let Some(value) = value {
            fixed += value;
        }
        main_lengths.push(value);
    }
    let fill = if fills == 0 {
        0.0
    } else {
        (available - fixed).max(0.0) / fills as f32
    };

    let mut cursor = if is_row { content.x } else { content.y };
    let mut out = Vec::with_capacity(node.children.len());
    for (child, main) in node.children.iter().zip(main_lengths) {
        let main = main.unwrap_or(fill).max(0.0);
        let (intrinsic_w, intrinsic_h) = child.content.intrinsic_size();
        let cross_length = if is_row {
            child.layout.height
        } else {
            child.layout.width
        };
        let cross_intrinsic = if is_row { intrinsic_h } else { intrinsic_w };
        let cross = resolve_length(cross_length, cross_extent, cross_intrinsic);
        let rect = if is_row {
            LogicalRect::new(cursor, content.y, main, cross)
        } else {
            LogicalRect::new(content.x, cursor, cross, main)
        };
        if !rect.is_valid() {
            return Err(CompileError::InvalidLayout(child.key.clone()));
        }
        out.push(rect);
        cursor += main + node.layout.gap;
    }
    Ok(out)
}

fn child_rect_overlay(child: &UiNode, content: LogicalRect) -> Result<LogicalRect, CompileError> {
    let (intrinsic_w, intrinsic_h) = child.content.intrinsic_size();
    let width = resolve_length(child.layout.width, content.width, intrinsic_w);
    let height = resolve_length(child.layout.height, content.height, intrinsic_h);
    let rect = LogicalRect::new(content.x, content.y, width, height);
    if rect.is_valid() {
        Ok(rect)
    } else {
        Err(CompileError::InvalidLayout(child.key.clone()))
    }
}

fn resolve_length(length: Length, available: f32, intrinsic: f32) -> f32 {
    match length {
        Length::Fill => available,
        Length::Intrinsic => intrinsic.min(available),
        Length::Fixed(value) => value.min(available),
        Length::Fraction(value) => available * value,
    }
    .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_visual_labels_elide_utf8_to_the_authored_width_budget() {
        let px = native_type_px(crate::type_scale::TypeStep::Secondary).get();
        let value = "Use default · Linear corrected · 終 👩‍💻";
        let label = elide_ui_label(value, 104.0, px);
        assert!(label.ends_with('…'));
        assert!(crate::tray_raster::ui_text_width(&label, px) <= 104.0);
        assert!(std::str::from_utf8(label.as_bytes()).is_ok());
        assert_eq!(elide_ui_label("Nord", 104.0, px), "Nord");
    }

    #[test]
    fn responsive_button_and_status_labels_preserve_complete_graphemes() {
        for (source, expected) in [("A👩‍💻WWWW", "A👩‍💻…"), ("AéWWWW", "Aé…"), ("A🇺🇳WWWW", "A🇺🇳…")]
        {
            for step in [TypeStep::Secondary, TypeStep::Caption] {
                let px = native_type_px(step).get();
                let max_width = visual_text_width(expected, px, crate::widget::TextFace::Ui) + 0.01;
                let fitted = if step == TypeStep::Secondary {
                    fit_native_button_label(source, max_width)
                } else {
                    fit_native_status_label(source, max_width)
                };
                assert_eq!(fitted, expected, "{source:?} at {step:?}");
            }
        }

        let capped_source = format!("{}👩‍💻W", "a".repeat(255));
        let capped_expected = format!("{}👩‍💻…", "a".repeat(255));
        assert_eq!(
            elide_text_label(
                &capped_source,
                100_000.0,
                native_type_px(TypeStep::Secondary).get(),
                crate::widget::TextFace::Ui,
            ),
            capped_expected
        );
    }

    #[test]
    fn clip_introspection_ignores_subpixel_intersection_noise_only() {
        let rect = LogicalRect::new(12.0, 76.0, 172.0, 28.3);
        let rounding_noise = LogicalRect::new(12.001, 76.0, 171.999, 28.299);
        let visible_clip = LogicalRect::new(13.0, 76.0, 171.0, 28.3);

        assert!(!materially_clipped(rect, rect));
        assert!(!materially_clipped(rect, rounding_noise));
        assert!(materially_clipped(rect, visible_clip));
    }

    #[test]
    fn paint_audit_reports_exact_text_and_button_overflow() {
        let tree = UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(8.0)).gap(4.0))
            .children(vec![
                UiNode::new(
                    "long-text",
                    UiContent::Text(TextSpec::text(
                        "A renderer-aware sentence that cannot fit this phone row",
                    )),
                )
                .layout(Layout::default().height(Length::Fixed(24.0))),
                button("long-button", "An impossibly verbose toolbar action"),
            ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 140.0, 100.0))
            .expect("audit fixture compiles");
        let audit = compiled.paint_audit_lines();
        assert!(audit[0].contains("text-nodes=2 overflow=2"));
        assert!(audit.iter().any(|line| {
            line.contains("key=\"long-text\"")
                && line.contains("kind=text")
                && line.contains("overflow=true")
                && line.contains("painted=\"")
                && line.contains('…')
        }));
        assert!(audit.iter().any(|line| {
            line.contains("key=\"long-button\"")
                && line.contains("kind=button")
                && line.contains("overflow=true")
                && line.contains('…')
        }));

        let painted = compiled
            .tray(aterm_render::Theme::default(), 13.0)
            .prims
            .into_iter()
            .filter_map(|primitive| match primitive {
                crate::widget::DrawPrim::Text { s, .. } => Some(s),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(painted.len(), 2);
        assert!(painted.iter().all(|text| text.ends_with('…')));
    }

    #[test]
    fn paint_audit_measures_text_against_the_effective_ancestor_clip() {
        let text = "This fits its authored width but not the clipped viewport";
        let tree = UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::row().clipped())
            .children(vec![
                UiNode::new("clipped-text", UiContent::Text(TextSpec::text(text))).layout(
                    Layout::default()
                        .width(Length::Fixed(360.0))
                        .height(Length::Fixed(28.0)),
                ),
            ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 132.0, 60.0))
            .expect("clipped audit fixture compiles");
        let paint = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "clipped-text")
            .unwrap();
        assert_eq!(paint.rect.width, 360.0);
        assert_eq!(paint.clip.width, 132.0);

        let audit = compiled.paint_audit_lines();
        assert!(audit[0].contains("clipped=1"));
        assert!(audit.iter().any(|line| {
            line.starts_with("paint-node key=\"clipped-text\"")
                && line.contains("rect=0.0,0.0,360.0,28.0")
                && line.contains("clip=0.0,0.0,132.0,28.0")
                && line.contains("visible=true")
                && line.contains("clipped=true")
        }));
        assert!(audit.iter().any(|line| {
            line.starts_with("paint-text key=\"clipped-text\"")
                && line.contains("overflow=true")
                && line.contains("clip-truncated=true")
                && line.contains("available=132.0")
                && line.contains("authored-available=360.0")
        }));
    }

    #[test]
    fn paint_audit_inventory_includes_preview_markdown_and_editor_paint_state() {
        let preview = UiTree::new(UiNode::new(
            "settings/preview",
            UiContent::SettingsPreview(Box::default()),
        ))
        .compile(LogicalRect::new(0.0, 0.0, 640.0, 220.0))
        .unwrap();
        let preview_audit = preview.paint_audit_lines();
        assert!(preview_audit.iter().any(|line| {
            line.starts_with("paint-node key=\"settings/preview\"")
                && line.contains("kind=settings-preview")
                && line.contains("role=Group")
        }));
        assert!(preview_audit.iter().any(|line| {
            line.starts_with("paint-preview key=\"settings/preview\"")
                && line.contains("audit-state=")
                && line.contains("animation=")
                && line.contains("paint-fingerprint=")
        }));

        let markdown = UiTree::new(UiNode::new(
            "markdown/block/4",
            UiContent::MarkdownBlock(MarkdownBlockSpec {
                text: "A paragraph that wraps naturally without being an overflow.".to_string(),
                kind: MarkdownBlockKind::Paragraph,
                dense: false,
                selectable: true,
                action: Some(ActionId::new("markdown/select-block/4")),
                selected: true,
                source: 80..142,
                visual_row: 0,
                total_visual_rows: 3,
                estimated_height: 96.0,
            }),
        ))
        .compile(LogicalRect::new(0.0, 0.0, 180.0, 96.0))
        .unwrap();
        let markdown_audit = markdown.paint_audit_lines();
        assert!(markdown_audit.iter().any(|line| {
            line.starts_with("paint-markdown key=\"markdown/block/4\"")
                && line.contains("block-kind=paragraph")
                && line.contains("source=80..142")
                && line.contains("selected=true")
                && line.contains("wrapped-lines=")
                && line.contains("elided=false")
                && line.contains("paint-fingerprint=")
        }));

        let editor = editor_viewport()
            .compile(LogicalRect::new(0.0, 0.0, 760.0, 420.0))
            .unwrap();
        let editor_audit = editor.paint_audit_lines();
        assert!(editor_audit.iter().any(|line| {
            line.starts_with("paint-editor key=\"editor/buffer\"")
                && line.contains("document=\"document:7@12\"")
                && line.contains("dirty=true")
                && line.contains("focused=true")
                && line.contains("visible-rows=2")
                && line.contains("carets=1")
                && line.contains("selections=1")
                && line.contains("modeline=")
                && line.contains("minibuffer-active=false")
                && line.contains("paint-fingerprint=")
        }));
    }

    fn button(key: &str, label: &str) -> UiNode {
        UiNode::new(
            key,
            UiContent::Button(Control::new(
                ButtonSpec::new(label),
                ActionId::new(format!("activate/{key}")),
            )),
        )
        .layout(Layout::default().height(Length::Fixed(40.0)))
    }

    fn editor_viewport() -> UiTree {
        use crate::native_editor::{
            EditorSelectionSpan, EditorViewportLine, EditorViewportProjection,
        };

        UiTree::new(UiNode::new(
            "editor/buffer",
            UiContent::TextViewport(TextViewportSpec {
                label: "notes.md".to_string(),
                document_key: "document:7@12".to_string(),
                selectable: true,
                projection: Some(EditorViewportProjection {
                    first_line: 40,
                    total_lines: 2_000_000,
                    lines: vec![
                        EditorViewportLine {
                            number: 40,
                            source: 9_000..9_016,
                            column_start: 0,
                            text: "let answer = 42;".to_string(),
                            selections: vec![EditorSelectionSpan {
                                bytes: 4..10,
                                continues: false,
                                primary: true,
                            }],
                            carets: vec![(10, true)],
                            syntax: Vec::new(),
                            diagnostics: Vec::new(),
                        },
                        EditorViewportLine {
                            number: 41,
                            source: 9_017..9_017,
                            column_start: 0,
                            text: String::new(),
                            selections: Vec::new(),
                            carets: Vec::new(),
                            syntax: Vec::new(),
                            diagnostics: Vec::new(),
                        },
                    ],
                }),
                preedit: "λ".to_string(),
                status: Some("Saved".to_string()),
                semantic_status: Some("Saved".to_string()),
                minibuffer: None,
                cursor_label: Some("Ln 41, Col 11".to_string()),
                dirty: true,
                saving: false,
                focused: true,
                action: Some(ActionId::new("editor/focus-buffer")),
            }),
        ))
    }

    #[test]
    fn editor_fingerprint_tracks_every_paint_only_viewport_state() {
        use crate::native_editor::{EditorDiagnosticSpan, EditorSyntaxClass, EditorSyntaxSpan};

        let compile = |tree: &UiTree| {
            tree.compile(LogicalRect::new(0.0, 0.0, 640.0, 360.0))
                .unwrap()
                .fingerprint()
        };
        let base = editor_viewport();
        let base_fp = compile(&base);

        let mut minibuffer = editor_viewport();
        let UiContent::TextViewport(spec) = &mut minibuffer.root.content else {
            panic!("editor viewport fixture")
        };
        spec.minibuffer = Some("Find: semantic".to_string());
        assert_ne!(base_fp, compile(&minibuffer));

        let mut cursor = editor_viewport();
        let UiContent::TextViewport(spec) = &mut cursor.root.content else {
            panic!("editor viewport fixture")
        };
        spec.cursor_label = Some("Ln 99, Col 3".to_string());
        assert_ne!(base_fp, compile(&cursor));

        let mut selection = editor_viewport();
        let UiContent::TextViewport(spec) = &mut selection.root.content else {
            panic!("editor viewport fixture")
        };
        spec.projection.as_mut().unwrap().lines[0].selections[0].bytes = 0..3;
        assert_ne!(base_fp, compile(&selection));

        let mut syntax = editor_viewport();
        let UiContent::TextViewport(spec) = &mut syntax.root.content else {
            panic!("editor viewport fixture")
        };
        spec.projection.as_mut().unwrap().lines[0]
            .syntax
            .push(EditorSyntaxSpan {
                bytes: 0..3,
                class: EditorSyntaxClass::Key,
            });
        assert_ne!(base_fp, compile(&syntax));

        let mut diagnostic = editor_viewport();
        let UiContent::TextViewport(spec) = &mut diagnostic.root.content else {
            panic!("editor viewport fixture")
        };
        spec.projection.as_mut().unwrap().lines[0]
            .diagnostics
            .push(EditorDiagnosticSpan {
                bytes: 4..10,
                error: true,
            });
        assert_ne!(base_fp, compile(&diagnostic));
    }

    fn form() -> UiTree {
        UiTree::new(
            UiNode::new(
                "app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(
                Layout::column()
                    .padding(Insets::all(8.0))
                    .gap(4.0)
                    .clipped(),
            )
            .children(vec![
                UiNode::new("title", UiContent::Text(TextSpec::heading("Settings")))
                    .layout(Layout::default().height(Length::Fixed(32.0))),
                button("save", "Save"),
                button("reset", "Reset"),
            ]),
        )
    }

    #[test]
    fn one_control_description_drives_every_observer() {
        let compiled = form()
            .compile(LogicalRect::new(0.0, 0.0, 320.0, 200.0))
            .unwrap();
        compiled.validate_parity().unwrap();

        let key = UiKey::new("save");
        let semantic = compiled.semantic(&key).unwrap();
        assert_eq!(semantic.role, SemanticRole::Button);
        assert_eq!(semantic.label, "Save");
        assert_eq!(semantic.action.as_ref().unwrap().as_str(), "activate/save");
        assert_eq!(compiled.focus_order, [key.clone(), UiKey::new("reset")]);
        assert_eq!(
            compiled.hit_test(10.0, 50.0).map(|hit| hit.action.as_str()),
            Some("activate/save")
        );
        assert!(compiled.controls_lines().iter().any(|line| {
            line.contains("key=\"save\"")
                && line.contains("role=Button")
                && line.contains("action=activate/save")
        }));
    }

    #[test]
    fn editor_viewport_paints_real_visible_text_selection_caret_and_status() {
        use crate::widget::DrawPrim;

        let compiled = editor_viewport()
            .compile(LogicalRect::new(0.0, 0.0, 760.0, 420.0))
            .unwrap();
        compiled.validate_parity().unwrap();
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(prims.iter().any(
            |primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "let answer = 42;")
        ));
        assert!(
            prims
                .iter()
                .any(|primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "λ"))
        );
        assert!(
            prims
                .iter()
                .any(|primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "Saved"))
        );
        assert!(
            prims
                .iter()
                .any(|primitive| matches!(primitive, DrawPrim::Stroke { h, .. } if *h > 10.0))
        );
        assert!(
            prims
                .iter()
                .any(|primitive| matches!(primitive, DrawPrim::ClipPush { .. }))
        );
        assert!(!prims.iter().any(
            |primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "document:7@12")
        ));
        assert_eq!(
            compiled
                .hit_test(100.0, 100.0)
                .map(|hit| hit.action.as_str()),
            Some("editor/focus-buffer")
        );
    }

    #[test]
    fn editor_viewport_paints_native_syntax_runs_and_diagnostic_underlines() {
        use crate::native_editor::{EditorDiagnosticSpan, EditorSyntaxClass, EditorSyntaxSpan};
        use crate::widget::DrawPrim;

        let mut tree = editor_viewport();
        let UiContent::TextViewport(spec) = &mut tree.root.content else {
            panic!("editor viewport fixture")
        };
        let line = &mut spec.projection.as_mut().unwrap().lines[0];
        line.syntax.push(EditorSyntaxSpan {
            bytes: 0..3,
            class: EditorSyntaxClass::Key,
        });
        line.diagnostics.push(EditorDiagnosticSpan {
            bytes: 4..10,
            error: true,
        });

        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 760.0, 420.0))
            .unwrap();
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(
            prims
                .iter()
                .any(|primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "let"))
        );
        assert!(prims.iter().any(|primitive| {
            matches!(
                primitive,
                DrawPrim::Stroke { h, w, .. } if *h == 1.0 && *w > 1.0
            )
        }));
    }

    #[test]
    fn editor_selection_caret_and_diagnostic_share_grapheme_cell_geometry() {
        use crate::native_editor::{
            EditorDiagnosticSpan, EditorSelectionSpan, EditorViewportLine, EditorViewportProjection,
        };
        use crate::type_scale::TypeStep;
        use crate::widget::DrawPrim;

        let text = "e\u{301}中👩‍💻".to_string();
        let after_accent = "e\u{301}".len();
        let after_cjk = after_accent + "中".len();
        let after_woman = after_cjk + "👩".len();
        let text_len = text.len();
        let spec = TextViewportSpec {
            label: "unicode.toml".to_string(),
            document_key: "document:unicode@1".to_string(),
            selectable: true,
            projection: Some(EditorViewportProjection {
                first_line: 0,
                total_lines: 1,
                lines: vec![EditorViewportLine {
                    number: 0,
                    source: 0..text_len,
                    column_start: 0,
                    text,
                    selections: vec![
                        EditorSelectionSpan {
                            bytes: 0..after_accent,
                            continues: false,
                            primary: true,
                        },
                        EditorSelectionSpan {
                            bytes: after_cjk..after_woman,
                            continues: false,
                            primary: false,
                        },
                    ],
                    carets: vec![(text_len, true)],
                    syntax: Vec::new(),
                    diagnostics: vec![
                        EditorDiagnosticSpan {
                            bytes: after_accent..after_cjk,
                            error: true,
                        },
                        EditorDiagnosticSpan {
                            bytes: after_cjk..after_woman,
                            error: false,
                        },
                    ],
                }],
            }),
            preedit: String::new(),
            status: None,
            semantic_status: None,
            minibuffer: None,
            cursor_label: None,
            dirty: false,
            saving: false,
            focused: true,
            action: Some(ActionId::new("editor/focus-buffer")),
        };
        let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
        let geometry = text_viewport_geometry(rect);
        let prims = UiTree::new(UiNode::new("editor/unicode", UiContent::TextViewport(spec)))
            .compile(rect)
            .unwrap()
            .tray(aterm_render::Theme::default(), 13.0)
            .prims;
        let close = |left: f32, right: f32| (left - right).abs() < 0.01;

        assert!(
            prims.iter().any(
                |primitive| matches!(primitive, DrawPrim::Text { s, .. } if s == "e\u{301}中👩‍💻")
            )
        );
        assert!(
            prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Panel { x, y, w, h, fill: [_, _, _, 86], .. }
                        if close(*x, geometry.text_x)
                            && close(*y, geometry.body_y + 1.0)
                            && close(*w, geometry.cell_w)
                            && close(*h, geometry.line_h - 2.0)
                )
            }),
            "the composed selection occupies exactly one cell"
        );
        assert!(
            prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Panel { x, w, fill: [_, _, _, 54], .. }
                        if close(*x, geometry.text_x + geometry.cell_w * 3.0)
                            && close(*w, geometry.cell_w * 2.0)
                )
            }),
            "a selection ending inside a ZWJ sequence expands to its two-cell grapheme"
        );

        let diagnostic_y = crate::tray_raster::row_baseline(
            geometry.body_y,
            geometry.line_h,
            native_type_px(TypeStep::Secondary).get(),
        ) + 2.0;
        assert!(
            prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Stroke { x, y, w, h, .. }
                        if close(*x, geometry.text_x + geometry.cell_w)
                            && close(*y, diagnostic_y)
                            && close(*w, geometry.cell_w * 2.0)
                            && close(*h, 1.0)
                )
            }),
            "the CJK diagnostic occupies exactly two cells"
        );
        assert!(
            prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Stroke { x, y, w, h, .. }
                        if close(*x, geometry.text_x + geometry.cell_w * 3.0)
                            && close(*y, diagnostic_y)
                            && close(*w, geometry.cell_w * 2.0)
                            && close(*h, 1.0)
                )
            }),
            "a diagnostic ending inside a ZWJ sequence expands to its two-cell grapheme"
        );
        assert!(
            prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Stroke { x, y, w, h, .. }
                        if close(*x, geometry.text_x + geometry.cell_w * 5.0)
                            && close(*y, geometry.body_y + 2.0)
                            && close(*w, 1.0)
                            && close(*h, geometry.line_h - 4.0)
                )
            }),
            "the caret follows the one-cell accent, two-cell CJK, and two-cell ZWJ emoji"
        );
    }

    #[test]
    fn editor_preedit_underlines_use_complete_grapheme_cell_widths() {
        use crate::native_editor::{EditorViewportLine, EditorViewportProjection};
        use crate::widget::DrawPrim;

        let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
        let geometry = text_viewport_geometry(rect);
        let close = |left: f32, right: f32| (left - right).abs() < 0.01;
        for (preedit, cells) in [("e\u{301}", 1usize), ("中", 2), ("👩‍💻", 2)] {
            let spec = TextViewportSpec {
                label: "ime.toml".to_string(),
                document_key: "document:ime@1".to_string(),
                selectable: true,
                projection: Some(EditorViewportProjection {
                    first_line: 0,
                    total_lines: 1,
                    lines: vec![EditorViewportLine {
                        number: 0,
                        source: 0..0,
                        column_start: 0,
                        text: String::new(),
                        selections: Vec::new(),
                        carets: vec![(0, true)],
                        syntax: Vec::new(),
                        diagnostics: Vec::new(),
                    }],
                }),
                preedit: preedit.to_string(),
                status: None,
                semantic_status: None,
                minibuffer: None,
                cursor_label: None,
                dirty: false,
                saving: false,
                focused: true,
                action: Some(ActionId::new("editor/focus-buffer")),
            };
            let prims = UiTree::new(UiNode::new("editor/ime", UiContent::TextViewport(spec)))
                .compile(rect)
                .unwrap()
                .tray(aterm_render::Theme::default(), 13.0)
                .prims;
            assert!(
                prims.iter().any(|primitive| {
                    matches!(primitive, DrawPrim::Text { s, .. } if s == preedit)
                }),
                "the complete preedit cluster is painted for {preedit:?}"
            );
            assert!(
                prims.iter().any(|primitive| {
                    matches!(
                        primitive,
                        DrawPrim::Stroke { x, y, w, h, .. }
                            if close(*x, geometry.text_x + 2.0)
                                && close(*y, geometry.body_y + geometry.line_h - 2.0)
                                && close(*w, cells as f32 * geometry.cell_w)
                                && close(*h, 1.0)
                    )
                }),
                "preedit underline width drifted for {preedit:?}"
            );
        }
    }

    #[test]
    fn editor_display_columns_are_utf8_safe_and_tab_aligned() {
        let column = crate::native_editor::editor_display_column;
        assert_eq!(column("α\tβ", 0, 0), 0);
        assert_eq!(column("α\tβ", "α".len(), 0), 1);
        assert_eq!(column("α\tβ", "α\t".len(), 0), 4);
        // Deliberately land inside β; the mapping retreats to its UTF-8 boundary.
        assert_eq!(column("α\tβ", "α\t".len() + 1, 0), 4);
        assert_eq!(column("α\tβ", "α\tβ".len(), 0), 5);
        assert_eq!(
            column("\tβ", "\t".len(), 3),
            1,
            "a shifted line keeps the canonical tab-stop phase"
        );

        let composed = "e\u{301}中👩‍💻";
        let after_accent = "e\u{301}".len();
        let after_cjk = after_accent + "中".len();
        assert_eq!(column(composed, 1, 0), 0, "never enter a combining cluster");
        assert_eq!(column(composed, after_accent, 0), 1);
        assert_eq!(column(composed, after_cjk, 0), 3);
        assert_eq!(column(composed, composed.len(), 0), 5);
    }

    #[test]
    fn text_field_paints_the_canonical_selection_caret_preedit_and_swatch() {
        use crate::widget::DrawPrim;

        let mut input = crate::native_text_input::TextInputState::new("aβz".to_string());
        input.set_selection(1, 3);
        input.set_preedit("に".to_string(), Some(0..3));
        let projection = input.projection();
        let control = Control::new(
            TextFieldSpec {
                label: "Accent".to_string(),
                placeholder: Some("effective default".to_string()),
                secret: false,
                visual_value: None,
                input: Some(projection.clone()),
                swatch: Some([12, 34, 56]),
            },
            ActionId::new("set/accent"),
        )
        .value(SemanticValue::Text(projection.text.clone()))
        .state(ControlState {
            focused: true,
            focus_visible: true,
            ..ControlState::default()
        });
        let compiled = UiTree::new(UiNode::new("accent", UiContent::TextField(control)))
            .compile(LogicalRect::new(0.0, 0.0, 280.0, 40.0))
            .unwrap();
        compiled.validate_parity().unwrap();
        assert_eq!(
            compiled.semantic(&UiKey::new("accent")).unwrap().value,
            SemanticValue::Text("aにz".to_string())
        );
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(
            prims
                .iter()
                .any(|prim| matches!(prim, DrawPrim::Text { s, .. } if s == "aにz"))
        );
        assert!(prims.iter().any(|prim| matches!(
            prim,
            DrawPrim::Panel {
                fill: [_, _, _, 82],
                ..
            }
        )));
        assert!(prims.iter().any(
            |prim| matches!(prim, DrawPrim::Stroke { h, width, .. } if *h == 1.0 && *width == 1.0)
        ));
        assert!(
            prims.iter().any(
                |prim| matches!(prim, DrawPrim::Stroke { w, h, .. } if *w == 1.0 && *h > 10.0)
            )
        );
        assert!(prims.iter().any(|prim| matches!(
            prim,
            DrawPrim::Panel {
                fill: [12, 34, 56, 255],
                ..
            }
        )));
    }

    #[test]
    fn text_field_pointer_uses_painted_proportional_grapheme_geometry() {
        let text = "Aé👩‍💻Z".to_string();
        let mut input = crate::native_text_input::TextInputState::new(text.clone());
        input.set_selection(3, 3);
        let control = Control::new(
            TextFieldSpec {
                label: "Color name".to_string(),
                placeholder: None,
                secret: false,
                visual_value: None,
                input: Some(input.projection()),
                swatch: Some([30, 40, 50]),
            },
            ActionId::new("set/color"),
        )
        .value(SemanticValue::Text(text.clone()))
        .state(ControlState {
            focused: true,
            ..ControlState::default()
        });
        let compiled = UiTree::new(UiNode::new("color", UiContent::TextField(control)))
            .compile(LogicalRect::new(20.0, 10.0, 300.0, 40.0))
            .unwrap();
        let key = UiKey::new("color");
        let paint = compiled.paint.iter().find(|node| node.key == key).unwrap();
        let UiContent::TextField(control) = &paint.content else {
            unreachable!();
        };
        let px = native_type_px(crate::type_scale::TypeStep::Secondary).get();
        let geometry = text_field_geometry(control, paint.rect, px);
        let cluster_start = text_field_x_for_byte(&text, &geometry, 3, px);
        let cluster_end = text_field_x_for_byte(&text, &geometry, 14, px);

        assert_eq!(
            compiled.text_field_byte_at(&key, cluster_start + (cluster_end - cluster_start) * 0.49),
            Some(3)
        );
        assert_eq!(
            compiled.text_field_byte_at(&key, cluster_start + (cluster_end - cluster_start) * 0.51),
            Some(14)
        );
        assert_eq!(
            compiled.text_field_byte_at(&key, paint.rect.x + 1.0),
            Some(geometry.visible.start),
            "leading padding clamps to the painted projection start"
        );
        assert_eq!(
            compiled.text_field_byte_at(&key, paint.rect.right() - 1.0),
            Some(geometry.visible.end),
            "the trailing swatch is still an end-caret target"
        );
        assert_eq!(compiled.text_field_byte_at(&key, f32::NAN), None);
        assert_eq!(
            compiled.text_field_byte_at(&UiKey::new("stale-field"), geometry.text_x),
            None
        );
    }

    #[test]
    fn text_field_pointer_is_bounded_to_the_clipped_visible_projection() {
        let text = format!("{}e\u{301}👩‍💻末", "long-prefix-".repeat(2_000));
        let input = crate::native_text_input::TextInputState::new(text.clone());
        let control = Control::new(
            TextFieldSpec {
                label: "Fallback fonts".to_string(),
                placeholder: None,
                secret: false,
                visual_value: None,
                input: Some(input.projection()),
                swatch: None,
            },
            ActionId::new("set/fonts"),
        )
        .value(SemanticValue::Text(text.clone()))
        .state(ControlState {
            focused: true,
            ..ControlState::default()
        });
        let compiled = UiTree::new(UiNode::new("fonts", UiContent::TextField(control)))
            .compile(LogicalRect::new(0.0, 0.0, 170.0, 40.0))
            .unwrap();
        let key = UiKey::new("fonts");
        let paint = compiled.paint.iter().find(|node| node.key == key).unwrap();
        let UiContent::TextField(control) = &paint.content else {
            unreachable!();
        };
        let geometry = text_field_geometry(
            control,
            paint.rect,
            native_type_px(crate::type_scale::TypeStep::Secondary).get(),
        );
        assert!(
            geometry.visible.start > 0,
            "long prefix is horizontally clipped"
        );
        assert!(geometry.visible.len() <= TEXT_FIELD_GEOMETRY_BYTES);
        let left = compiled.text_field_byte_at(&key, geometry.text_x).unwrap();
        let right = compiled
            .text_field_byte_at(&key, geometry.text_right)
            .unwrap();
        assert_eq!(left, geometry.visible.start);
        assert_eq!(right, geometry.visible.end);
        assert!(text.is_char_boundary(left) && text.is_char_boundary(right));
        assert!(text.grapheme_indices().any(|(byte, _)| byte == left));
        assert!(right == text.len() || text.grapheme_indices().any(|(byte, _)| byte == right));
    }

    #[test]
    fn slider_pointer_value_uses_the_painted_track_and_authored_step() {
        let slider = Control::new(
            SliderSpec {
                label: "Font size".to_string(),
                step: 2.0,
                display_value: "14".to_string(),
            },
            ActionId::new("set/font-size"),
        )
        .value(SemanticValue::Number {
            value: 14.0,
            minimum: 6.0,
            maximum: 32.0,
        });
        let compiled = UiTree::new(UiNode::new("font-size", UiContent::Slider(slider)))
            .compile(LogicalRect::new(20.0, 10.0, 280.0, 40.0))
            .unwrap();
        let key = UiKey::new("font-size");
        let paint = compiled
            .paint
            .iter()
            .find(|paint| paint.key == key)
            .unwrap();
        let geometry = slider_geometry(paint.rect);
        let x = geometry.track_x + (geometry.track_right - geometry.track_x) * 0.5;
        assert_eq!(compiled.slider_value_at(&key, x), Some(20.0));
        assert_eq!(
            compiled.slider_value_at(&key, geometry.track_x - 1.0),
            None,
            "the numeric readout focuses the slider without inventing a value"
        );
    }

    #[test]
    fn renderer_native_button_icons_emit_no_font_dependent_text() {
        use crate::widget::DrawPrim;

        for icon in [
            ButtonIcon::Back,
            ButtonIcon::Forward,
            ButtonIcon::Copy,
            ButtonIcon::External,
            ButtonIcon::Anchor,
            ButtonIcon::ChevronDown,
            ButtonIcon::Home,
            ButtonIcon::Modified,
            ButtonIcon::Appearance,
            ButtonIcon::Text,
            ButtonIcon::Cursor,
            ButtonIcon::Window,
            ButtonIcon::Keyboard,
            ButtonIcon::Terminal,
            ButtonIcon::Performance,
            ButtonIcon::Security,
            ButtonIcon::Diagnostics,
            ButtonIcon::Update,
            ButtonIcon::Info,
        ] {
            let mut prims = Vec::new();
            paint_button_icon(
                &mut prims,
                LogicalRect::new(0.0, 0.0, 44.0, 32.0),
                icon,
                [180, 210, 255],
            );
            assert!(!prims.is_empty(), "{icon:?}");
            assert!(
                prims
                    .iter()
                    .all(|prim| !matches!(prim, DrawPrim::Text { .. })),
                "{icon:?} must remain code-native"
            );
        }
    }

    #[test]
    fn disclosure_chevron_is_a_connected_downward_silhouette() {
        use crate::widget::DrawPrim;

        let mut prims = Vec::new();
        paint_button_icon(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 44.0, 32.0),
            ButtonIcon::ChevronDown,
            [180, 210, 255],
        );
        let rows = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .map(|(_, _, width, _)| *width)
                .collect::<Vec<_>>(),
            [10.0, 6.0, 2.0]
        );
        assert!(
            rows.windows(2)
                .all(|pair| pair[0].1 + pair[0].3 == pair[1].1)
        );
        assert!(rows.iter().all(|(_, _, _, height)| *height == 2.0));
    }

    #[test]
    fn history_arrows_are_connected_directional_silhouettes() {
        use crate::widget::DrawPrim;

        for icon in [ButtonIcon::Back, ButtonIcon::Forward] {
            let mut prims = Vec::new();
            paint_button_icon(
                &mut prims,
                LogicalRect::new(0.0, 0.0, 44.0, 32.0),
                icon,
                [180, 210, 255],
            );
            let segments = prims
                .iter()
                .filter(|prim| matches!(prim, DrawPrim::Line { .. }))
                .count();
            assert_eq!(segments, 3, "{icon:?} keeps one stem and two head segments");
            assert!(
                prims
                    .iter()
                    .all(|prim| matches!(prim, DrawPrim::Line { .. })),
                "{icon:?} must not regress to font or staircase geometry"
            );
        }
    }

    #[test]
    fn preserved_markdown_source_soft_wraps_at_words_before_hard_chunks() {
        assert_eq!(
            wrap_markdown_text("alpha beta gamma", 10, 4, true),
            ["alpha beta", "gamma"]
        );
        assert_eq!(
            wrap_markdown_text("supercalifragilistic", 5, 8, true),
            ["super", "calif", "ragil", "istic"]
        );
        assert_eq!(
            wrap_markdown_text("  let value = exact;", 14, 4, true),
            ["  let value =", "exact;"]
        );
    }

    #[test]
    fn code_text_uses_the_terminal_monospace_face() {
        use crate::widget::{DrawPrim as Prim, TextFace};

        let compiled = UiTree::new(UiNode::new(
            "code",
            UiContent::Text(TextSpec {
                text: "$ cargo test".to_string(),
                role: SemanticRole::Text,
                style: StyleRef::Code,
            }),
        ))
        .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
        .unwrap();
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(prims.iter().any(|primitive| matches!(
            primitive,
            Prim::Text { s, face, .. } if s == "$ cargo test" && *face == TextFace::Mono
        )));
    }

    #[test]
    fn text_viewport_geometry_uses_the_active_mono_raster_advance() {
        use crate::widget::TextWeight;

        let measured = crate::tray_raster::measure_text("M", 13.0, TextWeight::Regular);
        assert!(measured.is_finite() && measured > 0.0);
        let geometry = text_viewport_geometry(LogicalRect::new(0.0, 0.0, 760.0, 420.0));
        assert_eq!(geometry.cell_w, measured);
    }

    #[test]
    fn editor_capacity_uses_real_compact_landscape_desktop_and_scaled_geometry() {
        let phone = LogicalRect::new(0.0, 0.0, 320.0, 568.0);
        let landscape = LogicalRect::new(0.0, 0.0, 800.0, 320.0);
        let desktop = LogicalRect::new(0.0, 0.0, 1_200.0, 800.0);
        let phone_lines = editor_visible_line_capacity_at_scale(phone, 1.0);
        let landscape_lines = editor_visible_line_capacity_at_scale(landscape, 1.0);
        let desktop_lines = editor_visible_line_capacity_at_scale(desktop, 1.0);
        let scaled_phone_lines = editor_visible_line_capacity_at_scale(phone, 2.0);

        assert_eq!(phone_lines, 21);
        assert_eq!(landscape_lines, 11);
        assert_eq!(desktop_lines, 34);
        // At 2×, the compact command bar uses two complete 48pt rows and the
        // editor uses 40pt lines. The exact shared shell/viewport geometry leaves
        // nine materialized rows plus one reveal-ahead row.
        assert_eq!(scaled_phone_lines, 10);
        assert!(scaled_phone_lines < phone_lines);
        assert!(desktop_lines > landscape_lines);
    }

    #[test]
    fn text_viewport_pointer_mapping_shares_line_gutter_tab_and_utf8_geometry() {
        use crate::native_editor::{EditorViewportLine, EditorViewportProjection};
        use crate::widget::DrawPrim as Prim;

        let spec = TextViewportSpec {
            label: "unicode.txt".to_string(),
            document_key: "document:1@1".to_string(),
            selectable: true,
            projection: Some(EditorViewportProjection {
                first_line: 0,
                total_lines: 1,
                lines: vec![EditorViewportLine {
                    number: 0,
                    source: 100..105,
                    column_start: 0,
                    text: "α\tβ".to_string(),
                    selections: Vec::new(),
                    carets: vec![(0, true)],
                    syntax: Vec::new(),
                    diagnostics: Vec::new(),
                }],
            }),
            preedit: String::new(),
            status: None,
            semantic_status: None,
            minibuffer: None,
            cursor_label: None,
            dirty: false,
            saving: false,
            focused: true,
            action: Some(ActionId::new("editor/focus-buffer")),
        };
        let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
        let geometry = text_viewport_geometry(rect);
        let y = geometry.body_y + geometry.line_h * 0.5;

        assert_eq!(text_viewport_byte_at(&spec, rect, 10.0, y), None);
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x, 10.0),
            None
        );
        assert_eq!(text_viewport_byte_at(&spec, rect, rect.right(), y), None);
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x, y),
            Some(100)
        );
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x + 0.6 * geometry.cell_w, y),
            Some(102),
            "right half of α maps after its two-byte UTF-8 scalar"
        );
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x + 1.2 * geometry.cell_w, y),
            Some(102),
            "left side of the tab maps before it"
        );
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x + 3.0 * geometry.cell_w, y),
            Some(103),
            "right side of the four-column tab maps after it"
        );
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x + 4.1 * geometry.cell_w, y),
            Some(103)
        );
        assert_eq!(
            text_viewport_byte_at(&spec, rect, geometry.text_x + 4.8 * geometry.cell_w, y),
            Some(105)
        );
        assert_eq!(
            text_viewport_byte_at(
                &spec,
                rect,
                geometry.text_x,
                geometry.body_y + geometry.line_h + 1.0,
            ),
            None,
            "an unmaterialized row is not addressable"
        );
        for byte in [100, 102, 103, 105] {
            assert!(
                spec.projection.as_ref().unwrap().lines[0]
                    .text
                    .is_char_boundary(byte - 100)
            );
        }

        let unicode_text = "e\u{301}中👩‍💻".to_string();
        let unicode_start = 300usize;
        let after_accent = unicode_start + "e\u{301}".len();
        let after_cjk = after_accent + "中".len();
        let unicode_end = unicode_start + unicode_text.len();
        let unicode = TextViewportSpec {
            label: "graphemes.txt".to_string(),
            document_key: "document:3@1".to_string(),
            selectable: true,
            projection: Some(EditorViewportProjection {
                first_line: 0,
                total_lines: 1,
                lines: vec![EditorViewportLine {
                    number: 0,
                    source: unicode_start..unicode_end,
                    column_start: 0,
                    text: unicode_text,
                    selections: Vec::new(),
                    carets: vec![(0, true)],
                    syntax: Vec::new(),
                    diagnostics: Vec::new(),
                }],
            }),
            preedit: String::new(),
            status: None,
            semantic_status: None,
            minibuffer: None,
            cursor_label: None,
            dirty: false,
            saving: false,
            focused: true,
            action: Some(ActionId::new("editor/focus-buffer")),
        };
        for tenth_cell in 0..=50 {
            let mapped = text_viewport_byte_at(
                &unicode,
                rect,
                geometry.text_x + tenth_cell as f32 * 0.1 * geometry.cell_w,
                y,
            )
            .unwrap();
            assert!(
                [unicode_start, after_accent, after_cjk, unicode_end].contains(&mapped),
                "pointer mapping returned a byte inside a grapheme: {mapped}"
            );
        }
        assert_eq!(
            text_viewport_byte_at(&unicode, rect, geometry.text_x + 0.4 * geometry.cell_w, y,),
            Some(unicode_start)
        );
        assert_eq!(
            text_viewport_byte_at(&unicode, rect, geometry.text_x + 0.6 * geometry.cell_w, y,),
            Some(after_accent)
        );
        assert_eq!(
            text_viewport_byte_at(&unicode, rect, geometry.text_x + 1.9 * geometry.cell_w, y,),
            Some(after_accent)
        );
        assert_eq!(
            text_viewport_byte_at(&unicode, rect, geometry.text_x + 2.1 * geometry.cell_w, y,),
            Some(after_cjk)
        );
        assert_eq!(
            text_viewport_byte_at(&unicode, rect, geometry.text_x + 3.9 * geometry.cell_w, y,),
            Some(after_cjk)
        );
        assert_eq!(
            text_viewport_byte_at(&unicode, rect, geometry.text_x + 4.1 * geometry.cell_w, y,),
            Some(unicode_end)
        );

        let shifted = TextViewportSpec {
            label: "shifted.txt".to_string(),
            document_key: "document:2@1".to_string(),
            selectable: true,
            projection: Some(EditorViewportProjection {
                first_line: 0,
                total_lines: 1,
                lines: vec![EditorViewportLine {
                    number: 0,
                    source: 200..202,
                    column_start: 3,
                    text: "\tX".to_string(),
                    selections: Vec::new(),
                    carets: vec![(1, true)],
                    syntax: Vec::new(),
                    diagnostics: Vec::new(),
                }],
            }),
            preedit: String::new(),
            status: None,
            semantic_status: None,
            minibuffer: None,
            cursor_label: None,
            dirty: false,
            saving: false,
            focused: true,
            action: Some(ActionId::new("editor/focus-buffer")),
        };
        assert_eq!(
            text_viewport_byte_at(&shifted, rect, geometry.text_x + 0.4 * geometry.cell_w, y,),
            Some(200)
        );
        assert_eq!(
            text_viewport_byte_at(&shifted, rect, geometry.text_x + 0.6 * geometry.cell_w, y,),
            Some(201),
            "a tab beginning at global column three occupies exactly one painted cell"
        );
        let shifted_tree = UiTree::new(UiNode::new(
            "editor/shifted",
            UiContent::TextViewport(shifted),
        ));
        let shifted_prims = shifted_tree
            .compile(rect)
            .unwrap()
            .tray(aterm_render::Theme::default(), 13.0)
            .prims;
        let expected_caret_x = geometry.text_x + geometry.cell_w;
        assert!(
            shifted_prims.iter().all(|primitive| {
                !matches!(
                    primitive,
                    Prim::Text { s, .. } if s.contains('\t')
                )
            }),
            "the paint IR must expand tabs before they reach the font rasterizer"
        );
        assert!(
            shifted_prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    Prim::Text { x, s, .. }
                        if s == "X" && (*x - expected_caret_x).abs() < 0.01
                )
            }),
            "text after a shifted tab starts at the exact canonical cell"
        );
        assert!(
            shifted_prims.iter().any(|primitive| {
                matches!(
                    primitive,
                    Prim::Stroke { x, y: stroke_y, w, h, .. }
                        if (*x - expected_caret_x).abs() < 0.01
                            && (*stroke_y - (geometry.body_y + 2.0)).abs() < 0.01
                            && (*w - 1.0).abs() < 0.01
                            && (*h - (geometry.line_h - 4.0)).abs() < 0.01
                )
            }),
            "caret paint uses the same shifted tab phase as pointer mapping"
        );
    }

    #[test]
    fn host_focus_pass_updates_every_standard_control_before_compile() {
        let compiled = form()
            .apply_focus(Some(&UiKey::new("reset")))
            .compile(LogicalRect::new(0.0, 0.0, 320.0, 200.0))
            .unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("reset"))
                .and_then(|node| node.state)
                .is_some_and(|state| state.focused)
        );
        assert!(
            compiled
                .semantic(&UiKey::new("save"))
                .and_then(|node| node.state)
                .is_some_and(|state| !state.focused)
        );
    }

    #[test]
    fn disabled_control_is_semantic_and_painted_but_not_focusable_or_hittable() {
        let disabled = Control::new(ButtonSpec::new("Install"), ActionId::new("install")).state(
            ControlState {
                enabled: false,
                ..ControlState::default()
            },
        );
        let tree = UiTree::new(UiNode::new("install", UiContent::Button(disabled)));
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 100.0, 40.0))
            .unwrap();
        assert_eq!(compiled.paint.len(), 1);
        assert_eq!(compiled.semantics.len(), 1);
        assert!(compiled.hits.is_empty());
        assert!(compiled.focus_order.is_empty());
    }

    #[test]
    fn default_action_is_the_first_enabled_primary_button_in_authoring_order() {
        // A disabled Primary earlier in focus order is skipped, a Secondary is
        // never the default, and the enabled Primary wins even when it is not
        // first; a page with no enabled Primary button has no default at all.
        let disabled_primary = Control::new(ButtonSpec::new("Stale"), ActionId::new("stale"))
            .state(ControlState {
                enabled: false,
                ..ControlState::default()
            })
            .style(StyleRef::Primary);
        let secondary = Control::new(ButtonSpec::new("Check"), ActionId::new("check"))
            .style(StyleRef::Secondary);
        let primary = Control::new(ButtonSpec::new("Install"), ActionId::new("install"))
            .style(StyleRef::Primary);
        let row = |key: &str, control| {
            UiNode::new(key, UiContent::Button(control))
                .layout(Layout::default().height(Length::Fixed(40.0)))
        };
        let tree = UiTree::new(
            UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                .layout(Layout::column())
                .children(vec![
                    row("stale", disabled_primary),
                    row("check", secondary),
                    row("install", primary),
                ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 200.0, 120.0))
            .unwrap();
        let (key, action) = compiled.default_action.clone().expect("enabled Primary");
        assert_eq!(key, UiKey::new("install"));
        assert_eq!(action, ActionId::new("install"));

        // The default survives viewport clipping: a route's default is
        // semantic, so Return must fire it even scrolled out of view — while
        // every clipped observer (paint/semantics/focus) omits the button.
        let clipped = tree
            .compile(LogicalRect::new(0.0, 0.0, 200.0, 1.0))
            .unwrap();
        assert!(
            clipped
                .paint
                .iter()
                .all(|node| node.key != UiKey::new("install"))
        );
        assert_eq!(
            clipped.default_action,
            Some((UiKey::new("install"), ActionId::new("install")))
        );

        let no_primary = UiTree::new(
            UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                .layout(Layout::column())
                .children(vec![button("check", "Check")]),
        );
        let compiled = no_primary
            .compile(LogicalRect::new(0.0, 0.0, 200.0, 120.0))
            .unwrap();
        assert_eq!(compiled.default_action, None);
    }

    #[test]
    fn clipped_geometry_is_shared_by_hit_and_semantics() {
        let tree = UiTree::new(
            UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                .layout(Layout::column().clipped())
                .children(vec![
                    button("wide", "Wide").layout(
                        Layout::default()
                            .width(Length::Fixed(400.0))
                            .height(Length::Fixed(40.0)),
                    ),
                ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(10.0, 20.0, 100.0, 60.0))
            .unwrap();
        let semantic = compiled.semantic(&UiKey::new("wide")).unwrap();
        let hit = &compiled.hits[0];
        assert_eq!(semantic.rect, hit.rect);
        assert_eq!(semantic.rect, LogicalRect::new(10.0, 20.0, 100.0, 40.0));
        assert!(compiled.hit_test(109.9, 30.0).is_some());
        assert!(compiled.hit_test(110.0, 30.0).is_none());
    }

    #[test]
    fn duplicate_keys_fail_closed_before_dispatch() {
        let tree = UiTree::new(
            UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                .layout(Layout::column())
                .children(vec![button("same", "One"), button("same", "Two")]),
        );
        assert_eq!(
            tree.compile(LogicalRect::new(0.0, 0.0, 200.0, 100.0)),
            Err(CompileError::DuplicateKey(UiKey::new("same")))
        );
    }

    #[test]
    fn custom_nodes_require_a_named_audit_adapter() {
        let tree = UiTree::new(UiNode::new(
            "custom",
            UiContent::Custom(AuditedCustomNode {
                audit_id: "",
                role: SemanticRole::Button,
                label: "Novel".to_string(),
                value: None,
                action: Some(ActionId::new("novel")),
                focusable: true,
            }),
        ));
        assert_eq!(
            tree.compile(LogicalRect::new(0.0, 0.0, 100.0, 40.0)),
            Err(CompileError::CustomWithoutAudit(UiKey::new("custom")))
        );
    }

    #[test]
    fn paint_only_copy_renders_without_duplicate_semantics() {
        let complete = "On, currently silent while this window is unfocused.";
        let tree = UiTree::new(
            UiNode::new(
                "status",
                UiContent::Group(GroupSpec {
                    label: Some(complete.to_string()),
                    role: SemanticRole::Status,
                    style: StyleRef::Quiet,
                }),
            )
            .children(vec![
                UiNode::new(
                    "status/visual",
                    UiContent::Text(TextSpec {
                        text: "Window unfocused".to_string(),
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .paint_only(),
            ]),
        );

        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
            .unwrap();
        assert_eq!(compiled.paint.len(), 2);
        assert!(
            compiled
                .paint
                .iter()
                .any(|node| node.key == UiKey::new("status/visual"))
        );
        assert_eq!(compiled.semantics.len(), 1);
        assert_eq!(
            compiled.semantic(&UiKey::new("status")).unwrap().label,
            complete
        );
        assert!(compiled.semantic(&UiKey::new("status/visual")).is_none());
        compiled.validate_parity().unwrap();
    }

    #[test]
    fn paint_only_copy_participates_in_the_retained_raster_fingerprint() {
        let compile = |visual: &str| {
            UiTree::new(
                UiNode::new(
                    "status",
                    UiContent::Group(GroupSpec {
                        label: Some("Complete status that never changes".to_string()),
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .children(vec![
                    UiNode::new(
                        "status/visual",
                        UiContent::Text(TextSpec {
                            text: visual.to_string(),
                            role: SemanticRole::Status,
                            style: StyleRef::Quiet,
                        }),
                    )
                    .paint_only(),
                ]),
            )
            .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
            .unwrap()
        };
        let first = compile("Visual A");
        let second = compile("Visual B");

        assert_eq!(first.semantics, second.semantics);
        assert_ne!(first.paint, second.paint);
        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn paint_only_controls_fail_closed() {
        let tree = UiTree::new(button("hidden-action", "Hidden action").paint_only());
        assert_eq!(
            tree.compile(LogicalRect::new(0.0, 0.0, 120.0, 40.0)),
            Err(CompileError::PaintOnlyAction(UiKey::new("hidden-action")))
        );
    }

    #[test]
    fn fixed_and_fill_layout_is_deterministic() {
        let tree = UiTree::new(
            UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                .layout(Layout::row().gap(4.0))
                .children(vec![
                    button("nav", "Nav").layout(
                        Layout::default()
                            .width(Length::Fixed(80.0))
                            .height(Length::Fill),
                    ),
                    button("content", "Content")
                        .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                ]),
        );
        let compiled = tree
            .compile(LogicalRect::new(0.0, 0.0, 200.0, 100.0))
            .unwrap();
        assert_eq!(
            compiled.semantic(&UiKey::new("nav")).unwrap().rect,
            LogicalRect::new(0.0, 0.0, 80.0, 100.0)
        );
        assert_eq!(
            compiled.semantic(&UiKey::new("content")).unwrap().rect,
            LogicalRect::new(84.0, 0.0, 116.0, 100.0)
        );
    }

    /// The Tab Color wheel's pointer mapping and marker placement are exact
    /// inverses on the SAME polar convention the raster paints (clockwise
    /// turns from 12 o'clock, radius = saturation, full value): a pick at the
    /// marker of any committed color re-picks that color, the disk centre is
    /// white (saturation 0), and a point outside the disk is a miss — so a
    /// click can never commit a color the pixels did not show.
    #[test]
    fn color_wheel_pick_and_marker_round_trip() {
        let rect = LogicalRect::new(10.0, 20.0, 240.0, 240.0);
        let geometry = color_wheel_geometry(rect);
        assert_eq!(
            color_wheel_rgb_at(rect, geometry.cx, geometry.cy),
            Some([255, 255, 255]),
            "disk centre is the zero-saturation white axis"
        );
        assert_eq!(
            color_wheel_rgb_at(rect, geometry.cx + geometry.r + 2.0, geometry.cy),
            None,
            "outside the disk is a miss, not a pick"
        );
        // 12 o'clock at full radius is pure red (hue 0, saturation 1).
        let top = color_wheel_rgb_at(rect, geometry.cx, geometry.cy - geometry.r + 0.5).unwrap();
        assert!(
            top[0] > 250 && top[1] < 8 && top[2] < 8,
            "top is red: {top:?}"
        );
        for rgb in [[255, 42, 165], [255, 233, 74], [18, 171, 52], [64, 64, 255]] {
            let (mx, my) = color_wheel_marker_at(rect, rgb);
            let picked = color_wheel_rgb_at(rect, mx, my).expect("marker is on the disk");
            // The disk projects onto full value, so compare against the same
            // projection of the committed color (hue+saturation preserved).
            let (hue, saturation, _) = crate::widget::rgb_to_hsv(rgb);
            let projected = crate::widget::hsv_to_rgb(hue, saturation, 1.0);
            for channel in 0..3 {
                assert!(
                    (i32::from(picked[channel]) - i32::from(projected[channel])).abs() <= 3,
                    "marker roundtrip drifted for {rgb:?}: picked {picked:?} vs {projected:?}"
                );
            }
        }
    }
}
