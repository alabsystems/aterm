// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SUCCESSOR'S HANDOFF POLICY (the 2026-09-22/23 update audit, plan P0-5): a
//! small, signed TOML file inside a release's `.app` that lets the NEWER build tell
//! the OLDER one handing its sessions over to be more conservative.
//!
//! WHY IT EXISTS. An in-session update is decided by the OUTGOING build's code — the
//! park, the screen capture, the quiet gate — and the outgoing build is the older
//! one, so a producer bug cannot be fixed into the release that suffers it. The
//! capture ladder (`aterm-gui`'s `seamless::carry_for_wire`) makes the producer total
//! over the screen content it can SEE; this file is the escape hatch for the bugs it
//! cannot see: a capture path that is slow or panics, a proof that mismatches
//! itself, a park gate that never opens. From the release that seals it, the
//! successor can ask every affected producer to carry less — and only while later
//! releases keep it: a producer reads the policy of the release it hands over to
//! alone, so one that skipped the rescuing release is covered only if the newest
//! release still names it (`docs/RELEASING.md`).
//!
//! WHERE IT LIVES. The release cutter copies the checked-in [`SOURCE_PATH`]
//! (`publish/handoff-policy.toml`) into the bundle at [`BUNDLE_PATH`] BEFORE it
//! signs the bundle, so the code signature seals it with everything else under
//! `Contents/`. The outgoing build reads it from the CANDIDATE bundle — the staged
//! update, or the installed bundle an activation execs — only AFTER that bundle has
//! passed the same codesign policy and sealed-identity check that authorizes it as
//! the successor, and over the same bytes.
//!
//! WHY THAT IS ENOUGH TRUST. The candidate bundle is about to be exec'd as the
//! successor with every right this process has: it will own the terminal, the PTYs
//! and the user's files. A file sealed by that same signature can say nothing that
//! code could not do anyway, and every knob below can only LOWER what the producer
//! carries or relax a gate only the producer applies. None can touch a consumer
//! check, raise a cap, change the adoption proof, or skip the verification that
//! admitted the bundle.
//!
//! # Schema v1
//!
//! ```toml
//! schema = 1                              # required; any other value: file ignored
//! applies_to_producers = [min, max]       # optional; inclusive build numbers of the
//!                                         # OUTGOING builds that must follow it
//!                                         # (absent: every producer)
//! carry = "full" | "visible" | "repaint"  # optional; the HIGHEST rung the producer
//!                                         # may carry a session at
//! park_quiet_gate_at_land = "strict" | "relaxed"   # optional
//! ```
//!
//! * `carry = "visible"` carries no scrollback; `"repaint"` carries every session
//!   as a blank canonical screen that its program redraws, and no scrollback
//!   either. "No scrollback" covers both ways a producer carries it — the newest
//!   lines inside the screen carry and the whole history in a sidecar beside it
//!   ([`CarryCeiling::carries_scrollback`]) — and the lines left behind are counted
//!   and said like any scrollback an update cannot carry. `"full"` is the
//!   producer's own ladder, as it ships. A producer still takes a LOWER rung than
//!   the ceiling whenever its own ladder needs one.
//! * `park_quiet_gate_at_land = "relaxed"` lets the automatic lane's `Land` phase
//!   park over output still queued on a master at once, instead of after the
//!   producer's own bounded wait. `"strict"` is the producer's gate as it ships (the
//!   same as leaving the key out). It never relaxes the session-death check or the
//!   capture's mid-sequence check.
//! * `seamless` is RESERVED. It would route an update to a relaunch offer instead
//!   of the in-session handover, and the owner's rulings on update wording forbid
//!   asking a person to do that, so no reader implements it; the cutter refuses a
//!   policy that sets it.
//!
//! READING RULES — a producer that cannot follow a file falls back to today's
//! behaviour, never to something stricter:
//!
//! * No file: no policy.
//! * Unknown keys are ignored, so a later release can ADD a key without a schema
//!   bump. Bump `schema` only when an existing key changes meaning; a producer
//!   ignores a schema it does not know rather than guessing.
//! * A file that is unreadable, larger than [`MAX_POLICY_BYTES`], not TOML, or that
//!   gives a known key a value this reader does not know, is ignored whole — the
//!   caller logs one WARN naming why.
//! * A policy whose `applies_to_producers` range leaves out the running build is
//!   not for it and is ignored.
//!
//! The CUTTER reads the checked-in file strictly ([`HandoffPolicy::parse_for_cut`]):
//! an unknown key there is a typo that would ship as a policy nobody follows, so it
//! refuses the cut before a build number is claimed.

