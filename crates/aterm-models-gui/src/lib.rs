// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the window host.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_autopilot_timer;
mod models_chrome_regrow;
mod models_consent;
mod models_crash_journal;
mod models_desktop_alert_debt;
mod models_driver_geometry;
mod models_foreground_handback;
mod models_gui;
mod models_lights;
mod models_main_stall_refusal;
mod models_netprobe;
mod models_output_retention;
mod models_path_confine;
mod models_spare;
mod models_typed_rekey;

pub use models_autopilot_timer::{autopilot_timer_liveness, autopilot_timer_model};
pub use models_chrome_regrow::{chrome_regrow_floor_liveness, chrome_regrow_floor_model};
pub use models_consent::{debugger_access_change_liveness, debugger_access_change_model};
pub use models_crash_journal::crash_journal_claim_model;
pub use models_desktop_alert_debt::desktop_alert_debt_model;
pub use models_driver_geometry::{driver_geometry_liveness, driver_geometry_model};
pub use models_foreground_handback::{
    foreground_handback_model, foreground_handback_ownership_model,
};
pub use models_gui::*;
pub use models_lights::*;
pub use models_main_stall_refusal::main_thread_stall_refusal_model;
pub use models_netprobe::netprobe_shutdown_park_model;
pub use models_output_retention::output_retention_model;
pub use models_path_confine::path_confine_model;
pub use models_spare::spare_lifecycle_model;
pub use models_typed_rekey::typed_rekey_model;
