// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings tab application.
//!
//! This is an adapter over the shipping preference metadata and About/Update
//! models, not a second persistence or updater implementation.  It presents
//! edge-to-edge routes through [`crate::native_ui`] and emits typed host effects;
//! the legacy overlays remain available during the tab-host migration.

#![allow(
    dead_code,
    reason = "native Settings tab migration foundation; host wiring lands in stages"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::about::AboutState;
use crate::app_config::Config;
use crate::native_app::{
    ActionInvocation, AppDescriptor, AppEvent, AppIcon, AppIndicators, AppPresentation,
    ClipboardOutcome, ClipboardRequest, CloseReadiness, CloseRequest, Command, CommonViewState,
    ConfigEdit, ConfigPatch, ConfigPatchOutcome, EventResult, ExpectedConfigValue,
    ExternalOpenOutcome, ExternalOpenRequest, NativeAppModel, OperationId, PackagesOutcome,
    PackagesRequest, SemanticInput, TextInputEvent, UpdateCx, UpdateOutcome, UpdateRequest, ViewCx,
};
use crate::native_ui::{
    ActionId, ButtonIcon, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout, Length,
    RichTextSpec, SemanticRole, SemanticValue, SliderSpec, StyleRef, SwitchSpec, TextFieldSpec,
    TextSpec, UiContent, UiKey, UiNode, UiTree,
};
use crate::packages_screen::{PackagesBusy, PackagesProjection, PackagesState};
use crate::prefs::{self, EditField, EditKind, Section};
use crate::settings::SettingsState;
use crate::settings_preview::{
    AppearancePreviewSpec, CursorPostFxSpec, CursorPreviewSpec, PreviewAnimation,
    PreviewCursorStyle, PreviewScene, PreviewTerminalTheme, PreviewTrailStyle, SettingsPreviewSpec,
    TypographyPreviewSpec, WindowTabsPreviewSpec,
};
use crate::title_summary::{TitleSummaryHealth, TitleSummaryLocality, TitleSummaryRuntimeState};
use crate::update_screen::{UpdateProjection, UpdateState};

/// Stable, restoreable route identity. Paths are the control/restore contract;
/// labels are presentation and may be localized later.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SettingsRoute {
    Home,
    Modified,
    #[default]
    Appearance,
    TextFonts,
    CursorMotion,
    WindowTabs,
    KeyboardInput,
    Terminal,
    Performance,
    Security,
    Diagnostics,
    SoftwareUpdate,
    Packages,
    About,
}

impl SettingsRoute {
    pub(crate) const ALL: [Self; 14] = [
        Self::Home,
        Self::Modified,
        Self::Appearance,
        Self::TextFonts,
        Self::CursorMotion,
        Self::WindowTabs,
        Self::KeyboardInput,
        Self::Terminal,
        Self::Performance,
        Self::Security,
        Self::Diagnostics,
        Self::SoftwareUpdate,
        Self::Packages,
        Self::About,
    ];

    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::Home => "/home",
            Self::Modified => "/modified",
            Self::Appearance => "/appearance",
            Self::TextFonts => "/text-fonts",
            Self::CursorMotion => "/cursor-motion",
            Self::WindowTabs => "/window-tabs",
            Self::KeyboardInput => "/keyboard-input",
            Self::Terminal => "/terminal",
            Self::Performance => "/performance",
            Self::Security => "/security",
            Self::Diagnostics => "/diagnostics",
            Self::SoftwareUpdate => "/updates",
            Self::Packages => "/packages",
            Self::About => "/about",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Modified => "Modified",
            Self::Appearance => "Appearance",
            Self::TextFonts => "Text & Fonts",
            Self::CursorMotion => "Cursor & Motion",
            Self::WindowTabs => "Window & Tabs",
            Self::KeyboardInput => "Keyboard & Input",
            Self::Terminal => "Terminal",
            Self::Performance => "Performance",
            Self::Security => "Security",
            Self::Diagnostics => "Diagnostics",
            Self::SoftwareUpdate => "Software Update",
            Self::Packages => "Packages",
            Self::About => "About",
        }
    }

    pub(crate) fn from_path(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|route| route.path() == path)
    }

    fn section(self) -> Option<Section> {
        match self {
            Self::Appearance => Some(Section::Appearance),
            Self::TextFonts => Some(Section::Typography),
            Self::CursorMotion => Some(Section::Cursor),
            Self::WindowTabs => Some(Section::Window),
            Self::KeyboardInput => Some(Section::Input),
            Self::Terminal => Some(Section::Terminal),
            Self::Performance => Some(Section::Performance),
            Self::Security => Some(Section::Security),
            Self::Diagnostics => Some(Section::KittyLog),
            // Packages is a SPECIAL page (it renders its own switch rows and
            // status cards); its registry fields live in the search-only
            // `Section::Packages`, deliberately unowned by any ordinary page.
            Self::Home | Self::Modified | Self::SoftwareUpdate | Self::Packages | Self::About => {
                None
            }
        }
    }
}

enum PendingAction {
    Config(ConfigPatch),
    Undo,
    External,
    Update,
    Packages,
    Clipboard,
}

/// One explicit choice surface. Enum/theme controls open this renderer-native
/// picker instead of silently cycling and committing on every click. Its stable
/// index actions make pointer, keyboard, accessibility, and controller input
/// converge on the same options and OCC save path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChoiceOption {
    label: String,
    value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChoicePicker {
    key: String,
    options: Vec<ChoiceOption>,
    selected: usize,
    offset: usize,
}

impl ChoicePicker {
    // Two choices make one complete, thumb-friendly page even on a 320x568
    // host where the live renderer preview remains visible above the picker.
    // Keeping this in picker
    // state (rather than clipping an eight-item page at paint time) means
    // pointer, keyboard, accessibility, and preview focus all agree about the
    // options that are actually reachable.
    const PAGE_SIZE: usize = 2;

    fn new(
        key: &str,
        options: impl IntoIterator<Item = String>,
        current: &str,
        explicit: bool,
        effective_default: &str,
    ) -> Self {
        let current = current.strip_suffix(" (default)").unwrap_or(current).trim();
        let mut authored = vec![ChoiceOption {
            label: format!("Use default  ·  {effective_default}"),
            value: None,
        }];
        authored.extend(options.into_iter().map(|value| ChoiceOption {
            label: choice_label(&value),
            value: Some(value),
        }));
        if explicit
            && !authored.iter().any(|option| {
                option
                    .value
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(current))
            })
        {
            authored.push(ChoiceOption {
                label: format!("Current  ·  {current}"),
                value: Some(current.to_string()),
            });
        }
        let selected = if explicit {
            authored
                .iter()
                .position(|option| {
                    option
                        .value
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(current))
                })
                .unwrap_or(0)
        } else {
            0
        };
        let offset = (selected / Self::PAGE_SIZE) * Self::PAGE_SIZE;
        Self {
            key: key.to_string(),
            options: authored,
            selected,
            offset,
        }
    }

    fn visible_range(&self) -> std::ops::Range<usize> {
        self.offset.min(self.options.len())
            ..self
                .offset
                .saturating_add(Self::PAGE_SIZE)
                .min(self.options.len())
    }

    fn page(&mut self, delta: isize) {
        let pages = self.options.len().div_ceil(Self::PAGE_SIZE).max(1);
        let current = self.offset / Self::PAGE_SIZE;
        let next = current.saturating_add_signed(delta).min(pages - 1);
        self.offset = next * Self::PAGE_SIZE;
    }
}

/// One semantic transition of Settings' bounded virtual-page cursor. Pointer,
/// keyboard, wheel/controller, and accessibility actions all reduce through this
/// command set before the next semantic tree is compiled.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPageScrollCommand {
    Previous,
    Next,
    Absolute(usize),
    Lines(i32),
}

/// Clamp the virtual-page cursor before and after every transition. Clamping the
/// input first is deliberate: a route/search/config change may shrink the current
/// result set between events, and one reverse step must escape that stale offset
/// immediately instead of underflowing or remaining off-page.
#[doc(hidden)]
#[must_use]
pub fn settings_page_scroll_transition(
    current: usize,
    limit: usize,
    command: SettingsPageScrollCommand,
) -> usize {
    let current = current.min(limit);
    match command {
        SettingsPageScrollCommand::Previous => current.saturating_sub(1),
        SettingsPageScrollCommand::Next => current.saturating_add(1).min(limit),
        SettingsPageScrollCommand::Absolute(target) => target.min(limit),
        SettingsPageScrollCommand::Lines(lines) if lines < 0 => {
            current.saturating_sub(lines.unsigned_abs() as usize)
        }
        SettingsPageScrollCommand::Lines(lines) => {
            current.saturating_add(lines as usize).min(limit)
        }
    }
}

/// Honest per-view Settings state. Route, search, focus, transient form state,
/// and About text selection are view-local; committed configuration and updater
/// work remain process-global.
pub(crate) struct SettingsViewState {
    pub(crate) common: CommonViewState,
    pub(crate) route: SettingsRoute,
    pub(crate) search: String,
    pub(crate) legacy: SettingsState,
    /// Values that are literally present in `aterm.toml`, normalized through
    /// the same parser used by the process-wide transaction service.  This is
    /// intentionally separate from `EditField::seed`: Boolean seeds carry the
    /// effective default so a switch can paint truthfully even when its key is
    /// absent.
    raw_values: BTreeMap<String, String>,
    pub(crate) about: AboutState,
    pending: BTreeMap<OperationId, PendingAction>,
    pub(crate) feedback: Option<String>,
    pub(crate) last_undo: Option<u64>,
    pub(crate) page_scroll: usize,
    pub(crate) search_input: crate::native_text_input::TextInputState,
    pub(crate) editing_field: Option<String>,
    pub(crate) field_inputs: BTreeMap<String, crate::native_text_input::TextInputState>,
    choice_picker: Option<ChoicePicker>,
    /// Latest runtime observation for Smart Titles. Persisted config says what
    /// the user requested; this snapshot says whether the selected provider,
    /// managed runtime, and model are actually ready. `None` is rendered as
    /// unavailable rather than guessed from a loopback URL.
    title_summary_health: Option<TitleSummaryHealth>,
    /// The exact immutable non-text assets admitted with this config revision.
    /// Holding the outer Arc keeps Trail Pack and Nyan sprite identity atomic:
    /// previews never independently resolve a path or observe assets from two
    /// different revisions.
    assets: Arc<crate::app_config::ConfigAssetCatalog>,
}

impl SettingsViewState {
    /// Compatibility/test constructor. It is deliberately manifest-IO-free;
    /// production uses `from_snapshot`, which carries the canonical catalog.
    pub(crate) fn new(config: &Config) -> Self {
        Self::from_config_and_assets(config, crate::app_config::ConfigAssetCatalog::empty())
    }

    fn from_config_and_assets(
        config: &Config,
        assets: Arc<crate::app_config::ConfigAssetCatalog>,
    ) -> Self {
        let legacy =
            SettingsState::from_config_with_trail_pack_ids(config, &assets.trail_packs.ids);
        let raw_values = fallback_raw_values(config, &legacy.fields);
        Self {
            common: CommonViewState::default(),
            route: SettingsRoute::Appearance,
            search: String::new(),
            legacy,
            raw_values,
            about: AboutState::new(),
            pending: BTreeMap::new(),
            feedback: None,
            last_undo: None,
            page_scroll: 0,
            search_input: crate::native_text_input::TextInputState::new(String::new()),
            editing_field: None,
            field_inputs: BTreeMap::new(),
            choice_picker: None,
            title_summary_health: None,
            assets,
        }
    }

    /// Construct from the transaction service's canonical snapshot.  Production
    /// callers use this path so raw presence, OCC expectations, and displayed
    /// effective values all start from one durable source of truth.
    pub(crate) fn from_snapshot(
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) -> Result<Self, String> {
        let config = toml::from_str::<Config>(&snapshot.text)
            .map_err(|error| format!("aterm.toml is not a valid aterm config: {error}"))?;
        let mut state = Self::from_config_and_assets(&config, Arc::clone(&snapshot.assets));
        state.raw_values = snapshot.values()?;
        Ok(state)
    }

    fn replace_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) -> Result<(), String> {
        let config = toml::from_str::<Config>(&snapshot.text)
            .map_err(|error| format!("aterm.toml is not a valid aterm config: {error}"))?;
        let fields = prefs::editable_fields(&config);
        let raw_values = snapshot.values()?;
        let smart_titles_changed = prefs::SMART_TITLE_KEYS
            .iter()
            .any(|key| self.raw_values.get(*key) != raw_values.get(*key));
        self.legacy.fields = fields;
        self.legacy.trail_pack_ids = snapshot.assets.trail_packs.ids.clone();
        self.legacy.selected = self
            .legacy
            .selected
            .min(self.legacy.fields.len().saturating_sub(1));
        self.legacy.scroll = 0;
        self.raw_values = raw_values;
        if smart_titles_changed {
            // A status snapshot belongs to the exact provider authority that
            // produced it. Never paint old readiness/locality while a config
            // patch is switching provider, endpoint, model, or consent.
            self.title_summary_health = None;
        }
        self.assets = Arc::clone(&snapshot.assets);
        self.field_inputs.clear();
        self.editing_field = None;
        self.choice_picker = None;
        self.reduce_page_scroll(
            self.legacy.fields.len().saturating_sub(1),
            SettingsPageScrollCommand::Absolute(self.page_scroll),
        );
        Ok(())
    }

    fn reduce_page_scroll(&mut self, limit: usize, command: SettingsPageScrollCommand) {
        self.page_scroll = settings_page_scroll_transition(self.page_scroll, limit, command);
    }

    fn raw_value(&self, key: &str) -> Option<String> {
        self.raw_values.get(key).cloned()
    }

    pub(crate) fn trail_pack_catalog(&self) -> &Arc<crate::app_config::TrailPackCatalog> {
        &self.assets.trail_packs
    }

    pub(crate) fn config_assets(&self) -> &Arc<crate::app_config::ConfigAssetCatalog> {
        &self.assets
    }

    /// Publish a process-runtime Smart Titles observation into this view. The
    /// App owns polling and calls this only on a real change; native Settings
    /// remains an IO-free projection and never probes a provider while painting.
    pub(crate) fn replace_title_summary_health(&mut self, health: TitleSummaryHealth) -> bool {
        if self.title_summary_health.as_ref() == Some(&health) {
            return false;
        }
        self.title_summary_health = Some(health);
        self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
        true
    }

    fn is_explicit(&self, key: &str) -> bool {
        self.raw_values.contains_key(key)
    }

    fn clear_field_edit(&mut self) {
        if let Some(key) = self.editing_field.take() {
            let owned_focus = self.editing_field_has_focus(&key);
            if let Some(input) = self.field_inputs.get_mut(&key) {
                input.cancel_preedit();
            }
            if owned_focus {
                self.common.last_focus = None;
            }
        }
    }

    fn discard_field_edit(&mut self) {
        if let Some(key) = self.editing_field.take() {
            let owned_focus = self.editing_field_has_focus(&key);
            self.field_inputs.remove(&key);
            if owned_focus {
                self.common.last_focus = None;
            }
        }
    }

    fn focus_field(&mut self, focus: Option<&UiKey>) {
        let text_key = focus
            .and_then(|focus| focus.as_str().strip_prefix("settings/control/"))
            .and_then(|key| {
                self.legacy
                    .fields
                    .iter()
                    .find(|field| field.key == key && field_accepts_text_input(field))
                    .map(|field| {
                        (
                            field.key.to_string(),
                            field.seed.clone().unwrap_or_default(),
                        )
                    })
            });
        let Some((key, seed)) = text_key else {
            self.clear_field_edit();
            return;
        };
        if self.editing_field.as_deref() != Some(key.as_str()) {
            self.clear_field_edit();
        }
        self.field_inputs
            .entry(key.clone())
            .or_insert_with(|| crate::native_text_input::TextInputState::new(seed));
        self.editing_field = Some(key);
    }

    fn editing_field_has_focus(&self, key: &str) -> bool {
        self.common.last_focus.as_ref().is_some_and(|focus| {
            focus
                .as_str()
                .strip_prefix("settings/control/")
                .is_some_and(|focused| focused == key)
        })
    }

    fn action_keeps_field_focus(&self, action: &str) -> bool {
        self.editing_field.as_deref().is_some_and(|key| {
            action
                .strip_prefix("settings/set/")
                .is_some_and(|target| target == key)
        })
    }

    fn config_key_pending(&self, key: &str) -> bool {
        self.pending.values().any(|pending| {
            matches!(
                pending,
                PendingAction::Config(patch)
                    if patch.edits.iter().any(|edit| edit.key == key)
            )
        })
    }

    fn config_patch_pending(&self) -> bool {
        self.pending
            .values()
            .any(|pending| matches!(pending, PendingAction::Config(_)))
    }

    fn clear_search(&mut self) -> bool {
        let changed = !self.search.is_empty()
            || !self.search_input.value().is_empty()
            || self.legacy.searching
            || !self.legacy.query.is_empty();
        if changed {
            self.search_input = crate::native_text_input::TextInputState::new(String::new());
            self.search.clear();
            self.legacy.query.clear();
            self.legacy.searching = false;
            self.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
        }
        changed
    }

    pub(crate) fn navigate(&mut self, route: SettingsRoute) {
        self.clear_field_edit();
        let search_cleared = self.clear_search();
        if self.route == route && !search_cleared {
            return;
        }
        self.route = route;
        self.common.last_focus = Some(UiKey::new(format!("settings/page{}", route.path())));
        self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
        self.feedback = None;
        self.choice_picker = None;
        self.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
    }

    pub(crate) fn set_search(&mut self, query: String) {
        self.clear_field_edit();
        self.search = query.clone();
        self.legacy.query = query;
        self.legacy.searching = true;
        self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
        self.reduce_page_scroll(0, SettingsPageScrollCommand::Absolute(0));
    }

    fn sync_search_from_input(&mut self) {
        self.set_search(self.search_input.value().to_string());
    }

    fn search_has_focus(&self) -> bool {
        self.common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str() == "settings/search")
    }

    fn focused_setting_key(&self) -> Option<&str> {
        self.common
            .last_focus
            .as_ref()?
            .as_str()
            .strip_prefix("settings/control/")
    }

    /// Exact paint cadence for the renderer preview currently authored by this
    /// view. The event-loop scheduler consumes the same normalized spec as the
    /// semantic painter, preventing a second, drifting animation predicate.
    pub(crate) fn preview_animation(
        &self,
        phase_ms: u64,
        motion: crate::native_app::ViewMotionCx,
        terminal_font_px: f32,
        terminal_theme: aterm_render::Theme,
    ) -> PreviewAnimation {
        let Some(spec) =
            renderer_preview_spec(self, phase_ms, motion, terminal_font_px, terminal_theme)
        else {
            return PreviewAnimation::None;
        };
        spec.animation()
    }

    /// Pure candidate projection for the host's pre-view preparation phase.
    /// Request submission/polling occurs there, never during semantic compile
    /// or paint.
    pub(crate) fn preview_font_candidate(
        &self,
        _phase_ms: u64,
        _motion: crate::native_app::ViewMotionCx,
        _terminal_font_px: f32,
        _terminal_theme: aterm_render::Theme,
    ) -> Option<crate::widget::SemanticFontCandidate> {
        focused_preview_key(self).map(|_| preview_font_candidate(self))
    }

    /// Escape unwinds the deepest transient state first: field edit, global
    /// search, then compact-detail navigation. Keeping this pure view state
    /// makes keyboard, menu, controller, and accessibility actions converge.
    fn cancel_transient(&mut self) {
        if self.choice_picker.take().is_some() {
            return;
        }
        if self.editing_field.is_some() {
            self.discard_field_edit();
            return;
        }
        if self.search_has_focus()
            || !self.search_input.value().is_empty()
            || !self.search.is_empty()
        {
            self.clear_search();
            self.common.last_focus =
                Some(UiKey::new(format!("settings/page{}", self.route.path())));
            self.common.presentation_revision = self.common.presentation_revision.saturating_add(1);
            return;
        }
        if self.route != SettingsRoute::Home {
            self.navigate(SettingsRoute::Home);
        }
    }
}

/// Process-wide Settings controller. One instance may have one implicit view per
/// window; updater projection and committed revisions are shared across them.
pub(crate) struct SettingsApp {
    config_revision: u64,
    update_revision: u64,
    update: UpdateState,
    packages_revision: u64,
    packages: PackagesState,
}

impl SettingsApp {
    pub(crate) fn new(update: UpdateState) -> Self {
        Self::new_at_config_revision(update, 1)
    }

    /// Construct against the exact snapshot used to seed the first Settings
    /// view.  The transaction service permits safe stale rebases, but a freshly
    /// opened Settings controller is not stale: using revision 1 after the
    /// watcher has already advanced would manufacture a conflict for a key the
    /// user is looking at in the current snapshot.
    pub(crate) fn new_at_config_revision(update: UpdateState, config_revision: u64) -> Self {
        Self {
            config_revision: config_revision.max(1),
            update_revision: 1,
            update,
            packages_revision: 1,
            // Honest zero state until the host publishes a worker observation
            // (the page says "Reading package status…", claims nothing).
            packages: PackagesState::unobserved(),
        }
    }

    pub(crate) fn replace_update(&mut self, update: UpdateState, revision: u64) {
        self.update = update;
        self.update_revision = revision.max(self.update_revision);
    }

    pub(crate) fn replace_packages(&mut self, packages: PackagesState, revision: u64) {
        self.packages = packages;
        self.packages_revision = revision.max(self.packages_revision);
    }

    fn reduce_text_input(
        &mut self,
        view: &mut SettingsViewState,
        event: TextInputEvent,
        cx: &mut UpdateCx<'_>,
    ) {
        if view.editing_field.is_none() {
            let focus = view.common.last_focus.clone();
            view.focus_field(focus.as_ref());
        }
        let editing = view
            .editing_field
            .clone()
            .filter(|key| view.editing_field_has_focus(key));
        if view.editing_field.is_some() && editing.is_none() {
            view.clear_field_edit();
        }
        let input = if let Some(key) = editing.as_ref() {
            view.field_inputs.get_mut(key)
        } else if view.search_has_focus() {
            Some(&mut view.search_input)
        } else {
            None
        };
        let Some(input) = input else {
            return;
        };
        let submit = matches!(event, TextInputEvent::Submit);
        let cancel = matches!(event, TextInputEvent::Cancel);
        match event {
            TextInputEvent::Preedit(text) => input.set_preedit(text, None),
            TextInputEvent::Commit(text) => input.commit_preedit(&text),
            TextInputEvent::Backspace => input.delete_backward(),
            TextInputEvent::Delete => input.delete_forward(),
            TextInputEvent::Left { extend } => input.move_left(extend),
            TextInputEvent::Right { extend } => input.move_right(extend),
            TextInputEvent::Home { extend } => input.move_to_start(extend),
            TextInputEvent::End { extend } => input.move_to_end(extend),
            TextInputEvent::KillToEnd => input.kill_to_end(),
            TextInputEvent::KillToStart => input.kill_to_start(),
            TextInputEvent::DeleteWordBackward => input.delete_word_backward(),
            TextInputEvent::SelectAll => input.select_all(),
            TextInputEvent::Undo => {
                input.undo();
            }
            TextInputEvent::Redo => {
                input.redo();
            }
            TextInputEvent::Submit => input.cancel_preedit(),
            TextInputEvent::Cancel => input.cancel_preedit(),
        }

        if cancel {
            view.discard_field_edit();
            cx.repaint(crate::native_app::DamageRegion::All);
            return;
        }
        if let Some(key) = editing {
            if submit {
                if Self::reject_pending_key(view, &key, cx) {
                    view.clear_field_edit();
                    return;
                }
                let value = view
                    .field_inputs
                    .get(&key)
                    .map(|input| input.value().trim().to_string())
                    .filter(|value| !value.is_empty());
                if view.legacy.fields.iter().any(|field| field.key == key) {
                    let patch = ConfigPatch {
                        base_revision: self.config_revision,
                        edits: vec![ConfigEdit {
                            key: key.clone(),
                            expected: ExpectedConfigValue::Exact(view.raw_value(&key)),
                            value,
                        }],
                    };
                    let operation = cx.config_patch(patch.clone());
                    view.pending.insert(operation, PendingAction::Config(patch));
                    view.feedback = Some("Applying…".to_string());
                }
                view.clear_field_edit();
            }
        } else {
            view.sync_search_from_input();
        }
        cx.repaint(crate::native_app::DamageRegion::All);
    }

    /// Apply a pointer-resolved byte boundary to the reducer-owned field state.
    /// The host contributes only semantic geometry data; it never reaches into
    /// `TextInputState`. Returning `true` means the action was a recognized
    /// Settings text target (including a safely ignored stale field key).
    fn reduce_text_pointer(
        view: &mut SettingsViewState,
        action: &str,
        projected_byte: usize,
        extend: bool,
        cx: &mut UpdateCx<'_>,
    ) -> bool {
        if action == "settings/search" {
            view.clear_field_edit();
            apply_pointer_selection(&mut view.search_input, projected_byte, extend);
            view.common.last_focus = Some(UiKey::new("settings/search"));
            view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
            cx.repaint(crate::native_app::DamageRegion::All);
            return true;
        }

        let Some(key) = action.strip_prefix("settings/set/") else {
            return false;
        };
        let Some(field) = view.legacy.fields.iter().find(|field| field.key == key) else {
            // An action from a prior compiled Settings revision is consumed but
            // cannot create arbitrary field state or mutate configuration.
            return true;
        };
        if !field_accepts_text_input(field) {
            // Typed pointer positions are valid only for actual text fields.
            return true;
        }
        let seed = field.seed.clone().unwrap_or_default();
        let input = view
            .field_inputs
            .entry(key.to_string())
            .or_insert_with(|| crate::native_text_input::TextInputState::new(seed));
        apply_pointer_selection(input, projected_byte, extend);
        view.editing_field = Some(key.to_string());
        view.common.last_focus = Some(UiKey::new(format!("settings/control/{key}")));
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
        cx.repaint(crate::native_app::DamageRegion::All);
        true
    }

    fn reject_pending_key(view: &mut SettingsViewState, key: &str, cx: &mut UpdateCx<'_>) -> bool {
        if !view.config_key_pending(key) {
            return false;
        }
        view.feedback = Some("That setting is already being applied…".to_string());
        cx.repaint(crate::native_app::DamageRegion::All);
        true
    }

    fn reduce_slider_step(
        &mut self,
        view: &mut SettingsViewState,
        delta: isize,
        big: bool,
        cx: &mut UpdateCx<'_>,
    ) {
        let Some(key) = view.focused_setting_key() else {
            return;
        };
        let key = key.to_string();
        if Self::reject_pending_key(view, &key, cx) {
            return;
        }
        let Some(field) = view.legacy.fields.iter().find(|field| field.key == key) else {
            return;
        };
        if numeric_slider_value(field).is_none() {
            return;
        }
        let expected = view.raw_value(field.key);
        let Some((key, value)) = crate::settings::step_edit(field, delta, big) else {
            return;
        };
        let patch = ConfigPatch {
            base_revision: self.config_revision,
            edits: vec![ConfigEdit {
                key: key.to_string(),
                expected: ExpectedConfigValue::Exact(expected),
                value,
            }],
        };
        let operation = cx.config_patch(patch.clone());
        view.pending.insert(operation, PendingAction::Config(patch));
        view.feedback = Some("Applying…".to_string());
        cx.repaint(crate::native_app::DamageRegion::All);
    }

    fn reduce_action(
        &mut self,
        view: &mut SettingsViewState,
        invocation: ActionInvocation,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        let action = invocation.id.as_str();
        if !view.action_keeps_field_focus(action) {
            view.clear_field_edit();
        }
        if let Some(key) = action.strip_prefix("settings/set/")
            && Self::reject_pending_key(view, key, cx)
        {
            view.clear_field_edit();
            return EventResult::Handled;
        }
        if action == "settings/search-clear" {
            view.clear_field_edit();
            let _ = view.clear_search();
            // Clearing is an editing action, not navigation: keep the full
            // search bar active so touch, keyboard, switch control, and IME
            // users can immediately enter a replacement query.
            view.common.last_focus = Some(UiKey::new("settings/search"));
            view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }
        if let Some(SemanticInput::TextPosition { byte, extend }) = invocation.value.as_ref() {
            // A coordinate-bearing payload never falls through to a switch,
            // slider, enum, or generic activation path. Unknown/stale targets
            // fail closed without manufacturing a config patch.
            let _ = Self::reduce_text_pointer(view, action, *byte, *extend, cx);
            return EventResult::Handled;
        }
        if let Some(path) = action.strip_prefix("settings/route")
            && let Some(route) = SettingsRoute::from_path(path)
        {
            view.navigate(route);
            cx.invalidate_presentation();
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }

        if action == "settings/search" {
            if let Some(SemanticInput::Text(query)) = invocation.value {
                view.search_input = crate::native_text_input::TextInputState::new(query.clone());
                view.set_search(query);
            } else {
                view.clear_field_edit();
                view.common.last_focus = Some(UiKey::new("settings/search"));
            }
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }

        if action == "settings/page-up" {
            let limit = settings_page_scroll_limit(
                view,
                &self.update.projection(),
                &self.packages.projection(),
            );
            view.reduce_page_scroll(limit, SettingsPageScrollCommand::Previous);
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }
        if action == "settings/page-down" {
            let limit = settings_page_scroll_limit(
                view,
                &self.update.projection(),
                &self.packages.projection(),
            );
            view.reduce_page_scroll(limit, SettingsPageScrollCommand::Next);
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }
        if action == "settings/page-scroll" {
            if let Some(SemanticInput::Number(offset)) = invocation.value {
                let limit = settings_page_scroll_limit(
                    view,
                    &self.update.projection(),
                    &self.packages.projection(),
                );
                view.reduce_page_scroll(
                    limit,
                    SettingsPageScrollCommand::Absolute(offset.max(0.0) as usize),
                );
                cx.repaint(crate::native_app::DamageRegion::All);
            }
            return EventResult::Handled;
        }

        if action == "settings/choice-close" {
            view.choice_picker = None;
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }
        if action == "settings/choice-page-prev" || action == "settings/choice-page-next" {
            if let Some(picker) = view.choice_picker.as_mut() {
                picker.page(if action.ends_with("next") { 1 } else { -1 });
                // Moving between pages is itself preview navigation. Focus the
                // first newly-visible option so keyboard/controller users see
                // the candidate before committing it, exactly like pointer
                // users do when activating an option.
                view.common.last_focus = Some(UiKey::new(format!(
                    "settings/choice/{}/{}",
                    picker.key, picker.offset
                )));
                cx.repaint(crate::native_app::DamageRegion::All);
            }
            return EventResult::Handled;
        }
        if let Some(rest) = action.strip_prefix("settings/choice/")
            && let Some((key, raw_index)) = rest.rsplit_once('/')
            && let Ok(index) = raw_index.parse::<usize>()
        {
            if Self::reject_pending_key(view, key, cx) {
                view.choice_picker = None;
                return EventResult::Handled;
            }
            let value = view
                .choice_picker
                .as_ref()
                .filter(|picker| picker.key == key)
                .and_then(|picker| picker.options.get(index))
                .map(|option| option.value.clone());
            if let Some(value) = value {
                let patch = ConfigPatch {
                    base_revision: self.config_revision,
                    edits: vec![ConfigEdit {
                        key: key.to_string(),
                        expected: ExpectedConfigValue::Exact(view.raw_value(key)),
                        value,
                    }],
                };
                let operation = cx.config_patch(patch.clone());
                view.pending.insert(operation, PendingAction::Config(patch));
                view.choice_picker = None;
                view.feedback = Some("Applying…".to_string());
                view.common.last_focus = Some(UiKey::new(format!("settings/control/{key}")));
                cx.repaint(crate::native_app::DamageRegion::All);
            }
            return EventResult::Handled;
        }

        if let Some(key) = action.strip_prefix("settings/set/") {
            let Some(field) = view.legacy.fields.iter().find(|field| field.key == key) else {
                return EventResult::Handled;
            };
            if invocation.value.is_none()
                && let Some(options) = choices_for_field(field, &view.legacy.trail_pack_ids)
            {
                if view
                    .choice_picker
                    .as_ref()
                    .is_some_and(|picker| picker.key == key)
                {
                    view.choice_picker = None;
                } else {
                    let picker = ChoicePicker::new(
                        key,
                        options,
                        SettingsState::display_value(field),
                        view.is_explicit(key),
                        &effective_default_for_field(key),
                    );
                    // Opening a picker must not visually jump the preview to
                    // "Use default". Begin on the current committed choice;
                    // subsequent focus movement previews candidates without a
                    // write until activation.
                    let first = picker.selected;
                    view.choice_picker = Some(picker);
                    view.common.last_focus =
                        Some(UiKey::new(format!("settings/choice/{key}/{first}")));
                }
                view.clear_field_edit();
                cx.repaint(crate::native_app::DamageRegion::All);
                return EventResult::Handled;
            }
            if invocation.value.is_none() && numeric_slider_value(field).is_some() {
                view.clear_field_edit();
                view.common.last_focus = Some(UiKey::new(format!("settings/control/{key}")));
                cx.repaint(crate::native_app::DamageRegion::All);
                return EventResult::Handled;
            }
            if invocation.value.is_none()
                && !matches!(
                    field.kind,
                    EditKind::Bool | EditKind::Enum { .. } | EditKind::Theme
                )
            {
                view.editing_field = Some(key.to_string());
                view.field_inputs.entry(key.to_string()).or_insert_with(|| {
                    crate::native_text_input::TextInputState::new(
                        field.seed.clone().unwrap_or_default(),
                    )
                });
                view.common.last_focus = Some(UiKey::new(format!("settings/control/{key}")));
                cx.repaint(crate::native_app::DamageRegion::All);
                return EventResult::Handled;
            }
            let value = if let Some(SemanticInput::Number(value)) = invocation.value.as_ref()
                && let Some(range) = prefs::range_of(field.key)
            {
                let Some(value) = normalize_slider_value(*value, range) else {
                    view.feedback = Some("Value must be a finite number".to_string());
                    cx.repaint(crate::native_app::DamageRegion::All);
                    return EventResult::Handled;
                };
                Some(value)
            } else {
                semantic_value_for_field(field, invocation.value, &view.legacy.trail_pack_ids)
            };
            let patch = ConfigPatch {
                base_revision: self.config_revision,
                edits: vec![ConfigEdit {
                    key: key.to_string(),
                    expected: ExpectedConfigValue::Exact(view.raw_value(key)),
                    value,
                }],
            };
            let operation = cx.config_patch(patch.clone());
            view.pending.insert(operation, PendingAction::Config(patch));
            view.feedback = Some("Applying…".to_string());
            cx.repaint(crate::native_app::DamageRegion::All);
            return EventResult::Handled;
        }

        if let Some(key) = action.strip_prefix("settings/reset/") {
            if view.legacy.fields.iter().any(|field| field.key == key) {
                if Self::reject_pending_key(view, key, cx) {
                    return EventResult::Handled;
                }
                let patch = ConfigPatch {
                    base_revision: self.config_revision,
                    edits: vec![ConfigEdit {
                        key: key.to_string(),
                        expected: ExpectedConfigValue::Exact(view.raw_value(key)),
                        value: None,
                    }],
                };
                let operation = cx.config_patch(patch.clone());
                view.pending.insert(operation, PendingAction::Config(patch));
                view.feedback = Some("Resetting…".to_string());
                cx.repaint(crate::native_app::DamageRegion::All);
            }
            return EventResult::Handled;
        }

        match action {
            "settings/reset-all" => {
                if view.config_patch_pending() {
                    view.feedback = Some("Settings changes are already being applied…".to_string());
                    cx.repaint(crate::native_app::DamageRegion::All);
                    return EventResult::Handled;
                }
                let patch = ConfigPatch {
                    base_revision: self.config_revision,
                    edits: view
                        .legacy
                        .fields
                        .iter()
                        .map(|field| ConfigEdit {
                            key: field.key.to_string(),
                            expected: ExpectedConfigValue::Any,
                            value: None,
                        })
                        .collect(),
                };
                let operation = cx.config_patch(patch.clone());
                view.pending.insert(operation, PendingAction::Config(patch));
                view.feedback = Some("Resetting all settings…".to_string());
                cx.repaint(crate::native_app::DamageRegion::All);
                EventResult::Handled
            }
            "settings/undo" => {
                if view.config_patch_pending() {
                    view.feedback =
                        Some("Finish applying the current change before Undo.".to_string());
                    cx.repaint(crate::native_app::DamageRegion::All);
                    return EventResult::Handled;
                }
                if let Some(token) = view.last_undo.take() {
                    let operation = cx.config_undo(token);
                    view.pending.insert(operation, PendingAction::Undo);
                    view.feedback = Some("Undoing…".to_string());
                    cx.repaint(crate::native_app::DamageRegion::All);
                }
                EventResult::Handled
            }
            "about/copy-build-info" => {
                let operation = cx.clipboard(ClipboardRequest::CopyText {
                    text: crate::about::provenance_text(),
                    sensitive: false,
                });
                view.pending.insert(operation, PendingAction::Clipboard);
                view.feedback = Some("Copying build information…".to_string());
                EventResult::Handled
            }
            "about/open-site" => {
                if let Some(uri) = crate::about::site_url(&view.about) {
                    let operation = cx.open_external(ExternalOpenRequest {
                        uri,
                        user_initiated: true,
                    });
                    view.pending.insert(operation, PendingAction::External);
                    view.feedback = Some("Opening project site…".to_string());
                }
                EventResult::Handled
            }
            "updates/check" => {
                let update = self.update.projection();
                if !update.enabled {
                    view.feedback =
                        Some("Update checks are unavailable for this installation.".to_string());
                } else if update.checking {
                    view.feedback = Some("An update check is already in progress.".to_string());
                } else {
                    let operation = cx.update(UpdateRequest::Check);
                    view.pending.insert(operation, PendingAction::Update);
                    view.feedback = Some("Checking for updates…".to_string());
                }
                cx.repaint(crate::native_app::DamageRegion::All);
                EventResult::Handled
            }
            "updates/install-relaunch" => {
                let operation = cx.update(UpdateRequest::InstallAndRelaunch);
                view.pending.insert(operation, PendingAction::Update);
                view.feedback = Some("Checking relaunch safety…".to_string());
                EventResult::Handled
            }
            "updates/retry" => {
                let operation = cx.update(UpdateRequest::Retry);
                view.pending.insert(operation, PendingAction::Update);
                view.feedback = Some("Retrying update…".to_string());
                EventResult::Handled
            }
            "updates/install-when-safe" => {
                let operation = cx.update(UpdateRequest::InstallWhenSafe);
                view.pending.insert(operation, PendingAction::Update);
                view.feedback = Some("Update will install at the next safe quit.".to_string());
                EventResult::Handled
            }
            "packages/check" => {
                self.reduce_packages_verb(
                    view,
                    cx,
                    PackagesRequest::CheckUpdate,
                    "Checking toolchain packages…",
                );
                EventResult::Handled
            }
            "packages/install-default" => {
                self.reduce_packages_verb(
                    view,
                    cx,
                    PackagesRequest::InstallDefaultSet,
                    "Installing the ALab toolset…",
                );
                EventResult::Handled
            }
            _ => EventResult::Bubble,
        }
    }

    /// Shared admission for the two Packages action buttons: refuse honestly
    /// (unavailable / inert / busy) without minting an effect, else hand the
    /// request to the host executor and track the pending operation.
    fn reduce_packages_verb(
        &mut self,
        view: &mut SettingsViewState,
        cx: &mut UpdateCx<'_>,
        request: PackagesRequest,
        feedback: &str,
    ) {
        let packages = self.packages.projection();
        if !packages.observed {
            view.feedback = Some("Package status is still being read…".to_string());
        } else if !packages.available {
            view.feedback =
                Some("The bundled atpkg binary is not present beside this executable.".to_string());
        } else if !packages.manager_enabled {
            // The projection's detail line names the TRUE inert cause
            // (ATPKG_DISABLE opt-out vs no pinned root key) — reuse it rather
            // than asserting one cause here.
            view.feedback = Some(
                packages
                    .detail
                    .clone()
                    .unwrap_or_else(|| "The package manager is inert in this build.".to_string()),
            );
        } else if packages.busy.is_some() {
            view.feedback = Some("A packages operation is already running.".to_string());
        } else if packages.refreshing {
            // A silent status refresh holds the one-worker slot for a moment;
            // refuse in user voice instead of letting the host drop the click.
            view.feedback =
                Some("Still collecting package status — try again in a moment.".to_string());
        } else {
            let operation = cx.packages(request);
            view.pending.insert(operation, PendingAction::Packages);
            view.feedback = Some(feedback.to_string());
        }
        cx.repaint(crate::native_app::DamageRegion::All);
    }

    fn finish_config(
        &mut self,
        view: &mut SettingsViewState,
        operation: OperationId,
        outcome: ConfigPatchOutcome,
    ) {
        let Some(pending) = view.pending.remove(&operation) else {
            return;
        };
        match outcome {
            ConfigPatchOutcome::Applied { revision, undo } => {
                self.config_revision = self.config_revision.max(revision);
                if let PendingAction::Config(patch) = pending {
                    for edit in patch.edits {
                        if let Some(value) = edit.value.as_ref() {
                            view.raw_values.insert(edit.key.clone(), value.clone());
                        } else {
                            view.raw_values.remove(&edit.key);
                        }
                        if let Some(field) = view
                            .legacy
                            .fields
                            .iter_mut()
                            .find(|field| field.key == edit.key)
                        {
                            // A Boolean seed is its effective display state. On
                            // reset, keep it until the following ConfigChanged
                            // snapshot supplies the resolved default; treating
                            // `None` as false here would make default-on controls
                            // visibly lie for one reducer turn.
                            if edit.value.is_some() || !matches!(field.kind, EditKind::Bool) {
                                field.seed = edit.value;
                            }
                        }
                    }
                }
                view.last_undo = undo;
                view.feedback = Some(if undo.is_some() {
                    "Applied · Undo available".to_string()
                } else {
                    "Applied".to_string()
                });
            }
            ConfigPatchOutcome::Conflict { revision } => {
                self.config_revision = self.config_revision.max(revision);
                view.feedback = Some("Changed externally · Review before retrying".to_string());
            }
            ConfigPatchOutcome::Rejected { message } => {
                view.feedback = Some(format!("Couldn’t apply: {message}"));
            }
        }
        view.common.presentation_revision = view.common.presentation_revision.saturating_add(1);
    }

    fn finish_external(
        view: &mut SettingsViewState,
        operation: OperationId,
        outcome: ExternalOpenOutcome,
    ) {
        if !matches!(
            view.pending.remove(&operation),
            Some(PendingAction::External)
        ) {
            return;
        }
        view.feedback = Some(match outcome {
            ExternalOpenOutcome::Opened => "Project site opened".to_string(),
            ExternalOpenOutcome::Denied { message } => format!("Open denied: {message}"),
            ExternalOpenOutcome::Failed { message } => format!("Couldn’t open site: {message}"),
        });
    }

    fn finish_update(view: &mut SettingsViewState, operation: OperationId, outcome: UpdateOutcome) {
        if !matches!(view.pending.remove(&operation), Some(PendingAction::Update)) {
            return;
        }
        view.feedback = Some(match outcome {
            UpdateOutcome::Accepted => "Update request accepted".to_string(),
            UpdateOutcome::InstalledNeedsRelaunch { build, message } => {
                format!("Build {build} installed · {message}")
            }
            UpdateOutcome::Deferred { reason } => {
                format!("Update retained until terminal activity settles · {reason}")
            }
            UpdateOutcome::Blocked { reasons } => {
                format!("Before relaunch: {}", reasons.join(" · "))
            }
            UpdateOutcome::Failed { message } => format!("Update failed: {message}"),
        });
    }

    fn finish_packages(
        view: &mut SettingsViewState,
        operation: OperationId,
        outcome: PackagesOutcome,
    ) {
        if !matches!(
            view.pending.remove(&operation),
            Some(PendingAction::Packages)
        ) {
            return;
        }
        view.feedback = Some(match outcome {
            // Accepted = the worker is running; its completion arrives through
            // the packages projection revision, which repaints the status card.
            PackagesOutcome::Accepted => "Packages request accepted".to_string(),
            PackagesOutcome::Blocked { message } => format!("Packages: {message}"),
            PackagesOutcome::Failed { message } => format!("Packages request failed: {message}"),
        });
    }

    fn finish_clipboard(
        view: &mut SettingsViewState,
        operation: OperationId,
        outcome: ClipboardOutcome,
    ) {
        if !matches!(
            view.pending.remove(&operation),
            Some(PendingAction::Clipboard)
        ) {
            return;
        }
        view.feedback = Some(match outcome {
            ClipboardOutcome::Copied => "Build information copied".to_string(),
            ClipboardOutcome::Denied { message } => format!("Copy denied: {message}"),
            ClipboardOutcome::Failed { message } => format!("Couldn’t copy: {message}"),
        });
    }
}

fn settings_field_result_count(view: &SettingsViewState) -> usize {
    let query = view.search.trim().to_ascii_lowercase();
    let global_search = !query.is_empty();
    if !global_search && view.route == SettingsRoute::Home {
        return 0;
    }
    let modified_only = !global_search && view.route == SettingsRoute::Modified;
    view.legacy
        .fields
        .iter()
        .filter(|field| {
            modified_only
                || global_search
                || view
                    .route
                    .section()
                    .is_none_or(|section| prefs::section_of(field.key) == section)
        })
        .filter(|field| !modified_only || view.is_explicit(field.key))
        .filter(|field| field_match_score(field, &query).is_some())
        .count()
}

fn settings_page_scroll_limit(
    view: &SettingsViewState,
    update: &UpdateProjection,
    packages: &PackagesProjection,
) -> usize {
    if view.search.trim().is_empty()
        && let Some(limit) = special_page_scroll_limit(view.route, update, packages)
    {
        return limit;
    }
    let fields = settings_field_result_count(view);
    if focused_preview_key(view).is_some() && !prefers_smart_title_health(view) {
        // A short viewport exposes the demonstration as virtual slice zero;
        // slice one starts at the first real control. The viewport-independent
        // reducer therefore admits one extra bounded offset so a one-row final
        // window can still start at the last field.
        fields
    } else {
        fields.saturating_sub(1)
    }
}

fn prefers_smart_title_health(state: &SettingsViewState) -> bool {
    if state.route != SettingsRoute::WindowTabs || !state.search.trim().is_empty() {
        return false;
    }
    let focused_setting = state.focused_setting_key();
    let focused_visual_setting =
        focused_setting.is_some_and(|key| prefs::VISUAL_PREVIEW_KEYS.contains(&key));
    let focused_smart_title =
        focused_setting.is_some_and(|key| prefs::SMART_TITLE_KEYS.contains(&key));
    !focused_visual_setting && (state.title_summary_health.is_some() || focused_smart_title)
}

impl NativeAppModel for SettingsApp {
    type ViewState = SettingsViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: crate::native_app::AppKind::Settings,
            name: "Settings",
            icon: AppIcon::Settings,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        let handled = match event {
            AppEvent::Action(invocation) => return self.reduce_action(view, invocation, cx),
            AppEvent::FocusChanged(focus) => {
                view.focus_field(focus.as_ref());
                view.common.last_focus = focus;
                true
            }
            AppEvent::InsertText(text) => {
                if view
                    .editing_field
                    .as_deref()
                    .is_some_and(|key| !view.editing_field_has_focus(key))
                {
                    view.clear_field_edit();
                }
                if view.editing_field.is_none() {
                    let focus = view.common.last_focus.clone();
                    view.focus_field(focus.as_ref());
                }
                if let Some(key) = view.editing_field.clone()
                    && view.editing_field_has_focus(&key)
                    && let Some(input) = view.field_inputs.get_mut(&key)
                {
                    input.insert(&text);
                    true
                } else if view.search_has_focus() {
                    view.search_input.insert(&text);
                    view.sync_search_from_input();
                    true
                } else {
                    false
                }
            }
            AppEvent::TextInput(TextInputEvent::Undo)
                if view.editing_field.is_none()
                    && !view.search_has_focus()
                    && view.last_undo.is_some() =>
            {
                return self.reduce_action(
                    view,
                    ActionInvocation {
                        id: ActionId::new("settings/undo"),
                        value: None,
                    },
                    cx,
                );
            }
            AppEvent::TextInput(TextInputEvent::Left { extend })
                if view.focused_setting_key().is_some_and(|key| {
                    view.legacy
                        .fields
                        .iter()
                        .find(|field| field.key == key)
                        .is_some_and(|field| numeric_slider_value(field).is_some())
                }) =>
            {
                self.reduce_slider_step(view, -1, extend, cx);
                true
            }
            AppEvent::TextInput(TextInputEvent::Right { extend })
                if view.focused_setting_key().is_some_and(|key| {
                    view.legacy
                        .fields
                        .iter()
                        .find(|field| field.key == key)
                        .is_some_and(|field| numeric_slider_value(field).is_some())
                }) =>
            {
                self.reduce_slider_step(view, 1, extend, cx);
                true
            }
            AppEvent::TextInput(TextInputEvent::Cancel) => {
                view.cancel_transient();
                true
            }
            AppEvent::TextInput(event) => {
                self.reduce_text_input(view, event, cx);
                true
            }
            AppEvent::ScrollLines(lines) => {
                let limit = settings_page_scroll_limit(
                    view,
                    &self.update.projection(),
                    &self.packages.projection(),
                );
                view.reduce_page_scroll(limit, SettingsPageScrollCommand::Lines(lines));
                true
            }
            AppEvent::ConfigChanged(snapshot) => {
                self.config_revision = self.config_revision.max(snapshot.revision);
                match view.replace_snapshot(&snapshot) {
                    Ok(()) => {
                        if view
                            .feedback
                            .as_deref()
                            .is_some_and(|message| message.starts_with("Config reload failed:"))
                        {
                            view.feedback = None;
                        }
                    }
                    Err(message) => {
                        view.feedback = Some(format!("Config reload failed: {message}"));
                    }
                }
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                true
            }
            AppEvent::UpdateChanged { revision } => {
                self.update_revision = self.update_revision.max(revision);
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                true
            }
            AppEvent::PackagesChanged { revision } => {
                self.packages_revision = self.packages_revision.max(revision);
                view.common.presentation_revision =
                    view.common.presentation_revision.saturating_add(1);
                true
            }
            AppEvent::ConfigPatchFinished { operation, outcome } => {
                self.finish_config(view, operation, outcome);
                true
            }
            AppEvent::ExternalOpenFinished { operation, outcome } => {
                Self::finish_external(view, operation, outcome);
                true
            }
            AppEvent::UpdateFinished { operation, outcome } => {
                Self::finish_update(view, operation, outcome);
                true
            }
            AppEvent::PackagesFinished { operation, outcome } => {
                Self::finish_packages(view, operation, outcome);
                true
            }
            AppEvent::ClipboardFinished { operation, outcome } => {
                Self::finish_clipboard(view, operation, outcome);
                true
            }
            _ => false,
        };
        if handled {
            cx.repaint(crate::native_app::DamageRegion::All);
            EventResult::Handled
        } else {
            EventResult::Bubble
        }
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        settings_tree(
            view,
            &self.update.projection(),
            &self.packages.projection(),
            cx,
        )
    }

    fn commands(&self, view: &Self::ViewState, out: &mut Vec<Command>) {
        out.push(Command {
            id: ActionId::new("settings/search"),
            title: "Settings: Search".to_string(),
            shortcut: Some("Cmd-F".to_string()),
            enabled: true,
        });
        out.push(Command {
            id: ActionId::new("settings/undo"),
            title: "Settings: Undo Last Change".to_string(),
            shortcut: Some("Cmd-Z".to_string()),
            enabled: view.last_undo.is_some() && !view.config_patch_pending(),
        });
        out.push(Command {
            id: ActionId::new("settings/reset-all"),
            title: "Settings: Reset All".to_string(),
            shortcut: None,
            enabled: !view.config_patch_pending(),
        });
        for route in SettingsRoute::ALL {
            out.push(Command {
                id: route_action(route),
                title: format!("Settings: Go to {}", route.label()),
                shortcut: None,
                enabled: route != view.route,
            });
        }
        if view.route == SettingsRoute::SoftwareUpdate {
            let update = self.update.projection();
            out.push(Command {
                id: ActionId::new("updates/check"),
                title: "Updates: Check".to_string(),
                shortcut: None,
                enabled: update.enabled && !update.checking,
            });
        }
        if view.route == SettingsRoute::Packages {
            let packages = self.packages.projection();
            out.push(Command {
                id: ActionId::new("packages/check"),
                title: "Packages: Check & Update Now".to_string(),
                shortcut: None,
                enabled: packages.actions_enabled,
            });
        }
        if view.route == SettingsRoute::About {
            out.push(Command {
                id: ActionId::new("about/copy-build-info"),
                title: "About: Copy Build Information".to_string(),
                shortcut: None,
                enabled: true,
            });
        }
    }

    fn presentation(&self, view: &Self::ViewState) -> AppPresentation {
        let destination = if view.search.trim().is_empty() {
            view.route.label()
        } else {
            "Search Results"
        };
        AppPresentation {
            title: "Settings".to_string(),
            icon: AppIcon::Settings,
            indicators: AppIndicators {
                busy: !view.pending.is_empty(),
                ..AppIndicators::default()
            },
            closable: true,
            tooltip: Some(format!("Settings · {destination}")),
        }
    }

    fn prepare_close(&mut self, _request: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        // Config and update work is service-owned and survives the initiating
        // view. Closing Settings never discards committed configuration.
        CloseReadiness::Ready
    }
}

/// Best-effort raw projection for callers that only have the parsed `Config`
/// (principally unit tests and recovery fallback). Production views are seeded
/// from `ConfigSnapshot`, whose generic TOML projection covers every present
/// key—including future editable fields—without another hand-maintained list.
fn fallback_raw_values(config: &Config, fields: &[EditField]) -> BTreeMap<String, String> {
    fields
        .iter()
        .filter_map(|field| {
            let value = if matches!(field.kind, EditKind::Bool) {
                raw_bool_value(config, field.key).map(|value| value.to_string())
            } else {
                field.seed.clone()
            }?;
            Some((field.key.to_string(), value))
        })
        .collect()
}

fn raw_bool_value(config: &Config, key: &str) -> Option<bool> {
    let sparkle = config.sparkle_words.as_ref();
    let profanity = sparkle.and_then(|s| s.profanity.as_ref());
    let feline = sparkle.and_then(|s| s.feline.as_ref());
    let orca = sparkle.and_then(|s| s.orca.as_ref());
    let ink = sparkle.and_then(|s| s.ink.as_ref());
    let emphasis = sparkle.and_then(|s| s.emphasis.as_ref());
    match key {
        "gpu" => config.gpu,
        // The dotted `[matrix_rain] enabled` key (Recipe A): the CONFIGURED
        // value only — an absent table/key stays `None` (not explicit),
        // mirroring the snapshot projection.
        prefs::EDIT_MATRIX_RAIN_ENABLED => config.matrix_rain.as_ref().and_then(|mr| mr.enabled),
        // The dotted `[packages]` consent switches, same contract as above: the
        // CONFIGURED value only, so the fallback path reports a written
        // `auto_update`/`auto_install` as explicit (correct OCC expectations).
        prefs::EDIT_PACKAGES_AUTO_UPDATE => config.packages.as_ref().and_then(|p| p.auto_update),
        prefs::EDIT_PACKAGES_AUTO_INSTALL => config.packages.as_ref().and_then(|p| p.auto_install),
        // The DEPTH-2 `[sparkle_words.*]` Bool leaves (every Bool registered in
        // `prefs::NESTED_LEAVES` two tables down), same contract again: the
        // snapshot projection flattens dotted paths at any depth, and the
        // fallback must agree on what counts as explicitly configured.
        "sparkle_words.profanity.enabled" => profanity.and_then(|p| p.enabled),
        "sparkle_words.profanity.magic" => profanity.and_then(|p| p.magic),
        "sparkle_words.feline.enabled" => feline.and_then(|f| f.enabled),
        "sparkle_words.feline.idle" => feline.and_then(|f| f.idle),
        "sparkle_words.feline.gaze" => feline.and_then(|f| f.gaze),
        "sparkle_words.feline.magic" => feline.and_then(|f| f.magic),
        "sparkle_words.feline.allow_bare_cat" => feline.and_then(|f| f.allow_bare_cat),
        "sparkle_words.feline.cjk_single_char" => feline.and_then(|f| f.cjk_single_char),
        "sparkle_words.feline.log" => feline.and_then(|f| f.log),
        "sparkle_words.orca.enabled" => orca.and_then(|o| o.enabled),
        "sparkle_words.ink.enabled" => ink.and_then(|i| i.enabled),
        "sparkle_words.ink.loop" => ink.and_then(|i| i.loop_),
        "sparkle_words.emphasis.enabled" => emphasis.and_then(|e| e.enabled),
        "font_synthetic_style" => config.font_synthetic_style,
        "ligatures" => config.ligatures,
        "underline_skip_descenders" => config.underline_skip_descenders,
        "cursor_break_ligatures" => config.cursor_break_ligatures,
        "merged_ligatures" => config.merged_ligatures,
        "selection_inactive" => config.selection_inactive,
        "bold_is_bright" => config.bold_is_bright,
        "font_thicken" => config.font_thicken,
        "cursor_blink" => config.cursor_blink,
        "cursor_trail" => config.cursor_trail,
        "cursor_trail_ring" => config.cursor_trail_ring,
        "cursor_trail_bloom" => config.cursor_trail_bloom,
        "cursor_fire_shimmer" => config.cursor_fire_shimmer,
        "hdr_glow" => config.hdr_glow,
        "load_adaptive_motion" => config.load_adaptive_motion,
        "copy_on_select" => config.copy_on_select,
        "restore_session" => config.restore_session,
        "show_build_badge" => config.show_build_badge,
        "confirm_multiline_paste" => config.confirm_multiline_paste,
        "option_as_meta" => config.option_as_meta,
        "focus_boost" => config.focus_boost,
        "allow_osc52_query" => config.allow_osc52_query,
        "allow_window_ops" => config.allow_window_ops,
        "allow_notifications" => config.allow_notifications,
        "allow_palette_reconfigure" => config.allow_palette_reconfigure,
        "allow_kitty_file_transfer" => config.allow_kitty_file_transfer,
        "show_hud" => config.show_hud,
        "show_resources_hud" => config.show_resources_hud,
        "show_engine_hud" => config.show_engine_hud,
        _ => None,
    }
}

fn semantic_value_for_field(
    field: &EditField,
    input: Option<SemanticInput>,
    pack_ids: &[String],
) -> Option<String> {
    match input {
        Some(SemanticInput::Text(value)) => (!value.trim().is_empty()).then_some(value),
        Some(SemanticInput::Bool(value)) => Some(value.to_string()),
        Some(SemanticInput::Number(value)) => Some(value.to_string()),
        Some(SemanticInput::TextPosition { .. }) => None,
        None if matches!(field.kind, EditKind::Bool) => {
            let current = SettingsState::display_value(field)
                .parse::<bool>()
                .unwrap_or(false);
            Some((!current).to_string())
        }
        // The `cursor_trail_style` control cycles through the loaded `pack:<id>`
        // options too; every other Enum cycles its exact static options.
        None if matches!(field.kind, EditKind::Enum { .. })
            && field.key == prefs::EDIT_CURSOR_TRAIL_STYLE =>
        {
            let dynamic = prefs::cursor_trail_style_options(pack_ids.iter().map(String::as_str));
            let refs: Vec<&str> = dynamic.iter().map(String::as_str).collect();
            cycle_choice(SettingsState::display_value(field), &refs)
        }
        None if let EditKind::Enum { options } = field.kind => {
            cycle_choice(SettingsState::display_value(field), options)
        }
        None if matches!(field.kind, EditKind::Theme) => {
            let options = aterm_types::scheme::builtin_names();
            cycle_choice(SettingsState::display_value(field), &options)
        }
        None => field.seed.clone(),
    }
}

fn field_accepts_text_input(field: &EditField) -> bool {
    !matches!(
        field.kind,
        EditKind::Bool | EditKind::Enum { .. } | EditKind::Theme
    ) && numeric_slider_value(field).is_none()
}

/// Convert a byte in the painted projection (which may include IME marked
/// text) back to committed field coordinates before moving the reducer-owned
/// selection. A click within marked text deterministically chooses the nearer
/// edge of the replaced committed range and cancels composition via
/// `set_selection`.
fn apply_pointer_selection(
    input: &mut crate::native_text_input::TextInputState,
    projected_byte: usize,
    extend: bool,
) {
    let projected_byte = projected_byte.min(input.projection().text.len());
    let committed_byte = input.preedit().map_or(projected_byte, |preedit| {
        let replaced = input.selection().range();
        let marked_start = replaced.start;
        let marked_end = marked_start.saturating_add(preedit.text.len());
        if projected_byte <= marked_start {
            projected_byte
        } else if projected_byte >= marked_end {
            replaced
                .end
                .saturating_add(projected_byte.saturating_sub(marked_end))
        } else if projected_byte - marked_start <= marked_end - projected_byte {
            replaced.start
        } else {
            replaced.end
        }
    });
    let anchor = if extend {
        input.selection().anchor
    } else {
        committed_byte
    };
    input.set_selection(anchor, committed_byte);
}

fn cycle_choice(current: &str, choices: &[&str]) -> Option<String> {
    let current = current.strip_suffix(" (default)").unwrap_or(current).trim();
    let index = choices
        .iter()
        .position(|choice| choice.eq_ignore_ascii_case(current));
    choices
        .get(index.map_or(0, |index| (index + 1) % choices.len().max(1)))
        .map(|choice| (*choice).to_string())
}

fn choice_label(value: &str) -> String {
    if let Some(id) = value.strip_prefix("pack:") {
        return format!("Trail Pack · {}", sentence_case_token(id));
    }
    match value {
        "auto" => "Automatic".to_string(),
        "linear-corrected" => "Linear corrected".to_string(),
        "implicit" => "Implicit bidirectional text".to_string(),
        "disabled" => "Disabled".to_string(),
        "explicit" => "Explicit controls only".to_string(),
        "adaptive" => "Adaptive".to_string(),
        "always" => "Always".to_string(),
        "reduced" => "Reduced motion".to_string(),
        "full" => "Full motion".to_string(),
        value => sentence_case_token(value),
    }
}

fn sentence_case_token(value: &str) -> String {
    let mut label = value.replace(['-', '_'], " ");
    if let Some(first) = label.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    label
}

fn choices_for_field(field: &EditField, trail_pack_ids: &[String]) -> Option<Vec<String>> {
    match field.kind {
        EditKind::Enum { .. } if field.key == prefs::EDIT_CURSOR_TRAIL_STYLE => Some(
            prefs::cursor_trail_style_options(trail_pack_ids.iter().map(String::as_str)),
        ),
        EditKind::Enum { options } => {
            Some(options.iter().map(|option| (*option).to_string()).collect())
        }
        EditKind::Theme => Some(
            aterm_types::scheme::builtin_names()
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

/// Resolve the value produced by removing an override, independent of the
/// current field's configured seed/placeholder. Preference metadata is the
/// single authority for both the normal form and this picker label, so a
/// configured `theme = "Nord"` can never misleadingly offer
/// "Use default · Nord" when reset actually means `Default`.
fn effective_default_for_field(key: &str) -> String {
    let fields = prefs::editable_fields(&Config::default());
    let value = fields.iter().find(|field| field.key == key).map_or_else(
        || "default".to_string(),
        |field| SettingsState::display_value(field).to_string(),
    );
    value
        .strip_suffix(" (default)")
        .unwrap_or(&value)
        .trim()
        .to_string()
}

/// The option currently receiving keyboard focus is a genuine preview
/// candidate even before activation. Pointer activation commits immediately;
/// keyboard/controller browsing therefore gets the richer preview-before-save
/// behavior without inventing a hover-only state channel.
fn picker_choice<'a>(state: &'a SettingsViewState, key: &str) -> Option<&'a ChoiceOption> {
    let picker = state
        .choice_picker
        .as_ref()
        .filter(|picker| picker.key == key)?;
    let focused = state
        .common
        .last_focus
        .as_ref()
        .and_then(|focus| {
            let prefix = format!("settings/choice/{key}/");
            focus.as_str().strip_prefix(&prefix)?.parse::<usize>().ok()
        })
        .filter(|index| picker.visible_range().contains(index));
    let index = focused.unwrap_or(picker.selected);
    picker.options.get(index)
}

fn picker_candidate<'a>(state: &'a SettingsViewState, key: &str) -> Option<&'a str> {
    picker_choice(state, key)?.value.as_deref()
}

fn field_text(state: &SettingsViewState, key: &str, fallback: &str) -> String {
    let field = state.legacy.fields.iter().find(|field| field.key == key);
    let draft = state
        .field_inputs
        .get(key)
        .filter(|_| state.editing_field.as_deref() == Some(key))
        .map(|input| input.projection().text)
        .filter(|value| !value.trim().is_empty());
    let value = match picker_choice(state, key) {
        Some(ChoiceOption {
            value: Some(candidate),
            ..
        }) => candidate.clone(),
        // Building default preference metadata is deliberately lazy: this
        // branch is reached only while the reset candidate is highlighted,
        // never on every semantic preview frame.
        Some(ChoiceOption { value: None, .. }) => effective_default_for_field(key),
        None => draft
            .as_deref()
            .or_else(|| {
                field
                    .map(SettingsState::display_value)
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or(fallback)
            .to_string(),
    };
    value
        .strip_suffix(" (default)")
        .unwrap_or(&value)
        .trim()
        .to_string()
}

fn field_bool(state: &SettingsViewState, key: &str, fallback: bool) -> bool {
    field_text(state, key, if fallback { "true" } else { "false" })
        .parse::<bool>()
        .unwrap_or(fallback)
}

fn field_number<T>(state: &SettingsViewState, key: &str, fallback: T) -> T
where
    T: std::str::FromStr + Copy,
{
    field_text(state, key, "")
        .split_whitespace()
        .next()
        .and_then(|token| token.parse::<T>().ok())
        .unwrap_or(fallback)
}

fn field_color(state: &SettingsViewState, key: &str) -> Option<u32> {
    let value = field_text(state, key, "");
    crate::app_config::parse_hex_color(&value)
        .map(|color| (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b))
}

fn split_font_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn focused_preview_key(state: &SettingsViewState) -> Option<String> {
    let focused = state.common.last_focus.as_ref().map(UiKey::as_str);
    if let Some(key) = focused.and_then(|focus| focus.strip_prefix("settings/control/"))
        && prefs::VISUAL_PREVIEW_KEYS.contains(&key)
    {
        return Some(key.to_string());
    }
    if let Some(choice) = focused.and_then(|focus| focus.strip_prefix("settings/choice/"))
        && let Some((key, _)) = choice.rsplit_once('/')
        && prefs::VISUAL_PREVIEW_KEYS.contains(&key)
    {
        return Some(key.to_string());
    }
    if let Some(key) = state.editing_field.as_deref()
        && prefs::VISUAL_PREVIEW_KEYS.contains(&key)
    {
        return Some(key.to_string());
    }
    let query = state.search.trim().to_ascii_lowercase();
    if !query.is_empty()
        && let Some(field) = state
            .legacy
            .fields
            .iter()
            .filter(|field| prefs::VISUAL_PREVIEW_KEYS.contains(&field.key))
            .filter_map(|field| field_match_score(field, &query).map(|score| (score, field)))
            .min_by_key(|(score, field)| {
                (
                    *score,
                    prefs::section_of(field.key).order_index(),
                    prefs::group_of(field.key).1,
                )
            })
            .map(|(_, field)| field)
    {
        return Some(field.key.to_string());
    }
    state.route.section().and_then(|section| {
        prefs::VISUAL_PREVIEW_KEYS
            .iter()
            .copied()
            .find(|key| prefs::section_of(key) == section)
            .map(str::to_string)
    })
}

fn preview_route_for_key(key: &str) -> Option<SettingsRoute> {
    match prefs::section_of(key) {
        Section::Appearance => Some(SettingsRoute::Appearance),
        Section::Typography => Some(SettingsRoute::TextFonts),
        Section::Cursor => Some(SettingsRoute::CursorMotion),
        Section::Window => Some(SettingsRoute::WindowTabs),
        _ => None,
    }
}

fn preview_font_candidate(state: &SettingsViewState) -> crate::widget::SemanticFontCandidate {
    let family = |key: &str| {
        // Every preserved draft participates in the specimen, not just the
        // field that currently owns focus. This lets someone type Regular,
        // tab into Bold/Italic/Bold Italic, and compare the complete family
        // before Return commits any ConfigPatch.
        if let Some(value) = state.field_inputs.get(key) {
            return (!value.value().trim().is_empty()).then(|| value.value().trim().to_string());
        }
        state
            .raw_value(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let variation_text = field_text(state, prefs::EDIT_FONT_VARIATION, "");
    let mut variations = split_font_list(&variation_text)
        .into_iter()
        .filter_map(|value| aterm_render::variation::parse_variation_spec(&value))
        .map(|(tag, value)| crate::widget::SemanticVariation::new(tag, value))
        .collect::<Vec<_>>();
    let weight = field_number(state, prefs::EDIT_FONT_WEIGHT, 400_f32).clamp(1.0, 1_000.0);
    variations.retain(|variation| variation.tag != aterm_render::variation::WGHT_TAG);
    variations.push(crate::widget::SemanticVariation::new(
        aterm_render::variation::WGHT_TAG,
        weight,
    ));
    crate::widget::SemanticFontCandidate {
        regular: family(prefs::EDIT_FONT_FAMILY),
        bold: family(prefs::EDIT_FONT_FAMILY_BOLD),
        italic: family(prefs::EDIT_FONT_FAMILY_ITALIC),
        bold_italic: family(prefs::EDIT_FONT_FAMILY_BOLD_ITALIC),
        fallback: split_font_list(&field_text(state, prefs::EDIT_FALLBACK_FONTS, "")),
        symbol: family(prefs::EDIT_SYMBOL_FONT),
        emoji: family(prefs::EDIT_EMOJI_FONT),
        variations,
        synthetic_styles: field_bool(state, prefs::EDIT_FONT_SYNTHETIC_STYLE, true),
    }
}

fn preview_terminal_theme(
    state: &SettingsViewState,
    host_theme: aterm_render::Theme,
) -> PreviewTerminalTheme {
    let authored_theme = field_text(state, prefs::EDIT_THEME, "Default");
    let dark_host = aterm_render::theme_is_dark(host_theme.bg);
    let theme_name = authored_theme
        .split(',')
        .map(str::trim)
        .find_map(|part| {
            let (mode, name) = part.split_once(':')?;
            ((mode.eq_ignore_ascii_case("dark") && dark_host)
                || (mode.eq_ignore_ascii_case("light") && !dark_host))
                .then(|| name.trim().to_string())
        })
        .unwrap_or(authored_theme);
    let scheme = aterm_types::scheme::builtin(&theme_name);
    let mut candidate = scheme.as_ref().map_or_else(
        || PreviewTerminalTheme::from(host_theme),
        PreviewTerminalTheme::from_scheme,
    );
    let apply_draft = |key: &str, slot: &mut u32| {
        let is_live_draft = state.editing_field.as_deref() == Some(key);
        if (state.is_explicit(key) || is_live_draft)
            && let Some(color) = field_color(state, key)
        {
            *slot = color;
        }
    };
    apply_draft(prefs::EDIT_FOREGROUND, &mut candidate.fg);
    apply_draft(prefs::EDIT_BACKGROUND, &mut candidate.bg);
    apply_draft(prefs::EDIT_CURSOR_COLOR, &mut candidate.cursor);
    apply_draft(prefs::EDIT_SELECTION_COLOR, &mut candidate.selection);
    candidate
}

fn preview_reduced_motion(
    state: &SettingsViewState,
    motion: crate::native_app::ViewMotionCx,
) -> bool {
    if motion.serious {
        return true;
    }
    let mode = crate::motion::MotionMode::parse(&field_text(state, prefs::EDIT_MOTION, "auto"));
    let load_shed = motion.performance_reduced
        && field_bool(state, prefs::EDIT_LOAD_ADAPTIVE_MOTION, true)
        && mode == crate::motion::MotionMode::Auto;
    load_shed
        || crate::motion::MotionPolicy::resolve(mode, motion.system_reduced, motion.focused)
            == crate::motion::MotionPolicy::Reduced
}

fn renderer_preview(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_theme: aterm_render::Theme,
    prepared_font: Option<&crate::tray_raster::PreparedSemanticFont>,
    width: SettingsWidth,
) -> Option<UiNode> {
    let spec = renderer_preview_spec_with_font(
        state,
        phase_ms,
        motion,
        terminal_font_px,
        terminal_theme,
        prepared_font,
    )?;
    let preview_path = if state.search.trim().is_empty() {
        state.route.path()
    } else {
        "/search"
    };
    Some(crate::settings_preview::preview_node(
        format!("settings/preview{preview_path}"),
        spec,
        renderer_preview_height(width),
    ))
}

/// Build the one canonical preview spec used by semantic paint, introspection,
/// and host cadence selection. Search keeps a contextual preview for its first
/// visual result instead of turning the demonstration off.
fn renderer_preview_spec(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_theme: aterm_render::Theme,
) -> Option<SettingsPreviewSpec> {
    renderer_preview_spec_with_font(
        state,
        phase_ms,
        motion,
        terminal_font_px,
        terminal_theme,
        None,
    )
}

fn renderer_preview_spec_with_font(
    state: &SettingsViewState,
    phase_ms: u64,
    motion: crate::native_app::ViewMotionCx,
    terminal_font_px: f32,
    terminal_theme: aterm_render::Theme,
    prepared_font: Option<&crate::tray_raster::PreparedSemanticFont>,
) -> Option<SettingsPreviewSpec> {
    let focused_key = focused_preview_key(state)?;
    let route = preview_route_for_key(&focused_key)?;
    let font_px = field_number(state, prefs::EDIT_FONT_PX, terminal_font_px);
    let font_candidate = preview_font_candidate(state);
    let terminal_theme_candidate = preview_terminal_theme(state, terminal_theme);
    let appearance = AppearancePreviewSpec {
        window_theme: field_text(state, prefs::EDIT_WINDOW_THEME, "auto"),
        minimum_contrast: field_number(state, prefs::EDIT_MINIMUM_CONTRAST, 1.0_f32),
        selection_foreground: field_color(state, prefs::EDIT_SELECTION_FOREGROUND),
        selection_inactive: field_bool(state, prefs::EDIT_SELECTION_INACTIVE, false),
        bold_is_bright: field_bool(state, prefs::EDIT_BOLD_IS_BRIGHT, true),
        faint_opacity: field_number(state, prefs::EDIT_FAINT_OPACITY, 0.5_f32),
    };
    let typography = TypographyPreviewSpec {
        cursor_break_ligatures: field_bool(state, prefs::EDIT_CURSOR_BREAK_LIGATURES, false),
        underline_position: field_number(state, prefs::EDIT_ADJUST_UNDERLINE_POSITION, 0_i32),
        underline_thickness: field_number(state, prefs::EDIT_ADJUST_UNDERLINE_THICKNESS, 0_i32),
        underline_skip_descenders: field_bool(state, prefs::EDIT_UNDERLINE_SKIP_DESCENDERS, true),
        text_blending: if field_text(state, prefs::EDIT_TEXT_BLENDING, "linear-corrected")
            .eq_ignore_ascii_case("linear")
        {
            crate::widget::SpecimenTextBlending::Linear
        } else {
            crate::widget::SpecimenTextBlending::LinearCorrected
        },
        font_thicken: field_bool(state, prefs::EDIT_FONT_THICKEN, false),
        stem_gamma: field_number(state, prefs::EDIT_STEM_GAMMA, 1.0_f32),
        variations: font_candidate.variations.clone(),
    };

    let motion_raw = field_text(state, prefs::EDIT_MOTION, "auto");
    let motion_mode = crate::motion::MotionMode::parse(&motion_raw);
    let adaptive_motion = field_bool(state, prefs::EDIT_LOAD_ADAPTIVE_MOTION, true);
    let focus_load_probe = focused_key == prefs::EDIT_LOAD_ADAPTIVE_MOTION;
    let effective_load_reduced = motion.performance_reduced || focus_load_probe;
    let load_shed =
        effective_load_reduced && adaptive_motion && motion_mode == crate::motion::MotionMode::Auto;
    let motion_policy = if load_shed {
        crate::motion::MotionPolicy::Reduced
    } else {
        crate::motion::MotionPolicy::resolve(motion_mode, motion.system_reduced, motion.focused)
    };
    let reduced_motion = motion.serious || motion_policy == crate::motion::MotionPolicy::Reduced;
    let motion_reason = if motion.serious {
        "serious mode suppresses decorative motion and post-processing"
    } else if focus_load_probe && load_shed {
        "the bounded adaptive-motion demo is applying a representative load signal"
    } else if focus_load_probe {
        "the bounded adaptive-motion demo is bypassing its representative load signal"
    } else if load_shed {
        "adaptive performance load-shed is active"
    } else if !motion.focused {
        "the aterm window is unfocused"
    } else {
        match motion_mode {
            crate::motion::MotionMode::Full => "motion=full explicitly enables motion",
            crate::motion::MotionMode::Reduced => "motion=reduced explicitly disables motion",
            crate::motion::MotionMode::Auto if motion.system_reduced => {
                "motion=auto follows system Reduce Motion"
            }
            crate::motion::MotionMode::Auto => "motion=auto follows the active system policy",
        }
    };

    let committed_nyan = state
        .raw_value(prefs::EDIT_CURSOR_NYAN_SPRITE)
        .unwrap_or_default();
    // Asset resolution reads only AUTHORED sources — the live draft, else the
    // committed raw value. The field's DISPLAY text is presentation metadata
    // whose "built-in sprite" placeholder must never masquerade as an authored
    // (and thus unvalidated → Invalid) candidate for the DEFAULT state.
    let nyan_value = state
        .field_inputs
        .get(prefs::EDIT_CURSOR_NYAN_SPRITE)
        .filter(|_| state.editing_field.as_deref() == Some(prefs::EDIT_CURSOR_NYAN_SPRITE))
        .map_or_else(|| committed_nyan.clone(), |input| input.projection().text);
    let nyan_asset = if nyan_value.trim().is_empty() {
        crate::app_config::NyanSpriteAsset::BuiltIn
    } else if nyan_value.trim() == committed_nyan.trim() {
        state.config_assets().nyan_sprite.clone()
    } else {
        crate::app_config::NyanSpriteAsset::Invalid {
            source_id: Arc::from(nyan_value.as_str()),
            bounded_reason: Arc::from(
                "uncommitted source is disabled until config validation admits its decoded pixels",
            ),
        }
    };
    let post_fx = CursorPostFxSpec {
        nyan_sprite: if nyan_value.is_empty() {
            "built-in CatBaker".to_string()
        } else {
            nyan_value
        },
        nyan_asset,
        bloom: !motion.serious && field_bool(state, prefs::EDIT_CURSOR_TRAIL_BLOOM, true),
        bloom_strength: field_number(state, prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH, 0.85_f32),
        bloom_radius: field_number(state, prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS, 2.2_f32),
        fire_shimmer: !motion.serious && field_bool(state, prefs::EDIT_CURSOR_FIRE_SHIMMER, true),
        hdr_glow: !motion.serious && field_bool(state, prefs::EDIT_HDR_GLOW, true),
        sdr_boost: if motion.serious {
            0.0
        } else {
            field_number(state, prefs::EDIT_CURSOR_GLOW_SDR_BOOST, 0.25_f32)
        },
        motion_raw,
        motion_effective: if reduced_motion { "reduced" } else { "full" }.to_string(),
        motion_reason: motion_reason.to_string(),
        adaptive_motion,
        performance_reduced: effective_load_reduced,
    };

    let cursor_style = match field_text(state, prefs::EDIT_CURSOR_STYLE, "block").as_str() {
        "bar" => PreviewCursorStyle::Bar,
        "underline" => PreviewCursorStyle::Underline,
        "hidden" => PreviewCursorStyle::Hidden,
        _ => PreviewCursorStyle::Block,
    };
    let trail_style_value = field_text(state, prefs::EDIT_CURSOR_TRAIL_STYLE, "nyan rainbow");
    let resolved_trail =
        crate::app_config::resolve_trail_style(&trail_style_value, state.trail_pack_catalog());
    let mut cursor = CursorPreviewSpec {
        style: cursor_style,
        blink: field_bool(state, prefs::EDIT_CURSOR_BLINK, true),
        trail_enabled: !motion.serious && field_bool(state, prefs::EDIT_CURSOR_TRAIL, true),
        trail_style: PreviewTrailStyle::from_resolution(&trail_style_value, resolved_trail),
        trail_pack: resolved_trail.pack.map(Arc::new),
        color: field_color(state, prefs::EDIT_CURSOR_TRAIL_COLOR),
        accent: field_color(state, prefs::EDIT_CURSOR_TRAIL_ACCENT),
        duration_ms: field_number(state, prefs::EDIT_CURSOR_TRAIL_MS, 260_u64),
        length: field_number(state, prefs::EDIT_CURSOR_TRAIL_LENGTH, 24_usize),
        intensity: field_number(state, prefs::EDIT_CURSOR_TRAIL_INTENSITY, 0.7_f32),
        radius: field_number(state, prefs::EDIT_CURSOR_TRAIL_RADIUS, 0.6_f32),
        ring: field_bool(state, prefs::EDIT_CURSOR_TRAIL_RING, true),
    };
    if !motion.serious && focused_key == prefs::EDIT_CURSOR_FIRE_SHIMMER {
        // This control is dormant for every non-Fire trail. Give it a bounded
        // Fire runway while focused so On/Off changes the actual shimmer lane;
        // the authored trail_style value itself remains untouched.
        cursor.trail_style = PreviewTrailStyle::Fire;
        cursor.trail_pack = None;
        cursor.trail_enabled = true;
    }
    let mut spec = match route {
        SettingsRoute::Appearance => SettingsPreviewSpec::appearance(font_px),
        SettingsRoute::TextFonts => SettingsPreviewSpec::typography(font_px),
        SettingsRoute::CursorMotion => SettingsPreviewSpec::cursor(cursor.clone()),
        SettingsRoute::WindowTabs => SettingsPreviewSpec::window_tabs(
            WindowTabsPreviewSpec {
                columns: field_number(state, prefs::EDIT_COLUMNS, 80_usize),
                lines: field_number(state, prefs::EDIT_LINES, 24_usize),
                tab_strip_rows: field_number(state, prefs::EDIT_TAB_STRIP_ROWS, 1_usize),
                show_build_badge: field_bool(state, prefs::EDIT_SHOW_BUILD_BADGE, false),
                generate_activity: field_bool(state, prefs::EDIT_DESCRIPTIVE_TITLES, true)
                    && !field_text(
                        state,
                        prefs::EDIT_TITLE_SUMMARY_PROVIDER,
                        crate::app_config::TitleSummaryProvider::Builtin.as_str(),
                    )
                    .eq_ignore_ascii_case(crate::app_config::TitleSummaryProvider::Off.as_str()),
                tab_title_format: field_text(
                    state,
                    prefs::EDIT_TAB_TITLE_FORMAT,
                    "title-description",
                ),
                window_title_format: field_text(
                    state,
                    prefs::EDIT_WINDOW_TITLE_FORMAT,
                    "title-description",
                ),
            },
            font_px,
        ),
        _ => return None,
    };
    spec.scene = match route {
        SettingsRoute::Appearance => PreviewScene::Appearance,
        SettingsRoute::TextFonts => PreviewScene::Typography,
        SettingsRoute::CursorMotion => PreviewScene::CursorMotion,
        SettingsRoute::WindowTabs => PreviewScene::WindowTabs,
        _ => return None,
    };
    // Only the Cursor & Motion scene paints live cursor policy. The other
    // constructors deliberately install a visible, non-blinking cursor so
    // their `PreviewAnimation::None` contract is also pixel-true. Overwriting
    // that cursor here made their pixels cross blink edges without changing
    // the retained fingerprint or arming the scheduler.
    if route == SettingsRoute::CursorMotion {
        spec.cursor = cursor;
    }
    let focused_value = {
        let value = field_text(state, &focused_key, "");
        if value.is_empty() {
            effective_default_for_field(&focused_key)
        } else {
            value
        }
    };
    let prepared_font = prepared_font
        .filter(|prepared| prepared.matches(&font_candidate))
        .cloned()
        .unwrap_or_else(|| {
            crate::tray_raster::PreparedSemanticFont::unavailable(font_candidate.clone())
        });
    Some(
        spec.with_terminal_theme(terminal_theme_candidate)
            .with_typography(
                field_number(state, prefs::EDIT_LINE_HEIGHT, 1.0_f32),
                field_number(state, prefs::EDIT_ADJUST_BASELINE, 0_i32),
                field_bool(state, prefs::EDIT_LIGATURES, true),
                field_bool(state, prefs::EDIT_MERGED_LIGATURES, false),
                field_bool(state, prefs::EDIT_FONT_SYNTHETIC_STYLE, true),
            )
            .with_font_candidate(font_candidate)
            .with_prepared_font(prepared_font)
            .with_appearance(appearance)
            .with_typography_candidate(typography)
            .with_post_fx(post_fx)
            .with_focus(focused_key, focused_value)
            .with_phase(phase_ms)
            .with_reduced_motion(reduced_motion),
    )
}

const fn renderer_preview_height(width: SettingsWidth) -> f32 {
    match width {
        SettingsWidth::Compact => 108.0,
        SettingsWidth::Medium => 132.0,
        SettingsWidth::Wide => 156.0,
    }
}

fn route_action(route: SettingsRoute) -> ActionId {
    ActionId::new(format!("settings/route{}", route.path()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsWidth {
    Compact,
    Medium,
    Wide,
}

impl SettingsWidth {
    fn for_viewport(width: f32) -> Self {
        Self::for_viewport_at_scale(width, crate::native_appearance::text_scale())
    }

    fn for_viewport_at_scale(width: f32, text_scale: f32) -> Self {
        let effective_width = width / text_scale.clamp(0.85, 2.0);
        if effective_width >= 1_040.0 {
            Self::Wide
        } else if effective_width >= 760.0 {
            Self::Medium
        } else {
            Self::Compact
        }
    }

    fn row_height(self) -> f32 {
        self.row_height_at_scale(settings_text_scale())
    }

    fn row_height_at_scale(self, scale: f32) -> f32 {
        let scale = scale.clamp(0.85, 2.0);
        match self {
            // Compact rows contain a label line and a complete native control.
            // Growing the allocation with Dynamic Type keeps the two from
            // painting through one another at the platform maximum of 2×.
            Self::Compact => {
                let label_height = 22.0_f32.max(20.0 * scale);
                let control_height = scaled_control_height_at_scale(scale);
                72.0_f32.max(label_height + control_height + 4.0)
            }
            Self::Medium | Self::Wide => 44.0_f32.max(40.0 * scale),
        }
    }
}

fn settings_text_scale() -> f32 {
    crate::native_appearance::text_scale().clamp(0.85, 2.0)
}

fn scaled_control_height() -> f32 {
    scaled_control_height_at_scale(settings_text_scale())
}

fn scaled_control_height_at_scale(scale: f32) -> f32 {
    44.0_f32.max(32.0 * scale.clamp(0.85, 2.0))
}

fn compact_toolbar_height() -> f32 {
    compact_toolbar_height_at_scale(settings_text_scale())
}

fn compact_toolbar_height_at_scale(scale: f32) -> f32 {
    52.0_f32.max(40.0 * scale.clamp(0.85, 2.0))
}

fn page_heading_height() -> f32 {
    34.0 * settings_text_scale()
}

fn page_subtitle_height() -> f32 {
    24.0_f32.max(20.0 * settings_text_scale())
}

fn page_navigation_height() -> f32 {
    44.0_f32.max(32.0 * settings_text_scale())
}

/// The group card's own heading label row. [`group_heading_height`] MUST stay
/// `28 + this` (the card's 14px vertical padding pair), or the card's fixed
/// height allocation silently clips its last control at intermediate Dynamic
/// Type scales.
fn group_heading_label_height() -> f32 {
    22.0_f32.max(16.0 * settings_text_scale())
}

fn group_heading_height() -> f32 {
    28.0 + group_heading_label_height()
}

fn persistent_navigation_fits(width: SettingsWidth, height: f32) -> bool {
    debug_assert_ne!(width, SettingsWidth::Compact);
    let scale = settings_text_scale();
    // Mirror the exact lower bounds authored by `navigation`: the dense rail
    // derives its row height from the remaining budget and compresses toward
    // a dense-list floor before any clipping would be accepted, so the rail
    // is viable exactly when its fixed chrome plus fourteen floor-height
    // routes (and the 15 inter-child gaps) fit the viewport. The floor is
    // width-classed: a medium host (a 770x350 staged-glass window) holds its
    // labeled rail down to 18px rows, while the airier wide rail yields to
    // the compact home cards earlier (a 1200x400 short desktop) instead of
    // presenting a squeezed sidebar beside all that width.
    let dense = height < 620.0;
    let (vertical_padding, title, search, section, gap, route, sections) = if dense {
        let floor = if width == SettingsWidth::Wide {
            24.0_f32.max(20.0 * scale)
        } else {
            18.0_f32.max(15.0 * scale)
        };
        (
            4.0,
            30.0_f32.max(30.0 * scale),
            32.0_f32.max(40.0 * scale),
            0.0,
            1.0,
            floor,
            0.0,
        )
    } else {
        (
            if width == SettingsWidth::Wide {
                12.0
            } else {
                10.0
            },
            36.0_f32.max(30.0 * scale),
            36.0_f32.max(40.0 * scale),
            (if width == SettingsWidth::Wide {
                16.0_f32
            } else {
                12.0_f32
            })
            .max(14.0 * scale),
            2.0,
            32.0_f32.max(24.0 * scale),
            4.0,
        )
    };
    let routes = SettingsRoute::ALL.len() as f32;
    let child_gaps = (2.0 + sections + routes - 1.0).max(0.0);
    let fixed = vertical_padding * 2.0 + title + search + sections * section + child_gaps * gap;
    height >= fixed + routes * route
}

fn settings_tree(
    state: &SettingsViewState,
    update: &UpdateProjection,
    packages: &PackagesProjection,
    cx: &ViewCx<'_>,
) -> UiTree {
    let mut width = SettingsWidth::for_viewport(cx.viewport.width);
    if width != SettingsWidth::Compact && !persistent_navigation_fits(width, cx.viewport.height) {
        width = SettingsWidth::Compact;
    }
    let feedback = feedback_bar(state);
    let page = page(state, update, packages, cx, width);
    let (body, root_feedback) = match width {
        SettingsWidth::Compact => {
            let body = UiNode::new(
                "settings/body",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::column().height(Length::Fill).clipped())
            .children(vec![compact_toolbar(state), page]);
            (body, feedback)
        }
        SettingsWidth::Medium | SettingsWidth::Wide => {
            // Mutation feedback belongs to the content pane, not the application
            // frame. A full-width persistent Undo bar used to take height away from
            // the navigation rail and clip Software Update/About—the exact moment a
            // user needed Undo was also the moment primary destinations vanished.
            // Keep the rail full-height and let only the current page yield its last
            // control row to feedback.
            let mut content_children = vec![page];
            if let Some(feedback) = feedback {
                content_children.push(feedback);
            }
            let content = UiNode::new(
                "settings/content",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .clipped(),
            )
            .children(content_children);
            let body = UiNode::new(
                "settings/body",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fill).clipped())
            .children(vec![navigation(state, width, cx.viewport.height), content]);
            (body, None)
        }
    };

    let mut app_children = vec![body];
    if let Some(feedback) = root_feedback {
        app_children.push(feedback);
    }

    UiTree::new(
        UiNode::new(
            "settings/app",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().clipped())
        .children(app_children),
    )
}

fn settings_search(state: &SettingsViewState, placeholder: &str) -> UiNode {
    let (search_value, _) = state.search_input.display_projection();
    UiNode::new(
        "settings/search",
        UiContent::TextField(
            Control::new(
                TextFieldSpec {
                    label: "Search all settings".to_string(),
                    placeholder: Some(placeholder.to_string()),
                    secret: false,
                    visual_value: None,
                    input: Some(state.search_input.projection()),
                    swatch: None,
                },
                ActionId::new("settings/search"),
            )
            .value(SemanticValue::Text(search_value))
            .state(ControlState {
                focused: state
                    .common
                    .last_focus
                    .as_ref()
                    .is_some_and(|key| key.as_str() == "settings/search"),
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
}

fn settings_search_bar(state: &SettingsViewState, placeholder: &str, height: Length) -> UiNode {
    let mut children = vec![
        settings_search(state, placeholder)
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
    ];
    if !state.search_input.value().is_empty() || !state.search.is_empty() {
        children.push(
            UiNode::new(
                "settings/search-clear",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Clear search").visual_label("Clear"),
                        ActionId::new("settings/search-clear"),
                    )
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(52.0 * settings_text_scale().min(1.25)))
                    .height(Length::Fill),
            ),
        );
    }
    UiNode::new(
        "settings/search-bar",
        UiContent::Group(GroupSpec::new("Settings search")),
    )
    .layout(Layout::row().width(Length::Fill).height(height).gap(4.0))
    .children(children)
}

fn compact_toolbar(state: &SettingsViewState) -> UiNode {
    let leading_width = 94.0 * settings_text_scale().min(1.4);
    let leading = if state.route == SettingsRoute::Home {
        UiNode::new(
            "settings/compact-title",
            UiContent::Text(TextSpec::heading("Settings")),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(leading_width))
                .height(Length::Fill),
        )
    } else {
        UiNode::new(
            "settings/compact-back",
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Back to Settings").visual_label("< Settings"),
                    route_action(SettingsRoute::Home),
                )
                .style(StyleRef::Navigation),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(leading_width))
                .height(Length::Fill),
        )
    };
    let search = settings_search_bar(state, "Search…", Length::Fill);
    UiNode::new(
        "settings/compact-toolbar",
        UiContent::Group(GroupSpec::new("Settings toolbar").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(compact_toolbar_height()))
            .padding(Insets::symmetric(10.0, 8.0))
            .gap(8.0),
    )
    .children(vec![leading, search])
}

fn feedback_bar(state: &SettingsViewState) -> Option<UiNode> {
    let feedback = state.feedback.as_ref()?;
    let mut children = vec![
        UiNode::new(
            "settings/status",
            UiContent::Text(TextSpec {
                text: feedback.clone(),
                role: SemanticRole::Status,
                style: StyleRef::Plain,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
    ];
    if state.last_undo.is_some() && !state.config_patch_pending() {
        children.push(
            UiNode::new(
                "settings/undo",
                UiContent::Button(
                    Control::new(ButtonSpec::new("Undo"), ActionId::new("settings/undo"))
                        .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(68.0))
                    .height(Length::Fill),
            ),
        );
    }
    Some(
        UiNode::new(
            "settings/feedback",
            UiContent::Group(GroupSpec::new("Settings status").style(StyleRef::Secondary)),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(scaled_control_height()))
                .padding(Insets::symmetric(16.0, 5.0))
                .gap(8.0),
        )
        .children(children),
    )
}

fn navigation(state: &SettingsViewState, width: SettingsWidth, available_height: f32) -> UiNode {
    debug_assert_ne!(width, SettingsWidth::Compact);

    // Keep the whole information architecture visible at ordinary short window
    // heights.  The layout metrics, paint rects, hit rects, and accessibility
    // rects all come from this one tree, so the denser rail cannot drift from
    // interaction geometry.  At the common 513-logical-pixel content height the
    // route rows settle around 28px; taller windows retain the airier 32px rhythm.
    const ROUTES: f32 = 14.0;
    let dense = available_height < 620.0;
    let vertical_padding = if dense {
        4.0
    } else if width == SettingsWidth::Wide {
        12.0
    } else {
        10.0
    };
    let text_scale = settings_text_scale();
    let title_height = (if dense { 30.0_f32 } else { 36.0_f32 }).max(30.0 * text_scale);
    let search_height = (if dense { 32.0_f32 } else { 36.0_f32 }).max(40.0 * text_scale);
    let section_height = if dense {
        12.0_f32.max(14.0 * text_scale)
    } else if width == SettingsWidth::Wide {
        16.0_f32.max(14.0 * text_scale)
    } else {
        12.0_f32.max(14.0 * text_scale)
    };
    let gap = if dense { 1.0 } else { 2.0 };
    let route_height = if dense {
        // At short desktop heights, keep every text-labeled route and remove
        // only the redundant section eyebrows. That buys a genuine 28px row
        // instead of clipping 13 controls into ~23px slivers.
        let section_labels = 0.0;
        // Title + search + the 14 route rows = 16 children → 15 inter-child gaps.
        let child_gaps = 15.0;
        let fixed = vertical_padding * 2.0
            + title_height
            + search_height
            + section_labels * section_height
            + child_gaps * gap;
        // Derive the row from the real remaining budget so the rail NEVER
        // paints past the viewport: the last route row ("About") must stay
        // whole at the ordinary 849x460 logical content size, and a
        // 770x350 staged-glass host keeps its labeled medium rail. The row
        // relaxes toward the airier 32px rhythm when the budget allows and
        // compresses to a dense-list 18px floor before any clipping is
        // accepted; `persistent_navigation_fits` demotes to the compact
        // toolbar below that.
        ((available_height - fixed) / ROUTES).clamp(
            18.0_f32.max(15.0 * text_scale),
            32.0_f32.max(24.0 * text_scale),
        )
    } else {
        32.0_f32.max(24.0 * text_scale)
    };

    let mut items = Vec::new();
    items.push(
        UiNode::new(
            "settings/navigation-title",
            UiContent::Text(TextSpec::heading("Settings")),
        )
        .layout(Layout::default().height(Length::Fixed(title_height))),
    );
    items.push(settings_search_bar(
        state,
        "Search all settings…",
        Length::Fixed(search_height),
    ));
    let sections: &[(&str, &[SettingsRoute])] = &[
        ("", &[SettingsRoute::Home, SettingsRoute::Modified]),
        (
            "PERSONALIZE",
            &[
                SettingsRoute::Appearance,
                SettingsRoute::TextFonts,
                SettingsRoute::CursorMotion,
            ],
        ),
        (
            "WORKSPACE",
            &[
                SettingsRoute::WindowTabs,
                SettingsRoute::KeyboardInput,
                SettingsRoute::Terminal,
            ],
        ),
        (
            "SYSTEM",
            &[
                SettingsRoute::Performance,
                SettingsRoute::Security,
                SettingsRoute::Diagnostics,
            ],
        ),
        (
            "ATERM",
            &[
                SettingsRoute::SoftwareUpdate,
                SettingsRoute::Packages,
                SettingsRoute::About,
            ],
        ),
    ];
    for (section, routes) in sections {
        if !dense && !section.is_empty() {
            let content = UiContent::Text(TextSpec {
                text: (*section).to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            });
            items.push(
                UiNode::new(
                    format!("settings/nav-label/{}", section.to_ascii_lowercase()),
                    content,
                )
                .layout(Layout::default().height(Length::Fixed(section_height))),
            );
        }
        for route in *routes {
            // One icon vocabulary serves both responsive rails. The renderer
            // centers it in the 64px medium rail and pairs it with the full
            // semantic label in the wide rail.
            let button = ButtonSpec::new(route.label()).visual_icon(route_icon(*route));
            items.push(
                UiNode::new(
                    format!("settings/nav{}", route.path()),
                    UiContent::Button(
                        Control::new(button, route_action(*route))
                            .state(ControlState {
                                // Global search spans every route. Keeping the
                                // previously visited category highlighted makes
                                // the result set look category-scoped, so search
                                // deliberately has no selected rail item.
                                selected: state.search.trim().is_empty() && *route == state.route,
                                ..ControlState::default()
                            })
                            .style(StyleRef::Navigation),
                    ),
                )
                .layout(
                    Layout::default()
                        .height(Length::Fixed(route_height))
                        .width(Length::Fill),
                ),
            );
        }
    }

    UiNode::new(
        "settings/navigation",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Navigation)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(match width {
                SettingsWidth::Wide => 216.0,
                SettingsWidth::Medium => 196.0,
                SettingsWidth::Compact => unreachable!(),
            }))
            .height(Length::Fill)
            .padding(Insets::symmetric(
                if width == SettingsWidth::Wide {
                    14.0
                } else {
                    12.0
                },
                vertical_padding,
            ))
            .gap(gap)
            .clipped(),
    )
    .children(items)
}

const fn route_icon(route: SettingsRoute) -> ButtonIcon {
    match route {
        SettingsRoute::Home => ButtonIcon::Home,
        SettingsRoute::Modified => ButtonIcon::Modified,
        SettingsRoute::Appearance => ButtonIcon::Appearance,
        SettingsRoute::TextFonts => ButtonIcon::Text,
        SettingsRoute::CursorMotion => ButtonIcon::Cursor,
        SettingsRoute::WindowTabs => ButtonIcon::Window,
        SettingsRoute::KeyboardInput => ButtonIcon::Keyboard,
        SettingsRoute::Terminal => ButtonIcon::Terminal,
        SettingsRoute::Performance => ButtonIcon::Performance,
        SettingsRoute::Security => ButtonIcon::Security,
        SettingsRoute::Diagnostics => ButtonIcon::Diagnostics,
        SettingsRoute::SoftwareUpdate => ButtonIcon::Update,
        SettingsRoute::Packages => ButtonIcon::Packages,
        SettingsRoute::About => ButtonIcon::Info,
    }
}

fn page(
    state: &SettingsViewState,
    update: &UpdateProjection,
    packages: &PackagesProjection,
    cx: &ViewCx<'_>,
    width: SettingsWidth,
) -> UiNode {
    let global_search = !state.search.trim().is_empty();
    let children = if global_search {
        settings_fields_page(state, false, cx, width)
    } else {
        match state.route {
            SettingsRoute::Home => {
                home_page(state, update, width, cx.viewport.width, cx.viewport.height)
            }
            SettingsRoute::Modified => settings_fields_page(state, true, cx, width),
            SettingsRoute::Diagnostics => diagnostics_page(state, cx, width),
            SettingsRoute::SoftwareUpdate => {
                update_page(state, update, width, cx.viewport.width, cx.viewport.height)
            }
            SettingsRoute::Packages => packages_page(
                state,
                packages,
                width,
                cx.viewport.width,
                cx.viewport.height,
            ),
            SettingsRoute::About => about_page(state, width, cx.viewport.width, cx.viewport.height),
            _ => settings_fields_page(state, false, cx, width),
        }
    };
    let maximum = page_maximum(state.route, width);
    let insets = responsive_page_insets(cx.viewport.width, cx.viewport.height, width, maximum);
    let (page_key, page_label) = if global_search {
        ("settings/page/search-results".to_string(), "Search Results")
    } else {
        (
            format!("settings/page{}", state.route.path()),
            state.route.label(),
        )
    };
    UiNode::new(page_key, UiContent::Group(GroupSpec::new(page_label)))
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(insets)
                .gap(responsive_page_gap(width, cx.viewport.height))
                .clipped(),
        )
        .children(children)
}

fn page_maximum(route: SettingsRoute, width: SettingsWidth) -> f32 {
    match (route, width) {
        // Give the preview enough measure to read like a terminal workspace,
        // while keeping ordinary form pages deliberately compact and scannable.
        (SettingsRoute::Appearance, SettingsWidth::Wide) => 1_040.0,
        (SettingsRoute::Home, SettingsWidth::Wide) => 1_000.0,
        (SettingsRoute::About, SettingsWidth::Wide)
        | (SettingsRoute::SoftwareUpdate, SettingsWidth::Wide)
        | (SettingsRoute::Packages, SettingsWidth::Wide) => 1_000.0,
        (SettingsRoute::About | SettingsRoute::SoftwareUpdate | SettingsRoute::Packages, _) => {
            760.0
        }
        _ => 720.0,
    }
}

fn responsive_page_insets(
    viewport_width: f32,
    viewport_height: f32,
    width: SettingsWidth,
    maximum: f32,
) -> Insets {
    let mut insets = page_insets(viewport_width, width, maximum);
    if width == SettingsWidth::Compact {
        // Short landscape and maximum Dynamic Type need the same complete
        // preview/control geometry, not a clipped tail hidden by the page.
        // Keep generous horizontal breathing room while tightening only the
        // repeated vertical chrome around an already-bounded virtual slice.
        let vertical = compact_page_vertical_inset(viewport_height);
        insets.top = vertical;
        insets.bottom = vertical;
    }
    insets
}

fn responsive_page_gap(width: SettingsWidth, viewport_height: f32) -> f32 {
    if width == SettingsWidth::Compact {
        compact_page_gap(viewport_height)
    } else {
        12.0
    }
}

/// Special routes use the same wheel/controller scroll event as ordinary
/// settings, but virtualize coherent sections instead of field rows.  Returning
/// an explicit bound here keeps reducer state finite and makes End/PageDown
/// land on the last genuinely renderable section.
fn special_page_scroll_limit(
    route: SettingsRoute,
    update: &UpdateProjection,
    packages: &PackagesProjection,
) -> Option<usize> {
    match route {
        SettingsRoute::Home => Some(SettingsRoute::ALL.len().saturating_sub(2)),
        SettingsRoute::Diagnostics => Some(3),
        SettingsRoute::SoftwareUpdate => Some(
            update_compact_sections(update)
                .saturating_add(2)
                .saturating_sub(1),
        ),
        SettingsRoute::Packages => Some(packages_sections(packages).saturating_sub(1)),
        // Short landscape may split all seven build and five support rows into
        // individual complete cards in addition to identity/actions.
        SettingsRoute::About => Some(14_usize.saturating_sub(1)),
        _ => None,
    }
}

fn compact_about_section_count() -> usize {
    if settings_text_scale() > 1.25 { 4 } else { 3 }
}

/// A visible and semantic escape hatch for every bounded virtual window.
/// Wheel and PageUp/PageDown remain convenient, but neither is discoverable to
/// a switch-control or screen-reader user. These controls emit the exact same
/// reducer actions, so pointer, keyboard, controller, and accessibility paths
/// cannot drift.
fn page_navigation_node(
    key: &str,
    status_key: &str,
    label: &str,
    start: usize,
    end: usize,
    total: usize,
) -> UiNode {
    let button_width = (64.0 * settings_text_scale().min(1.25)).max(64.0);
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::new(format!("{label} page navigation"))),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(page_navigation_height()))
            .gap(6.0),
    )
    .children(vec![
        UiNode::new(
            format!("{key}/previous"),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(format!("Previous {label}")).visual_label("Prev"),
                    ActionId::new("settings/page-up"),
                )
                .state(ControlState {
                    enabled: start > 0,
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(button_width))
                .height(Length::Fill),
        ),
        UiNode::new(
            status_key,
            UiContent::Text(TextSpec {
                text: format!("{}–{end} of {total}", start + 1),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        UiNode::new(
            format!("{key}/next"),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(format!("Next {label}")).visual_label("Next"),
                    ActionId::new("settings/page-down"),
                )
                .state(ControlState {
                    enabled: end < total,
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(button_width))
                .height(Length::Fill),
        ),
    ])
}

fn diagnostics_page(
    state: &SettingsViewState,
    cx: &ViewCx<'_>,
    width: SettingsWidth,
) -> Vec<UiNode> {
    let metrics = crate::metrics::snapshot();
    let milliseconds = |nanoseconds: u64| nanoseconds as f64 / 1_000_000.0;
    let explicit = state
        .legacy
        .fields
        .iter()
        .filter(|field| state.is_explicit(field.key))
        .count();
    let breakpoint = match width {
        SettingsWidth::Compact => "compact",
        SettingsWidth::Medium => "medium",
        SettingsWidth::Wide => "wide",
    };
    let renderer = if metrics.backend_gpu { "GPU" } else { "CPU" };
    let render_health = if metrics.slow_frames == 0 {
        "No slow frames in the current measurement window"
    } else {
        "Slow frames detected in the current measurement window"
    };
    let event_health = if metrics.wake_heals == 0 && metrics.sync_releases_timeout == 0 {
        "No lost-wake heals or synchronized-output timeouts"
    } else {
        "Event-loop recovery activity detected"
    };
    let compact = width == SettingsWidth::Compact;
    let cards = [
        diagnostic_card(
            "renderer",
            "Renderer",
            &if compact {
                format!(
                    "{renderer} {:.2} · max {:.2} ms",
                    milliseconds(metrics.last_frame_render_ns),
                    milliseconds(metrics.max_frame_render_ns)
                )
            } else {
                format!(
                    "{renderer} · last {:.2} ms · max {:.2} ms",
                    milliseconds(metrics.last_frame_render_ns),
                    milliseconds(metrics.max_frame_render_ns)
                )
            },
            &if compact {
                format!(
                    "{} frames · {} slow",
                    metrics.frames_presented, metrics.slow_frames
                )
            } else {
                format!(
                    "{render_health} · {} frames · {} slow",
                    metrics.frames_presented, metrics.slow_frames
                )
            },
        ),
        diagnostic_card(
            "interaction",
            "Interaction pipeline",
            &if compact {
                format!(
                    "Input→present {:.2} · max {:.2} ms",
                    milliseconds(metrics.last_input_present_ns),
                    milliseconds(metrics.max_input_present_ns)
                )
            } else {
                format!(
                    "Input→present {:.2} ms · max {:.2} ms",
                    milliseconds(metrics.last_input_present_ns),
                    milliseconds(metrics.max_input_present_ns)
                )
            },
            &if compact {
                format!(
                    "Wake heals {} · timeouts {}",
                    metrics.wake_heals, metrics.sync_releases_timeout
                )
            } else {
                format!(
                    "{event_health} · wake heals {} · timeouts {}",
                    metrics.wake_heals, metrics.sync_releases_timeout
                )
            },
        ),
        diagnostic_card(
            "configuration",
            "Configuration service",
            &if compact {
                format!(
                    "Rev {} · {explicit} overrides · {} pending",
                    cx.config_revision,
                    state.pending.len()
                )
            } else {
                format!(
                    "Revision {} · {explicit} explicit overrides · {} pending",
                    cx.config_revision,
                    state.pending.len()
                )
            },
            &if compact {
                format!(
                    "Versioned writes · conflicts · undo · r{}",
                    cx.update_revision
                )
            } else {
                format!(
                    "Versioned writes, conflict checks, undo, and external reload · update revision {}",
                    cx.update_revision
                )
            },
        ),
        diagnostic_card(
            "semantic-ui",
            "Semantic Settings surface",
            &if compact {
                format!(
                    "{} controls · {} routes · {breakpoint}",
                    state.legacy.fields.len(),
                    SettingsRoute::ALL.len()
                )
            } else {
                format!(
                    "{} controls · {} routes · {breakpoint} layout",
                    state.legacy.fields.len(),
                    SettingsRoute::ALL.len()
                )
            },
            if compact {
                "One tree → pixels · input · accessibility"
            } else {
                "One semantic tree drives pixels, hit testing, accessibility, commands, and restore."
            },
        ),
    ];

    let card_height = diagnostic_card_height();
    let (range, grid) = if width == SettingsWidth::Wide {
        (
            None,
            UiNode::new(
                "settings/diagnostics/grid",
                UiContent::Group(GroupSpec::new("Live diagnostics")),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(card_height * 2.0 + 12.0))
                    .gap(12.0),
            )
            .children(
                cards
                    .chunks(2)
                    .enumerate()
                    .map(|(row, cards)| {
                        UiNode::new(
                            format!("settings/diagnostics/row/{row}"),
                            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                        )
                        .layout(Layout::row().height(Length::Fixed(card_height)).gap(12.0))
                        .children(cards.to_vec())
                    })
                    .collect(),
            ),
        )
    } else {
        let start = state.page_scroll.min(cards.len().saturating_sub(1));
        let visible_count = if width == SettingsWidth::Compact {
            let fixed = compact_toolbar_height()
                + 40.0
                + page_heading_height()
                + page_subtitle_height()
                + (22.0_f32.max(20.0 * settings_text_scale()))
                + page_navigation_height()
                + 54.0;
            let available = (cx.viewport.height - fixed).max(card_height);
            (((available + 12.0) / (card_height + 12.0)).floor() as usize).clamp(1, 2)
        } else {
            // The common macOS window has a 460pt native-content viewport after
            // unified titlebar chrome. Three 104pt cards plus heading/note cannot
            // fit there; exposing them anyway clipped Configuration and made the
            // fourth panel unreachable. Medium uses the same semantic pager as a
            // phone, two complete cards at a time. Wide remains the 2×2 dashboard.
            2
        };
        let end = start.saturating_add(visible_count).min(cards.len());
        let visible = cards[start..end].to_vec();
        let height =
            visible.len() as f32 * card_height + visible.len().saturating_sub(1) as f32 * 12.0;
        (
            Some(page_navigation_node(
                "settings/diagnostics/pagination",
                "settings/diagnostics/range",
                "Live panels",
                start,
                end,
                cards.len(),
            )),
            UiNode::new(
                "settings/diagnostics/grid",
                UiContent::Group(GroupSpec::new("Live diagnostics")),
            )
            .layout(Layout::column().height(Length::Fixed(height)).gap(12.0))
            .children(visible),
        )
    };

    let mut out = page_heading(
        "Diagnostics",
        if width == SettingsWidth::Compact {
            "Live renderer, event-loop, and UI facts."
        } else {
            "Live facts from aterm’s renderer, event loop, and semantic Settings runtime."
        },
    );
    out.push(
        UiNode::new(
            "settings/diagnostics/measurement-note",
            UiContent::Text(TextSpec {
                text: if compact {
                    "Live window metrics · current UI facts"
                } else {
                    "Performance counters cover the current metrics window; configuration and UI facts are current now."
                }
                .to_string(),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default()
                .height(Length::Fixed(22.0_f32.max(20.0 * settings_text_scale()))),
        ),
    );
    if let Some(range) = range {
        out.push(range);
    }
    out.push(grid);
    out
}

fn diagnostic_card(id: &str, title: &str, headline: &str, detail: &str) -> UiNode {
    UiNode::new(
        format!("settings/diagnostics/{id}"),
        UiContent::Group(GroupSpec::new(title).style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(diagnostic_card_height()))
            .padding(Insets::all(10.0))
            .gap(4.0),
    )
    .children(vec![
        UiNode::new(
            format!("settings/diagnostics/{id}/title"),
            UiContent::Text(TextSpec {
                text: title.to_ascii_uppercase(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default().height(Length::Fixed(18.0_f32.max(16.0 * settings_text_scale()))),
        ),
        UiNode::new(
            format!("settings/diagnostics/{id}/headline"),
            UiContent::Text(TextSpec {
                text: headline.to_string(),
                role: SemanticRole::Status,
                style: StyleRef::Primary,
            }),
        )
        .layout(
            Layout::default().height(Length::Fixed(22.0_f32.max(20.0 * settings_text_scale()))),
        ),
        UiNode::new(
            format!("settings/diagnostics/{id}/detail"),
            UiContent::Text(TextSpec {
                text: detail.to_string(),
                role: SemanticRole::Text,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fill)),
    ])
}

fn diagnostic_card_height() -> f32 {
    104.0_f32.max(84.0 * settings_text_scale())
}

fn page_insets(viewport_width: f32, width: SettingsWidth, maximum: f32) -> Insets {
    let (navigation, minimum, vertical) = match width {
        SettingsWidth::Wide => (216.0, 40.0, 32.0),
        SettingsWidth::Medium => (196.0, 24.0, 24.0),
        SettingsWidth::Compact => (0.0, 16.0, 20.0),
    };
    let canvas = (viewport_width - navigation).max(0.0);
    let horizontal = ((canvas - maximum) / 2.0).max(minimum);
    Insets::symmetric(horizontal, vertical)
}

fn compact_page_effective_height(viewport_height: f32) -> f32 {
    viewport_height / settings_text_scale()
}

fn compact_page_omits_heading(viewport_height: f32) -> bool {
    compact_page_effective_height(viewport_height) <= 240.0
}

fn compact_page_vertical_inset(viewport_height: f32) -> f32 {
    let effective = compact_page_effective_height(viewport_height);
    if effective <= 190.0 {
        0.0
    } else if effective <= 240.0 {
        4.0
    } else if effective <= 420.0 {
        12.0
    } else {
        20.0
    }
}

fn compact_page_gap(viewport_height: f32) -> f32 {
    if compact_page_omits_heading(viewport_height) {
        4.0
    } else if compact_page_effective_height(viewport_height) <= 420.0 {
        5.0
    } else {
        10.0
    }
}

/// Exact height available to one compact special-page section beneath the
/// toolbar and pager. About/Update author complete semantic sections against
/// this measure instead of relying on the page clip to hide their tails.
fn compact_special_section_height(state: &SettingsViewState, viewport_height: f32) -> f32 {
    let mut insets = page_insets(0.0, SettingsWidth::Compact, 0.0);
    let vertical = compact_page_vertical_inset(viewport_height);
    insets.top = vertical;
    insets.bottom = vertical;
    let page_gap = compact_page_gap(viewport_height);
    (viewport_height
        - compact_toolbar_height()
        - insets.top
        - insets.bottom
        - page_navigation_height()
        - page_gap
        - if state.feedback.is_some() {
            scaled_control_height()
        } else {
            0.0
        })
    .max(scaled_control_height())
}

/// Height actually owned by a non-compact page after the responsive page
/// insets and the content-scoped mutation bar have taken their measure. Special
/// dashboards use this before authoring fixed-height cards, rather than relying
/// on the compiler's final clip to hide an unreachable tail.
fn noncompact_page_content_height(
    state: &SettingsViewState,
    width: SettingsWidth,
    viewport_width: f32,
    viewport_height: f32,
    maximum: f32,
) -> f32 {
    debug_assert_ne!(width, SettingsWidth::Compact);
    let insets = page_insets(viewport_width, width, maximum);
    (viewport_height
        - if state.feedback.is_some() {
            scaled_control_height()
        } else {
            0.0
        }
        - insets.top
        - insets.bottom)
        .max(0.0)
}

fn page_heading(title: &str, subtitle: &str) -> Vec<UiNode> {
    let mut out = vec![
        UiNode::new(
            format!("settings/page-heading/{}", key_fragment(title)),
            UiContent::Text(TextSpec {
                text: title.to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Hero,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(page_heading_height()))),
    ];
    // A page may yield its one-line subtitle (short-landscape disclosure pages
    // do) — an empty subtitle authors no row instead of a blank 24px band that
    // pushes real controls past the viewport.
    if !subtitle.is_empty() {
        out.push(
            UiNode::new(
                format!("settings/page-subtitle/{}", key_fragment(title)),
                UiContent::Text(TextSpec {
                    text: subtitle.to_string(),
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(page_subtitle_height()))),
        );
    }
    out
}

fn choice_picker_columns(width: SettingsWidth, viewport_width: f32) -> usize {
    // Two columns keep each option readable at phone widths and avoid the
    // dense four-across matrix that made choices hard to scan.  Extremely
    // narrow compact hosts fall back to one column without changing the
    // semantic option set.
    if width == SettingsWidth::Compact && viewport_width < 350.0 {
        1
    } else {
        2
    }
}

fn choice_picker_height(picker: &ChoicePicker, width: SettingsWidth, viewport_width: f32) -> f32 {
    let columns = choice_picker_columns(width, viewport_width);
    let rows = picker.visible_range().len().div_ceil(columns).max(1);
    let paginated = picker.options.len() > ChoicePicker::PAGE_SIZE;
    let row_height = scaled_control_height();
    let pagination = if paginated { row_height } else { 0.0 };
    let gaps = (rows + usize::from(paginated)) as f32 * 6.0;
    24.0 + row_height + rows as f32 * row_height + pagination + gaps
}

fn choice_picker_node(
    state: &SettingsViewState,
    width: SettingsWidth,
    viewport_width: f32,
) -> Option<UiNode> {
    let picker = state.choice_picker.as_ref()?;
    let field = state
        .legacy
        .fields
        .iter()
        .find(|field| field.key == picker.key)?;
    let columns = choice_picker_columns(width, viewport_width);
    let row_height = scaled_control_height();
    let visible = picker.visible_range().collect::<Vec<_>>();
    let mut rows = Vec::new();
    for chunk in visible.chunks(columns) {
        rows.push(
            UiNode::new(
                format!("settings/choice-row/{}/{}", picker.key, rows.len()),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fixed(row_height)).gap(6.0))
            .children(
                chunk
                    .iter()
                    .map(|index| {
                        let option = &picker.options[*index];
                        let focused = state.common.last_focus.as_ref().is_some_and(|focus| {
                            focus.as_str() == format!("settings/choice/{}/{index}", picker.key)
                        });
                        UiNode::new(
                            format!("settings/choice/{}/{index}", picker.key),
                            UiContent::Button(
                                Control::new(
                                    ButtonSpec::new(option.label.clone()),
                                    ActionId::new(format!(
                                        "settings/choice/{}/{index}",
                                        picker.key
                                    )),
                                )
                                .state(ControlState {
                                    focused,
                                    selected: *index == picker.selected,
                                    ..ControlState::default()
                                })
                                // Choice candidates are discrete decisions, not
                                // destinations in the persistent sidebar. Keep a
                                // quiet card outline at rest so pointer users can
                                // discover the full hit target; focus/selection
                                // still promotes the same native accent states.
                                .style(StyleRef::Secondary),
                            ),
                        )
                        .layout(Layout::default().width(Length::Fill).height(Length::Fill))
                    })
                    .collect(),
            ),
        );
    }

    let mut children = vec![
        UiNode::new(
            format!("settings/choice-header/{}", picker.key),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::row().height(Length::Fixed(row_height)).gap(8.0))
        .children(vec![
            UiNode::new(
                format!("settings/choice-title/{}", picker.key),
                UiContent::Text(TextSpec {
                    text: format!("Choose {}", field.label),
                    role: SemanticRole::Heading,
                    style: StyleRef::Primary,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            UiNode::new(
                "settings/choice-close",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Close choices").visual_label("Done"),
                        ActionId::new("settings/choice-close"),
                    )
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(64.0))
                    .height(Length::Fill),
            ),
        ]),
    ];
    children.extend(rows);
    if picker.options.len() > ChoicePicker::PAGE_SIZE {
        let page = picker.offset / ChoicePicker::PAGE_SIZE + 1;
        let pages = picker.options.len().div_ceil(ChoicePicker::PAGE_SIZE);
        children.push(
            UiNode::new(
                format!("settings/choice-pages/{}", picker.key),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().height(Length::Fixed(row_height)).gap(8.0))
            .children(vec![
                UiNode::new(
                    "settings/choice-page-prev",
                    UiContent::Button(
                        Control::new(
                            ButtonSpec::new("Previous choices").visual_label("Prev"),
                            ActionId::new("settings/choice-page-prev"),
                        )
                        .state(ControlState {
                            enabled: page > 1,
                            ..ControlState::default()
                        })
                        .style(StyleRef::Quiet),
                    ),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(64.0))
                        .height(Length::Fill),
                ),
                UiNode::new(
                    format!("settings/choice-page-label/{}", picker.key),
                    UiContent::Text(TextSpec {
                        text: format!("Page {page} of {pages}"),
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                UiNode::new(
                    "settings/choice-page-next",
                    UiContent::Button(
                        Control::new(
                            ButtonSpec::new("Next choices").visual_label("Next"),
                            ActionId::new("settings/choice-page-next"),
                        )
                        .state(ControlState {
                            enabled: page < pages,
                            ..ControlState::default()
                        })
                        .style(StyleRef::Quiet),
                    ),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(64.0))
                        .height(Length::Fill),
                ),
            ]),
        );
    }

    Some(
        UiNode::new(
            format!("settings/choice-picker/{}", picker.key),
            UiContent::Group(
                GroupSpec::new(format!("{} choices", field.label)).style(StyleRef::Secondary),
            ),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(choice_picker_height(
                    picker,
                    width,
                    viewport_width,
                )))
                .padding(Insets::all(12.0))
                .gap(6.0)
                .clipped(),
        )
        .children(children),
    )
}

fn home_page(
    state: &SettingsViewState,
    update: &UpdateProjection,
    width: SettingsWidth,
    viewport_width: f32,
    viewport_height: f32,
) -> Vec<UiNode> {
    let explicit = state
        .legacy
        .fields
        .iter()
        .filter(|field| state.is_explicit(field.key))
        .count();
    let compact = width == SettingsWidth::Compact;
    let text_scale = settings_text_scale();
    let compact_title_height = 30.0 * text_scale;
    let compact_summary_height = 20.0_f32.max(18.0 * text_scale);
    let compact_hero_height =
        78.0_f32.max(compact_title_height + compact_summary_height + 4.0 + 24.0);
    let hero_height = if compact {
        compact_hero_height
    } else {
        136.0_f32.max(104.0 * text_scale)
    };
    let mut hero_children = vec![
        UiNode::new(
            "settings/home/title",
            UiContent::Text(TextSpec {
                text: "Make aterm yours".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Hero,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(if compact {
            compact_title_height
        } else {
            page_heading_height()
        }))),
    ];
    if compact {
        let state_summary = if explicit == 0 {
            "Defaults intact".to_string()
        } else {
            format!("{explicit} customized")
        };
        hero_children.push(
            UiNode::new(
                "settings/home/summary",
                UiContent::Text(TextSpec {
                    text: format!("{} settings  ·  {state_summary}", state.legacy.fields.len()),
                    role: SemanticRole::Status,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(compact_summary_height))),
        );
    } else {
        hero_children.extend([
            UiNode::new(
                "settings/home/byline",
                UiContent::Text(TextSpec {
                    text: crate::build_info::AUTHOR_COMPANY_BYLINE.to_string(),
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(20.0))),
            UiNode::new(
                "settings/home/subtitle",
                UiContent::Text(TextSpec {
                    text: "Search once, review what changed, or enter a focused workspace."
                        .to_string(),
                    role: SemanticRole::Text,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(24.0))),
            UiNode::new(
                "settings/home/summary",
                UiContent::Text(TextSpec {
                    text: format!(
                        "{} settings available  ·  choices save immediately; text saves on Return",
                        state.legacy.fields.len()
                    ),
                    role: SemanticRole::Status,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(20.0))),
        ]);
    }
    let hero = UiNode::new(
        "settings/home/hero",
        UiContent::Group(GroupSpec::new("Settings overview").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(hero_height))
            .padding(if compact {
                Insets::symmetric(14.0, 12.0)
            } else {
                Insets::symmetric(18.0, 16.0)
            })
            .gap(if compact { 4.0 } else { 6.0 }),
    )
    .children(hero_children);

    let theme = state
        .legacy
        .fields
        .iter()
        .find(|field| field.key == prefs::EDIT_THEME)
        .map(|field| field.seed.as_deref().unwrap_or(field.placeholder.as_str()))
        .unwrap_or("System theme");
    // This summary lives in one of three equal-width glance cards. Preserve
    // the full updater prose on `/updates`; Home uses a stable, renderer-safe
    // state label so a healthy status never degrades into `You’re up t…`.
    let update_summary = if update.checking {
        "Checking…"
    } else if update.staged.is_some() {
        "Ready"
    } else if update.enabled {
        "Current"
    } else {
        "Unavailable"
    };
    let overview = [
        (
            if explicit == 0 {
                "Modified · Defaults intact".to_string()
            } else {
                format!("Modified · Review {explicit}")
            },
            SettingsRoute::Modified,
        ),
        (format!("Appearance  ·  {theme}"), SettingsRoute::Appearance),
        (
            format!("Updates  ·  {update_summary}"),
            SettingsRoute::SoftwareUpdate,
        ),
    ];
    let overview = UiNode::new(
        "settings/home/overview",
        UiContent::Group(GroupSpec::new("Settings at a glance")),
    )
    .layout(Layout::row().height(Length::Fixed(52.0)).gap(10.0))
    .children(
        overview
            .into_iter()
            .map(|(label, route)| {
                UiNode::new(
                    format!("settings/home/overview{}", route.path()),
                    UiContent::Button(
                        Control::new(
                            ButtonSpec::new(label).trailing_icon(ButtonIcon::Forward),
                            route_action(route),
                        )
                        .style(StyleRef::Setting),
                    ),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill))
            })
            .collect(),
    );

    let mut routes = vec![
        (
            SettingsRoute::TextFonts,
            "Text & Fonts  ·  typography and shaping",
        ),
        (
            SettingsRoute::CursorMotion,
            "Cursor & Motion  ·  form, policy, and effects",
        ),
        (
            SettingsRoute::WindowTabs,
            "Window & Tabs  ·  Smart Titles and chrome",
        ),
        (
            SettingsRoute::KeyboardInput,
            "Keyboard & Input  ·  shortcuts & paste",
        ),
        (
            SettingsRoute::Terminal,
            "Terminal  ·  sessions and protocols",
        ),
        (
            SettingsRoute::Performance,
            "Performance  ·  renderer and resource use",
        ),
        (
            SettingsRoute::Security,
            "Security  ·  permissions and containment",
        ),
        (
            SettingsRoute::Diagnostics,
            "Diagnostics  ·  measured system health",
        ),
        (
            SettingsRoute::Packages,
            "Packages  ·  bundled ALab toolchain",
        ),
        (
            SettingsRoute::About,
            "About  ·  build provenance and project links",
        ),
    ];
    if width == SettingsWidth::Compact {
        // Compact has no persistent rail, so its Home page must expose every
        // destination that is not Home itself. The three at-a-glance routes
        // are repeated here because that overview is intentionally hidden at
        // phone widths.
        routes.splice(
            0..0,
            [
                (SettingsRoute::Modified, "Modified"),
                (SettingsRoute::Appearance, "Appearance"),
                (SettingsRoute::SoftwareUpdate, "Software Update"),
            ],
        );
    }
    let route_height = if compact {
        48.0_f32.max(scaled_control_height())
    } else {
        48.0_f32.max(36.0 * text_scale)
    };
    let route_gap = if compact { 6.0 } else { 8.0 };
    let explore_heading_height = 20.0_f32.max(16.0 * text_scale);
    let (route_start, route_end, route_columns, show_route_pager) = if compact {
        let columns = usize::from(viewport_width / text_scale >= 360.0) + 1;
        let fixed = compact_toolbar_height()
            + 40.0
            + compact_hero_height
            + 20.0_f32.max(16.0 * text_scale)
            + 46.0;
        let capacity_without_pager = (((viewport_height - fixed).max(route_height) + route_gap)
            / (route_height + route_gap))
            .floor() as usize
            * columns;
        let show_pager = capacity_without_pager < routes.len() || state.page_scroll > 0;
        let available = viewport_height
            - fixed
            - if show_pager {
                page_navigation_height() + 10.0
            } else {
                0.0
            };
        let capacity = ((((available.max(route_height) + route_gap) / (route_height + route_gap))
            .floor() as usize)
            * columns)
            .max(1);
        let start = state.page_scroll.min(routes.len().saturating_sub(1));
        let end = start.saturating_add(capacity).min(routes.len());
        (start, end, columns, show_pager)
    } else {
        let columns = 2;
        let content_height =
            noncompact_page_content_height(state, width, viewport_width, viewport_height, 1_000.0);
        let full_rows = routes.len().div_ceil(columns).max(1);
        let full_routes_height =
            full_rows as f32 * route_height + full_rows.saturating_sub(1) as f32 * route_gap;
        // Hero, two overview bands, the workspace heading, routes, and four
        // inter-section gaps. At the ordinary 849x460 logical viewport this
        // exceeds the page by more than two rows; clipping the tail made Home
        // look unfinished even though the same destinations remained in the
        // rail. A bounded route window keeps every authored card whole.
        let full_height =
            hero_height + 20.0 + 52.0 + explore_heading_height + full_routes_height + 4.0 * 12.0;
        let show_pager = full_height > content_height || state.page_scroll > 0;
        if show_pager {
            // Six authored children (hero, overview heading, overview, pager,
            // workspace heading, routes) create five page gaps.
            let fixed = hero_height
                + 20.0
                + 52.0
                + page_navigation_height()
                + explore_heading_height
                + 5.0 * 12.0;
            let available = (content_height - fixed).max(route_height);
            let rows =
                (((available + route_gap) / (route_height + route_gap)).floor() as usize).max(1);
            let capacity = rows.saturating_mul(columns).max(1);
            let start = state.page_scroll.min(routes.len().saturating_sub(1));
            let end = start.saturating_add(capacity).min(routes.len());
            (start, end, columns, true)
        } else {
            (0, routes.len(), columns, false)
        }
    };
    let visible_routes = &routes[route_start..route_end];
    let column_size = visible_routes.len().div_ceil(route_columns).max(1);
    let columns = visible_routes
        .chunks(column_size)
        .enumerate()
        .map(|(column, routes)| {
            UiNode::new(
                format!("settings/home/column/{column}"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .gap(route_gap),
            )
            .children(
                routes
                    .iter()
                    .map(|(route, detail)| {
                        let label = if width == SettingsWidth::Compact {
                            route.label()
                        } else {
                            detail
                        };
                        UiNode::new(
                            format!("settings/home{}", route.path()),
                            UiContent::Button(
                                Control::new(
                                    ButtonSpec::new(label).trailing_icon(ButtonIcon::Forward),
                                    route_action(*route),
                                )
                                .style(StyleRef::Secondary),
                            ),
                        )
                        .layout(Layout::default().height(Length::Fixed(route_height)))
                    })
                    .collect(),
            )
        })
        .collect();
    let mut home = vec![hero];
    if width != SettingsWidth::Compact {
        home.extend([
            UiNode::new(
                "settings/home/overview-heading",
                UiContent::Text(TextSpec {
                    text: "AT A GLANCE".to_string(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(20.0))),
            overview,
        ]);
    }
    if show_route_pager {
        home.push(page_navigation_node(
            "settings/home/pagination",
            "settings/home/range",
            "categories",
            route_start,
            route_end,
            routes.len(),
        ));
    }
    home.extend([
        UiNode::new(
            "settings/home/explore-heading",
            UiContent::Text(TextSpec {
                text: "FOCUSED WORKSPACES".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(explore_heading_height))),
        UiNode::new(
            "settings/home/routes",
            UiContent::Group(GroupSpec::new("Settings categories")),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(
                    column_size as f32 * route_height + (column_size - 1) as f32 * route_gap,
                ))
                .gap(10.0),
        )
        .children(columns),
    ]);
    home
}

fn settings_fields_page(
    state: &SettingsViewState,
    modified_only: bool,
    cx: &ViewCx<'_>,
    width: SettingsWidth,
) -> Vec<UiNode> {
    let query = state.search.trim().to_ascii_lowercase();
    let global_search = !query.is_empty();
    let show_renderer_preview = !modified_only && focused_preview_key(state).is_some();
    let prefer_smart_title_health = prefers_smart_title_health(state);
    // Global search never discloses: the searched-for rows are the entire
    // point of that page, so its demonstration shares the page with them and
    // the chrome sheds (below) to keep every actionable node whole instead.
    let preview_disclosure = show_renderer_preview
        && !prefer_smart_title_health
        && !global_search
        && width == SettingsWidth::Compact
        && cx.viewport.height <= 420.0;
    let show_renderer_preview_now = show_renderer_preview
        && !prefer_smart_title_health
        && (!preview_disclosure || state.page_scroll == 0);
    let show_smart_title_health = !modified_only
        && !global_search
        && prefer_smart_title_health
        && state.choice_picker.is_none()
        && !show_renderer_preview_now;
    let compact_smart_title_health =
        show_smart_title_health && width == SettingsWidth::Compact && cx.viewport.height <= 420.0;
    let title = if global_search {
        "Search Results"
    } else {
        state.route.label()
    };
    let subtitle = if global_search && width == SettingsWidth::Compact {
        "Every category, ranked by relevance."
    } else if global_search {
        "Results from every category, ranked by label, key, and authored keywords."
    } else if modified_only && width == SettingsWidth::Compact {
        "Overrides only; reset restores defaults."
    } else if modified_only {
        "Only explicit overrides are shown. Resetting returns to the effective default."
    } else {
        route_subtitle(state.route, width)
    };
    let mut fields: Vec<(u8, usize, &EditField)> = state
        .legacy
        .fields
        .iter()
        .enumerate()
        .filter(|(_, field)| {
            modified_only
                || global_search
                || state
                    .route
                    .section()
                    .is_none_or(|section| prefs::section_of(field.key) == section)
        })
        .filter(|(_, field)| !modified_only || state.is_explicit(field.key))
        .filter_map(|(index, field)| {
            field_match_score(field, &query).map(|score| (score, index, field))
        })
        .collect();
    fields.sort_by_key(|(score, index, field)| {
        (
            *score,
            prefs::section_of(field.key).order_index(),
            prefs::group_of(field.key).1,
            *index,
        )
    });

    let picker = choice_picker_node(state, width, cx.viewport.width);

    // Measure against the exact page origin and clip authored by `page()`. The
    // same budget decides both the semantic virtual window and the painted/hit
    // geometry, including the Smart Titles health showcase.
    let insets = responsive_page_insets(
        cx.viewport.width,
        cx.viewport.height,
        width,
        page_maximum(state.route, width),
    );
    let toolbar_height = if width == SettingsWidth::Compact {
        compact_toolbar_height()
    } else {
        0.0
    };
    let feedback_height = if state.feedback.is_some() {
        scaled_control_height()
    } else {
        0.0
    };
    let top_origin = toolbar_height + insets.top;
    let clip_bottom = cx.viewport.height - feedback_height - insets.bottom + 0.01;
    let body_height = (clip_bottom - top_origin).max(0.0);
    let page_gap = responsive_page_gap(width, cx.viewport.height);
    let results_height = if fields.len() > 1 {
        page_navigation_height()
    } else {
        22.0_f32.max(18.0 * settings_text_scale())
    };
    let first_row_extra = width.row_height() + 4.0 + group_heading_height();
    // Budget-derived compact chrome: the demonstration and one WHOLE control
    // row (paint/hit parity — a clipped hit is a broken promise) always win
    // over decoration. A compact page yields its one-line subtitle first, then
    // the page heading (the compact toolbar keeps naming the route), and
    // finally its pager row (wheel/keyboard still scroll the same bounded
    // window) before an actionable node may be clipped.
    let text_scale = settings_text_scale();
    let showcase_height = if show_renderer_preview_now {
        renderer_preview_height(width)
    } else if show_smart_title_health {
        smart_title_health_height(compact_smart_title_health)
    } else {
        0.0
    };
    // Exact stacked geometry of the pager and the first control under a
    // candidate chrome configuration — the same arithmetic the layout
    // performs, so the decision cannot drift from it. Returns
    // (pager_bottom, control_top, control_bottom).
    let stacked = |heading: bool, subtitle: bool, pager: bool, group_label: bool| {
        let mut top = top_origin;
        if heading {
            top += page_heading_height() + page_gap;
        }
        if subtitle {
            top += page_subtitle_height() + page_gap;
        }
        if showcase_height > 0.0 {
            top += showcase_height + page_gap;
        }
        let pager_bottom = if pager { top + results_height } else { top };
        if pager {
            top += results_height + page_gap;
        }
        top += 14.0; // group card top padding
        if group_label {
            top += group_heading_label_height() + 4.0;
        }
        // The row stacks its label over a control that fills the remainder.
        let control_top = top + 22.0_f32.max(20.0 * text_scale) + 4.0;
        let control_bottom = top + width.row_height();
        (pager_bottom, control_top, control_bottom)
    };
    let has_subtitle = !subtitle.is_empty();
    // Preference order: full chrome, then shed subtitle → heading → pager →
    // group caption. Pick the first configuration whose control paints WHOLE;
    // if none exists, the first whose control is entirely below the clip while
    // the pager (also a hit) stays whole.
    const SHED_STAGES: [(bool, bool, bool, bool); 5] = [
        (true, true, true, true),
        (true, false, true, true),
        (false, false, true, true),
        (false, false, false, true),
        (false, false, false, false),
    ];
    let shedding = width == SettingsWidth::Compact
        && !preview_disclosure
        && picker.is_none()
        && !fields.is_empty();
    let (show_heading, subtitle, show_pager, show_group_labels) = if preview_disclosure {
        let heading = if state.page_scroll == 0 || fields.is_empty() || picker.is_some() {
            // The disclosure page stacks heading + demonstration + its pager
            // with the exact responsive gaps inside the page clip.
            top_origin
                + page_heading_height()
                + page_gap
                + renderer_preview_height(width)
                + page_gap
                + results_height
                <= clip_bottom
        } else {
            page_heading_height() + page_gap + results_height + page_gap + first_row_extra
                <= body_height
        };
        let pager = state.page_scroll == 0
            || fields.is_empty()
            || picker.is_some()
            || results_height + page_gap + first_row_extra <= body_height;
        (heading, "", pager, true)
    } else if !shedding {
        (true, subtitle, true, true)
    } else {
        let stage = SHED_STAGES
            .iter()
            .copied()
            .find(|&(h, s, p, l)| stacked(h, s && has_subtitle, p, l).2 <= clip_bottom)
            .or_else(|| {
                SHED_STAGES.iter().copied().find(|&(h, s, p, l)| {
                    let (pager_bottom, control_top, _) = stacked(h, s && has_subtitle, p, l);
                    control_top >= clip_bottom && pager_bottom <= clip_bottom
                })
            })
            .unwrap_or((true, true, true, true));
        (
            stage.0,
            if stage.1 { subtitle } else { "" },
            stage.2,
            stage.3,
        )
    };
    // Sum exactly the prefix nodes that will be emitted. One trailing parent
    // gap after each prefix includes the separation before the first group.
    let mut used = 0.0;
    let mut prefix_nodes = 0usize;
    if show_heading {
        used += page_heading_height();
        prefix_nodes += 1;
    }
    if show_heading && !subtitle.is_empty() {
        used += page_subtitle_height();
        prefix_nodes += 1;
    }
    if showcase_height > 0.0 {
        used += showcase_height;
        prefix_nodes += 1;
    }
    if show_pager {
        used += results_height;
        prefix_nodes += 1;
    }
    used += prefix_nodes as f32 * page_gap;

    let mut out = if show_heading {
        page_heading(title, subtitle)
    } else {
        Vec::new()
    };
    if show_renderer_preview_now
        && let Some(preview) = renderer_preview(
            state,
            cx.animation_phase_ms,
            cx.motion,
            cx.terminal_font_px,
            cx.terminal_theme,
            cx.semantic_font.as_ref(),
            width,
        )
    {
        out.push(preview);
    }
    if show_smart_title_health {
        out.push(smart_title_health_card(state, compact_smart_title_health));
    }

    if let Some(picker) = picker {
        out.push(picker);
        // The picker is a complete transient decision surface. Keeping form
        // rows behind it in the same clipped page made their semantics appear
        // reachable even when no pixels or hit targets survived at 2× text.
        // Done restores the virtual form window at its exact prior offset.
        return out;
    }

    if fields.is_empty() {
        out.push(
            UiNode::new(
                "settings/empty",
                UiContent::Text(TextSpec::text(if query.is_empty() {
                    "No modified settings."
                } else {
                    "No settings match this search."
                })),
            )
            .layout(Layout::default().height(Length::Fixed(48.0))),
        );
        return out;
    }

    if preview_disclosure && state.page_scroll == 0 {
        out.push(page_navigation_node(
            "settings/results-window",
            "settings/results-range",
            "preview and settings",
            0,
            1,
            fields.len().saturating_add(1),
        ));
        return out;
    }

    let field_offset = if preview_disclosure {
        state.page_scroll.saturating_sub(1)
    } else {
        state.page_scroll
    };
    let start = field_offset.min(fields.len().saturating_sub(1));
    let mut visible_count = 0usize;
    let mut previous_group: Option<(usize, &str)> = None;
    for (_, _, field) in fields.iter().skip(start) {
        let group = (
            prefs::section_of(field.key).order_index(),
            prefs::group_of(field.key).0,
        );
        let new_group = previous_group != Some(group);
        let extra = width.row_height()
            + 4.0
            + if new_group {
                group_heading_height() + if visible_count == 0 { 0.0 } else { page_gap }
            } else {
                0.0
            };
        if visible_count > 0 && used + extra > body_height {
            break;
        }
        used += extra;
        visible_count += 1;
        previous_group = Some(group);
    }
    let visible_count = visible_count.max(1);
    let end = start.saturating_add(visible_count).min(fields.len());
    let total = fields.len();
    if !show_pager {
        // The tightest disclosure slice spends every row on the control
        // itself; wheel, keyboard, and the toolbar's back affordance remain
        // the bounded window's navigation.
    } else if start > 0 || end < total {
        out.push(page_navigation_node(
            "settings/results-window",
            "settings/results-range",
            "settings",
            start,
            end,
            total,
        ));
    } else {
        out.push(
            UiNode::new(
                "settings/results-window",
                UiContent::Group(GroupSpec::new("Visible settings range")),
            )
            .layout(
                Layout::row()
                    .height(Length::Fixed(results_height))
                    .gap(6.0),
            )
            .children(vec![
                UiNode::new(
                    "settings/results-range",
                    UiContent::Text(TextSpec {
                        text: if width == SettingsWidth::Compact {
                            format!("{total} settings")
                        } else {
                            format!(
                                "{total} settings  ·  choices save immediately; text saves on Return"
                            )
                        },
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
            ]),
        );
    }
    let fields = &fields[start..end];
    let mut groups = Vec::new();
    let mut cursor = 0usize;
    while cursor < fields.len() {
        let section = prefs::section_of(fields[cursor].2.key);
        let group = prefs::group_of(fields[cursor].2.key).0;
        let caption = if global_search {
            format!("{} · {group}", section.label())
        } else {
            group.to_string()
        };
        let start = cursor;
        while cursor < fields.len()
            && prefs::section_of(fields[cursor].2.key) == section
            && prefs::group_of(fields[cursor].2.key).0 == group
        {
            cursor += 1;
        }
        // Relevance is the primary global-search sort key, so one semantic
        // group can legitimately appear in multiple non-contiguous runs.
        // Anchor each run to its first stable setting key; caption-only keys
        // collided for queries such as "font".
        let group_anchor = key_fragment(fields[start].2.key);
        let rows: Vec<UiNode> = fields[start..cursor]
            .iter()
            .map(|(_, index, field)| {
                setting_row(state, *index, field, modified_only, global_search, width)
            })
            .collect();
        let height = if show_group_labels {
            group_heading_height() + rows.len() as f32 * (width.row_height() + 4.0)
        } else {
            // padding pair + rows and their inter-row gaps, no caption row
            24.0 + rows.len() as f32 * (width.row_height() + 4.0)
        };
        let label = show_group_labels.then(|| {
            UiNode::new(
                format!(
                    "settings/group-heading/{}/{}",
                    key_fragment(&caption),
                    group_anchor
                ),
                UiContent::Text(TextSpec {
                    text: caption.to_ascii_uppercase(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(group_heading_label_height())))
        });
        groups.push(
            UiNode::new(
                format!(
                    "settings/group/{}/{}/{}",
                    state.route.path(),
                    key_fragment(&caption),
                    group_anchor,
                ),
                UiContent::Group(GroupSpec::new(caption.clone()).style(StyleRef::Secondary)),
            )
            .layout(
                Layout::column()
                    .height(Length::Fixed(height))
                    .padding(Insets::all(14.0))
                    .gap(4.0)
                    .clipped(),
            )
            .children(label.into_iter().chain(rows).collect()),
        );
    }
    out.extend(groups);
    out
}

fn smart_title_health_height(compact: bool) -> f32 {
    let scale = settings_text_scale();
    let line_height = 24.0_f32.max(20.0 * scale);
    if compact {
        // Short landscape keeps one truthful headline plus one complete
        // consolidated diagnostic row. This leaves enough height for a whole
        // 2× Dynamic-Type control instead of clipping both the card and form.
        2.0 * line_height + 24.0
    } else {
        28.0_f32.max(22.0 * scale) + 6.0 * line_height + 24.0
    }
}

fn smart_title_health_card(state: &SettingsViewState, compact: bool) -> UiNode {
    let line_height = 24.0_f32.max(20.0 * settings_text_scale());
    let heading_height = 28.0_f32.max(22.0 * settings_text_scale());
    let (headline, headline_style, locality, transport, readiness, detail) = if let Some(health) =
        state.title_summary_health.as_ref()
    {
        let state_label = match health.state {
            TitleSummaryRuntimeState::Disabled => "Disabled",
            TitleSummaryRuntimeState::Builtin => "Ready",
            TitleSummaryRuntimeState::Idle => "Waiting for activity",
            TitleSummaryRuntimeState::Starting => "Starting",
            TitleSummaryRuntimeState::Ready => "Ready",
            TitleSummaryRuntimeState::BackingOff => "Retrying later",
            TitleSummaryRuntimeState::Error => "Needs attention",
        };
        let headline_style = match health.state {
            TitleSummaryRuntimeState::Ready | TitleSummaryRuntimeState::Builtin => {
                StyleRef::Success
            }
            TitleSummaryRuntimeState::Error => StyleRef::Danger,
            _ => StyleRef::Primary,
        };
        let headline = format!(
            "{} provider  ·  {state_label}",
            choice_label(health.provider.as_str())
        );
        let locality = match health.locality {
            TitleSummaryLocality::ManagedLocal => {
                "Attested runtime closure: every runtime code file passed pinned Apple Developer ID, Ollama code identity, permissions, and stable-identity checks; repeated before context is sent; direct loopback; cloud integration disabled."
                    .to_string()
            }
            TitleSummaryLocality::UnattestedLoopback => {
                "Untrusted localhost: not proof of local-only; explicit network consent required."
                    .to_string()
            }
            TitleSummaryLocality::Remote if health.endpoint.is_none() => {
                "Remote provider selected, but no endpoint is configured; no terminal context was sent."
                    .to_string()
            }
            TitleSummaryLocality::Remote => {
                "Remote provider: terminal context leaves this device; HTTPS and consent required."
                    .to_string()
            }
            TitleSummaryLocality::NotApplicable
                if health.provider == crate::app_config::TitleSummaryProvider::Builtin =>
            {
                "On-device heuristics; no process, model, credential, or network access."
                    .to_string()
            }
            TitleSummaryLocality::NotApplicable => "No provider connection is active.".to_string(),
        };
        let readiness = match health.provider {
            crate::app_config::TitleSummaryProvider::Ollama => {
                let runtime = if health.managed_install_present {
                    "managed runtime candidate found (attested before launch)"
                } else {
                    "managed runtime absent (no automatic download)"
                };
                let model = if health.model_ready {
                    "latest request confirmed the configured model"
                } else {
                    "latest request has not confirmed model readiness"
                };
                format!("{runtime}  ·  {model}")
            }
            crate::app_config::TitleSummaryProvider::OpenAiCompatible => {
                health.model.as_deref().map_or_else(
                    || "Provider-hosted model; aterm installs nothing.".to_string(),
                    |model| format!("Model {model} is provider-hosted; aterm installs nothing."),
                )
            }
            crate::app_config::TitleSummaryProvider::Builtin => {
                "Activity uses the built-in deterministic summarizer.".to_string()
            }
            crate::app_config::TitleSummaryProvider::Off => {
                "Generated Activity is off; an authored Description remains available.".to_string()
            }
        };
        let transport = if matches!(
            health.provider,
            crate::app_config::TitleSummaryProvider::Builtin
                | crate::app_config::TitleSummaryProvider::Off
        ) {
            "Transport not used.".to_string()
        } else {
            let timeout = health.timeout.map_or_else(
                || "timeout unknown".to_string(),
                |timeout| format!("{}s timeout", timeout.as_secs()),
            );
            let proxy = if health.provider == crate::app_config::TitleSummaryProvider::Ollama {
                "direct connection (forced)".to_string()
            } else {
                match health.proxy_mode {
                    Some(crate::app_config::TitleSummaryProxyMode::Environment) => {
                        "environment proxy + NO_PROXY".to_string()
                    }
                    Some(crate::app_config::TitleSummaryProxyMode::Direct) => {
                        "direct connection".to_string()
                    }
                    None => "proxy policy unknown".to_string(),
                }
            };
            let endpoint_uses_tls = health
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| endpoint.starts_with("https://"));
            let trust = if !endpoint_uses_tls {
                "TLS/CA not used"
            } else if health.ca_file.is_some() {
                "configured CA roots (platform roots replaced)"
            } else {
                "platform trust roots"
            };
            format!("Transport: {timeout}  ·  {proxy}  ·  {trust}")
        };
        let detail = if let Some(error) = health.last_error.as_deref() {
            health.next_retry_after.map_or_else(
                || format!("Last error: {error}"),
                |retry| {
                    format!(
                        "Last error: {error}  ·  Error retry in about {} seconds.",
                        retry.as_secs().max(1)
                    )
                },
            )
        } else if let Some(retry) = health.next_retry_after {
            format!("Error retry in about {} seconds.", retry.as_secs().max(1))
        } else if let Some(refresh) = health.next_refresh_after {
            format!(
                "Routine activity refresh in about {} seconds.",
                refresh.as_secs().max(1)
            )
        } else {
            "No provider error reported.".to_string()
        };
        (
            headline,
            headline_style,
            locality,
            transport,
            readiness,
            detail,
        )
    } else {
        (
            format!(
                "{} provider  ·  Status unavailable",
                choice_label(&field_text(
                    state,
                    prefs::EDIT_TITLE_SUMMARY_PROVIDER,
                    "builtin",
                ))
            ),
            StyleRef::Danger,
            "Runtime status unavailable; locality is not inferred from the endpoint.".to_string(),
            "Transport policy and certificate trust are unknown.".to_string(),
            "Provider and model readiness are unknown.".to_string(),
            "Manual connection testing is not available in this build.".to_string(),
        )
    };

    let status_line = |key: &str, text: String, role: SemanticRole, style: StyleRef| {
        UiNode::new(
            format!("settings/smart-titles/health/{key}"),
            UiContent::Text(TextSpec { text, role, style }),
        )
        .layout(Layout::default().height(Length::Fixed(line_height)))
    };
    let mut children = if compact {
        vec![
            status_line("state", headline, SemanticRole::Status, headline_style),
            status_line(
                "summary",
                format!("{locality}  ·  {transport}  ·  {readiness}  ·  {detail}"),
                SemanticRole::Status,
                if state
                    .title_summary_health
                    .as_ref()
                    .is_some_and(|health| health.last_error.is_some())
                {
                    StyleRef::Danger
                } else {
                    StyleRef::Quiet
                },
            ),
        ]
    } else {
        vec![
            UiNode::new(
                "settings/smart-titles/health/heading",
                UiContent::Text(TextSpec {
                    text: "LIVE ACTIVITY STATUS".to_string(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(heading_height))),
            status_line("state", headline, SemanticRole::Status, headline_style),
            status_line(
                "locality",
                locality,
                SemanticRole::Status,
                StyleRef::Primary,
            ),
            status_line(
                "transport",
                transport,
                SemanticRole::Status,
                StyleRef::Primary,
            ),
            status_line(
                "readiness",
                readiness,
                SemanticRole::Status,
                StyleRef::Primary,
            ),
            status_line(
                "detail",
                detail,
                SemanticRole::Status,
                if state
                    .title_summary_health
                    .as_ref()
                    .is_some_and(|health| health.last_error.is_some())
                {
                    StyleRef::Danger
                } else {
                    StyleRef::Quiet
                },
            ),
        ]
    };
    if !compact {
        children.push(status_line(
            "precedence",
            "Authored Description wins; generated Activity is its fallback.".to_string(),
            SemanticRole::Text,
            StyleRef::Quiet,
        ));
    }
    UiNode::new(
        "settings/smart-titles/health",
        UiContent::Group(GroupSpec::new("Smart Titles runtime health").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(smart_title_health_height(compact)))
            .padding(Insets::all(12.0))
            .gap(0.0)
            .clipped(),
    )
    .children(children)
}

fn setting_row(
    state: &SettingsViewState,
    index: usize,
    field: &EditField,
    modified_only: bool,
    global_search: bool,
    width: SettingsWidth,
) -> UiNode {
    let value = SettingsState::display_value(field).to_string();
    let editing_input = (state.editing_field.as_deref() == Some(field.key))
        .then(|| state.field_inputs.get(field.key))
        .flatten();
    let action = ActionId::new(format!("settings/set/{}", field.key));
    let focused = state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|key| key.as_str() == format!("settings/control/{}", field.key));
    let pending = state.config_key_pending(field.key);
    let control_state = ControlState {
        enabled: !pending,
        focused,
        busy: pending,
        ..ControlState::default()
    };
    let control = match field.kind {
        EditKind::Bool => UiContent::Switch(
            Control::new(
                SwitchSpec {
                    label: field.label.to_string(),
                    description: Some(field.key.to_string()),
                },
                action,
            )
            .value(SemanticValue::Bool(value.parse().unwrap_or(false)))
            .state(control_state)
            .style(StyleRef::Setting),
        ),
        EditKind::Enum { .. } | EditKind::Theme => UiContent::Button(
            Control::new(
                ButtonSpec {
                    label: value.clone(),
                    visual_label: Some(choice_label(&value)),
                    visual_icon: None,
                    trailing_icon: Some(ButtonIcon::ChevronDown),
                    description: Some(field.key.to_string()),
                },
                action,
            )
            .value(SemanticValue::Text(value))
            .state(ControlState {
                selected: state
                    .choice_picker
                    .as_ref()
                    .is_some_and(|picker| picker.key == field.key),
                ..control_state
            })
            .style(StyleRef::Setting),
        ),
        EditKind::Float | EditKind::Integer if numeric_slider_value(field).is_some() => {
            let range = prefs::range_of(field.key).expect("numeric slider has an authored range");
            let (numeric, display_value) =
                numeric_slider_value(field).expect("guard checked numeric slider value");
            UiContent::Slider(
                Control::new(
                    SliderSpec {
                        label: field.label.to_string(),
                        step: range.step,
                        display_value,
                    },
                    action,
                )
                .value(SemanticValue::Number {
                    value: numeric,
                    minimum: range.min,
                    maximum: range.max,
                })
                .state(control_state)
                .style(StyleRef::Setting),
            )
        }
        EditKind::Float | EditKind::Integer | EditKind::Text | EditKind::Color => {
            let field_text = editing_input.map_or_else(
                || field.seed.clone().unwrap_or_default(),
                |input| input.projection().text,
            );
            let semantic_text = if editing_input.is_some() {
                field_text.clone()
            } else if field_text.trim().is_empty() {
                value.clone()
            } else {
                field_text.clone()
            };
            let swatch = matches!(field.kind, EditKind::Color)
                .then(|| {
                    let candidate = if field_text.trim().is_empty() {
                        value.as_str()
                    } else {
                        field_text.as_str()
                    };
                    crate::app_config::parse_hex_color(candidate)
                        .map(|color| [color.r, color.g, color.b])
                })
                .flatten();
            UiContent::TextField(
                Control::new(
                    TextFieldSpec {
                        label: field.label.to_string(),
                        placeholder: Some(field.placeholder.clone()),
                        secret: false,
                        visual_value: Some(field_text),
                        input: editing_input
                            .map(crate::native_text_input::TextInputState::projection),
                        swatch,
                    },
                    action,
                )
                .value(SemanticValue::Text(semantic_text))
                .state(control_state)
                .style(StyleRef::Setting),
            )
        }
    };
    let mut label = if global_search {
        format!("{} · {}", field.label, prefs::section_of(field.key).label())
    } else {
        field.label.to_string()
    };
    label.push_str(if state.is_explicit(field.key) {
        "  ·  Override"
    } else {
        "  ·  Default"
    });
    let label_node =
        UiNode::new(
            format!("settings/label/{}", field.key),
            UiContent::Text(TextSpec {
                text: label,
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(
            if width == SettingsWidth::Compact {
                Length::Fixed(22.0_f32.max(20.0 * settings_text_scale()))
            } else {
                Length::Fill
            },
        ));
    let control_node = UiNode::new(format!("settings/control/{}", field.key), control).layout(
        Layout::default()
            .width(if width == SettingsWidth::Compact {
                if matches!(field.kind, EditKind::Bool) {
                    Length::Fixed(104.0 * settings_text_scale().min(1.4))
                } else {
                    Length::Fill
                }
            } else {
                match field.kind {
                    EditKind::Bool => Length::Fixed(104.0 * settings_text_scale().min(1.4)),
                    EditKind::Enum { .. } | EditKind::Theme => {
                        Length::Fixed(270.0 * settings_text_scale().min(1.25))
                    }
                    _ => Length::Fixed(300.0 * settings_text_scale().min(1.25)),
                }
            })
            .height(Length::Fill),
    );
    let mut control_children = vec![control_node];
    if modified_only {
        control_children.push(
            UiNode::new(
                format!("settings/reset/{}", field.key),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Reset"),
                        ActionId::new(format!("settings/reset/{}", field.key)),
                    )
                    .state(ControlState {
                        enabled: !pending,
                        busy: pending,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(72.0 * settings_text_scale().min(1.5)))
                    .height(Length::Fill),
            ),
        );
    }
    if width == SettingsWidth::Compact && matches!(field.kind, EditKind::Bool) {
        // A fill spacer keeps the compact switch anchored to the trailing edge
        // (after an optional Reset action) while the switch itself remains a
        // recognizable 104px control instead of stretching into a full-width
        // bar.  The row's 46px control band is also the complete hit target.
        let switch = control_children.remove(0);
        control_children.insert(
            0,
            UiNode::new(
                format!("settings/control-spacer/{}", field.key),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        );
        control_children.push(switch);
    }
    let (layout, children) = if width == SettingsWidth::Compact {
        let controls = UiNode::new(
            format!("settings/controls/{}", field.key),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::row()
                .width(Length::Fill)
                .height(Length::Fill)
                .gap(6.0),
        )
        .children(control_children);
        (
            Layout::column()
                .height(Length::Fixed(width.row_height()))
                .gap(4.0),
            vec![label_node, controls],
        )
    } else {
        let mut children = vec![label_node];
        children.extend(control_children);
        (
            Layout::row()
                .height(Length::Fixed(width.row_height()))
                .padding(Insets::symmetric(0.0, 4.0))
                .gap(12.0),
            children,
        )
    };
    UiNode::new(
        format!("settings/row/{index}/{}", field.key),
        UiContent::Group(GroupSpec::new(field.label)),
    )
    .layout(layout)
    .children(children)
}

fn numeric_slider_value(field: &EditField) -> Option<(f64, String)> {
    let range = prefs::range_of(field.key)?;
    let raw = SettingsState::display_value(field);
    let token = raw.split_whitespace().next().unwrap_or(raw);
    let value = token.parse::<f64>().ok()?;
    if !value.is_finite() || !(range.min..=range.max).contains(&value) {
        return None;
    }
    let snapped = range.min + ((value - range.min) / range.step).round() * range.step;
    ((snapped - value).abs() <= f64::EPSILON * value.abs().max(1.0) * 8.0)
        .then(|| (value, token.to_string()))
}

fn normalize_slider_value(value: f64, range: prefs::Range) -> Option<String> {
    if !value.is_finite() || !range.step.is_finite() || range.step <= 0.0 {
        return None;
    }
    let clamped = value.clamp(range.min, range.max);
    let steps = ((clamped - range.min) / range.step).round();
    Some(
        (range.min + steps * range.step)
            .clamp(range.min, range.max)
            .to_string(),
    )
}

fn about_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    viewport_width: f32,
    viewport_height: f32,
) -> Vec<UiNode> {
    let about = &state.about;
    let text_scale = settings_text_scale();
    let compact_section_height = compact_special_section_height(state, viewport_height);
    let short_compact = width == SettingsWidth::Compact && compact_section_height < 270.0;
    let split_compact_actions =
        width == SettingsWidth::Compact && (text_scale > 1.25 || short_compact);
    let value = |wanted: &str, fallback: &str| {
        about
            .semantic_rows()
            .iter()
            .find(|(key, _)| *key == wanted)
            .map_or_else(|| fallback.to_string(), |(_, value)| value.clone())
    };
    let compact_actions_height = scaled_control_height() * 2.0 + 8.0;
    let hero_actions = UiNode::new(
        "about/actions",
        UiContent::Group(GroupSpec::new("About actions")),
    )
    .layout(if width == SettingsWidth::Compact {
        Layout::column()
            .height(Length::Fixed(compact_actions_height))
            .gap(8.0)
    } else {
        Layout::row()
            .height(Length::Fixed(40.0_f32.max(32.0 * text_scale)))
            .gap(8.0)
    })
    .children(vec![
        UiNode::new(
            "about/copy-build-info",
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Copy Build Information"),
                    ActionId::new("about/copy-build-info"),
                )
                .style(StyleRef::Primary),
            ),
        )
        .layout(
            Layout::default()
                .width(if width == SettingsWidth::Compact {
                    Length::Fill
                } else {
                    Length::Fixed(190.0)
                })
                .height(Length::Fill),
        ),
        UiNode::new(
            "about/open-site",
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Open Project Site"),
                    ActionId::new("about/open-site"),
                )
                .state(ControlState {
                    enabled: crate::about::site_url(about).is_some(),
                    ..ControlState::default()
                })
                .style(StyleRef::Secondary),
            ),
        )
        .layout(
            Layout::default()
                .width(if width == SettingsWidth::Compact {
                    Length::Fill
                } else {
                    Length::Fixed(160.0)
                })
                .height(Length::Fill),
        ),
    ]);
    let eyebrow_height = 20.0_f32.max(16.0 * text_scale);
    let wordmark_height = 38.0_f32.max(30.0 * text_scale);
    let tagline_height = 26.0_f32.max(20.0 * text_scale);
    let status_height = 20.0_f32.max(18.0 * text_scale);
    let capability_height = 22.0_f32.max(18.0 * text_scale);
    let eyebrow = UiNode::new(
        "about/eyebrow",
        UiContent::Text(TextSpec {
            text: if width == SettingsWidth::Compact {
                "NATIVE TERMINAL · APP PLATFORM"
            } else {
                "THE NATIVE TERMINAL, AS AN APP PLATFORM"
            }
            .to_string(),
            role: SemanticRole::Heading,
            style: StyleRef::Quiet,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(eyebrow_height)));
    let wordmark = UiNode::new(
        "about/wordmark",
        UiContent::Text(TextSpec {
            text: "aterm".to_string(),
            role: SemanticRole::Heading,
            style: StyleRef::Hero,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(wordmark_height)));
    let tagline = UiNode::new(
        "about/tagline",
        UiContent::Text(TextSpec {
            text: if width == SettingsWidth::Compact {
                "Fast, hardened, and introspectable.".to_string()
            } else {
                value(
                    "tagline",
                    "A fast, introspectable terminal built as one trustworthy native surface.",
                )
            },
            role: SemanticRole::Text,
            style: StyleRef::Plain,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(tagline_height)));
    let version_summary = UiNode::new(
        "about/version-summary",
        UiContent::Text(TextSpec {
            text: format!(
                "Version {}  ·  Build {}",
                value("version", "development"),
                value("build", "local")
            ),
            role: SemanticRole::Status,
            style: StyleRef::Quiet,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(status_height)));
    let capabilities = UiNode::new(
        "about/byline",
        UiContent::Text(TextSpec {
            text: crate::build_info::AUTHOR_COMPANY_BYLINE.to_string(),
            role: SemanticRole::Text,
            style: StyleRef::Success,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(capability_height)));
    // A 320px-tall landscape host has room for a complete identity summary,
    // but not for decorative eyebrow/capability lines plus two touch actions.
    // Keep the essential wordmark, purpose, and exact version together; the
    // actions are the next pager section and no child is clipped.
    let mut hero_children = if short_compact {
        vec![wordmark, tagline, version_summary]
    } else {
        vec![eyebrow, wordmark, tagline, version_summary, capabilities]
    };
    if !split_compact_actions {
        hero_children.push(hero_actions.clone());
    }
    let hero_vertical_padding = if width == SettingsWidth::Compact {
        12.0
    } else {
        20.0
    };
    let identity_height = if short_compact {
        wordmark_height + tagline_height + status_height
    } else {
        eyebrow_height + wordmark_height + tagline_height + status_height + capability_height
    };
    let hero_height = (identity_height
        + if split_compact_actions {
            0.0
        } else if width == SettingsWidth::Compact {
            compact_actions_height
        } else {
            40.0_f32.max(32.0 * text_scale)
        }
        + hero_vertical_padding * 2.0
        + hero_children.len().saturating_sub(1) as f32 * 6.0)
        .max(if short_compact {
            0.0
        } else if width == SettingsWidth::Compact {
            270.0
        } else {
            238.0
        });
    let hero = UiNode::new(
        "about/hero",
        UiContent::Group(GroupSpec::new("About aterm").style(StyleRef::Primary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(hero_height))
            .padding(Insets::symmetric(24.0, hero_vertical_padding))
            .gap(6.0),
    )
    .children(hero_children);

    let build_rows = about
        .semantic_rows()
        .iter()
        .filter(|(key, _)| !matches!(*key, "tagline" | "author" | "company" | "site"))
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect::<Vec<_>>();
    let support_rows = vec![
        (
            "Project".to_string(),
            value("site", "Local source checkout"),
        ),
        (
            "Interface".to_string(),
            "Native semantic tab app".to_string(),
        ),
        ("Capture".to_string(), "WYSIWYG control image".to_string()),
        (
            "Accessibility".to_string(),
            "Structured native tree".to_string(),
        ),
    ];
    let metadata_value_width = about_metadata_value_width(viewport_width, width);
    let row_count = build_rows.len().max(support_rows.len());
    let metadata_heading_height = 22.0_f32.max(16.0 * text_scale);
    let metadata_row_height = 28.0_f32.max(18.0 * text_scale);
    let card_height = ABOUT_METADATA_CARD_PADDING * 2.0
        + metadata_heading_height
        + row_count as f32 * (metadata_row_height + 4.0);
    let build = metadata_card(
        "about/provenance",
        "BUILD INFORMATION",
        &build_rows,
        card_height,
        metadata_value_width,
    );
    let support = metadata_card(
        "about/support",
        "RUNTIME & SUPPORT",
        &support_rows,
        card_height,
        metadata_value_width,
    );
    if width == SettingsWidth::Compact {
        let mut sections = vec![hero];
        if split_compact_actions {
            sections.push(hero_actions);
        }
        if short_compact {
            let maximum_rows = ((compact_section_height
                - ABOUT_METADATA_CARD_PADDING * 2.0
                - metadata_heading_height)
                / (metadata_row_height + 4.0))
                .floor()
                .max(1.0) as usize;
            for (index, rows) in build_rows.chunks(maximum_rows).enumerate() {
                let key = if index == 0 {
                    "about/provenance".to_string()
                } else {
                    format!("about/provenance/continued-{index}")
                };
                let heading = if index == 0 {
                    "BUILD INFORMATION"
                } else {
                    "BUILD INFORMATION · CONTINUED"
                };
                let height = ABOUT_METADATA_CARD_PADDING * 2.0
                    + metadata_heading_height
                    + rows.len() as f32 * (metadata_row_height + 4.0);
                sections.push(metadata_card(
                    &key,
                    heading,
                    rows,
                    height,
                    metadata_value_width,
                ));
            }
            for (index, rows) in support_rows.chunks(maximum_rows).enumerate() {
                let key = if index == 0 {
                    "about/support".to_string()
                } else {
                    format!("about/support/continued-{index}")
                };
                let heading = if index == 0 {
                    "RUNTIME & SUPPORT"
                } else {
                    "RUNTIME & SUPPORT · CONTINUED"
                };
                let height = ABOUT_METADATA_CARD_PADDING * 2.0
                    + metadata_heading_height
                    + rows.len() as f32 * (metadata_row_height + 4.0);
                sections.push(metadata_card(
                    &key,
                    heading,
                    rows,
                    height,
                    metadata_value_width,
                ));
            }
        } else {
            sections.push(build);
            sections.push(compact_about_support(about));
        }
        let total = sections.len();
        let section = state.page_scroll.min(total.saturating_sub(1));
        let section_node = sections.swap_remove(section);
        return vec![
            page_navigation_node(
                "about/pagination",
                "about/range",
                "About sections",
                section,
                section + 1,
                total,
            ),
            section_node,
        ];
    }
    let cards_layout = Layout::row()
        .height(Length::Fixed(card_height))
        .gap(ABOUT_METADATA_CARD_GAP);
    let details = UiNode::new(
        "about/details",
        UiContent::Group(GroupSpec::new("About details")),
    )
    .layout(cards_layout)
    .children(vec![build, support]);
    let content_height = noncompact_page_content_height(
        state,
        width,
        viewport_width,
        viewport_height,
        if width == SettingsWidth::Wide {
            1_000.0
        } else {
            760.0
        },
    );
    let dashboard_height = hero_height + 12.0 + card_height;
    if dashboard_height > content_height || state.page_scroll > 0 {
        // The ordinary 849x460 desktop viewport cannot contain both fixed
        // cards. Present two complete semantic sections instead of clipping
        // build metadata below the fold. This is the same reducer action as
        // compact About, so pointer, keyboard, switch control, and a11y all
        // move the identical `page_scroll` state.
        let mut sections = vec![hero, details];
        let total = sections.len();
        let section = state.page_scroll.min(total.saturating_sub(1));
        let section_node = sections.swap_remove(section);
        return vec![
            page_navigation_node(
                "about/pagination",
                "about/range",
                "About sections",
                section,
                section + 1,
                total,
            ),
            section_node,
        ];
    }
    let mut out = vec![hero, details];
    if width == SettingsWidth::Wide && viewport_height >= 840.0 {
        out.push(about_principles());
    }
    out
}

fn compact_about_support(about: &AboutState) -> UiNode {
    let site = about
        .semantic_rows()
        .iter()
        .find(|(key, _)| *key == "site")
        .map_or("Local project", |(_, value)| value.as_str());
    let row = |id: &str, label: &str, value: String| {
        UiNode::new(
            format!("about/support/{id}"),
            UiContent::Group(GroupSpec::new(label)),
        )
        .layout(Layout::column().height(Length::Fixed(40.0)).gap(1.0))
        .children(vec![
            UiNode::new(
                format!("about/support/{id}/label"),
                UiContent::Text(TextSpec {
                    text: label.to_ascii_uppercase(),
                    role: SemanticRole::Heading,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(18.0))),
            UiNode::new(
                format!("about/support/{id}/value"),
                UiContent::Text(TextSpec {
                    text: value,
                    role: SemanticRole::Text,
                    style: StyleRef::Primary,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(21.0))),
        ])
    };
    UiNode::new(
        "about/support",
        UiContent::Group(GroupSpec::new("Runtime & support").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(230.0_f32.max(190.0 * settings_text_scale())))
            .padding(Insets::all(12.0))
            .gap(6.0),
    )
    .children(vec![
        UiNode::new(
            "about/support/heading",
            UiContent::Text(TextSpec {
                text: "RUNTIME & SUPPORT".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default().height(Length::Fixed(20.0_f32.max(16.0 * settings_text_scale()))),
        ),
        row("project", "Project", site.to_string()),
        row(
            "interface",
            "Interface",
            "Native semantic tab app".to_string(),
        ),
        row("capture", "Capture", "WYSIWYG exact pixels".to_string()),
        row(
            "accessibility",
            "Accessibility",
            "Structured native tree".to_string(),
        ),
    ])
}

fn about_principles() -> UiNode {
    let principles = [
        (
            "one-surface",
            "ONE SURFACE",
            "One tab system for terminals, tools, and documents.",
        ),
        (
            "visible-state",
            "VISIBLE STATE",
            "One structured model drives pixels and accessibility.",
        ),
        (
            "native-default",
            "NATIVE BY DEFAULT",
            "Keyboard speed meets native window behavior.",
        ),
    ];
    UiNode::new(
        "about/principles",
        UiContent::Group(GroupSpec::new("aterm design principles").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(140.0))
            .padding(Insets::all(18.0))
            .gap(10.0),
    )
    .children(vec![
        UiNode::new(
            "about/principles-heading",
            UiContent::Text(TextSpec {
                text: "WHY ATERM".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(22.0))),
        UiNode::new(
            "about/principles-grid",
            UiContent::Group(GroupSpec::new("Design principle summaries")),
        )
        .layout(Layout::row().height(Length::Fill).gap(22.0))
        .children(
            principles
                .into_iter()
                .map(|(key, heading, detail)| {
                    UiNode::new(
                        format!("about/principle/{key}"),
                        UiContent::Group(GroupSpec::new(heading)),
                    )
                    .layout(Layout::column().width(Length::Fill).gap(4.0))
                    .children(vec![
                        UiNode::new(
                            format!("about/principle/{key}/heading"),
                            UiContent::Text(TextSpec::heading(heading)),
                        )
                        .layout(Layout::default().height(Length::Fixed(24.0))),
                        UiNode::new(
                            format!("about/principle/{key}/detail"),
                            UiContent::Text(TextSpec {
                                text: detail.to_string(),
                                role: SemanticRole::Text,
                                style: StyleRef::Quiet,
                            }),
                        )
                        .layout(Layout::default().height(Length::Fixed(42.0))),
                    ])
                })
                .collect(),
        ),
    ])
}

const ABOUT_METADATA_CARD_GAP: f32 = 12.0;
const ABOUT_METADATA_CARD_PADDING: f32 = 18.0;
const ABOUT_METADATA_LABEL_WIDTH: f32 = 104.0;
const ABOUT_METADATA_ROW_GAP: f32 = 10.0;
const ABOUT_METADATA_TEXT_SAFETY: f32 = 4.0;

fn about_metadata_label_width() -> f32 {
    ABOUT_METADATA_LABEL_WIDTH * settings_text_scale().min(1.2)
}

/// The exact value-column measure produced by the responsive Settings page and
/// the two-card About workbench. Compact uses one full-width metadata card;
/// Medium and Wide split the available page measure evenly around one gap.
fn about_metadata_value_width(viewport_width: f32, width: SettingsWidth) -> f32 {
    let maximum = if width == SettingsWidth::Wide {
        1_000.0
    } else {
        760.0
    };
    let navigation = match width {
        SettingsWidth::Wide => 216.0,
        SettingsWidth::Medium => 196.0,
        SettingsWidth::Compact => 0.0,
    };
    let insets = page_insets(viewport_width, width, maximum);
    let page_width = (viewport_width - navigation - insets.left - insets.right).max(0.0);
    let card_width = if width == SettingsWidth::Compact {
        page_width
    } else {
        ((page_width - ABOUT_METADATA_CARD_GAP) / 2.0).max(0.0)
    };
    (card_width
        - ABOUT_METADATA_CARD_PADDING * 2.0
        - about_metadata_label_width()
        - ABOUT_METADATA_ROW_GAP)
        .max(0.0)
}

fn metadata_value_is_code(label: &str) -> bool {
    label.eq_ignore_ascii_case("commit") || label.eq_ignore_ascii_case("build")
}

fn metadata_value_text_width(text: &str, px: f32, code: bool) -> f32 {
    if code {
        crate::tray_raster::measure_text(text, px, crate::widget::TextWeight::Regular)
    } else {
        crate::tray_raster::ui_text_width(text, px)
    }
}

/// UTF-8-safe trailing ellipsis using the exact face and logical size that the
/// native tree compiler paints for metadata values.
fn elide_metadata_value(text: &str, max_width: f32, code: bool) -> String {
    let px = 13.0 * crate::native_appearance::text_scale();
    if !max_width.is_finite() || max_width <= 0.0 || !px.is_finite() || px <= 0.0 {
        return String::new();
    }
    let measure = |candidate: &str| metadata_value_text_width(candidate, px, code);
    if measure(text) <= max_width {
        return text.to_string();
    }
    for (end, _) in text.char_indices().rev() {
        let candidate = format!("{}…", &text[..end]);
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }
    String::new()
}

fn metadata_card(
    key: &str,
    heading: &str,
    rows: &[(String, String)],
    height: f32,
    value_width: f32,
) -> UiNode {
    let heading_height = 22.0_f32.max(16.0 * settings_text_scale());
    let row_height = 28.0_f32.max(18.0 * settings_text_scale());
    let mut children = vec![
        UiNode::new(
            format!("{key}/heading"),
            UiContent::Text(TextSpec {
                text: heading.to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(heading_height))),
    ];
    children.extend(rows.iter().map(|(label, value)| {
        let fragment = key_fragment(label);
        let code = metadata_value_is_code(label);
        let visual_value = elide_metadata_value(
            value,
            (value_width - ABOUT_METADATA_TEXT_SAFETY).max(0.0),
            code,
        );
        UiNode::new(
            format!("{key}/row/{fragment}"),
            UiContent::Group(GroupSpec::new(format!("{label}: {value}"))),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(row_height))
                .gap(ABOUT_METADATA_ROW_GAP),
        )
        .children(vec![
            UiNode::new(
                format!("{key}/label/{fragment}"),
                UiContent::Text(TextSpec {
                    text: label.to_string(),
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(about_metadata_label_width()))
                    .height(Length::Fill),
            ),
            UiNode::new(
                format!("{key}/value/{fragment}"),
                UiContent::Text(TextSpec {
                    text: visual_value,
                    role: SemanticRole::Text,
                    style: if code {
                        StyleRef::Code
                    } else {
                        StyleRef::Primary
                    },
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        ])
    }));
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::new(heading).style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .padding(Insets::all(ABOUT_METADATA_CARD_PADDING))
            .gap(4.0)
            .clipped(),
    )
    .children(children)
}

fn update_compact_sections(update: &UpdateProjection) -> usize {
    2 + usize::from(settings_text_scale() > 1.25)
        + usize::from(!update.changelog.is_empty())
        + usize::from(!update.outcome.is_empty())
}

fn update_release_notes_card(update: &UpdateProjection, height: f32) -> UiNode {
    UiNode::new(
        "updates/release-notes-card",
        UiContent::Group(GroupSpec::new("Release notes").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(height))
            .padding(Insets::all(16.0))
            .gap(8.0)
            .clipped(),
    )
    .children(vec![
        UiNode::new(
            "updates/release-notes-heading",
            UiContent::Text(TextSpec {
                text: "RELEASE NOTES".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default().height(Length::Fixed(22.0_f32.max(16.0 * settings_text_scale()))),
        ),
        UiNode::new(
            "updates/release-notes",
            UiContent::RichText(RichTextSpec {
                text: update.changelog.join("\n"),
                selectable: true,
            }),
        )
        .layout(Layout::default().height(Length::Fill).width(Length::Fill)),
    ])
}

fn update_page(
    state: &SettingsViewState,
    update: &UpdateProjection,
    width: SettingsWidth,
    viewport_width: f32,
    viewport_height: f32,
) -> Vec<UiNode> {
    let text_scale = settings_text_scale();
    let compact_section_height = compact_special_section_height(state, viewport_height);
    let short_compact = width == SettingsWidth::Compact && compact_section_height < 270.0;
    let split_compact_actions =
        width == SettingsWidth::Compact && (text_scale > 1.25 || short_compact);
    let split_short_medium_actions =
        width == SettingsWidth::Medium && viewport_height < 620.0 && update.staged.is_some();
    let split_actions = split_compact_actions || split_short_medium_actions;
    let fill_action_width = width == SettingsWidth::Compact || split_short_medium_actions;
    let mut actions = vec![
        UiNode::new(
            "updates/check",
            UiContent::Button(
                Control::new(
                    ButtonSpec::new("Check for Updates"),
                    ActionId::new("updates/check"),
                )
                .state(ControlState {
                    enabled: update.enabled && !update.checking,
                    busy: update.checking,
                    ..ControlState::default()
                })
                .style(if update.staged.is_none() {
                    StyleRef::Primary
                } else {
                    StyleRef::Secondary
                }),
            ),
        )
        .layout(
            Layout::default()
                .width(if fill_action_width {
                    Length::Fill
                } else {
                    Length::Fixed(164.0)
                })
                .height(Length::Fill),
        ),
    ];
    if update.staged.is_some() {
        actions.push(
            UiNode::new(
                "updates/install-relaunch",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Install & Relaunch"),
                        ActionId::new("updates/install-relaunch"),
                    )
                    .style(StyleRef::Primary),
                ),
            )
            .layout(
                Layout::default()
                    .width(if fill_action_width {
                        Length::Fill
                    } else {
                        Length::Fixed(164.0)
                    })
                    .height(Length::Fill),
            ),
        );
        actions.push(
            UiNode::new(
                "updates/install-when-safe",
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new("Install When Safe"),
                        ActionId::new("updates/install-when-safe"),
                    )
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(if fill_action_width {
                        Length::Fill
                    } else {
                        Length::Fixed(164.0)
                    })
                    .height(Length::Fill),
            ),
        );
    }
    let stack_actions = width != SettingsWidth::Wide && actions.len() > 1;
    let action_control_height = 36.0_f32.max(32.0 * text_scale);
    let action_height = if stack_actions {
        actions.len() as f32 * action_control_height + actions.len().saturating_sub(1) as f32 * 8.0
    } else {
        action_control_height
    };
    let action_layout = if stack_actions {
        Layout::column()
            .height(Length::Fixed(action_height))
            .gap(8.0)
    } else {
        Layout::row().height(Length::Fixed(action_height)).gap(8.0)
    };
    let actions_node = UiNode::new(
        "updates/actions",
        UiContent::Group(GroupSpec::new("Update actions")),
    )
    .layout(action_layout)
    .children(actions);
    let title_height = 20.0_f32.max(16.0 * text_scale);
    let headline_height = 42.0_f32.max(30.0 * text_scale);
    let current_height = 22.0_f32.max(18.0 * text_scale);
    let detail_height = 24.0_f32.max(20.0 * text_scale);
    let mut hero_children = vec![
        UiNode::new(
            "updates/title",
            UiContent::Text(TextSpec {
                text: "Software Update".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(title_height))),
        UiNode::new(
            "updates/headline",
            UiContent::Text(TextSpec {
                text: update.headline.clone(),
                role: SemanticRole::Heading,
                style: StyleRef::Hero,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(headline_height))),
        UiNode::new(
            "updates/current",
            UiContent::Text(TextSpec {
                text: format!(
                    "aterm {}  ·  build {}",
                    update.current_version, update.current_build
                ),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(current_height))),
        UiNode::new(
            "updates/detail",
            UiContent::Text(TextSpec {
                text: update.detail.clone().unwrap_or_else(|| {
                    if update.checking {
                        "Contacting the update service…".to_string()
                    } else if update.enabled {
                        if width == SettingsWidth::Compact {
                            "No newer build is staged."
                        } else {
                            "No newer build is staged for this installation."
                        }
                        .to_string()
                    } else {
                        if width == SettingsWidth::Compact {
                            "Update checks are unavailable."
                        } else {
                            "Update checks are unavailable for this installation."
                        }
                        .to_string()
                    }
                }),
                role: SemanticRole::Text,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(detail_height))),
    ];
    if !split_actions {
        hero_children.push(actions_node.clone());
    }
    let hero_height = title_height
        + headline_height
        + current_height
        + detail_height
        + if split_actions { 0.0 } else { action_height }
        + 40.0
        + hero_children.len().saturating_sub(1) as f32 * 8.0;
    let hero = UiNode::new(
        "updates/hero",
        UiContent::Group(GroupSpec::new("Software Update status").style(StyleRef::Primary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(hero_height))
            .padding(Insets::symmetric(22.0, 20.0))
            .gap(8.0),
    )
    .children(hero_children);

    let service_height = if width == SettingsWidth::Compact {
        220.0_f32.max(166.0 * text_scale)
    } else {
        hero_height.max(220.0_f32.max(166.0 * text_scale))
    };
    let workbench_height = hero_height.max(service_height);
    let service = update_service_card(update, service_height);
    let outcome_node = (!update.outcome.is_empty()).then(|| {
        UiNode::new(
            "updates/outcome",
            UiContent::Text(TextSpec {
                text: update.outcome.clone(),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(
            if width == SettingsWidth::Compact {
                44.0_f32.max(32.0 * text_scale)
            } else {
                28.0
            },
        )))
    });
    let mut compact_sections = if width == SettingsWidth::Compact {
        let mut sections = vec![hero.clone()];
        if split_compact_actions {
            sections.push(actions_node.clone());
        }
        sections.push(service.clone());
        sections
    } else {
        Vec::new()
    };
    if width == SettingsWidth::Compact && short_compact {
        // The full three-row installation card is 220px. Split it into two
        // complete cards when a landscape host owns less than that; every row
        // remains visible and introspectable rather than disappearing below a
        // clip edge.
        compact_sections.pop();
        compact_sections.extend([
            update_service_card_rows(update, "updates/detail-card", "THIS INSTALLATION", 0, 2),
            update_service_card_rows(
                update,
                "updates/detail-card/continued",
                "THIS INSTALLATION · CONTINUED",
                2,
                3,
            ),
        ]);
    }
    if width == SettingsWidth::Compact && !update.changelog.is_empty() {
        compact_sections.push(update_release_notes_card(
            update,
            if short_compact {
                compact_section_height
            } else {
                210.0_f32.max(170.0 * text_scale)
            },
        ));
    }
    if width == SettingsWidth::Compact
        && let Some(outcome) = outcome_node.clone()
    {
        compact_sections.push(outcome);
    }
    if width == SettingsWidth::Compact {
        let total = compact_sections.len();
        let section = state.page_scroll.min(total.saturating_sub(1));
        let section_node = compact_sections.swap_remove(section);
        let mut out = vec![page_navigation_node(
            "updates/pagination",
            "updates/range",
            "Update sections",
            section,
            section + 1,
            total,
        )];
        out.push(section_node);
        return out;
    }
    let workbench = UiNode::new(
        "updates/workbench",
        UiContent::Group(GroupSpec::new("Update status dashboard")),
    )
    .layout(if width == SettingsWidth::Compact {
        Layout::column()
            .height(Length::Fixed(hero_height * 2.0 + 12.0))
            .gap(12.0)
            .clipped()
    } else {
        Layout::row()
            .height(Length::Fixed(workbench_height))
            .gap(16.0)
            .clipped()
    })
    .children(vec![
        hero.layout(
            Layout::column()
                .width(if width == SettingsWidth::Compact {
                    Length::Fill
                } else {
                    Length::Fraction(0.62)
                })
                .height(Length::Fill)
                .padding(Insets::symmetric(22.0, 20.0))
                .gap(8.0),
        ),
        service,
    ]);

    let release_notes = (!update.changelog.is_empty())
        .then(|| update_release_notes_card(update, 210.0_f32.max(170.0 * text_scale)));
    let process =
        (update.changelog.is_empty() && viewport_height >= 620.0).then(update_process_card);
    let mut sections = vec![workbench.clone()];
    if split_short_medium_actions {
        sections.push(actions_node.clone());
    }
    if let Some(notes) = release_notes.clone() {
        sections.push(notes);
    }
    if let Some(process) = process.clone() {
        sections.push(process);
    }
    if let Some(outcome) = outcome_node.clone() {
        sections.push(outcome);
    }

    let section_heights = workbench_height
        + if split_short_medium_actions {
            action_height
        } else {
            0.0
        }
        + release_notes
            .as_ref()
            .map_or(0.0, |_| 210.0_f32.max(170.0 * text_scale))
        + process.as_ref().map_or(0.0, |_| 142.0)
        + outcome_node.as_ref().map_or(0.0, |_| 28.0);
    let authored_children = 2 + sections.len();
    let dashboard_height = page_heading_height()
        + page_subtitle_height()
        + section_heights
        + authored_children.saturating_sub(1) as f32 * 12.0;
    let content_height = noncompact_page_content_height(
        state,
        width,
        viewport_width,
        viewport_height,
        if width == SettingsWidth::Wide {
            1_000.0
        } else {
            760.0
        },
    );
    if dashboard_height > content_height || state.page_scroll > 0 {
        // A staged build adds three actions, release notes, and an outcome.
        // At 849x460 the workbench itself fits, but the old linear dashboard
        // clipped every following section and ignored `page_scroll`. Window the
        // complete cards with the shared semantic pager instead.
        let total = sections.len();
        let section = state.page_scroll.min(total.saturating_sub(1));
        let section_node = sections.swap_remove(section);
        let mut out = page_heading("Software Update", update_page_subtitle(width));
        out.extend([
            page_navigation_node(
                "updates/pagination",
                "updates/range",
                "Update sections",
                section,
                section + 1,
                total,
            ),
            section_node,
        ]);
        return out;
    }

    let mut out = page_heading("Software Update", update_page_subtitle(width));
    out.push(workbench);
    if let Some(notes) = release_notes {
        out.push(notes);
    }
    if let Some(process) = process {
        out.push(process);
    }
    if let Some(outcome) = outcome_node {
        out.push(outcome);
    }
    out
}

fn update_service_card(update: &UpdateProjection, height: f32) -> UiNode {
    update_service_card_rows_with_height(
        update,
        "updates/detail-card",
        "THIS INSTALLATION",
        0,
        3,
        height,
    )
}

fn update_service_card_rows(
    update: &UpdateProjection,
    key: &str,
    heading: &str,
    start: usize,
    end: usize,
) -> UiNode {
    let text_scale = settings_text_scale();
    let heading_height = 24.0_f32.max(16.0 * text_scale);
    let label_height = 18.0_f32.max(16.0 * text_scale);
    let value_height = 24.0_f32.max(20.0 * text_scale);
    let row_height = label_height + value_height + 2.0;
    let rows = end.saturating_sub(start);
    let height = 40.0 + heading_height + rows as f32 * row_height + rows as f32 * 8.0;
    update_service_card_rows_with_height(update, key, heading, start, end, height)
}

fn update_service_card_rows_with_height(
    update: &UpdateProjection,
    key: &str,
    heading: &str,
    start: usize,
    end: usize,
    height: f32,
) -> UiNode {
    let text_scale = settings_text_scale();
    let heading_height = 24.0_f32.max(16.0 * text_scale);
    let label_height = 18.0_f32.max(16.0 * text_scale);
    let value_height = 24.0_f32.max(20.0 * text_scale);
    let row_height = label_height + value_height + 2.0;
    let service = if update.enabled {
        "Available"
    } else {
        "Unavailable for this build"
    };
    let staged = update.staged.as_ref().map_or_else(
        || "No update staged".to_string(),
        |(build, version)| format!("Version {version} · build {build}"),
    );
    let rows = [
        ("service", "Update service", service.to_string()),
        (
            "running",
            "Running now",
            format!(
                "{} · build {}",
                update.current_version, update.current_build
            ),
        ),
        ("staged", "Install state", staged),
    ];
    let mut children = vec![
        UiNode::new(
            format!("{key}/heading"),
            UiContent::Text(TextSpec {
                text: heading.to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(heading_height))),
    ];
    children.extend(rows[start.min(rows.len())..end.min(rows.len())].iter().map(
        |(row_key, label, value)| {
            UiNode::new(
                format!("updates/service/{row_key}"),
                UiContent::Group(GroupSpec::new(*label)),
            )
            .layout(Layout::column().height(Length::Fixed(row_height)).gap(2.0))
            .children(vec![
                UiNode::new(
                    format!("updates/service/{row_key}/label"),
                    UiContent::Text(TextSpec {
                        text: (*label).to_string(),
                        role: SemanticRole::Text,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(label_height))),
                UiNode::new(
                    format!("updates/service/{row_key}/value"),
                    UiContent::Text(TextSpec {
                        text: value.clone(),
                        role: SemanticRole::Status,
                        style: if *row_key == "service" && update.enabled {
                            StyleRef::Success
                        } else {
                            StyleRef::Primary
                        },
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(value_height))),
            ])
        },
    ));
    UiNode::new(
        key,
        UiContent::Group(GroupSpec::new(heading).style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .padding(Insets::all(20.0))
            .gap(8.0)
            .clipped(),
    )
    .children(children)
}

fn update_process_card() -> UiNode {
    let steps = [
        (
            "check",
            "1  CHECK",
            "Compare this build with the update service.",
        ),
        ("review", "2  REVIEW", "Review changes before you install."),
        (
            "relaunch",
            "3  RELAUNCH",
            "Install a staged build only when you choose.",
        ),
    ];
    UiNode::new(
        "updates/process",
        UiContent::Group(GroupSpec::new("How aterm updates").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(142.0))
            .padding(Insets::all(18.0))
            .gap(10.0),
    )
    .children(vec![
        UiNode::new(
            "updates/process-heading",
            UiContent::Text(TextSpec {
                text: "A CLEAR, USER-CONTROLLED UPDATE FLOW".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(22.0))),
        UiNode::new(
            "updates/process-steps",
            UiContent::Group(GroupSpec::new("Update flow steps")),
        )
        .layout(Layout::row().height(Length::Fill).gap(22.0))
        .children(
            steps
                .into_iter()
                .map(|(key, heading, detail)| {
                    UiNode::new(
                        format!("updates/process/{key}"),
                        UiContent::Group(GroupSpec::new(heading)),
                    )
                    .layout(Layout::column().width(Length::Fill).gap(4.0))
                    .children(vec![
                        UiNode::new(
                            format!("updates/process/{key}/heading"),
                            UiContent::Text(TextSpec::heading(heading)),
                        )
                        .layout(Layout::default().height(Length::Fixed(24.0))),
                        UiNode::new(
                            format!("updates/process/{key}/detail"),
                            UiContent::Text(TextSpec {
                                text: detail.to_string(),
                                role: SemanticRole::Text,
                                style: StyleRef::Quiet,
                            }),
                        )
                        .layout(Layout::default().height(Length::Fixed(42.0))),
                    ])
                })
                .collect(),
        ),
    ])
}

/// The coherent card count the Packages page virtualizes over: the status
/// hero (with its action row), the consent-switch card, and the program list.
fn packages_sections(_packages: &PackagesProjection) -> usize {
    3
}

fn packages_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Bundled ALab tools and consent."
    } else {
        "The bundled ALab toolchain: what is installed, what updates itself, and the consent switches."
    }
}

/// One registry-backed Switch row for the Packages page. The control uses the
/// SAME `settings/set/<key>` action, focus key, pending gate, and semantic
/// value shape as an ordinary [`setting_row`] Bool, so OCC saves, Undo, and
/// accessibility behave identically — only the placement is special.
fn packages_switch_row(state: &SettingsViewState, key: &str, height: f32) -> Option<UiNode> {
    let field = state.legacy.fields.iter().find(|field| field.key == key)?;
    let value = SettingsState::display_value(field)
        .parse::<bool>()
        .unwrap_or(false);
    let pending = state.config_key_pending(key);
    let focused = state
        .common
        .last_focus
        .as_ref()
        .is_some_and(|focus| focus.as_str() == format!("settings/control/{key}"));
    Some(
        UiNode::new(
            format!("settings/control/{key}"),
            UiContent::Switch(
                Control::new(
                    SwitchSpec {
                        label: field.label.to_string(),
                        description: Some(field.key.to_string()),
                    },
                    ActionId::new(format!("settings/set/{key}")),
                )
                .value(SemanticValue::Bool(value))
                .state(ControlState {
                    enabled: !pending,
                    focused,
                    busy: pending,
                    ..ControlState::default()
                })
                .style(StyleRef::Setting),
            ),
        )
        .layout(Layout::default().height(Length::Fixed(height))),
    )
}

fn packages_page(
    state: &SettingsViewState,
    packages: &PackagesProjection,
    width: SettingsWidth,
    viewport_width: f32,
    viewport_height: f32,
) -> Vec<UiNode> {
    let text_scale = settings_text_scale();
    let title_height = 20.0_f32.max(16.0 * text_scale);
    let headline_height = 42.0_f32.max(30.0 * text_scale);
    let detail_height = 24.0_f32.max(20.0 * text_scale);
    let action_control_height = 36.0_f32.max(32.0 * text_scale);
    let stack_actions = width == SettingsWidth::Compact;

    // Action row: the two verbs through the host executor (co-located atpkg,
    // off the UI thread). Disabled with an honest reason whenever the manager
    // cannot act; the busy one paints as busy while its worker runs.
    let action_button = |key: &str, label: &str, busy: PackagesBusy, primary: bool| {
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(ButtonSpec::new(label), ActionId::new(key))
                    .state(ControlState {
                        enabled: packages.actions_enabled,
                        busy: packages.busy == Some(busy),
                        ..ControlState::default()
                    })
                    .style(if primary {
                        StyleRef::Primary
                    } else {
                        StyleRef::Secondary
                    }),
            ),
        )
        .layout(
            Layout::default()
                .width(if stack_actions {
                    Length::Fill
                } else {
                    Length::Fixed(196.0)
                })
                .height(Length::Fill),
        )
    };
    let actions = vec![
        action_button(
            "packages/check",
            "Check & Update Now",
            PackagesBusy::Check,
            true,
        ),
        action_button(
            "packages/install-default",
            "Install ALab Toolset",
            PackagesBusy::Install,
            false,
        ),
    ];
    let action_height = if stack_actions {
        actions.len() as f32 * action_control_height + actions.len().saturating_sub(1) as f32 * 8.0
    } else {
        action_control_height
    };
    let actions_node = UiNode::new(
        "packages/actions",
        UiContent::Group(GroupSpec::new("Package actions")),
    )
    .layout(if stack_actions {
        Layout::column()
            .height(Length::Fixed(action_height))
            .gap(8.0)
    } else {
        Layout::row().height(Length::Fixed(action_height)).gap(8.0)
    })
    .children(actions);

    // Status hero: headline + detail from the shared projection (screen ==
    // introspection: the strings come from ONE derivation in packages_screen).
    let service_line = if packages.observed && packages.manager_enabled {
        let mut line = String::new();
        if !packages.index_source.is_empty() {
            line.push_str("Index ");
            line.push_str(&packages.index_source);
        }
        if !packages.root_fingerprint.is_empty() {
            if !line.is_empty() {
                line.push_str("  ·  ");
            }
            line.push_str("root ");
            line.push_str(&packages.root_fingerprint);
        }
        line
    } else {
        String::new()
    };
    let service_height = if service_line.is_empty() {
        0.0
    } else {
        22.0_f32.max(18.0 * text_scale)
    };
    let mut hero_children = vec![
        UiNode::new(
            "packages/title",
            UiContent::Text(TextSpec {
                text: "Toolchain Packages".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(title_height))),
        UiNode::new(
            "packages/headline",
            UiContent::Text(TextSpec {
                text: packages.headline.clone(),
                role: SemanticRole::Heading,
                style: StyleRef::Hero,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(headline_height))),
        UiNode::new(
            "packages/detail",
            UiContent::Text(TextSpec {
                text: packages.detail.clone().unwrap_or_else(|| {
                    "Signed, pinned toolchain builds managed by atpkg.".to_string()
                }),
                role: SemanticRole::Text,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(detail_height))),
    ];
    if !service_line.is_empty() {
        hero_children.push(
            UiNode::new(
                "packages/service-line",
                UiContent::Text(TextSpec {
                    text: service_line,
                    role: SemanticRole::Status,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(service_height))),
        );
    }
    hero_children.push(actions_node);
    let hero_height = title_height
        + headline_height
        + detail_height
        + service_height
        + action_height
        + 40.0
        + hero_children.len().saturating_sub(1) as f32 * 8.0;
    let hero = UiNode::new(
        "packages/hero",
        UiContent::Group(GroupSpec::new("Toolchain package status").style(StyleRef::Primary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(hero_height))
            .padding(Insets::symmetric(22.0, 20.0))
            .gap(8.0),
    )
    .children(hero_children);

    // Consent switches: registry-backed dotted keys through the ordinary
    // ConfigPatch/OCC path — flipping auto_install here IS the §11 consent.
    let switch_height = scaled_control_height();
    let consent_heading_height = 22.0_f32.max(16.0 * text_scale);
    let consent_note_height = 34.0_f32.max(30.0 * text_scale);
    let mut consent_children = vec![
        UiNode::new(
            "packages/consent-heading",
            UiContent::Text(TextSpec {
                text: "AUTOMATIC MAINTENANCE".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(consent_heading_height))),
    ];
    let mut switch_rows = 0usize;
    for key in [
        prefs::EDIT_PACKAGES_AUTO_UPDATE,
        prefs::EDIT_PACKAGES_AUTO_INSTALL,
    ] {
        if let Some(row) = packages_switch_row(state, key, switch_height) {
            consent_children.push(row);
            switch_rows += 1;
        }
    }
    consent_children.push(
        UiNode::new(
            "packages/consent-note",
            UiContent::Text(TextSpec {
                text: "Auto-install fetches missing default-set members (multi-GB). \
                       Both switches write [packages] in aterm.toml; auto-update \
                       takes effect at the next launch."
                    .to_string(),
                role: SemanticRole::Text,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(consent_note_height))),
    );
    let consent_height = 40.0
        + consent_heading_height
        + switch_rows as f32 * switch_height
        + consent_note_height
        + consent_children.len().saturating_sub(1) as f32 * 8.0;
    let consent = UiNode::new(
        "packages/consent",
        UiContent::Group(GroupSpec::new("Automatic maintenance").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(consent_height))
            .padding(Insets::all(20.0))
            .gap(8.0),
    )
    .children(consent_children);

    // Managed-program list (read-only). Bounded rows keep the card height
    // deterministic; the overflow line says exactly how much is elided.
    const MAX_PROGRAM_ROWS: usize = 8;
    let row_height = 24.0_f32.max(20.0 * text_scale);
    let programs_heading_height = 22.0_f32.max(16.0 * text_scale);
    let mut program_children = vec![
        UiNode::new(
            "packages/programs-heading",
            UiContent::Text(TextSpec {
                text: "MANAGED PROGRAMS".to_string(),
                role: SemanticRole::Heading,
                style: StyleRef::Quiet,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(programs_heading_height))),
    ];
    let mut program_lines: Vec<(String, String)> = Vec::new();
    if !packages.observed {
        program_lines.push((
            "packages/programs/loading".to_string(),
            "Reading package status…".to_string(),
        ));
    } else if packages.programs.is_empty() {
        program_lines.push((
            "packages/programs/empty".to_string(),
            "No managed programs installed.".to_string(),
        ));
    } else {
        for row in packages.programs.iter().take(MAX_PROGRAM_ROWS) {
            let mut line = row.name.clone();
            if let Some(build) = row.installed_build {
                line.push_str("  ·  build ");
                line.push_str(&build.to_string());
            }
            if !row.state.is_empty() {
                line.push_str("  ·  ");
                line.push_str(&row.state);
            }
            if let Some(annotation) = row.annotation.as_deref() {
                line.push_str("  ·  ");
                line.push_str(annotation);
            }
            program_lines.push((format!("packages/programs/{}", row.name), line));
        }
        if packages.programs.len() > MAX_PROGRAM_ROWS {
            program_lines.push((
                "packages/programs/more".to_string(),
                format!(
                    "… and {} more (atpkg list has the full set)",
                    packages.programs.len() - MAX_PROGRAM_ROWS
                ),
            ));
        }
    }
    let program_rows = program_lines.len();
    program_children.extend(program_lines.into_iter().map(|(key, line)| {
        UiNode::new(
            key,
            UiContent::Text(TextSpec {
                text: line,
                role: SemanticRole::Status,
                style: StyleRef::Primary,
            }),
        )
        .layout(Layout::default().height(Length::Fixed(row_height)))
    }));
    let programs_height = 40.0
        + programs_heading_height
        + program_rows as f32 * row_height
        + program_children.len().saturating_sub(1) as f32 * 8.0;
    let programs = UiNode::new(
        "packages/programs",
        UiContent::Group(GroupSpec::new("Managed programs").style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .height(Length::Fixed(programs_height))
            .padding(Insets::all(20.0))
            .gap(8.0)
            .clipped(),
    )
    .children(program_children);

    let sections = vec![hero, consent, programs];
    debug_assert_eq!(sections.len(), packages_sections(packages));
    let section_heights = hero_height + consent_height + programs_height;
    let authored_children = 2 + sections.len();
    let dashboard_height = page_heading_height()
        + page_subtitle_height()
        + section_heights
        + authored_children.saturating_sub(1) as f32 * 12.0;
    let content_height = if width == SettingsWidth::Compact {
        compact_special_section_height(state, viewport_height)
    } else {
        noncompact_page_content_height(
            state,
            width,
            viewport_width,
            viewport_height,
            if width == SettingsWidth::Wide {
                1_000.0
            } else {
                760.0
            },
        )
    };
    if width == SettingsWidth::Compact || dashboard_height > content_height || state.page_scroll > 0
    {
        // Window whole cards with the shared semantic pager, exactly like the
        // Software Update dashboard: every card stays complete and reachable
        // instead of clipping below the page edge.
        let mut sections = sections;
        let total = sections.len();
        let section = state.page_scroll.min(total.saturating_sub(1));
        let section_node = sections.swap_remove(section);
        let mut out = page_heading("Packages", packages_page_subtitle(width));
        out.extend([
            page_navigation_node(
                "packages/pagination",
                "packages/range",
                "Package sections",
                section,
                section + 1,
                total,
            ),
            section_node,
        ]);
        return out;
    }
    let mut out = page_heading("Packages", packages_page_subtitle(width));
    out.extend(sections);
    out
}

/// Stable global-search rank: exact prefix, then word prefix, then substring,
/// then ordered subsequence. Labels, canonical keys, and authored keywords all
/// participate so users can search either product language or config language.
fn field_match_score(field: &EditField, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let label = field.label.to_ascii_lowercase();
    let key = field.key.to_ascii_lowercase();
    let keywords = prefs::keywords_of(field.key)
        .iter()
        .map(|keyword| keyword.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let candidates = std::iter::once(label.as_str())
        .chain(std::iter::once(key.as_str()))
        .chain(keywords.iter().map(String::as_str));
    candidates
        .filter_map(|candidate| {
            if candidate.starts_with(query) {
                Some(0)
            } else if candidate
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|word| word.starts_with(query))
            {
                Some(1)
            } else if candidate.contains(query) {
                Some(2)
            } else if is_subsequence(query, candidate) {
                Some(3)
            } else {
                None
            }
        })
        .min()
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut needle = needle.chars();
    let mut wanted = needle.next();
    for character in haystack.chars() {
        if Some(character) == wanted {
            wanted = needle.next();
            if wanted.is_none() {
                return true;
            }
        }
    }
    wanted.is_none()
}

fn update_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Build status, changes, and relaunch."
    } else {
        "Know what is running, what is available, and exactly when aterm will relaunch."
    }
}

fn route_subtitle(route: SettingsRoute, width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        return match route {
            SettingsRoute::Appearance => "Theme, color, contrast, and selection.",
            SettingsRoute::TextFonts => "Fonts, shaping, fallback, and glyphs.",
            SettingsRoute::CursorMotion => "Cursor form, motion, and visual trails.",
            SettingsRoute::WindowTabs => "Authored Description wins; Activity fills in.",
            SettingsRoute::KeyboardInput => "Keyboard, paste safety, and local echo.",
            SettingsRoute::Terminal => "Shell, scrollback, protocols, and sessions.",
            SettingsRoute::Performance => "Rendering, resources, and diagnostics.",
            SettingsRoute::Security => "Permissions and containment.",
            SettingsRoute::Diagnostics => "Live renderer, event-loop, and UI facts.",
            SettingsRoute::Home
            | SettingsRoute::Modified
            | SettingsRoute::SoftwareUpdate
            | SettingsRoute::Packages
            | SettingsRoute::About => "",
        };
    }
    match route {
        SettingsRoute::Appearance => "Theme, color, contrast, and selection behavior.",
        SettingsRoute::TextFonts => "Typography, shaping, fallback, and glyph rendering.",
        SettingsRoute::CursorMotion => "Cursor form, motion policy, and visual trails.",
        SettingsRoute::WindowTabs => {
            "Window geometry, title and Description formatting, live Activity, and chrome."
        }
        SettingsRoute::KeyboardInput => "Keyboard, clipboard, paste safety, and local echo.",
        SettingsRoute::Terminal => "Shell, scrollback, terminal protocols, and session behavior.",
        SettingsRoute::Performance => "Renderer, resource use, and the diagnostic HUD.",
        SettingsRoute::Security => "Explicit permissions and containment policy.",
        SettingsRoute::Diagnostics => "Measured render, network, build, and protocol health.",
        SettingsRoute::Home
        | SettingsRoute::Modified
        | SettingsRoute::SoftwareUpdate
        | SettingsRoute::Packages
        | SettingsRoute::About => "",
    }
}

fn key_fragment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_app::{AppEffect, AppViewState, NativeApp, NativeRuntime, WorkOwner};
    use crate::native_config_service::{
        ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ConfigSnapshot, ExpectedValue,
        VersionedConfigService,
    };
    use crate::native_ui::LogicalRect;
    use crate::tab_model::ViewStore;
    use std::collections::BTreeSet;

    fn trail_pack_asset(name: &str) -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-effects/assets/trail-packs")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn trail_pack_config(paths: &[String], style: &str) -> String {
        let paths = paths
            .iter()
            .map(|path| toml::Value::String(path.clone()).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "cursor_blink = false\ncursor_trail = true\ncursor_trail_style = {style:?}\ncursor_trail_packs = [{paths}]\n"
        )
    }

    fn authored_visual_state(key: &str, value: Option<&str>) -> SettingsViewState {
        // The comet-family trail knobs are inert under the additive default (its
        // typed wake is deliberately cadence-scaled in its signature spectrum
        // — duration/length/colour do not apply). Preview them under the
        // COMMITTED comet style whose engine honours them, on BOTH base and
        // candidate, so each knob's own effect is what the pixels prove.
        let config = if matches!(
            key,
            prefs::EDIT_CURSOR_TRAIL_MS
                | prefs::EDIT_CURSOR_TRAIL_LENGTH
                | prefs::EDIT_CURSOR_TRAIL_RADIUS
                | prefs::EDIT_CURSOR_TRAIL_RING
                | prefs::EDIT_CURSOR_TRAIL_COLOR
                | prefs::EDIT_CURSOR_TRAIL_ACCENT
        ) {
            Config {
                cursor_trail_style: Some("comet".to_string()),
                ..Config::default()
            }
        } else {
            Config::default()
        };
        let mut state = SettingsViewState::new(&config);
        state.route = preview_route_for_key(key).expect("visual route");
        state.common.last_focus = Some(UiKey::new(format!("settings/control/{key}")));
        if let Some(value) = value {
            state.editing_field = Some(key.to_string());
            state.field_inputs.insert(
                key.to_string(),
                crate::native_text_input::TextInputState::new(value.to_string()),
            );
        }
        state
    }

    fn compile_preview_for_pixels(
        spec: SettingsPreviewSpec,
    ) -> (crate::native_ui::CompiledUi, Vec<u8>) {
        let viewport = LogicalRect::new(0.0, 0.0, 520.0, 210.0);
        let tree = UiTree::new(
            UiNode::new(
                "root",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(8.0)))
            .children(vec![crate::settings_preview::preview_node(
                "preview", spec, 190.0,
            )]),
        );
        let compiled = tree.compile(viewport).expect("preview compiles");
        let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
        let pixels = crate::tray_raster::rasterize_tray(
            &prims,
            viewport.width as u32,
            viewport.height as u32,
            1.0,
            [0, 0, 0, 0],
        )
        .0;
        (compiled, pixels)
    }

    fn preview_body_pixels(compiled: &crate::native_ui::CompiledUi, pixels: &[u8]) -> Vec<u8> {
        const WIDTH: usize = 520;
        let rect = compiled
            .semantic(&UiKey::new("preview"))
            .expect("preview semantics")
            .rect;
        let x0 = (rect.x + 10.0).ceil().max(0.0) as usize;
        let x1 = (rect.right() - 10.0).floor().max(x0 as f32) as usize;
        // Paint's terminal canvas begins at header_h(27) + 5. Exclude one
        // additional AA row so the visible candidate badge cannot satisfy a
        // body-effect assertion.
        let y0 = (rect.y + 33.0).ceil().max(0.0) as usize;
        let y1 = (rect.bottom() - 10.0).floor().max(y0 as f32) as usize;
        let mut body = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
        for row in y0..y1 {
            let start = (row * WIDTH + x0) * 4;
            let end = (row * WIDTH + x1) * 4;
            body.extend_from_slice(&pixels[start..end]);
        }
        body
    }

    #[test]
    fn all_57_visual_fields_preview_candidate_semantics_fingerprint_pixels_and_isolation() {
        const CASES: [(&str, &str); 57] = [
            (prefs::EDIT_THEME, "Nord"),
            (prefs::EDIT_FOREGROUND, "#00FF66"),
            (prefs::EDIT_BACKGROUND, "#101820"),
            (prefs::EDIT_CURSOR_COLOR, "#FF00FF"),
            (prefs::EDIT_SELECTION_COLOR, "#335577"),
            (prefs::EDIT_SELECTION_FOREGROUND, "#00FF66"),
            (prefs::EDIT_WINDOW_THEME, "light"),
            // 10, not 4.5: the default palette is deliberately lifted so every
            // ANSI token already reaches ~4.5:1 on the dark bg (see
            // `aterm_types::color_palette`) — a 4.5 floor is pixel-neutral by
            // design, while 10 provably floors the faint/red/blue specimens.
            (prefs::EDIT_MINIMUM_CONTRAST, "10"),
            (prefs::EDIT_SELECTION_INACTIVE, "true"),
            (prefs::EDIT_BOLD_IS_BRIGHT, "false"),
            (prefs::EDIT_FAINT_OPACITY, "0.8"),
            (prefs::EDIT_FONT_FAMILY, "JetBrains Mono"),
            (prefs::EDIT_FONT_PX, "20"),
            (prefs::EDIT_FONT_FAMILY_BOLD, "Fira Code"),
            (prefs::EDIT_FONT_FAMILY_ITALIC, "Iosevka"),
            (prefs::EDIT_FONT_FAMILY_BOLD_ITALIC, "Cascadia Code"),
            (prefs::EDIT_FONT_SYNTHETIC_STYLE, "false"),
            (
                prefs::EDIT_FALLBACK_FONTS,
                "DejaVu Sans Mono, Noto Sans CJK",
            ),
            (prefs::EDIT_SYMBOL_FONT, "Symbola"),
            (prefs::EDIT_EMOJI_FONT, "Apple Color Emoji"),
            (prefs::EDIT_LIGATURES, "false"),
            (prefs::EDIT_LINE_HEIGHT, "1.5"),
            (prefs::EDIT_ADJUST_BASELINE, "3"),
            (prefs::EDIT_ADJUST_UNDERLINE_POSITION, "2"),
            (prefs::EDIT_ADJUST_UNDERLINE_THICKNESS, "3"),
            (prefs::EDIT_UNDERLINE_SKIP_DESCENDERS, "false"),
            (prefs::EDIT_CURSOR_BREAK_LIGATURES, "true"),
            (prefs::EDIT_MERGED_LIGATURES, "true"),
            (prefs::EDIT_TEXT_BLENDING, "linear"),
            (prefs::EDIT_FONT_THICKEN, "true"),
            (prefs::EDIT_STEM_GAMMA, "0.7"),
            (prefs::EDIT_FONT_WEIGHT, "700"),
            (prefs::EDIT_FONT_VARIATION, "wdth=85, slnt=-8"),
            (prefs::EDIT_CURSOR_STYLE, "bar"),
            (prefs::EDIT_CURSOR_BLINK, "false"),
            (prefs::EDIT_CURSOR_TRAIL, "false"),
            (prefs::EDIT_CURSOR_TRAIL_STYLE, "water"),
            (prefs::EDIT_CURSOR_TRAIL_MS, "900"),
            (prefs::EDIT_CURSOR_TRAIL_LENGTH, "48"),
            (prefs::EDIT_CURSOR_TRAIL_INTENSITY, "0.25"),
            (prefs::EDIT_CURSOR_TRAIL_RADIUS, "1.2"),
            (prefs::EDIT_CURSOR_TRAIL_RING, "false"),
            (prefs::EDIT_CURSOR_TRAIL_COLOR, "#FF5A7D"),
            (prefs::EDIT_CURSOR_TRAIL_ACCENT, "#00E5FF"),
            (prefs::EDIT_CURSOR_NYAN_SPRITE, "candidate-cat.png"),
            (prefs::EDIT_CURSOR_TRAIL_BLOOM, "false"),
            (prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH, "1.7"),
            (prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS, "4.0"),
            (prefs::EDIT_CURSOR_FIRE_SHIMMER, "false"),
            (prefs::EDIT_HDR_GLOW, "false"),
            (prefs::EDIT_CURSOR_GLOW_SDR_BOOST, "0.8"),
            (prefs::EDIT_MOTION, "reduced"),
            (prefs::EDIT_LOAD_ADAPTIVE_MOTION, "false"),
            (prefs::EDIT_COLUMNS, "120"),
            (prefs::EDIT_LINES, "40"),
            (prefs::EDIT_TAB_STRIP_ROWS, "3"),
            (prefs::EDIT_SHOW_BUILD_BADGE, "true"),
        ];
        assert_eq!(
            CASES.iter().map(|(key, _)| *key).collect::<BTreeSet<_>>(),
            prefs::VISUAL_PREVIEW_KEYS.iter().copied().collect(),
        );

        let mut renderer = aterm_render::Renderer::from_bytes(
            include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf"),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("embedded preview renderer");
        renderer.set_runtime_font_discovery(false);
        renderer.prepare_semantic_typography("Bold Italic != => 你好 ∑✓♥ 😀 🚀 👩‍💻");
        // SETTLED install: `set_chrome_fonts` would park an async prewarm
        // worker whose landing between two paints of one state could swap the
        // semantic cascade mid-assertion (cross-test flake under a parallel
        // suite run). The fixture warms the cascade inline instead.
        crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);

        for (key, candidate_value) in CASES {
            let base_state = authored_visual_state(key, None);
            let candidate_state = authored_visual_state(key, Some(candidate_value));
            let raw_before = candidate_state.raw_values.clone();
            let assets_before = Arc::clone(candidate_state.config_assets());
            // The frozen animation phase must make each candidate OBSERVABLE
            // (530ms blink half-periods): 720 is an ODD half-period, hiding a
            // blinking cursor — exactly what cursor_blink needs (hidden base vs
            // lit non-blinking candidate) — while cursor colour/style and the
            // cursor-anchored ligature break need an EVEN phase so the blinking
            // cursor is LIT in both frames.
            let phase_ms = if matches!(
                key,
                prefs::EDIT_CURSOR_COLOR
                    | prefs::EDIT_CURSOR_STYLE
                    | prefs::EDIT_CURSOR_BREAK_LIGATURES
            ) {
                480
            } else {
                720
            };
            let base = renderer_preview_spec(
                &base_state,
                phase_ms,
                crate::native_app::ViewMotionCx::default(),
                13.0,
                aterm_render::Theme::default(),
            )
            .unwrap_or_else(|| panic!("base preview for {key}"));
            let candidate = renderer_preview_spec(
                &candidate_state,
                phase_ms,
                crate::native_app::ViewMotionCx::default(),
                13.0,
                aterm_render::Theme::default(),
            )
            .unwrap_or_else(|| panic!("candidate preview for {key}"));
            let base_font = crate::tray_raster::prepare_semantic_font(&base.font_candidate);
            let candidate_font = if candidate.font_candidate == base.font_candidate {
                base_font.clone()
            } else {
                crate::tray_raster::prepare_semantic_font(&candidate.font_candidate)
            };
            let base = base.with_prepared_font(base_font);
            let candidate = candidate.with_prepared_font(candidate_font);
            assert!(
                candidate
                    .semantic_value()
                    .contains(&format!("normalized-candidate {key}={candidate_value}")),
                "literal candidate semantics for {key}: {}",
                candidate.semantic_value(),
            );
            assert_ne!(
                base.paint_fingerprint(),
                candidate.paint_fingerprint(),
                "{key}"
            );

            let (base_ui, base_pixels) = compile_preview_for_pixels(base);
            let (candidate_ui, candidate_pixels) = compile_preview_for_pixels(candidate);
            let base_semantic = base_ui
                .semantic(&UiKey::new("preview"))
                .expect("base preview semantics");
            let candidate_semantic = candidate_ui
                .semantic(&UiKey::new("preview"))
                .expect("candidate preview semantics");
            assert_eq!(
                base_semantic.rect, candidate_semantic.rect,
                "{key} geometry"
            );
            assert_eq!(base_ui.hits, candidate_ui.hits, "{key} hit geometry");
            assert_ne!(
                base_pixels, candidate_pixels,
                "{key} must change visible pixels"
            );
            // Body-observability exemptions — candidates the frozen specimen
            // CORRECTLY cannot show, which stay covered by the badge pixels,
            // semantics, and fingerprint asserted above:
            // - merged_ligatures (`admit_collapsed`) admits only the Cascadia
            //   N:1 collapsed-ligature convention; the deterministic JetBrains
            //   fixture (like DejaVu) uses the 1:1 spacer convention.
            // - trail length/radius/ring/accent decorate JUMP wakes (length
            //   caps a jump's swept path; radius/ring/accent shape the landing
            //   halo), while the preview's bounded script deliberately types
            //   single-cell moves only. Making these four body-observable
            //   requires scripting a jump into the preview motion.
            let body_exempt = matches!(
                key,
                prefs::EDIT_MERGED_LIGATURES
                    | prefs::EDIT_CURSOR_TRAIL_LENGTH
                    | prefs::EDIT_CURSOR_TRAIL_RADIUS
                    | prefs::EDIT_CURSOR_TRAIL_RING
                    | prefs::EDIT_CURSOR_TRAIL_ACCENT
            );
            if !body_exempt {
                let base_body = preview_body_pixels(&base_ui, &base_pixels);
                let candidate_body = preview_body_pixels(&candidate_ui, &candidate_pixels);
                assert_ne!(
                    base_body, candidate_body,
                    "{key} must change the actual specimen body, not only its candidate badge",
                );
            }

            assert_eq!(
                candidate_state.raw_values, raw_before,
                "{key} config text isolation"
            );
            assert!(
                Arc::ptr_eq(candidate_state.config_assets(), &assets_before),
                "{key} asset identity"
            );
            assert!(
                candidate_state.pending.is_empty(),
                "{key} starts no config transaction"
            );
        }
    }

    #[test]
    fn controller_starts_at_the_canonical_config_snapshot_revision() {
        let update = UpdateState::from_status(1, "test", None, false);
        let app = SettingsApp::new_at_config_revision(update, 37);
        assert_eq!(app.config_revision, 37);
    }

    #[test]
    fn trail_pack_catalog_refreshes_for_new_snapshot_and_replacement() {
        let synthwave_path = trail_pack_asset("synthwave.toml");
        let emberfall_path = trail_pack_asset("emberfall.toml");
        let synthwave_text =
            trail_pack_config(std::slice::from_ref(&synthwave_path), "pack:synthwave");
        let synthwave_config: Config = toml::from_str(&synthwave_text).unwrap();

        let direct = SettingsViewState::new(&synthwave_config);
        assert!(
            direct.legacy.trail_pack_ids.is_empty(),
            "compatibility constructor stays manifest-IO-free"
        );

        let mut service = VersionedConfigService::new(synthwave_text).unwrap();
        let service_snapshot = service.snapshot();
        let synthwave_fp = service_snapshot
            .assets
            .trail_packs
            .get("synthwave")
            .expect("loaded synthwave")
            .pack_fp;
        assert_ne!(synthwave_fp, 0);

        let mut snapshot = SettingsViewState::from_snapshot(&service_snapshot).unwrap();
        assert_eq!(snapshot.legacy.trail_pack_ids, ["synthwave"]);
        assert_eq!(
            snapshot
                .trail_pack_catalog()
                .get("synthwave")
                .map(|pack| pack.pack_fp),
            Some(synthwave_fp)
        );

        let replacement = service
            .replace_external(trail_pack_config(
                std::slice::from_ref(&emberfall_path),
                "pack:emberfall",
            ))
            .unwrap();
        snapshot.replace_snapshot(&replacement).unwrap();
        assert_eq!(snapshot.legacy.trail_pack_ids, ["emberfall"]);
        assert!(
            !snapshot
                .trail_pack_catalog()
                .packs
                .contains_key("synthwave")
        );
        assert!(
            snapshot
                .trail_pack_catalog()
                .packs
                .contains_key("emberfall")
        );
    }

    // The fallback raw projection (`SettingsViewState::new` — the recovery/test
    // path) must treat dotted keys exactly like the snapshot projection, at
    // BOTH registered depths: a configured `[matrix_rain]`/`[packages]` bool
    // and a configured `[sparkle_words.*]` sub-table bool are EXPLICIT
    // (present in the Modified review; OCC expects `Exact(Some(..))`), an
    // absent one is not.
    #[test]
    fn fallback_raw_projection_covers_dotted_keys() {
        let config: Config = toml::from_str(
            "[matrix_rain]\nenabled = true\n[packages]\nauto_update = false\nauto_install = true\n\
             [sparkle_words.profanity]\nenabled = false\n[sparkle_words.feline]\nenabled = true\n\
             [sparkle_words.ink]\nloop = true\n",
        )
        .unwrap();
        let state = SettingsViewState::new(&config);
        for (key, want) in [
            (prefs::EDIT_MATRIX_RAIN_ENABLED, "true"),
            (prefs::EDIT_PACKAGES_AUTO_UPDATE, "false"),
            (prefs::EDIT_PACKAGES_AUTO_INSTALL, "true"),
            ("sparkle_words.profanity.enabled", "false"),
            ("sparkle_words.feline.enabled", "true"),
            ("sparkle_words.ink.loop", "true"),
        ] {
            assert!(state.is_explicit(key), "{key} configured ⇒ explicit");
            assert_eq!(state.raw_value(key).as_deref(), Some(want), "{key}");
        }
        let absent = SettingsViewState::new(&Config::default());
        for key in [
            prefs::EDIT_MATRIX_RAIN_ENABLED,
            prefs::EDIT_PACKAGES_AUTO_UPDATE,
            prefs::EDIT_PACKAGES_AUTO_INSTALL,
            "sparkle_words.profanity.enabled",
            "sparkle_words.feline.enabled",
            "sparkle_words.ink.loop",
        ] {
            assert!(!absent.is_explicit(key), "{key} absent ⇒ not explicit");
        }
    }

    // The production snapshot path for a DEPTH-2 leaf: a hand-set
    // `sparkle_words.profanity.enabled` is explicit with its exact raw value
    // (the full-depth dotted projection, not the whole-table opaque entry),
    // and an absent one is not.
    #[test]
    fn snapshot_projection_covers_depth_two_dotted_keys() {
        let service =
            VersionedConfigService::new("[sparkle_words.profanity]\nenabled = false\n".into())
                .unwrap();
        let state = SettingsViewState::from_snapshot(&service.snapshot()).unwrap();
        assert!(state.is_explicit("sparkle_words.profanity.enabled"));
        assert_eq!(
            state
                .raw_value("sparkle_words.profanity.enabled")
                .as_deref(),
            Some("false")
        );
        let absent = VersionedConfigService::new(String::new()).unwrap();
        let absent = SettingsViewState::from_snapshot(&absent.snapshot()).unwrap();
        assert!(!absent.is_explicit("sparkle_words.profanity.enabled"));
    }

    #[test]
    fn native_trail_picker_has_dynamic_choices_and_human_labels() {
        let config = Config {
            cursor_trail_style: Some("pack:synthwave".to_string()),
            ..Config::default()
        };
        let state = SettingsViewState::new(&config);
        let field = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_CURSOR_TRAIL_STYLE)
            .expect("trail style field");
        let ids = vec!["synthwave".to_string(), "ember_fall".to_string()];
        let choices = choices_for_field(field, &ids).expect("trail choices");
        assert_eq!(
            &choices[..prefs::CURSOR_TRAIL_STYLES.len()],
            prefs::CURSOR_TRAIL_STYLES
        );
        assert_eq!(
            &choices[prefs::CURSOR_TRAIL_STYLES.len()..],
            ["pack:ember_fall", "pack:synthwave"]
        );

        let picker = ChoicePicker::new(
            prefs::EDIT_CURSOR_TRAIL_STYLE,
            choices,
            "pack:synthwave",
            true,
            "phaser",
        );
        assert_eq!(
            picker.options[picker.selected].value.as_deref(),
            Some("pack:synthwave")
        );
        assert_eq!(
            picker.options[picker.selected].label,
            "Trail Pack · Synthwave"
        );
        assert!(picker.options.iter().any(|option| {
            option.label == "Trail Pack · Ember fall"
                && option.value.as_deref() == Some("pack:ember_fall")
        }));
        assert_eq!(
            picker
                .options
                .iter()
                .filter(|option| option.value.as_deref() == Some("pack:synthwave"))
                .count(),
            1,
            "the configured pack appears exactly once"
        );
    }

    #[test]
    fn highlighted_uncommitted_trail_pack_is_the_live_preview_candidate() {
        let synthwave_path = trail_pack_asset("synthwave.toml");
        let service = VersionedConfigService::new(trail_pack_config(
            std::slice::from_ref(&synthwave_path),
            "phaser",
        ))
        .unwrap();
        let mut state = SettingsViewState::from_snapshot(&service.snapshot()).unwrap();
        state.navigate(SettingsRoute::CursorMotion);
        let motion = crate::native_app::ViewMotionCx::default();
        let theme = aterm_render::Theme::default();
        let before =
            renderer_preview_spec(&state, 900, motion, 13.0, theme).expect("committed preview");

        let field = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_CURSOR_TRAIL_STYLE)
            .expect("trail style field");
        let mut picker = ChoicePicker::new(
            prefs::EDIT_CURSOR_TRAIL_STYLE,
            choices_for_field(field, &state.legacy.trail_pack_ids).expect("trail choices"),
            SettingsState::display_value(field),
            state.is_explicit(prefs::EDIT_CURSOR_TRAIL_STYLE),
            &effective_default_for_field(prefs::EDIT_CURSOR_TRAIL_STYLE),
        );
        let pack_index = picker
            .options
            .iter()
            .position(|option| option.value.as_deref() == Some("pack:synthwave"))
            .expect("pack option");
        picker.offset = (pack_index / ChoicePicker::PAGE_SIZE) * ChoicePicker::PAGE_SIZE;
        state.choice_picker = Some(picker);
        state.common.last_focus = Some(UiKey::new(format!(
            "settings/choice/{}/{pack_index}",
            prefs::EDIT_CURSOR_TRAIL_STYLE
        )));

        let candidate =
            renderer_preview_spec(&state, 900, motion, 13.0, theme).expect("candidate preview");
        assert_eq!(candidate.cursor.trail_style, PreviewTrailStyle::Custom);
        assert_eq!(
            candidate
                .cursor
                .trail_pack
                .as_ref()
                .map(|pack| pack.pack_fp),
            state
                .trail_pack_catalog()
                .get("synthwave")
                .map(|pack| pack.pack_fp)
        );
        assert_eq!(candidate.animation(), PreviewAnimation::Continuous);
        assert!(
            candidate
                .semantic_value()
                .contains("Trail Pack interpreter is live")
        );
        assert_ne!(before.paint_fingerprint(), candidate.paint_fingerprint());
        assert_eq!(
            state.raw_value(prefs::EDIT_CURSOR_TRAIL_STYLE).as_deref(),
            Some("phaser"),
            "moving picker focus previews without persisting"
        );
    }

    #[test]
    fn preview_activity_comes_from_the_exact_authored_spec() {
        let appearance = SettingsViewState::new(&Config::default());
        let motion = crate::native_app::ViewMotionCx::default();
        let theme = aterm_render::Theme::default();
        assert_eq!(
            appearance.preview_animation(100, motion, 13.0, theme),
            PreviewAnimation::None,
            "Appearance is a genuinely static renderer sample"
        );
        assert!(
            renderer_preview_spec(&appearance, 100, motion, 17.5, theme)
                .unwrap()
                .semantic_value()
                .contains("17.5 px"),
            "an unset font-size preview uses the renderer's applied size"
        );
        assert!(
            renderer_preview_spec(&appearance, 100, motion, 1.0, theme)
                .unwrap()
                .semantic_value()
                .contains("6 px"),
            "host font fallback is normalized to the prefs minimum"
        );

        let hidden: Config = toml::from_str(
            "cursor_style = \"hidden\"\ncursor_blink = true\ncursor_trail = false\n",
        )
        .unwrap();
        let mut hidden = SettingsViewState::new(&hidden);
        hidden.navigate(SettingsRoute::CursorMotion);
        assert_eq!(
            hidden.preview_animation(100, motion, 13.0, theme),
            PreviewAnimation::None,
            "a hidden blink with no trail cannot change pixels"
        );

        let blink: Config =
            toml::from_str("cursor_style = \"block\"\ncursor_blink = true\ncursor_trail = false\n")
                .unwrap();
        let mut blink = SettingsViewState::new(&blink);
        blink.navigate(SettingsRoute::CursorMotion);
        assert_eq!(
            blink.preview_animation(100, motion, 13.0, theme),
            PreviewAnimation::BlinkEdge { after_ms: 430 }
        );

        let zero: Config = toml::from_str(
            "cursor_blink = false\ncursor_trail = true\ncursor_trail_style = \"phaser\"\ncursor_trail_intensity = 0.0\n",
        )
        .unwrap();
        let mut zero = SettingsViewState::new(&zero);
        zero.navigate(SettingsRoute::CursorMotion);
        assert_eq!(
            zero.preview_animation(100, motion, 13.0, theme),
            PreviewAnimation::None,
            "a zero-intensity trail does not arm invisible work"
        );
    }

    #[test]
    fn canonical_non_cursor_previews_are_pixel_static_across_live_blink_edges() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let config: Config = toml::from_str(
            "cursor_style = \"bar\"\ncursor_blink = true\ncursor_trail = true\ncursor_trail_style = \"phaser\"\n",
        )
        .unwrap();
        let motion = crate::native_app::ViewMotionCx::default();
        let theme = aterm_render::Theme::default();

        for route in [
            SettingsRoute::Appearance,
            SettingsRoute::TextFonts,
            SettingsRoute::WindowTabs,
        ] {
            let mut state = SettingsViewState::new(&config);
            state.navigate(route);
            let early = renderer_preview_spec(&state, 100, motion, 13.0, theme)
                .unwrap_or_else(|| panic!("early {route:?} preview"));
            let late = renderer_preview_spec(&state, 600, motion, 13.0, theme)
                .unwrap_or_else(|| panic!("late {route:?} preview"));

            assert_eq!(early.animation(), PreviewAnimation::None, "{route:?}");
            assert_eq!(late.animation(), PreviewAnimation::None, "{route:?}");
            assert!(!early.cursor.blink, "{route:?} keeps its static cursor");
            assert!(
                !early.cursor.trail_enabled,
                "{route:?} keeps cursor motion out of its scene"
            );
            assert_eq!(
                early.paint_fingerprint(),
                late.paint_fingerprint(),
                "{route:?} retained identity is phase-static"
            );
            assert_eq!(
                compile_preview_for_pixels(early).1,
                compile_preview_for_pixels(late).1,
                "{route:?} pixels are phase-static"
            );
        }
    }

    #[test]
    fn serious_mode_suppresses_native_preview_trails_post_fx_and_cadence() {
        let config: Config = toml::from_str(
            "cursor_blink = true\n\
             cursor_trail = true\n\
             cursor_trail_style = \"fire\"\n\
             cursor_trail_bloom = true\n\
             cursor_fire_shimmer = true\n\
             hdr_glow = true\n\
             cursor_glow_sdr_boost = 0.5\n",
        )
        .unwrap();
        let mut state = SettingsViewState::new(&config);
        state.navigate(SettingsRoute::CursorMotion);
        state.common.last_focus = Some(UiKey::new(format!(
            "settings/control/{}",
            prefs::EDIT_CURSOR_FIRE_SHIMMER
        )));
        let theme = aterm_render::Theme::default();

        let active = renderer_preview_spec(
            &state,
            100,
            crate::native_app::ViewMotionCx::default(),
            13.0,
            theme,
        )
        .expect("active cursor preview");
        assert!(
            active.cursor.trail_enabled,
            "negative control: trail is authored"
        );
        assert!(active.post_fx.bloom, "negative control: bloom is authored");
        assert!(
            active.post_fx.fire_shimmer,
            "negative control: fire shimmer is authored"
        );

        let serious_motion = crate::native_app::ViewMotionCx {
            serious: true,
            ..crate::native_app::ViewMotionCx::default()
        };
        let serious = renderer_preview_spec(&state, 100, serious_motion, 13.0, theme)
            .expect("serious cursor preview remains available");
        assert!(serious.reduced_motion);
        assert!(!serious.cursor.trail_enabled);
        assert!(!serious.post_fx.bloom);
        assert!(!serious.post_fx.fire_shimmer);
        assert!(!serious.post_fx.hdr_glow);
        assert_eq!(serious.post_fx.sdr_boost, 0.0);
        assert_eq!(serious.animation(), PreviewAnimation::None);
        assert!(preview_reduced_motion(&state, serious_motion));
        assert_ne!(active.paint_fingerprint(), serious.paint_fingerprint());
    }

    #[test]
    fn highlighted_motion_candidate_resolves_against_live_host_facts_before_commit() {
        let config: Config = toml::from_str("motion = \"reduced\"\n").unwrap();
        let mut state = SettingsViewState::new(&config);
        state.navigate(SettingsRoute::CursorMotion);
        let field = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_MOTION)
            .expect("motion field");
        let picker = ChoicePicker::new(
            prefs::EDIT_MOTION,
            choices_for_field(field, &state.legacy.trail_pack_ids).expect("motion choices"),
            SettingsState::display_value(field),
            true,
            &effective_default_for_field(prefs::EDIT_MOTION),
        );
        state.choice_picker = Some(picker);
        let focus = |state: &mut SettingsViewState, value: &str| {
            let picker = state.choice_picker.as_mut().unwrap();
            let index = picker
                .options
                .iter()
                .position(|option| option.value.as_deref() == Some(value))
                .unwrap();
            picker.offset = (index / ChoicePicker::PAGE_SIZE) * ChoicePicker::PAGE_SIZE;
            state.common.last_focus = Some(UiKey::new(format!(
                "settings/choice/{}/{index}",
                prefs::EDIT_MOTION
            )));
        };

        let constrained = crate::native_app::ViewMotionCx {
            system_reduced: true,
            focused: true,
            performance_reduced: true,
            serious: false,
        };
        focus(&mut state, "full");
        assert!(
            !preview_reduced_motion(&state, constrained),
            "highlighted Full overrides OS and adaptive reduction"
        );
        focus(&mut state, "auto");
        assert!(preview_reduced_motion(&state, constrained));
        focus(&mut state, "reduced");
        assert!(preview_reduced_motion(
            &state,
            crate::native_app::ViewMotionCx::default()
        ));

        focus(&mut state, "auto");
        let load_only = crate::native_app::ViewMotionCx {
            system_reduced: false,
            focused: true,
            performance_reduced: true,
            serious: false,
        };
        assert!(preview_reduced_motion(&state, load_only));
        state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == prefs::EDIT_LOAD_ADAPTIVE_MOTION)
            .unwrap()
            .seed = Some("false".to_string());
        assert!(!preview_reduced_motion(&state, load_only));

        focus(&mut state, "full");
        assert!(preview_reduced_motion(
            &state,
            crate::native_app::ViewMotionCx {
                focused: false,
                ..crate::native_app::ViewMotionCx::default()
            }
        ));
        assert_eq!(
            state.raw_value(prefs::EDIT_MOTION).as_deref(),
            Some("reduced")
        );
    }

    fn setup() -> (
        NativeRuntime,
        crate::native_app::AppInstanceId,
        crate::native_app::ViewId,
    ) {
        setup_with_update(UpdateState::from_status(1, "0.1.0", None, false))
    }

    fn setup_with_update(
        update: UpdateState,
    ) -> (
        NativeRuntime,
        crate::native_app::AppInstanceId,
        crate::native_app::ViewId,
    ) {
        let app = SettingsApp::new(update);
        let mut runtime = NativeRuntime::new();
        let instance = runtime.insert_instance(NativeApp::Settings(app)).unwrap();
        let mut core_views = ViewStore::default();
        let view = core_views.insert_native(instance).unwrap();
        runtime
            .attach_view(
                view,
                instance,
                AppViewState::Settings(Box::new(SettingsViewState::new(&Config::default()))),
            )
            .unwrap();
        (runtime, instance, view)
    }

    fn update_status(staged: bool) -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            enabled: true,
            current_build: 1,
            staged_build: staged.then_some(2),
            staged_version: staged.then(|| "0.2.0".to_string()),
            staged_commit: staged.then(|| "deadbeef".to_string()),
            staged_dmg_sha256: staged.then(|| "0".repeat(64)),
            changelog: staged
                .then(|| "### Better\n- compact Settings\n- semantic previews".to_string()),
            outcome: "Update service healthy".to_string(),
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_since: String::new(),
            failing_persistent: false,
            rescues: 0,
        }
    }

    fn view_cx() -> ViewCx<'static> {
        view_cx_at(1_200.0, 820.0)
    }

    fn view_cx_at(width: f32, height: f32) -> ViewCx<'static> {
        // NativeRuntime view tests bypass App/backend construction. Opt into
        // the exact host-prepared UI faces at this direct-view fixture boundary
        // instead of changing the cold font-store constructor under cfg(test).
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        ViewCx {
            viewport: LogicalRect::new(0.0, 0.0, width, height),
            config_revision: 1,
            update_revision: 1,
            animation_phase_ms: 720,
            motion: crate::native_app::ViewMotionCx::default(),
            terminal_font_px: 12.0,
            terminal_theme: aterm_render::Theme::default(),
            semantic_font: None,
            document: None,
        }
    }

    #[test]
    fn smart_title_health_is_runtime_backed_and_transport_truthful() {
        let (mut runtime, instance, view) = setup();
        let health = TitleSummaryHealth {
            state: TitleSummaryRuntimeState::Error,
            provider: crate::app_config::TitleSummaryProvider::Ollama,
            model: Some("qwen3.5:4b-q4_K_M".to_string()),
            endpoint: Some("http://127.0.0.1:11434/api/chat".to_string()),
            locality: TitleSummaryLocality::UnattestedLoopback,
            managed_install_present: false,
            model_ready: false,
            last_error: Some("managed runtime is not installed".to_string()),
            next_retry_after: Some(std::time::Duration::from_secs(9)),
            next_refresh_after: None,
            timeout: Some(std::time::Duration::from_secs(42)),
            proxy_mode: Some(crate::app_config::TitleSummaryProxyMode::Direct),
            ca_file: None,
        };
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!();
            };
            state.navigate(SettingsRoute::WindowTabs);
            assert!(state.replace_title_summary_health(health.clone()));
            assert!(!state.replace_title_summary_health(health));
        }

        let cx = view_cx();
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let label = |key: &str| {
            compiled
                .semantic(&UiKey::new(format!("settings/smart-titles/health/{key}")))
                .unwrap_or_else(|| panic!("missing Smart Titles health row {key}"))
                .label
                .clone()
        };
        assert!(label("state").contains("Needs attention"));
        assert!(label("locality").contains("not proof of local-only"));
        assert!(label("transport").contains("42s timeout"));
        assert!(label("transport").contains("direct connection (forced)"));
        assert!(label("transport").contains("TLS/CA not used"));
        assert!(label("readiness").contains("no automatic download"));
        assert!(label("readiness").contains("latest request has not confirmed model readiness"));
        assert!(label("detail").contains("managed runtime is not installed"));
        assert!(label("detail").contains("Error retry"));
        assert!(label("precedence").contains("Authored Description wins"));

        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!();
            };
            state.replace_title_summary_health(TitleSummaryHealth {
                state: TitleSummaryRuntimeState::Ready,
                provider: crate::app_config::TitleSummaryProvider::OpenAiCompatible,
                model: Some("private-model".to_string()),
                endpoint: Some("https://127.0.0.1:9443/v1/chat/completions".to_string()),
                locality: TitleSummaryLocality::UnattestedLoopback,
                managed_install_present: false,
                model_ready: true,
                last_error: None,
                next_retry_after: None,
                next_refresh_after: Some(std::time::Duration::from_secs(30)),
                timeout: Some(std::time::Duration::from_secs(20)),
                proxy_mode: Some(crate::app_config::TitleSummaryProxyMode::Direct),
                ca_file: Some("/tmp/private-model-ca.pem".to_string()),
            });
        }
        let openai = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let openai_transport = openai
            .semantic(&UiKey::new("settings/smart-titles/health/transport"))
            .unwrap()
            .label
            .clone();
        assert!(openai_transport.contains("direct connection"));
        assert!(openai_transport.contains("platform roots replaced"));
        let openai_detail = openai
            .semantic(&UiKey::new("settings/smart-titles/health/detail"))
            .unwrap()
            .label
            .clone();
        assert!(openai_detail.contains("Routine activity refresh"));

        for (width, height) in [(320.0, 568.0), (568.0, 320.0)] {
            let compact_cx = view_cx_at(width, height);
            let compact = runtime
                .render(instance, view, &compact_cx)
                .unwrap()
                .compile(compact_cx.viewport)
                .unwrap();
            let keys = if height <= 420.0 {
                vec!["state", "summary"]
            } else {
                vec![
                    "state",
                    "locality",
                    "transport",
                    "readiness",
                    "detail",
                    "precedence",
                ]
            };
            for key in keys {
                let node = compact
                    .semantic(&UiKey::new(format!("settings/smart-titles/health/{key}")))
                    .unwrap_or_else(|| panic!("{width}×{height} loses health row {key}"));
                assert!(
                    node.rect.bottom() <= compact_cx.viewport.bottom() + 0.01,
                    "{width}×{height} clips health row {key}: {:?}",
                    node.rect
                );
            }
        }
    }

    #[test]
    fn smart_title_health_never_infers_locality_and_invalidates_on_authority_change() {
        let mut state = SettingsViewState::new(&Config::default());
        state.navigate(SettingsRoute::WindowTabs);
        state.common.last_focus = Some(UiKey::new(format!(
            "settings/control/{}",
            prefs::EDIT_TITLE_SUMMARY_PROVIDER
        )));
        let cx = view_cx();
        let unavailable = settings_tree(
            &state,
            &UpdateState::from_status(1, "0.1.0", None, false).projection(),
            &PackagesState::unobserved().projection(),
            &cx,
        )
        .compile(cx.viewport)
        .unwrap();
        assert!(
            unavailable
                .semantic(&UiKey::new("settings/smart-titles/health/locality"))
                .expect("unavailable locality status")
                .label
                .contains("locality is not inferred")
        );

        state.replace_title_summary_health(TitleSummaryHealth {
            state: TitleSummaryRuntimeState::Error,
            provider: crate::app_config::TitleSummaryProvider::OpenAiCompatible,
            model: Some("private-model".to_string()),
            endpoint: None,
            locality: TitleSummaryLocality::Remote,
            managed_install_present: false,
            model_ready: false,
            last_error: Some("OpenAI-compatible provider requires an endpoint".to_string()),
            next_retry_after: None,
            next_refresh_after: None,
            timeout: Some(std::time::Duration::from_secs(20)),
            proxy_mode: Some(crate::app_config::TitleSummaryProxyMode::Environment),
            ca_file: None,
        });
        let missing_endpoint = settings_tree(
            &state,
            &UpdateState::from_status(1, "0.1.0", None, false).projection(),
            &PackagesState::unobserved().projection(),
            &cx,
        )
        .compile(cx.viewport)
        .unwrap();
        assert!(
            missing_endpoint
                .semantic(&UiKey::new("settings/smart-titles/health/locality"))
                .expect("missing-endpoint locality status")
                .label
                .contains("no terminal context was sent")
        );
        assert!(
            missing_endpoint
                .semantic(&UiKey::new("settings/smart-titles/health/transport"))
                .expect("missing-endpoint transport status")
                .label
                .contains("environment proxy + NO_PROXY")
        );

        state.replace_title_summary_health(TitleSummaryHealth {
            state: TitleSummaryRuntimeState::Builtin,
            provider: crate::app_config::TitleSummaryProvider::Builtin,
            model: None,
            endpoint: None,
            locality: TitleSummaryLocality::NotApplicable,
            managed_install_present: false,
            model_ready: false,
            last_error: None,
            next_retry_after: None,
            next_refresh_after: None,
            timeout: None,
            proxy_mode: None,
            ca_file: None,
        });
        let service = VersionedConfigService::new(
            "title_summary_provider = \"openai-compatible\"\n\
             title_summary_endpoint = \"https://models.example.test/v1/chat/completions\"\n\
             title_summary_allow_remote = true\n"
                .to_string(),
        )
        .unwrap();
        state.replace_snapshot(&service.snapshot()).unwrap();
        assert!(
            state.title_summary_health.is_none(),
            "a provider-authority change must retire stale locality/readiness"
        );
    }

    #[test]
    fn maximum_text_scale_keeps_smart_title_health_and_a_control_whole() {
        const CHILD: &str = "ATERM_SMART_TITLE_HEALTH_SCALE_CHILD";
        const EXACT: &str = "native_settings::tests::maximum_text_scale_keeps_smart_title_health_and_a_control_whole";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", EXACT, "--nocapture"])
                .env(CHILD, "1")
                .env("RUST_TEST_THREADS", "1")
                .status()
                .expect("launch isolated Smart Titles Dynamic Type test");
            assert!(status.success());
            return;
        }

        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: 2.0,
                ..crate::native_appearance::current_preferences()
            },
        );
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.navigate(SettingsRoute::WindowTabs);
        state.replace_title_summary_health(TitleSummaryHealth {
            state: TitleSummaryRuntimeState::Builtin,
            provider: crate::app_config::TitleSummaryProvider::Builtin,
            model: None,
            endpoint: None,
            locality: TitleSummaryLocality::NotApplicable,
            managed_install_present: false,
            model_ready: true,
            last_error: None,
            next_retry_after: None,
            next_refresh_after: Some(std::time::Duration::from_secs(15)),
            timeout: None,
            proxy_mode: None,
            ca_file: None,
        });

        for (width, height) in [(568.0, 320.0), (320.0, 420.0), (320.0, 568.0)] {
            let cx = view_cx_at(width, height);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            compiled.validate_parity().unwrap();
            let health = compiled
                .semantic(&UiKey::new("settings/smart-titles/health"))
                .expect("Smart Titles health remains visible");
            let health_paint = compiled
                .paint
                .iter()
                .find(|paint| paint.key == health.key)
                .expect("Smart Titles health has authored paint geometry");
            assert_eq!(
                health_paint.rect, health.rect,
                "{width}×{height} partially clips Smart Titles health"
            );
            assert!(
                health.rect.bottom() <= height + 0.01,
                "{width}×{height} clips Smart Titles health: {:?}",
                health.rect
            );
            let controls = compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with("settings/control/"))
                .collect::<Vec<_>>();
            assert!(
                !controls.is_empty(),
                "{width}×{height} must retain a real Window & Tabs control"
            );
            for control in controls {
                let paint = compiled
                    .paint
                    .iter()
                    .find(|paint| paint.key == control.key)
                    .unwrap_or_else(|| panic!("{} has paint geometry", control.key.as_str()));
                let hit = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key == control.key)
                    .unwrap_or_else(|| panic!("{} has hit geometry", control.key.as_str()));
                assert_eq!(
                    paint.rect,
                    control.rect,
                    "{width}×{height} partially clips {}",
                    control.key.as_str()
                );
                assert_eq!(
                    hit.rect,
                    control.rect,
                    "{width}×{height} hit geometry diverges for {}",
                    control.key.as_str()
                );
                assert!(
                    control.rect.bottom() <= height + 0.01,
                    "{width}×{height} clips {}: {:?}",
                    control.key.as_str(),
                    control.rect
                );
            }
        }
    }

    fn apply_real_config_patch(
        service: &mut VersionedConfigService,
        patch: &ConfigPatch,
    ) -> (ConfigSnapshot, Option<u64>) {
        let request = ConfigPatchRequest {
            base_revision: patch.base_revision,
            edits: patch
                .edits
                .iter()
                .map(|edit| ConfigKeyEdit {
                    key: edit.key.clone(),
                    expected: match &edit.expected {
                        ExpectedConfigValue::Any => ExpectedValue::Any,
                        ExpectedConfigValue::Exact(value) => ExpectedValue::Exact(value.clone()),
                    },
                    value: edit.value.clone(),
                })
                .collect(),
        };
        match service.patch(request) {
            ConfigPatchResult::Applied { snapshot, undo } => (snapshot, Some(undo.get())),
            other => panic!("real config service rejected Settings patch: {other:?}"),
        }
    }

    #[test]
    fn settings_about_and_updates_are_routes_in_one_app_tree() {
        let (mut runtime, instance, view) = setup();
        for route in [
            SettingsRoute::Appearance,
            SettingsRoute::About,
            SettingsRoute::SoftwareUpdate,
        ] {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(route),
                        value: None,
                    }),
                )
                .unwrap();
            let tree = runtime.render(instance, view, &view_cx()).unwrap();
            let compiled = tree
                .compile(LogicalRect::new(0.0, 0.0, 1_200.0, 820.0))
                .unwrap();
            compiled.validate_parity().unwrap();
            assert!(
                compiled
                    .semantics
                    .iter()
                    .any(|node| node.key.as_str() == format!("settings/page{}", route.path()))
            );
        }
    }

    #[test]
    fn cmd_z_falls_back_to_last_config_patch_when_no_text_field_owns_undo() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state.last_undo = Some(41);
        state.common.last_focus = Some(UiKey::new("settings/page/appearance"));

        let outcome = runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Undo))
            .unwrap();
        assert!(
            outcome
                .effects
                .iter()
                .any(|effect| { matches!(effect, AppEffect::ConfigUndo { token: 41, .. }) })
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert!(state.last_undo.is_none(), "one-shot token is consumed");
        assert_eq!(state.feedback.as_deref(), Some("Undoing…"));
    }

    #[test]
    fn escape_clears_search_then_unwinds_detail_route_to_home() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::Text("font".to_string())),
                }),
            )
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert!(state.search.is_empty());
        assert!(state.search_input.value().is_empty());
        assert_eq!(state.route, SettingsRoute::Appearance);

        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.route, SettingsRoute::Home);
    }

    #[test]
    fn singleton_controller_supports_distinct_window_local_views() {
        let (mut runtime, instance, first) = setup();
        let mut core_views = ViewStore::default();
        // Burn the first core id to mirror the id already held by `first`; stable
        // view ids are globally allocated by App in production.
        let _ = core_views.insert_native(instance).unwrap();
        let second = core_views.insert_native(instance).unwrap();
        assert_ne!(first, second);
        runtime
            .attach_view(
                second,
                instance,
                AppViewState::Settings(Box::new(SettingsViewState::new(&Config::default()))),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                first,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(first_state)) = runtime.view_state(first) else {
            panic!("first Settings view");
        };
        let Some(AppViewState::Settings(second_state)) = runtime.view_state(second) else {
            panic!("second Settings view");
        };
        assert_eq!(first_state.route, SettingsRoute::About);
        assert_eq!(second_state.route, SettingsRoute::Appearance);
        assert_eq!(
            runtime.instance_by_kind(crate::native_app::AppKind::Settings),
            Some(instance)
        );
    }

    #[test]
    fn config_patch_is_service_owned_and_survives_view_close() {
        let (mut runtime, instance, view) = setup();
        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/set/copy_on_select"),
                    value: Some(SemanticInput::Bool(true)),
                }),
            )
            .unwrap();
        let reply = outcome
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { reply, .. } => Some(reply.clone()),
                _ => None,
            })
            .expect("config effect");
        assert!(matches!(reply.work_owner, WorkOwner::Service { .. }));
        runtime.remove_view(view).unwrap();
        assert!(runtime.completion_is_current(&reply));
    }

    #[test]
    fn pointer_activation_opens_choice_surface_then_selection_patches() {
        let (mut runtime, instance, view) = setup();
        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{}", crate::prefs::EDIT_CURSOR_STYLE)),
                    value: None,
                }),
            )
            .unwrap();
        assert!(
            !outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. })),
            "opening choices must not silently change configuration"
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(
            state
                .choice_picker
                .as_ref()
                .map(|picker| picker.key.as_str()),
            Some(crate::prefs::EDIT_CURSOR_STYLE)
        );
        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!(
                        "settings/choice/{}/1",
                        crate::prefs::EDIT_CURSOR_STYLE
                    )),
                    value: None,
                }),
            )
            .unwrap();
        let edit = outcome.effects.iter().find_map(|effect| match effect {
            AppEffect::ConfigPatch { patch, .. } => patch.edits.first(),
            _ => None,
        });
        let edit = edit.expect("choice activation emits a durable patch");
        assert_eq!(edit.key, crate::prefs::EDIT_CURSOR_STYLE);
        assert!(
            edit.value
                .as_deref()
                .is_some_and(|value| crate::prefs::CURSOR_STYLES.contains(&value))
        );
        let ExpectedConfigValue::Exact(expected) = &edit.expected else {
            panic!("choice edit carries the UI-observed value");
        };
        assert_ne!(&edit.value, expected);
    }

    #[test]
    fn choice_fields_use_a_native_chevron_without_polluting_the_label() {
        let state = SettingsViewState::new(&Config::default());
        let field = EditField {
            label: "Theme",
            key: prefs::EDIT_THEME,
            kind: EditKind::Theme,
            seed: Some("Nord".to_string()),
            placeholder: "Nord".to_string(),
        };
        let row = setting_row(&state, 0, &field, false, false, SettingsWidth::Wide);
        let UiContent::Button(control) = &row.children[1].content else {
            panic!("theme choice is a button control");
        };

        assert_eq!(control.spec.label, "Nord");
        assert_eq!(control.spec.trailing_icon, Some(ButtonIcon::ChevronDown));
        assert!(!control.spec.label.ends_with('v'));
    }

    #[test]
    fn explicit_choice_offers_the_true_default_and_opens_on_current_value() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        let theme = state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == prefs::EDIT_THEME)
            .expect("theme field");
        theme.seed = Some("Nord".to_string());
        theme.placeholder = "Nord".to_string();
        state
            .raw_values
            .insert(prefs::EDIT_THEME.to_string(), "Nord".to_string());

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{}", prefs::EDIT_THEME)),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        let picker = state.choice_picker.as_ref().expect("theme picker");
        assert_eq!(picker.options[0].label, "Use default  ·  Default");
        assert_eq!(picker_candidate(state, prefs::EDIT_THEME), Some("Nord"));
        let expected_focus = format!("settings/choice/{}/{}", prefs::EDIT_THEME, picker.selected);
        assert_eq!(
            state.common.last_focus.as_ref().map(UiKey::as_str),
            Some(expected_focus.as_str())
        );
    }

    #[test]
    fn paging_choices_focuses_a_visible_preview_candidate() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{}", prefs::EDIT_THEME)),
                    value: None,
                }),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/choice-page-next"),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        let picker = state.choice_picker.as_ref().expect("theme picker");
        assert_eq!(picker.offset, ChoicePicker::PAGE_SIZE);
        let expected_focus = format!("settings/choice/{}/{}", prefs::EDIT_THEME, picker.offset);
        assert_eq!(
            state.common.last_focus.as_ref().map(UiKey::as_str),
            Some(expected_focus.as_str())
        );
        assert!(picker_candidate(state, prefs::EDIT_THEME).is_some());
    }

    #[test]
    fn theme_focus_previews_before_commit_and_unset_font_uses_host_size() {
        let (mut runtime, instance, view) = setup();
        let host_theme = aterm_render::Theme::default();
        let before = {
            let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                panic!("Settings view");
            };
            renderer_preview_spec(
                state,
                720,
                crate::native_app::ViewMotionCx::default(),
                12.0,
                host_theme,
            )
            .expect("appearance preview")
        };

        let opened = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{}", prefs::EDIT_THEME)),
                    value: None,
                }),
            )
            .unwrap();
        assert!(
            !opened
                .effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. })),
            "preview navigation must not persist the candidate"
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        let picker = state.choice_picker.as_mut().expect("theme picker");
        let nord = picker
            .options
            .iter()
            .position(|option| option.value.as_deref() == Some("Nord"))
            .expect("Nord built-in");
        picker.offset = (nord / ChoicePicker::PAGE_SIZE) * ChoicePicker::PAGE_SIZE;
        state.common.last_focus = Some(UiKey::new(format!(
            "settings/choice/{}/{nord}",
            prefs::EDIT_THEME
        )));
        let candidate = renderer_preview_spec(
            state,
            720,
            crate::native_app::ViewMotionCx::default(),
            12.0,
            host_theme,
        )
        .expect("candidate preview");
        let nord = aterm_types::scheme::builtin("Nord").expect("Nord scheme");
        assert_eq!(
            candidate.terminal_theme,
            Some(PreviewTerminalTheme::from_scheme(&nord))
        );
        assert_ne!(before.paint_fingerprint(), candidate.paint_fingerprint());
        assert!(!state.is_explicit(prefs::EDIT_THEME));

        state.route = SettingsRoute::TextFonts;
        state.choice_picker = None;
        let host_sized = renderer_preview_spec(
            state,
            720,
            crate::native_app::ViewMotionCx::default(),
            8.0,
            host_theme,
        )
        .expect("typography preview");
        assert_eq!(host_sized.font_px, 8.0);
    }

    #[test]
    fn view_owned_external_open_goes_stale_when_view_closes() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("about/open-site"),
                    value: None,
                }),
            )
            .unwrap();
        let reply = outcome.effects.iter().find_map(|effect| match effect {
            AppEffect::OpenExternal { reply, .. } => Some(reply.clone()),
            _ => None,
        });
        if let Some(reply) = reply {
            assert!(matches!(reply.work_owner, WorkOwner::View { .. }));
            runtime.remove_view(view).unwrap();
            assert!(!runtime.completion_is_current(&reply));
        }
    }

    #[test]
    fn route_paths_round_trip_for_restore_and_control() {
        for route in SettingsRoute::ALL {
            assert_eq!(SettingsRoute::from_path(route.path()), Some(route));
        }
        assert_eq!(SettingsRoute::from_path("/unknown"), None);
    }

    #[test]
    fn compact_page_subtitles_fit_the_phone_content_measure() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let mut copies = SettingsRoute::ALL
            .into_iter()
            .map(|route| route_subtitle(route, SettingsWidth::Compact))
            .filter(|copy| !copy.is_empty())
            .collect::<Vec<_>>();
        copies.extend([
            "Every category, ranked by relevance.",
            "Overrides only; reset restores defaults.",
            update_page_subtitle(SettingsWidth::Compact),
            packages_page_subtitle(SettingsWidth::Compact),
        ]);
        for copy in copies {
            let measured = crate::tray_raster::ui_text_width(copy, 13.0);
            assert!(
                measured <= 254.5,
                "compact subtitle overflows the 286.5pt phone page: {copy:?} is {measured:.1}pt",
            );
        }
    }

    /// A live packages snapshot exactly as the host builds one: through the
    /// service reducer (begin → worker report → finish), never hand-assembled.
    fn live_packages_state(busy: Option<crate::packages_screen::PackagesBusy>) -> PackagesState {
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(1971),
                state: "active".to_string(),
                tree_root: String::new(),
            },
        );
        let status = atpkg::Status {
            schema: 1,
            updated_at: "2026-07-21T00:00:00Z".to_string(),
            enabled: true,
            index_source: "alabsystems/aterm".to_string(),
            outcome: "up to date".to_string(),
            programs,
        };
        let mut service = crate::packages_screen::PackagesService::new();
        let sequence = service.begin(None).unwrap();
        assert!(service.finish(
            sequence,
            crate::packages_screen::PackagesStatusReport::from_parts(
                true,
                true,
                "fp".to_string(),
                Some(&status),
                &[],
            ),
        ));
        if busy.is_some() {
            let _ = service.begin(busy).unwrap();
        }
        service.state(true, true, false)
    }

    /// The Packages route is a first-class destination: rail entry, special
    /// page with both action buttons, the consent switches, the program list,
    /// and the contextual command all derive from one route + one projection.
    #[test]
    fn packages_route_renders_status_actions_switches_and_programs() {
        let (mut runtime, instance, view) = setup();
        assert!(runtime.replace_settings_packages(live_packages_state(None), 2));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Packages),
                    value: None,
                }),
            )
            .unwrap();
        let cx = view_cx();
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        compiled.validate_parity().unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("settings/page/packages"))
                .is_some(),
            "the /packages route owns its own page"
        );
        assert!(
            compiled
                .semantic(&UiKey::new("settings/nav/packages"))
                .is_some(),
            "the rail exposes the Packages destination"
        );
        for key in ["packages/check", "packages/install-default"] {
            let control = compiled
                .semantic(&UiKey::new(key))
                .unwrap_or_else(|| panic!("{key} button renders"));
            assert!(
                control.state.is_none_or(|state| state.enabled),
                "{key} is enabled on a live manager"
            );
            assert!(
                compiled.hits.iter().any(|hit| hit.key.as_str() == key),
                "{key} is activatable"
            );
        }
        for key in [
            prefs::EDIT_PACKAGES_AUTO_UPDATE,
            prefs::EDIT_PACKAGES_AUTO_INSTALL,
        ] {
            assert!(
                compiled
                    .semantic(&UiKey::new(format!("settings/control/{key}")))
                    .is_some(),
                "the {key} consent switch renders on the special page"
            );
        }
        assert!(
            compiled
                .semantic(&UiKey::new("packages/programs/ay"))
                .is_some(),
            "installed programs render as read-only rows"
        );

        let commands = runtime.commands(instance, view).unwrap();
        assert!(
            commands.iter().any(|command| command.id.as_str()
                == format!("settings/route{}", SettingsRoute::Packages.path())),
            "the route loop exports Go to Packages"
        );
        let check = commands
            .iter()
            .find(|command| command.id.as_str() == "packages/check")
            .expect("contextual packages/check command on the Packages route");
        assert!(check.enabled);
    }

    /// The action reducer refuses honestly (unobserved / busy) without minting
    /// an effect, dispatches the typed request when live, and settles pending
    /// state through PackagesFinished — the full effect-quartet contract.
    #[test]
    fn packages_actions_gate_dispatch_and_finish() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Packages),
                    value: None,
                }),
            )
            .unwrap();
        let check = || {
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("packages/check"),
                value: None,
            })
        };

        // Unobserved (fresh controller): refuse without an effect.
        let refused = runtime.dispatch(instance, view, check()).unwrap();
        assert!(
            refused
                .effects
                .iter()
                .all(|effect| !matches!(effect, AppEffect::Packages { .. })),
            "no effect before the first worker observation"
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(
            state
                .feedback
                .as_deref()
                .unwrap()
                .contains("still being read")
        );

        // Live manager: the typed request goes to the host executor.
        assert!(runtime.replace_settings_packages(live_packages_state(None), 3));
        let accepted = runtime.dispatch(instance, view, check()).unwrap();
        let operation = accepted
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::Packages { request, reply } => {
                    assert_eq!(*request, crate::native_app::PackagesRequest::CheckUpdate);
                    Some(reply.operation)
                }
                _ => None,
            })
            .expect("a live manager dispatches the packages effect");
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(state.pending.len(), 1);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::PackagesFinished {
                    operation,
                    outcome: crate::native_app::PackagesOutcome::Accepted,
                },
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(state.pending.is_empty());
        assert!(state.feedback.as_deref().unwrap().contains("accepted"));

        // Busy: the second verb is refused without a second effect.
        assert!(runtime.replace_settings_packages(
            live_packages_state(Some(crate::packages_screen::PackagesBusy::Check)),
            4,
        ));
        let busy = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("packages/install-default"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(
            busy.effects
                .iter()
                .all(|effect| !matches!(effect, AppEffect::Packages { .. })),
            "a running verb blocks a second one"
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(
            state
                .feedback
                .as_deref()
                .unwrap()
                .contains("already running")
        );
    }

    /// The consent switches on the special page are ordinary registry rows:
    /// activation flips the RESOLVED value through the dotted-key ConfigPatch
    /// (auto_update defaults ON → first flip writes false; auto_install
    /// defaults OFF → first flip writes true), with per-key OCC expectations.
    #[test]
    fn packages_consent_switches_patch_the_dotted_keys() {
        let (mut runtime, instance, view) = setup();
        assert!(runtime.replace_settings_packages(live_packages_state(None), 2));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Packages),
                    value: None,
                }),
            )
            .unwrap();
        for (key, expected_write) in [
            (prefs::EDIT_PACKAGES_AUTO_UPDATE, "false"),
            (prefs::EDIT_PACKAGES_AUTO_INSTALL, "true"),
        ] {
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(format!("settings/set/{key}")),
                        value: None,
                    }),
                )
                .unwrap();
            let edit = outcome
                .effects
                .iter()
                .find_map(|effect| match effect {
                    AppEffect::ConfigPatch { patch, .. } => patch.edits.first().cloned(),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} switch emits a config patch"));
            assert_eq!(edit.key, key);
            assert_eq!(edit.value.as_deref(), Some(expected_write));
            assert_eq!(
                edit.expected,
                ExpectedConfigValue::Exact(None),
                "an absent [packages] child key carries an exact-absent OCC expectation"
            );
        }
    }

    /// Packages revision fan-out mirrors the updater: the shared controller
    /// snapshot advances and every view bumps its presentation revision.
    #[test]
    fn packages_changed_revision_repaints_every_view() {
        let (mut runtime, instance, view) = setup();
        let before = match runtime.view_state(view) {
            Some(AppViewState::Settings(state)) => state.common.presentation_revision,
            _ => unreachable!(),
        };
        runtime
            .dispatch(instance, view, AppEvent::PackagesChanged { revision: 7 })
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(state.common.presentation_revision > before);
    }

    #[test]
    fn responsive_boundaries_keep_every_actionable_node_whole() {
        assert_eq!(SettingsWidth::for_viewport(759.99), SettingsWidth::Compact);
        assert_eq!(SettingsWidth::for_viewport(760.0), SettingsWidth::Medium);
        assert_eq!(SettingsWidth::for_viewport(1_039.99), SettingsWidth::Medium);
        assert_eq!(SettingsWidth::for_viewport(1_040.0), SettingsWidth::Wide);
        assert_eq!(
            SettingsWidth::for_viewport_at_scale(1_200.0, 2.0),
            SettingsWidth::Compact,
            "2× content responds to effective measure instead of clipping a desktop rail"
        );
        assert_eq!(SettingsWidth::Compact.row_height_at_scale(2.0), 108.0);
        assert_eq!(SettingsWidth::Medium.row_height_at_scale(2.0), 80.0);
        assert_eq!(scaled_control_height_at_scale(2.0), 64.0);
        assert_eq!(compact_toolbar_height_at_scale(2.0), 80.0);

        let (runtime, instance, view) = setup();
        for width in [360.0, 759.0, 760.0, 1_039.0, 1_040.0, 1_280.0] {
            let cx = view_cx_at(width, 820.0);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            compiled.validate_parity().unwrap();
            for hit in &compiled.hits {
                let painted = compiled
                    .paint
                    .iter()
                    .find(|node| node.key == hit.key)
                    .expect("action is painted");
                let delta = (painted.rect.x - hit.rect.x).abs()
                    + (painted.rect.y - hit.rect.y).abs()
                    + (painted.rect.width - hit.rect.width).abs()
                    + (painted.rect.height - hit.rect.height).abs();
                assert!(
                    delta < 0.01,
                    "{:?} clips at width {width}: paint={:?}, hit={:?}",
                    hit.key,
                    painted.rect,
                    hit.rect
                );
                assert!(hit.rect.width > 0.0 && hit.rect.height > 0.0);
            }
        }
    }

    #[test]
    fn visual_preview_matrix_covers_four_viewports_at_three_text_scales() {
        const CHILD: &str = "ATERM_SETTINGS_PREVIEW_MATRIX_CHILD";
        const SCALE: &str = "ATERM_SETTINGS_PREVIEW_MATRIX_SCALE";
        const EXACT: &str = "native_settings::tests::visual_preview_matrix_covers_four_viewports_at_three_text_scales";
        if std::env::var_os(CHILD).is_none() {
            for scale in ["1.0", "1.5", "2.0"] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", EXACT, "--nocapture"])
                    .env(CHILD, "1")
                    .env(SCALE, scale)
                    .env("RUST_TEST_THREADS", "1")
                    .status()
                    .expect("launch isolated Settings viewport/scale matrix");
                assert!(status.success(), "Settings matrix failed at {scale}×");
            }
            return;
        }

        let scale = std::env::var(SCALE)
            .expect("child scale")
            .parse::<f32>()
            .expect("numeric child scale");
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
        let mut renderer = aterm_render::Renderer::from_bytes(
            include_bytes!("../../aterm-render/tests/fixtures/jetbrains-mono.ttf"),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("matrix semantic renderer");
        renderer.set_runtime_font_discovery(false);
        renderer.prepare_semantic_typography("Bold Italic != => 你好 ∑✓♥ 😀 🚀 👩‍💻");
        // SETTLED install: `set_chrome_fonts` would park an async prewarm
        // worker whose landing between two paints of one state could swap the
        // semantic cascade mid-assertion (cross-test flake under a parallel
        // suite run). The fixture warms the cascade inline instead.
        crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);

        let compile = |runtime: &NativeRuntime,
                       instance: crate::native_app::AppInstanceId,
                       view: crate::native_app::ViewId,
                       width: f32,
                       height: f32| {
            let mut cx = view_cx_at(width, height);
            let state = match runtime.view_state(view) {
                Some(AppViewState::Settings(state)) => state,
                _ => panic!("Settings view state"),
            };
            cx.semantic_font = state
                .preview_font_candidate(
                    cx.animation_phase_ms,
                    cx.motion,
                    cx.terminal_font_px,
                    cx.terminal_theme,
                )
                .map(|candidate| crate::tray_raster::prepare_semantic_font(&candidate));
            runtime
                .render(instance, view, &cx)
                .expect("Settings renders")
                .compile(cx.viewport)
                .expect("Settings compiles")
        };
        let assert_whole =
            |compiled: &crate::native_ui::CompiledUi, width: f32, height: f32, context: &str| {
                compiled
                    .validate_parity()
                    .unwrap_or_else(|error| panic!("{context}: {error:?}"));
                for hit in &compiled.hits {
                    let paint = compiled
                        .paint
                        .iter()
                        .find(|paint| paint.key == hit.key)
                        .unwrap_or_else(|| panic!("{context}: {:?} has no paint", hit.key));
                    let parity_delta = (paint.rect.x - hit.rect.x).abs()
                        + (paint.rect.y - hit.rect.y).abs()
                        + (paint.rect.width - hit.rect.width).abs()
                        + (paint.rect.height - hit.rect.height).abs();
                    assert!(
                        parity_delta < 0.001,
                        "{context}: {:?} parity differs by {parity_delta}: paint={:?}, hit={:?}",
                        hit.key,
                        paint.rect,
                        hit.rect,
                    );
                    assert!(
                        hit.rect.x >= -0.01
                            && hit.rect.y >= -0.01
                            && hit.rect.right() <= width + 0.01
                            && hit.rect.bottom() <= height + 0.01,
                        "{context}: {:?} is clipped: {:?}",
                        hit.key,
                        hit.rect,
                    );
                }
            };

        let viewports = [
            (320.0, 568.0),
            (390.0, 844.0),
            (568.0, 320.0),
            (1_280.0, 800.0),
        ];
        let routes = [
            SettingsRoute::Appearance,
            SettingsRoute::TextFonts,
            SettingsRoute::CursorMotion,
            SettingsRoute::WindowTabs,
        ];
        for (width, height) in viewports {
            for route in routes {
                let (mut runtime, instance, view) = setup();
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.navigate(route);
                let compiled = compile(&runtime, instance, view, width, height);
                let context = format!("{} {width}×{height} at {scale}×", route.label());
                assert_whole(&compiled, width, height, &context);
                let preview_key = UiKey::new(format!("settings/preview{}", route.path()));
                let preview = compiled
                    .semantic(&preview_key)
                    .unwrap_or_else(|| panic!("{context}: live preview is visible"));
                let expected_height =
                    renderer_preview_height(SettingsWidth::for_viewport_at_scale(width, scale));
                assert!(
                    (preview.rect.height - expected_height).abs() < 0.01,
                    "{context}: preview height is {:?}, expected {expected_height}",
                    preview.rect,
                );

                if height <= 420.0 {
                    let navigation = compiled
                        .semantic(&UiKey::new("settings/results-window"))
                        .expect("short landscape discloses the next virtual slice");
                    assert!(navigation.rect.bottom() <= height + 0.01, "{context}");
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!()
                    };
                    state.page_scroll = 1;
                    let controls = compile(&runtime, instance, view, width, height);
                    let scrolled_context = format!("{context}, settings slice");
                    assert_whole(&controls, width, height, &scrolled_context);
                    assert!(
                        controls.semantic(&preview_key).is_none(),
                        "{scrolled_context}: preview yielded its bounded slice"
                    );
                    assert!(
                        controls
                            .semantics
                            .iter()
                            .any(|node| node.key.as_str().starts_with("settings/control/")),
                        "{scrolled_context}: the first real control is reachable"
                    );

                    if (scale - 2.0).abs() < f32::EPSILON {
                        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view)
                        else {
                            unreachable!()
                        };
                        let section = route.section().expect("preview route has a section");
                        let mut fields = state
                            .legacy
                            .fields
                            .iter()
                            .enumerate()
                            .filter(|(_, field)| prefs::section_of(field.key) == section)
                            .collect::<Vec<_>>();
                        fields.sort_by_key(|(index, field)| (prefs::group_of(field.key).1, *index));
                        let last_key = fields.last().expect("preview route has fields").1.key;
                        state.page_scroll = fields.len();
                        let final_window = compile(&runtime, instance, view, width, height);
                        let final_context = format!("{context}, final settings slice");
                        assert_whole(&final_window, width, height, &final_context);
                        assert!(
                            final_window
                                .semantic(&UiKey::new(format!("settings/control/{last_key}")))
                                .is_some(),
                            "{final_context}: last field {last_key} is reachable"
                        );
                    }
                }
            }

            let (mut runtime, instance, view) = setup();
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            state.set_search("font".to_string());
            state.common.last_focus = Some(UiKey::new("settings/search"));
            let searched = compile(&runtime, instance, view, width, height);
            let context = format!("font search {width}×{height} at {scale}×");
            assert_whole(&searched, width, height, &context);
            assert!(
                searched
                    .semantic(&UiKey::new("settings/preview/search"))
                    .is_some(),
                "{context}: global search retains the matching contextual preview"
            );
            assert!(
                searched.semantic(&UiKey::new("settings/search")).is_some(),
                "{context}: the full search surface remains present"
            );
        }
    }

    #[test]
    fn maximum_native_text_scale_keeps_a_phone_page_reachable_and_unclipped() {
        const CHILD: &str = "ATERM_SETTINGS_SCALE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_settings::tests::maximum_native_text_scale_keeps_a_phone_page_reachable_and_unclipped",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .expect("launch isolated Dynamic Type test process");
            assert!(status.success());
            return;
        }

        let previous = crate::native_appearance::current_preferences();
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: 2.0,
                ..previous
            },
        );
        let (mut runtime, instance, view) = setup();
        let cx = view_cx_at(320.0, 568.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        compiled.validate_parity().unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("settings/navigation"))
                .is_none()
        );
        let preview = compiled
            .semantic(&UiKey::new("settings/preview/appearance"))
            .expect("the live demonstration remains present at maximum text scale");
        assert_eq!(
            preview.rect.height,
            renderer_preview_height(SettingsWidth::Compact)
        );
        assert!(preview.rect.bottom() <= cx.viewport.bottom() + 0.01);
        let heading = compiled
            .semantic(&UiKey::new("settings/page-heading/appearance"))
            .unwrap();
        assert!(heading.rect.height >= 68.0);
        let controls = compiled
            .semantics
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/control/"))
            .collect::<Vec<_>>();
        assert!(!controls.is_empty());
        for control in controls {
            assert!(
                control.rect.height >= 64.0,
                "{} {:?}",
                control.key.as_str(),
                control.rect
            );
            assert!(control.rect.bottom() <= cx.viewport.bottom() + 0.01);
        }
        let next = compiled
            .semantic(&UiKey::new("settings/results-window/next"))
            .expect("omitted 2× controls remain reachable");
        assert!(next.state.unwrap().enabled);
        assert!(next.rect.bottom() <= cx.viewport.bottom() + 0.01);

        for route in [
            SettingsRoute::Home,
            SettingsRoute::About,
            SettingsRoute::Diagnostics,
            SettingsRoute::SoftwareUpdate,
        ] {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(route),
                        value: None,
                    }),
                )
                .unwrap();
            for _ in 0..SettingsRoute::ALL.len() {
                let page = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                page.validate_parity().unwrap();
                for hit in &page.hits {
                    assert!(
                        hit.rect.height >= 64.0,
                        "{} shrank below its 2× control allocation on {route:?}: {:?}",
                        hit.key.as_str(),
                        hit.rect
                    );
                    assert!(hit.rect.bottom() <= cx.viewport.bottom() + 0.01);
                }
                let Some(next) = page
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str().ends_with("/pagination/next"))
                    .map(|hit| hit.action.clone())
                else {
                    break;
                };
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next,
                            value: None,
                        }),
                    )
                    .unwrap();
            }
        }
        crate::native_appearance::install_preferences(previous);
    }

    #[test]
    fn appearance_uses_a_bounded_semantic_renderer_preview_at_every_width() {
        let (runtime, instance, view) = setup();
        let cx = view_cx_at(1_280.0, 820.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        compiled.validate_parity().unwrap();

        let preview = compiled
            .semantic(&UiKey::new("settings/preview/appearance"))
            .expect("Appearance has a first-class renderer preview node");
        assert!(preview.rect.bottom() <= cx.viewport.bottom());
        let SemanticValue::Text(value) = &preview.value else {
            panic!("preview exposes truthful semantic context");
        };
        assert!(value.contains("renderer preview"));
        assert!(value.contains("portable SDR tone-map: bloom"));
        assert!(value.contains("fire shimmer"));
        assert!(value.contains("no panel-headroom claim"));
        assert!(value.contains("SDR boost"));

        let medium = view_cx_at(900.0, 820.0);
        let medium = runtime
            .render(instance, view, &medium)
            .unwrap()
            .compile(medium.viewport)
            .unwrap();
        assert!(
            medium
                .semantic(&UiKey::new("settings/preview/appearance"))
                .is_some(),
            "responsive widths retain the same semantic demonstration"
        );
    }

    #[test]
    fn window_preview_consumes_the_real_smart_title_controls() {
        let config = Config {
            descriptive_titles: Some(false),
            tab_title_format: Some(crate::app_config::TitleFormat::DescriptionTitle),
            window_title_format: Some(crate::app_config::TitleFormat::Description),
            ..Config::default()
        };
        let mut state = SettingsViewState::new(&config);
        state.navigate(SettingsRoute::WindowTabs);
        state.common.last_focus = Some(UiKey::new(format!(
            "settings/control/{}",
            prefs::EDIT_TAB_TITLE_FORMAT
        )));
        let spec = renderer_preview_spec(
            &state,
            0,
            crate::native_app::ViewMotionCx::default(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("Window & Tabs has a renderer preview");
        assert_eq!(spec.scene, PreviewScene::WindowTabs);
        assert!(!spec.window_tabs.generate_activity);
        assert_eq!(spec.window_tabs.tab_title_format, "description-title");
        assert_eq!(spec.window_tabs.window_title_format, "description");
        assert_eq!(
            spec.animation(),
            crate::settings_preview::PreviewAnimation::None
        );
        let semantics = spec.semantic_value();
        assert!(semantics.contains("Activity fallback `running tests` is off"));
        assert!(semantics.contains("tab format description-title"));
        assert!(semantics.contains("window format description"));
        assert!(semantics.contains("no live provider-health claim"));
    }

    #[test]
    fn window_preview_provider_off_disables_activity_semantics_and_pixels() {
        let preview = |provider| {
            let config = Config {
                descriptive_titles: Some(true),
                title_summary_provider: Some(provider),
                ..Config::default()
            };
            let mut state = SettingsViewState::new(&config);
            state.navigate(SettingsRoute::WindowTabs);
            renderer_preview_spec(
                &state,
                0,
                crate::native_app::ViewMotionCx::default(),
                14.0,
                aterm_render::Theme::default(),
            )
            .expect("Window & Tabs has a renderer preview")
        };

        let enabled = preview(crate::app_config::TitleSummaryProvider::Builtin);
        let disabled = preview(crate::app_config::TitleSummaryProvider::Off);
        assert!(enabled.window_tabs.generate_activity);
        assert!(!disabled.window_tabs.generate_activity);
        assert!(
            disabled
                .semantic_value()
                .contains("Activity fallback `running tests` is off")
        );
        assert_ne!(
            compile_preview_for_pixels(enabled).1,
            compile_preview_for_pixels(disabled).1,
            "provider=off must visibly remove the generated Activity fallback"
        );
    }

    #[test]
    fn wide_appearance_stacks_preview_before_complete_controls() {
        let (runtime, instance, view) = setup();
        let cx = view_cx_at(1_424.0, 658.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        compiled.validate_parity().unwrap();

        let preview = compiled
            .semantic(&UiKey::new("settings/preview/appearance"))
            .unwrap();
        let range = compiled
            .semantic(&UiKey::new("settings/results-range"))
            .unwrap();
        assert!(preview.rect.bottom() < range.rect.y);
        let visible_controls = compiled
            .semantics
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/control/"))
            .count();
        assert!(
            visible_controls >= 3,
            "the top preview must leave a useful complete form window: {visible_controls}"
        );
    }

    #[test]
    fn about_and_updates_use_top_aligned_responsive_dashboards() {
        let (mut runtime, instance, view) = setup();
        let wide = view_cx_at(1_424.0, 658.0);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let about = runtime
            .render(instance, view, &wide)
            .unwrap()
            .compile(wide.viewport)
            .unwrap();
        let hero = about.semantic(&UiKey::new("about/hero")).unwrap();
        let details = about.semantic(&UiKey::new("about/details")).unwrap();
        assert!(hero.rect.y <= 40.0, "About starts at the page rhythm");
        assert!(details.rect.y > hero.rect.bottom());
        assert!(details.rect.bottom() <= wide.viewport.bottom());
        assert!(
            about.semantic(&UiKey::new("about/principles")).is_none(),
            "the short viewport keeps every shown card whole"
        );

        let tall = view_cx_at(1_424.0, 900.0);
        let tall_about = runtime
            .render(instance, view, &tall)
            .unwrap()
            .compile(tall.viewport)
            .unwrap();
        assert!(
            tall_about
                .semantic(&UiKey::new("about/principles"))
                .is_some(),
            "large windows use the extra vertical measure intentionally"
        );

        let compact_about_cx = view_cx_at(474.0, 658.0);
        let compact_hero = runtime
            .render(instance, view, &compact_about_cx)
            .unwrap()
            .compile(compact_about_cx.viewport)
            .unwrap();
        assert!(compact_hero.semantic(&UiKey::new("about/hero")).is_some());
        assert!(
            compact_hero
                .semantic(&UiKey::new("about/provenance"))
                .is_none()
        );
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(1))
            .unwrap();
        let compact_build = runtime
            .render(instance, view, &compact_about_cx)
            .unwrap()
            .compile(compact_about_cx.viewport)
            .unwrap();
        let build = compact_build
            .semantic(&UiKey::new("about/provenance"))
            .unwrap();
        assert!(build.rect.bottom() <= compact_about_cx.viewport.bottom());
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(1))
            .unwrap();
        let compact_support = runtime
            .render(instance, view, &compact_about_cx)
            .unwrap()
            .compile(compact_about_cx.viewport)
            .unwrap();
        let support = compact_support
            .semantic(&UiKey::new("about/support"))
            .unwrap();
        assert!(support.rect.bottom() <= compact_about_cx.viewport.bottom());

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::SoftwareUpdate),
                    value: None,
                }),
            )
            .unwrap();
        let updates = runtime
            .render(instance, view, &wide)
            .unwrap()
            .compile(wide.viewport)
            .unwrap();
        let status = updates.semantic(&UiKey::new("updates/hero")).unwrap();
        let service = updates
            .semantic(&UiKey::new("updates/detail-card"))
            .unwrap();
        assert!(status.rect.right() < service.rect.x);
        assert_eq!(status.rect.y, service.rect.y);
        assert_eq!(status.rect.bottom(), service.rect.bottom());
        assert!(updates.semantic(&UiKey::new("updates/process")).is_some());

        let compact = view_cx_at(600.0, 820.0);
        let compact_updates = runtime
            .render(instance, view, &compact)
            .unwrap()
            .compile(compact.viewport)
            .unwrap();
        let compact_status = compact_updates
            .semantic(&UiKey::new("updates/hero"))
            .unwrap();
        assert!(compact_status.rect.bottom() <= compact.viewport.bottom());
        assert!(
            compact_updates
                .semantic(&UiKey::new("updates/detail-card"))
                .is_none()
        );
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(1))
            .unwrap();
        let compact_details = runtime
            .render(instance, view, &compact)
            .unwrap()
            .compile(compact.viewport)
            .unwrap();
        let compact_service = compact_details
            .semantic(&UiKey::new("updates/detail-card"))
            .unwrap();
        assert!(compact_service.rect.bottom() <= compact.viewport.bottom());
        assert!(
            compact_details
                .semantic(&UiKey::new("updates/process"))
                .is_none()
        );
    }

    #[test]
    fn ordinary_medium_home_pages_only_whole_workspace_cards() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Home),
                    value: None,
                }),
            )
            .unwrap();
        // Exact logical native-content viewport measured from the ordinary
        // 1698x1024 Retina window after unified titlebar chrome.
        // 513pt is the ordinary medium content height at which the complete
        // 14-route labeled rail fits. At 460pt the responsive contract now
        // correctly switches to the compact one-pane surface.
        let cx = view_cx_at(849.0, 513.0);
        let expected = [
            SettingsRoute::TextFonts,
            SettingsRoute::CursorMotion,
            SettingsRoute::WindowTabs,
            SettingsRoute::KeyboardInput,
            SettingsRoute::Terminal,
            SettingsRoute::Performance,
            SettingsRoute::Security,
            SettingsRoute::Diagnostics,
            SettingsRoute::About,
        ];
        let mut seen = BTreeSet::new();
        for _ in 0..expected.len() {
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            compiled.validate_parity().unwrap();
            let audit = compiled.paint_audit_lines();
            let overflow = audit
                .iter()
                .filter(|line| line.contains("overflow=true"))
                .cloned()
                .collect::<Vec<_>>();
            let navigation_geometry = audit
                .iter()
                .filter(|line| line.starts_with("paint-node key=\"settings/nav/"))
                .cloned()
                .collect::<Vec<_>>();
            assert!(
                overflow.is_empty(),
                "ordinary Medium Home must paint complete chrome:\n{}\n{}",
                overflow.join("\n"),
                navigation_geometry.join("\n")
            );
            assert!(
                compiled
                    .semantic(&UiKey::new("settings/home/pagination"))
                    .is_some(),
                "short Medium Home is a bounded semantic route window"
            );
            for route in expected {
                let key = UiKey::new(format!("settings/home{}", route.path()));
                let Some(node) = compiled.semantic(&key) else {
                    continue;
                };
                assert!(node.rect.height >= 48.0);
                assert!(node.rect.bottom() <= cx.viewport.bottom() + 0.01);
                let hit = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key == key)
                    .expect("every visible workspace card is actionable");
                assert_eq!(hit.rect, node.rect);
                seen.insert(route.path());
            }
            let Some(next) = compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == "settings/home/pagination/next")
                .map(|hit| hit.action.clone())
            else {
                break;
            };
            assert_eq!(next.as_str(), "settings/page-down");
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: next,
                        value: None,
                    }),
                )
                .unwrap();
        }
        assert_eq!(seen.len(), expected.len());
    }

    #[test]
    fn ordinary_medium_about_pages_complete_build_and_support_metadata() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!();
        };
        state.feedback = Some("Copied build information".to_string());
        // Exercise the genuine medium workbench. Shorter heights that cannot
        // fit all 14 labeled routes intentionally select compact navigation.
        let cx = view_cx_at(849.0, 513.0);
        let hero = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        hero.validate_parity().unwrap();
        assert!(hero.semantic(&UiKey::new("about/hero")).is_some());
        assert!(hero.semantic(&UiKey::new("about/details")).is_none());
        for key in ["about/copy-build-info", "about/open-site"] {
            let node = hero.semantic(&UiKey::new(key)).unwrap();
            assert!(node.rect.bottom() <= cx.viewport.bottom() + 0.01);
        }
        let next = hero
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == "about/pagination/next")
            .expect("About exports a visible semantic Next action");
        assert_eq!(next.action.as_str(), "settings/page-down");
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: next.action.clone(),
                    value: None,
                }),
            )
            .unwrap();

        let details = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        details.validate_parity().unwrap();
        let details_card = details.semantic(&UiKey::new("about/details")).unwrap();
        assert!(details_card.rect.bottom() <= cx.viewport.bottom() + 0.01);
        let provenance = crate::build_info::about_fields()
            .into_iter()
            .filter(|(key, _)| !matches!(*key, "tagline" | "author" | "company" | "site"));
        for (label, _) in provenance {
            let key = format!("about/provenance/row/{}", key_fragment(label));
            let node = details
                .semantic(&UiKey::new(key.clone()))
                .unwrap_or_else(|| panic!("missing complete Medium metadata {key}"));
            assert!(node.rect.height >= 28.0);
            assert!(node.rect.bottom() <= cx.viewport.bottom() + 0.01);
        }
        for label in ["Project", "Interface", "Capture", "Accessibility"] {
            let key = format!("about/support/row/{}", key_fragment(label));
            let node = details
                .semantic(&UiKey::new(key.clone()))
                .unwrap_or_else(|| panic!("missing complete Medium support metadata {key}"));
            assert!(node.rect.height >= 28.0);
            assert!(node.rect.bottom() <= cx.viewport.bottom() + 0.01);
        }
        let previous = details
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == "about/pagination/previous")
            .expect("second About section exports semantic Previous");
        assert_eq!(previous.action.as_str(), "settings/page-up");
    }

    #[test]
    fn ordinary_medium_staged_update_pages_workbench_notes_and_outcome() {
        let status = update_status(true);
        let (mut runtime, instance, view) =
            setup_with_update(UpdateState::from_status(1, "0.1.0", Some(&status), false));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::SoftwareUpdate),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!();
        };
        state.feedback = Some("Update will install when safe".to_string());
        // Exercise the genuine medium workbench. Shorter heights that cannot
        // fit all 14 labeled routes intentionally select compact navigation.
        let cx = view_cx_at(849.0, 513.0);
        let mut seen = BTreeSet::new();
        for section in 0..4 {
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            compiled.validate_parity().unwrap();
            assert!(
                compiled
                    .semantic(&UiKey::new("settings/page-heading/software-update"))
                    .is_some(),
                "every paged update section retains its route identity"
            );
            for node in &compiled.semantics {
                if node.key.as_str().starts_with("updates/") {
                    assert!(
                        node.rect.bottom() <= cx.viewport.bottom() + 0.01,
                        "{} clips at 849x460: {:?}",
                        node.key.as_str(),
                        node.rect
                    );
                    seen.insert(node.key.as_str().to_string());
                }
            }
            for (key, minimum_height) in [
                ("updates/workbench", 220.0),
                ("updates/actions", 124.0),
                ("updates/release-notes-card", 210.0),
                ("updates/outcome", 28.0),
            ] {
                if let Some(node) = compiled.semantic(&UiKey::new(key)) {
                    assert!(
                        node.rect.height >= minimum_height,
                        "{key} is vertically clipped: {:?}",
                        node.rect
                    );
                }
            }
            if section == 0 {
                let next = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str() == "updates/pagination/next")
                    .expect("staged update exports semantic Next");
                assert_eq!(next.action.as_str(), "settings/page-down");
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next.action.clone(),
                            value: None,
                        }),
                    )
                    .unwrap();
            } else if section < 3 {
                // Wheel/keyboard and the visible Next control reduce to the
                // same bounded page_scroll state.
                runtime
                    .dispatch(instance, view, AppEvent::ScrollLines(1))
                    .unwrap();
            }
        }
        for key in [
            "updates/check",
            "updates/install-relaunch",
            "updates/install-when-safe",
            "updates/headline",
            "updates/detail-card",
            "updates/release-notes",
            "updates/outcome",
        ] {
            assert!(seen.contains(key), "missing paged Medium update node {key}");
        }
    }

    #[test]
    fn about_metadata_values_fit_responsive_rects_and_keep_full_semantics() {
        let ui_sample = "compiler-終-with-a-long-provenance-suffix";
        let ui_elided = elide_metadata_value(ui_sample, 112.0, false);
        assert!(ui_elided.ends_with('…'));
        assert!(
            metadata_value_text_width(
                &ui_elided,
                13.0 * crate::native_appearance::text_scale(),
                false,
            ) <= 112.0
        );
        let code_sample = "0123456789abcdef0123456789abcdef";
        let code_elided = elide_metadata_value(code_sample, 96.0, true);
        assert!(code_elided.ends_with('…'));
        assert!(
            metadata_value_text_width(
                &code_elided,
                13.0 * crate::native_appearance::text_scale(),
                true,
            ) <= 96.0
        );

        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let provenance = crate::build_info::about_fields()
            .into_iter()
            .filter(|(key, _)| !matches!(*key, "tagline" | "author" | "company" | "site"))
            .collect::<Vec<_>>();
        let px = 13.0 * crate::native_appearance::text_scale();

        for viewport_width in [474.0, 760.0, 1_040.0, 1_160.0] {
            let width = SettingsWidth::for_viewport(viewport_width);
            if width == SettingsWidth::Compact {
                runtime
                    .dispatch(instance, view, AppEvent::ScrollLines(1))
                    .unwrap();
            }
            let cx = view_cx_at(viewport_width, 658.0);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let expected_value_width = about_metadata_value_width(viewport_width, width);
            let visible_rows = match width {
                SettingsWidth::Compact | SettingsWidth::Wide => provenance.len(),
                SettingsWidth::Medium => 4,
            };

            for (label, full_value) in provenance.iter().take(visible_rows) {
                let fragment = key_fragment(label);
                let row = compiled
                    .semantic(&UiKey::new(format!("about/provenance/row/{fragment}")))
                    .unwrap();
                assert_eq!(row.label, format!("{label}: {full_value}"));

                let visual = compiled
                    .semantic(&UiKey::new(format!("about/provenance/value/{fragment}")))
                    .unwrap();
                assert!(
                    (visual.rect.width - expected_value_width).abs() <= 0.01,
                    "{viewport_width}px {label} rect {} != derived budget {expected_value_width}",
                    visual.rect.width,
                );
                let measured =
                    metadata_value_text_width(&visual.label, px, metadata_value_is_code(label));
                assert!(
                    measured <= visual.rect.width - ABOUT_METADATA_TEXT_SAFETY + 0.01,
                    "{viewport_width}px {label} paints {measured}px into {}px",
                    visual.rect.width,
                );
            }
        }
    }

    #[test]
    fn bounded_numeric_setting_is_a_real_slider_and_keyboard_steps_exactly() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state.route = SettingsRoute::TextFonts;
        let field = state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == prefs::EDIT_FONT_PX)
            .expect("font size field");
        field.seed = Some("14".to_string());
        field.placeholder = "14 px".to_string();
        state.common.last_focus = Some(UiKey::new(format!(
            "settings/control/{}",
            prefs::EDIT_FONT_PX
        )));

        let cx = view_cx_at(900.0, 820.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_FONT_PX));
        assert_eq!(
            compiled.semantic(&key).unwrap().value,
            SemanticValue::Number {
                value: 14.0,
                minimum: 6.0,
                maximum: 32.0,
            }
        );
        let slider = compiled
            .paint
            .iter()
            .find_map(|node| {
                (node.key == key)
                    .then_some(&node.content)
                    .and_then(|content| {
                        let UiContent::Slider(control) = content else {
                            return None;
                        };
                        Some(control)
                    })
            })
            .expect("numeric field paints as slider");
        assert_eq!(slider.spec.step, 1.0);
        assert_eq!(slider.spec.display_value, "14");

        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Right { extend: false }),
            )
            .unwrap();
        let edit = outcome.effects.iter().find_map(|effect| match effect {
            AppEffect::ConfigPatch { patch, .. } => patch.edits.first(),
            _ => None,
        });
        assert_eq!(edit.and_then(|edit| edit.value.as_deref()), Some("15"));

        let range = prefs::range_of(prefs::EDIT_FONT_PX).unwrap();
        assert_eq!(normalize_slider_value(13.49, range).as_deref(), Some("13"));
        assert_eq!(normalize_slider_value(99.0, range).as_deref(), Some("32"));
        assert_eq!(normalize_slider_value(f64::NAN, range), None);
    }

    #[test]
    fn color_override_has_a_truthful_swatch_and_source_context() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        let field = state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == prefs::EDIT_FOREGROUND)
            .expect("foreground field");
        field.seed = Some("#123456".to_string());
        field.placeholder = "#123456".to_string();
        state
            .raw_values
            .insert(prefs::EDIT_FOREGROUND.to_string(), "#123456".to_string());
        state.search_input = crate::native_text_input::TextInputState::new("foreground".into());
        state.set_search("foreground".to_string());

        let cx = view_cx_at(900.0, 820.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_FOREGROUND));
        let field = compiled
            .paint
            .iter()
            .find_map(|node| {
                (node.key == key)
                    .then_some(&node.content)
                    .and_then(|content| {
                        let UiContent::TextField(control) = content else {
                            return None;
                        };
                        Some(control)
                    })
            })
            .expect("color text field");
        assert_eq!(field.spec.swatch, Some([0x12, 0x34, 0x56]));
        let label = compiled
            .semantic(&UiKey::new(format!(
                "settings/label/{}",
                prefs::EDIT_FOREGROUND
            )))
            .unwrap();
        assert!(label.label.contains("Override"), "{}", label.label);
    }

    #[test]
    fn pointer_text_positions_reduce_to_grapheme_safe_caret_and_selection() {
        let (mut runtime, instance, view) = setup();
        let text = "Aé👩‍💻末";
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state.search_input = crate::native_text_input::TextInputState::new(text.to_string());
        state.set_search(text.to_string());

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::TextPosition {
                        byte: 3,
                        extend: false,
                    }),
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.selection().range(), 3..3);

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::TextPosition {
                        byte: 14,
                        extend: true,
                    }),
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.selection().anchor, 3);
        assert_eq!(state.search_input.selection().head, 14);
        assert_eq!(state.search_input.selected_text(), "👩‍💻");

        // A hostile/non-boundary byte is still normalized by reducer-owned
        // TextInputState and can never split the multibyte cluster.
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::TextPosition {
                        byte: 8,
                        extend: false,
                    }),
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(state.search_input.selection().range(), 3..3);
    }

    #[test]
    fn readline_events_edit_the_search_field_at_the_caret() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state.search_input = crate::native_text_input::TextInputState::new("cursor trail".into());
        state.set_search("cursor trail".to_string());
        state.common.last_focus = Some(UiKey::new("settings/search"));

        let text_input = |runtime: &mut NativeRuntime, event| {
            runtime
                .dispatch(instance, view, AppEvent::TextInput(event))
                .unwrap();
        };

        // Ctrl-A: the caret moves to the start — the value is untouched.
        text_input(&mut runtime, TextInputEvent::Home { extend: false });
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), "cursor trail");
        assert_eq!(state.search_input.selection().range(), 0..0);

        // Ctrl-K: kill to the end — the query empties and the filter follows.
        text_input(&mut runtime, TextInputEvent::KillToEnd);
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), "");
        assert_eq!(state.search, "", "the live filter tracks the killed query");

        // Retype, then Ctrl-W: the trailing word dies, the filter follows.
        text_input(&mut runtime, TextInputEvent::Commit("cursor trail".into()));
        text_input(&mut runtime, TextInputEvent::DeleteWordBackward);
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), "cursor ");
        assert_eq!(state.search, "cursor ");

        // Ctrl-A then Ctrl-D: forward-delete eats the FIRST grapheme, proving
        // the caret model really sits at the start (not append-at-end).
        text_input(&mut runtime, TextInputEvent::Home { extend: false });
        text_input(&mut runtime, TextInputEvent::Delete);
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), "ursor ");
        assert_eq!(state.search, "ursor ");

        // Ctrl-E then Ctrl-U: kill back to the start empties the field again.
        text_input(&mut runtime, TextInputEvent::End { extend: false });
        assert_eq!(
            match runtime.view_state(view) {
                Some(AppViewState::Settings(state)) => state.search_input.selection().range(),
                _ => panic!("Settings view"),
            },
            6..6
        );
        text_input(&mut runtime, TextInputEvent::KillToStart);
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), "");
        assert_eq!(state.search, "");
    }

    #[test]
    fn pointer_text_position_enters_real_field_edit_and_invalid_kinds_fail_closed() {
        let (mut runtime, instance, view) = setup();
        let text_key = prefs::EDIT_FONT_FAMILY;
        let slider_key = prefs::EDIT_FONT_PX;
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state.route = SettingsRoute::TextFonts;
        state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == text_key)
            .unwrap()
            .seed = Some("écho 👩‍💻 mono".to_string());
        state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == slider_key)
            .unwrap()
            .seed = Some("14".to_string());

        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{text_key}")),
                    value: Some(SemanticInput::TextPosition {
                        byte: 6,
                        extend: false,
                    }),
                }),
            )
            .unwrap();
        assert!(
            !outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. })),
            "caret movement never writes config"
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.editing_field.as_deref(), Some(text_key));
        assert_eq!(
            state.field_inputs[text_key].selection().range(),
            6..6,
            "the field opens at the pointer boundary, not its old end caret"
        );

        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{slider_key}")),
                    value: Some(SemanticInput::TextPosition {
                        byte: 1,
                        extend: false,
                    }),
                }),
            )
            .unwrap();
        assert!(
            !outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. }))
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(
            !state.field_inputs.contains_key(slider_key),
            "a typed slider cannot be coerced into text editing"
        );

        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/set/no-longer-live"),
                    value: Some(SemanticInput::TextPosition {
                        byte: 0,
                        extend: false,
                    }),
                }),
            )
            .unwrap();
        assert!(
            !outcome
                .effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. }))
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(!state.field_inputs.contains_key("no-longer-live"));
    }

    #[test]
    fn pointer_selection_maps_ime_projection_back_to_committed_text() {
        let mut input = crate::native_text_input::TextInputState::new("ab末cd".to_string());
        input.set_selection(2, 5);
        input.set_preedit("👩‍💻".to_string(), None);
        let projection = input.projection();
        assert_eq!(&projection.text, "ab👩‍💻cd");

        apply_pointer_selection(&mut input, 2 + "👩‍💻".len(), false);
        assert_eq!(input.selection().range(), 5..5);
        assert!(input.preedit().is_none());
        apply_pointer_selection(&mut input, 1, true);
        assert_eq!(input.selection().anchor, 5);
        assert_eq!(input.selection().head, 1);
    }

    #[test]
    fn compact_is_one_pane_and_medium_keeps_labeled_navigation() {
        let (mut runtime, instance, view) = setup();
        let compact = view_cx_at(600.0, 820.0);
        let detail = runtime
            .render(instance, view, &compact)
            .unwrap()
            .compile(compact.viewport)
            .unwrap();
        assert!(
            detail
                .semantic(&UiKey::new("settings/navigation"))
                .is_none()
        );
        assert!(
            detail
                .semantic(&UiKey::new("settings/compact-back"))
                .is_some()
        );

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Home),
                    value: None,
                }),
            )
            .unwrap();
        let home = runtime
            .render(instance, view, &compact)
            .unwrap()
            .compile(compact.viewport)
            .unwrap();
        assert!(
            home.semantic(&UiKey::new("settings/compact-back"))
                .is_none()
        );
        assert!(home.semantic(&UiKey::new("settings/home/about")).is_some());

        let medium = view_cx_at(800.0, 820.0);
        let rail = runtime
            .render(instance, view, &medium)
            .unwrap()
            .compile(medium.viewport)
            .unwrap();
        assert_eq!(
            rail.semantic(&UiKey::new("settings/navigation"))
                .unwrap()
                .rect
                .width,
            196.0
        );
        for route in SettingsRoute::ALL {
            let key = UiKey::new(format!("settings/nav{}", route.path()));
            let button = rail
                .paint
                .iter()
                .find_map(|node| {
                    (node.key == key)
                        .then_some(&node.content)
                        .and_then(|content| {
                            let UiContent::Button(control) = content else {
                                return None;
                            };
                            Some(control)
                        })
                })
                .unwrap_or_else(|| panic!("{} labeled navigation button", route.label()));
            assert_eq!(button.spec.visual_icon, Some(route_icon(route)));
            assert!(button.spec.visual_label.is_none());
            assert_eq!(button.spec.label, route.label());
        }

        let wide = view_cx_at(1_200.0, 820.0);
        let wide_rail = runtime
            .render(instance, view, &wide)
            .unwrap()
            .compile(wide.viewport)
            .unwrap();
        for route in SettingsRoute::ALL {
            let key = UiKey::new(format!("settings/nav{}", route.path()));
            let button = wide_rail
                .paint
                .iter()
                .find_map(|node| {
                    (node.key == key)
                        .then_some(&node.content)
                        .and_then(|content| {
                            let UiContent::Button(control) = content else {
                                return None;
                            };
                            Some(control)
                        })
                })
                .unwrap();
            assert_eq!(button.spec.visual_icon, Some(route_icon(route)));
            assert_eq!(button.spec.label, route.label());
        }
    }

    #[test]
    fn short_wide_sidebar_keeps_every_route_whole_and_selected_route_visible() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let cx = view_cx_at(1_200.0, 513.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let navigation = compiled
            .semantic(&UiKey::new("settings/navigation"))
            .expect("wide settings sidebar");
        for route in SettingsRoute::ALL {
            let key = UiKey::new(format!("settings/nav{}", route.path()));
            let route_node = compiled
                .semantic(&key)
                .unwrap_or_else(|| panic!("{} remains visible", route.label()));
            assert!(
                route_node.rect.height >= 22.0,
                "{} is clipped to {:?}",
                route.label(),
                route_node.rect
            );
            assert!(route_node.rect.bottom() <= navigation.rect.bottom() + 0.01);
            let hit = compiled
                .hits
                .iter()
                .find(|hit| hit.key == key)
                .expect("visible route has a hit target");
            assert_eq!(hit.rect, route_node.rect);
        }
    }

    #[test]
    fn mutation_feedback_never_obscures_or_reflows_primary_navigation() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!();
        };
        state.feedback = Some("Applied · Undo available".to_string());

        let cx = view_cx_at(1_200.0, 513.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let navigation = compiled
            .semantic(&UiKey::new("settings/navigation"))
            .expect("persistent sidebar");
        let feedback = compiled
            .semantic(&UiKey::new("settings/feedback"))
            .expect("mutation feedback");
        assert!(feedback.rect.x >= navigation.rect.right());
        assert_eq!(navigation.rect.height, cx.viewport.height);
        for route in SettingsRoute::ALL {
            let key = UiKey::new(format!("settings/nav{}", route.path()));
            let route_node = compiled
                .semantic(&key)
                .unwrap_or_else(|| panic!("{} remains reachable under feedback", route.label()));
            assert!(route_node.rect.bottom() <= navigation.rect.bottom() + 0.01);
        }
    }

    #[test]
    fn compact_preview_and_form_share_the_available_space_without_clipping() {
        let (runtime, instance, view) = setup();
        let cx = view_cx_at(600.0, 589.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let controls = compiled
            .semantics
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/control/"))
            .count();
        assert!(controls >= 2, "compact preview leaves useful controls");
        let preview = compiled
            .semantic(&UiKey::new("settings/preview/appearance"))
            .expect("compact keeps the live workbench");
        assert!(preview.rect.bottom() <= cx.viewport.bottom());
        let range = compiled
            .semantic(&UiKey::new("settings/results-range"))
            .expect("visible range status");
        // The Appearance roster total is derived from the live registry (the
        // full-coverage settings batch grew it past the original 11), so the
        // range status must agree with the real per-route field count.
        let appearance_total = prefs::editable_fields(&Config::default())
            .iter()
            .filter(|field| prefs::section_of(field.key) == prefs::Section::Appearance)
            .count();
        assert!(
            range
                .label
                .contains(&format!("1–{controls} of {appearance_total}")),
            "range status {:?} disagrees with the registry roster ({appearance_total})",
            range.label
        );
    }

    #[test]
    fn compact_route_without_preview_admits_four_complete_rows() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Terminal),
                    value: None,
                }),
            )
            .unwrap();
        let cx = view_cx_at(474.0, 658.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let controls = compiled
            .semantics
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/control/"))
            .count();
        assert!(controls >= 4);
        let range = compiled
            .semantic(&UiKey::new("settings/results-range"))
            .unwrap();
        assert!(!range.label.is_empty());
        for control in compiled
            .semantics
            .iter()
            .filter(|node| node.key.as_str().starts_with("settings/control/"))
        {
            assert!(control.rect.bottom() <= cx.viewport.bottom());
        }
    }

    #[test]
    fn compact_switch_is_trailing_touch_sized_and_never_a_full_width_bar() {
        for (viewport_width, viewport_height) in [(390.0, 568.0), (474.0, 658.0)] {
            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("settings/search"),
                        value: Some(SemanticInput::Text("cursor blink".to_string())),
                    }),
                )
                .unwrap();
            let cx = view_cx_at(viewport_width, viewport_height);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let key = UiKey::new(format!("settings/control/{}", prefs::EDIT_CURSOR_BLINK));
            let control = compiled.semantic(&key).expect("visible compact switch");
            let controls = compiled
                .semantic(&UiKey::new(format!(
                    "settings/controls/{}",
                    prefs::EDIT_CURSOR_BLINK
                )))
                .expect("full compact control band");
            assert!(
                (control.rect.width - 104.0).abs() < 0.01,
                "switch width is {:?}",
                control.rect
            );
            assert!(control.rect.height >= 44.0, "switch is touch-sized");
            assert!(control.rect.width < controls.rect.width * 0.5);
            assert!(
                (control.rect.right() - controls.rect.right()).abs() < 0.01,
                "switch trails its row: control={:?}, row={:?}",
                control.rect,
                controls.rect
            );
            let hit = compiled
                .hits
                .iter()
                .find(|hit| hit.key == key)
                .expect("switch exports a hit action");
            assert_eq!(hit.rect, control.rect);
            let SemanticValue::Bool(current) = &control.value else {
                panic!("switch exports boolean state");
            };
            let action = hit.action.clone();
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: action,
                        value: Some(SemanticInput::Bool(!*current)),
                    }),
                )
                .unwrap();
            assert!(outcome.effects.iter().any(|effect| matches!(
                effect,
                AppEffect::ConfigPatch { patch, .. }
                    if patch.edits.first().is_some_and(|edit| edit.key == prefs::EDIT_CURSOR_BLINK)
            )));
        }
    }

    #[test]
    fn compact_choice_picker_keeps_options_done_and_pagination_wholly_reachable() {
        for (viewport_width, viewport_height) in [
            // The ordinary 35-column Retina phone-width host presents an
            // exact 286.5-point semantic viewport. Keep it in the oracle so
            // page status copy cannot be squeezed by its sibling buttons.
            (286.5, 558.0),
            (320.0, 568.0),
            (390.0, 568.0),
            (474.0, 658.0),
        ] {
            let (mut runtime, instance, view) = setup();
            let open = || ActionInvocation {
                id: ActionId::new(format!("settings/set/{}", prefs::EDIT_THEME)),
                value: None,
            };
            runtime
                .dispatch(instance, view, AppEvent::Action(open()))
                .unwrap();
            let cx = view_cx_at(viewport_width, viewport_height);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let picker = compiled
                .semantic(&UiKey::new(format!(
                    "settings/choice-picker/{}",
                    prefs::EDIT_THEME
                )))
                .expect("compact theme picker");
            assert!(
                picker.rect.x >= 0.0
                    && picker.rect.y >= 0.0
                    && picker.rect.right() <= viewport_width + 0.01
                    && picker.rect.bottom() <= viewport_height + 0.01,
                "picker is whole at {viewport_width}x{viewport_height}: {:?}",
                picker.rect
            );

            let option_prefix = format!("settings/choice/{}/", prefs::EDIT_THEME);
            let options = compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with(&option_prefix))
                .collect::<Vec<_>>();
            assert_eq!(options.len(), ChoicePicker::PAGE_SIZE);
            for option in &options {
                assert!(option.rect.height >= 44.0);
                assert!(option.rect.width >= 140.0);
                assert!(option.rect.x >= picker.rect.x);
                assert!(option.rect.right() <= picker.rect.right() + 0.01);
                assert!(option.rect.bottom() <= picker.rect.bottom() + 0.01);
                let paint = compiled
                    .paint
                    .iter()
                    .find(|paint| paint.key == option.key)
                    .expect("choice is painted");
                let hit = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key == option.key)
                    .expect("choice is activatable");
                assert_eq!(paint.rect, option.rect, "choice is not clipped");
                assert_eq!(hit.rect, option.rect, "choice hit is not clipped");
            }
            for (index, option) in options.iter().enumerate() {
                for other in options.iter().skip(index + 1) {
                    assert!(
                        option.rect.intersect(other.rect).is_none(),
                        "choice text surfaces never overlap: {:?} and {:?}",
                        option.rect,
                        other.rect
                    );
                }
            }

            let control = |key: &str| {
                compiled
                    .semantic(&UiKey::new(key))
                    .unwrap_or_else(|| panic!("missing {key}"))
            };
            for key in [
                "settings/choice-close",
                "settings/choice-page-prev",
                "settings/choice-page-next",
            ] {
                let node = control(key);
                assert!(node.rect.height >= 44.0, "{key} is touch-sized");
                assert!(node.rect.bottom() <= picker.rect.bottom() + 0.01);
                assert!(node.rect.bottom() <= viewport_height + 0.01);
            }
            let page_label = control(&format!("settings/choice-page-label/{}", prefs::EDIT_THEME));
            let page_copy = "Page 1 of 7";
            let page_copy_width = crate::tray_raster::ui_text_width(page_copy, 13.0);
            assert!(
                page_label.rect.width + 0.01 >= page_copy_width,
                "{viewport_width}px picker clips {page_copy:?}: {:.1}px available, {page_copy_width:.1}px required",
                page_label.rect.width,
            );
            let close_action = compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == "settings/choice-close")
                .expect("Done is activatable")
                .action
                .clone();
            let next_action = compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == "settings/choice-page-next")
                .expect("Next is activatable")
                .action
                .clone();
            assert!(
                compiled
                    .hits
                    .iter()
                    .all(|hit| hit.key.as_str() != "settings/choice-page-prev"),
                "disabled Previous does not claim a hit"
            );

            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: close_action,
                        value: None,
                    }),
                )
                .unwrap();
            let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                panic!("Settings view");
            };
            assert!(state.choice_picker.is_none(), "Done closes the picker");

            runtime
                .dispatch(instance, view, AppEvent::Action(open()))
                .unwrap();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: next_action,
                        value: None,
                    }),
                )
                .unwrap();
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let first_next_key = UiKey::new(format!(
                "settings/choice/{}/{}",
                prefs::EDIT_THEME,
                ChoicePicker::PAGE_SIZE
            ));
            let first_next = compiled
                .hits
                .iter()
                .find(|hit| hit.key == first_next_key)
                .expect("next page is visible and activatable");
            assert!(
                compiled
                    .hits
                    .iter()
                    .any(|hit| hit.key.as_str() == "settings/choice-page-prev")
            );
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: first_next.action.clone(),
                        value: None,
                    }),
                )
                .unwrap();
            assert!(outcome.effects.iter().any(|effect| matches!(
                effect,
                AppEffect::ConfigPatch { patch, .. }
                    if patch.edits.first().is_some_and(|edit| edit.key == prefs::EDIT_THEME)
            )));
        }
    }

    #[test]
    fn compact_special_routes_semantically_page_to_every_action_and_value() {
        for viewport_width in [320.0, 390.0] {
            let cx = view_cx_at(viewport_width, 568.0);

            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::About),
                        value: None,
                    }),
                )
                .unwrap();
            let mut about_keys = BTreeSet::new();
            let about_sections = compact_about_section_count();
            for section in 0..about_sections {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                compiled.validate_parity().unwrap();
                for node in &compiled.semantics {
                    if node.key.as_str().starts_with("about/") {
                        assert!(
                            node.rect.bottom() <= cx.viewport.bottom() + 0.01,
                            "{} is clipped at {viewport_width}x568: {:?}",
                            node.key.as_str(),
                            node.rect
                        );
                        about_keys.insert(node.key.as_str().to_string());
                    }
                }
                if section + 1 < about_sections {
                    let next = compiled
                        .hits
                        .iter()
                        .find(|hit| hit.key.as_str() == "about/pagination/next")
                        .expect("About exposes a semantic Next action")
                        .action
                        .clone();
                    runtime
                        .dispatch(
                            instance,
                            view,
                            AppEvent::Action(ActionInvocation {
                                id: next,
                                value: None,
                            }),
                        )
                        .unwrap();
                }
            }
            for key in [
                "about/copy-build-info",
                "about/open-site",
                "about/provenance",
                "about/provenance/row/version",
                "about/provenance/row/build",
                "about/provenance/row/commit",
                "about/provenance/row/built",
                "about/provenance/row/arch",
                "about/provenance/row/compiler",
                "about/provenance/row/signature",
                "about/support",
            ] {
                assert!(
                    about_keys.contains(key),
                    "missing {key} at {viewport_width}x568"
                );
            }

            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::Diagnostics),
                        value: None,
                    }),
                )
                .unwrap();
            let mut diagnostic_keys = BTreeSet::new();
            for _ in 0..4 {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                for key in [
                    "settings/diagnostics/renderer",
                    "settings/diagnostics/interaction",
                    "settings/diagnostics/configuration",
                    "settings/diagnostics/semantic-ui",
                ] {
                    if let Some(card) = compiled.semantic(&UiKey::new(key)) {
                        assert!(card.rect.bottom() <= cx.viewport.bottom() + 0.01);
                        diagnostic_keys.insert(key);
                    }
                }
                let Some(next) = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str() == "settings/diagnostics/pagination/next")
                    .map(|hit| hit.action.clone())
                else {
                    break;
                };
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next,
                            value: None,
                        }),
                    )
                    .unwrap();
            }
            assert_eq!(diagnostic_keys.len(), 4);

            let update = update_status(true);
            let (mut runtime, instance, view) =
                setup_with_update(UpdateState::from_status(1, "0.1.0", Some(&update), false));
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::SoftwareUpdate),
                        value: None,
                    }),
                )
                .unwrap();
            let mut update_keys = BTreeSet::new();
            for section in 0..4 {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                compiled.validate_parity().unwrap();
                for node in &compiled.semantics {
                    if node.key.as_str().starts_with("updates/") {
                        assert!(
                            node.rect.bottom() <= cx.viewport.bottom() + 0.01,
                            "{} is clipped at {viewport_width}x568: {:?}",
                            node.key.as_str(),
                            node.rect
                        );
                        update_keys.insert(node.key.as_str().to_string());
                    }
                }
                if section < 3 {
                    let next = compiled
                        .hits
                        .iter()
                        .find(|hit| hit.key.as_str() == "updates/pagination/next")
                        .expect("Updates exposes a semantic Next action")
                        .action
                        .clone();
                    runtime
                        .dispatch(
                            instance,
                            view,
                            AppEvent::Action(ActionInvocation {
                                id: next,
                                value: None,
                            }),
                        )
                        .unwrap();
                }
            }
            for key in [
                "updates/check",
                "updates/install-relaunch",
                "updates/install-when-safe",
                "updates/headline",
                "updates/current",
                "updates/detail-card",
                "updates/service/service/value",
                "updates/service/running/value",
                "updates/service/staged/value",
                "updates/release-notes",
                "updates/outcome",
            ] {
                assert!(
                    update_keys.contains(key),
                    "missing {key} at {viewport_width}x568"
                );
            }
        }
    }

    #[test]
    fn numeric_slider_preserves_cursor_trail_default_and_both_endpoints() {
        let state = SettingsViewState::new(&Config::default());
        let field = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_CURSOR_TRAIL_MS)
            .expect("cursor trail duration field");
        let range = prefs::range_of(field.key).expect("bounded slider range");
        assert_eq!(range.step, 10.0);
        assert_eq!(
            numeric_slider_value(field),
            Some((260.0, "260".to_string()))
        );
        for expected in [30.0, 260.0, 2000.0] {
            assert_eq!(
                normalize_slider_value(expected, range),
                Some(expected.to_string())
            );
        }
        let row = setting_row(&state, 0, field, false, false, SettingsWidth::Compact);
        let compiled = UiTree::new(row)
            .compile(LogicalRect::new(0.0, 0.0, 320.0, 72.0))
            .unwrap();
        assert_eq!(
            compiled
                .semantic(&UiKey::new(format!(
                    "settings/control/{}",
                    prefs::EDIT_CURSOR_TRAIL_MS
                )))
                .expect("semantic slider")
                .value,
            SemanticValue::Number {
                value: 260.0,
                minimum: 30.0,
                maximum: 2000.0,
            }
        );
    }

    #[test]
    fn unset_text_setting_semantics_report_the_effective_default_while_paint_stays_quiet() {
        let state = SettingsViewState::new(&Config::default());
        let field = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_FONT_FAMILY)
            .expect("font family field");
        assert!(field.seed.is_none());
        assert!(!field.placeholder.is_empty());
        let row = setting_row(&state, 0, field, false, false, SettingsWidth::Compact);
        let compiled = UiTree::new(row)
            .compile(LogicalRect::new(0.0, 0.0, 390.0, 72.0))
            .unwrap();
        let key = UiKey::new(format!("settings/control/{}", field.key));
        assert_eq!(
            compiled.semantic(&key).expect("semantic text field").value,
            SemanticValue::Text(field.placeholder.clone())
        );
        let paint = compiled
            .paint
            .iter()
            .find(|node| node.key == key)
            .expect("paint text field");
        let UiContent::TextField(control) = &paint.content else {
            panic!("font family is a text field")
        };
        assert_eq!(control.spec.visual_value.as_deref(), Some(""));
    }

    #[test]
    fn update_check_command_and_reducer_share_enabled_state() {
        let assert_state = |update: UpdateState, expected_enabled: bool| {
            let (mut runtime, instance, view) = setup_with_update(update);
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::SoftwareUpdate),
                        value: None,
                    }),
                )
                .unwrap();
            let command = runtime
                .commands(instance, view)
                .unwrap()
                .into_iter()
                .find(|command| command.id.as_str() == "updates/check")
                .expect("update command");
            assert_eq!(command.enabled, expected_enabled);
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("updates/check"),
                        value: None,
                    }),
                )
                .unwrap();
            assert_eq!(
                outcome
                    .effects
                    .iter()
                    .filter(|effect| matches!(effect, AppEffect::Update { .. }))
                    .count(),
                usize::from(expected_enabled)
            );
        };

        assert_state(UpdateState::from_status(1, "0.1.0", None, false), false);
        let enabled = update_status(false);
        assert_state(
            UpdateState::from_status(1, "0.1.0", Some(&enabled), false),
            true,
        );
        assert_state(
            UpdateState::from_status(1, "0.1.0", Some(&enabled), true),
            false,
        );
    }

    #[test]
    fn virtual_window_exports_semantic_pagination_controls() {
        let (mut runtime, instance, view) = setup();
        let cx = view_cx();
        let first = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        assert!(
            first
                .semantic(&UiKey::new("settings/results-range"))
                .is_some()
        );
        let previous = first
            .semantic(&UiKey::new("settings/results-window/previous"))
            .expect("Previous remains in the accessibility tree while disabled");
        assert!(!previous.state.unwrap().enabled);
        let next = first
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == "settings/results-window/next")
            .expect("Next is visible and actionable")
            .action
            .clone();
        assert_eq!(next.as_str(), "settings/page-down");

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: next,
                    value: None,
                }),
            )
            .unwrap();
        let second = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        assert!(
            second
                .hits
                .iter()
                .any(|hit| hit.key.as_str() == "settings/results-window/previous"),
            "after paging, Previous is enabled through the same semantic path"
        );
        assert!(
            second
                .semantic(&UiKey::new("settings/results-range"))
                .unwrap()
                .label
                .starts_with("2–")
        );
    }

    #[test]
    fn compact_field_pager_reaches_every_control_at_phone_height() {
        let (mut runtime, instance, view) = setup();
        let expected = {
            let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                panic!("Settings view");
            };
            settings_field_result_count(state)
        };
        let cx = view_cx_at(320.0, 568.0);
        let mut seen = BTreeSet::new();
        for _ in 0..expected {
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            compiled.validate_parity().unwrap();
            for node in compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with("settings/control/"))
            {
                assert!(
                    node.rect.bottom() <= cx.viewport.bottom() + 0.01,
                    "{} is complete at 320×568: {:?}",
                    node.key.as_str(),
                    node.rect
                );
                seen.insert(node.key.as_str().to_string());
            }
            let Some(next) = compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == "settings/results-window/next")
                .map(|hit| hit.action.clone())
            else {
                break;
            };
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: next,
                        value: None,
                    }),
                )
                .unwrap();
        }
        assert_eq!(
            seen.len(),
            expected,
            "semantic Next reaches every omitted Appearance control"
        );
    }

    #[test]
    fn search_is_global_ranked_and_keeps_category_context() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::Text("clipboard".to_string())),
                }),
            )
            .unwrap();
        let cx = view_cx();
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("settings/control/copy_on_select"))
                .is_some()
        );
        let label = compiled
            .semantic(&UiKey::new("settings/label/copy_on_select"))
            .expect("globally matched input setting");
        assert!(
            label.label.contains("Input"),
            "category context: {}",
            label.label
        );
        assert!(
            compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with("settings/nav/"))
                .all(|node| node.state.is_none_or(|state| !state.selected)),
            "global search must not imply that results are route-scoped"
        );
    }

    #[test]
    fn default_on_boolean_apply_and_reset_round_trip_through_real_config_service() {
        let (mut runtime, instance, view) = setup();
        let mut service = VersionedConfigService::new(String::new()).unwrap();

        let modified = {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                panic!("Settings view");
            };
            state.navigate(SettingsRoute::Modified);
            runtime
                .render(instance, view, &view_cx())
                .unwrap()
                .compile(view_cx().viewport)
                .unwrap()
        };
        assert_eq!(
            modified
                .semantic(&UiKey::new("settings/empty"))
                .unwrap()
                .label,
            "No modified settings.",
            "resolved default-on Boolean seeds are not raw overrides"
        );

        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK)),
                    value: Some(SemanticInput::Bool(false)),
                }),
            )
            .unwrap();
        let (patch, operation) = outcome
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { patch, reply } => Some((patch.clone(), reply.operation)),
                _ => None,
            })
            .expect("Boolean edit reaches config service");
        assert_eq!(
            patch.edits[0].expected,
            ExpectedConfigValue::Exact(None),
            "the absent raw key, not effective true, is the OCC expectation"
        );
        let (snapshot, undo) = apply_real_config_patch(&mut service, &patch);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo,
                    },
                },
            )
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::ConfigChanged(snapshot.clone()))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        let blink = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_CURSOR_BLINK)
            .unwrap();
        assert_eq!(SettingsState::display_value(blink), "false");
        assert!(state.is_explicit(prefs::EDIT_CURSOR_BLINK));

        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/reset/{}", prefs::EDIT_CURSOR_BLINK)),
                    value: None,
                }),
            )
            .unwrap();
        let (patch, operation) = outcome
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { patch, reply } => Some((patch.clone(), reply.operation)),
                _ => None,
            })
            .expect("reset reaches config service");
        assert_eq!(
            patch.edits[0].expected,
            ExpectedConfigValue::Exact(Some("false".to_string()))
        );
        assert_eq!(patch.edits[0].value, None);
        let (snapshot, undo) = apply_real_config_patch(&mut service, &patch);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ConfigPatchFinished {
                    operation,
                    outcome: ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo,
                    },
                },
            )
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::ConfigChanged(snapshot.clone()))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        let blink = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == prefs::EDIT_CURSOR_BLINK)
            .unwrap();
        assert_eq!(
            SettingsState::display_value(blink),
            "true",
            "removing the key restores the effective default-on display"
        );
        assert!(!state.is_explicit(prefs::EDIT_CURSOR_BLINK));
    }

    #[test]
    fn config_snapshot_refreshes_every_view_after_external_or_cross_view_change() {
        let (mut runtime, instance, first) = setup();
        let mut core_views = ViewStore::default();
        let _ = core_views.insert_native(instance).unwrap();
        let second = core_views.insert_native(instance).unwrap();
        runtime
            .attach_view(
                second,
                instance,
                AppViewState::Settings(Box::new(SettingsViewState::new(&Config::default()))),
            )
            .unwrap();

        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let snapshot = service
            .replace_external("cursor_blink = false\nfont_px = 19.0\n".to_string())
            .unwrap();
        for view in [first, second] {
            runtime
                .dispatch(instance, view, AppEvent::ConfigChanged(snapshot.clone()))
                .unwrap();
            let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                panic!("Settings view");
            };
            let display = |key| {
                SettingsState::display_value(
                    state
                        .legacy
                        .fields
                        .iter()
                        .find(|field| field.key == key)
                        .unwrap(),
                )
                .to_string()
            };
            assert_eq!(display(prefs::EDIT_CURSOR_BLINK), "false");
            assert_eq!(display(prefs::EDIT_FONT_PX), "19");
            assert!(state.is_explicit(prefs::EDIT_CURSOR_BLINK));
            assert!(state.is_explicit(prefs::EDIT_FONT_PX));
        }
    }

    #[test]
    fn route_navigation_resolves_global_search_instead_of_leaving_dead_categories() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::Text("font".to_string())),
                }),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.route, SettingsRoute::About);
        assert!(state.search.is_empty());
        assert!(state.search_input.value().is_empty());
        let compiled = runtime
            .render(instance, view, &view_cx())
            .unwrap()
            .compile(view_cx().viewport)
            .unwrap();
        assert!(compiled.semantic(&UiKey::new("about/hero")).is_some());
        assert!(
            compiled
                .semantic(&UiKey::new("settings/page-heading/search-results"))
                .is_none()
        );
    }

    #[test]
    fn exact_phone_special_routes_have_zero_renderer_text_overflow() {
        let cx = view_cx_at(286.5, 558.0);
        let assert_fits = |compiled: &crate::native_ui::CompiledUi, context: &str| {
            let audit = compiled.paint_audit_lines();
            let overflow = audit
                .iter()
                .filter(|line| line.contains("overflow=true"))
                .cloned()
                .collect::<Vec<_>>();
            assert!(
                overflow.is_empty(),
                "{context} has renderer text overflow:\n{}",
                overflow.join("\n")
            );
            assert!(
                audit
                    .first()
                    .is_some_and(|line| line.contains("overflow=0")),
                "{context} publishes a zero-overflow summary"
            );
        };

        for route in [SettingsRoute::About, SettingsRoute::Diagnostics] {
            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(route),
                        value: None,
                    }),
                )
                .unwrap();
            for page in 0..8 {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                assert_fits(&compiled, &format!("{} page {}", route.label(), page + 1));
                let Some(next) = compiled
                    .hits
                    .iter()
                    .find(|hit| {
                        hit.key.as_str() == "about/pagination/next"
                            || hit.key.as_str() == "settings/diagnostics/pagination/next"
                    })
                    .map(|hit| hit.action.clone())
                else {
                    break;
                };
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next,
                            value: None,
                        }),
                    )
                    .unwrap();
            }
        }

        for staged in [false, true] {
            let update = update_status(staged);
            let (mut runtime, instance, view) =
                setup_with_update(UpdateState::from_status(1, "0.1.0", Some(&update), false));
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::SoftwareUpdate),
                        value: None,
                    }),
                )
                .unwrap();
            for page in 0..8 {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                assert_fits(
                    &compiled,
                    &format!("Software Update staged={staged} page {}", page + 1),
                );
                let Some(next) = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str() == "updates/pagination/next")
                    .map(|hit| hit.action.clone())
                else {
                    break;
                };
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next,
                            value: None,
                        }),
                    )
                    .unwrap();
            }
        }
    }

    #[test]
    fn compact_home_reaches_every_detail_route_without_a_hidden_rail() {
        for (viewport_width, viewport_height) in [(320.0, 568.0), (390.0, 568.0), (474.0, 658.0)] {
            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::Home),
                        value: None,
                    }),
                )
                .unwrap();
            let cx = view_cx_at(viewport_width, viewport_height);
            let mut route_actions = Vec::new();
            let mut seen = BTreeSet::new();
            for _ in 0..SettingsRoute::ALL.len() {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                compiled.validate_parity().unwrap();
                assert!(
                    compiled
                        .semantic(&UiKey::new("settings/navigation"))
                        .is_none()
                );
                assert!(
                    compiled
                        .semantic(&UiKey::new("settings/page/home"))
                        .is_some(),
                    "Home is the current compact route"
                );

                for route in SettingsRoute::ALL.into_iter().skip(1) {
                    let key = UiKey::new(format!("settings/home{}", route.path()));
                    let Some(node) = compiled.semantic(&key) else {
                        continue;
                    };
                    assert!(
                        node.rect.x >= 0.0
                            && node.rect.y >= 0.0
                            && node.rect.right() <= viewport_width + 0.01
                            && node.rect.bottom() <= viewport_height + 0.01,
                        "{} is wholly visible at {viewport_width}x{viewport_height}: {:?}",
                        route.label(),
                        node.rect
                    );
                    assert!(
                        node.rect.height >= 44.0,
                        "{} keeps a touch-sized hit target",
                        route.label()
                    );
                    let hit = compiled
                        .hits
                        .iter()
                        .find(|hit| hit.key == key)
                        .unwrap_or_else(|| panic!("{} is activatable", route.label()));
                    assert_eq!(hit.rect, node.rect, "{} is not clipped", route.label());
                    assert_eq!(hit.action, route_action(route));
                    if seen.insert(route.path()) {
                        route_actions.push((route, hit.action.clone()));
                    }
                }

                let Some(next) = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str() == "settings/home/pagination/next")
                    .map(|hit| hit.action.clone())
                else {
                    break;
                };
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next,
                            value: None,
                        }),
                    )
                    .unwrap();
            }
            assert_eq!(
                route_actions.len(),
                SettingsRoute::ALL.len() - 1,
                "semantic paging reaches every compact destination"
            );

            // Drive the exact actions exported by the hit map, not private
            // route state, so this also proves the visible phone controls are
            // wired through the normal semantic reducer path.
            for (route, action) in route_actions {
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: action,
                            value: None,
                        }),
                    )
                    .unwrap();
                let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                    panic!("Settings view");
                };
                assert_eq!(state.route, route);
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: route_action(SettingsRoute::Home),
                            value: None,
                        }),
                    )
                    .unwrap();
            }
        }
    }

    #[test]
    fn diagnostics_is_a_live_semantic_dashboard_not_an_empty_field_page() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Diagnostics),
                    value: None,
                }),
            )
            .unwrap();
        let wide = view_cx_at(1_200.0, 658.0);
        let dashboard = runtime
            .render(instance, view, &wide)
            .unwrap()
            .compile(wide.viewport)
            .unwrap();
        dashboard.validate_parity().unwrap();
        assert!(dashboard.semantic(&UiKey::new("settings/empty")).is_none());
        for key in [
            "settings/diagnostics/renderer",
            "settings/diagnostics/interaction",
            "settings/diagnostics/configuration",
            "settings/diagnostics/semantic-ui",
        ] {
            let card = dashboard
                .semantic(&UiKey::new(key))
                .unwrap_or_else(|| panic!("missing diagnostic card {key}"));
            assert!(card.rect.bottom() <= wide.viewport.bottom());
        }

        // A complete 14-route labeled rail requires the ordinary 513pt content
        // height. Medium still paginates rather than exposing a clipped third
        // card; shorter hosts correctly switch to the compact one-pane surface.
        let medium = view_cx_at(849.0, 513.0);
        let first = runtime
            .render(instance, view, &medium)
            .unwrap()
            .compile(medium.viewport)
            .unwrap();
        first.validate_parity().unwrap();
        assert!(
            first
                .semantic(&UiKey::new("settings/diagnostics/pagination"))
                .is_some()
        );
        for key in [
            "settings/diagnostics/renderer",
            "settings/diagnostics/renderer/detail",
            "settings/diagnostics/interaction",
            "settings/diagnostics/interaction/detail",
        ] {
            let node = first.semantic(&UiKey::new(key)).unwrap();
            assert!(
                node.rect.height >= 22.0,
                "{key} is clipped: {:?}",
                node.rect
            );
            assert!(node.rect.bottom() <= medium.viewport.bottom());
        }
        assert!(
            first
                .semantics
                .iter()
                .any(|node| node.label.contains("Input→present"))
        );

        for _ in 0..2 {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("settings/page-down"),
                        value: None,
                    }),
                )
                .unwrap();
        }
        let second = runtime
            .render(instance, view, &medium)
            .unwrap()
            .compile(medium.viewport)
            .unwrap();
        for key in [
            "settings/diagnostics/configuration",
            "settings/diagnostics/configuration/detail",
            "settings/diagnostics/semantic-ui",
            "settings/diagnostics/semantic-ui/detail",
        ] {
            let node = second
                .semantic(&UiKey::new(key))
                .unwrap_or_else(|| panic!("paged diagnostic node {key}"));
            assert!(
                node.rect.height >= 22.0,
                "{key} is clipped: {:?}",
                node.rect
            );
            assert!(node.rect.bottom() <= medium.viewport.bottom());
        }
    }

    #[test]
    fn text_edit_ownership_follows_exact_focus_click_and_navigation() {
        let (mut runtime, instance, view) = setup();
        let text_key = prefs::EDIT_FONT_FAMILY;
        let text_focus = UiKey::new(format!("settings/control/{text_key}"));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(text_focus.clone())),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Commit("Mono ".to_string())),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view");
        };
        assert_eq!(state.editing_field.as_deref(), Some(text_key));
        assert_eq!(state.field_inputs[text_key].value(), "Mono ");

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(format!(
                    "settings/control/{}",
                    prefs::EDIT_CURSOR_BLINK
                )))),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Commit("invisible".to_string())),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(state.editing_field.is_none());
        assert_eq!(state.field_inputs[text_key].value(), "Mono ");
        assert!(state.search_input.value().is_empty());

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(text_focus.clone())),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/page-down"),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(
            state.editing_field.is_none(),
            "another clicked control blurs the field"
        );

        runtime
            .dispatch(instance, view, AppEvent::FocusChanged(Some(text_focus)))
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(state.editing_field.is_none());
        assert_eq!(state.route, SettingsRoute::About);
    }

    #[test]
    fn escape_discards_field_draft_while_ordinary_blur_preserves_it() {
        let (mut runtime, instance, view) = setup();
        let text_key = prefs::EDIT_FONT_FAMILY;
        let text_focus = UiKey::new(format!("settings/control/{text_key}"));
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view");
        };
        state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == text_key)
            .unwrap()
            .seed = Some("Committed Mono".to_string());

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(text_focus.clone())),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::SelectAll),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Commit("Retained Draft".to_string())),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(format!(
                    "settings/control/{}",
                    prefs::EDIT_CURSOR_BLINK
                )))),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::FocusChanged(Some(text_focus.clone())),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(state.field_inputs[text_key].value(), "Retained Draft");

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::SelectAll),
            )
            .unwrap();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Commit("Cancelled Draft".to_string())),
            )
            .unwrap();
        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert!(state.editing_field.is_none());
        assert!(!state.field_inputs.contains_key(text_key));

        runtime
            .dispatch(instance, view, AppEvent::FocusChanged(Some(text_focus)))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(
            state.field_inputs[text_key].value(),
            "Committed Mono",
            "Escape reopens from committed reducer state, never the cancelled draft"
        );
    }

    #[test]
    fn route_and_search_scroll_clamps_recover_from_stale_overscroll_immediately() {
        let (mut runtime, instance, view) = setup();
        let route_limit = {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                panic!("Settings view");
            };
            state.route = SettingsRoute::Appearance;
            let count = state
                .legacy
                .fields
                .iter()
                .filter(|field| prefs::section_of(field.key) == Section::Appearance)
                .count();
            assert!(count > 1 && count < state.legacy.fields.len());
            state.page_scroll = usize::MAX;
            // Appearance owns virtual slice zero for its renderer preview, so
            // the genuine reducer limit is one past the last field offset.
            assert!(focused_preview_key(state).is_some());
            count
        };
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/page-up"),
                    value: None,
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(state.page_scroll, route_limit.saturating_sub(1));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/page-scroll"),
                    value: Some(SemanticInput::Number(f64::MAX)),
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(state.page_scroll, route_limit);

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::Text(
                        prefs::EDIT_FONT_FAMILY_BOLD_ITALIC.to_string(),
                    )),
                }),
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!();
        };
        assert_eq!(settings_field_result_count(state), 1);
        state.page_scroll = usize::MAX;
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(-1))
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(
            state.page_scroll, 0,
            "one reverse step escapes an obsolete all-fields scroll offset"
        );
    }

    #[test]
    fn pending_config_key_is_busy_and_rejects_a_second_mutation() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::CursorMotion),
                    value: None,
                }),
            )
            .unwrap();
        let action = ActionId::new(format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK));
        let first = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: action.clone(),
                    value: Some(SemanticInput::Bool(false)),
                }),
            )
            .unwrap();
        assert_eq!(
            first
                .effects
                .iter()
                .filter(|effect| matches!(effect, AppEffect::ConfigPatch { .. }))
                .count(),
            1
        );

        let second = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: action,
                    value: Some(SemanticInput::Bool(true)),
                }),
            )
            .unwrap();
        assert!(
            second
                .effects
                .iter()
                .all(|effect| !matches!(effect, AppEffect::ConfigPatch { .. })),
            "one key cannot enqueue a second write before its first completion"
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(state.pending.len(), 1);

        let cx = view_cx();
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let control = compiled
            .semantic(&UiKey::new(format!(
                "settings/control/{}",
                prefs::EDIT_CURSOR_BLINK
            )))
            .unwrap();
        let state = control.state.unwrap();
        assert!(!state.enabled);
        assert!(state.busy);
    }

    #[test]
    fn scrolling_windows_rows_without_changing_stable_keys() {
        let (mut runtime, instance, view) = setup();
        let cx = view_cx();
        let visible = |runtime: &NativeRuntime| {
            runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap()
                .semantics
                .into_iter()
                .filter(|node| node.key.as_str().starts_with("settings/control/"))
                .map(|node| node.key)
                .collect::<Vec<_>>()
        };
        let before = visible(&runtime);
        assert!(before.len() > 2, "test requires a window, got {before:?}");
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(1))
            .unwrap();
        let after = visible(&runtime);
        assert_eq!(before[1], after[0], "one-row scroll preserves row identity");
        assert_ne!(before[0], after[0]);
    }

    #[test]
    fn clear_search_is_visible_touchable_and_resets_the_complete_query_state() {
        for (width, height) in [(390.0, 568.0), (1_200.0, 820.0)] {
            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("settings/search"),
                        value: Some(SemanticInput::Text("font".to_string())),
                    }),
                )
                .unwrap();
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                panic!("Settings view")
            };
            state.search_input.select_all();
            state.page_scroll = 4;

            let cx = view_cx_at(width, height);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let clear = compiled
                .semantic(&UiKey::new("settings/search-clear"))
                .expect("nonempty search exports visible Clear");
            assert!(clear.rect.width >= 44.0 && clear.rect.height >= 32.0);
            assert!(
                compiled
                    .hits
                    .iter()
                    .any(|hit| hit.key.as_str() == "settings/search-clear")
            );

            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("settings/search-clear"),
                        value: None,
                    }),
                )
                .unwrap();
            let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
                panic!("Settings view")
            };
            assert!(state.search.is_empty());
            assert!(state.search_input.value().is_empty());
            assert_eq!(state.search_input.selection().range(), 0..0);
            assert!(!state.legacy.searching);
            assert!(state.legacy.query.is_empty());
            assert_eq!(state.page_scroll, 0);
            assert_eq!(
                state.common.last_focus.as_ref().map(UiKey::as_str),
                Some("settings/search")
            );
        }
    }

    #[test]
    fn global_search_projects_a_search_results_page_and_tab_tooltip() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/search"),
                    value: Some(SemanticInput::Text("cursor".to_string())),
                }),
            )
            .unwrap();
        let cx = view_cx();
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let page = compiled
            .semantic(&UiKey::new("settings/page/search-results"))
            .expect("search has its own semantic page identity");
        assert_eq!(page.label, "Search Results");
        assert!(
            compiled
                .semantic(&UiKey::new("settings/page/appearance"))
                .is_none()
        );
        assert_eq!(
            runtime
                .presentation(instance, view)
                .unwrap()
                .tooltip
                .as_deref(),
            Some("Settings · Search Results")
        );
    }

    #[test]
    fn all_four_font_family_drafts_drive_one_preview_without_committing() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::TextFonts),
                    value: None,
                }),
            )
            .unwrap();
        let authored = [
            (prefs::EDIT_FONT_FAMILY, "Draft Regular"),
            (prefs::EDIT_FONT_FAMILY_BOLD, "Draft Bold"),
            (prefs::EDIT_FONT_FAMILY_ITALIC, "Draft Italic"),
            (prefs::EDIT_FONT_FAMILY_BOLD_ITALIC, "Draft Bold Italic"),
        ];
        for (key, value) in authored {
            let focus = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(format!("settings/set/{key}")),
                        value: None,
                    }),
                )
                .unwrap();
            assert!(
                focus
                    .effects
                    .iter()
                    .all(|effect| !matches!(effect, AppEffect::ConfigPatch { .. }))
            );
            let typed = runtime
                .dispatch(instance, view, AppEvent::InsertText(value.to_string()))
                .unwrap();
            assert!(
                typed
                    .effects
                    .iter()
                    .all(|effect| !matches!(effect, AppEffect::ConfigPatch { .. }))
            );
        }
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view")
        };
        let spec = renderer_preview_spec(
            state,
            720,
            crate::native_app::ViewMotionCx::default(),
            13.0,
            aterm_render::Theme::default(),
        )
        .expect("Typography preview");
        assert_eq!(
            spec.font_candidate.regular.as_deref(),
            Some("Draft Regular")
        );
        assert_eq!(spec.font_candidate.bold.as_deref(), Some("Draft Bold"));
        assert_eq!(spec.font_candidate.italic.as_deref(), Some("Draft Italic"));
        assert_eq!(
            spec.font_candidate.bold_italic.as_deref(),
            Some("Draft Bold Italic")
        );
        for (key, _) in authored {
            assert!(!state.is_explicit(key), "draft {key} is not committed");
        }
        assert!(state.pending.is_empty());
    }

    #[test]
    fn about_copy_and_update_install_actions_emit_exact_typed_effects_and_complete() {
        let staged = update_status(true);
        let (mut runtime, instance, view) =
            setup_with_update(UpdateState::from_status(1, "0.1.0", Some(&staged), false));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let copied = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("about/copy-build-info"),
                    value: None,
                }),
            )
            .unwrap();
        let copy_operation = copied
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::Clipboard { request, reply } => {
                    assert_eq!(
                        request,
                        &ClipboardRequest::CopyText {
                            text: crate::about::provenance_text(),
                            sensitive: false,
                        }
                    );
                    Some(reply.operation)
                }
                _ => None,
            })
            .expect("copy emits one typed clipboard request");
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ClipboardFinished {
                    operation: copy_operation,
                    outcome: ClipboardOutcome::Copied,
                },
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view")
        };
        assert_eq!(state.feedback.as_deref(), Some("Build information copied"));

        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::SoftwareUpdate),
                    value: None,
                }),
            )
            .unwrap();
        let relaunch = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("updates/install-relaunch"),
                    value: None,
                }),
            )
            .unwrap();
        let relaunch_operation = relaunch
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::Update { request, reply } => {
                    assert_eq!(*request, UpdateRequest::InstallAndRelaunch);
                    Some(reply.operation)
                }
                _ => None,
            })
            .expect("relaunch emits typed updater request");
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::UpdateFinished {
                    operation: relaunch_operation,
                    outcome: UpdateOutcome::Blocked {
                        reasons: vec!["two sessions are still running".to_string()],
                    },
                },
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view")
        };
        assert_eq!(
            state.feedback.as_deref(),
            Some("Before relaunch: two sessions are still running")
        );

        let safe = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("updates/install-when-safe"),
                    value: None,
                }),
            )
            .unwrap();
        let safe_operation = safe
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::Update { request, reply } => {
                    assert_eq!(*request, UpdateRequest::InstallWhenSafe);
                    Some(reply.operation)
                }
                _ => None,
            })
            .expect("safe install emits typed updater request");
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::UpdateFinished {
                    operation: safe_operation,
                    outcome: UpdateOutcome::Accepted,
                },
            )
            .unwrap();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view")
        };
        assert_eq!(state.feedback.as_deref(), Some("Update request accepted"));
    }

    #[test]
    fn pending_config_disables_reset_and_undo_without_consuming_the_undo_token() {
        let (mut runtime, instance, view) = setup();
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            panic!("Settings view")
        };
        state.route = SettingsRoute::Modified;
        state.last_undo = Some(77);
        state
            .raw_values
            .insert(prefs::EDIT_CURSOR_BLINK.to_string(), "false".to_string());
        state
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == prefs::EDIT_CURSOR_BLINK)
            .unwrap()
            .seed = Some("false".to_string());
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("settings/set/{}", prefs::EDIT_CURSOR_BLINK)),
                    value: Some(SemanticInput::Bool(true)),
                }),
            )
            .unwrap();

        let cx = view_cx();
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let reset = compiled
            .semantic(&UiKey::new(format!(
                "settings/reset/{}",
                prefs::EDIT_CURSOR_BLINK
            )))
            .expect("pending modified row retains disabled Reset semantics");
        let reset_state = reset.state.unwrap();
        assert!(!reset_state.enabled && reset_state.busy);
        assert!(compiled.semantic(&UiKey::new("settings/undo")).is_none());
        assert!(
            !runtime
                .commands(instance, view)
                .unwrap()
                .into_iter()
                .find(|command| command.id.as_str() == "settings/undo")
                .unwrap()
                .enabled
        );

        let undo = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/undo"),
                    value: None,
                }),
            )
            .unwrap();
        assert!(
            undo.effects
                .iter()
                .all(|effect| !matches!(effect, AppEffect::ConfigUndo { .. }))
        );
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            panic!("Settings view")
        };
        assert_eq!(state.last_undo, Some(77));
        assert_eq!(
            state.feedback.as_deref(),
            Some("Finish applying the current change before Undo.")
        );
    }

    #[test]
    fn desktop_home_modified_card_names_the_destination_and_state() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Home),
                    value: None,
                }),
            )
            .unwrap();
        let cx = view_cx_at(1_200.0, 820.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let modified = compiled
            .semantic(&UiKey::new("settings/home/overview/modified"))
            .expect("desktop Modified glance card");
        assert!(modified.label.starts_with("Modified"));
        assert!(modified.label.contains("Defaults intact"));
    }

    #[test]
    fn short_landscape_home_about_and_update_are_complete_and_reachable() {
        for (viewport_width, viewport_height) in [(800.0, 320.0), (1_200.0, 400.0)] {
            let cx = view_cx_at(viewport_width, viewport_height);
            let (mut runtime, instance, view) = setup();
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::Home),
                        value: None,
                    }),
                )
                .unwrap();
            let mut routes = BTreeSet::new();
            for _ in 0..SettingsRoute::ALL.len() {
                let compiled = runtime
                    .render(instance, view, &cx)
                    .unwrap()
                    .compile(cx.viewport)
                    .unwrap();
                assert!(
                    compiled
                        .semantic(&UiKey::new("settings/navigation"))
                        .is_none()
                );
                for route in SettingsRoute::ALL.into_iter().skip(1) {
                    let key = UiKey::new(format!("settings/home{}", route.path()));
                    if let Some(node) = compiled.semantic(&key) {
                        assert!(node.rect.bottom() <= viewport_height + 0.01);
                        assert!(compiled.hits.iter().any(|hit| hit.key == key));
                        routes.insert(route.path());
                    }
                }
                let Some(next) = compiled
                    .hits
                    .iter()
                    .find(|hit| hit.key.as_str() == "settings/home/pagination/next")
                    .map(|hit| hit.action.clone())
                else {
                    break;
                };
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::Action(ActionInvocation {
                            id: next,
                            value: None,
                        }),
                    )
                    .unwrap();
            }
            assert_eq!(routes.len(), SettingsRoute::ALL.len() - 1);
        }

        let cx = view_cx_at(800.0, 320.0);
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();
        let mut about_seen = BTreeSet::new();
        for _ in 0..16 {
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let overflow = compiled
                .paint_audit_lines()
                .into_iter()
                .filter(|line| line.contains("overflow=true"))
                .collect::<Vec<_>>();
            assert!(
                overflow.is_empty(),
                "800×320 About overflow:\n{}",
                overflow.join("\n")
            );
            for node in &compiled.semantics {
                if node.key.as_str().starts_with("about/") {
                    assert!(
                        node.rect.bottom() <= 320.01,
                        "{}: {:?}",
                        node.key.as_str(),
                        node.rect
                    );
                    about_seen.insert(node.key.as_str().to_string());
                }
            }
            let Some(next) = compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == "about/pagination/next")
                .map(|hit| hit.action.clone())
            else {
                break;
            };
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: next,
                        value: None,
                    }),
                )
                .unwrap();
        }
        for key in [
            "about/copy-build-info",
            "about/open-site",
            "about/provenance",
            "about/support",
        ] {
            assert!(about_seen.contains(key), "short About never reached {key}");
        }

        let staged = update_status(true);
        let (mut runtime, instance, view) =
            setup_with_update(UpdateState::from_status(1, "0.1.0", Some(&staged), false));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::SoftwareUpdate),
                    value: None,
                }),
            )
            .unwrap();
        let mut update_seen = BTreeSet::new();
        for _ in 0..10 {
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let overflow = compiled
                .paint_audit_lines()
                .into_iter()
                .filter(|line| line.contains("overflow=true"))
                .collect::<Vec<_>>();
            assert!(
                overflow.is_empty(),
                "800×320 Update overflow:\n{}",
                overflow.join("\n")
            );
            for node in &compiled.semantics {
                if node.key.as_str().starts_with("updates/") {
                    assert!(
                        node.rect.bottom() <= 320.01,
                        "{}: {:?}",
                        node.key.as_str(),
                        node.rect
                    );
                    update_seen.insert(node.key.as_str().to_string());
                }
            }
            let Some(next) = compiled
                .hits
                .iter()
                .find(|hit| hit.key.as_str() == "updates/pagination/next")
                .map(|hit| hit.action.clone())
            else {
                break;
            };
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: next,
                        value: None,
                    }),
                )
                .unwrap();
        }
        for key in [
            "updates/check",
            "updates/install-relaunch",
            "updates/install-when-safe",
            "updates/service/service",
            "updates/service/running",
            "updates/service/staged",
            "updates/release-notes",
            "updates/outcome",
        ] {
            assert!(
                update_seen.contains(key),
                "short Update never reached {key}"
            );
        }
    }

    #[test]
    fn settings_home_paints_exact_author_and_company_byline() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::Home),
                    value: None,
                }),
            )
            .unwrap();

        let cx = view_cx_at(800.0, 820.0);
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let byline = compiled
            .semantic(&UiKey::new("settings/home/byline"))
            .expect("Settings Home exposes its attribution semantically");
        assert_eq!(byline.label, "By Andrew Yates \u{00b7} ALab");
        assert!(compiled.paint.iter().any(|node| {
            node.key == UiKey::new("settings/home/byline")
                && matches!(
                    &node.content,
                    UiContent::Text(TextSpec { text, .. })
                        if text == "By Andrew Yates \u{00b7} ALab"
                )
        }));
    }

    #[test]
    fn native_about_paints_exact_author_and_company_byline_at_every_width() {
        let (mut runtime, instance, view) = setup();
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: route_action(SettingsRoute::About),
                    value: None,
                }),
            )
            .unwrap();

        for (viewport_width, viewport_height) in [(1_200.0, 820.0), (474.0, 658.0)] {
            let cx = view_cx_at(viewport_width, viewport_height);
            let compiled = runtime
                .render(instance, view, &cx)
                .unwrap()
                .compile(cx.viewport)
                .unwrap();
            let byline = compiled
                .semantic(&UiKey::new("about/byline"))
                .expect("native About exposes its attribution semantically");
            assert_eq!(byline.label, "By Andrew Yates \u{00b7} ALab");
            assert!(compiled.paint.iter().any(|node| {
                node.key == UiKey::new("about/byline")
                    && matches!(
                        &node.content,
                        UiContent::Text(TextSpec { text, .. })
                            if text == "By Andrew Yates \u{00b7} ALab"
                    )
            }));
            assert!(
                compiled
                    .semantic(&UiKey::new("about/provenance/row/company"))
                    .is_none(),
                "company is part of the shared byline, not build metadata"
            );
        }
    }
}
