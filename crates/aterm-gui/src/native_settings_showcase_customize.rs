// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes' CUSTOMIZE SHEET: it stands in the chooser's place while open.

use super::*;

/// A sheet caption's height.
pub(super) fn caption_height() -> f32 {
    24.0_f32.max(18.0 * settings_text_scale())
}

/// A sheet section's caption over its first row.
pub(super) fn with_heading(
    key: &str,
    heading: &str,
    block: (UiNode, f32),
    width: f32,
) -> (UiNode, f32) {
    let heading_height = caption_height();
    let height = heading_height + 4.0 + block.1;
    (
        UiNode::new(
            format!("{key}/block"),
            UiContent::Group(GroupSpec::new(heading.to_lowercase())),
        )
        .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
        .children(vec![
            line(
                format!("{key}/heading"),
                heading,
                SemanticRole::Heading,
                StyleRef::Quiet,
                width,
                heading_height,
            ),
            block.0,
        ]),
        height,
    )
}

/// A sheet section's caption over all its rows, as one column of fixed children (the shape a list splits row by row, its caption the head every screen of it repeats).
pub(super) fn with_heading_rows(
    key: &str,
    heading: &str,
    rows: Vec<(UiNode, f32)>,
    width: f32,
) -> (UiNode, f32) {
    let heading_height = caption_height();
    let height = heading_height + rows.iter().map(|(_, height)| 4.0 + height).sum::<f32>();
    (
        UiNode::new(
            format!("{key}/block"),
            UiContent::Group(GroupSpec::new(heading.to_lowercase())),
        )
        .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
        .children(
            std::iter::once(line(
                format!("{key}/heading"),
                heading,
                SemanticRole::Heading,
                StyleRef::Quiet,
                width,
                heading_height,
            ))
            .chain(rows.into_iter().map(|(row, _)| row))
            .collect(),
        ),
        height,
    )
}

/// The height of every row of the sheet: a colour row, the palette's editor and tools, Save as theme.
pub(super) fn sheet_row_height() -> f32 {
    32.0_f32.max(28.0 * settings_text_scale())
}

/// A colour row's field at its widest and its least: the least still shows
/// the widest thing it holds whole (an upper-case hex such as `#DDDDDD`, or
/// "Automatic", the selected text's default), measured at the type size in
/// force, beside the colour's chip at its end, which is the well that opens
/// the picker ([`with_colour_well`]).
fn colour_field_widths() -> (f32, f32) {
    let chip = crate::native_ui::TEXT_FIELD_SWATCH_SIZE + crate::native_ui::TEXT_FIELD_SWATCH_GAP;
    let empty_well =
        crate::native_ui::TEXT_FIELD_END_BUTTON + crate::native_ui::TEXT_FIELD_END_BUTTON_GAP;
    let least = (crate::native_ui::text_field_width_for_text("#DDDDDD") + chip)
        .max(crate::native_ui::text_field_width_for_text("Automatic") + empty_well)
        .ceil()
        + 2.0;
    (least, least + 12.0 * settings_text_scale().clamp(1.0, 1.25))
}

/// A colour row's Reset, an icon button as wide as a Yours row's.
const RESET_ICON_WIDTH: f32 = 20.0;
/// The gap between a colour row's name and its field, and before Reset.
const COLOUR_ROW_GAP: f32 = 6.0;

