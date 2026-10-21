// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage tap's record across another account signing in to the home.
//!
//! aterm-agent's `accounts::note_reading` keys each reading by the profile the
//! host reads when it reads it (`profile::default_home_key`), not by the login
//! the reading's process was served on, and a running Claude Code adopts a
//! changed keychain item only within about 30 s (the accounts v2 design §5).
//! So a reading under a new key may carry the old account's figure: the record
//! keeps the instant its key changed (`Account::keyed_at`) and takes no figure
//! served before `KEY_ADOPT_S` after it. The tap's file keeps each figure's
//! first report (`tap::Reading::at`), so a figure read late is as old as it is.

use aterm_spec::derive::Model;

/// THE ACCOUNT A USAGE TAP'S RECORD HOLDS (the account-stats design §3.1, §9
/// A1 as built). Two accounts, A (`0`) and B (`1`).
///
/// The world: the home's profile names `home`; another account signs in to it
/// once (`SignIn`: B), and the running process, served on the login `on`,
/// adopts it within the bound (`Adopt`; `adopt` 1 until then). The process is
/// served (`Reply`): its tap first reports the figure of the login it was
/// served on (`file`: 1 A's, 2 B's), stamped then — `early` 1 where that was
/// before the bound after the record's key change, one seen (`gate` 1) or one
/// still to be seen (`gate` 0: the host notices a change only at or after it
/// happened). The bound passes (`Elapse`, `gate` 2) only once the process has
/// adopted the login. The host reads the file at a decided point (`Read`),
/// keyed by the account `home` names then: a key other than the record's
/// (`key`) drops what the record holds and starts the gate; a figure served
/// before the gate's end is not taken; any other is (`held`: 0 A's, 1 B's, 2
/// none).
///
/// `TheFigureIsTheKeysAccount`: the record never holds one account's figure
/// under the other's key. Each mutant breaks it alone: `ReadUngated` (the
/// key's instant not kept: a reading under the new key takes whatever figure
/// it carries) and `Restamp` (a re-report stamped by its own run, as the tap
/// stamped it before: an old login's figure, read late, looks served after the
/// bound). Tier-0 is aterm-models' registry sweep (`non_vacuity`); Tier-1 is
/// aterm-agent's `conformance_account_key`, over the real `note_reading` at
/// every reachable `Read`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_account_key_model() -> Model {
    aterm_spec::ty_model! {
        HarnessAccountKey {
            const Buggy = 0;
            var home = 0;
            var on = 0;
            var adopt = 0;
            var key = 0;
            var held = 0;
            var gate = 0;
            var file = 0;
            var early = 0;

            action SignIn when (home == 0) {
                home = 1;
                adopt = 1;
            }
            action Adopt when (adopt == 1) {
                on = home;
                adopt = 0;
            }
            action Elapse when (gate == 1 && adopt == 0) {
                gate = 2;
            }
            action Reply {
                file = on + 1;
                early = if (gate == 2) { 0 } else { 1 };
            }
            // `note_reading`: another key drops the record and starts the
            // gate; nothing served before the gate's end is taken.
            action Read when (file > 0) {
                held = if (key == home) {
                    if (early == 1 && gate > 0) { held } else { file - 1 }
                } else if (early == 1) {
                    2
                } else {
                    file - 1
                };
                gate = if (key == home) { gate } else { 1 };
                key = home;
            }

            // Mutants (Buggy = 1).
            action ReadUngated when (Buggy == 1 && file > 0) {
                held = file - 1;
                gate = if (key == home) { gate } else { 1 };
                key = home;
            }
            action Restamp when (Buggy == 1 && file > 0 && gate == 2 && early == 1) {
                early = 0;
            }

            invariant TheFigureIsTheKeysAccount: held == 2 || held == key;
        }
    }
}
