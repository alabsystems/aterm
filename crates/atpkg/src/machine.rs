// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Machine settings `aterm pkg machine apply` applies per the doctor's findings: the policy half (what to do given what `defaults` answered), kept pure and behind a port so no test touches the real `com.apple.universalcontrol` domain.

use crate::config::UniversalControlPolicy;

/// The one-line revert every surface that mentions the change prints (the window's `appstatus` ledger entry, doctor, this pass's log).
pub const UNIVERSAL_CONTROL_REVERT: &str = "defaults -currentHost delete \
    com.apple.universalcontrol Disable; defaults -currentHost delete \
    com.apple.universalcontrol DisableMagicEdges";

/// The `machine-settings:` entry for a Universal Control change (contract string).
pub const UNIVERSAL_CONTROL_ENTRY: &str = "universal-control disabled";

/// What one `defaults -currentHost read` answered about one key: the converge engine's tri-state read, which the machines families keep too.
pub use aterm_converge::Read;

/// Universal Control's two per-host switches, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UniversalControlState {
    /// `Disable`: `Some(true)` = the feature is off.
    pub disable: Option<bool>,
    /// `DisableMagicEdges`: `Some(true)` = no screen-edge hand-off.
    pub magic_edges: Option<bool>,
    /// Both keys were actually MEASURED — a value, or a documented absence.
    pub measured: bool,
}

impl UniversalControlState {
    /// Fold the port's two answers.
    #[must_use]
    pub fn from_reads(reads: [Read<bool>; 2]) -> Self {
        const fn value(read: &Read<bool>) -> Option<bool> {
            match read {
                Read::Value(v) => Some(*v),
                Read::Absent | Read::Unusable(_) => None,
            }
        }
        Self {
            disable: value(&reads[0]),
            magic_edges: value(&reads[1]),
            measured: !matches!(reads[0], Read::Unusable(_))
                && !matches!(reads[1], Read::Unusable(_)),
        }
    }
}

/// When the default policy turns Universal Control off on its own: one daily slot that the
/// first window or terminal session claims ([`launch_apply_due`]) — not every window.
const DAILY_OFF: &str = "aterm turns it off once a day, at the first window or terminal session";

impl UniversalControlState {
    /// Fully disabled: both keys true.
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.disable == Some(true) && self.magic_edges == Some(true)
    }

    /// The doctor's words for this state (macOS only; the caller decides whether to print).
    #[must_use]
    pub fn doctor_line(&self, policy: UniversalControlPolicy) -> String {
        crate::doctor::spelled(&self.doctor_row(policy))
    }

    /// [`Self::doctor_line`] as the typed row `aterm pkg doctor`'s `base.machine.universal-control` item reports.
    #[must_use]
    pub fn doctor_row(&self, policy: UniversalControlPolicy) -> crate::doctor::Row {
        self.row(policy, true)
    }

    /// [`Self::doctor_line`] without its `now:` action: the state alone, for a read whose own verdict names the action (bare `aterm pkg machine`), so one screen never names `aterm pkg machine apply` twice.
    #[must_use]
    pub fn state_line(&self, policy: UniversalControlPolicy) -> String {
        crate::doctor::spelled(&self.row(policy, false))
    }

    fn row(&self, policy: UniversalControlPolicy, with_action: bool) -> crate::doctor::Row {
        use crate::doctor::{Level, Row};
        let now = if with_action {
            " — now: `aterm pkg machine apply`"
        } else {
            ""
        };
        if !self.measured {
            return match policy {
                UniversalControlPolicy::Off => Row {
                    level: Level::Warn,
                    text: format!(
                        "Universal Control could not be read on this host; {DAILY_OFF}{now}"
                    ),
                },
                UniversalControlPolicy::Leave => Row {
                    level: Level::Ok,
                    text: "Universal Control could not be read on this host, and [machine] \
                           universal_control = \"leave\" means nothing would be written either \
                           way"
                    .to_string(),
                },
            };
        }
        if self.disabled() {
            let keep = match policy {
                UniversalControlPolicy::Off => "set [machine] universal_control = \"leave\" and ",
                UniversalControlPolicy::Leave => "",
            };
            return Row {
                level: Level::Ok,
                text: format!(
                    "Universal Control is disabled on this host; to turn it back on, {keep}\
                     run: {UNIVERSAL_CONTROL_REVERT}"
                ),
            };
        }
        let posture = if self.partial() {
            format!("half disabled{}", self.partial_note())
        } else {
            "at the OS default (the cursor roams to other Macs and iPads on this Apple \
             account)"
                .to_string()
        };
        match policy {
            UniversalControlPolicy::Off => Row {
                level: Level::Warn,
                text: format!(
                    "Universal Control is {posture}; {DAILY_OFF}, unless [machine] \
                     universal_control = \"leave\"{now}"
                ),
            },
            UniversalControlPolicy::Leave => Row {
                level: Level::Ok,
                text: format!(
                    "Universal Control left {posture} ([machine] universal_control = \"leave\")"
                ),
            },
        }
    }

    /// Exactly one of the two keys is set: a state no default has and no complete pass leaves.
    #[must_use]
    pub fn partial(&self) -> bool {
        !self.disabled() && (self.disable == Some(true) || self.magic_edges == Some(true))
    }

    /// The parenthetical naming which key a half state sets.
    const fn partial_note(&self) -> &'static str {
        match (self.disable, self.magic_edges) {
            (Some(true), _) => " (Disable is set, DisableMagicEdges is not)",
            (_, Some(true)) => " (DisableMagicEdges is set, Disable is not)",
            _ => "",
        }
    }
}

