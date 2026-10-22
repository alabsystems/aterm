// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed semantic UI tree for first-party tab applications.
//!
//! A control is described once as [`UiContent`].  [`UiTree::compile`] derives the
//! paint list, pointer hit index, semantic nodes, focus order, and controller
//! serialization from that one description.  Apps therefore cannot independently
//! author five subtly different versions of a switch or button.

use std::fmt;
use std::sync::Arc;

use aterm_grapheme::GraphemeClusters;
use aterm_hash::FxHashSet;

use crate::type_scale::TypeStep;

/// Stable logical identity of a semantic node.  Keys survive layout, filtering,
/// and route changes; vector positions are deliberately not identities.
///
/// The payload is an `Arc<str>`, not a `String`, because a key is authored ONCE
/// per node and then CLONED up to six times on the way through
/// [`UiTree::compile`] (duplicate-key set, paint node, semantic node, that
/// node's parent link, focus order, hit region) plus three more per node in the
/// accessibility projection — every one of those was a malloc + memcpy of the
/// key text on every native frame.  `Arc<str>` makes each of them a refcount
/// bump while `as_str`, `Debug`, `Ord`, and `Hash` all still forward to `str`,
/// so key ordering and hashing are unchanged.  `Arc` (not `Rc`) keeps `UiKey`
/// — and therefore the retained `CompiledUi` — `Send`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct UiKey(Arc<str>);

impl UiKey {
    pub(crate) fn new(value: impl Into<Arc<str>>) -> Self {
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
/// `Arc<str>` for the same reason [`UiKey`] is — an action is authored once and
/// cloned into the semantic node, the hit region, and the default-action slot.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ActionId(Arc<str>);

impl ActionId {
    pub(crate) fn new(value: impl Into<Arc<str>>) -> Self {
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

    /// The least rect holding both.
    pub(crate) fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
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

    /// This rect grown by `insets` on each side ([`Layout::outdent`]).
    fn outset(self, insets: Insets) -> Self {
        Self::new(
            self.x - insets.left,
            self.y - insets.top,
            self.width + insets.left + insets.right,
            self.height + insets.top + insets.bottom,
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
    /// How far a row or column's children are scrolled along its flow: the
    /// first starts this far before the content edge, so a clipped list
    /// anchored at its end shows its earliest child cut at the leading edge.
    pub(crate) offset: f32,
    /// How far this node reaches past its parent's content rect into the
    /// parent's padding, per side: the parent lays it out, then its rect
    /// grows by these insets, and where the parent clips its children to
    /// its content rect this node's clip grows the same way (never past the
    /// parent's own clip). A Settings page's own scrolling list reaches into
    /// the page's right gutter, where its thumb stands clear of the cards
    /// (`settings_scroll::seat_page_scroller`).
    pub(crate) outdent: Insets,
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
            offset: 0.0,
            outdent: Insets::default(),
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

    /// Scroll the children `offset` along the flow ([`Self::offset`]).
    pub(crate) fn scrolled(mut self, offset: f32) -> Self {
        self.offset = offset;
        self
    }

    /// Reach `outdent` past the parent's content rect ([`Self::outdent`]).
    pub(crate) fn outdent(mut self, outdent: Insets) -> Self {
        self.outdent = outdent;
        self
    }

    fn is_valid(self) -> bool {
        self.padding.is_valid()
            && self.outdent.is_valid()
            && self.gap.is_finite()
            && self.gap >= 0.0
            && self.offset.is_finite()
            && self.offset >= 0.0
            && length_valid(self.width)
            && length_valid(self.height)
    }
}

fn length_valid(length: Length) -> bool {
    match length {
        Length::Fill => true,
        Length::Fixed(v) => v.is_finite() && v >= 0.0,
        Length::Fraction(v) => v.is_finite() && (0.0..=1.0).contains(&v),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    Navigation,
    Status,
    /// A SCROLLING LIST of entries (Settings ▸ Messages' log, design ruling
    /// 264): a list with no pager, which a screen reader scrolls itself —
    /// the a11y model's `List` with its scroll actions, lowered to the same
    /// `ScrollLines` the wheel and the arrow keys send.
    List,
    /// A list of options one of which is chosen (Settings ▸ Themes' theme
    /// list): scrolls like [`Self::List`], and its rows are
    /// [`Self::ListBoxOption`]s.
    ListBox,
    /// One option of a [`Self::ListBox`]; `selected` marks the highlighted
    /// one. Pressing it chooses it.
    ListBoxOption,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ControlState {
    pub(crate) enabled: bool,
    pub(crate) focused: bool,
    pub(crate) focus_visible: bool,
    pub(crate) hovered: bool,
    pub(crate) pressed: bool,
    pub(crate) selected: bool,
    pub(crate) invalid: bool,
    pub(crate) busy: bool,
    /// A DISCLOSURE control's state (design ruling 262): `Some(true)` while
    /// what it discloses is open, `Some(false)` while it is closed, `None`
    /// for a control that discloses nothing. A screen reader hears it as
    /// expanded or collapsed — never as `selected` — and the painter draws the
    /// button flush, with its trailing chevron (`›` closed, `⌄` open).
    pub(crate) expanded: Option<bool>,
    /// The control works, but what it sets is idle right now (a sound row
    /// while Music effects is off, a Harness row while supervision is off, a
    /// setting with no effect here): it paints in quiet ink wherever it would
    /// paint the accent, says its value in quiet text, and stays operable.
    pub(crate) muted: bool,
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
            expanded: None,
            muted: false,
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
    /// A warning: its words in the danger colour. As a Heading, the Title
    /// step in bold.
    Danger,
    /// A WARNING HEADING AT THE COMPACT HEADING STEP: the Body step in bold
    /// (a [`Self::Plain`] heading's), in the danger colour — where a
    /// [`Self::Danger`] heading's Title step does not fit its column, so the
    /// warning keeps its colour as it gives up its size (Software Update's
    /// "Can't update" on a phone-width page at large text). Elsewhere it is
    /// [`Self::Danger`].
    DangerCompact,
    Navigation,
    Setting,
    Code,
    /// A FILTER CHIP (Settings ▸ Messages' severity and tag filters): a toggle
    /// whose state is a LUMINANCE change, not a hue — at rest the card's fill in
    /// a [`crate::native_appearance::NativeRoles::control_edge`] rim (3:1, the
    /// non-text floor for a control's boundary); selected, a solid accent fill
    /// inked in `on_accent` (4.5:1). Its label is centred in the slot its author
    /// measured for it.
    Chip,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Control<T> {
    pub(crate) spec: T,
    pub(crate) value: SemanticValue,
    pub(crate) state: ControlState,
    pub(crate) action: ActionId,
    pub(crate) style: StyleRef,
    /// The role assistive technology hears in place of the control kind's
    /// own: a button that is one option of a list box
    /// ([`SemanticRole::ListBoxOption`]). Paint and actions are the kind's.
    pub(crate) role: Option<SemanticRole>,
    /// THE PRESS GOES WHERE THE POINTER LOOKED ([`Self::look_first`]): a
    /// pointer press lands on this control only where the pointer moved onto
    /// it, or the person scrolled it there; where the layout moved it under a
    /// still pointer, the press chooses nothing and lights it, and the next
    /// press may take it. The same holds for any control the layout moves
    /// under a pointer resting on this one. Keyboard, assistive and `act`
    /// presses name the control and are never refused.
    pub(crate) look_first: bool,
}

impl<T> Control<T> {
    pub(crate) fn new(spec: T, action: ActionId) -> Self {
        Self {
            spec,
            value: SemanticValue::None,
            state: ControlState::default(),
            action,
            style: StyleRef::default(),
            role: None,
            look_first: false,
        }
    }

    /// Ask for a look before a pointer press ([`Self::look_first`]): for a
    /// control whose one press writes many things at once (SESSIONS'
    /// `Every session` row), where a press the layout moved under a still
    /// pointer would do more harm than a press that waits.
    pub(crate) fn look_first(mut self) -> Self {
        self.look_first = true;
        self
    }

    pub(crate) fn role(mut self, role: SemanticRole) -> Self {
        self.role = Some(role);
        self
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

    /// Whether the group paints a backdrop of its own (a card, a sidebar, a
    /// code canvas, the application surface), so all of its rect is drawn,
    /// not only its children ([`paint_compiled_node`]).
    pub(crate) fn paints_backdrop(&self) -> bool {
        matches!(
            self.role,
            SemanticRole::Application | SemanticRole::Navigation
        ) || matches!(
            self.style,
            StyleRef::Primary | StyleRef::Secondary | StyleRef::Code
        )
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
    /// An ATTENTION BADGE: a small dot in the danger role, painted at the end of
    /// a rail row (on the pictogram of an icon-only one). Paint only — a control
    /// that wears it states why in its semantic value, so the dot is never the
    /// only carrier of the fact (the Settings rail's Packages entry, 2026-09-22).
    pub(crate) badge: bool,
    /// A theme's COLOUR STRIP, painted along the bottom of a text button
    /// inside its own height: the first entry is the background the band is
    /// laid on, the rest (the 16 ANSI colours) sit on it as equal cells. Paint
    /// only, like the badge — the control's description says what it shows.
    pub(crate) swatches: Vec<[u8; 3]>,
    /// One COLOUR CHIP, the one a colour text field shows
    /// ([`TextFieldSpec::swatch`]), painted at the button's end ahead of its
    /// trailing icon. Paint only: the control's value names the colour.
    pub(crate) chip: Option<[u8; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonIcon {
    Back,
    Forward,
    Copy,
    ChevronDown,
    Home,
    Modified,
    Appearance,
    Text,
    Cursor,
    Window,
    Keyboard,
    Terminal,
    Security,
    Diagnostics,
    Update,
    Packages,
    Info,
    /// The mark on the saved option in a choice list.
    Check,
    /// A framed picture: Wallpaper.
    Image,
    /// A tab wearing its color: Tab Color.
    Swatch,
    /// A page with a folded corner: Manual's aterm.toml.
    Document,
    /// A robot's head: the Harness page (coding agents).
    Agents,
    /// A cursor with a fading trail behind it: the Cursor Styles page.
    Trail,
    /// A speaker: the Sound page.
    Sound,
    /// A set of palettes, four tinted squares: Settings ▸ Themes, whose row
    /// sits right under Appearance's ring while it shows.
    Themes,
    /// A four-pointed spark: Settings ▸ AI CLIs, whose row sits right under
    /// Harness's robot while it shows.
    Spark,
    /// A waste bin: move a file to the Trash (Settings ▸ Themes' own rows).
    Trash,
    /// An arrow turning back on itself: put a value back as it was
    /// (Settings ▸ Themes' colour rows).
    Reset,
    /// A cross: empty the field it sits in (Settings ▸ Themes' filter).
    Clear,
    /// A magnifying glass: open the search field (Settings' compact toolbar,
    /// where the bar is too narrow to hold a field that says "Search").
    Search,
}

impl ButtonSpec {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            visual_label: None,
            visual_icon: None,
            trailing_icon: None,
            description: None,
            badge: false,
            swatches: Vec::new(),
            chip: None,
        }
    }

    /// Paint a colour strip: `background`, with `cells` laid on it
    /// ([`Self::swatches`]).
    pub(crate) fn swatches(mut self, background: [u8; 3], cells: &[[u8; 3]]) -> Self {
        self.swatches = std::iter::once(background)
            .chain(cells.iter().copied())
            .collect();
        self
    }

    pub(crate) fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Wear the attention badge ([`Self::badge`]) or not.
    pub(crate) fn badge(mut self, on: bool) -> Self {
        self.badge = on;
        self
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
    /// What a reader says after the name ([`UiContent::description`]): a
    /// Settings row's note, as a switch's or a button's carries it.
    pub(crate) description: Option<String>,
    /// THE TRACK, where it is not the whole value domain: the span the bar
    /// draws and a drag maps over (Font size shows 8–36, the sizes people
    /// use), while the value's domain — what the keys, automation and a
    /// screen reader may set — stays the control's semantic minimum and
    /// maximum (Font size 6–200). `None`: the track is the domain. A value
    /// past the track's end fills the bar to that end.
    pub(crate) track: Option<(f64, f64)>,
}

impl SliderSpec {
    /// The span the bar draws and a drag maps over, for a value domain of
    /// `minimum..=maximum` ([`Self::track`]).
    pub(crate) fn track_span(&self, minimum: f64, maximum: f64) -> (f64, f64) {
        self.track.unwrap_or((minimum, maximum))
    }
}

/// A COLOUR WHEEL ([`UiContent::ColorWheel`]): the HSV disk at the colour's
/// brightness, with a marker at its hue and saturation. One control for any
/// colour a page picks (Settings' colour picker, every colour key alike).
///
/// The wheel draws the picker's own model, never one re-derived from the
/// saved 8-bit `#RRGGBB`: black has no hue, so a colour taken to brightness
/// 0 still marks the hue it was picked at, and raising the brightness again
/// gives that hue back. The control's value is the colour in words for
/// assistive technology and automation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColorWheelSpec {
    pub(crate) label: String,
    /// Hue (turns from 12 o'clock, clockwise), saturation (the radius) and
    /// brightness (the disk's), each in `0..=1`; `None` while no colour is
    /// known, which marks nothing on a full-brightness disk.
    pub(crate) hsv: Option<[f32; 3]>,
}

/// The `audit_id` naming the Top Settings rainbow hero banner — a
/// custom-paint node ([`crate::settings::paint_rainbow_banner`]): a purely
/// decorative, static composition (no action, not focusable) whose semantic
/// label simply names the scenery for accessibility.
pub(crate) const RAINBOW_BANNER_AUDIT: &str = "settings.home.rainbow";

/// Settings ▸ Messages' SEVERITY MARK (design ruling 262): a paint-only custom
/// node whose value is the wire's severity word (`error`, `warn`, `success`,
/// `info`). It draws the band's own icon for it at the row text's cap height —
/// ✕ in the danger red, ⚠ in the band's warn amber, ✓ in green, ℹ in the
/// muted secondary — and, for an Error or a Warning, a 3 pt rail down the
/// row's leading edge in the same hue. Decorative: the row's control says
/// the severity in words.
pub(crate) const MESSAGES_SEVERITY_AUDIT: &str = "settings.messages.severity";

/// Settings ▸ Messages' HAIRLINE between rows in the log's one container
/// (design ruling 262): a paint-only custom node, a 1 pt rule in the
/// separator role across its rect.
pub(crate) const MESSAGES_RULE_AUDIT: &str = "settings.messages.rule";

/// Settings ▸ Themes' row SLAB: a paint-only custom node whose value carries
/// one theme's background, foreground and four hues
/// ([`crate::settings_preview::theme_card::ShowcaseCard::slab_value`]). It
/// draws a 56×20 chip of that theme (40×20 on a narrow chooser) — its
/// background, "Aa" in its foreground and dots of its hues, as many as fit —
/// from panels and text alone. Decorative: the row's option
/// carries its name, appearance and contrast in words.
pub(crate) const THEME_SLAB_AUDIT: &str = "settings.themes.slab";

/// Settings ▸ Themes' palette SWATCH: a paint-only custom node whose value is
/// one `#rrggbb` colour (then `changed` for a slot the person tuned, which
/// wears an accent dot), drawn as a rounded chip with a hairline so a colour
/// equal to the page still shows its edge. The slot's button says which
/// colour it is in words.
pub(crate) const THEME_SWATCH_AUDIT: &str = "settings.themes.swatch";

/// A COLOUR CHIP after words that name a colour (Modified's "Default:
/// Automatic"): the chip a colour field and a colour button wear
/// ([`TextFieldSpec::swatch`], [`ButtonSpec::chip`]), drawn a gap after the
/// node's left edge and centred down it. Paint only; its value is the
/// `#RRGGBB`, and the words beside it name the colour for assistive
/// technology.
pub(crate) const COLOR_CHIP_AUDIT: &str = "settings.color.chip";

/// Settings ▸ Messages' SCROLL INDICATOR (design ruling 264): the log is one
/// scrolling list with no pager, and this paint-only custom node, laid over
/// the log's card, is the sighted reader's cue that it scrolls and where the
/// view is — a thin rounded thumb down the card's trailing edge. Its value is
/// `first,shown,total` in entries. Decorative: the list itself is the
/// screen reader's scrolling surface ([`SemanticRole::List`]).
pub(crate) const MESSAGES_SCROLL_AUDIT: &str = "settings.messages.scroll";

/// A Settings page's own SCROLL INDICATOR: [`MESSAGES_SCROLL_AUDIT`]'s node
/// for the page's list, its node reaching across the page's right gutter
/// (`settings_scroll::seat_page_scroller`), drawn in the gutter outside the
/// cards in the terminal's resting idiom ([`paint_page_scroll`]). Value and
/// semantics are [`MESSAGES_SCROLL_AUDIT`]'s.
pub(crate) const PAGE_SCROLL_AUDIT: &str = "settings.page.scroll";

/// A display face's NAME drawn in that face (Text & Fonts' Display faces
/// card): the label is the name, the value the face's id. A sample of the
/// letterforms the switch beside it turns on.
pub(crate) const DISPLAY_FACE_NAME_AUDIT: &str = "settings.text.display-face-name";

/// About's WORDMARK, its label "aterm", in the hero's type — the Display
/// step, bold — made smaller where its column cannot hold it whole
/// ([`wordmark_typography`]), so it is never cut ("at…" at 2× text on a
/// phone-width page, "ate…" on an 80 × 24 window). A Heading to assistive
/// technology.
pub(crate) const WORDMARK_AUDIT: &str = "settings.about.wordmark";

/// The size and face a [`WORDMARK_AUDIT`] node paints `label` in, in a node
/// `width` wide: the Display step in bold, scaled down to a hair under the
/// width where it is wider than that.
pub(crate) fn wordmark_typography(
    label: &str,
    width: f32,
) -> (crate::type_scale::StepPx, crate::widget::TextFace) {
    let face = crate::widget::TextFace::UiBold;
    let size = native_type_px(TypeStep::Display);
    let natural = crate::tray_raster::ui_text_width_for(face, label, size.get());
    let fit = if natural > width && natural > 0.0 && width > 0.0 {
        width * 0.99 / natural
    } else {
        1.0
    };
    (size.scaled(fit), face)
}

/// The [`crate::widget::TextFace`] and size a [`DISPLAY_FACE_NAME_AUDIT`]
/// node paints its label in: the face its value names, at the title size,
/// made smaller where a narrow column could not hold the name whole.
pub(crate) fn display_face_name_typography(
    spec: &AuditedCustomNode,
    width: f32,
) -> (crate::type_scale::StepPx, crate::widget::TextFace) {
    let face = spec
        .value
        .as_deref()
        .and_then(|id| {
            aterm_render::DISPLAY_FACES
                .iter()
                .position(|face| face.id == id)
        })
        .and_then(|index| u8::try_from(index).ok())
        .map_or(
            crate::widget::TextFace::Ui,
            crate::widget::TextFace::Display,
        );
    let size = native_type_px(TypeStep::Title);
    let natural = crate::tray_raster::ui_text_width_for(face, &spec.label, size.get());
    // A hair under the column, so the fitted name measures inside it.
    let fit = if natural > width && natural > 0.0 && width > 0.0 {
        width * 0.99 / natural
    } else {
        1.0
    };
    (size.scaled(fit), face)
}

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

/// The hue and saturation under `(x, y)` on the disk painted in `rect`, or
/// `None` outside it. Hue/saturation use EXACTLY the raster's per-pixel
/// convention (clockwise turns from 12 o'clock; radius = saturation), and do
/// not depend on the brightness the disk is painted at: a pick on a black
/// disk still picks a hue.
pub(crate) fn color_wheel_pick_at(rect: LogicalRect, x: f32, y: f32) -> Option<(f32, f32)> {
    let geometry = color_wheel_geometry(rect);
    let dx = x - geometry.cx;
    let dy = y - geometry.cy;
    let distance = (dx * dx + dy * dy).sqrt();
    if distance > geometry.r {
        return None;
    }
    let hue = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
    Some((hue, (distance / geometry.r).min(1.0)))
}

/// The marker position for `hue` and `saturation` on the wheel in `rect`:
/// the exact inverse of [`color_wheel_pick_at`]'s polar mapping.
pub(crate) fn color_wheel_marker_at(rect: LogicalRect, hue: f32, saturation: f32) -> (f32, f32) {
    let geometry = color_wheel_geometry(rect);
    let theta = hue * std::f32::consts::TAU;
    (
        geometry.cx + geometry.r * saturation * theta.sin(),
        geometry.cy - geometry.r * saturation * theta.cos(),
    )
}

/// How many decimals `value` is written with (`0.05` → 2, `1.0` → 0), at most 6.
fn authored_decimals(value: f64) -> i32 {
    (0..6)
        .find(|decimals| {
            let scaled = value * 10_f64.powi(*decimals);
            (scaled - scaled.round()).abs() <= 1e-9 * scaled.abs().max(1.0)
        })
        .unwrap_or(6)
}

/// `value` on the slider's grid: the nearest `step` from `minimum`, inside the
/// range, rounded to the decimals the range is written in. Binary floats make
/// `0.1 + 4 × 0.05` come out as `0.30000000000000004`; this keeps it `0.3`, so
/// what is saved and what the row reads are the number the slider shows.
pub(crate) fn snap_slider_value(value: f64, minimum: f64, maximum: f64, step: f64) -> f64 {
    let steps = ((value.clamp(minimum, maximum) - minimum) / step).round();
    let snapped = (minimum + steps * step).clamp(minimum, maximum);
    let scale = 10_f64.powi(authored_decimals(step).max(authored_decimals(minimum)));
    (snapped * scale).round() / scale
}

pub(crate) struct SliderGeometry {
    pub(crate) track_x: f32,
    pub(crate) track_right: f32,
    /// The track's centre line.
    pub(crate) track_y: f32,
    /// Where the readout may start and where it ends (set flush right).
    pub(crate) value_left: f32,
    pub(crate) value_right: f32,
    /// The band the readout's line is centred in: `(top, height)`.
    pub(crate) value_band: (f32, f32),
}

/// A slider readout's width in the mono face it paints in, never under a
/// mono cell per character: the readout is set flush right, so an
/// underestimate pushes its last characters past the control's edge
/// ("512 cell").
fn slider_readout_width(text: &str, px: f32) -> f32 {
    crate::tray_raster::ui_text_width_for(crate::widget::TextFace::Mono, text, px)
        .max(text.chars().count() as f32 * px * 0.62)
}

pub(crate) fn slider_geometry(rect: LogicalRect) -> SliderGeometry {
    // Room for a readout with its unit — "2000 ms", "512 cells", "300%",
    // about a dozen mono cells at the caption size the readout paints in,
    // so it grows with larger text — never more than half the control.
    let px = native_type_px(TypeStep::Caption).get();
    let reserve = px * 7.6;
    // A CONTROL TOO NARROW FOR THAT ROOM BESIDE A TRACK AS WIDE (a phone-width
    // column at large text) SETS ITS READOUT OVER THE TRACK, where it is tall
    // enough for both: the readout flush right on a line of its own, the
    // track the control's whole width under it. Decided by the control's size
    // and the text size, never by the value, so the track never changes
    // length as the value does. Beside a 67pt track at 2×, "5000 ms" painted
    // 95.5pt into a 57pt slot.
    let line = (px * 1.3).ceil();
    let stack = line + 4.0 + 6.0;
    if reserve > rect.width * 0.5 && rect.height >= stack + 12.0 {
        let top = rect.y + (rect.height - stack) / 2.0;
        return SliderGeometry {
            track_x: rect.x + 12.0,
            track_right: (rect.right() - 12.0).max(rect.x + 12.0),
            track_y: top + line + 4.0 + 3.0,
            value_left: rect.x + 12.0,
            value_right: rect.right() - 10.0,
            value_band: (top, line),
        };
    }
    let value_width = reserve.min(rect.width * 0.5).max(42.0);
    let track_right = (rect.right() - value_width - 12.0).max(rect.x + 12.0);
    SliderGeometry {
        track_x: rect.x + 12.0,
        track_right,
        track_y: rect.y + rect.height / 2.0,
        value_left: track_right + 6.0,
        value_right: rect.right() - 10.0,
        value_band: (rect.y, rect.height),
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
    /// Quiet words at the field's end, ahead of its swatch: a Settings field
    /// holding an unsaved draft says "Unsaved" there, in the field it is
    /// typed into, so the draft adds no line to its row. Paint only: the
    /// control's description says it.
    pub(crate) trailing_hint: Option<String>,
    /// What a reader says after the name ([`UiContent::description`]): a
    /// Settings row's note, why a value was refused, that a draft waits.
    pub(crate) description: Option<String>,
    /// A button laid over the field's end ([`TEXT_FIELD_END_BUTTON`] square,
    /// its own node: the filter's Clear): the text stops short of it.
    pub(crate) end_button: bool,
}

/// The room a text field's trailing hint takes, its gap included, and the
/// hint's size: the caption rung, in the UI face.
fn text_field_hint_width(control: &Control<TextFieldSpec>) -> f32 {
    control.spec.trailing_hint.as_deref().map_or(0.0, |hint| {
        crate::tray_raster::ui_text_width(hint, native_type_px(TypeStep::Caption).get())
            + TEXT_FIELD_SWATCH_GAP
    })
}

/// The side of the button a field keeps room for at its end
/// ([`TextFieldSpec::end_button`]), and the gap before it.
pub(crate) const TEXT_FIELD_END_BUTTON: f32 = 20.0;
pub(crate) const TEXT_FIELD_END_BUTTON_GAP: f32 = 4.0;

/// THE WORDS AN EMPTY FIELD SHOWS, FITTED TO ITS BOX: whole where they fit
/// `room` (at `px` in `face`); else without a trailing " (default)" — the
/// quiet tint already says the value is unset, and the semantic value keeps
/// the words — else a shorter form of them, else elided, never cut
/// mid-glyph.
fn fit_placeholder_to(
    placeholder: &str,
    room: f32,
    px: f32,
    face: crate::widget::TextFace,
) -> String {
    let fits = |text: &str| visual_text_width(text, px, face) <= room + 0.01;
    if fits(placeholder) {
        return placeholder.to_string();
    }
    // A path keeps its file's name, its components whole either side of
    // the ellipsis ([`path_forms`]), as a path field's value does.
    if reads_as_path(placeholder) {
        let start = path_name_start(placeholder);
        let name = &placeholder[start..];
        // The separator before the name, as the path spells it.
        let led = &placeholder[start.saturating_sub(1)..];
        return path_forms(placeholder)
            .into_iter()
            .map(|(form, _, _)| form)
            // Then the file's name alone after its ellipsis, then bare.
            .chain([format!("{TEXT_FIELD_ELLIPSIS}{led}"), name.to_string()])
            .find(|form| fits(form))
            .unwrap_or_else(|| elide_text_label(name, room, px, face));
    }
    // Its honest shorter forms, longest first: without its "(default)";
    // without the aside in parentheses it ends in ("none (HH:MM)" reads
    // "none"); its leading whole words with an ellipsis after them, saying
    // more follows ("ai-fleet.toml…"); and only where not even its first word
    // fits, elided inside it. A narrow field painted "ai-fleet.toml beside
    // ater…", a word cut where whole ones fit.
    let plain = placeholder
        .strip_suffix(" (default)")
        .unwrap_or(placeholder);
    let bare = plain
        .strip_suffix(')')
        .and_then(|open| open.rsplit_once(" ("))
        .map_or(plain, |(bare, _)| bare);
    if let Some(form) = [plain, bare]
        .into_iter()
        .find(|form| !form.is_empty() && fits(form))
    {
        return form.to_string();
    }
    let words = bare.split_whitespace().collect::<Vec<_>>();
    (1..words.len())
        .rev()
        .map(|kept| format!("{}\u{2026}", words[..kept].join(" ")))
        .find(|form| fits(form))
        .unwrap_or_else(|| elide_text_label(bare, room, px, face))
}

/// The room an empty text field `field_width` wide gives its placeholder:
/// its text box less its insets and, with `swatch`, its colour swatch.
pub(crate) fn field_placeholder_room(field_width: f32, swatch: bool) -> f32 {
    let swatch_space = if swatch {
        TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP
    } else {
        0.0
    };
    (field_width - 2.0 * TEXT_FIELD_HORIZONTAL_PADDING - swatch_space).max(0.0)
}

/// A placeholder's plain words: without a trailing " (default)" (the quiet
/// tint says it is unset) and the aside in parentheses it ends in.
pub(crate) fn plain_placeholder(placeholder: &str) -> String {
    let plain = placeholder
        .strip_suffix(" (default)")
        .unwrap_or(placeholder);
    plain
        .strip_suffix(')')
        .and_then(|open| open.rsplit_once(" ("))
        .map_or(plain, |(bare, _)| bare)
        .to_string()
}

/// A field's placeholder fitted to the text box its geometry gives it: the
/// painter's safety net ([`fit_placeholder_to`]). A page fits it when it is
/// authored ([`fit_native_field_placeholder`]), so the audit, which compares
/// this with the authored words, reports a placeholder nobody fitted.
fn fitted_placeholder(
    control: &Control<TextFieldSpec>,
    geometry: &TextFieldGeometry,
    px: f32,
) -> String {
    let face = if control.style == StyleRef::Code {
        crate::widget::TextFace::Mono
    } else {
        crate::widget::TextFace::Ui
    };
    let placeholder = control.spec.placeholder.as_deref().unwrap_or_default();
    let room = (geometry.text_right - geometry.text_x).max(0.0);
    // A placeholder its author set on lines is fitted line by line.
    placeholder
        .split('\n')
        .map(|line| fit_placeholder_to(line, room, px, face))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `placeholder` fitted, when it is authored, to a text field `field_width`
/// points wide that does (`swatch`) or does not draw a color swatch: the
/// text box [`text_field_geometry`] gives an empty field, at the field's
/// size in the UI face ([`fit_placeholder_to`]).
pub(crate) fn fit_native_field_placeholder(
    placeholder: &str,
    field_width: f32,
    swatch: bool,
) -> String {
    let swatch_space = if swatch {
        TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP
    } else {
        0.0
    };
    fit_placeholder_to(
        placeholder,
        (field_width - 2.0 * TEXT_FIELD_HORIZONTAL_PADDING - swatch_space).max(0.0),
        native_type_px(TypeStep::Secondary).get(),
        crate::widget::TextFace::Ui,
    )
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

/// Where a text button's (and a disclosure row's) label starts from its own
/// left edge. A list that sets rows of words beside such buttons starts its
/// words here too, so the column has one left edge.
pub(crate) const BUTTON_LABEL_INSET: f32 = 10.0;
pub(crate) const TEXT_FIELD_HORIZONTAL_PADDING: f32 = 10.0;
pub(crate) const TEXT_FIELD_SWATCH_SIZE: f32 = 22.0;
pub(crate) const TEXT_FIELD_SWATCH_GAP: f32 = 8.0;
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
    /// A field not being edited whose value is wider than it shows what it
    /// cuts as an ellipsis ([`TEXT_FIELD_ELLIPSIS`]): before the value's end
    /// for a path (`lead`, so the file's name shows), after its start
    /// otherwise (`trail`). `text_x` already stands after a leading one.
    pub(crate) ellipsis: TextFieldEllipsis,
}

/// Where a field not being edited marks the part of its value it cannot
/// show ([`TextFieldGeometry::ellipsis`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TextFieldEllipsis {
    /// The value shows whole, or the field is being edited.
    #[default]
    None,
    /// "…" then the value's end: a path, whose file name is its end.
    Lead,
    /// The value's start then "…".
    Trail,
    /// A path's head up to this byte, "…", then the shown range: whole
    /// components each side of the ellipsis ([`path_forms`]).
    Middle(usize),
}

/// The mark a field draws for the part of its value it cannot show.
pub(crate) const TEXT_FIELD_ELLIPSIS: &str = "…";

/// A PATH'S SHORTER FORMS, longest first — one rule wherever a path must
/// give way (a CLI's detail line, a path field, a path placeholder): its
/// head and its tail around an ellipsis, whole components each side, the
/// tail never under two components (the folder and the file) where it has
/// them (`/Users/…/bin/claude`); then the tail alone (`…/bin/claude`). Each
/// form is `(painted, head_end, tail_start)`: the painted words are
/// `path[..head_end]`, the ellipsis, then `path[tail_start..]`. A Windows
/// path's components end at either separator ([`path_separators`]).
pub(crate) fn path_forms(path: &str) -> Vec<(String, usize, usize)> {
    let slashes: Vec<usize> = path
        .match_indices(path_separators(path))
        .map(|(at, _)| at)
        .collect();
    let parts = slashes.len() + 1;
    let mut forms = Vec::new();
    let least = 2.min(parts.saturating_sub(1)).max(1);
    for tail in (least..parts).rev() {
        // The tail's `parts - tail` components before it end at this slash.
        let tail_start = slashes[parts - tail - 1];
        for head in (1..parts - tail).rev() {
            let head_end = slashes[head - 1] + 1;
            forms.push((
                format!(
                    "{}{TEXT_FIELD_ELLIPSIS}{}",
                    &path[..head_end],
                    &path[tail_start..]
                ),
                head_end,
                tail_start,
            ));
        }
        forms.push((
            format!("{TEXT_FIELD_ELLIPSIS}{}", &path[tail_start..]),
            0,
            tail_start,
        ));
    }
    forms
}

/// Whether a field's value reads as a filesystem path, whose end (the file's
/// name) says more than its start: `/…`, `~/…`, `C:\…`, `\\server\…`.
pub(crate) fn reads_as_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.starts_with('/')
        || text.starts_with("~/")
        || text.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

/// What ends a component of `path`: `/`, and on a Windows path (`C:\…`,
/// `\\server\…`) `\` too. A Unix file's name may hold a `\`, so only a path
/// that reads as a Windows one breaks there.
pub(crate) fn path_separators(path: &str) -> &'static [char] {
    if path.starts_with('/') || path.starts_with("~/") {
        &['/']
    } else {
        &['/', '\\']
    }
}

/// Where the last component of `path` (its file's name) starts, after its
/// last separator ([`path_separators`]); `0` where it has none.
pub(crate) fn path_name_start(path: &str) -> usize {
    path.rfind(path_separators(path)).map_or(0, |at| at + 1)
}

/// How wide a field with no colour chip must be to show `words` whole.
pub(crate) fn text_field_width_for_text(words: &str) -> f32 {
    let size = native_type_px(TypeStep::Secondary).get();
    crate::tray_raster::ui_text_width(words, size) + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING
}

/// The width a text field needs to show `words` whole, its insets and, with
/// `swatch`, its colour swatch included: the width the fit audit holds it to.
/// A row that would squeeze the field narrower lays its neighbours out on
/// another line instead.
pub(crate) fn text_field_width_for(words: &str, swatch: bool) -> f32 {
    let size = native_type_px(TypeStep::Secondary).get();
    let swatch = if swatch {
        TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP
    } else {
        0.0
    };
    crate::tray_raster::ui_text_width(words, size) + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING + swatch
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
    let button_space = if control.spec.end_button {
        TEXT_FIELD_END_BUTTON + TEXT_FIELD_END_BUTTON_GAP
    } else {
        0.0
    };
    let text_right = (rect.right()
        - TEXT_FIELD_HORIZONTAL_PADDING
        - swatch_space
        - button_space
        - text_field_hint_width(control))
    .max(text_x);
    let available = (text_right - text_x).max(0.0);
    let text = text_field_visual_text(control);
    if text.is_empty() || available <= 0.0 {
        return TextFieldGeometry {
            text_x,
            text_right,
            visible: 0..0,
            ellipsis: TextFieldEllipsis::None,
        };
    }

    let focused = control.state.focused && control.spec.input.is_some();
    // NOT BEING EDITED AND TOO WIDE: the field says what it cuts with an
    // ellipsis, and a path keeps its end (the file's name) rather than its
    // start. Editing scrolls the value under the caret instead.
    if !focused && crate::tray_raster::ui_text_width(text, text_px) > available {
        let mark = crate::tray_raster::ui_text_width(TEXT_FIELD_ELLIPSIS, text_px);
        let room = (available - mark).max(0.0);
        if reads_as_path(text) {
            // Its components whole either side of the ellipsis where a form
            // of it fits ([`path_forms`]): `/Users/…/dotfiles/ai-fleet.toml`,
            // never a folder cut inside its name.
            if let Some((_, head_end, tail_start)) = path_forms(text)
                .into_iter()
                .find(|(form, _, _)| crate::tray_raster::ui_text_width(form, text_px) <= available)
            {
                let lead = head_end == 0;
                return TextFieldGeometry {
                    // A leading ellipsis stands before `text_x`.
                    text_x: if lead { text_x + mark } else { text_x },
                    text_right,
                    visible: tail_start..text.len(),
                    ellipsis: if lead {
                        TextFieldEllipsis::Lead
                    } else {
                        TextFieldEllipsis::Middle(head_end)
                    },
                };
            }
            let scan_start = grapheme_boundary_at_or_before(
                text,
                text.len().saturating_sub(TEXT_FIELD_GEOMETRY_BYTES),
            );
            let mut start = text.len();
            let boundaries = text[scan_start..]
                .grapheme_indices()
                .map(|(offset, _)| scan_start + offset)
                .collect::<Vec<_>>();
            for candidate in boundaries.into_iter().rev() {
                if crate::tray_raster::ui_text_width(&text[candidate..], text_px) > room {
                    break;
                }
                start = candidate;
            }
            return TextFieldGeometry {
                text_x: text_x + mark,
                text_right,
                visible: start..text.len(),
                ellipsis: TextFieldEllipsis::Lead,
            };
        }
        let scan_end =
            grapheme_boundary_at_or_before(text, TEXT_FIELD_GEOMETRY_BYTES.min(text.len()));
        let mut end = 0;
        for (offset, grapheme) in text[..scan_end].grapheme_indices() {
            let candidate = offset + grapheme.len();
            if crate::tray_raster::ui_text_width(&text[..candidate], text_px) > room {
                break;
            }
            end = candidate;
        }
        return TextFieldGeometry {
            text_x,
            text_right,
            visible: 0..end,
            ellipsis: TextFieldEllipsis::Trail,
        };
    }
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
        ellipsis: TextFieldEllipsis::None,
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

fn text_field_caret_rect(
    input: &crate::native_text_input::TextInputProjection,
    geometry: &TextFieldGeometry,
    rect: LogicalRect,
    text_px: f32,
) -> LogicalRect {
    let caret = input
        .selection
        .head
        .clamp(geometry.visible.start, geometry.visible.end);
    let x = if input.text.is_empty() {
        geometry.text_x
    } else {
        text_field_x_for_byte(&input.text, geometry, caret, text_px)
    };
    LogicalRect::new(x, rect.y + 7.0, 1.0, (rect.height - 14.0).max(1.0))
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
    /// Styled spans of `text` (byte ranges, disjoint, in order) — bold, emphasis,
    /// strikethrough, inline code, links — recorded by the lowering that
    /// produced `text` ([`crate::native_markdown::lower_inline_text_styled`]) and
    /// handed out per block by
    /// [`crate::native_markdown::MarkdownDocument::block_styles`]. PAINT only: semantics,
    /// accessibility and copy keep reading the flat `text`. Empty for a block
    /// with no inline markup, and for code/table blocks, which paint verbatim.
    pub(crate) inline: Vec<(std::ops::Range<usize>, crate::native_markdown::InlineStyle)>,
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
    pub(crate) preedit_caret: Option<usize>,
    /// Bounded paint string shown in the footer.
    pub(crate) status: Option<String>,
    /// Complete status value announced by accessibility. This is deliberately
    /// separate from `status`: fitting a narrow footer must never truncate a
    /// diagnostic before it reaches the semantic tree.
    pub(crate) semantic_status: Option<String>,
    pub(crate) minibuffer: Option<String>,
    /// Caret byte in the displayed minibuffer label; absent for prefix HUDs.
    pub(crate) minibuffer_caret: Option<usize>,
    pub(crate) cursor_label: Option<String>,
    pub(crate) dirty: bool,
    pub(crate) saving: bool,
    pub(crate) focused: bool,
    pub(crate) action: Option<ActionId>,
}

/// Canonical editor viewport geometry shared by paint and accessibility. The
/// values are logical native-app pixels; the accessibility host applies the final
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

fn editor_caret_rect(
    geometry: &TextViewportGeometry,
    row: usize,
    line: &crate::native_editor::EditorViewportLine,
    byte: usize,
) -> LogicalRect {
    let col = crate::native_editor::editor_display_column(&line.text, byte, line.column_start);
    LogicalRect::new(
        geometry.text_x + col as f32 * geometry.cell_w,
        geometry.body_y + row as f32 * geometry.line_h + 2.0,
        1.0,
        geometry.line_h - 4.0,
    )
}

/// Primary document composition follows the platform's selected marked-text
/// position. The committed caret still supplies the painted preedit origin.
fn editor_composition_caret_rect(mut caret: LogicalRect, spec: &TextViewportSpec) -> LogicalRect {
    if !spec.preedit.is_empty() {
        let byte = grapheme_boundary_at_or_before(
            &spec.preedit,
            spec.preedit_caret
                .unwrap_or(spec.preedit.len())
                .min(spec.preedit.len()),
        );
        let prefix = spec.preedit[..byte].replace(['\r', '\n'], "↵");
        let size = native_type_px(crate::type_scale::TypeStep::Secondary).get();
        caret.x += 2.0
            + crate::tray_raster::measure_text(&prefix, size, crate::widget::TextWeight::Regular);
    }
    caret
}

/// The append-only minibuffer caret is already mapped into the painted label;
/// a shortcut HUD/status has no input caret and therefore cannot claim it.
fn editor_minibuffer_caret_rect(
    spec: &TextViewportSpec,
    rect: LogicalRect,
    geometry: &TextViewportGeometry,
) -> Option<LogicalRect> {
    let label = spec.minibuffer.as_deref()?;
    let byte = grapheme_boundary_at_or_before(label, spec.minibuffer_caret?.min(label.len()));
    let caption = native_type_px(crate::type_scale::TypeStep::Caption).get();
    let footer_y = geometry.body_y + geometry.body_h;
    let baseline = crate::tray_raster::row_baseline(footer_y, geometry.footer_h, caption);
    let x = rect.x
        + EDITOR_FOOTER_TEXT_INSET
        + crate::tray_raster::measure_text(
            &label[..byte],
            caption,
            crate::widget::TextWeight::Regular,
        );
    LogicalRect::new(x, baseline - caption, 1.0, caption + 2.0).intersect(LogicalRect::new(
        rect.x + EDITOR_FOOTER_TEXT_INSET,
        footer_y,
        (rect.width - EDITOR_FOOTER_TEXT_INSET).max(0.0),
        geometry.footer_h,
    ))
}

/// Where the editor footer's status or minibuffer text starts. There is one
/// keymap, so no badge names it ahead of the text.
const EDITOR_FOOTER_TEXT_INSET: f32 = 12.0;

/// The editor rect's fixed chrome bands and text metre, at 1×. These are the
/// pixels `text_viewport_geometry_at_scale` spends before and after the
/// document, so any policy that reserves room for the *document* has to know
/// them — see `editor_shell_metrics_at_scale`, which sizes the completion
/// palette against whatever is left.
const EDITOR_HEADER_H: f32 = 44.0;
const EDITOR_FOOTER_H: f32 = 30.0;
const EDITOR_LINE_H: f32 = 20.0;

pub(crate) fn text_viewport_geometry_at_scale(
    rect: LogicalRect,
    text_scale: f32,
) -> TextViewportGeometry {
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
    let header_h = (EDITOR_HEADER_H * chrome_scale).min(rect.height);
    let footer_h = (EDITOR_FOOTER_H * chrome_scale).min((rect.height - header_h).max(0.0));
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
        line_h: EDITOR_LINE_H * text_scale,
        cell_w,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EditorShellMetrics {
    pub(crate) outer_padding: f32,
    /// A narrow window's denser toolbar rhythm (gap and button height). Not a
    /// layout switch: which buttons share a row is the toolbar plan's
    /// ([`plan_editor_toolbar`]).
    pub(crate) dense_commands: bool,
    pub(crate) command_gap: f32,
    /// The toolbar's width: the buttons flow into rows of it.
    pub(crate) command_bar_width: f32,
    /// Rows the toolbar plan fills.
    pub(crate) command_rows: usize,
    pub(crate) command_button_height: f32,
    pub(crate) command_bar_height: f32,
    pub(crate) content_gap: f32,
    pub(crate) palette_padding: f32,
    pub(crate) palette_header_height: f32,
    pub(crate) palette_row_height: f32,
    pub(crate) palette_row_gap: f32,
    pub(crate) palette_visible_rows: usize,
    pub(crate) palette_height: f32,
}

/// What the editor's assist panel over the file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorPalette {
    /// No panel.
    None,
    /// One line of help for the key under the caret, nothing listed: the
    /// panel is that line alone, with no row under it.
    Help,
    /// This many candidates listed under the help line.
    Rows(usize),
}

impl From<usize> for EditorPalette {
    fn from(candidates: usize) -> Self {
        if candidates == 0 {
            Self::None
        } else {
            Self::Rows(candidates)
        }
    }
}

/// What the editor's toolbar holds beyond its fixed commands — the facts the
/// view and the host both read, so the rows the view draws and the file rows
/// the host sizes the caret for come out of one plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorToolbar {
    /// Previous and Next config problem: a config file with findings.
    pub(crate) problem_navigation: bool,
    /// Reload in Commands' place: the file changed on disk under local edits.
    pub(crate) reload: bool,
    /// An `edit` ticket's header row heads the bar (`native_app::EditHeader`):
    /// one more row, above the buttons.
    pub(crate) edit_header: bool,
}

/// One button of the editor's toolbar: its key, its words (the long form,
/// and a shorter real word where one exists), and where it stands in the
/// order buttons give way when even short words need more rows than the bar
/// allows (`None`: it never does).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EditorToolbarItem {
    pub(crate) key: &'static str,
    pub(crate) long: &'static str,
    pub(crate) short: &'static str,
    /// Lower gives way first. Only commands `M-x` also lists give way.
    pub(crate) yields: Option<u8>,
}

/// The toolbar's buttons, in reading order.
pub(crate) fn editor_toolbar_items(toolbar: EditorToolbar) -> Vec<EditorToolbarItem> {
    let item = |key, long, short, yields| EditorToolbarItem {
        key,
        long,
        short,
        yields,
    };
    let mut items = vec![
        item("editor/save-button", "Save", "Save", None),
        item("editor/undo-button", "Undo", "Undo", None),
        item("editor/redo-button", "Redo", "Redo", Some(0)),
        item("editor/find-button", "Find", "Find", Some(2)),
        item(
            "editor/goto-line-button",
            "Go to Line",
            "Go to Line",
            Some(1),
        ),
        if toolbar.reload {
            item("editor/revert-button", "Reload", "Reload", None)
        } else {
            item("editor/commands-button", "Commands", "Commands", None)
        },
    ];
    if toolbar.problem_navigation {
        items.push(item(
            "editor/config-problem-previous-button",
            "Prev Issue",
            "Prev",
            None,
        ));
        items.push(item(
            "editor/config-problem-next-button",
            "Next Issue",
            "Next",
            None,
        ));
    }
    items
}

/// A toolbar button's width for `label`: its words and padding, never under
/// the touch minimum.
pub(crate) fn editor_toolbar_button_width(label: &str) -> f32 {
    (native_button_label_width(label) + 28.0).max(56.0)
}

/// The rows the editor's toolbar draws: each a run of `(key, words, width)`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EditorToolbarPlan {
    pub(crate) rows: Vec<Vec<(&'static str, &'static str, f32)>>,
}

/// The most rows the toolbar takes before buttons give way.
pub(crate) const EDITOR_TOOLBAR_MAX_ROWS: usize = 3;

/// THE EDITOR TOOLBAR, MEASURED: `items` flow in order into rows of
/// `bar_width` at their own widths. The fewest rows the bar can hold them in
/// win; within them every button keeps its long words where it can, the last
/// buttons taking their short words first. Only where even short words need
/// more than [`EDITOR_TOOLBAR_MAX_ROWS`] rows do the buttons whose commands
/// `M-x` also lists give way, in their `yields` order. A label is never cut
/// to make a row fit.
pub(crate) fn plan_editor_toolbar(
    items: &[EditorToolbarItem],
    bar_width: f32,
    gap: f32,
) -> EditorToolbarPlan {
    let flow = |chosen: &[(usize, &'static str)]| {
        let mut rows: Vec<Vec<(&'static str, &'static str, f32)>> = Vec::new();
        let mut used = 0.0_f32;
        for &(index, label) in chosen {
            let width = editor_toolbar_button_width(label);
            match rows.last_mut() {
                Some(row) if used + gap + width <= bar_width + 0.01 => {
                    row.push((items[index].key, label, width));
                    used += gap + width;
                }
                _ => {
                    rows.push(vec![(items[index].key, label, width)]);
                    used = width;
                }
            }
        }
        rows
    };
    let mut kept: Vec<usize> = (0..items.len()).collect();
    loop {
        for target in 1..=EDITOR_TOOLBAR_MAX_ROWS {
            let mut chosen: Vec<(usize, &'static str)> = kept
                .iter()
                .map(|&index| (index, items[index].long))
                .collect();
            // Short words from the last button back, one at a time.
            let mut next_short = chosen.len();
            loop {
                let rows = flow(&chosen);
                if rows.len() <= target {
                    return EditorToolbarPlan { rows };
                }
                let Some(at) = (0..next_short)
                    .rev()
                    .find(|&at| items[chosen[at].0].short != chosen[at].1)
                else {
                    break;
                };
                chosen[at].1 = items[chosen[at].0].short;
                next_short = at;
            }
        }
        // Even short words need more rows: the next button that yields goes.
        let yielding = kept
            .iter()
            .enumerate()
            .filter_map(|(at, &index)| items[index].yields.map(|rank| (rank, at)))
            .min();
        match yielding {
            Some((_, at)) => {
                kept.remove(at);
            }
            None => {
                // Nothing left to give way: as many rows as the words need.
                let chosen: Vec<(usize, &'static str)> = kept
                    .iter()
                    .map(|&index| (index, items[index].short))
                    .collect();
                return EditorToolbarPlan {
                    rows: flow(&chosen),
                };
            }
        }
    }
}

pub(crate) fn editor_shell_metrics(
    viewport: LogicalRect,
    palette: impl Into<EditorPalette>,
    toolbar: EditorToolbar,
) -> EditorShellMetrics {
    editor_shell_metrics_at_scale(
        viewport,
        palette,
        toolbar,
        crate::native_appearance::text_scale(),
    )
}

pub(crate) fn editor_shell_metrics_at_scale(
    viewport: LogicalRect,
    palette: impl Into<EditorPalette>,
    toolbar: EditorToolbar,
    text_scale: f32,
) -> EditorShellMetrics {
    let palette = palette.into();
    let palette_candidates = match palette {
        EditorPalette::Rows(candidates) => candidates,
        EditorPalette::None | EditorPalette::Help => 0,
    };
    let text_scale = if text_scale.is_finite() && text_scale > 0.0 {
        text_scale.clamp(0.85, 2.0)
    } else {
        1.0
    };
    let chrome_scale = text_scale.min(1.35);
    let dense_commands = viewport.width < 390.0 * text_scale.min(1.4);
    let short = viewport.height < 440.0 * text_scale.min(1.2);
    let outer_padding = if short {
        6.0 * chrome_scale
    } else if viewport.width < 560.0 * text_scale.min(1.4) {
        8.0 * chrome_scale
    } else {
        16.0 * chrome_scale
    };
    let command_gap = if dense_commands {
        4.0 * chrome_scale
    } else {
        6.0 * chrome_scale
    };
    let command_button_height = if dense_commands {
        (34.0 * text_scale).clamp(34.0, 48.0)
    } else {
        (36.0 * text_scale).clamp(36.0, 48.0)
    };
    let command_bar_width = (viewport.width - outer_padding * 2.0).max(0.0);
    let command_rows = plan_editor_toolbar(
        &editor_toolbar_items(toolbar),
        command_bar_width,
        command_gap,
    )
    .rows
    .len()
    .max(1)
        + usize::from(toolbar.edit_header);
    let command_bar_height =
        command_button_height * command_rows as f32 + command_gap * (command_rows - 1) as f32;
    let content_gap = 8.0 * chrome_scale;
    let palette_padding = 6.0 * chrome_scale;
    let palette_header_height = (28.0 * text_scale).clamp(28.0, 40.0);
    let palette_row_height = (34.0 * text_scale).clamp(34.0, 48.0);
    let palette_row_gap = 3.0 * chrome_scale;
    // THE EDITOR FLOOR IS COUNTED IN DOCUMENT ROWS, NOT RECT PIXELS. The rect
    // this reserves is not document: `text_viewport_geometry_at_scale` spends
    // EDITOR_HEADER_H on the modeline and EDITOR_FOOTER_H on the status strip
    // before one line of the file is painted. The old floor — a flat 78 px —
    // therefore promised 78 - 74 = 4 px of body, i.e. ZERO complete text rows,
    // and at the default 80x24 window (viewport 744x420) the arithmetic landed
    // on a 94 px editor rect: one visible line of aterm.toml under a 262 px
    // completion list the user never asked for, on a page that advertises
    // "Edit aterm.toml with live help". Reserve the chrome plus whole lines so
    // the floor means what it says, and let the palette page what is left.
    let editor_chrome_height = (EDITOR_HEADER_H + EDITOR_FOOTER_H) * chrome_scale;
    let minimum_editor_height =
        editor_chrome_height + MINIMUM_EDITOR_DOCUMENT_ROWS as f32 * EDITOR_LINE_H * text_scale;
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
    let palette_height = match palette {
        EditorPalette::None => 0.0,
        // The help line and the panel's padding: no row under it.
        EditorPalette::Help => palette_chrome,
        EditorPalette::Rows(_) => {
            palette_chrome
                + palette_visible_rows as f32 * palette_row_height
                + palette_visible_rows as f32 * palette_row_gap
        }
    };
    EditorShellMetrics {
        outer_padding,
        dense_commands,
        command_gap,
        command_bar_width,
        command_rows,
        command_button_height,
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

/// Document rows the editor keeps before the completion palette may claim a
/// single pixel. Six is the smallest run that shows an erroring config line
/// *in context* — the line above it and the line below — at the default window,
/// and it is what makes the buffer the larger of the two regions there
/// (205 px of editor against 151 px of palette at viewport 744x420).
const MINIMUM_EDITOR_DOCUMENT_ROWS: usize = 6;

/// Exact editor paint rectangle derived from the same responsive shell metrics
/// consumed by `EditorApp::view`. The host uses this before reducer input so
/// caret reveal and the renderer share one row-capacity truth.
pub(crate) fn editor_text_viewport_rect_with_palette(
    viewport: LogicalRect,
    palette: impl Into<EditorPalette>,
    toolbar: EditorToolbar,
) -> LogicalRect {
    editor_text_viewport_rect_at_scale(
        viewport,
        palette,
        toolbar,
        crate::native_appearance::text_scale(),
    )
}

fn editor_text_viewport_rect_at_scale(
    viewport: LogicalRect,
    palette: impl Into<EditorPalette>,
    toolbar: EditorToolbar,
    text_scale: f32,
) -> LogicalRect {
    let metrics = editor_shell_metrics_at_scale(viewport, palette, toolbar, text_scale);
    let gaps = if metrics.palette_height <= 0.0 {
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

/// The document rows the editor shows WHOLE: the count caret reveal and
/// scrolling keep the caret within, so the caret line is never the one cut
/// by the footer (or hidden under it). [`editor_painted_line_capacity_with_palette`]
/// is the count the painter projects, a part row and one more included.
pub(crate) fn editor_visible_line_capacity_with_palette(
    viewport: LogicalRect,
    palette: impl Into<EditorPalette>,
    toolbar: EditorToolbar,
) -> usize {
    let geometry = text_viewport_geometry(editor_text_viewport_rect_with_palette(
        viewport, palette, toolbar,
    ));
    whole_editor_rows(&geometry)
}

/// The document rows the editor PAINTS: every whole row, the part row the
/// footer cuts, and one ahead, so a scroll never shows a blank band.
pub(crate) fn editor_painted_line_capacity_with_palette(
    viewport: LogicalRect,
    palette: impl Into<EditorPalette>,
    toolbar: EditorToolbar,
) -> usize {
    let geometry = text_viewport_geometry(editor_text_viewport_rect_with_palette(
        viewport, palette, toolbar,
    ));
    ((geometry.body_h / geometry.line_h).ceil() as usize)
        .saturating_add(1)
        .clamp(1, 256)
}

fn whole_editor_rows(geometry: &TextViewportGeometry) -> usize {
    // A hair of slack: a body that is a whole number of rows tall in exact
    // arithmetic must not lose its last row to float rounding.
    (((geometry.body_h + 0.01) / geometry.line_h).floor() as usize).clamp(1, 256)
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
    /// Interaction state for a focusable node, as the controls carry it:
    /// [`UiTree::apply_interaction`] sets both, assistive technology hears
    /// `focused`, and the adapter's painter rings the node where
    /// `focus_visible`. Authors leave both false.
    pub(crate) focused: bool,
    pub(crate) focus_visible: bool,
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
    ColorWheel(Control<ColorWheelSpec>),
    Custom(AuditedCustomNode),
}

impl UiContent {
    /// Whether a pointer press on this control waits for a look
    /// ([`Control::look_first`]).
    fn look_first(&self) -> bool {
        match self {
            Self::Button(control) => control.look_first,
            Self::Switch(control) => control.look_first,
            Self::Slider(control) => control.look_first,
            Self::TextField(control) => control.look_first,
            _ => false,
        }
    }

    /// The words this control's value reads as, where they are not its
    /// bare value: a slider's painted readout, with its unit
    /// ([`SliderSpec::display_value`]). Every observer that says the value
    /// says these words ([`SemanticNode::value_text`]).
    pub(crate) fn value_text(&self) -> Option<String> {
        match self {
            Self::Slider(control) => {
                let readout = control.spec.display_value.trim();
                (!readout.is_empty()).then(|| readout.to_string())
            }
            _ => None,
        }
    }

    /// The control's description, where it authored one (ruling 267).
    fn description(&self) -> Option<String> {
        match self {
            Self::Button(control) => control.spec.description.clone(),
            Self::Switch(control) => control.spec.description.clone(),
            Self::SettingsPreview(spec) => spec.semantic_description(),
            Self::Slider(control) => control.spec.description.clone(),
            Self::TextField(control) => control.spec.description.clone(),
            _ => None,
        }
        .filter(|description| !description.trim().is_empty())
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
            Self::ColorWheel(control) => {
                control_projection(SemanticRole::Button, &control.spec.label, control)
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
            // A looping preview is its own Pause/Play control (WCAG 2.2.2):
            // the whole card is the target, so it stays a full-size control
            // at every text scale, and a screen reader hears the motion state
            // in the value and the gesture in the description.
            Self::SettingsPreview(spec) => SemanticProjection {
                role: SemanticRole::Group,
                label: spec.semantic_label(),
                value: SemanticValue::Text(spec.semantic_value()),
                state: None,
                action: spec.pause_action(),
                focusable: spec.pause_control(),
                audit_id: None,
            },
            Self::Custom(spec) => SemanticProjection {
                role: spec.role,
                label: spec.label.clone(),
                value: spec
                    .value
                    .as_ref()
                    .map_or(SemanticValue::None, |v| SemanticValue::Text(v.clone())),
                // An actionable custom node is a control, and assistive
                // technology hears it as an enabled one; without an action it
                // is a dimmed one, or scenery.
                state: (spec.action.is_some() || spec.focusable).then(|| ControlState {
                    enabled: spec.action.is_some(),
                    focused: spec.focused,
                    focus_visible: spec.focus_visible,
                    ..ControlState::default()
                }),
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
        role: control.role.unwrap_or(role),
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
    /// This node is a scrolling list and this its ROOM ([`Self::room`]).
    pub(crate) room: Option<ListRoom>,
    /// This subtree is a list's row off its screen ([`Self::offscreen`]).
    pub(crate) offscreen: bool,
    /// This group is a SEGMENTED CONTROL ([`Self::segmented`]).
    pub(crate) segmented: bool,
    /// Where this boxed group runs on into the pieces of the same card over
    /// and under it ([`Joins`]).
    pub(crate) joins: Joins,
}

/// A BOXED GROUP LISTED A PIECE AT A TIME READS AS ONE CARD: where it runs
/// on into the piece over it (`above`) its top edge is open, and where it
/// goes on in a piece `below` points under it (the gap its list keeps
/// between the two) its bottom edge is open and its face carried over that
/// gap to meet the next piece. A box split between a list's rows used to
/// draw each piece in a box of its own, closed at both ends.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Joins {
    pub(crate) above: bool,
    pub(crate) below: Option<f32>,
}

impl Joins {
    /// Whether the box is closed at both ends.
    pub(crate) fn closed(self) -> bool {
        !self.above && self.below.is_none()
    }
}

/// A scrolling list's ROOM ([`UiNode::room`]): the height from its top where
/// it shows its stops — the room a status bar leaves it — and whether the
/// strip under that, which the page keeps for a bar that is not up, holds the
/// peek of a list that goes on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ListRoom {
    pub(crate) height: f32,
    pub(crate) fades: bool,
    /// The list holds every row it has in the accessibility and control
    /// trees: the rows it does not draw are its rows [`UiNode::offscreen`],
    /// and a node of a row it draws that its room does not show — cut away
    /// by its edge, or a picture in the strip under its room — is in the
    /// tree off its screen too, with its whole bounds, never hit or in the
    /// Tab order.
    pub(crate) lists_all: bool,
}

/// How opaque a list's peek is where its strip begins ([`ListRoom::fades`]),
/// and at the strip's end: it fades from the one to the other, eased out, so
/// nothing steps at the room's edge and a control under it reads at once as
/// going out of view, never as one to press.
const LIST_FADE_FROM: f32 = 1.0;
const LIST_FADE_TO: f32 = 0.0;

impl UiNode {
    pub(crate) fn new(key: impl Into<Arc<str>>, content: UiContent) -> Self {
        Self {
            key: UiKey::new(key),
            layout: Layout::default(),
            content,
            children: Vec::new(),
            paint_only: false,
            room: None,
            offscreen: false,
            segmented: false,
            joins: Joins::default(),
        }
    }

    /// A SEGMENTED CONTROL: this group's control children are one choice,
    /// one control to the keyboard. The compiler makes them ONE Tab stop —
    /// the focused segment, else the selected one, else the first enabled
    /// ([`CompiledUi::focus_order`]) — and records the group
    /// ([`CompiledUi::segment_groups`]): Left and Right move the keyboard
    /// between its segments ([`CompiledUi::segment_step`]) and Return or
    /// Space chooses the one it is on (its own action), as a platform
    /// segmented control does. Every segment stays in the tree and the hit
    /// regions, so a screen reader, a click and automation reach each one,
    /// under the group's one label.
    pub(crate) fn segmented(mut self) -> Self {
        self.segmented = true;
        self
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

    /// [`Self::paint_only`] for this node and everything under it: a
    /// decorative face whose words and marks its parent's control already
    /// carries, so no part of it reaches the accessibility or controls tree
    /// (`paint_only` alone hides the one node, never its children).
    pub(crate) fn paint_only_tree(mut self) -> Self {
        self.paint_only = true;
        self.children = self
            .children
            .into_iter()
            .map(Self::paint_only_tree)
            .collect();
        self
    }

    /// Make this clipped column a scrolling list whose ROOM is the first
    /// `height` of it (THE LIST'S EDGE RULE). Whatever part of a node shows
    /// inside the room is live — read out, in the Tab order, answering the
    /// pointer over exactly what shows — whole or cut by either edge, as in
    /// any native scroll view; a press on one an edge cuts scrolls its stop
    /// into view ([`crate::native_settings`]'s reveal). What the list draws
    /// past its room, in the strip the page keeps for a status bar that is not
    /// up, is a picture: never live, so a bar that comes takes nothing a
    /// person can press, and where the list goes on (`fades`) drawn fading
    /// out, so nothing there reads as a control to press. A list's edges cut
    /// on purpose: the paint audit counts no node they cut, or the strip
    /// holds, as clipped. A press on the picture of a control in the strip
    /// scrolls that control in and works nothing
    /// ([`CompiledUi::list_picture_at`]).
    ///
    /// A LIST'S EDGES NEVER SLICE A LINE OF WORDS OR A PICTOGRAM: whatever
    /// sits over the room is kept whole there — the page's pinned heading or
    /// preview, a caption the list keeps over its rows
    /// ([`Self::under_caption`]), the sidebar's search field — so a line of
    /// words the top edge cuts through paints nothing, nor does one the
    /// bottom edge cuts where no strip fades under the list (a bar is up,
    /// the page keeps none, the window's own edge), nor a pictogram whose
    /// ink either edge cuts ([`PaintNode::cut_edges`]), as a caption a
    /// list's edge would cut paints none of its words: a line sliced under
    /// it read as a stray line of what is over it, a sliver of an icon as a
    /// mark. Faces, strokes and pictures paint as they are, and a control an
    /// edge cuts stays live where it shows.
    pub(crate) fn room(mut self, height: f32, fades: bool) -> Self {
        self.room = Some(ListRoom {
            height,
            fades,
            lists_all: false,
        });
        self
    }

    /// [`Self::room`] for the rows a list scrolls UNDER A CAPTION it keeps
    /// whole over them (the last screen's group caption, a picker's section
    /// heading): a room of their own inside the list, the first `height` of
    /// this clipped column, its top edge the caption's bottom. That edge is a
    /// list's edge — a node it cuts is live where it shows and the paint
    /// audit counts its cut as meant, while every row it leaves whole is
    /// judged as any other — and, a list's top edge, it slices no line of
    /// words: the card's backdrop shows there, so no sliced line sits under
    /// the caption. The list's strip goes on fading under it.
    pub(crate) fn under_caption(self, height: f32) -> Self {
        self.room(height, false)
    }

    /// Mark this subtree a scrolling list's row OFF ITS SCREEN: laid out
    /// where scrolling would bring it, and in the accessibility and control
    /// trees there with its whole bounds, so a screen reader and `controls`
    /// see every row the list holds — but never painted, hit or in the Tab
    /// order: what is not on screen answers no pointer and takes no focus.
    pub(crate) fn offscreen(mut self) -> Self {
        self.offscreen = true;
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
    #[cfg(test)]
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
        fn visit(
            node: &mut UiNode,
            focus: Option<&UiKey>,
            hovered: Option<&UiKey>,
            pressed: Option<&UiKey>,
            focus_visible: bool,
        ) {
            let key = &node.key;
            let state = match &mut node.content {
                UiContent::Button(control) => Some(&mut control.state),
                UiContent::Switch(control) => Some(&mut control.state),
                UiContent::Slider(control) => Some(&mut control.state),
                UiContent::TextField(control) => Some(&mut control.state),
                UiContent::ColorWheel(control) => Some(&mut control.state),
                UiContent::Group(_)
                | UiContent::Text(_)
                | UiContent::RichText(_)
                | UiContent::MarkdownBlock(_)
                | UiContent::TextViewport(_)
                | UiContent::SettingsPreview(_) => None,
                UiContent::Custom(spec) => {
                    if spec.focusable {
                        spec.focused = focus == Some(key);
                        spec.focus_visible = spec.focused && focus_visible;
                    }
                    None
                }
            };
            if let Some(state) = state {
                let is = |target: Option<&UiKey>| target == Some(key);
                // A control that cannot be pressed is never painted focused: one
                // that disabled itself under the keyboard (a pager's Older at its
                // end, s-25) left the focus order, and a ring on it would claim
                // a focus Tab cannot reach.
                state.focused = is(focus) && state.enabled;
                state.hovered = is(hovered);
                state.pressed = is(pressed) && state.hovered;
                state.focus_visible = state.focused && focus_visible;
            }
            for child in &mut node.children {
                visit(child, focus, hovered, pressed, focus_visible);
            }
        }
        visit(&mut self.root, focus, hovered, pressed, focus_visible);
    }

    /// Compile all observable UI products from one typed tree.
    ///
    /// CONSUMES the authored tree. Every production caller renders a fresh tree
    /// and drops it on the next line, and the compiler is the only reader — so
    /// taking `self` by value lets each node's [`UiContent`] be MOVED into its
    /// paint node instead of deep-cloned. That clone was the single largest
    /// per-frame allocation in the native path: for the editor it copied the
    /// entire visible `TextViewportSpec` projection (up to 256 lines, each an
    /// owned `String` of up to 4 KiB plus its selection/caret/syntax/diagnostic
    /// vectors) once per frame, and for a Markdown reader the whole block text.
    pub(crate) fn compile(self, viewport: LogicalRect) -> Result<CompiledUi, CompileError> {
        if !viewport.is_valid() || viewport.is_empty() {
            return Err(CompileError::InvalidViewport);
        }
        let mut compiler = Compiler {
            seen: FxHashSet::default(),
            not_tab_stops: FxHashSet::default(),
            output: CompiledUi {
                bounds: viewport,
                ..CompiledUi::default()
            },
        };
        // From the AUTHORED tree, not the clipped observers — see the field doc.
        // Hoisted ABOVE the traversal because the traversal now consumes the
        // tree; it still walks every child (a clipped subtree keeps contributing
        // its default action), and on the error path its result is discarded by
        // the `?` exactly as before.
        fn find_default(node: &UiNode) -> Option<(UiKey, ActionId)> {
            if let UiContent::Button(control) = &node.content
                && control.style == StyleRef::Primary
                && control.state.enabled
            {
                return Some((node.key.clone(), control.action.clone()));
            }
            node.children.iter().find_map(find_default)
        }
        let default_action = find_default(&self.root);
        compiler.node(self.root, viewport, viewport, None, None)?;
        compiler.output.default_action = default_action;
        Ok(compiler.output)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaintNode {
    pub(crate) key: UiKey,
    pub(crate) rect: LogicalRect,
    pub(crate) clip: LogicalRect,
    pub(crate) content: UiContent,
    /// Cut on purpose: by a scrolling list's edge, or drawn in the strip
    /// under its room ([`UiNode::room`]). Only what such an edge itself cuts
    /// is: a row it leaves whole is judged as any other node.
    pub(crate) peek: bool,
    /// The band, top to bottom, over which this node fades: the strip under a
    /// scrolling list's room where the list goes on ([`ListRoom::fades`]).
    pub(crate) fade: Option<(f32, f32)>,
    /// Where the edges of this node's list cut across it ([`UiNode::room`]):
    /// what of its words an edge cuts through paints nothing
    /// ([`drop_cut_marks`]).
    pub(crate) cut_edges: CutEdges,
    /// Where this boxed group runs on into the pieces of its card
    /// ([`UiNode::joins`]).
    pub(crate) joins: Joins,
}

impl PaintNode {
    /// Whether an ancestor clip removes a perceptible part of this node by
    /// accident: a peek's cut is deliberate.
    pub(crate) fn clipped(&self) -> bool {
        !self.peek && materially_clipped(self.rect, self.clip)
    }

    /// The draw primitives this node paints, as [`CompiledUi::tray`] paints
    /// it (inside its clip, a line of words a caption's edge cuts through
    /// dropped): what the audit reads to say which of its words it paints.
    fn painted_prims(&self) -> Vec<crate::widget::DrawPrim> {
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let mut prims = Vec::new();
        let icons = painted_icons(|| paint_compiled_node(&mut prims, self, roles, theme, 14.0));
        drop_cut_marks(&mut prims, 0, &icons, self.cut_edges);
        prims
    }
}

/// How far over its baseline and under it a line of words a `px` run paints
/// reaches: the UI faces' ascender, accents over capitals included, and
/// their descender (San Francisco's and DejaVu's reach about 0.95 and 0.24
/// of the em).
fn word_line_band(baseline: f32, px: f32) -> (f32, f32) {
    (baseline - 0.95 * px, baseline + 0.25 * px)
}

/// How far two marks must cross, both ways, to be painted over each other
/// ([`CompiledUi::ink_overlaps`]): more than a glyph's side bearing, so two
/// marks set side by side at their measured advances do not count.
const INK_OVERLAP_LEAST: f32 = 1.0;

/// How tall a line of words' body is over its baseline, of its size: the
/// cap height the UI faces centre their rows on (`tray_raster::row_baseline`)
/// — what two lines that run into each other both cover.
const WORDS_BODY_EM: f32 = 0.7;

/// One mark a node paints ([`CompiledUi::ink_overlaps`]): the node's index,
/// its words (`None` for an icon), and where its ink is.
struct InkMark {
    node: usize,
    words: Option<String>,
    rect: LogicalRect,
}

/// The marks `prims`, painted by node `node` inside `clip`, draw: each line
/// of words over its body (its advance, its cap band), each icon stroke,
/// dot, ring or message icon over its ink, cut to the clips it is drawn in.
fn ink_marks(
    node: usize,
    prims: &[crate::widget::DrawPrim],
    clip: LogicalRect,
    out: &mut Vec<InkMark>,
) {
    use crate::widget::DrawPrim;
    let mut clips = vec![clip];
    for prim in prims {
        let current = *clips.last().unwrap_or(&clip);
        let (words, rect) = match prim {
            DrawPrim::ClipPush { x, y, w, h } => {
                clips.push(
                    current
                        .intersect(LogicalRect::new(*x, *y, *w, *h))
                        .unwrap_or_default(),
                );
                continue;
            }
            DrawPrim::ClipPop => {
                if clips.len() > 1 {
                    clips.pop();
                }
                continue;
            }
            DrawPrim::Text {
                x,
                baseline,
                s,
                px,
                weight,
                face,
                ..
            } => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let width = |text: &str| {
                    if *face == crate::widget::TextFace::Mono {
                        crate::tray_raster::measure_text(text, *px, *weight)
                    } else {
                        crate::tray_raster::ui_text_width_for(*face, text, *px)
                    }
                };
                let lead = width(&s[..s.len() - s.trim_start().len()]);
                let body = WORDS_BODY_EM * px;
                (
                    Some(trimmed.to_string()),
                    LogicalRect::new(x + lead, baseline - body, width(trimmed), body),
                )
            }
            DrawPrim::Line { .. } | DrawPrim::Dot { .. } | DrawPrim::Ring { .. } => {
                match prim_ink(prim) {
                    Some(rect) => (None, rect),
                    None => continue,
                }
            }
            DrawPrim::BandIcon { x, y, size, .. } => (None, LogicalRect::new(*x, *y, *size, *size)),
            _ => continue,
        };
        if let Some(rect) = rect.intersect(current).filter(|rect| !rect.is_empty()) {
            out.push(InkMark { node, words, rect });
        }
    }
}

/// Where a pictogram's prim puts ink ([`paint_button_icon`]): its outline
/// with the stroke's width, its block, its segment, dot or ring.
fn prim_ink(prim: &crate::widget::DrawPrim) -> Option<LogicalRect> {
    use crate::widget::DrawPrim;
    match prim {
        DrawPrim::Stroke {
            x, y, w, h, width, ..
        } => {
            let half = width * 0.5;
            Some(LogicalRect::new(x - half, y - half, w + width, h + width))
        }
        DrawPrim::Panel { x, y, w, h, .. } => Some(LogicalRect::new(*x, *y, *w, *h)),
        DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            ..
        } => {
            let half = width * 0.5;
            Some(LogicalRect::new(
                x1.min(*x2) - half,
                y1.min(*y2) - half,
                (x1 - x2).abs() + *width,
                (y1 - y2).abs() + *width,
            ))
        }
        DrawPrim::Dot { cx, cy, r, .. } => Some(LogicalRect::new(cx - r, cy - r, 2.0 * r, 2.0 * r)),
        DrawPrim::Ring {
            cx, cy, r_outer, ..
        } => Some(LogicalRect::new(
            cx - r_outer,
            cy - r_outer,
            2.0 * r_outer,
            2.0 * r_outer,
        )),
        _ => None,
    }
}

/// Whether two marks one node draws are one mark painted twice: the parts
/// of one icon, or a run of its words painted again over themselves on the
/// same line at the same size (an editor line's syntax colours, drawn over
/// the line in place).
fn repaints(a: &InkMark, b: &InkMark) -> bool {
    match (&a.words, &b.words) {
        (None, None) => true,
        (Some(one), Some(other)) => {
            (a.rect.y - b.rect.y).abs() < 0.01
                && (a.rect.height - b.rect.height).abs() < 0.01
                && (one.contains(other.as_str()) || other.contains(one.as_str()))
        }
        _ => false,
    }
}

/// One icon of the strokes, dots and rings a node draws that touch: the
/// marks from `start` on are one node's, and its icons' parts are one mark.
fn merge_icon_strokes(marks: &mut Vec<InkMark>, start: usize) {
    let mut merged = true;
    while merged {
        merged = false;
        'pairs: for a in start..marks.len() {
            if marks[a].words.is_some() {
                continue;
            }
            for b in a + 1..marks.len() {
                if marks[b].words.is_some() {
                    continue;
                }
                let (one, other) = (marks[a].rect, marks[b].rect);
                let grown = LogicalRect::new(
                    one.x - INK_OVERLAP_LEAST,
                    one.y - INK_OVERLAP_LEAST,
                    one.width + 2.0 * INK_OVERLAP_LEAST,
                    one.height + 2.0 * INK_OVERLAP_LEAST,
                );
                if grown.intersect(other).is_some() {
                    marks[a].rect = one.union(other);
                    marks.remove(b);
                    merged = true;
                    break 'pairs;
                }
            }
        }
    }
}

/// Where the edges of a node's scrolling list cut across it
/// ([`UiNode::room`]): what of its words an edge cuts through paints nothing
/// ([`drop_cut_marks`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CutEdges {
    /// The list's top edge — under the page's pinned heading, a caption the
    /// list keeps, the sidebar's search field.
    pub(crate) top: Option<f32>,
    /// The list's bottom edge.
    pub(crate) bottom: Option<f32>,
}

impl CutEdges {
    /// A node no list edge cuts across.
    #[cfg(test)]
    pub(crate) const NONE: Self = Self {
        top: None,
        bottom: None,
    };

    /// Whether an edge cuts across the node.
    pub(crate) const fn any(self) -> bool {
        self.top.is_some() || self.bottom.is_some()
    }
}

/// Which of `prims`, a node's own, the edges of its list cut away
/// ([`drop_cut_marks`]): each line of words an edge cuts through, and each
/// pictogram (`icons`, ranges of `prims`) whose ink it cuts through. Marked
/// in `dropped`, index for index.
fn cut_marks(
    prims: &[crate::widget::DrawPrim],
    icons: &[std::ops::Range<usize>],
    edges: CutEdges,
    dropped: &mut [bool],
) {
    let crosses = |top: f32, bottom: f32| {
        [edges.top, edges.bottom]
            .into_iter()
            .flatten()
            .any(|edge| top < edge - CLIP_EDGE_EPSILON && bottom > edge + CLIP_EDGE_EPSILON)
    };
    if !edges.any() {
        return;
    }
    for (index, prim) in prims.iter().enumerate() {
        if let crate::widget::DrawPrim::Text { baseline, px, .. } = prim {
            let (top, bottom) = word_line_band(*baseline, *px);
            if crosses(top, bottom) {
                dropped[index] = true;
            }
        }
    }
    for span in icons {
        let Some(ink) = prims
            .get(span.clone())
            .into_iter()
            .flatten()
            .filter_map(prim_ink)
            .reduce(|one, other| one.union(other))
        else {
            continue;
        };
        if crosses(ink.y, ink.bottom()) {
            for prim in span.clone() {
                if let Some(dropped) = dropped.get_mut(prim) {
                    *dropped = true;
                }
            }
        }
    }
}

/// WORDS AND PICTOGRAMS AT A LIST'S EDGES ARE WHOLE OR NOT DRAWN
/// ([`UiNode::room`]): drop each line of words from `prims[from..]` that the
/// list's top edge, or its bottom edge where no strip fades under it, cuts
/// through, and each pictogram (`icons`, absolute ranges of `prims`) whose
/// ink it cuts through, so what is behind shows there, as a caption the
/// list's edge would cut paints none of its words. What the edges leave
/// whole, and every face, stroke and picture, paints as it is: a control
/// they cut stays live where it shows.
fn drop_cut_marks(
    prims: &mut Vec<crate::widget::DrawPrim>,
    from: usize,
    icons: &[std::ops::Range<usize>],
    edges: CutEdges,
) {
    if !edges.any() || from >= prims.len() {
        return;
    }
    let own: Vec<std::ops::Range<usize>> = icons
        .iter()
        .filter(|span| span.start >= from)
        .map(|span| span.start - from..span.end - from)
        .collect();
    let mut dropped = vec![false; prims.len() - from];
    cut_marks(&prims[from..], &own, edges, &mut dropped);
    let mut index = 0;
    prims.retain(|_| {
        let keep = index < from || !dropped[index - from];
        index += 1;
        keep
    });
}

/// What a node inside a scrolling list inherits from it ([`UiNode::room`]).
#[derive(Clone, Copy, Debug)]
struct ListContext {
    /// The list's room on screen: where what it draws is live.
    room: LogicalRect,
    /// The strip under the room where the list fades, top to bottom.
    fade: Option<(f32, f32)>,
    /// The list holds every row in the tree ([`ListRoom::lists_all`]).
    lists_all: bool,
    /// The room's top edge, which slices no line of words ([`UiNode::room`]).
    top_edge: f32,
    /// The list's bottom edge where no strip fades under it — a bar is up,
    /// the page keeps none, the list's last screen, the window's own edge —
    /// which slices no line of words either.
    bottom_edge: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HitRegion {
    pub(crate) key: UiKey,
    pub(crate) rect: LogicalRect,
    pub(crate) action: ActionId,
    /// A pointer press here waits for a look ([`Control::look_first`]).
    pub(crate) look_first: bool,
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
    /// A control's DESCRIPTION — what a screen reader says after its name
    /// (`Error, CI, 3 minutes ago` on a Messages row). It rode the paint node
    /// only, so no observer saw it (design ruling 267): every observer —
    /// the accessibility projection and `inspect … tree` — reads it here.
    pub(crate) description: Option<String>,
    /// A list's row off its screen ([`UiNode::offscreen`]): in the tree at
    /// the bounds scrolling would give it, with nothing painted or hit.
    pub(crate) offscreen: bool,
    /// The words a numeric value reads as, where they are not the bare
    /// number ([`UiContent::value_text`]): a slider's readout with its unit
    /// ("12 px"), so a screen reader hears what the control paints.
    pub(crate) value_text: Option<String>,
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
    /// Update swaps Primary between "Check for Updates" and "Update to
    /// Latest Now" as a build stages), so "first" is the unique one. Recorded
    /// from the AUTHORED tree before viewport clipping — a route's default is
    /// semantic, not visual, so Return works even with the button scrolled out
    /// of view (clipped subtrees are absent from every other observer here).
    /// A focused control and a focused text field's Submit take precedence at
    /// the call site, and Space never falls back to this (Space activates only
    /// the focused control on macOS).
    pub(crate) default_action: Option<(UiKey, ActionId)>,
    /// Every segmented control ([`UiNode::segmented`]): its group's key and
    /// its segments, in order.
    pub(crate) segment_groups: Vec<SegmentGroup>,
    /// What of each enabled control a scrolling list draws in the strip
    /// under its room ([`UiNode::room`]): a picture, never live, but a press
    /// on it is the list's — it scrolls that control into the room and
    /// works nothing ([`Self::list_picture_at`]).
    pub(crate) list_pictures: Vec<ListPicture>,
}

/// The picture of a control a list draws in the strip under its room
/// ([`CompiledUi::list_pictures`]): the control's key, and where it shows
/// there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListPicture {
    pub(crate) key: UiKey,
    pub(crate) rect: LogicalRect,
}

/// One segmented control as compiled ([`UiNode::segmented`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SegmentGroup {
    pub(crate) key: UiKey,
    pub(crate) segments: Vec<UiKey>,
}

impl CompiledUi {
    /// THE STRIP UNDER A LIST'S ROOM TAKES A PRESS AS A SCROLL: the control
    /// whose picture a list draws there under `(x, y)`, which a press nudges
    /// into the room without working it, as a platform list scrolls a row a
    /// press finds half hidden into view — a click on what looks like a
    /// control is never silently lost. `None` where no picture is.
    pub(crate) fn list_picture_at(&self, x: f32, y: f32) -> Option<&UiKey> {
        self.list_pictures
            .iter()
            .rev()
            .find(|picture| picture.rect.contains(x, y))
            .map(|picture| &picture.key)
    }

    /// THE ARROW MODEL OF A SEGMENTED CONTROL: from segment `key`, `step`
    /// segments on (Left -1, Right 1), stopping at its ends. `None` where
    /// `key` is no segment; `Some(None)` at the end it would pass, where the
    /// arrow is still the control's and moves nothing.
    pub(crate) fn segment_step(&self, key: &UiKey, step: isize) -> Option<Option<UiKey>> {
        let group = self
            .segment_groups
            .iter()
            .find(|group| group.segments.contains(key))?;
        let at = group.segments.iter().position(|segment| segment == key)?;
        Some(
            at.checked_add_signed(step)
                .and_then(|target| group.segments.get(target))
                .cloned(),
        )
    }

    /// The focused input's visible caret, in this retained leaf's logical
    /// pixels. Uses the same geometry as paint; a clipped or unfocused caret
    /// cannot anchor an OS candidate window over unrelated content.
    pub(crate) fn ime_caret_rect(&self) -> Option<LogicalRect> {
        self.paint.iter().rev().find_map(|node| {
            let caret = match &node.content {
                UiContent::TextField(control) if control.state.focused && control.state.enabled => {
                    let input = control.spec.input.as_ref()?;
                    let size = native_type_px(crate::type_scale::TypeStep::Secondary).get();
                    let geometry = text_field_geometry(control, node.rect, size);
                    text_field_caret_rect(input, &geometry, node.rect, size).intersect(
                        LogicalRect::new(
                            geometry.text_x,
                            node.rect.y + 2.0,
                            (geometry.text_right - geometry.text_x).max(0.0),
                            (node.rect.height - 4.0).max(0.0),
                        ),
                    )?
                }
                UiContent::TextViewport(spec) if spec.focused => {
                    let geometry = text_viewport_geometry(node.rect);
                    if spec.minibuffer_caret.is_some() {
                        return editor_minibuffer_caret_rect(spec, node.rect, &geometry)?
                            .intersect(node.clip)?
                            .intersect(self.bounds);
                    }
                    let projection = spec.projection.as_ref()?;
                    let (row, line, byte) =
                        projection
                            .lines
                            .iter()
                            .enumerate()
                            .find_map(|(row, line)| {
                                line.carets
                                    .iter()
                                    .find(|(_, primary)| *primary)
                                    .map(|(byte, _)| (row, line, *byte))
                            })?;
                    editor_composition_caret_rect(
                        editor_caret_rect(&geometry, row, line, byte),
                        spec,
                    )
                    .intersect(LogicalRect::new(
                        node.rect.x,
                        geometry.body_y,
                        node.rect.width,
                        geometry.body_h,
                    ))?
                }
                _ => return None,
            };
            caret.intersect(node.clip)?.intersect(self.bounds)
        })
    }

    /// Topmost enabled control at the point, matching paint order.
    pub(crate) fn hit_test(&self, x: f32, y: f32) -> Option<&HitRegion> {
        self.hits.iter().rev().find(|hit| hit.rect.contains(x, y))
    }

    pub(crate) fn semantic(&self, key: &UiKey) -> Option<&SemanticNode> {
        self.semantics.iter().find(|node| &node.key == key)
    }

    /// Snap one pointer position through the exact HSV disk painted for `key`
    /// (a [`UiContent::ColorWheel`]): the hue and saturation under it, only
    /// for a point INSIDE the disk of a wheel that takes a pick, using the
    /// SAME polar-hue math the rasterizer paints (`tray_raster::hsv_disk`), so
    /// the committed colour and the pixel under the pointer can never
    /// disagree. Anywhere else (including the node's corners outside the disk)
    /// is `None`: a miss, not a pick. The brightness is the picker's own, so
    /// a pick on a black disk keeps its hue.
    pub(crate) fn color_wheel_pick_at(&self, key: &UiKey, x: f32, y: f32) -> Option<(f32, f32)> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let paint = self.paint.iter().find(|node| &node.key == key)?;
        let UiContent::ColorWheel(control) = &paint.content else {
            return None;
        };
        if !control.state.enabled {
            return None;
        }
        color_wheel_pick_at(paint.rect, x, y)
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
        // A drag maps over the track the bar draws, never past the domain.
        let (low, high) = control.spec.track_span(minimum, maximum);
        let (low, high) = (low.max(minimum), high.min(maximum));
        if high <= low {
            return Some(low);
        }
        let fraction = ((x - geometry.track_x) / width).clamp(0.0, 1.0) as f64;
        let raw = low + (high - low) * fraction;
        Some(snap_slider_value(raw, low, high, control.spec.step))
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
                            "enabled:{} focused:{} focus-visible:{} hovered:{} pressed:{} selected:{} invalid:{} busy:{} muted:{}",
                            state.enabled,
                            state.focused,
                            state.focus_visible,
                            state.hovered,
                            state.pressed,
                            state.selected,
                            state.invalid,
                            state.busy,
                            state.muted,
                        )
                    },
                );
                // A row its list holds off screen says so: its rect is
                // where scrolling puts it, over whatever shows there now.
                let offscreen = if node.offscreen { " offscreen=true" } else { "" };
                format!(
                    "ui key={:?} role={:?} label={:?} value={} action={} state={} rect={:.1},{:.1},{:.1},{:.1}{offscreen}",
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
        let clipped = self.paint.iter().filter(|node| node.clipped()).count();
        let overlaps = self.ink_overlaps();
        let mut lines = vec![format!(
            "paint-audit text-nodes={} overflow={overflow} viewport={:.1},{:.1},{:.1},{:.1} nodes={} semantics={} hits={} focusable={} clipped={clipped} overlaps={} compiled-fingerprint={:016x}",
            audits.len(),
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            self.bounds.height,
            self.paint.len(),
            self.semantics.len(),
            self.hits.len(),
            self.focus_order.len(),
            overlaps.len(),
            self.fingerprint(),
        )];
        lines.extend(self.paint.iter().map(|paint| self.paint_node_audit(paint)));
        lines.extend(audits.iter().map(text_fit_line));
        lines.extend(overlaps);
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

    /// Every fit failure [`Self::paint_audit_lines`] reports, WITHOUT formatting
    /// the whole audit: a text node whose fit audit overflows (clip truncation
    /// included — [`finish_text_fit`] folds it into `overflow`), a node an
    /// ancestor clip materially cuts (the summary's `clipped=`), and a text
    /// viewport whose `paint-editor` record says `clip-truncated=true`. Each
    /// failure is the audit's own record for that node, so a caller that finds
    /// this non-empty can print it as-is. It is the cheap form of "zero
    /// overflow, zero clipped" for sweeps that compile thousands of pages: the
    /// full audit renders a `paint-node` record per node, each with a linear
    /// semantic and focus-order lookup, and only a failure needs to read it.
    /// Two painted marks over each other (a `paint-overlap` record,
    /// [`Self::ink_overlaps`]) are a failure too.
    #[cfg(test)]
    pub(crate) fn fit_failures(&self) -> Vec<String> {
        let mut failures = self.ink_overlaps();
        for paint in &self.paint {
            if let Some(audit) = text_fit_audit(paint).filter(|audit| audit.overflow) {
                failures.push(text_fit_line(&audit));
            }
            if paint.clipped() {
                failures.push(self.paint_node_audit(paint));
            }
            if let UiContent::TextViewport(spec) = &paint.content {
                let line = text_viewport_paint_audit(paint, spec);
                if line.contains(" clip-truncated=true ") {
                    failures.push(line);
                }
            }
        }
        failures
    }

    /// THE EDGE AUDIT: every line of words and every pictogram the page
    /// paints that a clip cuts through — part of it painted, the rest cut
    /// away above or below — outside the strip a list fades in under its
    /// room, where what it draws is a picture fading out. Measured on what
    /// is painted, as [`Self::tray`] paints it, each mark inside the clips
    /// it is drawn in.
    #[cfg(test)]
    pub(crate) fn sliced_marks(&self) -> Vec<String> {
        use crate::widget::DrawPrim;
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let crosses = |top: f32, bottom: f32, edge: f32| {
            top < edge - CLIP_EDGE_EPSILON && bottom > edge + CLIP_EDGE_EPSILON
        };
        let mut out = Vec::new();
        for node in &self.paint {
            let mut prims = Vec::new();
            let icons = painted_icons(|| paint_compiled_node(&mut prims, node, roles, theme, 14.0));
            let mut dropped = vec![false; prims.len()];
            cut_marks(&prims, &icons, node.cut_edges, &mut dropped);
            // Each prim's clip, as the painter nests them inside the node's.
            let mut clips = vec![node.clip];
            let mut clip_of = Vec::with_capacity(prims.len());
            for prim in &prims {
                let current = *clips.last().unwrap_or(&node.clip);
                clip_of.push(current);
                match prim {
                    DrawPrim::ClipPush { x, y, w, h } => clips.push(
                        current
                            .intersect(LogicalRect::new(*x, *y, *w, *h))
                            .unwrap_or_default(),
                    ),
                    DrawPrim::ClipPop => {
                        clips.pop();
                    }
                    _ => {}
                }
            }
            // A cut in the strip a list fades in is a picture fading out.
            let faded = |edge: f32| {
                node.fade
                    .is_some_and(|(top, bottom)| edge >= top - 0.5 && edge <= bottom + 0.5)
            };
            // Cut by a clip over the node, not the node's own bounds (a line
            // set in a box a little shorter than its accents' reach is the
            // fit audit's to judge).
            let sliced = |clip: LogicalRect, top: f32, bottom: f32| {
                !clip.is_empty()
                    && ((clip.y > node.rect.y + CLIP_EDGE_EPSILON && crosses(top, bottom, clip.y))
                        || (clip.bottom() < node.rect.bottom() - CLIP_EDGE_EPSILON
                            && crosses(top, bottom, clip.bottom())
                            && !faded(clip.bottom())))
            };
            for (index, prim) in prims.iter().enumerate() {
                if dropped[index] {
                    continue;
                }
                if let DrawPrim::Text {
                    baseline, px, s, ..
                } = prim
                {
                    let (top, bottom) = word_line_band(*baseline, *px);
                    if sliced(clip_of[index], top, bottom) {
                        out.push(format!(
                            "paint-slice key={:?} words={s:?} band={top:.1}..{bottom:.1} clip={:?}",
                            node.key.as_str(),
                            clip_of[index]
                        ));
                    }
                }
            }
            for span in icons {
                if span.clone().any(|index| dropped[index]) {
                    continue;
                }
                let Some(ink) = prims[span.clone()]
                    .iter()
                    .filter_map(prim_ink)
                    .reduce(|one, other| one.union(other))
                else {
                    continue;
                };
                let clip = clip_of.get(span.start).copied().unwrap_or(node.clip);
                if sliced(clip, ink.y, ink.bottom()) {
                    out.push(format!(
                        "paint-slice key={:?} icon ink={ink:?} clip={clip:?}",
                        node.key.as_str()
                    ));
                }
            }
        }
        out
    }

    /// THE AUDIT JUDGES WHAT PAINTS OVER WHAT: a `paint-overlap` record for
    /// every two marks the page paints whose ink crosses — a line of words,
    /// or an icon (a pictogram, whatever it is drawn with — its segments, its
    /// outlines and blocks ([`ICON_SPANS`]) — a dot, a ring, a message icon)
    /// — within one node (a preview header's state run under its Pause
    /// glyph) or between two (sibling labels run into each other, a label
    /// under a neighbour's pictogram). An icon's own parts are one mark;
    /// what a node fills or strokes under its marks (a card, a control's
    /// face, a switch's track, a terminal sample) is none.
    /// Measured on what is painted ([`paint_compiled_node`], as
    /// [`Self::tray`] paints it), each mark inside the clips it is drawn in.
    pub(crate) fn ink_overlaps(&self) -> Vec<String> {
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let mut marks = Vec::new();
        let mut prims = Vec::new();
        for (index, node) in self.paint.iter().enumerate() {
            prims.clear();
            let icons = painted_icons(|| paint_compiled_node(&mut prims, node, roles, theme, 14.0));
            let mut dropped = vec![false; prims.len()];
            cut_marks(&prims, &icons, node.cut_edges, &mut dropped);
            let start = marks.len();
            // A pictogram is one mark over all it draws, its outlines and
            // blocks too: one a list's edge cuts through paints nothing.
            for span in &icons {
                if span.clone().any(|prim| dropped[prim]) {
                    continue;
                }
                let ink = prims[span.clone()]
                    .iter()
                    .filter_map(prim_ink)
                    .reduce(|one, other| one.union(other));
                if let Some(rect) = ink
                    .and_then(|ink| ink.intersect(node.clip))
                    .filter(|rect| !rect.is_empty())
                {
                    marks.push(InkMark {
                        node: index,
                        words: None,
                        rect,
                    });
                }
            }
            drop_cut_marks(&mut prims, 0, &icons, node.cut_edges);
            ink_marks(index, &prims, node.clip, &mut marks);
            merge_icon_strokes(&mut marks, start);
        }
        let mut overlaps = Vec::new();
        for (at, a) in marks.iter().enumerate() {
            for b in &marks[at + 1..] {
                if a.node == b.node && repaints(a, b) {
                    continue;
                }
                let Some(cross) = a.rect.intersect(b.rect) else {
                    continue;
                };
                if cross.width < INK_OVERLAP_LEAST || cross.height < INK_OVERLAP_LEAST {
                    continue;
                }
                let ink = |mark: &InkMark| match &mark.words {
                    Some(words) => format!("words:{words:?}"),
                    None => "icon".to_string(),
                };
                overlaps.push(format!(
                    "paint-overlap a={:?} a-ink={} b={:?} b-ink={} at={:.1},{:.1},{:.1},{:.1}",
                    self.paint[a.node].key.as_str(),
                    ink(a),
                    self.paint[b.node].key.as_str(),
                    ink(b),
                    cross.x,
                    cross.y,
                    cross.width,
                    cross.height,
                ));
            }
        }
        overlaps
    }

    /// The `paint-text` record [`Self::paint_audit_lines`] prints for the FIRST
    /// painted node keyed `key` that has a text-fit audit — the same line, built
    /// without building the rest of the audit.
    #[cfg(test)]
    pub(crate) fn text_fit_audit_line(&self, key: &UiKey) -> Option<String> {
        self.paint
            .iter()
            .filter(|paint| paint.key == *key)
            .find_map(text_fit_audit)
            .map(|audit| text_fit_line(&audit))
    }

    fn paint_node_audit(&self, paint: &PaintNode) -> String {
        let semantic = self.semantic(&paint.key);
        let role = semantic.map_or_else(|| "-".to_string(), |node| format!("{:?}", node.role));
        let label = semantic.map_or_else(String::new, |node| node.label.clone());
        let value =
            semantic.map_or_else(|| "-".to_string(), |node| semantic_value_words(&node.value));
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
            paint.clipped(),
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
    ///
    /// This runs once per native frame (and once per visible native leaf in the
    /// split path), so it deliberately touches NO heap: the semantic half hashes
    /// the typed fields directly rather than round-tripping them through
    /// `controls_lines()`'s `format!` per node, and the paint fallback streams
    /// `Debug` straight into the hasher through [`HashFmt`] instead of
    /// materializing a `String` per node. FxHash replaces SipHash for the same
    /// reason the rest of the workspace uses it: this is a cache key over
    /// first-party data, not a hash table exposed to untrusted input.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hash = aterm_hash::FxHasher::default();
        self.bounds.x.to_bits().hash(&mut hash);
        self.bounds.y.to_bits().hash(&mut hash);
        self.bounds.width.to_bits().hash(&mut hash);
        self.bounds.height.to_bits().hash(&mut hash);
        // Exactly the fields `controls_lines()` renders — key, role, label,
        // value, action, state, rect — fed to the hasher untouched. Geometry is
        // hashed at full precision instead of the audit line's `{:.1}`, which
        // only ever makes the fingerprint MORE discriminating (the paint loop
        // below already hashes the same rect bit-exactly).
        for node in &self.semantics {
            hash_semantic_node(node, &mut hash);
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
            node.fade
                .map(|(top, bottom)| (top.to_bits(), bottom.to_bits()))
                .hash(&mut hash);
            ui_content_kind(&node.content).hash(&mut hash);
            match &node.content {
                UiContent::SettingsPreview(spec) => spec.paint_fingerprint().hash(&mut hash),
                UiContent::MarkdownBlock(spec) => {
                    markdown_paint_fingerprint(node, spec).hash(&mut hash);
                }
                UiContent::TextViewport(spec) => text_viewport_fingerprint(spec).hash(&mut hash),
                content => hash_debug(content, &mut hash),
            }
        }
        hash.finish() | 1
    }

    /// Bootstrap native-app painter. Typed controls lower to the shared
    /// renderer-independent draw vocabulary, so CPU, GPU, and image capture
    /// consume identical pixels. Document apps may later lower large text runs
    /// directly to the retained scene without changing their semantic tree.
    ///
    /// One theme plays both parts here — chrome roles AND terminal specimen —
    /// which is exact whenever chrome follows the terminal. A host forcing the
    /// chrome side (Linux config `window_theme=light|dark`,
    /// [`crate::App::chrome_palette_theme`]) must call
    /// [`Self::tray_with_chrome`] so the Settings preview keeps showing real
    /// terminal colors on the forced chrome.
    #[cfg(test)]
    pub(crate) fn tray(
        &self,
        theme: aterm_render::Theme,
        base_px: f32,
    ) -> crate::widget::TrayInput {
        self.tray_with_chrome(theme, theme, base_px)
    }

    /// [`Self::tray`] with the CHROME palette and the TERMINAL theme split: the
    /// role tokens every control paints with derive from `chrome` (its theme,
    /// palette hues and authored accent and border), while
    /// `terminal_theme` reaches only the nodes that display terminal content
    /// (the Settings appearance preview's live-colors fallback). The two are
    /// the same value everywhere chrome follows the terminal.
    pub(crate) fn tray_with_chrome(
        &self,
        chrome: impl Into<crate::chrome_band::ChromePalette>,
        terminal_theme: aterm_render::Theme,
        base_px: f32,
    ) -> crate::widget::TrayInput {
        let theme = terminal_theme;
        let roles = crate::settings::Roles::from_theme(chrome);
        let mut prims = Vec::with_capacity(self.paint.len() * 5);
        for node in &self.paint {
            if let Some((top, bottom)) = node.fade {
                prims.push(crate::widget::DrawPrim::FadePush {
                    y: top,
                    h: bottom - top,
                    from: LIST_FADE_FROM,
                    to: LIST_FADE_TO,
                });
            }
            prims.push(crate::widget::DrawPrim::ClipPush {
                x: node.clip.x,
                y: node.clip.y,
                w: node.clip.width,
                h: node.clip.height,
            });
            let from = prims.len();
            if node.cut_edges.any() {
                let icons =
                    painted_icons(|| paint_compiled_node(&mut prims, node, roles, theme, base_px));
                drop_cut_marks(&mut prims, from, &icons, node.cut_edges);
            } else {
                paint_compiled_node(&mut prims, node, roles, theme, base_px);
            }
            prims.push(crate::widget::DrawPrim::ClipPop);
            if node.fade.is_some() {
                prims.push(crate::widget::DrawPrim::FadePop);
            }
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
    ///
    /// [`Compiler::node`] establishes it by construction (one paint node per
    /// key, and a hit region exactly when the semantic node is actionable and
    /// enabled), so the per-frame compile does not re-check it; the structural
    /// tests and the inspection fallback do.
    ///
    /// Indexed, not nested-scanned: the three per-semantic linear scans it
    /// replaces cost `S x (P + 2H)` key comparisons — quadratic in page size.
    /// One `O(P + H)` sweep builds the tally, then each actionable semantic is
    /// a single lookup. Semantics are still walked in THEIR authored order so
    /// the FIRST offending key is the one reported.
    pub(crate) fn validate_parity(&self) -> Result<(), CompileError> {
        // (paint count, hit count, first hit's action) per key. `Compiler::seen`
        // already rejects duplicate keys before anything is pushed, so a count
        // can only be 0 or 1 — but the counts are kept exact so this stays a
        // faithful restatement of the scans, not a restatement of the invariant.
        let mut index: aterm_hash::FxHashMap<&UiKey, (u32, u32, Option<&ActionId>)> =
            aterm_hash::FxHashMap::with_capacity_and_hasher(
                self.paint.len(),
                aterm_hash::FxBuildHasher,
            );
        for node in &self.paint {
            index.entry(&node.key).or_default().0 += 1;
        }
        for node in &self.hits {
            let entry = index.entry(&node.key).or_default();
            entry.1 += 1;
            // First-wins, matching the `.find(..)` this replaces.
            if entry.2.is_none() {
                entry.2 = Some(&node.action);
            }
        }
        for semantic in self.semantics.iter().filter(|node| {
            !node.offscreen && node.action.is_some() && node.state.is_none_or(|state| state.enabled)
        }) {
            // A key absent from the index tallies 0/0 and therefore fails the
            // same way the scans did.
            let (painted, hit, hit_action) = index.get(&semantic.key).copied().unwrap_or_default();
            if painted != 1 || hit != 1 {
                return Err(CompileError::ObserverMismatch(semantic.key.clone()));
            }
            if hit_action != semantic.action.as_ref() {
                return Err(CompileError::ObserverMismatch(semantic.key.clone()));
            }
        }
        Ok(())
    }
}

/// `fmt::Write` sink that streams a formatter's output straight into a hasher.
///
/// The paint fingerprint needs the COMPLETE `Debug` projection of the ordinary
/// control specs (they are value-only structs, so `Debug` is an exhaustive,
/// derive-checked field list — a new field cannot silently escape the cache
/// key). It does not need that projection to exist as a `String`, which is what
/// `format!("{content:?}").hash(..)` was buying once per paint node per frame.
struct HashFmt<'a, H: std::hash::Hasher>(&'a mut H);

impl<H: std::hash::Hasher> fmt::Write for HashFmt<'_, H> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

/// Hash a value's `Debug` rendering without allocating it.
fn hash_debug<H: std::hash::Hasher>(value: &impl fmt::Debug, hash: &mut H) {
    use fmt::Write as _;

    // Infallible: `HashFmt::write_str` never errors, and `Debug` for these
    // value-only structs never returns `Err` on its own.
    let _ = write!(HashFmt(hash), "{value:?}");
    // Terminator, so two adjacent nodes cannot alias by concatenation — the
    // same job `str`'s own `Hash` impl does with its trailing 0xff.
    hash.write_u8(0xff);
}

/// Hash one semantic node's observable fields — the exact set
/// [`CompiledUi::controls_lines`] renders, minus the `format!` per node.
fn hash_semantic_node<H: std::hash::Hasher>(node: &SemanticNode, hash: &mut H) {
    use std::hash::Hash;

    node.key.as_str().hash(hash);
    node.role.hash(hash);
    node.label.hash(hash);
    match &node.value {
        SemanticValue::None => 0_u8.hash(hash),
        SemanticValue::Text(value) => {
            1_u8.hash(hash);
            value.hash(hash);
        }
        SemanticValue::Bool(value) => {
            2_u8.hash(hash);
            value.hash(hash);
        }
        SemanticValue::Number {
            value,
            minimum,
            maximum,
        } => {
            3_u8.hash(hash);
            value.to_bits().hash(hash);
            minimum.to_bits().hash(hash);
            maximum.to_bits().hash(hash);
        }
    }
    node.state.hash(hash);
    node.action.as_ref().map(ActionId::as_str).hash(hash);
    node.rect.x.to_bits().hash(hash);
    node.rect.y.to_bits().hash(hash);
    node.rect.width.to_bits().hash(hash);
    node.rect.height.to_bits().hash(hash);
}

fn hash_text_viewport<H: std::hash::Hasher>(spec: &TextViewportSpec, hash: &mut H) {
    use std::hash::Hash;

    spec.label.hash(hash);
    spec.document_key.hash(hash);
    spec.selectable.hash(hash);
    spec.preedit.hash(hash);
    spec.preedit_caret.hash(hash);
    spec.status.hash(hash);
    spec.semantic_status.hash(hash);
    spec.minibuffer.hash(hash);
    spec.minibuffer_caret.hash(hash);
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

    // FxHash, not SipHash: this walks the whole visible editor projection
    // (up to 256 lines of up to 4 KiB) on every frame, and it is a repaint
    // cache key over first-party text, never a hash table under attack.
    let mut hash = aterm_hash::FxHasher::default();
    hash_text_viewport(spec, &mut hash);
    hash.finish() | 1
}

/// A semantic value as the audit and the control verbs print it.
pub(crate) fn semantic_value_words(value: &SemanticValue) -> String {
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
                "enabled:{} focused:{} focus-visible:{} hovered:{} pressed:{} selected:{} invalid:{} busy:{} muted:{}",
                state.enabled,
                state.focused,
                state.focus_visible,
                state.hovered,
                state.pressed,
                state.selected,
                state.invalid,
                state.busy,
                state.muted,
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
        UiContent::ColorWheel(_) => "color-wheel",
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

/// The `paint-text` audit record for one text-fit audit.
fn text_fit_line(audit: &TextFitAudit) -> String {
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
    let clip_truncated = node.clipped() || available + CLIP_EDGE_EPSILON < authored_available;
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

/// Does this [`StyleRef::Navigation`] control paint as a TOOLBAR CHIP?
///
/// `Navigation` covers two different shapes, and the painter already tells them
/// apart by exactly these two fields:
///
/// * a RAIL ROW — a leading pictogram and/or a left-aligned label, one of a
///   STACK that runs down a column of its own: the Settings rail, the compact
///   Settings category list, the markdown reader's outline. It has a rail edge,
///   because the column it is stacked in is the rail.
/// * a TOOLBAR CHIP — a `visual_label` CENTERED in a slot the AUTHOR measured
///   for that exact label (`markdown_toolbar_label_width` = the REGULAR advance
///   of the word plus a fixed pad; `compact_toolbar_leading` the same
///   idea), one of a group laid out along a single toolbar ROW. There is no rail
///   under it and no edge for anything to dock to.
///
/// This is the scope of the INDICATOR BAR, and of that alone: a 4pt full-bleed
/// bar seated at the row's leading edge is read as structure — this row is
/// docked to the rail — and on a chip it is a stripe shoved against a word. It
/// costs no width either way, so every rail row can wear it.
///
/// Whether a row can afford to grow INK is a separate and narrower question,
/// and a stack is not enough to answer it: see
/// [`navigation_is_rail_destination`].
fn navigation_is_chip(control: &Control<ButtonSpec>) -> bool {
    control.style == StyleRef::Navigation
        && control.spec.visual_icon.is_none()
        && control.spec.visual_label.is_some()
}

/// Whether a text button paints its `visual_label` CENTRED in its slot: a
/// toolbar chip ([`navigation_is_chip`]) and a filter chip ([`StyleRef::Chip`]),
/// whose authors measured the slot for the word. The painter and the fit audit
/// read this one predicate.
fn centres_its_label(control: &Control<ButtonSpec>) -> bool {
    matches!(control.style, StyleRef::Navigation | StyleRef::Chip)
        && control.spec.visual_icon.is_none()
        && control.spec.visual_label.is_some()
}

/// Is this [`StyleRef::Navigation`] control a rail DESTINATION — one of a fixed,
/// authored set of places the rail can go?
///
/// The tell is the LEADING PICTOGRAM, and it is not decoration: a glyph exists
/// only where an author sat down with a CLOSED vocabulary and drew one per
/// member. That same author wrote each member's label and sized the column the
/// set lands in, so the labels and the slot were measured against each other and
/// the slack between them is deliberate. `route_icon` and `SettingsRoute::label`
/// are that vocabulary — 16 members, longest `Keyboard & Input`, which clears
/// the narrower of the two rails by 7pt in SEMIBOLD; and the Settings rail grows
/// where a wider face would not clear it (`settings_navigation_width`).
///
/// (Other `Navigation` controls carry a pictogram — the compact toolbar's
/// leading Home/Back button, the reader's history and copy buttons — but none of
/// them is ever `selected`, so none of them reaches the one state
/// [`navigation_label_type`] spends this on. They are icon-ONLY besides: under
/// the 96pt the painter needs for a label beside a glyph, they draw the glyph
/// and nothing else.)
///
/// A `Navigation` row WITHOUT a pictogram is one of two other things, and
/// neither has that slack:
///
/// * a TOOLBAR CHIP ([`navigation_is_chip`]) — `Length::Fixed` at
///   `markdown_toolbar_label_width`, the REGULAR advance of its own word plus a
///   fixed pad. There is no slack by construction; the slot IS the word.
/// * a row carrying text the app did not AUTHOR — a markdown outline heading, a
///   link label, an image alt. `bounded_markdown_label` caps it at 32 characters
///   and the column is sized by the reader's measure, not by the words, so the
///   label arrives at the brim of its slot as a matter of course:
///   `Getting started with aterm` measures 159.7pt regular in the outline's
///   166pt slot, and 173.1pt semibold.
///
/// See [`navigation_label_type`], which spends the distinction.
fn navigation_is_rail_destination(control: &Control<ButtonSpec>) -> bool {
    control.style == StyleRef::Navigation && control.spec.visual_icon.is_some()
}

/// Whether a `Navigation` row stacked in a column (not a chip) is selected the
/// way WinUI's `NavigationView` selects one: a neutral subtle fill and a 3x16pt
/// accent pill at its leading edge, its ink and icon unchanged. Windows only:
/// there the accent is the user's own (`native_appearance::system_accent`), and
/// a row washed, rimmed, inked and barred in it read as four selections at once
/// (the 2026-10-17 audit's `accent-not-windows-accent`). The pill needs no
/// contrast trade: the label stays the primary ink. Chips keep the accent cell
/// a segmented control's selection is.
const FLUENT_RAIL_SELECTION: bool = cfg!(windows);

/// THE decision about what type a button's label is painted in — read by
/// [`paint_compiled_node`], which draws it, and by [`text_fit_audit`], which
/// measures and elides it. One function, so the repo's own paint-truth audit
/// cannot measure a different face from the one that reaches the glass.
///
/// Only ONE state moves it off the regular UI face: a SELECTED rail
/// DESTINATION. That row's ink IS the accent, and the accent is floored at
/// [`crate::native_appearance::ACCENT_INK_FLOOR`] (3.0) against its own wash —
/// a legibility guarantee, not a selection signal, which left the selected row
/// measuring as the QUIETEST row in the column. Semibold carries at any contrast
/// ratio; it just is not free, and only a destination row has the width to pay.
///
/// THE WHOLE `Navigation` + `selected` SET, and what each member gets:
///
/// * `settings/nav{path}` — the Settings rail, wide (216pt) and medium (196pt),
///   or wider where a label in SEMIBOLD needs it (the rail is sized from its
///   labels in this face). A route pictogram always, plus the full label.
///   SEMIBOLD, and this is the row the mark was measured on and repaired for.
/// * `settings/categories{path}` — the compact category list, and the
///   side-by-side pager that shows one of it at a time. Route pictogram plus the
///   same 16 authored labels. SEMIBOLD.
/// * `markdown/outline-item/{i}` — the reader's outline. A bare document
///   heading, capped at 32 characters, in a column sized for the reader. NO
///   semibold: it fits at rest and elides in bold, so the mark meant to say
///   "selected" would say it by deleting a word. It keeps the rail bar, the
///   accent wash and the accent ink — every mark that costs it no width. (No
///   row wears an outline at rest: on a row the outline is focus's alone.)
/// * `markdown/preview-mode`, `/source-mode`, `/split-mode`, `/mode-compact`,
///   `/selection-button` — toolbar chips in author-measured fixed slots. NO
///   semibold (and no rail bar): a chip is the selected cell of a segmented
///   control, and it reads as one on the wash, the rim and the ink alone.
///
/// The rest of `Navigation` is never `selected` and so never reaches this
/// branch at all: the compact Settings toolbar's leading button, and the
/// reader's link rows, image rows, history, edit, copy and paging buttons.
fn navigation_label_type(
    control: &Control<ButtonSpec>,
) -> (crate::widget::TextWeight, crate::widget::TextFace) {
    use crate::widget::{TextFace, TextWeight};

    if control.state.selected && navigation_is_rail_destination(control) {
        (TextWeight::Bold, TextFace::UiBold)
    } else {
        (TextWeight::Regular, TextFace::Ui)
    }
}

/// The width a button's label gives up to its attention badge
/// ([`ButtonSpec::badge`]): the dot and the gap before it. The painter and
/// [`text_fit_audit`] read this one number, so the audit measures the slot the
/// glass has.
fn badge_reserve(spec: &ButtonSpec) -> f32 {
    if spec.badge { 16.0 } else { 0.0 }
}

/// The room an icon navigation row's arrow takes at its end, where it has
/// one. The painter and [`text_fit_audit`] read this one number: the audit
/// used to give the label the arrow's 34pt too, so a label the painter cut
/// to "C…" was audited as fitting.
fn navigation_trailing_reserve(spec: &ButtonSpec) -> f32 {
    if spec.trailing_icon.is_some() {
        34.0
    } else {
        0.0
    }
}

/// Where a navigation destination's label starts in its row: the 6pt lead, the
/// 28pt icon slot and a 6pt gap. It stops [`NAVIGATION_LABEL_END`] short of the
/// row's end. The painter, the fit audit and the Settings rail's width read
/// these two numbers.
pub(crate) const NAVIGATION_LABEL_LEAD: f32 = 40.0;

/// How far a navigation destination's label stops short of its row's end, ahead
/// of its badge ([`NAVIGATION_LABEL_LEAD`]).
pub(crate) const NAVIGATION_LABEL_END: f32 = 12.0;

/// The width a text button's label gives up at its end, ahead of the trailing
/// slot: its attention badge ([`badge_reserve`]) and its colour chip
/// ([`ButtonSpec::chip`]) with the gap before it. The painter, the audit and
/// [`text_button_label_room`] read this one number.
fn button_end_reserve(spec: &ButtonSpec) -> f32 {
    badge_reserve(spec)
        + if spec.chip.is_some() {
            TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP
        } else {
            0.0
        }
}

/// THE LINES A TEXT BUTTON PAINTS ITS LABEL ON: the label's own lines, an
/// author's `\n` between them ([`native_button_label_lines`]); one line for a
/// label with none. Each is fitted to the button's room as a one-line label
/// is, and they stand one under another, [`native_button_line_height`]
/// apart, centred in the button.
fn button_label_lines(label: &str) -> impl Iterator<Item = &str> {
    label.split('\n')
}

/// How far apart a text button's label lines stand ([`button_label_lines`]):
/// the label's size, and a quarter of it between lines.
pub(crate) fn native_button_line_height() -> f32 {
    (native_type_px(TypeStep::Secondary).get() * 1.25).ceil()
}

/// A BUTTON'S LABEL WRAPPED TO ITS ROOM, at its words: `label` on as many
/// lines as a text button's label `room` wide needs, each as full as it holds
/// — or `None` where a word of it is wider than the room on its own, which
/// no line could hold whole. A word ends where the shared rule lets a line
/// break short of a hyphen ([`aterm_grapheme::pieces`]): at a space, and
/// between CJK characters, which go on with nothing between. A button keeps
/// its words in a narrow column at large text this way, its painted words
/// still one phrase of its name.
pub(crate) fn native_button_label_lines(label: &str, room: f32) -> Option<Vec<String>> {
    let fits = |text: &str| native_button_label_width(text) <= room + TEXT_FIT_SLACK;
    let mut lines: Vec<String> = Vec::new();
    for piece in aterm_grapheme::pieces(label, aterm_grapheme::BreakKind::is_word_boundary) {
        let word = piece.text;
        if !fits(word) {
            return None;
        }
        let joiner = if piece.spaced { " " } else { "" };
        match lines.last_mut() {
            Some(line) if fits(&format!("{line}{joiner}{word}")) => {
                line.push_str(joiner);
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    Some(lines)
}

/// The height a text button needs for `lines` lines of its label: its one
/// line's `single` height, or the lines and the padding a one-line button
/// keeps over and under its line, whichever is taller.
pub(crate) fn native_button_height_for_lines(lines: usize, single: f32) -> f32 {
    let line = native_button_line_height();
    let padding = (single - line).max(0.0);
    single.max(lines as f32 * line + padding)
}

/// The room for the label of a text button `width` wide (not a navigation
/// row): its 10pt lead, the trailing icon's slot or the 10pt end, and what
/// [`button_end_reserve`] keeps. A caller that fits a label before it paints
/// measures the slot the painter elides to.
pub(crate) fn text_button_label_room(spec: &ButtonSpec, width: f32) -> f32 {
    (width - 10.0 - text_button_trailing_reserve(spec) - button_end_reserve(spec)).max(0.0)
}

/// A text button's end: the trailing icon's slot, or the 10pt end.
fn text_button_trailing_reserve(spec: &ButtonSpec) -> f32 {
    if spec.trailing_icon.is_some() {
        38.0
    } else {
        10.0
    }
}

/// A colour chip, `TEXT_FIELD_SWATCH_SIZE` square with its top-left at
/// `(x, y)`: a rounded fill with a hairline, so a colour near the surface's
/// still reads as a chip. A colour text field and a button wear the same one.
fn paint_color_chip(
    prims: &mut Vec<crate::widget::DrawPrim>,
    (x, y): (f32, f32),
    color: [u8; 3],
    roles: crate::settings::Roles,
) {
    use crate::widget::{DrawPrim, rgba};
    prims.push(DrawPrim::Panel {
        x,
        y,
        w: TEXT_FIELD_SWATCH_SIZE,
        h: TEXT_FIELD_SWATCH_SIZE,
        radius: 6.0,
        fill: rgba(color, 255),
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

/// The attention badge: a 7pt dot in the danger role centred on `(cx, cy)`.
fn paint_badge(
    prims: &mut Vec<crate::widget::DrawPrim>,
    cx: f32,
    cy: f32,
    roles: crate::settings::Roles,
) {
    prims.push(crate::widget::DrawPrim::Dot {
        cx,
        cy,
        r: 3.5,
        color: crate::widget::rgba(roles.danger, 255),
    });
}

fn text_fit_audit(node: &PaintNode) -> Option<TextFitAudit> {
    // A node whose words the edge of a kept caption cuts through paints none
    // of them ([`drop_cut_marks`]): it has no words to fit.
    if node.cut_edges.any()
        && !node
            .painted_prims()
            .iter()
            .any(|prim| matches!(prim, crate::widget::DrawPrim::Text { .. }))
    {
        return None;
    }
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
            // Measure the face that is PAINTED. A selected rail row wears
            // semibold, a different font with wider advances, so an audit that
            // measured the regular metrics would report a fit the painter never
            // draws — the one row that grew ink is exactly the row this audit
            // exists to catch overflowing.
            let (_, face) = navigation_label_type(control);
            let size = native_type_px(TypeStep::Secondary).get();
            let (label, text_x, text_right) = if control.spec.visual_icon.is_some() {
                if navigation && node.rect.width >= 96.0 {
                    (
                        control
                            .spec
                            .visual_label
                            .as_ref()
                            .unwrap_or(&control.spec.label),
                        node.rect.x + NAVIGATION_LABEL_LEAD,
                        node.rect.right()
                            - NAVIGATION_LABEL_END
                            - navigation_trailing_reserve(&control.spec)
                            - badge_reserve(&control.spec),
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
                // A disclosure's label ends at its chevron's slot.
                let trailing_reserve = if control.state.expanded.is_some()
                    && control.style == StyleRef::Plain
                {
                    DISCLOSURE_LABEL_END
                } else {
                    text_button_trailing_reserve(&control.spec) + button_end_reserve(&control.spec)
                };
                // A label on more lines than one is audited line by line:
                // painted is every line as the painter fits it, required the
                // widest, the room the narrowest any line has.
                let text_right = node.rect.right() - trailing_reserve;
                let mut painted = Vec::new();
                // The widest line's measure, and each room's slack over its
                // own line: a centred short line starts late and has less
                // room, which it does not need. The tightest slack is what
                // the label has to spare.
                let (mut required, mut authored_slack, mut slack) =
                    (0.0_f32, f32::INFINITY, f32::INFINITY);
                for line in button_label_lines(label) {
                    let width = crate::tray_raster::ui_text_width_for(face, line, size);
                    let text_x = if centres_its_label(control) {
                        node.rect.x + ((node.rect.width - width) / 2.0).max(0.0)
                    } else {
                        node.rect.x + if navigation { 12.0 } else { BUTTON_LABEL_INSET }
                    };
                    let room = (text_right - text_x).max(0.0);
                    painted.push(elide_text_label(line, room, size, face));
                    required = required.max(width);
                    authored_slack = authored_slack.min(room - width);
                    slack = slack.min(effective_horizontal_width(node, text_x, text_right) - width);
                }
                return Some(finish_text_fit(
                    node,
                    "button",
                    label,
                    painted.join("\n"),
                    required,
                    (required + authored_slack).max(0.0),
                    (required + slack).max(0.0),
                ));
            };
            let authored_available = (text_right - text_x).max(0.0);
            let available = effective_horizontal_width(node, text_x, text_right);
            let painted = elide_text_label(label, authored_available, size, face);
            Some(finish_text_fit(
                node,
                "button",
                label,
                painted,
                crate::tray_raster::ui_text_width_for(face, label, size),
                authored_available,
                available,
            ))
        }
        // A plain switch paints no word (design ruling 262).
        UiContent::Switch(control) if control.style == StyleRef::Quiet => None,
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
            let required = slider_readout_width(&control.spec.display_value, size);
            let text_x = (geometry.value_right - required).max(geometry.value_left);
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
                fitted_placeholder(control, &geometry, size)
            } else {
                let shown = &actual[geometry.visible.clone()];
                match geometry.ellipsis {
                    TextFieldEllipsis::None => shown.to_string(),
                    TextFieldEllipsis::Lead => format!("{TEXT_FIELD_ELLIPSIS}{shown}"),
                    TextFieldEllipsis::Trail => format!("{shown}{TEXT_FIELD_ELLIPSIS}"),
                    TextFieldEllipsis::Middle(head) => {
                        format!("{}{TEXT_FIELD_ELLIPSIS}{shown}", &actual[..head])
                    }
                }
            };
            // A declared ellipsis is deliberate fitting, as a fitted button
            // label is: the field is held to the words it chose to paint.
            let (source, field_x) = if geometry.ellipsis == TextFieldEllipsis::None {
                (source, geometry.text_x)
            } else {
                let mark = crate::tray_raster::ui_text_width(TEXT_FIELD_ELLIPSIS, size);
                let field_x = match geometry.ellipsis {
                    TextFieldEllipsis::Lead => geometry.text_x - mark,
                    _ => geometry.text_x,
                };
                (painted.clone(), field_x)
            };
            let authored_available = (geometry.text_right - field_x).max(0.0);
            let available = effective_horizontal_width(node, field_x, geometry.text_right);
            // A placeholder set on lines is as wide as its widest line.
            let required = source
                .split('\n')
                .map(|line| crate::tray_raster::ui_text_width(line, size))
                .fold(0.0_f32, f32::max);
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
            // A cut on purpose: a scrolling list's edge ([`UiNode::room`]).
            let visible_height = if node.peek {
                node.rect.height
            } else {
                node.clip.height.min(node.rect.height)
            };
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
            let available = effective_horizontal_width(node, node.rect.x, node.rect.right());
            let required = if spec.audit_id == DISPLAY_FACE_NAME_AUDIT {
                let (size, face) = display_face_name_typography(spec, node.rect.width);
                crate::tray_raster::ui_text_width_for(face, &spec.label, size.get())
            } else if spec.audit_id == WORDMARK_AUDIT {
                let (size, face) = wordmark_typography(&spec.label, node.rect.width);
                crate::tray_raster::ui_text_width_for(face, &spec.label, size.get())
            } else {
                crate::tray_raster::ui_text_width(&spec.label, native_type_px(TypeStep::Body).get())
            };
            Some(finish_text_fit(
                node,
                "custom",
                &spec.label,
                spec.label.clone(),
                required,
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

fn hash_markdown_kind<H: std::hash::Hasher>(kind: &MarkdownBlockKind, hash: &mut H) {
    use std::hash::Hash;

    match kind {
        MarkdownBlockKind::Heading(level) => {
            0_u8.hash(hash);
            level.hash(hash);
        }
        MarkdownBlockKind::Paragraph => 1_u8.hash(hash),
        MarkdownBlockKind::ListItem { depth, ordinal } => {
            2_u8.hash(hash);
            depth.hash(hash);
            ordinal.hash(hash);
        }
        MarkdownBlockKind::Quote => 3_u8.hash(hash),
        MarkdownBlockKind::Code { language } => {
            4_u8.hash(hash);
            language.hash(hash);
        }
        MarkdownBlockKind::Table => 5_u8.hash(hash),
        MarkdownBlockKind::Rule => 6_u8.hash(hash),
    }
}

fn markdown_paint_fingerprint(node: &PaintNode, spec: &MarkdownBlockSpec) -> u64 {
    use std::hash::{Hash, Hasher};

    // FxHash, not SipHash: `spec.text` is up to 128 KiB of block source and this
    // runs per visible block per frame.
    let mut hash = aterm_hash::FxHasher::default();
    node.rect.x.to_bits().hash(&mut hash);
    node.rect.y.to_bits().hash(&mut hash);
    node.rect.width.to_bits().hash(&mut hash);
    node.rect.height.to_bits().hash(&mut hash);
    node.clip.x.to_bits().hash(&mut hash);
    node.clip.y.to_bits().hash(&mut hash);
    node.clip.width.to_bits().hash(&mut hash);
    node.clip.height.to_bits().hash(&mut hash);
    // The typed kind, not its `markdown_kind_label(..)` `String`: one fewer
    // heap allocation per visible block per frame, and strictly more
    // discriminating (the label collapses `Code { language: None }` and
    // `Code { language: Some("") }` to the same "code:plain" text).
    hash_markdown_kind(&spec.kind, &mut hash);
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
    for (span, style) in &spec.inline {
        span.start.hash(&mut hash);
        span.end.hash(&mut hash);
        style.hash(&mut hash);
    }
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
    let footer_available = effective_horizontal_width(
        node,
        node.rect.x + EDITOR_FOOTER_TEXT_INSET,
        node.rect.right(),
    );
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
        node.clipped() || visible_rows < painted_rows,
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
    text_typography_for(spec.role, spec.style)
}

/// The size and face the painter will draw a [`TextSpec`] of this role and
/// style at.
///
/// Copy that an author WRAPS has to be measured at exactly these metrics.
/// Wrapping at any other size hands the painter a line it must then elide,
/// which is the whole failure [`text_fit_audit`] reports as `overflow=true` —
/// the reason this pair is published instead of re-derived by each author.
pub(crate) fn text_paint_metrics(
    role: SemanticRole,
    style: StyleRef,
) -> (f32, crate::widget::TextFace) {
    let (step, face) = text_typography_for(role, style);
    (native_type_px(step).get(), face)
}

fn text_typography_for(role: SemanticRole, style: StyleRef) -> (TypeStep, crate::widget::TextFace) {
    use crate::widget::TextFace;

    match role {
        SemanticRole::Heading if style == StyleRef::Hero => (TypeStep::Display, TextFace::UiBold),
        SemanticRole::Heading if matches!(style, StyleRef::Quiet | StyleRef::Accent) => {
            (TypeStep::Caption, TextFace::UiBold)
        }
        // Compact native pages use Plain for a large-type heading: Body-bold
        // remains visibly hierarchical at the platform maximum without
        // forcing route names to ellipsize in a 320-point host.
        SemanticRole::Heading if matches!(style, StyleRef::Plain | StyleRef::DangerCompact) => {
            (TypeStep::Body, TextFace::UiBold)
        }
        SemanticRole::Heading => (TypeStep::Title, TextFace::UiBold),
        // Settings ▸ Messages' technical details (design ruling 262): small
        // monospace, secondary — a status-sized code line.
        SemanticRole::Status if style == StyleRef::Code => (TypeStep::Caption, TextFace::Mono),
        _ if style == StyleRef::Code => (TypeStep::Body, TextFace::Mono),
        SemanticRole::Status if style == StyleRef::Primary => (TypeStep::Body, TextFace::Ui),
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
                });
                // A Hero application surface glows like the terminal it
                // configures: a dim diagonal spectrum wash over the whole
                // canvas, with the opaque cards and rail floating on it. The
                // view asks for it with the style (Settings does, except under
                // Serious Mode); it yields to the high-contrast preference,
                // where every point of text contrast belongs to the reader.
                // The wash is static — retained-raster cached. The ink audit
                // ([`CompiledUi::ink_overlaps`], while [`ICON_SPANS`]
                // collects) paints none: its tiles are opaque fills under
                // every mark, never one, and a whole grid of them a screen.
                if spec.style == StyleRef::Hero
                    && !crate::native_appearance::current_preferences().high_contrast
                    && !ICON_SPANS.with(|spans| spans.borrow().is_some())
                {
                    crate::settings::paint_settings_aurora(
                        prims,
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                        roles.surface,
                        quiet_text(&roles),
                    );
                }
            } else if spec.role == SemanticRole::Navigation {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 0.0,
                    fill: rgba(roles.elevated, 245),
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
                // A one-pixel inner light along the top edge is the whole nod
                // to dimensionality — a material sheen, not an elevation lie.
                let hero = spec.style == StyleRef::Primary;
                // A piece of a card that runs on is drawn as the card is,
                // its open edges carried past a clip at its own bounds (and
                // the gap to the next piece), so no edge closes it there.
                let (rect, joined) = open_box(prims, rect, node.joins, 12.0);
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 12.0,
                    fill: rgba(
                        if hero {
                            mix_rgb(roles.elevated, roles.accent, 0.10)
                        } else {
                            roles.elevated
                        },
                        255,
                    ),
                });
                prims.push(DrawPrim::Stroke {
                    x: rect.x + 0.5,
                    y: rect.y + 0.5,
                    w: (rect.width - 1.0).max(0.0),
                    h: (rect.height - 1.0).max(0.0),
                    radius: 12.0,
                    width: 1.0,
                    color: rgba(if hero { roles.accent } else { roles.separator }, 150),
                });
                if !node.joins.above {
                    prims.push(DrawPrim::Panel {
                        x: rect.x + 14.0,
                        y: rect.y + 1.0,
                        w: (rect.width - 28.0).max(0.0),
                        h: 1.0,
                        radius: 0.5,
                        fill: rgba(roles.text_primary, 14),
                    });
                }
                if joined {
                    prims.push(DrawPrim::ClipPop);
                }
            } else if spec.style == StyleRef::Code {
                // A terminal/editor canvas should read as a working surface
                // inside the surrounding card, not as another generic card.
                // Keep it theme-derived and deliberately a step quieter than
                // `surface`; the sample text supplies the visual hierarchy.
                let (rect, joined) = open_box(prims, rect, node.joins, 10.0);
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 10.0,
                    fill: rgba(mix_rgb(roles.surface, roles.elevated, 0.12), 255),
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
                if joined {
                    prims.push(DrawPrim::ClipPop);
                }
            }
        }
        UiContent::Text(spec) => {
            let (step, face) = text_typography(spec);
            let color = match spec.role {
                SemanticRole::Heading if spec.style == StyleRef::Quiet => {
                    readable_secondary(&roles)
                }
                SemanticRole::Heading if spec.style == StyleRef::Accent => roles.accent_ink,
                // A warning heading is the warning's colour at every step: the
                // Heading arm below used to take it, so "Can't update" and
                // "Not applying" painted in the body's ink at every size.
                SemanticRole::Heading
                    if matches!(spec.style, StyleRef::Danger | StyleRef::DangerCompact) =>
                {
                    roles.danger
                }
                SemanticRole::Heading => roles.text_primary,
                SemanticRole::Status if spec.style == StyleRef::Code => readable_secondary(&roles),
                _ if spec.style == StyleRef::Code => roles.text_primary,
                SemanticRole::Status if spec.style == StyleRef::Success => roles.success,
                SemanticRole::Status
                    if matches!(spec.style, StyleRef::Danger | StyleRef::DangerCompact) =>
                {
                    roles.danger
                }
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
        UiContent::Button(control)
            if control.state.expanded.is_some() && control.style == StyleRef::Plain =>
        {
            paint_disclosure_button(prims, rect, control, &roles);
        }
        UiContent::Button(control) => {
            // The style the surface is painted in ([`button_paint_style`]).
            let style = button_paint_style(control);
            let primary = style == StyleRef::Primary;
            // A destructive action (Reset All, Discard): a danger-tinted
            // button with a danger rim and label.
            let danger = style == StyleRef::Danger;
            let selected = control.state.selected;
            let navigation = style == StyleRef::Navigation;
            let quiet = style == StyleRef::Quiet;
            // A filter chip ([`StyleRef::Chip`]): selected is a solid accent
            // fill, resting the card's fill in its control edge.
            let chip = style == StyleRef::Chip;
            let chip_selected = chip && selected;
            // A button that cannot be pressed LOOKS inert, not only grey-texted:
            // its surface and rim sink toward the background, so a disabled
            // Save beside a live Find reads as a button at rest, never as an
            // empty field with a placeholder. A busy one (Save while saving)
            // keeps its surface: it is working, not unavailable.
            let inert = style == StyleRef::Secondary
                && !control.state.enabled
                && !control.state.busy
                && !control.state.focus_visible;
            // HOW THE SELECTED RAIL ROW SPELLS "SELECTED".
            //
            // Its ink IS the accent, and the accent is floored at
            // [`crate::native_appearance::ACCENT_INK_FLOOR`] (3.0) against the
            // wash it is inked on. That floor is a LEGIBILITY guarantee, not a
            // selection signal: on the shipping forced palettes it settles the
            // selected row at 3.03:1 (forced-light) / 3.12:1 (forced-dark) while
            // its unselected siblings sit near 10:1 on the same rail — so the
            // selected row measured as the QUIETEST row in the column and read
            // as "disabled", the exact inversion of what it means.
            //
            // Raising that floor to 4.5 is the repair a previous pass already
            // rejected, and rightly: the accent is one global identity colour
            // and pushing it there pastels it everywhere it lands on dark
            // chrome. So the answer is local and structural — selection is
            // spelled by things that carry at ANY contrast ratio: SEMIBOLD ink,
            // which thickens every stem the way a native sidebar's selected row
            // does, and a real rail indicator seated against the rail edge
            // instead of a 3pt stub floating inside it. The accent identity, the
            // wash, and the floor are all untouched.
            //
            // SCOPE, and it is not one scope but three, because the marks do not
            // all cost the same thing.
            //
            // The WASH, the accent INK and the RIM cost nothing but colour, so
            // every selected `Navigation` control wears them; on a toolbar chip
            // the three of them together are exactly a segmented control's
            // selected cell.
            //
            // The BAR costs no width either, but it asserts a rail edge to dock
            // to, so it goes to every row that is stacked in a column of its own
            // and to no chip ([`navigation_is_chip`]) — a 4pt full-bleed stripe
            // shoved against a word in a toolbar reads as damage.
            //
            // SEMIBOLD costs WIDTH, in a slot that was measured in the regular
            // face, so it goes only where an author left slack to spend: a rail
            // DESTINATION, whose label and whose column were sized against each
            // other out of one closed vocabulary ([`navigation_label_type`] names
            // the whole set). A row carrying a document heading is stacked in the
            // same shape and has no such slack — it arrives at the brim of its
            // slot at rest — so growing its ink would spell "selected" by
            // deleting a word, which is not a selection mark but a loss.
            let nav_selected = navigation && selected;
            let nav_selected_row = nav_selected && !navigation_is_chip(control);
            // A row stacked in a column ([`FLUENT_RAIL_SELECTION`]): its states
            // are neutral fills, and a selected one wears the pill, not the
            // accent wash, ink and bar described above.
            let fluent_row = FLUENT_RAIL_SELECTION && navigation && !navigation_is_chip(control);
            let (nav_weight, nav_face) = navigation_label_type(control);
            // IDLE ([`ControlState::muted`]): what the control sets does nothing
            // right now (a Harness row while supervision is off), so every mark
            // this button would paint in the accent — the selected segment's
            // wash, rim and ink, a pressed wash, a rail bar — is painted in the
            // quiet ink the idle switch's track wears instead. The chosen cell
            // still reads as chosen (its wash and rim stay, in grey, as a
            // segmented control in an inactive macOS window), never as live.
            // The control stays operable, so its words stay legible
            // ([`control_text_color`]), and a focus ring stays the accent:
            // focus is the keyboard's, live either way.
            let idle = control.state.muted && control.state.enabled;
            let mark = if idle {
                roles.text_tertiary
            } else {
                roles.accent
            };
            // The two accent-tinted states a SELECTED navigation row can be in
            // paint the accent as their own ink, so their mix amounts are the
            // ones `native_appearance` floors that accent against.
            let fill = if chip_selected {
                // A luminance step from a resting chip, not only a hue: the
                // accent itself, a little deeper under the pointer or a press.
                if control.state.pressed || control.state.hovered {
                    mix_rgb(mark, roles.text_primary, 0.12)
                } else {
                    mark
                }
            } else if chip && !control.state.hovered && !control.state.pressed {
                roles.elevated
            } else if primary {
                mix_rgb(roles.elevated, mark, 0.20)
            } else if danger && !control.state.pressed {
                mix_rgb(
                    roles.elevated,
                    roles.danger,
                    if control.state.hovered { 0.24 } else { 0.16 },
                )
            } else if fluent_row && (selected || control.state.pressed) {
                // WinUI's subtle fills: a selected row rests at the hover step
                // (its pill tells the two apart), steps up under the pointer,
                // and a press is lighter than either.
                let step = if control.state.pressed {
                    0.05
                } else if control.state.hovered {
                    0.12
                } else {
                    0.09
                };
                mix_rgb(roles.elevated, roles.text_primary, step)
            } else if control.state.pressed {
                mix_rgb(
                    roles.elevated,
                    mark,
                    crate::native_appearance::ACCENT_WASH_PRESSED,
                )
            } else if selected {
                mix_rgb(
                    roles.elevated,
                    mark,
                    crate::native_appearance::ACCENT_WASH_SELECTED,
                )
            } else if control.state.hovered {
                mix_rgb(roles.elevated, roles.text_primary, 0.09)
            } else if navigation {
                // Rail rows rest flush with the rail; only their states paint.
                roles.elevated
            } else {
                control_rest_fill(&roles)
            };
            if !navigation || selected || control.state.hovered || control.state.pressed {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y,
                    w: rect.width,
                    h: rect.height,
                    radius: 8.0,
                    fill: rgba(
                        fill,
                        if quiet {
                            72
                        } else if inert {
                            110
                        } else {
                            255
                        },
                    ),
                });
            }
            if primary
                || danger
                || chip
                || control.state.focus_visible
                || control.state.invalid
                || (nav_selected && !nav_selected_row)
                || matches!(style, StyleRef::Secondary | StyleRef::Setting)
            {
                // ON A ROW, THE OUTLINE MEANS FOCUS ALONE: a selected rail or
                // list row is spelled by its wash, its docked accent bar and
                // its accent ink, as a macOS sidebar's is, and wears no rim at
                // rest — so the keyboard's ring, wherever it is, is the only
                // outline on the rail. A selected segment chip keeps a cell rim
                // in the accent hue, framing the chosen cell inside its group's
                // own outline (on a chip focus and selection move together).
                // An invalid value (a font that was not found) is marked the
                // way an invalid text field is, in the danger hue.
                let (rim, rim_alpha) = if control.state.invalid {
                    (roles.danger, 220)
                } else if control.state.focus_visible {
                    (roles.accent, 255)
                } else if chip_selected {
                    (mark, 255)
                } else if chip {
                    (roles.control_edge, 255)
                } else if primary {
                    (mark, 176)
                } else if danger {
                    (roles.danger, 200)
                } else if nav_selected {
                    // The chosen segment's cell rim.
                    (mark, 150)
                } else if inert {
                    (roles.separator, 96)
                } else {
                    (roles.separator, 176)
                };
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
                    color: rgba(rim, rim_alpha),
                });
            }
            if control.state.focus_visible
                && ((nav_selected && !nav_selected_row) || chip_selected || primary || danger)
            {
                // A selected segment or chip, default or destructive control
                // already rests in a coloured rim or fill, so the 2pt accent
                // ring alone barely changes it. An inner ring in the label
                // colour makes keyboard focus on it as plain as on any other
                // control. A selected row rests in no rim, so its ring is plain
                // on its own.
                prims.push(DrawPrim::Stroke {
                    x: rect.x + 3.5,
                    y: rect.y + 3.5,
                    w: (rect.width - 7.0).max(0.0),
                    h: (rect.height - 7.0).max(0.0),
                    radius: 5.0,
                    width: 1.5,
                    color: rgba(roles.text_primary, 255),
                });
            }
            if nav_selected_row && fluent_row {
                // WinUI's selection pill: 3pt by 16pt, rounded, at the row's
                // leading edge, centred; a short dense row keeps 4pt above and
                // below it.
                let h = 16.0_f32.min((rect.height - 8.0).max(0.0));
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y + (rect.height - h) / 2.0,
                    w: 3.0,
                    h,
                    radius: 1.5,
                    fill: rgba(mark, 255),
                });
            } else if nav_selected_row {
                // The rail indicator. 4pt and nearly full-bleed: a bar this
                // shape is read as structure (the row is docked to the rail's
                // edge) rather than as decoration, and structure survives the
                // accent's floor being only 3:1 against its own wash.
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y + 4.0,
                    w: 4.0,
                    h: (rect.height - 8.0).max(0.0),
                    radius: 2.0,
                    fill: rgba(mark, 255),
                });
            }
            let color = if chip_selected && control.state.enabled {
                crate::native_appearance::ensure_text_contrast(roles.on_accent, fill)
            } else if nav_selected && !idle && !fluent_row {
                // The accent as text: legible on its own wash, as every
                // label is ([`crate::native_appearance::NativeRoles::accent_ink`]).
                // A Fluent row keeps its ink; the pill says selected.
                roles.accent_ink
            } else if danger && control.state.enabled {
                crate::native_appearance::ensure_text_contrast(roles.danger, fill)
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
                    // A row that goes somewhere ends in its arrow.
                    let trailing_reserve = navigation_trailing_reserve(&control.spec);
                    // Measured on the face it is PAINTED in: the semibold a
                    // selected row wears is a different font with different
                    // advances, so eliding against the regular metrics would
                    // let the one row that grew ink overflow its own slot.
                    let label = elide_text_label(
                        visual_label,
                        (rect.width
                            - NAVIGATION_LABEL_LEAD
                            - NAVIGATION_LABEL_END
                            - trailing_reserve
                            - badge_reserve(&control.spec))
                        .max(0.0),
                        size.get(),
                        nav_face,
                    );
                    prims.push(text_prim(
                        rect.x + NAVIGATION_LABEL_LEAD,
                        row_baseline(rect.y, rect.height, size.get()),
                        label,
                        size,
                        nav_weight,
                        nav_face,
                        rgba(color, 255),
                    ));
                    if control.spec.badge {
                        paint_badge(
                            prims,
                            rect.right() - 18.0 - trailing_reserve,
                            rect.y + rect.height / 2.0,
                            roles,
                        );
                    }
                    if let Some(icon) = control.spec.trailing_icon {
                        let icon_rect = LogicalRect::new(
                            (rect.right() - 34.0).max(rect.x),
                            rect.y,
                            28.0_f32.min(rect.width),
                            rect.height,
                        );
                        paint_button_icon(prims, icon_rect, icon, color);
                    }
                } else {
                    paint_button_icon(prims, rect, icon, color);
                    if control.spec.badge {
                        // On the pictogram's shoulder: an icon-only row has no end.
                        paint_badge(
                            prims,
                            rect.x + rect.width / 2.0 + 9.0,
                            rect.y + rect.height / 2.0 - 9.0,
                            roles,
                        );
                    }
                }
            } else {
                let size = native_type_px(TypeStep::Secondary);
                let label = control
                    .spec
                    .visual_label
                    .as_ref()
                    .unwrap_or(&control.spec.label);
                let trailing_reserve = text_button_trailing_reserve(&control.spec);
                // Each painted line of the label ([`button_label_lines`]):
                // where it starts, and its words fitted to the room after it.
                let painted_lines = button_label_lines(label)
                    .map(|line| {
                        let text_x = if centres_its_label(control) {
                            let width =
                                crate::tray_raster::ui_text_width_for(nav_face, line, size.get());
                            rect.x + ((rect.width - width) / 2.0).max(0.0)
                        } else {
                            rect.x + if navigation { 12.0 } else { BUTTON_LABEL_INSET }
                        };
                        let painted = elide_text_label(
                            line,
                            (rect.right()
                                - trailing_reserve
                                - button_end_reserve(&control.spec)
                                - text_x)
                                .max(0.0),
                            size.get(),
                            nav_face,
                        );
                        (text_x, painted)
                    })
                    .collect::<Vec<_>>();
                if control.spec.badge {
                    paint_badge(
                        prims,
                        rect.right() - trailing_reserve - 8.0,
                        rect.y + rect.height / 2.0,
                        roles,
                    );
                }
                // The colour the button names, as its field would show it:
                // after the label, ahead of the badge and the trailing icon.
                // A button that paints no words and no icon is a colour well,
                // its chip centred in it.
                if let Some(color) = control.spec.chip {
                    let well = label.is_empty()
                        && control.spec.trailing_icon.is_none()
                        && !control.spec.badge;
                    let x = if well {
                        rect.x + (rect.width - TEXT_FIELD_SWATCH_SIZE) / 2.0
                    } else {
                        rect.right()
                            - trailing_reserve
                            - badge_reserve(&control.spec)
                            - TEXT_FIELD_SWATCH_SIZE
                    };
                    paint_color_chip(
                        prims,
                        (x, rect.y + (rect.height - TEXT_FIELD_SWATCH_SIZE) / 2.0),
                        color,
                        roles,
                    );
                }
                // A colour strip takes the bottom of the row; the label is
                // centred in what is left above it, its lines one under
                // another ([`native_button_line_height`]).
                let strip = paint_button_swatches(prims, rect, &control.spec.swatches, roles);
                let line_height = if painted_lines.len() > 1 {
                    native_button_line_height()
                } else {
                    rect.height - strip
                };
                let top = rect.y
                    + ((rect.height - strip - line_height * painted_lines.len() as f32) / 2.0)
                        .max(0.0);
                for (index, (text_x, painted)) in painted_lines.into_iter().enumerate() {
                    prims.push(text_prim(
                        text_x,
                        row_baseline(top + index as f32 * line_height, line_height, size.get()),
                        painted,
                        size,
                        nav_weight,
                        nav_face,
                        rgba(color, 255),
                    ));
                }
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
            // A PLAIN switch (`StyleRef::Quiet`, design ruling 262): the track and
            // the thumb alone, flush right in its slot — no boxed surface and no
            // `On` word beside it, a focus ring only when the keyboard is on it.
            // On Windows no switch is boxed ([`FLUENT_SWITCH`]).
            let plain = control.style == StyleRef::Quiet;
            if plain || FLUENT_SWITCH {
                if control.state.focus_visible {
                    prims.push(DrawPrim::Stroke {
                        x: rect.x + 0.5,
                        y: rect.y + 0.5,
                        w: (rect.width - 1.0).max(0.0),
                        h: (rect.height - 1.0).max(0.0),
                        radius: 7.0,
                        width: 2.0,
                        color: rgba(roles.accent, 255),
                    });
                }
            } else {
                paint_control_surface(prims, rect, control.state, roles);
            }
            let on = matches!(control.value, SemanticValue::Bool(true));
            let enabled = control.state.enabled;
            if FLUENT_SWITCH {
                paint_fluent_switch(prims, rect, control.state, on, roles);
                if plain {
                    return;
                }
                // The word sits just left of the track, as Windows Settings sets it.
                let size = native_type_px(TypeStep::Secondary);
                let word = if on { "On" } else { "Off" };
                let width = crate::tray_raster::ui_text_width_for(TextFace::Ui, word, size.get());
                let right = rect.right() - 8.0 - FLUENT_TRACK.0 - 12.0;
                prims.push(text_prim(
                    (right - width).max(rect.x + 10.0),
                    row_baseline(rect.y, rect.height, size.get()),
                    word.to_string(),
                    size,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(control_text_color(control.state, &roles), 255),
                ));
                return;
            }
            let track_w = 42.0_f32.min((rect.width - 8.0).max(0.0));
            let track_h = 22.0_f32.min((rect.height - 8.0).max(0.0));
            // A switch that cannot be operated must LOOK inert: the track
            // fill sinks toward the surface and the thumb dims to the same
            // subordinate tone `control_text_color` gives the caption.
            // Full-strength accent on a disabled switch left the off-macOS
            // Updates toggle pixel-identical to a live one (2026-08 settings
            // audit).
            prims.push(DrawPrim::Capsule {
                x: rect.right() - track_w - 8.0,
                y: rect.y + (rect.height - track_h) / 2.0,
                w: track_w,
                h: track_h,
                frac: if on { 1.0 } else { 0.0 },
                fill: rgba(
                    if enabled && !control.state.muted {
                        roles.accent
                    } else if enabled {
                        // Idle: the switch still says On, in quiet ink.
                        roles.text_tertiary
                    } else {
                        mix_rgb(roles.accent, roles.elevated, 0.55)
                    },
                    if enabled { 255 } else { 150 },
                ),
                track: rgba(roles.control_track, if enabled { 180 } else { 90 }),
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
                    if !enabled {
                        mix_rgb(roles.text_tertiary, roles.text_secondary, 0.35)
                    } else if on {
                        roles.on_accent
                    } else {
                        roles.text_secondary
                    },
                    if enabled { 255 } else { 170 },
                ),
            });
            let size = native_type_px(TypeStep::Secondary);
            if plain {
                return;
            }
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
            // The bar draws the track ([`SliderSpec::track`]); a value past
            // its end fills it to that end.
            let fraction = match control.value {
                SemanticValue::Number {
                    value,
                    minimum,
                    maximum,
                } => {
                    let (low, high) = control.spec.track_span(minimum, maximum);
                    if high > low {
                        ((value - low) / (high - low)).clamp(0.0, 1.0) as f32
                    } else {
                        0.0
                    }
                }
                _ => 0.0,
            };
            let geometry = slider_geometry(rect);
            // A slider that cannot move fills in the disabled ink, so it does
            // not read as live; one busy with its own work keeps the accent.
            let fill = if control.state.muted && control.state.enabled {
                // Idle: the slider still shows its value, in quiet ink.
                roles.text_tertiary
            } else if control.state.enabled || control.state.busy {
                roles.accent
            } else {
                control_text_color(control.state, &roles)
            };
            prims.push(DrawPrim::Capsule {
                x: geometry.track_x,
                y: geometry.track_y - 3.0,
                w: (geometry.track_right - geometry.track_x).max(0.0),
                h: 6.0,
                frac: fraction,
                fill: rgba(fill, 255),
                track: rgba(roles.control_track, 180),
            });
            // The number draws in the UI face it is measured in, as a switch's
            // On/Off does: never the terminal font, which a display face can
            // turn playful.
            let size = native_type_px(TypeStep::Caption);
            let width = slider_readout_width(&control.spec.display_value, size.get());
            let (band_top, band_height) = geometry.value_band;
            prims.push(text_prim(
                (geometry.value_right - width).max(geometry.value_left),
                row_baseline(band_top, band_height, size.get()),
                control.spec.display_value.clone(),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(control_text_color(control.state, &roles), 255),
            ));
        }
        UiContent::TextField(control) => {
            paint_control_surface(prims, rect, control.state, roles);
            let size = native_type_px(TypeStep::Secondary);
            let geometry = text_field_geometry(control, rect, size.get());
            let actual = text_field_visual_text(control);
            // An empty field's placeholder is fitted to its text box, never
            // cut mid-glyph ([`fitted_placeholder`]); the audit still says
            // where authoring did not fit it.
            let placeholder;
            let (value, quiet) = if actual.is_empty() {
                placeholder = fitted_placeholder(control, &geometry, size.get());
                (placeholder.as_str(), true)
            } else {
                (
                    &actual[geometry.visible.start.min(actual.len())
                        ..geometry.visible.end.min(actual.len())],
                    false,
                )
            };
            // A cut value says so: "…" before a path's end, after anything
            // else's start ([`TextFieldGeometry::ellipsis`]).
            let value = match geometry.ellipsis {
                TextFieldEllipsis::None => value.to_string(),
                TextFieldEllipsis::Lead => format!("{TEXT_FIELD_ELLIPSIS}{value}"),
                TextFieldEllipsis::Trail => format!("{value}{TEXT_FIELD_ELLIPSIS}"),
                TextFieldEllipsis::Middle(head) => format!(
                    "{}{TEXT_FIELD_ELLIPSIS}{value}",
                    &actual[..head.min(actual.len())]
                ),
            };
            let value_x = match geometry.ellipsis {
                TextFieldEllipsis::Lead => {
                    geometry.text_x
                        - crate::tray_raster::ui_text_width(TEXT_FIELD_ELLIPSIS, size.get())
                }
                _ => geometry.text_x,
            };
            prims.push(DrawPrim::ClipPush {
                x: value_x,
                y: rect.y + 2.0,
                w: (geometry.text_right - value_x).max(0.0),
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
                    });
                }
            }
            // A placeholder its author set on lines paints them one under
            // another, centred in the field ([`fit_row_field_placeholder`]).
            let value_lines = value.split('\n').map(str::to_string).collect::<Vec<_>>();
            let line_height = if value_lines.len() > 1 {
                native_button_line_height()
            } else {
                rect.height
            };
            let top =
                rect.y + ((rect.height - line_height * value_lines.len() as f32) / 2.0).max(0.0);
            for (index, line) in value_lines.into_iter().enumerate() {
                prims.push(text_prim(
                    value_x,
                    row_baseline(top + index as f32 * line_height, line_height, size.get()),
                    line,
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
            }
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
                let caret = text_field_caret_rect(input, &geometry, rect, size.get());
                prims.push(DrawPrim::Stroke {
                    x: caret.x,
                    y: caret.y,
                    w: caret.width,
                    h: caret.height,
                    radius: 0.0,
                    width: 1.0,
                    color: rgba(roles.accent, 255),
                });
            }
            prims.push(DrawPrim::ClipPop);
            if let Some(hint) = control.spec.trailing_hint.as_deref() {
                let caption = native_type_px(TypeStep::Caption);
                let width = crate::tray_raster::ui_text_width(hint, caption.get());
                let swatch_space = if control.spec.swatch.is_some() {
                    TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP
                } else {
                    0.0
                };
                let button_space = if control.spec.end_button {
                    TEXT_FIELD_END_BUTTON + TEXT_FIELD_END_BUTTON_GAP
                } else {
                    0.0
                };
                prims.push(text_prim(
                    rect.right()
                        - TEXT_FIELD_HORIZONTAL_PADDING
                        - swatch_space
                        - button_space
                        - width,
                    row_baseline(rect.y, rect.height, caption.get()),
                    hint.to_string(),
                    caption,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(quiet_text(&roles), 255),
                ));
            }
            if let Some(color) = control.spec.swatch {
                paint_color_chip(
                    prims,
                    (
                        rect.right() - TEXT_FIELD_HORIZONTAL_PADDING - TEXT_FIELD_SWATCH_SIZE,
                        rect.y + (rect.height - TEXT_FIELD_SWATCH_SIZE) / 2.0,
                    ),
                    color,
                    roles,
                );
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
        // A colour wheel: the shared HSV disk primitive at the picker's
        // brightness, and a two-tone marker at its hue and saturation, in
        // the colour they make.
        UiContent::ColorWheel(control) => {
            let geometry = color_wheel_geometry(rect);
            // The keyboard's focus ring, in the accent every control's ring
            // uses, just outside the disk.
            if control.state.focus_visible {
                let ring = geometry.r + 4.0;
                prims.push(crate::widget::DrawPrim::Stroke {
                    x: geometry.cx - ring,
                    y: geometry.cy - ring,
                    w: ring * 2.0,
                    h: ring * 2.0,
                    radius: ring,
                    width: 2.0,
                    color: rgba(roles.accent, 255),
                });
            }
            let brightness = control.spec.hsv.map_or(1.0, |[_, _, value]| value);
            prims.push(crate::widget::DrawPrim::HsvDisk {
                cx: geometry.cx,
                cy: geometry.cy,
                r: geometry.r,
                value: brightness,
            });
            if let Some([hue, saturation, value]) = control.spec.hsv {
                let rgb = crate::widget::hsv_to_rgb(hue, saturation, value);
                let (mx, my) = color_wheel_marker_at(rect, hue, saturation);
                // Contrast ring first, then the colour's dot: the marker reads
                // on every hue at every saturation and brightness.
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
                });
                prims.push(crate::widget::DrawPrim::Dot {
                    cx: mx,
                    cy: my,
                    r: 5.0,
                    color: rgba(rgb, 255),
                });
            }
        }
        UiContent::Custom(spec) => {
            // TWO custom nodes carry a raster lowering; every other custom node
            // keeps the plain label projection below. The rainbow banner is
            // pure static decoration — its semantics live entirely on the node.
            if spec.audit_id == MESSAGES_SEVERITY_AUDIT {
                paint_messages_severity(prims, rect, spec.value.as_deref(), &roles);
                return;
            }
            if spec.audit_id == MESSAGES_SCROLL_AUDIT {
                paint_messages_scroll(prims, rect, spec.value.as_deref(), &roles);
                return;
            }
            if spec.audit_id == PAGE_SCROLL_AUDIT {
                paint_page_scroll(prims, rect, spec.value.as_deref(), &roles);
                return;
            }
            if spec.audit_id == COLOR_CHIP_AUDIT {
                if let Some(rgb) = spec
                    .value
                    .as_deref()
                    .and_then(aterm_types::scheme::parse_hex)
                {
                    paint_color_chip(
                        prims,
                        (
                            rect.x + TEXT_FIELD_SWATCH_GAP,
                            rect.y + (rect.height - TEXT_FIELD_SWATCH_SIZE) / 2.0,
                        ),
                        [rgb.r, rgb.g, rgb.b],
                        roles,
                    );
                }
                return;
            }
            if spec.audit_id == THEME_SWATCH_AUDIT {
                // `#rrggbb`, then `changed` for a slot the person tuned: it
                // wears a 4pt accent dot at its top right.
                let mut words = spec.value.as_deref().unwrap_or("").split_whitespace();
                let hex = words.next().and_then(aterm_types::scheme::parse_hex);
                let changed = words.next() == Some("changed");
                if let Some(rgb) = hex {
                    prims.push(crate::widget::DrawPrim::Panel {
                        x: rect.x,
                        y: rect.y,
                        w: rect.width,
                        h: rect.height,
                        radius: 4.0,
                        fill: rgba([rgb.r, rgb.g, rgb.b], 255),
                    });
                    prims.push(crate::widget::DrawPrim::Stroke {
                        x: rect.x + 0.5,
                        y: rect.y + 0.5,
                        w: (rect.width - 1.0).max(0.0),
                        h: (rect.height - 1.0).max(0.0),
                        radius: 3.5,
                        width: 1.0,
                        color: rgba(roles.separator, 200),
                    });
                    if changed && rect.width >= 10.0 && rect.height >= 10.0 {
                        prims.push(crate::widget::DrawPrim::Panel {
                            x: rect.right() - 7.0,
                            y: rect.y + 3.0,
                            w: 4.0,
                            h: 4.0,
                            radius: 2.0,
                            fill: rgba(roles.accent, 255),
                        });
                    }
                }
                return;
            }
            if spec.audit_id == THEME_SLAB_AUDIT {
                crate::settings_preview::theme_card::paint_row_slab(
                    prims,
                    rect,
                    spec.value.as_deref(),
                    &roles,
                );
                return;
            }
            if spec.audit_id == MESSAGES_RULE_AUDIT {
                prims.push(DrawPrim::Panel {
                    x: rect.x,
                    y: rect.y + (rect.height - 1.0).max(0.0) / 2.0,
                    w: rect.width,
                    h: 1.0,
                    radius: 0.0,
                    fill: rgba(roles.separator, 200),
                });
                return;
            }
            if spec.audit_id == RAINBOW_BANNER_AUDIT {
                let (sky, rim) = rainbow_banner_sky(&roles);
                crate::settings::paint_rainbow_banner(
                    prims,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    sky,
                    rim,
                );
                if spec.value.as_deref() == Some("system-state-kitty") {
                    crate::system_state::paint_brand_kitty(prims, rect);
                }
                return;
            }
            if spec.audit_id == WORDMARK_AUDIT {
                let (size, face) = wordmark_typography(&spec.label, rect.width);
                prims.push(text_prim(
                    rect.x,
                    row_baseline(rect.y, rect.height, size.get()),
                    spec.label.clone(),
                    size,
                    TextWeight::Regular,
                    face,
                    rgba(roles.text_primary, 255),
                ));
                return;
            }
            if spec.audit_id == DISPLAY_FACE_NAME_AUDIT {
                let (size, face) = display_face_name_typography(spec, rect.width);
                prims.push(text_prim(
                    rect.x,
                    row_baseline(rect.y, rect.height, size.get()),
                    spec.label.clone(),
                    size,
                    TextWeight::Regular,
                    face,
                    rgba(roles.text_primary, 255),
                ));
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
///
/// A cut label ends after a whole word, as a cut status does
/// ([`fit_native_status_label`]): a word cut mid-way reads as another word or
/// none.
pub(crate) fn fit_native_button_label(value: &str, max_width: f32) -> String {
    whole_word_cut(
        value,
        elide_ui_label(value, max_width, native_type_px(TypeStep::Secondary).get()),
    )
}

/// The width the painter needs to draw a native button label WHOLE, in the
/// exact face and rung [`fit_native_button_label`] elides against.
///
/// A picker whose option spellings are owned elsewhere — a config vocabulary,
/// a catalog of built-in names — cannot shorten its longest label to suit a
/// button authored from a flat constant. Sizing that button from this measure
/// is how such a control gets a slot its own widest option occupies, instead
/// of ellipsizing the value the control exists to state.
pub(crate) fn native_button_label_width(value: &str) -> f32 {
    visual_text_width(
        value,
        native_type_px(TypeStep::Secondary).get(),
        crate::widget::TextFace::Ui,
    )
}

/// THE ONE SEPARATOR between facts a native page paints in a run: a setting
/// row's name and its tags, the tags it moves to its second line, a
/// caption's counts, a package's details. One owner, so the same facts read
/// the same wherever they are placed, beside a name or under it.
pub(crate) const FACT_SEPARATOR: &str = " \u{b7} ";

/// Bounded visual projection for a one-line native body label. The caller's
/// semantic wrapper remains responsible for the complete authored copy.
///
/// A cut label ends after a whole word ([`whole_word_cut`]): Messages' one
/// line heads read "Claude Code is not inst…" at 36 columns.
pub(crate) fn fit_native_text_label(value: &str, max_width: f32) -> String {
    whole_word_cut(
        value,
        elide_ui_label(value, max_width, native_type_px(TypeStep::Body).get()),
    )
}

/// The width the painter needs to draw one-line native status copy WHOLE,
/// in the exact face and rung [`fit_native_status_label`] elides against.
pub(crate) fn native_status_label_width(value: &str) -> f32 {
    visual_text_width(
        value,
        native_type_px(TypeStep::Caption).get(),
        crate::widget::TextFace::Ui,
    )
}

/// Bounded visual projection for one-line native status copy. The owning
/// semantic Group remains responsible for the complete message.
///
/// A cut status ends after a whole word ("Cursor blink was not saved:
/// ~/.config/aterm…", never "…~/.config/at…"): a word cut mid-way reads as
/// another word or none. Only a status whose first word alone is too wide is
/// cut inside it.
pub(crate) fn fit_native_status_label(value: &str, max_width: f32) -> String {
    let fitted = elide_text_label(
        value,
        max_width,
        native_type_px(TypeStep::Caption).get(),
        crate::widget::TextFace::Ui,
    );
    whole_word_cut(value, fitted)
}

/// `fitted`, a cut of `value` ending in `…`, backed off to the last whole
/// word it keeps; `fitted` as it is where it is `value` whole, or where it
/// keeps no whole word. Never wider than `fitted`.
pub(crate) fn whole_word_cut(value: &str, fitted: String) -> String {
    let Some(stem) = fitted.strip_suffix('…') else {
        return fitted;
    };
    if value == fitted || !value.starts_with(stem) {
        return fitted;
    }
    // The cut fell between words already.
    if value[stem.len()..].starts_with(char::is_whitespace) {
        return fitted;
    }
    match stem.rfind(char::is_whitespace) {
        Some(space) if !stem[..space].trim_end().is_empty() => {
            let kept = stem[..space].trim_end_matches(|c: char| {
                c.is_whitespace() || matches!(c, ',' | ';' | ':' | '·' | '—' | '–')
            });
            if kept.is_empty() {
                fitted
            } else {
                format!("{kept}…")
            }
        }
        _ => fitted,
    }
}

fn visual_text_width(value: &str, px: f32, face: crate::widget::TextFace) -> f32 {
    if face == crate::widget::TextFace::Mono {
        crate::tray_raster::measure_text(value, px, crate::widget::TextWeight::Regular)
    } else {
        crate::tray_raster::ui_text_width_for(face, value, px)
    }
}

/// How far a text may measure past the width it is fitted to and still be
/// drawn whole: a hundredth of a point, far under a device pixel at any scale.
///
/// A width that comes back through a layout is not bit-exact: a control laid
/// out at its label's measure plus its padding, then fitted to that width less
/// the padding, gets `(w + pad) - pad`, which in `f32` can land a few
/// millionths under `w`. Compared exactly, a label given its own measure was
/// elided ("Refre…" on Harness at 220x60); this slack is what that rounding
/// costs, never room for a visible overflow.
pub(crate) const TEXT_FIT_SLACK: f32 = 0.01;

pub(crate) fn elide_text_label(
    value: &str,
    max_width: f32,
    px: f32,
    face: crate::widget::TextFace,
) -> String {
    const MAX_VISUAL_GRAPHEMES: usize = 256;

    if max_width <= 0.0 {
        return String::new();
    }
    let max_width = max_width + TEXT_FIT_SLACK;
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
    if face != crate::widget::TextFace::Mono {
        // The longest leading run of whole graphemes that fits beside the
        // ellipsis — every boundary measured in one pass of the pen, as the
        // shed-one-and-measure-again loop below finds it.
        let cuts = output
            .grapheme_indices()
            .map(|(at, _)| at)
            .chain(std::iter::once(output.len()))
            .collect::<Vec<_>>();
        let widths = crate::tray_raster::ui_text_widths_at(face, &output, &cuts, "", px);
        let keep = (1..cuts.len())
            .rev()
            .find(|&at| widths[at] + ellipsis_width <= max_width)
            .map_or(0, |at| cuts[at]);
        if keep < output.len() {
            output.truncate(keep);
            elided = true;
        }
    } else {
        while !output.is_empty()
            && visual_text_width(&output, px, face) + ellipsis_width > max_width
        {
            let Some((offset, _)) = output.grapheme_indices().last() else {
                output.clear();
                break;
            };
            output.truncate(offset);
            elided = true;
        }
    }
    if elided && ellipsis_width <= max_width {
        // "Typeset…", never "Typeset …": the ellipsis follows the last word.
        output.truncate(output.trim_end().len());
        output.push('…');
    }
    output
}

thread_local! {
    /// While the paint audit reads what a node paints
    /// ([`CompiledUi::ink_overlaps`]), or a list's edge cuts across it
    /// ([`drop_cut_marks`]): the run of prims each pictogram
    /// [`paint_button_icon`] draws, whatever prims it draws it with — the
    /// outlines and blocks of a pictogram are its ink, where the same prims
    /// elsewhere are a control's face.
    static ICON_SPANS: std::cell::RefCell<Option<Vec<std::ops::Range<usize>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `paint`, noting the run of prims each pictogram it draws
/// ([`ICON_SPANS`]): as absolute ranges of the prims it pushes to.
fn painted_icons(paint: impl FnOnce()) -> Vec<std::ops::Range<usize>> {
    let outer = ICON_SPANS.with(|spans| spans.borrow_mut().replace(Vec::new()));
    paint();
    let icons = ICON_SPANS
        .with(|spans| spans.borrow_mut().take())
        .unwrap_or_default();
    ICON_SPANS.with(|spans| *spans.borrow_mut() = outer);
    icons
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
    let from = prims.len();
    paint_button_icon_prims(prims, rect, icon, color);
    ICON_SPANS.with(|spans| {
        if let Some(spans) = spans.borrow_mut().as_mut() {
            spans.push(from..prims.len());
        }
    });
}

/// The prims of [`paint_button_icon`]'s pictogram.
fn paint_button_icon_prims(
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
        // Windows draws the thin chevron of WinUI's ComboBox (Segoe Fluent Icons
        // E70D) as two antialiased segments meeting at the point, like Back and
        // Forward; the stepped filled triangle read as a foreign widget there.
        ButtonIcon::ChevronDown if FLUENT_SWITCH => {
            prims.push(segment(cx - 4.5, cy - 2.25, cx, cy + 2.25));
            prims.push(segment(cx, cy + 2.25, cx + 4.5, cy - 2.25));
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
            // A pencil: its body, its cap, and its point.
            prims.push(segment(cx - 5.0, cy + 3.0, cx + 3.0, cy - 5.0));
            prims.push(segment(cx - 3.0, cy + 5.0, cx + 5.0, cy - 3.0));
            prims.push(segment(cx + 3.0, cy - 5.0, cx + 5.0, cy - 3.0));
            prims.push(segment(cx - 5.0, cy + 3.0, cx - 6.5, cy + 6.5));
            prims.push(segment(cx - 6.5, cy + 6.5, cx - 3.0, cy + 5.0));
        }
        ButtonIcon::Appearance => prims.push(DrawPrim::Ring {
            cx,
            cy,
            r_outer: 8.0,
            thickness: 3.0,
            track: rgba(color, 70),
            frac: 0.5,
            fill: rgba(color, 240),
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
                });
            }
            prims.push(line(cx - 5.0, cy + 3.0, 10.0, 1.0));
        }
        ButtonIcon::Terminal => {
            prims.push(outline(cx - 9.0, cy - 7.0, 18.0, 14.0, 2.0));
            prims.push(line(cx - 5.0, cy - 2.0, 4.0, 1.0));
            prims.push(line(cx + 1.0, cy + 3.0, 5.0, 1.0));
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
                });
            }
        }
        ButtonIcon::Update => {
            // A download arrow in a circle.
            prims.push(outline(cx - 8.0, cy - 8.0, 16.0, 16.0, 8.0));
            prims.push(segment(cx, cy - 4.5, cx, cy + 4.0));
            prims.push(segment(cx - 3.5, cy + 0.5, cx, cy + 4.0));
            prims.push(segment(cx + 3.5, cy + 0.5, cx, cy + 4.0));
        }
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
            });
            prims.push(line(cx, cy, 1.0, 6.0));
        }
        ButtonIcon::Check => {
            prims.push(segment(cx - 5.5, cy + 0.5, cx - 1.5, cy + 4.5));
            prims.push(segment(cx - 1.5, cy + 4.5, cx + 6.0, cy - 4.5));
        }
        ButtonIcon::Image => {
            prims.push(outline(cx - 9.0, cy - 7.0, 18.0, 14.0, 2.0));
            prims.push(DrawPrim::Dot {
                cx: cx + 4.0,
                cy: cy - 2.5,
                r: 1.6,
                color: rgba(color, 240),
            });
            prims.push(segment(cx - 7.0, cy + 5.0, cx - 2.0, cy - 1.0));
            prims.push(segment(cx - 2.0, cy - 1.0, cx + 3.0, cy + 5.0));
        }
        ButtonIcon::Swatch => {
            prims.push(outline(cx - 8.0, cy - 6.0, 16.0, 12.0, 2.0));
            prims.push(block(cx - 8.0, cy - 6.0, 8.0, 5.0, 1.5));
        }
        ButtonIcon::Themes => {
            // Four palettes in a 2×2 grid, each its own strength of the ink,
            // so the set reads as several schemes rather than one swatch.
            for (dx, dy, alpha) in [
                (-1.0_f32, -1.0_f32, 240_u8),
                (1.0, -1.0, 170),
                (-1.0, 1.0, 110),
                (1.0, 1.0, 200),
            ] {
                prims.push(DrawPrim::Panel {
                    x: cx + dx * 1.5 + (dx - 1.0) * 3.0,
                    y: cy + dy * 1.5 + (dy - 1.0) * 3.0,
                    w: 6.0,
                    h: 6.0,
                    radius: 1.5,
                    fill: rgba(color, alpha),
                });
            }
        }
        ButtonIcon::Spark => {
            // A four-pointed star: each point's two edges meet the next
            // point's at a pinched waist, and a small dot rides its shoulder.
            let waist = 2.0_f32;
            for (px, py, wx, wy) in [
                (0.0_f32, -8.0_f32, 1.0_f32, -1.0_f32),
                (8.0, 0.0, 1.0, 1.0),
                (0.0, 8.0, -1.0, 1.0),
                (-8.0, 0.0, -1.0, -1.0),
            ] {
                prims.push(segment(cx + px, cy + py, cx + wx * waist, cy + wy * waist));
                // The waist after this point leads into the next one.
                let (nx, ny) = (-py, px);
                prims.push(segment(cx + wx * waist, cy + wy * waist, cx + nx, cy + ny));
            }
            prims.push(DrawPrim::Dot {
                cx: cx + 6.5,
                cy: cy - 6.5,
                r: 1.3,
                color: rgba(color, 220),
            });
        }
        ButtonIcon::Trash => {
            // The lid and its handle, then the bin with two ribs.
            prims.push(line(cx - 6.0, cy - 5.0, 12.0, 1.0));
            prims.push(line(cx - 2.0, cy - 7.0, 4.0, 1.0));
            prims.push(outline(cx - 4.5, cy - 3.5, 9.0, 10.0, 1.5));
            prims.push(line(cx - 1.5, cy - 1.0, 1.0, 5.0));
            prims.push(line(cx + 0.5, cy - 1.0, 1.0, 5.0));
        }
        ButtonIcon::Clear => {
            prims.push(segment(cx - 3.5, cy - 3.5, cx + 3.5, cy + 3.5));
            prims.push(segment(cx - 3.5, cy + 3.5, cx + 3.5, cy - 3.5));
        }
        ButtonIcon::Search => {
            // The lens, and its handle out to the lower right.
            prims.push(outline(cx - 6.0, cy - 6.0, 10.0, 10.0, 5.0));
            prims.push(segment(cx + 2.6, cy + 2.6, cx + 6.5, cy + 6.5));
        }
        ButtonIcon::Reset => {
            // Most of a circle, open at the top right, and the arrowhead on
            // its open end pointing back the way it turns.
            let r = 5.5_f32;
            let steps = 10;
            let (from, to) = (-0.25_f32, 1.45_f32);
            let point = |t: f32| {
                let angle = std::f32::consts::PI * t;
                (cx + r * angle.cos(), cy + r * angle.sin())
            };
            for step in 0..steps {
                let (x1, y1) = point(from + (to - from) * step as f32 / steps as f32);
                let (x2, y2) = point(from + (to - from) * (step + 1) as f32 / steps as f32);
                prims.push(segment(x1, y1, x2, y2));
            }
            // The head's two barbs lean back from the tip, either side of
            // the way the arc runs there.
            let (tip_x, tip_y) = point(to);
            let along = std::f32::consts::PI * to + std::f32::consts::FRAC_PI_2;
            for lean in [0.7_f32, -0.7] {
                let back = along + std::f32::consts::PI + lean;
                prims.push(segment(
                    tip_x,
                    tip_y,
                    tip_x + 3.5 * back.cos(),
                    tip_y + 3.5 * back.sin(),
                ));
            }
        }
        ButtonIcon::Document => {
            prims.push(outline(cx - 6.0, cy - 8.0, 12.0, 16.0, 2.0));
            prims.push(segment(cx + 1.5, cy - 8.0, cx + 6.0, cy - 3.5));
            prims.push(line(cx - 3.0, cy, 6.0, 1.0));
            prims.push(line(cx - 3.0, cy + 3.5, 4.0, 1.0));
        }
        ButtonIcon::Agents => {
            prims.push(outline(cx - 7.0, cy - 4.0, 14.0, 11.0, 3.0));
            prims.push(line(cx, cy - 7.0, 1.0, 3.0));
            prims.push(DrawPrim::Dot {
                cx: cx + 0.5,
                cy: cy - 7.5,
                r: 1.2,
                color: rgba(color, 240),
            });
            for x in [-3.0_f32, 3.0] {
                prims.push(DrawPrim::Dot {
                    cx: cx + x,
                    cy: cy + 1.5,
                    r: 1.4,
                    color: rgba(color, 240),
                });
            }
        }
        ButtonIcon::Trail => {
            prims.push(block(cx + 3.0, cy - 7.0, 5.0, 14.0, 1.0));
            for (offset, r, alpha) in [
                (-2.0_f32, 1.8_f32, 210_u8),
                (-6.0, 1.4, 150),
                (-9.5, 1.0, 95),
            ] {
                prims.push(DrawPrim::Dot {
                    cx: cx + offset,
                    cy,
                    r,
                    color: rgba(color, alpha),
                });
            }
        }
        ButtonIcon::Sound => {
            // A speaker's box and cone, and two waves in front of it.
            prims.push(block(cx - 9.0, cy - 3.0, 4.0, 6.0, 1.0));
            prims.push(segment(cx - 5.0, cy - 3.0, cx, cy - 7.0));
            prims.push(segment(cx, cy - 7.0, cx, cy + 7.0));
            prims.push(segment(cx, cy + 7.0, cx - 5.0, cy + 3.0));
            prims.push(segment(cx + 3.5, cy - 3.0, cx + 4.5, cy));
            prims.push(segment(cx + 4.5, cy, cx + 3.5, cy + 3.0));
            prims.push(segment(cx + 6.5, cy - 6.0, cx + 8.5, cy));
            prims.push(segment(cx + 8.5, cy, cx + 6.5, cy + 6.0));
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
            });
            prims.push(DrawPrim::Panel {
                x: rect.x,
                y: rect.y + 2.0,
                w: 3.0,
                h: (rect.height - 4.0).max(0.0),
                radius: 1.5,
                fill: rgba(roles.accent, 220),
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
    if !spec.inline.is_empty() && !preserve_lines {
        let base = StyledBase {
            x: text_x,
            y: text_y,
            bottom: rect.bottom(),
            line_h,
            size,
            face,
            weight,
            color,
        };
        paint_styled_markdown_rows(prims, spec, &base, max_columns, lines.len(), roles);
        return;
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

/// The unstyled look of a prose block's rows, which styled spans modify.
struct StyledBase {
    x: f32,
    y: f32,
    bottom: f32,
    line_h: f32,
    size: crate::type_scale::StepPx,
    face: crate::widget::TextFace,
    weight: crate::widget::TextWeight,
    color: [u8; 3],
}

/// For each wrapped prose row, its space-separated pieces with the byte range of
/// `text` each came from — the words (or the part of one a CJK break or a chunk
/// of an over-long word left on the row) the wrap placed on the row, in order.
/// The words are the text cut at its spaces as the wrap cuts them
/// ([`aterm_grapheme::pieces`]: a no-break space is glue). A piece the walk
/// cannot place (the tail ellipsis row's truncated word) carries `None` and
/// paints unstyled.
fn wrapped_piece_ranges(
    text: &str,
    rows: &[String],
) -> Vec<Vec<(String, Option<std::ops::Range<usize>>)>> {
    let words: Vec<std::ops::Range<usize>> = if text.is_ascii() {
        text.split(char::is_whitespace)
            .filter(|word| !word.is_empty())
            .map(|word| {
                let at = word.as_ptr() as usize - text.as_ptr() as usize;
                at..at + word.len()
            })
            .collect()
    } else {
        aterm_grapheme::pieces(text, |kind| kind == aterm_grapheme::BreakKind::Space)
            .into_iter()
            .map(|piece| piece.at..piece.at + piece.text.len())
            .collect()
    };
    let (mut wi, mut at) = (0usize, words.first().map_or(0, |w| w.start));
    rows.iter()
        .map(|row| {
            row.split(' ')
                .filter(|piece| !piece.is_empty())
                .map(|piece| {
                    let Some(word) = words.get(wi) else {
                        return (piece.to_string(), None);
                    };
                    // A word that holds a space the rule keeps (before a CJK
                    // closer) goes on past it.
                    let rest = &text[at..word.end];
                    at += rest.len() - rest.trim_start().len();
                    let end = text[at..word.end]
                        .char_indices()
                        .nth(piece.chars().count())
                        .map_or(word.end, |(offset, _)| at + offset);
                    if &text[at..end] != piece {
                        return (piece.to_string(), None);
                    }
                    let range = at..end;
                    if end >= word.end {
                        wi += 1;
                        at = words.get(wi).map_or(text.len(), |w| w.start);
                    } else {
                        at = end;
                    }
                    (piece.to_string(), Some(range))
                })
                .collect()
        })
        .collect()
}

/// Paint a prose block's visible rows with its inline styles: strong spans in the
/// bold face, inline code in the mono face on a tinted chip, links in the accent
/// with an underline, strikethrough with a line through, emphasis tinted toward
/// the accent (the raster path has no italic UI face). Each segment advances by
/// its measured width in the face it is drawn with. Wrapping is exactly the flat
/// path's (`wrap_markdown_text`), so rows and scrolling do not move.
fn paint_styled_markdown_rows(
    prims: &mut Vec<crate::widget::DrawPrim>,
    spec: &MarkdownBlockSpec,
    base: &StyledBase,
    max_columns: usize,
    visible_rows: usize,
    roles: crate::settings::Roles,
) {
    use crate::tray_raster::{measure_text, row_baseline, ui_text_width_for};
    use crate::widget::{DrawPrim, TextFace, rgba, text_prim};

    let px = base.size.get();
    let measure = |face: TextFace, weight, s: &str| match face {
        TextFace::Mono => measure_text(s, px, weight),
        TextFace::Ui | TextFace::UiBold | TextFace::Display(_) => ui_text_width_for(face, s, px),
    };
    let requested = spec
        .visual_row
        .saturating_add(visible_rows.max(1))
        .clamp(1, 131_072);
    let rows = wrap_markdown_text(&spec.text, max_columns, 0, requested, false);
    let pieces = wrapped_piece_ranges(&spec.text, &rows);
    let space = measure(base.face, base.weight, " ");
    for (line_index, row) in pieces
        .into_iter()
        .skip(spec.visual_row)
        .take(visible_rows)
        .enumerate()
    {
        let y = base.y + line_index as f32 * base.line_h;
        if y >= base.bottom {
            break;
        }
        let baseline = row_baseline(y, base.line_h, px);
        let mut x = base.x;
        for (piece_index, (piece, range)) in row.into_iter().enumerate() {
            if piece_index > 0 {
                x += space;
            }
            // The piece split at every style boundary inside it.
            let mut segments: Vec<(String, crate::native_markdown::InlineStyle)> = Vec::new();
            match range {
                None => segments.push((piece, Default::default())),
                Some(range) => {
                    let mut at = range.start;
                    for (span, style) in spec
                        .inline
                        .iter()
                        .filter(|(span, _)| span.start < range.end && range.start < span.end)
                    {
                        let from = span.start.max(range.start);
                        let to = span.end.min(range.end);
                        if at < from {
                            segments.push((spec.text[at..from].to_string(), Default::default()));
                        }
                        segments.push((spec.text[from..to].to_string(), *style));
                        at = to;
                    }
                    if at < range.end {
                        segments.push((spec.text[at..range.end].to_string(), Default::default()));
                    }
                }
            }
            for (s, style) in segments {
                let face = if style.code {
                    TextFace::Mono
                } else if style.strong {
                    TextFace::UiBold
                } else {
                    base.face
                };
                let color = if style.link {
                    roles.accent
                } else if style.emphasis {
                    mix_rgb(base.color, roles.accent, 0.35)
                } else {
                    base.color
                };
                let w = measure(face, base.weight, &s);
                if style.code {
                    prims.push(DrawPrim::Panel {
                        x: x - 2.0,
                        y: baseline - px * 0.95,
                        w: w + 4.0,
                        h: px * 1.3,
                        radius: 4.0,
                        fill: rgba(mix_rgb(roles.elevated, roles.surface, 0.20), 255),
                    });
                }
                if style.strike {
                    prims.push(DrawPrim::Stroke {
                        x,
                        y: baseline - px * 0.32,
                        w,
                        h: 1.0,
                        radius: 0.5,
                        width: 1.0,
                        color: rgba(color, 220),
                    });
                }
                if style.link {
                    prims.push(DrawPrim::Stroke {
                        x,
                        y: baseline + 2.0,
                        w,
                        h: 1.0,
                        radius: 0.5,
                        width: 1.0,
                        color: rgba(roles.accent, 200),
                    });
                }
                prims.push(text_prim(
                    x,
                    baseline,
                    s,
                    base.size,
                    base.weight,
                    face,
                    rgba(color, 255),
                ));
                x += w;
            }
        }
    }
}

/// Bounded, allocation-conscious wrapping for visible reader blocks. The app
/// already caps source copied into a block; this cap also bounds paint prims.
///
/// `skip` is the number of leading wrapped rows the caller does not want. They
/// are still COUNTED — the row budget and the tail-ellipsis rule below are
/// defined over the whole wrap, not over the returned window — but never
/// materialized. Intra-block scrolling therefore costs one `String` per VISIBLE
/// row instead of one per row from the top of the block; the thousand-line
/// fence in the shipped Manual, scrolled near its end, used to build and
/// immediately drop ~970 of them on every frame.
fn wrap_markdown_text(
    text: &str,
    max_columns: usize,
    skip: usize,
    max_lines: usize,
    preserve_lines: bool,
) -> Vec<String> {
    // A char is a column of the face's average advance, a wide one (CJK, a
    // fullwidth form, an emoji) two, as the block's row estimate counts them
    // ([`crate::native_markdown::reader_columns`]); and a line breaks where
    // the shared rule lets it ([`aterm_grapheme::pieces`]): at its spaces,
    // and between CJK characters, kinsoku kept. ASCII text takes neither
    // scan and wraps exactly as it always did.
    use crate::native_markdown::reader_columns as columns;
    let max_columns = max_columns.max(1);
    // Upstream caps block text at 128 KiB. Even the adversarial one-byte-line
    // case is therefore bounded while still allowing a tail viewport far past
    // the former 512-row ceiling.
    let max_lines = max_lines.clamp(1, 131_072);
    let mut output = Vec::with_capacity(max_lines.saturating_sub(skip).min(16));
    // `produced` is the total row counter this function used to keep implicitly
    // in `output.len()`; `produced_chars` is the emitted-character total the
    // tail-ellipsis rule compares against, which must still cover the SKIPPED
    // prefix or that rule would change meaning.
    let mut produced = 0usize;
    let mut produced_chars = 0usize;
    // Hoisted out of the source-line loop: a long fence is thousands of lines,
    // and a fresh `Vec<char>` per line was thousands of malloc/free pairs per
    // frame for a buffer that is dead the moment the line is wrapped.
    let mut characters: Vec<char> = Vec::new();
    // A preserved line's breaks between CJK characters, as the char each
    // would start the next row at.
    let mut cjk_starts: Vec<usize> = Vec::new();
    // A flowed line's pieces: whether white space came before each, and it.
    let mut words: Vec<(bool, &str)> = Vec::new();
    for source_line in text.lines().chain(text.is_empty().then_some("")) {
        if produced >= max_lines {
            break;
        }
        if preserve_lines {
            characters.clear();
            characters.extend(source_line.chars());
            cjk_starts.clear();
            if !source_line.is_ascii() {
                let mut breaks = aterm_grapheme::line_breaks(source_line)
                    .into_iter()
                    .filter(|brk| brk.kind == aterm_grapheme::BreakKind::Ideographic)
                    .map(|brk| brk.at)
                    .peekable();
                for (index, (byte, _)) in source_line.char_indices().enumerate() {
                    if breaks.next_if_eq(&byte).is_some() {
                        cjk_starts.push(index);
                    }
                }
            }
            if characters.is_empty() {
                if produced >= skip {
                    output.push(String::new());
                }
                produced += 1;
            } else {
                let mut start = 0usize;
                while start < characters.len() {
                    if produced >= max_lines {
                        break;
                    }
                    // As many chars as the row's columns hold, at least one.
                    let mut hard_end = start;
                    let mut used = 0usize;
                    while let Some(&character) = characters.get(hard_end) {
                        let width = columns(character);
                        if hard_end > start && used + width > max_columns {
                            break;
                        }
                        used += width;
                        hard_end += 1;
                    }
                    if hard_end == characters.len() {
                        if produced >= skip {
                            output.push(characters[start..hard_end].iter().collect());
                        }
                        produced += 1;
                        produced_chars += hard_end - start;
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
                    // Or the last break between CJK characters that fits.
                    let cjk_end = cjk_starts[..cjk_starts.partition_point(|at| *at <= hard_end)]
                        .last()
                        .copied()
                        .filter(|end| *end > start);
                    let end = soft_end.max(cjk_end).unwrap_or(hard_end);
                    if produced >= skip {
                        output.push(characters[start..end].iter().collect());
                    }
                    produced += 1;
                    produced_chars += end - start;
                    start = end;
                    while start < characters.len() && characters[start].is_whitespace() {
                        start += 1;
                    }
                }
            }
            continue;
        }
        words.clear();
        if source_line.is_ascii() {
            words.extend(source_line.split_whitespace().map(|word| (true, word)));
        } else {
            words.extend(
                aterm_grapheme::pieces(source_line, aterm_grapheme::BreakKind::is_word_boundary)
                    .into_iter()
                    .map(|piece| (piece.spaced, piece.text)),
            );
        }
        let mut line = String::new();
        let mut line_columns = 0usize;
        // `line.chars().count()`, which the ellipsis rule totals.
        let mut line_chars = 0usize;
        for &(spaced, word) in &words {
            // A space between it and the line, unless it goes on the line's
            // last piece with nothing between (a CJK character's next).
            let separator = usize::from(!line.is_empty() && spaced);
            let word_columns: usize = word.chars().map(columns).sum();
            if line_columns + separator + word_columns <= max_columns {
                if separator == 1 {
                    line.push(' ');
                }
                line.push_str(word);
                line_columns += separator + word_columns;
                line_chars += separator + word.chars().count();
                continue;
            }
            if !line.is_empty() {
                let row = std::mem::take(&mut line);
                if produced >= skip {
                    output.push(row);
                }
                produced += 1;
                produced_chars += line_chars;
                if produced >= max_lines {
                    break;
                }
            }
            let mut chunk = String::new();
            let mut chunk_columns = 0usize;
            let mut chunk_chars = 0usize;
            for character in word.chars() {
                let width = columns(character);
                // A wide char that would end past the row starts the next.
                if chunk_columns + width > max_columns && !chunk.is_empty() {
                    let row = std::mem::take(&mut chunk);
                    if produced >= skip {
                        output.push(row);
                    }
                    produced += 1;
                    produced_chars += chunk_chars;
                    chunk_columns = 0;
                    chunk_chars = 0;
                    if produced >= max_lines {
                        break;
                    }
                }
                chunk.push(character);
                chunk_columns += width;
                chunk_chars += 1;
                if chunk_columns >= max_columns {
                    let row = std::mem::take(&mut chunk);
                    if produced >= skip {
                        output.push(row);
                    }
                    produced += 1;
                    produced_chars += chunk_chars;
                    chunk_columns = 0;
                    chunk_chars = 0;
                    if produced >= max_lines {
                        break;
                    }
                }
            }
            line = chunk;
            line_columns = chunk_columns;
            line_chars = chunk_chars;
        }
        if produced >= max_lines {
            break;
        }
        if !line.is_empty() || source_line.is_empty() {
            if produced >= skip {
                output.push(line);
            }
            produced += 1;
            produced_chars += line_chars;
        }
    }
    if produced == max_lines
        && text.chars().count() > produced_chars
        && let Some(last) = output.last_mut()
    {
        // As many chars as the row's columns hold beside the `…`.
        let keep = max_columns.saturating_sub(1);
        let mut used = 0usize;
        *last = last
            .chars()
            .take_while(|character| {
                used += columns(*character);
                used <= keep
            })
            .collect::<String>();
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
    // `skip = visual_row` replaces a `drain(..visual_row)` over rows that were
    // built only to be thrown away. `truncate` still stands: the word-wrap path
    // can overshoot the budget by one row (it pushes, then tests), exactly as
    // it did before.
    let mut lines = wrap_markdown_text(text, max_columns, visual_row, requested, preserve_lines);
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
    });
    prims.push(DrawPrim::Panel {
        x: rect.x,
        y: body_y,
        w: gutter_w,
        h: body_h,
        radius: 0.0,
        fill: rgba(mix_rgb(roles.surface, roles.elevated, 0.52), 255),
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
                let base_caret = editor_caret_rect(&geometry, row, line, *byte);
                let caret = if *primary {
                    editor_composition_caret_rect(base_caret, spec)
                } else {
                    base_caret
                };
                let x = base_caret.x;
                prims.push(DrawPrim::Stroke {
                    x: caret.x,
                    y: caret.y,
                    w: caret.width,
                    h: caret.height,
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
                    // The marked range covers complete display cells even
                    // when the current font cannot paint a cluster. The input
                    // caret separately follows the text's painted advance.
                    let preedit_width =
                        crate::native_editor::editor_display_column(&preedit, preedit.len(), 0)
                            .max(1) as f32
                            * cell_w;
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
                        w: preedit_width.max(2.0),
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
        });
        let message = spec
            .minibuffer
            .as_ref()
            .or(spec.status.as_ref())
            .map(String::as_str)
            .unwrap_or("Ready");
        prims.push(text_prim(
            rect.x + EDITOR_FOOTER_TEXT_INSET,
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
        if let Some(caret) = editor_minibuffer_caret_rect(spec, rect, &geometry) {
            prims.push(DrawPrim::Stroke {
                x: caret.x,
                y: caret.y,
                w: caret.width,
                h: caret.height,
                radius: 0.0,
                width: 1.0,
                color: rgba(roles.accent, 255),
            });
        }
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

/// The resting fill of an INTERACTIVE control. One perceptual step above
/// `elevated` — controls sit ON cards that are themselves `elevated`, so a
/// control filled with the card's own color dissolves into it and only its
/// hairline is left doing the work. The step is small enough that
/// `text_primary` (contrast-ensured against `surface`) keeps comfortable
/// margin on top of it.
fn control_rest_fill(roles: &crate::settings::Roles) -> [u8; 3] {
    control_rest_fill_over(roles.elevated, roles)
}

/// Whether a light theme's resting controls rise toward white above the card
/// they sit on, as WinUI's `ControlFillColorDefault` (white at 70%) does, rather
/// than sinking a step toward the ink. Windows only, with the cards that rise
/// there (`native_appearance`'s light elevation): a light Settings page drew its
/// dropdowns, buttons and search box darker than their cards (the 2026-10-18
/// judge's `light-settings-controls-sunken`). macOS and Linux keep their look.
const LIGHT_CONTROLS_RISE: bool = cfg!(windows);

/// How far toward white a light resting control rises over its ground
/// ([`LIGHT_CONTROLS_RISE`]): WinUI's 70% white.
const LIGHT_CONTROL_RISE: f32 = 0.7;

/// Whether a QUIET button that shows words is painted as WinUI's standard
/// Button: `ControlFillColorDefault` (the resting control fill, opaque) inside
/// `ControlElevationBorder` (the separator rim), as a Secondary button is.
/// Windows only. A quiet text button rested at 72/255 of that fill with no rim
/// (`#F8F8F8` on a `#F6F6F6` card in Default Light, 1.01:1; `#2B2B2C` on
/// `#29292A` in dark), so Browse themes…, Reset, Import… and Folder read as
/// plain text beside the bordered Customize (the 2026-10-18 Fluent judge's
/// `J4F-8-settings-secondary-buttons-invisible`). Windows 11 Settings and
/// Windows Terminal's settings draw such text actions as standard buttons; a
/// subtle button (no fill until hovered) is WinUI's for icon commands, so a
/// quiet button that shows only a pictogram keeps its quiet look. macOS and
/// Linux keep their quiet buttons.
const FLUENT_TEXT_BUTTONS: bool = cfg!(windows);

/// The style a button's surface is painted in: its own, except that a quiet
/// button that shows words is painted as a Secondary one where
/// [`FLUENT_TEXT_BUTTONS`]. Its label, metrics and semantics are its own.
fn button_paint_style(control: &Control<ButtonSpec>) -> StyleRef {
    if FLUENT_TEXT_BUTTONS && control.style == StyleRef::Quiet && control.spec.visual_icon.is_none()
    {
        StyleRef::Secondary
    } else {
        control.style
    }
}

/// A resting control's fill over `ground`, the surface it sits on: a step toward
/// the ink on a dark theme; on a light one toward white where
/// [`LIGHT_CONTROLS_RISE`], else toward the ink. The Settings controls sit on
/// `elevated`; the command palette's filter field on its card's `surface`.
pub(crate) fn control_rest_fill_over(ground: [u8; 3], roles: &crate::settings::Roles) -> [u8; 3] {
    if LIGHT_CONTROLS_RISE && !crate::native_appearance::surface_is_dark(roles.surface) {
        mix_rgb(ground, [0xFF, 0xFF, 0xFF], LIGHT_CONTROL_RISE)
    } else {
        mix_rgb(ground, roles.text_primary, 0.05)
    }
}

/// A box drawn as a piece of a card that runs on ([`Joins`]): where it does,
/// the clip its open edges stand past is pushed (its own bounds, and the gap
/// to the next piece under it), and the rect to draw the box in returned —
/// carried `radius` and its stroke past each open edge, so no rounded corner
/// or stroke closes it there. Returns that rect and whether a clip was
/// pushed (its caller pops it after the box).
fn open_box(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    joins: Joins,
    radius: f32,
) -> (LogicalRect, bool) {
    if joins.closed() {
        return (rect, false);
    }
    let below = joins.below.unwrap_or(0.0).max(0.0);
    prims.push(crate::widget::DrawPrim::ClipPush {
        x: rect.x,
        y: rect.y,
        w: rect.width,
        h: rect.height + below,
    });
    let past = radius + 2.0;
    let top = rect.y - if joins.above { past } else { 0.0 };
    let bottom = rect.bottom()
        + if joins.below.is_some() {
            below + past
        } else {
            0.0
        };
    (
        LogicalRect::new(rect.x, top, rect.width, (bottom - top).max(0.0)),
        true,
    )
}

/// Where a DISCLOSURE button's label ends, from its trailing edge: the
/// chevron's slot ([`paint_disclosure_button`]).
pub(crate) const DISCLOSURE_LABEL_END: f32 = 34.0;

/// The room a disclosure button `width` wide gives its label's lines.
pub(crate) fn disclosure_label_room(width: f32) -> f32 {
    (width - BUTTON_LABEL_INSET - DISCLOSURE_LABEL_END).max(0.0)
}

/// A DISCLOSURE button ([`ControlState::expanded`], design ruling 262): a row
/// of a list, not a raised control — flush at rest, a quiet wash under the
/// pointer, pressed or open, the focus ring when the keyboard is on it, and a
/// trailing chevron — `›` closed, `⌄` open — in the secondary ink. Its label
/// (when it paints one) is the Secondary step, left.
fn paint_disclosure_button(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    control: &Control<ButtonSpec>,
    roles: &crate::settings::Roles,
) {
    use crate::tray_raster::row_baseline;
    use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};

    let state = control.state;
    let open = state.expanded == Some(true);
    let wash = if state.pressed {
        Some(0.14)
    } else if state.hovered {
        Some(0.08)
    } else if open {
        Some(0.05)
    } else {
        None
    };
    if let Some(t) = wash {
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: rect.height,
            radius: 6.0,
            fill: rgba(mix_rgb(roles.elevated, roles.text_primary, t), 255),
        });
    }
    if state.focus_visible {
        prims.push(DrawPrim::Stroke {
            x: rect.x + 1.0,
            y: rect.y + 1.0,
            w: (rect.width - 2.0).max(0.0),
            h: (rect.height - 2.0).max(0.0),
            radius: 6.0,
            width: 2.0,
            color: rgba(roles.accent, 255),
        });
    }
    let label = control
        .spec
        .visual_label
        .as_ref()
        .unwrap_or(&control.spec.label);
    if !label.is_empty() {
        let size = native_type_px(TypeStep::Secondary);
        // Its lines one under another, centred, as a text button's
        // ([`button_label_lines`]), each in the room the chevron leaves.
        let lines = button_label_lines(label).collect::<Vec<_>>();
        let line_height = if lines.len() > 1 {
            native_button_line_height()
        } else {
            rect.height
        };
        let top = rect.y + ((rect.height - line_height * lines.len() as f32) / 2.0).max(0.0);
        for (index, line) in lines.into_iter().enumerate() {
            prims.push(text_prim(
                rect.x + BUTTON_LABEL_INSET,
                row_baseline(top + index as f32 * line_height, line_height, size.get()),
                elide_text_label(
                    line,
                    (rect.width - BUTTON_LABEL_INSET - DISCLOSURE_LABEL_END).max(0.0),
                    size.get(),
                    TextFace::Ui,
                ),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(control_text_color(state, roles), 255),
            ));
        }
    }
    let ink = rgba(readable_secondary(roles), 235);
    let (cx, cy) = (rect.right() - 16.0, rect.y + rect.height / 2.0);
    let segment = |x1: f32, y1: f32, x2: f32, y2: f32| DrawPrim::Line {
        x1,
        y1,
        x2,
        y2,
        width: 1.6,
        color: ink,
    };
    if open {
        prims.push(segment(cx - 4.5, cy - 2.25, cx, cy + 2.25));
        prims.push(segment(cx, cy + 2.25, cx + 4.5, cy - 2.25));
    } else {
        prims.push(segment(cx - 2.25, cy - 4.5, cx + 2.25, cy));
        prims.push(segment(cx + 2.25, cy, cx - 2.25, cy + 4.5));
    }
}

/// Settings ▸ Messages' severity mark ([`MESSAGES_SEVERITY_AUDIT`]): the
/// band's drawn icon at the Body step's cap height, centred in the rect after
/// the rail's room, and the rail for an Error or a Warning. The node's value
/// is `severity:glyph`: the ICON is the one the band draws for the row's
/// glyph (`aterm v0.92.0 is ready` keeps its ✓, `aterm crashed last time`
/// its ⚠ — ruling 263), the severity's own where the glyph has none; the
/// COLOUR and the rail are always the severity's.
/// Settings ▸ Messages' scroll thumb ([`MESSAGES_SCROLL_AUDIT`]): `value` is
/// `first,shown,total` — the entries the view starts at and shows, of how
/// many. A 3 pt rounded thumb down the rect's trailing edge, inset clear of
/// the card's rounded corners, its length the share shown (never under
/// 18 pt) and its place the share scrolled past. Nothing where every entry is
/// shown.
fn paint_messages_scroll(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &crate::settings::Roles,
) {
    use crate::widget::{DrawPrim, rgba};

    let Some((first, shown, total)) = scroll_share(value) else {
        return;
    };
    let track_y = rect.y + 10.0;
    let track_h = (rect.height - 20.0).max(0.0);
    let thumb_h = (track_h * shown / total).max(18.0).min(track_h);
    let scrolled = (first / (total - shown)).clamp(0.0, 1.0);
    prims.push(DrawPrim::Panel {
        x: rect.right() - 7.0,
        y: (track_h - thumb_h).mul_add(scrolled, track_y),
        w: 3.0,
        h: thumb_h,
        radius: 1.5,
        fill: rgba(readable_secondary(roles), 120),
    });
}

/// The `first,shown,total` of a scroll indicator's value, as floats, or
/// `None` where every entry is shown.
fn scroll_share(value: Option<&str>) -> Option<(f32, f32, f32)> {
    let mut parts = value
        .unwrap_or("")
        .split(',')
        .map(|part| part.trim().parse::<u32>().unwrap_or(0));
    let (first, shown, total) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    (total > 0 && shown < total).then_some((first as f32, shown as f32, total as f32))
}

/// A Settings page's scroll indicator ([`PAGE_SCROLL_AUDIT`]) in the page's
/// right gutter: the terminal's resting thumb in points, the sizes of the
/// 100% Windows cell's ([`crate::scrollbar::Metrics`]): under the Fluent
/// chrome WinUI's thin panning indicator (`Metrics::fine`, 2 pt), elsewhere
/// the Classic thin thumb (3 pt, [`crate::scrollbar::rest_width`]), at the
/// Fluent terminal's doubled resting opacity on every chrome
/// ([`crate::scrollbar::rest_alpha`]; the Classic terminal's own, a third
/// lighter, would leave the page's only scroll cue under 3:1, and macOS and
/// Linux show no terminal thumb by default to match), in the page's primary
/// ink as the terminal's is its foreground, composed
/// over the page's surface as the terminal's quads compose over its ground
/// and drawn opaque ([`crate::scrollbar::over_ground`]: this raster blends
/// a translucent fill in linear light, which lifted the thumb toward the
/// page); the margin from the pane's right edge is the terminal thumb's from
/// the window's, so it falls on whole device pixels at 100%, 150% and 200%,
/// and the track is the list's height less the terminal's inset. It used to be a
/// 3 pt thumb in the secondary ink at 47%, 7 pt inside the list's edge: over
/// the cards' right padding and across the gaps between them, antialiased
/// at both edges (the 2026-10-18 judge's `J4F-6-settings-scroller-own-idiom`).
/// It does not widen under the pointer: it is a picture of where the list
/// is, with nothing to drag, and the list moves a stop at a time.
fn paint_page_scroll(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &crate::settings::Roles,
) {
    use crate::scrollbar::{ALPHA_REST, Metrics, over_ground, rest_alpha, rest_width};
    use crate::widget::{DrawPrim, rgba};

    let Some((first, shown, total)) = scroll_share(value) else {
        return;
    };
    let m = Metrics::for_cell(9, 23);
    let fluent = crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Fluent;
    let width = rest_width(m, fluent) as f32;
    let inset = m.inset as f32;
    let track_y = rect.y + inset;
    let track_h = (rect.height - 2.0 * inset).max(0.0);
    let thumb_h = (track_h * shown / total).max(m.min_len as f32).min(track_h);
    let scrolled = (first / (total - shown)).clamp(0.0, 1.0);
    let alpha = rest_alpha(ALPHA_REST, true);
    let (tone, opaque) = over_ground(roles.text_primary, roles.surface, alpha, alpha);
    prims.push(DrawPrim::Panel {
        x: rect.right() - m.margin as f32 - width,
        y: (track_h - thumb_h).mul_add(scrolled, track_y),
        w: width,
        h: thumb_h,
        radius: width / 2.0,
        fill: rgba(tone, opaque),
    });
}

fn paint_messages_severity(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    value: Option<&str>,
    roles: &crate::settings::Roles,
) {
    use crate::widget::{DrawPrim, rgba};

    let value = value.unwrap_or("info");
    let (severity, glyph) = value
        .split_once(':')
        .map_or((value, None), |(sev, rest)| (sev, rest.chars().next()));
    let (icon, color, rail) = match severity {
        "error" => (aterm_render::BandIcon::Error, roles.danger, true),
        "warn" => (
            aterm_render::BandIcon::Warn,
            crate::chrome_band::warn_mark(roles.elevated),
            true,
        ),
        "success" => (aterm_render::BandIcon::Success, roles.success, false),
        _ => (
            aterm_render::BandIcon::Info,
            readable_secondary(roles),
            false,
        ),
    };
    let icon = glyph
        .and_then(aterm_render::BandIcon::for_char)
        .unwrap_or(icon);
    if rail {
        prims.push(DrawPrim::Panel {
            x: rect.x,
            y: rect.y + 3.0,
            w: 3.0,
            h: (rect.height - 6.0).max(0.0),
            radius: 1.5,
            fill: rgba(color, 255),
        });
    }
    // The icon's square: the Body step's capital height reads at about 0.7
    // of its pixel size; the icon's own raster fits its cap to 0.68 of the
    // square, so a square of the step's size stands the mark as tall as the
    // row's capitals.
    let size = native_type_px(TypeStep::Body).get().round();
    let x = rect.x + 3.0 + ((rect.width - 3.0 - size) / 2.0).max(0.0);
    let y = rect.y + ((rect.height - size) / 2.0).max(0.0);
    prims.push(DrawPrim::BandIcon {
        x,
        y,
        size,
        icon,
        color: rgba(color, 255),
    });
}

/// Height a button's colour strip takes from the bottom of the row, band and
/// margin included.
const BUTTON_SWATCH_RESERVE: f32 = 12.0;

/// Paint a [`ButtonSpec::swatches`] strip along the bottom of `rect`: a band
/// in the first colour, the rest as equal cells laid on it, so each colour is
/// seen against the background it is drawn for. Returns the height taken
/// (zero when there is no strip or the row has no room for one).
fn paint_button_swatches(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    swatches: &[[u8; 3]],
    roles: crate::settings::Roles,
) -> f32 {
    use crate::widget::DrawPrim;
    let Some((background, cells)) = swatches.split_first() else {
        return 0.0;
    };
    let inset = 10.0;
    let band_w = rect.width - 2.0 * inset;
    if band_w < 16.0 || rect.height < 2.0 * BUTTON_SWATCH_RESERVE + 8.0 {
        return 0.0;
    }
    let band_h = 7.0;
    let band_x = rect.x + inset;
    let band_y = rect.bottom() - BUTTON_SWATCH_RESERVE + 1.0;
    prims.push(DrawPrim::Panel {
        x: band_x,
        y: band_y,
        w: band_w,
        h: band_h,
        radius: 2.0,
        fill: crate::widget::rgba(*background, 255),
    });
    prims.push(DrawPrim::Stroke {
        x: band_x + 0.5,
        y: band_y + 0.5,
        w: band_w - 1.0,
        h: band_h - 1.0,
        radius: 1.5,
        width: 1.0,
        color: crate::widget::rgba(roles.separator, 160),
    });
    if !cells.is_empty() {
        let pad = 2.0;
        let cell_w = (band_w - 2.0 * pad) / cells.len() as f32;
        for (index, cell) in cells.iter().enumerate() {
            prims.push(DrawPrim::Panel {
                x: band_x + pad + index as f32 * cell_w,
                y: band_y + pad,
                w: cell_w,
                h: band_h - 2.0 * pad,
                radius: 0.0,
                fill: crate::widget::rgba(*cell, 255),
            });
        }
    }
    BUTTON_SWATCH_RESERVE
}

/// Windows draws its switches as WinUI's ToggleSwitch ([`paint_fluent_switch`]): no
/// boxed surface around them, an outlined track when Off, a 12 px thumb. The boxed
/// switch with a filled Off track read as disabled there (the 2026-10-17 audit). macOS
/// and Linux keep the switch they had.
const FLUENT_SWITCH: bool = cfg!(windows);

/// WinUI's ToggleSwitch track, `(width, height)` in logical px.
const FLUENT_TRACK: (f32, f32) = (40.0, 20.0);

/// One switch as WinUI's ToggleSwitch, flush right in `rect`: On is the accent-filled
/// track with the thumb in the on-accent ink at its right; Off is the track OUTLINED in
/// a strong stroke over a faint wash, the thumb in secondary ink at its left. The thumb
/// is 12 px, 14 px under the pointer. A switch that cannot be operated sinks both
/// toward the surface, as the boxed switch does.
fn paint_fluent_switch(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: LogicalRect,
    state: ControlState,
    on: bool,
    roles: crate::settings::Roles,
) {
    use crate::widget::{DrawPrim, rgba};

    let (w, h) = (
        FLUENT_TRACK.0.min((rect.width - 8.0).max(0.0)),
        FLUENT_TRACK.1.min((rect.height - 4.0).max(0.0)),
    );
    let (x, y) = (rect.right() - w - 8.0, rect.y + (rect.height - h) / 2.0);
    let enabled = state.enabled;
    let alpha = if enabled { 255 } else { 120 };
    if on {
        let fill = if enabled && !state.muted {
            roles.accent
        } else if enabled {
            roles.text_tertiary
        } else {
            mix_rgb(roles.accent, roles.elevated, 0.55)
        };
        prims.push(DrawPrim::Capsule {
            x,
            y,
            w,
            h,
            frac: 1.0,
            fill: rgba(fill, alpha),
            track: rgba(fill, alpha),
        });
    } else {
        let wash = if state.hovered { 0x1A } else { 0x0F };
        prims.push(DrawPrim::Capsule {
            x,
            y,
            w,
            h,
            frac: 0.0,
            fill: rgba(roles.text_primary, wash),
            track: rgba(roles.text_primary, wash),
        });
        prims.push(DrawPrim::Stroke {
            x: x + 0.5,
            y: y + 0.5,
            w: (w - 1.0).max(0.0),
            h: (h - 1.0).max(0.0),
            radius: (h - 1.0).max(0.0) / 2.0,
            width: 1.0,
            color: rgba(roles.text_secondary, if enabled { 200 } else { 90 }),
        });
    }
    let r: f32 = if state.hovered && enabled { 7.0 } else { 6.0 };
    let inset = h / 2.0;
    prims.push(DrawPrim::Dot {
        cx: if on { x + w - inset } else { x + inset },
        cy: y + h / 2.0,
        r: r.min((h / 2.0 - 2.0).max(2.0)),
        color: rgba(
            if !enabled {
                mix_rgb(roles.text_tertiary, roles.text_secondary, 0.35)
            } else if on {
                roles.on_accent
            } else {
                roles.text_secondary
            },
            if enabled { 255 } else { 170 },
        ),
    });
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
                mix_rgb(
                    roles.elevated,
                    roles.accent,
                    crate::native_appearance::ACCENT_WASH_PRESSED,
                )
            } else if state.hovered {
                mix_rgb(roles.elevated, roles.text_primary, 0.09)
            } else {
                control_rest_fill(&roles)
            },
            255,
        ),
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
    if !state.enabled {
        // Disabled remains visibly subordinate, but never disappears into the
        // surface—especially for icon-only document actions.
        mix_rgb(roles.text_tertiary, roles.text_secondary, 0.35)
    } else if state.muted {
        // Idle ([`ControlState::muted`]): the value is said in quiet ink, a
        // step below a live control's, and still legible — the control works.
        quiet_text(roles)
    } else {
        roles.text_primary
    }
}

/// Native tab apps use an OS-like, zoom-independent type ladder. We still mint
/// every size through the repository's named type-scale proof token, but pick
/// the base that lands exactly on the app-shell contract rather than inheriting
/// the terminal's monospace zoom: 24 / 20 / 13 / 13 / 11 logical pixels.
pub(crate) fn native_type_px(step: crate::type_scale::TypeStep) -> crate::type_scale::StepPx {
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
    crate::native_appearance::mix_srgb(a, b, amount)
}

/// The Top Settings hero banner's `(sky, rim)` — the ONE place the rainbow
/// banner's surface is decided, resolved from the SAME role palette every other
/// Settings card paints from ([`crate::settings::paint_rainbow_banner`] takes
/// them the way [`crate::settings::paint_settings_aurora`] takes `surface`).
///
/// A SKY is the most OPEN neutral the palette has, so the base is whichever of
/// `surface` and `elevated` MEASURES lighter. It used to be picked by proxy —
/// `elevated` when the chrome classifies dark, `surface` otherwise — on the
/// reasoning that both are built by walking the window background toward the
/// ink, so the page paper is the open one on light chrome and the card is the
/// open one on dark chrome. That reasoning is sound and the proxy still gets it
/// wrong, because the two questions are answered by DIFFERENT predicates: the
/// ramp's direction is fixed by which of `bg` and `fg` is lighter, while the
/// proxy classifies one derived neutral against the dark/light pole, and one
/// step of ramp is enough to cross it. A theme at `#979797` on dark ink builds
/// `surface #8F8F8F` over `elevated #868686` while `elevated` itself classifies
/// dark, so the proxy chose the darker of the two and the banner sank below its
/// own cards. Measuring answers the question the doc always asked.
///
/// The POUR is picked by measuring too, and against the same base, which is the
/// half the proxy kept. `surface_is_dark(base)` asks the dark/light pole to
/// stand in for "will this green lift the sky or shade it" — a question about
/// two specific colours, one of which is the green. So the code asks it of the
/// two colours instead: pour the leaf green when it MEASURES more open than the
/// base, and the deep one otherwise. On the light-ink half of the mid-tone band
/// the threshold said "light" of an `#A1A1A1` base and poured the forest green
/// into it, and because that base is `elevated` — the very tone the cards on the
/// banner are painted in — the shade had nowhere to come from and the banner
/// sank below its own cards across the whole upper band.
///
/// The tint is the palette's own green, from the same OKLCH conditioning every
/// other role goes through, so the mint identity survives on BOTH sides instead
/// of importing the landing page's authored `#EDF6EC` — which was byte-identical
/// in all four appearance states and left a near-white slab in the middle of the
/// forced-DARK page. It takes that green from
/// [`crate::native_appearance::success_tint`] and NOT from the `success` role:
/// the role is ink and carries ink's 4.5:1 floor, which against a mid-tone
/// surface is unreachable upward and pushes the role to a near-black green — a
/// wash toward THAT is not mint, it is mud. The tint is the same seed with the
/// floor left off, which is what decoration is entitled to.
///
/// The two wash amounts are NOT a fudge, they follow from where the tint SITS
/// relative to the sky, which is the same measurement that chose it. A sky
/// already above the leaf green — near-white paper — can only take the
/// `0.49`-lightness one, and every part of a wash toward it is a SHADE: pour
/// much and the sky turns sage and sits heavier than the white cards it is
/// supposed to float above, which is what a first pass at a flat 0.14 did. A sky
/// below the leaf green takes the `0.72`-lightness one and the same wash is a
/// TINT that lifts — it can take three times as much before it stops reading as
/// a neutral with weather in it. A whisper where the green can only shade and a
/// real pour where it can lift is what lands both in the same perceptual place,
/// which is the point.
///
/// The rim is the card `separator` carrying the same tint at double strength, so
/// the banner's hairline is the same material as every other card's edge with
/// one step of green in it, not a second colour.
///
/// And the sky is HELD to the property its name states rather than trusted to
/// land on it. When `elevated` is the most open neutral the base IS the cards,
/// so the sky begins with no headroom whatsoever and even the lifting pour can
/// come back a hair under: a chromatic mix is not monotone in luminance — near
/// the crossover the leaf green's low red and blue cost more than its green
/// gains. A pour is wanted for its HUE; the height it happens to spend is not
/// part of the ask, and [`crate::native_appearance::at_least_as_open_as`] hands
/// that back, against the cards themselves. Where the base is `surface` it is a
/// full ramp step above the cards, the intended SHADE fits inside that step and
/// the hold never fires — all four shipping appearance states are byte-identical
/// with it and without it.
fn rainbow_banner_sky(roles: &crate::settings::Roles) -> ([u8; 3], [u8; 3]) {
    // `contrast_ratio` against black is monotone in relative luminance, so this
    // is a luminance probe — the same one the banner's test uses to state the
    // property "a sky is never heavier than the cards standing on it".
    let openness = |c: [u8; 3]| crate::native_appearance::contrast_ratio(c, [0, 0, 0]);
    let base = if openness(roles.elevated) >= openness(roles.surface) {
        roles.elevated
    } else {
        roles.surface
    };
    // Which way does the pour go? MEASURE it, on the same probe and against the
    // same palette the base was chosen with: the leaf green either sits above
    // this base, in which case pouring it LIFTS the sky and it can take a real
    // pour, or it does not, and every part of the pour is a SHADE the sky pays
    // for in height. That comparison is the question `success_tint` is asking
    // when it asks for a side — and unlike `surface_is_dark`, it is asked of the
    // two colours that are actually about to be mixed, so it cannot be right
    // about the window and wrong about the mix.
    let lift = openness(crate::native_appearance::success_tint(true)) >= openness(base);
    let tint = crate::native_appearance::success_tint(lift);
    let wash = if lift { 0.14 } else { 0.045 };
    (
        crate::native_appearance::at_least_as_open_as(mix_rgb(base, tint, wash), roles.elevated),
        mix_rgb(roles.separator, tint, wash * 2.0),
    )
}

pub(crate) fn readable_secondary(roles: &crate::settings::Roles) -> [u8; 3] {
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
        StyleRef::Accent => roles.accent_ink,
        StyleRef::Success => roles.success,
        StyleRef::Danger | StyleRef::DangerCompact => roles.danger,
        StyleRef::Quiet => quiet_text(roles),
        StyleRef::Plain
        | StyleRef::Secondary
        | StyleRef::Navigation
        | StyleRef::Setting
        | StyleRef::Chip => readable_secondary(roles),
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
    /// A button whose painted words are not one phrase of its accessible
    /// name ([`visible_label_in_name`]). An authoring rule a development build
    /// holds, so the variant exists where its one refusal does.
    #[cfg(debug_assertions)]
    VisibleLabelNotInName(UiKey),
}

/// THE WORDS A CONTROL PAINTS ARE IN ITS NAME (WCAG 2.5.3, Label in Name):
/// a speech user says what they see, so a button's painted words
/// (`visual_label`) must be one phrase of its accessible name (`label`),
/// whole words, case and spacing aside, an elision's "…" taken off;
/// anything the name adds beyond them belongs after them or in the
/// description. A painted word is a word of the name, never the inside of a
/// longer one ("View" is not said by "Review"), except where an ellipsis
/// cut it. An empty painted label (a card surface) or a glyph with no word
/// in it (an arrow) carries no words to match.
pub(crate) fn visible_label_in_name(visual: &str, name: &str) -> bool {
    // A label painted on lines ([`native_button_label_lines`]) is its words
    // with spaces between; one broken inside a word ends its line in a `-` or
    // after a `/`, which joins the word again — a hyphen the break added, or
    // one the word has; and a code identifier broken with no mark (an
    // aterm.toml key) is its lines joined as they are.
    if visual.contains('\n') {
        let joined = |hyphen: &str| {
            visual
                .replace("-\n", hyphen)
                .replace("/\n", "/")
                .replace('\n', " ")
        };
        return [joined(""), joined("-"), visual.replace('\n', "")]
            .iter()
            .any(|visual| visible_label_in_name(visual, name));
    }
    let words = |text: &str| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let visual = words(visual).replace("...", "\u{2026}");
    // A glyph with no word in it ("‹") is an icon: there is nothing to say.
    if !visual.chars().any(char::is_alphanumeric) {
        return true;
    }
    // An elided label is pieces of its words, in order, wherever the
    // ellipsis took the rest ("More sty…", "bidi — Bi…onal text"): a piece
    // may end inside a word where an ellipsis follows it, and start inside
    // one where an ellipsis comes before it, and nowhere else.
    let name = words(name);
    let edge = |text: &str, at: usize, before: bool| {
        let next = if before {
            text[..at].chars().next_back()
        } else {
            text[at..].chars().next()
        };
        next.is_none_or(|ch| !ch.is_alphanumeric())
    };
    let pieces: Vec<&str> = visual.split('\u{2026}').collect();
    let mut from = 0;
    for (index, raw) in pieces.iter().enumerate() {
        let piece = raw.trim();
        if piece.is_empty() {
            continue;
        }
        // Cut where an ellipsis touches the piece (no space between).
        let cut_before = index > 0 && !raw.starts_with(' ');
        let cut_after = index + 1 < pieces.len() && !raw.ends_with(' ');
        let found = name[from..]
            .match_indices(piece)
            .map(|(at, _)| from + at)
            .find(|&at| {
                (cut_before || edge(&name, at, true))
                    && (cut_after || edge(&name, at + piece.len(), false))
            });
        match found {
            Some(at) => from = at + piece.len(),
            None => return false,
        }
    }
    true
}

struct Compiler {
    // FxHash, not SipHash: this set exists only to reject duplicate keys inside
    // one compile, is keyed by internal authored keys (never untrusted input),
    // and is rebuilt from scratch every native frame — the DoS resistance was
    // paying for nothing.
    seen: FxHashSet<UiKey>,
    /// Segments that are not their control's Tab stop ([`UiNode::segmented`]).
    not_tab_stops: FxHashSet<UiKey>,
    output: CompiledUi,
}

/// A segmented control's segments ([`UiNode::segmented`]): the buttons and
/// switches under it, in order, with their state — its children, or the
/// lines a narrow column wraps them onto.
pub(crate) fn segments_of(children: &[UiNode]) -> Vec<(UiKey, ControlState)> {
    fn walk(children: &[UiNode], out: &mut Vec<(UiKey, ControlState)>) {
        for child in children {
            if child.paint_only {
                continue;
            }
            match &child.content {
                UiContent::Button(control) => out.push((child.key.clone(), control.state)),
                UiContent::Switch(control) => out.push((child.key.clone(), control.state)),
                _ => walk(&child.children, out),
            }
        }
    }
    let mut out = Vec::new();
    walk(children, &mut out);
    out
}

impl Compiler {
    /// Takes the node BY VALUE so its `content` can be moved into the paint
    /// node. Everything else the node carries (`layout`, `children`) is still
    /// read in full afterwards — the destructure below just separates the field
    /// that moves from the fields that do not.
    fn node(
        &mut self,
        node: UiNode,
        rect: LogicalRect,
        inherited_clip: LogicalRect,
        parent: Option<&UiKey>,
        list: Option<ListContext>,
    ) -> Result<(), CompileError> {
        let UiNode {
            key,
            layout,
            content,
            children,
            paint_only,
            room,
            offscreen,
            segmented,
            joins,
        } = node;
        if segmented {
            // One Tab stop for the control: the focused segment, else the
            // selected one, else the first that takes the keyboard.
            let segments = segments_of(&children);
            let stop = segments
                .iter()
                .find(|(_, state)| state.focused)
                .or_else(|| segments.iter().find(|(_, state)| state.selected))
                .or_else(|| segments.iter().find(|(_, state)| state.enabled))
                .map(|(key, _)| key.clone());
            for (segment, _) in &segments {
                if stop.as_ref() != Some(segment) {
                    self.not_tab_stops.insert(segment.clone());
                }
            }
            self.output.segment_groups.push(SegmentGroup {
                key: key.clone(),
                segments: segments.into_iter().map(|(key, _)| key).collect(),
            });
        }
        if key.as_str().trim().is_empty() {
            return Err(CompileError::EmptyKey);
        }
        if !layout.is_valid() || !rect.is_valid() {
            return Err(CompileError::InvalidLayout(key));
        }
        if !self.seen.insert(key.clone()) {
            return Err(CompileError::DuplicateKey(key));
        }
        if let UiContent::Custom(custom) = &content
            && custom.audit_id.trim().is_empty()
        {
            return Err(CompileError::CustomWithoutAudit(key));
        }

        // Runs BEFORE `content` moves into the paint node; `semantic()` clones
        // only the labels it needs.
        let projection = content.semantic();
        let description = content.description();
        let look_first = content.look_first();
        let value_text = content.value_text();
        if paint_only && (projection.action.is_some() || projection.focusable) {
            return Err(CompileError::PaintOnlyAction(key));
        }
        // An authoring rule, held where every page compiles in development
        // and in the tests that walk every route; a release never refuses a
        // page over a wording.
        #[cfg(debug_assertions)]
        if let UiContent::Button(control) = &content
            && !paint_only
            && let Some(visual) = control.spec.visual_label.as_deref()
            && !visible_label_in_name(visual, &control.spec.label)
        {
            return Err(CompileError::VisibleLabelNotInName(key));
        }
        if offscreen {
            return self.offscreen_node(
                key,
                &layout,
                children,
                rect,
                parent,
                (projection, description, value_text, paint_only),
            );
        }

        let own_clip = inherited_clip.intersect(rect);
        let Some(visible) = own_clip else {
            // A row the list's edge cuts away entirely is off its screen in a
            // list that holds every row in the tree ([`UiNode::offscreen`]).
            if list.is_some_and(|list| list.lists_all) {
                return self.offscreen_node(
                    key,
                    &layout,
                    children,
                    rect,
                    parent,
                    (projection, description, value_text, paint_only),
                );
            }
            // A clipped subtree is not materialized in any observer.  Virtualized
            // views can compile requested semantic ranges separately.
            return Ok(());
        };
        // THE LIST'S EDGE RULE ([`UiNode::room`]): inside a scrolling list a
        // node is live over exactly the part of it its room shows, whole or
        // cut by either edge; what the strip under the room holds is a picture.
        let live_visible = match list {
            Some(list) => visible.intersect(list.room),
            None => Some(visible),
        };
        let edge_cut = list.is_some_and(|list| {
            rect.y < list.room.y - CLIP_EDGE_EPSILON
                || rect.bottom() > list.room.bottom() + CLIP_EDGE_EPSILON
        });
        let fade = list
            .and_then(|list| list.fade)
            .filter(|(top, _)| visible.bottom() > top + CLIP_EDGE_EPSILON);
        let across = |edge: &f32| {
            rect.y < edge - CLIP_EDGE_EPSILON && rect.bottom() > edge + CLIP_EDGE_EPSILON
        };
        let cut_edges = CutEdges {
            top: list.map(|list| list.top_edge).filter(across),
            bottom: list.and_then(|list| list.bottom_edge).filter(across),
        };
        self.output.paint.push(PaintNode {
            key: key.clone(),
            rect,
            clip: visible,
            content,
            peek: edge_cut,
            fade,
            cut_edges,
            joins,
        });
        let live = match live_visible {
            Some(shown) if !paint_only => Some(shown),
            _ => None,
        };
        // What of an enabled control the strip under a list's room shows is
        // its picture there: a press on it scrolls it into the room.
        if let Some(list) = list
            && !paint_only
            && projection.action.is_some()
            && projection.state.is_none_or(|state| state.enabled)
            && visible.bottom() > list.room.bottom() + CLIP_EDGE_EPSILON
        {
            let top = visible.y.max(list.room.bottom());
            self.output.list_pictures.push(ListPicture {
                key: key.clone(),
                rect: LogicalRect::new(visible.x, top, visible.width, visible.bottom() - top),
            });
        }
        // A list that holds every row in the tree has a node its room does
        // not show there off its screen, with its whole bounds.
        let listed = live.is_none() && !paint_only && list.is_some_and(|list| list.lists_all);
        if listed {
            self.output.semantics.push(SemanticNode {
                key: key.clone(),
                parent: parent.cloned(),
                rect,
                role: projection.role,
                label: projection.label.clone(),
                value: projection.value.clone(),
                state: projection.state,
                action: projection.action.clone(),
                audit_id: projection.audit_id,
                description: description.clone(),
                offscreen: true,
                value_text: value_text.clone(),
            });
        }
        if let Some(shown) = live {
            self.output.semantics.push(SemanticNode {
                key: key.clone(),
                parent: parent.cloned(),
                rect: shown,
                role: projection.role,
                label: projection.label,
                value: projection.value,
                state: projection.state,
                action: projection.action.clone(),
                audit_id: projection.audit_id,
                description,
                offscreen: false,
                value_text,
            });
            if projection.focusable && !self.not_tab_stops.contains(&key) {
                self.output.focus_order.push(key.clone());
            }
            if let Some(action) = projection.action
                && projection.state.is_none_or(|state| state.enabled)
            {
                self.output.hits.push(HitRegion {
                    key: key.clone(),
                    rect: shown,
                    action,
                    look_first,
                });
            }
        }

        let content_rect = rect.inset(layout.padding);
        let child_clip = if layout.clip {
            inherited_clip.intersect(content_rect).unwrap_or_default()
        } else {
            inherited_clip
        };
        if child_clip.is_empty() {
            return Ok(());
        }
        // A list's children inherit its room, its top edge and the strip under
        // it. A room inside a list (the rows under a kept caption) is that
        // list's room cut down to its own, its top edge its own, over the strip
        // its list keeps fading under it.
        let list = match room {
            Some(room) => {
                let shown = LogicalRect::new(rect.x, rect.y, rect.width, room.height.max(0.0));
                let shown = match list {
                    Some(outer) => shown.intersect(outer.room).unwrap_or_default(),
                    None => shown,
                };
                let fade = (room.fades && rect.height > room.height + CLIP_EDGE_EPSILON)
                    .then(|| (rect.y + room.height, rect.bottom()))
                    .or(list.and_then(|outer| outer.fade));
                Some(ListContext {
                    room: shown,
                    fade,
                    lists_all: room.lists_all || list.is_some_and(|outer| outer.lists_all),
                    top_edge: shown.y,
                    // Where the list's children are cut off under it, its
                    // own clip inside every clip over it; a list that fades
                    // there draws a picture fading out instead.
                    bottom_edge: fade.is_none().then(|| child_clip.bottom()),
                })
            }
            None => list,
        };
        let child_rects = layout_children(&layout, &children, content_rect)?;
        let semantic_parent = if live.is_some() || listed {
            Some(&key)
        } else {
            parent
        };
        for (child, child_rect) in children.into_iter().zip(child_rects) {
            // A child reaching into this node's padding ([`Layout::outdent`])
            // is clipped to the content rect grown as far as it reaches.
            let clip = if layout.clip && child.layout.outdent != Insets::default() {
                inherited_clip
                    .intersect(content_rect.outset(child.layout.outdent))
                    .unwrap_or_default()
            } else {
                child_clip
            };
            self.node(child, child_rect, clip, semantic_parent, list)?;
        }
        Ok(())
    }

    /// A list's row off its screen ([`UiNode::offscreen`]) and everything in
    /// it: semantics at the laid-out bounds, nothing painted, hit or
    /// focusable, whatever the list's clip.
    fn offscreen_node(
        &mut self,
        key: UiKey,
        layout: &Layout,
        children: Vec<UiNode>,
        rect: LogicalRect,
        parent: Option<&UiKey>,
        (projection, description, value_text, paint_only): (
            SemanticProjection,
            Option<String>,
            Option<String>,
            bool,
        ),
    ) -> Result<(), CompileError> {
        if !paint_only {
            self.output.semantics.push(SemanticNode {
                key: key.clone(),
                parent: parent.cloned(),
                rect,
                role: projection.role,
                label: projection.label,
                value: projection.value,
                state: projection.state,
                action: projection.action,
                audit_id: projection.audit_id,
                description,
                offscreen: true,
                value_text,
            });
        }
        let parent = if paint_only { parent } else { Some(&key) };
        let child_rects = layout_children(layout, &children, rect.inset(layout.padding))?;
        for (mut child, child_rect) in children.into_iter().zip(child_rects) {
            child.offscreen = true;
            self.node(child, child_rect, child_rect, parent, None)?;
        }
        Ok(())
    }
}

fn layout_children(
    layout: &Layout,
    children: &[UiNode],
    content: LogicalRect,
) -> Result<Vec<LogicalRect>, CompileError> {
    if children.is_empty() {
        return Ok(Vec::new());
    }
    let mut rects = layout_children_in(layout, children, content)?;
    // Each child reaching past the content rect grows by its outdent.
    for (rect, child) in rects.iter_mut().zip(children) {
        if child.layout.outdent != Insets::default() {
            *rect = rect.outset(child.layout.outdent);
        }
    }
    Ok(rects)
}

/// [`layout_children`] before any child's [`Layout::outdent`].
fn layout_children_in(
    layout: &Layout,
    children: &[UiNode],
    content: LogicalRect,
) -> Result<Vec<LogicalRect>, CompileError> {
    if layout.flow == Flow::Overlay {
        return children
            .iter()
            .map(|child| child_rect_overlay(child, content))
            .collect::<Result<Vec<_>, _>>();
    }

    let is_row = layout.flow == Flow::Row;
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
    let total_gap = layout.gap * children.len().saturating_sub(1) as f32;
    let available = (main_extent - total_gap).max(0.0);
    let mut fixed = 0.0;
    let mut fills = 0usize;
    let mut main_lengths = Vec::with_capacity(children.len());
    for child in children {
        let main = if is_row {
            child.layout.width
        } else {
            child.layout.height
        };
        let value = match main {
            Length::Fill => {
                fills += 1;
                None
            }
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

    let mut cursor = if is_row { content.x } else { content.y } - layout.offset;
    let mut out = Vec::with_capacity(children.len());
    for (child, main) in children.iter().zip(main_lengths) {
        let main = main.unwrap_or(fill).max(0.0);
        let cross_length = if is_row {
            child.layout.height
        } else {
            child.layout.width
        };
        let cross = resolve_length(cross_length, cross_extent);
        let rect = if is_row {
            LogicalRect::new(cursor, content.y, main, cross)
        } else {
            LogicalRect::new(content.x, cursor, cross, main)
        };
        if !rect.is_valid() {
            return Err(CompileError::InvalidLayout(child.key.clone()));
        }
        out.push(rect);
        cursor += main + layout.gap;
    }
    Ok(out)
}

fn child_rect_overlay(child: &UiNode, content: LogicalRect) -> Result<LogicalRect, CompileError> {
    let width = resolve_length(child.layout.width, content.width);
    let height = resolve_length(child.layout.height, content.height);
    let rect = LogicalRect::new(content.x, content.y, width, height);
    if rect.is_valid() {
        Ok(rect)
    } else {
        Err(CompileError::InvalidLayout(child.key.clone()))
    }
}

fn resolve_length(length: Length, available: f32) -> f32 {
    match length {
        Length::Fill => available,
        Length::Fixed(value) => value.min(available),
        Length::Fraction(value) => available * value,
    }
    .max(0.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A ONE-LINE STATUS IS CUT AFTER A WHOLE WORD (the message band's rule,
    /// on Settings' status line): at every width the cut keeps whole words
    /// and `…`, never a word's first letters, and a status that fits stays
    /// whole. Negative control: the general label elision it narrows cuts
    /// the same status mid-word.
    #[test]
    fn a_status_is_cut_after_a_whole_word() {
        let status = "Cursor blink was not saved: ~/.config/aterm isn\u{2019}t writable \u{b7} check its permissions";
        let whole = native_status_label_width(status);
        let mut mid_word = 0;
        let mut width = 40.0_f32;
        while width < whole + 4.0 {
            let shown = fit_native_status_label(status, width);
            if shown != status {
                let stem = shown.strip_suffix('…').expect("a cut ends in an ellipsis");
                // Only the first word, alone too wide, is cut inside.
                assert!(
                    !stem.contains(char::is_whitespace)
                        || status[stem.len()..].starts_with(|c: char| {
                            c.is_whitespace() || matches!(c, ':' | ',' | '·')
                        }),
                    "{width}: {shown:?} cuts a word"
                );
                assert!(native_status_label_width(&shown) <= width + TEXT_FIT_SLACK);
            }
            let general = elide_text_label(
                status,
                width,
                native_type_px(TypeStep::Caption).get(),
                crate::widget::TextFace::Ui,
            );
            if let Some(stem) = general.strip_suffix('…')
                && !status[stem.len()..].starts_with(char::is_whitespace)
            {
                mid_word += 1;
            }
            width += 3.0;
        }
        assert_eq!(fit_native_status_label(status, whole + 1.0), status);
        assert!(
            mid_word > 0,
            "the control: the general elision cuts mid-word"
        );
    }

    /// A CUT ONE-LINE LABEL ENDS AFTER A WHOLE WORD, as a cut status does: a
    /// body label (Messages' one-line heads) and a button label at every
    /// width keep whole words and `…`, never a word's first letters ("Claude
    /// Code is not inst…" at 36 columns); one whose first word alone is too
    /// wide is cut inside it, and one that fits stays whole.
    #[test]
    fn a_cut_one_line_label_ends_after_a_whole_word() {
        let label = "Claude Code is not installed";
        let words: Vec<&str> = label.split_whitespace().collect();
        for (fit, measure) in [
            (
                fit_native_text_label as fn(&str, f32) -> String,
                native_type_px(TypeStep::Body).get(),
            ),
            (
                fit_native_button_label as fn(&str, f32) -> String,
                native_type_px(TypeStep::Secondary).get(),
            ),
        ] {
            let whole = crate::tray_raster::ui_text_width(label, measure);
            let first = crate::tray_raster::ui_text_width("Claude…", measure);
            let mut width = first + 1.0;
            while width < whole {
                let cut = fit(label, width);
                let stem = cut.strip_suffix('…').unwrap_or(&cut).trim_end();
                let kept = stem.split_whitespace().count();
                assert_eq!(
                    stem,
                    words[..kept].join(" "),
                    "{width}: {cut:?} ends after a whole word"
                );
                width += 3.0;
            }
            assert_eq!(fit(label, whole + 1.0), label);
        }
    }

    /// PAGE-CHROME TEXT ON THE AURORA READS. The heading, subtitle, pager and
    /// empty line sit straight on the Settings aurora, so every tile holds the
    /// quietest of those inks (and the heading's) at 4.5:1, on dark and light
    /// chrome. The negative control is the wash's unheld pour, which the dark
    /// default palette's yellow-green band fails.
    #[test]
    fn every_aurora_tile_holds_the_page_chrome_ink() {
        use crate::native_appearance::{contrast_ratio, forced_chrome_theme};
        use crate::settings::{AURORA_INK_FLOOR, aurora_tile_unheld};
        let host = aterm_render::Theme::default();
        let themes = [
            ("default", host),
            ("forced light", forced_chrome_theme(host, false)),
            ("forced dark", forced_chrome_theme(host, true)),
        ];
        for (name, theme) in themes {
            let roles = crate::settings::Roles::from_theme(theme);
            let ink = quiet_text(&roles);
            let mut prims = Vec::new();
            crate::settings::paint_settings_aurora(
                &mut prims,
                0.0,
                0.0,
                1_120.0,
                640.0,
                roles.surface,
                ink,
            );
            assert!(!prims.is_empty(), "{name}: the aurora paints");
            let mut darkest = f32::INFINITY;
            for prim in &prims {
                let crate::widget::DrawPrim::Panel { fill, .. } = prim else {
                    panic!("{name}: the aurora is tiles");
                };
                let tile = [fill[0], fill[1], fill[2]];
                for (what, text) in [("quiet", ink), ("heading", roles.text_primary)] {
                    let ratio = contrast_ratio(text, tile);
                    darkest = darkest.min(ratio);
                    assert!(
                        ratio >= AURORA_INK_FLOOR,
                        "{name}: {what} ink on tile {tile:?} is {ratio:.2}:1"
                    );
                }
            }
            assert!(darkest.is_finite());
        }
        let roles = crate::settings::Roles::from_theme(host);
        let ink = quiet_text(&roles);
        let failing = (0..64)
            .flat_map(|j| (0..112).map(move |i| (i, j)))
            .filter(|&(i, j)| {
                contrast_ratio(ink, aurora_tile_unheld(roles.surface, i, j)) < AURORA_INK_FLOOR
            })
            .count();
        assert!(
            failing > 0,
            "control: the unheld pour fails the quiet ink somewhere on the default palette"
        );
    }

    /// A CONTROL THAT CANNOT BE PRESSED IS NEVER PAINTED FOCUSED (s-25): a button
    /// that disabled itself under the keyboard (a pager's Older at its end) kept
    /// its focus ring though it had left the focus order. NEGATIVE CONTROL: the
    /// same button enabled takes the focus.
    #[test]
    fn a_disabled_control_is_never_painted_focused() {
        let button = |enabled: bool| {
            UiNode::new(
                "test/older",
                UiContent::Button(
                    Control::new(ButtonSpec::new("Older"), ActionId::new("test/older")).state(
                        ControlState {
                            enabled,
                            ..ControlState::default()
                        },
                    ),
                ),
            )
            .layout(Layout::default().height(Length::Fixed(32.0)))
        };
        for enabled in [false, true] {
            let tree = UiTree::new(
                UiNode::new("test", UiContent::Group(GroupSpec::new("test")))
                    .layout(Layout::column())
                    .children(vec![button(enabled)]),
            )
            .apply_focus(Some(&UiKey::new("test/older")));
            let UiContent::Button(control) = &tree.root.children[0].content else {
                unreachable!();
            };
            assert_eq!(control.state.focused, enabled, "enabled={enabled}");
            assert_eq!(control.state.focus_visible, enabled, "enabled={enabled}");
        }
    }

    /// The hero banner's sky survives a MID-TONE terminal theme.
    ///
    /// The banner tints its sky with the palette's own green, and the first pass
    /// took that green from the `success` ROLE. `success` is INK — it labels and
    /// it rules — so [`crate::native_appearance::roles`] holds it to 4.5:1
    /// against `surface`, and [`crate::native_appearance::ensure_contrast`]
    /// reaches a target by moving LIGHTNESS. Against the neutral
    /// `#8F8F8F`..`#A3A3A3` band a real terminal theme lands on, 4.5:1 is
    /// unreachable upward: the only route left is DOWN, and the role arrives as
    /// a near-black green (`#000901` over `#777777`). Pouring 14 % of that into
    /// the banner did not tint it mint, it turned it into a slab measurably
    /// DARKER than the cards standing on it — 3.32 against the cards' 4.12 — the
    /// same "a card that does not follow the page" defect the banner's authored
    /// `#EDF6EC` was replaced to fix, arriving from the other side.
    ///
    /// The banner now takes the un-floored hue
    /// ([`crate::native_appearance::success_tint`]) and picks BOTH its base and
    /// its pour by MEASURING — which neutral is the more open, and which of the
    /// two greens is more open than that — so both halves of "a sky is the most
    /// open neutral, with green weather in it" hold on every theme rather than
    /// only on the four the shipping palettes produce. Where the base IS the
    /// cards there is no height to spend and the pour is held to that
    /// ([`crate::native_appearance::at_least_as_open_as`]), so the first
    /// assertion below is a property of the code and not of where a particular
    /// theme's green happened to land.
    #[test]
    fn the_hero_banner_sky_survives_a_mid_tone_terminal_theme() {
        use crate::native_appearance::{contrast_ratio, forced_chrome_theme};
        use aterm_render::Theme;

        // `contrast_ratio` against black is monotone in relative luminance, so
        // it reads as a luminance probe.
        let openness = |c: [u8; 3]| contrast_ratio(c, [0, 0, 0]);
        // How far a colour leans green, in the crude channel terms the wash
        // actually moves. A tint the eye can see has to beat rounding by more
        // than a step or two.
        let green_lean = |c: [u8; 3]| i32::from(c[1]) * 2 - i32::from(c[0]) - i32::from(c[2]);
        const VISIBLE_LEAN: i32 = 6;

        // The band where a terminal background classifies neither light nor
        // dark with any margin, swept in BOTH ink polarities at every step and
        // with one chromatic member in each so the assertions cannot lean on a
        // neutral base.
        //
        // Both halves are load-bearing and the light-ink half is the one a
        // previous pass sampled too thinly: it stopped at `#808080`, and the
        // sky sank below its cards from `#8C8C8C` up — `#9C9C9C on light ink`
        // measured 8.21 against the cards' 8.53. The classifier that chose the
        // pour crosses at a different place on each side, so a band sampled on
        // one side is not a band.
        let mut band = Vec::new();
        let mut bg = 0x60_u32;
        while bg <= 0xB0 {
            let neutral = (bg << 16) | (bg << 8) | bg;
            let name = format!("#{bg:02X}{bg:02X}{bg:02X}");
            band.push((format!("{name} on light ink"), neutral, 0x00EE_EEEE_u32));
            band.push((format!("{name} on dark ink"), neutral, 0x0011_1111));
            bg += 4;
        }
        band.push(("#797676 on light ink".to_string(), 0x0079_7676, 0x00EE_EEEE));
        band.push(("#999955 on dark ink".to_string(), 0x0099_9955, 0x0022_2211));
        band.push(("#999955 on light ink".to_string(), 0x0099_9955, 0x00EE_EEEE));
        let mut themes: Vec<(String, Theme)> = band
            .into_iter()
            .map(|(label, bg, fg)| {
                let theme = Theme {
                    fg,
                    bg,
                    cursor: 0x0085_9900,
                    selection: 0x00EE_E8D5,
                };
                (label, theme)
            })
            .collect();
        // And both forced chrome sides, whose skies sit at the band's ends.
        themes.push((
            "forced-light".to_string(),
            forced_chrome_theme(Theme::default(), false),
        ));
        themes.push((
            "forced-dark".to_string(),
            forced_chrome_theme(FORCED_DARK_TERMINAL, true),
        ));

        for (label, theme) in themes {
            let roles = crate::settings::Roles::from_theme(theme);
            let (sky, rim) = rainbow_banner_sky(&roles);

            // A SKY, never a slab: the banner is the most open surface on the
            // page, so it can never sit heavier than the cards on it.
            assert!(
                openness(sky) >= openness(roles.elevated),
                "{label}: the banner sky {sky:?} ({:.2}) sits heavier than the cards {:?} ({:.2})",
                openness(sky),
                roles.elevated,
                openness(roles.elevated),
            );
            // And it is MINT, not mud — the wash has to arrive as green, which
            // a wash toward a contrast-clamped near-black green does not.
            let base = if openness(roles.elevated) >= openness(roles.surface) {
                roles.elevated
            } else {
                roles.surface
            };
            assert!(
                green_lean(sky) - green_lean(base) >= VISIBLE_LEAN,
                "{label}: the banner sky {sky:?} carries no visible green over its base {base:?}",
            );
            // The hairline keeps the same job it has on every other state.
            let rim_vs_sky = contrast_ratio(rim, sky);
            assert!(
                rim_vs_sky > 1.08,
                "{label}: the banner rim {rim:?} vanishes into its own sky ({rim_vs_sky:.2}:1)"
            );
            assert!(
                rim_vs_sky <= contrast_ratio(roles.text_primary, sky),
                "{label}: the banner rim outshouts the page's own ink"
            );
        }
    }

    /// THE ATTENTION BADGE (2026-09-22): a rail row that wears it paints ONE dot in
    /// the danger role inside the row — past its label, on the pictogram's
    /// shoulder of an icon-only row, and clear of a trailing chevron's slot — and
    /// the fit audit gives the dot its room ([`badge_reserve`]). A row without it
    /// paints no dot at all.
    #[test]
    fn a_badged_rail_row_paints_one_danger_dot_clear_of_its_label_and_chevron() {
        use crate::native_appearance::forced_chrome_theme;
        use crate::widget::DrawPrim;
        use aterm_render::Theme;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let theme = forced_chrome_theme(Theme::default(), false);
        let roles = crate::settings::Roles::from_theme(theme);
        let node = |spec: ButtonSpec, rect: LogicalRect| PaintNode {
            cut_edges: CutEdges::NONE,
            peek: false,
            fade: None,
            key: UiKey::new("settings/nav/packages"),
            rect,
            clip: rect,
            content: UiContent::Button(
                Control::new(spec, ActionId::new("settings/route/packages"))
                    .style(StyleRef::Navigation),
            ),
            joins: Joins::default(),
        };
        let dots = |node: &PaintNode| -> Vec<(f32, f32, f32)> {
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, node, roles, theme, 14.0);
            prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Dot {
                        cx, cy, r, color, ..
                    } if color[..3] == roles.danger => Some((*cx, *cy, *r)),
                    _ => None,
                })
                .collect()
        };
        let icon_row = || ButtonSpec::new("Packages").visual_icon(ButtonIcon::Packages);
        let chevron_row = || ButtonSpec::new("Packages").trailing_icon(ButtonIcon::Forward);
        for (shape, spec, rect, labelled) in [
            (
                "icon row",
                icon_row as fn() -> ButtonSpec,
                LogicalRect::new(8.0, 40.0, 180.0, 34.0),
                true,
            ),
            (
                "icon-only row",
                icon_row,
                LogicalRect::new(8.0, 40.0, 48.0, 34.0),
                false,
            ),
            (
                "chevron row",
                chevron_row,
                LogicalRect::new(0.0, 0.0, 320.0, 44.0),
                true,
            ),
        ] {
            assert!(
                dots(&node(spec(), rect)).is_empty(),
                "{shape}: no badge, no dot"
            );
            let badged = node(spec().badge(true), rect);
            let found = dots(&badged);
            assert_eq!(found.len(), 1, "{shape}: one dot: {found:?}");
            let (cx, cy, r) = found[0];
            assert!(rect.contains(cx, cy), "{shape}: {cx},{cy} inside {rect:?}");
            if shape == "chevron row" {
                // The chevron is painted in the row's last 34pt.
                assert!(
                    cx + r < rect.right() - 34.0,
                    "the dot ({cx}±{r}) stays left of the chevron's slot"
                );
            }
            if labelled {
                let audit = text_fit_audit(&badged).expect("a rail label is audited");
                let plain = text_fit_audit(&node(spec(), rect)).expect("audited");
                assert!(
                    audit.authored_available < plain.authored_available,
                    "{shape}: the badge takes its room from the label's slot: {} vs {}",
                    audit.authored_available,
                    plain.authored_available
                );
            }
        }
    }

    /// A COLOUR CHIP ON A BUTTON: a button that names a colour wears the chip a
    /// colour text field wears, 22pt square, at its end ahead of the trailing
    /// arrow, and its label gives the chip its room in the painter and in the
    /// fit audit alike. A button without one paints no chip.
    #[test]
    fn button_color_chip_paints_beside_the_trailing_icon() {
        use crate::native_appearance::forced_chrome_theme;
        use crate::widget::DrawPrim;
        use aterm_render::Theme;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let theme = forced_chrome_theme(Theme::default(), false);
        let roles = crate::settings::Roles::from_theme(theme);
        let orange = [0xE6, 0x7A, 0x00];
        let rect = LogicalRect::new(20.0, 30.0, 140.0, 36.0);
        let node = |chip: bool| {
            let spec = ButtonSpec::new("Choose the selected tab color; now #E67A00")
                .visual_label("#E67A00 and a label long enough to be cut")
                .trailing_icon(ButtonIcon::Forward);
            PaintNode {
                cut_edges: CutEdges::NONE,
                peek: false,
                fade: None,
                key: UiKey::new("settings/control/active_tab_color"),
                rect,
                clip: rect,
                content: UiContent::Button(
                    Control::new(
                        ButtonSpec {
                            chip: chip.then_some(orange),
                            ..spec
                        },
                        ActionId::new("settings/route/tab-color"),
                    )
                    .style(StyleRef::Setting),
                ),
                joins: Joins::default(),
            }
        };
        let paint = |chip: bool| {
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node(chip), roles, theme, 14.0);
            prims
        };
        let chips = |prims: &[DrawPrim]| -> Vec<(f32, f32, f32, f32)> {
            prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Panel {
                        x, y, w, h, fill, ..
                    } if fill[..3] == orange => Some((*x, *y, *w, *h)),
                    _ => None,
                })
                .collect()
        };
        assert!(chips(&paint(false)).is_empty(), "no chip, no colour");
        let prims = paint(true);
        let found = chips(&prims);
        assert_eq!(found.len(), 1, "one chip: {found:?}");
        let (x, y, w, h) = found[0];
        assert_eq!((w, h), (TEXT_FIELD_SWATCH_SIZE, TEXT_FIELD_SWATCH_SIZE));
        assert!(
            x >= rect.x && y >= rect.y && y + h <= rect.bottom(),
            "inside the button: {found:?} in {rect:?}"
        );
        // The arrow is painted in the button's last 34pt.
        assert!(
            x + w <= rect.right() - 34.0,
            "the chip ends before the arrow's slot: {found:?}"
        );
        assert!(
            (y + h / 2.0 - (rect.y + rect.height / 2.0)).abs() < 0.01,
            "centred on the row"
        );
        // The label is cut before the chip, in the painter and the audit.
        let text_right = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { x, s, px, face, .. } => {
                    Some(x + crate::tray_raster::ui_text_width_for(*face, s, *px))
                }
                _ => None,
            })
            .expect("the label is painted");
        assert!(
            text_right <= x - TEXT_FIELD_SWATCH_GAP + 0.5,
            "the label ends at {text_right}, before the chip at {x}"
        );
        let with_chip = text_fit_audit(&node(true)).expect("audited");
        let plain = text_fit_audit(&node(false)).expect("audited");
        assert!(
            (plain.authored_available
                - with_chip.authored_available
                - (TEXT_FIELD_SWATCH_SIZE + TEXT_FIELD_SWATCH_GAP))
                .abs()
                < 0.01,
            "the chip takes its room from the label: {} vs {}",
            plain.authored_available,
            with_chip.authored_available
        );
        assert!(
            (text_button_label_room(
                &ButtonSpec {
                    chip: Some(orange),
                    ..ButtonSpec::new("x").trailing_icon(ButtonIcon::Forward)
                },
                rect.width
            ) - with_chip.authored_available)
                .abs()
                < 0.01,
            "a caller that fits a label first measures the painter's slot"
        );
    }

    /// SEMIBOLD is a rail DESTINATION's mark, and only a destination's.
    ///
    /// `StyleRef::Navigation` dresses three shapes, and the marks are not all
    /// affordable on all of them. The wash, the accent ink and the rim cost
    /// nothing but colour and go everywhere. The rail BAR costs no width either
    /// but asserts a rail edge, so it goes to the rows that are stacked in a
    /// column of their own and not to a toolbar chip. SEMIBOLD costs WIDTH, in a
    /// slot that was measured in the regular face, so it goes only where an
    /// author left slack to spend:
    ///
    /// * the Settings rail row — one of 16 authored labels in a rail the same
    ///   author sized. `Keyboard & Input`, the longest, clears the narrower rail
    ///   by 7pt in semibold. It grows ink.
    /// * the markdown OUTLINE row — a document heading capped at 32 characters,
    ///   in a column sized for the reader. It is rail-SHAPED and keeps the bar,
    ///   but `Getting started with aterm` measures 159.7pt regular in its 166pt
    ///   slot and 173.1pt semibold, so growing its ink would spell "selected" by
    ///   deleting the last word. It does not.
    /// * the markdown mode CHIP — `Length::Fixed` at
    ///   `markdown_toolbar_label_width`, the regular advance of its own word plus
    ///   a fixed pad. No slack exists by construction; it was painting `Previ…`
    ///   in a slot that fits `Preview`. No semibold, and no bar either.
    ///
    /// The two that give up semibold keep every mark that costs them no width.
    #[test]
    fn semibold_goes_only_where_a_slot_was_sized_with_room_to_grow_ink() {
        use crate::native_appearance::forced_chrome_theme;
        use crate::widget::{DrawPrim, TextFace};
        use aterm_render::Theme;

        // Measure on the faces the app really installs — a semibold that is
        // silently the regular face proves nothing about a slot measured for
        // regular.
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();

        let rect = LogicalRect::new(8.0, 40.0, 180.0, 34.0);
        let node = |spec: ButtonSpec, selected: bool| {
            let mut control =
                Control::new(spec, ActionId::new("chrome/nav")).style(StyleRef::Navigation);
            control.state.selected = selected;
            PaintNode {
                cut_edges: CutEdges::NONE,
                peek: false,
                fade: None,
                key: UiKey::new("chrome/nav"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                joins: Joins::default(),
            }
        };
        // The three shapes that actually ship, exactly as their authors build
        // them: the Settings rail row (a route pictogram, and a label only at
        // large Dynamic Type), the markdown outline row (a bare document
        // heading, left aligned), and the markdown mode chip (a centred
        // `visual_label` in a measured slot).
        let rail_row = || ButtonSpec::new("Appearance").visual_icon(ButtonIcon::Home);
        let outline_row = || ButtonSpec::new("A document heading");
        let mode_chip = || ButtonSpec::new("Preview mode").visual_label("Preview");

        let theme = forced_chrome_theme(Theme::default(), false);
        let roles = crate::settings::Roles::from_theme(theme);
        let paint = |spec: ButtonSpec, selected: bool| {
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node(spec, selected), roles, theme, 14.0);
            prims
        };
        let semibold = |prims: &[DrawPrim]| {
            prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { face, .. } if *face == TextFace::UiBold))
        };
        let accent_rim = |prims: &[DrawPrim]| {
            prims.iter().any(|p| {
                matches!(
                    p,
                    DrawPrim::Stroke { w, color, .. }
                        if *w >= rect.width - 2.0 && color[..3] == roles.accent
                )
            })
        };
        // The label is the accent AS TEXT: its hue at the text floor.

        // SEMIBOLD is only the destination's.
        assert!(
            semibold(&paint(rail_row(), true)),
            "the selected rail destination is not semibold — selection reads as nothing"
        );
        assert!(
            !semibold(&paint(rail_row(), false)),
            "a resting rail row is already semibold"
        );
        assert!(
            !semibold(&paint(outline_row(), true)),
            "a selected outline row grew semibold ink in a slot its heading already fills"
        );
        let chip = paint(mode_chip(), true);
        assert!(
            !semibold(&chip),
            "a selected toolbar chip grew semibold ink in a slot measured for regular"
        );
        // It still has to READ as selected, on a mark that costs it no width.
        assert!(
            accent_rim(&chip),
            "a selected toolbar chip lost its accent rim"
        );

        // The defect itself, at the AUTHORED slot: `markdown_toolbar_label_width`
        // is the regular advance of the word plus a fixed pad, floored at the
        // touch minimum. The selected chip has to paint the whole word in it.
        let px = native_type_px(TypeStep::Secondary).get();
        let slot = (crate::tray_raster::ui_text_width("Preview", 13.0) + 20.0)
            .ceil()
            .max(62.0);
        let chip_rect = LogicalRect::new(8.0, 40.0, slot, 34.0);
        let mut control = Control::new(mode_chip(), ActionId::new("markdown/mode/preview"))
            .style(StyleRef::Navigation);
        control.state.selected = true;
        let chip_node = PaintNode {
            cut_edges: CutEdges::NONE,
            peek: false,
            fade: None,
            key: UiKey::new("markdown/preview-mode"),
            rect: chip_rect,
            clip: chip_rect,
            content: UiContent::Button(control),
            joins: Joins::default(),
        };
        let mut prims = Vec::new();
        paint_compiled_node(&mut prims, &chip_node, roles, theme, 14.0);
        let painted = prims.iter().find_map(|p| match p {
            DrawPrim::Text { s, .. } => Some(s.clone()),
            _ => None,
        });
        assert_eq!(
            painted.as_deref(),
            Some("Preview"),
            "the selected mode chip elided its own word inside its authored {slot}pt slot \
             (semibold in a space measured for regular at {px}px)"
        );
    }

    /// ON WINDOWS A SELECTED RAIL ROW IS A WINUI NAVIGATION ITEM
    /// ([`FLUENT_RAIL_SELECTION`]): a 3x16pt accent pill at its leading edge
    /// and its own primary ink, with no accent rim or accent label; a toolbar
    /// chip keeps its accent cell. Elsewhere the row keeps the accent ink and
    /// the 4pt bar, and wears no rim either: on a row the outline is keyboard
    /// focus's alone. NEGATIVE CONTROL: the row unselected wears neither pill
    /// nor bar.
    #[test]
    fn a_selected_rail_row_wears_its_platforms_selection() {
        use crate::native_appearance::forced_chrome_theme;
        use crate::widget::DrawPrim;
        use aterm_render::Theme;

        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let theme = forced_chrome_theme(Theme::default(), false);
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(8.0, 40.0, 180.0, 34.0);
        let paint = |spec: ButtonSpec, selected: bool| {
            let mut control =
                Control::new(spec, ActionId::new("chrome/nav")).style(StyleRef::Navigation);
            control.state.selected = selected;
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                peek: false,
                fade: None,
                key: UiKey::new("chrome/nav"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            prims
        };
        // The leading accent mark, as `(width, height)`.
        let mark = |prims: &[DrawPrim]| {
            prims.iter().find_map(|p| match p {
                DrawPrim::Panel { x, w, h, fill, .. }
                    if *x == rect.x && *w <= 4.0 && fill[..3] == roles.accent =>
                {
                    Some((*w, *h))
                }
                _ => None,
            })
        };
        // A stroke around the row (the pictogram's own strokes are ink).
        let accent_rim = |prims: &[DrawPrim]| {
            prims.iter().any(|p| {
                matches!(
                    p,
                    DrawPrim::Stroke { w, color, .. }
                        if *w >= rect.width - 8.0 && color[..3] == roles.accent
                )
            })
        };
        let ink = |prims: &[DrawPrim]| {
            prims.iter().find_map(|p| match p {
                DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                _ => None,
            })
        };
        let rail_row = || ButtonSpec::new("Appearance").visual_icon(ButtonIcon::Home);
        let selected = paint(rail_row(), true);
        assert_eq!(
            mark(&paint(rail_row(), false)),
            None,
            "a resting row is unmarked"
        );
        if FLUENT_RAIL_SELECTION {
            assert_eq!(mark(&selected), Some((3.0, 16.0)), "WinUI's pill");
            assert!(!accent_rim(&selected), "no accent rim around the row");
            assert_eq!(
                ink(&selected),
                Some(roles.text_primary),
                "the ink is unchanged"
            );
            let chip = paint(
                ButtonSpec::new("Preview mode").visual_label("Preview"),
                true,
            );
            assert!(accent_rim(&chip), "a chip keeps its accent cell");
        } else {
            assert_eq!(
                mark(&selected),
                Some((4.0, rect.height - 8.0)),
                "the rail bar"
            );
            assert!(!accent_rim(&selected), "no rim: the outline is focus's");
            assert_eq!(ink(&selected), Some(roles.accent_ink), "the accent as text");
        }
    }

    /// The paint-truth audit measures the face the painter DRAWS.
    ///
    /// [`text_fit_audit`] is the repo's own overflow sensor: it re-derives the
    /// label's slot and re-runs the elision, and its answer is what
    /// introspection reports. A selected rail row paints in semibold — a
    /// different font with different advances — so an audit that elided with the
    /// REGULAR metrics would report a fit for a string the painter never draws,
    /// and would be blindest at exactly the row that grew ink. Both now go
    /// through [`navigation_label_type`], so this compares the two answers
    /// directly, on labels long enough that the elision is doing work.
    #[test]
    fn the_paint_audit_elides_the_same_label_the_painter_paints() {
        use crate::native_appearance::forced_chrome_theme;
        use crate::widget::DrawPrim;
        use aterm_render::Theme;

        // On the real faces, so regular and semibold have different advances and
        // a divergence between the two paths has somewhere to show up.
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();

        let theme = forced_chrome_theme(Theme::default(), true);
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(8.0, 40.0, 132.0, 34.0);
        let specs = [
            ButtonSpec::new("Appearance and window theme").visual_icon(ButtonIcon::Home),
            ButtonSpec::new("A document heading long enough to elide"),
            ButtonSpec::new("Preview mode").visual_label("Preview"),
            ButtonSpec::new("Software Update").trailing_icon(ButtonIcon::Forward),
            ButtonSpec::new("Appearance and window theme")
                .visual_icon(ButtonIcon::Home)
                .badge(true),
            ButtonSpec::new("Software Update")
                .trailing_icon(ButtonIcon::Forward)
                .badge(true),
        ];
        for spec in specs {
            for selected in [false, true] {
                let mut control = Control::new(spec.clone(), ActionId::new("chrome/nav"))
                    .style(StyleRef::Navigation);
                control.state.selected = selected;
                let node = PaintNode {
                    cut_edges: CutEdges::NONE,
                    peek: false,
                    fade: None,
                    key: UiKey::new("chrome/nav"),
                    rect,
                    clip: rect,
                    content: UiContent::Button(control),
                    joins: Joins::default(),
                };
                let mut prims = Vec::new();
                paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
                let painted = prims.iter().find_map(|p| match p {
                    DrawPrim::Text { s, .. } => Some(s.clone()),
                    _ => None,
                });
                let audited = text_fit_audit(&node).map(|audit| audit.painted);
                assert_eq!(
                    painted, audited,
                    "{:?} (selected={selected}): the paint-truth audit and the painter \
                     disagree about the label that reaches the glass",
                    spec.label
                );
            }
        }
    }

    /// A light terminal theme, as the forced-DARK chrome side starts from.
    const FORCED_DARK_TERMINAL: aterm_render::Theme = aterm_render::Theme {
        fg: 0x0065_7B83,
        bg: 0x00FD_F6E3,
        cursor: 0x0085_9900,
        selection: 0x00EE_E8D5,
    };

    /// The Top Settings hero banner follows the RESOLVED chrome palette.
    ///
    /// It used to fill with the landing page's authored `#EDF6EC` mint —
    /// byte-identical on every chrome side, which put a near-white slab in the
    /// middle of the forced-DARK Settings page. The sky is a function of the
    /// palette, so the two forced sides paint different skies, each on its own
    /// side (the sky's openness and rim on both sides are swept with the band in
    /// [`the_hero_banner_sky_survives_a_mid_tone_terminal_theme`]).
    #[test]
    fn the_hero_banner_sky_follows_the_resolved_chrome_palette() {
        use crate::native_appearance::{forced_chrome_theme, surface_is_dark};
        use aterm_render::Theme;

        let sky_for =
            |theme: Theme| rainbow_banner_sky(&crate::settings::Roles::from_theme(theme)).0;
        let light_sky = sky_for(forced_chrome_theme(Theme::default(), false));
        let dark_sky = sky_for(forced_chrome_theme(FORCED_DARK_TERMINAL, true));
        assert_ne!(
            light_sky, dark_sky,
            "the two forced chrome sides paint the same sky — window_theme is a no-op here again"
        );
        assert!(
            !surface_is_dark(light_sky),
            "the forced-LIGHT banner sky {light_sky:?} classifies dark"
        );
        assert!(
            surface_is_dark(dark_sky),
            "the forced-DARK banner sky {dark_sky:?} classifies light"
        );
    }

    /// ON A RAIL ROW THE OUTLINE IS FOCUS ALONE: in all four (selected,
    /// focused) combinations, on both forced sides, a full-width accent
    /// stroke appears exactly when the row has keyboard focus, 2pt wide; and
    /// a focused selected row keeps its bar and wash, so it still reads as
    /// selected under the ring.
    #[test]
    fn a_rail_rows_outline_is_focus_alone_never_selection() {
        use crate::native_appearance::forced_chrome_theme;
        use crate::widget::DrawPrim;
        use aterm_render::Theme;

        let terminal_light = Theme {
            fg: 0x0065_7B83,
            bg: 0x00FD_F6E3,
            cursor: 0x0085_9900,
            selection: 0x00EE_E8D5,
        };
        let rect = LogicalRect::new(8.0, 40.0, 180.0, 34.0);
        let row = |selected: bool, focus_visible: bool| {
            let mut control = Control::new(
                ButtonSpec::new("Top Settings").visual_icon(ButtonIcon::Home),
                ActionId::new("settings/nav/top"),
            )
            .style(StyleRef::Navigation);
            control.state.selected = selected;
            control.state.focus_visible = focus_visible;
            PaintNode {
                cut_edges: CutEdges::NONE,
                peek: false,
                fade: None,
                key: UiKey::new("settings/nav/top"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                joins: Joins::default(),
            }
        };
        for (label, theme) in [
            ("forced-light", forced_chrome_theme(Theme::default(), false)),
            ("forced-dark", forced_chrome_theme(terminal_light, true)),
        ] {
            let roles = crate::settings::Roles::from_theme(theme);
            for selected in [false, true] {
                for focus_visible in [false, true] {
                    let mut prims = Vec::new();
                    paint_compiled_node(
                        &mut prims,
                        &row(selected, focus_visible),
                        roles,
                        theme,
                        14.0,
                    );
                    let ring = prims.iter().find_map(|p| match p {
                        DrawPrim::Stroke {
                            w, width, color, ..
                        } if *w >= rect.width - 2.0 && color[..3] == roles.accent => Some(*width),
                        _ => None,
                    });
                    assert_eq!(
                        ring,
                        focus_visible.then_some(2.0),
                        "{label}: selected={selected} focus={focus_visible}"
                    );
                    if selected {
                        let bar = prims.iter().any(|p| {
                            matches!(
                                p,
                                DrawPrim::Panel { x, w, fill, .. }
                                    if (*x - rect.x).abs() < 0.01
                                        && *w <= 6.0
                                        && fill[..3] == roles.accent
                            )
                        });
                        let wash = prims.iter().any(
                            |p| matches!(p, DrawPrim::Panel { w, .. } if *w >= rect.width - 0.01),
                        );
                        assert!(bar && wash, "{label}: focus={focus_visible}");
                    }
                }
            }
        }
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

    /// A LABEL GIVEN ITS OWN WIDTH IS DRAWN WHOLE (Harness at 220x60 drew
    /// "Refre…" and "Open aterm.to…" in buttons sized for them): a button is
    /// laid out at its label's measure plus its padding, and its label is
    /// fitted to that width less the padding, and in `f32` `(w + 40) - 40`
    /// can land a few millionths under `w`. The fit compared that with `<=`
    /// and elided a label that fits. Every label below lost its last letters
    /// that way; the others are the control, and a label one pixel too wide is
    /// still elided.
    #[test]
    fn a_label_fitted_to_its_own_measure_through_a_padded_width_is_whole() {
        let face = crate::widget::TextFace::Ui;
        // Every rung a larger text size gives the button face, too.
        for px in [11.0_f32, 12.0, 13.0, 14.0, 15.0, 16.5, 18.0, 21.0] {
            for label in ["Refresh", "Open aterm.toml", "Tab Control", "Show tab"] {
                for pad in [24.0_f32, 28.0, 40.0, 56.0] {
                    let width = visual_text_width(label, px, face) + pad;
                    assert_eq!(
                        elide_text_label(label, width - pad, px, face),
                        label,
                        "{label:?} at {px}px padded by {pad}"
                    );
                }
            }
        }
        let px = native_type_px(TypeStep::Secondary).get();
        for label in [
            "Refresh",
            "Open aterm.toml",
            "AI CLIs",
            "Tab Control",
            "Show tab",
            "Cancel",
            "Reset All…",
            "Open Developer Tools…",
        ] {
            for pad in [21.0_f32, 24.0, 26.0, 28.0, 40.0, 50.0, 56.0] {
                let width = native_button_label_width(label) + pad;
                assert_eq!(
                    fit_native_button_label(label, width - pad),
                    label,
                    "{label:?} padded by {pad}"
                );
            }
            let short = native_button_label_width(label) - 1.0;
            assert!(
                elide_text_label(label, short, px, face).ends_with('…'),
                "{label:?} one pixel short"
            );
        }
    }

    /// An elided label ends on its last word: a cut that lands just past a
    /// space draws "Typeset…", never "Typeset …".
    #[test]
    fn an_elided_label_never_leaves_a_space_before_its_ellipsis() {
        let px = native_type_px(TypeStep::Body).get();
        let face = crate::widget::TextFace::Ui;
        // Room for "Typeset " and the ellipsis, not for the next letter.
        let width =
            visual_text_width("Typeset ", px, face) + visual_text_width("…", px, face) + 0.01;
        assert!(
            visual_text_width("Typeset M", px, face) + visual_text_width("…", px, face) > width,
            "the cut lands on the space"
        );
        assert_eq!(
            elide_text_label("Typeset Mirage", width, px, face),
            "Typeset…"
        );
    }

    /// A prose block over the 128 KiB paint bound whose `**bold**` run straddles
    /// the cut paints its LAST row: the spans the view hands the painter
    /// (`native_app::bounded_markdown_block_paint`) are clipped to the block's own
    /// text, never into the three-byte `…` the cut appends. Clipped to the display
    /// length (the pre-fix call site), the bold span ended at byte 131_070 —
    /// inside the ellipsis at 131_069..131_072 — and the painter's slice panicked
    /// when the reader scrolled to the end of the block.
    #[test]
    fn a_cut_block_whose_span_straddles_the_cut_paints_its_last_row() {
        // 131_066 plain bytes, then the bold run: the cut lands at 131_069
        // (128 KiB minus the ellipsis), three bytes into "bbbb".
        let source = format!("{}**bbbb** tail", "a".repeat(131_066));
        let document = crate::native_markdown::parse(&source);
        let block = &document.blocks[0];
        assert!(
            matches!(
                block,
                crate::native_markdown::MarkdownBlock::Paragraph { .. }
            ),
            "one paragraph: {block:?}"
        );
        let (text, inline) = crate::native_app::bounded_markdown_block_paint(&document, 0, block);
        assert!(
            text.ends_with('…') && text.len() <= 128 * 1024,
            "the block was cut"
        );
        let own = text.len() - '…'.len_utf8();
        assert!(
            inline
                .iter()
                .any(|(span, style)| style.strong && span.start < own),
            "the straddling bold run survives the cut: {inline:?}"
        );
        for (span, _) in &inline {
            assert!(
                span.end <= own && text.is_char_boundary(span.end),
                "a span ends in the block's own text, not the ellipsis: {span:?}"
            );
        }
        let max_columns = 80;
        let rows = wrap_markdown_text(&text, max_columns, 0, 131_072, false);
        let spec = MarkdownBlockSpec {
            text: text.clone(),
            kind: MarkdownBlockKind::Paragraph,
            dense: false,
            selectable: true,
            action: None,
            selected: false,
            source: 0..source.len(),
            visual_row: rows.len() - 1,
            total_visual_rows: rows.len(),
            estimated_height: 48.0,
            inline,
        };
        let base = StyledBase {
            x: 0.0,
            y: 0.0,
            bottom: 1_000.0,
            line_h: 18.0,
            size: native_type_px(TypeStep::Body),
            face: crate::widget::TextFace::Ui,
            weight: crate::widget::TextWeight::Regular,
            color: [0, 0, 0],
        };
        let mut prims = Vec::new();
        paint_styled_markdown_rows(
            &mut prims,
            &spec,
            &base,
            max_columns,
            1,
            crate::settings::Roles::from_theme(aterm_render::Theme::default()),
        );
        let painted: String = prims
            .iter()
            .filter_map(|p| match p {
                crate::widget::DrawPrim::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            painted.ends_with('…'),
            "the last row ends in the cut's ellipsis: {painted:?}"
        );
        assert!(
            prims.iter().any(|p| matches!(
                p,
                crate::widget::DrawPrim::Text { s, face: crate::widget::TextFace::UiBold, .. }
                    if s.starts_with('b')
            )),
            "the bold run's kept bytes paint bold on the last row: {painted:?}"
        );
    }

    /// A prose block's inline spans PAINT: the strong word in the bold UI face,
    /// inline code in the mono face on a chip, a link in the accent with an
    /// underline, strikethrough with a line through — each segment advancing by
    /// its measured width so nothing overlaps.
    #[test]
    fn a_markdown_paragraph_paints_its_inline_spans() {
        let source = "plain **bold** `code` ~~gone~~ [site](https://x.example)";
        let document = crate::native_markdown::parse(source);
        let crate::native_markdown::MarkdownBlock::Paragraph { text, .. } = &document.blocks[0]
        else {
            panic!("one paragraph");
        };
        let block = |inline| {
            UiTree::new(UiNode::new(
                "markdown/block/0",
                UiContent::MarkdownBlock(MarkdownBlockSpec {
                    text: text.clone(),
                    kind: MarkdownBlockKind::Paragraph,
                    dense: false,
                    selectable: true,
                    action: None,
                    selected: false,
                    source: 0..source.len(),
                    visual_row: 0,
                    total_visual_rows: 1,
                    estimated_height: 48.0,
                    inline,
                }),
            ))
            .compile(LogicalRect::new(0.0, 0.0, 900.0, 48.0))
            .unwrap()
        };
        let styled = block(document.block_styles(0, text.len()));
        let prims = styled.tray(aterm_render::Theme::default(), 13.0).prims;
        let texts: Vec<(&str, crate::widget::TextFace, f32)> = prims
            .iter()
            .filter_map(|p| match p {
                crate::widget::DrawPrim::Text { s, face, x, .. } => Some((s.as_str(), *face, *x)),
                _ => None,
            })
            .collect();
        let face_of = |word: &str| {
            texts
                .iter()
                .find(|(s, _, _)| *s == word)
                .map(|(_, face, _)| *face)
                .unwrap_or_else(|| panic!("{word} painted: {texts:?}"))
        };
        assert_eq!(face_of("bold"), crate::widget::TextFace::UiBold);
        assert_eq!(face_of("code"), crate::widget::TextFace::Mono);
        assert_eq!(face_of("plain"), crate::widget::TextFace::Ui);
        assert!(
            texts.windows(2).all(|pair| pair[0].2 < pair[1].2),
            "segments advance left to right: {texts:?}"
        );
        let strokes = prims
            .iter()
            .filter(|p| matches!(p, crate::widget::DrawPrim::Stroke { h, .. } if *h <= 1.0))
            .count();
        assert!(strokes >= 2, "a strike line and a link underline");
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

    /// A disabled switch must not paint pixel-identical to an enabled one
    /// (2026-08 settings audit: the off-macOS Updates toggle looked live).
    /// The track capsule and the thumb dot both dim; geometry stays put.
    #[test]
    fn disabled_switch_dims_its_track_and_thumb() {
        let switch_tree = |enabled: bool| {
            UiTree::new(
                UiNode::new(
                    "app",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
                )
                .layout(Layout::column().padding(Insets::all(8.0)))
                .children(vec![
                    UiNode::new(
                        "switch",
                        UiContent::Switch(
                            crate::native_ui::Control::new(
                                SwitchSpec {
                                    label: "Automatic updates".to_string(),
                                    description: None,
                                },
                                ActionId::new("test/switch"),
                            )
                            .value(SemanticValue::Bool(true))
                            .state(ControlState {
                                enabled,
                                ..ControlState::default()
                            }),
                        ),
                    )
                    .layout(Layout::default().height(Length::Fixed(36.0))),
                ]),
            )
            .compile(LogicalRect::new(0.0, 0.0, 220.0, 52.0))
            .expect("switch fixture compiles")
            .tray(aterm_render::Theme::default(), 13.0)
            .prims
        };
        let feature = |prims: &[crate::widget::DrawPrim]| {
            prims
                .iter()
                .filter_map(|prim| match prim {
                    crate::widget::DrawPrim::Capsule { fill, track, .. } => {
                        Some(format!("capsule fill={fill:?} track={track:?}"))
                    }
                    crate::widget::DrawPrim::Dot { color, cx, cy, .. } => {
                        Some(format!("dot color={color:?} at {cx},{cy}"))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let live = switch_tree(true);
        let inert = switch_tree(false);
        let live_features = feature(&live);
        let inert_features = feature(&inert);
        assert_eq!(live_features.len(), inert_features.len());
        assert_ne!(
            live_features, inert_features,
            "a disabled switch must be visibly dimmed, not a pixel twin of the live control"
        );
        // The thumb stays in the same place — only its color yields.
        let position = |features: &[String]| {
            features
                .iter()
                .find(|feature| feature.starts_with("dot"))
                .and_then(|feature| feature.split(" at ").nth(1))
                .map(str::to_string)
        };
        assert_eq!(position(&live_features), position(&inert_features));
    }

    /// `CompiledUi::fit_failures` is the cheap verdict the zero-overflow sweeps
    /// assert on instead of the full paint audit. Pin it to the audit it replaces:
    /// its records are EXACTLY the audit lines the full-audit check selected
    /// (`overflow=true`, `clip-truncated=true`, and the `clipped=true` paint-node
    /// records the summary's `clipped=` counts), and it is empty exactly when the
    /// full audit's summary reads `overflow=0` and `clipped=0` with no such line.
    fn assert_fit_failures_match_full_audit(compiled: &CompiledUi) -> Vec<String> {
        let audit = compiled.paint_audit_lines();
        let summary = &audit[0];
        let selected = audit[1..]
            .iter()
            .filter(|line| {
                line.contains("overflow=true")
                    || line.contains("clip-truncated=true")
                    || (line.starts_with("paint-node ") && line.contains(" clipped=true "))
                    || line.starts_with("paint-overlap ")
            })
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let failures = compiled.fit_failures();
        assert_eq!(
            failures
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            selected,
            "fit_failures must be the full audit's failure records"
        );
        assert_eq!(
            failures.len(),
            selected.len(),
            "fit_failures reports each failure record once"
        );
        let clean_summary = summary.contains(" overflow=0 ")
            && summary.contains(" clipped=0 ")
            && summary.contains(" overlaps=0 ");
        assert_eq!(
            failures.is_empty(),
            clean_summary && selected.is_empty(),
            "fit_failures passes exactly when the full audit passes: {summary}"
        );
        failures
    }

    /// THE AUDIT REPORTS MARKS PAINTED OVER EACH OTHER (#50): two labels a
    /// layout stacks run into each other, a label over a switch's thumb runs
    /// into an icon, and a label over a button's pictogram drawn of outlines
    /// alone (the Window glyph, which the audit did not read as ink) runs
    /// into that — a `paint-overlap` record each, counted in the summary and
    /// failing the cheap verdict; the same marks side by side, and a
    /// switch's own track, thumb and words, are none.
    #[test]
    fn paint_audit_reports_marks_painted_over_each_other() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let text = |key: &str, words: &str| {
            UiNode::new(key, UiContent::Text(TextSpec::text(words)))
                .layout(Layout::default().height(Length::Fixed(24.0)))
        };
        let switch = |key: &str| {
            UiNode::new(
                key,
                UiContent::Switch(
                    Control::new(
                        SwitchSpec {
                            label: key.to_string(),
                            description: None,
                        },
                        ActionId::new(key),
                    )
                    .value(SemanticValue::Bool(false))
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(Layout::default().height(Length::Fixed(24.0)))
        };
        let pictogram = |key: &str| {
            UiNode::new(
                key,
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Window").visual_icon(ButtonIcon::Window),
                        ActionId::new(key),
                    )
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(32.0))
                    .height(Length::Fixed(24.0)),
            )
        };
        let row = |key: &str, layout: Layout, children: Vec<UiNode>| {
            UiNode::new(
                key,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(layout.height(Length::Fixed(24.0)))
            .children(children)
        };
        let compile = |children: Vec<UiNode>| {
            UiTree::new(
                UiNode::new(
                    "app",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
                )
                .layout(Layout::column().padding(Insets::all(8.0)).gap(8.0))
                .children(children),
            )
            .compile(LogicalRect::new(0.0, 0.0, 240.0, 120.0))
            .expect("compiles")
        };
        let stacked = compile(vec![
            row(
                "labels",
                Layout::default(),
                vec![text("state", "Live trail"), text("badge", "Shape: Block")],
            ),
            row(
                "thumb",
                Layout::default().width(Length::Fixed(100.0)),
                vec![text("over", "Typing glow"), switch("glow")],
            ),
            row(
                "glyph",
                Layout::default(),
                vec![text("under", "Windows"), pictogram("window")],
            ),
        ]);
        let overlaps = stacked.ink_overlaps();
        assert!(
            overlaps.iter().any(|line| line.contains("a=\"state\"")
                && line.contains("b=\"badge\"")
                && line.contains("a-ink=words:\"Live trail\"")),
            "{overlaps:#?}"
        );
        assert!(
            overlaps.iter().any(|line| line.contains("a=\"over\"")
                && line.contains("b=\"glow\"")
                && line.contains("b-ink=icon")),
            "{overlaps:#?}"
        );
        assert!(
            overlaps.iter().any(|line| line.contains("a=\"under\"")
                && line.contains("b=\"window\"")
                && line.contains("b-ink=icon")),
            "{overlaps:#?}"
        );
        assert_eq!(overlaps.len(), 3, "{overlaps:#?}");
        let audit = stacked.paint_audit_lines();
        assert!(audit[0].contains(" overlaps=3 "), "{}", audit[0]);
        assert_eq!(assert_fit_failures_match_full_audit(&stacked).len(), 3);
        // CONTROL: side by side, nothing overlaps.
        let apart = compile(vec![
            row(
                "labels",
                Layout::row().gap(12.0),
                vec![text("state", "Live trail"), text("badge", "Shape: Block")],
            ),
            row(
                "thumb",
                Layout::row().gap(12.0),
                vec![text("over", "Typing glow"), switch("glow")],
            ),
            row(
                "glyph",
                Layout::row().gap(12.0),
                vec![text("under", "Windows"), pictogram("window")],
            ),
        ]);
        assert!(
            apart.ink_overlaps().is_empty(),
            "{:#?}",
            apart.ink_overlaps()
        );
        assert!(assert_fit_failures_match_full_audit(&apart).is_empty());
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
            .clone()
            .compile(LogicalRect::new(0.0, 0.0, 140.0, 100.0))
            .expect("audit fixture compiles");
        let audit = compiled.paint_audit_lines();
        assert!(audit[0].contains("text-nodes=2 overflow=2"));
        let failures = assert_fit_failures_match_full_audit(&compiled);
        assert_eq!(
            failures.len(),
            2,
            "both overflowing nodes fail the cheap verdict"
        );
        let roomy = tree
            .compile(LogicalRect::new(0.0, 0.0, 1200.0, 100.0))
            .expect("roomy audit fixture compiles");
        assert!(
            assert_fit_failures_match_full_audit(&roomy).is_empty(),
            "the same tree with room to fit passes both verdicts"
        );
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
        let failures = assert_fit_failures_match_full_audit(&compiled);
        assert!(
            failures
                .iter()
                .any(|line| line.starts_with("paint-node key=\"clipped-text\"")),
            "the ancestor-clipped node fails the cheap verdict: {failures:?}"
        );
        assert!(
            failures
                .iter()
                .any(|line| line.starts_with("paint-text key=\"clipped-text\"")),
            "the clip-truncated text fails the cheap verdict: {failures:?}"
        );
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

    pub(crate) fn button(key: &str, label: &str) -> UiNode {
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
                preedit_caret: None,
                status: Some("Saved".to_string()),
                semantic_status: Some("Saved".to_string()),
                minibuffer: None,
                minibuffer_caret: None,
                cursor_label: Some("Ln 41, Col 11".to_string()),
                dirty: true,
                saving: false,
                focused: true,
                action: Some(ActionId::new("editor/focus-buffer")),
            }),
        ))
    }

    /// A focused one-line editor viewport over `text`: its bytes at `source`
    /// in the document, its first column at `column_start`, its caret first.
    fn one_line_viewport(
        source: std::ops::Range<usize>,
        column_start: usize,
        text: &str,
    ) -> TextViewportSpec {
        use crate::native_editor::{EditorViewportLine, EditorViewportProjection};
        TextViewportSpec {
            label: "one-line.txt".to_string(),
            document_key: "document:1@1".to_string(),
            selectable: true,
            projection: Some(EditorViewportProjection {
                first_line: 0,
                total_lines: 1,
                lines: vec![EditorViewportLine {
                    number: 0,
                    source,
                    column_start,
                    text: text.to_string(),
                    selections: Vec::new(),
                    carets: vec![(0, true)],
                    syntax: Vec::new(),
                    diagnostics: Vec::new(),
                }],
            }),
            preedit: String::new(),
            preedit_caret: None,
            status: None,
            semantic_status: None,
            minibuffer: None,
            minibuffer_caret: None,
            cursor_label: None,
            dirty: false,
            saving: false,
            focused: true,
            action: Some(ActionId::new("editor/focus-buffer")),
        }
    }

    #[test]
    fn editor_fingerprint_tracks_every_paint_only_viewport_state() {
        use crate::native_editor::{EditorDiagnosticSpan, EditorSyntaxClass, EditorSyntaxSpan};

        let fingerprint = |edit: &dyn Fn(&mut TextViewportSpec)| {
            let mut tree = editor_viewport();
            let UiContent::TextViewport(spec) = &mut tree.root.content else {
                panic!("editor viewport fixture")
            };
            edit(spec);
            tree.compile(LogicalRect::new(0.0, 0.0, 640.0, 360.0))
                .unwrap()
                .fingerprint()
        };
        let base = fingerprint(&|_| {});
        type Edit<'a> = &'a dyn Fn(&mut TextViewportSpec);
        let edits: [(&str, Edit); 5] = [
            ("minibuffer", &|spec| {
                spec.minibuffer = Some("Find: semantic".to_string());
            }),
            ("cursor label", &|spec| {
                spec.cursor_label = Some("Ln 99, Col 3".to_string());
            }),
            ("selection", &|spec| {
                spec.projection.as_mut().unwrap().lines[0].selections[0].bytes = 0..3;
            }),
            ("syntax", &|spec| {
                spec.projection.as_mut().unwrap().lines[0]
                    .syntax
                    .push(EditorSyntaxSpan {
                        bytes: 0..3,
                        class: EditorSyntaxClass::Key,
                    });
            }),
            ("diagnostic", &|spec| {
                spec.projection.as_mut().unwrap().lines[0]
                    .diagnostics
                    .push(EditorDiagnosticSpan {
                        bytes: 4..10,
                        error: true,
                    });
            }),
        ];
        for (what, edit) in edits {
            assert_ne!(base, fingerprint(edit), "{what} must move the fingerprint");
        }
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
    fn editor_ime_caret_tracks_document_composition_and_typed_minibuffer() {
        use crate::widget::DrawPrim;
        let bounds = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
        let compile = |preedit_caret, minibuffer, minibuffer_caret| {
            let mut tree = editor_viewport();
            let UiContent::TextViewport(spec) = &mut tree.root.content else {
                panic!("editor")
            };
            // The cold direct-view Mono stack has only the embedded face;
            // uncovered CJK glyphs have no painted advance until coverage lands.
            // Each tested interval includes a covered Latin glyph so movement
            // remains a non-vacuous assertion without requiring host fonts.
            spec.preedit = "e\u{301}日A本B".to_string();
            spec.preedit_caret = preedit_caret;
            spec.minibuffer = minibuffer;
            spec.minibuffer_caret = minibuffer_caret;
            tree.compile(bounds).unwrap()
        };
        let mut previous = None;
        for byte in [0, 3, 7, 11] {
            let compiled = compile(Some(byte), None, None);
            let caret = compiled.ime_caret_rect().unwrap();
            let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
            assert!(
                prims.iter().any(|prim| matches!(prim,
                    DrawPrim::Stroke { x, y, w, h, .. }
                        if (*x, *y, *w, *h) == (caret.x, caret.y, caret.width, caret.height)
                )),
                "candidate caret must be painted at preedit byte {byte}"
            );
            if let Some(previous) = previous {
                assert!(caret.x > previous);
            }
            previous = Some(caret.x);
        }
        assert_eq!(
            compile(None, None, None).ime_caret_rect(),
            compile(Some(11), None, None).ime_caret_rect()
        );
        let mut tree = editor_viewport();
        let UiContent::TextViewport(spec) = &mut tree.root.content else {
            panic!("editor")
        };
        spec.preedit = "e\u{301}日👩‍💻\n本".to_string();
        spec.preedit_caret = Some("e\u{301}日👩‍💻\n".len());
        let compiled = tree.compile(bounds).unwrap();
        let caret = compiled.ime_caret_rect().unwrap();
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        let preedit_x = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { x, s, .. } if s == "e\u{301}日👩‍💻↵本" => Some(*x),
                _ => None,
            })
            .unwrap();
        let advance = crate::tray_raster::measure_text(
            "e\u{301}日👩‍💻↵",
            native_type_px(crate::type_scale::TypeStep::Secondary).get(),
            crate::widget::TextWeight::Regular,
        );
        assert!(
            (caret.x - preedit_x - advance).abs() < 0.01,
            "marked caret follows actual fallback-font advances"
        );
        let label = "I-search: λ日本Z".to_string();
        let start = "I-search: λ".len();
        let typed = compile(Some(3), Some(label.clone()), Some(start));
        let footer = typed.ime_caret_rect().unwrap();
        let end = compile(Some(3), Some(label.clone()), Some(label.len()))
            .ime_caret_rect()
            .unwrap();
        assert!(end.x > footer.x);
        let prims = typed.tray(aterm_render::Theme::default(), 13.0).prims;
        assert!(prims.iter().any(|prim| matches!(prim,
            DrawPrim::Stroke { x, y, w, h, .. }
                if (*x, *y, *w, *h) == (footer.x, footer.y, footer.width, footer.height)
        )));
        let hud = compile(Some(3), Some("C-x …".to_string()), None);
        assert_eq!(
            hud.ime_caret_rect(),
            compile(Some(3), None, None).ime_caret_rect()
        );
        assert!(
            hud.ime_caret_rect().unwrap().y < footer.y,
            "shortcut HUD is not a text input"
        );
        let mut clipped = typed;
        clipped.paint[0].clip.height = footer.y;
        assert!(
            clipped.ime_caret_rect().is_none(),
            "a hidden minibuffer cannot fall back to the document caret"
        );
        let mut unfocused = compile(Some(3), None, None);
        let UiContent::TextViewport(spec) = &mut unfocused.paint[0].content else {
            panic!("editor")
        };
        spec.focused = false;
        assert!(unfocused.ime_caret_rect().is_none(), "an unfocused editor");
    }

    #[test]
    fn editor_selection_caret_and_diagnostic_share_grapheme_cell_geometry() {
        use crate::native_editor::{EditorDiagnosticSpan, EditorSelectionSpan};
        use crate::type_scale::TypeStep;
        use crate::widget::DrawPrim;

        let text = "e\u{301}中👩‍💻".to_string();
        let after_accent = "e\u{301}".len();
        let after_cjk = after_accent + "中".len();
        let after_woman = after_cjk + "👩".len();
        let text_len = text.len();
        let mut spec = one_line_viewport(0..text_len, 0, &text);
        let line = &mut spec.projection.as_mut().expect("one line").lines[0];
        line.selections = vec![
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
        ];
        line.carets = vec![(text_len, true)];
        line.diagnostics = vec![
            EditorDiagnosticSpan {
                bytes: after_accent..after_cjk,
                error: true,
            },
            EditorDiagnosticSpan {
                bytes: after_cjk..after_woman,
                error: false,
            },
        ];
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
        use crate::widget::DrawPrim;

        let rect = LogicalRect::new(0.0, 0.0, 760.0, 420.0);
        let geometry = text_viewport_geometry(rect);
        let close = |left: f32, right: f32| (left - right).abs() < 0.01;
        for (preedit, cells) in [("e\u{301}", 1usize), ("中", 2), ("👩‍💻", 2)] {
            let mut spec = one_line_viewport(0..0, 0, "");
            spec.preedit = preedit.to_string();
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

    /// A button's colour strip paints its background band and one cell per
    /// colour inside the button's own rect, under the label; a button without
    /// one paints none (negative control).
    #[test]
    fn button_swatch_strip_paints_inside_the_row() {
        use crate::widget::DrawPrim;

        let background = [1, 2, 3];
        let cells: Vec<[u8; 3]> = (0..16_u8).map(|i| [10 + i, 100, 200]).collect();
        let rect = LogicalRect::new(0.0, 0.0, 240.0, 44.0);
        let paint = |spec: ButtonSpec| {
            let compiled = UiTree::new(UiNode::new(
                "theme",
                UiContent::Button(Control::new(spec, ActionId::new("choose"))),
            ))
            .compile(rect)
            .unwrap();
            compiled.validate_parity().unwrap();
            compiled.tray(aterm_render::Theme::default(), 13.0).prims
        };
        let fills = |prims: &[DrawPrim]| {
            prims
                .iter()
                .filter_map(|prim| match prim {
                    DrawPrim::Panel {
                        x, y, w, h, fill, ..
                    } => Some(((*x, *y, *w, *h), *fill)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let prims = paint(ButtonSpec::new("Nord").swatches(background, &cells));
        let panels = fills(&prims);
        for color in std::iter::once(background).chain(cells.iter().copied()) {
            let (bounds, _) = panels
                .iter()
                .find(|(_, fill)| *fill == crate::widget::rgba(color, 255))
                .unwrap_or_else(|| panic!("{color:?} is painted"));
            let (x, y, w, h) = *bounds;
            assert!(
                x >= rect.x
                    && y >= rect.y
                    && x + w <= rect.right() + 0.01
                    && y + h <= rect.bottom(),
                "{color:?} at {bounds:?} stays inside {rect:?}"
            );
        }
        let plain = fills(&paint(ButtonSpec::new("Nord")));
        assert!(
            plain
                .iter()
                .all(|(_, fill)| *fill != crate::widget::rgba(background, 255))
        );
    }

    /// THE WORDS A BUTTON PAINTS ARE IN ITS NAME (WCAG 2.5.3): one phrase,
    /// case and spacing aside, an elision matched as the pieces it kept in
    /// order, a glyph with no word exempt; a button that paints words its
    /// name lacks does not compile. Negative controls: "Open Log" over "Open
    /// the package log", "Colors" over "Appearance".
    #[test]
    fn a_buttons_painted_words_are_in_its_name() {
        for (visual, name) in [
            ("Clear", "Clear search"),
            ("More sty\u{2026}", "More styles"),
            (
                "bidi \u{2014} Bi\u{2026}onal text",
                "Insert bidi \u{2014} Bidirectional text",
            ),
            (
                "Show  in finder",
                "Show in Finder: the folder that holds aterm.toml",
            ),
            ("\u{2039}", "Previous config completions"),
            (
                "View",
                "Review in Modified: view the unsaved Settings drafts",
            ),
            ("Open\u{2026} site", "Open Project Site"),
            ("", "A card's surface"),
        ] {
            assert!(
                visible_label_in_name(visual, name),
                "{visual:?} in {name:?}"
            );
        }
        for (visual, name) in [
            ("Open Log", "Open the package log"),
            ("Colors", "Appearance"),
            ("Updates", "Software Update"),
            ("Card\u{2026}dis", "Discard"),
            // A word inside a longer one is not that word.
            ("View", "Review in Modified: the unsaved Settings drafts"),
            ("Art", "Restart now"),
        ] {
            assert!(
                !visible_label_in_name(visual, name),
                "{visual:?} in {name:?}"
            );
        }
        let compile = |visual: &str, name: &str| {
            UiTree::new(UiNode::new(
                "button",
                UiContent::Button(Control::new(
                    ButtonSpec::new(name).visual_label(visual),
                    ActionId::new("button"),
                )),
            ))
            .compile(LogicalRect::new(0.0, 0.0, 200.0, 36.0))
        };
        assert!(compile("Open Log", "Open Log").is_ok());
        assert_eq!(
            compile("Open Log", "Open the package log").err(),
            Some(CompileError::VisibleLabelNotInName(UiKey::new("button")))
        );
    }

    /// A SEGMENTED CONTROL IS ONE TAB STOP ([`UiNode::segmented`]): the
    /// focused segment, else the selected one; every segment stays in the
    /// tree and is hit; Left/Right step between them and stop at the ends.
    /// Negative control: the same buttons in a plain group are three stops.
    #[test]
    fn a_segmented_control_is_one_tab_stop() {
        let segment = |key: &str, selected: bool, focused: bool| {
            UiNode::new(
                key,
                UiContent::Button(
                    Control::new(ButtonSpec::new(key), ActionId::new(key)).state(ControlState {
                        selected,
                        focused,
                        ..ControlState::default()
                    }),
                ),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill))
        };
        let compile = |segmented: bool, focused: Option<&str>| {
            let mut group = UiNode::new("choice", UiContent::Group(GroupSpec::new("Choice")))
                .layout(
                    Layout::row()
                        .width(Length::Fill)
                        .height(Length::Fixed(36.0)),
                )
                .children(
                    ["a", "b", "c"]
                        .into_iter()
                        .map(|key| segment(key, key == "a", focused == Some(key)))
                        .collect(),
                );
            if segmented {
                group = group.segmented();
            }
            UiTree::new(group)
                .compile(LogicalRect::new(0.0, 0.0, 300.0, 36.0))
                .unwrap()
        };
        let keys = |compiled: &CompiledUi| {
            compiled
                .focus_order
                .iter()
                .map(|key| key.as_str().to_string())
                .collect::<Vec<_>>()
        };
        let rest = compile(true, None);
        assert_eq!(keys(&rest), ["a"], "the selected segment");
        assert_eq!(rest.hits.len(), 3, "every segment is hit");
        assert_eq!(keys(&compile(true, Some("b"))), ["b"], "the focused one");
        assert_eq!(
            rest.segment_step(&UiKey::new("a"), 1),
            Some(Some(UiKey::new("b")))
        );
        assert_eq!(
            rest.segment_step(&UiKey::new("c"), 1),
            Some(None),
            "its end"
        );
        assert_eq!(
            rest.segment_step(&UiKey::new("choice"), 1),
            None,
            "no segment"
        );
        assert_eq!(
            keys(&compile(false, None)),
            ["a", "b", "c"],
            "a plain group"
        );
    }

    /// A BOX LISTED A PIECE AT A TIME READS AS ONE CARD ([`Joins`]): a piece
    /// that runs on into the one over it draws no top edge, one that goes on
    /// under it no bottom edge, its face carried over the gap to the next —
    /// its box drawn past a clip at its own bounds and that gap. CONTROL: a
    /// closed box is drawn at its bounds, with no clip of its own.
    #[test]
    fn a_joined_box_draws_no_edge_where_it_runs_on() {
        use crate::widget::DrawPrim;
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(10.0, 100.0, 200.0, 80.0);
        let piece = |joins: Joins| PaintNode {
            key: UiKey::new("card/part-1"),
            rect,
            clip: rect,
            content: UiContent::Group(
                GroupSpec::unlabeled(SemanticRole::Group).style(StyleRef::Secondary),
            ),
            peek: false,
            fade: None,
            cut_edges: CutEdges::NONE,
            joins,
        };
        let drawn = |joins: Joins| {
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &piece(joins), roles, theme, 14.0);
            prims
        };
        let joined = drawn(Joins {
            above: true,
            below: Some(6.0),
        });
        assert!(
            matches!(
                joined.first(),
                Some(DrawPrim::ClipPush { y, h, .. })
                    if (*y - rect.y).abs() < 0.01 && (*h - (rect.height + 6.0)).abs() < 0.01
            ),
            "{joined:?}"
        );
        let face = joined
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel { y, h, .. } if *h > 40.0 => Some((*y, *y + *h)),
                _ => None,
            })
            .expect("the card's face");
        assert!(face.0 < rect.y - 1.0, "open over it: {face:?}");
        assert!(
            face.1 > rect.bottom() + 6.0 + 1.0,
            "open under it, over the gap: {face:?}"
        );
        assert!(
            matches!(joined.last(), Some(DrawPrim::ClipPop)),
            "{joined:?}"
        );
        // CONTROL: a closed box.
        let closed = drawn(Joins::default());
        assert!(
            !closed
                .iter()
                .any(|prim| matches!(prim, DrawPrim::ClipPush { .. } | DrawPrim::ClipPop))
        );
        let face = closed
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel { y, h, .. } if *h > 40.0 => Some((*y, *y + *h)),
                _ => None,
            })
            .expect("the card's face");
        assert_eq!(face, (rect.y, rect.bottom()));
    }

    /// A PLACEHOLDER FITS ITS BOX: whole where it fits, its plain words
    /// without " (default)" where only they fit, else elided; a swatch takes
    /// its room from the box.
    #[test]
    fn a_field_placeholder_fits_its_box() {
        let placeholder = "Automatic (default)";
        assert_eq!(
            fit_native_field_placeholder(placeholder, 200.0, false),
            placeholder
        );
        assert_eq!(
            fit_native_field_placeholder(placeholder, 118.0, false),
            "Automatic"
        );
        let narrow = fit_native_field_placeholder(placeholder, 40.0, false);
        assert!(narrow.ends_with('\u{2026}'), "{narrow:?}");
        let px = native_type_px(TypeStep::Secondary).get();
        let box_of = |field: f32| field - 2.0 * TEXT_FIELD_HORIZONTAL_PADDING;
        assert!(visual_text_width(&narrow, px, crate::widget::TextFace::Ui) <= box_of(40.0) + 0.01);
        // The width where the whole words just fit, less a swatch, drops
        // the "(default)".
        let whole = visual_text_width(placeholder, px, crate::widget::TextFace::Ui)
            + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING;
        assert_eq!(
            fit_native_field_placeholder(placeholder, whole, false),
            placeholder
        );
        assert_ne!(
            fit_native_field_placeholder(placeholder, whole, true),
            placeholder
        );
        // Its shorter forms keep whole words: the aside in parentheses goes
        // first, then the words after the first that fit, an ellipsis saying
        // more follows — never a word cut where whole ones fit (a narrow
        // field painted "ai-fleet.toml beside ater…").
        let field_for = |text: &str| {
            visual_text_width(text, px, crate::widget::TextFace::Ui)
                + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING
        };
        assert_eq!(
            fit_native_field_placeholder("none (HH:MM)", field_for("none") + 1.0, false),
            "none"
        );
        let fleet = "ai-fleet.toml beside aterm.toml";
        assert_eq!(
            fit_native_field_placeholder(fleet, field_for("ai-fleet.toml beside\u{2026}"), false),
            "ai-fleet.toml beside\u{2026}"
        );
        assert_eq!(
            fit_native_field_placeholder(fleet, field_for("ai-fleet.toml beside a"), false),
            "ai-fleet.toml beside\u{2026}"
        );
    }

    /// A WINDOWS PATH GIVES WAY AT ITS FOLDERS TOO ([`path_forms`]): its
    /// components end at `\` as a Unix path's end at `/`, so a narrow field
    /// keeps its file's name (`C:\…\aterm\ai-fleet.toml`, then
    /// `…\ai-fleet.toml`). The forms split at `/` alone, and a Windows path had
    /// none: its placeholder was elided from its start, the name lost, and a
    /// value cut grapheme by grapheme. CONTROL: a Unix file's name may hold a
    /// `\`, which ends no component there.
    #[test]
    fn a_windows_path_gives_way_at_its_folders() {
        let path = r"C:\Users\someone\AppData\Roaming\aterm\ai-fleet.toml";
        let forms: Vec<String> = path_forms(path).into_iter().map(|form| form.0).collect();
        assert!(
            forms.contains(&r"C:\…\aterm\ai-fleet.toml".to_string()),
            "{forms:?}"
        );
        assert_eq!(
            forms.last().map(String::as_str),
            Some(r"…\aterm\ai-fleet.toml")
        );
        assert_eq!(&path[path_name_start(path)..], "ai-fleet.toml");
        let px = native_type_px(TypeStep::Secondary).get();
        let field_for = |text: &str| {
            visual_text_width(text, px, crate::widget::TextFace::Ui)
                + 2.0 * TEXT_FIELD_HORIZONTAL_PADDING
        };
        assert_eq!(
            fit_native_field_placeholder(path, field_for(r"…\ai-fleet.toml") + 1.0, false),
            r"…\ai-fleet.toml"
        );
        // CONTROL: a `\` in a Unix file's name.
        let unix = r"/Users//someone/odd\name.toml";
        assert_eq!(&unix[path_name_start(unix)..], r"odd\name.toml");
        assert_eq!(
            path_forms(unix).last().map(|form| form.0.as_str()),
            Some(r"…/someone/odd\name.toml")
        );
    }

    /// The painter never cuts a placeholder mid-glyph: one nobody fitted is
    /// painted elided inside the box, and the audit still reports it
    /// (overflow=true, painted the elided words) — the negative control for
    /// a page that forgot to fit its own.
    #[test]
    fn an_unfitted_placeholder_paints_elided_and_is_reported() {
        use crate::widget::DrawPrim;
        let control = Control::new(
            TextFieldSpec {
                label: "Selected tab color".to_string(),
                placeholder: Some("Automatic (default)".to_string()),
                secret: false,
                visual_value: None,
                input: None,
                swatch: None,
                trailing_hint: None,
                description: None,
                end_button: false,
            },
            ActionId::new("set/tab"),
        )
        .value(SemanticValue::Text(String::new()));
        let compiled = UiTree::new(UiNode::new("tab", UiContent::TextField(control)))
            .compile(LogicalRect::new(0.0, 0.0, 70.0, 36.0))
            .unwrap();
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        let painted = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { s, .. } if !s.is_empty() => Some(s.clone()),
                _ => None,
            })
            .expect("the placeholder paints");
        assert!(painted.ends_with('\u{2026}'), "{painted:?}");
        let px = native_type_px(TypeStep::Secondary).get();
        assert!(
            visual_text_width(&painted, px, crate::widget::TextFace::Ui)
                <= 70.0 - 2.0 * TEXT_FIELD_HORIZONTAL_PADDING + 0.01
        );
        let audit = compiled
            .paint_audit_lines()
            .into_iter()
            .find(|line| line.starts_with("paint-text") && line.contains("kind=text-field"))
            .expect("the field is audited");
        assert!(audit.contains("overflow=true"), "{audit}");
        assert!(audit.contains(&format!("painted={painted:?}")), "{audit}");
    }

    /// AN ICON ROW'S ARROW IS ITS AUDIT'S TOO: a navigation row with an icon
    /// and an arrow paints its label in the room the arrow leaves, and the
    /// fit audit measures that room. It gave the label the arrow's 34pt, so a
    /// page result the painter cut was audited as fitting. CONTROL: the same
    /// row without its arrow holds the label.
    #[test]
    fn an_icon_row_s_label_is_audited_in_the_room_its_arrow_leaves() {
        let audit = |arrow: bool| {
            let mut spec = ButtonSpec::new("Cursor Motion")
                .visual_icon(ButtonIcon::Search)
                .visual_label("Cursor Motion");
            if arrow {
                spec = spec.trailing_icon(ButtonIcon::Forward);
            }
            let control = Control::new(spec, ActionId::new("go")).style(StyleRef::Navigation);
            let width = NAVIGATION_LABEL_LEAD
                + NAVIGATION_LABEL_END
                + native_button_label_width("Cursor Motion")
                + 8.0;
            UiTree::new(UiNode::new("go", UiContent::Button(control)))
                .compile(LogicalRect::new(0.0, 0.0, width, 44.0))
                .unwrap()
                .paint_audit_lines()
                .into_iter()
                .find(|line| line.starts_with("paint-text key=\"go\""))
                .expect("the row is audited")
        };
        let with_arrow = audit(true);
        assert!(with_arrow.contains("overflow=true"), "{with_arrow}");
        let without = audit(false);
        assert!(without.contains("overflow=false"), "{without}");
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
                trailing_hint: None,
                description: None,
                end_button: false,
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
        let caret = compiled.ime_caret_rect().expect("focused field caret");
        assert!(
            prims.iter().any(|prim| matches!(
                prim,
                DrawPrim::Stroke { x, y, w, h, .. }
                    if (*x, *y, *w, *h) == (caret.x, caret.y, caret.width, caret.height)
            )),
            "IME anchor must be the painted composition caret"
        );
        let mut clipped = compiled.clone();
        clipped.paint[0].clip = LogicalRect::new(0.0, 0.0, caret.x, 40.0);
        assert!(
            clipped.ime_caret_rect().is_none(),
            "clipped caret cannot anchor IME"
        );
        assert!(
            prims
                .iter()
                .any(|prim| matches!(prim, DrawPrim::Text { s, .. } if s == "aにz"))
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
                trailing_hint: None,
                description: None,
                end_button: false,
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
                trailing_hint: None,
                description: None,
                end_button: false,
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
                description: None,
                track: None,
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
        // The readout draws in the UI face it is measured in, never the
        // terminal face a display face turns playful.
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        let readout = prims.iter().find_map(|prim| match prim {
            crate::widget::DrawPrim::Text { s, face, .. } if s == "14" => Some(*face),
            _ => None,
        });
        assert_eq!(readout, Some(crate::widget::TextFace::Ui));
    }

    #[test]
    fn fractional_slider_steps_snap_to_the_decimal_the_slider_shows() {
        assert_eq!(snap_slider_value(0.3, 0.0, 1.0, 0.05), 0.3);
        assert_eq!(snap_slider_value(0.7, 0.0, 1.0, 0.05), 0.7);
        assert_eq!(snap_slider_value(0.86, 0.0, 1.0, 0.05), 0.85);
        assert_eq!(snap_slider_value(1.3, 0.1, 3.0, 0.05), 1.3);
        assert_eq!(snap_slider_value(7.0, 0.0, 64.0, 1.0), 7.0);
        assert_eq!(snap_slider_value(2.0, 0.0, 1.0, 0.05), 1.0);
    }

    #[test]
    fn renderer_native_button_icons_emit_no_font_dependent_text() {
        use crate::widget::DrawPrim;

        for icon in [
            ButtonIcon::Back,
            ButtonIcon::Forward,
            ButtonIcon::Copy,
            ButtonIcon::ChevronDown,
            ButtonIcon::Home,
            ButtonIcon::Modified,
            ButtonIcon::Appearance,
            ButtonIcon::Text,
            ButtonIcon::Cursor,
            ButtonIcon::Window,
            ButtonIcon::Keyboard,
            ButtonIcon::Terminal,
            ButtonIcon::Security,
            ButtonIcon::Diagnostics,
            ButtonIcon::Update,
            ButtonIcon::Packages,
            ButtonIcon::Info,
            ButtonIcon::Check,
            ButtonIcon::Image,
            ButtonIcon::Swatch,
            ButtonIcon::Document,
            ButtonIcon::Agents,
            ButtonIcon::Trail,
            ButtonIcon::Sound,
            ButtonIcon::Themes,
            ButtonIcon::Spark,
            ButtonIcon::Trash,
            ButtonIcon::Reset,
            ButtonIcon::Clear,
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
    fn preserved_markdown_source_soft_wraps_at_words_before_hard_chunks() {
        assert_eq!(
            wrap_markdown_text("alpha beta gamma", 10, 0, 4, true),
            ["alpha beta", "gamma"]
        );
        assert_eq!(
            wrap_markdown_text("supercalifragilistic", 5, 0, 8, true),
            ["super", "calif", "ragil", "istic"]
        );
        assert_eq!(
            wrap_markdown_text("  let value = exact;", 14, 0, 4, true),
            ["  let value =", "exact;"]
        );
    }

    /// The reader's wraps of Latin text as `tests/fixtures/latin-markdown-wraps.txt`
    /// records them: [`wrap_markdown_text`] flowed and preserved, at every
    /// column count to 24 and a few wider, whole and cut to a few rows (the
    /// tail ellipsis), and [`wrapped_piece_ranges`] of the flowed rows, as
    /// they broke before CJK took the shared line breaks (taken from that
    /// code, 2026-10-21).
    fn latin_markdown_wraps() -> String {
        let corpus = [
            "alpha beta gamma delta epsilon zeta",
            "supercalifragilisticexpialidocious and more",
            "  let value = exact;  // two  spaces\tand a tab",
            "one\ntwo\nthree\n\nfive six",
            "aarch64-apple-darwin ~/.config/aterm/aterm.toml --columns",
            "café naïve “quoted” — dash … end, (paren) [bracket] 50%",
            "a\u{0301}b c\u{0327}d e",
            "abcde fghij",
            "",
            " ",
        ];
        let mut out = String::new();
        for text in corpus {
            for columns in (1..=24).chain([32, 40, 80]) {
                for preserve in [false, true] {
                    for lines in [2usize, 131_072] {
                        let rows = wrap_markdown_text(text, columns, 0, lines, preserve);
                        out.push_str(&format!(
                            "W {text:?} {columns} {preserve} {lines} => {rows:?}\n"
                        ));
                        if !preserve {
                            out.push_str(&format!(
                                "R {text:?} {columns} {lines} => {:?}\n",
                                wrapped_piece_ranges(text, &rows)
                            ));
                        }
                    }
                }
            }
        }
        out
    }

    /// LATIN TEXT WRAPS IN THE READER WHERE IT ALWAYS DID: counting a wide
    /// char as two columns and breaking between CJK characters (the shared
    /// line breaks) moved no row, cut and piece of a Latin paragraph or
    /// code line (the golden record, taken from the code that counted chars
    /// and broke only at white space).
    #[test]
    fn latin_text_wraps_in_the_reader_where_it_always_did() {
        let golden = include_str!("../tests/fixtures/latin-markdown-wraps.txt");
        let now = latin_markdown_wraps();
        let moved: Vec<(&str, &str)> = golden
            .lines()
            .zip(now.lines())
            .filter(|(was, is)| was != is)
            .take(8)
            .collect();
        assert!(moved.is_empty(), "moved wraps (was, is): {moved:#?}");
        assert_eq!(golden.lines().count(), now.lines().count());
    }

    /// A CJK PARAGRAPH OR CODE LINE IN THE READER WRAPS INSIDE ITS COLUMN,
    /// between its characters: at every column a Manual window gives a
    /// block, 1× and 2× text, each painted row measures inside the block's
    /// text column, no row starts with a closer or a small kana, and the
    /// rows rejoined are the text. NEGATIVE CONTROL: counted a column a
    /// char, a Japanese row was laid out at nearly twice the column's width
    /// and painted past it (and broke grapheme by grapheme, a row starting
    /// with "。").
    #[test]
    fn a_cjk_block_wraps_inside_the_readers_column() {
        const NO_ROW_START: [char; 6] = ['。', '、', '」', 'ー', 'ァ', 'ょ'];
        let text = "設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\
                    コーヒーブレイク中にアップデートをダウンロードしています、ちょっと待ってください。";
        let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
        for scale in [1.0_f32, 2.0] {
            std::thread::Builder::new()
                .name(format!("CJK reader at {scale}\u{d7}"))
                .stack_size(8 << 20)
                .spawn(move || {
                    crate::native_appearance::install_preferences(
                        crate::native_appearance::AppearancePreferences {
                            text_scale: scale,
                            ..crate::native_appearance::current_preferences()
                        },
                    );
                    for width in (160..=480).step_by(16).map(|width| width as f32) {
                        for (kind, column) in [
                            (MarkdownBlockKind::Paragraph, width),
                            (MarkdownBlockKind::Code { language: None }, width - 24.0),
                        ] {
                            let context = format!("{scale}\u{d7} {width}pt {kind:?}");
                            let spec = MarkdownBlockSpec {
                                text: text.to_string(),
                                kind: kind.clone(),
                                dense: false,
                                selectable: true,
                                action: None,
                                selected: false,
                                source: 0..text.len(),
                                estimated_height: 4_000.0,
                                visual_row: 0,
                                total_visual_rows: 64,
                                inline: Vec::new(),
                            };
                            let mut prims = Vec::new();
                            paint_markdown_block(
                                &mut prims,
                                LogicalRect::new(0.0, 0.0, width, 4_000.0),
                                &spec,
                                roles,
                            );
                            let rows: Vec<String> = prims
                                .iter()
                                .filter_map(|prim| match prim {
                                    crate::widget::DrawPrim::Text { s, px, face, .. } => {
                                        let painted = visual_text_width(s, *px, *face);
                                        assert!(
                                            painted <= column + 0.5,
                                            "{context}: {s:?} paints {painted:.1}pt in a \
                                             {column}pt column"
                                        );
                                        Some(s.clone())
                                    }
                                    _ => None,
                                })
                                .collect();
                            assert!(rows.len() > 1, "{context}: {rows:?}");
                            for row in rows.iter().skip(1) {
                                let first = row.chars().next().unwrap_or(' ');
                                assert!(
                                    !NO_ROW_START.contains(&first),
                                    "{context}: a row starts with {first:?}: {rows:?}"
                                );
                            }
                            assert_eq!(rows.concat(), text, "{context}: rejoined");
                        }
                    }
                })
                .expect("a thread for the scale")
                .join()
                .unwrap_or_else(|_| panic!("at {scale}\u{d7}"));
        }
    }

    /// `skip` is a pure window over the SAME wrap: the returned rows, the row
    /// budget, and the tail-ellipsis rule must not depend on where the window
    /// starts. Checked against the un-skipped wrap for every start offset over
    /// both wrap modes, including the saturated (ellipsized) case.
    #[test]
    fn wrap_skip_returns_exactly_the_unskipped_suffix() {
        const CASES: [(&str, usize, bool); 6] = [
            ("alpha beta gamma delta epsilon zeta", 10, false),
            ("alpha beta gamma delta epsilon zeta", 10, true),
            ("supercalifragilisticexpialidocious", 5, true),
            ("one\ntwo\nthree\nfour\nfive\nsix\nseven", 6, true),
            ("one\ntwo\nthree\nfour\nfive\nsix\nseven", 6, false),
            ("", 8, true),
        ];
        for (text, columns, preserve) in CASES {
            for total in 1..=8usize {
                let full = wrap_markdown_text(text, columns, 0, total, preserve);
                for skip in 0..=total + 1 {
                    let windowed = wrap_markdown_text(text, columns, skip, total, preserve);
                    let expected = full.get(skip.min(full.len())..).unwrap_or_default();
                    assert_eq!(
                        windowed, expected,
                        "text={text:?} columns={columns} preserve={preserve} total={total} skip={skip}"
                    );
                }
            }
        }
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

    /// THE TOOLBAR PLAN: buttons flow in order at their own widths into the
    /// fewest rows; within them the last buttons take their short words
    /// first; past three rows the buttons M-x also lists give way (Redo, then
    /// Go to Line, then Find), never Save, Commands or the problem pair; and
    /// no plan cuts a label.
    #[test]
    fn the_editor_toolbar_plan_wraps_shortens_and_yields_in_order() {
        let toolbar = EditorToolbar {
            problem_navigation: true,
            reload: false,
            edit_header: false,
        };
        let items = editor_toolbar_items(toolbar);
        let gap = 4.0;
        let width = |label: &str| editor_toolbar_button_width(label);
        let long_total: f32 =
            items.iter().map(|item| width(item.long)).sum::<f32>() + gap * (items.len() - 1) as f32;
        let words = |plan: &EditorToolbarPlan| {
            plan.rows
                .iter()
                .map(|row| row.iter().map(|(_, label, _)| *label).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        // Room for all in long words: one row.
        let plan = plan_editor_toolbar(&items, long_total, gap);
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(words(&plan)[0].last(), Some(&"Next Issue"));
        // A little short: the last button takes its short word, still one row.
        let less = long_total - (width("Next Issue") - width("Next")) + 0.5;
        let plan = plan_editor_toolbar(&items, less, gap);
        assert_eq!(plan.rows.len(), 1, "{:?}", words(&plan));
        assert_eq!(words(&plan)[0].last(), Some(&"Next"));
        assert_eq!(words(&plan)[0][6], "Prev Issue", "the last first");
        // Half the room: two rows, in long words, in reading order.
        let plan = plan_editor_toolbar(&items, long_total * 0.55, gap);
        assert_eq!(plan.rows.len(), 2);
        let flat: Vec<&str> = words(&plan).concat();
        assert_eq!(
            flat,
            items.iter().map(|item| item.long).collect::<Vec<_>>(),
            "order kept, long words kept"
        );
        // Barely more than one button wide: past three rows, the M-x-listed
        // buttons give way in order, and what stays is whole.
        let narrow = width("Next Issue") + 1.0;
        let plan = plan_editor_toolbar(&items, narrow, gap);
        let flat: Vec<&str> = words(&plan).concat();
        for kept in ["Save", "Undo", "Commands"] {
            assert!(flat.contains(&kept), "{kept}: {flat:?}");
        }
        assert!(flat.contains(&"Prev") || flat.contains(&"Prev Issue"));
        assert!(!flat.contains(&"Redo"), "Redo gives way first: {flat:?}");
        for row in &plan.rows {
            let used: f32 = row.iter().map(|(_, _, w)| w).sum::<f32>()
                + gap * (row.len().saturating_sub(1)) as f32;
            assert!(used <= narrow + 0.01 || row.len() == 1, "{row:?}");
            for (_, label, w) in row {
                assert_eq!(*w, width(label), "never squeezed");
            }
        }
        // Between: three rows of short words keep every button.
        let three = plan_editor_toolbar(&items, long_total * 0.36, gap);
        assert!(three.rows.len() <= EDITOR_TOOLBAR_MAX_ROWS);
        assert_eq!(
            three.rows.concat().len(),
            items.len(),
            "{:?}",
            words(&three)
        );
    }

    /// The floor is a promise about DOCUMENT rows, so it has to hold across the
    /// window sizes a person actually uses — not only at the default one. Below
    /// ~335 px of viewport the palette's own "always show one candidate" floor
    /// takes over, which is deliberate: an assist with zero visible candidates
    /// helps nobody.
    #[test]
    fn a_full_completion_page_never_costs_the_editor_its_document_rows() {
        for width in [480, 744, 900, 1_440] {
            for height in 335..=1_200 {
                for candidates in [1_usize, 3, 8] {
                    let viewport = LogicalRect::new(0.0, 0.0, width as f32, height as f32);
                    let editor = editor_text_viewport_rect_at_scale(
                        viewport,
                        candidates,
                        EditorToolbar::default(),
                        1.0,
                    );
                    let geometry = text_viewport_geometry_at_scale(editor, 1.0);
                    let rows = (geometry.body_h / geometry.line_h).floor() as usize;
                    assert!(
                        rows >= MINIMUM_EDITOR_DOCUMENT_ROWS,
                        "{width}x{height} with {candidates} candidates paints {rows} rows"
                    );
                }
            }
        }

        // Dynamic Type scales the floor with the text it protects: a 2× reader
        // keeps whole rows of their own 40 px lines rather than a taller list.
        let large_type = LogicalRect::new(0.0, 0.0, 744.0, 760.0);
        let editor =
            editor_text_viewport_rect_at_scale(large_type, 8_usize, EditorToolbar::default(), 2.0);
        let geometry = text_viewport_geometry_at_scale(editor, 2.0);
        assert!(
            (geometry.body_h / geometry.line_h).floor() as usize >= MINIMUM_EDITOR_DOCUMENT_ROWS,
            "2× landing paints {} px of body over {} px lines",
            geometry.body_h,
            geometry.line_h
        );
    }

    #[test]
    fn text_viewport_pointer_mapping_shares_line_gutter_tab_and_utf8_geometry() {
        use crate::widget::DrawPrim as Prim;

        let spec = one_line_viewport(100..105, 0, "α\tβ");
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

        let unicode_text = "e\u{301}中👩‍💻".to_string();
        let unicode_start = 300usize;
        let after_accent = unicode_start + "e\u{301}".len();
        let after_cjk = after_accent + "中".len();
        let unicode_end = unicode_start + unicode_text.len();
        let unicode = one_line_viewport(unicode_start..unicode_end, 0, &unicode_text);
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

        let mut shifted = one_line_viewport(200..202, 3, "\tX");
        shifted.projection.as_mut().expect("one line").lines[0].carets = vec![(1, true)];
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
            .clone()
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

    /// A tree no observer could agree on fails to compile, before anything is
    /// dispatched: two nodes under one key, a custom node with no audit
    /// adapter, and an action on a paint-only node.
    #[test]
    fn ambiguous_trees_fail_closed_before_dispatch() {
        let custom = UiNode::new(
            "custom",
            UiContent::Custom(AuditedCustomNode {
                audit_id: "",
                role: SemanticRole::Button,
                label: "Novel".to_string(),
                value: None,
                action: Some(ActionId::new("novel")),
                focusable: true,
                focused: false,
                focus_visible: false,
            }),
        );
        for (tree, refused) in [
            (
                UiTree::new(
                    UiNode::new("root", UiContent::Group(GroupSpec::new("root")))
                        .layout(Layout::column())
                        .children(vec![button("same", "One"), button("same", "Two")]),
                ),
                CompileError::DuplicateKey(UiKey::new("same")),
            ),
            (
                UiTree::new(custom),
                CompileError::CustomWithoutAudit(UiKey::new("custom")),
            ),
            (
                UiTree::new(button("hidden-action", "Hidden action").paint_only()),
                CompileError::PaintOnlyAction(UiKey::new("hidden-action")),
            ),
        ] {
            assert_eq!(
                tree.compile(LogicalRect::new(0.0, 0.0, 200.0, 100.0)),
                Err(refused.clone()),
                "{refused:?}"
            );
        }
    }

    /// A paint-only copy paints without a second semantic node, and still
    /// moves the retained raster's fingerprint when only its pixels change.
    #[test]
    fn paint_only_copy_renders_without_duplicate_semantics() {
        let complete =
            "On. Typing notes are silent while the cursor trail is off; other sounds still play.";
        let compile = |visual: &str| {
            UiTree::new(
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
        let compiled = compile("Typing notes off");
        assert_eq!(compiled.paint.len(), 2);
        assert_eq!(compiled.semantics.len(), 1);
        assert_eq!(
            compiled.semantic(&UiKey::new("status")).unwrap().label,
            complete
        );
        assert!(compiled.semantic(&UiKey::new("status/visual")).is_none());
        compiled.validate_parity().unwrap();

        let other = compile("Visual B");
        assert_eq!(compiled.semantics, other.semantics);
        assert_ne!(compiled.fingerprint(), other.fingerprint());
    }

    /// A paint-only tree paints every node of a decorative face and puts
    /// none of them in the tree; `paint_only` alone hides only the node it
    /// marks (the negative control: its child still projects).
    #[test]
    fn paint_only_tree_hides_the_whole_face() {
        let compile = |face: UiNode| {
            UiTree::new(
                UiNode::new("row", UiContent::Group(GroupSpec::new("Row"))).children(vec![face]),
            )
            .compile(LogicalRect::new(0.0, 0.0, 240.0, 40.0))
            .unwrap()
        };
        let face = || {
            UiNode::new(
                "row/face",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .children(vec![UiNode::new(
                "row/face/name",
                UiContent::Text(TextSpec {
                    text: "Row".to_string(),
                    role: SemanticRole::Text,
                    style: StyleRef::Primary,
                }),
            )])
        };
        let whole = compile(face().paint_only_tree());
        assert_eq!(whole.paint.len(), 3);
        let keys: Vec<&str> = whole
            .semantics
            .iter()
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(keys, vec!["row"]);
        let shallow = compile(face().paint_only());
        assert!(shallow.semantic(&UiKey::new("row/face/name")).is_some());
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

    /// The wheel's polar mapping (clockwise turns from 12 o'clock, radius =
    /// saturation) is one map both ways: a pick at the marker of any hue and
    /// saturation picks them again, the centre is saturation 0, a point
    /// outside the disk is a miss, and the pick does not depend on the
    /// brightness the disk is painted at.
    #[test]
    fn color_wheel_pick_and_marker_round_trip() {
        let rect = LogicalRect::new(10.0, 20.0, 240.0, 240.0);
        let geometry = color_wheel_geometry(rect);
        let (_, centre) = color_wheel_pick_at(rect, geometry.cx, geometry.cy).unwrap();
        assert_eq!(centre, 0.0, "disk centre is saturation 0");
        assert_eq!(
            color_wheel_pick_at(rect, geometry.cx + geometry.r + 2.0, geometry.cy),
            None,
            "outside the disk is a miss, not a pick"
        );
        // 12 o'clock at full radius is hue 0; 3 o'clock a quarter turn.
        let (top, full) =
            color_wheel_pick_at(rect, geometry.cx, geometry.cy - geometry.r + 0.01).unwrap();
        assert!(top < 1e-3 || top > 1.0 - 1e-3, "top is hue 0: {top}");
        assert!(full > 0.99, "full radius is saturation 1: {full}");
        let (right, _) =
            color_wheel_pick_at(rect, geometry.cx + geometry.r - 0.01, geometry.cy).unwrap();
        assert!(
            (right - 0.25).abs() < 1e-3,
            "3 o'clock is hue 0.25: {right}"
        );
        for (hue, saturation) in [(0.0889, 1.0), (0.5, 0.4), (0.91, 0.75), (0.33, 0.05)] {
            let (mx, my) = color_wheel_marker_at(rect, hue, saturation);
            let (h, s) = color_wheel_pick_at(rect, mx, my).expect("marker is on the disk");
            assert!(
                (h - hue).abs() < 1e-3 && (s - saturation).abs() < 1e-3,
                "marker round trip drifted for {hue}/{saturation}: {h}/{s}"
            );
        }
    }

    /// A FIELD NOT BEING EDITED SAYS WHAT IT CUTS: a path wider than its
    /// field paints "…" and its end, the file's name included, and a click at
    /// the field's end lands on the value's end; any other value paints its
    /// start and "…". Both are deliberate fitting to the fit audit. Negative
    /// control: a value that fits, or a field being edited, paints no mark.
    #[test]
    fn an_unfocused_field_marks_what_it_cuts() {
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        // As wide as a settings row's field.
        let rect = LogicalRect::new(0.0, 0.0, 300.0, 36.0);
        let field = |value: &str, focused: bool| {
            let mut control = Control::new(
                TextFieldSpec {
                    label: "Wallpaper image".to_string(),
                    description: None,
                    trailing_hint: None,
                    placeholder: None,
                    secret: false,
                    visual_value: None,
                    input: focused.then(|| {
                        crate::native_text_input::TextInputState::new(value.to_string())
                            .projection()
                    }),
                    swatch: None,
                    end_button: false,
                },
                ActionId::new("settings/set/wallpaper"),
            )
            .value(SemanticValue::Text(value.to_string()));
            control.state.enabled = true;
            control.state.focused = focused;
            PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("settings/control/wallpaper"),
                rect,
                clip: rect,
                content: UiContent::TextField(control),
                peek: false,
                fade: None,
                joins: Joins::default(),
            }
        };
        let painted = |node: &PaintNode| {
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, node, roles, theme, 14.0);
            prims
                .into_iter()
                .find_map(|prim| match prim {
                    crate::widget::DrawPrim::Text { s, .. } => Some(s),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let path =
            "/Users//someone/Library/Mobile Documents/Pictures/Backdrops/sunset-over-the-bay.png";
        let node = field(path, false);
        let shown = painted(&node);
        assert!(
            shown.starts_with(TEXT_FIELD_ELLIPSIS) && shown.ends_with("sunset-over-the-bay.png"),
            "a path keeps its end: {shown:?}"
        );
        let UiContent::TextField(control) = &node.content else {
            unreachable!()
        };
        let size = native_type_px(TypeStep::Secondary).get();
        let geometry = text_field_geometry(control, rect, size);
        assert_eq!(geometry.visible.end, path.len());
        let audit = text_fit_audit(&node).expect("audited");
        assert!(!audit.overflow, "deliberate fitting: {audit:?}");
        let words = "an ordinary value far too long to show whole in this field";
        let shown = painted(&field(words, false));
        assert!(
            shown.starts_with("an ordinary") && shown.ends_with(TEXT_FIELD_ELLIPSIS),
            "anything else keeps its start: {shown:?}"
        );
        // Negative controls.
        assert_eq!(painted(&field("/tmp/wall.png", false)), "/tmp/wall.png");
        assert!(!painted(&field(path, true)).contains(TEXT_FIELD_ELLIPSIS));
    }

    /// A wheel at brightness 0 still marks the hue it was picked at: the
    /// marker is the picker's model, not the black `#000000` it saves.
    /// Negative control: no colour marks nothing.
    #[test]
    fn a_black_wheel_marks_the_kept_hue() {
        use crate::widget::DrawPrim;
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(0.0, 0.0, 200.0, 200.0);
        let paint = |hsv: Option<[f32; 3]>| {
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("wheel"),
                rect,
                clip: rect,
                content: UiContent::ColorWheel(Control::new(
                    ColorWheelSpec {
                        label: "Foreground color wheel".to_string(),
                        hsv,
                    },
                    ActionId::new("settings/color-pick/foreground"),
                )),
                peek: false,
                fade: None,
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            prims
        };
        let prims = paint(Some([0.25, 1.0, 0.0]));
        let disk = prims.iter().find_map(|prim| match prim {
            DrawPrim::HsvDisk { value, .. } => Some(*value),
            _ => None,
        });
        assert_eq!(disk, Some(0.0), "the disk is painted black");
        let (mx, my) = color_wheel_marker_at(rect, 0.25, 1.0);
        let dots: Vec<(f32, f32)> = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Dot { cx, cy, .. } => Some((*cx, *cy)),
                _ => None,
            })
            .collect();
        assert!(
            !dots.is_empty() && dots.iter().all(|&(x, y)| x == mx && y == my),
            "the marker sits at 3 o'clock on the rim: {dots:?} vs {mx},{my}"
        );
        let geometry = color_wheel_geometry(rect);
        assert!(mx > geometry.cx + geometry.r * 0.9, "off centre: {mx}");
        assert!(
            paint(None)
                .iter()
                .all(|prim| !matches!(prim, DrawPrim::Dot { .. })),
            "no colour, no marker"
        );
    }

    /// Keyboard focus on a SELECTED segment (or default button) is as plain
    /// as on any other control: an inner ring in the label
    /// colour, which a selected control at rest does not wear; a destructive
    /// button wears a danger rim and label.
    #[test]
    fn focus_on_a_selected_segment_wears_a_ring_and_danger_reads_as_danger() {
        use crate::widget::DrawPrim;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let theme = aterm_render::Theme::default();
        let roles = crate::settings::Roles::from_theme(theme);
        let rect = LogicalRect::new(10.0, 10.0, 120.0, 44.0);
        let paint = |style: StyleRef, selected: bool, focus_visible: bool| {
            let mut control = Control::new(
                ButtonSpec::new("Auto").visual_label("Auto"),
                ActionId::new("settings/set/bidi"),
            )
            .style(style);
            control.state.selected = selected;
            control.state.focus_visible = focus_visible;
            let node = PaintNode {
                cut_edges: CutEdges::NONE,
                key: UiKey::new("settings/control/bidi"),
                rect,
                clip: rect,
                content: UiContent::Button(control),
                peek: false,
                fade: None,
                joins: Joins::default(),
            };
            let mut prims = Vec::new();
            paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
            prims
        };
        let inner_ring = |prims: &[DrawPrim]| {
            prims.iter().any(|prim| {
                matches!(prim, DrawPrim::Stroke { w, color, .. }
                    if *w < rect.width - 2.0 && color[..3] == roles.text_primary)
            })
        };
        assert!(inner_ring(&paint(StyleRef::Navigation, true, true)));
        assert!(!inner_ring(&paint(StyleRef::Navigation, true, false)));
        let danger = paint(StyleRef::Danger, false, false);
        assert!(danger.iter().any(|prim| matches!(prim,
            DrawPrim::Stroke { color, .. } if color[..3] == roles.danger)));
    }

    /// AN IDLE CHOICE LOOKS IDLE (`ControlState::muted`: Default autopilot
    /// while supervision is off): its selected segment keeps a wash and a rim,
    /// so the choice still reads, but none of it — wash, rim, label — is the
    /// accent a live choice wears, and its label is the quiet text every idle
    /// control says its value in. A popup's value goes quiet the same way. A
    /// focus ring stays the accent. NEGATIVE CONTROL: the live segment paints
    /// the accent's wash, rim and ink.
    #[test]
    fn a_muted_selected_segment_paints_no_accent() {
        use crate::widget::DrawPrim;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let terminal = aterm_render::Theme::default();
        for theme in [
            terminal,
            crate::native_appearance::forced_chrome_theme(terminal, false),
        ] {
            let roles = crate::settings::Roles::from_theme(theme);
            let rect = LogicalRect::new(10.0, 10.0, 88.0, 36.0);
            let paint = |style: StyleRef, selected: bool, muted: bool, focus_visible: bool| {
                let mut control = Control::new(
                    ButtonSpec::new("Default autopilot: Cruise").visual_label("Cruise"),
                    ActionId::new("settings/set/harness.autopilot"),
                )
                .style(style);
                control.state.selected = selected;
                control.state.muted = muted;
                control.state.focus_visible = focus_visible;
                let node = PaintNode {
                    cut_edges: CutEdges::NONE,
                    key: UiKey::new("settings/control/harness.autopilot"),
                    rect,
                    clip: rect,
                    content: UiContent::Button(control),
                    peek: false,
                    fade: None,
                    joins: Joins::default(),
                };
                let mut prims = Vec::new();
                paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
                prims
            };
            let wash = mix_rgb(
                roles.elevated,
                roles.accent,
                crate::native_appearance::ACCENT_WASH_SELECTED,
            );
            let accented = |rgb: &[u8]| {
                rgb == roles.accent || rgb == roles.accent_ink || rgb == wash.as_slice()
            };
            let panels = |prims: &[DrawPrim]| -> Vec<[u8; 3]> {
                prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                        _ => None,
                    })
                    .collect()
            };
            let strokes = |prims: &[DrawPrim]| -> Vec<[u8; 3]> {
                prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Stroke { color, .. } => Some([color[0], color[1], color[2]]),
                        _ => None,
                    })
                    .collect()
            };
            let ink = |prims: &[DrawPrim]| -> Vec<[u8; 3]> {
                prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                        _ => None,
                    })
                    .collect()
            };
            let live = paint(StyleRef::Navigation, true, false, false);
            assert!(
                panels(&live).iter().any(|rgb| accented(rgb)),
                "{theme:?}: the live choice washes in the accent"
            );
            assert!(strokes(&live).iter().any(|rgb| accented(rgb)));
            assert_eq!(ink(&live), [roles.accent_ink]);
            let idle = paint(StyleRef::Navigation, true, true, false);
            assert!(
                panels(&idle)
                    .iter()
                    .chain(&strokes(&idle))
                    .chain(&ink(&idle))
                    .all(|rgb| !accented(rgb)),
                "{theme:?}: no accent on an idle choice: {idle:?}"
            );
            assert!(
                !panels(&idle).is_empty() && !strokes(&idle).is_empty(),
                "{theme:?}: the idle choice still has its cell and rim"
            );
            let unchosen = paint(StyleRef::Navigation, false, true, false);
            assert!(
                panels(&unchosen).is_empty() && strokes(&unchosen).is_empty(),
                "{theme:?}: and still reads chosen beside an idle unchosen one, flush"
            );
            assert_eq!(ink(&idle), [quiet_text(&roles)], "{theme:?}");
            // The focus ring is the keyboard's: the accent, idle or not.
            assert!(
                strokes(&paint(StyleRef::Navigation, true, true, true)).contains(&roles.accent),
                "{theme:?}: an idle choice under focus wears the accent ring"
            );
            // A popup's value: quiet when idle, primary when live.
            assert_eq!(
                ink(&paint(StyleRef::Setting, false, false, false)),
                [roles.text_primary]
            );
            assert_eq!(
                ink(&paint(StyleRef::Setting, false, true, false)),
                [quiet_text(&roles)]
            );
        }
    }

    /// A RESTING CONTROL IS RAISED, NOT SUNKEN, on a Windows light theme: a
    /// Secondary button ("More styles…") and a Setting popup ("Default Light")
    /// rest lighter than the card under them, WinUI's near-white control fill,
    /// where they used to mix a step toward the ink and sit darker (#ececec on a
    /// #f6f6f6 card, the 2026-10-18 judge). Dark themes keep the step toward the
    /// ink, which is lighter there too. NEGATIVE CONTROL: macOS and Linux keep
    /// their light controls a step darker than the card.
    #[test]
    fn resting_controls_rise_above_their_card_on_windows_light_themes() {
        use crate::native_appearance::{contrast_ratio, surface_is_dark};
        use crate::widget::DrawPrim;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let openness = |c: [u8; 3]| contrast_ratio(c, [0, 0, 0]);
        let mut light = 0;
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
            let theme = aterm_render::Theme::from(&scheme);
            let roles = crate::settings::Roles::from_theme(theme);
            let rect = LogicalRect::new(10.0, 10.0, 160.0, 36.0);
            for style in [StyleRef::Secondary, StyleRef::Setting] {
                let control = Control::new(
                    ButtonSpec::new("More styles").visual_label("More styles"),
                    ActionId::new("settings/open/cursor.styles"),
                )
                .style(style);
                let node = PaintNode {
                    cut_edges: CutEdges::NONE,
                    key: UiKey::new("settings/control/cursor.styles"),
                    rect,
                    clip: rect,
                    content: UiContent::Button(control),
                    peek: false,
                    fade: None,
                    joins: Joins::default(),
                };
                let mut prims = Vec::new();
                paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
                let fill = prims
                    .iter()
                    .find_map(|prim| match prim {
                        DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                        _ => None,
                    })
                    .expect("a resting control paints its fill");
                assert_eq!(fill, control_rest_fill(&roles), "{name} {style:?}");
                let card = roles.elevated;
                if surface_is_dark(roles.surface) {
                    assert!(openness(fill) > openness(card), "{name}: dark rest");
                } else {
                    light += 1;
                    assert_eq!(
                        openness(fill) > openness(card),
                        cfg!(windows),
                        "{name} {style:?}: rest fill {fill:?} on card {card:?}"
                    );
                }
            }
        }
        assert!(light >= 8, "light built-ins measured: {light}");
    }

    /// A QUIET BUTTON THAT SHOWS WORDS IS A BUTTON on Windows: Browse
    /// themes…, Reset, Import… and Folder paint WinUI's standard Button, the
    /// opaque resting fill inside the separator rim, exactly as the
    /// Secondary Customize beside them does, at rest, hovered and disabled.
    /// They rested at 72/255 of that fill with no rim, 1.01:1 on a light card
    /// (the 2026-10-18 Fluent judge's
    /// `J4F-8-settings-secondary-buttons-invisible`). A quiet icon button
    /// stays subtle (no rim). NEGATIVE CONTROL: macOS and Linux keep the quiet
    /// text button rimless.
    #[test]
    fn quiet_text_buttons_are_standard_buttons_on_windows() {
        use crate::widget::DrawPrim;
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let rimmed = |prims: &[DrawPrim]| {
            prims
                .iter()
                .any(|prim| matches!(prim, DrawPrim::Stroke { width, .. } if *width == 1.0))
        };
        for name in ["Default Light", "Default"] {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in scheme");
            let theme = aterm_render::Theme::from(&scheme);
            let roles = crate::settings::Roles::from_theme(theme);
            let rect = LogicalRect::new(10.0, 10.0, 120.0, 36.0);
            let paint = |spec: ButtonSpec, style: StyleRef, state: ControlState| {
                let control = Control::new(spec, ActionId::new("settings/route/themes"))
                    .state(state)
                    .style(style);
                let node = PaintNode {
                    cut_edges: CutEdges::NONE,
                    key: UiKey::new("settings/theme/browse"),
                    rect,
                    clip: rect,
                    content: UiContent::Button(control),
                    peek: false,
                    fade: None,
                    joins: Joins::default(),
                };
                let mut prims = Vec::new();
                paint_compiled_node(&mut prims, &node, roles, theme, 14.0);
                prims
            };
            let words = || ButtonSpec::new("Browse themes").visual_label("Browse themes\u{2026}");
            for state in [
                ControlState::default(),
                ControlState {
                    hovered: true,
                    ..ControlState::default()
                },
                ControlState {
                    enabled: false,
                    ..ControlState::default()
                },
            ] {
                let quiet = paint(words(), StyleRef::Quiet, state);
                let secondary = paint(words(), StyleRef::Secondary, state);
                assert_eq!(rimmed(&quiet), cfg!(windows), "{name} {state:?}");
                assert_eq!(
                    format!("{quiet:?}") == format!("{secondary:?}"),
                    cfg!(windows),
                    "{name} {state:?}: a quiet text button is a Secondary one on Windows only"
                );
            }
            let rest = paint(words(), StyleRef::Quiet, ControlState::default());
            let fill = rest
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Panel { fill, .. } => Some(*fill),
                    _ => None,
                })
                .expect("a resting quiet button paints its fill");
            let rest_fill = control_rest_fill(&roles);
            if cfg!(windows) {
                assert_eq!(
                    fill,
                    [rest_fill[0], rest_fill[1], rest_fill[2], 255],
                    "{name}"
                );
            } else {
                assert_eq!(fill[3], 72, "{name}: the quiet wash elsewhere");
            }
            // A quiet icon button stays subtle on every platform.
            let icon = paint(
                ButtonSpec::new("Browse themes").visual_icon(ButtonIcon::Forward),
                StyleRef::Quiet,
                ControlState::default(),
            );
            assert!(!rimmed(&icon), "{name}: a quiet icon button has no rim");
        }
    }

    /// WORDS AND PICTOGRAMS AT A LIST'S EDGES ARE WHOLE OR NOT DRAWN (#98,
    /// #99): a list whose bottom no strip fades under (a bar up, the page's
    /// edge) paints none of a line of words its bottom edge cuts through, and
    /// neither edge paints any of a pictogram whose ink it cuts through,
    /// where each painted a sliver under the search field or over the
    /// window's bottom (Top Settings' "COLOR PREVIEW" at 220x12 at 2× text,
    /// the sidebar's icons); what they leave whole paints as it is, and a
    /// control they cut stays live where it shows. CONTROL: where the strip
    /// fades under the room, the cut line paints, fading out with it.
    #[test]
    fn a_lists_edges_cut_no_line_of_words_or_pictogram_through() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let list = |fades: bool| {
            let icon = UiNode::new(
                "list/top",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Window").visual_icon(ButtonIcon::Window),
                        ActionId::new("list/top"),
                    )
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(32.0))
                    .height(Length::Fixed(24.0)),
            );
            let filler = UiNode::new(
                "list/filler",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::default().height(Length::Fixed(52.0)));
            let words = UiNode::new(
                "list/bottom",
                UiContent::Text(TextSpec::text("COLOR PREVIEW")),
            )
            .layout(Layout::default().height(Length::Fixed(24.0)));
            // 72pt drawn, its room the first 60: a strip of 12 under it.
            let room = if fades { 60.0 } else { 72.0 };
            UiTree::new(
                UiNode::new(
                    "app",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
                )
                .layout(Layout::column().padding(Insets::all(8.0)))
                .children(vec![
                    UiNode::new(
                        "list",
                        UiContent::Group(GroupSpec::unlabeled(SemanticRole::List)),
                    )
                    .layout(
                        Layout::column()
                            .height(Length::Fixed(72.0))
                            .scrolled(12.0)
                            .clipped(),
                    )
                    .room(room, fades)
                    .children(vec![icon, filler, words]),
                ]),
            )
            .compile(LogicalRect::new(0.0, 0.0, 240.0, 120.0))
            .expect("compiles")
        };
        let words_painted = |compiled: &CompiledUi| {
            compiled.tray(aterm_render::Theme::default(), 14.0).prims.iter().any(|prim| {
                matches!(prim, crate::widget::DrawPrim::Text { s, .. } if s.contains("PREVIEW"))
            })
        };
        let hard = list(false);
        // The words (72..96) cross the list's 80pt bottom (8 + 72), the icon
        // (-4..20) its 8pt top.
        let node = |key: &str| {
            hard.paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .unwrap_or_else(|| panic!("{key} is painted"))
                .cut_edges
        };
        assert!(
            node("list/bottom").bottom.is_some(),
            "{:?}",
            node("list/bottom")
        );
        assert!(node("list/top").top.is_some(), "{:?}", node("list/top"));
        assert!(!words_painted(&hard), "the bottom edge slices no words");
        assert!(hard.sliced_marks().is_empty(), "{:#?}", hard.sliced_marks());
        assert!(
            hard.semantic(&UiKey::new("list/top")).is_some(),
            "the cut button is live where it shows"
        );
        // The pictogram's prims are gone with its cut ink: the button paints
        // fewer prims than it does whole.
        let prims_of = |compiled: &CompiledUi, key: &str| {
            let node = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .expect("painted");
            node.painted_prims().len()
        };
        let whole = UiTree::new(
            UiNode::new(
                "list/top",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Window").visual_icon(ButtonIcon::Window),
                        ActionId::new("list/top"),
                    )
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(32.0))
                    .height(Length::Fixed(24.0)),
            ),
        )
        .compile(LogicalRect::new(0.0, 0.0, 32.0, 24.0))
        .expect("compiles");
        assert!(prims_of(&hard, "list/top") < prims_of(&whole, "list/top"));
        // CONTROL: the fading strip draws the cut line, a picture fading out.
        let fading = list(true);
        assert!(words_painted(&fading), "the strip fades the line out");
    }
}
