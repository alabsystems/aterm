// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mutation-owned invalidation of the platform Option key route.

use crate::{WindowId, WindowState};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    rc::{Rc, Weak},
};

type Queue = RefCell<BTreeMap<WindowId, Weak<SignalState>>>;
type Pending = Rc<Queue>;

#[derive(Debug, Default)]
struct SignalState {
    owner: RefCell<Option<(WindowId, Weak<Queue>)>>,
    queued: Cell<bool>,
    #[cfg(test)]
    evaluations: Cell<u64>,
    #[cfg(test)]
    classifiers: Cell<WindowClassifierWork>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Signal(Rc<SignalState>);

impl Signal {
    #[cfg(test)]
    pub(crate) fn record_evaluation(&self) {
        self.0.evaluations.set(self.0.evaluations.get() + 1);
    }
    #[cfg(test)]
    pub(crate) fn evaluations(&self) -> u64 {
        self.0.evaluations.get()
    }

    pub(crate) fn invalidate(&self) {
        if self.0.queued.replace(true) {
            return;
        }
        if let Some((id, pending)) = self.0.owner.borrow().as_ref()
            && let Some(pending) = pending.upgrade()
        {
            pending.borrow_mut().insert(*id, Rc::downgrade(&self.0));
        }
    }

    fn bind(&self, id: WindowId, pending: &Pending) {
        *self.0.owner.borrow_mut() = Some((id, Rc::downgrade(pending)));
        self.0.queued.set(false);
        self.invalidate();
    }
}

/// Actual invalidation predicate calls, independent of published route projections.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowClassifierWork {
    front: u64,
    presence: u64,
    rename: u64,
}

fn classify_front(
    signal: &Signal,
    value: Option<crate::front_content::FrontContent>,
) -> Option<crate::front_content::FrontContent> {
    #[cfg(test)]
    {
        let mut work = signal.0.classifiers.get();
        work.front += 1;
        signal.0.classifiers.set(work);
    }
    #[cfg(not(test))]
    let _ = signal;
    value
}
fn classify_presence<T>(signal: &Signal, value: &Option<T>) -> bool {
    #[cfg(test)]
    {
        let mut work = signal.0.classifiers.get();
        work.presence += 1;
        signal.0.classifiers.set(work);
    }
    #[cfg(not(test))]
    let _ = signal;
    value.is_some()
}

#[cfg(test)]
thread_local! {
    static GENERAL_SNAPSHOT_MUTANT: Cell<bool> = const { Cell::new(false) };
}

/// Run the actual mutable membership path with the rejected snapshot behavior.
#[cfg(test)]
pub(crate) fn with_general_snapshot_mutant_for_negative_control<T>(run: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            GENERAL_SNAPSHOT_MUTANT.with(|flag| flag.set(self.0));
        }
    }
    let reset = Reset(GENERAL_SNAPSHOT_MUTANT.with(|flag| flag.replace(true)));
    let result = run();
    drop(reset);
    result
}

/// Read-only projection: mutable route payloads are available only through Routing.
pub(crate) struct ReadOnly {
    pub(crate) front_content: Option<crate::front_content::FrontContent>,
    pub(crate) search: Option<crate::app_search::SearchState>,
    pub(crate) tab_menu: Option<crate::tab_menu::TabMenu>,
    pub(crate) overlay: Option<crate::overlay::Overlay>,
    pub(crate) rename_edit: Option<crate::app_rename::TabRenameEdit>,
}

/// One logical route identity, with private payload slots and no mutable aggregate projection.
pub(crate) struct Routing {
    signal: Signal,
    data: ReadOnly,
}
impl Routing {
    pub(crate) fn new(front: Option<crate::front_content::FrontContent>) -> Self {
        Self {
            signal: Signal::default(),
            data: ReadOnly {
                front_content: front,
                search: None,
                tab_menu: None,
                overlay: None,
                rename_edit: None,
            },
        }
    }
    pub(crate) fn read(&self) -> &ReadOnly {
        &self.data
    }
    pub(crate) fn signal(&self) -> &Signal {
        &self.signal
    }
    #[cfg(test)]
    pub(crate) fn classifier_work(&self) -> WindowClassifierWork {
        self.signal.0.classifiers.get()
    }

