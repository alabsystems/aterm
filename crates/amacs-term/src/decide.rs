// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which face an `amacs` run edits with (design §6.1): pure, and table-tested
//! row by row (`tests/decide_table.rs`).
//!
//! [`decide`] reads the rows that come before the handoff, in the design's
//! order: a terminal asked for, then no aterm window to hand to, then no
//! terminal to edit in. When none of them decides, the face is native and the
//! client asks the window ([`aterm_ctl::edit::edit`]); [`after_handoff`] reads
//! the answer's rows, and [`wait_status`] the wait's (design §6.5).
//!
//! The rules themselves are scalar fns ([`native_face`], [`no_terminal_exit`],
//! [`wait_status`]) that carry Trust contracts: each `ensures` cites a Clean
//! theorem about the compiled body, kernel-checked whenever Trust verifies
//! the crate (SPECS.md). Trust's kernel import takes only scalars today, so
//! [`decide`] and the client are thin wrappers that hand the cores their
//! inputs as `bool`s and `u8`s.

/// `[amacs] open_in` (design §9): where `amacs` edits when it could do either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OpenIn {
    /// A native view in the aterm window hosting the tab (the default).
    #[default]
    Window,
    /// This terminal, as `-nw` asks.
    Terminal,
}

/// What [`decide`] reads: the flags given and the facts of the caller's
/// terminal and environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Inputs {
    /// `-nw` or `--tty`.
    pub tty_flag: bool,
    /// `--wait`.
    pub wait: bool,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `ATERM_PARENT_SESSION_ID` is set: an aterm window hosts this terminal.
    pub parent_session: bool,
    /// `TMUX` is set: a tmux pane, whose window may not be the one hosting it.
    pub tmux: bool,
    /// `STY` is set: a GNU screen window, likewise.
    pub sty: bool,
    /// Standard input is a terminal.
    pub stdin_tty: bool,
    /// Standard output is a terminal.
    pub stdout_tty: bool,
    pub open_in: OpenIn,
}

/// The face an edit runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Face {
    /// Hand the file to the aterm window hosting this terminal and wait.
    Native,
    /// Edit in this terminal.
    Terminal,
    /// Edit nowhere: exit with this status, saying this on stderr.
    Exit(u8, String),
}

/// The status of a run with no terminal to edit in, and of a usage error.
pub const EXIT_USAGE: u8 = 2;

/// What a run with neither a terminal nor `--wait`/`--no-wait` says. The
/// design's line goes on `, or amacs --apply for scripted edits`; that clause
/// joins it with `--apply` (slice A3), so the line never names a flag that
/// does not exist.
pub const NO_TERMINAL: &str =
    "amacs: no terminal to edit in (stdin/stdout are not a tty); use --wait to open a window view";

/// What a handoff whose delivery is uncertain says: the window may have
/// taken the request and the view may still open, so no second editor starts.
pub const NO_ANSWER: &str = "amacs: the aterm window may have taken the request but no answer came; \
                             nothing else was opened, look for its Editor tab";

/// The rows before the handoff, in the design's order: [`native_face`] and
/// [`no_terminal_exit`] decide, and every other run edits in the terminal.
#[must_use]
pub fn decide(inputs: &Inputs) -> Face {
    let Inputs {
        tty_flag,
        wait,
        no_wait,
        parent_session,
        tmux,
        sty,
        stdin_tty,
        stdout_tty,
        open_in,
    } = *inputs;
    let open_in_terminal = open_in == OpenIn::Terminal;
    if native_face(
        tty_flag,
        open_in_terminal,
        parent_session,
        tmux,
        sty,
        stdin_tty,
        stdout_tty,
        wait,
        no_wait,
    ) {
        return Face::Native;
    }
    if no_terminal_exit(
        tty_flag,
        open_in_terminal,
        parent_session,
        tmux,
        sty,
        stdin_tty,
        stdout_tty,
        wait,
        no_wait,
    ) {
        return Face::Exit(EXIT_USAGE, NO_TERMINAL.to_string());
    }
    Face::Terminal
}