/// `defaults read`'s rendering of a boolean, as bytes: `1`/`0`, `true`/`false`, `YES`/`NO` (any case), surrounded by whitespace.
#[must_use]
#[cfg(any(target_os = "macos", test))]
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

/// The two Universal Control switches (`keys`, in order) from one `defaults -currentHost read`
/// of their whole domain: `ran` is its zero exit and stdout, `None` when it did not run or
/// passed its deadline. A key the domain's top level does not hold is absent, and so are both
/// when `defaults` exits non-zero printing nothing (no such domain); a key whose value is no
/// boolean, or a domain that cannot be read, is unusable. One spawn answers both keys.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn universal_control_from_domain(
    ran: Option<(bool, Vec<u8>)>,
    keys: [&str; 2],
) -> [Read<bool>; 2] {
    let unusable = |why: &str| {
        [
            Read::Unusable(why.to_owned()),
            Read::Unusable(why.to_owned()),
        ]
    };
    let out = match ran {
        None => return unusable("`defaults read` did not run or passed its deadline"),
        Some((false, out)) if out.iter().all(u8::is_ascii_whitespace) => {
            return [Read::Absent, Read::Absent];
        }
        Some((false, _)) => {
            return unusable("`defaults read` exited nonzero and printed something");
        }
        Some((true, out)) => out,
    };
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some("{") {
        return unusable("`defaults read` printed no domain");
    }
    let mut found: [Option<&str>; 2] = [None, None];
    let mut depth = 1usize;
    let mut closed = false;
    for line in lines {
        if depth == 1 && line == "}" {
            closed = true;
            break;
        }
        if depth == 1
            && let Some((key, value)) = line.split_once(" = ")
            && let Some(at) = keys.iter().position(|k| *k == key.trim_matches('"'))
        {
            found[at] = Some(value.trim_end_matches(';'));
        }
        // Nested dictionaries and arrays open and close on their own lines' ends.
        if line.ends_with('{') || line.ends_with('(') {
            depth += 1;
        } else if line.starts_with('}') || line.starts_with(')') {
            depth = depth.saturating_sub(1).max(1);
        }
    }
    if !closed {
        return unusable("`defaults read` printed a domain cut short");
    }
    let read = |key: &str, value: Option<&str>| match value {
        None => Read::Absent,
        Some(value) => parse_defaults_bool(value.trim_matches('"').as_bytes()).map_or_else(
            || Read::Unusable(format!("`defaults read` printed no boolean for {key}")),
            Read::Value,
        ),
    };
    [read(keys[0], found[0]), read(keys[1], found[1])]
}

/// The port to the real `defaults` domain — one read, one write — so the policy is testable against a fake and the tests never touch the machine.
pub trait UniversalControlPort {
    /// `[Disable, DisableMagicEdges]` as [`crate::platform::universal_control_state`].
    fn state(&self) -> [Read<bool>; 2];
    /// Write both keys true; `true` iff both writes succeeded.
    fn disable(&self) -> bool;
}

/// The real port: `/usr/bin/defaults -currentHost`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDefaults;

impl UniversalControlPort for SystemDefaults {
    fn state(&self) -> [Read<bool>; 2] {
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

/// Apply `policy` through `port`: `Disabled` only when this pass changed something, which is what the `machine-settings:` marker reports.
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
    if universal_control_state(port).disabled() {
        UniversalControlOutcome::Disabled
    } else {
        UniversalControlOutcome::WriteFailed
    }
}

/// The file whose presence says Rosetta 2 is installed: its runtime, an arm64 Mach-O that the install lays in `/Library/Apple/usr/libexec/oah/` (with the `runtime`, `debugserver` and `translate_tool` links) beside `RosettaLinux`, which a Mac without Rosetta 2 already holds.
pub const ROSETTA_RUNTIME: &str = "/Library/Apple/usr/libexec/oah/libRosettaRuntime";

/// Whether this build runs where Rosetta 2 can be missing: macOS on Apple silicon.
pub const ROSETTA_APPLIES: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// The `machine-settings:` entry for a Rosetta 2 install (contract string).
pub const ROSETTA_ENTRY: &str = "rosetta installed";

/// How long the install may take before it is killed and reported as failed.
pub const ROSETTA_INSTALL_WITHIN: std::time::Duration = std::time::Duration::from_secs(300);

/// Rosetta 2 as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosettaPosture {
    /// [`ROSETTA_RUNTIME`] is there.
    Present,
    /// `stat` said it is not there: the install is next.
    Missing,
    /// `stat` could not say either way.
    Unknown,
    /// Not macOS on Apple silicon ([`ROSETTA_APPLIES`]): nothing was read.
    NotApplicable,
}

