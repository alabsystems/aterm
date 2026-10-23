// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings colour picker ([`crate::native_ui::UiContent::ColorWheel`], [`color_picker_node`]) for
//! [`ColorTarget::Key`] and [`ColorTarget::Slot`], with HSV retention
//! ([`SettingsViewState::color_kept`]), default swatches ([`unset_color_swatch`]), pointer picks
//! ([`SemanticInput::ColorPick`]), and cursor-matched trail colour ([`prefs::TRAIL_COLOR_CURSOR`]).

use super::*;
use crate::native_ui::ColorWheelSpec;

/// A colour the picker made, with the exact hue, saturation and brightness that made it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct KeptColor {
    key: &'static str,
    rgb: [u8; 3],
    hsv: [f32; 3],
}

/// How many colours the picker remembers: enough for an Undo trail back
/// through every colour of a session's picking.
const KEPT_COLORS: usize = 32;

/// One turn of the wheel's hue under one Left or Right (Shift: six).
const HUE_STEP: f32 = 1.0 / 72.0;

/// One step of the wheel's saturation under one Up or Down.
const SATURATION_STEP: f32 = 0.05;

/// Brightness moves in whole twentieths, the slider's grid.
const BRIGHTNESS_STEP: f64 = 0.05;

/// The node keys' and actions' name for the palette slot Themes' Customize sheet has open.
pub(super) const PALETTE_SLOT_ID: &str = "palette-slot";

/// The names the picker remembers each palette slot's colours under.
const PALETTE_SLOT_KEPT: [&str; 16] = [
    "palette-slot/0",
    "palette-slot/1",
    "palette-slot/2",
    "palette-slot/3",
    "palette-slot/4",
    "palette-slot/5",
    "palette-slot/6",
    "palette-slot/7",
    "palette-slot/8",
    "palette-slot/9",
    "palette-slot/10",
    "palette-slot/11",
    "palette-slot/12",
    "palette-slot/13",
    "palette-slot/14",
    "palette-slot/15",
];

/// What a picker picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColorTarget {
    Key(&'static str),
    Slot(usize),
}

impl ColorTarget {
    /// The node keys' and actions' name: the key, or [`PALETTE_SLOT_ID`].
    const fn id(self) -> &'static str {
        match self {
            Self::Key(key) => key,
            Self::Slot(_) => PALETTE_SLOT_ID,
        }
    }

    /// The name the picker remembers this target's colours under.
    fn kept(self) -> &'static str {
        match self {
            Self::Key(key) => key,
            Self::Slot(index) => PALETTE_SLOT_KEPT[index.min(15)],
        }
    }
}

/// The target a picker's node keys and actions name `id`.
pub(super) fn color_target(state: &SettingsViewState, id: &str) -> Option<ColorTarget> {
    if id == PALETTE_SLOT_ID {
        return state
            .showcase
            .slot
            .as_ref()
            .map(|slot| ColorTarget::Slot(slot.index));
    }
    color_key(state, id).map(ColorTarget::Key)
}

impl SettingsViewState {
    /// Remember that the picker made `key`'s colour from `hsv` (`key` as
    /// [`ColorTarget::kept`] names it), most recent first, and return the
    /// colour.
    fn keep_color(&mut self, key: &'static str, hsv: [f32; 3]) -> [u8; 3] {
        let rgb = crate::widget::hsv_to_rgb(hsv[0], hsv[1], hsv[2]);
        self.color_kept
            .retain(|kept| !(kept.key == key && kept.rgb == rgb));
        self.color_kept.insert(0, KeptColor { key, rgb, hsv });
        self.color_kept.truncate(KEPT_COLORS);
        rgb
    }

    /// The colour key whose picker is drawn under its row.
    pub(super) fn open_color_picker(&self) -> Option<&'static str> {
        self.color_picker.filter(|_| self.choice_picker.is_none())
    }

    /// Close the colour picker, the keyboard back on the Choose… that opened it.
    pub(super) fn close_color_picker(&mut self) -> bool {
        let Some(key) = self.color_picker.take() else {
            return false;
        };
        self.common.last_focus = Some(UiKey::new(format!("settings/color-open/{key}")));
        self.reveal_control(UiKey::new(format!("settings/control/{key}")));
        true
    }

    /// What the wheel that holds the keyboard picks for.
    pub(super) fn focused_color_wheel(&self) -> Option<ColorTarget> {
        let id = self
            .common
            .last_focus
            .as_ref()?
            .as_str()
            .strip_prefix("settings/color/")?
            .strip_suffix("/wheel")?;
        color_target(self, id)
    }

    /// What the Brightness slider that holds the keyboard sets.
    pub(super) fn focused_color_brightness(&self) -> Option<ColorTarget> {
        let id = self
            .common
            .last_focus
            .as_ref()?
            .as_str()
            .strip_prefix("settings/color/")?
            .strip_suffix("/brightness")?;
        color_target(self, id)
    }
}

/// `key` as the registry's own key, where Settings edits it as a colour.
pub(super) fn color_key(state: &SettingsViewState, key: &str) -> Option<&'static str> {
    state
        .field_by_key(key)
        .filter(|field| matches!(field.kind, EditKind::Color))
        .map(|field| field.key)
}

/// `#RRGGBB`.
fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

/// The colours the theme itself draws for the colour keys while they are
/// unset: the working theme (pinned, drafted or saved, on the side the
/// preview shows) with no colour key over it.
fn theme_own_colors(state: &SettingsViewState) -> crate::resolved_colors::ResolvedColors {
    let theme = state
        .environment_override(prefs::EDIT_THEME)
        .map(|active| active.effective.clone())
        .or_else(|| {
            (state.is_explicit(prefs::EDIT_THEME)
                || state.field_has_unsaved_draft(prefs::EDIT_THEME))
            .then(|| field_text(state, prefs::EDIT_THEME, ""))
        })
        .filter(|value| !value.trim().is_empty());
    let config = Config {
        theme,
        ..Config::default()
    };
    let side = theme_visible_side(state, state.presented_motion.get().system_dark);
    config.resolved_colors(side, &state.assets.themes, None)
}

