// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed re-key: a shell spawned before the re-key channel gets a new
//! shell-integration key from the relaunch line typed at its prompt. The window writes the
//! key to a one-use file and authorizes it; the line reads it and removes the file. A file
//! still there at settlement means the line never ran, and the key is taken back.
//!
//! The same line upgrades a healthy shell from before loaders. There the file carries the
//! key the shell already signs with and nothing is authorized: an older key-only line reads
//! only the first line, so an empty one would take the shell off its key.

use aterm_spec::derive::Model;

/// One degraded (or healthy) tab, one typed re-key, one settlement.
///
/// State:
/// * `working` — 1 when the engine already verified the shell's own key (`integration=on`).
/// * `sh` — the key the shell signs with: 1 its own, 2 the typed key, 0 none (it read an
///   empty first line).
/// * `eng` — the key the engine verifies: 0 none, 1 the shell's own, 2 the typed key;
///   `prior` is what it was before the typed key replaced it.
/// * `issued`, `file` — the typed key was issued; its one-use file still exists.
///   `settled` — the window settled it (`rekey withdraw`, expiry, or the next issue).
/// * `fkey` — the key on the file's first line (0 empty, 1 the shell's own, 2 fresh): what
///   any typed line, this build's or an older key-only one, takes as the shell's key.
/// * `upg` — the file also names this build's loader.
///
/// `Issue` writes the file and authorizes the key, for a degraded tab only. `Upgrade`, for
/// a healthy tab whose shell predates loaders, writes the file with the shell's current key
/// and authorizes nothing. `Read` is the typed line running. `Settle` goes by the file:
/// still there, it is removed and `eng` restored to `prior`; gone, the key is kept.
///
/// `Buggy = 1` authorizes with no way back (`Settle` looks at nothing), re-keys healthy
/// tabs too, and writes an empty first line on upgrade; each breaks an invariant.
///
/// Tier-1: `aterm-gui/src/typed_rekey_conformance.rs` drives the real `shell_rekey::typed`
/// issue, upgrade and settle against every schedule, with `Buggy = 1` as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn typed_rekey_model() -> Model {
    aterm_spec::ty_model! {
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