    /// Negative control for attaching the actual predicates to general window borrows.
    #[cfg(test)]
    pub(crate) fn classify_snapshot_for_negative_control(
        &self,
    ) -> (
        Option<crate::front_content::FrontContent>,
        bool,
        bool,
        bool,
        bool,
    ) {
        (
            classify_front(&self.signal, self.data.front_content),
            classify_presence(&self.signal, &self.data.search),
            classify_presence(&self.signal, &self.data.tab_menu),
            classify_presence(&self.signal, &self.data.overlay),
            rename_blocks(&self.signal, self.data.rename_edit.as_ref()),
        )
    }

    pub(crate) fn set_front(&mut self, front: Option<crate::front_content::FrontContent>) {
        if classify_front(&self.signal, self.data.front_content)
            != classify_front(&self.signal, front)
        {
            self.signal.invalidate();
        }
        self.data.front_content = front;
    }
    pub(crate) fn set_search(&mut self, value: Option<crate::app_search::SearchState>) {
        if classify_presence(&self.signal, &self.data.search)
            != classify_presence(&self.signal, &value)
        {
            self.signal.invalidate();
        }
        self.data.search = value;
    }
    #[cfg(test)]
    pub(crate) fn take_search(&mut self) -> Option<crate::app_search::SearchState> {
        let value = self.data.search.take();
        if classify_presence(&self.signal, &value) {
            self.signal.invalidate();
        }
        value
    }
    pub(crate) fn search_mut(&mut self) -> Option<&mut crate::app_search::SearchState> {
        self.data.search.as_mut()
    }
    pub(crate) fn set_menu(&mut self, value: Option<crate::tab_menu::TabMenu>) {
        if classify_presence(&self.signal, &self.data.tab_menu)
            != classify_presence(&self.signal, &value)
        {
            self.signal.invalidate();
        }
        self.data.tab_menu = value;
    }
    pub(crate) fn menu_mut(&mut self) -> Option<&mut crate::tab_menu::TabMenu> {
        self.data.tab_menu.as_mut()
    }
    pub(crate) fn take_menu(&mut self) -> Option<crate::tab_menu::TabMenu> {
        let value = self.data.tab_menu.take();
        if classify_presence(&self.signal, &value) {
            self.signal.invalidate();
        }
        value
    }
    pub(crate) fn set_overlay(&mut self, value: Option<crate::overlay::Overlay>) {
        if classify_presence(&self.signal, &self.data.overlay)
            != classify_presence(&self.signal, &value)
        {
            self.signal.invalidate();
        }
        self.data.overlay = value;
    }
    pub(crate) fn overlay_mut(&mut self) -> Option<&mut crate::overlay::Overlay> {
        self.data.overlay.as_mut()
    }
    pub(crate) fn set_rename(&mut self, value: Option<crate::app_rename::TabRenameEdit>) {
        if rename_blocks(&self.signal, self.data.rename_edit.as_ref())
            != rename_blocks(&self.signal, value.as_ref())
        {
            self.signal.invalidate();
        }
        self.data.rename_edit = value;
    }
    pub(crate) fn take_rename(&mut self) -> Option<crate::app_rename::TabRenameEdit> {
        let value = self.data.rename_edit.take();
        if rename_blocks(&self.signal, value.as_ref()) {
            self.signal.invalidate();
        }
        value
    }
    pub(crate) fn rename_mut(&mut self) -> Option<RenameMutation<'_>> {
        let value = self.data.rename_edit.as_mut()?;
        Some(RenameMutation {
            before: rename_blocks(&self.signal, Some(value)),
            value,
            signal: &self.signal,
        })
    }
}
fn rename_blocks(signal: &Signal, edit: Option<&crate::app_rename::TabRenameEdit>) -> bool {
    #[cfg(test)]
    {
        let mut work = signal.0.classifiers.get();
        work.rename += 1;
        signal.0.classifiers.set(work);
    }
    #[cfg(not(test))]
    let _ = signal;
    edit.is_some_and(|edit| edit.surface == crate::app_rename::RenameSurface::InGrid)
}
pub(crate) struct RenameMutation<'a> {
    value: &'a mut crate::app_rename::TabRenameEdit,
    before: bool,
    signal: &'a Signal,
}
impl Deref for RenameMutation<'_> {
    type Target = crate::app_rename::TabRenameEdit;
    fn deref(&self) -> &Self::Target {
        self.value
    }
}
impl DerefMut for RenameMutation<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value
    }
}
impl Drop for RenameMutation<'_> {
    fn drop(&mut self) {
        if self.before != rename_blocks(self.signal, Some(self.value)) {
            self.signal.invalidate();
        }
    }
}

