// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAUNCH WITNESS: where the person's gesture count stood when the shell signed that a command started — `status launch=<why> launch_seq=<n|->` and `text --json`'s `"launch"`.

use std::sync::{Mutex, PoisonError};

use aterm_core::terminal::{ClockReading, Terminal};

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
    pub(crate) const fn word(self) -> &'static str {
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
    /// The engine's started-command count at the mark: one per command line, so a supervisor uses a witness once per id.
    pub(crate) id: u64,
    /// The person count loaded in the mark's hold.
    pub(crate) seq: u64,
    /// The command's first word, when it is one of [`AGENTS`].
    pub(crate) word: Option<&'static str>,
    /// How the mark was judged; `since` is a read's ([`LaunchWitness::read`]).
    pub(crate) why: Why,
}

/// A mark whose screen is still to be read, handed from the hold to [`LaunchWitness::read_screen`] after it.
pub(crate) struct Unread {
    id: u64,
    word: &'static str,
    rows: Vec<String>,
}

/// THE LAST COMMAND'S START, whatever its word ([`LaunchWitness::started`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Started {
    /// The person count loaded in the mark's hold ([`Launch::seq`]).
    pub(crate) seq: u64,
    /// The wall clock the hold took the mark at, unix ms.
    pub(crate) at_ms: u64,
}

/// The last mark and the wall clock its hold took it at, unix ms.
#[derive(Clone, Copy, Debug)]
struct Mark {
    launch: Launch,
    at_ms: u64,
}

/// One session's last mark.
#[derive(Debug, Default)]
pub(crate) struct LaunchWitness {
    mark: Mutex<Option<Mark>>,
}

impl LaunchWitness {
    /// One slice of the program's output through the engine at the reader's `clock`, as the PTY reader's parse stage feeds it, under the terminal lock the caller holds (`t`): the started-command count, the slice, then the mark ([`Self::at_hold`]) in that same hold.
    pub(crate) fn process(
        &self,
        t: &mut Terminal,
        bytes: &[u8],
        clock: ClockReading,
        person: &HumanInputStamp,
    ) -> Option<Unread> {
        let before = t.started_command_seq();
        t.process_at(bytes, clock);
        self.at_hold(t, before, person)
    }

    /// THE MARK, under the terminal lock right after a slice is processed,
    /// `started_before` the engine's started-command count read in the same
    /// hold before the slice.
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
        *self.lock() = Some(Mark {
            launch: Launch {
                id,
                seq: probe.seq,
                word,
                why,
            },
            at_ms: now_unix_ms(),
        });
        unread
    }

    /// The mark's screen, read with the terminal lock released: a box the reader of its word finds there (by any cursor: an earlier run's box does not hold it) makes it `boxed`, else `person`.
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
            .map(|m| &mut m.launch)
            .filter(|m| m.id == id && m.why == Why::Pending)
        {
            mark.why = if boxed { Why::Boxed } else { Why::Person };
        }
    }

    /// The last mark against a read whose person count is `human_seq`: a `person` mark the count has moved past reads `since`.
    pub(crate) fn read(&self, human_seq: u64) -> Option<Launch> {
        let mut launch = (*self.lock())?.launch;
        if launch.why == Why::Person && launch.seq != human_seq {
            launch.why = Why::Since;
        }
        Some(launch)
    }

    /// THE LAST COMMAND'S START, of any word: the person count its hold loaded and the wall clock
    /// it took it at.
    pub(crate) fn started(&self) -> Option<Started> {
        let mark = (*self.lock())?;
        Some(Started {
            seq: mark.launch.seq,
            at_ms: mark.at_ms,
        })
    }

    /// `status`'s `launch=<why|none> launch_seq=<n|->`.
    pub(crate) fn status_fields(&self, human_seq: u64) -> String {
        match self.read(human_seq) {
            Some(l) => format!("launch={} launch_seq={}", l.why.word(), l.seq),
            None => "launch=none launch_seq=-".to_string(),
        }
    }

    /// `text --json`'s `"launch"` value: the whole witness for `person`, the word alone otherwise, `null` for no mark.
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
        self.mark_at_for_test(launch, 0);
    }

    /// [`Self::mark_for_test`] taken at `at_ms` (unix ms).
    #[cfg(test)]
    pub(crate) fn mark_at_for_test(&self, launch: Launch, at_ms: u64) {
        *self.lock() = Some(Mark { launch, at_ms });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Mark>> {
        self.mark.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The wall clock, unix ms: a mark's instant ([`Started::at_ms`]), which is
/// compared with the kernel's start of a process (unix seconds), so it is
/// the wall clock, not this process's monotonic one.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// The program a command line starts, when it is one of [`AGENTS`]: the
/// basename of its first word exactly, so a wrapper (`exec claude`, `env …`)
/// or an assignment names none.
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
    /// marks carry.
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
            w.process(&mut t, bytes, ClockReading::now(), person)
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
        hold(&term, trust_dialog().as_bytes(), &person, &w);
        assert_eq!(w.read(7), Some(witnessed), "output after the mark");
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

    /// THE LAUNCH'S INSTANT is every mark's, whatever its word: a command no
    /// rule covers (`sh run-gemini.sh`, a controller's line) is started at
    /// the wall clock its hold read, with the person count loaded there; a
    /// newer command's start replaces both.
    #[test]
    fn every_command_start_notes_its_instant_and_count() {
        let (t, hex) = signed_prompt();
        let term = Mutex::new(t);
        let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
        assert_eq!(w.started(), None, "no mark yet");
        hold(&term, b"sh run-gemini.sh", &person, &w);
        assert_eq!(w.started(), None, "no command start");
        typed_and_entered(&person, "x");
        let before = now_unix_ms();
        hold(&term, &preexec("sh run-gemini.sh", &hex), &person, &w);
        let after = now_unix_ms();
        let started = w.started().expect("a start");
        assert_eq!(started.seq, 2, "the count its hold loaded");
        assert!(
            (before..=after).contains(&started.at_ms),
            "{before} <= {} <= {after}",
            started.at_ms
        );
        assert_eq!(w.read(2).map(|l| l.why), Some(Why::Other));
        person.note(0, Gesture::Other);
        assert_eq!(w.started(), Some(started));
        hold(
            &term,
            format!("\x1b]133;D;0;id={hex}\x07\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07")
                .as_bytes(),
            &person,
            &w,
        );
        hold(&term, &preexec("gemini", &hex), &person, &w);
        let next = w.started().expect("the next start");
        assert_eq!(next.seq, 3);
        assert!(next.at_ms >= started.at_ms);
        let term = Mutex::new(Terminal::new(24, 80));
        let w = LaunchWitness::default();
        hold(
            &term,
            b"\x1b]133;A\x07$ \x1b]133;B\x07gemini\r\n\x1b]633;E;gemini\x07\x1b]133;C\x07",
            &person,
            &w,
        );
        assert_eq!(w.started(), None, "unsigned");
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
            .process(
                &mut t,
                &preexec("codex", &hex),
                ClockReading::now(),
                &person,
            )
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
