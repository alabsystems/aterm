// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's ACCOUNT ROTATION (aterm-agent `harness/accounts.rs`,
//! `docs/DESIGN-claude-accounts-2026-10-01.md` §6 and §10): which Claude Code
//! account each session of one home runs on, as usage limits come and go.
//!
//! One home runs on ONE account at a time (`active`). A session at a usage
//! wall, or ahead of one, is moved to the next account of the owner's roster —
//! fresh before low, dedicated before shared — by a rotation that ONE session
//! makes alone; the others follow only once the account it chose has served a
//! response, and take that session's place when it has been silent for ten
//! minutes. The owner's `use <label>` pins an account and everyone follows.
//! A machine rotates at most so many times an hour.
//!
//! STATUS: the model is authored here, not derived from the code. Tier-0 is
//! `tests/derived_supervise.rs`; Tier-1 is aterm-agent's
//! `tests/supervise_conformance_account_rotation.rs`, which walks every state
//! of this model against the real `accounts::advise` and its transactions,
//! and against the real turn-end decider (`supervise::policy::turn_end`'s
//! `decide_turn_end`, over a real `TurnEndState`) fed that advice as a host
//! hands it over. Nothing here has run against two real accounts.
//!
//! And THE `/usage` PROBE that feeds the move ahead of a wall
//! ([`supervisor_usage_probe_model`], `docs/SPEC-claude-accounts-probe-
//! 2026-10-01.md` §5): the loop types `/usage`, reads the Settings panel
//! once it is settled, and closes it with an Escape — written only where it
//! lands on the panel and nowhere else. Tier-0 is `tests/derived_supervise.rs`;
//! its Tier-1 (the real `Session` over the scripted server, every `/usage`
//! and every Escape projected onto the model) is the loop's to add with the
//! probe itself.
//!
//! And THE LIMIT RESET ([`supervisor_limit_reset_model`], `docs/SPEC-claude-
//! accounts-resets-2026-10-02.md` §5): at a wall whose dialog offers the
//! owner's limit reset, the loop takes the grant row and answers the
//! confirmation it opens — Enter only on the exact one, past the vendor's
//! guard, on a claim still in flight, and an Escape for anything else.
//! Tier-0 is `tests/derived_supervise.rs`; its Tier-1 (the real `Session`
//! over a scripted server whose dialog reacts to what is written, every key
//! projected onto the model) is the loop's to add with the step. Nothing of
//! it has been seen on a real screen: the vendor's side is READ in the
//! 2.1.287 bundle.

use super::{Liveness, Model};