impl RosettaPosture {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not-applicable",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "present" => Self::Present,
            "missing" => Self::Missing,
            "unknown" => Self::Unknown,
            "not-applicable" => Self::NotApplicable,
            _ => return None,
        })
    }

    /// The doctor's row for this posture under `[machine] rosetta = wanted`, or `None` where Rosetta 2 does not apply.
    #[must_use]
    pub fn doctor_row(self, wanted: bool) -> Option<crate::doctor::Row> {
        use crate::doctor::{Level, Row};
        let (level, text) = match (self, wanted) {
            (Self::NotApplicable, _) => return None,
            (Self::Present, _) => (
                Level::Ok,
                "Rosetta 2 is installed (x86_64 programs run on this Mac)".to_string(),
            ),
            (Self::Missing, true) => (
                Level::Warn,
                "Rosetta 2 is not installed — x86_64 programs cannot run on this Mac; aterm \
                 installs it once a day, at the first window or terminal session, as you and \
                 with no password, unless [machine] rosetta = false"
                    .to_string(),
            ),
            (Self::Missing, false) => (
                Level::Ok,
                "Rosetta 2 is not installed, and [machine] rosetta = false leaves it so"
                    .to_string(),
            ),
            (Self::Unknown, _) => (
                Level::Warn,
                format!(
                    "Rosetta 2 could not be read on this Mac ({ROSETTA_RUNTIME} could not be \
                     checked), so nothing is installed on that reading"
                ),
            ),
        };
        Some(Row { level, text })
    }
}

/// The port to the real machine — one probe, one install — so the policy is testable against a fake and no test runs `softwareupdate`.
pub trait RosettaPort {
    /// Whether [`ROSETTA_RUNTIME`] is there: `Ok(false)` only when `stat` said it is not, `Err(why)` when it could not say.
    fn present(&self) -> Result<bool, String>;
    /// Run the install once; `Err(why)` unless it exited 0 within [`ROSETTA_INSTALL_WITHIN`].
    fn install(&self) -> Result<(), String>;
}

/// The real port: a `stat` of [`ROSETTA_RUNTIME`] and `/usr/sbin/softwareupdate --install-rosetta --agree-to-license` ([`crate::platform::rosetta_install`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRosetta;

impl RosettaPort for SystemRosetta {
    fn present(&self) -> Result<bool, String> {
        crate::platform::rosetta_present(std::path::Path::new(ROSETTA_RUNTIME))
    }
    fn install(&self) -> Result<(), String> {
        crate::platform::rosetta_install(ROSETTA_INSTALL_WITHIN)
    }
}

/// Read Rosetta 2 through `port`, where it applies ([`ROSETTA_APPLIES`]).
#[must_use]
pub fn rosetta_posture(port: &dyn RosettaPort) -> RosettaPosture {
    rosetta_posture_on(ROSETTA_APPLIES, port)
}

/// [`rosetta_posture`] with where it applies injected.
fn rosetta_posture_on(applies: bool, port: &dyn RosettaPort) -> RosettaPosture {
    if !applies {
        return RosettaPosture::NotApplicable;
    }
    match port.present() {
        Ok(true) => RosettaPosture::Present,
        Ok(false) => RosettaPosture::Missing,
        Err(_) => RosettaPosture::Unknown,
    }
}

/// What one pass did about Rosetta 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosettaOutcome {
    /// Not macOS on Apple silicon, or `[machine] rosetta = false`: nothing read, nothing run.
    NotApplicable,
    /// Already installed: nothing run.
    AlreadyPresent,
    /// Installed by this pass and read back — the `machine-settings:` entry.
    Installed,
    /// Nothing changed, and why: the probe could not say, the install failed, or it exited 0 and the runtime is still not there.
    Failed(String),
}

/// Install Rosetta 2 through `port` where it applies and `[machine] rosetta` wants it: `Installed` only when this pass ran the install and the runtime then reads present.
#[must_use]
pub fn apply_rosetta(wanted: bool, port: &dyn RosettaPort) -> RosettaOutcome {
    apply_rosetta_on(ROSETTA_APPLIES, wanted, port)
}

/// [`apply_rosetta`] with where it applies injected, so both arms are tested on any host.
fn apply_rosetta_on(applies: bool, wanted: bool, port: &dyn RosettaPort) -> RosettaOutcome {
    if !applies || !wanted {
        return RosettaOutcome::NotApplicable;
    }
    match port.present() {
        Ok(true) => return RosettaOutcome::AlreadyPresent,
        Ok(false) => {}
        Err(why) => {
            return RosettaOutcome::Failed(format!(
                "{ROSETTA_RUNTIME} could not be checked ({why}), so nothing was installed"
            ));
        }
    }
    if let Err(why) = port.install() {
        return RosettaOutcome::Failed(why);
    }
    match port.present() {
        Ok(true) => RosettaOutcome::Installed,
        Ok(false) => RosettaOutcome::Failed(format!(
            "softwareupdate exited 0, but {ROSETTA_RUNTIME} is still not there"
        )),
        Err(why) => RosettaOutcome::Failed(format!(
            "softwareupdate exited 0, but {ROSETTA_RUNTIME} could not be checked ({why})"
        )),
    }
}

