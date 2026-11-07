// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The allowed direct path (`docs/DESIGN-lan-broker-2026-11-05.md` §5): while no broker is
//! installed, a process whose code macOS already allowed on the local network (for its account,
//! as the app macOS holds responsible for it) serves the broker's protocol to itself,
//! in-process, over a socketpair, so a Mac updated from a build that had no broker (0.158.0 and
//! earlier, whose aterm a person once allowed) keeps its LAN until the broker is installed, with
//! no password and no alert (`docs/measured/lan-update-2026-11-06.md`).
//!
//! This crate is below the broker's server (`aterm-lan`), so the server and the reading of
//! macOS's answer come from above: the `aterm` executable [`provide`]s them once, at start. A
//! process that provided none (a test binary, a tool) has no direct path, as before. The path is
//! taken only for [`super::Unavailable::NotInstalled`]: a broker that answers in another protocol
//! or refuses this process is a broker, and its own words stand. It serves LAN uses, never a
//! question about the broker itself (`super::Arrivals`, which hears a broker arrive).

use std::sync::OnceLock;

use crate::CtlStream;

/// What serves the broker's protocol in this process: the broker's own server, under this
/// process's code, and its reading of whether macOS allowed that code.
pub trait Direct: Send + Sync {
    /// Whether macOS already answered Allow for this process's code on the local network, read
    /// from macOS's own record and asking nothing: the answer's words, or why there is none. Only
    /// an answered Allow is an allowance, so a process that serves itself after one raises no
    /// Local Network alert.
    ///
    /// # Errors
    /// No allowance, in words.
    fn allowed(&self) -> Result<String, String>;

    /// Serve one connection of the broker's protocol on `stream` until either end closes. Called
    /// on a thread of its own.
    fn serve(&self, stream: CtlStream);
}

static PROVIDED: OnceLock<&'static dyn Direct> = OnceLock::new();

/// Give this process its direct path (once; a second provision is ignored and says so).
///
/// # Errors
/// A direct path was already provided.
pub fn provide(direct: &'static dyn Direct) -> Result<(), &'static str> {
    PROVIDED
        .set(direct)
        .map_err(|_| "this process's direct LAN path was already provided")
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SEAM: std::cell::Cell<Option<&'static dyn Direct>> = const { std::cell::Cell::new(None) };
}

/// `f` with `direct` as this thread's direct path, then the thread's own again: the test arm (a
/// test binary otherwise has none), carried into the thread that serves it.
#[cfg(any(test, debug_assertions))]
pub fn seam_here<R>(direct: &'static dyn Direct, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<&'static dyn Direct>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SEAM.with(|here| here.set(self.0));
        }
    }
    let _restore = Restore(SEAM.with(|here| here.replace(Some(direct))));
    f()
}

/// This thread's direct path: a test's seam, else the provided one. A test binary never reads
/// the provided path, so no test reaches the person's network through it.
fn current() -> Option<&'static dyn Direct> {
    #[cfg(any(test, debug_assertions))]
    if let Some(seamed) = SEAM.with(std::cell::Cell::get) {
        return Some(seamed);
    }
    if crate::test_binary::scratch_root().is_some() {
        return None;
    }
    PROVIDED.get().copied()
}

/// The near end of an in-process session and macOS's allowance in words, when this process has a
/// direct path and macOS allowed its code, the far end served on a thread of its own; otherwise
/// why not (`None`: no direct path here).
pub(super) fn open() -> Result<(CtlStream, String), Option<String>> {
    let direct = current().ok_or(None)?;
    let allowed = direct.allowed().map_err(Some)?;
    let (near, far) = CtlStream::pair().map_err(|e| Some(format!("a socketpair: {e}")))?;
    #[cfg(any(test, debug_assertions))]
    let lan = super::seam();
    let spawned = std::thread::Builder::new()
        .name("lan-direct".into())
        .spawn(move || {
            #[cfg(any(test, debug_assertions))]
            super::seam_here(lan, || direct.serve(far));
            #[cfg(not(any(test, debug_assertions)))]
            direct.serve(far);
        });
    spawned.map_err(|e| Some(format!("no thread to serve this process's LAN: {e}")))?;
    Ok((near, allowed))
}
