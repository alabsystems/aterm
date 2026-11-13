// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Versioned semantic control for first-party native tab applications ([`App`]).

use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use aterm_types::app_inspection::{
    ActRequest as WireActRequest, InspectRequest as WireInspectRequest, InspectionEnvelope,
    InspectionProjection, InspectionSubject, OpenAppRequest as WireOpenAppRequest, WireViewId,
};

use crate::native_app::{ActionInvocation, EventResult, SemanticInput, ViewCx};
use crate::native_ui::{ActionId, CompiledUi, SemanticRole, SemanticValue, UiKey};
use crate::tab_model::{LogicalRect, SplitAxis, SplitTree, Tab, View, ViewId};
use crate::{App, WindowId};

#[path = "app_control_network.rs"]
mod network;
#[cfg(test)]
pub(crate) use network::INSPECTED_FOR;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InspectRequest {
    Tabs,
    Network,
    View {
        view: ViewId,
        projection: InspectionProjection,
    },
    /// A `doc` read (`control_doc`) ([`App::answer_doc`]).
    Doc(crate::app_doc::DocCall),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActRequest {
    pub(crate) view: ViewId,
    pub(crate) ui_key: String,
    pub(crate) action: String,
    pub(crate) value: Option<String>,
}

/// The Settings actions only a person's own click or key presses, each with the buttons its
/// refusal names. `act` refuses them by the action's prefix before any view is read, so a page
/// that does not show the button yet refuses it the same way; every press of a native control
/// no person is known to stand behind (a control client's key or click, an accessibility
/// client's request) is refused them as it is dispatched ([`App::person_only_press_refusal`]).
const PERSON_ONLY_PREFIXES: [(&str, &str); 2] = [
    (
        crate::native_settings::MACOS_ACCESS_GESTURE_PREFIX,
        "Settings ▸ Security's macOS access buttons",
    ),
    (
        crate::native_settings::OWNER_RULES_PREFIX,
        "Settings ▸ Harness's Owner rules buttons",
    ),
];

/// The buttons `action` is one of where it is a person-only press ([`PERSON_ONLY_PREFIXES`]);
/// `None` for every other action.
fn person_only_buttons(action: &str) -> Option<&'static str> {
    PERSON_ONLY_PREFIXES
        .iter()
        .find(|(prefix, _)| action.starts_with(prefix))
        .map(|(_, buttons)| *buttons)
}

/// Why `act` refuses `action` ([`PERSON_ONLY_PREFIXES`]); `None` for every other action.
fn person_only_refusal(action: &str) -> Option<String> {
    person_only_buttons(action)
        .map(|buttons| format!("`act` does not press {buttons}; press them in the window"))
}

