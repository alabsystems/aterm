// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Closed history owns detached construction until a single exclusive publication.

use super::{App, BuiltRestoreTab, WindowId, cold, restore};
use crate::closed_recovery::{ClosedTab, ClosedView, ReopenCandidate};
use crate::tab_model::{SplitTree, ViewId};

pub(crate) enum Source {
    Tab(ReopenCandidate<ClosedTab>),
    View(ReopenCandidate<ClosedView>),
}

impl Source {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Tab(a), Self::Tab(b)) => a.token == b.token,
            (Self::View(a), Self::View(b)) => a.token == b.token,
            _ => false,
        }
    }

    fn fresh(&self, app: &App, now: u64) -> bool {
        match self {
            Self::Tab(candidate) => app
                .closed_recovery
                .tabs
                .matches_candidate(candidate.token, now),
            Self::View(candidate) => app
                .closed_recovery
                .views
                .matches_candidate(candidate.token, now),
        }
    }

    fn is_tab(&self) -> bool {
        matches!(self, Self::Tab(_))
    }
}

/// This owner is never cloned. A completion can only be used by its retained builder.
pub(crate) struct Pending {
    source: Source,
    window: WindowId,
    survivor: Option<SplitTree<ViewId>>,
    build: cold::BuildingTab,
    canceling: bool,
}

fn subtree<'a>(
    mut root: &'a SplitTree<ViewId>,
    path: &[restore::RestoreBranch],
) -> Option<&'a SplitTree<ViewId>> {
    for branch in path {
        let SplitTree::Split { first, second, .. } = root else {
            return None;
        };
        root = match branch {
            restore::RestoreBranch::First => first,
            restore::RestoreBranch::Second => second,
        };
    }
    Some(root)
}

