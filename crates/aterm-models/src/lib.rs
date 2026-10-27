// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Every aterm model in one binary.

use aterm_spec::derive::Model;

/// A domain crate and one model constructor it exports.
type Domain = (&'static str, fn() -> Model);

/// One model of each domain crate.
const DOMAINS: [Domain; 15] = [
    ("aterm-models-agh", aterm_models_agh::agh_route_wall_model),
    ("aterm-models-db", aterm_models_db::offline_rights_model),
    (
        "aterm-models-aos",
        aterm_models_aos::aos_boundary_publication_model,
    ),
    (
        "aterm-models-atpkg",
        aterm_models_atpkg::atpkg_contention_release_park_model,
    ),
    (
        "aterm-models-core",
        aterm_models_core::resize_row_reuse_model,
    ),
    (
        "aterm-models-effects",
        aterm_models_effects::console_cursor_home_model,
    ),
    (
        "aterm-models-gui",
        aterm_models_gui::chrome_regrow_floor_model,
    ),
    (
        "aterm-models-harness",
        aterm_models_harness::claude_idle_at_composer_model,
    ),
    (
        "aterm-models-misc",
        aterm_models_misc::trail_audio_lifecycle_model,
    ),
    (
        "aterm-models-native",
        aterm_models_native::control_connection_admission_model,
    ),
    (
        "aterm-models-release",
        aterm_models_release::release_publish_once_model,
    ),
    (
        "aterm-models-render",
        aterm_models_render::resize_render_model,
    ),
    (
        "aterm-models-session",
        aterm_models_session::broadcast_cursor_checkpoint_model,
    ),
    ("aterm-models-setup", aterm_models_setup::setup_ledger_model),
    (
        "aterm-models-update",
        aterm_models_update::native_update_handoff_bind_retry_model,
    ),
];

/// Link every domain crate's models into the calling binary, and return the domain crates.
///
/// # Panics
/// When a domain crate registered no model in this binary.
#[must_use]
pub fn link_all() -> Vec<&'static str> {
    let domains = std::hint::black_box(DOMAINS);
    let locations: Vec<&'static str> =
        aterm_spec::registry::iter::<aterm_spec::xref::RegisteredModel>()
            .map(|r| r.location)
            .collect();
    domains
        .iter()
        .map(|(krate, model)| {
            let name = model().name;
            let prefix = format!("crates/{krate}/");
            assert!(
                locations.iter().any(|at| at.starts_with(&prefix)),
                "{krate} registered no model here (not even `{name}`): its models did not link"
            );
            *krate
        })
        .collect()
}
