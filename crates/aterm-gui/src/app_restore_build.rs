// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One cold-restored tab, built a leaf at a time while its source remains capturable.

use super::{App, BuiltRestoreLeaf, BuiltRestoreTab, LeafIds, WindowId, restore};
use crate::tab_model::{SplitAxis, SplitTree, Tab, TabId, ViewId};

#[derive(Clone)]
pub(super) struct Anchor {
    pub tab: TabId,
    pub view: ViewId,
    pub original: Tab,
}

enum Work {
    Tree(Box<restore::RestoredSplitTree>, Vec<restore::RestoreBranch>),
    Join(SplitAxis, f32),
}

struct WaitingDocument {
    path: Vec<restore::RestoreBranch>,
    descriptor: restore::NativeLeafRestore,
    admission: crate::app_documents::RestoreDocumentAdmission,
}

pub(super) enum Step {
    Waiting,
    Advanced,
    Complete(BuiltRestoreTab),
    Failed(String),
}

pub(super) struct BuildingTab {
    pub index: usize,
    pub record: restore::RestoredTab,
    pub anchor: Option<Anchor>,
    work: Vec<Work>,
    roots: Vec<SplitTree<ViewId>>,
    leaves: Vec<(Vec<restore::RestoreBranch>, BuiltRestoreLeaf)>,
    reusable: Option<(ViewId, u64)>,
    reusable_before: Option<(ViewId, u64)>,
    waiting: Option<WaitingDocument>,
    bootstrap_path: Option<(Vec<restore::RestoreBranch>, ViewId)>,
    seeded: bool,
    reopen: bool,
}

impl BuildingTab {
    /// A native-only source may replace the initial shell only while that shell is unused.
    /// Read the same live evidence for capture and publication; a snapshot never consumes it.
    pub fn preserves_bootstrap(&self, app: &App, wid: WindowId) -> bool {
        let Some(anchor) = self.anchor.as_ref() else {
            return false;
        };
        if self.record.root.first_terminal_leaf().is_some() {
            return false;
        }
        let Some(window) = app.windows.get(&wid) else {
            return false;
        };
        let Some(tab) = window
            .tab_set
            .get(anchor.tab)
            .filter(|tab| tab.root.contains(anchor.view))
        else {
            return false;
        };
        if tab.root != SplitTree::Leaf(anchor.view)
            || tab.zoomed
            || tab.parked
            || tab.ephemeral
            || tab.fleet
        {
            return true;
        }
        let Some(session) = app
            .view_store
            .get(anchor.view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
        else {
            return false;
        };
        let Some(live) = app.pool.get(session) else {
            return false;
        };
        let driver_metadata = {
            let meta = live.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            crate::session_timeline::MetaField::ALL
                .into_iter()
                .any(|field| meta.driver_wrote(field))
        };
        live.ctx.human_input.seq() != 0
            || live.ctx.typed.driver_admitted()
            || live.ctx.output_echo.retains_input()
            || driver_metadata
            || !app.desk_holds(session).is_empty()
            || app.pool.theme_decided(session)
    }

    #[cfg(all(test, unix))]
    pub fn owned_leaf_count(&self) -> usize {
        self.leaves.len()
    }

    pub fn waiting(&self) -> bool {
        self.waiting.is_some()
    }
    pub fn new(
        index: usize,
        record: restore::RestoredTab,
        anchor: Option<Anchor>,
        reusable: Option<(ViewId, u64)>,
    ) -> Self {
        let work = vec![Work::Tree(Box::new(record.root.clone()), Vec::new())];
        let bootstrap_path = reusable
            .and_then(|(view, _)| first_terminal_path(&record.root).map(|path| (path, view)));
        Self {
            index,
            record,
            anchor,
            work,
            roots: Vec::new(),
            leaves: Vec::new(),
            reusable,
            reusable_before: reusable,
            waiting: None,
            bootstrap_path,
            seeded: false,
            reopen: false,
        }
    }

    /// Closed history uses live identities and keeps every failure recoverable.
    pub fn reopen(index: usize, record: restore::RestoredTab) -> Self {
        let mut build = Self::new(index, record, None, None);
        build.reopen = true;
        build
    }

    pub fn validate_cancel(&self, app: &App) -> Result<(), String> {
        for (_, (view, _, _)) in &self.leaves {
            app.validate_restore_view_rollback(*view)?;
        }
        Ok(())
    }

    /// A shared canonical head may change while a later leaf waits. Restore the saved
    /// document offsets against that current head in the final exclusive transition.
    pub fn prepare_reopen_publication(&self, app: &mut App) -> Result<(), String> {
        debug_assert!(self.reopen);
        for (path, (view, _, _)) in &self.leaves {
            let mut source = &self.record.root;
            for branch in path {
                let restore::RestoredSplitTree::Split { first, second, .. } = source else {
                    return Err("closed reopen lost its source leaf path".into());
                };
                source = match branch {
                    restore::RestoreBranch::First => first,
                    restore::RestoreBranch::Second => second,
                };
            }
            if let restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Native(descriptor),
            } = source
                && matches!(descriptor.restore_tag.as_str(), "editor" | "markdown")
                && !app.apply_native_view_restore(*view, descriptor)
            {
                return Err("closed reopen document state changed before publication".into());
            }
        }
        Ok(())
    }

