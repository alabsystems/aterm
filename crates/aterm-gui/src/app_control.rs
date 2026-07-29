// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Versioned semantic control for first-party native tab applications.
//!
//! Requests target stable `ViewId`s, never the focus observed on the control
//! thread. Resolution, semantic-key validation, reducer dispatch, effect
//! execution, presentation refresh, and repaint scheduling all happen in one
//! main-loop turn through [`App`].

use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use aterm_types::app_inspection::{
    ActRequest as WireActRequest, InspectRequest as WireInspectRequest, InspectionEnvelope,
    InspectionProjection, InspectionSubject, OpenAppRequest as WireOpenAppRequest, WireViewId,
};

use crate::native_app::{ActionInvocation, AppEvent, EventResult, SemanticInput, ViewCx};
use crate::native_ui::{ActionId, CompiledUi, SemanticRole, SemanticValue, UiKey};
use crate::tab_model::{LogicalRect, SplitAxis, SplitTree, Tab, View, ViewId};
use crate::{App, WindowId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InspectRequest {
    Tabs,
    View {
        view: ViewId,
        projection: InspectionProjection,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActRequest {
    pub(crate) view: ViewId,
    pub(crate) ui_key: String,
    pub(crate) action: String,
    pub(crate) value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenRequest {
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

fn parse_view(view: WireViewId<'_>) -> Result<ViewId, String> {
    let raw = view
        .as_str()
        .parse::<u64>()
        .map_err(|_| "view id must be an unsigned integer".to_string())?;
    Ok(ViewId::from_stored(raw))
}

impl App {
    pub(crate) fn inspect_app(&self, request: InspectRequest) -> Result<Vec<String>, String> {
        let topology = self.inspect_tabs_body();
        match request {
            InspectRequest::Tabs => {
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
        }
    }

    pub(crate) fn act_app(&mut self, request: ActRequest) -> Result<String, String> {
        let (wid, _) = self.locate_native_view(request.view)?;

        // Compile and validate in this same main-loop turn. A focus change between
        // socket parse and delivery cannot redirect the action to another view.
        let inspected = self.compile_view(request.view)?;
        let key = UiKey::new(request.ui_key.clone());
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
        let result = self.dispatch_native_view_event(
            wid,
            request.view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(request.action.clone()),
                value,
            }),
        )?;
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

    pub(crate) fn open_app(&mut self, request: OpenRequest) -> Result<String, String> {
        match request {
            OpenRequest::Settings(route) => self
                .open_settings_tab(route)
                .then(|| format!("app settings {}", route.path()))
                .ok_or_else(|| "Settings could not be opened in the requesting window".to_string()),
            OpenRequest::Markdown(uri) => {
                self.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
            }
            OpenRequest::Editor(uri) => {
                self.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            }
        }
    }

    fn locate_native_view(
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
        // The active view must be byte-for-byte the same compilation used by
        // paint, pointer hit testing, accessibility, and capture.  In
        // particular that path resolves the window's Retina scale and content
        // padding; rebuilding here from physical split bounds made inspection
        // report a 216px wide sidebar while the retained app-render artifact
        // used the 64px rail.
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
        // A visible sibling in a heterogeneous split is not the focused native
        // view, but it still has an exact retained semantic/raster artifact. Use
        // that artifact whenever its retained view identity and device geometry
        // still match the visible plan. Its model stamp may legitimately be stale:
        // after a reducer mutation but before present, it is precisely the
        // retained app-present artifact. Recompiling through the generic
        // inactive layout path would describe neither its retained split bounds
        // nor the font/theme inputs that produced its retained pixels.
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
                    config_revision: self.native_config_service.snapshot().revision,
                    update_revision: self.native_updater_service.snapshot().revision,
                    animation_phase_ms,
                    motion: self.native_view_motion_cx(wid, view),
                    terminal_font_px: self.win_font_px(wid),
                    terminal_theme: self.theme,
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
        let retained = window.leaf_render_cache.get(&view)?.native.as_ref()?;
        (retained.stamp.view == view
            && retained.stamp.instance == native.instance
            && retained.width == width
            && retained.height == height
            && retained.compiled.bounds == viewport)
            .then(|| {
                (
                    retained.compiled.clone(),
                    retained.stamp,
                    retained.stamp == current,
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
        tab.root
            .layout(self.content_bounds(wid)?, 1.0, 1.0)
            .into_iter()
            .find_map(|leaf| (leaf.value == view).then_some(leaf.rect))
            .ok_or_else(|| "view has no layout leaf".to_string())
    }

    fn inspect_tabs_body(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (wid, ws) in &self.windows {
            lines.push(format!(
                "window id={} focused={} tabs={} active={}",
                wid.0,
                self.frontmost_window == Some(*wid),
                ws.tab_set.len(),
                ws.tab_set.active_id().map_or(0, |id| id.get()),
            ));
            let bounds = self.content_bounds(*wid).unwrap_or_default();
            for (index, tab) in ws.tab_set.tabs().iter().enumerate() {
                let leaves = tab.root.layout(bounds, 1.0, 1.0);
                // Presentation titles are deliberately stable model metadata;
                // terminal chrome can instead be composed from user metadata,
                // OSC/cwd, and Smart Titles activity. The refresh path caches the
                // exact string it handed to the toolbar per TAB even when this is a
                // headless window, so inspection must read that same cache. Before
                // the first refresh, mirror `tab_titles`' stable fallback exactly.
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
                    "  tab id={} index={} kind={} title={:?} state={} active={}",
                    tab.id.get(),
                    index + 1,
                    tab_kind(self, tab),
                    title,
                    indicator_state(tab),
                    ws.tab_set.active_id() == Some(tab.id),
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

/// Canonical semantic controls plus the visible, source-addressed editor
/// projection carried by its `TextViewport`. Generic controls remain byte-for-
/// byte identical; editor observers gain the modeline/minibuffer and rows that
/// are already driving paint, pointer mapping, and accessibility.
fn semantic_controls_lines(compiled: &CompiledUi) -> Vec<String> {
    let mut lines = compiled.controls_lines();
    lines.extend(editor_viewport_inspection_lines(compiled));
    lines.extend(markdown_view_inspection_lines(compiled));
    lines
}

fn semantic_tree_lines(compiled: &CompiledUi) -> Vec<String> {
    let mut lines = compiled
        .semantics
        .iter()
        .map(|node| {
            let parent = node.parent.as_ref().map_or("-", UiKey::as_str);
            let action = node.action.as_ref().map_or("-", ActionId::as_str);
            format!(
                "node key={:?} parent={:?} role={:?} label={:?} value={} action={} state={} rect={:.1},{:.1},{:.1},{:.1}",
                node.key.as_str(),
                parent,
                node.role,
                node.label,
                semantic_value(&node.value),
                action,
                semantic_state(node.state),
                node.rect.x,
                node.rect.y,
                node.rect.width,
                node.rect.height,
            )
        })
        .collect::<Vec<_>>();
    lines.extend(editor_viewport_inspection_lines(compiled));
    lines.extend(markdown_view_inspection_lines(compiled));
    lines
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
        let painted_status = spec.status.as_deref().unwrap_or("Ready");
        let semantic_status = spec
            .semantic_status
            .as_deref()
            .or(spec.status.as_deref())
            .unwrap_or("Ready");
        let minibuffer = spec.minibuffer.as_deref();
        let footer = minibuffer.unwrap_or(painted_status);
        let modeline = format!("EDIT · {} · {cursor} | EMACS · {footer}", spec.label);
        lines.push(format!(
            "editor-view key={:?} mode=EDIT keymap=EMACS document={:?} dirty={} saving={} focused={} cursor={cursor:?}",
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
            semantic_status != painted_status,
        ));
        lines.push(format!(
            "editor-minibuffer key={:?} active={} value={:?}",
            paint.key.as_str(),
            minibuffer.is_some(),
            minibuffer.unwrap_or("Inactive"),
        ));
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
    let blocks = compiled
        .paint
        .iter()
        .filter_map(|paint| match &paint.content {
            crate::native_ui::UiContent::MarkdownBlock(spec) => Some((paint, spec)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if blocks.is_empty()
        && compiled
            .semantics
            .iter()
            .all(|node| !node.key.as_str().starts_with("markdown/"))
    {
        return Vec::new();
    }

    let has = |key: &str| compiled.semantic(&UiKey::new(key)).is_some();
    let mode = if has("markdown/split") {
        "split"
    } else if has("markdown/source") && !has("markdown/preview") {
        "source"
    } else {
        "preview"
    };
    let status = compiled
        .semantic(&UiKey::new("markdown/status"))
        .map_or("Unavailable", |node| node.label.as_str());
    let paging = |key: &str| {
        compiled.semantic(&UiKey::new(key)).map_or_else(
            || (false, "-"),
            |node| {
                (
                    node.state.is_none_or(|state| state.enabled),
                    node.action.as_ref().map_or("-", ActionId::as_str),
                )
            },
        )
    };
    let (previous_enabled, previous_action) = paging("markdown/previous-page-button");
    let (next_enabled, next_action) = paging("markdown/next-page-button");
    let focused = compiled
        .semantics
        .iter()
        .find(|node| node.state.is_some_and(|state| state.focused))
        .map_or("-", |node| node.key.as_str());
    let selected_mode = [
        ("preview", "markdown/preview-mode"),
        ("source", "markdown/source-mode"),
        ("split", "markdown/split-mode"),
    ]
    .into_iter()
    .find(|(_, key)| {
        compiled
            .semantic(&UiKey::new(*key))
            .and_then(|node| node.state)
            .is_some_and(|state| state.selected)
    })
    .map_or(mode, |(mode, _)| mode);

    let mut lines = vec![format!(
        "markdown-view mode={} selected-mode={} status={:?} focused-key={} previous-enabled={} previous-action={} next-enabled={} next-action={}",
        mode,
        selected_mode,
        status,
        focused,
        previous_enabled,
        previous_action,
        next_enabled,
        next_action,
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

fn semantic_value(value: &SemanticValue) -> String {
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
    if indicators.attention {
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
    let ids = tree.leaves();
    let mut matching = leaves.iter().filter(|leaf| ids.contains(&leaf.value));
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
    // Bind the mutex to a NAMED receiver before `.lock()` so the lock-order census can
    // resolve its identity (a `.lock()` on an anonymous `get_or_init(..)` expression is
    // an UNKNOWN-identity site, which the census's zero-unknowns obligation rejects).
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
mod tests {
    use super::*;

    #[test]
    fn wire_requests_become_stable_owned_targets() {
        assert_eq!(
            parse_inspect("app/v1 view 42 controls").unwrap(),
            InspectRequest::View {
                view: ViewId::from_stored(42),
                projection: InspectionProjection::Controls,
            }
        );
        let action = parse_act("app/v1 view 42 settings/font_px settings/set/font_px 16").unwrap();
        assert_eq!(action.view, ViewId::from_stored(42));
        assert_eq!(action.value.as_deref(), Some("16"));
        assert!(matches!(
            parse_open("app settings /about").unwrap(),
            OpenRequest::Settings(crate::native_settings::SettingsRoute::About)
        ));
    }

    #[test]
    fn editor_inspection_exposes_semantic_and_painted_statuses_and_diagnostics() {
        use crate::native_editor::{
            EditorDiagnosticSpan, EditorViewportLine, EditorViewportProjection,
        };
        use crate::native_ui::{
            Layout, Length, LogicalRect as UiLogicalRect, TextViewportSpec, UiContent, UiNode,
            UiTree,
        };

        let semantic_status =
            "Unknown config key `mystery`; open completion help for registered settings";
        let compiled = UiTree::new(
            UiNode::new(
                "editor/buffer",
                UiContent::TextViewport(TextViewportSpec {
                    label: "aterm.toml".to_string(),
                    document_key: "document:config@4".to_string(),
                    selectable: true,
                    projection: Some(EditorViewportProjection {
                        first_line: 8,
                        total_lines: 12,
                        lines: vec![EditorViewportLine {
                            number: 8,
                            source: 100..114,
                            column_start: 0,
                            text: "mystery = true".to_string(),
                            selections: Vec::new(),
                            carets: vec![(14, true)],
                            syntax: Vec::new(),
                            diagnostics: vec![
                                EditorDiagnosticSpan {
                                    bytes: 0..7,
                                    error: false,
                                },
                                EditorDiagnosticSpan {
                                    bytes: 10..14,
                                    error: true,
                                },
                            ],
                        }],
                    }),
                    preedit: String::new(),
                    status: Some("Unknown config key…".to_string()),
                    semantic_status: Some(semantic_status.to_string()),
                    minibuffer: None,
                    cursor_label: Some("Ln 9, Col 15".to_string()),
                    dirty: true,
                    saving: false,
                    focused: true,
                    action: Some(ActionId::new("editor/focus-buffer")),
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
        )
        .compile(UiLogicalRect::new(0.0, 0.0, 260.0, 180.0))
        .unwrap();

        let lines = editor_viewport_inspection_lines(&compiled);
        assert!(lines.iter().any(|line| {
            line.starts_with("editor-status ")
                && line.contains(&format!("value={semantic_status:?}"))
        }));
        assert!(lines.iter().any(|line| {
            line.starts_with("editor-painted-status ")
                && line.contains("value=\"Unknown config key…\"")
                && line.contains("truncated=true")
        }));
        assert!(lines.iter().any(|line| {
            line.starts_with("editor-row ")
                && line.contains("diagnostics=[0..7@100..107:warning,10..14@110..114:error]")
        }));
    }

    #[test]
    fn manual_controller_inspection_uses_live_config_diagnostics() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-control-manual-diagnostics-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "font_px = \"not-a-number\"\n").unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = 35;
            window.rows = 20;
        }
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (_, view) = app.active_native_view(wid).unwrap();
        let lines = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Text,
            })
            .unwrap();

        assert!(lines.iter().any(|line| {
            line.starts_with("editor-status ")
                && line.contains("font_px")
                && line.contains("not-a-number")
        }));
        assert!(lines.iter().any(|line| {
            line.starts_with("editor-painted-status ") && line.contains("truncated=true")
        }));
        assert!(lines.iter().any(|line| {
            line.starts_with("editor-row ")
                && line.contains("diagnostics=[")
                && line.contains(":error")
        }));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tabs_inspection_uses_effective_smart_title_with_stable_fallback() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        // A cache miss is explicit and deterministic: inspection mirrors the
        // same stable fallback as `tab_titles` instead of guessing from terminal
        // state on this read-only path, including its empty-title normalization.
        let tab_id = {
            let window = app.windows.get_mut(&wid).expect("headless window");
            window.tab_chrome_titles_by_tab.clear();
            let tab = window.tab_set.tab_at_mut(0).expect("terminal tab");
            tab.presentation.title.clear();
            tab.id
        };
        let empty_fallback = app.inspect_app(InspectRequest::Tabs).unwrap();
        let empty_line = empty_fallback
            .iter()
            .find(|line| line.starts_with(&format!("  tab id={} ", tab_id.get())))
            .expect("terminal tab inspection");
        assert!(
            empty_line.contains("kind=terminal") && empty_line.contains("title=\"aterm\""),
            "empty title did not mirror chrome fallback: {empty_line:?}"
        );
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .tab_at_mut(0)
            .unwrap()
            .presentation
            .title = "stable-terminal".to_string();
        let fallback = app.inspect_app(InspectRequest::Tabs).unwrap();
        let fallback_line = fallback
            .iter()
            .find(|line| line.starts_with(&format!("  tab id={} ", tab_id.get())))
            .expect("terminal tab inspection");
        assert!(fallback_line.contains("title=\"stable-terminal\""));

        // Drive the real composition and refresh path. An authored Title owns
        // the stable lane while generated Activity fills Description; the
        // headless cache is the exact label a windowed toolbar would receive.
        app.config.descriptive_titles = Some(true);
        app.config.title_summary_provider = Some(crate::app_config::TitleSummaryProvider::Builtin);
        app.config.tab_title_format = Some(crate::app_config::TitleFormat::TitleDescription);
        {
            let session = app.pool.get(0).expect("session 0");
            assert_eq!(
                session
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set("title", Some("title-smoke".to_string())),
                Some(true)
            );
        }
        app.note_title_activity(0);
        app.title_summaries
            .set_test_activity(0, "Reviewing release checks");
        app.refresh_window_tabs(wid);

        let inspected = app.inspect_app(InspectRequest::Tabs).unwrap();
        let inspected_line = inspected
            .iter()
            .find(|line| line.starts_with(&format!("  tab id={} ", tab_id.get())))
            .expect("terminal tab inspection");
        assert!(
            inspected_line.contains("title=\"title-smoke · Reviewing release checks\""),
            "inspection diverged from refreshed title chrome: {inspected_line:?}"
        );
    }

    #[test]
    fn tabs_inspection_keeps_distinct_labels_for_tabs_sharing_one_session() {
        let mut app = App::headless_for_test();
        let source = WindowId(0);
        let original_id = app.windows[&source].tab_set.active_id().unwrap();

        // Exercise the supported Cmd-Shift-O then Cmd-Shift-M topology: two
        // stable tabs in one window now view the same pooled terminal session.
        let shared_window = app
            .open_active_session_in_new_window_logical()
            .expect("share the active session into another window");
        let shared_id = app.windows[&shared_window].tab_set.active_id().unwrap();
        app.migrate_active_tab_to_next_window();
        assert_eq!(app.windows.len(), 1);
        assert_eq!(app.pool.views(0), Some(2));

        {
            let window = app.windows.get_mut(&source).unwrap();
            window.tab_title_cache.clear();
            window.tab_chrome_titles_by_tab.clear();
            for index in 0..window.tab_set.len() {
                let tab = window.tab_set.tab_at_mut(index).unwrap();
                tab.presentation.title = if tab.id == original_id {
                    "source-fallback".to_string()
                } else if tab.id == shared_id {
                    "shared-fallback".to_string()
                } else {
                    panic!("unexpected tab {}", tab.id.get());
                };
            }
        }

        // Make both title reads take their tab-specific presentation fallback.
        // A session-keyed inspection shadow aliases these and reports the last
        // tab's title twice; the per-tab shadow must preserve both vector slots.
        let term = app.pool.get(0).unwrap().term.clone();
        let parser_guard = term.lock().unwrap();
        app.refresh_window_tabs(source);
        let inspected = app.inspect_app(InspectRequest::Tabs).unwrap();
        let original_line = inspected
            .iter()
            .find(|line| line.starts_with(&format!("  tab id={} ", original_id.get())))
            .expect("original tab inspection");
        let shared_line = inspected
            .iter()
            .find(|line| line.starts_with(&format!("  tab id={} ", shared_id.get())))
            .expect("shared tab inspection");
        assert!(original_line.contains("title=\"source-fallback\""));
        assert!(shared_line.contains("title=\"shared-fallback\""));
        drop(parser_guard);

        let shared_index = app.windows[&source]
            .tab_set
            .tabs()
            .iter()
            .position(|tab| tab.id == shared_id)
            .unwrap();
        assert!(!app.close_tab_at(source, shared_index));
        let window = &app.windows[&source];
        assert!(window.tab_chrome_titles_by_tab.contains_key(&original_id));
        assert!(!window.tab_chrome_titles_by_tab.contains_key(&shared_id));
    }

    #[test]
    fn document_open_uses_host_grant_and_native_tab_path() {
        let mut app = App::headless_for_test();
        let dir =
            std::env::temp_dir().join(format!("aterm-control-document-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("README.md");
        std::fs::write(&path, "# Native app\n").unwrap();
        let request = format!("app markdown file://{}", path.to_string_lossy());
        let opened = app.open_app(parse_open(&request).unwrap()).unwrap();
        assert!(opened.starts_with("app markdown file://"));
        assert!(
            app.inspect_tabs_body()
                .iter()
                .any(|line| line.contains("app=markdown"))
        );
        let (_, view) = app.active_native_view(WindowId(0)).unwrap();
        let audit = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Audit,
            })
            .unwrap();
        assert!(audit[1].contains("compiled-fingerprint="));
        assert!(audit.iter().any(|line| {
            line.starts_with("markdown-view ")
                && line.contains("mode=preview")
                && line.contains("selected-mode=preview")
                && line.contains("previous-enabled=false")
                && line.contains("next-enabled=false")
        }));
        assert!(audit.iter().any(|line| {
            line.starts_with("paint-markdown ")
                && line.contains("block-kind=heading-1")
                && line.contains("source=0..")
        }));
        assert!(
            audit.iter().any(|line| {
                line.starts_with("paint-node key=\"markdown/mode-compact\"")
                    && line.contains("selected:true")
                    && line.contains("action=markdown/mode/source")
            }),
            "{audit:#?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_inspection_and_exact_action_are_end_to_end() {
        let mut app = App::headless_for_test();
        if let Some(window) = app.windows.get_mut(&WindowId(0)) {
            window.cols = 140;
            // Fourteen complete Settings destinations need enough vertical
            // room for the labeled medium rail. Keep this test above the
            // intentional compact-category breakpoint so it continues to
            // exercise width responsiveness rather than short-window fallback.
            window.rows = 100;
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let tabs = app.inspect_app(InspectRequest::Tabs).unwrap();
        assert!(tabs[0].starts_with("app-inspection version=app/v1"));
        assert!(tabs.iter().any(|line| line.contains("app=settings")));

        let (_, view) = app.active_native_view(WindowId(0)).unwrap();
        let controls = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Controls,
            })
            .unwrap();
        assert!(controls.iter().any(|line| line.contains("about/open-site")));
        let audit = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Audit,
            })
            .unwrap();
        assert!(audit[0].contains("projection=audit"));
        assert!(audit.iter().any(|line| line.starts_with("paint-audit ")));
        assert!(
            audit
                .iter()
                .any(|line| line.contains("key=\"about/wordmark\""))
        );

        // Navigate through the exact semantic key/action pair. A mismatched
        // action is rejected before reducer dispatch.
        assert!(
            app.act_app(ActRequest {
                view,
                ui_key: "settings/nav/appearance".to_string(),
                action: "settings/route/appearance".to_string(),
                value: None,
            })
            .is_ok()
        );
        assert!(
            app.act_app(ActRequest {
                view,
                ui_key: "settings/nav/appearance".to_string(),
                action: "updates/install-relaunch".to_string(),
                value: None,
            })
            .unwrap_err()
            .contains("does not match")
        );
    }

    #[test]
    fn inspection_reuses_staged_glass_after_responsive_resize_and_route_change() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (cell_width, cell_height) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        // Model a realistic Retina window: start just inside the wide layout,
        // then resize below its 1,040-logical-pixel breakpoint while retaining
        // enough height for the persistent navigation rail.
        let wide_columns = (2_100usize.saturating_sub(pad * 2) / cell_width.max(1)).max(1);
        let medium_columns = (2_000usize.saturating_sub(pad * 2) / cell_width.max(1)).max(1);
        let rows = (1_200usize.saturating_sub(pad * 2) / cell_height.max(1)).max(1);
        if let Some(window) = app.windows.get_mut(&wid) {
            window.scale = 2.0;
            window.cols = u16::try_from(wide_columns).unwrap();
            window.rows = u16::try_from(rows).unwrap();
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let (_, view) = app.active_native_view(wid).unwrap();

        // Stage the exact artifact paint/capture will lower, then navigate by
        // its semantic key, resize across a responsive breakpoint, and stage
        // again. Inspection must borrow that second artifact verbatim.
        assert!(app.prepare_native_input_scratch(wid));
        let wide = app.cached_native_ui(wid).unwrap().compiled.clone();
        assert!(
            wide.bounds.width >= 1_040.0,
            "wide bounds: {:?}",
            wide.bounds
        );
        assert_eq!(
            wide.semantic(&UiKey::new("settings/navigation"))
                .unwrap()
                .rect
                .width,
            216.0,
            "the first staged artifact uses the wide navigation rail"
        );
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = u16::try_from(medium_columns).unwrap();
        }
        assert!(app.prepare_native_input_scratch(wid));
        let retained_compilation = app.cached_native_ui(wid).unwrap().compiled.clone();
        let inspected = app.compile_view(view).unwrap();
        assert_eq!(inspected.source, InspectionCompileSource::Staged);
        assert_eq!(retained_compilation, inspected.compiled);
        assert!(
            (760.0..1_040.0).contains(&inspected.compiled.bounds.width),
            "medium-width bounds: {:?}",
            inspected.compiled.bounds
        );
        assert!(
            inspected
                .compiled
                .semantic(&UiKey::new("settings/compact-toolbar"))
                .is_none(),
            "a 1,000×600 logical window has room for the labeled macOS-style rail"
        );
        assert_eq!(
            inspected
                .compiled
                .semantic(&UiKey::new("settings/navigation"))
                .expect("medium navigation rail")
                .rect
                .width,
            196.0,
            "the staged artifact must reflect the wide-to-medium breakpoint"
        );

        let tree = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Tree,
            })
            .unwrap();
        assert!(tree[1].contains("source=staged"));
        assert_eq!(
            &tree[2..],
            semantic_tree_lines(&retained_compilation),
            "control geometry and semantics are the staged app-render compilation"
        );

        // Background views cannot claim retained-artifact parity. Their deterministic
        // fallback is deliberately labeled in the wire projection.
        assert!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .switch_to_index(0)
        );
        app.sync_window(wid);
        let inactive = app.compile_view(view).unwrap();
        assert_eq!(inactive.source, InspectionCompileSource::InactiveFallback);
        let inactive_wire = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Controls,
            })
            .unwrap();
        assert!(inactive_wire[1].contains("source=inactive-fallback"));
    }

    #[test]
    fn inspection_uses_exact_retained_native_sibling_when_terminal_has_focus() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, native_view) = app.active_native_view(wid).unwrap();
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert_eq!(
            app.windows[&wid].tab_set.active().unwrap().focus,
            terminal_view,
            "the terminal sibling owns focus"
        );
        app.prepare_heterogeneous_input_scratch(wid)
            .expect("mixed frame is retained");
        let retained = app.windows[&wid].leaf_render_cache[&native_view]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .clone();

        let inspected = app.compile_view(native_view).unwrap();
        assert_eq!(inspected.source, InspectionCompileSource::Retained);
        assert_eq!(inspected.compiled, retained);
        let wire = app
            .inspect_app(InspectRequest::View {
                view: native_view,
                projection: InspectionProjection::Audit,
            })
            .unwrap();
        assert!(wire[1].contains("source=retained"), "{wire:#?}");
        assert!(wire[1].contains(&format!(
            "compiled-fingerprint={:016x}",
            retained.fingerprint()
        )));

        // Advance the native reducer without presenting another composite. The
        // old retained raster is still exactly the retained app-present artifact,
        // so Audit must keep describing it and explicitly report that the model
        // has moved on.
        let outcome = app
            .native_runtime
            .dispatch(
                instance,
                native_view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("settings/route/appearance"),
                    value: None,
                }),
            )
            .unwrap();
        assert_eq!(outcome.result, EventResult::Handled);
        app.invalidate_native_view_cache(wid, native_view, crate::native_app::DamageRegion::All);

        let stale_on_glass = app.compile_view(native_view).unwrap();
        assert_eq!(stale_on_glass.source, InspectionCompileSource::Retained);
        assert!(!stale_on_glass.model_current);
        assert_eq!(stale_on_glass.compiled, retained);
        let stale_wire = app
            .inspect_app(InspectRequest::View {
                view: native_view,
                projection: InspectionProjection::Audit,
            })
            .unwrap();
        assert!(stale_wire[1].contains("source=retained"));
        assert!(stale_wire[1].contains("model-current=false"));
    }

    #[test]
    fn inactive_settings_inspection_uses_its_own_window_font() {
        let mut app = App::headless_for_test();
        app.windows.get_mut(&WindowId(0)).unwrap().metrics.font_px = 12.0;
        let next_session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(next_session), 50, 140);
        {
            let window = app.windows.get_mut(&second).unwrap();
            window.metrics.font_px = 24.0;
            window.scale = 2.0;
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let (_, view) = app.active_native_view(second).unwrap();

        // Force the Settings view down compile_view's inactive fallback rather than
        // the active/staged app_native path. The control observer must still resolve
        // the window that owns the view, not the process-global renderer activation.
        assert!(
            app.windows
                .get_mut(&second)
                .unwrap()
                .tab_set
                .switch_to_index(0)
        );
        app.sync_window(second);
        assert_eq!(app.font_px, crate::FONT_PX);

        let inspected = app.compile_view(view).unwrap();
        assert_eq!(inspected.source, InspectionCompileSource::InactiveFallback);
        assert_eq!(inspected.window, second);
        assert_eq!(inspected.scale, 2.0);
        let preview = inspected
            .compiled
            .semantics
            .iter()
            .find(|node| node.label == "Typography preview")
            .expect("Typography renderer preview semantics");
        let SemanticValue::Text(value) = &preview.value else {
            panic!("Typography preview has a text semantic value");
        };
        assert!(
            value.contains("at 24 pixels"),
            "inactive inspection must use the owning window's font size: {value}"
        );

        let wire = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Text,
            })
            .unwrap();
        assert!(wire[1].contains("source=inactive-fallback"));
        assert!(wire.iter().any(|line| line.contains("at 24 pixels")));
    }

    #[test]
    fn editor_inspection_and_command_controls_share_the_live_reducer() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = 92;
            window.rows = 34;
        }
        let dir = std::env::temp_dir().join(format!(
            "aterm-control-editor-inspection-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        let original = "alpha needle\nbeta line\n";
        std::fs::write(&path, original).unwrap();
        app.open_app(parse_open(&format!("app editor file://{}", path.to_string_lossy())).unwrap())
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        let inspect = |app: &App, projection| {
            app.inspect_app(InspectRequest::View { view, projection })
                .unwrap()
        };
        for projection in [
            InspectionProjection::Text,
            InspectionProjection::Controls,
            InspectionProjection::Tree,
            InspectionProjection::Audit,
        ] {
            let lines = inspect(&app, projection);
            assert!(lines[1].contains("compiled-fingerprint="));
            assert!(lines.iter().any(|line| {
                line.contains("editor-view")
                    && line.contains("mode=EDIT")
                    && line.contains("keymap=EMACS")
                    && line.contains("dirty=false")
                    && line.contains("saving=false")
            }));
            assert!(lines.iter().any(|line| {
                line.contains("editor-row") && line.contains(r#"text="alpha needle""#)
            }));
            assert!(lines.iter().any(|line| {
                line.contains("editor-row")
                    && line.contains("carets=[0@0:primary]")
                    && line.contains("selections=[]")
            }));
            assert!(lines.iter().any(|line| line.contains("editor-modeline")));
            assert!(lines.iter().any(|line| {
                line.contains("editor-minibuffer")
                    && line.contains("active=false")
                    && line.contains(r#"value="Inactive""#)
            }));
            if projection == InspectionProjection::Audit {
                assert!(
                    lines.iter().any(|line| {
                        line.starts_with("paint-editor key=\"editor/buffer\"")
                            && line.contains("focused=false")
                            && line.contains("carets=1")
                            && line.contains("selections=0")
                            && line.contains("paint-fingerprint=")
                    }),
                    "{lines:#?}"
                );
                assert!(lines.iter().any(|line| {
                    line.starts_with("paint-node key=\"editor/buffer\"")
                        && line.contains("kind=text-viewport")
                        && line.contains("focus-index=")
                        && line.contains("action=editor/focus-buffer")
                }));
            }
        }
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("editor state remains live");
        };
        state.buffer.as_mut().unwrap().selections =
            vec![crate::native_editor::Selection { anchor: 0, head: 5 }];
        let selected_tree = inspect(&app, InspectionProjection::Tree);
        assert!(selected_tree.iter().any(|line| {
            line.contains("editor-row")
                && line.contains("carets=[5@5:primary]")
                && line.contains("selections=[0..5@0..5:primary]")
        }));
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("editor state remains live");
        };
        state.buffer.as_mut().unwrap().selections = vec![crate::native_editor::Selection::caret(0)];

        let controls = inspect(&app, InspectionProjection::Controls);
        for (key, action) in [
            ("editor/save-button", "editor/save"),
            ("editor/undo-button", "editor/undo"),
            ("editor/redo-button", "editor/redo"),
            ("editor/find-button", "editor/find"),
            ("editor/goto-line-button", "editor/goto-line"),
            ("editor/commands-button", "editor/commands"),
        ] {
            assert!(controls.iter().any(|line| {
                line.contains(&format!("key={key:?}")) && line.contains(&format!("action={action}"))
            }));
        }

        let before_find_revision = inspect(&app, InspectionProjection::Text)[0].clone();
        app.act_app(ActRequest {
            view,
            ui_key: "editor/find-button".to_string(),
            action: "editor/find".to_string(),
            value: None,
        })
        .unwrap();
        let find_inspection = inspect(&app, InspectionProjection::Text);
        assert_ne!(find_inspection[0], before_find_revision);
        assert!(find_inspection.iter().any(|line| {
            line.contains("editor-minibuffer")
                && line.contains("active=true")
                && line.contains("I-search:")
        }));
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor state remains live");
        };
        assert!(matches!(
            state.buffer.as_ref().unwrap().minibuffer,
            crate::native_editor::Minibuffer::Search { ref query, .. } if query.is_empty()
        ));

        let before_paste_revision = find_inspection[0].clone();
        assert!(
            app.native_input_event(wid, &crate::input::InputEvent::Paste("needle".to_string()),)
        );
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor state remains live");
        };
        assert!(matches!(
            state.buffer.as_ref().unwrap().minibuffer,
            crate::native_editor::Minibuffer::Search { ref query, .. } if query == "needle"
        ));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            original
        );
        let search_text = inspect(&app, InspectionProjection::Text);
        assert_ne!(search_text[0], before_paste_revision);
        assert!(search_text.iter().any(|line| {
            line.contains("editor-minibuffer")
                && line.contains("active=true")
                && line.contains("I-search: needle")
        }));

        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::TextInput(crate::native_app::TextInputEvent::Cancel),
        )
        .unwrap();
        for (key, action, expected) in [
            ("editor/goto-line-button", "editor/goto-line", "Goto line:"),
            ("editor/commands-button", "editor/commands", "M-x"),
        ] {
            app.act_app(ActRequest {
                view,
                ui_key: key.to_string(),
                action: action.to_string(),
                value: None,
            })
            .unwrap();
            assert!(
                inspect(&app, InspectionProjection::Text)
                    .iter()
                    .any(|line| line.contains("editor-minibuffer") && line.contains(expected))
            );
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::TextInput(crate::native_app::TextInputEvent::Cancel),
            )
            .unwrap();
        }

        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::TextInput(crate::native_app::TextInputEvent::Commit("!".to_string())),
        )
        .unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            format!("!{original}")
        );
        for (key, action, expected_text) in [
            ("editor/undo-button", "editor/undo", original.to_string()),
            ("editor/redo-button", "editor/redo", format!("!{original}")),
        ] {
            app.act_app(ActRequest {
                view,
                ui_key: key.to_string(),
                action: action.to_string(),
                value: None,
            })
            .unwrap();
            assert_eq!(
                app.document_store.snapshot(document).unwrap().text.as_ref(),
                expected_text
            );
        }
        app.act_app(ActRequest {
            view,
            ui_key: "editor/save-button".to_string(),
            action: "editor/save".to_string(),
            value: None,
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("!{original}")
        );
        assert!(
            inspect(&app, InspectionProjection::Text)
                .iter()
                .any(|line| {
                    line.contains("editor-status") && line.contains(r#"value="Saved""#)
                })
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