/// Universal Control, as the two per-host keys read — the coarse posture a report names, derived from [`UniversalControlState`] and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UcPosture {
    /// Both keys true: the pass's wanted state.
    Disabled,
    /// Neither key set: the OS default — the cursor roams.
    Default,
    /// Exactly one of the two set: a half state the pass completes rather than reports as done.
    Partial,
    /// A key read as something this module does not treat as a switch, or the read could not be made.
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
    /// The coarse posture: `Unknown` when either read measured nothing; otherwise both keys true is `Disabled`, one is `Partial`, and neither is the OS default (an absent key is the ordinary state of a machine that never turned the feature off).
    #[must_use]
    pub const fn posture(&self) -> UcPosture {
        // An UNMEASURED read is `Unknown`, which is the variant's whole purpose: before
        // this, a `defaults` that would not run reported the OS default and Settings
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

/// Whether this process's home is the account's — the synthetic-home rule the apply path enforces (`defaults` writes the account's per-host domain and ignores `$HOME`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomePosture {
    /// `$HOME` is the account home: the settings apply here.
    Account,
    /// `$HOME` is somewhere else: a synthetic machine, nothing is applied.
    Mismatch,
    /// The account home could not be resolved, so `$HOME` cannot be proven to be it: nothing is applied.
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

/// What bare `atpkg machine` measured — the whole of it, as one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineState {
    /// The two keys' posture.
    pub universal_control: UcPosture,
    /// `[machine] universal_control`, resolved.
    pub policy: UniversalControlPolicy,
    /// The synthetic-home rule's answer.
    pub home: HomePosture,
    /// The config file exists and does not parse, so the opt-out could not be read and nothing may be applied ([`crate::config::MachineConfig::unreadable`]).
    pub config_unreadable: bool,
    /// Rosetta 2 as read ([`rosetta_posture`]).
    pub rosetta: RosettaPosture,
    /// `[machine] rosetta`, resolved.
    pub rosetta_wanted: bool,
}

impl MachineState {
    /// Whether an apply would change this machine: the CLI's `next —` line and the window's Apply button both read it, so the two cannot drift.
    #[must_use]
    pub fn next(&self) -> bool {
        self.universal_control_next() || self.rosetta_next()
    }

    /// Whether an apply would turn Universal Control off: an apply may run here, the policy is off, and it is not already off; an unreadable posture is applied again, never skipped.
    #[must_use]
    pub fn universal_control_next(&self) -> bool {
        self.applies_here()
            && self.policy == UniversalControlPolicy::Off
            && !matches!(self.universal_control, UcPosture::Disabled)
    }

    /// Whether an apply would install Rosetta 2: an apply may run here, `[machine] rosetta` wants it, and it read missing.
    #[must_use]
    pub fn rosetta_next(&self) -> bool {
        self.applies_here() && self.rosetta_wanted && self.rosetta == RosettaPosture::Missing
    }

    /// Nothing stands in the way of an apply: this account's home, a readable config.
    fn applies_here(&self) -> bool {
        self.home == HomePosture::Account && !self.config_unreadable
    }
}

/// The marker body, `key=value` pairs joined by `; ` — the same shape as the `machine-settings:` body, so one reader handles both.
#[must_use]
pub fn machine_state_line(s: &MachineState) -> String {
    format!(
        "universal-control={}; policy={}; home={}; rosetta={}; rosetta-policy={}",
        s.universal_control.as_str(),
        match s.policy {
            UniversalControlPolicy::Off => "off",
            UniversalControlPolicy::Leave => "leave",
        },
        s.home.as_str(),
        s.rosetta.as_str(),
        if s.rosetta_wanted { "install" } else { "leave" },
    ) + if s.config_unreadable {
        "; config=unreadable"
    } else {
        ""
    }
}

/// Parse a [`machine_state_line`] body.
#[must_use]
pub fn parse_machine_state(body: &str) -> Option<MachineState> {
    let mut uc = None;
    let mut policy = None;
    let mut home = None;
    let mut config_unreadable = false;
    let mut rosetta = Some(RosettaPosture::NotApplicable);
    let mut rosetta_wanted = Some(true);
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
            "home" => home = HomePosture::parse(v),
            "rosetta" => rosetta = RosettaPosture::parse(v),
            "rosetta-policy" => {
                rosetta_wanted = match v {
                    "install" => Some(true),
                    "leave" => Some(false),
                    _ => None,
                }
            }
            "config" => config_unreadable = v == "unreadable",
            _ => {}
        }
    }
    Some(MachineState {
        universal_control: uc?,
        policy: policy?,
        home: home?,
        config_unreadable,
        rosetta: rosetta?,
        rosetta_wanted: rosetta_wanted?,
    })
}

/// The `[machine]` table as one stable text, what [`record_applied`] keeps and [`changed_since_applied`] compares, spelled from the table as written (the raw `Option`s), so an edit is a change even when it lands on a default. v3 added `rosetta`: a stamp an older atpkg wrote reads as changed, so the next pass applies once more and installs Rosetta 2 where it is missing.
#[must_use]
pub fn applied_fingerprint(cfg: &crate::config::MachineConfig) -> String {
    format!(
        "atpkg-machine-applied v3\nuniversal_control={:?}\nrosetta={:?}\nunreadable={}\n",
        cfg.universal_control, cfg.rosetta, cfg.unreadable
    )
}