/// The colour an unset colour key draws, the one its placeholder names.
pub(super) fn unset_color_swatch(state: &SettingsViewState, key: &str) -> Option<[u8; 3]> {
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    match key {
        prefs::EDIT_CURSOR_TRAIL_COLOR => {
            let style = crate::app_config::resolve_trail_style(&saved_cursor_style(state)).style?;
            let pinned = state.is_explicit(prefs::EDIT_CURSOR_COLOR);
            let (color, _) = aterm_effects::cursor_fx::own_trail_color(style, pinned)?;
            Some(rgb(color))
        }
        prefs::EDIT_CURSOR_TRAIL_ACCENT => {
            use aterm_effects::cursor_fx::AccentBase;
            let style = crate::app_config::resolve_trail_style(&saved_cursor_style(state)).style?;
            let color = prefs::trail_color_setting(
                state.raw_value(prefs::EDIT_CURSOR_TRAIL_COLOR).as_deref(),
            );
            let pinned = state.is_explicit(prefs::EDIT_CURSOR_COLOR);
            match aterm_effects::cursor_fx::unset_accent_base(style, color, pinned) {
                AccentBase::Own(base, _) | AccentBase::Trail(base) => {
                    Some(rgb(aterm_effects::cursor_fx::brighten(base, 1.5)))
                }
                AccentBase::Cursor => None,
            }
        }
        prefs::EDIT_ACTIVE_TAB_COLOR => Some(state.presented_motion.get().automatic_tab_color),
        prefs::EDIT_FOREGROUND => Some(rgb(theme_own_colors(state).theme().fg)),
        prefs::EDIT_BACKGROUND => Some(rgb(theme_own_colors(state).theme().bg)),
        prefs::EDIT_CURSOR_COLOR => Some(rgb(theme_own_colors(state).theme().cursor)),
        prefs::EDIT_SELECTION_COLOR => Some(rgb(theme_own_colors(state).theme().selection)),
        prefs::EDIT_SELECTION_FOREGROUND => theme_own_colors(state)
            .selection_fg()
            .map(|color| [color.r, color.g, color.b]),
        _ => None,
    }
}

/// The colour Modified names as a colour row's default.
pub(super) fn default_color_chip(state: &SettingsViewState, field: &EditField) -> Option<[u8; 3]> {
    if !matches!(field.kind, EditKind::Color) || default_value_label(field.key).is_none() {
        return None;
    }
    unset_color_swatch(state, field.key)
}

/// The colour `key` draws now.
pub(super) fn color_now(state: &SettingsViewState, key: &str) -> Option<[u8; 3]> {
    let raw = state.raw_value(key);
    if trail_color_is_cursor(key, raw.as_deref()) {
        // The cursor's colour, which a program may recolour: no one colour.
        return None;
    }
    raw.as_deref()
        .and_then(aterm_types::scheme::parse_hex)
        .map(|color| [color.r, color.g, color.b])
        .or_else(|| unset_color_swatch(state, key))
}

/// Whether `raw` is the trail colour's word for the cursor's colour
/// ([`prefs::TRAIL_COLOR_CURSOR`]).
fn trail_color_is_cursor(key: &str, raw: Option<&str>) -> bool {
    key == prefs::EDIT_CURSOR_TRAIL_COLOR
        && prefs::trail_color_setting(raw) == aterm_effects::cursor_fx::TrailColor::Cursor
}

/// The colour `target` draws now: a key's ([`color_now`]), or the open
/// palette slot's draft, else the colour the slot shows.
fn target_now(state: &SettingsViewState, target: ColorTarget) -> Option<[u8; 3]> {
    match target {
        ColorTarget::Key(key) => color_now(state, key),
        ColorTarget::Slot(index) => {
            let shown = state.showcase_colors().scheme.ansi.get(index).copied();
            state
                .showcase
                .slot
                .as_ref()
                .filter(|slot| slot.index == index)
                .and_then(|slot| aterm_types::scheme::parse_hex(slot.input.value().trim()))
                .or(shown)
                .map(|color| [color.r, color.g, color.b])
        }
    }
}

/// THE PICKER'S MODEL of `target`'s colour, as hue, saturation and
/// brightness: the exact ones the picker made the colour now drawn from,
/// where it made it, else the colour's own.
fn target_hsv(state: &SettingsViewState, target: ColorTarget) -> Option<[f32; 3]> {
    let rgb = target_now(state, target)?;
    let kept = target.kept();
    Some(
        state
            .color_kept
            .iter()
            .find(|entry| entry.key == kept && entry.rgb == rgb)
            .map_or_else(
                || {
                    let (hue, saturation, value) = crate::widget::rgb_to_hsv(rgb);
                    [hue, saturation, value]
                },
                |entry| entry.hsv,
            ),
    )
}

/// [`target_hsv`], for the reducer's slider steps.
pub(super) fn color_target_hsv(state: &SettingsViewState, target: ColorTarget) -> Option<[f32; 3]> {
    target_hsv(state, target)
}

/// What a picker shows for its target: the names its controls speak and
/// the colour in words.
struct ColorSubject {
    target: ColorTarget,
    /// "Foreground color wheel", "Colour wheel for palette colour 3, yellow".
    wheel_label: String,
    /// "Foreground brightness".
    brightness_label: String,
    /// "Choose the foreground color".
    card_label: String,
    /// The colour in words ([`color_words`]).
    words: String,
}

