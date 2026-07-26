// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shipping AccessKit seam for native tab applications.
//!
//! Paint stages a projection from its exact [`crate::native_ui::CompiledUi`]. A successful
//! present consumes that stage into the window adapter and retains only a stable action-route
//! table stamped with the originating view lifecycle generation.

use crate::native_accessibility::{
    AccessibilityOwner, NativeAccessibilityProjection, PublishedNativeAccessibility,
    RoutedAccessibilityAction, StagedNativeAccessibility, compose_native_accessibility,
    project_native_accessibility_for_view_in_container, route_accessibility_action,
};
use crate::native_app::{ActionInvocation, AppEvent};
use crate::native_ui::CompiledUi;
use crate::tab_model::{View, ViewId};
use crate::{App, WindowId};

impl App {
    /// Stage accessibility from the same compiled semantic snapshot paint is about to lower.
    pub(crate) fn stage_native_accessibility(
        &mut self,
        wid: WindowId,
        view: ViewId,
        _compiled: &CompiledUi,
    ) {
        let Ok((owners, focused_native)) = self.visible_native_accessibility_owners(wid) else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        // A heterogeneous frame is staged in one transaction after every leaf
        // cache has been updated. Publishing only the focused sibling here would
        // knowingly expose an incomplete tree.
        if owners.len() != 1 || owners[0].view != view {
            self.clear_staged_native_accessibility(wid);
            return;
        }
        let Ok(projection) =
            self.retained_native_accessibility_projection(wid, &owners, focused_native, false)
        else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_a11y_staged = Some(StagedNativeAccessibility { owners, projection });
        }
    }

    /// Stage one composite from the retained semantic artifacts painted for all
    /// visible native siblings in a heterogeneous frame.
    pub(crate) fn stage_visible_native_accessibility(&mut self, wid: WindowId) {
        let Ok((owners, focused_native)) = self.visible_native_accessibility_owners(wid) else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        if owners.is_empty() {
            self.clear_staged_native_accessibility(wid);
            return;
        }
        let Ok(projection) =
            self.retained_native_accessibility_projection(wid, &owners, focused_native, false)
        else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        if let Some(window) = self.windows.get_mut(&wid) {
            window.native_a11y_staged = Some(StagedNativeAccessibility { owners, projection });
        }
    }

    /// Produce the native tree for a publish. Both the staged path and fallback
    /// consume retained semantic/raster artifacts; a missing or stale presented
    /// leaf fails closed instead of publishing a hypothetical recompile.
    pub(crate) fn take_native_accessibility_update(
        &mut self,
        wid: WindowId,
    ) -> Option<Result<(accesskit::TreeUpdate, PublishedNativeAccessibility), String>> {
        let (owners, focused_native) = match self.visible_native_accessibility_owners(wid) {
            Ok((owners, focused_native)) if !owners.is_empty() => (owners, focused_native),
            Ok(_) => return None,
            Err(error) => return Some(Err(error)),
        };
        let staged = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.native_a11y_staged.take())
            .filter(|staged| staged.owners == owners);

        let projection = match staged {
            Some(staged) => staged.projection,
            None => {
                match self.retained_native_accessibility_projection(
                    wid,
                    &owners,
                    focused_native,
                    true,
                ) {
                    Ok(projection) => projection,
                    Err(error) => return Some(Err(error)),
                }
            }
        };
        let (update, routes, virtual_text) = projection.into_update_routes_and_virtual_text();
        let primary = focused_native
            .and_then(|view| owners.iter().copied().find(|owner| owner.view == view))
            .unwrap_or(owners[0]);
        Some(Ok((
            update,
            PublishedNativeAccessibility::composite(primary, owners, routes, virtual_text),
        )))
    }

    fn retained_native_accessibility_projection(
        &self,
        wid: WindowId,
        owners: &[AccessibilityOwner],
        focused_native: Option<ViewId>,
        require_presented: bool,
    ) -> Result<NativeAccessibilityProjection, String> {
        let mut projections = Vec::with_capacity(owners.len());
        for owner in owners {
            let artifact = self
                .retained_native_leaf_artifact(wid, owner.view, require_presented)
                .ok_or_else(|| "native accessibility retained leaf is stale".to_string())?;
            if artifact.generation != owner.generation {
                return Err("native accessibility retained leaf is stale".to_string());
            }
            let focus = self
                .native_runtime
                .view_state(owner.view)
                .and_then(|state| state.common().last_focus.as_ref());
            let transform = self
                .retained_native_accessibility_transform_for_view(wid, owner.view)
                .ok_or_else(|| "native accessibility retained placement is invalid".to_string())?;
            let projection = project_native_accessibility_for_view_in_container(
                artifact.compiled,
                focus,
                transform,
                *owner,
            )
            .map_err(|error| format!("native accessibility projection failed: {error:?}"))?;
            projections.push((*owner, projection));
        }
        let bounds = self
            .native_accessibility_window_bounds(wid)
            .ok_or_else(|| "native accessibility window is no longer live".to_string())?;
        compose_native_accessibility(projections, focused_native, bounds)
            .map_err(|error| format!("native accessibility composition failed: {error:?}"))
    }

    fn visible_native_accessibility_owners(
        &self,
        wid: WindowId,
    ) -> Result<(Vec<AccessibilityOwner>, Option<ViewId>), String> {
        let plan = self
            .active_visible_leaf_plan(wid)
            .ok_or_else(|| "native accessibility window is no longer live".to_string())?;
        let mut owners = Vec::new();
        let mut focused_native = None;
        for leaf in &plan.leaves {
            if !matches!(self.view_store.get(leaf.view), Some(View::Native(_))) {
                continue;
            }
            let generation = self
                .native_runtime
                .view_generation(leaf.view)
                .ok_or_else(|| "native accessibility view is no longer live".to_string())?;
            owners.push(AccessibilityOwner {
                view: leaf.view,
                generation,
            });
            if leaf.focused {
                focused_native = Some(leaf.view);
            }
        }
        Ok((owners, focused_native))
    }

    /// Map one native leaf's logical coordinates to the exact physical-pixel
    /// position used by the presented card, including raw-window remainder bands.
    #[cfg(test)]
    fn native_accessibility_transform_for_view(
        &self,
        wid: WindowId,
        view: ViewId,
    ) -> Option<accesskit::Affine> {
        if let Some(transform) = self.retained_native_accessibility_transform_for_view(wid, view) {
            return Some(transform);
        }
        let window = self.windows.get(&wid)?;
        let plan = self.active_visible_leaf_plan(wid)?;
        let leaf = plan.leaf(view)?;
        let (frame_x, frame_y) = self.frame_origin(wid);
        let (cw, ch) = self.win_cell_size(wid);
        let (x, y) = if plan.leaves.len() == 1 {
            (
                frame_x,
                frame_y.saturating_add(self.native_content_origin_y(wid) as i64),
            )
        } else {
            let leaf_x = (leaf.rect.origin.x * cw as f32).round().max(0.0) as usize;
            let leaf_y = (leaf.rect.origin.y * ch as f32).round().max(0.0) as usize;
            (
                frame_x
                    .saturating_add(self.win_pad(wid) as i64)
                    .saturating_add(leaf_x as i64),
                frame_y
                    .saturating_add(self.native_content_origin_y(wid) as i64)
                    .saturating_add(leaf_y as i64),
            )
        };
        let scale = window.scale.max(f64::EPSILON);
        Some(
            accesskit::Affine::translate(accesskit::Vec2::new(x as f64, y as f64))
                * accesskit::Affine::scale(scale),
        )
    }

    /// Transform sourced only from the compositor's retained destination
    /// record. `SettingsCard` is the one presented native layer for both a
    /// singleton and a heterogeneous split; each `NativeLeafRaster` stores its
    /// exact destination inside that layer. Bounds checks reject a torn pairing.
    fn retained_native_accessibility_transform_for_view(
        &self,
        wid: WindowId,
        view: ViewId,
    ) -> Option<accesskit::Affine> {
        let window = self.windows.get(&wid)?;
        let card = window.settings_card.as_ref()?;
        let raster = window.leaf_render_cache.get(&view)?.native.as_ref()?;
        if raster.presented_x.checked_add(raster.width)? > card.pw
            || raster.presented_y.checked_add(raster.height)? > card.ph
        {
            return None;
        }
        let (frame_x, frame_y) = self.frame_origin(wid);
        let x = frame_x
            .saturating_add(i64::from(card.dx))
            .saturating_add(i64::from(raster.presented_x));
        let y = frame_y
            .saturating_add(i64::from(card.dy))
            .saturating_add(i64::from(raster.presented_y));
        let scale = window.scale.max(f64::EPSILON);
        Some(
            accesskit::Affine::translate(accesskit::Vec2::new(x as f64, y as f64))
                * accesskit::Affine::scale(scale),
        )
    }

    /// Compatibility accessor for single/focused-native tests and host seams.
    #[cfg(test)]
    fn native_accessibility_transform(&self, wid: WindowId) -> Option<accesskit::Affine> {
        let (_, view) = self.active_native_view(wid)?;
        self.native_accessibility_transform_for_view(wid, view)
    }

    fn native_accessibility_window_bounds(&self, wid: WindowId) -> Option<accesskit::Rect> {
        let window = self.windows.get(&wid)?;
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let (width, height) = window.win_px.map_or_else(
            || {
                (
                    usize::from(window.cols)
                        .saturating_mul(cw)
                        .saturating_add(pad.saturating_mul(2)),
                    self.native_content_origin_y(wid)
                        .saturating_add(usize::from(window.rows).saturating_mul(ch))
                        .saturating_add(pad),
                )
            },
            |size| (size.width.max(1) as usize, size.height.max(1) as usize),
        );
        Some(accesskit::Rect {
            x0: 0.0,
            y0: 0.0,
            x1: width.max(1) as f64,
            y1: height.max(1) as f64,
        })
    }

    fn clear_staged_native_accessibility(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_a11y_staged = None;
        }
    }

    pub(crate) fn clear_native_accessibility_routes(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_a11y_staged = None;
            ws.native_a11y_published = None;
        }
    }

    /// Route a platform request only through the exact native view/generation whose tree
    /// was published. Any tab switch, detach, generation change, unknown node, or unsupported
    /// action is a silent fail-closed no-op as required by AccessKit's action contract.
    pub(crate) fn on_native_accessibility_action(
        &mut self,
        wid: WindowId,
        request: accesskit::ActionRequest,
    ) {
        let Some(published) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.native_a11y_published.clone())
        else {
            return;
        };
        let Ok((live_owners, _)) = self.visible_native_accessibility_owners(wid) else {
            self.clear_native_accessibility_routes(wid);
            return;
        };
        if published.owners() != live_owners {
            self.clear_native_accessibility_routes(wid);
            return;
        }
        let Some(owner) = published.route_owner(request.target_node) else {
            return;
        };
        let Ok(routed) = route_accessibility_action(&published, &request) else {
            return;
        };

        // Never let a previously staged frame overwrite the post-action state.
        self.clear_staged_native_accessibility(wid);
        let focused = self
            .active_visible_leaf_plan(wid)
            .is_some_and(|plan| plan.focused == owner.view);
        if !focused {
            let moved = self
                .windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.active_mut())
                .is_some_and(|tab| tab.set_focus(owner.view));
            if !moved {
                self.clear_native_accessibility_routes(wid);
                return;
            }
            self.sync_window(wid);
        }
        let event = match routed {
            RoutedAccessibilityAction::Focus { key } => AppEvent::FocusChanged(Some(key)),
            RoutedAccessibilityAction::Activate { action, value, .. } => {
                AppEvent::Action(ActionInvocation { id: action, value })
            }
            RoutedAccessibilityAction::Scroll { lines, .. } => AppEvent::ScrollLines(lines),
            RoutedAccessibilityAction::ReplaceSelectedText { text, .. } => {
                AppEvent::InsertText(text)
            }
            RoutedAccessibilityAction::SetTextSelection {
                anchor_byte,
                focus_byte,
                ..
            } => AppEvent::EditorSetSelection {
                anchor: anchor_byte,
                head: focus_byte,
            },
        };
        if self
            .dispatch_native_view_event(wid, owner.view, event)
            .is_err()
        {
            return;
        }
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
        // Refresh synchronously for screen readers even when the visual frame is coalesced.
        self.push_a11y_tree(wid);
    }
}