use std::path::Path;

/// The one schema this reader implements.
pub const SCHEMA: i64 = 1;

/// Where the policy sits inside a release `.app`, relative to the bundle root.
/// Under `Contents/`, so the bundle's code signature seals it.
pub const BUNDLE_PATH: &str = "Contents/Resources/aterm-handoff-policy.toml";

/// The checked-in source the release cutter copies, relative to the workspace root.
pub const SOURCE_PATH: &str = "publish/handoff-policy.toml";

/// The largest policy file a reader admits. Schema v1 is four keys; the bound only
/// keeps a hostile or corrupt file from costing a read of arbitrary size.
pub const MAX_POLICY_BYTES: u64 = 16 * 1024;

/// Every key schema v1 defines.
const KNOWN_KEYS: [&str; 4] = [
    "schema",
    "applies_to_producers",
    "carry",
    "park_quiet_gate_at_land",
];

/// Keys no reader implements, refused by the cutter by name.
const RESERVED_KEYS: [&str; 1] = ["seamless"];

/// The highest rung a producer may carry a session's screen at, from most faithful
/// to least. Ordered in that direction — `Full < Visible < Repaint` — so the `max`
/// of two ceilings is the more conservative one, and a ceiling GREATER than the one
/// a capture was taken under asks for less than that capture carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CarryCeiling {
    /// The producer's own ladder, scrollback included.
    Full,
    /// The visible screen and its scalars, never scrollback.
    Visible,
    /// A blank, canonical screen its program redraws.
    Repaint,
}

impl CarryCeiling {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "full" => Some(Self::Full),
            "visible" => Some(Self::Visible),
            "repaint" => Some(Self::Repaint),
            _ => None,
        }
    }

    /// The key's spelling of this value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Visible => "visible",
            Self::Repaint => "repaint",
        }
    }

    /// Whether a park under this ceiling carries ANY of a session's scrollback —
    /// `Full` only. `"visible"` and `"repaint"` promise "never scrollback", and the
    /// producer carries scrollback two ways: inside the screen carry (its bounded
    /// newest lines) and beside it (`aterm-gui`'s history carry, the whole history
    /// in a sidecar file). Both read this one answer, so neither can carry what the
    /// other withholds.
    #[must_use]
    pub const fn carries_scrollback(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// How the automatic lane's `Land` phase treats output queued on a master.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LandQuietGate {
    /// The producer's gate as it ships.
    Strict,
    /// Park over queued output at once.
    Relaxed,
}

impl LandQuietGate {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "strict" => Some(Self::Strict),
            "relaxed" => Some(Self::Relaxed),
            _ => None,
        }
    }

    /// The key's spelling of this value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Relaxed => "relaxed",
        }
    }
}

/// `applies_to_producers`: the OUTGOING builds a policy is for, both ends inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProducerRange {
    pub min_build: u64,
    pub max_build: u64,
}

impl ProducerRange {
    /// Whether `build` is inside the range.
    #[must_use]
    pub const fn contains(self, build: u64) -> bool {
        self.min_build <= build && build <= self.max_build
    }
}

/// One parsed schema-v1 policy. Every field is optional; the default is no policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HandoffPolicy {
    pub applies_to_producers: Option<ProducerRange>,
    pub carry: Option<CarryCeiling>,
    pub park_quiet_gate_at_land: Option<LandQuietGate>,
}

