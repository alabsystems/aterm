// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground handback — the rule that a program which loses the terminal
//! with program-only modes armed hands them back at the byte where it lost it,
//! and not a byte later or earlier.
//!
//! 2026-09-25: a Claude Code was killed while it held the terminal. It had
//! entered the alt screen and armed kitty flags, modifyOtherKeys 2, SGR
//! any-motion mouse tracking, focus reports, 2026 and a hidden cursor; a killed
//! program restores nothing, so the zsh that reclaimed the terminal ran under
//! all of it. Every mouse move typed `ESC[<32;…M` at the prompt, Ctrl chords
//! arrived as CSI-u and zle rang the bell at each, and the prompt was hidden:
//! the owner read the tab as crashed.
//!
//! The fix cuts the PTY stream where `tcgetpgrp(master)` changes and runs
//! `Terminal::foreground_handback` there. The ordering is the whole point: the
//! handback must land AFTER every byte of the dead holder (or it is undone by
//! the holder's tail) and BEFORE the reclaiming shell's own mode setup (zsh's
//! `ESC[?2004h`, which a handback after it would switch off).
//!
//! The 2026-09-25 review added two more obligations. A foreground change is
//! not a death: a job stopped with Ctrl-Z, or a live `gdb -tui` handing the
//! terminal to its inferior, still runs and keeps its modes — stripping them
//! left it running without them after `fg` (`Alive`). And a reader restarted
//! mid-session (the overlap handoff's park, a resume, the adopted readers of
//! the next process) must start from the holder the previous reader last saw,
//! or a job that died while the readers were parked is never an edge at all —
//! the incident again, with no handback (`Carry`).
//!
//! The second 2026-09-25 review added a third. A one-shot command that sets a
//! DISPLAY mode on purpose and exits cleanly — `tput civis`, or the `tput
//! smcup` of the common `tput smcup; cmd; tput rmcup` wrapper — is gone the
//! moment it exits, exactly like a SIGKILLed program, and nothing tells the two
//! apart. Handing its mode back reverted the cursor and dropped the wrapped
//! command onto the main screen. The incident's harm is the INPUT-hijacking
//! modes (mouse, kitty, modifyOtherKeys, 1004, 2048, 2031), so only a gone
//! program that armed one of those is handed back (`Display`).

use super::Model;

