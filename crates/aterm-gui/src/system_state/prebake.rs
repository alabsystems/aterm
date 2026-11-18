// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup screen's images, baked before they are drawn (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §4.5). Nothing bakes on the thread that paints: the raster draws only what is kept
//! ([`crate::widget::ImageArt::kept`]), and each System State view asks here, as its
//! [`Requester`], for the images of the moment it wants. Each view's newest request replaces its
//! own older one and no other view's, so a live resize that asks at every width bakes the width
//! it ends on, not each width it passed, and two views (two windows at different widths or
//! display scales) never drop each other's bakes. One worker bakes what the views want, one image
//! at a time, the newest request's first; each bake that lands wakes the window, and the view
//! draws it. Meanwhile the view draws the moment it last drew where that moment was laid out
//! after the same words at the same size on the same ground, else the words alone
//! ([`super::lockup::LockupSpec::paint`]).
//!
//! What is kept is what the live views want and last drew, nothing older: each drawing pass
//! records what it drew ([`Requester::drew`]) and the bakes no view wants or drew go, so a run's
//! superseded moments (every step is a new road) never pile up. A view that closes takes its
//! wants with it, and one whose lockup draws no images (Serious Mode, High Contrast, a pane too
//! narrow or a window too short for the road) wants and keeps none ([`Requester::none`]). Once no
//! System State view is on screen (a tab behind another, a window minimised or covered) nothing
//! is queued and each view keeps only the moment it last drew ([`set_shown`]), until one is
//! again: the page that comes back with its words, their size and its ground as they were
//! draws the moment it left at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::widget::ImageArt;

/// An image to bake: the art and its device width and height.
pub(crate) type Key = (ImageArt, u32, u32);

/// Where the worker's bakes come from and go: the image cache and the window in the app, a
/// recording stand-in in a test.
pub(crate) trait Oven: Send + Sync + 'static {
    /// Whether `key` is baked and kept.
    fn kept(&self, key: Key) -> bool;
    /// Bake `key` (the slow part, with no lock held).
    fn bake(&self, key: Key) -> Arc<[u8]>;
    /// Keep a bake that landed.
    fn keep(&self, key: Key, pixels: Arc<[u8]>);
    /// A bake was kept: wake whoever draws it.
    fn landed(&self);
    /// Drop every bake kept but `wanted`.
    fn retain(&self, wanted: &[Key]);
}

/// What one view asked for and drew.
#[derive(Default)]
struct Want {
    /// Its newest request: the images of the moment it wants.
    wanted: Vec<Key>,
    /// The images it drew last.
    drawn: Vec<Key>,
    /// Its newest request's images not yet baked, in its order.
    todo: Vec<Key>,
    /// When it asked: the newest request bakes first.
    stamp: u64,
}

/// The views' wants and the one worker's state.
#[derive(Default)]
struct Queue {
    /// Each live view's, by its [`Requester`] id.
    wants: Vec<(u64, Want)>,
    /// The requests' clock.
    clock: u64,
    /// The worker is alive; a request finding it so only queues its images.
    working: bool,
    /// No System State view is on screen: requests are noted, nothing bakes, and only what each
    /// view last drew is kept.
    hidden: bool,
    /// Bumped by each release: a bake begun before it is not kept.
    epoch: u64,
}

impl Queue {
    /// Every image a live view drew last.
    fn drawn(&self) -> Vec<Key> {
        let mut keys: Vec<Key> = Vec::new();
        for (_, want) in &self.wants {
            for key in &want.drawn {
                if !keys.contains(key) {
                    keys.push(*key);
                }
            }
        }
        keys
    }

    /// Every image a live view wants or drew.
    fn wanted(&self) -> Vec<Key> {
        let mut keys: Vec<Key> = Vec::new();
        for (_, want) in &self.wants {
            for key in want.wanted.iter().chain(&want.drawn) {
                if !keys.contains(key) {
                    keys.push(*key);
                }
            }
        }
        keys
    }

    fn want(&mut self, who: u64) -> &mut Want {
        let at = match self.wants.iter().position(|(id, _)| *id == who) {
            Some(at) => at,
            None => {
                self.wants.push((who, Want::default()));
                self.wants.len() - 1
            }
        };
        &mut self.wants[at].1
    }

    /// The next image to bake: the first left of the newest request with any left.
    fn next(&mut self) -> Option<Key> {
        let want = self
            .wants
            .iter_mut()
            .map(|(_, want)| want)
            .filter(|want| !want.todo.is_empty())
            .max_by_key(|want| want.stamp)?;
        let key = want.todo.remove(0);
        for (_, want) in &mut self.wants {
            want.todo.retain(|queued| *queued != key);
        }
        Some(key)
    }
}

