// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground handback: a program that loses the terminal with program-only modes armed
//! hands them back at the byte where it lost it. A killed program restores nothing, so without
//! it the reclaiming shell runs under its alt screen, mouse tracking, kitty flags and hidden
//! cursor.
//!
//! The PTY stream is cut where `tcgetpgrp(master)` changes and `Terminal::foreground_handback`
//! runs there. It must land after every byte of the dead holder (or the holder's tail undoes it)
//! and before the reclaiming shell's own mode setup (zsh's `ESC[?2004h`, which a later handback
//! would switch off).
//!
//! Three more obligations. A foreground change is not a death: a job stopped with Ctrl-Z, or a
//! live `gdb -tui` handing the terminal to its inferior, keeps its modes (`Alive`). A reader
//! restarted mid-session (the overlap handoff's park, a resume, the next process's adopted
//! readers) starts from the holder the previous reader last saw, or a job that died while the
//! readers were parked is never an edge (`Carry`). A one-shot command that sets a display mode
//! on purpose and exits (`tput civis`, the `tput smcup` of `tput smcup; cmd; tput rmcup`) looks
//! exactly like a killed program, so only a gone program that armed an input-hijacking mode
//! (mouse, kitty, modifyOtherKeys, 1004, 2004, 2048, 2031) is handed back (`Display`). 2004 is
//! one because a `cat` that armed it leaves the host's multi-line paste guard off
//! (docs/AUDIT-side-effects-2026-10-07.md, PG1).

use aterm_spec::derive::Model;

