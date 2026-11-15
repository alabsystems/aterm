// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The App's side of Settings ▸ This Mac (`docs/DESIGN-machine-names-2026-11-04.md` §5.6): which
//! views show it, what its requests do, and the state it is published with. On macOS a request
//! goes to the window's name writer ([`crate::machine_title::page`]); elsewhere it writes
//! nothing, and the page names `aterm name` on Linux and says other platforms keep no machine
//! name yet.

use crate::native_app::{AppEvent, MachineRequest};
use crate::native_settings::{MachinePageState, SettingsRoute};

/// What the App keeps for Settings ▸ This Mac.
#[derive(Debug, Default)]
pub(crate) struct MachinePageHost {
    /// How many page reads were asked for: only the newest one's lands.
    #[cfg(target_os = "macos")]
    pub(crate) reads: u64,
    /// How many page writes have started: each failure names its own, so one a later write
    /// superseded is dropped.
    #[cfg(target_os = "macos")]
    pub(crate) sets: u64,
    /// What the page shows, as last published.
    pub(crate) state: MachinePageState,
    /// The published state's revision.
    revision: u64,
}

/// What the page says on Linux, where it edits nothing: what changes this machine's name.
#[cfg(target_os = "linux")]
const UNSUPPORTED: &str = "`aterm name` in a terminal changes this machine's number and nickname";
/// What the page says on the other platforms, which keep no machine name to change.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const UNSUPPORTED: &str = "this platform keeps no machine name yet";

impl crate::App {
    /// Whether native view `view` is a Settings view on Settings ▸ This Mac.
    pub(crate) fn machine_page_in(&self, view: crate::tab_model::ViewId) -> bool {
        matches!(self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == SettingsRoute::Machine)
    }

    /// Whether some Settings view holds Settings ▸ This Mac, on screen or not: a background tab
    /// or a covered window still shows its read when it comes back to the front.
    #[cfg(target_os = "macos")]
    pub(crate) fn machine_page_held(&self) -> bool {
        self.view_store
            .iter()
            .any(|(view, _)| self.machine_page_in(view))
    }

    /// One of the page's requests ([`crate::native_app::AppEffect::Machine`]).
    pub(crate) fn execute_machine_page(&mut self, request: MachineRequest) {
        #[cfg(target_os = "macos")]
        self.machine_page_request(request);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = request;
            self.machine_page.state.read =
                crate::native_settings::PageRead::Unsupported(UNSUPPORTED);
            self.machine_page_published();
        }
    }

    /// Publish the page's state to every Settings view, and whether this instance has windows
    /// that Authorize Naming…'s tab can open in.
    pub(crate) fn machine_page_published(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.machine_page.state.windowed = !self.headless;
        }
        self.machine_page.revision = self.machine_page.revision.wrapping_add(1);
        let revision = self.machine_page.revision;
        if self
            .native_runtime
            .replace_settings_machine(self.machine_page.state.clone(), revision)
        {
            self.dispatch_to_settings_views(&AppEvent::MachineChanged { revision });
        }
    }
}