/// One worker baking what the views want.
pub(crate) struct Baker<O: Oven> {
    oven: O,
    queue: Mutex<Queue>,
    /// Bake on the asking thread instead of a worker's (a test's own cache is its thread's).
    inline: bool,
}

impl<O: Oven> Baker<O> {
    pub(crate) fn new(oven: O) -> Arc<Self> {
        Arc::new(Self {
            oven,
            queue: Mutex::new(Queue::default()),
            inline: false,
        })
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// View `who` wants `keys`: they replace its older request, each not kept is queued, and
    /// the worker starts if it is not running.
    pub(crate) fn want(self: &Arc<Self>, who: u64, keys: &[Key]) {
        let mut queue = self.queue();
        queue.clock += 1;
        let stamp = queue.clock;
        let want = queue.want(who);
        want.wanted = keys.to_vec();
        want.todo = keys
            .iter()
            .copied()
            .filter(|key| !self.oven.kept(*key))
            .collect();
        want.todo.dedup();
        want.stamp = stamp;
        self.start(queue);
    }

    /// View `who` drew `keys`: the bakes no live view wants or drew go.
    pub(crate) fn drew(&self, who: u64, keys: &[Key]) {
        let mut queue = self.queue();
        queue.want(who).drawn = keys.to_vec();
        self.oven.retain(&queue.wanted());
    }

    /// View `who` draws no images: it wants none and drew none, and the bakes only it wanted or
    /// drew go.
    pub(crate) fn none(&self, who: u64) {
        let mut queue = self.queue();
        let want = queue.want(who);
        if want.wanted.is_empty() && want.drawn.is_empty() && want.todo.is_empty() {
            return;
        }
        *want = Want::default();
        self.oven.retain(&queue.wanted());
    }

    /// View `who` closed: its wants go, and the bakes only it wanted or drew.
    pub(crate) fn forget(&self, who: u64) {
        let mut queue = self.queue();
        queue.wants.retain(|(id, _)| *id != who);
        self.oven.retain(&queue.wanted());
    }

    /// Whether any System State view is on screen. Hidden, nothing is queued, a bake under way
    /// is not kept, and each view keeps only the moment it last drew, so the first pass after it
    /// shows again, its words, their size and its ground as they were, draws that moment, not
    /// the words alone; shown again, what each view last asked for and has not kept is queued
    /// anew.
    pub(crate) fn set_shown(self: &Arc<Self>, shown: bool) {
        let mut queue = self.queue();
        if queue.hidden != shown {
            return;
        }
        queue.hidden = !shown;
        if shown {
            for (_, want) in &mut queue.wants {
                want.todo = want
                    .wanted
                    .iter()
                    .copied()
                    .filter(|key| !self.oven.kept(*key))
                    .collect();
            }
            self.start(queue);
        } else {
            for (_, want) in &mut queue.wants {
                want.todo.clear();
            }
            queue.epoch = queue.epoch.wrapping_add(1);
            self.oven.retain(&queue.drawn());
        }
    }

    /// Start the worker on what is queued, unless it runs, nothing is queued or no view shows.
    fn start(self: &Arc<Self>, mut queue: MutexGuard<'_, Queue>) {
        if queue.working || queue.hidden || queue.wants.iter().all(|(_, want)| want.todo.is_empty())
        {
            return;
        }
        if self.inline {
            drop(queue);
            #[cfg(test)]
            if HELD.with(std::cell::Cell::get) {
                return;
            }
            self.work_inline();
            return;
        }
        queue.working = true;
        let baker = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("aterm-lockup-bake".into())
            .spawn(move || baker.work());
        if started.is_err() {
            // No thread: nothing bakes, and each view keeps drawing what it has (the words alone
            // where nothing was baked); its next request tries again.
            queue.working = false;
            for (_, want) in &mut queue.wants {
                want.todo.clear();
            }
        }
    }

    /// The worker: the newest request's next image, until none is left.
    fn work(&self) {
        loop {
            let (key, epoch) = {
                let mut queue = self.queue();
                let next = if queue.hidden { None } else { queue.next() };
                let Some(key) = next else {
                    queue.working = false;
                    return;
                };
                (key, queue.epoch)
            };
            self.bake_one(key, epoch);
        }
    }

    /// Bake what is queued on this thread (a test's).
    fn work_inline(&self) {
        loop {
            let (key, epoch) = {
                let mut queue = self.queue();
                let Some(key) = queue.next() else {
                    return;
                };
                (key, queue.epoch)
            };
            self.bake_one(key, epoch);
        }
    }

    /// Bake `key` and keep it where, as it lands, no release came since it began and a live
    /// view still wants it.
    fn bake_one(&self, key: Key, epoch: u64) {
        if self.oven.kept(key) {
            return;
        }
        let pixels = self.oven.bake(key);
        let queue = self.queue();
        if queue.epoch == epoch
            && queue
                .wants
                .iter()
                .any(|(_, want)| want.wanted.contains(&key))
        {
            self.oven.keep(key, pixels);
            drop(queue);
            self.oven.landed();
        }
    }

    /// Drop what is queued and every bake kept; a bake under way is not kept.
    pub(crate) fn release(&self) {
        let mut queue = self.queue();
        for (_, want) in &mut queue.wants {
            want.todo.clear();
        }
        queue.epoch = queue.epoch.wrapping_add(1);
        self.oven.retain(&[]);
    }

    /// Whether the worker is running (a test's wait).
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        self.queue().working
    }
}

