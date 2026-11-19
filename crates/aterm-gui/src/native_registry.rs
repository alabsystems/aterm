// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aRegistry's machine list, stored account assignments and signed machine observations.
//! Reads are shared by all views. Each view follows its own stable machine key.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIndicators, AppKind, AppPresentation, CloseReadiness, CloseRequest,
    Command, CommonViewState, DamageRegion, EventResult, NativeAppModel, SemanticInput,
    TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::TextInputState;
use crate::native_ui::{
    ActionId, ButtonIcon, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length,
    SemanticRole, SemanticValue, StyleRef, TextFieldSpec, UiContent, UiKey, UiNode, UiTree,
};
use crate::qos::Role;
use crate::registry_machines::{self, Machine, ReadError};
use crate::tables_store::Paths;

pub(crate) const SEARCH: &str = "registry/search";
const CLEAR: &str = "registry/clear-search";
const MACHINE: &str = "registry/machine/";
const MACHINES: &str = "registry/machines";
const OPEN: &str = "registry/open-in-tables";
const REFRESH: &str = "registry/refresh";
const PREVIOUS: &str = "registry/list/previous";
const NEXT: &str = "registry/list/next";

pub(crate) struct RegistryApp {
    paths: Option<Paths>,
    wake: Arc<dyn Fn() + Send + Sync>,
    pending: Option<mpsc::Receiver<registry_machines::Inventory>>,
    rows: Option<Vec<Machine>>,
    revision: u64,
    endings: BTreeMap<String, String>,
    error: Option<ReadError>,
}

impl RegistryApp {
    pub(crate) fn new(
        paths: Option<Paths>,
        role: Role,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let mut app = Self {
            paths,
            wake: Arc::new(wake),
            pending: None,
            rows: None,
            revision: 0,
            endings: BTreeMap::new(),
            error: None,
        };
        app.refresh(role);
        app
    }

    pub(crate) fn paths(&self) -> Option<Paths> {
        self.paths.clone()
    }