impl ColorSubject {
    fn of(state: &SettingsViewState, target: ColorTarget) -> Option<Self> {
        match target {
            ColorTarget::Key(key) => {
                let field = state.field_by_key(key)?;
                let noun = color_noun(field.label);
                let mut spoken = noun.clone();
                if let Some(first) = spoken.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                Some(Self {
                    target,
                    wheel_label: format!("{spoken} wheel"),
                    brightness_label: format!("{} brightness", field.label),
                    card_label: format!("Choose the {noun}"),
                    words: color_words(state, field),
                })
            }
            ColorTarget::Slot(index) => {
                let name = super::showcase_page::SLOT_NAMES.get(index)?;
                let slot = format!("palette colour {index}, {name}");
                Some(Self {
                    target,
                    wheel_label: format!("Colour wheel for {slot}"),
                    brightness_label: format!("Brightness of {slot}"),
                    card_label: format!("Choose {slot}"),
                    words: target_now(state, target).map_or_else(String::new, hex),
                })
            }
        }
    }
}

/// The colour a row names, in words: the saved `#RRGGBB`, or the default's
/// words and the colour they name ("Automatic (default), #3A3A3E").
fn color_words(state: &SettingsViewState, field: &EditField) -> String {
    if trail_color_is_cursor(field.key, state.raw_value(field.key).as_deref()) {
        return prefs::TRAIL_COLOR_CURSOR_LABEL.to_string();
    }
    if let Some(color) = state
        .raw_value(field.key)
        .as_deref()
        .and_then(aterm_types::scheme::parse_hex)
    {
        return hex([color.r, color.g, color.b]);
    }
    match unset_color_swatch(state, field.key) {
        Some(rgb) => format!("{}, {}", field.placeholder, hex(rgb)),
        None => field.placeholder.clone(),
    }
}

/// "foreground color", "cursor color": the setting as the colour it is.
fn color_noun(label: &str) -> String {
    let lower = label.to_lowercase();
    if lower.ends_with("color") {
        lower
    } else {
        format!("{lower} color")
    }
}

