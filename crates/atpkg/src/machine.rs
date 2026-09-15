// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Machine settings the update/seed pass applies per the doctor's own findings (R5,
//! owner decisions 2026-09-10): the POLICY half — what to do given what `defaults`
//! answered — kept pure and behind a port so no test ever touches the real
//! `com.apple.universalcontrol` domain. The Spotlight half is [`crate::noindex::apply`].
//!
//! Universal Control is macOS's cursor/keyboard roaming between Macs and iPads signed
//! into the same Apple account. The owner runs with it disabled
//! (`docs/SESSION-watchdog-noindex-2026-09-02.md`: `Disable = 1`), and a fresh machine
//! sits at the OS default with neither key set. The switch is per-host and per-user
//! (`defaults -currentHost`), no sudo — so a first-open pass can apply it, and one
//! pasted line reverts it: [`UNIVERSAL_CONTROL_REVERT`].

use crate::config::UniversalControlPolicy;

/// The one-line revert every surface that mentions the change must print (pull-down
/// row, doctor, this pass's log). The pass writes BOTH per-host keys
/// (`platform::universal_control_disable`), so the revert deletes both: `Disable` is
/// the feature, `DisableMagicEdges` the screen-edge hand-off, and deleting one leaves
/// the other set. Joined by `;`, not `&&`: `defaults delete` exits 1 on a key that is
/// already absent, and the second delete must run regardless.
pub const UNIVERSAL_CONTROL_REVERT: &str = "defaults -currentHost delete \
    com.apple.universalcontrol Disable; defaults -currentHost delete \
    com.apple.universalcontrol DisableMagicEdges";

/// The `machine-settings:` entry for a Universal Control change (contract string).
pub const UNIVERSAL_CONTROL_ENTRY: &str = "universal-control disabled";

/// Universal Control's two per-host switches, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UniversalControlState {
    /// `Disable`: `Some(true)` = the feature is off.
    pub disable: Option<bool>,
    /// `DisableMagicEdges`: `Some(true)` = no screen-edge hand-off.
    pub magic_edges: Option<bool>,
}

impl UniversalControlState {
    /// Fully disabled — BOTH keys true. One key alone is a half state the pass
    /// completes rather than reports as done.
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.disable == Some(true) && self.magic_edges == Some(true)
    }

    /// The doctor's words for this state (macOS only; the caller decides whether to
    /// print). `ok` when disabled — with the revert, so the line is never a dead end —
    /// `warn` when the pass is about to change it, `ok` when the policy says leave.
    #[must_use]
    pub fn doctor_line(&self, policy: UniversalControlPolicy) -> String {
        if self.disabled() {
            return format!(
                "ok — Universal Control is disabled on this host (cursor and keyboard stay \
                 on this Mac); revert: {UNIVERSAL_CONTROL_REVERT}"
            );
        }
        match policy {
            UniversalControlPolicy::Off => format!(
                "warn — Universal Control is at the OS default (the cursor roams to other \
                 Macs and iPads on this Apple account){}; every pass disables it first thing \
                 ([machine] universal_control = \"off\") — now: `aterm pkg machine apply`; \
                 keep it: universal_control = \"leave\"",
                self.partial_note()
            ),
            UniversalControlPolicy::Leave => format!(
                "ok — Universal Control left at the OS default{} ([machine] \
                 universal_control = \"leave\")",
                self.partial_note()
            ),
        }
    }

    /// `, Disable set but not DisableMagicEdges` style note for a half state.
    fn partial_note(&self) -> &'static str {
        match (self.disable, self.magic_edges) {
            (Some(true), _) => " (Disable is set, DisableMagicEdges is not)",
            (_, Some(true)) => " (DisableMagicEdges is set, Disable is not)",
            _ => "",
        }
    }
}