impl HandoffPolicy {
    /// Parse a policy the way a PRODUCER reads one: unknown keys are ignored (a
    /// later release may add keys), and anything this reader cannot interpret is an
    /// `Err` naming why — which the caller treats as no policy at all.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::parse_with(text, false)
    }

    /// Parse the checked-in policy the way the RELEASE CUTTER reads it: everything
    /// [`Self::parse`] refuses, plus any key schema v1 does not define — a typo such
    /// as `cary = "repaint"` would otherwise ship as a policy no producer follows.
    pub fn parse_for_cut(text: &str) -> Result<Self, String> {
        Self::parse_with(text, true)
    }

    fn parse_with(text: &str, strict: bool) -> Result<Self, String> {
        let value: aterm_toml::Value =
            aterm_toml::from_str(text).map_err(|error| format!("not TOML: {error}"))?;
        let table = value
            .as_table()
            .ok_or_else(|| "not a TOML table".to_string())?;
        if strict {
            if let Some(key) = table
                .keys()
                .find(|key| RESERVED_KEYS.contains(&key.as_str()))
            {
                return Err(format!(
                    "`{key}` is reserved: no build implements it, because it would replace the \
                     in-session handover with a relaunch offer"
                ));
            }
            if let Some(key) = table.keys().find(|key| !KNOWN_KEYS.contains(&key.as_str())) {
                return Err(format!(
                    "unknown key `{key}` (schema {SCHEMA} defines {})",
                    KNOWN_KEYS.join(", ")
                ));
            }
        }
        match table.get("schema").map(aterm_toml::Value::as_integer) {
            Some(Some(SCHEMA)) => {}
            Some(Some(other)) => {
                return Err(format!(
                    "schema {other}, and this build reads schema {SCHEMA} only"
                ));
            }
            Some(None) => return Err("`schema` is not an integer".to_string()),
            None => return Err(format!("no `schema = {SCHEMA}` line")),
        }
        let applies_to_producers = match table.get("applies_to_producers") {
            None => None,
            Some(value) => Some(parse_range(value)?),
        };
        let carry =
            match table.get("carry") {
                None => None,
                Some(value) => Some(value.as_str().and_then(CarryCeiling::parse).ok_or_else(
                    || format!("`carry` is {value}, not \"full\", \"visible\" or \"repaint\""),
                )?),
            };
        let park_quiet_gate_at_land = match table.get("park_quiet_gate_at_land") {
            None => None,
            Some(value) => Some(value.as_str().and_then(LandQuietGate::parse).ok_or_else(
                || format!("`park_quiet_gate_at_land` is {value}, not \"strict\" or \"relaxed\""),
            )?),
        };
        Ok(Self {
            applies_to_producers,
            carry,
            park_quiet_gate_at_land,
        })
    }

    /// Whether a producer running `build` must follow this policy: every build when
    /// no range is given, else the builds inside it.
    #[must_use]
    pub fn applies_to(&self, build: u64) -> bool {
        self.applies_to_producers
            .is_none_or(|range| range.contains(build))
    }

    /// Whether the policy asks a producer for anything at all. The checked-in file
    /// is empty by default, and an empty policy is no policy.
    #[must_use]
    pub fn asks_anything(&self) -> bool {
        self.carry.is_some_and(|carry| carry != CarryCeiling::Full) || self.relaxes_land_gate()
    }

    /// The highest rung the producer may carry at: [`CarryCeiling::Full`] (its own
    /// ladder) when the policy does not say.
    #[must_use]
    pub fn carry_ceiling(&self) -> CarryCeiling {
        self.carry.unwrap_or(CarryCeiling::Full)
    }

    /// Whether the `Land` phase may park over queued output at once.
    #[must_use]
    pub fn relaxes_land_gate(&self) -> bool {
        self.park_quiet_gate_at_land == Some(LandQuietGate::Relaxed)
    }
}

impl std::fmt::Display for HandoffPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut said = Vec::new();
        if let Some(range) = self.applies_to_producers {
            said.push(format!(
                "applies_to_producers=[{}, {}]",
                range.min_build, range.max_build
            ));
        }
        if let Some(carry) = self.carry {
            said.push(format!("carry={}", carry.as_str()));
        }
        if let Some(gate) = self.park_quiet_gate_at_land {
            said.push(format!("park_quiet_gate_at_land={}", gate.as_str()));
        }
        if said.is_empty() {
            formatter.write_str("no knobs set")
        } else {
            formatter.write_str(&said.join(" "))
        }
    }
}

fn parse_range(value: &aterm_toml::Value) -> Result<ProducerRange, String> {
    let wrong = || format!("`applies_to_producers` is {value}, not [min_build, max_build]");
    let array = value.as_array().ok_or_else(wrong)?;
    let [min, max] = array.as_slice() else {
        return Err(wrong());
    };
    let build = |value: &aterm_toml::Value| {
        value
            .as_integer()
            .and_then(|build| u64::try_from(build).ok())
    };
    let (Some(min_build), Some(max_build)) = (build(min), build(max)) else {
        return Err(wrong());
    };
    if min_build > max_build {
        return Err(format!(
            "`applies_to_producers` is [{min_build}, {max_build}], an empty range"
        ));
    }
    Ok(ProducerRange {
        min_build,
        max_build,
    })
}