/// ACCOUNT ROTATION (aterm-agent `harness/accounts.rs`: `advise`,
/// `begin_rotation`, `take_canary`, `prove`, `strike`, `pin`, `unpin`,
/// `note_wall`, `note_sample`, `forget_session`, and `condition`; and
/// `supervise/policy/turn_end.rs`: `decide_turn_end`, `restarted`, `acted`,
/// `account_point`, `account_answer`).
///
/// THE ROSTER is three accounts, numbered as a candidate sorts: `1` and `2`
/// are DEDICATED (`1` is the one the roster's key puts first), `3` is SHARED.
/// `d1`, `d2`, `sh` are their conditions: 0 fresh (usable or never seen), 1
/// low, 2 limited (a wall stands).
///
/// THE MACHINE'S STATE: `act` the active account (0 none: a machine that has
/// never rotated); `run` what the supervisor READS of `act`, kept true by
/// every step that changes it — 0 nothing to run on (no active account, or a
/// limited one), 1 low, 2 fresh; `prov` 1 once `act` has served a response
/// since it was chosen (or is the owner's word); `prev` the account a strike
/// goes back to while `act` is not proven (0 none) and `pp` 1 when THAT
/// account had served when it was left; `hold` 1 while the canary's hold
/// stands — the one session on an account that has not answered holds the
/// others back, for ten minutes and no longer; `pin` the account the owner
/// pinned (0 none); `rot` the rotations made this hour, against `Budget`.
///
/// TWO SESSIONS: `s1`, `s2` the account each one's process runs under. A
/// session whose account is limited is AT ITS WALL, unless its turn ended
/// without one (`Leave…`: a wall another tab recorded); any other point is a
/// plain turn end. `jobs` 1 while session 1's agent has background work, so
/// the host cannot relaunch it (the environment's, and NOT fair: it may never
/// end). `w1`, `w2` 1 while the session waits its wall out, asleep until
/// something wakes it. `n1`, `n2` 1 while the session stands at the NOTICE of
/// a wall the owner's word has just lifted (`Use…` of the account it is on,
/// with that account limited): its screen still shows the limit, and its
/// account can be run on. Like the record of the last step below, the notice
/// is the instant's: every step of the world's forgets it.
///
/// THE LAST STEP: `mv` the session a move was just made for (0: the last step
/// was not a session's move), with `pa` and `ph` what `act` and `hold` were
/// BEFORE it. Every other step clears the three. They are the history the
/// laws about a move are stated over, written from the pre-state by every
/// move, sound or not; no step reports on itself.
///
/// THE WORLD: a session hits its account's limit (`Wall…`; the canary's own,
/// struck, is the `Strike`); a `/usage` read shows it low (`Low…`); a window
/// resets (`Reset…`); the owner pins and unpins (`Use…`, `Unpin`); the
/// agent's jobs start and end; the account a rotation chose answers with a
/// response (`Served`) or with a wall of its own (`Strike`); the canary's
/// hold ends (`Lapse`).
///
/// THE SUPERVISOR: `Move1`, `Move2` take the advice a session gets at its
/// point — the canary's place, design §6's rule 2 (follow), rule 3 (at a
/// wall) and rule 4 (ahead of it) — and make the move in one step; `Leave1`,
/// `Leave2` are the same at a plain turn end on a limited account; `Wait1`,
/// `Wait2` put a session at its wall to sleep when there is no move to make;
/// `Resume1`, `Resume2` continue a session at the notice of a wall the owner
/// lifted — where it is, at once and once (design §6, "The owner lifted this
/// wall": the roster's `Resume`, answered by the turn-end policy with that
/// wall's own continuation; no relaunch, nothing committed).
/// In the real code a move is three hands: the roster's advice
/// (`accounts::advise`), the turn-end policy's decision on it
/// (`Restart::Account`, `at: Wall` at a wall and `at: Ahead` at a plain turn
/// end; a wait cut to the instant the reason for it ends, or to the roster's
/// next opening where that comes first — the instant a `Reset…` stands for),
/// and the host's relaunch. `jobs` is the host's `wait:changed`: the policy
/// holds the point on its restart back-off and records nothing, so the move
/// is asked again.
///
/// # What is NOT modelled
///
/// * TIME. No instant, no ladder and no 15-minute rule: a `Wait`'s instant and
///   its preference for a dedicated account; "low only when nothing is usable
///   within 15 minutes"; THE LEAVE GUARD (a session is not moved early off an
///   account whose low window resets within 15 minutes — here every low
///   account is one to leave, and it is low until its `Reset…`, however near
///   that is); the hold's ten minutes (`Lapse` is the clock, and is not fair);
///   and the hour lapsing (`rot` only grows: the model is ONE rotation hour,
///   and `Budget` is how many rotations it has left).
/// * The week that resets soonest: `1` before `2` stands for whatever the
///   roster's key says of two dedicated accounts alike in freshness.
/// * A login that is signed out or not enrolled, the login-wall strike and a
///   move that failed with no word about the account.
/// * THE PROBE (its own model, [`supervisor_usage_probe_model`]: here a
///   `Low…` step IS the settled read, already recorded), a reset offer
///   about to expire, and limit resets. One thing of the probe's is this
///   model's too: no `Low…` step is about the active account while it has
///   not answered — and the real roster does not advise the probe there
///   either (`accounts::advise`: the unproven active is never probed).
/// * `accounts = "off"` and rule 1's stays: the model is `early`, on a
///   session the roster governs.
/// * The compare-and-sets as steps of their own. A rotation, and a take of
///   the canary's place, are advised, committed and relaunched at once; a
///   relaunch the host refuses is undone (`Strike::NotMoved`) and is no step
///   at all. Tier-1 drives the lost compare-and-set and the undo of both
///   against the real transactions.
/// * WHICH session is the canary: `hold` is the machine's, and the session a
///   rotation moved is on the account it chose at once. So a canary still on
///   its old account (its relaunch never came) is not a state here.
/// * Session 2's jobs, and which walls rotate (a session or weekly limit and
///   a model bucket do, a spend or credit wall never: the wall here is one
///   that does).
/// * THE SESSION'S OWN BOUNDS (design §6, "Bounds. Per session"), which are
///   the turn-end policy's memory of its one session and no variable here:
///   the LATCH (a relaunch asked at a wall that the host answers with a word
///   that is no wait — `refused:…`, `failed:…`: a line that cannot hold the
///   login, a launch that cannot be carried — is not asked again while that
///   wall stands, or until the accounts' news reaches the session — the
///   owner's word, an account proven or struck), the same ahead of a wall
///   for the one account refused,
///   the CAP of four moves a session makes in an hour, and the move not yet
///   ANSWERED (until a turn asked under the new login has been served, a
///   session asks for no move ahead of a wall). A `Move…` is enabled here
///   wherever the roster allows it; under any of the four the real decider
///   gives today's action instead, and never a move this model does not
///   make. So `NoWaitWhileOneIsUsable` and the liveness claim are about a
///   session whose host MAKES the relaunch or says `wait:`: one it has
///   refused waits its wall out beside a usable account, for the rest of
///   that wall. Tier-1 binds each bound around every ask of the walk and
///   along a carried history.
/// * A LIFTED NOTICE THAT OUTLASTS A STEP OF THE WORLD'S. `Resume…` is the
///   step right after the owner's word, and the model carries the notice no
///   further: any step of the world's forgets it (`n1`, `n2`), and the
///   session is from then on at a plain turn end, its next act the
///   continuation — the reading "the continuation went at once". The real
///   notice stands on the screen until something is typed, so the real
///   session may still be at it when a wall is recorded on that account
///   again (then it is an ordinary wall: rotated from), when the owner's
///   word moves on to another account or a proven fresh one is active (then
///   that is followed first), or within another account's canary's hold
///   (then it waits for the hold); the roster's unit tests sweep those. Nor
///   is the row's DATE a variable: the real `Resume` asks that the row the
///   session stands at was written before the clearing (Tier-1 asks about
///   such a row at every `Resume…`, and its control is the same wall with
///   no dated row, which is rotated from), and "once per clearing" is the
///   policy's memory of the clearing's instant (Tier-1, at every `Use…`
///   taken with a session at that account's wall). What the account answers
///   the continuation with — a served turn, or a new limit, which is an
///   ordinary `Wall…` and ends the pin — is the world's next step here.
/// * THE CONTINUATION AFTER A MOVE: a move made at a wall owes that wall's
///   resume at the relaunched prompt (bound in Tier-1 at every move made);
///   here a moved session is simply on its new account.
/// * What slices 3 and 4 put in the HOST and the policy beside this rule,
///   each unit-tested there: the lost login right after a move (the account
///   struck signed-out and the next one asked for, no `/login` typed while a
///   candidate remains); the pre-flight (`claude auth status` under the
///   candidate's login before the signal); the check that the relaunched
///   process's environment names the account, and the way back when it
///   leaves unanswered; and THE WAKE — here every step that could change a
///   sleeper's answer clears `w1`/`w2`, where the real session is decided
///   again by the host's stamp for a ledger row (`Served`, `Strike`, the
///   owner's word) and by its own wait's instant for the clock: the hold's
///   end or the hour's, or any account's reset where that comes first
///   (Tier-1 reads every held session's wait a minute from each `Reset…`).
/// * `accounts = "wall"` (move only at the wall): rule 4 and `Leave…` (a
///   rotation ahead of a wall) are then never advised, and nothing is
///   probed. The model is `early`; Tier-1 walks both modes, `wall` as its
///   control.
/// * What a sample showed under a wall the owner clears: `Use…` makes a
///   limited account fresh.
/// * What wakes a sleeper beyond the steps named above. A wall recorded on
///   the canary's account wakes nobody (no ledger row of that kind does), so
///   a session asleep for the hold sleeps on to the hold's end though it
///   could rotate at once: `NoWaitWhileOneIsUsable` and the liveness claim
///   both read "within the canary's hold" for that reason.
///
/// # What the laws do NOT forbid
///
/// * At its wall a session takes the place of a silent canary on a LOW
///   account while a fresh one is free: nobody rotates past an account that
///   can be run on and has not answered.
/// * With the hour's rotations spent, a session at its wall joins a proven
///   `active` that is low while a fresh account is free, rather than wait the
///   wall out beside an account it can run on.
/// * `Served` and `Strike` are the vendor's and are not fair: an account that
///   never answers stays unproven for good. What changed is that it no longer
///   holds anyone: each session takes the canary's place in turn.
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

            // ---- the world ----
            // Every step of the world's forgets the record of the last move
            // (`mv`, `pa`, `ph`): the laws over it are about a move just made.
            // And the notice of a wall the owner lifted (`n1`, `n2`): it is
            // continued at once, or the model reads it as continued.
            action JobsStart when (jobs == 0) {
                jobs = 1;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
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
            }
            // A session on the account hits its usage limit. Walls are OBSERVED:
            // one is recorded only by a session running on that account. A wall
            // on the pinned account ends the pin. On an account that has not
            // answered yet this is the wall recorded with NO strike (`note_wall`):
            // `active` stays, nobody is woken, and whoever is at a wall rotates
            // past it. The canary's own wall, struck, is the `Strike` below.
            action WallD1 when (d1 <= 1 && (s1 == 1 || s2 == 1)) {
                d1 = 2;
                run = if act == 1 { 0 } else { run };
                pin = if pin == 1 { 0 } else { pin };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
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
            }
            // A settled `/usage` read on the account shows it low. It is read
            // from a session that has had a response there, so never from an
            // account still waiting for its first.
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
            }
            action LowSh when (sh == 0 && (s1 == 3 || s2 == 3) && (prov == 1 || act <= 2)) {
                sh = 1;
                run = if act == 3 { 1 } else { run };
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
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
            }
            // The owner's word, `accounts use <label>`: its walls are cleared
            // (what a sample showed is not), and it is active, proven and
            // pinned. The canary's record goes. The wake marker is touched.
            // A session that stood at one of those walls stands at its notice
            // from here, on an account that can be run on (`n1`, `n2`): it is
            // continued there (`Resume…`).
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
            }
            // With a wall of its own: it is struck (limited), and `active` goes
            // back to the account it was rotated from — PROVEN OR NOT AS THAT
            // ACCOUNT WAS when it was left (`pp`): one that never served is not
            // followed for having been gone back to. Nobody else was moved. The
            // struck account is never the one gone back to, so what is read of
            // that one is its own condition.
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
            }
            // The canary's hold ends: its ten minutes pass, or its agent ends.
            // `active` is NOT put back. Whoever waited for it decides again.
            action Lapse when (1 <= act && prov == 0 && hold == 1) {
                hold = 0;
                w1 = 0;
                w2 = 0;
                n1 = 0;
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
            }

            // ---- the supervisor: session 1 at its point ----
            // The move is made when the host can relaunch the session; while
            // the agent's jobs run it is refused and nothing has changed.
            action Move1 when (
                w1 == 0 && jobs == 0 && (
                    // THE CANARY'S PLACE. `active` is another account, can be
                    // run on and has not answered, and its canary's hold has
                    // lapsed: this session takes the place — at its wall
                    // whatever is left of the account, ahead of one only onto
                    // a fresh account.
                    ((s1 > act || act > s1) && 1 <= run && prov == 0 && hold == 0
                        && (run == 2
                            || (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2)
                            || (s1 == 3 && sh == 2)))
                    // RULE 2, the follow: `active` is another account, proven,
                    // and fresh or the owner's pin. A proven account that is
                    // low and not pinned is not followed for its own sake; at
                    // this session's wall it is what is left when no fresh
                    // account is, or the hour is spent — and is rotated past
                    // for a fresh one otherwise (the update decides which).
                    || ((s1 > act || act > s1) && 1 <= run && prov == 1
                        && (run == 2 || 1 <= pin
                            || (s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2)
                            || (s1 == 3 && sh == 2)))
                    // RULE 3, at its wall with nothing to follow: `active` is
                    // nothing to run on, another account is not limited, and
                    // the hour is not spent.
                    || (((s1 == 1 && d1 == 2) || (s1 == 2 && d2 == 2) || (s1 == 3 && sh == 2))
                        && run == 0 && rot <= Budget - 1 && (d1 <= 1 || d2 <= 1 || sh <= 1))
                    // RULE 4, ahead of its wall: its account is low, no pin
                    // holds, a FRESH account is free and the hour is not
                    // spent; `active` is nothing to run on, or is proven and
                    // low (its own account, or another one, rotated past).
                    // Never from an account that has not answered.
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
                // strike goes back to and whether THAT account had served, and
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
            }
            // A PLAIN TURN END ON A LIMITED ACCOUNT: the wall is one another tab
            // recorded, and this session's own turn ended without one. It is
            // left as a low account is, and more readily: for a fresh account
            // only, and never onto a low one.
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
            }
            // No move to make at its wall, so it sleeps: the host cannot
            // relaunch it (its jobs); an account that has not answered is out
            // and its canary's hold stands; the hour's rotations are spent; or
            // every other account is limited too.
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
            }
            // THE OWNER LIFTED THIS WALL (design §6, between rules 2 and 3): the
            // session stands at the notice of the wall the owner's word just
            // cleared, on an account that can be run on. It is continued
            // THERE, at once — the wall's own continuation typed, no relaunch
            // (so the agent's jobs do not hold it) and nothing committed —
            // once: the notice is behind it.
            action Resume1 when (
                w1 == 0 && n1 == 1
                && ((s1 == 1 && d1 <= 1) || (s1 == 2 && d2 <= 1) || (s1 == 3 && sh <= 1))
            ) {
                n1 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
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
            }
            // THE OWNER LIFTED THIS WALL (design §6, between rules 2 and 3): the
            // session stands at the notice of the wall the owner's word just
            // cleared, on an account that can be run on. It is continued
            // THERE, at once — the wall's own continuation typed, no relaunch
            // (so the agent's jobs do not hold it) and nothing committed —
            // once: the notice is behind it.
            action Resume2 when (
                w2 == 0 && n2 == 1
                && ((s2 == 1 && d1 <= 1) || (s2 == 2 && d2 <= 1) || (s2 == 3 && sh <= 1))
            ) {
                n2 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
            }

            // ---- the defects (dead at the committed config) ----
            // Each is session 1's or session 2's wrong step, or the world's
            // wall nobody read, and records itself as any move does (`mv`,
            // `pa`, `ph` from the state before it): no defect marks itself.
            // A session a defect moves leaves its notice as one a sound move
            // does (`n1`, `n2`).
            // A defect fires wherever its guard holds, which at `Buggy = 1`
            // includes a state an earlier defect left.
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
            }
            // Today's rule: the wall is waited out, with a fresh account free.
            action WaitsOnTheWall when (
                Buggy == 1 && w1 == 0 && jobs == 0 && s1 == 1 && d1 == 2 && d2 == 0
                && run == 0 && rot <= Budget - 1
            ) {
                w1 = 1;
                mv = 0;
                pa = 0;
                ph = 0;
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
            }
            // Today's rule at the wall the owner lifted: the notice is waited
            // out to its own reset, on an account that can be run on (what
            // every such session did until 2026-10-01: told to rotate, which
            // the pin refuses, and asked again on every back-off rung).
            action WaitsAtTheLiftedWall when (Buggy == 1 && w1 == 0 && jobs == 0 && n1 == 1) {
                w1 = 1;
                n1 = 0;
                mv = 0;
                pa = 0;
                ph = 0;
            }

            // ---- the laws ----
            // The four about a session's move are stated over what can be SEEN
            // after it — where the session is, what `active` is, what each
            // account's condition is — against the record of the state before
            // it. None reads a mark a step sets on itself: a `Move` that picked
            // wrong IS the violation, whatever it says of itself.
            //
            // Nobody is moved into a limited account: what is read of `active`
            // is true (it is followed only while it can be run on), and a
            // session just moved is not on a limited account.
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
            // A session just moved is on a LOW account only when no account at
            // all is fresh — the one it left included, so nobody goes from a
            // fresh account to a low one — or when it joined `active` rather
            // than rotated: in the place of a canary (nobody rotates past an
            // account that has not answered), under the owner's pin, or with
            // the hour's rotations spent.
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
            // A session sleeps only at its wall, and only for a reason the
            // design names: the agent's jobs, WITHIN THE CANARY'S HOLD, the
            // breaker, or nothing usable. (The hold is the reason while it
            // stands even once a wall is recorded on the canary's account:
            // that wall wakes nobody, and the hold's end does.) The real
            // session has one reason more, which is no variable here: its
            // own latch, after a relaunch its host refused (not modelled).
            // A session at the notice of a wall the owner lifted is on an
            // account that is not limited, so it never sleeps there: waited
            // out, that notice is `WaitsAtTheLiftedWall`.
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
        }
    }
}