/// `defaults read`'s rendering of a boolean, as bytes: `1`/`0`, `true`/`false`,
/// `YES`/`NO` (any case), surrounded by whitespace. Anything else — a dict, a string —
/// is `None`: not a switch this module understands, so never "set". Byte-based so
/// [`crate::platform::unix`] (compiled under the strict gate, no `fmt::Arguments`) can
/// call it.
#[must_use]
pub fn parse_defaults_bool(stdout: &[u8]) -> Option<bool> {
    let trimmed: Vec<u8> = stdout
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if trimmed.is_empty() || trimmed.len() > 5 {
        return None;
    }
    let lower: Vec<u8> = trimmed.iter().map(u8::to_ascii_lowercase).collect();
    match lower.as_slice() {
        b"1" | b"true" | b"yes" => Some(true),
        b"0" | b"false" | b"no" => Some(false),
        _ => None,
    }
}

/// The port to the real `defaults` domain — one read, one write — so the policy is
/// testable against a fake and the tests never touch the machine.
pub trait UniversalControlPort {
    /// `[Disable, DisableMagicEdges]` as [`crate::platform::universal_control_state`].
    fn state(&self) -> [Option<bool>; 2];
    /// Write both keys true; `true` iff both writes succeeded.
    fn disable(&self) -> bool;
}

/// The real port: `/usr/bin/defaults -currentHost`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDefaults;

impl UniversalControlPort for SystemDefaults {
    fn state(&self) -> [Option<bool>; 2] {
        crate::platform::universal_control_state()
    }
    fn disable(&self) -> bool {
        crate::platform::universal_control_disable()
    }
}

/// Read the state through `port`.
#[must_use]
pub fn universal_control_state(port: &dyn UniversalControlPort) -> UniversalControlState {
    let [disable, magic_edges] = port.state();
    UniversalControlState {
        disable,
        magic_edges,
    }
}

/// What one pass did about Universal Control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UniversalControlOutcome {
    /// Not macOS, or the policy is `leave`: nothing read, nothing written.
    NotApplicable,
    /// Already disabled (both keys): nothing written.
    AlreadyDisabled,
    /// Both keys were written true this pass — the `machine-settings:` entry.
    Disabled,
    /// The write did not succeed; nothing to report as changed.
    WriteFailed,
}

/// Apply `policy` through `port`: `Disabled` ONLY when this pass changed something,
/// which is what the `machine-settings:` marker reports. Idempotent — a disabled host
/// reads `AlreadyDisabled` on every later pass and the marker stays silent.
#[must_use]
pub fn apply_universal_control(
    policy: UniversalControlPolicy,
    port: &dyn UniversalControlPort,
) -> UniversalControlOutcome {
    if !cfg!(target_os = "macos") || policy == UniversalControlPolicy::Leave {
        return UniversalControlOutcome::NotApplicable;
    }
    if universal_control_state(port).disabled() {
        return UniversalControlOutcome::AlreadyDisabled;
    }
    if !port.disable() {
        return UniversalControlOutcome::WriteFailed;
    }
    // Read back: a write that did not land is not a change to announce.
    if universal_control_state(port).disabled() {
        UniversalControlOutcome::Disabled
    } else {
        UniversalControlOutcome::WriteFailed
    }
}

// ---------------------------------------------------------------------------------------
// The READ verb's state — one byte-stable line the window parses, and the one pure
// verdict both the CLI's `next —` line and the window's Apply button derive from.
// ---------------------------------------------------------------------------------------

/// Universal Control, as the two per-host keys read — the coarse posture a report
/// names, derived from [`UniversalControlState`] and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UcPosture {
    /// Both keys true: the pass's wanted state.
    Disabled,
    /// Neither key set: the OS default — the cursor roams.
    Default,
    /// Exactly one of the two set: a half state the pass completes rather than reports
    /// as done.
    Partial,
    /// A key read as something this module does not treat as a switch, or the read
    /// could not be made. Never "off".
    Unknown,
}

impl UcPosture {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            UcPosture::Disabled => "disabled",
            UcPosture::Default => "default",
            UcPosture::Partial => "partial",
            UcPosture::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "disabled" => UcPosture::Disabled,
            "default" => UcPosture::Default,
            "partial" => UcPosture::Partial,
            "unknown" => UcPosture::Unknown,
            _ => return None,
        })
    }
}