/// `<prefix>/machine.applied` — the `[machine]` table the last apply acted on.
#[must_use]
pub fn applied_stamp_path(layout: &crate::store::Layout) -> std::path::PathBuf {
    layout.prefix.join("machine.applied")
}

/// Whether `cfg` differs from the table the last apply recorded — an absent or unreadable stamp counts as changed, so a machine that never applied them does at its next pass.
#[must_use]
pub fn changed_since_applied(
    layout: &crate::store::Layout,
    cfg: &crate::config::MachineConfig,
) -> bool {
    crate::metadata_io::read_bounded_regular_utf8(&applied_stamp_path(layout), 4096)
        .map_or(true, |had| had != applied_fingerprint(cfg))
}

/// Forget the applied table, so the next pass applies it again: what an apply that did not finish (a failed `defaults` write) leaves.
pub fn forget_applied(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(applied_stamp_path(layout));
}

/// Record `cfg` as the applied table: temp + rename, only when the bytes differ, and only where the prefix already exists (a pass or the apply queue creates it; a stamp never does).
pub fn record_applied(layout: &crate::store::Layout, cfg: &crate::config::MachineConfig) {
    let path = applied_stamp_path(layout);
    let text = applied_fingerprint(cfg);
    if !layout.prefix.is_dir() || !changed_since_applied(layout, cfg) {
        return;
    }
    let tmp = layout
        .prefix
        .join(format!("machine.applied.tmp-{}", std::process::id()));
    if std::fs::write(&tmp, text).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// How often a launch runs `aterm pkg machine apply` on its own: once a day.
pub const LAUNCH_APPLY_EVERY_SECS: u64 = 24 * 60 * 60;

/// The one line a launch logs when it skips the day's apply under a private state root ([`LaunchApply::PrivateStateRoot`]).
pub const PRIVATE_STATE_ROOT_SKIP: &str = "atpkg machine apply: skipped at launch \u{2014} \
     this process runs under a private state root (ATERM_STATE_HOME), and the [machine] \
     settings are this Mac's; the day's slot is left unclaimed, and `aterm pkg machine apply` \
     typed in a terminal still applies them";

/// What a launch does about the day's `aterm pkg machine apply` ([`launch_apply_due`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchApply {
    /// This launch claimed the day's slot: it runs the apply.
    Due,
    /// Another launch holds the day's slot: nothing runs.
    NotDue,
    /// This process runs under a private state root (`aterm_types::dirs::runs_under_private_state_root`): nothing runs, and the slot was never claimed, so the day's apply stays the owner's ([`PRIVATE_STATE_ROOT_SKIP`]).
    PrivateStateRoot,
}

