// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Machine settings `aterm pkg machine apply` applies per the doctor's own findings (R5,
//! owner decisions 2026-09-10; a pass applies them only after an edit to the `[machine]`
//! table since Phase 3, [`changed_since_applied`]): the POLICY half — what to do given what `defaults`
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

/// The one-line revert every surface that mentions the change must print (the window's
/// `appstatus` ledger entry, doctor, this pass's log). The pass writes BOTH per-host keys
/// (`platform::universal_control_disable`), so the revert deletes both: `Disable` is
/// the feature, `DisableMagicEdges` the screen-edge hand-off, and deleting one leaves
/// the other set. Joined by `;`, not `&&`: `defaults delete` exits 1 on a key that is
/// already absent, and the second delete must run regardless.
pub const UNIVERSAL_CONTROL_REVERT: &str = "defaults -currentHost delete \
    com.apple.universalcontrol Disable; defaults -currentHost delete \
    com.apple.universalcontrol DisableMagicEdges";

/// The `machine-settings:` entry for a Universal Control change (contract string).
pub const UNIVERSAL_CONTROL_ENTRY: &str = "universal-control disabled";

/// What one `defaults -currentHost read` answered about one key.
///
/// ABSENT AND UNUSABLE ARE NOT THE SAME ANSWER (2026-09-15). Both used to arrive as
/// `None`, so a `defaults` that could not be run, was killed at its deadline, or
/// answered something this module does not understand was reported to the user as the
/// OS DEFAULT: "the cursor roams to other Macs and iPads", with Apply offered as if the
/// machine had been measured. It had not been. A key that is absent is a measurement —
/// `defaults` exits 1 with empty output to say "does not exist", which IS the OS
/// default — and anything else is the absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRead {
    /// The key holds a value this module understands.
    Value(bool),
    /// The key is not set: the documented "does not exist" answer.
    Absent,
    /// Nothing could be learned: `defaults` would not run, took too long, or said
    /// something unreadable.
    Unusable,
}

/// Universal Control's two per-host switches, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UniversalControlState {
    /// `Disable`: `Some(true)` = the feature is off.
    pub disable: Option<bool>,
    /// `DisableMagicEdges`: `Some(true)` = no screen-edge hand-off.
    pub magic_edges: Option<bool>,
    /// Both keys were actually MEASURED — a value, or a documented absence. `false`
    /// when either read was [`KeyRead::Unusable`], and then neither `Option` above may
    /// be read as "the OS default".
    pub measured: bool,
}