trust_contract_fn! {
    /// Design §6.1's rows before the handoff, read in order, say the native
    /// face: no terminal asked for (`-nw`/`--tty`, `[amacs] open_in =
    /// "terminal"`), an aterm window hosting this terminal with no tmux or
    /// screen between, and a terminal on both stdin and stdout or a wait flag
    /// given. Proved for every input: an agent's tool call (no terminal) never
    /// opens a view, and so never hangs on one, unless it asked with `--wait`
    /// or `--no-wait`.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "Trust's kernel import takes scalars only: each input of the table is its own bool"
    )]
    pub fn native_face(
        tty_flag: bool,
        open_in_terminal: bool,
        parent_session: bool,
        tmux: bool,
        sty: bool,
        stdin_tty: bool,
        stdout_tty: bool,
        wait: bool,
        no_wait: bool,
    ) -> bool
    clauses [ensures result == (!tty_flag && !open_in_terminal && parent_session && !tmux && !sty
        && ((stdin_tty && stdout_tty) || wait || no_wait)) by native_face_is_its_rows]
    {
        if tty_flag || open_in_terminal {
            return false;
        }
        if !parent_session || tmux || sty {
            return false;
        }
        (stdin_tty && stdout_tty) || wait || no_wait
    }
}

trust_contract_fn! {
    /// Design §6.1's third row, under the two before it: an aterm window hosts
    /// this terminal directly and no terminal was asked for, but stdin or
    /// stdout is not a terminal and neither `--wait` nor `--no-wait` was
    /// given. The run exits [`EXIT_USAGE`] saying [`NO_TERMINAL`].
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "Trust's kernel import takes scalars only: each input of the table is its own bool"
    )]
    pub fn no_terminal_exit(
        tty_flag: bool,
        open_in_terminal: bool,
        parent_session: bool,
        tmux: bool,
        sty: bool,
        stdin_tty: bool,
        stdout_tty: bool,
        wait: bool,
        no_wait: bool,
    ) -> bool
    clauses [ensures result == (!tty_flag && !open_in_terminal && parent_session && !tmux && !sty
        && !(stdin_tty && stdout_tty) && !wait && !no_wait) by no_terminal_exit_is_its_rows]
    {
        if tty_flag || open_in_terminal {
            return false;
        }
        if !parent_session || tmux || sty {
            return false;
        }
        // An agent's tool call has no terminal: it must never hang, and never
        // open a view nobody watches, unless it said which it wants.
        !(stdin_tty && stdout_tty) && !wait && !no_wait
    }
}

// Specs `native-face` and `no-terminal-exit` (SPECS.md): for every one of the
// 512 inputs, each row fn is its closed form, and the two never both hold, so
// `decide` reads the same table whichever it asks first. Kernel-checked
// against the compiled bodies.
verify_island! {
    clean {
        theorem native_face_is_its_rows
            (tty term parent tmux sty stdin stdout wait nowait : Bool) :
            trust_import_amacs_term__decide__native_face
                tty term parent tmux sty stdin stdout wait nowait =
              (((((!tty && !term) && parent) && !tmux) && !sty)
                && (((stdin && stdout) || wait) || nowait)) := by
          simp only [trust_import_amacs_term__decide__native_face]
          cases tty <;> cases term <;> cases parent <;> cases tmux <;> cases sty <;>
            cases stdin <;> cases stdout <;> cases wait <;> cases nowait <;> rfl

        theorem no_terminal_exit_is_its_rows
            (tty term parent tmux sty stdin stdout wait nowait : Bool) :
            trust_import_amacs_term__decide__no_terminal_exit
                tty term parent tmux sty stdin stdout wait nowait =
              (((((((!tty && !term) && parent) && !tmux) && !sty)
                && !(stdin && stdout)) && !wait) && !nowait) := by
          simp only [trust_import_amacs_term__decide__no_terminal_exit]
          cases tty <;> cases term <;> cases parent <;> cases tmux <;> cases sty <;>
            cases stdin <;> cases stdout <;> cases wait <;> cases nowait <;> rfl

        theorem native_face_and_no_terminal_exit_are_exclusive
            (tty term parent tmux sty stdin stdout wait nowait : Bool) :
            (trust_import_amacs_term__decide__native_face
                tty term parent tmux sty stdin stdout wait nowait
              && trust_import_amacs_term__decide__no_terminal_exit
                tty term parent tmux sty stdin stdout wait nowait) = false := by
          simp only [trust_import_amacs_term__decide__native_face,
            trust_import_amacs_term__decide__no_terminal_exit]
          cases tty <;> cases term <;> cases parent <;> cases tmux <;> cases sty <;>
            cases stdin <;> cases stdout <;> cases wait <;> cases nowait <;> rfl
    }
}