    pub(crate) fn refresh(&mut self, role: Role) {
        if self.pending.is_some() {
            return;
        }
        match registry_machines::load(self.paths(), role, Arc::clone(&self.wake)) {
            Ok(rx) => {
                self.pending = Some(rx);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn poll(&mut self) -> bool {
        let Some(rx) = &self.pending else {
            return false;
        };
        let reply = match rx.try_recv() {
            Ok(reply) => reply,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err(ReadError::Unavailable(
                "The registry reader stopped.".into(),
            )),
        };
        self.pending = None;
        match reply {
            Ok(rows) => {
                self.revision = self.revision.wrapping_add(1);
                self.endings = duplicate_name_endings(&rows);
                self.rows = Some(rows);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }

    fn machine(&self, key: &str) -> Option<&Machine> {
        self.rows.as_ref()?.iter().find(|r| r.key == key)
    }

    pub(crate) fn field_input<'a>(
        view: &'a RegistryViewState,
        key: &str,
    ) -> Option<&'a TextInputState> {
        (key == SEARCH && !view.narrow_detail()).then_some(&view.search.input)
    }

    fn listing(&self, view: &RegistryViewState) -> MachineList<'_> {
        let rows = self.rows.as_deref().unwrap_or_default();
        let search = &view.search;
        let matches = if search.needle.is_empty() {
            None
        } else {
            let mut found = search.found.borrow_mut();
            if found
                .as_ref()
                .is_none_or(|found| found.revision != self.revision)
            {
                let needle = search.needle.as_str();
                let indices = rows
                    .iter()
                    .enumerate()
                    .filter_map(|(i, row)| {
                        (row.name.to_lowercase().contains(needle)
                            || row.key.to_lowercase().contains(needle))
                        .then_some(i)
                    })
                    .collect::<Arc<[usize]>>();
                *found = Some(SearchMatches {
                    revision: self.revision,
                    indices,
                });
            }
            found.as_ref().map(|found| Arc::clone(&found.indices))
        };
        MachineList { rows, matches }
    }

    fn status(&self) -> Option<String> {
        if let Some(why) = &self.error {
            return Some(if self.rows.is_some() {
                format!("Couldn’t refresh. Showing the last read. {why}")
            } else {
                format!("Couldn’t read the registry. {why}")
            });
        }
        self.pending.as_ref().map(|_| {
            if self.rows.is_some() {
                "Refreshing machines…"
            } else {
                "Reading machines…"
            }
            .into()
        })
    }
}

/// Only ambiguous names need a key on the machine picker. In reverse-key order, the
/// longest shared ending is with one of the two neighbours.
fn duplicate_name_endings(rows: &[Machine]) -> BTreeMap<String, String> {
    let mut names = BTreeMap::<String, Vec<&str>>::new();
    for machine in rows {
        names
            .entry(machine.name.to_lowercase())
            .or_default()
            .push(&machine.key);
    }
    let mut endings = BTreeMap::new();
    for keys in names.values_mut().filter(|keys| keys.len() > 1) {
        keys.sort_unstable_by(|a, b| a.bytes().rev().cmp(b.bytes().rev()));
        for (i, &key) in keys.iter().enumerate() {
            let neighbours = &keys[i.saturating_sub(1)..(i + 2).min(keys.len())];
            let ending = crate::native_tables::rows::ending(key, neighbours);
            endings.insert(key.to_owned(), ending.to_owned());
        }
    }
    endings
}

struct MachineSearch {
    input: TextInputState,
    needle: String,
    found: RefCell<Option<SearchMatches>>,
}

struct SearchMatches {
    revision: u64,
    indices: Arc<[usize]>,
}

impl Default for MachineSearch {
    fn default() -> Self {
        Self {
            input: TextInputState::new(String::new()),
            needle: String::new(),
            found: RefCell::default(),
        }
    }
}

/// The unfiltered picker indexes the read directly. A query holds only matching row indices
/// in the read's order, reused until either the query or the read changes.
struct MachineList<'a> {
    rows: &'a [Machine],
    matches: Option<Arc<[usize]>>,
}

impl<'a> MachineList<'a> {
    fn len(&self) -> usize {
        self.matches
            .as_ref()
            .map_or(self.rows.len(), |found| found.len())
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn iter(&self) -> impl Iterator<Item = &'a Machine> + '_ {
        (0..self.len()).map(|i| &self.rows[self.matches.as_ref().map_or(i, |found| found[i])])
    }
}

#[derive(Default)]
pub(crate) struct RegistryViewState {
    pub(crate) common: CommonViewState,
    machine: Option<String>,
    search: MachineSearch,
    finding: bool,
    list_start: Cell<usize>,
    list_room: Cell<usize>,
    reveal_selected: Cell<bool>,
    detail_scroll: Cell<f32>,
    detail_limit: Cell<f32>,
    body_height: Cell<f32>,
    wide: Cell<bool>,
}

impl RegistryViewState {
    pub(crate) fn selected(&self) -> Option<&str> {
        self.machine.as_deref()
    }

    pub(crate) fn select(&mut self, key: Option<String>) {
        self.finding = false;
        if key != self.machine {
            self.machine = key;
            self.reveal_selected.set(self.machine.is_some());
            self.detail_scroll.set(0.0);
            self.common.presentation_revision = self.common.presentation_revision.wrapping_add(1);
        }
    }

    fn narrow_detail(&self) -> bool {
        !self.wide.get() && self.machine.is_some() && !self.finding
    }

    fn in_search(&self) -> bool {
        self.common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str() == SEARCH)
            && !self.narrow_detail()
    }

    pub(crate) fn cancel_preedit(&mut self, field: Option<&UiKey>) -> bool {
        if field.is_some_and(|key| key.as_str() == SEARCH) && self.search.input.preedit().is_some()
        {
            self.search.input.cancel_preedit();
            true
        } else {
            false
        }
    }

    fn edit_search(&mut self, event: TextInputEvent) {
        let event = match event {
            TextInputEvent::Commit(text) => TextInputEvent::Commit(single_line(text)),
            other => other,
        };
        event.edit(&mut self.search.input);
        self.update_query();
    }

    fn update_query(&mut self) {
        let needle = self.search.input.value().trim().to_lowercase();
        if self.search.needle != needle {
            self.search.needle = needle;
            self.search.found.get_mut().take();
            self.list_start.set(0);
            self.reveal_selected.set(self.search.needle.is_empty());
        }
    }