/// The subject's colour wheel, `height` tall. Its value is the colour in
/// words; it takes a pick ([`SemanticInput::ColorPick`]), a `#RRGGBB`, or,
/// pressed with no value, sends the keyboard to the hex row.
fn color_wheel_node(state: &SettingsViewState, subject: &ColorSubject, height: f32) -> UiNode {
    let id = subject.target.id();
    UiNode::new(
        format!("settings/color/{id}/wheel"),
        UiContent::ColorWheel(
            Control::new(
                ColorWheelSpec {
                    label: subject.wheel_label.clone(),
                    hsv: target_hsv(state, subject.target),
                },
                ActionId::new(format!("settings/color-pick/{id}")),
            )
            .value(SemanticValue::Text(subject.words.clone()))
            .state(ControlState::default())
            .style(StyleRef::Setting),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
}

/// The subject's Brightness slider: the colour's brightness in whole
/// percent, its hue and saturation kept.
fn color_brightness_node(state: &SettingsViewState, subject: &ColorSubject) -> UiNode {
    let id = subject.target.id();
    let hsv = target_hsv(state, subject.target);
    let brightness = hsv.map_or(1.0, |[_, _, value]| {
        (f64::from(value) * 100.0).round() / 100.0
    });
    let slider_key = format!("settings/color/{id}/brightness");
    UiNode::new(
        slider_key.clone(),
        UiContent::Slider(
            Control::new(
                SliderSpec {
                    description: None,
                    track: None,
                    label: subject.brightness_label.clone(),
                    step: BRIGHTNESS_STEP,
                    display_value: format!("{:.0}%", brightness * 100.0),
                },
                ActionId::new(format!("settings/color-brightness/{id}")),
            )
            .value(SemanticValue::Number {
                value: brightness,
                minimum: 0.0,
                maximum: 1.0,
            })
            .state(ControlState {
                enabled: hsv.is_some(),
                focused: state
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|focus| focus.as_str() == slider_key),
                ..ControlState::default()
            })
            .style(StyleRef::Setting),
        ),
    )
}

/// Brightness as a row of a column `column_width` wide, laid out the way a
/// registry row is at `width`.
fn color_brightness_row(
    state: &SettingsViewState,
    subject: &ColorSubject,
    width: SettingsWidth,
    column_width: f32,
) -> (UiNode, f32) {
    let id = subject.target.id();
    let key = format!("settings/color/{id}/brightness-row");
    let slider = color_brightness_node(state, subject);
    let Some((button, button_width)) = cursor_color_button(state, subject) else {
        return page_control_row(&key, "Brightness", slider, width, column_width);
    };
    let (alone, height) = page_control_row(&key, "Brightness", slider.clone(), width, column_width);
    let control_width = match alone.children.get(1).map(|control| control.layout.width) {
        Some(Length::Fixed(room)) => room,
        _ => column_width,
    };
    const GAP: f32 = 8.0;
    if control_width - button_width - GAP >= PAGE_CONTROL_FLOOR * settings_text_scale().min(1.25) {
        let mut slider = slider;
        slider.layout = Layout::default().width(Length::Fill).height(Length::Fill);
        let control = UiNode::new(
            format!("settings/color/{id}/brightness-controls"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::row().gap(GAP))
        .children(vec![slider, button]);
        return page_control_row(&key, "Brightness", control, width, column_width);
    }
    let line = scaled_control_height();
    let button_line = UiNode::new(
        format!("settings/color/{id}/cursor-line"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::row().height(Length::Fixed(line)))
    .children(vec![button]);
    let total = height + 4.0 + line;
    (
        UiNode::new(
            format!("settings/color/{id}/brightness-block"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().height(Length::Fixed(total)).gap(4.0))
        .children(vec![alone, button_line]),
        total,
    )
}

/// The node key of the trail colour picker's Cursor color.
fn cursor_color_button_key(key: &str) -> String {
    format!("settings/color/{key}/cursor")
}

/// THE TRAIL COLOUR'S CURSOR COLOR, in its picker, and its width: chosen
/// (marked, Brightness idle with no one colour to brighten), the trail is
/// drawn in the cursor's colour, following it as a program recolours it
/// (`cursor`, [`prefs::TRAIL_COLOR_CURSOR`], what the old `classic mono` style
/// says); pressed again, or a pick on the wheel, and the trail takes a colour
/// of its own.
fn cursor_color_button(state: &SettingsViewState, subject: &ColorSubject) -> Option<(UiNode, f32)> {
    let ColorTarget::Key(key) = subject.target else {
        return None;
    };
    if key != prefs::EDIT_CURSOR_TRAIL_COLOR {
        return None;
    }
    const VISUAL: &str = "Cursor color";
    let button_key = cursor_color_button_key(key);
    let chosen = trail_color_is_cursor(key, state.raw_value(key).as_deref());
    let width = crate::native_ui::native_button_label_width(VISUAL) + 26.0 + 22.0;
    let button = UiNode::new(
        button_key.clone(),
        UiContent::Button(
            Control::new(
                ButtonSpec {
                    label: if chosen {
                        "Cursor color, chosen: the trail follows the cursor's color".to_string()
                    } else {
                        "Cursor color: draw the trail in the cursor's color".to_string()
                    },
                    visual_label: Some(VISUAL.to_string()),
                    visual_icon: None,
                    trailing_icon: chosen.then_some(ButtonIcon::Check),
                    description: Some(
                        "The trail follows the cursor's color, as programs change it; pressed \
                         again, it keeps the color the cursor has now"
                            .to_string(),
                    ),
                    badge: false,
                    swatches: Vec::new(),
                    chip: None,
                    chevron_row: None,
                },
                ActionId::new(format!("settings/color-cursor/{key}")),
            )
            .value(SemanticValue::Text(
                if chosen { "Chosen" } else { "Not chosen" }.to_string(),
            ))
            .state(ControlState {
                selected: chosen,
                focused: state
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|focus| focus.as_str() == button_key),
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fill),
    );
    Some((button, width))
}

/// The wheel beside its controls, at least `floor` wide.
const SIDE_WHEEL_FLOOR: f32 = 96.0;
/// The wheel over its controls, at least this tall.
const WHEEL_FLOOR: f32 = 120.0;
/// Between the wheel and the column beside it.
const SIDE_GAP: f32 = 16.0;
/// The least column that still holds a label over its control.
const SIDE_COLUMN_FLOOR: f32 = 220.0;

/// The wheel's natural size at this width.
fn natural_wheel(width: SettingsWidth) -> f32 {
    if width == SettingsWidth::Compact {
        200.0
    } else {
        240.0
    }
}

/// THE PICKER BESIDE ITS CONTROLS: the wheel, and the column `controls`
/// builds for a column so wide (laid out as a `column` page lays its rows
/// out: a compact column puts each label over its control), in one row
/// `text_width` wide and at most `room` tall.
fn color_picker_beside(
    state: &SettingsViewState,
    subject: &ColorSubject,
    (width, floor, column): (SettingsWidth, f32, SettingsWidth),
    text_width: f32,
    (room, reserve): (f32, f32),
    controls: &dyn Fn(SettingsWidth, f32) -> Vec<(UiNode, f32)>,
) -> Option<(UiNode, f32)> {
    let side = room.clamp(floor, natural_wheel(width));
    let column_width = text_width - side - SIDE_GAP;
    let rows = controls(column, column_width);
    let column_height = rows.iter().map(|(_, height)| height).sum::<f32>()
        + rows.len().saturating_sub(1) as f32 * 4.0;
    let height = side.max(column_height);
    if column_width < SIDE_COLUMN_FLOOR || side.max(column_height + reserve) > room {
        return None;
    }
    let id = subject.target.id();
    let column = UiNode::new(
        format!("settings/color/{id}/controls"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .gap(4.0),
    )
    .children(rows.into_iter().map(|(node, _)| node).collect());
    let mut wheel = color_wheel_node(state, subject, height);
    wheel.layout.width = Length::Fixed(side);
    Some((
        UiNode::new(
            format!("settings/color/{id}/picker"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::row().height(Length::Fixed(height)).gap(SIDE_GAP))
        .children(vec![wheel, column]),
        height,
    ))
}

/// The rows of the picker over its controls: the wheel, yielding height
/// toward its `floor` where `room` is short, then the controls.
fn color_picker_stacked(
    state: &SettingsViewState,
    subject: &ColorSubject,
    (width, floor): (SettingsWidth, f32),
    text_width: f32,
    room: f32,
    controls: &dyn Fn(SettingsWidth, f32) -> Vec<(UiNode, f32)>,
) -> Vec<(UiNode, f32)> {
    let mut rows = controls(width, text_width);
    let rest = rows.iter().map(|(_, height)| height + 4.0).sum::<f32>();
    let wheel = (room - rest).clamp(floor, natural_wheel(width));
    rows.insert(0, (color_wheel_node(state, subject, wheel), wheel));
    rows
}

/// The least wheel a picker under a row draws, on a page whose list is
/// short (80x24's Search, a preview above it): still a disk to pick on.
const ROW_WHEEL_FLOOR: f32 = 64.0;

/// The least wheel beside its controls in a list shorter than any other
/// form of the picker: a disk the arrow keys and a pointer still pick on.
const TIGHT_WHEEL_FLOOR: f32 = 40.0;

/// `row` (a colour row, `key`'s) with its open picker under it, as one stop of the list.
pub(super) fn with_open_color_picker(
    state: &SettingsViewState,
    key: &str,
    row: UiNode,
    (width, text_width): (SettingsWidth, f32),
    (room, most): (f32, f32),
) -> UiNode {
    let row_height = fixed_height(&row);
    let picker =
        |room: f32| color_picker_node(state, key, width, text_width, room - row_height - 4.0);
    let Some((picker, height)) = picker(room)
        .filter(|(_, height)| row_height + 4.0 + height <= room)
        .or_else(|| picker(most.max(room)))
    else {
        return row;
    };
    UiNode::new(
        format!("settings/color/{key}/row"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(row_height + 4.0 + height))
            .gap(4.0),
    )
    .children(vec![row, picker])
}

/// The picker under a colour row, where its Choose… opened it.
pub(super) fn color_picker_node(
    state: &SettingsViewState,
    key: &str,
    width: SettingsWidth,
    text_width: f32,
    room: f32,
) -> Option<(UiNode, f32)> {
    let key = state.open_color_picker().filter(|open| *open == key)?;
    color_picker_card(state, ColorTarget::Key(key), width, text_width, room)
}

/// The palette slot's picker, under the open slot's hex field on Themes' Customize sheet.
pub(super) fn palette_slot_picker(
    state: &SettingsViewState,
    column_width: f32,
    room: f32,
) -> Option<(UiNode, f32)> {
    let index = state.showcase.slot.as_ref()?.index;
    color_picker_card(
        state,
        ColorTarget::Slot(index),
        SettingsWidth::Compact,
        column_width,
        room,
    )
}

/// The wheel and Brightness for `target`, beside each other where the
/// column has room, else stacked, at most `room` tall.
fn color_picker_card(
    state: &SettingsViewState,
    target: ColorTarget,
    width: SettingsWidth,
    text_width: f32,
    room: f32,
) -> Option<(UiNode, f32)> {
    let subject = ColorSubject::of(state, target)?;
    let id = target.id();
    let controls = |row_width: SettingsWidth, column_width: f32| {
        vec![color_brightness_row(
            state,
            &subject,
            row_width,
            column_width,
        )]
    };
    const PAD: f32 = 4.0;
    let inner = (room - 2.0 * PAD).max(scaled_control_height());
    let text_width = text_width - 2.0 * PAD;
    let beside = |floor: f32, column: SettingsWidth| {
        color_picker_beside(
            state,
            &subject,
            (width, floor, column),
            text_width,
            (inner, 0.0),
            &controls,
        )
        .map(|(picker, height)| (vec![picker], height))
    };
    let stacked = || {
        let rows = color_picker_stacked(
            state,
            &subject,
            (width, ROW_WHEEL_FLOOR),
            text_width,
            inner,
            &controls,
        );
        let height = rows.iter().map(|(_, height)| height).sum::<f32>()
            + rows.len().saturating_sub(1) as f32 * 4.0;
        (rows.into_iter().map(|(node, _)| node).collect(), height)
    };
    let (children, height): (Vec<UiNode>, f32) = beside(ROW_WHEEL_FLOOR, SettingsWidth::Compact)
        .unwrap_or_else(|| {
            let (rows, height) = stacked();
            if height <= inner {
                (rows, height)
            } else {
                beside(TIGHT_WHEEL_FLOOR, SettingsWidth::Medium).unwrap_or((rows, height))
            }
        });
    let height = height + 2.0 * PAD;
    let stack = UiNode::new(
        format!("settings/color/{id}/stack"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .padding(Insets::all(PAD))
            .gap(4.0),
    )
    .children(children);
    Some((
        UiNode::new(
            format!("settings/color/{id}/card"),
            UiContent::Group(GroupSpec::new(subject.card_label.clone())),
        )
        .layout(Layout::row().height(Length::Fixed(height)))
        .children(vec![stack]),
        height,
    ))
}

/// Whether the selected tab color's row is only a way to Tab Color.
pub(super) fn tab_color_row_links(
    state: &SettingsViewState,
    field: &EditField,
    modified_only: bool,
) -> bool {
    field.key == prefs::EDIT_ACTIVE_TAB_COLOR
        && !modified_only
        && state.search.trim().is_empty()
        && state.route != SettingsRoute::TabColor
        && !state.field_has_unsaved_draft(field.key)
        && !state.field_errors.contains_key(field.key)
}

/// Whether a colour row offers Choose…, the picker under the row.
pub(super) fn offers_color_chooser(
    state: &SettingsViewState,
    field: &EditField,
    modified_only: bool,
) -> bool {
    let searching = !state.search.trim().is_empty();
    matches!(field.kind, EditKind::Color)
        && (searching || modified_only || state.route != SettingsRoute::TabColor)
        && !tab_color_row_links(state, field, modified_only)
}

/// The control a row takes where a page owns its key, applied to a finished row's control.
pub(super) fn page_owned_control_form(
    state: &SettingsViewState,
    field: &EditField,
    modified_only: bool,
    (width, inline, text_width): (SettingsWidth, bool, f32),
    row: &mut UiNode,
) {
    if tab_color_row_links(state, field, modified_only) {
        link_tab_color_row(state, field, row);
    } else if offers_color_chooser(state, field, modified_only) {
        add_color_chooser(state, field, row, (width, inline, text_width));
    } else if wallpaper_row_links(state, field, modified_only) {
        link_wallpaper_row(state, field, row);
    }
}

/// Whether Modified's wallpaper row is a way to the Wallpaper page: while
/// the row holds no draft of its own to correct.
fn wallpaper_row_links(state: &SettingsViewState, field: &EditField, modified_only: bool) -> bool {
    field.key == prefs::EDIT_WALLPAPER
        && modified_only
        && state.raw_value(field.key).is_some()
        && !state.field_has_unsaved_draft(field.key)
        && !state.field_errors.contains_key(field.key)
}

/// MODIFIED'S WALLPAPER ROW: its control becomes a button that names the
/// image as the Wallpaper page's status does (the file's name and size, not
/// the head of its path; [`wallpaper_words`]), the whole path its value, and
/// opens the Wallpaper page, which chooses and removes it.
fn link_wallpaper_row(state: &SettingsViewState, field: &EditField, row: &mut UiNode) {
    let key = format!("settings/control/{}", field.key);
    let Some((visual, spoken)) = wallpaper_words(state) else {
        return;
    };
    let Some(control) = find_node(row, &key) else {
        return;
    };
    let path = state.raw_value(field.key).unwrap_or_default();
    let focused = state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|focus| focus.as_str() == key);
    control.content = UiContent::Button(
        Control::new(
            ButtonSpec {
                label: format!("{visual}: Wallpaper image {spoken}; opens Wallpaper"),
                visual_label: Some(visual.clone()),
                visual_icon: None,
                trailing_icon: Some(ButtonIcon::Forward),
                description: Some("Opens Wallpaper, to choose or remove the image".to_string()),
                badge: false,
                swatches: Vec::new(),
                chip: None,
                chevron_row: None,
            },
            ActionId::new(format!("settings/route{}", SettingsRoute::Wallpaper.path())),
        )
        .value(SemanticValue::Text(path))
        .state(ControlState {
            focused,
            ..ControlState::default()
        })
        .style(StyleRef::Setting),
    );
}

/// The keys a row's page-owned control adds to the keyboard order after its field.
pub(super) fn page_owned_control_keys(
    state: &SettingsViewState,
    field: &EditField,
    modified_only: bool,
) -> Option<UiKey> {
    offers_color_chooser(state, field, modified_only)
        .then(|| UiKey::new(format!("settings/color-open/{}", field.key)))
}

/// The keys an open picker under `key`'s row adds to the keyboard order, after the row's own.
pub(super) fn open_color_picker_keys(state: &SettingsViewState, key: &str) -> Vec<UiKey> {
    match state.open_color_picker().filter(|open| *open == key) {
        Some(key) => {
            let mut keys = vec![
                UiKey::new(format!("settings/color/{key}/wheel")),
                UiKey::new(format!("settings/color/{key}/brightness")),
            ];
            if key == prefs::EDIT_CURSOR_TRAIL_COLOR {
                keys.push(UiKey::new(cursor_color_button_key(key)));
            }
            keys
        }
        None => Vec::new(),
    }
}

fn find_node<'a>(node: &'a mut UiNode, key: &str) -> Option<&'a mut UiNode> {
    if node.key.as_str() == key {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node(child, key))
}

/// The Window page's selected tab color row: its control becomes a button
/// that names the colour in use, wears its chip (the automatic colour's
/// while none is set) and opens the Tab Color page, which holds the wheel,
/// the brightness and the hex field.
fn link_tab_color_row(state: &SettingsViewState, field: &EditField, row: &mut UiNode) {
    let key = format!("settings/control/{}", field.key);
    let Some(control) = find_node(row, &key) else {
        return;
    };
    let saved = state
        .raw_value(field.key)
        .as_deref()
        .and_then(aterm_types::scheme::parse_hex)
        .map(|color| [color.r, color.g, color.b]);
    let shown = saved.map_or_else(|| field.placeholder.clone(), hex);
    let words = color_words(state, field);
    let focused = state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|focus| focus.as_str() == key);
    control.content = UiContent::Button(
        Control::new(
            ButtonSpec {
                label: format!("Choose the selected tab color; now {words}"),
                visual_label: Some(shown),
                visual_icon: None,
                trailing_icon: Some(ButtonIcon::Forward),
                description: Some(
                    "Opens Tab Color, with the color wheel, brightness and hex field".to_string(),
                ),
                badge: false,
                swatches: Vec::new(),
                chip: color_now(state, field.key),
                chevron_row: None,
            },
            ActionId::new(format!("settings/route{}", SettingsRoute::TabColor.path())),
        )
        .value(SemanticValue::Text(words))
        .state(ControlState {
            focused,
            ..ControlState::default()
        })
        .style(StyleRef::Setting),
    );
}

/// A colour row's Choose…: after its hex field (narrowed to what a hex
/// colour needs) and ahead of its Reset; it opens the picker under the row,
/// and closes it again.
fn add_color_chooser(
    state: &SettingsViewState,
    field: &EditField,
    row: &mut UiNode,
    (width, inline, text_width): (SettingsWidth, bool, f32),
) {
    const LABEL: &str = "Choose…";
    let key = field.key;
    let open = state.open_color_picker() == Some(key);
    let button_width = (crate::native_ui::native_button_label_width(LABEL) + 26.0).max(72.0);
    let opener = format!("settings/color-open/{key}");
    let noun = color_noun(field.label);
    let (label, visual) = if open {
        (format!("Done choosing the {noun}"), "Done")
    } else {
        (format!("Choose the {noun}"), LABEL)
    };
    let button = UiNode::new(
        opener.clone(),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(label).visual_label(visual),
                ActionId::new(opener),
            )
            .style(StyleRef::Secondary),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(button_width))
            .height(Length::Fill),
    );
    if width == SettingsWidth::Compact && !inline {
        let plain = crate::native_ui::plain_placeholder(&field.placeholder);
        let widest = plain
            .split_whitespace()
            .map(|word| crate::native_ui::text_field_width_for(word, true))
            .fold(0.0_f32, f32::max);
        let words = crate::native_ui::text_field_width_for("#DDDDDD", true).max(widest);
        if text_width - button_width - 6.0 >= words {
            if let Some(controls) = row.children.get_mut(1) {
                let at = controls.children.len().min(1);
                controls.children.insert(at, button);
            }
        } else {
            let mut button = button;
            let px = crate::native_ui::native_type_px(crate::type_scale::TypeStep::Secondary).get();
            let beside_chip = crate::native_ui::field_placeholder_room(text_width, true);
            let costs_words = unset_field_words(state, field).is_some_and(|words| {
                crate::native_ui::plain_placeholder(&words)
                    .split_whitespace()
                    .any(|word| {
                        crate::tray_raster::ui_text_width(word, px)
                            > beside_chip + crate::native_ui::TEXT_FIT_SLACK
                    })
            });
            if costs_words
                && let Some(control) = find_node(row, &format!("settings/control/{key}"))
                && let UiContent::TextField(field) = &mut control.content
            {
                field.spec.swatch_line = true;
            }
            let width = button_width.min(text_width);
            let mut lines = 1;
            if let UiContent::Button(control) = &mut button.content {
                let (words, count) = button_words_in(&control.spec, visual, &[], width);
                control.spec.visual_label = Some(words);
                lines = count;
            }
            let height =
                crate::native_ui::native_button_height_for_lines(lines, scaled_control_height());
            if let Length::Fixed(row_height) = row.layout.height {
                row.layout.height = Length::Fixed(row_height + 4.0 + height);
            }
            button.layout.width = Length::Fixed(width);
            row.children.push(
                UiNode::new(
                    format!("settings/color/{key}/chooser-line"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().height(Length::Fixed(height)))
                .children(vec![button]),
            );
        }
    } else {
        let field_width = 180.0 * settings_text_scale().min(1.25);
        if let Some(control) = row.children.get_mut(1) {
            control.layout.width = Length::Fixed(field_width);
        }
        let at = row.children.len().min(2);
        row.children.insert(at, button);
    }
}

/// The tab color page: the colour picker for the selected tab's colour as a page.
pub(super) fn tab_color_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    viewport: LogicalRect,
) -> Vec<UiNode> {
    let text_width = settings_group_text_width(viewport, width);
    let gap = settings_page_gap(state, width, viewport);
    let (mut out, used) = pinned_heading(
        state,
        width,
        viewport,
        gap,
        SettingsRoute::TabColor.label(),
        route_subtitle(SettingsRoute::TabColor),
    );
    let room =
        (settings_list_body_height(state, width, viewport) - used).max(scaled_control_height());
    let steady_room = (settings_steady_body_height(state, width, viewport) - used)
        .min(room)
        .max(scaled_control_height());
    let Some(subject) = ColorSubject::of(state, ColorTarget::Key(prefs::EDIT_ACTIVE_TAB_COLOR))
    else {
        return out;
    };
    let controls = |row_width: SettingsWidth, column_width: f32| {
        let mut rows = vec![color_brightness_row(
            state,
            &subject,
            row_width,
            column_width,
        )];
        rows.extend(page_setting_row(
            state,
            prefs::EDIT_ACTIVE_TAB_COLOR,
            row_width,
            column_width,
        ));
        rows
    };
    let card = |rows: Vec<(UiNode, f32)>| page_group_card("Color", "tab-color", rows, text_width);
    let chrome = {
        let empty = card(Vec::new());
        empty.height() + empty.gap
    };
    let refusal = if state
        .field_errors
        .contains_key(prefs::EDIT_ACTIVE_TAB_COLOR)
    {
        0.0
    } else {
        row_detail_line_height()
    };
    let beside = |chrome: f32| {
        let fit = |reserve: f32| {
            color_picker_beside(
                state,
                &subject,
                (width, SIDE_WHEEL_FLOOR, SettingsWidth::Compact),
                text_width,
                (steady_room - chrome, reserve),
                &controls,
            )
        };
        fit(refusal).or_else(|| fit(0.0))
    };
    let card = match beside(chrome) {
        Some(picker) => card(vec![picker]),
        None => match beside(2.0 * SETTINGS_GROUP_CARD_INSET) {
            Some(picker) => {
                let mut card = card(vec![picker]);
                card.head.clear();
                card
            }
            None => card(color_picker_stacked(
                state,
                &subject,
                (
                    width,
                    WHEEL_FLOOR.min(steady_room - 4.0).max(TIGHT_WHEEL_FLOOR),
                ),
                text_width,
                steady_room - chrome,
                &controls,
            )),
        },
    };
    out.push(scroll::list_page_peeking(
        state,
        "settings/tab-color/list",
        "Tab color",
        vec![ListItem::Split(card)],
        room,
        gap,
        settings_list_overhang(state, width, viewport),
    ));
    out
}

impl SettingsApp {
    /// Make `target`'s colour the one `hsv` gives and remember the exact hue,
    /// saturation and brightness that made it, even where the colour is the
    /// one already in force (the model still follows the slider).
    fn commit_color_hsv(
        &mut self,
        view: &mut SettingsViewState,
        target: ColorTarget,
        hsv: [f32; 3],
        cx: &mut UpdateCx<'_>,
    ) {
        if hsv.iter().any(|part| !part.is_finite()) {
            return;
        }
        let key = match target {
            ColorTarget::Key(key) => key,
            ColorTarget::Slot(_) => {
                let rgb = view.keep_color(target.kept(), hsv.map(|part| part.clamp(0.0, 1.0)));
                self.edit_palette_slot(view, &hex(rgb));
                cx.repaint(crate::native_app::DamageRegion::All);
                return;
            }
        };
        let hsv = hsv.map(|part| part.clamp(0.0, 1.0));
        let rgb = view.keep_color(key, hsv);
        self.commit_color_value(view, key, hex(rgb), cx);
    }

    /// Save `value` as `key`'s colour at once, replacing any draft in its
    /// row's hex field; nothing is written where the key already holds it.
    fn commit_color_value(
        &mut self,
        view: &mut SettingsViewState,
        key: &'static str,
        value: String,
        cx: &mut UpdateCx<'_>,
    ) {
        view.field_inputs.remove(key);
        view.field_errors.remove(key);
        let current = view.raw_value(key);
        if current
            .as_deref()
            .is_some_and(|current| current.trim().eq_ignore_ascii_case(&value))
        {
            cx.repaint(crate::native_app::DamageRegion::All);
            return;
        }
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits: vec![ConfigEdit {
                key: key.to_string(),
                expected: ExpectedConfigValue::Exact(current),
                value: Some(value),
            }],
        };
        if Self::submit_config(view, cx, patch).is_some() {
            view.feedback = Some("Applying…".to_string());
        }
        cx.repaint(crate::native_app::DamageRegion::All);
    }

    /// The trail colour's Cursor color ([`cursor_color_button`]): chosen,
    /// it saves the cursor's colour by its word; unchosen, the colour the
    /// cursor has now as the trail's own, so the trail looks the same and
    /// the wheel starts from it.
    fn commit_cursor_color(
        &mut self,
        view: &mut SettingsViewState,
        key: &'static str,
        wanted: Option<bool>,
        cx: &mut UpdateCx<'_>,
    ) {
        let switch = UiKey::new(cursor_color_button_key(key));
        view.common.last_focus = Some(switch.clone());
        view.reveal_control(switch);
        let on = trail_color_is_cursor(key, view.raw_value(key).as_deref());
        if wanted.unwrap_or(!on) == on {
            cx.repaint(crate::native_app::DamageRegion::All);
            return;
        }
        let value = if on {
            let Some(cursor) = color_now(view, prefs::EDIT_CURSOR_COLOR) else {
                return;
            };
            hex(cursor)
        } else {
            prefs::TRAIL_COLOR_CURSOR.to_string()
        };
        self.commit_color_value(view, key, value, cx);
    }

    /// Make `target`'s colour the one at `brightness`, its hue and saturation kept.
    pub(super) fn commit_color_brightness(
        &mut self,
        view: &mut SettingsViewState,
        target: ColorTarget,
        brightness: f64,
        cx: &mut UpdateCx<'_>,
    ) {
        let Some([hue, saturation, _]) = target_hsv(view, target) else {
            return;
        };
        if !brightness.is_finite() {
            return;
        }
        let value = crate::native_ui::snap_slider_value(brightness, 0.0, 1.0, BRIGHTNESS_STEP);
        self.commit_color_hsv(view, target, [hue, saturation, value as f32], cx);
    }

    /// The arrow keys on a focused wheel.
    pub(super) fn step_color_wheel(
        &mut self,
        view: &mut SettingsViewState,
        target: ColorTarget,
        hue: f32,
        saturation: f32,
        cx: &mut UpdateCx<'_>,
    ) {
        let [h, s, v] = target_hsv(view, target).unwrap_or([0.0, 0.0, 1.0]);
        let h = (h + hue * HUE_STEP).rem_euclid(1.0);
        let s = ((s + saturation * SATURATION_STEP) * 20.0).round() / 20.0;
        self.commit_color_hsv(view, target, [h, s.clamp(0.0, 1.0), v], cx);
        view.reveal_control(UiKey::new(format!("settings/color/{}/wheel", target.id())));
    }

    /// The colour picker's actions.
    pub(super) fn reduce_color_action(
        &mut self,
        view: &mut SettingsViewState,
        invocation: &ActionInvocation,
        cx: &mut UpdateCx<'_>,
    ) -> Option<EventResult> {
        let action = invocation.id.as_str();
        let (verb, id) = action.strip_prefix("settings/color-")?.split_once('/')?;
        let target = color_target(view, id)?;
        let wheel = UiKey::new(format!("settings/color/{}/wheel", target.id()));
        match (verb, target) {
            ("pick", _) => match (invocation.value.as_ref(), target) {
                (Some(SemanticInput::ColorPick { hue, saturation }), _) => {
                    let brightness = target_hsv(view, target).map_or(1.0, |[_, _, value]| value);
                    view.common.last_focus = Some(wheel.clone());
                    self.commit_color_hsv(view, target, [*hue, *saturation, brightness], cx);
                    view.reveal_control(wheel);
                }
                (Some(SemanticInput::Text(text)), ColorTarget::Slot(_)) => {
                    self.edit_palette_slot(view, text);
                    view.common.last_focus = Some(wheel.clone());
                    view.reveal_control(wheel);
                }
                (Some(SemanticInput::Text(text)), ColorTarget::Key(key)) => {
                    view.field_inputs.remove(key);
                    view.field_errors.remove(key);
                    let set = ActionInvocation {
                        id: ActionId::new(format!("settings/set/{key}")),
                        value: Some(SemanticInput::Text(text.clone())),
                    };
                    let _ = self.reduce_action(view, set, cx);
                    if !view.field_errors.contains_key(key) {
                        view.common.last_focus = Some(wheel.clone());
                        view.reveal_control(wheel);
                    }
                }
                (_, ColorTarget::Slot(_)) => {
                    let field = UiKey::new(super::showcase_page::SHOWCASE_PALETTE_SLOT);
                    view.common.last_focus = Some(field.clone());
                    view.reveal_control(field);
                }
                (_, ColorTarget::Key(key)) => {
                    let set = ActionInvocation {
                        id: ActionId::new(format!("settings/set/{key}")),
                        value: None,
                    };
                    return Some(self.reduce_action(view, set, cx));
                }
            },
            ("brightness", _) => {
                view.common.last_focus = Some(UiKey::new(format!(
                    "settings/color/{}/brightness",
                    target.id()
                )));
                if let Some(SemanticInput::Number(value)) = invocation.value.as_ref() {
                    self.commit_color_brightness(view, target, *value, cx);
                }
            }
            ("cursor", ColorTarget::Key(key)) if key == prefs::EDIT_CURSOR_TRAIL_COLOR => {
                let wanted = match invocation.value.as_ref() {
                    Some(SemanticInput::Bool(wanted)) => Some(*wanted),
                    _ => None,
                };
                self.commit_cursor_color(view, key, wanted, cx);
            }
            ("open", ColorTarget::Key(key)) => {
                if view.open_color_picker() == Some(key) {
                    view.close_color_picker();
                } else {
                    view.choice_picker = None;
                    view.color_picker = Some(key);
                    view.common.last_focus = Some(wheel.clone());
                    view.reveal_control(wheel);
                }
            }
            _ => return None,
        }
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        cx.repaint(crate::native_app::DamageRegion::All);
        Some(EventResult::Handled)
    }
}