/// How the window answered the request for a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// `OK edit`: the view is open.
    Opened,
    /// `ERR …`: the window refused it.
    Refused,
    /// No window could be asked: the connection failed.
    Unreachable,
    /// The window took the request and did not answer in time (its own
    /// `call_main` deadline, or the client's).
    TimedOut,
}

/// The rows after the handoff: an open view is waited on; a refusal or a
/// window nobody could reach falls back to this terminal; a request the
/// window took and never answered exits 1, never a second editor.
#[inline]
#[must_use]
pub fn after_handoff(handoff: Handoff) -> Face {
    match handoff {
        Handoff::Opened => Face::Native,
        Handoff::Refused | Handoff::Unreachable => Face::Terminal,
        Handoff::TimedOut => Face::Exit(1, NO_ANSWER.to_string()),
    }
}

// How the wait on one file's view ended (design §6.5), as `wait_status` reads
// it. Trust's kernel import takes no enum yet (SPECS.md, Gap 1), so the client
// names the outcome with one of these scalars.

/// The person pressed done (C-c C-c): saved, then closed.
pub const WAIT_FINISHED: u8 = 0;
/// The view closed; the file on disk is the answer.
pub const WAIT_CLOSED: u8 = 1;
/// The person pressed abort (C-c C-k): changes discarded, then closed.
pub const WAIT_ABORTED: u8 = 2;
/// The aterm serving the edit went away and no successor took the wait.
pub const WAIT_REPLACED: u8 = 3;
/// The wait failed any other way (the window refused it, or answered a line
/// the client cannot read).
pub const WAIT_FAILED: u8 = 4;

trust_contract_fn! {
    /// The exit status a wait that ended as `end` (a `WAIT_*`) leaves, or 0
    /// when the run goes on to its next file (design §6.5): a finished or
    /// closed view is 0, `git commit` and the like reading the file on disk;
    /// an abort is 1, so the caller drops what it was editing for; a server
    /// gone with no successor is [`aterm_ctl::EXIT_REPLACED`]; any other
    /// failure is 1. Proved for every `u8`.
    #[must_use]
    pub fn wait_status(end: u8) -> u8
    clauses [ensures result == wait_status_table(end) by wait_status_is_the_table]
    {
        match end {
            WAIT_FINISHED | WAIT_CLOSED => 0,
            WAIT_REPLACED => aterm_ctl::EXIT_REPLACED,
            _ => 1,
        }
    }
}

// Spec `wait-status` (SPECS.md): design §6.5's table, kernel-checked against
// the compiled body for every `u8`. Read off the table, the status is 0 only
// for a finished or closed view, never for an abort or a failure.
verify_island! {
    clean {
        def wait_status_table (e : UInt8) : UInt8 :=
          if e = 0 then 0 else if e = 1 then 0 else if e = 3 then 75 else 1

        theorem wait_status_is_the_table (e : UInt8) :
            trust_import_amacs_term__decide__wait_status e = wait_status_table e := by
          simp only [trust_import_amacs_term__decide__wait_status, wait_status_table]
    }
}