/// One job's life under one shell, the gather thread that reads the PTY and
/// samples the foreground group, and the parse thread that runs the handback.
///
/// State:
/// * `fg` — `tcgetpgrp`: 0 the shell, 1 the job. `life` — 0 at the prompt, 1
///   the job holds the terminal, 2 the job is dead, 3 the shell reclaimed it
///   from the dead job, 4 the job let go of the terminal ALIVE (stopped, or
///   handed to its own child under job control), 5 the shell (or that child —
///   the handback cannot tell them apart) holds it while the job lives.
/// * `armed` — the job's modes have been parsed (they are the job's to keep
///   while it lives).
/// * `wj`/`qj` — job writes so far (at most 2) and job output queued unread in
///   the tty; `ws`/`qs` — the shell's post-reclaim output (… zle's `2004h`)
///   written, and queued.
/// * `tail` — HISTORY (residual R1): job output was still unread when the
///   shell reclaimed. Those bytes are parsed after any handback, so the
///   invariant excuses them.
/// * `rd` — 1 while the drain spins right after a read (µs); 0 parked or idle.
///   `Reclaim when rd == 0` is assumption R2: a reap plus `tcsetpgrp` does not
///   complete inside one spin window.
/// * `run` — the cutter's running sample. `bj1`/`bs1` — job/shell output in the
///   open batch BEFORE the cut (at or before the last sample equal to `run`);
///   `bj2`/`bs2` — after the last sample. `edge` — the open batch carries a cut.
/// * `full`, `dj1`, `dj2`, `ds1`, `ds2`, `dedge` — the delivered batch.
/// * `leak` — program-owned modes in force (the evidence gate is open);
///   `paste` — bracketed paste in force; `prompt` — the shell's post-reclaim
///   output has been parsed.
/// * `swept` — the UI thread's status sweep has observed the reclaimed shell.
/// * `hij` — 1 when the job's modes include an INPUT-hijacking one (the
///   incident, `Launch`; its writes arm bracketed paste too), 0 when they are
///   display modes only (a one-shot `tput smcup` / `tput civis`, `OneShot`,
///   whose writes touch nothing else).
///
/// Actions: `Launch` (zle turns 2004 off, the job gets the terminal), its
/// display-only twin `OneShot`, or
/// `Adopt` (the reader attached mid-job: its first sample is the job and the
/// job's modes are already in force); `JobWrite`, `Die`, `Stop` (the job lets
/// go alive), `Reclaim`, `ShellWrite`, `Resume` (`fg`: zle turns 2004 off, the
/// job gets the terminal back and repaints over the shell's text — or the
/// child hands the terminal back); `Restart` (the
/// reader is parked and a new one attached — its cutter is seeded with the
/// carried holder, or with a fresh probe at `Carry = 0`). `Die` is taken only
/// once the reader has seen the job hold the terminal (`run == 1`), with the
/// job's output still queued (the tail), or with nothing armed: a job whose
/// whole life falls between two samples is residual R3. `ReadJob`/`ReadShell` (FIFO: the shell's bytes are behind the
/// job's); `Park` (the pre-park sample: unchanged moves the cut forward,
/// changed sets `edge` and delivers); `Deliver` (the batch-end sample);
/// `Parse` (side 1, the handback at the cut iff there is evidence, side 2);
/// `Sweep` (the UI thread's status sweep sees the shell back in front — at a
/// moment unordered against the byte stream, and without knowing whether the
/// job that left is dead or only stopped).
///
/// Invariants: `PromptNotHijacked` — once the shell's output is parsed, no
/// program mode is in force unless R1's tail put it back, the job that armed
/// them is alive (`life == 5`: a stopped job keeps its modes, as it did before
/// the handback existed), or they are a one-shot's display modes (`hij == 0`:
/// the shell's input is not hijacked by them). `OneShotKeepsItsModes` — a
/// display-only job's modes, once parsed, stay in force: they were set on
/// purpose. `ShellKeepsItsOwnModes` — once the shell's output is
/// parsed, its `2004h` is in force: the handback never lands after it.
/// `JobKeepsItsModesWhileAlive` — once the job's modes are parsed, they stay in
/// force for as long as the job exists.
///
/// Constants: `Buggy = 1` is the design this replaced — no in-stream handback
/// (a schedule without `Sweep` is exactly the 2026-09-25 terminal), with the
/// restore done by the status sweep instead. It is caught on BOTH invariants:
/// a sweep that runs after the prompt drew switches the shell's own `2004h`
/// off, and one that has not run yet leaves the prompt under the dead
/// program's modes; and a sweep that finds the shell in front of a STOPPED job
/// strips the job's modes (`JobKeepsItsModesWhileAlive`). At `Buggy = 0` the sweep still runs and touches no mode
/// (the handback never runs from the sweep). `Split = 0` samples only at batch
/// end, which lets a batch carry both the job's tail and the shell's prompt
/// with nowhere to cut between them (caught on `PromptNotHijacked`; Tier-0 in
/// `aterm-spec/tests/derived_foreground_handback.rs`). `Alive = 0` is the design
/// the review caught: hand back at EVERY foreground change with evidence — a
/// stopped job loses its modes (caught on `JobKeepsItsModesWhileAlive`).
/// `Carry = 0` seeds a restarted reader with a fresh probe: a job that died
/// while the readers were parked leaves no edge (caught on
/// `PromptNotHijacked`). `Display = 1` is the rule the second review caught:
/// display modes alone count as orphaned, so `tput smcup` is reverted the
/// moment `tput` exits (caught on `OneShotKeepsItsModes`).
///
/// Tier-1 (`aterm-gui/src/foreground_handback_conformance.rs`) drives the real
/// PTY reader (gather + parse threads) with a scripted foreground probe through
/// the incident, adopted, stop/`fg` and parked-reader schedules (and a live
/// holder handing the terminal to its own child), projects `fg`, `life`,
/// `leak`, `paste` and `prompt` from the real engine at every checkpoint, and
/// replays the incident with no probe, and the parked schedule with no carry,
/// as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foreground_handback_model() -> Model {
    crate::ty_model! {
        ForegroundHandback {
            const Buggy = 0;
            const Split = 1;
            const Alive = 1;
            const Carry = 1;
            const Display = 0;
            var fg = 0;
            var life = 0;
            var wj = 0;
            var qj = 0;
            var ws = 0;
            var qs = 0;
            var tail = 0;
            var rd = 0;
            var run = 0;
            var bj1 = 0;
            var bj2 = 0;
            var bs1 = 0;
            var bs2 = 0;
            var edge = 0;
            var full = 0;
            var dj1 = 0;
            var dj2 = 0;
            var ds1 = 0;
            var ds2 = 0;
            var dedge = 0;
            var leak = 0;
            var paste = 1;
            var prompt = 0;
            var swept = 0;
            var armed = 0;
            var hij = 1;

            action Launch when (life == 0) { life = 1; fg = 1; paste = 0; }
            action OneShot when (life == 0) { life = 1; fg = 1; paste = 0; hij = 0; }
            action Adopt when (life == 0) { life = 1; fg = 1; run = 1; leak = 1; armed = 1; }
            action JobWrite when (life == 1 && wj <= 1) { wj = wj + 1; qj = qj + 1; }
            action Die when (life == 1 && (run == 1 || qj > 0 || armed == 0)) { life = 2; }
            action Stop when (life == 1) { life = 4; }
            action Reclaim when ((life == 2 || life == 4) && rd == 0) {
                life = if life == 2 { 3 } else { 5 };
                fg = 0;
                tail = if qj > 0 { 1 } else { tail };
            }
            action ShellWrite when ((life == 3 || life == 5) && ws == 0) { ws = 1; qs = 1; }
            action Resume when (life == 5 && qs == 0 && full == 0 && bs1 + bs2 == 0) {
                life = 1;
                fg = 1;
                ws = 0;
                prompt = 0;
                paste = 0;
            }
            action Restart when (rd == 0 && full == 0 && bj1 + bj2 + bs1 + bs2 == 0) {
                run = if Carry == 1 { run } else { fg };
            }
            action ReadJob when (qj > 0 && edge == 0) { qj = qj - 1; bj2 = 1; rd = 1; }
            action ReadShell when (qs == 1 && qj == 0 && edge == 0) { qs = 0; bs2 = 1; rd = 1; }
            action Park when (rd == 1) {
                rd = 0;
                bj1 = if Split == 1 && fg == run && bj2 == 1 { 1 } else { bj1 };
                bj2 = if Split == 1 && fg == run { 0 } else { bj2 };
                bs1 = if Split == 1 && fg == run && bs2 == 1 { 1 } else { bs1 };
                bs2 = if Split == 1 && fg == run { 0 } else { bs2 };
                edge = if Split == 1 { if fg == run { 0 } else { 1 } } else { edge };
                run = if Split == 1 { fg } else { run };
            }
            action Deliver when (rd == 0 && full == 0 && bj1 + bj2 + bs1 + bs2 > 0) {
                dj1 = if edge == 0 && fg == run { if bj1 + bj2 > 0 { 1 } else { 0 } } else { bj1 };
                dj2 = if edge == 0 && fg == run { 0 } else { bj2 };
                ds1 = if edge == 0 && fg == run { if bs1 + bs2 > 0 { 1 } else { 0 } } else { bs1 };
                ds2 = if edge == 0 && fg == run { 0 } else { bs2 };
                dedge = if edge == 1 { 1 } else { if fg == run { 0 } else { 1 } };
                run = if edge == 1 { run } else { fg };
                bj1 = 0;
                bj2 = 0;
                bs1 = 0;
                bs2 = 0;
                edge = 0;
                full = 1;
            }
            action Parse when (full == 1) {
                leak = if dj2 == 1 { 1 } else {
                    if dedge == 1 && Buggy == 0 && (dj1 == 1 || leak == 1)
                        && (Alive == 0 || life == 2 || life == 3)
                        && (Display == 1 || hij == 1) { 0 } else {
                        if dj1 == 1 { 1 } else { leak }
                    }
                };
                paste = if ds2 == 1 || (dj2 == 1 && hij == 1) { 1 } else {
                    if dedge == 1 && Buggy == 0 && (dj1 == 1 || leak == 1)
                        && (Alive == 0 || life == 2 || life == 3)
                        && (Display == 1 || hij == 1) { 0 } else {
                        if ds1 == 1 || (dj1 == 1 && hij == 1) { 1 } else { paste }
                    }
                };
                armed = if dj1 + dj2 > 0 { 1 } else { armed };
                prompt = if ds1 + ds2 > 0 { 1 } else { prompt };
                full = 0;
                dj1 = 0;
                dj2 = 0;
                ds1 = 0;
                ds2 = 0;
                dedge = 0;
            }

            action Sweep when ((life == 3 || life == 5) && swept == 0) {
                swept = 1;
                leak = if Buggy == 1 { 0 } else { leak };
                paste = if Buggy == 1 { 0 } else { paste };
            }

            invariant PromptNotHijacked:
                prompt == 0 || leak == 0 || tail == 1 || life == 5 || hij == 0;
            invariant ShellKeepsItsOwnModes: prompt == 0 || paste == 1;
            invariant JobKeepsItsModesWhileAlive:
                armed == 0 || leak == 1 || life == 2 || life == 3;
            invariant OneShotKeepsItsModes: hij == 1 || armed == 0 || leak == 1;
        }
    }
}

