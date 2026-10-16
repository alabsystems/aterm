// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's Claude Code account machines (aterm-agent `harness/accounts.rs`,
//! `docs/DESIGN-claude-accounts-2026-10-01.md` §6 and §10).
//!
//! * [`supervisor_account_rotation_model`]: which account each session of one home runs on.
//!   One home runs on one account at a time (`active`). A session at a usage wall, or ahead
//!   of one, moves to the next account of the roster (fresh before low, dedicated before
//!   shared) by a rotation one session makes alone; the others follow once the chosen
//!   account has served a response, and take that session's place after ten silent
//!   minutes. The owner's `use <label>` pins an account and everyone follows. Rotations an
//!   hour are capped. Tier-1: aterm-agent `tests/supervise_conformance_account_rotation.rs`
//!   walks every state against the real `accounts::advise`, its transactions, and
//!   `decide_turn_end`.
//! * [`supervisor_usage_probe_model`] (`docs/SPEC-claude-accounts-probe-2026-10-01.md` §5):
//!   the `/usage` probe that feeds a move ahead of a wall.
//! * [`supervisor_limit_reset_model`] (`docs/SPEC-claude-accounts-resets-2026-10-02.md`
//!   §5): taking the owner's limit reset a wall dialog offers.
//!
//! The models are authored, not derived from the code. Tier-0 is
//! `tests/derived_supervise.rs`; Tier-1 for the probe and the reset is aterm-agent
//! `supervise/run_engine_tests.rs`, over a scripted server. The vendor's side of both is read
//! from the Claude Code 2.1.287 bundle, never seen on a screen.

use super::{Liveness, Model};

