// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The one door a Proton mailbox's address list leaves the code-fetch path through
//! (`docs/DESIGN-alab-lab-model-2026-11-01.md` §4.10, step 10). When the harness has a mailbox open
//! to read a sign-in code, it hands the list that mailbox's own unlock already read to [`note`]: no
//! request is made for it, and nothing here reads a mailbox that was not open anyway. The `aterm`
//! executable fills the hook with [`install`] in a release build, where it becomes the lab
//! registry's `mailbox-obs` row; until then, and in every test, nothing is published.

use std::sync::OnceLock;

/// One address of an open mailbox's list.
#[derive(Clone, PartialEq, Eq)]
pub struct Address {
    pub email: String,
    /// The account's own address (its original, the short-domain twin, an outside sign-in
    /// address), which stands for the mailbox's login.
    pub own: bool,
    /// Mail sent to it lands in this mailbox.
    pub receives: bool,
}

/// One open mailbox's address list. No `Debug` shows an address or the login.
#[derive(Clone, PartialEq, Eq)]
pub struct Seen {
    /// The address the harness signed in to the mailbox with.
    pub login: String,
    /// The address whose code the mailbox was opened to read.
    pub target: Option<String>,
    pub addresses: Vec<Address>,
    /// The mailbox returned its whole list.
    pub complete: bool,
}

impl std::fmt::Debug for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seen")
            .field("target", &self.target.is_some())
            .field("addresses", &self.addresses.len())
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}

type Sink = Box<dyn Fn(&Seen) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Fills the hook, once per process: the lab registry's discovery, at the composing executable's
/// start. Returns whether this call filled it. No test calls it, so no test publishes.
pub fn install(sink: Sink) -> bool {
    SINK.set(sink).is_ok()
}

/// Hands one open mailbox's list on. The sink returns at once; whatever it writes runs elsewhere,
/// so a sign-in never waits on it.
pub fn note(seen: &Seen) {
    #[cfg(any(test, debug_assertions))]
    CAPTURE.with(|held| {
        if let Some(noted) = held.borrow_mut().as_mut() {
            noted.push(seen.clone());
        }
    });
    if let Some(sink) = SINK.get() {
        sink(seen);
    }
}

/// The list of the mailbox `mailbox` holds, once its unlock read one: what the code-fetch path
/// notes on opening it. A mailbox another reader holds this instant is passed over, since the
/// list is only worth what costs nothing.
pub fn of_open(
    mailbox: &std::sync::Mutex<aterm_mail::proton::Proton>,
    login: &str,
    target: Option<&str>,
) -> Option<Seen> {
    let held = mailbox.try_lock().ok()?;
    let list = held.address_list()?;
    Some(Seen {
        login: login.to_owned(),
        target: target.map(str::to_owned),
        addresses: list
            .addresses
            .iter()
            .map(|listed| Address {
                email: listed.email.clone(),
                own: listed.kind.own(),
                receives: listed.receives,
            })
            .collect(),
        complete: list.complete,
    })
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Vec<Seen>>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: runs `body` and returns what it noted on this thread.
#[cfg(any(test, debug_assertions))]
pub fn captured<R>(body: impl FnOnce() -> R) -> (R, Vec<Seen>) {
    struct Reset(Option<Option<Vec<Seen>>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                let _ = CAPTURE.try_with(|held| {
                    *held.borrow_mut() = previous;
                });
            }
        }
    }
    let previous = CAPTURE.with(|held| held.borrow_mut().replace(Vec::new()));
    let mut reset = Reset(Some(previous));
    let out = body();
    let previous = reset.0.take().unwrap_or_default();
    let noted = CAPTURE
        .with(|held| std::mem::replace(&mut *held.borrow_mut(), previous).unwrap_or_default());
    (out, noted)
}

#[cfg(test)]
#[path = "mailbox_seen_tests.rs"]
mod tests;