/// The app's oven: the process's image cache, each landing a wake to the setup screen.
pub(crate) struct Cache;

impl Oven for Cache {
    fn kept(&self, key: Key) -> bool {
        let (art, w, h) = key;
        art.is_kept(w, h)
    }

    fn bake(&self, key: Key) -> Arc<[u8]> {
        let (art, w, h) = key;
        art.bake(w, h).into()
    }

    fn keep(&self, key: Key, pixels: Arc<[u8]>) {
        let (art, w, h) = key;
        art.keep(w, h, pixels);
    }

    fn landed(&self) {
        crate::workstation_setup::send_setup_wake(crate::Wake::LockupBaked);
    }

    fn retain(&self, wanted: &[Key]) {
        crate::widget::retain_images(wanted);
    }
}

/// The process's baker; a test thread's own, which bakes on that thread into its own cache.
fn with_baker<T>(f: impl FnOnce(&Arc<Baker<Cache>>) -> T) -> T {
    #[cfg(not(test))]
    {
        static BAKER: std::sync::LazyLock<Arc<Baker<Cache>>> =
            std::sync::LazyLock::new(|| Baker::new(Cache));
        f(&BAKER)
    }
    #[cfg(test)]
    {
        BAKER.with(f)
    }
}

/// One System State view's part in what is baked and kept: what it wants and what it drew. It
/// goes with its view, and takes what only that view wanted with it.
pub(crate) struct Requester {
    id: u64,
}

impl Default for Requester {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl Drop for Requester {
    fn drop(&mut self) {
        #[cfg(not(test))]
        with_baker(|baker| baker.forget(self.id));
        // A view a test thread holds to its end goes after the thread's baker.
        #[cfg(test)]
        let _ = BAKER.try_with(|baker| baker.forget(self.id));
    }
}

impl Requester {
    /// This view wants the images of `keys` (its moment); those not kept are baked.
    pub(crate) fn want(&self, keys: &[Key]) {
        #[cfg(test)]
        REQUESTS.with(|requests| requests.borrow_mut().push(keys.to_vec()));
        with_baker(|baker| baker.want(self.id, keys));
    }

    /// This view drew `keys`: what no view wants or drew goes.
    pub(crate) fn drew(&self, keys: &[Key]) {
        with_baker(|baker| baker.drew(self.id, keys));
    }

    /// This view's lockup draws no images: it wants and keeps none.
    pub(crate) fn none(&self) {
        with_baker(|baker| baker.none(self.id));
    }
}

/// Whether every one of `keys` is baked and kept.
pub(crate) fn ready(keys: &[Key]) -> bool {
    keys.iter().all(|key| Cache.kept(*key))
}

/// The last System State view closed: drop what is queued and every bake kept.
pub(crate) fn release() {
    with_baker(|baker| baker.release());
}

/// Whether any System State view is on screen ([`Baker::set_shown`]).
pub(crate) fn set_shown(shown: bool) {
    with_baker(|baker| baker.set_shown(shown));
}

#[cfg(test)]
thread_local! {
    /// This test thread's baker, baking on the thread into its cache.
    static BAKER: Arc<Baker<Cache>> = Arc::new(Baker {
        oven: Cache,
        queue: Mutex::new(Queue::default()),
        inline: true,
    });
    /// Every request this test thread made, oldest first.
    static REQUESTS: std::cell::RefCell<Vec<Vec<Key>>> = const { std::cell::RefCell::new(Vec::new()) };
    /// This test thread holds its bakes: a request is queued and nothing bakes until
    /// [`settle`].
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hold this test thread's bakes (`true`) or bake each request at once again.
#[cfg(test)]
pub(crate) fn hold(held: bool) {
    HELD.with(|cell| cell.set(held));
}

/// This test thread's requests so far, taking them.
#[cfg(test)]
pub(crate) fn take_requests() -> Vec<Vec<Key>> {
    REQUESTS.with(|requests| std::mem::take(&mut *requests.borrow_mut()))
}

/// Bake what this test thread's views want and is not kept, as the worker would.
#[cfg(test)]
pub(crate) fn settle() {
    with_baker(|baker| baker.work_inline());
}

#[cfg(test)]
#[path = "prebake_tests.rs"]
mod tests;