    fn clear_search(&mut self) {
        self.search.input = TextInputState::new(String::new());
        self.update_query();
        self.common.last_focus = Some(UiKey::new(SEARCH));
    }

    fn open_machine(&mut self, machine: &Machine) {
        self.select(Some(machine.key.clone()));
        self.common.last_focus = Some(if self.wide.get() {
            UiKey::new(format!("{MACHINE}{}", machine.key))
        } else {
            UiKey::new(OPEN)
        });
    }

    fn back_to_list(&mut self, rows: &MachineList<'_>) {
        let machine = rows
            .iter()
            .find(|row| Some(row.key.as_str()) == self.selected())
            .or_else(|| rows.iter().nth(self.list_start.get()))
            .or_else(|| rows.iter().next());
        self.common.last_focus = Some(machine.map_or_else(
            || UiKey::new(SEARCH),
            |row| UiKey::new(format!("{MACHINE}{}", row.key)),
        ));
        self.select(None);
        self.reveal_selected.set(true);
    }

    fn scroll_list(&self, lines: isize, count: usize) {
        let last = count.saturating_sub(self.list_room.get());
        self.list_start
            .set(self.list_start.get().saturating_add_signed(lines).min(last));
    }

    fn reveal(&self, index: usize) {
        let start = self.list_start.get();
        if index < start {
            self.list_start.set(index);
        } else if index >= start.saturating_add(self.list_room.get()) {
            self.list_start
                .set((index + 1).saturating_sub(self.list_room.get()));
        }
    }

    fn on_detail(&self, key: &str) -> bool {
        self.narrow_detail() || key.starts_with("registry/detail") || key == OPEN
    }

