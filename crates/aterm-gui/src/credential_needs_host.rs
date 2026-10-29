// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS MAC'S CREDENTIAL NEEDS ON THE DIGEST (docs/DESIGN-credential-requests-2026-10-27.md
//! §10): the need ledger states [`Family::Secrets`] as a level, one entry per need that awaits a
//! person, and none for a need the lab is still working on. A need that ends (given, declined,
//! retired) takes its entry down at the next pass. The window reads the ledger's file only
//! when its size or time moved, at most every half minute, and never moves a need. The vault,
//! and so the ledger, runs on unix alone.

use aterm_messages::exceptions::Family;
use std::time::{Duration, Instant, SystemTime};

/// How often the ledger's stamp is looked at.
const EVERY: Duration = Duration::from_secs(30);

/// What the window last stated.
#[derive(Default)]
pub(crate) struct CredentialNeedsHost {
    due: Option<Instant>,
    /// The ledger file's stamp the last statement read (`Some(None)`: no ledger).
    seen: Option<Option<(Option<SystemTime>, u64)>>,
}

impl crate::App {
    /// State this Mac's needs awaiting a person, when the ledger moved.
    pub(crate) fn service_credential_needs(&mut self, now: Instant) {
        if self.credential_needs.due.is_some_and(|due| now < due) {
            return;
        }
        self.credential_needs.due = Some(now + EVERY);
        let Ok(paths) = aterm_secrets::client::Paths::live() else {
            return;
        };
        let stamp = std::fs::metadata(paths.dir.join(aterm_secrets::need::LEDGER))
            .ok()
            .map(|meta| (meta.modified().ok(), meta.len()));
        if self.credential_needs.seen == Some(stamp) {
            return;
        }
        let Ok(needs) = aterm_secrets::need::peek(&paths) else {
            return;
        };
        self.credential_needs.seen = Some(stamp);
        self.except_replace(Family::Secrets, &aterm_secrets::need::exceptions(&needs));
    }
}