/// THE COLOUR WELL: the chip at a colour row's field's end (the colour in
/// force, [`crate::native_ui::TextFieldSpec::swatch`]) made a button laid
/// over it, the same size, so it costs the row's name no width.
fn with_colour_well(
    state: &SettingsViewState,
    key: &str,
    name: &str,
    field: UiNode,
    (field_width, height): (f32, f32),
    shown: Option<[u8; 3]>,
) -> UiNode {
    let opener = format!("settings/color-open/{key}");
    let open = state.open_color_picker() == Some(key);
    let lower = name.to_lowercase();
    let label = if open {
        format!("Done choosing the {lower} color")
    } else {
        format!("Choose the {lower} color")
    };
    let mut spec = ButtonSpec::new(label).visual_label("");
    spec.chip = shown;
    if shown.is_none() {
        spec = spec.visual_icon(ButtonIcon::Themes);
    }
    let size = crate::native_ui::TEXT_FIELD_SWATCH_SIZE;
    let well = UiNode::new(
        opener.clone(),
        UiContent::Button(
            Control::new(spec, ActionId::new(opener.clone()))
                .state(ControlState {
                    selected: open,
                    focused: focused(state, &opener),
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(size))
            .height(Length::Fill),
    );
    let pad = ((height - size) / 2.0).max(0.0);
    let lead = UiNode::new(
        format!("settings/showcase/well/{key}/lead"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill))
    .paint_only();
    let end = UiNode::new(
        format!("settings/showcase/well/{key}/end"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::row()
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Insets {
                top: pad,
                right: crate::native_ui::TEXT_FIELD_HORIZONTAL_PADDING,
                bottom: pad,
                left: 0.0,
            }),
    )
    .children(vec![lead, well]);
    let mut field = field;
    field.layout.width = Length::Fill;
    UiNode::new(
        format!("settings/showcase/well/{key}"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(field_width))
            .height(Length::Fixed(height)),
    )
    .children(vec![field, end])
}

/// The width the widest colour row name paints at.
fn colour_label_width() -> f32 {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
    SHOWCASE_COLOR_KEYS
        .iter()
        .filter_map(|key| colour_row_label(key))
        .map(|name| crate::tray_raster::ui_text_width_for(face, name, px))
        .fold(0.0, f32::max)
        + 1.0
}

/// The least column the colour rows hold on one line in ([`customize_row`]): the widest name, the field at its least, and Reset's place.
pub(super) fn colour_rows_least_column() -> f32 {
    colour_label_width()
        + COLOUR_ROW_GAP
        + colour_field_widths().0
        + RESET_ICON_WIDTH
        + COLOUR_ROW_GAP
}

/// The line a colour row's name takes over its field where the two do not
/// share one ([`customize_row`]): a form's label, in the caption type
/// ([`COLOUR_NAME_TYPE`]).
fn colour_name_height() -> f32 {
    14.0_f32.max(13.0 * settings_text_scale())
}

/// The type a colour row's name takes over its field: the caption step, in
/// the secondary ink of a form's label.
const COLOUR_NAME_TYPE: (SemanticRole, StyleRef) = (SemanticRole::Status, StyleRef::Plain);

/// The gap between a colour row's name and the field under it.
const COLOUR_NAME_GAP: f32 = 2.0;

/// Whether the colour rows of a sheet whose rows take `column_width` while its list scrolls share one line each ([`colour_rows_least_column`]).
pub(super) fn colour_rows_on_one_line(column_width: f32) -> bool {
    column_width + 0.01 >= colour_rows_least_column()
}

/// A colour row's height in the form `one_line` says: one line where the
/// name, the field at its least and Reset share it, else the name's line
/// over them.
fn colour_row_height(one_line: bool) -> f32 {
    if one_line {
        sheet_row_height()
    } else {
        colour_name_height() + COLOUR_NAME_GAP + sheet_row_height()
    }
}

/// A colour row's field width on one line in a column `column_width` wide:
/// what the names and Reset's place leave, between the least and the
/// widest.
fn colour_field_width(column_width: f32) -> f32 {
    let (least, widest) = colour_field_widths();
    (column_width - colour_label_width() - COLOUR_ROW_GAP - RESET_ICON_WIDTH - COLOUR_ROW_GAP)
        .clamp(least, widest)
}

/// The word a colour row shows for `key`: what the colour paints, short
/// enough that the name, the field and Reset share one line in the
/// chooser's column.
fn colour_row_label(key: &str) -> Option<&'static str> {
    Some(match key {
        prefs::EDIT_FOREGROUND => "Foreground",
        prefs::EDIT_BACKGROUND => "Background",
        prefs::EDIT_CURSOR_COLOR => "Cursor",
        prefs::EDIT_SELECTION_COLOR => "Selection",
        prefs::EDIT_SELECTION_FOREGROUND => "Selected text",
        _ => return None,
    })
}

/// One colour override of the Customize sheet on one line: its name, the registry's own field (the draft, patch and undo lane of every Settings field) showing the colour in force, and Reset's place, its icon there while the person sets the colour, so the fields stay in one column.
pub(super) fn customize_row(
    state: &SettingsViewState,
    key: &str,
    column_width: f32,
    one_line: bool,
) -> Option<(UiNode, f32)> {
    let index = state.field_index_of(key)?;
    let field = state.legacy.fields.get(index)?;
    let name = colour_row_label(key)?;
    let field_width = if one_line {
        colour_field_width(column_width)
    } else {
        (column_width - RESET_ICON_WIDTH - COLOUR_ROW_GAP).max(0.0)
    };
    let generic = finish_setting_row_narrowed(
        state,
        index,
        field,
        SettingsWidth::Medium,
        column_width,
        None,
        field_width,
    );
    let control_key = format!("settings/control/{key}");
    let reset_key = format!("settings/reset/{key}");
    let mut control = find_node(&generic, &control_key)?.clone();
    let explicit = state.is_explicit(key);
    let editing = state.editing_field.as_deref() == Some(key);
    let shown = theme_colour_of(state, key);
    let well_color = match &control.content {
        UiContent::TextField(text) => text.spec.swatch.or(shown),
        _ => shown,
    };
    if let UiContent::TextField(text) = &mut control.content {
        text.spec.label = format!("{name} color");
        text.spec.swatch = well_color;
        text.spec.end_button = well_color.is_none();
        if !explicit {
            if !editing {
                text.spec.visual_value = Some(String::new());
            }
            if let Some([r, g, b]) = shown {
                text.spec.placeholder = Some(format!("#{r:02x}{g:02x}{b:02x}"));
            } else if let (false, SemanticValue::Text(words)) = (editing, &text.value) {
                text.spec.placeholder = Some(words.clone());
            }
        }
    }
    let height = sheet_row_height();
    control.layout = Layout::default()
        .width(Length::Fixed(field_width))
        .height(Length::Fixed(height));
    let mut reset = find_node(&generic, &reset_key).cloned();
    if let Some(node) = reset.as_mut() {
        if let UiContent::Button(button) = &mut node.content {
            button.state.enabled = button.state.enabled && explicit;
            button.spec = ButtonSpec::new(format!("Reset {name} to the theme's color"))
                .visual_label("")
                .visual_icon(ButtonIcon::Reset);
            button.style = StyleRef::Quiet;
        }
        node.layout = Layout::default()
            .width(Length::Fixed(RESET_ICON_WIDTH))
            .height(Length::Fill);
    }
    let gap = COLOUR_ROW_GAP;
    let tail = RESET_ICON_WIDTH + gap;
    let (role, style) = if one_line {
        (SemanticRole::Text, StyleRef::Primary)
    } else {
        COLOUR_NAME_TYPE
    };
    let label_width = if one_line {
        column_width - field_width - gap - tail
    } else {
        column_width
    }
    .max(0.0);
    let label = UiNode::new(
        format!("settings/label/{key}"),
        UiContent::Text(TextSpec {
            text: fit_text(name, role, style, label_width),
            role,
            style,
        }),
    )
    .layout(Layout::default().width(Length::Fill).height(Length::Fill))
    .paint_only();
    let well = with_colour_well(state, key, name, control, (field_width, height), well_color);
    let (mut children, label) = if one_line {
        (vec![label, well], None)
    } else {
        (vec![well], Some(label))
    };
    let pad = ((height - RESET_ICON_WIDTH) / 2.0).max(0.0);
    let slot = reset.unwrap_or_else(|| {
        UiNode::new(
            format!("settings/reset-slot/{key}"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(RESET_ICON_WIDTH))
                .height(Length::Fill),
        )
        .paint_only()
    });
    children.push(
        UiNode::new(
            format!("settings/reset-frame/{key}"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .width(Length::Fixed(RESET_ICON_WIDTH))
                .height(Length::Fill)
                .padding(Insets {
                    top: pad,
                    right: 0.0,
                    bottom: pad,
                    left: 0.0,
                }),
        )
        .children(vec![slot]),
    );
    let row = UiNode::new(
        format!("settings/row/{index}/{key}"),
        UiContent::Group(GroupSpec::new(format!("{name} color"))),
    );
    let (row, height) = match label {
        None => {
            let row = row
                .layout(
                    Layout::row()
                        .width(Length::Fill)
                        .height(Length::Fixed(height))
                        .gap(gap),
                )
                .children(children);
            (row, height)
        }
        Some(mut label) => {
            label.layout.height = Length::Fixed(colour_name_height());
            let controls = UiNode::new(
                format!("settings/showcase/colour-controls/{key}"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::row()
                    .width(Length::Fill)
                    .height(Length::Fixed(height))
                    .gap(gap),
            )
            .children(children);
            let total = colour_name_height() + COLOUR_NAME_GAP + height;
            let row = row
                .layout(
                    Layout::column()
                        .width(Length::Fill)
                        .height(Length::Fixed(total))
                        .gap(COLOUR_NAME_GAP),
                )
                .children(vec![label, controls]);
            (row, total)
        }
    };
    let mut out = vec![(row, height)];
    if let Some(error) = state.field_errors.get(key) {
        let note = caption_height();
        out.push((
            line(
                format!("settings/note/{key}"),
                error,
                SemanticRole::Status,
                StyleRef::Danger,
                column_width,
                note,
            ),
            note,
        ));
    }
    let height = out.iter().map(|(_, height)| height).sum::<f32>();
    if out.len() == 1 {
        return out.pop();
    }
    Some((
        UiNode::new(
            format!("settings/colour-row/{key}"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().height(Length::Fixed(height)))
        .children(out.into_iter().map(|(node, _)| node).collect()),
        height,
    ))
}

/// The node keyed `key` under `node`.
fn find_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.key.as_str() == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, key))
}

/// The colour the theme shown draws for the colour row `key`, overrides
/// included; `None` for a row that is not a colour, or a selection
/// foreground left to the contrast floor.
fn theme_colour_of(state: &SettingsViewState, key: &str) -> Option<[u8; 3]> {
    let resolved = state.showcase_colors();
    let theme = resolved.theme();
    let packed = |rgb: u32| [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
    match key {
        prefs::EDIT_FOREGROUND => Some(packed(theme.fg)),
        prefs::EDIT_BACKGROUND => Some(packed(theme.bg)),
        prefs::EDIT_CURSOR_COLOR => Some(packed(theme.cursor)),
        prefs::EDIT_SELECTION_COLOR => Some(packed(theme.selection)),
        prefs::EDIT_SELECTION_FOREGROUND => resolved
            .selection_fg()
            .map(|color| [color.r, color.g, color.b]),
        _ => None,
    }
}

/// What the palette's tools row says while no slot is open.
const PALETTE_HINT: &str = "Choose a color to change it";

/// The least pitch, centre to centre, of the palette's chips along a row: a
/// pointer's target, as the 24pt of WCAG 2.5.8 takes it.
const CHIP_PITCH_LEAST: f32 = 24.0;

/// The gap between two chips of a row.
const CHIP_GAP: f32 = 6.0;

/// How many chips a row of the palette holds in a column `column_width`
/// wide: the eight of a half (normal, bright) where they keep
/// [`CHIP_PITCH_LEAST`], else four, a half over two rows.
fn chips_per_row(column_width: f32) -> usize {
    if (column_width + CHIP_GAP) / 8.0 + 0.01 >= CHIP_PITCH_LEAST {
        8
    } else {
        4
    }
}

/// The palette: sixteen chips (normal over bright, eight to a row, or four where eight would stand closer than [`CHIP_PITCH_LEAST`]), the open slot's editor, and Reset palette.
pub(super) fn palette_blocks(
    state: &SettingsViewState,
    column_width: f32,
    room: f32,
) -> Vec<(UiNode, f32)> {
    let colors = state.showcase_colors().scheme.ansi;
    let chip_height = 24.0_f32.max(22.0 * settings_text_scale());
    let per_row = chips_per_row(column_width);
    let chip_row = |row: usize| {
        let chips: Vec<UiNode> = (row * per_row..(row + 1) * per_row)
            .map(|index| {
                let key = format!("{PALETTE_PREFIX}{index}");
                let hex = hex_of(colors[index]);
                let changed = state.slot_on_list(index)
                    || state
                        .raw_values
                        .contains_key(&format!("{}.{index}", prefs::EDIT_PALETTE_OVERRIDES));
                let selected = state
                    .showcase
                    .slot
                    .as_ref()
                    .is_some_and(|slot| slot.index == index);
                let hit = UiNode::new(
                    key.clone(),
                    UiContent::Button(
                        Control::new(
                            ButtonSpec::new(format!(
                                "Palette color {index}, {}: {hex}{}",
                                SLOT_NAMES[index],
                                if changed { ", changed" } else { "" }
                            ))
                            .visual_label(""),
                            ActionId::new(key.clone()),
                        )
                        .value(SemanticValue::Text(hex.clone()))
                        .state(ControlState {
                            selected,
                            focused: focused(state, &key),
                            ..ControlState::default()
                        })
                        .style(if selected {
                            StyleRef::Navigation
                        } else {
                            StyleRef::Secondary
                        }),
                    ),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill));
                let swatch = UiNode::new(
                    format!("{key}/swatch"),
                    UiContent::Custom(AuditedCustomNode {
                        audit_id: crate::native_ui::THEME_SWATCH_AUDIT,
                        role: SemanticRole::Group,
                        label: String::new(),
                        value: Some(if changed {
                            format!("{hex} changed")
                        } else {
                            hex
                        }),
                        action: None,
                        focusable: false,
                        focused: false,
                        focus_visible: false,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill))
                .paint_only();
                let face = UiNode::new(
                    format!("{key}/face"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .padding(Insets::all(4.0)),
                )
                .children(vec![swatch])
                .paint_only();
                UiNode::new(
                    format!("{key}/frame"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill))
                .children(vec![hit, face])
            })
            .collect();
        UiNode::new(
            format!("settings/showcase/palette-row/{row}"),
            UiContent::Group(GroupSpec::new(if row * per_row < 8 {
                "Normal colors"
            } else {
                "Bright colors"
            })),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(chip_height))
                .gap(CHIP_GAP),
        )
        .children(chips)
    };
    let rows = 16 / per_row;
    let mut controls = Vec::new();
    let mut field = None;
    if let Some(SlotEditor { index, input, .. }) = state.showcase.slot.as_ref() {
        let swatch = aterm_types::scheme::parse_hex(input.value().trim())
            .map(|color| [color.r, color.g, color.b]);
        field = Some(text_field(
            state,
            SHOWCASE_PALETTE_SLOT,
            &format!("Palette color {index}, {}", SLOT_NAMES[*index]),
            "#rrggbb",
            input,
            swatch,
            swatch.is_none(),
        ));
        controls.push(button(
            PALETTE_SLOT_RESET,
            &format!(
                "Reset color {index}, {}, to the theme's",
                SLOT_NAMES[*index]
            ),
            "Reset color",
            StyleRef::Quiet,
            ControlState::default(),
            Some(hug_width("Reset color")),
        ));
        controls.push(button(
            PALETTE_SLOT_CLOSE,
            "Close the palette color editor",
            "Close",
            StyleRef::Quiet,
            ControlState::default(),
            Some(hug_width("Close")),
        ));
    } else {
        let room = column_width - hug_width("Reset palette") - 6.0;
        let fitted = fit_text(PALETTE_HINT, SemanticRole::Text, StyleRef::Quiet, room);
        let hint = UiNode::new(
            "settings/showcase/palette-hint",
            if fitted == PALETTE_HINT {
                UiContent::Text(TextSpec {
                    text: fitted,
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                })
            } else {
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group))
            },
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        controls.push(hint);
    }
    let reset_all = button(
        PALETTE_RESET_ALL,
        "Reset palette to the theme's colors",
        "Reset palette",
        StyleRef::Quiet,
        ControlState {
            enabled: (state.is_explicit(prefs::EDIT_PALETTE)
                || state.is_explicit(prefs::EDIT_PALETTE_OVERRIDES)
                || state.field_has_unsaved_draft(prefs::EDIT_PALETTE)),
            focused: focused(state, PALETTE_RESET_ALL),
            ..ControlState::default()
        },
        Some(hug_width("Reset palette")),
    );
    let fixed: f32 = controls
        .iter()
        .chain([&reset_all])
        .map(|node| match node.layout.width {
            Length::Fixed(width) => width + 6.0,
            _ => 0.0,
        })
        .sum::<f32>()
        - 6.0;
    let own_row = field.is_some() && fixed > column_width;
    let mut reset_row = None;
    if own_row {
        let spacer = UiNode::new(
            "settings/showcase/palette-reset-row/spacer",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        reset_row = Some(control_row(
            "settings/showcase/palette-reset-row",
            "Reset palette",
            vec![spacer, reset_all],
        ));
    } else {
        controls.push(reset_all);
    }
    let (tools, tools_height) =
        control_row("settings/showcase/palette-tools", "Palette color", controls);
    let mut blocks = Vec::new();
    match field {
        None => blocks.push(with_heading_rows(
            "settings/showcase/palette-section",
            "PALETTE",
            (0..rows)
                .map(|row| (chip_row(row), chip_height))
                .chain([(tools, tools_height)])
                .collect(),
            column_width,
        )),
        Some(field) if rows > 2 => {
            let open = state
                .showcase
                .slot
                .as_ref()
                .map_or(0, |slot| slot.index / per_row);
            let (slot_row, slot_height) = control_row(
                "settings/showcase/palette-slot-row",
                "Palette color",
                vec![field],
            );
            let mut parts = vec![chip_row(open), slot_row, tools];
            let mut editor_height = chip_height + 6.0 + slot_height + 6.0 + tools_height;
            if let Some((row, height)) = reset_row {
                parts.push(row);
                editor_height += height + 6.0;
            }
            let editor = UiNode::new(
                "settings/showcase/palette-and-tools",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(editor_height))
                    .gap(6.0),
            )
            .children(parts);
            let mut editor = Some((editor, editor_height));
            blocks.push(with_heading_rows(
                "settings/showcase/palette-section",
                "PALETTE",
                (0..rows)
                    .map(|row| {
                        if row == open {
                            editor.take().expect("one open row")
                        } else {
                            (chip_row(row), chip_height)
                        }
                    })
                    .collect(),
                column_width,
            ));
        }
        Some(field) => {
            let grid_height = 2.0 * chip_height + 4.0;
            let grid = UiNode::new(
                "settings/showcase/palette",
                UiContent::Group(GroupSpec::new("Palette")),
            )
            .layout(Layout::column().height(Length::Fixed(grid_height)).gap(4.0))
            .children(vec![chip_row(0), chip_row(1)]);
            let (slot_row, slot_height) = control_row(
                "settings/showcase/palette-slot-row",
                "Palette color",
                vec![field],
            );
            let mut parts = vec![grid, slot_row, tools];
            let mut section_height = grid_height + 6.0 + slot_height + 6.0 + tools_height;
            if let Some((row, height)) = reset_row {
                parts.push(row);
                section_height += height + 6.0;
            }
            let section = UiNode::new(
                "settings/showcase/palette-and-tools",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(section_height))
                    .gap(6.0),
            )
            .children(parts);
            blocks.push(with_heading(
                "settings/showcase/palette-section",
                "PALETTE",
                (section, section_height),
                column_width,
            ));
        }
    }
    blocks.extend(palette_slot_picker(
        state,
        column_width,
        colour_picker_room().min(room),
    ));
    blocks
}