#[cfg(test)]
mod tests {
    use accesskit::{Action, ActionRequest, TreeId};
    use aterm_spec::derive::{Model, composite_accessibility_route_model};
    use aterm_spec::interp::{State, admits};

    use super::*;
    use crate::native_accessibility::{
        PublishedNativeAccessibility, compose_native_accessibility, project_native_accessibility,
        project_native_accessibility_for_view_in_container, stable_node_id,
        stable_node_id_for_view,
    };
    use crate::native_app::{AppKind, AppViewState};
    use crate::native_settings::SettingsRoute;
    use crate::native_ui::UiKey;

    #[derive(Clone, Copy)]
    struct RouteProjection {
        owner_one_generation: i64,
        owner_two_generation: i64,
        published_one_generation: i64,
        published_two_generation: i64,
        focus_owner: i64,
        pending: i64,
        target_owner: i64,
        target_generation: i64,
        dispatched_owner: i64,
        dispatched_generation: i64,
        cross_dispatch: i64,
        stale_dispatch: i64,
    }

    impl Default for RouteProjection {
        fn default() -> Self {
            Self {
                owner_one_generation: 1,
                owner_two_generation: 1,
                published_one_generation: 0,
                published_two_generation: 0,
                focus_owner: 2,
                pending: 0,
                target_owner: 0,
                target_generation: 0,
                dispatched_owner: 0,
                dispatched_generation: 0,
                cross_dispatch: 0,
                stale_dispatch: 0,
            }
        }
    }