/// Who owns an INPUT mode the handback may have to hand back — the parse
/// stage's `FgOwners` (`aterm-gui/src/foreground_handback.rs`) — across
/// several jobs under one shell, when the reader can MISS a job.
///
/// 2026-09-27, the handback lane at load 59-65: a starved gather read a
/// one-shot `/usr/bin/printf '\e[?1000h'` only after zsh had taken the
/// terminal back (residuals R1/R3 of [`foreground_handback_model`]), so its
/// bytes were parsed as the SHELL's and the mouse bit's owner became the
/// shell. Losing that one handback is the documented residual. What was not:
/// `FgOwners::observe` moved a bit's owner only when the bit came ON, so every
/// later job that armed mouse tracking over the stale bit (`?1003h`: the
/// evidence stays "mouse on") never owned it, the only owner was the shell —
/// the new holder at every death edge, and alive — and no later death in that
/// session was ever handed back again (ten lane rows, the holder's SIGKILL
/// included). zsh's builtin `printf '\e[?1000h'` reached the same state with
/// no load at all. The rule now: a bit whose SETTER the holder's bytes carry
/// is that holder's, whether or not it was already on
/// (`Terminal::take_evidence_asserted`).
///
/// State:
/// * `n` — jobs launched so far (at most `Jobs`); `life` — 0 the shell holds
///   the terminal, 1 a job holds it, 2 the job is dead and the shell has not
///   reclaimed yet.
/// * `seen` — the reader has sampled the current job holding the terminal: its
///   bytes are parsed as its own. A job the reader never sees is residual R3
///   (its whole life between two samples) or R1 (its bytes read after the
///   reclaim): its bytes are parsed as the shell's.
/// * `bit` — the input mode (mouse tracking) is in force; `owner` — who
///   `FgOwners` says armed it: 0 nobody, 1 the shell, 2 the current job.
/// * `obs` — the current job armed the mode in bytes parsed as its own;
///   `done` — HISTORY: the job the shell just reclaimed from did (reset when
///   the shell or the next job arms anything); `backs` — handbacks so far.
///
/// Actions: `Launch` (the shell gives a job the terminal), `Sample` (the
/// gather sees it), `Arm` (the job's `?1000h`/`?1003h` is parsed: as the
/// job's once seen, as the shell's before), `Die`, `Reclaim` (the shell takes
/// the terminal back: the edge exists only when the job was seen, and the
/// handback runs when the dead job owns the input mode), `ShellArm` (the shell
/// arms the mode itself: zsh's builtin `printf`).
///
/// Invariant: `ObservedArmIsHandedBack` — at the prompt after a job that
/// armed the mode in its OWN bytes, the mode is not in force. A missed job
/// (`obs == 0`) loses its handback — the residual — and only its own: the
/// next job that arms is handed back.
///
/// `Buggy = 1` is the rule this replaced: a bit that stayed on keeps its
/// owner. Caught after one missed job (the lane's ten FAILs) and after the
/// shell's own `printf`.
///
/// Tier-1 (`aterm-gui/src/foreground_handback_conformance.rs`, the
/// `ownership_*` rows) drives the real PTY reader with a scripted probe that
/// never shows the missed one-shot, projects `bit` (mouse tracking on),
/// `life`, `n` and `backs` (the timeline's `modes-restored` events) at every
/// checkpoint, and checks that the real end state is NOT the `Buggy = 1`
/// replay's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foreground_handback_ownership_model() -> Model {
    crate::ty_model! {
        ForegroundHandbackOwnership {
            const Buggy = 0;
            const Jobs = 2;
            var n = 0;
            var life = 0;
            var seen = 0;
            var bit = 0;
            var owner = 0;
            var obs = 0;
            var done = 0;
            var backs = 0;

            action Launch when (life == 0 && n <= Jobs - 1) {
                life = 1;
                n = n + 1;
                seen = 0;
                obs = 0;
                done = 0;
            }
            action Sample when (life == 1 && seen == 0) { seen = 1; }
            action Arm when (life == 1) {
                owner = if Buggy == 1 && bit == 1 { owner } else {
                    if seen == 1 { 2 } else { 1 }
                };
                bit = 1;
                obs = if seen == 1 { 1 } else { obs };
            }
            action Die when (life == 1) { life = 2; }
            action Reclaim when (life == 2) {
                bit = if seen == 1 && owner == 2 { 0 } else { bit };
                owner = if seen == 1 && owner == 2 { 0 } else { owner };
                backs = if seen == 1 && owner == 2 { backs + 1 } else { backs };
                done = obs;
                life = 0;
            }
            action ShellArm when (life == 0) {
                owner = if Buggy == 1 && bit == 1 { owner } else { 1 };
                bit = 1;
                done = 0;
            }

            invariant ObservedArmIsHandedBack: life > 0 || done == 0 || bit == 0;
        }
    }
}