impl App {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "Begin",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "BeginWaiting",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedReopenPublication",
            action = "Replace",
            reason = "Environment: another close replaces the newest opaque history token. The reopen owner observes its own ledger; it never selects or commits the replacement."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedReopenPublication",
            action = "Expire",
            reason = "Environment: elapsed monotonic time expires the retained history candidate. The owner observes freshness at each progress/publication boundary; it does not advance the clock."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedReopenPublication",
            action = "Kill",
            reason = "Environment: the selected window closes outside the reopen transaction. The owner observes that exact destination disappearing and rolls back without redirecting it."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClosedReopenPublication",
            action = "Edit",
            reason = "Environment: input in another published view changes the shared canonical head. The detached owner joins that head and restores offsets in its exclusive publication preflight."
        )
    )]
    pub(crate) fn begin_closed_reopen(&mut self, source: Source) -> Result<(), String> {
        if self
            .closed_reopens
            .iter()
            .any(|pending| pending.source.same(&source) && !pending.canceling)
        {
            return Ok(());
        }
        // Replacing an operation cancels its exact prefix before accepting another owner.
        self.cancel_closed_reopen(source.is_tab())?;
        let (window, index, record, survivor) = match &source {
            Source::Tab(candidate) => {
                let closed = &candidate.value;
                let window = self
                    .windows
                    .contains_key(&closed.original_window)
                    .then_some(closed.original_window)
                    .or(self.frontmost_window)
                    .ok_or_else(|| "no window for reopened tab".to_string())?;
                (window, closed.original_index, closed.tab.clone(), None)
            }
            Source::View(candidate) => {
                let closed = &candidate.value;
                let host = self
                    .windows
                    .get(&closed.original_window)
                    .and_then(|window| window.tab_set.get(closed.original_tab));
                let survivor = host
                    .and_then(|tab| subtree(&tab.root, &closed.placement.parent_path))
                    .cloned();
                let window = host
                    .map(|_| closed.original_window)
                    .or(self.frontmost_window)
                    .ok_or_else(|| "no window for reopened view".to_string())?;
                let index = self
                    .windows
                    .get(&window)
                    .map_or(0, |window| window.tab_set.len());
                (
                    window,
                    index,
                    restore::RestoredTab {
                        root: restore::RestoredSplitTree::leaf(closed.view.clone()),
                        focused_path: Vec::new(),
                        zoomed: false,
                        parked: false,
                        ephemeral: false,
                        fleet: false,
                    },
                    survivor,
                )
            }
        };
        self.closed_reopens.push(Pending {
            source,
            window,
            survivor,
            build: cold::BuildingTab::reopen(index, record),
            canceling: false,
        });
        self.progress_closed_reopens();
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "Cancel",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    pub(crate) fn cancel_closed_reopen(&mut self, tabs: bool) -> Result<(), String> {
        let Some(index) = self
            .closed_reopens
            .iter()
            .position(|pending| pending.source.is_tab() == tabs)
        else {
            return Ok(());
        };
        self.closed_reopens[index].canceling = true;
        self.closed_reopens[index].build.validate_cancel(self)?;
        let pending = self.closed_reopens.remove(index);
        pending.build.cancel(self);
        Ok(())
    }

    /// One leaf per operation per event-loop turn. Read and journal wakes use this same owner.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "Wait",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    pub(crate) fn progress_closed_reopens(&mut self) {
        let mut retained = std::mem::take(&mut self.closed_reopens);
        let mut again = false;
        for index in (0..retained.len()).rev() {
            let mut operation = retained.remove(index);
            let now = self.lat_epoch.elapsed().as_millis() as u64;
            if !operation.source.fresh(self, now)
                || !self
                    .windows
                    .get(&operation.window)
                    .is_some_and(|window| !window.pending_close)
            {
                operation.canceling = true;
            }
            let canceled = operation.canceling;
            let result = if canceled {
                Err("closed reopen canceled or its candidate/window disappeared".to_string())
            } else {
                match operation.build.step(self, operation.window) {
                    cold::Step::Waiting => {
                        retained.insert(index, operation);
                        continue;
                    }
                    cold::Step::Advanced => {
                        again = true;
                        retained.insert(index, operation);
                        continue;
                    }
                    cold::Step::Complete(built) => self.publish_closed_reopen(&operation, built),
                    cold::Step::Failed(error) => Err(error),
                }
            };
            if let Err(error) = result {
                operation.canceling = true;
                if let Err(refusal) = operation.build.validate_cancel(self) {
                    // Never drop custody on a refused rollback; the owner remains canceling.
                    aterm_log::warn!("closed reopen rollback retained: {refusal}");
                    retained.insert(index, operation);
                } else {
                    operation.build.cancel(self);
                }
                if !canceled {
                    self.pending_action_refusal = Some(error);
                }
            }
        }
        self.closed_reopens = retained;
        #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
        crate::restore_measurement::observe_closed_reopen_fixture(self);
        if again && let Some(proxy) = &self.proxy {
            let _ = proxy.send_event(crate::Wake::RestoreDocumentReady);
        }
    }

    #[cfg(test)]
    pub(crate) fn reopen_closed_tab_and_wait_for_test(&mut self) -> Result<(), String> {
        self.reopen_closed_history_and_wait_for_test(true)
    }

    #[cfg(test)]
    pub(crate) fn reopen_closed_view_and_wait_for_test(&mut self) -> Result<(), String> {
        self.reopen_closed_history_and_wait_for_test(false)
    }

    #[cfg(test)]
    fn reopen_closed_history_and_wait_for_test(&mut self, tabs: bool) -> Result<(), String> {
        let drive = |app: &mut App| {
            let previous = app.pending_action_refusal.take();
            let result = if tabs {
                app.reopen_last_closed_tab()
            } else {
                app.reopen_last_closed_view()
            };
            if let Err(error) = result {
                app.pending_action_refusal = previous;
                return Err(error);
            }
            let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !app.closed_reopens.is_empty() {
                app.progress_closed_reopens();
                assert!(
                    std::time::Instant::now() < until,
                    "closed reopen did not settle"
                );
                aterm_uds::deadline::poll_yield();
            }
            let refusal = app.pending_action_refusal.take();
            app.pending_action_refusal = previous;
            refusal.map_or(Ok(()), Err)
        };
        crate::app_documents::with_headless_restore_admission_for_test(|| drive(self))
    }

    /// No yield or event dispatch occurs between freshness, custody, topology and consumption.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "Publish",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    fn publish_closed_reopen(
        &mut self,
        operation: &Pending,
        built: BuiltRestoreTab,
    ) -> Result<(), String> {
        let now = self.lat_epoch.elapsed().as_millis() as u64;
        if !operation.source.fresh(self, now) {
            return Err("closed recovery candidate changed before publication".into());
        }
        operation.build.validate_cancel(self)?;
        operation.build.prepare_reopen_publication(self)?;
        let (tab, _) = built;
        let window = self
            .windows
            .get(&operation.window)
            .filter(|window| !window.pending_close)
            .ok_or_else(|| "closed reopen target window disappeared".to_string())?;
        let mut graft = None;
        if let Source::View(candidate) = &operation.source {
            let closed = &candidate.value;
            if let Some(current) = window.tab_set.get(closed.original_tab)
                && operation.survivor.as_ref().is_some_and(|survivor| {
                    subtree(&current.root, &closed.placement.parent_path) == Some(survivor)
                })
            {
                let mut replacement = current.clone();
                let path = crate::tab_model::SplitPath::from_branches(
                    closed
                        .placement
                        .parent_path
                        .iter()
                        .map(|branch| match branch {
                            restore::RestoreBranch::First => crate::tab_model::SplitBranch::First,
                            restore::RestoreBranch::Second => crate::tab_model::SplitBranch::Second,
                        })
                        .collect(),
                );
                let branch = match closed.placement.removed_branch {
                    restore::RestoreBranch::First => crate::tab_model::SplitBranch::First,
                    restore::RestoreBranch::Second => crate::tab_model::SplitBranch::Second,
                };
                let axis = match closed.placement.axis {
                    restore::SplitKind::Horizontal => crate::tab_model::SplitAxis::Horizontal,
                    restore::SplitKind::Vertical => crate::tab_model::SplitAxis::Vertical,
                };
                if replacement.root.restore_collapsed_leaf(
                    &path,
                    branch,
                    axis,
                    closed.placement.ratio,
                    tab.focus,
                ) {
                    replacement.focus = tab.focus;
                    replacement.zoomed = false;
                    graft = Some(replacement);
                }
            }
        }
        if graft.is_none() && window.tab_set.get(tab.id).is_some() {
            return Err("closed reopen tab identity collided".into());
        }
        // Build the prospective terminal projection before consuming any attachment.
        let prospective = graft.as_ref().unwrap_or(&tab);
        let projection = self.closed_reopen_projection(prospective)?;
        let topology = self.prepare_closed_reopen_topology(
            operation.window,
            tab.clone(),
            graft,
            projection,
            operation.build.index,
        )?;
        let now = self.lat_epoch.elapsed().as_millis() as u64;
        if !operation.source.fresh(self, now) {
            return Err("closed recovery candidate expired during preflight".into());
        }
        self.validate_closed_reopen_documents(&tab)?;
        self.publish_restore_document_views(&tab)?;
        let opened = self.install_closed_reopen_topology(operation.window, topology);
        match &operation.source {
            Source::Tab(candidate) => {
                self.closed_recovery
                    .tabs
                    .commit(candidate.token, now)
                    .expect("exclusive candidate preflight");
            }
            Source::View(candidate) => {
                self.closed_recovery
                    .views
                    .commit(candidate.token, now)
                    .expect("exclusive candidate preflight");
            }
        }
        self.note_restored_opens(operation.window, &opened);
        self.publish_finished_restore_tab(operation.window, Some(opened.id));
        self.refresh_disambiguated_document_titles();
        self.refresh_aggregate_tab_presentation(operation.window, opened.id);
        self.resize_panes(operation.window);
        self.resync_active_or_window(operation.window);
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "app_closed_reopen_tests.rs"]
mod tests;
