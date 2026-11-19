// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Opt-in local timing for the normal aRegistry reader and compiled machine picker.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{MACHINE, ReadError, RegistryApp, RegistryViewState};
use crate::native_ui::{LogicalRect, SemanticRole, UiKey};
use crate::qos::Role;
use crate::tables_store::Paths;

/// Measure one open against this Mac's existing local inventory. Return only aggregate text.
/// A fresh process gets a fresh reader. The caller chooses whether to start this observation.
///
/// # Errors
/// The local replica or a complete, usable machine picker could not be observed.
pub fn measure_local_registry() -> Result<String, String> {
    let paths = Paths::locate().map_err(|_| "status=unavailable".to_owned())?;
    measure(paths)
}

fn measure(paths: Paths) -> Result<String, String> {
    // Check names only. An authenticated descriptor can establish Commons custody on its
    // first ordinary catalog read; the timing probe is for a replica already in use.
    if !aterm_secrets::commons::bootstrap::custody_opened(&paths)
        .map_err(|_| "status=unavailable".to_owned())?
    {
        return Err("status=unavailable".to_owned());
    }
    let view = RegistryViewState::default();
    let (signal, wakes) = mpsc::channel();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(60);
    let mut app = RegistryApp::new(Some(paths), Role::Responsive, move || {
        let _ = signal.send(Instant::now());
    });
    loop {
        if let Some(error) = &app.error {
            let status = match error {
                ReadError::Incomplete => "incomplete",
                ReadError::Store(error)
                    if error.repair_cause()
                        == aterm_types::network_state::RepairCause::NotReady =>
                {
                    "transient"
                }
                ReadError::Store(_) | ReadError::Unavailable(_) => "unavailable",
            };
            return Err(format!(
                "status={status} elapsed_ms={}",
                start.elapsed().as_millis()
            ));
        }
        let woke = wakes
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| {
                let status = match error {
                    mpsc::RecvTimeoutError::Timeout => "timeout",
                    mpsc::RecvTimeoutError::Disconnected => "unavailable",
                };
                format!("status={status} elapsed_ms={}", start.elapsed().as_millis())
            })?;
        app.poll();
        if let Some(rows) = app.rows.as_ref() {
            let width = 960.0;
            let height = 760.0;
            let ui = app
                .page(&view, width, height)
                .compile(LogicalRect::new(0.0, 0.0, width, height))
                .map_err(|_| "status=invalid-page".to_owned())?;
            let status =
                if rows.is_empty() && ui.semantic(&UiKey::new("registry/list/empty")).is_some() {
                    "empty"
                } else if ui.semantics.iter().any(|node| {
                    node.key.as_str().starts_with(MACHINE)
                        && node.role == SemanticRole::ListBoxOption
                        && node.state.is_some_and(|state| state.enabled)
                        && node
                            .action
                            .as_ref()
                            .is_some_and(|action| action.as_str() == node.key.as_str())
                        && !node.offscreen
                }) {
                    "selectable"
                } else {
                    return Err("status=invalid-page".to_owned());
                };
            return Ok(format!(
                "status={status} machines={} reader_woke_ms={} page_ready_ms={}",
                rows.len(),
                woke.duration_since(start).as_millis(),
                start.elapsed().as_millis()
            ));
        }
    }
}

#[cfg(test)]
#[path = "registry_open_measure_tests.rs"]
mod tests;
