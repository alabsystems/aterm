// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS' COLOUR PICKER: one picker for every colour key.
//!
//! The picker is a colour wheel ([`crate::native_ui::UiContent::ColorWheel`]),
//! a Brightness slider and the key's ordinary hex row, the same three for
//! Foreground, Background, the cursor, selection and trail colours and the
//! selected tab's colour ([`ColorTarget::Key`]), and for the palette slot
//! Themes' Customize sheet has open ([`ColorTarget::Slot`]). Tab Color is that
//! picker as a page; on every other page a colour row's Choose… opens it under
//! the row ([`color_picker_node`]) and reads Done while it is open; Escape
//! closes it too. The open palette slot's editor always carries it.
//!
//! **One model, not the 8-bit hex.** The picker keeps the hue, saturation and
//! brightness each colour it made was made from
//! ([`SettingsViewState::color_kept`]). A saved `#RRGGBB` cannot hold them
//! (black has no hue, and near black a channel has a dozen levels), so a
//! colour taken to brightness 0 and back keeps its hue, a round trip through
//! a dim brightness gives the colour back exactly, and the wheel marks the
//! hue a black colour was picked at. A colour set any other way (typed,
//! Reset, aterm.toml, `settings set`) is read from its hex.
//!
//! **Every colour is a value.** An unset key's colour is the one the app
//! draws for it ([`unset_color_swatch`]): the theme's own, the cursor style's
//! own trail colour, the automatic selected-tab colour the window paints. The
//! row's chip, the wheel's marker and Brightness start from it.
//!
//! **Its own actions.** The wheel is `settings/color-pick/<key>` and
//! Brightness `settings/color-brightness/<key>`, so a pick keeps the wheel in
//! view (a change keeps the control it was made on in view), never the hex
//! row that `settings/set/<key>` names. A pointer pick carries the hue and
//! saturation under it ([`SemanticInput::ColorPick`]); a semantic
//! `#RRGGBB` saves through the hex row's own path, so a value that is not a
//! colour becomes the row's repairable draft. The arrow keys turn the hue
//! (Left, Right) and change the saturation (Up, Down) of a focused wheel.
//! The trail colour's picker also holds Cursor color
//! (`settings/color-cursor/<key>`), the choice of the one value beside a
//! colour that key takes: the cursor's colour ([`prefs::TRAIL_COLOR_CURSOR`]).

use super::*;
use crate::native_ui::ColorWheelSpec;

/// A colour the picker made, with the exact hue, saturation and brightness
/// that made it ([`SettingsViewState::color_kept`]).
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

/// The node keys' and actions' name for the palette slot Themes' Customize
/// sheet has open (`settings/color/palette-slot/wheel`).
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

/// WHAT A PICKER PICKS FOR: a colour key, saved through its row's own path
/// as each pick is made, or the palette slot Themes' Customize sheet has
/// open, whose editor holds the pick as its draft until it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColorTarget {
    Key(&'static str),
    Slot(usize),
}

impl ColorTarget {
    /// The node keys' and actions' name: the key, or [`PALETTE_SLOT_ID`].
    fn id(self) -> &'static str {
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

/// The target a picker's node keys and actions name `id`: a colour key, or
/// the open palette slot.
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

    /// The colour key whose picker is drawn under its row: none while a
    /// choice picker stands in front of the page.
    pub(super) fn open_color_picker(&self) -> Option<&'static str> {
        self.color_picker.filter(|_| self.choice_picker.is_none())
    }

    /// Close the colour picker, the keyboard back on the Choose… that opened
    /// it. Whether one was open.
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