/// Custody stays outside the general mutable window payload projection.
struct WindowEntry {
    value: Box<WindowState>,
    tabs: crate::tab_model::TitleLease,
    epochs: crate::tab_model::TitleLease,
}

/// Membership binds intrinsic window and title signals, including restored/replaced roots.
#[derive(Default)]
pub(crate) struct Windows {
    titles: crate::tab_model::TitleAuthority,
    values: BTreeMap<WindowId, WindowEntry>,
    pending: Pending,
    option_as_meta: Cell<Option<bool>>,
}
impl Windows {
    pub(crate) fn insert(
        &mut self,
        id: WindowId,
        mut value: Box<WindowState>,
    ) -> Option<Box<WindowState>> {
        let previous = self.remove(&id);
        value.routing.signal().bind(id, &self.pending);
        let tabs = value.tab_set.bind_title_owner(id.0, &self.titles);
        let epochs = value.tab_title_epochs.bind_owner(id.0, &self.titles);
        self.values.insert(
            id,
            WindowEntry {
                value,
                tabs,
                epochs,
            },
        );
        previous
    }
    pub(crate) fn remove(&mut self, id: &WindowId) -> Option<Box<WindowState>> {
        let WindowEntry {
            value,
            tabs,
            epochs,
        } = self.values.remove(id)?;
        drop(tabs);
        drop(epochs);
        *value.routing.signal().0.owner.borrow_mut() = None;
        self.pending.borrow_mut().remove(id);
        Some(value)
    }
    pub(crate) fn get(&self, id: &WindowId) -> Option<&WindowState> {
        self.values.get(id).map(|entry| entry.value.as_ref())
    }
    pub(crate) fn contains_key(&self, id: &WindowId) -> bool {
        self.values.contains_key(id)
    }
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub(crate) fn keys(&self) -> WindowKeys<'_> {
        WindowKeys {
            values: self.values.keys(),
        }
    }
    pub(crate) fn iter(&self) -> WindowReadIter<'_> {
        WindowReadIter {
            values: self.values.iter(),
        }
    }
    pub(crate) fn values(
        &self,
    ) -> impl DoubleEndedIterator<Item = &Box<WindowState>> + ExactSizeIterator {
        self.iter().map(|(_, value)| value)
    }
    pub(crate) fn range<R: std::ops::RangeBounds<WindowId>>(
        &self,
        range: R,
    ) -> impl DoubleEndedIterator<Item = (&WindowId, &Box<WindowState>)> {
        self.values
            .range(range)
            .map(|(id, entry)| (id, &entry.value))
    }
    /// This projection cannot replace a window root or its routing aggregate.
    pub(crate) fn presence_mut(
        &mut self,
        id: &WindowId,
    ) -> Option<&mut crate::presence::WindowView> {
        self.values
            .get_mut(id)
            .map(|entry| &mut entry.value.presence)
    }
    /// Pane geometry is not a recorded route input; canonical front changes use sync_window.
    pub(crate) fn active_tree_mut(&mut self, id: &WindowId) -> Option<&mut crate::pane::PaneTree> {
        let window = &mut self.values.get_mut(id)?.value;
        window.layouts.get_mut(window.tabs.active)
    }
    pub(crate) fn get_mut(&mut self, id: &WindowId) -> Option<WindowMutation<'_>> {
        self.values
            .get_mut(id)
            .map(|entry| WindowMutation::new(*id, &self.pending, &self.titles, entry))
    }
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = WindowMutation<'_>> {
        self.iter_mut().map(|(_, window)| window)
    }
    pub(crate) fn iter_mut(&mut self) -> WindowIterMut<'_> {
        WindowIterMut {
            values: self.values.iter_mut(),
            pending: &self.pending,
            titles: &self.titles,
        }
    }
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        let ids: Vec<_> = self.values.keys().copied().collect();
        for id in ids {
            self.remove(&id);
        }
    }
    pub(crate) fn title_authority(&self) -> &crate::tab_model::TitleAuthority {
        &self.titles
    }
    pub(crate) fn take_title_pending(&mut self) -> std::collections::BTreeSet<u64> {
        self.titles.take_pending()
    }
    pub(crate) fn take_pending(&self, option_as_meta: bool) -> Vec<WindowId> {
        if self.option_as_meta.replace(Some(option_as_meta)) != Some(option_as_meta) {
            self.invalidate_all();
        }
        std::mem::take(&mut *self.pending.borrow_mut())
            .into_iter()
            .filter_map(|(id, signal)| {
                let signal = signal.upgrade()?;
                signal.queued.set(false);
                self.values
                    .get(&id)
                    .filter(|entry| Rc::ptr_eq(&entry.value.routing.signal().0, &signal))
                    .map(|_| id)
            })
            .collect()
    }
    pub(crate) fn invalidate_all(&self) {
        for entry in self.values.values() {
            entry.value.routing.signal().invalidate();
        }
    }
}
impl std::ops::Index<&WindowId> for Windows {
    type Output = Box<WindowState>;
    fn index(&self, id: &WindowId) -> &Self::Output {
        &self.values[id].value
    }
}