/// What reading a candidate bundle's policy file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyRead {
    /// The bundle carries no policy file — every release before this one.
    Absent,
    /// A schema-v1 policy.
    Parsed(HandoffPolicy),
    /// A file that is there but cannot be followed, and why. Ignored: the producer
    /// behaves exactly as with no file.
    Ignored(String),
    /// The read never reached the file's bytes — the system refused it FOR NOW (no
    /// descriptor left, no memory, an I/O error, an interrupted call) — and why.
    ///
    /// NOT A VERDICT ON THE FILE (round seven, H1 finding 57). Filed as
    /// [`Self::Ignored`], a moment of descriptor exhaustion was cached with the
    /// candidate's verified pass as "the successor asks nothing", and for the pass's
    /// freshness window every park ran the very capture the release had sealed a
    /// policy to route around. A reader treats this as "unknown": the pre-verify
    /// refuses as a passing condition, and the next attempt reads the file again.
    Unread(String),
}

/// Read the policy of the bundle rooted at `app_root`.
///
/// ONLY EVER CALLED ON A BUNDLE THAT HAS PASSED ITS CODESIGN CHECK — see the module
/// doc for why that is the whole trust argument; `aterm-update`'s pre-verification
/// is the one caller, and it reads after the check and never on a refusal.
///
/// The file must be a regular file (a symlink is never followed) no larger than
/// [`MAX_POLICY_BYTES`], holding UTF-8 TOML.
#[must_use]
pub fn read_from_bundle(app_root: &Path) -> PolicyRead {
    let path = app_root.join(BUNDLE_PATH);
    judge(&path, read_bounded(&path))
}

/// What [`read_bounded`]'s answer for the policy at `path` means.
fn judge(path: &Path, read: Result<Option<String>, ReadFailure>) -> PolicyRead {
    match read {
        Ok(None) => PolicyRead::Absent,
        Ok(Some(text)) => match HandoffPolicy::parse(&text) {
            Ok(policy) => PolicyRead::Parsed(policy),
            Err(why) => PolicyRead::Ignored(format!("{}: {why}", path.display())),
        },
        Err(ReadFailure::File(why)) => PolicyRead::Ignored(format!("{}: {why}", path.display())),
        Err(ReadFailure::Moment(why)) => PolicyRead::Unread(format!("{}: {why}", path.display())),
    }
}

/// Why [`read_bounded`] could not hand back the file's text.
enum ReadFailure {
    /// A fact about the FILE: not a regular file, too large, not UTF-8, or an error
    /// the same file would give again.
    File(String),
    /// A fact about this MOMENT ([`is_momentary`]): the bytes were never reached.
    Moment(String),
}

/// Whether an I/O error says the system could not serve the read just then, not
/// that the file is unreadable: out of descriptors (`EMFILE`, `ENFILE`), out of
/// memory or buffers (`ENOMEM`, `ENOBUFS`), a device error (`EIO`), an interrupted
/// or would-block call (`EINTR`, `EAGAIN`).
fn is_momentary(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::OutOfMemory
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::ENOBUFS | libc::EIO)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// An I/O error of the read, sorted by [`is_momentary`].
fn unreadable(error: &std::io::Error) -> ReadFailure {
    let why = format!("unreadable: {error}");
    if is_momentary(error) {
        ReadFailure::Moment(why)
    } else {
        ReadFailure::File(why)
    }
}