/// One job's life under one shell, the gather thread that reads the PTY and samples the
/// foreground group, and the parse thread that runs the handback.
///
/// State:
/// * `fg` — `tcgetpgrp`: 0 the shell, 1 the job. `life` — 0 at the prompt, 1 the job holds the
///   terminal, 2 the job is dead, 3 the shell reclaimed it from the dead job, 4 the job let go
///   alive (stopped, or handed to its own child), 5 the shell (or that child: the handback cannot
///   tell them apart) holds it while the job lives.
/// * `armed` — the job's modes have been parsed (the job's to keep while it lives).
/// * `wj`/`qj` — job writes so far (at most 2) and job output queued unread in the tty; `ws`/`qs`
///   — the shell's post-reclaim output (zle's `2004h`) written, and queued.
/// * `tail` — history (residual R1): job output was still unread when the shell reclaimed. Those
///   bytes are parsed after any handback, so the invariant excuses them.
/// * `rd` — 1 while the drain spins right after a read (µs); 0 parked or idle. `Reclaim when
///   rd == 0` is assumption R2: a reap plus `tcsetpgrp` does not complete inside one spin window.
/// * `run` — the cutter's running sample. `bj1`/`bs1` — job/shell output in the open batch
///   before the cut (at or before the last sample equal to `run`); `bj2`/`bs2` — after the last
///   sample. `edge` — the open batch carries a cut.
/// * `full`, `dj1`, `dj2`, `ds1`, `ds2`, `dedge` — the delivered batch.
/// * `leak` — program-owned modes other than 2004 in force; `paste` — bracketed paste in force;
///   `prompt` — the shell's post-reclaim output has been parsed. `leak` and `paste` each open the
///   evidence gate, so `leak == 0` does not mean the gate is closed. 2004 is kept apart because
///   zle arms it at every prompt as the live shell's own; the job arms it only alongside an input
///   mode (`hij`), so the job's handback reads `leak`.
/// * `swept` — the UI thread's status sweep has observed the reclaimed shell.
/// * `hij` — 1 when the job's modes include an input-hijacking one (`Launch`; its writes arm
///   bracketed paste too), 0 for display modes only (`OneShot`: `tput smcup` / `tput civis`).
///
/// Actions: `Launch` (zle turns 2004 off, the job gets the terminal), its display-only twin
/// `OneShot`, or `Adopt` (the reader attached mid-job: its first sample is the job and the job's
/// modes are already in force); `JobWrite`, `Die`, `Stop` (the job lets go alive), `Reclaim`,
/// `ShellWrite`, `Resume` (`fg`: zle turns 2004 off, the job gets the terminal back and repaints,
/// or the child hands it back); `Restart` (the reader is parked and a new one attached, its cutter
/// seeded with the carried holder, or a fresh probe at `Carry = 0`). `Die` is taken only once the
/// reader has seen the job hold the terminal (`run == 1`), with output still queued (the tail), or
/// with nothing armed: a job whose whole life falls between two samples is residual R3.
/// `ReadJob`/`ReadShell` (FIFO: the shell's bytes are behind the job's); `Park` (the pre-park
/// sample: unchanged moves the cut forward, changed sets `edge` and delivers); `Deliver` (the
/// batch-end sample); `Parse` (side 1, the handback at the cut iff there is evidence, side 2);
/// `Sweep` (the UI thread's status sweep sees the shell back in front, unordered against the byte
/// stream and blind to whether the job left dead or stopped).
///
/// Invariants: `PromptNotHijacked` — once the shell's output is parsed, no program mode is in
/// force unless R1's tail put it back, the job is alive (`life == 5`), or they are a one-shot's
/// display modes (`hij == 0`). `OneShotKeepsItsModes` — a display-only job's parsed modes stay in
/// force. `ShellKeepsItsOwnModes` — once the shell's output is parsed, its `2004h` is in force.
/// `JobKeepsItsModesWhileAlive` — the job's parsed modes stay in force while it exists.
///
/// Constants: `Buggy = 1` restores modes from the status sweep instead of in-stream: a sweep after
/// the prompt switches the shell's `2004h` off (`ShellKeepsItsOwnModes`), one not yet run leaves
/// the prompt under the dead program's modes (`PromptNotHijacked`), and one that finds a stopped
/// job strips its modes (`JobKeepsItsModesWhileAlive`). At `Buggy = 0` the sweep runs and touches
/// no mode. `Split = 0` samples only at batch end, so a batch can carry the job's tail and the
/// shell's prompt with nowhere to cut (`PromptNotHijacked`). `Alive = 0` hands back at every
/// foreground change with evidence, so a stopped job loses its modes
/// (`JobKeepsItsModesWhileAlive`). `Carry = 0` seeds a restarted reader with a fresh probe, so a
/// job that died while the readers were parked leaves no edge (`PromptNotHijacked`).
/// `Display = 1` counts display modes alone as orphaned, so `tput smcup` is reverted when `tput`
/// exits (`OneShotKeepsItsModes`).
///
/// Tier-1 (`aterm-gui/src/foreground_handback_conformance.rs`) drives the real PTY reader (gather
/// and parse threads) with a scripted foreground probe through the killed-holder, adopted,
/// stop/`fg`, parked-reader and holder-hands-to-child schedules, projects `fg`, `life`, `leak`,
/// `paste` and `prompt` at every checkpoint, and replays the killed holder with no probe and the
/// parked schedule with no carry as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foreground_handback_model() -> Model {
    aterm_spec::ty_model! {
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

/// Who owns an input mode the handback may have to hand back (`FgOwners` in
/// `aterm-gui/src/foreground_handback.rs`) across several jobs under one shell, when the reader
/// can miss a job.
///
/// A job the reader misses (residuals R1/R3 of [`foreground_handback_model`]) has its bytes parsed
/// as the shell's, so the shell owns the mode and that one handback is lost. The rule that keeps
/// the loss to that job: a bit whose setter the holder's bytes carry is that holder's, whether or
/// not it was already on (`Terminal::take_evidence_asserted`, `FgOwners::observe`). Moving the
/// owner only when the bit comes on would leave the shell owning every later job's `?1003h` over
/// the stale bit, so no later death is handed back; zsh's builtin `printf '\e[?1000h'` reaches
/// that state with no missed job.
///
/// State:
/// * `n` — jobs launched so far (at most `Jobs`); `life` — 0 the shell holds the terminal, 1 a
///   job holds it, 2 the job is dead and the shell has not reclaimed yet.
/// * `seen` — the reader has sampled the current job holding the terminal, so its bytes are
///   parsed as its own; an unseen job's are parsed as the shell's.
/// * `bit` — mouse tracking is in force; `owner` — who `FgOwners` says armed it: 0 nobody, 1 the
///   shell, 2 the current job.
/// * `obs` — the current job armed the mode in bytes parsed as its own; `done` — history: the job
///   the shell just reclaimed from did (reset when the shell or the next job arms anything);
///   `backs` — handbacks so far.
///
/// Actions: `Launch`, `Sample` (the gather sees the job), `Arm` (the job's `?1000h`/`?1003h` is
/// parsed: as the job's once seen, as the shell's before), `Die`, `Reclaim` (the edge exists only
/// when the job was seen; the handback runs when the dead job owns the mode), `ShellArm` (the
/// shell arms the mode itself).
///
/// Invariant: `ObservedArmIsHandedBack` — at the prompt after a job that armed the mode in its own
/// bytes, the mode is not in force. A missed job (`obs == 0`) loses only its own handback.
///
/// `Buggy = 1`: a bit that stayed on keeps its owner; caught after one missed job and after the
/// shell's own `printf`.
///
/// Tier-1 (`aterm-gui/src/foreground_handback_conformance.rs`, the
/// `foreground_handback_ownership_*` tests) drives the real PTY reader with a scripted probe that
/// never shows the missed one-shot, projects `bit`, `life`, `n` and `backs` (the timeline's
/// `modes-restored` events) at every checkpoint, and checks the real end state is not the
/// `Buggy = 1` replay's.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn foreground_handback_ownership_model() -> Model {
    aterm_spec::ty_model! {
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