/// EVERY USABLE ACCOUNT IS TRIED ([`supervisor_account_rotation_model`]):
/// `[]<>(session 1 is not at its wall \/ its agent's jobs run \/ the canary's
/// hold stands \/ the hour's rotations are spent \/ every other account is
/// limited)` — on every fair behaviour, a session does not sit at a usage wall
/// for good while an account it could be moved to is usable.
///
/// Fairness assumed, and nothing more: the supervisor takes the advice a
/// session gets at its point and makes the move (`Move1`: the design's
/// `Advise` and `Move`, which are one step here) — so the claim is about a
/// session whose host MAKES the relaunch once the agent's jobs let it. A
/// relaunch the host refuses for good (a line that cannot hold the login)
/// latches the real session for the rest of that wall, and is no step of
/// this model's (its "not modelled" list). The world is NOT fair. The
/// agent's jobs may run for ever, and then the session stays where it is.
/// The account a rotation chose may never answer — and that is no exception
/// any more: the claim excepts only the canary's HOLD, which the clock ends,
/// and once it has lapsed the fair step is this session taking the canary's
/// place. (The clock itself is not assumed fair here: `Lapse` may never come,
/// and then the hold stands. `tests/derived_supervise.rs` also checks the
/// reading with the clock fair and no exception for the canary at all.)
///
/// The claim is session 1's. Session 2's twin (the same goal over `s2`, under
/// `Move2`) is checked beside it in `tests/derived_supervise.rs`; the two do
/// not hold as ONE conjunction, because walls that keep resetting can leave
/// one session or the other at its wall at every instant while each keeps
/// being moved.
///
/// Broken alone by `WaitsOnTheWall`: today's rule, a wall waited out with a
/// fresh account free.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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

