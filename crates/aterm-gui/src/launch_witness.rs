// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAUNCH WITNESS: where the person's gesture count stood when the shell
//! signed that a command started — `status launch=<why> launch_seq=<n|->` and
//! `text --json`'s `"launch"`.
//!
//! **Why it exists.** A supervisor keeps its hands off a session for `[harness]
//! human_grace_s` after a person's gesture (`human_ms=`). `claude⏎` typed in a
//! new folder draws a folder-trust dialog whose last gesture is that Enter, so
//! the dialog stood the whole grace on `No, exit`. The exemption the audit of
//! 2026-10-16 removed (b99456fef1) dated the dialog by its verdict's
//! publication and the supervisor's first sight of it; neither says what
//! caused the dialog, and a key on a dialog already drawn passed as a launch.
//! This records an order of events instead.
//!
//! **The mark** ([`LaunchWitness::process`]). The PTY reader's parse stage
//! feeds each slice of output to the engine under the terminal lock. In the
//! hold whose slice made the engine count a started command
//! (`Terminal::started_command_seq`: an OSC 133/633 `C` the A→B→C gate
//! accepted, and only while the session requires the shell's nonce, so the
//! mark is the shell's, never a program's output), still under that lock, it
//! loads the person count with its plain-Enter marker
//! ([`HumanInputStamp::probe`]), reads the first word of the shell's own
//! command line (OSC 633;E) and copies the screen. After the lock the copy is
//! read by that program's screen reader ([`LaunchWitness::read_screen`]).
//!
//! **The proof** — why a read whose count equals the mark's means the person
//! has not keyed on anything the command drew:
//! 1. The shell writes the `C` before it forks the command (zsh's preexec), so
//!    every byte the command writes follows it in the stream: in the mark's
//!    slice or a later one.
//! 2. Every frame is drawn from the engine under the same lock (`term_lock_ui`
//!    locks the same mutex), and every control read is made under it. So no
//!    byte of the mark's slice, or after it, was on any screen before the
//!    mark's hold ended.
//! 3. The count `S` loaded in that hold holds every gesture counted before the
//!    load; one counted after makes the count exceed `S` (a release add, an
//!    acquire load).
//! 4. So a read whose count is still `S` follows no gesture made after the
//!    command could show anything: the person cannot have reacted to its
//!    dialog. A key fenced `if-human=S`, checked under the lock at its write
//!    (`crate::control_input::InputFence`), carries that to the write.
//! 5. The order says nothing of a box ALREADY on screen at the mark — an
//!    earlier run's, which the person may have keyed on. The copy does: a box
//!    there is `boxed`.
//!
//! Nothing in it rests on an age, a clock or a sweep interval. The rest is
//! what makes the launch the person's: the last gesture counted at the mark
//! is a plain Enter key press ([`crate::human_input::Gesture::PlainEnter`]:
//! the key that accepts a typed line), and the command's first word is
//! `claude` or `codex`. A gesture after that Enter but before the mark
//! (`claude⏎↓`) may reach the program as input to its first screen: `typed`.
//!
//! **The words** (`why`): `person` — the mark's last gesture a plain Enter,
//! its word an agent this rule covers, no box at the mark, no gesture since;
//! `since` — `person` at the mark, a gesture counted since (judged at each
//! read, against the read's own count); `typed` — the mark's last gesture was
//! not a plain Enter, or there was none (a controller's line); `boxed` — a box
//! was on screen at the mark; `other` — the command's first word is not one
//! this rule covers (`exec claude`, a wrapper, an assignment, a line with no
//! 633;E, or an alias by another name in zsh and fish, which report the line
//! as typed; bash reports the command an alias expands to, so a bash alias
//! for `claude` reads `claude`); `pending` — the mark taken and its screen
//! not yet read (the instant after the hold, or a reader that panicked on
//! it); `none` (`null` in `text --json`) — no mark since this process began
//! serving the session.
//!
//! **Residual, said plainly.** Keys typed after the launching Enter and
//! counted before the mark's hold (`claude⏎⏎`) are inside `S`: they cannot be
//! reactions to the dialog (it was not yet drawable), their effect shows in
//! the read the supervisor judges, and it requires `input=clear`; a design
//! that closes them would sample the tty's queue at the foreground edge. A
//! person's own key written after the supervisor's fenced arrow lands on the
//! moved focus, as when the grace ends. The stamp has no per-write source,
//! so a controller's `claude⏎` written after the person's last Enter reads
//! as their launch; no gesture since is still no reaction to its dialog, and
//! a driver holding the session (`hand=`) holds it. A shell without the signed
//! integration, tmux or ssh, `exec` or a wrapper (and in zsh and fish an alias
//! by another name) give no `person`, so the grace holds there. The count
//! restarts in a new process, so a mark is never carried across a seamless
//! update.
//!
//! **Where it applies.** The mark is recorded wherever a signed shell
//! integration runs, on any host: zsh, bash, fish and PowerShell (its
//! PSReadLine shim signs 633;E and 133;C), WSL's bash included (the Windows
//! read loop feeds this same parse stage). The harness answers under it only on a
//! `status input=clear`, which needs the tty's input queue measured — today
//! macOS alone; elsewhere `input=-`, and the grace holds.