pub(crate) struct WindowKeys<'a> {
    values: std::collections::btree_map::Keys<'a, WindowId, WindowEntry>,
}
impl<'a> Iterator for WindowKeys<'a> {
    type Item = &'a WindowId;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl DoubleEndedIterator for WindowKeys<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.values.next_back()
    }
}
impl ExactSizeIterator for WindowKeys<'_> {}

pub(crate) struct WindowReadIter<'a> {
    values: std::collections::btree_map::Iter<'a, WindowId, WindowEntry>,
}
impl<'a> Iterator for WindowReadIter<'a> {
    type Item = (&'a WindowId, &'a Box<WindowState>);
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next().map(|(id, entry)| (id, &entry.value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl DoubleEndedIterator for WindowReadIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.values
            .next_back()
            .map(|(id, entry)| (id, &entry.value))
    }
}
impl ExactSizeIterator for WindowReadIter<'_> {}
impl<'a> IntoIterator for &'a Windows {
    type Item = (&'a WindowId, &'a Box<WindowState>);
    type IntoIter = WindowReadIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a> IntoIterator for &'a mut Windows {
    type Item = (&'a WindowId, WindowMutation<'a>);
    type IntoIter = WindowIterMut<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

/// Ordinary mutable loans retain private custody and compare payload identities only.
pub(crate) struct WindowMutation<'a> {
    entry: &'a mut WindowEntry,
    pending: &'a Pending,
    id: WindowId,
    original: Signal,
    titles: &'a crate::tab_model::TitleAuthority,
}
impl<'a> WindowMutation<'a> {
    fn new(
        id: WindowId,
        pending: &'a Pending,
        titles: &'a crate::tab_model::TitleAuthority,
        entry: &'a mut WindowEntry,
    ) -> Self {
        #[cfg(test)]
        if crate::title_authority::faulty(crate::title_authority::Fault::GeneralSnapshot) {
            for tab in entry.value.tab_set.tabs() {
                let _ = std::hint::black_box(tab.focus);
                crate::work_counts::title_drift_tab_visited();
            }
        }
        #[cfg(test)]
        if GENERAL_SNAPSHOT_MUTANT.with(Cell::get) {
            let _ = entry.value.routing.classify_snapshot_for_negative_control();
        }
        Self {
            original: entry.value.routing.signal().clone(),
            entry,
            pending,
            id,
            titles,
        }
    }
}
impl Deref for WindowMutation<'_> {
    type Target = Box<WindowState>;
    fn deref(&self) -> &Self::Target {
        &self.entry.value
    }
}
impl DerefMut for WindowMutation<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entry.value
    }
}
impl Drop for WindowMutation<'_> {
    fn drop(&mut self) {
        #[cfg(test)]
        let omit_title_roots = crate::title_authority::faulty(crate::title_authority::Fault::Root);
        #[cfg(not(test))]
        let omit_title_roots = false;
        if !omit_title_roots {
            let tabs = self.entry.value.tab_set.title_owner();
            if !self.entry.tabs.same_identity(&tabs) {
                self.entry
                    .value
                    .tab_title_epochs
                    .invalidate_membership_evidence();
                self.entry.tabs = self
                    .entry
                    .value
                    .tab_set
                    .bind_title_owner(self.id.0, self.titles);
            }
            let epochs = self.entry.value.tab_title_epochs.owner();
            if !self.entry.epochs.same_identity(&epochs) {
                self.entry.epochs = self
                    .entry
                    .value
                    .tab_title_epochs
                    .bind_owner(self.id.0, self.titles);
            }
        }
        if !Rc::ptr_eq(&self.original.0, &self.entry.value.routing.signal().0) {
            let mut owner = self.original.0.owner.borrow_mut();
            if owner.as_ref().is_some_and(|(id, queue)| {
                *id == self.id && queue.ptr_eq(&Rc::downgrade(self.pending))
            }) {
                *owner = None;
            }
            self.pending.borrow_mut().remove(&self.id);
            self.entry
                .value
                .routing
                .signal()
                .bind(self.id, self.pending);
        }
    }
}