    fn scroll(&self, key: &str, step: i32, page: bool, count: usize) {
        if self.on_detail(key) {
            let size = if page {
                self.body_height.get().max(28.0)
            } else {
                28.0
            };
            self.detail_scroll.set(
                (self.detail_scroll.get() + size * step as f32).clamp(0.0, self.detail_limit.get()),
            );
        } else {
            let rows = if page {
                self.list_room.get() as isize
            } else {
                1
            };
            self.scroll_list(step as isize * rows, count);
        }
    }
}

impl NativeAppModel for RegistryApp {
    type ViewState = RegistryViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::Registry,
            name: "aRegistry",
            icon: AppKind::Registry.icon(),
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut RegistryViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        let rows = self.listing(view);
        match event {
            AppEvent::FocusChanged(focus) => {
                if focus.as_ref().is_some_and(|key| key.as_str() == SEARCH) {
                    view.finding = true;
                } else {
                    view.search.input.cancel_preedit();
                }
                view.common.last_focus = focus;
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) if view.in_search() => {
                view.edit_search(TextInputEvent::Commit(text));
            }
            AppEvent::TextInput(event) if view.in_search() => match event {
                TextInputEvent::Cancel if view.search.input.preedit().is_some() => {
                    view.search.input.cancel_preedit()
                }
                TextInputEvent::Cancel if !view.search.input.value().is_empty() => {
                    view.clear_search()
                }
                TextInputEvent::Cancel => {
                    if view.finding
                        && !view.wide.get()
                        && let Some(m) = view.selected().and_then(|k| self.machine(k))
                    {
                        view.open_machine(m);
                    } else if let Some(m) = rows.iter().nth(view.list_start.get()) {
                        view.common.last_focus = Some(UiKey::new(format!("{MACHINE}{}", m.key)));
                    }
                }
                TextInputEvent::Submit => {
                    if let Some(m) = rows.iter().next() {
                        view.open_machine(m);
                    }
                }
                event => view.edit_search(event),
            },
            AppEvent::ScrollAt { lines, key } => {
                view.scroll(key.as_str(), lines, false, rows.len())
            }
            AppEvent::ScrollLines(step) | AppEvent::ScrollPage(step) => {
                let key = view.common.last_focus.as_ref().map_or("", UiKey::as_str);
                view.scroll(
                    key,
                    step,
                    matches!(event, AppEvent::ScrollPage(_)),
                    rows.len(),
                );
            }
            AppEvent::StepFocus(step)
                if view.on_detail(view.common.last_focus.as_ref().map_or("", UiKey::as_str)) =>
            {
                view.scroll(OPEN, step, false, rows.len())
            }
            AppEvent::StepFocus(step) if !rows.is_empty() && !view.narrow_detail() => {
                let focused = view
                    .common
                    .last_focus
                    .as_ref()
                    .and_then(|k| k.as_str().strip_prefix(MACHINE));
                let index = focused
                    .and_then(|key| rows.iter().position(|r| r.key == key))
                    .map_or(view.list_start.get(), |i| {
                        i.saturating_add_signed(step.signum() as isize)
                    })
                    .min(rows.len() - 1);
                view.reveal(index);
                let Some(machine) = rows.iter().nth(index) else {
                    return EventResult::Bubble;
                };
                view.common.last_focus = Some(UiKey::new(format!("{MACHINE}{}", machine.key)));
                view.common.focus_visible = true;
            }
            AppEvent::TextInput(TextInputEvent::Cancel) if view.selected().is_some() => {
                view.back_to_list(&rows);
            }
            AppEvent::Action(action) => match action.id.as_str() {
                SEARCH => {
                    view.finding = true;
                    view.common.last_focus = Some(UiKey::new(SEARCH));
                    match action.value {
                        Some(SemanticInput::Text(text)) => {
                            view.search.input = TextInputState::new(single_line(text));
                            view.update_query();
                        }
                        Some(SemanticInput::TextPosition { byte, extend }) => {
                            let anchor = if extend {
                                view.search.input.selection().anchor
                            } else {
                                byte
                            };
                            view.search.input.set_selection(anchor, byte);
                        }
                        None => view.search.input.select_all(),
                        _ => return EventResult::Bubble,
                    }
                }
                CLEAR => view.clear_search(),
                REFRESH => self.refresh(Role::Responsive),
                MACHINES => view.back_to_list(&rows),
                PREVIOUS => view.scroll_list(-(view.list_room.get() as isize), rows.len()),
                NEXT => view.scroll_list(view.list_room.get() as isize, rows.len()),
                OPEN => {
                    let Some(machine) = view
                        .selected()
                        .and_then(|k| self.machine(k))
                        .filter(|_| view.wide.get() || view.narrow_detail())
                    else {
                        return EventResult::Bubble;
                    };
                    cx.open_registry_machine_in_tables(machine.key.clone());
                }
                key => {
                    let Some(machine) = key
                        .strip_prefix(MACHINE)
                        .and_then(|k| rows.iter().find(|m| m.key == k))
                    else {
                        return EventResult::Bubble;
                    };
                    view.open_machine(machine);
                }
            },
            _ => return EventResult::Bubble,
        }
        cx.repaint(DamageRegion::All);
        EventResult::Handled
    }

    fn view(&self, view: &RegistryViewState, cx: &ViewCx<'_>) -> UiTree {
        self.page(view, cx.viewport.width, cx.viewport.height)
    }