/// Account rotation (aterm-agent `harness/accounts.rs`: `advise`, `begin_rotation`,
/// `take_canary`, `prove`, `strike`, `pin`, `unpin`, `note_wall`, `note_sample`,
/// `forget_session`, `condition`; `supervise/policy/turn_end.rs`: `decide_turn_end`,
/// `restarted`, `acted`, `account_point`, `account_answer`).
///
/// The roster is three accounts, numbered as a candidate sorts: `1` and `2` dedicated (`1`
/// first by the roster's key), `3` shared. `d1`, `d2`, `sh` are their conditions: 0 fresh
/// (usable or never seen), 1 low, 2 limited (a wall stands).
///
/// The machine:
/// * `act` — the active account (0: never rotated). `run` — what the supervisor reads of
///   `act`, kept true by every step that changes it: 0 nothing to run on (none, or
///   limited), 1 low, 2 fresh.
/// * `prov` — 1 once `act` has served a response since it was chosen (or is the owner's
///   word). `prev` — where a strike goes back to while `act` is unproven (0 none); `pp` — 1
///   when that account had served when it was left.
/// * `hold` — 1 while the canary's hold stands: the one session on an unanswered account
///   holds the others back, for ten minutes. `pin` — the owner's pinned account (0 none).
///   `rot` — rotations this hour, against `Budget`.
///
/// Two sessions:
/// * `s1`, `s2` — each process's account. A session on a limited account is at its wall,
///   unless its turn ended without one (`Leave…`: a wall another tab recorded).
/// * `jobs` — 1 while session 1's agent has background work, so the host cannot relaunch
///   it (the environment's, and not fair).
/// * `w1`, `w2` — 1 while the session sleeps out its wall.
/// * `n1`, `n2` — 1 while the session stands at the notice of a wall the owner's `Use…`
///   just lifted: its screen still shows the limit, but its account can be run on. Every
///   world step forgets it.
///
/// The last step's record, which the move laws are stated over: `mv` the session a move was
/// just made for (0 none), `pa` and `ph` what `act` and `hold` were before it, `pw` 1 when
/// it was made at that session's wall. Every move writes them from the pre-state, sound or
/// not; no step reports on itself. Every other step clears them, except the move's own
/// continuation (`Resumed…`, `CarriesOnAfterAnEarlyMove`), which sets only `cx`: 1 once a
/// continuation of the move's own was typed into the relaunched session.
///
/// The world: walls (`Wall…`; the canary's own, struck, is `Strike`), `/usage` reads
/// showing low (`Low…`), window resets (`Reset…`), the owner's pin (`Use…`, `Unpin`), the
/// agent's jobs, the chosen account answering (`Served`) or walling (`Strike`), and the
/// hold ending (`Lapse`).
///
/// The supervisor: `Move…` takes the advice a session gets at its point (the canary's
/// place; design §6 rule 2, follow; rule 3, at a wall; rule 4, ahead of one) and makes the
/// move in one step; `Leave…` is the same at a plain turn end on a limited account; `Wait…`
/// sleeps a session at its wall when there is no move; `Resume…` continues a session at a
/// lifted wall's notice in place, at once and once (the roster's `Resume`; no relaunch,
/// nothing committed); `Resumed…` types the wall's `usage-resume@v1` into a session just
/// moved at its wall, once (the relaunch itself types nothing; the accounts design v2,
/// §3.5). In the code a move is the roster's advice, the turn-end decision on it
/// (`Restart::Account`, `at: Wall` or `at: Ahead`), and the host's relaunch. `jobs` is the
/// host's `wait:changed`: the policy holds the point on its restart back-off, so the move
/// is asked again.
///
/// # Not modelled
///
/// * Time: a `Wait`'s instant and its preference for a dedicated account; "low only when
///   nothing is usable within 15 minutes"; the leave guard (no early move off an account
///   whose low window resets within 15 minutes; here a low account is one to leave until
///   its `Reset…`); the hold's ten minutes (`Lapse`, not fair); the hour lapsing (`rot`
///   only grows: the model is one rotation hour).
/// * The week that resets soonest: `1` before `2` stands for the roster's key on two
///   dedicated accounts alike in freshness.
/// * Signed-out or unenrolled logins, the login-wall strike, a move that failed with no
///   word about the account.
/// * The probe ([`supervisor_usage_probe_model`]; a `Low…` step is its settled read), a
///   reset offer about to expire, limit resets. Shared with the probe: no `Low…` is about
///   an unanswered active account (`accounts::advise` never probes it).
/// * `accounts = "off"` and rule 1's stays: the model is `early`. `accounts = "wall"` (no
///   rule 4, no `Leave…`, no probe) is Tier-1's control.
/// * Compare-and-sets as steps: a rotation or a canary take is advised, committed and
///   relaunched at once; a refused relaunch is undone (`Strike::NotMoved`) and is no step.
///   Tier-1 drives the lost compare-and-set and both undos against the real transactions.
/// * Which session is the canary: `hold` is the machine's, and a rotated session is on the
///   chosen account at once, so a canary still on its old account is not a state here.
/// * Session 2's jobs, and which walls rotate (session, weekly and model-bucket limits do;
///   spend and credit walls never; the wall here is one that does).
/// * The per-session bounds (design §6, "Bounds. Per session"), held in the turn-end
///   policy's memory: the latch after a relaunch the host answers `refused:…` or
///   `failed:…`, the cap of four moves an hour, and no move ahead of a wall until a turn
///   under the new login was served. `Move…` is enabled wherever the roster allows; under a
///   bound the real decider gives its non-account action. So `NoWaitWhileOneIsUsable` and
///   the liveness claim are about a session whose host makes the relaunch or says `wait:`.
///   Tier-1 binds each bound.
/// * A lifted notice outlasting a world step: the model reads the continuation as going at
///   once. The real notice stands until something is typed (the roster's unit tests sweep
///   what can happen meanwhile), and `Resume`'s row date and "once per clearing" are bound
///   in Tier-1.
/// * The continuation after a move, beyond `cx`: the point's own act after a move ahead of
///   a wall, and that the wall's resume goes at the relaunched session's first point.
///   Tier-1 binds both at every move.
/// * The host's and policy's own steps, unit-tested there: the lost login right after a
///   move, the check that the relaunched environment names the account, and the wake (here
///   every step that could change a sleeper's answer clears `w1`/`w2`).
/// * What a sample showed under a wall the owner clears: `Use…` makes a limited account
///   fresh.
/// * A wall recorded on the canary's account wakes nobody, so a session asleep for the hold
///   sleeps to the hold's end though it could rotate: `NoWaitWhileOneIsUsable` and the
///   liveness claim read "within the canary's hold" for that reason.
///
/// # What the laws allow
///
/// * At its wall a session takes a silent canary's place on a low account while a fresh one
///   is free: nobody rotates past an account that can be run on and has not answered.
/// * With the hour's rotations spent, a session at its wall joins a proven low `active`
///   while a fresh account is free, rather than wait beside an account it can run on.
/// * `Served` and `Strike` are the vendor's and not fair: an account that never answers
///   stays unproven, but holds nobody past its hold; each session takes the canary's place
///   in turn.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_account_rotation_model() -> Model {
    crate::ty_model! {
        SupervisorAccountRotation {
            const Buggy = 0;
            const Budget = 2;
            var d1 = 0;
            var d2 = 0;
            var sh = 0;
            var act = 0;
            var run = 0;
            var prov = 0;
            var prev = 0;
            var pp = 0;
            var pin = 0;
            var hold = 0;
            var s1 = 1;
            var s2 = 1;
            var jobs = 0;
            var w1 = 0;
            var w2 = 0;
            var n1 = 0;
            var n2 = 0;
            var rot = 0;
            var mv = 0;
            var pa = 0;
            var ph = 0;
            var pw = 0;
            var cx = 0;

            // ---- the world ----
            // Every world step forgets the last move's record (`mv`, `pa`, `ph`,
            // `pw`, `cx`) and a lifted wall's notice (`n1`, `n2`): the laws over
            // them are about a move just made, and the notice is continued at
            // once or read as continued.
            action JobsStart when (jobs == 0) {
                jobs = 1;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The relaunch asked for on the back-off finds the host free.
            action JobsEnd when (jobs == 1) {
                jobs = 0;
                w1 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // A session on the account hits its usage limit. Walls are observed:
            // only a session running on the account records one. A wall on the
            // pinned account ends the pin. On an unanswered account this is the
            // wall recorded with no strike (`note_wall`): `active` stays, nobody
            // is woken, and whoever is at a wall rotates past it. The canary's
            // own wall, struck, is `Strike`.
            action WallD1 when (d1 <= 1 && (s1 == 1 || s2 == 1)) {
                d1 = 2;
                run = if act == 1 { 0 } else { run };
                pin = if pin == 1 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action WallD2 when (d2 <= 1 && (s1 == 2 || s2 == 2)) {
                d2 = 2;
                run = if act == 2 { 0 } else { run };
                pin = if pin == 2 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action WallSh when (sh <= 1 && (s1 == 3 || s2 == 3)) {
                sh = 2;
                run = if act == 3 { 0 } else { run };
                pin = if pin == 3 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // A settled `/usage` read shows the account low. It is read from a
            // session that has had a response there, so never from an account
            // still waiting for its first.
            action LowD1 when (
                d1 == 0 && (s1 == 1 || s2 == 1) && (prov == 1 || act == 0 || 2 <= act)
            ) {
                d1 = 1;
                run = if act == 1 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action LowD2 when (
                d2 == 0 && (s1 == 2 || s2 == 2) && (prov == 1 || act <= 1 || act == 3)
            ) {
                d2 = 1;
                run = if act == 2 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action LowSh when (sh == 0 && (s1 == 3 || s2 == 3) && (prov == 1 || act <= 2)) {
                sh = 1;
                run = if act == 3 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The account's window resets: it is fresh, and whoever waits
            // decides again (the wait's instant has come).
            action ResetD1 when (1 <= d1) {
                d1 = 0;
                run = if act == 1 { 2 } else { run };
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action ResetD2 when (1 <= d2) {
                d2 = 0;
                run = if act == 2 { 2 } else { run };
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action ResetSh when (1 <= sh) {
                sh = 0;
                run = if act == 3 { 2 } else { run };
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The owner's `accounts use <label>`: its walls are cleared (not what
            // a sample showed), and it is active, proven and pinned; the canary's
            // record goes and sleepers wake. A session at one of those walls now
            // stands at its notice (`n1`, `n2`), continued by `Resume…`.
            action UseD1 when (pin == 0 || 2 <= pin) {
                d1 = if d1 == 2 { 0 } else { d1 };
                act = 1;
                run = if d1 == 1 { 1 } else { 2 };
                prov = 1;
                prev = 0;
                pp = 0;
                pin = 1;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = if s1 == 1 && d1 == 2 { 1 } else { 0 };
                n2 = if s2 == 1 && d1 == 2 { 1 } else { 0 };
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action UseD2 when (pin <= 1 || pin == 3) {
                d2 = if d2 == 2 { 0 } else { d2 };
                act = 2;
                run = if d2 == 1 { 1 } else { 2 };
                prov = 1;
                prev = 0;
                pp = 0;
                pin = 2;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = if s1 == 2 && d2 == 2 { 1 } else { 0 };
                n2 = if s2 == 2 && d2 == 2 { 1 } else { 0 };
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action UseSh when (pin <= 2) {
                sh = if sh == 2 { 0 } else { sh };
                act = 3;
                run = if sh == 1 { 1 } else { 2 };
                prov = 1;
                prev = 0;
                pp = 0;
                pin = 3;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = if s1 == 3 && sh == 2 { 1 } else { 0 };
                n2 = if s2 == 3 && sh == 2 { 1 } else { 0 };
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            action Unpin when (1 <= pin) {
                pin = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The account a rotation chose answers a session on it. With a
            // response: it is proven, and only now is it followed.
            action Served when (1 <= run && prov == 0 && (s1 == act || s2 == act)) {
                prov = 1;
                prev = 0;
                pp = 0;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // With a wall of its own: it is struck (limited), and `active` goes
            // back to the account it was rotated from, proven only if that one
            // had served when it was left (`pp`). Nobody else was moved. The
            // struck account is never the one gone back to.
            action Strike when (1 <= run && prov == 0 && (s1 == act || s2 == act)) {
                d1 = if act == 1 { 2 } else { d1 };
                d2 = if act == 2 { 2 } else { d2 };
                sh = if act == 3 { 2 } else { sh };
                act = prev;
                run = if (prev == 1 && d1 == 0) || (prev == 2 && d2 == 0) || (prev == 3 && sh == 0)
                {
                    2
                } else if (prev == 1 && d1 == 1) || (prev == 2 && d2 == 1) || (prev == 3 && sh == 1)
                {
                    1
                } else {
                    0
                };
                prov = pp;
                prev = 0;
                pp = 0;
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The canary's hold ends: its ten minutes pass, or its agent ends.
            // `active` is not put back; whoever waited for it decides again.
            action Lapse when (1 <= act && prov == 0 && hold == 1) {
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }

            // ---- the supervisor: session 1 at its point ----
            // While the agent's jobs run the relaunch is refused and nothing
            // changes.
            action Move1 when (
                w1 == 0 && jobs == 0 && (
                    // The canary's place: `active` is another account, can be
                    // run on, has not answered, and its canary's hold lapsed.
                    // At its wall the session takes it whatever is left of it;
                    // ahead of one, only a fresh account.
                    ((s1 > act || act > s1) && 1 <= run && prov == 0 && hold == 0
                        && (run == 2
                            || (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2)
                            || (s1 == 3 && sh == 2)))
                    // Rule 2, the follow: `active` is another account, proven,
                    // and fresh or pinned. A proven low unpinned account is
                    // followed only at this session's wall, when no fresh account
                    // is free or the hour is spent (else the update rotates past
                    // it).
                    || ((s1 > act || act > s1) && 1 <= run && prov == 1
                        && (run == 2 || 1 <= pin
                            || (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2)
                            || (s1 == 3 && sh == 2)))
                    // Rule 3, at its wall with nothing to follow: `active` is
                    // nothing to run on, another account is not limited, and
                    // the hour is not spent.
                    || (((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                        && run == 0 && rot <= Budget - 1 && (d1 <= 1 || d2 <= 1 || sh <= 1))
                    // Rule 4, ahead of its wall: its account is low, no pin, a
                    // fresh account is free, the hour is not spent, and `active`
                    // is nothing to run on or proven and low. Never from an
                    // account that has not answered.
                    || (((s1 == 1 && d1 == 1) || (s1 == 2 && d2 == 1) || (s1 == 3 && sh == 1))
                        && pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                // Rotated, it goes to the next candidate by the one key: fresh
                // before low, dedicated before shared. Else onto `active`.
                s1 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                // A rotation's account has not answered yet, remembers where a
                // strike goes back to and whether that account had served, and
                // counts toward the hour. Following changes none of these.
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                // The session that rotated, or that took a silent canary's
                // place, is the canary now: its own hold starts.
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                // The record of this step, from the state before it.
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = if (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2) { 1 } else { 0 };
                cx = 0;
            }
            // A plain turn end on a limited account: the wall is one another tab
            // recorded, and the session is left as one on a low account is.
            action Leave1 when (
                w1 == 0 && jobs == 0
                && ((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                && (
                    ((s1 > act || act > s1) && run == 2 && prov == 0 && hold == 0)
                    || ((s1 > act || act > s1) && prov == 1
                        && (run == 2 || (run == 1 && 1 <= pin)))
                    || (pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s1 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 0;
                cx = 0;
            }
            // No move to make at its wall, so it sleeps: its jobs block the
            // relaunch; an unanswered account's canary hold stands; the hour's
            // rotations are spent; or every other account is limited too.
            action Wait1 when (
                w1 == 0
                && ((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                && (
                    jobs == 1
                    || (1 <= run && prov == 0 && hold == 1)
                    || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                )
            ) {
                w1 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The owner lifted this wall (design §6, between rules 2 and 3): the
            // session stands at the notice of a wall the owner's word cleared, on
            // an account that can be run on. It is continued there, once, at once:
            // the wall's own continuation typed, no relaunch (so the agent's jobs
            // do not hold it), nothing committed.
            action Resume1 when (
                w1 == 0 && n1 == 1
                && ((s1 == 1 && d1 <= 1) || (s1 == 2 && d2 <= 1) || (s1 == 3 && sh <= 1))
            ) {
                n1 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }

            // The wall's resume after a move made at this session's wall (the
            // accounts design v2, §3.5): the move cut a turn short, so the
            // continuation that wall would have had at its reset is typed at the
            // relaunched session's first point, once.
            action Resumed1 when (mv == 1 && pw == 1 && cx == 0) {
                cx = 1;
            }

            // ---- the supervisor: session 2 at its point (the same rule) ----
            action Move2 when (
                w2 == 0 && (
                    ((s2 > act || act > s2) && 1 <= run && prov == 0 && hold == 0
                        && (run == 2
                            || (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2)
                            || (s2 == 3 && sh == 2)))
                    || ((s2 > act || act > s2) && 1 <= run && prov == 1
                        && (run == 2 || 1 <= pin
                            || (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2)
                            || (s2 == 3 && sh == 2)))
                    || (((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                        && run == 0 && rot <= Budget - 1 && (d1 <= 1 || d2 <= 1 || sh <= 1))
                    || (((s2 == 1 && d1 == 1) || (s2 == 2 && d2 == 1) || (s2 == 3 && sh == 1))
                        && pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s2 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n2 = 0;
                mv = 2;
                pa = act;
                ph = hold;
                pw = if (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2) { 1 } else { 0 };
                cx = 0;
            }
            action Leave2 when (
                w2 == 0
                && ((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                && (
                    ((s2 > act || act > s2) && run == 2 && prov == 0 && hold == 0)
                    || ((s2 > act || act > s2) && prov == 1
                        && (run == 2 || (run == 1 && 1 <= pin)))
                    || (pin == 0 && rot <= Budget - 1 && (d1 == 0 || d2 == 0 || sh == 0)
                        && (run == 0 || (prov == 1 && run == 1)))
                )
            ) {
                s2 = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                act = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                {
                    if d1 == 0 { 1 } else if d2 == 0 { 2 } else if sh == 0 { 3 }
                    else if d1 == 1 { 1 } else if d2 == 1 { 2 } else { 3 }
                } else {
                    act
                };
                run = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { if (d1 == 0 || d2 == 0 || sh == 0) { 2 } else { 1 } } else { run };
                prov = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { 0 } else { prov };
                prev = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { act } else { prev };
                pp = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { prov } else { pp };
                rot = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                { rot + 1 } else { rot };
                hold = if run == 0
                    || (prov == 1 && run == 1 && pin == 0 && (d1 == 0 || d2 == 0 || sh == 0)
                        && rot <= Budget - 1)
                    || prov == 0
                { 1 } else { hold };
                n2 = 0;
                mv = 2;
                pa = act;
                ph = hold;
                pw = 0;
                cx = 0;
            }
            action Wait2 when (
                w2 == 0
                && ((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                && (
                    (1 <= run && prov == 0 && hold == 1)
                    || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                )
            ) {
                w2 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // A lifted wall's notice, continued in place (`Resume1`'s rule).
            action Resume2 when (
                w2 == 0 && n2 == 1
                && ((s2 == 1 && d1 <= 1) || (s2 == 2 && d2 <= 1) || (s2 == 3 && sh <= 1))
            ) {
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }

            // The wall's resume after a move made at this session's wall, once
            // (`Resumed1`'s rule).
            action Resumed2 when (mv == 2 && pw == 1 && cx == 0) {
                cx = 1;
            }

            // ---- the defects (dead at the committed config) ----
            // Each is a session's wrong step, or a world wall nobody read. Each
            // records itself as a sound move does (`mv`, `pa`, `ph`, `pw` from the
            // pre-state; `n1`, `n2` cleared) and fires wherever its guard holds,
            // which at `Buggy = 1` includes states an earlier defect left.
            //
            // A rotation on a stale reading: the account it moves to is limited.
            action RotatesIntoALimited when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 2
                && (act <= 1 || act == 3) && run == 0 && rot <= Budget - 1
            ) {
                s1 = 2;
                act = 2;
                run = 0;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            // A wall on the active account that nobody read: it still reads as
            // one to run on, and the others are moved into it.
            action WallUnread when (
                Buggy == 1 && act == 2 && 1 <= run && prov == 1 && d2 <= 1
                && (s1 == 2 || s2 == 2)
            ) {
                d2 = 2;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The shared account taken while a dedicated one is fresh.
            action SharedFirst when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0 && sh == 0
                && run == 0 && rot <= Budget - 1
            ) {
                s1 = 3;
                act = 3;
                run = 2;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            // A low account taken while a fresh one is free (the dedicated one
            // first, whatever is left of it).
            action LowFirst when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 1 && sh == 0
                && run == 0 && rot <= Budget - 1
            ) {
                s1 = 2;
                act = 2;
                run = 1;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            // The herd: the other session follows an account that has not
            // served a response yet, while its canary's hold stands.
            action FollowsUnproven when (
                Buggy == 1 && w2 == 0 && 1 <= run && prov == 0 && hold == 1
                && (s2 > act || act > s2)
            ) {
                s2 = act;
                n2 = 0;
                mv = 2;
                pa = act;
                ph = hold;
                pw = if (s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2) { 1 } else { 0 };
                cx = 0;
            }
            // The wall waited out with a fresh account free.
            action WaitsOnTheWall when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0
                && run == 0 && rot <= Budget - 1
            ) {
                w1 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // The early move made under the owner's pin.
            action MovesEarlyUnderAPin when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && pin == 1 && d1 == 1 && d2 == 0
                && rot <= Budget - 1
            ) {
                s1 = 2;
                act = 2;
                run = 2;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 0;
                cx = 0;
            }
            // One more rotation with the hour's spent.
            action RotatesPastTheBreaker when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0
                && run == 0 && rot == Budget
            ) {
                s1 = 2;
                act = 2;
                run = 2;
                prov = 0;
                prev = act;
                pp = prov;
                hold = 1;
                rot = rot + 1;
                n1 = 0;
                mv = 1;
                pa = act;
                ph = hold;
                pw = 1;
                cx = 0;
            }
            // The notice of a wall the owner lifted waited out to its own reset,
            // on an account that can be run on.
            action WaitsAtTheLiftedWall when (Buggy == 1 && w1 == 0 && jobs == 0 && n1 == 1) {
                w1 = 1;
                n1 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
                pw = 0;
                cx = 0;
            }
            // After a move made ahead of a wall, the relaunch's own carry-on is
            // typed: "decide for yourself" where the point would have had no such
            // word.
            action CarriesOnAfterAnEarlyMove when (Buggy == 1 && mv == 1 && pw == 0 && cx == 0) {
                cx = 1;
            }

            // ---- the laws ----
            // The four move laws compare what can be seen after the move (where
            // the session is, `active`, each account's condition) with the record
            // of the state before it; none reads a mark a step sets on itself.
            //
            // Nobody is moved into a limited account: what is read of `active` is
            // true, and a session just moved is not on a limited account.
            invariant NeverIntoALimitedAccount:
                (run == 0 || (act == 1 && d1 <= 1) || (act == 2 && d2 <= 1)
                    || (act == 3 && sh <= 1))
                && (mv == 0
                    || (mv == 1
                        && ((s1 == 1 && d1 <= 1) || (s1 == 2 && d2 <= 1) || (s1 == 3 && sh <= 1)))
                    || (mv == 2
                        && ((s2 == 1 && d1 <= 1) || (s2 == 2 && d2 <= 1) || (s2 == 3 && sh <= 1))));
            // A session a rotation just moved (`active` is not what it was) is
            // on the shared account only when no dedicated one is fresh.
            invariant DedicatedBeforeShared:
                mv == 0 || act == pa || (mv == 1 && s1 <= 2) || (mv == 2 && s2 <= 2)
                || (1 <= d1 && 1 <= d2);
            // A session just moved is on a low account only when no account at
            // all is fresh (the one it left included), or when it joined `active`
            // rather than rotated: in a canary's place, under the owner's pin, or
            // with the hour's rotations spent.
            invariant FreshBeforeLow:
                mv == 0
                || (mv == 1
                    && ((s1 == 1 && (d1 == 0 || d1 == 2)) || (s1 == 2 && (d2 == 0 || d2 == 2))
                        || (s1 == 3 && (sh == 0 || sh == 2))))
                || (mv == 2
                    && ((s2 == 1 && (d1 == 0 || d1 == 2)) || (s2 == 2 && (d2 == 0 || d2 == 2))
                        || (s2 == 3 && (sh == 0 || sh == 2))))
                || (1 <= d1 && 1 <= d2 && 1 <= sh)
                || (act == pa && (prov == 0 || 1 <= pin || Budget <= rot));
            // A session that just joined `active` (no rotation) joined an
            // account that has served a response — or took the place of a
            // canary whose hold had lapsed.
            invariant OnlyAProvenActiveIsFollowed:
                mv == 0 || act > pa || pa > act || prov == 1 || ph == 0;
            // A session sleeps only at its wall, and only for the agent's jobs,
            // within the canary's hold (even once a wall is recorded on the
            // canary's account: that wall wakes nobody), the breaker, or nothing
            // usable. The real session's latch after a refused relaunch is not
            // modelled. A session at a lifted wall's notice is on an account that
            // is not limited, so it never sleeps there (`WaitsAtTheLiftedWall`).
            invariant NoWaitWhileOneIsUsable:
                (w1 == 0 || (
                    ((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                    && (
                        jobs == 1
                        || (1 <= act && prov == 0 && hold == 1)
                        || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                    )
                ))
                && (w2 == 0 || (
                    ((s2 == 1 && d1 == 2) || (s2 == 2 && d2 == 2) || (s2 == 3 && sh == 2))
                    && (
                        (1 <= act && prov == 0 && hold == 1)
                        || (run == 0 && (Budget <= rot || (d1 == 2 && d2 == 2 && sh == 2)))
                    )
                ));
            // While the pin holds, `active` is the account the owner named.
            invariant TheOwnersWordHolds: pin == 0 || (act == pin && prov == 1);
            invariant NoPingPong: rot <= Budget;
            // A move made ahead of a wall types nothing of its own: the one
            // continuation a move owes is the wall's resume, after a move made
            // at one.
            invariant NothingTypedAfterAnEarlyMove: cx == 0 || pw == 1;
        }
    }
}

/// `EveryUsableAccountIsTried` ([`supervisor_account_rotation_model`]): `[]<>(session 1 is
/// not at its wall \/ its agent's jobs run \/ the canary's hold stands \/ the hour's
/// rotations are spent \/ every other account is limited)`. A session does not sit at a
/// usage wall for good while an account it could move to is usable.
///
/// Fair: only `Move1`, so the claim is about a session whose host makes the relaunch once
/// the jobs allow it; a relaunch refused for good latches the real session for that wall
/// (not modelled). The world is not fair: the jobs may run for ever, the chosen account may
/// never answer, and `Lapse` may never come. Only the canary's hold is excepted; once it
/// lapses, the fair step takes the canary's place. `tests/derived_supervise.rs` also checks
/// the reading with `Lapse` fair and no canary exception, and session 2's twin under
/// `Move2`. The two do not hold as one conjunction: walls that keep resetting can leave one
/// session or the other at its wall at every instant.
///
/// Broken alone by `WaitsOnTheWall`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::supervisor_account_rotation_model)]
pub fn supervisor_account_rotation_liveness() -> Liveness {
    use super::{and_, cst, eq, int, le, or_, var};
    let on = |account: i64| eq(var("s1"), int(account));
    let limited = |condition: &'static str| eq(var(condition), int(2));
    let not_at_its_wall = or_(
        or_(
            and_(on(1), le(var("d1"), int(1))),
            and_(on(2), le(var("d2"), int(1))),
        ),
        and_(on(3), le(var("sh"), int(1))),
    );
    let the_canarys_hold_stands = and_(
        and_(le(int(1), var("act")), eq(var("prov"), int(0))),
        eq(var("hold"), int(1)),
    );
    let every_other_is_limited = and_(
        and_(or_(on(1), limited("d1")), or_(on(2), limited("d2"))),
        or_(on(3), limited("sh")),
    );
    let nothing_to_rotate_to = and_(
        eq(var("run"), int(0)),
        or_(le(cst("Budget"), var("rot")), every_other_is_limited),
    );
    Liveness {
        name: "EveryUsableAccountIsTried",
        goal: or_(
            or_(not_at_its_wall, eq(var("jobs"), int(1))),
            or_(the_canarys_hold_stands, nothing_to_rotate_to),
        ),
        weak: vec!["Move1"],
        strong: vec![],
        mutants: vec!["WaitsOnTheWall"],
    }
}

/// The `/usage` probe (aterm-agent `supervise/turn_end_loop.rs`: `Session::usage_probe`,
/// `Session::usage_panel_look`; the gate `TurnEndState::probe_point`; the readers
/// `harness::usage::usage_panel_read` and `aterm_phase::settings_panel`;
/// `docs/SPEC-claude-accounts-probe-2026-10-01.md` §3, §5): the loop types `/usage` into an
/// idle Claude Code, reads the Settings panel it opens, and closes it with an Escape.
/// Nothing is written where it could land anywhere else: no `/usage` over a panel or under
/// a person's hand; no Escape without the panel on the grid, under a hand, or into a
/// running turn (where Escape stops the turn). Nothing is recorded from a panel that is not
/// settled or was not read whole by the step that opened it, and a panel the loop opened is
/// never forgotten.
///
/// State:
/// * `pan` — the harness's panel: 0 none, 1 open and unsettled (loading, refreshing, an
///   enrich error, its tail under the fold), 2 open and settled.
/// * `ph` — the loop's memory: 0 idle, 1 the host's claim taken, 2 `/usage` typed and the
///   read in flight (the loop's `Owed`, and the ledger's `typed` row for a later loop),
///   3 closing (the host was told; the Escape and seeing the panel gone remain).
/// * `own` — 1 while the panel is one this step opened and has read without a break; a
///   person's key, a hold, a turn or a loop restart ends it.
/// * `hand` — a person's keystroke within the grace; `hold` — the session halted (no step
///   writes under one); `turn` — a turn running.
/// * `tries` — Escapes that did not take, up to `Tries`; `told` — 1 once the host was told
///   the panel is stuck.
///
/// History marks, written from the pre-state by every step of a kind, sound or not: each
/// Escape writes `blind` (no panel on the grid), `handkey` and `turnkey`; each `/usage`
/// writes `handkey` and `stacked` (a panel open or still owed); each sample writes `stale`
/// (not settled) and `foreign` (not this step's to read). Only a defect writes `booked`:
/// the probe is no act of the turn-end policy's (no budget, no awaited act). `bad` is 1
/// once any defect fired; each fires only from a state the sound loop reaches.
///
/// A turn may retire the panel at its boundary (`TurnRetiresThePanel`, read from the
/// vendor's bundle). On `LoopRestarts` a successor reads the ledger, so a panel typed for
/// stays owed, and nothing else is remembered.
///
/// The supervisor: `Claim` (at an idle point, no hand); `Type` (the fenced `/usage` and its
/// Enter); `TypeSkipped` (the write skipped or refused); `GiveUpNotOpened` (the composer is
/// back and no panel came: no key); `Sample` (a settled panel this step read whole);
/// `NoSample` (the read's bound ran out, or the tail is under the fold: the host is told
/// why); `EscCloses` / `EscFails` (the guarded Escape: panel on the grid, no hand, hold or
/// turn, and only once the host was told or for a panel not this step's; a failed one
/// climbs the ladder, tells the host once at `Tries`, and keeps trying); `SeesGone` (the
/// probe is over).
///
/// # Not modelled
///
/// * Time: the open and settle waits, the ladder's pauses, the grace as a duration
///   (`GraceEnds` is the clock), the 90 s between any two accounts' claims and 180 s for
///   one (the roster's unit tests).
/// * What the loop can know of a hand: `hand` is the world's truth. A successor reads no
///   person stamp from before it started, so the real loop lets a panel taken up from the
///   ledger stand one grace before it presses (the spec's P19; the loop's unit tests).
/// * Gates that are not this screen's: the renderer (`alt_screen`), the fences, the fold
///   latch per grid size, the point decided again after the probe, the sample bound to the
///   claimed process.
/// * A person's own `/usage` panel. One opened right after a loop died mid-probe is closed
///   after two idle minutes (`LoopRestarts` then `Opens` here).
/// * Two accounts.
///
/// # Defects (`Buggy = 1`), each caught alone by its own law
///
/// `EscOnTheOpenTimeout` (at an idle composer an Escape does nothing, and a second within
/// the vendor's window opens Rewind); `EscUnderAHand`, `TypeUnderAHand`; `EscIntoATurn`;
/// `TypeOverAPanel`; `TypeBooksAnAct`; `SampleUnsettled`; `SampleNotRead`;
/// `EscFailsUntold`; `GoesIdleAtTheLastTry` (also the liveness mutant).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_usage_probe_model() -> Model {
    crate::ty_model! {
        SupervisorUsageProbe {
            const Buggy = 0;
            const Tries = 2;
            var pan = 0;
            var ph = 0;
            var own = 0;
            var hand = 0;
            var hold = 0;
            var turn = 0;
            var tries = 0;
            var told = 0;
            var blind = 0;
            var stale = 0;
            var foreign = 0;
            var handkey = 0;
            var turnkey = 0;
            var stacked = 0;
            var booked = 0;
            var bad = 0;

            // ---- the world ----
            action Opens when (ph == 2 && pan == 0) {
                pan = 1;
            }
            action Settles when (pan == 1) {
                pan = 2;
            }
            action Unsettles when (pan == 2) {
                pan = 1;
            }
            action PersonTypes when (hand == 0) {
                hand = 1;
                own = 0;
            }
            action GraceEnds when (hand == 1) {
                hand = 0;
            }
            action HoldOn when (hold == 0) {
                hold = 1;
                own = 0;
            }
            action HoldOff when (hold == 1) {
                hold = 0;
            }
            action TurnStarts when (turn == 0) {
                turn = 1;
                own = 0;
            }
            action TurnEnds when (turn == 1) {
                turn = 0;
            }
            action TurnRetiresThePanel when (turn == 1 && pan > 0) {
                turn = 0;
                pan = 0;
                own = 0;
            }
            action PersonCloses when (pan > 0) {
                pan = 0;
                own = 0;
            }
            // A successor reads the ledger: a `/usage` typed with no row
            // after it stays owed; a claim not typed for is nothing.
            action LoopRestarts when (ph > 0) {
                ph = if ph > 1 { 2 } else { 0 };
                own = 0;
                tries = 0;
                told = 0;
            }

            // ---- the supervisor ----
            action Claim when (ph == 0 && pan == 0 && hand == 0 && hold == 0 && turn == 0) {
                ph = 1;
            }
            action Type when (ph == 1 && pan == 0 && hand == 0 && hold == 0 && turn == 0) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
            }
            action TypeSkipped when (ph == 1) {
                ph = 0;
            }
            action GiveUpNotOpened when (ph == 2 && pan == 0) {
                ph = 0;
                own = 0;
            }
            action Sample when (ph == 2 && pan == 2 && own == 1 && hand == 0) {
                ph = 3;
                own = 0;
                stale = if pan == 2 { stale } else { 1 };
                foreign = if own == 1 { foreign } else { 1 };
            }
            action NoSample when (ph == 2 && pan > 0 && own == 1) {
                ph = 3;
                own = 0;
            }
            action EscCloses when (
                ph > 1 && pan > 0 && (ph == 3 || own == 0) &&
                hand == 0 && hold == 0 && turn == 0
            ) {
                pan = 0;
                ph = 3;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
            }
            action EscFails when (
                ph > 1 && pan > 0 && (ph == 3 || own == 0) &&
                hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 3;
                tries = if tries <= Tries - 1 { tries + 1 } else { tries };
                told = if Tries - 1 <= tries { 1 } else { told };
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
            }
            action SeesGone when (ph == 3 && pan == 0) {
                ph = 0;
                own = 0;
                tries = 0;
                told = 0;
            }

            // ---- the defects: each the step above with a guard dropped ----
            // The open wait ran out and the loop pressed anyway.
            action EscOnTheOpenTimeout when (
                Buggy == 1 && bad == 0 &&
                ph == 2 && pan == 0 && hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 3;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            // The person's grace ignored at the Escape.
            action EscUnderAHand when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && pan > 0 && hand == 1 && hold == 0 && turn == 0
            ) {
                pan = 0;
                ph = 3;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            // An Escape with the panel over a running turn: it stops the turn.
            action EscIntoATurn when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && pan > 0 && hand == 0 && hold == 0 && turn == 1
            ) {
                ph = 3;
                turn = 0;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            // The person's grace ignored at the `/usage`.
            action TypeUnderAHand when (
                Buggy == 1 && bad == 0 &&
                ph == 1 && pan == 0 && hand == 1 && hold == 0 && turn == 0
            ) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
                bad = 1;
            }
            // A second `/usage` while a panel is open or still owed.
            action TypeOverAPanel when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
                bad = 1;
            }
            // The probe booked as an act of the turn-end policy's.
            action TypeBooksAnAct when (
                Buggy == 1 && bad == 0 &&
                ph == 1 && pan == 0 && hand == 0 && hold == 0 && turn == 0
            ) {
                ph = 2;
                own = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                stacked = if pan > 0 || ph > 1 { 1 } else { stacked };
                booked = 1;
                bad = 1;
            }
            // Figures recorded from a panel still loading or refreshing.
            action SampleUnsettled when (
                Buggy == 1 && bad == 0 &&
                ph == 2 && pan == 1 && own == 1 && hand == 0
            ) {
                ph = 3;
                own = 0;
                stale = if pan == 2 { stale } else { 1 };
                foreign = if own == 1 { foreign } else { 1 };
                bad = 1;
            }
            // Figures recorded from a panel the step did not read whole:
            // after a restart, a person's key, a hold or a turn.
            action SampleNotRead when (
                Buggy == 1 && bad == 0 &&
                ph == 2 && pan == 2 && own == 0
            ) {
                ph = 3;
                stale = if pan == 2 { stale } else { 1 };
                foreign = if own == 1 { foreign } else { 1 };
                bad = 1;
            }
            // The ladder's last rung reached, and nobody told.
            action EscFailsUntold when (
                Buggy == 1 && bad == 0 &&
                ph > 1 && pan > 0 && (ph == 3 || own == 0) &&
                hand == 0 && hold == 0 && turn == 0 && tries == Tries - 1
            ) {
                ph = 3;
                tries = Tries;
                blind = if pan == 0 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                turnkey = if turn == 1 { 1 } else { turnkey };
                bad = 1;
            }
            // The loop gives the panel up at the last try and goes idle.
            action GoesIdleAtTheLastTry when (
                Buggy == 1 && bad == 0 && ph == 3 && pan > 0 && Tries <= tries
            ) {
                ph = 0;
                own = 0;
                tries = 0;
                told = 0;
                bad = 1;
            }

            invariant EscOnlyAtAPanel: blind == 0;
            invariant OnlyASettledPanelIsASample: stale == 0;
            invariant NoSampleFromAPanelNotRead: foreign == 0;
            invariant NothingWrittenUnderAHand: handkey == 0;
            invariant NoEscapeIntoARunningTurn: turnkey == 0;
            invariant NoSecondUsageOverAPanel: stacked == 0;
            invariant TheProbeIsNoAct: booked == 0;
            invariant AStuckPanelIsTold: tries <= Tries - 1 || told == 1;
            // The loop never forgets a panel it opened: one on the grid is
            // owed, in memory or by the ledger's row.
            invariant AnOpenPanelIsOwed: pan == 0 || ph > 1;
        }
    }
}

/// `EveryOpenedPanelCloses` ([`supervisor_usage_probe_model`]): `[]<>(no panel is open \/
/// a person's hand is on the session \/ it is held \/ a turn runs)`. A panel the harness
/// opened does not stand for good while nothing blocks the Escape.
///
/// Fair: the read ends (`NoSample`: its bound runs out) and an Escape pressed for ever is
/// taken (`EscCloses`: the ladder has no last rung). A person, a hold and a turn are
/// exceptions in the goal, not fairness: taking turns, they can keep the Escape disabled at
/// every instant while each ends infinitely often. After a loop restart the panel is still
/// owed, and closed.
///
/// Broken alone by `GoesIdleAtTheLastTry`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::supervisor_usage_probe_model)]
pub fn supervisor_usage_probe_liveness() -> Liveness {
    use super::{eq, int, or_, var};
    Liveness {
        name: "EveryOpenedPanelCloses",
        goal: or_(
            or_(eq(var("pan"), int(0)), eq(var("hand"), int(1))),
            or_(eq(var("hold"), int(1)), eq(var("turn"), int(1))),
        ),
        weak: vec!["NoSample", "EscCloses"],
        strong: vec![],
        mutants: vec!["GoesIdleAtTheLastTry"],
    }
}

/// The limit reset (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §3, §5; aterm-agent
/// `supervise/policy/reset.rs` `confirm_verdict`; `harness/accounts.rs` `claim_reset`,
/// `reset_in_flight`, `reset_outcome`; `supervise/reset_loop.rs` `reset_take`,
/// `reset_look`, `reset_run`; the readers of `aterm_phase::reset`): at a usage wall whose
/// dialog offers the owner's limit reset, the loop takes the grant row, reads the
/// confirmation that opens, and presses Enter on `Yes, use my reset`, or Escapes it. A
/// reset is the owner's scarce purchase, so: one attempt a wall, on a claim made for it,
/// never the weekly row that can stand in its slot; Enter only on the exact confirmation of
/// the row this loop took, past the vendor's guard, focus on Yes, claim still in flight;
/// nothing keyed under a person's hand; only the vendor's line says a reset was spent; a
/// kept or failed attempt is final; nobody is moved off an account while its reset is being
/// made; a confirmation the loop opened is never forgotten.
///
/// State:
/// * `dlg` — the vendor's box on session one's screen: 0 none, 1 the wall dialog with the
///   grant row (`Use your reset now (…)`), 2 with the weekly row in that slot (`Reset your
///   session limit now`, which claims at once), 3 the title over `Checking for a reset…`,
///   4 the exact confirmation, 5 one the loop must keep (`confirm_verdict`: the early-use
///   line, another limit, a count or day not the row's, a deadline that does not qualify),
///   6 the title over `Resetting your limits…`.
/// * `guard` — 1 inside the vendor's guard (confirm and cancel refused for 250 ms from
///   mount); `focus` — 1 on Yes, where it opens.
/// * `claim` — the machine's one slot for this wall: 0 none, 1 session one's and in
///   flight, 2 done (an outcome recorded, or lapsed: attempted either way).
/// * `ph` — the loop's memory, kept in the session ledger for a later loop: 0 idle, 1 row
///   taken, 2 Enter written, 3 Escape written. `own` — 1 while this loop took the row; a
///   restart ends it.
/// * `line` — the vendor's transcript line: 0 none, 1 `Limits reset` (spent), 2 the limit
///   lifted by itself, 3 `Reset kept`, 4 anything else (a failure).
/// * `s2` — another session on the account: 0 working, 1 at the wall, 2 moved, 3 continued
///   in place.
/// * `hand` — a person's keystroke within the grace; `hold` — the session halted (no step
///   writes under one).
///
/// History marks, written from the pre-state by every step of a kind, sound or not: each
/// row take writes `twice` (no claim in flight for it, a step still owed, or a line already
/// said at this wall), `weekly` and `handkey`; each Enter writes `early` (not the exact
/// confirmation), `blind` (inside the guard, focus off Yes, another loop's row, or a claim
/// no longer in flight) and `handkey`; each Escape and wait-row press writes `handkey`;
/// each claim writes `retried` (a kept or failed line stands); each move of session two
/// writes `moved` (claim in flight, or a spent or lifted line said). Only defects write
/// `typed` (a typed command) and `falseused` (a spend recorded away from its line). `bad`
/// is 1 once any defect fired; each fires only from a state the sound loop reaches, so the
/// `Buggy = 1` space stays walkable.
///
/// The world may open the wall dialog again at the same wall, under a step still owed too.
/// A loop restart leaves the step owed where the ledger's last row stands, no longer this
/// loop's own. Of the supervisor's steps, a dropped Enter or Escape leaves the confirmation
/// to be judged again; `Escape` is allowed at the exact confirmation too (nothing obliges a
/// spend) and at a `Checking…` that outlasts its bound, never at `Resetting…`; `S2Moves`
/// is rotation rule 3.
///
/// # Not modelled
///
/// * Time: the two 24 h qualifying rules, the loop's wait bounds, the Escape ladder and its
///   one `inform`, the three Enters before an Escape, the settle as a duration (`GuardEnds`
///   is the clock). One assumption: the claim's ten minutes do not run out over a reset in
///   the making (`ClaimLapses` is disabled at `Resetting…` and over an unread spent or
///   lifted line; else "every claim ends" would hold whatever the loop did). The vendor
///   bounds its claim at 35 s and the loop its read at 10 s, so the real Enter must not be
///   written on a claim about to lapse, or another session could be moved off an account
///   whose reset lands a moment later.
/// * The default-login rule, the zone, every text: `dlg` 4 against 5 is all the reader and
///   `confirm_verdict` make of a confirmation.
/// * A person's own confirmation, Enter or Escape: `hand` only stops the loop's keys.
/// * A row press that never lands. A claim with no row taken ends by the clock here; the
///   real loop records it kept when it leaves the dialog.
/// * `Resetting…` outlasting the loop's bound: the vendor answers first here.
/// * What the weekly row or a typed command does once written.
/// * The other session beyond its advice (its own dialog, its jobs, the rotation of
///   [`supervisor_account_rotation_model`]); a second wall or account: `claim == 2` is this
///   wall's tombstone for good.
/// * `[harness] limit_reset` off: then the supervisor has only `PressWait`, and a
///   successor's `Escape` of a step an earlier loop opened (the loop's unit tests).
///
/// # Defects (`Buggy = 1`) and the law each breaks
///
/// * `TakeRowUnclaimed`, `NeverReadsTheLine` (also `EveryClaimEnds`'s mutant):
///   `OneAttemptPerWall`.
/// * `EnterTheInexactConfirmation`: `EnterOnlyOnTheExactConfirmation`.
/// * `EnterInsideTheGuard`, `EnterOffYes`, `EnterAsASuccessor`, `EnterOnALapsedClaim`:
///   `EnterOnlyPastTheGuardOnYes`.
/// * `TakeRowByItsPlace`: `NeverTheWeeklyRow`. `TypesTheCommand`: `NeverTheTypedCommand`.
/// * `S2MovesUnderAReset`: `NoMoveOffAnAccountBeingReset`.
/// * `TakeRowUnderAHand`, `EnterUnderAHand`, `EscapeUnderAHand`, `PressWaitUnderAHand`:
///   `NothingKeyedUnderAHand`.
/// * `UsedRecordedAtEnter`: `OnlyTheLineSaysSpent`.
/// * `ClaimsAgainAfterAFailure`: `AFailureIsNotRetried`, and `OneAttemptPerWall` a step
///   later.
/// * `StopsTryingItsEscape` (also `EveryHarnessConfirmationCloses`'s mutant):
///   `AnOpenConfirmationIsOwed`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_limit_reset_model() -> Model {
    crate::ty_model! {
        SupervisorLimitReset {
            const Buggy = 0;
            var dlg = 0;
            var guard = 0;
            var focus = 0;
            var claim = 0;
            var ph = 0;
            var own = 0;
            var s2 = 0;
            var hand = 0;
            var hold = 0;
            var line = 0;
            var twice = 0;
            var blind = 0;
            var early = 0;
            var weekly = 0;
            var typed = 0;
            var moved = 0;
            var handkey = 0;
            var falseused = 0;
            var retried = 0;
            var bad = 0;

            // ---- the world ----
            action WallOpens when (dlg == 0) {
                dlg = 1;
            }
            action WallOpensWeekly when (dlg == 0) {
                dlg = 2;
            }
            // The confirmation opens on Yes, inside its guard.
            action OpensTheConfirmation when (dlg == 3) {
                dlg = 4;
                guard = 1;
                focus = 1;
            }
            action OpensAnInexactConfirmation when (dlg == 3) {
                dlg = 5;
                guard = 1;
                focus = 1;
            }
            // The row's own answer (no reset to use, the limit lifted
            // already, the check failed): a line, and no confirmation.
            action OpensAnAnswer when (dlg == 3) {
                dlg = 0;
                line = 4;
            }
            action GuardEnds when (guard == 1) {
                guard = 0;
            }
            action FocusMoves when (dlg == 4 || dlg == 5) {
                focus = 1 - focus;
            }
            action PersonTypes when (hand == 0) {
                hand = 1;
            }
            action GraceEnds when (hand == 1) {
                hand = 0;
            }
            action HoldOn when (hold == 0) {
                hold = 1;
            }
            action HoldOff when (hold == 1) {
                hold = 0;
            }
            action ServerAnswersSpent when (dlg == 6) {
                dlg = 0;
                line = 1;
            }
            action ServerAnswersLifted when (dlg == 6) {
                dlg = 0;
                line = 2;
            }
            // Every other answer, the vendor's own timeout among them.
            action ServerAnswersFailed when (dlg == 6) {
                dlg = 0;
                line = 4;
            }
            // The claim's ten minutes pass with no outcome recorded. Not over
            // a reset in the making (the model's one assumption about time).
            action ClaimLapses when (claim == 1 && dlg <= 5 && (line == 0 || 3 <= line)) {
                claim = 2;
            }
            // A successor reads the ledger: the step stays owed where its
            // last row stands, and the row is not one this loop took.
            action LoopRestarts when (own == 1) {
                own = 0;
            }
            action S2AtWall when (s2 == 0) {
                s2 = 1;
            }

            // ---- the supervisor ----
            // The host's claim, taken last, and only at the grant row.
            action Claim when (claim == 0 && ph == 0 && dlg == 1 && hand == 0 && hold == 0) {
                claim = 1;
                retried = if line == 3 || line == 4 { 1 } else { retried };
            }
            // The row's arrows and Enter, on a claim just made for it: the
            // vendor latches the take and checks for a reset.
            action TakeRow when (claim == 1 && ph == 0 && dlg == 1 && hand == 0 && hold == 0) {
                dlg = 3;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action Enter when (
                dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action EnterDropped when (
                dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            // At a confirmation the vendor answers the Escape with its kept
            // line; at `Checking…` it closes and says nothing.
            action Escape when (
                ph > 0 && (dlg == 3 || ((dlg == 4 || dlg == 5) && guard == 0))
                && hand == 0 && hold == 0
            ) {
                dlg = 0;
                guard = 0;
                focus = 0;
                ph = 3;
                line = if dlg == 3 { line } else { 3 };
                handkey = if hand == 1 { 1 } else { handkey };
            }
            action EscapeDropped when (
                ph > 0 && (dlg == 4 || dlg == 5) && guard == 0 && hand == 0 && hold == 0
            ) {
                ph = 3;
                handkey = if hand == 1 { 1 } else { handkey };
            }
            // The step's end: the host is told what the transcript says, or
            // that it says nothing. The claim is done and the loop idle.
            action ReadLine when (ph > 0 && dlg <= 2) {
                claim = 2;
                ph = 0;
                own = 0;
            }
            // `limit-wait@v1`: the wait row, never under a step still owed.
            action PressWait when ((dlg == 1 || dlg == 2) && ph == 0 && hand == 0 && hold == 0) {
                dlg = 0;
                handkey = if hand == 1 { 1 } else { handkey };
            }
            // The advice to the other session at the wall. While the claim
            // is in flight it is a moment's wait, which changes nothing.
            action S2Waits when (s2 == 1 && claim == 1) {}
            action S2Resumes when (s2 == 1 && claim == 2 && (line == 1 || line == 2)) {
                s2 = 3;
            }
            action S2Moves when (
                s2 == 1 && (claim == 0 || claim == 2) && (line == 0 || 3 <= line)
            ) {
                s2 = 2;
                moved = if claim == 1 || line == 1 || line == 2 { 1 } else { moved };
            }

            // ---- the defects: each the step above with a guard dropped ----
            // The row taken with no claim in flight for it: none made, one
            // that is done, or one whose row is taken already.
            action TakeRowUnclaimed when (
                Buggy == 1 && bad == 0
                && dlg == 1 && (claim == 0 || claim == 2 || ph > 0) && hand == 0 && hold == 0
            ) {
                dlg = 3;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // The dialog gone after its Enter, the loop goes idle and tells
            // nobody. The claim stays its own, so the row is taken again
            // (`TakeRow`, sound) when the dialog comes back.
            action NeverReadsTheLine when (
                Buggy == 1 && bad == 0 && ph == 2 && dlg == 0 && claim == 1
            ) {
                ph = 0;
                own = 0;
                bad = 1;
            }
            // The early-use confirmation, or any other the loop must keep.
            action EnterTheInexactConfirmation when (
                Buggy == 1 && bad == 0
                && dlg == 5 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // Before the box has stood its settle: the vendor refuses it.
            action EnterInsideTheGuard when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 1 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // With the focus on `No, keep it`: the Enter answers that row.
            action EnterOffYes when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 0 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 0;
                line = 3;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // By a loop that did not take the row: it holds no row to hold
            // the confirmation to.
            action EnterAsASuccessor when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 0 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // On a claim that lapsed: nobody is held off the account any more.
            action EnterOnALapsedClaim when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 2
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // The row matched by its place: taken with the weekly row in the
            // slot, which claims at once.
            action TakeRowByItsPlace when (
                Buggy == 1 && bad == 0
                && dlg == 2 && claim == 1 && ph == 0 && hand == 0 && hold == 0
            ) {
                dlg = 6;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // `/limit-reset` or `/rate-limit-options` typed with no dialog.
            action TypesTheCommand when (
                Buggy == 1 && bad == 0 && dlg == 0 && ph == 0 && hand == 0 && hold == 0
            ) {
                typed = 1;
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // The other session moved while the reset is in flight, or off
            // the account it has just refilled.
            action S2MovesUnderAReset when (
                Buggy == 1 && bad == 0 && s2 == 1 && (claim == 1 || line == 1 || line == 2)
            ) {
                s2 = 2;
                moved = if claim == 1 || line == 1 || line == 2 { 1 } else { moved };
                bad = 1;
            }
            // The person's grace ignored, at each key the loop writes.
            action TakeRowUnderAHand when (
                Buggy == 1 && bad == 0
                && claim == 1 && ph == 0 && dlg == 1 && hand == 1 && hold == 0
            ) {
                dlg = 3;
                ph = 1;
                own = 1;
                twice = if claim == 1 && ph == 0 && line == 0 { twice } else { 1 };
                weekly = if dlg == 2 { 1 } else { weekly };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EnterUnderAHand when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 1 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                ph = 2;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action EscapeUnderAHand when (
                Buggy == 1 && bad == 0
                && ph > 0 && (dlg == 3 || ((dlg == 4 || dlg == 5) && guard == 0))
                && hand == 1 && hold == 0
            ) {
                dlg = 0;
                guard = 0;
                focus = 0;
                ph = 3;
                line = if dlg == 3 { line } else { 3 };
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            action PressWaitUnderAHand when (
                Buggy == 1 && bad == 0
                && (dlg == 1 || dlg == 2) && ph == 0 && hand == 1 && hold == 0
            ) {
                dlg = 0;
                handkey = if hand == 1 { 1 } else { handkey };
                bad = 1;
            }
            // The Enter taken for the spend: used recorded as it is written,
            // whatever the vendor then says.
            action UsedRecordedAtEnter when (
                Buggy == 1 && bad == 0
                && dlg == 4 && guard == 0 && focus == 1 && own == 1 && claim == 1
                && (ph == 1 || ph == 2) && hand == 0 && hold == 0
            ) {
                dlg = 6;
                focus = 0;
                claim = 2;
                ph = 0;
                own = 0;
                early = if dlg == 5 { 1 } else { early };
                blind = if guard == 1 || focus == 0 || own == 0 || claim == 2 { 1 } else { blind };
                handkey = if hand == 1 { 1 } else { handkey };
                falseused = if line == 1 { falseused } else { 1 };
                bad = 1;
            }
            // The wall claimed again after its reset was kept or failed.
            action ClaimsAgainAfterAFailure when (
                Buggy == 1 && bad == 0
                && claim == 2 && ph == 0 && dlg == 1 && (line == 3 || line == 4)
                && hand == 0 && hold == 0
            ) {
                claim = 1;
                retried = if line == 3 || line == 4 { 1 } else { retried };
                bad = 1;
            }
            // The loop gives the confirmation up after an Escape that did not
            // take, and goes idle.
            action StopsTryingItsEscape when (
                Buggy == 1 && bad == 0 && ph == 3 && (dlg == 4 || dlg == 5)
            ) {
                ph = 0;
                own = 0;
                bad = 1;
            }

            invariant OneAttemptPerWall: twice == 0;
            invariant EnterOnlyOnTheExactConfirmation: early == 0;
            invariant EnterOnlyPastTheGuardOnYes: blind == 0;
            invariant NeverTheWeeklyRow: weekly == 0;
            invariant NeverTheTypedCommand: typed == 0;
            invariant NoMoveOffAnAccountBeingReset: moved == 0;
            invariant NothingKeyedUnderAHand: handkey == 0;
            invariant OnlyTheLineSaysSpent: falseused == 0;
            invariant AFailureIsNotRetried: retried == 0;
            // The loop never forgets a confirmation it opened: one on the
            // screen of a wall that was claimed is owed, in memory or by the
            // ledger's row.
            invariant AnOpenConfirmationIsOwed: dlg <= 3 || dlg > 5 || ph > 0 || claim == 0;
        }
    }
}

/// `EveryClaimEnds` ([`supervisor_limit_reset_model`]): `[]<>(no claim is in flight)`. A
/// claim holds every other session of the account at its wall, so it must not stay in
/// flight for good.
///
/// Fair: the loop's step ends (`ReadLine`), a claim with no outcome lapses (`ClaimLapses`:
/// what ends one whose row was never taken, or whose confirmation stands under a hand for
/// good), and the vendor answers its claim, if only by its 35 s timeout
/// (`ServerAnswersFailed`). Nothing is assumed of a person, a hold, the loop's keys or what
/// the vendor's check opens. The clock alone does not carry it: over a reset in the making
/// the claim does not lapse, so a spent or lifted line ends its claim only by being read.
///
/// Broken alone by `NeverReadsTheLine`, which leaves a spent reset's claim in flight and
/// every other session of the account waiting on it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::supervisor_limit_reset_model)]
pub fn supervisor_limit_reset_claim_liveness() -> Liveness {
    use super::{eq, int, or_, var};
    Liveness {
        name: "EveryClaimEnds",
        goal: or_(eq(var("claim"), int(0)), eq(var("claim"), int(2))),
        weak: vec!["ReadLine", "ClaimLapses", "ServerAnswersFailed"],
        strong: vec![],
        mutants: vec!["NeverReadsTheLine"],
    }
}

/// `EveryHarnessConfirmationCloses` ([`supervisor_limit_reset_model`]): `[]<>(no
/// confirmation is on the screen \/ a person's hand is on the session \/ it is held)`. A
/// confirmation the harness's row opened, and the claiming line after its Enter, does not
/// stand for good while nothing blocks the Escape.
///
/// Fair: the guard ends (`GuardEnds`), an Escape pressed for ever is taken (`Escape`: the
/// ladder has no last rung), and the vendor answers its claim, if only by its timeout
/// (`ServerAnswersFailed`). A person and a hold are exceptions in the goal, not fairness:
/// taking turns, they can keep the Escape disabled at every instant while each ends
/// infinitely often. After a loop restart the confirmation is still owed.
///
/// Broken alone by `StopsTryingItsEscape`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::supervisor_limit_reset_model)]
pub fn supervisor_limit_reset_confirmation_liveness() -> Liveness {
    use super::{eq, int, le, or_, var};
    Liveness {
        name: "EveryHarnessConfirmationCloses",
        goal: or_(
            le(var("dlg"), int(3)),
            or_(eq(var("hand"), int(1)), eq(var("hold"), int(1))),
        ),
        weak: vec!["GuardEnds", "Escape", "ServerAnswersFailed"],
        strong: vec![],
        mutants: vec!["StopsTryingItsEscape"],
    }
}