use std::sync::{Mutex, PoisonError};

use aterm_core::terminal::Terminal;

use crate::human_input::HumanInputStamp;

/// The command words a mark can witness: the programs whose folder-trust
/// dialog the harness answers.
const AGENTS: [&str; 2] = ["claude", "codex"];

/// What a mark says ([`LaunchWitness::read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    Person,
    Since,
    Typed,
    Boxed,
    Other,
    Pending,
}

impl Why {
    /// The wire word.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Since => "since",
            Self::Typed => "typed",
            Self::Boxed => "boxed",
            Self::Other => "other",
            Self::Pending => "pending",
        }
    }
}

/// The last mark, as a read publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Launch {
    /// The engine's started-command count at the mark: one per command line,
    /// so a supervisor uses a witness once per id.
    pub(crate) id: u64,
    /// The person count loaded in the mark's hold.
    pub(crate) seq: u64,
    /// The command's first word, when it is one of [`AGENTS`].
    pub(crate) word: Option<&'static str>,
    /// How the mark was judged; `since` is a read's ([`LaunchWitness::read`]).
    pub(crate) why: Why,
}

/// A mark whose screen is still to be read, handed from the hold to
/// [`LaunchWitness::read_screen`] after it.
pub(crate) struct Unread {
    id: u64,
    word: &'static str,
    rows: Vec<String>,
}

/// One session's last mark. Lives on [`crate::SessionCtx`]; its lock is a leaf,
/// taken after the terminal lock and never before it.
#[derive(Debug, Default)]
pub(crate) struct LaunchWitness {
    mark: Mutex<Option<Launch>>,
}

impl LaunchWitness {
    /// One slice of the program's output through the engine, as the PTY
    /// reader's parse stage feeds it, under the terminal lock the caller
    /// holds (`t`): the started-command count, the slice, then the mark
    /// ([`Self::at_hold`]) in that same hold. The screen copy it returns is
    /// read after the lock ([`Self::read_screen`]).
    pub(crate) fn process(
        &self,
        t: &mut Terminal,
        bytes: &[u8],
        person: &HumanInputStamp,
    ) -> Option<Unread> {
        let before = t.started_command_seq();
        t.process(bytes);
        self.at_hold(t, before, person)
    }

