// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit, owned end-to-end release capability evidence. This never runs from
//! background convergence and never publishes a GitHub draft.

mod apple;
mod artifact;
mod credentials;
mod encoding;
mod engine;
mod http;
mod journal;
mod model;
mod native;
mod port;
mod remote;
mod request;
mod scope;

pub use port::{EffectError, Git, Invocation, Node};
pub use request::{Mode, Request};

use crate::{Error, Result, client::Paths};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Execute only an explicitly named capability transaction. Native mutations are confined
/// to its fixed scratch payload and owned remote objects; this never publishes a release.
///
/// # Errors
/// Invalid or changed custody/authority, unsafe evidence paths, unavailable native tools,
/// malformed responses, foreign remote objects and failed durable writes are refused.
pub fn execute(paths: &Paths, request: &Request, git: Arc<dyn Git>) -> Result<aterm_json::Value> {
    if request.mode == Mode::Status {
        let store = journal::Store::open(&request.evidence_dir, &request.nonce, None)?;
        let journal = store
            .load()?
            .ok_or_else(|| Error::new("probe has no recorded journal"))?;
        return report(&journal, "historical_unrevalidated", false);
    }
    if !cfg!(target_os = "macos") {
        return Err(Error::new("this binary capability probe requires macOS"));
    }
    let deadline = Instant::now() + Duration::from_secs(20 * 60);
    let scope = scope::Scope {
        paths: paths.clone(),
        git,
    };
    let seed = if request.mode == Mode::Run {
        let (subject, _) = scope.current(deadline)?;
        let now =
            aterm_time::unix_millis().ok_or_else(|| Error::new("probe clock is unavailable"))?;
        Some(model::Journal {
            nonce: request.nonce.clone(),
            subject,
            repositories: request
                .owners
                .iter()
                .cloned()
                .map(model::Repository::new)
                .collect(),
            created_ms: now,
            updated_ms: now,
            artifact: None,
            submission: None,
            apple_accepted: false,
            pending: None,
            cleanup_only: false,
        })
    } else {
        None
    };
    let mut store = journal::Store::open(&request.evidence_dir, &request.nonce, seed.as_ref())?;
    let mut journal = store
        .load()?
        .ok_or_else(|| Error::new("probe has no reserved journal"))?;
    scope.require(&journal.subject, deadline)?;
    // Refuse an unauthorized owner before signing or submitting anything.
    for index in 0..journal.repositories.len() {
        credentials::route(&scope, &journal, index, deadline)?;
    }
    if request.mode == Mode::Cleanup && !journal.cleanup_only {
        journal.cleanup_only = true;
        store.save(&journal)?;
    }
    let mut host = native::Native::new(scope, request.evidence_dir.clone());
    loop {
        match engine::step(&mut journal, &mut host, &mut store, deadline)? {
            engine::Progress::Advanced => {}
            engine::Progress::Complete => return report(&journal, "complete", true),
            engine::Progress::HistoricalComplete => {
                return report(&journal, "historical_complete_unrevalidated_uploads", true);
            }
            engine::Progress::Clean => return report(&journal, "clean", true),
            engine::Progress::Pending(reason) => {
                crate::log_line(&format!("aterm release probe: pending reason={reason}"));
                if Instant::now() >= deadline {
                    return report(&journal, reason, false);
                }
                std::thread::sleep(
                    Duration::from_secs(60).min(deadline.saturating_duration_since(Instant::now())),
                );
            }
        }
    }
}

// Call sites supply fixed stage names. Never include an error's free-form message:
// lower-level errors can contain paths, response bodies or credential information.
fn diagnostic(stage: &'static str, error: &Error) {
    crate::log_line(&format!(
        "aterm release probe: preflight stage={stage} kind={:?}",
        error.kind()
    ));
}

fn diagnosed<T>(stage: &'static str, result: Result<T>) -> Result<T> {
    result.inspect_err(|error| diagnostic(stage, error))
}

fn report(journal: &model::Journal, state: &str, current: bool) -> Result<aterm_json::Value> {
    let recorded = aterm_json::from_slice::<aterm_json::Value>(&encoding::encode(journal)?)
        .map_err(|_| Error::new("cannot encode probe report"))?;
    Ok(
        aterm_json::json!({"state":state,"current_authority_and_remote_checks":current,
        "fresh_binary_publication_proof":state=="complete",
        "credential_route":"authenticated_default_alab_publication",
        "ordinary_git_route":"current_configuration","local_evidence_retained":true,
        "observed_unix_ms":aterm_time::unix_millis(),"evidence":recorded}),
    )
}