    fn commands(&self, _: &RegistryViewState, _: &mut Vec<Command>) {}
    fn presentation(&self, _: &RegistryViewState) -> AppPresentation {
        AppPresentation {
            title: "aRegistry".into(),
            icon: AppKind::Registry.icon(),
            indicators: AppIndicators {
                busy: self.pending.is_some(),
                ..AppIndicators::default()
            },
            closable: true,
            tooltip: Some("Fleet machines and account assignments".into()),
        }
    }
    fn prepare_close(&mut self, _: CloseRequest, _: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

fn single_line(text: String) -> String {
    if text.contains(['\r', '\n']) {
        text.replace("\r\n", "\n").replace(['\r', '\n'], " ")
    } else {
        text
    }
}

fn button(key: &str, label: &str, enabled: bool) -> UiNode {
    let width = crate::native_ui::native_button_label_width(label) + 24.0;
    sized_button(key, ButtonSpec::new(label), enabled, width)
}

fn sized_button(key: &str, spec: ButtonSpec, enabled: bool, width: f32) -> UiNode {
    UiNode::new(
        key,
        UiContent::Button(Control::new(spec, ActionId::new(key)).state(ControlState {
            enabled,
            ..ControlState::default()
        })),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fixed(button_height())),
    )
}

fn button_height() -> f32 {
    30.0 * crate::native_appearance::text_scale().max(1.0)
}

fn group(
    key: &str,
    label: &str,
    role: SemanticRole,
    layout: Layout,
    children: Vec<UiNode>,
) -> UiNode {
    UiNode::new(
        key,
        UiContent::Group(GroupSpec {
            label: Some(label.into()),
            role,
            style: StyleRef::Plain,
        }),
    )
    .layout(layout)
    .children(children)
}

fn line(key: &str, words: &str, width: f32, role: SemanticRole, style: StyleRef) -> UiNode {
    crate::native_about::wrapped(key, words, width.max(1.0), role, style)
}

fn height(node: &UiNode) -> f32 {
    match node.layout.height {
        Length::Fixed(h) => h,
        _ => 0.0,
    }
}

impl RegistryApp {
    fn search_field(view: &RegistryViewState) -> UiNode {
        let input = &view.search.input;
        UiNode::new(
            SEARCH,
            UiContent::TextField(
                Control::new(
                    TextFieldSpec {
                        label: "Find machines".into(),
                        placeholder: Some("Find machines".into()),
                        secret: false,
                        visual_value: None,
                        input: Some(input.projection()),
                        swatch: None,
                        swatch_line: false,
                        trailing_hint: None,
                        description: Some("Search by machine name or key".into()),
                        end_button: false,
                    },
                    ActionId::new(SEARCH),
                )
                .value(SemanticValue::Text(input.value().into()))
                .state(ControlState {
                    focused: view.in_search(),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(button_height())))
    }

    fn page(&self, view: &RegistryViewState, width: f32, height_px: f32) -> UiTree {
        let scale = crate::native_appearance::text_scale().max(1.0);
        let wide = width >= 620.0 * scale;
        view.wide.set(wide);
        let pad = if wide { 20.0 } else { 12.0 };
        let width = (width - pad * 2.0).max(1.0);
        let narrow_detail = view.narrow_detail();
        let mut header = Vec::new();
        if narrow_detail {
            header.push(button(MACHINES, "All machines", true));
        } else {
            if wide {
                let mut title = line(
                    "registry/title",
                    "Machines",
                    width,
                    SemanticRole::Heading,
                    StyleRef::Primary,
                );
                title.layout.width = Length::Fixed(crate::native_workers::text_width(
                    "Machines",
                    SemanticRole::Heading,
                    StyleRef::Primary,
                ));
                header.push(title);
            }
            header.push(Self::search_field(view));
            if !view.search.input.value().is_empty() {
                header.push(sized_button(
                    CLEAR,
                    ButtonSpec::new("Clear search").visual_icon(ButtonIcon::Clear),
                    true,
                    button_height(),
                ));
            }
        }
        header.push(button(REFRESH, "Refresh", self.pending.is_none()));
        let header_h = header.iter().map(height).fold(button_height(), f32::max);
        let mut children = vec![group(
            "registry/header",
            "Fleet registry",
            SemanticRole::Group,
            Layout::row().gap(8.0).height(Length::Fixed(header_h)),
            header,
        )];
        let mut body_h = height_px - 2.0 * pad - header_h - 10.0;
        if let Some(status) = self.status() {
            let status = line(
                "registry/status",
                &status,
                width,
                SemanticRole::Status,
                StyleRef::Note,
            );
            body_h -= height(&status) + 10.0;
            children.push(status);
        }
        let body_h = body_h.max(0.0);
        view.body_height.set(body_h);
        let sidebar = if wide {
            (width * 0.32).min(320.0 * scale)
        } else {
            width
        };
        let mut panes = Vec::new();
        if !narrow_detail {
            panes.push(self.list(view, sidebar, body_h));
        }
        if wide || narrow_detail {
            let detail_w = if wide { width - sidebar - 20.0 } else { width };
            panes.push(self.detail(view, detail_w, body_h));
        }
        children.push(group(
            "registry/body",
            "Machines",
            SemanticRole::Group,
            Layout::row()
                .gap(20.0)
                .height(Length::Fixed(body_h))
                .clipped(),
            panes,
        ));
        UiTree::new(group(
            "registry/root",
            "aRegistry",
            SemanticRole::Application,
            Layout::column()
                .gap(10.0)
                .padding(Insets::all(pad))
                .clipped(),
            children,
        ))
    }

    fn row_label(&self, machine: &Machine, width: f32) -> ButtonSpec {
        let role = SemanticRole::Button;
        let style = StyleRef::Plain;
        let fit = |s: &str, w: f32| crate::native_workers::one_line(s, role, style, w);
        let (label, shown) = if let Some(ending) = self.endings.get(&machine.key) {
            let suffix = format!(" · {ending}");
            let room = width - crate::native_workers::text_width(&suffix, role, style);
            let shown = if room >= crate::native_workers::text_width("…", role, style) {
                format!("{}{suffix}", fit(&machine.name, room))
            } else {
                fit(ending, width)
            };
            (format!("{}{suffix}", machine.name), shown)
        } else {
            (machine.name.clone(), fit(&machine.name, width))
        };
        ButtonSpec::new(label)
            .description(format!("Machine key {}", machine.key))
            .visual_label(shown)
            .icon_hidden(true)
    }

    fn list(&self, view: &RegistryViewState, width: f32, room: f32) -> UiNode {
        let rows = self.listing(view);
        let row_h = button_height();
        let fit = (room / (row_h + 4.0)).floor() as usize;
        let paged = rows.len() > fit.max(1);
        let inline_pages = paged && fit < 2;
        let capacity = fit
            .saturating_sub(usize::from(paged && !inline_pages))
            .max(1);
        let list_width = if inline_pages {
            (width - 2.0 * row_h - 8.0).max(1.0)
        } else {
            width
        };
        view.list_room.set(capacity);
        if self.rows.is_some()
            && view.reveal_selected.replace(false)
            && let Some(key) = view.selected().or_else(|| {
                view.common
                    .last_focus
                    .as_ref()?
                    .as_str()
                    .strip_prefix(MACHINE)
            })
            && let Some(i) = rows.iter().position(|r| r.key == key)
        {
            view.reveal(i);
        }
        let start = view
            .list_start
            .get()
            .min(rows.len().saturating_sub(capacity));
        view.list_start.set(start);
        let mut entries = Vec::new();
        if self.rows.is_some() && rows.is_empty() {
            entries.push(line(
                "registry/list/empty",
                if self.rows.as_ref().is_some_and(Vec::is_empty) {
                    "No machines registered."
                } else {
                    "No matching machines."
                },
                width,
                SemanticRole::Status,
                StyleRef::Note,
            ));
        }
        for machine in rows.iter().skip(start).take(capacity) {
            let id = format!("{MACHINE}{}", machine.key);
            let selected = view.selected() == Some(machine.key.as_str());
            let spec = self.row_label(machine, list_width - 34.0);
            entries.push(
                UiNode::new(
                    id.clone(),
                    UiContent::Button(
                        Control::new(spec, ActionId::new(id))
                            .role(SemanticRole::ListBoxOption)
                            .state(ControlState {
                                selected,
                                ..ControlState::default()
                            })
                            .style(if selected {
                                StyleRef::Primary
                            } else {
                                StyleRef::Plain
                            }),
                    ),
                )
                .layout(Layout::default().height(Length::Fixed(row_h))),
            );
        }
        let list = group(
            "registry/list/rows",
            "Registered machines",
            SemanticRole::ListBox,
            Layout::column().gap(4.0),
            entries,
        );
        let mut children = vec![list];
        let outer = if inline_pages {
            for (key, name, icon, enabled) in [
                (PREVIOUS, "Previous", ButtonIcon::Back, start > 0),
                (
                    NEXT,
                    "Next",
                    ButtonIcon::Forward,
                    start + capacity < rows.len(),
                ),
            ] {
                children.push(sized_button(
                    key,
                    ButtonSpec::new(name).visual_icon(icon),
                    enabled,
                    row_h,
                ));
            }
            Layout::row()
        } else {
            if paged {
                children.push(group(
                    "registry/list/pages",
                    "More machines",
                    SemanticRole::Navigation,
                    Layout::row().gap(4.0).height(Length::Fixed(row_h)),
                    vec![
                        button(PREVIOUS, "Previous", start > 0),
                        button(NEXT, "Next", start + capacity < rows.len()),
                    ],
                ));
            }
            Layout::column()
        };
        group(
            "registry/list",
            "Machine list",
            SemanticRole::Group,
            outer
                .gap(4.0)
                .width(Length::Fixed(width))
                .height(Length::Fixed(room))
                .clipped(),
            children,
        )
    }

    fn detail(&self, view: &RegistryViewState, width: f32, room: f32) -> UiNode {
        let heading = |key: &str, words: &str| {
            line(key, words, width, SemanticRole::Heading, StyleRef::Primary)
        };
        let text =
            |key: &str, words: &str| line(key, words, width, SemanticRole::Text, StyleRef::Plain);
        let note =
            |key: &str, words: &str| line(key, words, width, SemanticRole::Text, StyleRef::Note);
        let mut items = Vec::new();
        let machine = view.selected().and_then(|key| self.machine(key));
        if let Some(machine) = machine {
            items.push(heading("registry/detail/name", &machine.name));
            let identity = if machine.status.is_empty() {
                machine.key.clone()
            } else {
                format!("{} · {}", machine.status, machine.key)
            };
            items.push(note("registry/detail/identity", &identity));
            items.push(heading("registry/detail/accounts", "Assigned AI accounts"));
            if machine.assignments.is_empty() {
                items.push(note("registry/detail/no-accounts", "No assigned accounts."));
            }
            for (pk, name) in &machine.assignments {
                items.push(text(&format!("registry/detail/account/{pk}"), name));
            }
            let observations = if machine.observations.len() > 1 {
                "Latest signed observations"
            } else {
                "Latest signed observation"
            };
            items.push(heading("registry/detail/observations", observations));
            if machine.observations.is_empty() {
                items.push(note(
                    "registry/detail/no-observations",
                    "No signed observation recorded.",
                ));
            }
            for (i, seen) in machine.observations.iter().enumerate() {
                items.push(text(
                    &format!("registry/detail/observation/{i}"),
                    &seen.summary,
                ));
                let time = seen.at_ms.map_or_else(
                    || "Observation time unavailable".into(),
                    crate::native_tables::stamp_words,
                );
                items.push(note(
                    &format!("registry/detail/writer/{i}"),
                    &format!("Writer {} · {time}", seen.writer),
                ));
            }
        } else if let Some(rows) = &self.rows {
            let message = if view.selected().is_some() {
                Some("The selected machine is no longer listed.")
            } else if rows.is_empty() {
                None
            } else {
                Some("Select a machine to see its assignments and observations.")
            };
            if let Some(message) = message {
                items.push(line(
                    "registry/detail/empty",
                    message,
                    width,
                    SemanticRole::Status,
                    StyleRef::Note,
                ));
            }
        }
        let content_h =
            items.iter().map(height).sum::<f32>() + 8.0 * items.len().saturating_sub(1) as f32;
        // Keep the inspector action beside a short record and visible below a scrolling one.
        let text_room = content_h.min(if machine.is_some() {
            (room - button_height() - 8.0).max(0.0)
        } else {
            room
        });
        let limit = (content_h - text_room).max(0.0);
        view.detail_limit.set(limit);
        view.detail_scroll.set(view.detail_scroll.get().min(limit));
        let details = group(
            "registry/detail/content",
            "Machine information",
            SemanticRole::Group,
            Layout::column()
                .gap(8.0)
                .height(Length::Fixed(text_room))
                .clipped()
                .scrolled(view.detail_scroll.get()),
            items,
        )
        .room(text_room, limit > 0.0)
        .lists_every_row();
        let mut children = vec![details];
        if machine.is_some() {
            children.push(button(OPEN, "Open in Tables", true));
        }
        group(
            "registry/detail",
            "Machine details",
            SemanticRole::Group,
            Layout::column()
                .gap(8.0)
                .height(Length::Fixed(room))
                .clipped(),
            children,
        )
    }
}

#[cfg(all(unix, feature = "bench-support"))]
#[path = "registry_open_measure.rs"]
pub(crate) mod open_measure;

#[cfg(test)]
#[path = "native_registry_tests.rs"]
mod tests;
