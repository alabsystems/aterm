// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shipping accessibility seam (`aterm_a11y::model`) for native tab applications.

use crate::native_accessibility::{
    AccessibilityOwner, NativeAccessibilityProjection, PublishedNativeAccessibility,
    RoutedAccessibilityAction, StagedNativeAccessibility, compose_native_accessibility,
    project_native_accessibility_for_view_in_container, route_accessibility_action,
};
use crate::native_app::{ActionInvocation, AppEvent};
use crate::tab_model::{View, ViewId};
use crate::{App, WindowId};
#[cfg(a11y_tree)]
use std::time::Instant;

/// How often a window that cannot present tells a client. No frame paces it, and a program's output
/// would otherwise rebuild the tree on each wake, a few milliseconds apart. A person hears nothing
/// faster than this.
#[cfg(a11y_tree)]
const PARKED_TELL_PACE: std::time::Duration = std::time::Duration::from_millis(250);

/// A view's compile, and where it sits on its window.
type NativeAccessibilitySource<'a> = (
    std::borrow::Cow<'a, crate::native_ui::CompiledUi>,
    aterm_a11y::model::Affine,
);

impl App {
    /// Stage accessibility from the same compiled semantic snapshot paint is about to lower.
    pub(crate) fn stage_native_accessibility(
        &mut self,
        wid: WindowId,
        view: ViewId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        if self
            .windows
            .get(&wid)
            .is_none_or(|window| !window.a11y_active)
        {
            self.clear_staged_native_accessibility(wid);
            return;
        }
        let Ok((owners, focused_native)) = self.visible_native_accessibility_owners_from_plan(plan)
        else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        if owners.len() != 1 || owners[0].view != view {
            self.clear_staged_native_accessibility(wid);
            return;
        }
        let Ok(projection) = self.retained_native_accessibility_projection(
            wid,
            &owners,
            focused_native,
            false,
            plan,
        ) else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        let staged = StagedNativeAccessibility::new(owners, projection);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.native_a11y_staged = Some(staged);
        }
    }

    /// Stage one composite accessibility projection for all visible native leaves.
    pub(crate) fn stage_visible_native_accessibility(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        if self
            .windows
            .get(&wid)
            .is_none_or(|window| !window.a11y_active)
        {
            self.clear_staged_native_accessibility(wid);
            return;
        }
        let Ok((owners, focused_native)) = self.visible_native_accessibility_owners_from_plan(plan)
        else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        if owners.is_empty() {
            self.clear_staged_native_accessibility(wid);
            return;
        }
        let Ok(projection) = self.retained_native_accessibility_projection(
            wid,
            &owners,
            focused_native,
            false,
            plan,
        ) else {
            self.clear_staged_native_accessibility(wid);
            return;
        };
        let staged = StagedNativeAccessibility::new(owners, projection);
        if let Some(window) = self.windows.get_mut(&wid) {
            window.native_a11y_staged = Some(staged);
        }
    }

    /// Send a client what a window that cannot present owes it, at most once per
    /// [`PARKED_TELL_PACE`]; a question that comes sooner falls due at the pace instead, so a
    /// stream's last bytes are told.
    ///
    /// No frame will publish here. A tree the failed frame staged would wait for a present that is
    /// not coming, and a page that moved by itself, under a file reloaded beneath it say, would
    /// leave the client's tree behind it: its next selection would land at old offsets in the new
    /// text. With neither there is nothing to send, since a raster that holds the model says what
    /// the client already read, shown or not. A window that reads differently from what its client
    /// holds tells it the same way. An overlay's tree does not follow the page.
    #[cfg(a11y_tree)]
    pub(crate) fn publish_in_a_parked_window(&mut self, wid: WindowId, now: Instant) {
        let Some(window) = self.windows.get_mut(&wid) else {
            return;
        };
        if window.overlay.is_some() || !window.a11y_active {
            return;
        }
        if let Some(next) = window.a11y_parked_next.filter(|next| now < *next) {
            window.a11y_parked_due = Some(next);
            return;
        }
        let next = now + PARKED_TELL_PACE;
        window.a11y_parked_next = Some(next);
        window.a11y_parked_due = None;
        let staged = window.native_a11y_staged.is_some();
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return;
        };
        if self.waits_out_an_update(wid, &plan, next) {
            return;
        }
        let moved = plan.leaves.iter().any(|leaf| {
            matches!(self.view_store.get(leaf.view), Some(View::Native(_)))
                && self
                    .retained_native_leaf_artifact_from_plan(wid, leaf.view, false, &plan)
                    .is_none()
        });
        if staged || moved || self.reads_differ(wid, &plan) {
            self.push_a11y_tree_with_plan(wid, Some(&plan));
        }
    }

    /// Whether window `wid` holds its tell for a synchronized update, as a frame waits one out:
    /// until `next`, and as the screen stands after that.
    #[cfg(a11y_tree)]
    fn waits_out_an_update(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        next: Instant,
    ) -> bool {
        let open_update = plan.leaves.iter().any(|leaf| {
            self.view_store
                .get(leaf.view)
                .copied()
                .and_then(View::terminal_session)
                .and_then(|session| self.pool.shown(session))
                .is_some_and(|(term, owner)| {
                    let site = crate::metrics::TermWaitSite::Redraw;
                    crate::term_lock_ui(&term, &owner.ctx.ui_waiting, site)
                        .modes()
                        .synchronized_output()
                })
        });
        let Some(window) = self.windows.get_mut(&wid) else {
            return true;
        };
        let waits = open_update && !window.a11y_parked_held;
        window.a11y_parked_held = waits;
        if waits {
            window.a11y_parked_due = Some(next);
        }
        waits
    }

    /// Whether window `wid` reads differently from what its client holds.
    #[cfg(a11y_tree)]
    fn reads_differ(&self, wid: WindowId, plan: &crate::tab_model::VisibleLeafPlan) -> bool {
        let reads = self.client_reads(wid, Some(plan));
        self.windows
            .get(&wid)
            .is_some_and(|window| window.a11y_reads != reads)
    }

    /// Runs each parked window's read that its pace held back and that has fallen due.
    #[cfg(a11y_tree)]
    pub(crate) fn serve_parked_tells(&mut self, now: Instant) {
        let due: Vec<WindowId> = self
            .windows
            .iter_mut()
            .filter(|(_, window)| window.a11y_parked_due.is_some_and(|at| at <= now))
            .map(|(wid, window)| {
                window.a11y_parked_due = None;
                *wid
            })
            .collect();
        for wid in due {
            if self
                .windows
                .get(&wid)
                .is_some_and(|window| window.present_retry.parked)
            {
                self.publish_in_a_parked_window(wid, now);
            }
        }
    }

    /// Produce the native accessibility tree for a publish.
    pub(crate) fn take_native_accessibility_update(
        &mut self,
        wid: WindowId,
    ) -> Option<Result<(aterm_a11y::model::TreeUpdate, PublishedNativeAccessibility), String>> {
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return Some(Err(
                "native accessibility window is no longer live".to_string()
            ));
        };
        self.take_native_accessibility_update_from_plan(wid, &plan)
    }

    pub(crate) fn take_native_accessibility_update_from_plan(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<Result<(aterm_a11y::model::TreeUpdate, PublishedNativeAccessibility), String>> {
        let (owners, focused_native) =
            match self.visible_native_accessibility_owners_from_plan(plan) {
                Ok((owners, focused_native)) if !owners.is_empty() => (owners, focused_native),
                Ok(_) => return None,
                Err(error) => return Some(Err(error)),
            };
        // A frame whose present failed leaves its tree here, and the model can move past it.
        let staged = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.native_a11y_staged.take())
            .filter(|staged| {
                self.windows
                    .get(&wid)
                    .is_some_and(|ws| staged.matches(&owners, &ws.current_title))
                    && owners.iter().all(|owner| {
                        self.retained_native_leaf_artifact_from_plan(wid, owner.view, false, plan)
                            .is_some()
                    })
            });

        let projection = match staged {
            Some(staged) => staged.projection,
            None => {
                let read = self.native_accessibility_projection_from(
                    wid,
                    &owners,
                    focused_native,
                    |owner| {
                        self.retained_native_accessibility_source(wid, owner, true, plan)
                            .or_else(|_| self.live_native_accessibility_source(wid, owner, plan))
                    },
                );
                match read {
                    Ok(projection) => projection,
                    Err(error) => return Some(Err(error)),
                }
            }
        };
        let (update, routes, virtual_text) = projection.into_update_routes_and_virtual_text();
        #[cfg(a11y_tree)]
        let update = match self
            .with_mixed_terminal_panes(wid, plan, update)
            .and_then(|update| self.with_window_chrome(wid, plan, update))
        {
            Ok(update) => update,
            Err(error) => return Some(Err(error)),
        };
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
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Result<NativeAccessibilityProjection, String> {
        self.native_accessibility_projection_from(wid, owners, focused_native, |owner| {
            self.retained_native_accessibility_source(wid, owner, require_presented, plan)
        })
    }

    /// `owner`'s compile and place as its last frame left them.
    fn retained_native_accessibility_source(
        &self,
        wid: WindowId,
        owner: &AccessibilityOwner,
        require_presented: bool,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Result<NativeAccessibilitySource<'_>, String> {
        let artifact = self
            .retained_native_leaf_artifact_from_plan(wid, owner.view, require_presented, plan)
            .ok_or_else(|| "native accessibility retained leaf is stale".to_string())?;
        if artifact.generation != owner.generation {
            return Err("native accessibility retained leaf is stale".to_string());
        }
        let transform = self
            .retained_native_accessibility_transform_for_view(wid, owner.view)
            .ok_or_else(|| "native accessibility retained placement is invalid".to_string())?;
        Ok((std::borrow::Cow::Borrowed(artifact.compiled), transform))
    }

    /// `owner`'s compile and place as the next frame of `plan` will have them.
    ///
    /// No frame answers for a page that owes a paint, was released while hidden, or has just
    /// opened, but a client reads the model, not the glass: its tree must offer what a press can
    /// reach now. So this compiles at the viewport dispatch compiles at and places the page where a
    /// frame will, and reads nothing a paint left, which leaves paint and damage as they are. Until
    /// that frame lands the tree runs ahead of the glass.
    fn live_native_accessibility_source(
        &self,
        wid: WindowId,
        owner: &AccessibilityOwner,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Result<NativeAccessibilitySource<'_>, String> {
        let Some(View::Native(native)) = self.view_store.get(owner.view).copied() else {
            return Err("native accessibility view is gone".to_string());
        };
        let viewport = self.native_ui_viewport_in_plan(wid, owner.view, plan)?;
        let compiled = self.compiled_native_ui_for(wid, native.instance, owner.view, viewport)?;
        let transform = self
            .planned_native_accessibility_transform(wid, owner.view, plan)
            .ok_or_else(|| "native accessibility view is not in its plan".to_string())?;
        Ok((std::borrow::Cow::Owned(compiled), transform))
    }

    fn native_accessibility_projection_from<'a>(
        &'a self,
        wid: WindowId,
        owners: &[AccessibilityOwner],
        focused_native: Option<ViewId>,
        source: impl Fn(&AccessibilityOwner) -> Result<NativeAccessibilitySource<'a>, String>,
    ) -> Result<NativeAccessibilityProjection, String> {
        let mut projections = Vec::with_capacity(owners.len());
        for owner in owners {
            let (compiled, transform) = source(owner)?;
            let focus = self
                .native_runtime
                .view_state(owner.view)
                .and_then(|state| state.common().last_focus.as_ref());
            let projection = project_native_accessibility_for_view_in_container(
                &compiled, focus, transform, *owner,
            )
            .map_err(|error| format!("native accessibility projection failed: {error:?}"))?;
            projections.push((*owner, projection));
        }
        let bounds = self
            .native_accessibility_window_bounds(wid)
            .ok_or_else(|| "native accessibility window is no longer live".to_string())?;
        let title = self
            .windows
            .get(&wid)
            .map(|ws| ws.current_title.as_str())
            .unwrap_or_default();
        compose_native_accessibility(projections, focused_native, bounds, title)
            .map_err(|error| format!("native accessibility composition failed: {error:?}"))
    }

    fn visible_native_accessibility_owners(
        &self,
        wid: WindowId,
    ) -> Result<(Vec<AccessibilityOwner>, Option<ViewId>), String> {
        let ws = self
            .windows
            .get(&wid)
            .ok_or_else(|| "native accessibility window is no longer live".to_string())?;
        let tab = ws
            .tab_set
            .active()
            .ok_or_else(|| "native accessibility window is no longer live".to_string())?;
        let mut owners = Vec::new();
        let mut focused_native = None;
        let mut error = None;
        let _ = crate::active_tab_shows(ws, |view| {
            if !matches!(self.view_store.get(view), Some(View::Native(_))) {
                return false;
            }
            let Some(generation) = self.native_runtime.view_generation(view) else {
                error = Some("native accessibility view is no longer live".to_string());
                return true;
            };
            let Some(review_revision) = self
                .native_runtime
                .view_state(view)
                .map(|state| state.common().review_revision)
            else {
                error = Some("native accessibility view is no longer live".to_string());
                return true;
            };
            owners.push(AccessibilityOwner {
                view,
                generation,
                review_revision,
            });
            if view == tab.focus {
                focused_native = Some(view);
            }
            false
        });
        if let Some(error) = error {
            return Err(error);
        }
        Ok((owners, focused_native))
    }

    fn visible_native_accessibility_owners_from_plan(
        &self,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Result<(Vec<AccessibilityOwner>, Option<ViewId>), String> {
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
                review_revision: self
                    .native_runtime
                    .view_state(leaf.view)
                    .ok_or_else(|| "native accessibility view is no longer live".to_string())?
                    .common()
                    .review_revision,
            });
            if leaf.focused {
                focused_native = Some(leaf.view);
            }
        }
        Ok((owners, focused_native))
    }

    /// Map one native leaf's logical coordinates to physical-pixel card coordinates.
    #[cfg(test)]
    fn native_accessibility_transform_for_view(
        &self,
        wid: WindowId,
        view: ViewId,
    ) -> Option<aterm_a11y::model::Affine> {
        self.retained_native_accessibility_transform_for_view(wid, view)
            .or_else(|| {
                let plan = self.active_visible_leaf_plan(wid)?;
                self.planned_native_accessibility_transform(wid, view, &plan)
            })
    }

    /// Where a frame of `plan` puts `view`: a lone page fills the card at the window's edge, and a
    /// leaf of a composite sits at its layout slot inside the padded card.
    fn planned_native_accessibility_transform(
        &self,
        wid: WindowId,
        view: ViewId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<aterm_a11y::model::Affine> {
        let leaf = plan.leaf(view)?;
        let card_y = self.native_content_origin_y(wid) as i64;
        if plan.leaves.len() <= 1 {
            return self.native_accessibility_transform_at(wid, 0, card_y);
        }
        let (cw, ch) = self.win_cell_size(wid);
        let leaf_x = (leaf.rect.origin.x * cw as f32).round().max(0.0) as i64;
        let leaf_y = (leaf.rect.origin.y * ch as f32).round().max(0.0) as i64;
        self.native_accessibility_transform_at(
            wid,
            (self.win_pad(wid) as i64).saturating_add(leaf_x),
            card_y.saturating_add(leaf_y),
        )
    }

    /// Return the transform sourced from the compositor's retained destination record.
    fn retained_native_accessibility_transform_for_view(
        &self,
        wid: WindowId,
        view: ViewId,
    ) -> Option<aterm_a11y::model::Affine> {
        let window = self.windows.get(&wid)?;
        let card = window.settings_card.as_ref()?;
        let raster = window.leaf_render_cache.get(&view)?.native.as_ref()?;
        if raster.presented_x.checked_add(raster.width)? > card.pw
            || raster.presented_y.checked_add(raster.height)? > card.ph
        {
            return None;
        }
        self.native_accessibility_transform_at(
            wid,
            i64::from(card.dx).saturating_add(i64::from(raster.presented_x)),
            i64::from(card.dy).saturating_add(i64::from(raster.presented_y)),
        )
    }

    /// The transform of a page `x`, `y` physical pixels inside `wid`'s frame.
    fn native_accessibility_transform_at(
        &self,
        wid: WindowId,
        x: i64,
        y: i64,
    ) -> Option<aterm_a11y::model::Affine> {
        let (frame_x, frame_y) = self.frame_origin(wid);
        let scale = self.windows.get(&wid)?.scale.max(f64::EPSILON);
        let x = frame_x.saturating_add(x);
        let y = frame_y.saturating_add(y);
        Some(
            aterm_a11y::model::Affine::translate(aterm_a11y::model::Vec2::new(x as f64, y as f64))
                * aterm_a11y::model::Affine::scale(scale),
        )
    }

    /// Compatibility accessor for single/focused-native tests and host seams.
    #[cfg(test)]
    fn native_accessibility_transform(&self, wid: WindowId) -> Option<aterm_a11y::model::Affine> {
        let (_, view) = self.active_native_view(wid)?;
        self.native_accessibility_transform_for_view(wid, view)
    }

    fn native_accessibility_window_bounds(&self, wid: WindowId) -> Option<aterm_a11y::model::Rect> {
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
        Some(aterm_a11y::model::Rect {
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

    /// Route a platform accessibility request through the published native view and generation.
    pub(crate) fn on_native_accessibility_action(
        &mut self,
        wid: WindowId,
        request: aterm_a11y::model::ActionRequest,
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

        self.clear_staged_native_accessibility(wid);
        let focused = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.active())
            .is_some_and(|tab| tab.focus == owner.view);
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
        let result = match routed {
            RoutedAccessibilityAction::Activate { key, action, value } => self
                .dispatch_native_control_action(
                    wid,
                    owner.view,
                    key,
                    ActionInvocation { id: action, value },
                    true,
                ),
            RoutedAccessibilityAction::Focus { key } => {
                self.focus_native_control(wid, owner.view, key, true)
            }
            RoutedAccessibilityAction::Scroll { key, lines } => {
                self.dispatch_native_view_event(wid, owner.view, AppEvent::ScrollAt { lines, key })
            }
            RoutedAccessibilityAction::ScrollIntoView { key } => {
                self.dispatch_native_view_event(wid, owner.view, AppEvent::RevealControl(key))
            }
            RoutedAccessibilityAction::ReplaceSelectedText { text, .. } => {
                self.dispatch_native_view_event(wid, owner.view, AppEvent::InsertText(text))
            }
            RoutedAccessibilityAction::SetTextSelection {
                anchor_byte,
                focus_byte,
                ..
            } => self.dispatch_native_view_event(
                wid,
                owner.view,
                AppEvent::EditorSetSelection {
                    anchor: anchor_byte,
                    head: focus_byte,
                },
            ),
        };
        if result.is_err() {
            return;
        }
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
        self.push_a11y_tree(wid);
    }

    /// What an attached accessibility client sends for `action` on `view`'s control `key`,
    /// carrying `data`: `wid`'s tree is published as the client reads it, and the request names
    /// that control's node and action context.
    #[cfg(test)]
    pub(crate) fn accessibility_request_for_test(
        &mut self,
        wid: WindowId,
        view: ViewId,
        key: &str,
        action: aterm_a11y::model::Action,
        data: Option<aterm_a11y::model::ActionData>,
    ) -> aterm_a11y::model::ActionRequest {
        self.windows
            .get_mut(&wid)
            .expect("headless window")
            .a11y_active = true;
        assert!(self.prepare_native_input_scratch(wid));
        let (update, published) = self
            .take_native_accessibility_update(wid)
            .expect("an accessibility update")
            .expect("a valid accessibility update");
        let request = accessibility_request_on(&update, view, key, action, data);
        self.windows
            .get_mut(&wid)
            .expect("headless window")
            .native_a11y_published = Some(published);
        request
    }

    /// What a client sends for `action` on `view`'s control `key` from the tree it last received:
    /// nothing is painted or published first, so this is how a client acts between frames.
    #[cfg(all(test, a11y_tree))]
    pub(crate) fn request_on_the_sent_tree(
        &self,
        wid: WindowId,
        view: ViewId,
        key: &str,
        action: aterm_a11y::model::Action,
        data: Option<aterm_a11y::model::ActionData>,
    ) -> aterm_a11y::model::ActionRequest {
        let update = self.windows[&wid].a11y_sent.last().expect("a sent tree");
        accessibility_request_on(update, view, key, action, data)
    }
}

/// The request a client builds from `update` for `action` on `view`'s control `key`.
#[cfg(test)]
fn accessibility_request_on(
    update: &aterm_a11y::model::TreeUpdate,
    view: ViewId,
    key: &str,
    action: aterm_a11y::model::Action,
    data: Option<aterm_a11y::model::ActionData>,
) -> aterm_a11y::model::ActionRequest {
    let node = crate::native_accessibility::stable_node_id_for_view(
        view,
        &crate::native_ui::UiKey::new(key),
    );
    let control = &update
        .nodes
        .iter()
        .find(|(id, _)| *id == node)
        .unwrap_or_else(|| panic!("the tree publishes {key}"))
        .1;
    assert!(control.supports_action(action), "{key} takes {action:?}");
    let context = control.action_context();
    assert!(context.is_some(), "{key} carries an action context");
    aterm_a11y::model::ActionRequest {
        context,
        action,
        target_tree: aterm_a11y::model::TreeId::ROOT,
        target_node: node,
        data,
    }
}

#[cfg(test)]
#[path = "app_native_accessibility_tests.rs"]
mod tests;