/// THE `/usage` PROBE (aterm-agent `supervise/turn_end_loop.rs` and
/// `run.rs`: `Session::usage_probe`, `Session::usage_panel_look`; the gate
/// `TurnEndState::probe_point`; the reader `harness::usage::usage_panel_read`
/// and `aterm_phase::settings_panel`; `docs/SPEC-claude-accounts-probe-
/// 2026-10-01.md` §3, §5): the loop types `/usage` into an idle Claude Code,
/// reads the Settings panel it opens, and closes it with an Escape. The
/// property the owner's sessions need: NOTHING IS WRITTEN WHERE IT COULD
/// LAND ANYWHERE ELSE — no `/usage` over a panel or under a person's hand,
/// no Escape without the panel on the grid, under a hand or into a running
/// turn (where Escape stops the turn) — nothing is recorded from a panel
/// that is not settled or was not read whole by the step that opened it,
/// and a panel the loop opened is never forgotten.
///
/// `pan` is the harness's panel on the screen: 0 none, 1 open and
/// UNSETTLED (loading, refreshing, an enrich error, its tail under the
/// fold), 2 open and settled. `ph` is the loop's memory: 0 idle, 1 the
/// host's claim taken, 2 `/usage` typed and the read in flight (the loop's
/// `Owed`, and what the session ledger's `typed` row says to a later loop),
/// 3 closing (the host was told; what is left is the Escape, and seeing the
/// panel gone). `own` is 1 while the panel is one THIS step opened and has
/// read without a break: a person's key, a hold, a turn or a loop restart
/// ends it. `hand` a person's keystroke within the grace; `hold` the session
/// halted (the server refuses every write: no step here writes under one,
/// sound or not); `turn` a turn running. `tries` the Escapes that did not
/// take, to `Tries`, and `told` 1 once the host was told the panel is stuck.
///
/// THE HISTORY, written from the PRE-STATE by every step of its kind, sound
/// or not — no step reports on itself: every Escape writes `blind` (no panel
/// was on the grid), `handkey` (a person's hand) and `turnkey` (a turn
/// running); every `/usage` writes `handkey` and `stacked` (a panel open, or
/// one still owed); every sample writes `stale` (the panel was not settled)
/// and `foreign` (it was not this step's to read). `booked` is the one mark a
/// defect alone writes: the probe is no act of the turn-end policy's (no
/// budget, no awaited act), which is a claim that the step does NOT do
/// something. `bad` is 1 once any defect fired: each fires only from a state
/// the sound loop reaches, so the `Buggy = 1` space stays walkable.
///
/// THE WORLD: the panel opens after the Enter (`Opens`), settles and
/// unsettles (a refresh); a person types and their grace ends; the session
/// is held and released; a turn starts and ends — and the vendor may retire
/// the panel at a turn's boundary (`TurnRetiresThePanel`: READ in the
/// 2.1.287 bundle, not measured); a person closes the panel; the loop is
/// restarted (`LoopRestarts`: a successor reads the ledger, so a panel typed
/// for stays owed, and nothing else is remembered).
///
/// THE SUPERVISOR: `Claim` (the host's claim, at an idle point with nobody's
/// hand on the session), `Type` (the fenced `/usage` and its Enter),
/// `TypeSkipped` (the write was skipped or refused: nothing owed),
/// `GiveUpNotOpened` (the composer is back and no panel came: NO KEY),
/// `Sample` (a settled panel this step read whole), `NoSample` (the read's
/// bound ran out, or the tail is under the fold: the host is told why),
/// `EscCloses` / `EscFails` (the guarded Escape, only with the panel on the
/// grid, nobody's hand, no hold and no turn — and only once the host was
/// told, or for a panel not this step's to read; a failed one climbs the
/// ladder and at `Tries` tells the host, ONCE, and goes on trying),
/// `SeesGone` (the panel is gone: the probe is over).
///
/// # What is NOT modelled
///
/// * TIME: the open wait, the settle wait, the ladder's pauses, the
///   person's grace as a duration (`GraceEnds` is the clock), the 90 s
///   between any two accounts' claims and the 180 s for one (the roster's
///   unit tests).
/// * WHAT THE LOOP CAN KNOW OF A HAND. `hand` is the world's truth and the
///   sound steps read it. A successor loop reads no person stamp from before
///   it started, so the real loop lets a panel it took up from the ledger
///   stand one grace in its own sight before it presses (the spec's P19):
///   that wait is the loop's unit tests'.
/// * The gates that are not this screen's: the renderer (`alt_screen`), the
///   fences a host must know, the fold latch per grid size, the point
///   decided again after the probe, the sample bound to the claimed process.
/// * A person's OWN `/usage` panel: `pan` is the harness's. The residual the
///   spec states (a person who opens `/usage` right after a loop died
///   mid-probe has it closed after two idle minutes) is `LoopRestarts` then
///   `Opens` here.
/// * Two accounts. Nothing here has run against a real session at all: the
///   model is authored from the spec and the vendor's bundle.
///
/// # The defects (`Buggy = 1`), each dead at the committed config and caught
/// alone by its own law
///
/// `EscOnTheOpenTimeout` (an Escape when the panel never came: at an idle
/// composer it does nothing, and a second one within the vendor's window
/// opens Rewind); `EscUnderAHand`, `TypeUnderAHand`; `EscIntoATurn` (it
/// stops the turn); `TypeOverAPanel` (a second `/usage` while one is open or
/// owed); `TypeBooksAnAct`; `SampleUnsettled`; `SampleNotRead` (after a
/// restart or a break); `EscFailsUntold` (the ladder's last rung reached and
/// nobody told); `GoesIdleAtTheLastTry` (the loop gives the panel up at
/// `Tries` and never looks at it again — also the liveness claim's mutant).
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