/// The least width the new theme's name field keeps beside Save and Cancel.
const SAVE_NAME_MIN_WIDTH: f32 = 180.0;

/// Save as theme: its button, or — once pressed — the name, Save and Cancel
/// in its place, "Include type settings" (only while the type in force
/// differs from aterm's own), and why a name cannot be used.
fn save_blocks(state: &SettingsViewState, column_width: f32) -> Vec<(UiNode, f32)> {
    let Some(save) = state.showcase.save.as_ref() else {
        let mut open = button(
            SHOWCASE_SAVE_AS,
            "Save as theme\u{2026}",
            "Save as theme\u{2026}",
            StyleRef::Secondary,
            ControlState {
                focused: focused(state, SHOWCASE_SAVE_AS),
                ..ControlState::default()
            },
            Some(hug_width("Save as theme\u{2026}").min(column_width)),
        );
        if let UiContent::Button(control) = &mut open.content {
            control.spec.description =
                Some("Writes the colors shown to your themes folder, then uses it".to_string());
        }
        let spacer = UiNode::new(
            "settings/showcase/save-as-row/spacer",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        let height = sheet_row_height();
        return vec![(
            UiNode::new(
                "settings/showcase/save-as-row",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fixed(height)))
            .children(vec![open, spacer]),
            height,
        )];
    };
    let problem = save.error.clone();
    let field = text_field(
        state,
        SHOWCASE_SAVE_NAME,
        "New theme name",
        "Theme name",
        &save.name,
        None,
        problem.is_some(),
    );
    let mut confirm = button(
        SAVE_CONFIRM,
        "Save the colors shown as a new theme",
        "Save",
        StyleRef::Secondary,
        ControlState {
            enabled: problem.is_none(),
            focused: focused(state, SAVE_CONFIRM),
            ..ControlState::default()
        },
        Some(hug_width("Save")),
    );
    if let UiContent::Button(control) = &mut confirm.content {
        control.spec.description = Some(if save.include_type {
            "Writes the colors and type settings shown to your themes folder, then uses it"
                .to_string()
        } else {
            "Writes the colors shown to your themes folder, then uses it".to_string()
        });
    }
    let cancel = button(
        SAVE_CANCEL,
        "Cancel saving a theme",
        "Cancel",
        StyleRef::Quiet,
        ControlState {
            focused: focused(state, SAVE_CANCEL),
            ..ControlState::default()
        },
        Some(hug_width("Cancel")),
    );
    let buttons_width = hug_width("Save") + hug_width("Cancel") + 12.0;
    let mut blocks = if column_width - buttons_width < SAVE_NAME_MIN_WIDTH {
        let spacer = UiNode::new(
            "settings/showcase/save-row/spacer",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill));
        vec![
            control_row("settings/showcase/save-name-row", "Theme name", vec![field]),
            control_row(
                "settings/showcase/save-row",
                "Save as theme",
                vec![confirm, cancel, spacer],
            ),
        ]
    } else {
        vec![control_row(
            "settings/showcase/save-row",
            "Save as theme",
            vec![field, confirm, cancel],
        )]
    };
    if let Some(problem) = problem {
        let height = caption_height();
        blocks.push((
            line(
                "settings/showcase/save-note",
                &problem,
                SemanticRole::Status,
                StyleRef::Danger,
                column_width,
                height,
            ),
            height,
        ));
    }
    if state.save_type_in_force() && !state.save_typography().is_empty() {
        let switch_width = 52.0 * settings_text_scale().min(1.4);
        let include_type = UiNode::new(
            SHOWCASE_SAVE_TYPE,
            UiContent::Switch(
                Control::new(
                    SwitchSpec {
                        label: "Include type settings".to_string(),
                        description: Some(
                            "Also write the font, line height, ligatures, weight and size \
                             scale in force"
                                .to_string(),
                        ),
                    },
                    ActionId::new(SHOWCASE_SAVE_TYPE),
                )
                .value(SemanticValue::Bool(save.include_type))
                .state(ControlState {
                    focused: focused(state, SHOWCASE_SAVE_TYPE),
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(switch_width))
                .height(Length::Fill),
        );
        blocks.push(control_row(
            "settings/showcase/save-type-row",
            "Include type settings",
            vec![
                line(
                    "settings/showcase/save-type-label",
                    "Include type settings",
                    SemanticRole::Text,
                    StyleRef::Plain,
                    (column_width - switch_width - 6.0).max(0.0),
                    sheet_row_height(),
                )
                .paint_only(),
                include_type,
            ],
        ));
    }
    blocks
}

/// How tall the colour picker under a sheet row (or the open palette slot's
/// field) may be: a wheel the column's width or less over Brightness.
fn colour_picker_room() -> f32 {
    sheet_row_height() * 7.0
}

/// The least list the sheet keeps, `column_width` wide: COLORS over one whole colour row (a screen that starts in the section repeats its caption), and an open picker, a colour row's or the palette slot's, whole in its least form on a screen of its own ([`sheet_blocks`]).
pub(super) fn sheet_list_least(state: &SettingsViewState, column_width: f32) -> f32 {
    let captioned_row =
        caption_height() + 4.0 + colour_row_height(colour_rows_on_one_line(column_width));
    let colour_picker = state
        .open_color_picker()
        .filter(|open| SHOWCASE_COLOR_KEYS.contains(open))
        .and_then(|open| color_picker_node(state, open, SettingsWidth::Compact, column_width, 0.0))
        .map_or(0.0, |(_, height)| height);
    let slot_picker =
        palette_slot_picker(state, column_width, 0.0).map_or(0.0, |(_, height)| height);
    captioned_row.max(colour_picker).max(slot_picker)
}

/// The sheet's blocks, in order: the colour rows under COLORS (each on one line, or its name over its field, as `one_line` says), the palette under PALETTE, then Save as theme, for a list `room` tall: an open colour picker is sized so its block is one stop of it, with the row (or the palette slot) it sets.
pub(super) fn sheet_blocks(
    state: &SettingsViewState,
    column_width: f32,
    room: f32,
    one_line: bool,
) -> Vec<(UiNode, f32)> {
    let mut blocks = Vec::new();
    let picker = |key: &str, room: f32| {
        color_picker_node(state, key, SettingsWidth::Compact, column_width, room)
    };
    let caption = caption_height() + 4.0;
    let under = room - caption;
    let captioned = state
        .open_color_picker()
        .filter(|open| SHOWCASE_COLOR_KEYS.contains(open))
        .and_then(|open| picker(open, under.min(colour_picker_room())))
        .is_none_or(|(_, height)| height <= under + 0.5);
    if captioned {
        let mut rows: Vec<(UiNode, f32)> = Vec::new();
        for key in SHOWCASE_COLOR_KEYS {
            let Some(row) = customize_row(state, key, column_width, one_line) else {
                continue;
            };
            let shared = (under - row.1 - 4.0).min(colour_picker_room());
            rows.push(row);
            rows.extend(
                picker(key, shared)
                    .filter(|(_, height)| *height <= shared + 0.5)
                    .or_else(|| picker(key, under.min(colour_picker_room()))),
            );
        }
        if !rows.is_empty() {
            blocks.push(with_heading_rows(
                "settings/showcase/colours",
                "COLORS",
                rows,
                column_width,
            ));
        }
    } else {
        let mut first = true;
        for key in SHOWCASE_COLOR_KEYS {
            let Some(row) = customize_row(state, key, column_width, one_line) else {
                continue;
            };
            let block = if first {
                with_heading("settings/showcase/colours", "COLORS", row, column_width)
            } else {
                row
            };
            first = false;
            let shared = (room - block.1 - 6.0).min(colour_picker_room());
            blocks.push(block);
            blocks.extend(
                picker(key, shared)
                    .filter(|(_, height)| *height <= shared + 0.5)
                    .or_else(|| picker(key, room.min(colour_picker_room()))),
            );
        }
    }
    blocks.extend(other_side_warning(state, column_width));
    blocks.extend(palette_blocks(state, column_width, room));
    blocks.extend(save_blocks(state, column_width));
    blocks
}

/// The text contrast the person's colors leave the theme of the side the page does not show, while the theme follows the system — where that is under 3:1 and the theme alone is not, so these edits did it: (the words the sheet shows, what a reader hears).
pub(super) fn other_side_contrast(state: &SettingsViewState) -> Option<(String, String)> {
    if prefs::theme_mode(state.raw_value(prefs::EDIT_THEME).as_deref())
        != prefs::ThemeMode::FollowSystem
    {
        return None;
    }
    let other = match theme_visible_side(state, state.presented_motion.get().system_dark) {
        aterm_types::Appearance::Dark => aterm_types::Appearance::Light,
        aterm_types::Appearance::Light => aterm_types::Appearance::Dark,
    };
    let text_contrast = |colors: &crate::resolved_colors::ResolvedColors| {
        let rgb = |color: aterm_types::Rgb| [color.r, color.g, color.b];
        aterm_messages::ink::contrast(rgb(colors.scheme.foreground), rgb(colors.scheme.background))
    };
    let drawn = preview_resolved_colors_on(state, other, None);
    let alone = Config {
        theme: state.raw_value(prefs::EDIT_THEME),
        ..Config::default()
    }
    .resolved_colors(other, &state.assets.themes, None);
    let ratio = text_contrast(&drawn);
    if ratio >= 3.0 || text_contrast(&alone) < 3.0 {
        return None;
    }
    let figure = crate::settings_preview::theme_card::contrast_figure(ratio);
    let side = match other {
        aterm_types::Appearance::Dark => "Dark",
        aterm_types::Appearance::Light => "Light",
    };
    let theme = state.showcase.label_of(&drawn.id);
    Some((
        format!("\u{26a0} {side} side: text contrast {figure}:1"),
        format!(
            "These colors leave {theme}, the {side} side\u{2019}s theme, at text contrast {figure}:1"
        ),
    ))
}

/// [`other_side_contrast`] as the line under the color rows.
fn other_side_warning(state: &SettingsViewState, column_width: f32) -> Option<(UiNode, f32)> {
    let (shown, spoken) = other_side_contrast(state)?;
    let height = caption_height();
    let words = line(
        "settings/showcase/other-side",
        &shown,
        SemanticRole::Status,
        StyleRef::Danger,
        column_width,
        height,
    );
    let node = UiNode::new(
        "settings/showcase/other-side/block",
        UiContent::Group(GroupSpec::new(spoken)),
    )
    .layout(Layout::column().height(Length::Fixed(height)))
    .children(vec![words.paint_only()]);
    Some((node, height))
}
