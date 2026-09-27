// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed re-key — how a shell spawned before the re-key channel gets a new
//! shell-integration key from a line typed at its prompt, and why the key can
//! never outlive the file that was to hand it over.
//!
//! 2026-09-24/26: both of the owner's tabs read `status integration=degraded`
//! (the 0.91 parent of a seamless update dropped their nonces), and the re-key
//! channel built for that heals only shells spawned WITH it. The live agent
//! upgrade types one line into such a shell anyway — its relaunch line, at the
//! moment the shell holds the terminal — so the window hands the key over
//! there: it writes a fresh key to a one-use file, authorizes it as one the
//! shell has not taken, and the line reads it and removes the file. Unlike the
//! channel's key it is TAKEN BACK whenever its line never runs, and the file
//! decides that: still there at settlement, the line never ran.
//!
//! 2026-09-26, the LOADER / BODY split: the same line UPGRADES a shell whose
//! integration predates loaders — healthy ones too, which no key needs to
//! reach. For those the window writes the key the shell already signs with,
//! and authorizes nothing: an older sweep's key-only line reads only that
//! first line, so an empty one there would have taken the shell off its key.

use super::Model;

/// One degraded (or healthy) tab, one typed re-key, one settlement.
///
/// State:
/// * `phase` — 0 before the tab's state is chosen, 1 after.
/// * `working` — 1 when the engine already verified the shell's own key
///   (`integration=on`); 0 when it had lost it (`degraded`, the owner's tabs).
/// * `sh` — the key the shell signs with: 1 its own (from its spawn), 2 the
///   typed key (the line ran), 0 none (it read an EMPTY line for its key).
/// * `eng` — the key the engine verifies: 0 none (the lost nonce), 1 the
///   shell's own, 2 the typed key.
/// * `prior` — what `eng` was when the typed key replaced it (the way back).
/// * `issued`, `file` — the typed key was issued; its one-use file still holds
///   it. `settled` — the window has settled it (the sweep's `rekey withdraw`,
///   the expiry, or the next issue).
/// * `fkey` — the key the file's FIRST line holds (0 empty, 1 the shell's own,
///   2 the fresh key): what any typed line — this build's, or an older sweep's
///   key-only one — reads as the shell's key.
/// * `upg` — the file also names this build's loader (the typed UPGRADE of a
///   shell from before loaders, 2026-09-26).
///
/// Actions: `Lose`/`Keep` choose the tab. `Issue` — the window writes the file
/// and authorizes the key, only for a tab whose integration is degraded.
/// `Upgrade` — for a HEALTHY tab whose shell predates loaders, the window writes
/// the file naming its loader, with the key the shell already signs with on the
/// first line, and authorizes nothing. `Read` — the typed line runs: the shell
/// takes the file's first line as its key, the file is gone. `Settle` — by the
/// file: still there, it is removed and `eng` restored to `prior`; gone, the key
/// is kept.
///
/// Invariants: `WorkingKeyNeverMoved` — a tab whose marks verified keeps
/// verifying the key its shell signs with. `UnreadKeyTakenBack` — once settled,
/// the typed key is authorized only if the shell took it. `NoLiveKeyInAFile` —
/// once settled, no file holds the key the engine verifies.
///
/// `Buggy = 1` is the re-key channel's own semantics applied to a shell with
/// no hook, plus a posture-blind issue: the key authorized with no way back
/// and the file left for a reader that will never come (`Settle` looks at
/// nothing), and a healthy tab re-keyed too — and an upgrade that writes an
/// EMPTY first line, which an older sweep's key-only line takes for the key. It
/// is caught on all three: a working shell moved off its key while the file
/// waits (or onto no key at all), and after the settle an unread key standing
/// — in a file anyone who can read the control dir can take it from.
///
/// Tier-0: `aterm-spec/tests/derived_typed_rekey.rs`. Tier-1
/// (`aterm-gui/src/typed_rekey_conformance.rs`) drives the real
/// `shell_rekey::typed` issue, upgrade and settle, the real engine and the real
/// file through every schedule of this model, projects `eng` from which key the
/// engine verifies and `fkey` from the file, and replays them against
/// `Buggy = 1` as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn typed_rekey_model() -> Model {
    crate::ty_model! {
        TypedRekey {
            const Buggy = 0;
            var phase = 0;
            var working = 0;
            var sh = 1;
            var eng = 0;
            var prior = 0;
            var issued = 0;
            var file = 0;
            var settled = 0;
            var fkey = 0;
            var upg = 0;

            action Lose when (phase == 0) { phase = 1; }
            action Keep when (phase == 0) { phase = 1; working = 1; eng = 1; }
            action Issue when (phase == 1 && issued == 0 && (eng == 0 || Buggy == 1)) {
                issued = 1;
                file = 1;
                prior = eng;
                eng = 2;
                fkey = 2;
            }
            action Upgrade when (phase == 1 && issued == 0 && eng == 1) {
                issued = 1;
                file = 1;
                upg = 1;
                prior = eng;
                fkey = if Buggy == 1 { 0 } else { eng };
            }
            action Read when (file == 1) {
                file = 0;
                sh = fkey;
            }
            action Settle when (issued == 1 && settled == 0) {
                settled = 1;
                file = if Buggy == 1 { file } else { 0 };
                eng = if Buggy == 0 && file == 1 { prior } else { eng };
            }

            invariant WorkingKeyNeverMoved: working == 0 || eng == sh;
            invariant UnreadKeyTakenBack: settled == 0 || sh == 2 || eng <= 1;
            invariant NoLiveKeyInAFile: settled == 0 || file == 0 || fkey > eng || eng > fkey;
        }
    }
}