impl UniversalControlState {
    /// Fold the port's two answers.
    #[must_use]
    pub const fn from_reads(reads: [KeyRead; 2]) -> Self {
        const fn value(read: KeyRead) -> Option<bool> {
            match read {
                KeyRead::Value(v) => Some(v),
                KeyRead::Absent | KeyRead::Unusable => None,
            }
        }
        Self {
            disable: value(reads[0]),
            magic_edges: value(reads[1]),
            measured: !matches!(reads[0], KeyRead::Unusable)
                && !matches!(reads[1], KeyRead::Unusable),
        }
    }
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
        if !self.measured {
            // Never "the OS default": nothing was measured, and the remedy is the same
            // either way, so say what is true and name the command that would tell us.
            return match policy {
                UniversalControlPolicy::Off => "warn — Universal Control could not be read \
                     on this host (`defaults -currentHost read com.apple.universalcontrol` did \
                     not answer), so whether it is on here is unknown; the next aterm window (or \
                     the day's first terminal session) writes both keys anyway ([machine] universal_control = \"off\") — now: `aterm pkg \
                     machine apply`"
                    .to_string(),
                UniversalControlPolicy::Leave => {
                    "ok — Universal Control could not be read on this host, and [machine] \
                     universal_control = \"leave\" means nothing would be written either way"
                        .to_string()
                }
            };
        }
        if self.disabled() {
            // THE REVERT ALONE DOES NOT HOLD. With the default policy the next aterm window
            // (or the day's first terminal session) disables it again, so a line that offers
            // only the two `defaults delete`s sends the reader to a change that lasts until
            // then. The command stays byte-identical ([`UNIVERSAL_CONTROL_REVERT`] is
            // a contract string); what follows it is the half that makes it stick.
            let keep = match policy {
                UniversalControlPolicy::Off => {
                    " — and set [machine] universal_control = \"leave\", or the next aterm \
                     window (or the day's first terminal session) disables it again"
                }
                UniversalControlPolicy::Leave => "",
            };
            return format!(
                "ok — Universal Control is disabled on this host (cursor and keyboard stay \
                 on this Mac); revert: {UNIVERSAL_CONTROL_REVERT}{keep}"
            );
        }
        // A HALF-DISABLED HOST IS NOT AT THE OS DEFAULT. One key set is a state someone
        // (or a previous half-finished pass) made, and calling it the default while the
        // parenthetical says which key IS set contradicted itself in one sentence.
        let posture = if self.partial() {
            format!("half disabled{}", self.partial_note())
        } else {
            "at the OS default (the cursor roams to other Macs and iPads on this Apple \
             account)"
                .to_string()
        };
        match policy {
            UniversalControlPolicy::Off => format!(
                "warn — Universal Control is {posture}; the next aterm window (or the day's \
                 first terminal session) disables it ([machine] universal_control = \"off\") — now: \
                 `aterm pkg machine apply`; keep it: universal_control = \"leave\""
            ),
            UniversalControlPolicy::Leave => format!(
                "ok — Universal Control left {posture} ([machine] universal_control = \"leave\")"
            ),
        }
    }

    /// Exactly one of the two keys is set: a state no default has and no complete pass
    /// leaves.
    #[must_use]
    pub fn partial(&self) -> bool {
        !self.disabled() && (self.disable == Some(true) || self.magic_edges == Some(true))
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
    fn state(&self) -> [KeyRead; 2];
    /// Write both keys true; `true` iff both writes succeeded.
    fn disable(&self) -> bool;
}

/// The real port: `/usr/bin/defaults -currentHost`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDefaults;

impl UniversalControlPort for SystemDefaults {
    fn state(&self) -> [KeyRead; 2] {
        crate::platform::universal_control_state()
    }
    fn disable(&self) -> bool {
        crate::platform::universal_control_disable()
    }
}

/// Read the state through `port`.
#[must_use]
pub fn universal_control_state(port: &dyn UniversalControlPort) -> UniversalControlState {
    UniversalControlState::from_reads(port.state())
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
        // An UNMEASURED read is `Unknown`, which is the variant's whole purpose: before
        // this, a `defaults` that would not run reported the OS default and the card
        // told the user the cursor roams, on no evidence.
        if !self.measured {
            return UcPosture::Unknown;
        }
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
    /// The config file exists and does not parse, so neither opt-out could be read and
    /// nothing may be applied ([`crate::config::MachineConfig::unreadable`]).
    ///
    /// OPTIONAL IN THE RECORD, and `false` when absent, so a reader from before this key
    /// existed keeps working and a record written by one still parses here.
    pub config_unreadable: bool,
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
        // Nothing is next when nothing may be applied: a synthetic home, or a config
        // whose opt-outs could not be read.
        if self.home != HomePosture::Account || self.config_unreadable {
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
    ) + if s.config_unreadable {
        // APPENDED, NEVER INSERTED: every key before this one keeps its place, so a
        // reader that predates the key sees the record it has always seen and this one
        // is simply ignored by it (`parse_machine_state` skips unknown keys).
        "; config=unreadable"
    } else {
        ""
    }
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
    let mut config_unreadable = false;
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
            // Optional: absent means readable, which is what every record written
            // before this key existed means.
            "config" => config_unreadable = v == "unreadable",
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
        config_unreadable,
    })
}

/// The `[machine]` table as one stable text — what [`record_applied`] keeps and
/// [`changed_since_applied`] compares — spelled from the table as WRITTEN (the raw
/// `Option`s), so an edit is a change even when it lands on a default.
#[must_use]
pub fn applied_fingerprint(cfg: &crate::config::MachineConfig) -> String {
    format!(
        "atpkg-machine-applied v1\nspotlight_noindex={:?}\nuniversal_control={:?}\n\
         unreadable={}\n",
        cfg.spotlight_noindex, cfg.universal_control, cfg.unreadable
    )
}