    /// A snapshot borrows live state; it never consumes a ticket, view, session or source leaf.
    pub fn snapshot(&self, app: &App) -> restore::RestoredTab {
        let mut record = self.record.clone();
        let leaves = self
            .leaves
            .iter()
            .map(|(path, (view, _, _))| (path, view))
            .chain(
                self.bootstrap_path
                    .iter()
                    .filter(|_| self.seeded)
                    .map(|(path, view)| (path, view)),
            );
        for (path, view) in leaves {
            if let Some(mut current) = app.view_restore_descriptor_carrying(
                *view,
                false,
                self.record.parked || self.record.ephemeral,
            ) && let Some(leaf) = source_leaf_mut(&mut record.root, path)
            {
                if let restore::RestoredView::Terminal(terminal) = &mut current
                    && let Some(id) = terminal.local_id
                {
                    terminal.agent = app
                        .session_sid(id)
                        .and_then(|sid| app.pending_restored_agent_by_sid(&sid, false))
                        .or_else(|| app.unhanded_restored_agent(id).map(Box::new));
                    terminal.holds = app.desk_holds(id);
                }
                *leaf = current;
            }
        }
        record
    }

    pub fn cancel(self, app: &mut App) {
        let leaves: Vec<_> = self.leaves.into_iter().map(|(_, leaf)| leaf).collect();
        app.rollback_recursive_restore_leaves(&leaves, self.reusable_before);
    }

    fn retain(&mut self, path: Vec<restore::RestoreBranch>, leaf: BuiltRestoreLeaf) {
        self.roots.push(SplitTree::Leaf(leaf.0));
        self.leaves.push((path, leaf));
    }

    fn advanced(&mut self, app: &mut App, wid: WindowId) -> Step {
        while matches!(self.work.last(), Some(Work::Join(..))) {
            let Some(Work::Join(axis, ratio)) = self.work.pop() else {
                unreachable!()
            };
            let Some(second) = self.roots.pop() else {
                return Step::Failed("missing restored second branch".into());
            };
            let Some(first) = self.roots.pop() else {
                return Step::Failed("missing restored first branch".into());
            };
            self.roots.push(SplitTree::Split {
                axis,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            });
        }
        if !self.work.is_empty() {
            return Step::Advanced;
        }
        if self.roots.len() != 1 {
            return Step::Failed("restored tab has no complete root".into());
        }
        let root = self.roots.pop().expect("one complete root");
        let leaves: Vec<_> = self.leaves.iter().map(|(_, leaf)| leaf.clone()).collect();
        match app.finish_recursive_restore_tab(wid, &self.record, root, &leaves) {
            Ok(tab) => {
                if !self.reopen {
                    app.note_restored_opens(wid, &tab.0);
                }
                Step::Complete(tab)
            }
            Err(error) => Step::Failed(error),
        }
    }