pub(crate) struct WindowIterMut<'a> {
    values: std::collections::btree_map::IterMut<'a, WindowId, WindowEntry>,
    pending: &'a Pending,
    titles: &'a crate::tab_model::TitleAuthority,
}
impl<'a> Iterator for WindowIterMut<'a> {
    type Item = (&'a WindowId, WindowMutation<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next().map(|(id, entry)| {
            (
                id,
                WindowMutation::new(*id, self.pending, self.titles, entry),
            )
        })
    }
}

/// Native state and its front-window subscription share one ownership boundary.
#[derive(Default)]
pub(crate) struct Views(BTreeMap<crate::tab_model::ViewId, NativeState>);
struct NativeState {
    value: crate::native_app::AppViewState,
    subscriber: Option<Signal>,
    #[cfg(test)]
    classifications: Cell<u64>,
}
impl NativeState {
    fn invalidate(&self) {
        if let Some(signal) = &self.subscriber {
            signal.invalidate();
        }
    }
}
impl Views {
    #[cfg(test)]
    pub(crate) fn classifier_work(&self) -> Vec<(crate::tab_model::ViewId, u64)> {
        self.0
            .iter()
            .map(|(id, state)| (*id, state.classifications.get()))
            .collect()
    }
    pub(crate) fn bind(&mut self, view: crate::tab_model::ViewId, signal: Signal) {
        if let Some(state) = self.0.get_mut(&view) {
            state.subscriber = Some(signal);
        }
    }
    pub(crate) fn unbind(&mut self, view: crate::tab_model::ViewId, signal: &Signal) {
        if let Some(state) = self.0.get_mut(&view)
            && state
                .subscriber
                .as_ref()
                .is_some_and(|owner| Rc::ptr_eq(&owner.0, &signal.0))
        {
            state.subscriber = None;
        }
    }
    pub(crate) fn get(
        &self,
        view: &crate::tab_model::ViewId,
    ) -> Option<&crate::native_app::AppViewState> {
        self.0.get(view).map(|state| &state.value)
    }
    pub(crate) fn contains_key(&self, view: &crate::tab_model::ViewId) -> bool {
        self.0.contains_key(view)
    }
    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (&crate::tab_model::ViewId, &crate::native_app::AppViewState)> {
        self.0.iter().map(|(id, state)| (id, &state.value))
    }
    pub(crate) fn values(&self) -> impl Iterator<Item = &crate::native_app::AppViewState> {
        self.0.values().map(|state| &state.value)
    }
    pub(crate) fn get_mut(&mut self, view: &crate::tab_model::ViewId) -> Option<ViewMutation<'_>> {
        let state = self.0.get_mut(view)?;
        let before = editor_route(state);
        Some(ViewMutation { state, before })
    }
    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = ViewMutation<'_>> {
        self.0.values_mut().map(|state| {
            let before = editor_route(state);
            ViewMutation { state, before }
        })
    }
    pub(crate) fn insert(
        &mut self,
        view: crate::tab_model::ViewId,
        value: crate::native_app::AppViewState,
    ) -> Option<crate::native_app::AppViewState> {
        let subscriber = self.0.get_mut(&view).and_then(|old| {
            old.invalidate();
            old.subscriber.take()
        });
        self.0
            .insert(
                view,
                NativeState {
                    value,
                    subscriber,
                    #[cfg(test)]
                    classifications: Cell::new(0),
                },
            )
            .map(|state| state.value)
    }
    pub(crate) fn remove(
        &mut self,
        view: &crate::tab_model::ViewId,
    ) -> Option<crate::native_app::AppViewState> {
        let state = self.0.remove(view)?;
        state.invalidate();
        Some(state.value)
    }
}

fn editor_route(state: &NativeState) -> bool {
    #[cfg(test)]
    state.classifications.set(state.classifications.get() + 1);
    state.value.editor_has_typing_focus()
}

pub(crate) struct ViewMutation<'a> {
    state: &'a mut NativeState,
    before: bool,
}
impl Deref for ViewMutation<'_> {
    type Target = crate::native_app::AppViewState;
    fn deref(&self) -> &Self::Target {
        &self.state.value
    }
}
impl DerefMut for ViewMutation<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state.value
    }
}
impl Drop for ViewMutation<'_> {
    fn drop(&mut self) {
        if self.before != editor_route(self.state) {
            self.state.invalidate();
        }
    }
}