/// `Ok(None)` when there is no file at all; `Err` for one that cannot be read as a
/// small regular UTF-8 file.
///
/// OPENED FIRST, JUDGED BY WHAT WAS OPENED. The path is opened without following a
/// link and WITHOUT BLOCKING, and the descriptor itself must be a regular file
/// within [`MAX_POLICY_BYTES`] before a byte is read. A check of the path followed
/// by an open leaves a window in which a same-user process can rename a FIFO onto
/// it, and a blocking `open(2)` of a FIFO waits for a writer that may never come —
/// with the apply lock held on the staged lane, and every PTY reader parked on the
/// fork lane (the same race `aterm-gui`'s seamless reader closes the same way).
/// A FIFO opened non-blocking returns at once and is refused as not a regular file.
fn read_bounded(path: &Path) -> Result<Option<String>, ReadFailure> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // `O_NOFOLLOW` on a symbolic link: never followed, never read.
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(ReadFailure::File(
                "not a regular file (a symbolic link)".to_string(),
            ));
        }
        Err(error) => return Err(unreadable(&error)),
    };
    let metadata = file.metadata().map_err(|error| unreadable(&error))?;
    if !metadata.file_type().is_file() {
        return Err(ReadFailure::File("not a regular file".to_string()));
    }
    if metadata.len() > MAX_POLICY_BYTES {
        return Err(ReadFailure::File(format!(
            "{} bytes, over the {MAX_POLICY_BYTES}-byte bound",
            metadata.len()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_POLICY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unreadable(&error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_POLICY_BYTES {
        return Err(ReadFailure::File(format!(
            "over the {MAX_POLICY_BYTES}-byte bound"
        )));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ReadFailure::File("not UTF-8".to_string()))
}

/// What a producer running `producer_build` does with what it read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Adoption {
    /// Nothing to follow: no file, or a policy that asks for nothing.
    None,
    /// A policy this producer must follow at its next park.
    Follow(HandoffPolicy),
    /// A policy for other producers; this one ignores it.
    NotForThisBuild(HandoffPolicy),
    /// A file this producer cannot follow, and why.
    Ignored(String),
}

impl Adoption {
    /// Decide, purely, what `read` means for a producer running `producer_build`.
    #[must_use]
    pub fn of(read: &PolicyRead, producer_build: u64) -> Self {
        match read {
            PolicyRead::Absent => Self::None,
            PolicyRead::Ignored(why) => Self::Ignored(why.clone()),
            // Never reached through `aterm-update`'s pre-verifications, which refuse
            // an unread policy as a passing condition before any caller adopts it;
            // total here, and read as the one thing it can be: nothing to follow.
            PolicyRead::Unread(why) => Self::Ignored(format!("not read just then: {why}")),
            PolicyRead::Parsed(policy) if !policy.applies_to(producer_build) => {
                Self::NotForThisBuild(*policy)
            }
            PolicyRead::Parsed(policy) if policy.asks_anything() => Self::Follow(*policy),
            PolicyRead::Parsed(_) => Self::None,
        }
    }

    /// The policy to follow, if any.
    #[must_use]
    pub fn policy(&self) -> Option<HandoffPolicy> {
        match self {
            Self::Follow(policy) => Some(*policy),
            Self::None | Self::NotForThisBuild(_) | Self::Ignored(_) => None,
        }
    }
}

/// [`Adoption::of`], plus the ONE log line it deserves: a WARN for a file that
/// cannot be followed, an INFO for a policy that is followed or is not for this
/// build, nothing when there is nothing to say. `candidate` names the bundle for
/// the log ("staged update build 1790…").
#[must_use]
pub fn adopt(read: &PolicyRead, producer_build: u64, candidate: &str) -> Option<HandoffPolicy> {
    let adoption = Adoption::of(read, producer_build);
    match &adoption {
        Adoption::None => {}
        Adoption::Follow(policy) => aterm_log::info!(
            "update apply: {candidate} carries a handoff policy for this build \
             ({producer_build}): {policy}; the next park follows it"
        ),
        Adoption::NotForThisBuild(policy) => aterm_log::info!(
            "update apply: {candidate} carries a handoff policy for other builds ({policy}); \
             this build ({producer_build}) ignores it"
        ),
        Adoption::Ignored(why) => aterm_log::warn!(
            "update apply: {candidate}'s handoff policy is ignored ({why}); the park runs as \
             this build ships"
        ),
    }
    adoption.policy()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str) -> HandoffPolicy {
        HandoffPolicy::parse(text).unwrap_or_else(|why| panic!("{text:?}: {why}"))
    }

    #[test]
    fn every_v1_key_parses_and_every_key_is_optional() {
        assert_eq!(policy("schema = 1\n"), HandoffPolicy::default());
        assert_eq!(
            policy(
                "schema = 1\napplies_to_producers = [1790000000, 1790999999]\ncarry = \
                 \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n"
            ),
            HandoffPolicy {
                applies_to_producers: Some(ProducerRange {
                    min_build: 1_790_000_000,
                    max_build: 1_790_999_999,
                }),
                carry: Some(CarryCeiling::Repaint),
                park_quiet_gate_at_land: Some(LandQuietGate::Relaxed),
            }
        );
        for (spelling, ceiling) in [
            ("full", CarryCeiling::Full),
            ("visible", CarryCeiling::Visible),
            ("repaint", CarryCeiling::Repaint),
        ] {
            assert_eq!(
                policy(&format!("schema = 1\ncarry = \"{spelling}\"\n")).carry_ceiling(),
                ceiling
            );
        }
        assert!(
            !policy("schema = 1\npark_quiet_gate_at_land = \"strict\"\n").relaxes_land_gate(),
            "strict is the gate as it ships"
        );
    }

    /// "visible" and "repaint" carry NO scrollback — the one reading both of the
    /// producer's scrollback carries take — and the order runs from most faithful
    /// to least, so a greater ceiling asks for less (what the fork lane's check of
    /// a capture against a policy read after it compares).
    #[test]
    fn only_a_full_ceiling_carries_scrollback_and_greater_is_more_conservative() {
        assert!(CarryCeiling::Full.carries_scrollback());
        assert!(!CarryCeiling::Visible.carries_scrollback());
        assert!(!CarryCeiling::Repaint.carries_scrollback());
        assert!(CarryCeiling::Full < CarryCeiling::Visible);
        assert!(CarryCeiling::Visible < CarryCeiling::Repaint);
        assert_eq!(
            CarryCeiling::Full.max(CarryCeiling::Repaint),
            CarryCeiling::Repaint,
            "the max of two ceilings is the more conservative"
        );
    }

    /// A later release may ADD keys without a schema bump, so a producer ignores
    /// what it does not know — including the reserved `seamless`, which no build
    /// implements. The cutter, reading our own file, refuses both.
    #[test]
    fn unknown_keys_are_ignored_by_a_producer_and_refused_by_the_cutter() {
        let text = "schema = 1\ncarry = \"visible\"\nsome_future_knob = 3\n[future]\nx = 1\n";
        assert_eq!(policy(text).carry, Some(CarryCeiling::Visible));
        let refused = HandoffPolicy::parse_for_cut(text).expect_err("a typo refuses the cut");
        assert!(refused.contains("unknown key"), "{refused}");
        let reserved = "schema = 1\nseamless = false\n";
        assert_eq!(policy(reserved), HandoffPolicy::default());
        let refused = HandoffPolicy::parse_for_cut(reserved).expect_err("reserved");
        assert!(refused.contains("`seamless` is reserved"), "{refused}");
        assert_eq!(
            HandoffPolicy::parse_for_cut("schema = 1\ncarry = \"repaint\"\n")
                .expect("a v1 policy passes the cutter")
                .carry,
            Some(CarryCeiling::Repaint)
        );
    }

    /// Anything a v1 reader cannot interpret is the whole file ignored — never a
    /// guess, and never a stricter park than the build ships with.
    #[test]
    fn a_malformed_policy_is_refused_whole_and_says_why() {
        for (text, why) in [
            ("carry = \"repaint\"\n", "no `schema = 1` line"),
            ("schema = 2\ncarry = \"repaint\"\n", "schema 2"),
            ("schema = \"1\"\n", "not an integer"),
            ("schema = 1\ncarry = \"sideways\"\n", "`carry`"),
            ("schema = 1\ncarry = 3\n", "`carry`"),
            (
                "schema = 1\npark_quiet_gate_at_land = \"loose\"\n",
                "park_quiet",
            ),
            (
                "schema = 1\napplies_to_producers = [5]\n",
                "applies_to_producers",
            ),
            (
                "schema = 1\napplies_to_producers = [1, 2, 3]\n",
                "applies_to",
            ),
            ("schema = 1\napplies_to_producers = [-1, 2]\n", "applies_to"),
            (
                "schema = 1\napplies_to_producers = [\"a\", 2]\n",
                "applies_to",
            ),
            ("schema = 1\napplies_to_producers = [9, 2]\n", "empty range"),
            ("schema = 1\napplies_to_producers = 7\n", "applies_to"),
            ("schema = 1\ncarry = \"repaint\n", "not TOML"),
        ] {
            let refused = HandoffPolicy::parse(text).expect_err(text);
            assert!(refused.contains(why), "{text:?}: {refused}");
        }
    }

    #[test]
    fn the_range_is_inclusive_and_absent_means_every_producer() {
        let ranged = policy("schema = 1\napplies_to_producers = [100, 200]\ncarry = \"visible\"\n");
        for (build, applies) in [
            (0, false),
            (99, false),
            (100, true),
            (150, true),
            (200, true),
            (201, false),
            (u64::MAX, false),
        ] {
            assert_eq!(ranged.applies_to(build), applies, "build {build}");
        }
        let single = policy("schema = 1\napplies_to_producers = [7, 7]\n");
        assert!(single.applies_to(7) && !single.applies_to(6) && !single.applies_to(8));
        let everyone = policy("schema = 1\ncarry = \"visible\"\n");
        assert!(everyone.applies_to(0) && everyone.applies_to(u64::MAX));
    }

    #[test]
    fn a_producer_follows_only_a_policy_that_asks_something_of_its_own_build() {
        let parsed = |text: &str| PolicyRead::Parsed(policy(text));
        let repaint = parsed("schema = 1\napplies_to_producers = [10, 20]\ncarry = \"repaint\"\n");
        assert_eq!(
            Adoption::of(&repaint, 15)
                .policy()
                .map(|p| p.carry_ceiling()),
            Some(CarryCeiling::Repaint)
        );
        assert!(matches!(
            Adoption::of(&repaint, 21),
            Adoption::NotForThisBuild(_)
        ));
        assert_eq!(Adoption::of(&repaint, 9).policy(), None);
        // An empty policy — the checked-in default — and a policy that asks for
        // the build's own behaviour are nothing to follow.
        for nothing in [
            "schema = 1\n",
            "schema = 1\ncarry = \"full\"\npark_quiet_gate_at_land = \"strict\"\n",
        ] {
            assert_eq!(
                Adoption::of(&parsed(nothing), 15),
                Adoption::None,
                "{nothing}"
            );
        }
        assert_eq!(Adoption::of(&PolicyRead::Absent, 15), Adoption::None);
        assert_eq!(
            Adoption::of(&PolicyRead::Ignored("bad".to_string()), 15),
            Adoption::Ignored("bad".to_string())
        );
        let relaxed = parsed("schema = 1\npark_quiet_gate_at_land = \"relaxed\"\n");
        assert!(
            Adoption::of(&relaxed, 15)
                .policy()
                .is_some_and(|p| p.relaxes_land_gate() && p.carry_ceiling() == CarryCeiling::Full)
        );
    }

    /// A scratch `.app` root with the policy file written as `write` says.
    fn scratch_bundle(label: &str, write: impl FnOnce(&Path)) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aterm-handoff-policy-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let resources = root.join("Contents/Resources");
        std::fs::create_dir_all(&resources).expect("scratch bundle");
        write(&root.join(BUNDLE_PATH));
        root
    }

    #[test]
    fn a_bundle_policy_is_read_absent_or_ignored_never_followed_through_a_link() {
        let absent = scratch_bundle("absent", |_| {});
        assert_eq!(read_from_bundle(&absent), PolicyRead::Absent);

        let parsed = scratch_bundle("parsed", |path| {
            std::fs::write(path, "schema = 1\ncarry = \"visible\"\n").expect("write")
        });
        assert_eq!(
            read_from_bundle(&parsed),
            PolicyRead::Parsed(HandoffPolicy {
                carry: Some(CarryCeiling::Visible),
                ..HandoffPolicy::default()
            })
        );

        let malformed = scratch_bundle("malformed", |path| {
            std::fs::write(path, "schema = 1\ncarry = \"sideways\"\n").expect("write")
        });
        assert!(
            matches!(read_from_bundle(&malformed), PolicyRead::Ignored(why) if why.contains("carry"))
        );

        let huge = scratch_bundle("huge", |path| {
            let mut text = "schema = 1\n".to_string();
            text.push_str(&"# padding\n".repeat(2048));
            std::fs::write(path, text).expect("write")
        });
        assert!(
            matches!(read_from_bundle(&huge), PolicyRead::Ignored(why) if why.contains("bound"))
        );

        let binary = scratch_bundle("binary", |path| {
            std::fs::write(path, [0xff, 0xfe, 0x00]).expect("write")
        });
        assert!(
            matches!(read_from_bundle(&binary), PolicyRead::Ignored(why) if why.contains("UTF-8"))
        );

        let directory = scratch_bundle("directory", |path| {
            std::fs::create_dir_all(path).expect("mkdir")
        });
        assert!(
            matches!(read_from_bundle(&directory), PolicyRead::Ignored(why) if why.contains("regular"))
        );

        #[cfg(unix)]
        {
            let elsewhere = std::env::temp_dir().join(format!(
                "aterm-handoff-policy-target-{}.toml",
                std::process::id()
            ));
            std::fs::write(&elsewhere, "schema = 1\ncarry = \"repaint\"\n").expect("write");
            let linked = scratch_bundle("linked", |path| {
                std::os::unix::fs::symlink(&elsewhere, path).expect("symlink")
            });
            assert!(
                matches!(read_from_bundle(&linked), PolicyRead::Ignored(why) if why.contains("regular")),
                "a link out of the bundle is never followed"
            );
            let _ = std::fs::remove_file(&elsewhere);
            let _ = std::fs::remove_dir_all(&linked);
        }
        for root in [absent, parsed, malformed, huge, binary, directory] {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// A READ THE SYSTEM REFUSED FOR NOW IS UNREAD, NOT IGNORED (round seven, H1
    /// finding 57). Out of descriptors (a launchd-started window's soft limit of 256
    /// on a large desk), out of memory, an I/O error: the file's bytes were never
    /// reached, so nothing is known about what it asks. Filed as `Ignored` — the
    /// verdict for a malformed file — it was cached as "the successor asks nothing"
    /// and every park for ten minutes ran the capture the policy was sealed to avoid.
    /// An error the same file would give again stays a verdict on the file.
    #[cfg(unix)]
    #[test]
    fn a_read_the_system_refused_for_now_is_unread_and_a_file_error_is_ignored() {
        for errno in [
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOMEM,
            libc::ENOBUFS,
            libc::EIO,
            libc::EINTR,
            libc::EAGAIN,
        ] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                matches!(unreadable(&error), ReadFailure::Moment(why) if why.contains("unreadable")),
                "errno {errno} ({error}) is a moment"
            );
        }
        for errno in [libc::EACCES, libc::EPERM, libc::EISDIR, libc::ENOTDIR] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                matches!(unreadable(&error), ReadFailure::File(_)),
                "errno {errno} ({error}) is a fact about the file"
            );
        }
        // And what each becomes, read off a bundle.
        let at = std::path::Path::new("/x/Contents/Resources/aterm-handoff-policy.toml");
        let unread = judge(
            at,
            Err(unreadable(&std::io::Error::from_raw_os_error(libc::EMFILE))),
        );
        assert!(matches!(
            judge(
                at,
                Err(unreadable(&std::io::Error::from_raw_os_error(libc::EACCES)))
            ),
            PolicyRead::Ignored(_)
        ));
        assert!(matches!(&unread, PolicyRead::Unread(why) if why.contains("Too many open files")));
        // Adoption stays total: nothing to follow, never a policy made up.
        assert_eq!(Adoption::of(&unread, 1).policy(), None);
    }

    /// A FIFO WHERE THE POLICY SHOULD BE IS REFUSED AT ONCE, NEVER WAITED ON. The
    /// reader runs with the apply lock held (the staged lane) or with every PTY
    /// reader parked (the fork lane), and a blocking `open(2)` of a FIFO waits for
    /// a writer that never comes. The file is opened first and judged by what was
    /// opened, so this is exactly the open a FIFO renamed onto the path between a
    /// check and an open would meet. RED with a blocking open: the reader parks in
    /// `open` and the ten-second `recv_timeout` below gives up.
    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_the_policy_is_refused_without_waiting_for_a_writer() {
        let fifo = scratch_bundle("fifo", |path| {
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .expect("no NUL in a temp path");
            // SAFETY: `name` is a valid NUL-terminated path for the call.
            let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
            assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let root = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_from_bundle(&root));
        });
        let read = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the reader must refuse a FIFO at once, not wait for a writer");
        assert!(
            matches!(&read, PolicyRead::Ignored(why) if why.contains("regular")),
            "{read:?}"
        );
        let _ = std::fs::remove_dir_all(fifo);
    }
}