impl App {
    /// Why the gesture being dispatched does not press `action`: it is a person-only press
    /// ([`PERSON_ONLY_PREFIXES`]) and no person is known to stand behind the gesture
    /// ([`App::gesture_unvouched`]). `None` for a person's own press and for every other action.
    pub(crate) fn person_only_press_refusal(&self, action: &ActionId) -> Option<String> {
        self.gesture_unvouched()?;
        person_only_buttons(action.as_str()).map(|buttons| {
            format!("{buttons} take a person's own click or key; press them in the window")
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenRequest {
    Allocation { split: bool },
    Mail,
    About,
    SystemState,
    Workers,
    Tables,
    Settings(crate::native_settings::SettingsRoute),
    Markdown(String),
    Editor(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspectionCompileSource {
    Presented,
    Staged,
    Retained,
    ActiveFallback,
    InactiveFallback,
}

impl InspectionCompileSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Presented => "presented",
            Self::Staged => "staged",
            Self::Retained => "retained",
            Self::ActiveFallback => "active-fallback",
            Self::InactiveFallback => "inactive-fallback",
        }
    }
}

struct InspectedView {
    compiled: CompiledUi,
    source: InspectionCompileSource,
    view: ViewId,
    window: WindowId,
    scale: f32,
    generation: u64,
    geometry: u64,
    config_revision: u64,
    update_revision: u64,
    document_seq: Option<u64>,
    presentation_revision: u64,
    paint_revision: u64,
    model_current: bool,
    capture_serial: u64,
}

impl InspectedView {
    fn source_line(&self) -> String {
        format!(
            "inspection-source source={} view={} window={} generation={} geometry={:016x} scale={:.3} viewport={:.1},{:.1},{:.1},{:.1} config-revision={} update-revision={} document-seq={} presentation-revision={} paint-revision={:016x} model-current={} capture-serial={} compiled-fingerprint={:016x}",
            self.source.as_str(),
            self.view.get(),
            self.window.0,
            self.generation,
            self.geometry,
            self.scale,
            self.compiled.bounds.x,
            self.compiled.bounds.y,
            self.compiled.bounds.width,
            self.compiled.bounds.height,
            self.config_revision,
            self.update_revision,
            self.document_seq
                .map_or_else(|| "-".to_string(), |seq| seq.to_string()),
            self.presentation_revision,
            self.paint_revision,
            self.model_current,
            self.capture_serial,
            self.compiled.fingerprint(),
        )
    }
}

pub(crate) fn parse_inspect(rest: &str) -> Result<InspectRequest, String> {
    match aterm_types::app_inspection::parse_inspect(rest).map_err(|error| error.to_string())? {
        WireInspectRequest::Tabs => Ok(InspectRequest::Tabs),
        WireInspectRequest::Network => Ok(InspectRequest::Network),
        WireInspectRequest::View { view, projection } => Ok(InspectRequest::View {
            view: parse_view(view)?,
            projection,
        }),
    }
}

pub(crate) fn parse_act(rest: &str) -> Result<ActRequest, String> {
    let WireActRequest {
        view,
        ui_key,
        action,
        value,
    } = aterm_types::app_inspection::parse_act(rest).map_err(|error| error.to_string())?;
    Ok(ActRequest {
        view: parse_view(view)?,
        ui_key: ui_key.to_string(),
        action: action.to_string(),
        value: value.map(str::to_string),
    })
}

pub(crate) fn parse_open(rest: &str) -> Result<OpenRequest, String> {
    match aterm_types::app_inspection::parse_open_app(rest).map_err(|error| error.to_string())? {
        WireOpenAppRequest::Mail => Ok(OpenRequest::Mail),
        WireOpenAppRequest::About => Ok(OpenRequest::About),
        WireOpenAppRequest::SystemState => Ok(OpenRequest::SystemState),
        WireOpenAppRequest::Workers => Ok(OpenRequest::Workers),
        WireOpenAppRequest::Tables => Ok(OpenRequest::Tables),
        WireOpenAppRequest::Allocation { split } => Ok(OpenRequest::Allocation { split }),
        WireOpenAppRequest::Settings { route } => {
            let route = if route == "/" {
                Some(crate::native_settings::SettingsRoute::Home)
            } else {
                crate::native_settings::SettingsRoute::from_path(route)
            }
            .ok_or_else(|| format!("unknown Settings route {route:?}"))?;
            Ok(OpenRequest::Settings(route))
        }
        WireOpenAppRequest::Markdown { uri } => Ok(OpenRequest::Markdown(uri.to_string())),
        WireOpenAppRequest::Editor { uri } => Ok(OpenRequest::Editor(uri.to_string())),
    }
}

/// What `inspect` and the visible-text snapshot print in place of a private document's view.
pub(crate) const PRIVATE_VIEW_WITHHELD: &str = "withheld: a private document's text is not served";

/// What `text`, `screen`, `image`, `window`, `video`, `pointer` and `invoke` answer while a private
/// document's view is the front tab's content or one of its panes, and `act` on that view: as
/// `doc read` refuses the document, a capture of the frame or a read of the front would carry the
/// same values, and a press, click or menu action there would select, copy or close them for a
/// controller.
pub(crate) const PRIVATE_VIEW_REFUSED: &str =
    "refused private view: the front tab shows a private document";

/// What `hwkey` answers while any tab of the front window holds a private document's view: a
/// burst reads the front once per turn, so a key that switches tabs could otherwise carry the
/// burst's later keys into that view before the next read.
pub(crate) const PRIVATE_WINDOW_REFUSED: &str =
    "refused private view: the front window holds a private document";

fn parse_view(view: WireViewId<'_>) -> Result<ViewId, String> {
    let raw = view
        .as_str()
        .parse::<u64>()
        .map_err(|_| "view id must be an unsigned integer".to_string())?;
    Ok(ViewId::from_stored(raw))
}

impl App {
    pub(crate) fn inspect_app(&self, request: InspectRequest) -> Result<Vec<String>, String> {
        match request {
            InspectRequest::Network => network::inspect(self),
            InspectRequest::Tabs => {
                let topology = self.inspect_tabs_body();
                let revision = inspection_revision(self.inspection_fingerprint(&topology));
                let mut lines = Vec::with_capacity(topology.len() + 1);
                lines.push(
                    InspectionEnvelope {
                        revision,
                        subject: InspectionSubject::Tabs,
                    }
                    .header_line(),
                );
                lines.extend(topology);
                Ok(lines)
            }
            InspectRequest::View { view, projection } => {
                let topology = self.inspect_tabs_body();
                let inspected = self.compile_view(view)?;
                let revision = inspection_revision(
                    self.inspection_fingerprint(&topology)
                        ^ inspected.compiled.fingerprint().rotate_left(17),
                );
                let wire = view.get().to_string();
                let wire = WireViewId::new(&wire).map_err(|error| error.to_string())?;
                let mut lines = vec![
                    InspectionEnvelope {
                        revision,
                        subject: InspectionSubject::View {
                            view: wire,
                            projection,
                        },
                    }
                    .header_line(),
                    inspected.source_line(),
                ];
                if self.view_is_private(view) {
                    lines.push(PRIVATE_VIEW_WITHHELD.to_owned());
                    return Ok(lines);
                }
                lines.extend(match projection {
                    InspectionProjection::Text => semantic_text_lines(&inspected.compiled),
                    InspectionProjection::Controls => semantic_controls_lines(&inspected.compiled),
                    InspectionProjection::Tree => semantic_tree_lines(&inspected.compiled),
                    InspectionProjection::Audit => {
                        let mut audit = inspected.compiled.paint_audit_lines();
                        audit.extend(editor_viewport_inspection_lines(&inspected.compiled));
                        audit.extend(markdown_view_inspection_lines(&inspected.compiled));
                        audit
                    }
                });
                Ok(lines)
            }
            InspectRequest::Doc(call) => {
                self.answer_doc(call);
                Ok(Vec::new())
            }
        }
    }

    pub(crate) fn act_app(&mut self, request: ActRequest) -> Result<String, String> {
        if let Some(refusal) = person_only_refusal(&request.action) {
            return Err(refusal);
        }
        let (wid, _) = self.locate_native_view(request.view)?;
        if self.view_is_private(request.view) {
            return Err(PRIVATE_VIEW_REFUSED.to_owned());
        }

        let mut inspected = self.compile_view(request.view)?;
        let key = UiKey::new(request.ui_key.clone());
        if inspected
            .compiled
            .semantic(&key)
            .is_none_or(|node| node.offscreen)
            && match self.native_runtime.view_state_mut(request.view) {
                Some(crate::native_app::AppViewState::About(state)) => {
                    state.reveal_listed_control(&key)
                }
                Some(crate::native_app::AppViewState::Settings(state)) => {
                    state.reveal_listed_control(&key)
                }
                Some(crate::native_app::AppViewState::SystemState(state)) => {
                    state.reveal_listed_control(&key)
                }
                Some(crate::native_app::AppViewState::Workers(state)) => {
                    state.reveal_listed_control(&key)
                }
                Some(crate::native_app::AppViewState::Tables(state)) => {
                    state.reveal_listed_control(&key)
                }
                _ => false,
            }
        {
            self.invalidate_native_view_cache(
                wid,
                request.view,
                crate::native_app::DamageRegion::All,
            );
            inspected = self.compile_view(request.view)?;
        }
        let node = inspected
            .compiled
            .semantic(&key)
            .cloned()
            .ok_or_else(|| "unknown or stale UI key".to_string())?;
        let Some(node_action) = node.action.as_ref() else {
            return Err("semantic node is not actionable".to_string());
        };
        if node_action.as_str() != request.action {
            return Err("UI key/action pair does not match the live semantic tree".to_string());
        }
        if node.state.is_some_and(|state| !state.enabled) {
            return Err("semantic action is disabled".to_string());
        }
        let value = semantic_input(&node.role, &node.value, request.value.as_deref())?;
        let result = self.with_gesture_source(true, |app| {
            app.dispatch_native_control_action(
                wid,
                request.view,
                key,
                ActionInvocation {
                    id: ActionId::new(request.action.clone()),
                    value,
                },
                true,
            )
        })?;
        if result != EventResult::Handled {
            return Err("native reducer did not handle the semantic action".to_string());
        }
        Ok(format!(
            "acted view={} key={:?} action={}",
            request.view.get(),
            request.ui_key,
            request.action,
        ))
    }

    /// Refuse control-surface opens inside file-provider domains (`NEVER_COVERED`).
    pub(crate) fn refuse_file_provider_open(uri: &str) -> Result<(), String> {
        Self::refuse_file_provider_open_in(uri, std::env::var_os("HOME").map(Into::into))
    }

    /// [`Self::refuse_file_provider_open`] for the home `home`.
    fn refuse_file_provider_open_in(
        uri: &str,
        home: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        let Ok(path) = crate::native_document_host::file_uri_path(uri) else {
            return Ok(());
        };
        let Some(home) = home else {
            return Ok(());
        };
        let resolved = std::fs::canonicalize(&path).unwrap_or(path);
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        if atpkg::protected::under_file_provider_domain(&home, &resolved) {
            return Err(
                "refusing to open a document inside a cloud-storage folder from the control \
                 surface: macOS asks for that access in aterm's name, and no Full Disk Access \
                 grant covers it, so a program in a session must not be able to raise it. Open \
                 this one from the app itself."
                    .to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn open_app(&mut self, request: OpenRequest) -> Result<String, String> {
        match request {
            OpenRequest::Allocation { split } => self
                .open_allocation(split)
                .map(|()| "app allocation".into()),
            OpenRequest::Mail => self
                .open_mail_tab()
                .then(|| "app mail".to_owned())
                .ok_or_else(|| "Mail could not be opened".to_owned()),
            OpenRequest::About => self
                .open_about_tab()
                .then(|| "app about".to_string())
                .ok_or_else(|| "About could not be opened in the requesting window".to_string()),
            OpenRequest::SystemState => self
                .open_system_state_tab()
                .then(|| "app system-state".to_string())
                .ok_or_else(|| {
                    "System State could not be opened in the requesting window".to_string()
                }),
            OpenRequest::Workers => self.open_workers_tab().map(|()| "app workers".to_string()),
            OpenRequest::Tables => self.open_tables_tab().map(|()| "app tables".to_string()),
            OpenRequest::Settings(route) => self
                .open_settings_tab(route)
                .then(|| format!("app settings {}", route.path()))
                .ok_or_else(|| "Settings could not be opened in the requesting window".to_string()),
            OpenRequest::Markdown(uri) => {
                Self::refuse_file_provider_open(&uri)?;
                self.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
            }
            OpenRequest::Editor(uri) => {
                Self::refuse_file_provider_open(&uri)?;
                self.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            }
        }
    }

    /// Whether `view` edits a private document
    /// ([`crate::native_document_journal::private_uri`]): `inspect` and the visible-text snapshot
    /// withhold its text, as `doc read` refuses it.
    pub(crate) fn view_is_private(&self, view: ViewId) -> bool {
        let Some(View::Native(native)) = self.view_store.get(view).copied() else {
            return false;
        };
        self.native_runtime
            .document_id(native.instance)
            .and_then(|document| self.document_store.canonical_uri(document))
            .is_some_and(crate::native_document_journal::private_uri)
    }

    /// Whether `view` is the Recovery tab a handoff stood in for private documents
    /// ([`crate::restore::WindowLayout::stand_in_for_private_documents`]).
    fn view_stands_in_for_private(&self, view: ViewId) -> bool {
        let Some(View::Native(native)) = self.view_store.get(view).copied() else {
            return false;
        };
        matches!(
            self.native_runtime.app(native.instance),
            Some(crate::native_app::NativeApp::Recovery(recovery))
                if recovery.restore_tag == crate::restore::PRIVATE_DOCUMENT_TAG
        )
    }

    /// Whether no capture describes `view`: a private document, or the Recovery tab standing in for
    /// one, whose words are only for the update that closed the document. A quit or a crash keeps
    /// neither, so a next launch opens a new shell as ever, and a later handoff with nothing else
    /// stands the tab in again.
    pub(crate) fn view_has_no_capture(&self, view: ViewId) -> bool {
        self.view_is_private(view) || self.view_stands_in_for_private(view)
    }

    /// Whether a frame laid out by `plan` shows a private document's view
    /// ([`Self::view_is_private`]) in any pane.
    pub(crate) fn plan_shows_private(&self, plan: &crate::tab_model::VisibleLeafPlan) -> bool {
        plan.leaves
            .iter()
            .any(|leaf| self.view_is_private(leaf.view))
    }

    /// Whether `window`'s focused content is a private document's view.
    pub(crate) fn front_view_is_private(&self, window: WindowId) -> bool {
        self.active_native_view(window)
            .is_some_and(|(_, view)| self.view_is_private(view))
    }

    /// Why a controller's `event`, aimed at `window`'s front (`session` `None`), is refused: the
    /// focused content is a private document's view, which no controller types into, pastes into
    /// or copies from. A geometry or focus event touches none of its text.
    pub(crate) fn controller_input_refusal(
        &self,
        window: WindowId,
        session: Option<u64>,
        event: &crate::input::InputEvent,
    ) -> Option<&'static str> {
        use crate::input::InputEvent;
        (session.is_none()
            && !matches!(
                event,
                InputEvent::Resize { .. }
                    | InputEvent::ResizeWindowPx { .. }
                    | InputEvent::Focus(_)
            )
            && self.front_view_is_private(window))
        .then_some(PRIVATE_VIEW_REFUSED)
    }

    /// Whether `window`'s front tab shows a private document's view in any pane.
    pub(crate) fn window_shows_private(&self, window: WindowId) -> bool {
        self.active_visible_leaf_plan(window)
            .is_some_and(|plan| self.plan_shows_private(&plan))
    }

    /// What a controller's `invoke` answers while the front window's front tab shows a private
    /// document's view in any pane ([`Self::window_shows_private`]): a menu action there would
    /// select, copy or close its values for the controller.
    pub(crate) fn invoke_private_refusal(&self) -> Option<&'static str> {
        self.frontmost_window
            .is_some_and(|window| self.window_shows_private(window))
            .then_some(PRIVATE_VIEW_REFUSED)
    }

    /// Whether any tab of `window`, in front or not, parked or not, holds a private document's
    /// view.
    pub(crate) fn window_holds_private(&self, window: WindowId) -> bool {
        self.window_holds_a_view(window, |view| self.view_is_private(view))
    }

    /// Whether any tab of `window`, in front or not, parked or not, holds a view no capture
    /// describes ([`Self::view_has_no_capture`]).
    pub(crate) fn window_holds_no_capture(&self, window: WindowId) -> bool {
        self.window_holds_a_view(window, |view| self.view_has_no_capture(view))
    }

    fn window_holds_a_view(&self, window: WindowId, holds: impl Fn(ViewId) -> bool) -> bool {
        self.windows.get(&window).is_some_and(|ws| {
            ws.tab_set
                .tabs()
                .iter()
                .any(|tab| tab.root.any_leaf(&mut |&view| holds(view)))
        })
    }

    pub(crate) fn locate_native_view(
        &self,
        view: ViewId,
    ) -> Result<(WindowId, crate::tab_model::AppInstanceId), String> {
        let Some(View::Native(native)) = self.view_store.get(view).copied() else {
            return Err("no such native view".to_string());
        };
        let window = self
            .windows
            .iter()
            .find_map(|(wid, ws)| {
                ws.tab_set
                    .tabs()
                    .iter()
                    .any(|tab| tab.root.contains(view))
                    .then_some(*wid)
            })
            .ok_or_else(|| "native view is not attached to a window".to_string())?;
        Ok((window, native.instance))
    }

    fn compile_view(&self, view: ViewId) -> Result<InspectedView, String> {
        let (wid, instance) = self.locate_native_view(view)?;
        let capture_serial = self
            .windows
            .get(&wid)
            .map_or(0, |window| window.capture_present_serial);
        let scale = self
            .windows
            .get(&wid)
            .map_or(1.0, |window| window.scale.max(f64::EPSILON) as f32);
        if self
            .active_native_view(wid)
            .is_some_and(|(_, active_view)| active_view == view)
        {
            if let Some(frame) = self.cached_native_ui(wid) {
                let source = match frame.phase {
                    crate::app_native::NativeCompiledPhase::Presented => {
                        InspectionCompileSource::Presented
                    }
                    crate::app_native::NativeCompiledPhase::Staged => {
                        InspectionCompileSource::Staged
                    }
                };
                return Ok(InspectedView {
                    compiled: frame.compiled.clone(),
                    source,
                    view,
                    window: wid,
                    scale,
                    generation: frame.stamp.generation,
                    geometry: frame.stamp.geometry,
                    config_revision: frame.stamp.config_revision,
                    update_revision: frame.stamp.update_revision,
                    document_seq: frame.stamp.document_seq,
                    presentation_revision: frame.stamp.presentation_revision,
                    paint_revision: frame.stamp.paint_revision,
                    model_current: true,
                    capture_serial,
                });
            }
            let stamp = self.native_ui_compile_stamp(wid)?;
            return self.compiled_native_ui(wid).map(|compiled| InspectedView {
                compiled,
                source: InspectionCompileSource::ActiveFallback,
                view,
                window: wid,
                scale,
                generation: stamp.generation,
                geometry: stamp.geometry,
                config_revision: stamp.config_revision,
                update_revision: stamp.update_revision,
                document_seq: stamp.document_seq,
                presentation_revision: stamp.presentation_revision,
                paint_revision: stamp.paint_revision,
                model_current: true,
                capture_serial,
            });
        }
        if let Some((compiled, stamp, model_current)) = self.retained_visible_native_ui(wid, view) {
            return Ok(InspectedView {
                compiled,
                source: InspectionCompileSource::Retained,
                view,
                window: wid,
                scale,
                generation: stamp.generation,
                geometry: stamp.geometry,
                config_revision: stamp.config_revision,
                update_revision: stamp.update_revision,
                document_seq: stamp.document_seq,
                presentation_revision: stamp.presentation_revision,
                paint_revision: stamp.paint_revision,
                model_current,
                capture_serial,
            });
        }
        let viewport = self.view_rect(wid, view)?;
        let document = self
            .native_runtime
            .document_id(instance)
            .and_then(|document| self.document_store.snapshot(document));
        let ui_viewport = crate::native_ui::LogicalRect::new(
            viewport.origin.x / scale,
            viewport.origin.y / scale,
            viewport.size.width / scale,
            viewport.size.height / scale,
        );
        let animation_phase_ms =
            u64::try_from(self.lat_epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        let semantic_font = self.prepare_native_semantic_font(wid, view, animation_phase_ms);
        let tree = self
            .native_runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport: ui_viewport,
                    animation_phase_ms,
                    motion: self.native_view_motion_cx(wid, view),
                    terminal_font_px: self.win_font_px(wid),
                    terminal_scale: self.windows[&wid].scale.max(f64::EPSILON) as f32,
                    semantic_font,
                    document: document.as_ref(),
                },
            )
            .map_err(|error| format!("native render failed: {error:?}"))?;
        let compiled = tree
            .compile(ui_viewport)
            .map_err(|error| format!("native compile failed: {error:?}"))?;
        compiled
            .validate_parity()
            .map_err(|error| format!("native observer parity failed: {error:?}"))?;
        let stamp = self.native_ui_compile_stamp_for(wid, instance, view, ui_viewport)?;
        Ok(InspectedView {
            compiled,
            source: InspectionCompileSource::InactiveFallback,
            view,
            window: wid,
            scale,
            generation: stamp.generation,
            geometry: stamp.geometry,
            config_revision: stamp.config_revision,
            update_revision: stamp.update_revision,
            document_seq: stamp.document_seq,
            presentation_revision: stamp.presentation_revision,
            paint_revision: stamp.paint_revision,
            model_current: true,
            capture_serial,
        })
    }

    fn retained_visible_native_ui(
        &self,
        wid: WindowId,
        view: ViewId,
    ) -> Option<(CompiledUi, crate::app_native::NativeUiCompileStamp, bool)> {
        let plan = self.active_visible_leaf_plan(wid)?;
        let leaf = plan.leaf(view)?;
        let View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        let window = self.windows.get(&wid)?;
        let scale = window.scale.max(f64::EPSILON) as f32;
        let (cw, ch) = self.win_cell_size(wid);
        let width = (leaf.rect.size.width * cw as f32).round().max(1.0) as u32;
        let height = (leaf.rect.size.height * ch as f32).round().max(1.0) as u32;
        let viewport = crate::native_ui::LogicalRect::new(
            0.0,
            0.0,
            width as f32 / scale,
            height as f32 / scale,
        );
        let current = self
            .native_ui_compile_stamp_for(wid, native.instance, view, viewport)
            .ok()?;
        let cache = window.leaf_render_cache.get(&view)?;
        let retained = cache.native.as_ref()?;
        (retained.stamp.view == view
            && retained.stamp.instance == native.instance
            && retained.width == width
            && retained.height == height
            && retained.compiled.bounds == viewport)
            .then(|| {
                (
                    retained.compiled.clone(),
                    retained.stamp,
                    // An invalidation keeps the raster glass shows, stamp and all, though what the
                    // view draws may have moved where the stamp does not look.
                    cache.native_tree_current(retained) && retained.stamp == current,
                )
            })
    }

    fn content_bounds(&self, wid: WindowId) -> Result<LogicalRect, String> {
        let ws = self
            .windows
            .get(&wid)
            .ok_or_else(|| "unknown window".to_string())?;
        let (cw, ch) = self.win_cell_size(wid);
        Ok(LogicalRect::new(
            0.0,
            0.0,
            usize::from(ws.cols).saturating_mul(cw) as f32,
            usize::from(ws.rows).saturating_mul(ch) as f32,
        ))
    }

    fn view_rect(&self, wid: WindowId, view: ViewId) -> Result<LogicalRect, String> {
        let ws = self
            .windows
            .get(&wid)
            .ok_or_else(|| "unknown window".to_string())?;
        let tab = ws
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.contains(view))
            .ok_or_else(|| "view is not attached to the window".to_string())?;
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw as f32, ch as f32);
        if let Some(leaf) = self
            .plan_tab(tab, ws.rows, ws.cols)
            .leaves
            .into_iter()
            .find(|leaf| leaf.view == view)
        {
            let r = leaf.rect;
            return Ok(LogicalRect::new(
                r.origin.x * cw,
                r.origin.y * ch,
                r.size.width * cw,
                r.size.height * ch,
            ));
        }
        tab.root
            .layout(self.content_bounds(wid)?, 1.0, 1.0)
            .into_iter()
            .find_map(|leaf| (leaf.value == view).then_some(leaf.rect))
            .ok_or_else(|| "view has no layout leaf".to_string())
    }

    pub(crate) fn inspect_tabs_body(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (wid, ws) in &self.windows {
            lines.push(format!(
                "window id={} focused={} tabs={} active={} rows={} cols={}",
                wid.0,
                self.frontmost_window == Some(*wid),
                ws.tab_set.len(),
                ws.tab_set.active_id().map_or(0, |id| id.get()),
                ws.rows,
                ws.cols,
            ));
            let bounds = self.content_bounds(*wid).unwrap_or_default();
            for (index, tab) in ws.tab_set.tabs().iter().enumerate() {
                let leaves = tab.root.layout(bounds, 1.0, 1.0);
                let stable_fallback = if tab.presentation.title.is_empty() {
                    "aterm"
                } else {
                    tab.presentation.title.as_str()
                };
                let title = ws
                    .tab_chrome_titles_by_tab
                    .get(&tab.id)
                    .map_or(stable_fallback, |(_, title)| title.as_str());
                lines.push(format!(
                    "  tab id={} index={} kind={} title={:?} state={} active={} parked={}",
                    tab.id.get(),
                    index + 1,
                    tab_kind(self, tab),
                    title,
                    indicator_state(tab),
                    ws.tab_set.active_id() == Some(tab.id),
                    tab.parked,
                ));
                serialize_split(self, tab, &tab.root, &leaves, "/", 4, &mut lines);
            }
        }
        lines
    }

    fn inspection_fingerprint(&self, topology: &[String]) -> u64 {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        topology.hash(&mut hash);
        self.native_config_service
            .snapshot()
            .revision
            .hash(&mut hash);
        let mut views: Vec<_> = self.view_store.iter().collect();
        views.sort_by_key(|(view, _)| *view);
        for (view, kind) in views {
            view.get().hash(&mut hash);
            if let View::Native(native) = kind {
                native.instance.get().hash(&mut hash);
                if let Some(state) = self.native_runtime.view_state(view) {
                    state.common().presentation_revision.hash(&mut hash);
                }
                if let Some(document) = self.native_runtime.document_id(native.instance)
                    && let Some(snapshot) = self.document_store.snapshot(document)
                {
                    snapshot.seq.0.hash(&mut hash);
                }
            }
        }
        hash.finish()
    }
}

fn semantic_input(
    role: &SemanticRole,
    value: &SemanticValue,
    supplied: Option<&str>,
) -> Result<Option<SemanticInput>, String> {
    let Some(supplied) = supplied else {
        return Ok(None);
    };
    match (role, value) {
        (SemanticRole::Switch, SemanticValue::Bool(_)) => supplied
            .parse::<bool>()
            .map(SemanticInput::Bool)
            .map(Some)
            .map_err(|_| "switch value must be true or false".to_string()),
        (
            SemanticRole::Slider,
            SemanticValue::Number {
                minimum, maximum, ..
            },
        ) => {
            let number = supplied
                .parse::<f64>()
                .map_err(|_| "slider value must be a number".to_string())?;
            if !number.is_finite() || number < *minimum || number > *maximum {
                return Err(format!("slider value is outside {minimum}..{maximum}"));
            }
            Ok(Some(SemanticInput::Number(number)))
        }
        (SemanticRole::TextField, _) | (_, SemanticValue::Text(_)) => {
            Ok(Some(SemanticInput::Text(supplied.to_string())))
        }
        _ => Err("this semantic action does not accept a value".to_string()),
    }
}

pub(crate) fn semantic_text_lines(compiled: &CompiledUi) -> Vec<String> {
    let mut lines = compiled
        .semantics
        .iter()
        .filter_map(|node| {
            let value = match &node.value {
                SemanticValue::Text(value) if value != &node.label => Some(value.as_str()),
                SemanticValue::Bool(value) => {
                    return Some(format!(
                        "text key={:?} role={:?} label={:?} value={value}",
                        node.key.as_str(),
                        node.role,
                        node.label,
                    ));
                }
                SemanticValue::Number { value, .. } => {
                    return Some(format!(
                        "text key={:?} role={:?} label={:?} value={value}",
                        node.key.as_str(),
                        node.role,
                        node.label,
                    ));
                }
                _ => None,
            };
            if node.label.is_empty() && value.is_none() {
                return None;
            }
            Some(format!(
                "text key={:?} role={:?} label={:?}{}",
                node.key.as_str(),
                node.role,
                node.label,
                value.map_or_else(String::new, |value| format!(" value={value:?}")),
            ))
        })
        .collect::<Vec<_>>();
    lines.extend(editor_viewport_inspection_lines(compiled));
    lines.extend(markdown_view_inspection_lines(compiled));
    lines
}

/// Canonical semantic controls and visible editor projection from `TextViewport`.
fn semantic_controls_lines(compiled: &CompiledUi) -> Vec<String> {
    let mut lines = compiled.controls_lines();
    lines.extend(editor_viewport_inspection_lines(compiled));
    lines.extend(markdown_view_inspection_lines(compiled));
    lines
}

/// [`semantic_tree_lines`] for the page tests that pin what a driver reads.
#[cfg(test)]
pub(crate) fn semantic_tree_lines_for_test(compiled: &CompiledUi) -> Vec<String> {
    semantic_tree_lines(compiled)
}

fn semantic_tree_lines(compiled: &CompiledUi) -> Vec<String> {
    let mut lines = compiled
        .semantics
        .iter()
        .map(|node| {
            let parent = node.parent.as_ref().map_or("-", UiKey::as_str);
            let action = node.action.as_ref().map_or("-", ActionId::as_str);
            let mut line = format!(
                "node key={:?} parent={:?} role={:?} label={:?} value={} action={} state={} rect={:.1},{:.1},{:.1},{:.1}",
                node.key.as_str(),
                parent,
                node.role,
                node.label,
                crate::native_ui::semantic_value_words(&node.value),
                action,
                semantic_state(node.state),
                node.rect.x,
                node.rect.y,
                node.rect.width,
                node.rect.height,
            );
            if let Some(expanded) = node.state.and_then(|state| state.expanded) {
                line.push_str(&format!(" expanded={expanded}"));
            }
            if let Some(description) = &node.description {
                line.push_str(&format!(" description={description:?}"));
            }
            if let Some(words) = &node.value_text {
                line.push_str(&format!(" value-text={words:?}"));
            }
            line
        })
        .collect::<Vec<_>>();
    lines.extend(editor_viewport_inspection_lines(compiled));
    lines.extend(markdown_view_inspection_lines(compiled));
    lines
}

/// A grid view's inspection: its size, the rows it draws, its current cell, and its open cell
/// editor. Rows and columns count from 1, the header being row 1, as the gutter numbers them.
fn grid_inspection_lines(
    lines: &mut Vec<String>,
    key: &str,
    grid: &crate::native_editor::GridProjection,
) {
    lines.push(format!(
        "grid key={key:?} records={} columns={} delimiter={:?} shown={} pinned={} data_rows={} panel_lines={} problem_line={}",
        grid.records,
        grid.columns,
        grid.delimiter.char(),
        grid.shown
            .iter()
            .map(|shown| (shown.index + 1).to_string())
            .collect::<Vec<_>>()
            .join(","),
        u8::from(grid.pinned),
        grid.rows_layout.data,
        grid.rows_layout.panel,
        grid.problem_line
            .map_or_else(|| "none".to_string(), |line| line.to_string()),
    ));
    for row in std::iter::once(&grid.header).chain(&grid.rows) {
        let cells = row
            .cells
            .iter()
            .map(|cell| {
                if cell.missing {
                    "∅".to_string()
                } else {
                    cell.text.clone()
                }
            })
            .collect::<Vec<_>>();
        lines.push(format!(
            "grid-row key={key:?} row={} cells={cells:?}",
            row.record + 1
        ));
    }
    let cursor = &grid.cursor;
    lines.push(format!(
        "grid-cell key={key:?} row={} col={} line={} quoted={} missing={} value={:?}",
        cursor.record + 1,
        cursor.column + 1,
        cursor.line,
        cursor.quoted,
        cursor.missing,
        cursor.value,
    ));
    if let Some(draft) = grid.draft.as_ref() {
        lines.push(format!(
            "grid-draft key={key:?} caret={} value={:?}",
            draft.caret, draft.text
        ));
    }
}

fn editor_viewport_inspection_lines(compiled: &CompiledUi) -> Vec<String> {
    let mut lines = Vec::new();
    for paint in &compiled.paint {
        if paint.key.as_str() != "editor/buffer" {
            continue;
        }
        let crate::native_ui::UiContent::TextViewport(spec) = &paint.content else {
            continue;
        };
        let cursor = spec.cursor_label.as_deref().unwrap_or("Unavailable");
        let semantic_status = spec
            .semantic_status
            .as_deref()
            .or(spec.status.as_deref())
            .unwrap_or("Ready");
        let minibuffer = spec.minibuffer.as_deref();
        let painted_status = minibuffer.unwrap_or(spec.status.as_deref().unwrap_or("Ready"));
        let footer = painted_status;
        let mode = if spec.grid.is_some() { "GRID" } else { "EDIT" };
        let modeline = format!("{mode} · {} · {cursor} | EMACS · {footer}", spec.label);
        lines.push(format!(
            "editor-view key={:?} mode={mode} keymap=EMACS document={:?} dirty={} saving={} focused={} cursor={cursor:?}",
            paint.key.as_str(),
            spec.document_key,
            spec.dirty,
            spec.saving,
            spec.focused,
        ));
        lines.push(format!(
            "editor-modeline key={:?} value={modeline:?}",
            paint.key.as_str(),
        ));
        lines.push(format!(
            "editor-status key={:?} value={semantic_status:?}",
            paint.key.as_str(),
        ));
        lines.push(format!(
            "editor-painted-status key={:?} value={painted_status:?} truncated={}",
            paint.key.as_str(),
            !crate::native_ui::footer_shows_whole(semantic_status, painted_status),
        ));
        lines.push(format!(
            "editor-minibuffer key={:?} active={} value={:?}",
            paint.key.as_str(),
            minibuffer.is_some(),
            minibuffer.unwrap_or("Inactive"),
        ));
        if let Some(grid) = spec.grid.as_ref() {
            grid_inspection_lines(&mut lines, paint.key.as_str(), grid);
            continue;
        }
        let Some(projection) = spec.projection.as_ref() else {
            lines.push(format!(
                "editor-rows key={:?} visible=0 total=unknown",
                paint.key.as_str(),
            ));
            continue;
        };
        lines.push(format!(
            "editor-rows key={:?} first={} visible={} total={}",
            paint.key.as_str(),
            projection.first_line.saturating_add(1),
            projection.lines.len(),
            projection.total_lines,
        ));
        for row in &projection.lines {
            let carets = row
                .carets
                .iter()
                .map(|(byte, primary)| {
                    format!(
                        "{}@{}{}",
                        byte,
                        row.source.start.saturating_add(*byte),
                        if *primary { ":primary" } else { "" },
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let selections = row
                .selections
                .iter()
                .map(|selection| {
                    format!(
                        "{}..{}@{}..{}{}{}",
                        selection.bytes.start,
                        selection.bytes.end,
                        row.source.start.saturating_add(selection.bytes.start),
                        row.source.start.saturating_add(selection.bytes.end),
                        if selection.primary { ":primary" } else { "" },
                        if selection.continues {
                            ":continues"
                        } else {
                            ""
                        },
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let diagnostics = row
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    format!(
                        "{}..{}@{}..{}:{}",
                        diagnostic.bytes.start,
                        diagnostic.bytes.end,
                        row.source.start.saturating_add(diagnostic.bytes.start),
                        row.source.start.saturating_add(diagnostic.bytes.end),
                        if diagnostic.error { "error" } else { "warning" },
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            lines.push(format!(
                "editor-row key={:?} line={} source={}..{} column={} text={:?} carets=[{}] selections=[{}] diagnostics=[{}]",
                paint.key.as_str(),
                row.number.saturating_add(1),
                row.source.start,
                row.source.end,
                row.column_start,
                row.text,
                carets,
                selections,
                diagnostics,
            ));
        }
    }
    lines
}

fn markdown_view_inspection_lines(compiled: &CompiledUi) -> Vec<String> {
    if compiled
        .semantic(&UiKey::new("editor/markdown/toolbar"))
        .is_none()
    {
        return Vec::new();
    }
    let blocks = compiled
        .paint
        .iter()
        .filter_map(|paint| match &paint.content {
            crate::native_ui::UiContent::MarkdownBlock(spec) => Some((paint, spec)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mode = [
        ("read", "editor/markdown/read"),
        ("edit", "editor/markdown/edit"),
        ("split", "editor/markdown/split"),
    ]
    .into_iter()
    .find(|(_, key)| {
        compiled
            .semantic(&UiKey::new(*key))
            .and_then(|node| node.state)
            .is_some_and(|state| state.selected)
    })
    .map_or("-", |(mode, _)| mode);
    let status = compiled
        .semantic(&UiKey::new("editor/markdown/save-status"))
        .map_or("-", |node| node.label.as_str());
    let focused = compiled
        .semantics
        .iter()
        .find(|node| node.state.is_some_and(|state| state.focused))
        .map_or("-", |node| node.key.as_str());
    let mut lines = vec![format!(
        "markdown-view mode={} selected-mode={} status={:?} focused-key={}",
        mode, mode, status, focused,
    )];
    let source_start = blocks.iter().map(|(_, spec)| spec.source.start).min();
    let source_end = blocks.iter().map(|(_, spec)| spec.source.end).max();
    let selected = blocks.iter().filter(|(_, spec)| spec.selected).count();
    lines.push(format!(
        "markdown-visible blocks={} source={}..{} selected={}",
        blocks.len(),
        source_start.map_or_else(|| "-".to_string(), |value| value.to_string()),
        source_end.map_or_else(|| "-".to_string(), |value| value.to_string()),
        selected,
    ));
    for (paint, spec) in blocks {
        lines.push(format!(
            "markdown-block key={:?} source={}..{} selected={} selectable={} dense={} text={:?}",
            paint.key.as_str(),
            spec.source.start,
            spec.source.end,
            spec.selected,
            spec.selectable,
            spec.dense,
            spec.text,
        ));
    }
    lines
}

fn semantic_state(state: Option<crate::native_ui::ControlState>) -> String {
    state.map_or_else(
        || "-".to_string(),
        |state| {
            format!(
                "enabled={},focused={},focus-visible={},hovered={},pressed={},selected={},invalid={},busy={}",
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

fn tab_kind(app: &App, tab: &Tab) -> &'static str {
    if tab.root.len() != 1 {
        return "mixed";
    }
    match app.view_store.get(tab.focus) {
        Some(View::Terminal(_)) => "terminal",
        Some(View::Native(_)) => "app",
        None => "unknown",
    }
}

fn indicator_state(tab: &Tab) -> String {
    let indicators = tab.presentation.indicators;
    let mut states = Vec::new();
    if indicators.dirty {
        states.push("dirty");
    }
    if indicators.busy {
        states.push("busy");
    }
    if indicators.wants_attention() {
        states.push("attention");
    }
    if states.is_empty() {
        "clean".to_string()
    } else {
        states.join(",")
    }
}

fn serialize_split(
    app: &App,
    tab: &Tab,
    tree: &SplitTree<ViewId>,
    leaves: &[crate::tab_model::LayoutLeaf<ViewId>],
    path: &str,
    indent: usize,
    lines: &mut Vec<String>,
) {
    let pad = " ".repeat(indent);
    match tree {
        SplitTree::Leaf(view) => {
            let rect = leaves
                .iter()
                .find(|leaf| leaf.value == *view)
                .map(|leaf| leaf.rect)
                .unwrap_or_default();
            match app.view_store.get(*view).copied() {
                Some(View::Terminal(terminal)) => lines.push(format!(
                    "{pad}leaf path={path} view={} rect_px={} focused={} kind=terminal session={}",
                    view.get(),
                    rect_text(rect),
                    tab.focus == *view,
                    terminal.session,
                )),
                Some(View::Native(native)) => {
                    let kind = app
                        .native_runtime
                        .app(native.instance)
                        .map_or("unknown", |app| app.kind().as_str());
                    let title = app
                        .native_runtime
                        .presentation(native.instance, *view)
                        .map_or_else(|_| tab.presentation.title.clone(), |value| value.title);
                    lines.push(format!(
                        "{pad}leaf path={path} view={} rect_px={} focused={} kind=app app={} title={title:?}",
                        view.get(),
                        rect_text(rect),
                        tab.focus == *view,
                        kind,
                    ));
                }
                None => lines.push(format!(
                    "{pad}leaf path={path} view={} rect_px={} focused={} kind=missing",
                    view.get(),
                    rect_text(rect),
                    tab.focus == *view,
                )),
            }
        }
        SplitTree::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let rect = subtree_rect(tree, leaves);
            let axis = match axis {
                SplitAxis::Horizontal => "horizontal",
                SplitAxis::Vertical => "vertical",
            };
            lines.push(format!(
                "{pad}split path={path} axis={axis} ratio={ratio:.4} rect_px={}",
                rect_text(rect),
            ));
            let first_path = if path == "/" {
                "/0".to_string()
            } else {
                format!("{path}/0")
            };
            let second_path = if path == "/" {
                "/1".to_string()
            } else {
                format!("{path}/1")
            };
            serialize_split(app, tab, first, leaves, &first_path, indent + 2, lines);
            serialize_split(app, tab, second, leaves, &second_path, indent + 2, lines);
        }
    }
}

fn subtree_rect(
    tree: &SplitTree<ViewId>,
    leaves: &[crate::tab_model::LayoutLeaf<ViewId>],
) -> LogicalRect {
    let mut matching = leaves.iter().filter(|leaf| tree.contains(leaf.value));
    let Some(first) = matching.next() else {
        return LogicalRect::default();
    };
    let mut left = first.rect.origin.x;
    let mut top = first.rect.origin.y;
    let mut right = left + first.rect.size.width;
    let mut bottom = top + first.rect.size.height;
    for leaf in matching {
        left = left.min(leaf.rect.origin.x);
        top = top.min(leaf.rect.origin.y);
        right = right.max(leaf.rect.origin.x + leaf.rect.size.width);
        bottom = bottom.max(leaf.rect.origin.y + leaf.rect.size.height);
    }
    LogicalRect::new(left, top, right - left, bottom - top)
}

fn rect_text(rect: LogicalRect) -> String {
    format!(
        "{:.1},{:.1},{:.1},{:.1}",
        rect.origin.x, rect.origin.y, rect.size.width, rect.size.height,
    )
}

fn inspection_revision(fingerprint: u64) -> u64 {
    static CLOCK: OnceLock<Mutex<(u64, u64)>> = OnceLock::new();
    let clock_mutex = CLOCK.get_or_init(|| Mutex::new((0, 0)));
    let mut clock = clock_mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if clock.0 != fingerprint {
        clock.0 = fingerprint;
        clock.1 = clock.1.saturating_add(1).max(1);
    }
    clock.1.max(1)
}

#[cfg(test)]
#[path = "app_control_tests.rs"]
mod tests;
