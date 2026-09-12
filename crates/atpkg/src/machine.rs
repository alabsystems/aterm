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
                 Macs and iPads on this Apple account){}; the next update pass disables it \
                 ([machine] universal_control = \"off\") — keep it: universal_control = \
                 \"leave\"",
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
        assert!(line.contains("next update pass disables it"), "{line}");
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
}
