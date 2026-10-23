// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Face selection ([`decide`]) and handoff ([`after_handoff`], [`wait_status`]) for `amacs`.

/// Preferred face from `[amacs] open_in` when either face is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OpenIn {
    /// A native view in the aterm window hosting the tab (the default).
    #[default]
    Window,
    /// This terminal, as `-nw` asks.
    Terminal,
}

/// What [`decide`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Inputs {
    /// `-nw` or `--tty`.
    pub tty_flag: bool,
    /// `--wait`.
    pub wait: bool,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `ATERM_PARENT_SESSION_ID` is set.
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

/// What a run with neither a terminal nor `--wait`/`--no-wait` says.
pub const NO_TERMINAL: &str =
    "amacs: no terminal to edit in (stdin/stdout are not a tty); use --wait to open a window view";

/// What a handoff whose delivery is uncertain says.
pub const NO_ANSWER: &str = "amacs: the aterm window may have taken the request but no answer came; \
                             nothing else was opened, look for its Editor tab";

/// The rows before the handoff, in the design's order ([`native_face`], [`no_terminal_exit`]).
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
    /// Pre-handoff decision rows select the native face.
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
    /// Pre-handoff exit status when no terminal is available ([`EXIT_USAGE`], [`NO_TERMINAL`]).
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
        !(stdin_tty && stdout_tty) && !wait && !no_wait
    }
}

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
    /// No window could be asked.
    Unreachable,
    /// The window took the request and did not answer in time.
    TimedOut,
}

/// The rows after the handoff.
#[inline]
#[must_use]
pub fn after_handoff(handoff: Handoff) -> Face {
    match handoff {
        Handoff::Opened => Face::Native,
        Handoff::Refused | Handoff::Unreachable => Face::Terminal,
        Handoff::TimedOut => Face::Exit(1, NO_ANSWER.to_string()),
    }
}

/// The person pressed done (C-c C-c).
pub const WAIT_FINISHED: u8 = 0;
/// The view closed.
pub const WAIT_CLOSED: u8 = 1;
/// The person pressed abort (C-c C-k).
pub const WAIT_ABORTED: u8 = 2;
/// The aterm serving the edit went away and no successor took the wait.
pub const WAIT_REPLACED: u8 = 3;
/// The wait failed any other way.
pub const WAIT_FAILED: u8 = 4;

trust_contract_fn! {
    /// The exit status a wait that ended as `end` ([`aterm_ctl::EXIT_REPLACED`]).
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

verify_island! {
    clean {
        def wait_status_table (e : UInt8) : UInt8 :=
          if e = 0 then 0 else if e = 1 then 0 else if e = 3 then 75 else 1

        theorem wait_status_is_the_table (e : UInt8) :
            trust_import_amacs_term__decide__wait_status e = wait_status_table e := by
          simp only [trust_import_amacs_term__decide__wait_status, wait_status_table]
    }
}