/// EVERY OPENED PANEL CLOSES ([`supervisor_usage_probe_model`]): `[]<>(no
/// panel is open \/ a person's hand is on the session \/ it is held \/ a
/// turn runs)` — on every fair behaviour, a panel the harness opened does not
/// stand for good while nothing stands in the Escape's way.
///
/// Fairness assumed, and nothing more: the read ends (`NoSample`: its bound
/// runs out, whatever the panel shows), and an Escape pressed for ever at a
/// panel is taken at last (`EscCloses`: the loop presses on its ladder with
/// no last rung, and the vendor's `confirm:no` closes the dialog). THE
/// WORLD IS NOT FAIR, and is not assumed to be: a person may type for ever, a
/// hold may never lift, a turn may never end — each is an EXCEPTION in the
/// goal and not a fairness assumption, because a person, a hold and a turn
/// taking turns can keep the Escape disabled at every instant while each of
/// them ends infinitely often (so fairness of `GraceEnds`, `HoldOff` and
/// `TurnEnds` would not carry the claim, and none is claimed). Nor is the
/// loop's own restart: after one the panel is still owed, and closed.
///
/// Broken alone by `GoesIdleAtTheLastTry`: a loop that gives up at `Tries`
/// and never looks at the panel again leaves it open for good.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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

