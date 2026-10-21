// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The words a job row prints: the `sys jobs` row (§11.10, `key=value`
//! fields in one order), the findings' rows (§11.9), and the person's
//! sentence (§11.7 "Shown as"). Agents read the row; the person's surfaces
//! never print φ, they say the same number as "silent 6m12s, about 26× its
//! usual 14 s gap".

use std::fmt::Write as _;

use super::{Class, Findings, JobKind, JobRow, Verdict, Wait, WaitName};

/// φ × 1000 as the row prints it: `0`, `0.43`, `8.27`, `15.2`, `130`.
#[must_use]
pub fn phi_text(phi_milli: u64) -> String {
    if phi_milli == 0 {
        return "0".to_owned();
    }
    let s = if phi_milli < 10_000 {
        let h = (phi_milli + 5) / 10;
        format!("{}.{:02}", h / 100, h % 100)
    } else if phi_milli < 100_000 {
        let t = (phi_milli + 50) / 100;
        format!("{}.{}", t / 10, t % 10)
    } else {
        return ((phi_milli + 500) / 1000).to_string();
    };
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_owned()
}

/// Tenths as `0.9`.
#[must_use]
pub fn deci_text(deci: u64) -> String {
    format!("{}.{}", deci / 10, deci % 10)
}

/// An age in the person's words: `41s`, `7m`, `2h5m`.
#[must_use]
pub fn age_words(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

fn exact_age(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

impl JobRow {
    /// The `sys jobs` row.
    #[must_use]
    pub fn wire(&self) -> String {
        let mut o = format!(
            "job id={} kind={} consumer={}",
            self.id,
            self.kind.word(),
            self.consumer.word()
        );
        if let Some(p) = self.promoted_ms {
            let _ = write!(o, " promoted=timeout:{p}");
        }
        if let Some(t) = &self.task {
            let _ = write!(o, " task={}", exe_word(t));
        }
        if self.kind == JobKind::AgentCall {
            let _ = write!(o, " pgid={}", self.pgid);
        }
        let _ = write!(
            o,
            " exe={} class={}",
            exe_word(&self.exe),
            self.class.word()
        );
        if let Some(w) = &self.wait {
            let _ = write!(o, " wait={}", w.name.word());
            wait_fields(&mut o, w);
        }
        if let Some(n) = &self.named {
            let _ = write!(o, " named={}", n.word.word());
            if let Some(h) = &n.holder {
                let _ = write!(o, " holder={}:{}", h.pid, exe_word(&h.exe));
                if let Some(t) = h.tab {
                    let _ = write!(o, ":tab{t}");
                }
                if let Some(c) = h.class {
                    let _ = write!(o, ":{}", c.word());
                }
            }
        }
        if let Some(s) = &self.stop {
            let _ = write!(
                o,
                " stop={} stopped_pid={}:{}",
                if s.parked { "parked" } else { "held" },
                s.pid,
                exe_word(&s.name)
            );
        }
        if let Some(i) = &self.input {
            let _ = write!(
                o,
                " input={} input_bytes={} input_wait_ms={}",
                i.word.word(),
                i.bytes,
                i.wait_ms
            );
        }
        if let Some(c) = self.cpu_mc {
            let _ = write!(o, " cpu_mc={c}");
        }
        if let Some(z) = &self.zombies {
            let _ = write!(
                o,
                " zombies={} zombie_parent={} zombies_per_min={}",
                z.count,
                z.parent,
                deci_text(z.per_min_deci)
            );
        }
        if self.class != Class::Running && self.verdict != Verdict::Exited {
            if let Some(q) = self.quiet_ms {
                let _ = write!(o, " quiet_ms={q}");
            }
            if let Some(p) = self.phi_milli {
                let _ = write!(o, " phi={}", phi_text(p));
            }
            if self.class == Class::Waiting
                && let (Some(mu), Some(from)) = (self.mu_ms, self.mu_from)
            {
                let _ = write!(o, " mu_ms={mu} mu_from={}", from.word());
            }
        }
        if let Some(e) = self.exit.filter(|_| self.verdict == Verdict::Exited) {
            let _ = write!(o, " exit={e}");
        }
        let _ = write!(o, " verdict={}", self.verdict.word());
        if self.no_progress {
            o.push_str(" no_progress=1");
        }
        o
    }

    /// The person's sentence for the verdict; empty for `fine`.
    #[must_use]
    pub fn said(&self) -> String {
        let age = self.quiet_ms.map(age_words).unwrap_or_default();
        match self.verdict {
            Verdict::Fine => String::new(),
            Verdict::WaitingYou => "waiting for you: password prompt".to_owned(),
            Verdict::Parked => "parked".to_owned(),
            Verdict::NeverReturns => "never returns by itself".to_owned(),
            Verdict::Exited => self
                .exit
                .map_or_else(|| "exited".to_owned(), |e| format!("exited {e}")),
            Verdict::Busy => format!("busy, no output {age}"),
            Verdict::Slow => "slow: the Mac is swapping".to_owned(),
            Verdict::Quiet | Verdict::Stuck => {
                let rank = self.verdict.word();
                if let Some(i) = self.input.filter(|_| self.class == Class::UnreadInput) {
                    let keys = if i.bytes == 1 { "key" } else { "keys" };
                    return format!("{rank}: {} {keys} unread {}", i.bytes, age_words(i.wait_ms));
                }
                if let Some(s) = &self.stop {
                    return format!(
                        "{rank}: pid {} ({}) stopped, {age}",
                        s.pid,
                        person_name(&s.name)
                    );
                }
                let on = match self.class {
                    Class::Polling => "waking with no progress".to_owned(),
                    Class::Zombie => "an unreaped child".to_owned(),
                    _ => self
                        .wait
                        .as_ref()
                        .map_or_else(|| "a wait".to_owned(), wait_words),
                };
                format!("{rank} on {on}, {age}")
            }
        }
    }

    /// The quiet age against μ, as the person reads φ: "silent 6m12s, about
    /// 26× its usual 14 s gap"; `None` without both.
    #[must_use]
    pub fn gap(&self) -> Option<String> {
        let (q, mu) = (self.quiet_ms?, self.mu_ms?);
        let times = (q + mu / 2) / mu.max(1);
        Some(format!(
            "silent {}, about {times}× its usual {} s gap",
            exact_age(q),
            (mu + 500) / 1000
        ))
    }
}

fn wait_fields(o: &mut String, w: &Wait) {
    match w.name {
        WaitName::Pipe | WaitName::PipeFull | WaitName::Serves => {
            if let Some(p) = w.peer {
                let _ = write!(o, " peer={p}");
            }
        }
        WaitName::Tcp => {
            if let Some((remote, state, queued)) = &w.tcp {
                if let Some(r) = remote {
                    let _ = write!(o, " remote={r}");
                }
                let _ = write!(o, " state={} queued={queued}", state.word());
            }
        }
        WaitName::Follow => {
            if let Some(fd) = w.fd {
                let _ = write!(o, " fd={fd}");
            }
        }
        WaitName::Listen => {
            if let Some(p) = w.port {
                let _ = write!(o, " port={p}");
            }
        }
        _ => {}
    }
}

/// The wait as the person reads it after "quiet on" / "stuck on".
fn wait_words(w: &Wait) -> String {
    match w.name {
        WaitName::Tcp => match &w.tcp {
            Some((Some(r), _, _)) => format!("tcp {r}"),
            _ => "tcp".to_owned(),
        },
        WaitName::Pipe => w
            .peer
            .map_or_else(|| "a pipe".to_owned(), |p| format!("a pipe from pid {p}")),
        WaitName::PipeFull => w.peer.map_or_else(
            || "a full pipe".to_owned(),
            |p| format!("a full pipe to pid {p}"),
        ),
        WaitName::Kernel => "an uninterruptible kernel wait".to_owned(),
        WaitName::EventLoop => "an event loop, nothing pending".to_owned(),
        WaitName::Unnamed => "a wait not named".to_owned(),
        n => n.word().to_owned(),
    }
}

/// The longest `exe=` word (§13.3).
pub const EXE_WORD_MAX: usize = 32;

/// Whether `s` may print as an `exe=` (or `task=`) word: it matches
/// `^[A-Za-z0-9._+-]{1,32}$` (design §13.3, the foreign-text rule). A process
/// name is text whoever built the binary chose, and an agent reads the row:
/// no control byte, escape sequence, bidi override, quote, space or `=`
/// reaches it.
#[must_use]
pub fn is_exe_word(s: &str) -> bool {
    (1..=EXE_WORD_MAX).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// A process name as the row prints it: itself when [`is_exe_word`], else
/// `-`.
#[must_use]
pub fn exe_word(s: &str) -> &str {
    if is_exe_word(s) { s } else { "-" }
}

/// A process name as the person's sentence shows it: control characters and
/// invisible format characters (bidi overrides and isolates, zero-width
/// joiners, the byte-order mark, tag characters) dropped, at most
/// [`EXE_WORD_MAX`] characters; `?` when nothing is left.
#[must_use]
pub fn person_name(s: &str) -> String {
    let kept: String = s
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .take(EXE_WORD_MAX)
        .collect();
    let kept = kept.trim();
    if kept.is_empty() {
        "?".to_owned()
    } else {
        kept.to_owned()
    }
}

/// Unicode format characters that change how text around them is shown.
const fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

impl Findings {
    /// One `finding` row per finding.
    #[must_use]
    pub fn wire_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for n in &self.stopped {
            out.push(format!(
                "finding kind=stopped pid={} exe={}",
                n.pid,
                exe_word(&n.name)
            ));
        }
        for z in &self.zombies {
            out.push(format!(
                "finding kind=zombies parent={} exe={} count={} per_min={}",
                z.ppid,
                exe_word(&z.name),
                z.count,
                deci_text(z.per_min_deci)
            ));
        }
        for n in &self.d_state {
            out.push(format!(
                "finding kind=d-state pid={} exe={} for_ms={}",
                n.pid,
                exe_word(&n.name),
                n.for_ms
            ));
        }
        for n in &self.busy_orphans {
            out.push(format!(
                "finding kind=busy-orphan pid={} exe={} for_ms={}",
                n.pid,
                exe_word(&n.name),
                n.for_ms
            ));
        }
        let mut t = format!("finding kind=process-table own={}", self.own_procs);
        if let Some(l) = self.process_limit {
            let _ = write!(t, " limit={l}");
        }
        out.push(t);
        out
    }
}