impl UniversalControlState {
    /// The coarse posture. `Unknown` only when NEITHER key could be read as a switch —
    /// `[None, None]` is the OS default (an absent key is the ordinary state of a
    /// machine that never turned the feature off), which is why the platform layer
    /// returns `None` for absent and this cannot tell absent from unreadable; the
    /// distinction the doctor makes is carried by `disable_read`'s prose, not here.
    #[must_use]
    pub fn posture(&self) -> UcPosture {
        match (self.disable, self.magic_edges) {
            (Some(true), Some(true)) => UcPosture::Disabled,
            (Some(true), _) | (_, Some(true)) => UcPosture::Partial,
            _ => UcPosture::Default,
        }
    }
}

/// Whether this process's home is the account's — the synthetic-home rule the apply
/// path enforces (`defaults` writes the account's per-host domain and ignores `$HOME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomePosture {
    /// `$HOME` is the account home: the settings apply here.
    Account,
    /// `$HOME` is somewhere else: a synthetic machine, nothing is applied.
    Mismatch,
    /// The account home could not be resolved, so `$HOME` cannot be proven to be it:
    /// nothing is applied.
    Unresolved,
}

impl HomePosture {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            HomePosture::Account => "account",
            HomePosture::Mismatch => "mismatch",
            HomePosture::Unresolved => "unresolved",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "account" => HomePosture::Account,
            "mismatch" => HomePosture::Mismatch,
            "unresolved" => HomePosture::Unresolved,
            _ => return None,
        })
    }
}

/// What bare `atpkg machine` measured — the whole of it, as one record. Printed as the
/// [`MACHINE_STATE_MARKER`](crate::cli::MACHINE_STATE_MARKER) line and parsed back by
/// the window through [`parse_machine_state`], so the two cannot disagree about a
/// field's spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineState {
    /// The two keys' posture.
    pub universal_control: UcPosture,
    /// `[machine] universal_control`, resolved.
    pub policy: UniversalControlPolicy,
    /// `[machine] spotlight_noindex`, resolved.
    pub spotlight_noindex: bool,
    /// Cargo target dirs under the home whose NAME leaves them open to Spotlight.
    pub exposed: usize,
    /// Cargo target dirs already hidden by name.
    pub hidden: usize,
    /// Of the exposed, how many a pass WOULD migrate (a dry-run apply: beside a
    /// `Cargo.toml`, no live build, nothing in the way). The rest need
    /// `aterm pkg noindex apply <dir>` by name, and a count that could never fall to
    /// zero is the trap the doctor records — so this is the number the verdict uses.
    pub would_migrate: usize,
    /// `false` when the scan hit its budget, so a report says "at least".
    pub scan_complete: bool,
    /// The synthetic-home rule's answer.
    pub home: HomePosture,
}

/// What is left for an apply to do, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineNext {
    /// Universal Control is not yet fully disabled and the policy says disable it.
    pub universal_control: bool,
    /// Target dirs a pass would rename now.
    pub spotlight: usize,
}

/// THE verdict: `None` when an apply would change nothing — the CLI then says so and
/// the window's Apply button is disabled — else what it would do. Pure over its inputs
/// so the two surfaces cannot drift, and so the half-state and the "counts but cannot
/// migrate" cases are pinned by a table rather than by prose.
///
/// The home posture is NOT an input: a mismatched home means nothing is applied at
/// all, and that is the caller's line to print before it asks what is left.
#[must_use]
pub fn machine_next(
    uc: UcPosture,
    policy: UniversalControlPolicy,
    spotlight_noindex: bool,
    would_migrate: usize,
) -> Option<MachineNext> {
    let universal_control =
        policy == UniversalControlPolicy::Off && !matches!(uc, UcPosture::Disabled);
    let spotlight = if spotlight_noindex { would_migrate } else { 0 };
    if !universal_control && spotlight == 0 {
        return None;
    }
    Some(MachineNext {
        universal_control,
        spotlight,
    })
}

impl MachineState {
    /// What an apply would still do on this machine.
    #[must_use]
    pub fn next(&self) -> Option<MachineNext> {
        if self.home != HomePosture::Account {
            return None;
        }
        machine_next(
            self.universal_control,
            self.policy,
            self.spotlight_noindex,
            self.would_migrate,
        )
    }
}