/// THE LIMIT RESET (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §3,
/// §5; aterm-agent `supervise/policy/reset.rs`: `confirm_verdict`;
/// `harness/accounts.rs`: `claim_reset`, `reset_in_flight`, `reset_outcome`;
/// the readers of `aterm_phase::reset`; and the loop's step, which the spec
/// names `reset_take`, `reset_run` and `reset_look`): at a usage wall whose
/// dialog offers the owner's limit reset, the loop takes the grant row,
/// reads the confirmation that opens, and presses Enter on `Yes, use my
/// reset` — or Escapes it. A reset is the owner's scarce purchase, so the
/// property is: ONE ATTEMPT A WALL, AND NO KEY THAT COULD SPEND ANYTHING
/// ELSE — the row is taken once, on a claim made for it, and never the
/// weekly row that can stand in its slot; Enter is written only on the
/// exact confirmation of the row this loop took, past the vendor's guard,
/// with the focus on Yes and the claim still in flight; nothing is typed
/// and nothing is keyed under a person's hand; only the vendor's own line
/// says a reset was spent; a kept or failed attempt is final; nobody is
/// moved off an account while its reset is being made; and a confirmation
/// the loop opened is never forgotten.
///
/// `dlg` is the vendor's box on session one's screen: 0 none, 1 the wall
/// dialog with the GRANT row (`Use your reset now (…)`), 2 the wall dialog
/// with the WEEKLY row in that slot (`Reset your session limit now`, which
/// claims at once), 3 the dialog's title over `Checking for a reset…`, 4
/// the confirmation, exact, 5 a confirmation the loop must keep (its
/// early-use line, or whatever else `confirm_verdict` keeps it for:
/// another limit, a count or a day that is not the row's, a deadline that
/// does not qualify), 6 the confirmation's title over `Resetting your
/// limits…`. `guard` is 1 while the confirmation is inside the vendor's
/// guard (confirm and cancel are refused for 250 ms from its mount);
/// `focus` is 1 while its focus is on Yes, where it opens.
///
/// `claim` is the machine's one slot for this wall: 0 none, 1 session
/// one's and in flight, 2 done — an outcome recorded, or lapsed with none:
/// attempted either way. `ph` is the loop's memory, which the session
/// ledger keeps for a later loop: 0 idle, 1 the row taken, 2 the Enter
/// written, 3 the Escape written. `own` is 1 while the row was taken by
/// THIS loop: a restart ends it. `line` is what the vendor has said of the
/// attempt, in the transcript: 0 nothing, 1 `Limits reset` (a reset
/// SPENT), 2 the limit had lifted by itself, 3 `Reset kept`, 4 any other
/// line (a failure). `s2` is another session on the same account: 0
/// working, 1 at the wall, 2 moved to another account, 3 continued where
/// it is. `hand` a person's keystroke within the grace; `hold` the session
/// halted (the server refuses every write: no step here writes under one,
/// sound or not).
///
/// THE HISTORY, written from the PRE-STATE by every step of its kind, sound
/// or not — no step reports on itself: every take of a row writes `twice`
/// (no claim was in flight for it, a step was still owed, or a line had
/// already been said at this wall), `weekly` (the dialog held the weekly
/// row) and `handkey` (a person's hand); every Enter writes `early` (the
/// confirmation was not the exact one), `blind` (inside the guard, the
/// focus off Yes, a row another loop took, or a claim no longer in flight)
/// and `handkey`; every Escape and every press of the wait row writes
/// `handkey`; every claim writes `retried` (a kept or failed line stands);
/// every move of session two writes `moved` (the claim in flight, or a
/// spent or lifted line said). `typed` and `falseused` are the two marks a
/// defect alone writes, each a claim that the loop does NOT do something:
/// it types no command, and it records a spend nowhere but at the line
/// that says so. `bad` is 1 once any defect fired: each fires only from a
/// state the sound loop reaches, so the `Buggy = 1` space stays walkable.
///
/// THE WORLD: the vendor opens the wall dialog, with the grant row or with
/// the weekly row in its slot (`WallOpens`, `WallOpensWeekly`: it may come
/// again at the same wall, under a step still owed too); the taken row's
/// check ends in the confirmation, in one the loop must keep, or in an
/// answer line and no confirmation (`OpensTheConfirmation`,
/// `OpensAnInexactConfirmation`, `OpensAnAnswer`); the guard's 250 ms pass
/// (`GuardEnds`); the focus moves (`FocusMoves`); a person types and their
/// grace ends; the session is held and released; the vendor answers its
/// claim (`ServerAnswersSpent`, `ServerAnswersLifted`,
/// `ServerAnswersFailed`); the claim's ten minutes pass (`ClaimLapses`);
/// the loop is restarted (`LoopRestarts`: a successor reads the session
/// ledger, so the step stays owed where its last row stands, and is no
/// longer this loop's own); the other session reaches the wall
/// (`S2AtWall`).
///
/// THE SUPERVISOR: `Claim` (the host's claim, taken last: the grant row
/// read by its template, nobody's hand, no step owed); `TakeRow` (the
/// row's arrows and Enter, on a claim just made for it: the dialog is its
/// title over `Checking…` from there); `Enter` / `EnterDropped` (the fenced
/// Enter on Yes; one the vendor drops leaves the confirmation standing,
/// and it is judged again); `Escape` / `EscapeDropped` (the guarded
/// Escape: at any confirmation of a step that is owed, past the guard —
/// the exact one too, for nothing here makes the loop spend — and at a
/// `Checking…` that outlasts its bound; never at `Resetting…`);
/// `ReadLine` (the step's end: no pending line and no confirmation is on
/// the screen, the transcript's line is read, or its bound has run out,
/// and the host is told: the claim is done and the loop idle); `PressWait`
/// (`limit-wait@v1`'s press of the wait row, with no step owed); and the
/// host's advice to the other session at the wall: `S2Waits` (a moment,
/// while the claim is in flight: it changes nothing, and is here for where
/// it is enabled), `S2Resumes` (a spent or lifted line was recorded, which
/// cleared the walls: it continues where it is), `S2Moves` (the roster's
/// rule 3, as ever: no claim in flight and no reset made).
///
/// # What is NOT modelled
///
/// * TIME: the two 24 h rules a reset qualifies by, the loop's bounds on
///   each wait, the Escape's ladder and its one `inform`, the three Enters
///   before an Escape, the settle as a duration (`GuardEnds` is the clock,
///   and a refused key re-arming the guard is no step). And ONE THING OF
///   TIME THE MODEL ASSUMES: the claim's ten minutes do not run out over a
///   reset in the making. `ClaimLapses` is not enabled at `Resetting…`, nor
///   over a spent or lifted line the loop has not read yet: the vendor
///   bounds its claim at 35 s and the loop its read at 10 s, against ten
///   minutes — so the real Enter must not be written on a claim about to
///   lapse, or another session could be moved off an account whose reset
///   lands a moment later. With a clock free to end any claim, "every claim
///   ends" would hold whatever the loop did.
/// * The default-login rule, the zone, every text. `dlg` 4 against 5 is the
///   whole of what the reader and `confirm_verdict` make of a confirmation.
/// * A PERSON'S OWN confirmation: `dlg` is the box the harness's row
///   opened, and one opened with no step of the harness's is answered by
///   the ordinary rules. Nor a person's own Enter or Escape on the
///   harness's box: `hand` only stops the loop's keys.
/// * A row's press that never lands: `TakeRow` is the press seen through.
///   A claim made with no row taken is ended by the clock here; the real
///   loop records it kept when it leaves the dialog.
/// * `Resetting…` outlasting the loop's bound on it: the vendor's answer
///   comes first here.
/// * What the weekly row or a typed command does once written: the state a
///   defect leaves stands for "the wrong thing is in flight".
/// * The other session beyond the advice it is given: its own dialog
///   (`limit-wait@v1` answers it as ever), its jobs, and the rotation
///   itself ([`supervisor_account_rotation_model`]). And a second wall or a
///   second account: `claim == 2` is this wall's tombstone for good.
/// * `[harness] limit_reset` off: then no step of the supervisor's but
///   `PressWait` exists, and a successor's `Escape` (`own = 0`) of a step
///   an earlier loop opened, which ends with nobody told (the loop's unit
///   tests).
/// * Anything SEEN. Every text and every transition of the vendor's reset
///   program here is READ in the 2.1.287 bundle; none has been on a screen.
///
/// # The defects (`Buggy = 1`), each dead at the committed config
///
/// `TakeRowUnclaimed` (the row taken with no claim made, on one that is
/// done, or under a step still owed) and `NeverReadsTheLine` (the dialog
/// gone after its Enter, the loop goes idle and tells nobody: the claim
/// stays its own, and at the dialog's next mount the row is taken again —
/// also the first liveness claim's mutant): `OneAttemptPerWall`. `EnterTheInexactConfirmation`:
/// `EnterOnlyOnTheExactConfirmation`. `EnterInsideTheGuard`,
/// `EnterOffYes`, `EnterAsASuccessor`, `EnterOnALapsedClaim`:
/// `EnterOnlyPastTheGuardOnYes`. `TakeRowByItsPlace` (the slot's row,
/// whatever it says): `NeverTheWeeklyRow`. `TypesTheCommand`:
/// `NeverTheTypedCommand`. `S2MovesUnderAReset`:
/// `NoMoveOffAnAccountBeingReset`. `TakeRowUnderAHand`, `EnterUnderAHand`,
/// `EscapeUnderAHand`, `PressWaitUnderAHand`: `NothingKeyedUnderAHand`.
/// `UsedRecordedAtEnter` (the Enter taken for the spend):
/// `OnlyTheLineSaysSpent`. `ClaimsAgainAfterAFailure`:
/// `AFailureIsNotRetried` where the claim is made — and
/// `OneAttemptPerWall` a step later, where its row is taken: the same
/// defect seen twice. `StopsTryingItsEscape` (the loop gives a confirmation
/// up after an Escape that did not take — also the second liveness claim's
/// mutant): `AnOpenConfirmationIsOwed`.
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