/// `<prefix>/machine.applied` — the `[machine]` table the last apply acted on.
#[must_use]
pub fn applied_stamp_path(layout: &crate::store::Layout) -> std::path::PathBuf {
    layout.prefix.join("machine.applied")
}

/// Whether `cfg` differs from the table the last apply recorded — an absent or unreadable
/// stamp counts as changed, so a machine that never applied them does at its next pass.
#[must_use]
pub fn changed_since_applied(
    layout: &crate::store::Layout,
    cfg: &crate::config::MachineConfig,
) -> bool {
    crate::metadata_io::read_bounded_regular_utf8(&applied_stamp_path(layout), 4096)
        .map_or(true, |had| had != applied_fingerprint(cfg))
}

/// Forget the applied table, so the next pass's edge applies it again — what an apply that
/// did not FINISH leaves (a live build skipped, a rename or a `defaults` write that failed):
/// the passes carry only a changed table, so without this nothing but the next window (or
/// the day's first terminal session) would retry what it missed. Best-effort.
pub fn forget_applied(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(applied_stamp_path(layout));
}

/// Record `cfg` as the applied table: temp + rename, only when the bytes differ, and only
/// where the prefix already exists (a pass creates it; a stamp never does). Best-effort —
/// a stamp that did not land means the next pass applies once more.
pub fn record_applied(layout: &crate::store::Layout, cfg: &crate::config::MachineConfig) {
    let path = applied_stamp_path(layout);
    let text = applied_fingerprint(cfg);
    if !layout.prefix.is_dir() || !changed_since_applied(layout, cfg) {
        return;
    }
    let tmp = layout
        .prefix
        .join(format!("machine.applied.tmp-{}", std::process::id()));
    if crate::call2(std::fs::write, &tmp, text).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// THE PASSES CARRY ONLY AN EDIT (Phase 3): a machine that never applied the `[machine]`
    /// table reads "changed"; recording it makes the same table unchanged (and a second
    /// record writes nothing); an edit — even one that lands on a default — reads changed
    /// again. No prefix, no stamp: a stamp never creates the store.
    #[test]
    fn the_applied_table_is_recorded_and_an_edit_reads_as_changed() {
        let p = std::env::temp_dir().join(format!("atpkg-machine-applied-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        let layout = crate::store::Layout { prefix: p.clone() };
        let default = crate::config::MachineConfig::default();
        record_applied(&layout, &default);
        assert!(!p.exists(), "no prefix: nothing is created");
        std::fs::create_dir_all(&p).unwrap();
        assert!(changed_since_applied(&layout, &default), "never applied");
        record_applied(&layout, &default);
        assert!(!changed_since_applied(&layout, &default));
        let stamp = std::fs::metadata(applied_stamp_path(&layout))
            .unwrap()
            .modified()
            .unwrap();
        record_applied(&layout, &default);
        assert_eq!(
            std::fs::metadata(applied_stamp_path(&layout))
                .unwrap()
                .modified()
                .unwrap(),
            stamp,
            "unchanged: not rewritten"
        );
        // An apply that did not finish forgets it: the next pass applies again.
        forget_applied(&layout);
        assert!(
            changed_since_applied(&layout, &default),
            "forgotten: retried"
        );
        record_applied(&layout, &default);
        let leave = crate::config::parse_machine("[machine]\nuniversal_control = \"leave\"\n");
        assert!(changed_since_applied(&layout, &leave), "an edit");
        let spelled_default = crate::config::parse_machine("[machine]\nspotlight_noindex = true\n");
        assert!(
            changed_since_applied(&layout, &spelled_default),
            "spelling out a default is an edit too"
        );
        let _ = std::fs::remove_dir_all(&p);
    }

    /// A fake domain: the two keys, and a count of writes.
    struct Fake {
        disable: Cell<Option<bool>>,
        edges: Cell<Option<bool>>,
        writes: Cell<u32>,
        write_ok: bool,
        /// `defaults` itself could not answer — the read that taught us nothing.
        unusable: bool,
    }

    impl Fake {
        fn at(disable: Option<bool>, edges: Option<bool>) -> Self {
            Fake {
                disable: Cell::new(disable),
                edges: Cell::new(edges),
                writes: Cell::new(0),
                write_ok: true,
                unusable: false,
            }
        }
    }

    impl UniversalControlPort for Fake {
        fn state(&self) -> [KeyRead; 2] {
            let read = |v: Option<bool>| {
                if self.unusable {
                    KeyRead::Unusable
                } else {
                    v.map_or(KeyRead::Absent, KeyRead::Value)
                }
            };
            [read(self.disable.get()), read(self.edges.get())]
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
            measured: true,
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
        // The OS default is a MEASURED state: both keys absent, both reads answered.
        let default = UniversalControlState::from_reads([KeyRead::Absent, KeyRead::Absent]);
        assert!(default.measured);
        assert_eq!(default.posture(), UcPosture::Default);
        let line = default.doctor_line(UniversalControlPolicy::Off);
        assert!(line.starts_with("warn — "), "{line}");
        // The promise, and the door: WHAT applies it — the next window, or the day's first
        // terminal session, since the passes carry only an edit to the table (Phase 3;
        // "every pass disables it first thing" was the 2026-09-14 promise, and stopped
        // being true) and a session applies them once a day, not at every tab — and the
        // verb is how a person gets it NOW rather than then.
        assert!(
            line.contains(
                "the next aterm window (or the day's first terminal session) disables it"
            ),
            "{line}"
        );
        assert!(!line.contains("every pass"), "{line}");
        assert!(line.contains("now: `aterm pkg machine apply`"), "{line}");
        assert!(
            !line.contains("next update pass"),
            "the old promise must not survive: {line}"
        );
        assert!(line.contains("universal_control = \"leave\""), "{line}");
        let line = default.doctor_line(UniversalControlPolicy::Leave);
        assert!(line.starts_with("ok — "), "{line}");
        assert!(line.contains("left at the OS default"), "{line}");

        // A READ THAT DID NOT HAPPEN SAYS SO. `UniversalControlState::default()` is the
        // unmeasured value, and it must never render as the OS default.
        let unread = UniversalControlState::default();
        assert!(!unread.measured);
        assert_eq!(unread.posture(), UcPosture::Unknown);
        for (policy, lead) in [
            (UniversalControlPolicy::Off, "warn — "),
            (UniversalControlPolicy::Leave, "ok — "),
        ] {
            let line = unread.doctor_line(policy);
            assert!(line.starts_with(lead), "{line}");
            assert!(line.contains("could not be read"), "{line}");
            assert!(
                !line.contains("the cursor roams to other"),
                "an unread host must not be described as the measured default: {line}"
            );
        }

        // A HALF-DISABLED HOST IS NOT THE DEFAULT, whatever the policy says.
        let half_measured =
            UniversalControlState::from_reads([KeyRead::Value(true), KeyRead::Absent]);
        for policy in [UniversalControlPolicy::Off, UniversalControlPolicy::Leave] {
            let line = half_measured.doctor_line(policy);
            assert!(line.contains("half disabled"), "{line}");
            assert!(
                !line.contains("at the OS default"),
                "one key set is not the default: {line}"
            );
            assert!(line.contains("Disable is set"), "{line}");
        }
        let half = UniversalControlState {
            measured: true,
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
                        config_unreadable: uc == UcPosture::Unknown,
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
            measured: true,
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
            config_unreadable: false,
        };
        assert_eq!(s.next(), None);
        // The other refusal: a config that does not parse cannot say whether either
        // setting was switched off, so there is nothing to offer either.
        assert_eq!(
            MachineState {
                home: HomePosture::Account,
                config_unreadable: true,
                ..s.clone()
            }
            .next(),
            None,
            "an unreadable config offers no next step"
        );
        // A record written before the key existed parses, and means "readable".
        let older = "universal-control=default; policy=off; noindex=true; spotlight-exposed=3; \
                     spotlight-hidden=0; spotlight-migratable=3; scan=complete; home=account";
        let parsed = parse_machine_state(older).expect("the older record still parses");
        assert!(!parsed.config_unreadable);
        assert!(parsed.next().is_some());
        // And the new key round-trips.
        let flagged = MachineState {
            home: HomePosture::Account,
            config_unreadable: true,
            ..s.clone()
        };
        let line = machine_state_line(&flagged);
        assert!(line.ends_with("; config=unreadable"), "{line}");
        assert_eq!(parse_machine_state(&line), Some(flagged));
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