    /// At most one leaf is allocated or one completed admission installed per call.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "Stage",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClosedReopenPublication",
            action = "FirstReady",
            project = "aterm_gui::app_restore::closed::tests::project"
        )
    )]
    pub fn step(&mut self, app: &mut App, wid: WindowId) -> Step {
        if !self.seeded {
            if let Some((_, session)) = self.reusable
                && let Some(terminal) = self.record.root.first_terminal_leaf()
            {
                app.carry_restored_identity(
                    terminal,
                    super::FillingShell::Registered(session),
                    LeafIds::Retired,
                );
                app.carry_restored_theme(session, terminal);
            }
            self.seeded = true;
        }
        if let Some(waiting) = self.waiting.as_mut() {
            let Some(result) = app.poll_restore_document_admission(&mut waiting.admission) else {
                return Step::Waiting;
            };
            let waiting = self.waiting.take().expect("the completed admission");
            let leaf = app.with_detached_restore_leaf(wid, |app| {
                app.build_admitted_restore_leaf_policy(
                    wid,
                    &waiting.descriptor,
                    result,
                    self.reopen,
                )
            });
            return match leaf {
                Ok(leaf) => {
                    self.retain(waiting.path, leaf);
                    self.advanced(app, wid)
                }
                Err(error) => Step::Failed(error),
            };
        }
        while let Some(work) = self.work.pop() {
            match work {
                Work::Join(axis, ratio) => {
                    let Some(second) = self.roots.pop() else {
                        return Step::Failed("missing restored second branch".into());
                    };
                    let Some(first) = self.roots.pop() else {
                        return Step::Failed("missing restored first branch".into());
                    };
                    self.roots.push(SplitTree::Split {
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    });
                }
                Work::Tree(node, path) => match *node {
                    restore::RestoredSplitTree::Split {
                        axis,
                        ratio,
                        first,
                        second,
                    } => {
                        let axis = match axis {
                            restore::SplitKind::Horizontal => SplitAxis::Horizontal,
                            restore::SplitKind::Vertical => SplitAxis::Vertical,
                        };
                        let mut right = path.clone();
                        right.push(restore::RestoreBranch::Second);
                        let mut left = path;
                        left.push(restore::RestoreBranch::First);
                        self.work.push(Work::Join(axis, ratio.clamp(0.05, 0.95)));
                        self.work.push(Work::Tree(second, right));
                        self.work.push(Work::Tree(first, left));
                    }
                    restore::RestoredSplitTree::Leaf { view } => {
                        if matches!(view, restore::RestoredView::Terminal(_))
                            && let Some((reused, _)) = self.reusable.take()
                        {
                            let presentation =
                                app.live_view_presentation(reused).unwrap_or_else(|| {
                                    crate::tab_model::TabPresentation::terminal("Terminal")
                                });
                            self.retain(path, (reused, presentation, None));
                            return self.advanced(app, wid);
                        }
                        if let restore::RestoredView::Native(descriptor) = &view
                            && matches!(descriptor.restore_tag.as_str(), "editor" | "markdown")
                        {
                            if self.reopen
                                && let Some(result) =
                                    app.build_existing_reopen_document(wid, descriptor)
                            {
                                return match result {
                                    Ok(leaf) => {
                                        self.retain(path, leaf);
                                        self.advanced(app, wid)
                                    }
                                    Err(error) => Step::Failed(error),
                                };
                            }
                            match app.begin_restore_document_admission(wid, descriptor) {
                                Ok(admission) => {
                                    self.waiting = Some(WaitingDocument {
                                        path,
                                        descriptor: descriptor.clone(),
                                        admission,
                                    });
                                    return Step::Waiting;
                                }
                                Err(error) => {
                                    if self.reopen {
                                        return Step::Failed(error);
                                    }
                                    return match app.with_detached_restore_leaf(wid, |app| {
                                        app.build_admitted_restore_leaf(wid, descriptor, Err(error))
                                    }) {
                                        Ok(leaf) => {
                                            self.retain(path, leaf);
                                            self.advanced(app, wid)
                                        }
                                        Err(error) => Step::Failed(error),
                                    };
                                }
                            }
                        }
                        return match app.with_detached_restore_leaf(wid, |app| {
                            app.build_recursive_restore_leaf_policy(
                                wid,
                                &view,
                                &mut self.reusable,
                                if self.reopen {
                                    LeafIds::Live
                                } else {
                                    LeafIds::Retired
                                },
                                crate::app_tabs::NativeInstallTarget::DetachedRestore(wid),
                                self.reopen,
                            )
                        }) {
                            Ok(leaf) => {
                                self.retain(path, leaf);
                                self.advanced(app, wid)
                            }
                            Err(error) => Step::Failed(error),
                        };
                    }
                },
            }
        }
        self.advanced(app, wid)
    }
}

fn first_terminal_path(root: &restore::RestoredSplitTree) -> Option<Vec<restore::RestoreBranch>> {
    match root {
        restore::RestoredSplitTree::Leaf {
            view: restore::RestoredView::Terminal(_),
        } => Some(Vec::new()),
        restore::RestoredSplitTree::Leaf { .. } => None,
        restore::RestoredSplitTree::Split { first, second, .. } => first_terminal_path(first)
            .map(|mut path| {
                path.insert(0, restore::RestoreBranch::First);
                path
            })
            .or_else(|| {
                first_terminal_path(second).map(|mut path| {
                    path.insert(0, restore::RestoreBranch::Second);
                    path
                })
            }),
    }
}

pub(super) fn graft(
    root: &mut SplitTree<ViewId>,
    at: ViewId,
    replacement: &mut Option<SplitTree<ViewId>>,
) -> bool {
    match root {
        SplitTree::Leaf(view) if *view == at => {
            *root = replacement.take().expect("one graft target");
            true
        }
        SplitTree::Leaf(_) => false,
        SplitTree::Split { first, second, .. } => {
            graft(first, at, replacement) || graft(second, at, replacement)
        }
    }
}

fn source_leaf_mut<'a>(
    tree: &'a mut restore::RestoredSplitTree,
    path: &[restore::RestoreBranch],
) -> Option<&'a mut restore::RestoredView> {
    let mut tree = tree;
    for branch in path {
        let restore::RestoredSplitTree::Split { first, second, .. } = tree else {
            return None;
        };
        tree = match branch {
            restore::RestoreBranch::First => first,
            restore::RestoreBranch::Second => second,
        };
    }
    match tree {
        restore::RestoredSplitTree::Leaf { view } => Some(view),
        _ => None,
    }
}