impl LaunchApply {
    /// Whether the launch runs the apply.
    pub fn runs(self, say: impl FnOnce(&'static str)) -> bool {
        match self {
            Self::Due => true,
            Self::NotDue => false,
            Self::PrivateStateRoot => {
                say(PRIVATE_STATE_ROOT_SKIP);
                false
            }
        }
    }
}

/// Whether this launch runs the day's `aterm pkg machine apply`: it claims the one daily slot in `machine-apply.stamp` ([`aterm_update_core::pkg_check::claim`]).
#[must_use]
pub fn launch_apply_due(layout: &crate::store::Layout, now_unix: i64) -> LaunchApply {
    launch_apply_decision(
        layout,
        now_unix,
        aterm_types::dirs::runs_under_private_state_root(),
    )
}

/// [`launch_apply_due`] with the private-state-root answer injected; private, so only this
/// module's tests pass it in.
#[must_use]
fn launch_apply_decision(
    layout: &crate::store::Layout,
    now_unix: i64,
    private_state_root: bool,
) -> LaunchApply {
    if private_state_root {
        return LaunchApply::PrivateStateRoot;
    }
    if aterm_update_core::pkg_check::claim(
        &layout.machine_apply_stamp(),
        now_unix,
        LAUNCH_APPLY_EVERY_SECS,
    ) {
        LaunchApply::Due
    } else {
        LaunchApply::NotDue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The first launch of the day claims the slot; every later one, whichever lane, runs
    /// nothing until a day has passed.
    #[test]
    fn the_launch_apply_is_claimed_once_a_day() {
        let prefix = crate::testdir::dir("atpkg-machine-daily-");
        let layout = crate::store::Layout {
            prefix: prefix.clone(),
        };
        let now = 1_790_000_000_i64;
        let day = i64::try_from(LAUNCH_APPLY_EVERY_SECS).unwrap();
        let due = |at| launch_apply_decision(&layout, at, false);
        assert_eq!(due(now), LaunchApply::Due);
        assert_eq!(due(now + 60), LaunchApply::NotDue);
        assert_eq!(due(now + day - 1), LaunchApply::NotDue);
        assert_eq!(due(now + day), LaunchApply::Due);
        assert!(layout.machine_apply_stamp().is_file());
        let _ = std::fs::remove_dir_all(&prefix);
    }

    /// Under a private state root a launch runs and claims nothing, however often it asks,
    /// so the owner's first launch of the day still claims the slot; only the skip says a
    /// line.
    #[test]
    fn a_private_state_root_skips_the_launch_apply_before_the_claim() {
        let prefix = crate::testdir::dir("atpkg-machine-private-");
        let layout = crate::store::Layout {
            prefix: prefix.clone(),
        };
        let now = 1_790_000_000_i64;
        for at in [now, now + 60, now + 2 * 86_400] {
            assert_eq!(
                launch_apply_decision(&layout, at, true),
                LaunchApply::PrivateStateRoot
            );
        }
        assert!(
            !layout.machine_apply_stamp().exists(),
            "the skip precedes the claim: no stamp"
        );
        assert_eq!(launch_apply_decision(&layout, now, false), LaunchApply::Due);
        assert!(layout.machine_apply_stamp().is_file());
        assert_eq!(
            launch_apply_decision(&layout, now + 1, true),
            LaunchApply::PrivateStateRoot
        );

        let mut said = Vec::new();
        assert!(!LaunchApply::PrivateStateRoot.runs(|line| said.push(line)));
        assert_eq!(said, [PRIVATE_STATE_ROOT_SKIP]);
        assert!(LaunchApply::Due.runs(|line| said.push(line)));
        assert!(!LaunchApply::NotDue.runs(|line| said.push(line)));
        assert_eq!(said.len(), 1, "only the skip says anything");
        let _ = std::fs::remove_dir_all(&prefix);
    }

    /// A never-applied table reads changed; recording it makes it unchanged (and a second
    /// record writes nothing); an edit, even onto a default, reads changed again.
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
        forget_applied(&layout);
        assert!(
            changed_since_applied(&layout, &default),
            "forgotten: retried"
        );
        record_applied(&layout, &default);
        let leave = crate::config::parse_machine("[machine]\nuniversal_control = \"leave\"\n");
        assert!(changed_since_applied(&layout, &leave), "an edit");
        let spelled_default =
            crate::config::parse_machine("[machine]\nuniversal_control = \"off\"\n");
        assert!(
            changed_since_applied(&layout, &spelled_default),
            "spelling out a default is an edit too"
        );
        let no_rosetta = crate::config::parse_machine("[machine]\nrosetta = false\n");
        assert!(changed_since_applied(&layout, &no_rosetta), "rosetta off");
        record_applied(&layout, &no_rosetta);
        assert!(!changed_since_applied(&layout, &no_rosetta));
        let rosetta_on = crate::config::parse_machine("[machine]\nrosetta = true\n");
        assert!(
            changed_since_applied(&layout, &rosetta_on),
            "rosetta back on"
        );
        std::fs::write(
            applied_stamp_path(&layout),
            "atpkg-machine-applied v2\nuniversal_control=None\nunreadable=false\n",
        )
        .unwrap();
        assert!(changed_since_applied(&layout, &default), "a v2 stamp");
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
        fn state(&self) -> [Read<bool>; 2] {
            let read = |v: Option<bool>| {
                if self.unusable {
                    Read::Unusable("defaults timed out".into())
                } else {
                    v.map_or(Read::Absent, Read::Value)
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

    /// One read of the whole domain answers both switches, as two key reads did. NEGATIVE
    /// CONTROL: a key only a nested dictionary holds is absent at the top level, a value that
    /// is no boolean is unusable, and a missing domain leaves both absent.
    #[test]
    fn one_domain_read_answers_both_universal_control_switches() {
        const KEYS: [&str; 2] = ["Disable", "DisableMagicEdges"];
        let read = |ok: bool, out: &str| {
            universal_control_from_domain(Some((ok, out.as_bytes().to_vec())), KEYS)
        };
        assert_eq!(
            read(true, "{\n    Disable = 1;\n    DisableMagicEdges = 1;\n}\n"),
            [Read::Value(true), Read::Value(true)]
        );
        assert_eq!(
            read(true, "{\n    Disable = 0;\n}\n"),
            [Read::Value(false), Read::Absent]
        );
        assert_eq!(
            read(
                true,
                "{\n    Other =     {\n        Disable = 1;\n    };\n    DisableMagicEdges = \
                 true;\n}\n"
            ),
            [Read::Absent, Read::Value(true)],
            "a nested key is not the domain's"
        );
        assert!(matches!(
            read(true, "{\n    Disable = \"maybe\";\n}\n"),
            [Read::Unusable(_), Read::Absent]
        ));
        assert_eq!(read(false, ""), [Read::Absent, Read::Absent], "no domain");
        assert!(matches!(
            read(false, "something"),
            [Read::Unusable(_), Read::Unusable(_)]
        ));
        assert!(matches!(
            read(true, "{\n    Disable = 1;\n"),
            [Read::Unusable(_), Read::Unusable(_)]
        ));
        assert!(matches!(
            universal_control_from_domain(None, KEYS),
            [Read::Unusable(_), Read::Unusable(_)]
        ));
    }

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
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &fake),
            UniversalControlOutcome::AlreadyDisabled
        );
        assert_eq!(fake.writes.get(), 1);
        let half = Fake::at(Some(true), None);
        assert!(!universal_control_state(&half).disabled());
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &half),
            UniversalControlOutcome::Disabled
        );
        let leave = Fake::at(None, None);
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Leave, &leave),
            UniversalControlOutcome::NotApplicable
        );
        assert_eq!(leave.writes.get(), 0);
        let mut broken = Fake::at(Some(false), Some(false));
        broken.write_ok = false;
        assert_eq!(
            apply_universal_control(UniversalControlPolicy::Off, &broken),
            UniversalControlOutcome::WriteFailed
        );
        assert_eq!(broken.writes.get(), 1);
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
                    for rosetta in [
                        RosettaPosture::Present,
                        RosettaPosture::Missing,
                        RosettaPosture::Unknown,
                        RosettaPosture::NotApplicable,
                    ] {
                        for rosetta_wanted in [true, false] {
                            let s = MachineState {
                                universal_control: uc,
                                policy,
                                home,
                                config_unreadable: uc == UcPosture::Unknown,
                                rosetta,
                                rosetta_wanted,
                            };
                            let line = machine_state_line(&s);
                            assert_eq!(parse_machine_state(&line), Some(s.clone()), "{line}");
                        }
                    }
                }
            }
        }
        let full = "universal-control=disabled; policy=off; home=account";
        assert!(parse_machine_state(full).is_some());
        assert!(parse_machine_state(&format!("{full}; future-key=whatever")).is_some());
        assert!(parse_machine_state("universal-control=disabled; policy=off").is_none());
        assert!(parse_machine_state(&full.replace("home=account", "home=elsewhere")).is_none());
        assert!(parse_machine_state("").is_none());
        let older = parse_machine_state(full).unwrap();
        assert_eq!(older.rosetta, RosettaPosture::NotApplicable);
        assert!(older.rosetta_wanted);
        assert!(!older.rosetta_next());
        assert!(parse_machine_state(&format!("{full}; rosetta=installed")).is_none());
        assert!(parse_machine_state(&format!("{full}; rosetta-policy=yes")).is_none());
        assert_eq!(
            parse_machine_state(&format!("{full}; rosetta=missing; rosetta-policy=leave"))
                .map(|s| (s.rosetta, s.rosetta_wanted)),
            Some((RosettaPosture::Missing, false))
        );
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
        let read = UniversalControlState::from_reads([Read::Absent, Read::Absent]);
        assert!(read.measured);
        assert_eq!(read.posture(), UcPosture::Default);
        assert_eq!(
            UniversalControlState::from_reads([Read::Value(true), Read::Absent]).posture(),
            UcPosture::Partial
        );
        let unread = UniversalControlState::default();
        assert!(!unread.measured);
        assert_eq!(unread.posture(), UcPosture::Unknown);
        for other in [Read::Absent, Read::Value(true)] {
            let unusable = Read::Unusable("defaults passed its deadline".into());
            for reads in [[unusable.clone(), other.clone()], [other.clone(), unusable]] {
                let state = UniversalControlState::from_reads(reads);
                assert!(!state.measured);
                assert_eq!(state.posture(), UcPosture::Unknown);
            }
        }
    }

    /// The verdict the read verb's `next —` line and the window's Apply button both read.
    #[test]
    fn an_apply_is_next_only_where_it_would_change_universal_control() {
        use UcPosture::*;
        use UniversalControlPolicy::*;
        let at = |universal_control, policy| MachineState {
            universal_control,
            policy,
            home: HomePosture::Account,
            config_unreadable: false,
            rosetta: RosettaPosture::Present,
            rosetta_wanted: true,
        };
        assert!(!at(Disabled, Off).next(), "everything done");
        assert!(at(Default, Off).next());
        assert!(
            at(Partial, Off).next(),
            "a half state is completed, not reported done"
        );
        assert!(
            at(Unknown, Off).next(),
            "an unreadable posture is re-applied, never skipped"
        );
        assert!(!at(Default, Leave).next(), "policy leave");
        let s = at(Default, Off);
        assert!(
            !MachineState {
                home: HomePosture::Mismatch,
                ..s.clone()
            }
            .next()
        );
        assert!(
            !MachineState {
                config_unreadable: true,
                ..s.clone()
            }
            .next(),
            "an unreadable config offers no next step"
        );
        let flagged = MachineState {
            config_unreadable: true,
            ..s.clone()
        };
        let line = machine_state_line(&flagged);
        assert!(line.ends_with("; config=unreadable"), "{line}");
        assert_eq!(parse_machine_state(&line), Some(flagged));
        let missing = MachineState {
            rosetta: RosettaPosture::Missing,
            ..at(Disabled, Off)
        };
        assert!(missing.next() && missing.rosetta_next());
        assert!(!missing.universal_control_next());
        assert!(
            MachineState {
                policy: Leave,
                ..missing.clone()
            }
            .next(),
            "the Universal Control opt-out is not the Rosetta one"
        );
        for (rosetta, rosetta_wanted, why) in [
            (RosettaPosture::Missing, false, "rosetta = false"),
            (
                RosettaPosture::Unknown,
                true,
                "an unread runtime is not missing",
            ),
            (RosettaPosture::NotApplicable, true, "not Apple silicon"),
            (RosettaPosture::Present, true, "installed"),
        ] {
            let state = MachineState {
                rosetta,
                rosetta_wanted,
                ..missing.clone()
            };
            assert!(!state.next(), "{why}");
        }
        assert!(
            !MachineState {
                home: HomePosture::Mismatch,
                ..missing.clone()
            }
            .next()
        );
        assert!(
            !MachineState {
                config_unreadable: true,
                ..missing.clone()
            }
            .next()
        );
        let both = MachineState {
            universal_control: Default,
            ..missing
        };
        assert!(both.universal_control_next() && both.rosetta_next());
        assert_eq!(s.universal_control_next(), s.next());
    }

    /// A fake Mac for Rosetta 2: whether the runtime is there, whether the probe can say,
    /// whether the install succeeds and lands, and how often it ran.
    struct FakeRosetta {
        present: Cell<bool>,
        probe_fails: bool,
        install_ok: bool,
        install_lands: bool,
        installs: Cell<u32>,
    }

    impl FakeRosetta {
        fn missing() -> Self {
            FakeRosetta {
                present: Cell::new(false),
                probe_fails: false,
                install_ok: true,
                install_lands: true,
                installs: Cell::new(0),
            }
        }
    }

    impl RosettaPort for FakeRosetta {
        fn present(&self) -> Result<bool, String> {
            if self.probe_fails {
                Err("permission denied".into())
            } else {
                Ok(self.present.get())
            }
        }
        fn install(&self) -> Result<(), String> {
            self.installs.set(self.installs.get() + 1);
            if !self.install_ok {
                return Err("softwareupdate exited 1: Package Authoring Error".into());
            }
            if self.install_lands {
                self.present.set(true);
            }
            Ok(())
        }
    }

    /// The policy over the fake: a missing Rosetta 2 is installed once and read back, an
    /// installed one is left alone, `rosetta = false` and a Mac where it does not apply run
    /// nothing, and an unread probe, a failed install and one that did not land are
    /// failures that install nothing more.
    #[test]
    fn rosetta_is_installed_once_where_it_applies_and_never_on_an_unproven_read() {
        let mac = FakeRosetta::missing();
        assert_eq!(
            rosetta_posture_on(true, &mac),
            RosettaPosture::Missing,
            "read before the install"
        );
        assert_eq!(
            apply_rosetta_on(true, true, &mac),
            RosettaOutcome::Installed
        );
        assert_eq!(mac.installs.get(), 1);
        assert_eq!(rosetta_posture_on(true, &mac), RosettaPosture::Present);
        assert_eq!(
            apply_rosetta_on(true, true, &mac),
            RosettaOutcome::AlreadyPresent
        );
        assert_eq!(mac.installs.get(), 1);
        for (applies, wanted) in [(true, false), (false, true), (false, false)] {
            let mac = FakeRosetta {
                probe_fails: true,
                ..FakeRosetta::missing()
            };
            assert_eq!(
                apply_rosetta_on(applies, wanted, &mac),
                RosettaOutcome::NotApplicable
            );
            assert_eq!(mac.installs.get(), 0);
        }
        assert_eq!(
            rosetta_posture_on(false, &FakeRosetta::missing()),
            RosettaPosture::NotApplicable
        );
        let unread = FakeRosetta {
            probe_fails: true,
            ..FakeRosetta::missing()
        };
        assert_eq!(rosetta_posture_on(true, &unread), RosettaPosture::Unknown);
        assert!(matches!(
            apply_rosetta_on(true, true, &unread),
            RosettaOutcome::Failed(why) if why.contains("could not be checked")
        ));
        assert_eq!(unread.installs.get(), 0);
        let refused = FakeRosetta {
            install_ok: false,
            ..FakeRosetta::missing()
        };
        assert!(matches!(
            apply_rosetta_on(true, true, &refused),
            RosettaOutcome::Failed(why) if why.contains("exited 1")
        ));
        let hollow = FakeRosetta {
            install_lands: false,
            ..FakeRosetta::missing()
        };
        assert!(matches!(
            apply_rosetta_on(true, true, &hollow),
            RosettaOutcome::Failed(why) if why.contains("still not there")
        ));
        assert_eq!(hollow.installs.get(), 1);
        let mac = FakeRosetta::missing();
        let expected = if ROSETTA_APPLIES {
            RosettaOutcome::Installed
        } else {
            RosettaOutcome::NotApplicable
        };
        assert_eq!(apply_rosetta(true, &mac), expected);
    }

    /// The doctor's Rosetta rows: none where it does not apply, a warning only when the
    /// apply would install it or could not read it, and the opt-out read as a choice.
    #[test]
    fn the_rosetta_doctor_row_warns_only_where_an_apply_would_act_or_could_not_read() {
        use crate::doctor::Level;
        assert!(RosettaPosture::NotApplicable.doctor_row(true).is_none());
        let row = |p: RosettaPosture, wanted| p.doctor_row(wanted).unwrap();
        assert_eq!(row(RosettaPosture::Present, true).level, Level::Ok);
        let missing = row(RosettaPosture::Missing, true);
        assert_eq!(missing.level, Level::Warn);
        assert!(
            missing.text.starts_with("Rosetta 2 is not installed — "),
            "the fact is the first clause: {}",
            missing.text
        );
        assert!(
            missing.text.contains("[machine] rosetta = false"),
            "{}",
            missing.text
        );
        let left = row(RosettaPosture::Missing, false);
        assert_eq!(left.level, Level::Ok);
        assert!(left.text.contains("rosetta = false"), "{}", left.text);
        assert_eq!(row(RosettaPosture::Unknown, true).level, Level::Warn);
    }
}