/// THE COLOUR AN UNSET COLOUR KEY DRAWS, the one its placeholder names: the
/// theme's own foreground, background, cursor or selection, or its own
/// selected-text colour; the saved cursor style's own trail colour (Laser's
/// violet), and the accent brighter than what a window brightens (that
/// colour, or the trail colour chosen); the automatic selected-tab colour
/// the window paints. The row's chip shows it and the picker starts from it.
///
/// `None` where no single colour is true to the default: a trail colour that
/// follows the cursor colour (which a program may recolour), or a selected
/// text colour the renderer picks for contrast.
pub(super) fn unset_color_swatch(state: &SettingsViewState, key: &str) -> Option<[u8; 3]> {
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    match key {
        prefs::EDIT_CURSOR_TRAIL_COLOR => {
            let style = crate::app_config::resolve_trail_style(&saved_cursor_style(state)).style?;
            // The colour law the row's placeholder names
            // (`prefs::editable_fields`): a saved cursor colour pins Beam,
            // Comet and Sparkle to it.
            let pinned = state.is_explicit(prefs::EDIT_CURSOR_COLOR);
            let (color, _) = aterm_effects::cursor_fx::own_trail_color(style, pinned)?;
            Some(rgb(color))
        }
        // The accent brightens what a window brightens: the style's own
        // colour (under the same law), or the trail colour chosen where the
        // style draws it.
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

/// The colour Modified names a colour row's default by: the colour the key
/// draws while unset ([`unset_color_swatch`]), for a key whose default
/// Modified names ("Default: Automatic").
pub(super) fn default_color_chip(state: &SettingsViewState, field: &EditField) -> Option<[u8; 3]> {
    if !matches!(field.kind, EditKind::Color) || default_value_label(field.key).is_none() {
        return None;
    }
    unset_color_swatch(state, field.key)
}

/// The colour `key` draws now: its saved value, else its default
/// ([`unset_color_swatch`]); a saved value that is not a colour draws the
/// default too. `None` while no single colour is known.
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
/// where it made it, else the colour's own. `None` while no colour is known.
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
/// percent, its hue and saturation kept. Off while no colour is known.
fn color_brightness_node(state: &SettingsViewState, subject: &ColorSubject) -> UiNode {
    let id = subject.target.id();
    let hsv = target_hsv(state, subject.target);
    // Whole percent, the readout's grain, so the slider's value carries no
    // float noise (#CCCCCC is 0.8, not 0.800000011920929).
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
/// registry row is at `width`. In the trail colour's picker its Cursor color
/// ([`cursor_color_button`]) ends the row, so the picker is no taller for it
/// and fits every list Brightness alone fits (80x24's Adjust card holds one
/// row beside the wheel); where the slider beside it would be squeezed below
/// its least, the button takes a line of its own under the slider.
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
    // The room the row gives its control: beside its label, or the column.
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
/// of its own. So the word is a choice in the picker, not only something to
/// type. `None` for every other target: no other colour takes the word.
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
    // The label, the check it wears while chosen, and the button's insets.
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
/// `text_width` wide and at most `room` tall. `None` where the column would
/// be too narrow or the row taller than the room.
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
    // `reserve` stays free under the column, inside the room.
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

/// `row` (a colour row, `key`'s) with its open picker under it, as one stop
/// of the list, so the hex field and the wheel it sets are on one screen
/// together; the row alone while its picker is closed. `(room, most)`: the
/// stop is at most `room` tall (the list's screen with the page's preview at
/// its least) where the picker fits there, else at most `most` (the screen
/// the preview yields to a taller stop).
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

/// THE PICKER UNDER A COLOUR ROW, where its Choose… opened it: the wheel and
/// Brightness, beside each other where the column has room, else stacked, at
/// most `room` tall. The row above it holds the hex field, and its Choose…
/// reads Done while the picker is open.
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

/// THE PALETTE SLOT'S PICKER, under the open slot's hex field on Themes'
/// Customize sheet: the wheel over Brightness, in a column `column_width`
/// wide, at most `room` tall. A pick is the slot's draft, as a typed hex is.
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
    // The stack's padding is on every side: its rows have the card's width
    // less both insets.
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
    // Beside its controls where the column has room; else stacked where that
    // fits; else, in a list too short for either (80x24's Customize sheet,
    // a single row's height), a smaller wheel beside them with Brightness's
    // label leading its slider, so the picker is one whole stop there too.
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
    // ONE STOP: the wheel and Brightness are never listed apart (a list
    // lists a column of fixed rows row by row), so a list that brings the
    // wheel into view shows the Brightness it goes with.
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

/// Whether the selected tab color's row is only a way to Tab Color: on the
/// Window page and every listing but Modified, Search and Tab Color itself,
/// while it holds nothing of its own to edit. A held draft (a refused value
/// among them) keeps the field, so the row can show and take the correction
/// its tag and reason ask for.
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

/// Whether a colour row offers Choose…, the picker under the row: wherever
/// it is listed (Modified too) but Tab Color, whose page is the picker, and
/// never where the row is a way to Tab Color ([`tab_color_row_links`]).
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

/// THE CONTROL A ROW TAKES where a page owns its key, applied to a finished
/// row's control: the selected tab color's row on the Window page becomes a
/// way to Tab Color ([`link_tab_color_row`]); a colour row elsewhere gains
/// Choose… ([`add_color_chooser`]); Modified's wallpaper row names the image
/// as the Wallpaper page does and goes there ([`link_wallpaper_row`]).
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
        link_wallpaper_row(state, field, row, text_width);
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
/// opens the Wallpaper page, which chooses and removes it. The row's Reset
/// still removes it.
fn link_wallpaper_row(state: &SettingsViewState, field: &EditField, row: &mut UiNode, room: f32) {
    let key = format!("settings/control/{}", field.key);
    let Some((visual, spoken)) = wallpaper_words(state) else {
        return;
    };
    let Some(control) = find_node(row, &key) else {
        return;
    };
    let width = match control.layout.width {
        Length::Fixed(width) => width,
        _ => room,
    };
    // The label's room: the button's insets and its forward arrow.
    let shown = crate::native_ui::fit_native_button_label(&visual, (width - 48.0).max(0.0));
    let path = state.raw_value(field.key).unwrap_or_default();
    let focused = state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|focus| focus.as_str() == key);
    control.content = UiContent::Button(
        Control::new(
            ButtonSpec {
                // Named by the words it paints first (the file's name and
                // size), then the row and the whole image.
                label: format!("{visual}: Wallpaper image {spoken}; opens Wallpaper"),
                visual_label: Some(shown),
                visual_icon: None,
                trailing_icon: Some(ButtonIcon::Forward),
                description: Some("Opens Wallpaper, to choose or remove the image".to_string()),
                badge: false,
                swatches: Vec::new(),
                chip: None,
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

/// The keys a row's page-owned control adds to the keyboard order after its
/// field ([`page_owned_control_form`]): Choose….
pub(super) fn page_owned_control_keys(
    state: &SettingsViewState,
    field: &EditField,
    modified_only: bool,
) -> Option<UiKey> {
    offers_color_chooser(state, field, modified_only)
        .then(|| UiKey::new(format!("settings/color-open/{}", field.key)))
}

/// The keys an open picker under `key`'s row adds to the keyboard order,
/// after the row's own: the wheel, then Brightness, then the trail colour's
/// Cursor color ([`cursor_color_button`]).
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
    // Unset (or not a colour, which the window draws as unset): the words
    // the Tab Color page's hex field shows, and the colour they name.
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
        // The controls row under the label: the field fills and the button
        // follows it. Where that would squeeze the field narrower than a
        // written colour and its swatch (large type on a phone), the button
        // takes a line of its own under the field. Measured on what the
        // field must show whole whatever is written — "#RRGGBB" (an unset
        // field fits its placeholder to what is left) — so writing or
        // clearing the colour never moves the button or the row's height.
        // So must its placeholder's least whole words ("Theme…" of "Theme
        // (#282A36)"): beside a Choose… that left it less, it was cut inside
        // its first word ("Them…").
        let least = field
            .placeholder
            .split_whitespace()
            .next()
            .map(|first| {
                if first.len() < field.placeholder.trim().len() {
                    format!("{first}\u{2026}")
                } else {
                    first.to_string()
                }
            })
            .unwrap_or_default();
        let words = crate::native_ui::text_field_width_for("#DDDDDD", true)
            .max(crate::native_ui::text_field_width_for(&least, true));
        if text_width - button_width - 6.0 >= words {
            if let Some(controls) = row.children.get_mut(1) {
                let at = controls.children.len().min(1);
                controls.children.insert(at, button);
            }
        } else {
            // On a line of its own the button is never wider than the column,
            // and its words wrap to it, never cut ("Choos…" at 2× text in
            // a 22-column window).
            let mut button = button;
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

/// THE TAB COLOR PAGE: the colour picker for the selected tab's colour as a
/// page. The wheel beside (or over) Brightness and the ordinary hex row for
/// the key (the keyboard, VoiceOver and automation path, the same control
/// every colour row uses) with its Reset. The card is split between its rows
/// where it is taller than the room, never clipped.
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
    // THE WHEEL NEVER JUMPS: the picker is sized against the room the page
    // has with the Undo bar a pick brings up showing, whether or not it is
    // ([`settings_steady_body_height`]), so the first pick on a short window
    // (80x24, which keeps no room for the bar) neither shrinks the wheel nor
    // moves it from under the pointer. The list still gives up the bar's
    // room itself.
    let steady_room = (settings_steady_body_height(state, width, viewport) - used)
        .min(room)
        .max(scaled_control_height());
    let Some(subject) = ColorSubject::of(state, ColorTarget::Key(prefs::EDIT_ACTIVE_TAB_COLOR))
    else {
        return out;
    };
    // Brightness and the hex field, laid out for a column so wide. The hex
    // field says "Automatic (default)" while none is set.
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
    // THE PICKER: the wheel beside the controls it sets, one stop of the
    // list, so the wheel, the brightness and the hex value it makes are on
    // one screen together, at the shipped default size as well, with or
    // without the Undo bar. The column lays its labels over their controls
    // where it is narrow.
    // The card less its one row: padding, caption, and the gap after it.
    let chrome = {
        let empty = card(Vec::new());
        empty.height() + empty.gap
    };
    // A refused hex value says why under its row. Where the picker has room
    // for that line too, it is laid out keeping it, so a refusal moves
    // nothing; where it has none (80x24), as it is.
    let refusal = if state
        .field_errors
        .contains_key(prefs::EDIT_ACTIVE_TAB_COLOR)
    {
        0.0
    } else {
        row_detail_line_height()
    };
    // Where the card's COLOR caption is what keeps the picker off one screen
    // (80x24, sized as with the Undo bar up), the caption yields: the pinned
    // "Tab Color" heading names the card, and a pick on the wheel never
    // re-lays the page out and scrolls the wheel away from the pointer.
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
            // Too narrow or too short for both: the wheel over the
            // controls, yielding height toward its floor before the card
            // would need to scroll, and split between its rows where it
            // still does not fit — the wheel's row no taller than the list
            // itself, so a screen can show it whole: its 120pt floor in the
            // 100pt list of a 102x12 window at 2x text was cut on every
            // screen.
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
    /// one already in force (the model still follows the slider). A key
    /// saves at once, the pick replacing any draft in its row's hex field; the
    /// open palette slot takes it as its editor's draft, which the demo draws
    /// and the editor saves when it closes.
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
    /// the wheel starts from it. `wanted` is the state asked for; `None`
    /// turns it over, as a press does.
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

    /// Make `target`'s colour the one at `brightness`, its hue and saturation
    /// kept. Nothing to brighten while no colour is known.
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

    /// The arrow keys on a focused wheel: `hue` turns (a 72nd each, Shift a
    /// twelfth), `saturation` steps (a twentieth each), the brightness kept.
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

    /// The colour picker's actions: a pick on the wheel, Brightness, the
    /// trail colour's Cursor color, and Choose… (Done while open). `None` for
    /// any other action.
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
            // A pick keeps the wheel in view and the keyboard on it: the
            // control it was made on, never the hex row.
            ("pick", _) => match (invocation.value.as_ref(), target) {
                (Some(SemanticInput::ColorPick { hue, saturation }), _) => {
                    let brightness = target_hsv(view, target).map_or(1.0, |[_, _, value]| value);
                    view.common.last_focus = Some(wheel.clone());
                    self.commit_color_hsv(view, target, [*hue, *saturation, brightness], cx);
                    view.reveal_control(wheel);
                }
                // The palette slot's wheel takes a semantic colour as its
                // field takes a typed one: the editor's draft.
                (Some(SemanticInput::Text(text)), ColorTarget::Slot(_)) => {
                    self.edit_palette_slot(view, text);
                    view.common.last_focus = Some(wheel.clone());
                    view.reveal_control(wheel);
                }
                (Some(SemanticInput::Text(text)), ColorTarget::Key(key)) => {
                    // A semantic colour saves through the hex row's own path,
                    // replacing its draft; one that is not a colour becomes
                    // the row's repairable draft, the keyboard on it.
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
                // Pressed with no value: the hex field takes the keyboard.
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