    /// THE MARK, under the terminal lock right after a slice is processed,
    /// `started_before` the engine's started-command count read in the same
    /// hold before the slice. Records a mark when the slice started a command
    /// on a session that requires the shell's nonce, and returns its screen
    /// copy when only that reading is left to judge.
    fn at_hold(
        &self,
        t: &Terminal,
        started_before: u64,
        person: &HumanInputStamp,
    ) -> Option<Unread> {
        let id = t.started_command_seq();
        if id == started_before || !t.is_require_shell_integration_nonce() {
            return None;
        }
        let probe = person.probe();
        let word = t
            .current_block()
            .and_then(|b| b.commandline.as_deref())
            .and_then(agent_word);
        let why = match word {
            None => Why::Other,
            Some(_) if !probe.plain_enter => Why::Typed,
            Some(_) => Why::Pending,
        };
        let unread = word.filter(|_| why == Why::Pending).map(|word| Unread {
            id,
            word,
            rows: (0..usize::from(t.rows()))
                .map(|r| crate::control::visible_row(t, r))
                .collect(),
        });
        *self.lock() = Some(Launch {
            id,
            seq: probe.seq,
            word,
            why,
        });
        unread
    }

    /// The mark's screen, read with the terminal lock released: a box the
    /// reader of its word finds there (by any cursor: an earlier run's box
    /// does not hold it) makes it `boxed`, else `person`. A reader that
    /// panics leaves it `pending`. A newer mark is never touched.
    pub(crate) fn read_screen(&self, unread: Unread) {
        let Unread { id, word, rows } = unread;
        let read = || {
            let reading = aterm_phase::read(Some(word), &rows, None);
            reading.phase == aterm_phase::Phase::Prompt || reading.prompt.is_some()
        };
        let Some(boxed) =
            crate::reader_guard::read_or_none("launch", "launch witness", &rows, read)
        else {
            return;
        };
        if let Some(mark) = self
            .lock()
            .as_mut()
            .filter(|m| m.id == id && m.why == Why::Pending)
        {
            mark.why = if boxed { Why::Boxed } else { Why::Person };
        }
    }

    /// The last mark against a read whose person count is `human_seq`: a
    /// `person` mark the count has moved past reads `since`. `None`: no mark.
    pub(crate) fn read(&self, human_seq: u64) -> Option<Launch> {
        let mut launch = (*self.lock())?;
        if launch.why == Why::Person && launch.seq != human_seq {
            launch.why = Why::Since;
        }
        Some(launch)
    }

    /// `status`'s `launch=<why|none> launch_seq=<n|->`.
    pub(crate) fn status_fields(&self, human_seq: u64) -> String {
        match self.read(human_seq) {
            Some(l) => format!("launch={} launch_seq={}", l.why.word(), l.seq),
            None => "launch=none launch_seq=-".to_string(),
        }
    }

    /// `text --json`'s `"launch"` value: the whole witness for `person`,
    /// the word alone otherwise, `null` for no mark.
    pub(crate) fn json(&self, human_seq: u64) -> String {
        match self.read(human_seq) {
            Some(Launch {
                id,
                seq,
                word: Some(word),
                why: Why::Person,
            }) => format!("{{\"id\":{id},\"seq\":{seq},\"word\":\"{word}\",\"why\":\"person\"}}"),
            Some(l) => format!("{{\"why\":\"{}\"}}", l.why.word()),
            None => "null".to_string(),
        }
    }