/// EVERY CLAIM ENDS ([`supervisor_limit_reset_model`]): `[]<>(no claim is
/// in flight)` — on every fair behaviour a claim, which holds every other
/// session of the account at its wall, does not stay in flight for good.
///
/// Fairness assumed, and nothing more. THE LOOP'S: its step ends
/// (`ReadLine`: with no pending line and no confirmation on the screen, the
/// line is read, or its bound runs out, and the host is told). THE CLOCK'S:
/// a claim with no outcome lapses (`ClaimLapses`) — which is what ends one
/// whose loop never took the row, or whose confirmation stands under a
/// person's hand for good. THE VENDOR'S: its own claim is answered, if only
/// by its timeout (`ServerAnswersFailed`: it bounds the call at 35 s, READ).
/// Nothing is assumed of a person, of a hold, of the loop's keys or of what
/// the vendor's check opens.
///
/// The clock alone does not carry it: over a reset in the making the model's
/// claim does not lapse (the model's doc says what that assumes of the real
/// one), so a spent or lifted line ends its claim only by being read.
///
/// Broken alone by `NeverReadsTheLine`: the loop that goes idle at the
/// dialog's end leaves the claim of a reset that was spent in flight for
/// good, and every other session of the account waiting on it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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

/// EVERY HARNESS CONFIRMATION CLOSES ([`supervisor_limit_reset_model`]):
/// `[]<>(no confirmation is on the screen \/ a person's hand is on the
/// session \/ it is held)` — on every fair behaviour, a confirmation the
/// harness's row opened, and the claiming line that follows its Enter, does
/// not stand for good while nothing stands in the Escape's way.
///
/// Fairness assumed, and nothing more: the vendor's guard ends
/// (`GuardEnds`: its 250 ms), an Escape pressed for ever at a confirmation
/// is taken at last (`Escape`: the loop presses on a ladder with no last
/// rung), and the vendor answers its own claim, if only by its timeout
/// (`ServerAnswersFailed`). THE WORLD IS NOT FAIR, and is not assumed to
/// be: a person may type for ever and a hold may never lift — each is an
/// EXCEPTION in the goal and not a fairness assumption, because a person
/// and a hold taking turns can keep the Escape disabled at every instant
/// while each of them ends infinitely often. Nor is the loop's own restart:
/// after one the confirmation is still owed, and Escaped.
///
/// Broken alone by `StopsTryingItsEscape`: a loop that gives the
/// confirmation up after an Escape that did not take leaves it open for
/// good.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