/// The marker body, `key=value` pairs joined by `; ` — the same shape as the
/// `machine-settings:` body, so one reader handles both.
#[must_use]
pub fn machine_state_line(s: &MachineState) -> String {
    format!(
        "universal-control={}; policy={}; noindex={}; spotlight-exposed={}; \
         spotlight-hidden={}; spotlight-migratable={}; scan={}; home={}",
        s.universal_control.as_str(),
        match s.policy {
            UniversalControlPolicy::Off => "off",
            UniversalControlPolicy::Leave => "leave",
        },
        s.spotlight_noindex,
        s.exposed,
        s.hidden,
        s.would_migrate,
        if s.scan_complete {
            "complete"
        } else {
            "partial"
        },
        s.home.as_str(),
    )
}

/// Parse a [`machine_state_line`] body. Every field required, order free, unknown
/// keys ignored (a newer atpkg may add one); a missing or malformed field is `None`,
/// never a default — a window that guessed a posture would be the false confidence the
/// pass had before 2026-09-14.
#[must_use]
pub fn parse_machine_state(body: &str) -> Option<MachineState> {
    let mut uc = None;
    let mut policy = None;
    let mut noindex = None;
    let mut exposed = None;
    let mut hidden = None;
    let mut migratable = None;
    let mut scan = None;
    let mut home = None;
    for pair in body.split(';') {
        let Some((k, v)) = pair.trim().split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "universal-control" => uc = UcPosture::parse(v),
            "policy" => {
                policy = match v {
                    "off" => Some(UniversalControlPolicy::Off),
                    "leave" => Some(UniversalControlPolicy::Leave),
                    _ => None,
                }
            }
            "noindex" => noindex = v.parse::<bool>().ok(),
            "spotlight-exposed" => exposed = v.parse::<usize>().ok(),
            "spotlight-hidden" => hidden = v.parse::<usize>().ok(),
            "spotlight-migratable" => migratable = v.parse::<usize>().ok(),
            "scan" => {
                scan = match v {
                    "complete" => Some(true),
                    "partial" => Some(false),
                    _ => None,
                }
            }
            "home" => home = HomePosture::parse(v),
            _ => {}
        }
    }
    Some(MachineState {
        universal_control: uc?,
        policy: policy?,
        spotlight_noindex: noindex?,
        exposed: exposed?,
        hidden: hidden?,
        would_migrate: migratable?,
        scan_complete: scan?,
        home: home?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A fake domain: the two keys, and a count of writes.
    struct Fake {
        disable: Cell<Option<bool>>,
        edges: Cell<Option<bool>>,
        writes: Cell<u32>,
        write_ok: bool,
    }

    impl Fake {
        fn at(disable: Option<bool>, edges: Option<bool>) -> Self {
            Fake {
                disable: Cell::new(disable),
                edges: Cell::new(edges),
                writes: Cell::new(0),
                write_ok: true,
            }
        }
    }

    impl UniversalControlPort for Fake {
        fn state(&self) -> [Option<bool>; 2] {
            [self.disable.get(), self.edges.get()]
        }
        fn disable(&self) -> bool {
            self.writes.set(self.writes.get() + 1);
            if self.write_ok {
                self.disable.set(Some(true));
                self.edges.set(Some(true));
            }
            self.write_ok
        }
    }

    #[test]
    fn defaults_bool_parses_the_renderings_defaults_prints_and_nothing_else() {
        assert_eq!(parse_defaults_bool(b"1\n"), Some(true));
        assert_eq!(parse_defaults_bool(b"0\n"), Some(false));
        assert_eq!(parse_defaults_bool(b"  true "), Some(true));
        assert_eq!(parse_defaults_bool(b"YES"), Some(true));
        assert_eq!(parse_defaults_bool(b"No\n"), Some(false));
        assert_eq!(parse_defaults_bool(b""), None);
        assert_eq!(parse_defaults_bool(b"\n"), None);
        assert_eq!(parse_defaults_bool(b"{\n    Configuration = x;\n}\n"), None);
        assert_eq!(parse_defaults_bool(b"2"), None);
        assert_eq!(parse_defaults_bool(b"truee"), None);
    }

    // The policy over the fake: the OS default is disabled ONCE (the pass's entry),
    // a disabled host is left alone on every later pass, a half state is completed,
    // `leave` never reads or writes, and a failed write is not announced.
    #[test]
    fn the_pass_disables_once_and_never_touches_a_disabled_host_or_a_leave_policy() {
        if !cfg!(target_os = "macos") {
            let fake = Fake::at(None, None);
            assert_eq!(
                apply_universal_control(UniversalControlPolicy::Off, &fake),
                UniversalControlOutcome::NotApplicable
            );
            assert_eq!(fake.writes.get(), 0);
            return;
        }
        let fake = Fake::at(None, None);
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &fake),
            UniversalControlOutcome::Disabled
        );
        assert_eq!(fake.writes.get(), 1);
        assert!(universal_control_state(&fake).disabled());
        // Idempotent: the next pass changes nothing and says nothing.
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &fake),
            UniversalControlOutcome::AlreadyDisabled
        );
        assert_eq!(fake.writes.get(), 1);
        // A half state (the owner's machine before DisableMagicEdges existed) completes.
        let half = Fake::at(Some(true), None);
        assert!(!universal_control_state(&half).disabled());
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &half),
            UniversalControlOutcome::Disabled
        );
        // `leave` is inert whatever the state.
        let leave = Fake::at(None, None);
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Leave, &leave),
            UniversalControlOutcome::NotApplicable
        );
        assert_eq!(leave.writes.get(), 0);
        // A write that fails is not a change.
        let mut broken = Fake::at(Some(false), Some(false));
        broken.write_ok = false;
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &broken),
            UniversalControlOutcome::WriteFailed
        );
        assert_eq!(broken.writes.get(), 1);
    }

    #[test]
    fn doctor_words_carry_the_revert_and_the_opt_out() {
        let off = UniversalControlState {
            disable: Some(true),
            magic_edges: Some(true),
        };
        let line = off.doctor_line(UniversalControlPolicy::Off);
        assert!(line.starts_with("ok — "), "{line}");
        assert!(line.contains(UNIVERSAL_CONTROL_REVERT), "{line}");
        assert_eq!(
            UNIVERSAL_CONTROL_REVERT,
            "defaults -currentHost delete com.apple.universalcontrol Disable; \
             defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
        );
        let default = UniversalControlState::default();
        let line = default.doctor_line(UniversalControlPolicy::Off);
        assert!(line.starts_with("warn — "), "{line}");
        // The promise, and the door: "first thing" is the 2026-09-14 fix (the apply used
        // to sit inside `if failures == 0` at the END of the pass, so a machine whose
        // toolchain pass never came clean was promised this on every run and never got
        // it), and the verb is how a person gets it NOW rather than at the next pass.
        assert!(
            line.contains("every pass disables it first thing"),
            "{line}"
        );
        assert!(line.contains("now: `aterm pkg machine apply`"), "{line}");
        assert!(
            !line.contains("next update pass"),
            "the old promise must not survive: {line}"
        );
        assert!(line.contains("universal_control = \"leave\""), "{line}");
        let line = default.doctor_line(UniversalControlPolicy::Leave);
        assert!(line.starts_with("ok — "), "{line}");
        assert!(line.contains("left at the OS default"), "{line}");
        let half = UniversalControlState {
            disable: Some(true),
            magic_edges: None,
        };
        assert!(
            half.doctor_line(UniversalControlPolicy::Off)
                .contains("Disable is set, DisableMagicEdges is not")
        );
        assert_eq!(UNIVERSAL_CONTROL_ENTRY, "universal-control disabled");
    }

    /// Every posture round-trips through the wire line, so the window and the CLI cannot
    /// disagree about a spelling; an unknown key is ignored (a newer atpkg), a missing
    /// one is a refusal, never a guess.
    #[test]
    fn machine_state_line_round_trips() {
        for uc in [
            UcPosture::Disabled,
            UcPosture::Default,
            UcPosture::Partial,
            UcPosture::Unknown,
        ] {
            for policy in [UniversalControlPolicy::Off, UniversalControlPolicy::Leave] {
                for home in [
                    HomePosture::Account,
                    HomePosture::Mismatch,
                    HomePosture::Unresolved,
                ] {
                    let s = MachineState {
                        universal_control: uc,
                        policy,
                        spotlight_noindex: policy == UniversalControlPolicy::Off,
                        exposed: 4,
                        hidden: 2,
                        would_migrate: 3,
                        scan_complete: home != HomePosture::Mismatch,
                        home,
                    };
                    let line = machine_state_line(&s);
                    assert_eq!(parse_machine_state(&line), Some(s.clone()), "{line}");
                }
            }
        }
        let full = "universal-control=disabled; policy=off; noindex=true; spotlight-exposed=1; \
                    spotlight-hidden=9; spotlight-migratable=0; scan=complete; home=account";
        assert!(parse_machine_state(full).is_some());
        assert!(parse_machine_state(&format!("{full}; future-key=whatever")).is_some());
        assert!(parse_machine_state("universal-control=disabled; policy=off").is_none());
        assert!(parse_machine_state(&full.replace("home=account", "home=elsewhere")).is_none());
        assert!(parse_machine_state("").is_none());
    }

    /// The posture derivation: both keys = disabled, one key = partial, neither = the
    /// OS default (an absent key is the ordinary state, not an unknown one).
    #[test]
    fn universal_control_posture_reads_both_keys() {
        let at = |d, e| UniversalControlState {
            disable: d,
            magic_edges: e,
        };
        assert_eq!(at(Some(true), Some(true)).posture(), UcPosture::Disabled);
        assert_eq!(at(Some(true), None).posture(), UcPosture::Partial);
        assert_eq!(at(None, Some(true)).posture(), UcPosture::Partial);
        assert_eq!(at(Some(false), Some(true)).posture(), UcPosture::Partial);
        assert_eq!(at(None, None).posture(), UcPosture::Default);
        assert_eq!(at(Some(false), Some(false)).posture(), UcPosture::Default);
    }

    /// THE VERDICT TABLE — the read verb's `next —` line and the window's Apply button
    /// both read this. The live case that motivated it: Universal Control already off,
    /// one target dir a pass would rename — the old read said "nothing to apply".
    #[test]
    fn machine_read_hint_covers_spotlight_as_well_as_universal_control() {
        use UcPosture::*;
        use UniversalControlPolicy::*;
        assert_eq!(
            machine_next(Disabled, Off, true, 1),
            Some(MachineNext {
                universal_control: false,
                spotlight: 1
            }),
            "the live case: UC done, one dir to hide"
        );
        assert_eq!(
            machine_next(Disabled, Off, true, 0),
            None,
            "everything done"
        );
        assert_eq!(
            machine_next(Disabled, Off, false, 1),
            None,
            "noindex switched off"
        );
        assert_eq!(
            machine_next(Default, Off, true, 0),
            Some(MachineNext {
                universal_control: true,
                spotlight: 0
            })
        );
        assert_eq!(
            machine_next(Partial, Off, true, 0).map(|n| n.universal_control),
            Some(true),
            "a half state is completed, not reported done"
        );
        assert_eq!(machine_next(Default, Leave, true, 0), None, "policy leave");
        assert_eq!(
            machine_next(Unknown, Off, true, 2),
            Some(MachineNext {
                universal_control: true,
                spotlight: 2
            }),
            "an unreadable posture is re-applied, never skipped"
        );
        // And the record's own `next()` refuses under a synthetic home, whatever is left.
        let s = MachineState {
            universal_control: Default,
            policy: Off,
            spotlight_noindex: true,
            exposed: 3,
            hidden: 0,
            would_migrate: 3,
            scan_complete: true,
            home: HomePosture::Mismatch,
        };
        assert_eq!(s.next(), None);
        assert!(
            MachineState {
                home: HomePosture::Account,
                ..s
            }
            .next()
            .is_some()
        );
    }
}