    /// A mark as the parse stage would leave it, for a test of a reader of it.
    #[cfg(test)]
    pub(crate) fn mark_for_test(&self, launch: Launch) {
        *self.lock() = Some(launch);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Launch>> {
        self.mark.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The program a command line starts, when it is one of [`AGENTS`]: the
/// basename of its first word exactly, so a wrapper (`exec claude`, `env …`)
/// or an assignment names none. The line is the shell's 633;E: zsh and fish
/// report it as typed, so an alias by another name names none there; bash
/// reports `$BASH_COMMAND`, the command an alias expands to, so a bash alias
/// for `claude` names `claude`.
fn agent_word(line: &str) -> Option<&'static str> {
    let first = line.split_whitespace().next()?;
    let name = first.rsplit('/').next()?;
    AGENTS.into_iter().find(|agent| *agent == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::human_input::Gesture;

    /// A terminal whose host required and authorized the shell's nonce (every
    /// aterm-spawned integrated shell), at a signed prompt, and the hex its
    /// marks carry. Wide enough for a dialog's rows.
    fn signed_prompt() -> (Terminal, String) {
        let nonce = [0x5Au8; 32];
        let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let mut t = Terminal::new(50, 140);
        t.authorize_shell_integration(nonce);
        t.set_require_shell_integration_nonce(true);
        t.process(format!("\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07").as_bytes());
        (t, hex)
    }

    /// What zsh's preexec writes for `line` once the person's Enter is read
    /// (`aterm_shell_integration.zsh`): 633;E, then 133;C, each signed.
    fn preexec(line: &str, hex: &str) -> Vec<u8> {
        format!("\x1b]633;E;{line};id={hex}\x07\x1b]133;C;id={hex}\x07").into_bytes()
    }

    /// Claude Code's folder-trust dialog as it drew it (aterm-phase's capture).
    fn trust_dialog() -> String {
        use aterm_phase::prompt::fixtures as f;
        format!("{}\r\n", f::screen(f::TRUST).join("\r\n"))
    }

    fn enter() -> Gesture {
        Gesture::PlainEnter
    }

    /// One parse-stage hold over `bytes`, as the reader takes it: the slice
    /// and its mark under the lock, then the screen read with it released.
    fn hold(term: &Mutex<Terminal>, bytes: &[u8], person: &HumanInputStamp, w: &LaunchWitness) {
        let unread = {
            let mut t = term.lock().expect("term");
            w.process(&mut t, bytes, person)
        };
        if let Some(unread) = unread {
            w.read_screen(unread);
        }
    }

    /// A person types `line` (a gesture a character) and presses Enter.
    fn typed_and_entered(person: &HumanInputStamp, line: &str) {
        for _ in line.chars() {
            person.note(0, Gesture::Other);
        }
        person.note(0, enter());
    }

    /// THE POSITIVE PATH at the parse stage: `claude⏎` typed and its signed
    /// command start parsed records the count of that Enter as `person`, with
    /// its id and word, on a screen showing no box — and the dialog Claude
    /// Code then draws does not change it, while a gesture counted after the
    /// mark's hold makes every later read say `since`.
    #[test]
    fn a_signed_command_start_records_the_count_at_its_hold() {
        let (t, hex) = signed_prompt();
        let term = Mutex::new(t);
        let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
        assert_eq!(w.read(0), None, "no mark yet");
        assert_eq!(w.json(0), "null");
        assert_eq!(w.status_fields(0), "launch=none launch_seq=-");
        typed_and_entered(&person, "claude");
        let mut slice = b"claude\r\n".to_vec();
        slice.extend(preexec("claude", &hex));
        hold(&term, &slice, &person, &w);
        let witnessed = Launch {
            id: 1,
            seq: 7,
            word: Some("claude"),
            why: Why::Person,
        };
        assert_eq!(w.read(7), Some(witnessed));
        assert_eq!(
            w.json(7),
            "{\"id\":1,\"seq\":7,\"word\":\"claude\",\"why\":\"person\"}"
        );
        assert_eq!(w.status_fields(7), "launch=person launch_seq=7");
        // The dialog the program draws after the mark is no box AT the mark.
        hold(&term, trust_dialog().as_bytes(), &person, &w);
        assert_eq!(w.read(7), Some(witnessed), "output after the mark");
        // A gesture after the hold: every read with its count says `since`.
        person.note(0, Gesture::Other);
        assert_eq!(w.read(8).map(|l| l.why), Some(Why::Since));
        assert_eq!(w.json(8), "{\"why\":\"since\"}");
        assert_eq!(w.status_fields(8), "launch=since launch_seq=7");
    }

    /// Each premise's negative control: a box on screen at the mark is
    /// `boxed`; a first word no rule covers (a wrapper, a path to another
    /// program, an assignment) is `other`; a last gesture that was not a plain
    /// Enter (a letter after it, or none at all: a controller's line) is
    /// `typed`; an unsigned session and a slice with no command start record
    /// nothing; a newer mark replaces an older one.
    #[test]
    fn every_premise_missing_is_a_word_other_than_person() {
        // The screen `before` the mark (by default the line's echo), then its
        // signed command start, after `gestures`.
        let at_mark = |before: &str, line: &str, gestures: &[Gesture]| {
            let (mut t, hex) = signed_prompt();
            t.process(before.as_bytes());
            let term = Mutex::new(t);
            let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
            for g in gestures {
                person.note(0, *g);
            }
            hold(&term, &preexec(line, &hex), &person, &w);
            w.read(person.seq()).map(|l| l.why)
        };
        let enter_last = [Gesture::Other, enter()];
        let echo = |line: &str| format!("{line}\r\n");
        assert_eq!(
            at_mark(&echo("claude"), "claude", &enter_last),
            Some(Why::Person)
        );
        let codex = "/opt/bin/codex --yolo";
        assert_eq!(at_mark(&echo(codex), codex, &enter_last), Some(Why::Person));
        assert_eq!(
            at_mark(
                &format!("claude\r\n{}", trust_dialog()),
                "claude",
                &enter_last
            ),
            Some(Why::Boxed),
            "a dialog on screen at the mark"
        );
        for line in [
            "exec claude",
            "FOO=1 claude",
            "env claude",
            "claudette",
            "/bin/ls",
        ] {
            assert_eq!(
                at_mark(&echo(line), line, &enter_last),
                Some(Why::Other),
                "{line}"
            );
        }
        assert_eq!(
            at_mark(&echo("claude"), "claude", &[enter(), Gesture::Other]),
            Some(Why::Typed),
            "a key after the Enter"
        );
        assert_eq!(
            at_mark(&echo("claude"), "claude", &[]),
            Some(Why::Typed),
            "no gesture: a controller's line"
        );
        // An unsigned session's marks are any program's to forge: none taken.
        let term = Mutex::new(Terminal::new(24, 80));
        let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
        person.note(0, enter());
        hold(
            &term,
            b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]633;E;claude\x07\x1b]133;C\x07",
            &person,
            &w,
        );
        assert_eq!(w.read(1), None, "unsigned");
        // A slice that starts no command leaves the mark as it was.
        let (t, hex) = signed_prompt();
        let term = Mutex::new(t);
        hold(&term, b"claude", &person, &w);
        assert_eq!(w.read(1), None, "no command start");
        hold(&term, &preexec("claude", &hex), &person, &w);
        assert_eq!(w.read(1).map(|l| (l.id, l.why)), Some((1, Why::Person)));
        hold(&term, b"\x1b[2J working", &person, &w);
        assert_eq!(
            w.read(1).map(|l| l.id),
            Some(1),
            "output is no command start"
        );
        // The next command line's mark replaces it.
        person.note(0, enter());
        hold(
            &term,
            format!("\x1b]133;D;0;id={hex}\x07\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07")
                .as_bytes(),
            &person,
            &w,
        );
        hold(&term, &preexec("ls", &hex), &person, &w);
        assert_eq!(
            w.read(2).map(|l| (l.id, l.seq, l.why)),
            Some((2, 2, Why::Other))
        );
    }

    /// The mark is taken IN the hold that parsed the command start, never
    /// after it: a gesture counted between that hold and the screen's
    /// reading after the lock is outside the mark (`since`), and a mark whose
    /// reading is still owed reads `pending`, never `person`.
    #[test]
    fn the_count_is_the_holds_and_an_unread_mark_is_pending() {
        let (mut t, hex) = signed_prompt();
        let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
        person.note(0, enter());
        let unread = w
            .process(&mut t, &preexec("codex", &hex), &person)
            .expect("a screen to read");
        assert_eq!(w.read(1).map(|l| l.why), Some(Why::Pending));
        assert_eq!(w.json(1), "{\"why\":\"pending\"}");
        person.note(0, Gesture::Other);
        w.read_screen(unread);
        assert_eq!(
            w.read(2),
            Some(Launch {
                id: 1,
                seq: 1,
                word: Some("codex"),
                why: Why::Since
            })
        );
    }
}
