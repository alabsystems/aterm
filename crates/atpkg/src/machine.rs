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

/// When the default policy turns Universal Control off on its own: one daily slot that an aterm
/// start claims, the app's or a terminal session's ([`launch_apply_due`]) — never a window a
/// running aterm opens.
const DAILY_OFF: &str = "aterm turns it off once a day, when it starts";

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
                 installs it once a day, when it starts, as you and \
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
    let _ = std::fs::remove_file(&tmp);
    let wrote = crate::platform::open_create_new_write(&tmp, 0o644)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
    if wrote.is_err() || std::fs::rename(&tmp, &path).is_err() {
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
#[path = "machine_tests.rs"]
mod tests;