    fn project_route(model: &Model, projection: RouteProjection) -> State {
        let mut state = model.init_state();
        state.insert("owner_one_generation", projection.owner_one_generation);
        state.insert("owner_two_generation", projection.owner_two_generation);
        state.insert(
            "published_one_generation",
            projection.published_one_generation,
        );
        state.insert(
            "published_two_generation",
            projection.published_two_generation,
        );
        state.insert("focus_owner", projection.focus_owner);
        state.insert("pending", projection.pending);
        state.insert("target_owner", projection.target_owner);
        state.insert("target_generation", projection.target_generation);
        state.insert("dispatched_owner", projection.dispatched_owner);
        state.insert("dispatched_generation", projection.dispatched_generation);
        state.insert("cross_dispatch", projection.cross_dispatch);
        state.insert("stale_dispatch", projection.stale_dispatch);
        state
    }

    fn assert_route_transition(model: &Model, before: &State, after: &State, action: &'static str) {
        assert_eq!(
            model.successors(action, before).as_slice(),
            std::slice::from_ref(after),
            "shipping route transition must conform specifically to {action}"
        );
        assert_eq!(admits(model, before, after), Some(action));
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, after),
                "post-state violates {}::{}: {after:?}",
                model.name,
                invariant.name
            );
        }
    }

    #[test]
    fn presented_snapshot_publishes_its_exact_projection_and_generation() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::About));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        let generation = app
            .native_runtime
            .view_generation(view)
            .expect("live view generation");
        let compiled = app.compiled_native_ui(wid).expect("compiled native UI");
        let focus = compiled
            .focus_order
            .first()
            .cloned()
            .expect("focusable Settings control");
        app.native_runtime
            .view_state_mut(view)
            .expect("live view")
            .common_mut()
            .last_focus = Some(focus.clone());
        assert!(app.prepare_native_input_scratch(wid));
        let compiled = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("retained native frame")
            .compiled
            .clone();
        let transform = app
            .native_accessibility_transform(wid)
            .expect("live window transform");
        let owner = AccessibilityOwner { view, generation };
        let leaf = project_native_accessibility_for_view_in_container(
            &compiled,
            Some(&focus),
            transform,
            owner,
        )
        .expect("expected leaf accessibility projection");
        let leaf_root = leaf.update().tree.as_ref().expect("leaf tree").root;
        let expected = compose_native_accessibility(
            vec![(owner, leaf)],
            Some(view),
            app.native_accessibility_window_bounds(wid)
                .expect("window bounds"),
        )
        .expect("expected accessibility projection")
        .into_update();

        app.stage_native_accessibility(wid, view, &compiled);
        let (actual, published) = app
            .take_native_accessibility_update(wid)
            .expect("active native view")
            .expect("native accessibility update");

        assert_eq!(actual, expected);
        assert_eq!(actual.nodes.len(), compiled.semantics.len() + 1);
        assert_eq!(actual.focus, stable_node_id_for_view(view, &focus));
        let root = actual.tree.as_ref().expect("full tree").root;
        assert_eq!(
            actual
                .nodes
                .iter()
                .find(|(node, _)| *node == leaf_root)
                .expect("leaf root node")
                .1
                .transform(),
            Some(&transform)
        );
        assert_eq!(
            actual
                .nodes
                .iter()
                .find(|(node, _)| *node == root)
                .expect("window root")
                .1
                .role(),
            accesskit::Role::Window
        );
        assert_eq!(published.view, view);
        assert_eq!(published.generation, generation);
        assert!(
            published
                .route(stable_node_id_for_view(view, &focus))
                .is_some()
        );
        assert!(
            app.windows
                .get(&wid)
                .expect("headless window")
                .native_a11y_staged
                .is_none(),
            "a presented projection is consumed exactly once"
        );
    }

    #[test]
    fn editor_publish_retains_visible_text_coordinates_and_canonical_source_bytes() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let dir =
            std::env::temp_dir().join(format!("aterm-native-a11y-editor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unicode.txt");
        std::fs::write(&path, "alpha\naé\tz\nomega\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy());
        app.open_document_tab(AppKind::Editor, &uri).unwrap();

        let (instance, view) = app.active_native_view(wid).expect("native editor view");
        let document = app
            .native_runtime
            .document_id(instance)
            .expect("editor document");
        let snapshot = app.document_store.snapshot(document).unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        let compiled = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .clone();
        app.stage_native_accessibility(wid, view, &compiled);
        let (update, published) = app
            .take_native_accessibility_update(wid)
            .expect("native update")
            .expect("valid update");

        let target = published
            .virtual_text()
            .first()
            .expect("published virtual editor text");
        assert!(!target.lines.is_empty());
        assert_eq!(
            target.visible_range,
            target.lines.first().unwrap().source.start..target.lines.last().unwrap().source.end
        );
        for line in &target.lines {
            let source = usize::try_from(line.source.start).unwrap()
                ..usize::try_from(line.source.end).unwrap();
            assert_eq!(&snapshot.text[source], line.text);
            assert_eq!(
                line.character_positions.len() + 1,
                line.character_source_offsets.len()
            );
            assert_eq!(line.character_positions.len(), line.character_widths.len());
            assert!(update.nodes.iter().any(|(node, access)| {
                *node == line.node
                    && access.role() == accesskit::Role::TextRun
                    && access.value() == Some(line.text.as_str())
            }));
        }
        assert_eq!(
            target.primary_selection,
            Some(crate::native_accessibility::VirtualTextSelection {
                anchor_byte: 0,
                focus_byte: 0,
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn platform_focus_routes_to_exact_live_view_and_rejects_stale_generation() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::About));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        let generation = app
            .native_runtime
            .view_generation(view)
            .expect("live view generation");
        let compiled = app.compiled_native_ui(wid).expect("compiled native UI");
        let key = compiled
            .focus_order
            .first()
            .cloned()
            .expect("focusable Settings control");
        let node = stable_node_id(&key);
        let (_, routes) = project_native_accessibility(&compiled, None)
            .expect("native accessibility projection")
            .into_update_and_routes();
        app.windows
            .get_mut(&wid)
            .expect("headless window")
            .native_a11y_published =
            Some(PublishedNativeAccessibility::new(view, generation, routes));
        app.native_runtime
            .view_state_mut(view)
            .expect("live view")
            .common_mut()
            .last_focus = None;
        let request = ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: node,
            data: None,
        };

        app.on_native_accessibility_action(wid, request.clone());
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .expect("live view")
                .common()
                .last_focus,
            Some(key.clone())
        );

        let (_, routes) = project_native_accessibility(&compiled, None)
            .expect("native accessibility projection")
            .into_update_and_routes();
        app.windows
            .get_mut(&wid)
            .expect("headless window")
            .native_a11y_published = Some(PublishedNativeAccessibility::new(
            view,
            generation + 1,
            routes,
        ));
        app.native_runtime
            .view_state_mut(view)
            .expect("live view")
            .common_mut()
            .last_focus = None;

        app.on_native_accessibility_action(wid, request);
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .expect("live view")
                .common()
                .last_focus,
            None,
            "a route stamped for another lifecycle must not dispatch"
        );
        assert!(
            app.windows
                .get(&wid)
                .expect("headless window")
                .native_a11y_published
                .is_none(),
            "stale routes are discarded fail-closed"
        );
    }

    #[test]
    fn platform_click_dispatches_the_published_key_action_pair() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = 140;
            window.rows = 50;
        }
        assert!(app.open_settings_tab(SettingsRoute::About));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        let generation = app
            .native_runtime
            .view_generation(view)
            .expect("live view generation");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(settings)) if settings.route == SettingsRoute::About
        ));

        let compiled = app.compiled_native_ui(wid).expect("compiled native UI");
        let key = UiKey::new("settings/nav/appearance");
        let semantic = compiled.semantic(&key).expect("Appearance navigation node");
        assert_eq!(
            semantic.action.as_ref().map(|action| action.as_str()),
            Some("settings/route/appearance")
        );
        let projection =
            project_native_accessibility(&compiled, None).expect("native accessibility projection");
        let node = projection
            .id_for_key(&key)
            .expect("stable Appearance node id");
        let (_, routes) = projection.into_update_and_routes();
        app.windows
            .get_mut(&wid)
            .expect("headless window")
            .native_a11y_published =
            Some(PublishedNativeAccessibility::new(view, generation, routes));

        app.on_native_accessibility_action(
            wid,
            ActionRequest {
                action: Action::Click,
                target_tree: TreeId::ROOT,
                target_node: node,
                data: None,
            },
        );

        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(AppViewState::Settings(settings))
                if settings.route == SettingsRoute::Appearance
        ));
    }

    fn assert_native_sibling_geometry(scale: f64, axis: crate::tab_model::SplitAxis) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The win_pad negative control below is provable only with a REAL
        // compositor pad: seal the scale-derived boot pad into the headless
        // record exactly like an attach would (never-attached records carry 0).
        app.backend.set_pad((12.0 * scale).round() as usize);
        app.seed_headless_boot_metrics();
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.cols = 140;
            window.rows = 48;
            window.scale = scale;
        }
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let (instance, first) = app.active_native_view(wid).expect("first Settings view");
        let second = app
            .split_active_with_native(
                wid,
                axis,
                instance,
                AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                    &app.config,
                ))),
            )
            .expect("second Settings view");
        assert_eq!(app.active_native_view(wid), Some((instance, second)));
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());

        let (frame_x, frame_y) = app.frame_origin(wid);
        let (card_x, card_y) = {
            let card = app.windows[&wid]
                .settings_card
                .as_ref()
                .expect("presented composite card");
            (i64::from(card.dx), i64::from(card.dy))
        };
        let root_keys = [first, second].map(|view| {
            app.windows[&wid].leaf_render_cache[&view]
                .native
                .as_ref()
                .expect("retained native raster")
                .compiled
                .semantics
                .iter()
                .find(|semantic| semantic.parent.is_none())
                .expect("semantic root")
                .key
                .clone()
        });
        let focus_key = app.windows[&wid].leaf_render_cache[&first]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .focus_order
            .first()
            .cloned()
            .expect("focusable first Settings control");
        let (update, published) = app
            .take_native_accessibility_update(wid)
            .expect("visible native siblings")
            .expect("valid composite tree");
        assert_eq!(published.owners().len(), 2);
        let window_root = update.tree.as_ref().expect("window tree").root;
        let window_node = update
            .nodes
            .iter()
            .find(|(id, _)| *id == window_root)
            .expect("window root");
        let roots = [
            stable_node_id_for_view(first, &root_keys[0]),
            stable_node_id_for_view(second, &root_keys[1]),
        ];
        assert_ne!(
            roots[0], roots[1],
            "identical sibling keys are view-qualified"
        );
        assert_eq!(window_node.1.children(), roots);

        for (view, root) in [first, second].into_iter().zip(roots) {
            let raster = app.windows[&wid].leaf_render_cache[&view]
                .native
                .as_ref()
                .expect("retained presented leaf");
            let x = frame_x + card_x + i64::from(raster.presented_x);
            let y = frame_y + card_y + i64::from(raster.presented_y);
            let expected = accesskit::Affine::translate(accesskit::Vec2::new(x as f64, y as f64))
                * accesskit::Affine::scale(scale);
            assert_eq!(
                update
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == root)
                    .expect("native leaf root")
                    .1
                    .transform(),
                Some(&expected),
                "AccessKit geometry must equal the retained raster placement at {scale}x"
            );
            let owner = published.route_owner(root).expect("root route owner");
            assert_eq!(owner.view, view);
            assert_eq!(
                owner.generation,
                app.native_runtime.view_generation(view).unwrap()
            );
            if view == second {
                let omitted_pad = accesskit::Affine::translate(accesskit::Vec2::new(
                    (frame_x + i64::from(raster.presented_x)) as f64,
                    y as f64,
                )) * accesskit::Affine::scale(scale);
                assert_ne!(
                    expected, omitted_pad,
                    "omitting the compositor card's win_pad destination must fail at {scale}x"
                );
                match axis {
                    crate::tab_model::SplitAxis::Horizontal => {
                        assert!(raster.presented_x > 0);
                        let omitted_leaf = accesskit::Affine::translate(accesskit::Vec2::new(
                            (frame_x + card_x) as f64,
                            y as f64,
                        )) * accesskit::Affine::scale(scale);
                        assert_ne!(
                            expected, omitted_leaf,
                            "omitting the retained leaf x destination must fail at {scale}x"
                        );
                    }
                    crate::tab_model::SplitAxis::Vertical => {
                        assert!(raster.presented_y > 0);
                        let omitted_leaf = accesskit::Affine::translate(accesskit::Vec2::new(
                            x as f64,
                            (frame_y + card_y) as f64,
                        )) * accesskit::Affine::scale(scale);
                        assert_ne!(
                            expected, omitted_leaf,
                            "omitting the retained leaf y destination must fail at {scale}x"
                        );
                    }
                }
            }
        }
        assert_eq!(
            update.focus,
            stable_node_id_for_view(second, &root_keys[1]),
            "the focused native sibling owns AccessKit focus"
        );

        if scale == 1.0 {
            let target = stable_node_id_for_view(first, &focus_key);
            app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
            app.on_native_accessibility_action(
                wid,
                ActionRequest {
                    action: Action::Focus,
                    target_tree: TreeId::ROOT,
                    target_node: target,
                    data: None,
                },
            );
            assert_eq!(
                app.active_visible_leaf_plan(wid).unwrap().focused,
                first,
                "an action on a visible sibling focuses that exact pane"
            );
            assert_eq!(
                app.native_runtime
                    .view_state(first)
                    .unwrap()
                    .common()
                    .last_focus,
                Some(focus_key)
            );
        }
    }

    #[test]
    fn native_native_composite_matches_presented_geometry_at_one_and_two_x() {
        for scale in [1.0, 2.0] {
            assert_native_sibling_geometry(scale, crate::tab_model::SplitAxis::Horizontal);
            assert_native_sibling_geometry(scale, crate::tab_model::SplitAxis::Vertical);
        }
    }

    #[test]
    fn retained_pointer_hit_is_view_exact_and_rejects_staged_stale_or_torn_artifacts() {
        for scale in [1.0, 2.0] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            {
                let window = app.windows.get_mut(&wid).unwrap();
                window.cols = 140;
                window.rows = 48;
                window.scale = scale;
            }
            assert!(app.open_settings_tab(SettingsRoute::Home));
            let (instance, first) = app.active_native_view(wid).unwrap();
            let second = app
                .split_active_with_native(
                    wid,
                    crate::tab_model::SplitAxis::Horizontal,
                    instance,
                    AppViewState::Settings(Box::new(
                        crate::native_settings::SettingsViewState::new(&app.config),
                    )),
                )
                .unwrap();
            assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, second);
            assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
            assert!(
                app.retained_native_leaf_artifact(wid, first, true)
                    .is_none(),
                "a merely staged raster must not accept pointer input"
            );
            for cache in app
                .windows
                .get_mut(&wid)
                .unwrap()
                .leaf_render_cache
                .values_mut()
            {
                if let Some(raster) = cache.native.as_mut() {
                    raster.presented = true;
                }
            }

            let (raw_x, raw_y, expected_key) = {
                let artifact = app
                    .retained_native_leaf_artifact(wid, first, true)
                    .expect("exact first sibling artifact");
                let key = artifact
                    .compiled
                    .focus_order
                    .first()
                    .cloned()
                    .expect("focusable control");
                let rect = artifact.compiled.semantic(&key).unwrap().rect;
                (
                    artifact.device_x as f64 + f64::from(rect.x + rect.width * 0.5) * scale,
                    artifact.device_y as f64 + f64::from(rect.y + rect.height * 0.5) * scale,
                    key,
                )
            };
            let (artifact, local_x, local_y) = app
                .retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                .expect("presented first sibling hit");
            assert_eq!(
                artifact.view, first,
                "pointer ownership comes from the retained destination, not focused sibling"
            );
            assert_eq!(
                artifact.compiled.hit_test(local_x, local_y).unwrap().key,
                expected_key
            );
            assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, second);

            let original_stamp = app.windows[&wid].leaf_render_cache[&first]
                .native
                .as_ref()
                .unwrap()
                .stamp;
            app.windows
                .get_mut(&wid)
                .unwrap()
                .leaf_render_cache
                .get_mut(&first)
                .unwrap()
                .native
                .as_mut()
                .unwrap()
                .stamp
                .generation = original_stamp.generation.saturating_add(1);
            assert!(
                app.retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                    .is_none(),
                "a stale lifecycle generation fails closed"
            );
            app.windows
                .get_mut(&wid)
                .unwrap()
                .leaf_render_cache
                .get_mut(&first)
                .unwrap()
                .native
                .as_mut()
                .unwrap()
                .stamp = original_stamp;
            app.windows
                .get_mut(&wid)
                .unwrap()
                .leaf_render_cache
                .get_mut(&first)
                .unwrap()
                .native
                .as_mut()
                .unwrap()
                .stamp
                .geometry ^= 1;
            assert!(
                app.retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                    .is_none(),
                "a stale geometry stamp fails closed"
            );
            let card_width = app.windows[&wid].settings_card.as_ref().unwrap().pw;
            {
                let raster = app
                    .windows
                    .get_mut(&wid)
                    .unwrap()
                    .leaf_render_cache
                    .get_mut(&first)
                    .unwrap()
                    .native
                    .as_mut()
                    .unwrap();
                raster.stamp = original_stamp;
                raster.presented_x = card_width;
            }
            assert!(
                app.retained_native_leaf_at_pointer(wid, raw_x, raw_y)
                    .is_none(),
                "a torn card/leaf placement fails closed"
            );
        }
    }

    #[test]
    fn retained_native_pointer_and_accessibility_survive_a_signed_surface_crop() {
        use winit::dpi::PhysicalSize;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("native Settings view");
        assert!(app.prepare_native_input_scratch(wid));
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.leaf_render_cache.get_mut(&view))
            .and_then(|cache| cache.native.as_mut())
            .expect("retained native raster")
            .presented = true;

        let (rows, cols) = {
            let window = &app.windows[&wid];
            (
                u16::try_from(window.input_scratch.cells.len()).unwrap(),
                window.cols,
            )
        };
        let full = app.frame_px(rows, cols);
        app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
            full.width.saturating_sub(8).max(1),
            full.height.saturating_sub(8).max(1),
        ));
        let origin = app.frame_origin(wid);
        assert!(origin.0 < 0 && origin.1 < 0, "fixture is a centred crop");

        let artifact = app
            .retained_native_leaf_artifact(wid, view, true)
            .expect("a partially visible retained leaf remains authoritative");
        assert!(
            artifact.device_x < 0 || artifact.device_y < 0,
            "retained destination preserves signed window coordinates"
        );
        let (hit, local_x, local_y) = app
            .retained_native_leaf_at_pointer(wid, 0.5, 0.5)
            .expect("visible intersection routes pointer input");
        assert_eq!(hit.view, view);
        assert!(local_x >= 0.0 && local_y >= 0.0);

        let transform = app
            .native_accessibility_transform(wid)
            .expect("partially visible retained scene remains publishable");
        let expected = accesskit::Affine::translate(accesskit::Vec2::new(
            artifact.device_x as f64,
            artifact.device_y as f64,
        )) * accesskit::Affine::scale(artifact.scale);
        assert_eq!(transform, expected);
    }

    #[test]
    fn terminal_focus_keeps_visible_native_sibling_in_the_composite_tree() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = 120;
            window.rows = 42;
        }
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("Settings view");
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert_eq!(app.active_native_view(wid), None);
        assert_eq!(
            app.active_visible_leaf_plan(wid).unwrap().focused,
            terminal_view
        );
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let root_key = app.windows[&wid].leaf_render_cache[&native_view]
            .native
            .as_ref()
            .expect("retained native sibling")
            .compiled
            .semantics
            .iter()
            .find(|semantic| semantic.parent.is_none())
            .unwrap()
            .key
            .clone();
        let (update, published) = app
            .take_native_accessibility_update(wid)
            .expect("visible native sibling despite terminal focus")
            .expect("valid mixed tree");
        let window_root = update.tree.as_ref().unwrap().root;
        let native_root = stable_node_id_for_view(native_view, &root_key);
        assert_eq!(
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == window_root)
                .unwrap()
                .1
                .children(),
            [native_root]
        );
        assert_eq!(
            update.focus, window_root,
            "terminal focus is represented by the host root, never a hidden native control"
        );
        assert_eq!(
            published.route_owner(native_root).unwrap().view,
            native_view
        );
    }

    #[test]
    fn composite_route_conforms_and_rejects_wrong_owner_and_stale_generation() {
        let model = composite_accessibility_route_model();
        let initial_projection = RouteProjection::default();
        let initial = project_route(&model, initial_projection);
        assert_eq!(initial, model.init_state());

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let (instance, first) = app.active_native_view(wid).unwrap();
        let second = app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                instance,
                AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                    &app.config,
                ))),
            )
            .unwrap();
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let first_key = app.windows[&wid].leaf_render_cache[&first]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .focus_order
            .first()
            .cloned()
            .expect("first view focus route");
        let first_node = stable_node_id_for_view(first, &first_key);
        let (_, published) = app.take_native_accessibility_update(wid).unwrap().unwrap();
        let first_generation = app.native_runtime.view_generation(first).unwrap();
        let second_generation = app.native_runtime.view_generation(second).unwrap();
        assert_eq!(
            published.route_owner(first_node),
            Some(AccessibilityOwner {
                view: first,
                generation: first_generation,
            })
        );
        let mut published_projection = initial_projection;
        published_projection.published_one_generation = 1;
        published_projection.published_two_generation = 1;
        let published_state = project_route(&model, published_projection);
        assert_route_transition(&model, &initial, &published_state, "Publish");

        let mut requested_projection = published_projection;
        requested_projection.pending = 1;
        requested_projection.target_owner = 1;
        requested_projection.target_generation = 1;
        let requested_state = project_route(&model, requested_projection);
        assert_route_transition(&model, &published_state, &requested_state, "RequestOne");

        // Genuine wrong-owner negative: corrupt only the sibling owner stamp.
        // The shipping route guard must notice that the view-qualified NodeId no
        // longer agrees with its route owner and perform no reducer dispatch.
        let mut wrong_owner = published.clone();
        assert!(wrong_owner.retag_route_for_test(
            first_node,
            AccessibilityOwner {
                view: second,
                generation: second_generation,
            },
        ));
        app.native_runtime
            .view_state_mut(first)
            .unwrap()
            .common_mut()
            .last_focus = None;
        app.native_runtime
            .view_state_mut(second)
            .unwrap()
            .common_mut()
            .last_focus = None;
        app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(wrong_owner);
        app.on_native_accessibility_action(
            wid,
            ActionRequest {
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node: first_node,
                data: None,
            },
        );
        assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, second);
        assert!(
            app.native_runtime
                .view_state(first)
                .unwrap()
                .common()
                .last_focus
                .is_none()
        );
        assert!(
            app.native_runtime
                .view_state(second)
                .unwrap()
                .common()
                .last_focus
                .is_none()
        );
        let mut cross_view_mutant = requested_projection;
        cross_view_mutant.pending = 0;
        cross_view_mutant.dispatched_owner = 2;
        cross_view_mutant.dispatched_generation = 1;
        cross_view_mutant.cross_dispatch = 1;
        let cross_view_mutant = project_route(&model, cross_view_mutant);
        assert_eq!(admits(&model, &requested_state, &cross_view_mutant), None);
        assert!(!model.check_invariant("NoCrossViewDispatch", &cross_view_mutant));

        // Genuine positive: the same request through the unmodified composite
        // focuses and dispatches only to the first sibling.
        app.windows.get_mut(&wid).unwrap().native_a11y_published = Some(published);
        app.on_native_accessibility_action(
            wid,
            ActionRequest {
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node: first_node,
                data: None,
            },
        );
        assert_eq!(app.active_visible_leaf_plan(wid).unwrap().focused, first);
        assert_eq!(
            app.native_runtime
                .view_state(first)
                .unwrap()
                .common()
                .last_focus,
            Some(first_key)
        );
        assert!(
            app.native_runtime
                .view_state(second)
                .unwrap()
                .common()
                .last_focus
                .is_none()
        );
        let mut routed_projection = requested_projection;
        routed_projection.pending = 0;
        routed_projection.focus_owner = 1;
        routed_projection.dispatched_owner = 1;
        routed_projection.dispatched_generation = 1;
        let routed_state = project_route(&model, routed_projection);
        assert_route_transition(&model, &requested_state, &routed_state, "Route");

        // Fresh split for a real stale-generation rejection. The published
        // composite is stamped one generation behind its still-live second
        // sibling; the shipping all-owner guard clears it before dispatch.
        let mut stale_app = App::headless_for_test();
        assert!(stale_app.open_settings_tab(SettingsRoute::Home));
        let (stale_instance, stale_first) = stale_app.active_native_view(wid).unwrap();
        let stale_second = stale_app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                stale_instance,
                AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                    &stale_app.config,
                ))),
            )
            .unwrap();
        assert!(stale_app.prepare_heterogeneous_input_scratch(wid).is_some());
        let (live_owners, focused_native) =
            stale_app.visible_native_accessibility_owners(wid).unwrap();
        let mut stale_owners = live_owners.clone();
        let stale_second_owner = stale_owners
            .iter_mut()
            .find(|owner| owner.view == stale_second)
            .unwrap();
        assert!(stale_second_owner.generation > 0);
        stale_second_owner.generation -= 1;
        let stale_second_owner = *stale_second_owner;
        let mut projections = Vec::new();
        for owner in &stale_owners {
            let compiled = stale_app.windows[&wid].leaf_render_cache[&owner.view]
                .native
                .as_ref()
                .unwrap()
                .compiled
                .clone();
            let focus = stale_app
                .native_runtime
                .view_state(owner.view)
                .and_then(|state| state.common().last_focus.as_ref());
            let transform = stale_app
                .native_accessibility_transform_for_view(wid, owner.view)
                .unwrap();
            projections.push((
                *owner,
                project_native_accessibility_for_view_in_container(
                    &compiled, focus, transform, *owner,
                )
                .unwrap(),
            ));
        }
        let projection = compose_native_accessibility(
            projections,
            focused_native,
            stale_app.native_accessibility_window_bounds(wid).unwrap(),
        )
        .unwrap();
        let (_, routes, virtual_text) = projection.into_update_routes_and_virtual_text();
        let stale_published = PublishedNativeAccessibility::composite(
            stale_second_owner,
            stale_owners,
            routes,
            virtual_text,
        );
        let stale_key = stale_app.windows[&wid].leaf_render_cache[&stale_second]
            .native
            .as_ref()
            .unwrap()
            .compiled
            .focus_order
            .first()
            .cloned()
            .unwrap();
        let stale_node = stable_node_id_for_view(stale_second, &stale_key);
        stale_app
            .native_runtime
            .view_state_mut(stale_second)
            .unwrap()
            .common_mut()
            .last_focus = None;
        stale_app
            .windows
            .get_mut(&wid)
            .unwrap()
            .native_a11y_published = Some(stale_published);
        stale_app.on_native_accessibility_action(
            wid,
            ActionRequest {
                action: Action::Focus,
                target_tree: TreeId::ROOT,
                target_node: stale_node,
                data: None,
            },
        );
        assert!(
            stale_app
                .native_runtime
                .view_state(stale_second)
                .unwrap()
                .common()
                .last_focus
                .is_none(),
            "a delayed lifecycle route must never dispatch"
        );
        assert!(
            stale_app.windows[&wid].native_a11y_published.is_none(),
            "the stale composite is discarded fail-closed"
        );

        let mut advanced_projection = published_projection;
        advanced_projection.owner_two_generation = 2;
        let advanced_state = project_route(&model, advanced_projection);
        assert_route_transition(&model, &published_state, &advanced_state, "AdvanceTwo");
        let mut stale_request_projection = advanced_projection;
        stale_request_projection.pending = 1;
        stale_request_projection.target_owner = 2;
        stale_request_projection.target_generation = 1;
        let stale_request_state = project_route(&model, stale_request_projection);
        assert_route_transition(&model, &advanced_state, &stale_request_state, "RequestTwo");
        let mut rejected_projection = stale_request_projection;
        rejected_projection.pending = 0;
        let rejected_state = project_route(&model, rejected_projection);
        assert_route_transition(&model, &stale_request_state, &rejected_state, "RejectStale");
        let mut stale_mutant = stale_request_projection;
        stale_mutant.pending = 0;
        stale_mutant.dispatched_owner = 2;
        stale_mutant.dispatched_generation = 1;
        stale_mutant.stale_dispatch = 1;
        let stale_mutant = project_route(&model, stale_mutant);
        assert_eq!(admits(&model, &stale_request_state, &stale_mutant), None);
        assert!(!model.check_invariant("NoStaleGenerationDispatch", &stale_mutant));
        assert!(live_owners.iter().any(|owner| owner.view == stale_first));
    }
}
