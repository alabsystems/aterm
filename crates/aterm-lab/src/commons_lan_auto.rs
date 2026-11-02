// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A new private identity asks the live default vault, with no supplied approval or password.

use super::{HANG, Res, emit, fail, prepare, write_new};
use aterm_json::json;
use aterm_secrets::client::{self, JoinOptions, JoinPassword, Membership, Paths};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::{Duration, Instant};

/// Retain this new profile: an admitted identity belongs to the live vault.
/// The probe installs no services and supplies no approval or replacement trust.
pub(super) fn run(root: &Path) -> Res {
    if !client::alab_signer_built_in() {
        return Err(fail(
            "automatic ALab admission needs the compiled signing authority",
        ));
    }
    let world = prepare(root, "recipient")?;
    let paths = Paths::for_home(&world.home);
    let initial = client::status(&paths).map_err(fail)?;
    if initial.config.is_some() || initial.machine.is_some() {
        return Err(fail(
            "automatic admission needs a new, unconfigured private profile",
        ));
    }
    let label = format!(
        "lab-auto-{}",
        aterm_codec::hex::encode_lower(&aterm_db::crypto::random::<4>().map_err(fail)?)
    );
    emit(&json!({
        "phase":"unconfigured", "world":world.dir.display().to_string(),
        "requested_host":label, "private_identity_present":false,
        "supplied_approval":false, "supplied_password":false, "service_install":false
    }))?;

    let started = Instant::now();
    let deadline = started + HANG;
    let requested = Cell::new(false);
    let events = RefCell::new(Vec::new());
    let joined = client::join_if_unchanged_until(
        &paths,
        &JoinOptions {
            source: String::new(),
            host: Some(label.clone()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| requested.set(true),
            say: &|event| events.borrow_mut().push(event.to_string()),
        },
        None,
        deadline,
    );
    let mut error = joined.as_ref().err().map(ToString::to_string);
    let mut status = client::status(&paths).map_err(fail)?;
    let mut reads = 0_u64;
    let mut pause = Duration::ZERO;
    while joined.is_ok()
        && matches!(status.membership, Membership::Pending)
        && Instant::now() < deadline
    {
        crate::stop::sleep(pause.min(deadline.saturating_duration_since(Instant::now())))?;
        if Instant::now() >= deadline {
            break;
        }
        let read_started = Instant::now();
        reads += 1;
        error = client::sync_reader_until(&paths, deadline)
            .err()
            .map(|error| error.to_string());
        status = client::status(&paths).map_err(fail)?;
        // The ordinary join waits out five seconds after a quick read.
        pause = Duration::from_secs(5).saturating_sub(read_started.elapsed());
    }
    let (membership, sequence) = match status.membership {
        Membership::Unconfigured => ("unconfigured", None),
        Membership::Pending => ("pending", None),
        Membership::Plain => ("plain", None),
        Membership::In { seq, .. } => ("in", Some(seq)),
        Membership::Revoked { seq } => ("revoked", Some(seq)),
    };
    let trusted = status.config.as_ref().is_some_and(client::alab_vault);
    let admitted = trusted && membership == "in";
    let source = status.config.as_ref().map(|config| match &config.source {
        aterm_secrets::transport::Source::Url(url) => aterm_secrets::transport::describe(url),
        aterm_secrets::transport::Source::Path(path) => path.display().to_string(),
    });
    let machine = status
        .machine
        .as_ref()
        .map(|(recipient, code)| json!({"recipient":recipient.to_bech32(),"code":code}));
    let receipt = json!({
        "schema":1, "kind":"default-alab-admission", "world":world.dir.display().to_string(),
        "requested_host":label, "machine":machine, "request_posted":requested.get(),
        "source":source, "trusted_alab":trusted, "membership":membership, "sequence":sequence,
        "vault_admitted":admitted, "materialized_names":status.names.len(),
        "reader_passes":reads, "elapsed_ms":started.elapsed().as_millis() as u64,
        "hang_detector_ms":HANG.as_millis() as u64,
        "supplied_approval":false, "supplied_password":false, "service_install":false,
        "events":events.into_inner(), "last_error":error,
        "commons_admission_tested":false, "first_gui_start_tested":false
    });
    write_new(
        &world.dir.join("auto-join.json"),
        &aterm_json::to_vec(&receipt).map_err(fail)?,
    )?;
    emit(&receipt)?;
    if admitted {
        Ok(0)
    } else {
        Err(fail(
            "default ALab admission was not completed; the private profile and receipt are retained",
        ))
    }
}
