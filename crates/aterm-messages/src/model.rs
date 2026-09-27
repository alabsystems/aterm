// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The message model: ids, severities, the closed tag and glyph sets, the
//! plain-data intents a host performs, holds, meters and the [`Message`]
//! builder every reporter posts through. Everything here is data: no clock,
//! no cells, no IO.

use std::borrow::Cow;
use std::fmt;

use crate::glass::Fnv;
use crate::text::clip;
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Duration, HOLD_ERROR, HOLD_INFO, HOLD_SUCCESS, HOLD_WARN,
    KEY_CAP, MAX_ACTIONS, STATS_CAP, TITLE_CAP,
};

/// Process-monotonic, never 0, rising across launches (seeded from the loaded
/// log) and across the seamless handoff (the carry ships `next_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u64);

impl MessageId {
    /// The first id a fresh log mints.
    pub const FIRST: MessageId = MessageId(1);

    /// The raw number, for the wire and the log.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// An id from its raw number; `None` for 0, which no message ever has.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// The id after this one (saturating at `u64::MAX`, which no process
    /// reaches).
    #[must_use]
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// FOUR severities. `Ord` is glass rank (Error first). "Notice" is a HOLD, not
/// a severity: an Info row with [`Hold::Default`] holds [`HOLD_INFO`] (30 s,
/// the old `appnotice` row).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Good news: an install finished, an update applied.
    Success,
    /// A fact worth a glance: a notice, a live meter, a question.
    Info,
    /// Something is wrong and a person may want to act.
    Warn,
    /// Something failed: a crash last time, a lost GPU.
    Error,
}

impl Severity {
    /// The wire word: `success` | `info` | `warn` | `error`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// The severity for a wire word; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "success" => Some(Self::Success),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// The hold a [`Hold::Default`] row of this severity takes: 8 s / 30 s /
    /// 45 s / 60 s.
    #[must_use]
    pub const fn default_hold(self) -> Duration {
        match self {
            Self::Success => HOLD_SUCCESS,
            Self::Info => HOLD_INFO,
            Self::Warn => HOLD_WARN,
            Self::Error => HOLD_ERROR,
        }
    }

    /// The glyph a row of this severity carries unless the reporter picks
    /// another: ✓ ℹ ⚠ ✕.
    #[must_use]
    pub const fn default_glyph(self) -> Glyph {
        match self {
            Self::Success => Glyph('\u{2713}'),
            Self::Info => Glyph('\u{2139}'),
            Self::Warn => Glyph('\u{26a0}'),
            Self::Error => Glyph('\u{2715}'),
        }
    }

    /// Every severity, in rank order.
    pub const ALL: [Severity; 4] = [Self::Success, Self::Info, Self::Warn, Self::Error];
}

/// Why a string is not a [`Tag`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagError {
    /// The empty string.
    Empty,
    /// More than 24 characters.
    TooLong,
    /// A character outside `[a-z0-9-]`.
    BadChar(char),
}

impl fmt::Display for TagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty tag"),
            Self::TooLong => f.write_str("tag longer than 24 characters"),
            Self::BadChar(c) => write!(f, "tag character {c:?} outside [a-z0-9-]"),
        }
    }
}

impl std::error::Error for TagError {}

/// A lowercase `[a-z0-9-]{1,24}` word naming the reporter family. Constants
/// for every reporter live in [`tags`]; [`Tag::try_new`] admits the wire's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(Cow<'static, str>);

impl Tag {
    /// The longest tag admitted.
    pub(crate) const MAX_LEN: usize = 24;

    /// Validate a wire word into a tag.
    ///
    /// # Errors
    /// [`TagError`] names the first reason the word is not a tag.
    pub(crate) fn try_new(s: &str) -> Result<Self, TagError> {
        if s.is_empty() {
            return Err(TagError::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(TagError::TooLong);
        }
        if let Some(bad) = s
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
        {
            return Err(TagError::BadChar(bad));
        }
        Ok(Self(Cow::Owned(s.to_string())))
    }

    /// The word.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A vocabulary constant; validated by the closed-set test.
    const fn word(s: &'static str) -> Self {
        Self(Cow::Borrowed(s))
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The reporter vocabulary: one constant per family, and [`tags::ALL`].
pub mod tags {
    use super::Tag;

    /// `aterm.toml` and keybinding diagnostics.
    pub const CONFIG: Tag = Tag::word("config");
    /// The previous process ended badly.
    pub const CRASH: Tag = Tag::word("crash");
    /// The `ALab` toolchain install (`atpkg`).
    pub const TOOLCHAIN: Tag = Tag::word("toolchain");
    /// aterm's own self-update.
    pub const UPDATE: Tag = Tag::word("update");
    /// Managed programs and machine settings.
    pub const PACKAGES: Tag = Tag::word("packages");
    /// File access, consent and TCC.
    pub const PRIVACY: Tag = Tag::word("privacy");
    /// Sessions, restore and the seamless handoff.
    pub const SESSION: Tag = Tag::word("session");
    /// Window and presence facts.
    pub const WINDOW: Tag = Tag::word("window");
    /// The GPU and the raster path.
    pub const RENDER: Tag = Tag::word("render");
    /// The accessibility publisher.
    pub const A11Y: Tag = Tag::word("a11y");
    /// The fabric and peer messaging.
    pub const FABRIC: Tag = Tag::word("fabric");
    /// An agent harness's notes (`appnotice harness`): records only, never a
    /// row (design ruling 39, implemented by ruling 61).
    pub const HARNESS: Tag = Tag::word("harness");
    /// Everything the host reports about itself.
    pub const SYSTEM: Tag = Tag::word("system");
    /// A script's `notice progress` row that named no tag (design ruling
    /// 265): a script's work is not aterm's own `system`.
    pub const SCRIPT: Tag = Tag::word("script");

    /// The fourteen, in reporter order.
    pub const ALL: &[&Tag] = &[
        &CONFIG, &CRASH, &TOOLCHAIN, &UPDATE, &PACKAGES, &PRIVACY, &SESSION, &WINDOW, &RENDER,
        &A11Y, &FABRIC, &HARNESS, &SYSTEM, &SCRIPT,
    ];
}

/// One text-presentation glyph from the band's CLOSED set. `⚙` is OUTSIDE it
/// on purpose: it renders BLANK in the headless CPU font
/// (status_bars.rs:1626-1628). A capture test in the host asserts each
/// admitted glyph paints a non-blank cell headless; [`Glyph::FALLBACK`] (`!`)
/// stands in for anything a reporter tries outside the set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Glyph(char);

impl Glyph {
    /// The admitted glyphs.
    pub const ALLOWED: &'static [char] = &[
        '\u{2139}', // ℹ
        '\u{2713}', // ✓
        '\u{26a0}', // ⚠
        '\u{2715}', // ✕
        '\u{21e3}', // ⇣
        '\u{21bb}', // ↻
        '\u{2191}', // ↑
        '\u{23f8}', // ⏸
        '\u{2726}', // ✦
        '!', '\u{00b7}', // ·
        '\u{2026}', // …
    ];
    /// What stands in for a glyph outside the set.
    pub const FALLBACK: Glyph = Glyph('!');

    /// A glyph from the closed set; `None` outside it.
    #[must_use]
    pub fn new(ch: char) -> Option<Self> {
        Self::ALLOWED.contains(&ch).then_some(Self(ch))
    }

    /// The glyph, or [`Glyph::FALLBACK`] when `ch` is outside the set.
    #[must_use]
    pub fn or_fallback(ch: char) -> Self {
        Self::new(ch).unwrap_or(Self::FALLBACK)
    }

    /// The character.
    #[must_use]
    pub const fn ch(self) -> char {
        self.0
    }
}

/// What a `Not now` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The Full Disk Access question. (The elevated-install question went with the
    /// OS-installer protocols, 2026-09-24; a carried `not-now:admin-step:` decodes to
    /// nothing and the loader drops it.)
    FileAccess,
}

/// THE OWNER'S WORD on a live agent upgrade, pressed on its row: the three
/// words `aterm harness upgrade <tab> --now|--defer|--skip` writes, as the
/// band says them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeWord {
    /// Move the agent at its next turn end (`--now`).
    Now,
    /// Not for a day (`--defer`).
    NotToday,
    /// Stay on the running build until a newer one than this target comes
    /// (`--skip`).
    Skip,
}

impl UpgradeWord {
    /// Every word, in the order a row or a menu offers them.
    pub const ALL: [UpgradeWord; 3] = [Self::Now, Self::NotToday, Self::Skip];

    /// The codec's word: `now`, `not-today`, `skip`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::NotToday => "not-today",
            Self::Skip => "skip",
        }
    }

    /// [`Self::as_str`] read back.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.as_str() == word)
    }

    /// The full label — the capsule's, the tab menu's and a screen reader's.
    /// "Skip version", not "Skip this version": a label is at most 16 chars
    /// (invariant 23).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Now => "Upgrade now",
            Self::NotToday => "Not today",
            Self::Skip => "Skip version",
        }
    }
}

/// Plain-data INTENTS the HOST performs (`ATERM_DESIGN` §2.2: the engine
/// REQUESTS, the frontend PERFORMS). Every variant is codec-able (strings and
/// ints only) so the log can re-offer it from Settings ▸ Messages. Labels are
/// DERIVED from the intent — one table, [`Intent::label`] / [`Intent::short`]
/// — so the reporter table and the label table cannot disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// The implicit last capsule; dropped if a reporter authors it.
    Details,
    /// Open a Settings route (`/packages`, `/updates`, `/manual`,
    /// `/appearance`, `/messages`, …).
    OpenSettings {
        /// A `SettingsRoute::path()` string.
        route: String,
    },
    /// Open `aterm.toml` in the editor, at a line when the report has one.
    OpenConfigEditor {
        /// `None` for launch warnings, which carry no line.
        line: Option<u32>,
    },
    /// Open a host-minted path under the log directory; the host re-validates
    /// before opening.
    OpenPath {
        /// The absolute path.
        path: String,
    },
    /// Open a system settings pane (`full-disk-access`); macOS performs,
    /// others refuse honestly.
    OpenSystemPane {
        /// The pane's name.
        pane: String,
    },
    /// Install a staged update now.
    ApplyUpdate {
        /// The staged build number.
        build: u64,
    },
    /// Decline a question for now.
    NotNow {
        /// Which question.
        decision: Decision,
    },
    /// Open a new window (the GPU-lost remedy).
    NewWindow,
    /// Drop the rest of a large paste still on its way to a session's
    /// program (design ruling 231): the part the program already took stays,
    /// a bracketed paste is closed, and input typed after the paste goes
    /// through. The row's own work, so the one decision a progress row
    /// carries.
    StopPaste {
        /// The session the paste is going to.
        session: u64,
    },
    /// Show tab `tab` (1-based) of window `window`, raising it: the strain
    /// row's navigation to the program it names (design ruling 243). The
    /// band is app-wide, so the row names the window the tab is in and a
    /// press from any window's band lands on that one (ruling 247). A
    /// navigation, never consequential.
    ///
    /// scope-waiver: "app-wide" describes where this addressed navigation
    /// may be shown. Each intent carries its destination window and tab;
    /// duplicating the data neither multiplies an enforced budget nor changes
    /// that destination. This variant owns no scope-enforcing state.
    ShowTab {
        /// The tab's number in its window, 1-based.
        tab: u16,
        /// The host's id of the window the tab is in.
        window: u64,
    },
    /// THE OWNER'S WORD on the live agent upgrade in tab `tab` — the row's
    /// press of what `aterm harness upgrade <tab> --now|--defer|--skip`
    /// writes, written by the host in-process, on the same path. `to` is
    /// the build the row was drawn for: the host writes the word only while
    /// the tab's upgrade still moves to it, so a press on a row that moved
    /// on lands on nothing. Not [`Intent::ends_with_row`]: a record keeps
    /// it, and Settings ▸ Messages offers it while that upgrade stands.
    AgentUpgrade {
        /// The tab the agent runs in (`s-<hex>`).
        tab: String,
        /// The target build the row named.
        to: String,
        /// Which word.
        word: UpgradeWord,
    },
}

/// `Show tab N` for tabs 1–9, the only numbers a tab strip shows by number
/// (labels are static: the reader and the capsule share one table).
const SHOW_TAB: [&str; 10] = [
    "Show tab",
    "Show tab 1",
    "Show tab 2",
    "Show tab 3",
    "Show tab 4",
    "Show tab 5",
    "Show tab 6",
    "Show tab 7",
    "Show tab 8",
    "Show tab 9",
];

/// The short form: `Tab N`.
const TAB_N: [&str; 10] = [
    "Tab", "Tab 1", "Tab 2", "Tab 3", "Tab 4", "Tab 5", "Tab 6", "Tab 7", "Tab 8", "Tab 9",
];

impl Intent {
    /// The full capsule label — the words a screen reader says.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Details => "Details \u{203a}",
            Self::OpenSettings { route } => match route.as_str() {
                "/packages" => "Packages",
                "/updates" => "Software Update",
                "/manual" => "Manual",
                "/appearance" => "Appearance",
                "/messages" => "Messages",
                _ => "Settings",
            },
            Self::OpenConfigEditor { .. } => "Open aterm.toml",
            Self::OpenPath { .. } => "Open log",
            Self::OpenSystemPane { .. } => "Open Settings",
            Self::ApplyUpdate { .. } => "Install now",
            Self::NotNow { .. } => "Not now",
            Self::NewWindow => "New window",
            Self::StopPaste { .. } => "Stop paste",
            Self::ShowTab { tab, .. } => SHOW_TAB[usize::from(*tab).min(10) % 10],
            Self::AgentUpgrade { word, .. } => word.label(),
        }
    }

    /// The short capsule form the width law falls back to.
    #[must_use]
    pub(crate) fn short(&self) -> &'static str {
        match self {
            Self::Details => "\u{203a}",
            Self::OpenSettings { route } => match route.as_str() {
                "/packages" => "Packages",
                "/updates" => "Updates",
                "/manual" => "Manual",
                "/appearance" => "Look",
                "/messages" => "Messages",
                _ => "Settings",
            },
            Self::OpenConfigEditor { .. } => "Edit",
            Self::OpenPath { .. } => "Log",
            Self::OpenSystemPane { .. } => "Settings",
            // One install verb on every surface (ruling 69).
            Self::ApplyUpdate { .. } => "Install",
            Self::NotNow { .. } => "Not now",
            Self::NewWindow => "Window",
            Self::StopPaste { .. } => "Stop",
            Self::ShowTab { tab, .. } => TAB_N[usize::from(*tab).min(10) % 10],
            Self::AgentUpgrade { word, .. } => match word {
                UpgradeWord::Now => "Now",
                // A day off, in the eight cells a short form has.
                UpgradeWord::NotToday => "Tomorrow",
                UpgradeWord::Skip => "Skip",
            },
        }
    }

    /// ONLY `NotNow` closes its row: every other intent leaves the row for a
    /// supersede or a fold (the R16 rule, design §1.4).
    #[must_use]
    pub(crate) const fn closes_row(&self) -> bool {
        matches!(self, Self::NotNow { .. })
    }

    /// An intent that acts on the LIVE row itself — a decision, a stop, the
    /// tab a strain row named — and so ends with it: a retired record never
    /// offers it again (design ruling 265; the wire's `messages` rows too,
    /// ruling 267).
    #[must_use]
    pub const fn ends_with_row(&self) -> bool {
        matches!(
            self,
            Self::NotNow { .. } | Self::StopPaste { .. } | Self::ShowTab { .. }
        )
    }

    /// `NotNow`: a row carrying one is a DECISION row.
    #[must_use]
    pub const fn is_ask(&self) -> bool {
        matches!(self, Self::NotNow { .. })
    }

    /// A CONSEQUENTIAL intent — a press that changes the machine or the
    /// window rather than opening a page: `ApplyUpdate`, `OpenSystemPane`,
    /// `NewWindow`, `StopPaste` (it drops bytes; ruling 231), and an agent
    /// upgrade's `Upgrade now` (the agent is ended and resumed on the newer
    /// build at its next turn end). The glass paints these, and only
    /// these, as the accent-filled Primary chip; a navigation
    /// (`OpenSettings`, `OpenPath`, `OpenConfigEditor`) or a decline
    /// (`NotNow`, an upgrade's `Not today` and `Skip version`) is the quiet
    /// Secondary chip, so a toolchain row's
    /// `Packages` no longer shouts and the launch pass never stacks two
    /// accent chips (Phase 1 review ruling 18, 2026-09-22). `Details` is
    /// neither: it wears its own role.
    #[must_use]
    pub const fn is_consequential(&self) -> bool {
        matches!(
            self,
            Self::ApplyUpdate { .. }
                | Self::OpenSystemPane { .. }
                | Self::NewWindow
                | Self::StopPaste { .. }
                | Self::AgentUpgrade {
                    word: UpgradeWord::Now,
                    ..
                }
        )
    }

    /// A press that STOPS the row's own work in flight (`StopPaste`, design
    /// ruling 232): the one decision a progress row may carry. The glass
    /// rule ([`Intent::is_consequential`]) paints it as the accent chip;
    /// the attention rule keeps the row Progress.
    #[must_use]
    pub const fn stops_work(&self) -> bool {
        matches!(self, Self::StopPaste { .. })
    }

    /// The log/carry form: `details`, `open-settings:/packages`,
    /// `open-config-editor:<line|->`, `open-path:<esc>`,
    /// `open-system-pane:<pane>`, `apply-update:<build>`, `not-now:file-access`,
    /// `new-window`, `agent-upgrade:<word>:<to>:<tab>` (`:` escaped in both).
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::Details => "details".to_string(),
            Self::OpenSettings { route } => format!("open-settings:{}", escape_payload(route)),
            Self::OpenConfigEditor { line } => match line {
                Some(n) => format!("open-config-editor:{n}"),
                None => "open-config-editor:-".to_string(),
            },
            Self::OpenPath { path } => format!("open-path:{}", escape_payload(path)),
            Self::OpenSystemPane { pane } => format!("open-system-pane:{}", escape_payload(pane)),
            Self::ApplyUpdate { build } => format!("apply-update:{build}"),
            Self::NotNow { decision } => match decision {
                Decision::FileAccess => "not-now:file-access".to_string(),
            },
            Self::NewWindow => "new-window".to_string(),
            Self::StopPaste { session } => format!("stop-paste:{session}"),
            Self::ShowTab { tab, window } => format!("show-tab:{tab}:{window}"),
            Self::AgentUpgrade { tab, to, word } => format!(
                "agent-upgrade:{}:{}:{}",
                word.as_str(),
                escape_field(to),
                escape_field(tab)
            ),
        }
    }

    /// The inverse of [`Intent::encode`]; unknown or malformed ⇒ `None` (the
    /// loader drops it).
    #[must_use]
    pub fn decode(s: &str) -> Option<Intent> {
        let (kind, payload) = s.split_once(':').unwrap_or((s, ""));
        match kind {
            "details" if payload.is_empty() => Some(Self::Details),
            "new-window" if payload.is_empty() => Some(Self::NewWindow),
            "open-settings" => Some(Self::OpenSettings {
                route: unescape_payload(payload),
            }),
            "open-config-editor" => {
                let line = if payload == "-" {
                    None
                } else {
                    Some(payload.parse::<u32>().ok()?)
                };
                Some(Self::OpenConfigEditor { line })
            }
            "open-path" => Some(Self::OpenPath {
                path: unescape_payload(payload),
            }),
            "open-system-pane" => Some(Self::OpenSystemPane {
                pane: unescape_payload(payload),
            }),
            "apply-update" => Some(Self::ApplyUpdate {
                build: payload.parse().ok()?,
            }),
            "stop-paste" => Some(Self::StopPaste {
                session: payload.parse().ok()?,
            }),
            "show-tab" => {
                let (tab, window) = payload.split_once(':')?;
                Some(Self::ShowTab {
                    tab: tab.parse().ok().filter(|t| *t > 0)?,
                    window: window.parse().ok()?,
                })
            }
            "not-now" => match payload.split_once(':').unwrap_or((payload, "")) {
                ("file-access", "") => Some(Self::NotNow {
                    decision: Decision::FileAccess,
                }),
                _ => None,
            },
            "agent-upgrade" => {
                let (word, rest) = payload.split_once(':')?;
                let (to, tab) = rest.split_once(':')?;
                let (to, tab) = (unescape_payload(to), unescape_payload(tab));
                (!to.is_empty() && !tab.is_empty()).then_some(())?;
                Some(Self::AgentUpgrade {
                    tab,
                    to,
                    word: UpgradeWord::parse(word)?,
                })
            }
            _ => None,
        }
    }
}

/// [`escape_payload`] with the codec's own separator escaped too: a field
/// between two `:`s.
fn escape_field(s: &str) -> String {
    escape_payload(s).replace(':', "%3A")
}

/// Every intent variant once, with a representative payload — the label
/// table's test walks it, and so does the codec's.
#[must_use]
#[cfg(test)]
pub(crate) fn every_intent() -> Vec<Intent> {
    vec![
        Intent::Details,
        Intent::OpenSettings {
            route: "/packages".into(),
        },
        Intent::OpenSettings {
            route: "/updates".into(),
        },
        Intent::OpenSettings {
            route: "/manual".into(),
        },
        Intent::OpenSettings {
            route: "/appearance".into(),
        },
        Intent::OpenSettings {
            route: "/messages".into(),
        },
        Intent::OpenSettings {
            route: "/other".into(),
        },
        Intent::OpenConfigEditor { line: Some(7) },
        Intent::OpenConfigEditor { line: None },
        Intent::OpenPath {
            path: "/tmp/x.log".into(),
        },
        Intent::OpenSystemPane {
            pane: "full-disk-access".into(),
        },
        Intent::ApplyUpdate { build: 1234 },
        Intent::NotNow {
            decision: Decision::FileAccess,
        },
        Intent::NewWindow,
        Intent::StopPaste { session: 3 },
        Intent::ShowTab { tab: 2, window: 7 },
        Intent::AgentUpgrade {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            to: "2.1.282".into(),
            word: UpgradeWord::Now,
        },
        Intent::AgentUpgrade {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            to: "0.157.1".into(),
            word: UpgradeWord::NotToday,
        },
        // A hostile field: the codec's separator and a control character.
        Intent::AgentUpgrade {
            tab: "s-a:b".into(),
            to: "2.1.282:%\t".into(),
            word: UpgradeWord::Skip,
        },
    ]
}

/// `%` and every control character as `%XX`, so a payload can never carry
/// the codec's separators; everything else verbatim (a path stays readable).
fn escape_payload(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c.is_control() {
            let b = u32::from(c);
            if b < 0x100 {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
            // A control character outside Latin-1 has no place in a path;
            // it is dropped rather than encoded.
        } else {
            out.push(c);
        }
    }
    out
}

/// The inverse of [`escape_payload`]; a malformed `%` passes through.
fn unescape_payload(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let mut probe = chars.clone();
            let hi = probe.next().and_then(|h| h.to_digit(16));
            let lo = probe.next().and_then(|l| l.to_digit(16));
            if let (Some(hi), Some(lo)) = (hi, lo)
                && let Some(decoded) = char::from_u32(hi * 16 + lo)
            {
                out.push(decoded);
                chars = probe;
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Which capsule a press landed on: an authored intent by index, or the
/// implicit `Details ›`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionIndex(pub u8);

impl ActionIndex {
    /// The implicit `Details ›` capsule.
    pub const DETAILS: ActionIndex = ActionIndex(255);

    /// `true` for the implicit capsule.
    #[must_use]
    pub const fn is_details(self) -> bool {
        self.0 == Self::DETAILS.0
    }
}

/// How long a row stays and what ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The severity's hold, anchored at FIRST glass
    /// (status_bars.rs:906-917, generalised).
    Default,
    /// A specific hold, anchored the same way.
    For(Duration),
    /// Unfinished: stays until a restate or resolve; folds Stale if silent
    /// past the cap.
    Live {
        /// How long the row may go without a restate.
        stale_after: Duration,
    },
    /// Until resolve, or until read (update health, GPU lost, a11y dead):
    /// Details, a body press or a navigation capsule folds it (design §10.4.3,
    /// E3), because the person has seen it and gone to fix it.
    Standing,
    /// A decision row (Full Disk Access): ranks first; leaves on `NotNow`,
    /// supersede or expiry.
    Ask {
        /// The question's patience.
        for_: Duration,
    },
    /// Recorded, never on glass (managed-current repeats).
    LogOnly,
}

impl Hold {
    /// The wire/carry word: `default`, `for:<secs>`, `live:<secs>`,
    /// `standing`, `ask:<secs>`, `log-only`.
    #[must_use]
    pub fn encode(self) -> String {
        match self {
            Self::Default => "default".to_string(),
            Self::For(d) => format!("for:{}", d.as_secs()),
            Self::Live { stale_after } => format!("live:{}", stale_after.as_secs()),
            Self::Standing => "standing".to_string(),
            Self::Ask { for_ } => format!("ask:{}", for_.as_secs()),
            Self::LogOnly => "log-only".to_string(),
        }
    }

    /// The inverse of [`Hold::encode`]; unknown ⇒ `None`.
    #[must_use]
    pub fn decode(s: &str) -> Option<Self> {
        let (kind, secs) = s.split_once(':').unwrap_or((s, ""));
        let dur = || secs.parse::<u64>().ok().map(Duration::from_secs);
        match kind {
            "default" if secs.is_empty() => Some(Self::Default),
            "standing" if secs.is_empty() => Some(Self::Standing),
            "log-only" if secs.is_empty() => Some(Self::LogOnly),
            "for" => dur().map(Self::For),
            "live" => dur().map(|stale_after| Self::Live { stale_after }),
            "ask" => dur().map(|for_| Self::Ask { for_ }),
            _ => None,
        }
    }

    /// `true` for the holds a fold ends (Default / For / Ask).
    #[must_use]
    pub const fn is_held(self) -> bool {
        matches!(self, Self::Default | Self::For(_) | Self::Ask { .. })
    }
}

/// A row's progress: a fill, a volatile stats string (≤ [`STATS_CAP`]
/// chars, never persisted, never in the spoken label), the continuous
/// quantity behind the fill and the resource the current phase loads — or,
/// with no known fraction, work in flight ([`Meter::busy`]).
///
/// THE INDICATOR IS THE METER'S STATE, never the hold's (design ruling 139,
/// the merge's M4): a fill draws the bar (glide, glint, stall), `busy` the
/// comet, and neither draws no indicator at all — the row
/// is still. A [`Hold::Live`] row that is WORK carries a fill or `busy`
/// (its reporter sets it: [`Message::in_flight`] or [`Meter::busy`]); a
/// Live row blocked on the person carries neither and stands still.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Meter {
    /// 0..=1000, or `None` for work with no fraction yet.
    pub fill_permille: Option<u16>,
    /// `512 MB / 1.2 GB` and the like.
    pub stats: String,
    /// The continuous quantity behind the fill — the ETA's ONLY input. A
    /// reporter supplies it only when `total` spans the deliverable the title
    /// names; a phase of a larger job supplies none, and the row shows no ETA
    /// rather than a wrong one. Volatile: never persisted, never carried.
    pub amount: Option<Amount>,
    /// The resource the current phase loads, DECLARED by the reporter for the
    /// phase's duration, and only for a very heavy phase (design §10.6).
    pub load: Option<Load>,
    /// Work in flight with no known fraction: the comet along the row, on
    /// the engine's frame grid ([`crate::animate`]); the row's glyph cell
    /// keeps its own icon (no spinner beside the comet, ruling 251). Never
    /// with a fill (normalized clears it). Volatile: never persisted and never on the wire; carried across the
    /// handoff ([`crate::carry::CarriedMessage::busy`]); never spoken as a
    /// word (the row is a progress indicator with no value).
    pub busy: bool,
    /// A MEASURED LEVEL, not progress (design ruling 208): the fill is a
    /// gauge of how loaded the machine is — it glides up AND down, has no
    /// glint, no percent word, no ETA and no stall tone, never completes
    /// (it retires by a Vanish only: [`crate::MessageCenter::resolve`]
    /// refuses it) and is admitted on the strain row alone
    /// ([`crate::strain::level_fault`]). Only with a fill (normalized clears
    /// it otherwise). Volatile like `busy`: never persisted, never on the
    /// wire, never carried (the successor's strain engine starts calm).
    pub level: bool,
}

impl Meter {
    /// A busy meter: work in flight with no known fraction, `stats` beside
    /// it (the bytes so far, or nothing).
    #[must_use]
    pub fn busy(stats: impl AsRef<str>) -> Self {
        Self {
            stats: clip(stats.as_ref(), STATS_CAP),
            busy: true,
            ..Self::default()
        }
    }

    /// A measured LEVEL ([`Meter::level`] the field): `permille` of a
    /// resource in use, `stats` beside it — never an amount, never busy.
    #[must_use]
    pub fn level(permille: u16, stats: impl AsRef<str>) -> Self {
        Self {
            fill_permille: Some(permille.min(1000)),
            stats: clip(stats.as_ref(), STATS_CAP),
            level: true,
            ..Self::default()
        }
    }

    /// A meter with its fill clamped, its stats sanitized, an amount with no
    /// total dropped, `done` clamped to `total`, a missing fill filled from
    /// `done / total`, and `busy` only where there is still no fill. A level
    /// is a level only with a fill of its own, and carries no amount (a
    /// gauge has no ETA) and no busy.
    #[must_use]
    pub fn normalized(self) -> Self {
        self.normalized_measured(false)
    }

    /// [`Self::normalized`], with `measured` naming a fill measured finer
    /// than its count by an amount not at hand: a CARRIED row's (ruling
    /// 263) — the parent's fill already agreed with its count, and the carry
    /// ships no amount — is kept inside the count's span as a measured one
    /// is, never snapped back to the count's floor (`3 of 10 programs` at
    /// 36 % stays 36 %, not 30 %, across a handoff).
    #[must_use]
    pub(crate) fn normalized_measured(self, measured: bool) -> Self {
        let level = self.level && self.fill_permille.is_some();
        let amount = self
            .amount
            .filter(|a| a.total > 0 && !level)
            .map(|a| Amount {
                done: a.done.min(a.total),
                ..a
            });
        let fill_permille = self
            .fill_permille
            .map(|p| p.min(1000))
            .or_else(|| amount.map(Amount::permille));
        // A COUNT a determinate row shows and its fill agree (design ruling
        // 259). With no amount the fill is only the reporter's figure, so
        // the count IS it: `3 of 4 tabs` is 75 %, never 80 %. With an amount
        // the fill is measured finer than the count (a pass's programs by
        // phase: 36 % while the fourth of ten installs), and it is kept
        // inside the count's own span — never under `3 of 10`'s 30 %, never at
        // `4 of 10`'s 40 % before the count says four. A measured level is a
        // gauge, not a count; a busy row (no fill) stays a comet.
        let counted = (!level)
            .then(|| count_span(&self.stats))
            .flatten()
            .filter(|_| fill_permille.is_some());
        let fill_permille = match (counted, amount.is_some() || measured) {
            (Some((lo, _)), false) => Some(lo),
            (Some((lo, hi)), true) => fill_permille.map(|p| p.clamp(lo, hi)),
            (None, _) => fill_permille,
        };
        Self {
            fill_permille,
            stats: clip(&self.stats, STATS_CAP),
            amount,
            load: self.load,
            busy: self.busy && fill_permille.is_none(),
            level,
        }
    }
}

/// The fill span a WHOLE-NUMBER COUNT in the stats' first clause allows —
/// `3 of 4 tabs` → `(750, 1000)`, `(A/B, (A+1)/B)` in permille, the whole bar
/// once the count is complete (design ruling 259) — or `None` when the first
/// clause is no such count (`1.2M of 3.4M lines`, a pair of sizes, words),
/// its total is zero, or it counts past its total.
fn count_span(stats: &str) -> Option<(u16, u16)> {
    let (done, total) = count_of(stats)?;
    let at = |n: u64| {
        let (n, t) = (u128::from(n.min(total)), u128::from(total));
        u16::try_from((n * 1000 + t / 2) / t).unwrap_or(1000)
    };
    Some((at(done), at(done + 1)))
}

/// `(A, B)` of a whole-number count `A of B unit` in the stats' first clause.
fn count_of(stats: &str) -> Option<(u64, u64)> {
    let clause = stats.split(crate::PIECE_SEP).next()?.trim();
    let mut words = clause.split(' ');
    let (Some(done), Some("of"), Some(total), Some(unit)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return None;
    };
    let whole = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit());
    if !whole(done) || !whole(total) || !unit.starts_with(char::is_alphabetic) {
        return None;
    }
    let (done, total) = (done.parse::<u64>().ok()?, total.parse::<u64>().ok()?);
    (total > 0 && done <= total).then_some((done, total))
}

/// The continuous quantity behind a determinate fill: `done` of `total`
/// `unit`s, in one `series` (a new series resets the estimator).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Amount {
    /// Which run of work this is: [`Amount::series_of`] the deliverable's
    /// label, so a new download is a new series.
    pub series: u64,
    /// How much is done.
    pub done: u64,
    /// How much the deliverable is.
    pub total: u64,
    /// What `done` and `total` count.
    pub unit: Unit,
}

impl Amount {
    /// The series id of a deliverable's label (FNV-1a).
    #[must_use]
    pub fn series_of(label: &str) -> u64 {
        let mut h = Fnv::new();
        h.str(label);
        h.finish()
    }

    /// `done / total` in permille, rounded to nearest; 0 with no total.
    #[must_use]
    pub(crate) fn permille(self) -> u16 {
        if self.total == 0 {
            return 0;
        }
        let done = u128::from(self.done.min(self.total));
        let total = u128::from(self.total);
        u16::try_from((done * 1000 + total / 2) / total).unwrap_or(1000)
    }
}

/// What an [`Amount`] counts. Bytes read "stalled" ten seconds after they
/// stop; a count of items or steps may honestly sit still while one item
/// takes long, so it reads "stalled" only past several of its own gaps
/// between advances (design ruling 265).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Bytes moved.
    Bytes,
    /// Items done (programs, files).
    Items,
    /// Steps of a fixed plan.
    Steps,
    /// [`Unit::Steps`] at a phase of the plan that HOLDS the fill with no
    /// meter of its own — a toolchain program verifying or linking,
    /// whose share sits at 500‰ or 990‰ until the phase ends (design ruling
    /// 266). The same count as `Steps` (the estimator keeps its readings
    /// across the switch), but its silence here is the plan, not a stall: it
    /// never reads "stalled", and the count's patience restarts when the
    /// phase ends. Never on the wire (its word is `steps`).
    HeldSteps,
}

impl Unit {
    /// The wire's word for it: `bytes`, `items` or `steps`.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Items => "items",
            Self::Steps | Self::HeldSteps => "steps",
        }
    }

    /// What the count IS, for the estimator: a held step is a step.
    #[must_use]
    pub(crate) const fn count(self) -> Self {
        match self {
            Self::HeldSteps => Self::Steps,
            other => other,
        }
    }

    /// The inverse of [`Unit::word`] (the wire's `unit=`); anything else ⇒
    /// `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Bytes, Self::Items, Self::Steps]
            .into_iter()
            .find(|u| u.word() == s)
    }
}

/// The resource a very heavy phase loads — the words that explain why the
/// machine is slow while it lasts (design §10.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Load {
    /// A large download.
    Network,
    /// Extraction or linking of a large tree.
    Disk,
    /// Verification of a large payload.
    Cpu,
    /// Memory is full and the machine swaps: the strain row's words for a
    /// memory culprit (design ruling 207). Not a wire word: `notice`'s
    /// `load=` stays the three above ([`Load::WIRE`]).
    Memory,
}

impl Load {
    /// Every load, for the slot that must fit the widest words.
    pub const ALL: [Load; 4] = [Self::Network, Self::Disk, Self::Cpu, Self::Memory];

    /// The loads a script may declare over the wire (`notice progress …
    /// load=`): a machine's memory is aterm's to measure, never a script's
    /// to claim.
    pub(crate) const WIRE: [Load; 3] = [Self::Network, Self::Disk, Self::Cpu];

    /// The band's words for it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Network => "network busy",
            Self::Disk => "disk busy",
            Self::Cpu => "CPU busy",
            Self::Memory => "memory full",
        }
    }

    /// The one-word resource — the wire's `load=` and the `messages` row's
    /// `load=`: `network`, `disk`, `cpu` or `memory`
    /// ([`Load::words`] stays the band's).
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Disk => "disk",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
        }
    }

    /// The inverse of [`Load::word`]; anything else ⇒ `None`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.word() == s)
    }

    /// [`Load::parse`] over the wire's words only ([`Load::WIRE`]).
    #[must_use]
    pub(crate) fn parse_wire(s: &str) -> Option<Self> {
        Self::WIRE.into_iter().find(|l| l.word() == s)
    }
}

/// A set of [`Load`]s: the kinds a row may show over its life, which size its
/// reserved load slot (design ruling 221). Empty is the undeclared row, whose
/// slot is sized for every load's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Loads(u8);

impl Loads {
    /// No load.
    pub(crate) const NONE: Self = Self(0);
    /// Every load.
    pub const ALL: Self = Self(0b1111);

    const fn bit(l: Load) -> u8 {
        match l {
            Load::Network => 1,
            Load::Disk => 2,
            Load::Cpu => 4,
            Load::Memory => 8,
        }
    }

    /// This set with `l` in it.
    #[must_use]
    pub(crate) const fn with(self, l: Load) -> Self {
        Self(self.0 | Self::bit(l))
    }

    /// Both sets together.
    #[must_use]
    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `true` for the empty set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `l` is in the set.
    #[must_use]
    pub(crate) const fn contains(self, l: Load) -> bool {
        self.0 & Self::bit(l) != 0
    }

    /// The set as one byte (the layout fingerprint's).
    #[must_use]
    pub(crate) const fn bits(self) -> u8 {
        self.0
    }

    /// The loads in the set, in [`Load::ALL`]'s order.
    pub(crate) fn iter(self) -> impl Iterator<Item = Load> {
        Load::ALL.into_iter().filter(move |&l| self.contains(l))
    }

    /// The set a row RESERVES once it shows `l` (design ruling 221): its
    /// declaration with `l` in it, or — undeclared — every load, as before.
    #[must_use]
    pub(crate) const fn reserved_with(self, l: Load) -> Self {
        if self.is_empty() {
            Self::ALL
        } else {
            self.with(l)
        }
    }
}

impl FromIterator<Load> for Loads {
    fn from_iter<I: IntoIterator<Item = Load>>(iter: I) -> Self {
        iter.into_iter().fold(Self::NONE, Self::with)
    }
}

/// Where a message came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A reporter in the host process.
    Host,
    /// `notice post` / `appnotice` over the control socket.
    Wire,
    /// Re-seeded from the seamless-handoff carry.
    Carried,
}

impl Origin {
    /// The log word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Wire => "wire",
            Self::Carried => "carried",
        }
    }

    /// The origin for a log word; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "host" => Some(Self::Host),
            "wire" => Some(Self::Wire),
            "carried" => Some(Self::Carried),
            _ => None,
        }
    }

    /// The wire OWNS a message minted by it or keyed in its namespace (a
    /// carried wire row's origin is `Carried`, its key is still
    /// [`crate::WIRE_KEY_PREFIX`]'s): the one class the duplicate test and
    /// the log ring's wire share both use (design rulings 167, 192, 194).
    #[must_use]
    pub fn wire_owned(self, key: Option<&str>) -> bool {
        self == Self::Wire || key.is_some_and(|k| k.starts_with(crate::WIRE_KEY_PREFIX))
    }
}

/// The wall clock at ingress, minted by the HOST (D4): the engine never reads
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct WallStamp {
    /// Milliseconds since the Unix epoch.
    pub unix_ms: u64,
}

/// One message as a reporter posts it. Every field is public plain data;
/// the builder sanitizes and caps as it goes, and [`Message::normalized`]
/// (applied by `post`) re-applies every cap so a struct literal is safe too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The reporter family.
    pub tag: Tag,
    /// Rank and default hold.
    pub severity: Severity,
    /// The glyph at column 1.
    pub glyph: Glyph,
    /// ≤ [`TITLE_CAP`] chars, one line, sanitized.
    pub title: String,
    /// ≤ [`DETAIL_LINES_CAP`] × [`DETAIL_LINE_CAP`], no line empty; `[0]`
    /// is the excerpt source.
    pub detail: Vec<String>,
    /// ≤ [`MAX_ACTIONS`]; `Details` implicit.
    pub actions: Vec<Intent>,
    /// How long it stays.
    pub hold: Hold,
    /// A live meter.
    pub meter: Option<Meter>,
    /// Supersede key: a new post with the same key replaces the live one in
    /// its slot.
    pub key: Option<String>,
    /// Where it came from.
    pub origin: Origin,
    /// Whether the band paints `detail[0]` beside the title (default true).
    /// False when no detail line changes what the person does; every line
    /// still rides Details, the log and Settings ▸ Messages, and the spoken
    /// name follows the paint.
    pub excerpt: bool,
    /// `None`: eligible for the glass at post. `Some(d)`: not on the glass
    /// until `posted_at + d` — a row that would appear and fold inside the
    /// grace costs two re-grids for nothing (design §10.4.3, E2).
    pub reveal_after: Option<Duration>,
    /// The words the row FINISHES with — what its Complete echo paints in
    /// the title's place (`Installed aterm vX` for `Finishing aterm vX`).
    /// `None`: [`Message::finished_title`] derives them from the title's
    /// leading present participle ([`crate::words::finished_form`]). The
    /// log keeps them too (design ruling 259): a live row that delivered is
    /// LOGGED under its finished words, its live title the first detail
    /// line; and an ask's declared words (`Asked for Full Disk Access`) are
    /// the title it is logged under once it is answered or folds.
    pub finished: Option<String>,
    /// The loads the row may show over its life (design ruling 221), declared
    /// by its reporter: its reserved load slot is sized to their words, not to
    /// every load's. Empty (the default) reserves for every load.
    pub loads: Loads,
    /// The load this work IS (design ruling 246): a download's network, a
    /// removal's disk, a build's CPU. Its words say nothing the title does not
    /// (`Downloading … · network busy`), so they show only while the machine
    /// is measurably slowing other work ([`crate::MessageCenter::set_slowing`]);
    /// a load outside it (a download's extraction making the disk busy) shows
    /// as before. `None`: every declared load shows.
    pub primary: Option<Load>,
    /// A RECORD ABOUT A PREVIOUS RUN (design ruling 259): `aterm crashed
    /// last time`. It ranks one severity class below its own on the glass
    /// ([`crate::glass::rank`]), so a past run's news never takes the row
    /// of something happening now. Set by the builder that knows it
    /// ([`Message::retrospective`]); never inferred from the words.
    pub retrospective: bool,
}

impl Message {
    /// A message with the severity's glyph and [`Hold::Default`].
    #[must_use]
    pub fn new(tag: Tag, severity: Severity, title: impl AsRef<str>) -> Self {
        Self {
            tag,
            glyph: severity.default_glyph(),
            severity,
            title: clip(title.as_ref(), TITLE_CAP),
            detail: Vec::new(),
            actions: Vec::new(),
            hold: Hold::Default,
            meter: None,
            key: None,
            origin: Origin::Host,
            excerpt: true,
            reveal_after: None,
            finished: None,
            loads: Loads::NONE,
            primary: None,
            retrospective: false,
        }
    }

    /// The loads this row may show over its life (design ruling 221): its
    /// load slot is reserved at the widest of their words.
    #[must_use]
    pub fn loads(mut self, loads: impl IntoIterator<Item = Load>) -> Self {
        self.loads = loads.into_iter().collect();
        self
    }

    /// The load this work IS ([`Message::primary`]): its words show only
    /// while other work is measurably slowed. It joins the declared loads.
    #[must_use]
    pub fn primary_load(mut self, l: Load) -> Self {
        self.primary = Some(l);
        self.loads = self.loads.with(l);
        self
    }

    /// A record about a PREVIOUS RUN ([`Message::retrospective`], ruling 259):
    /// it ranks one severity class lower on the glass.
    #[must_use]
    pub fn retrospective(mut self) -> Self {
        self.retrospective = true;
        self
    }

    /// The band paints the title alone: `detail[0]` does not change what the
    /// person does, so every line waits behind Details.
    #[must_use]
    pub fn no_excerpt(mut self) -> Self {
        self.excerpt = false;
        self
    }

    /// The words the row finishes with, where the closed participle table
    /// would read wrong ([`Message::finished`]; sanitized, capped; empty
    /// clears them).
    #[must_use]
    pub fn finished_as(mut self, words: impl AsRef<str>) -> Self {
        let w = clip(words.as_ref(), TITLE_CAP);
        self.finished = (!w.is_empty()).then_some(w);
        self
    }

    /// The title a Complete echo paints: the declared finished words, else
    /// the title's participle read finished, else the title itself.
    #[must_use]
    pub fn finished_title(&self) -> String {
        self.finished_words().unwrap_or_else(|| self.title.clone())
    }

    /// The row's words in the past tense, when there are any: the declared
    /// finished words, else the title's participle read finished. `None` for
    /// a free-text title (a wire script's `Uploading the backup`), whose
    /// Complete echo still needs a word saying it finished (ruling 247).
    #[must_use]
    pub fn finished_words(&self) -> Option<String> {
        match &self.finished {
            Some(w) => Some(w.clone()),
            None => crate::words::finished_form(&self.title),
        }
    }

    /// Not on the glass until `d` after the post (the progress grace).
    #[must_use]
    pub fn reveal_after(mut self, d: Duration) -> Self {
        self.reveal_after = Some(d);
        self
    }

    /// Declares the row's indicator: work in flight. With no fraction yet
    /// that is a BUSY meter ([`Meter::busy`]: the comet); a
    /// meter the builder already set keeps its fill and stats, and is busy
    /// only while it has no fill.
    #[must_use]
    pub fn in_flight(mut self) -> Self {
        let m = self.meter.get_or_insert_default();
        m.busy = m.fill_permille.is_none();
        self
    }

    /// One detail line (sanitized, capped; dropped past the line cap). An
    /// EMPTY line is dropped too — the wire parser already does the same —
    /// so the log's `detail=` is never ambiguous between no lines and one
    /// blank one, and every message the engine holds reads back from its
    /// own record.
    #[must_use]
    pub fn line(mut self, l: impl AsRef<str>) -> Self {
        let l = clip(l.as_ref(), DETAIL_LINE_CAP);
        if !l.is_empty() && self.detail.len() < DETAIL_LINES_CAP {
            self.detail.push(l);
        }
        self
    }

    /// Detail lines, each as [`Message::line`].
    #[must_use]
    pub fn lines<I: IntoIterator<Item = String>>(mut self, l: I) -> Self {
        for line in l {
            self = self.line(line);
        }
        self
    }

    /// A sentence received from elsewhere (an updater's error, a pass's
    /// cause), kept WHOLE across detail lines ([`text::split_sentence`],
    /// each ≤ [`DETAIL_LINE_CAP`], at most [`DETAIL_LINES_CAP`] of them) —
    /// never pre-cut. A short excerpt the band should show first is a
    /// [`Message::line`] before it.
    #[must_use]
    pub fn sentence(self, s: impl AsRef<str>) -> Self {
        self.lines(crate::text::split_sentence(s.as_ref(), DETAIL_LINE_CAP))
    }

    /// An authored intent; `Details` is implicit and ignored, and the
    /// third and later are dropped.
    #[must_use]
    pub fn action(mut self, i: Intent) -> Self {
        if i != Intent::Details && self.actions.len() < MAX_ACTIONS {
            self.actions.push(i);
        }
        self
    }

    /// The hold.
    #[must_use]
    pub fn hold(mut self, h: Hold) -> Self {
        self.hold = h;
        self
    }

    /// A meter (normalized).
    #[must_use]
    pub fn meter(mut self, m: Meter) -> Self {
        self.meter = Some(m.normalized());
        self
    }

    /// The supersede key (sanitized, capped; empty clears it).
    #[must_use]
    pub fn key(mut self, k: &str) -> Self {
        let k = clip(k, KEY_CAP);
        self.key = (!k.is_empty()).then_some(k);
        self
    }

    /// The glyph.
    #[must_use]
    pub fn glyph(mut self, g: Glyph) -> Self {
        self.glyph = g;
        self
    }

    /// The origin.
    #[must_use]
    pub(crate) fn origin(mut self, o: Origin) -> Self {
        self.origin = o;
        self
    }

    /// Applied by `post`: sanitize + cap title/detail/key/stats, drop empty
    /// detail lines, drop `Intent::Details`, truncate to [`MAX_ACTIONS`], and
    /// normalize the meter (a fill clears `busy`). The indicator is the
    /// meter's state and is NEVER invented here: a Live row that declared
    /// neither a fill nor `busy` is still (design ruling 139). Never rejects.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.title = clip(&self.title, TITLE_CAP);
        for line in &mut self.detail {
            *line = clip(line, DETAIL_LINE_CAP);
        }
        self.detail.retain(|line| !line.is_empty());
        self.detail.truncate(DETAIL_LINES_CAP);
        self.actions.retain(|i| *i != Intent::Details);
        self.actions.truncate(MAX_ACTIONS);
        let carried = self.origin == Origin::Carried;
        self.meter = self.meter.map(|m| m.normalized_measured(carried));
        self.key = self
            .key
            .as_deref()
            .map(|k| clip(k, KEY_CAP))
            .filter(|k| !k.is_empty());
        self.finished = self
            .finished
            .as_deref()
            .map(|w| clip(w, TITLE_CAP))
            .filter(|w| !w.is_empty());
        self
    }

    /// `true` when any authored intent is a decision.
    #[must_use]
    pub fn is_ask(&self) -> bool {
        matches!(self.hold, Hold::Ask { .. }) || self.actions.iter().any(Intent::is_ask)
    }
}

/// An in-place change to a live row: every field optional, none logged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restatement {
    /// New title.
    pub title: Option<String>,
    /// New detail lines.
    pub detail: Option<Vec<String>>,
    /// `Some(None)` clears the meter.
    pub meter: Option<Option<Meter>>,
    /// New authored intents.
    pub actions: Option<Vec<Intent>>,
    /// New hold (re-anchored if the row is on glass).
    pub hold: Option<Hold>,
    /// New severity (re-ranks).
    pub severity: Option<Severity>,
    /// New glyph.
    pub glyph: Option<Glyph>,
    /// New excerpt flag ([`Message::excerpt`]).
    pub excerpt: Option<bool>,
    /// New finished words ([`Message::finished`]; `Some(None)` clears them).
    /// Left unset, a restatement that changes the title drops the old
    /// declared words — they finished the old title.
    pub finished: Option<Option<String>>,
}

impl Restatement {
    /// `true` when nothing is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant 22: the vocabulary is closed and every word in it is a tag;
    /// the glyph set admits the band's glyphs and nothing else.
    #[test]
    fn tags_and_glyphs_are_closed_sets() {
        assert_eq!(tags::ALL.len(), 14, "fourteen reporter families");
        for tag in tags::ALL {
            let again = Tag::try_new(tag.as_str()).expect("a vocabulary word validates");
            assert_eq!(&again, *tag);
        }
        assert_eq!(Tag::try_new(""), Err(TagError::Empty));
        assert_eq!(Tag::try_new("Config"), Err(TagError::BadChar('C')));
        assert_eq!(Tag::try_new("a b"), Err(TagError::BadChar(' ')));
        assert_eq!(Tag::try_new(&"x".repeat(25)), Err(TagError::TooLong));
        assert!(Tag::try_new(&"x".repeat(24)).is_ok());

        assert!(
            Glyph::new('\u{2715}').is_some(),
            "✕ paints in cells already"
        );
        assert!(Glyph::new('\u{2699}').is_none(), "⚙ is blank headless");
        assert!(Glyph::new('\u{1f680}').is_none(), "no emoji");
        assert_eq!(Glyph::or_fallback('\u{2699}'), Glyph::FALLBACK);
        assert_eq!(Glyph::FALLBACK.ch(), '!');
        for sev in Severity::ALL {
            assert!(Glyph::ALLOWED.contains(&sev.default_glyph().ch()));
            assert_eq!(Severity::parse(sev.as_str()), Some(sev));
        }
        assert!(Severity::Error > Severity::Warn && Severity::Warn > Severity::Info);
        assert!(Severity::Info > Severity::Success);
    }

    /// Invariant 23: one label table. Every variant has a non-empty label
    /// ≤ 16 chars and a short ≤ 8, and the two differ only where the table
    /// says they do. The row predicates walk the same table: only `NotNow`
    /// closes, the two decision intents ask, and exactly the four
    /// machine-changing intents are consequential (the Primary chip —
    /// ruling 18, 2026-09-22).
    #[test]
    fn labels_are_one_table() {
        let same_short: &[&str] = &[
            "Packages", "Manual", "Messages", "Settings", "Install", "Not now",
        ];
        for intent in every_intent() {
            let label = intent.label();
            let short = intent.short();
            assert!(
                !label.is_empty() && label.chars().count() <= 16,
                "{intent:?}: {label:?}"
            );
            assert!(
                !short.is_empty() && short.chars().count() <= 8,
                "{intent:?}: {short:?}"
            );
            if same_short.contains(&label) {
                assert_eq!(label, short, "{intent:?} is its own short form");
            } else {
                assert_ne!(label, short, "{intent:?} has a distinct short form");
            }
        }
        assert_eq!(Intent::Details.label(), "Details \u{203a}");
        assert_eq!(Intent::Details.short(), "\u{203a}");
        assert_eq!(Intent::OpenPath { path: "/x".into() }.short(), "Log");
        assert_eq!(Intent::OpenConfigEditor { line: None }.short(), "Edit");
        // One install verb on every surface (ruling 69): the Version menu's
        // "Install aterm vX now", the palette's "Install update now", and the
        // band's capsule.
        assert_eq!(Intent::ApplyUpdate { build: 1 }.label(), "Install now");
        assert_eq!(Intent::ApplyUpdate { build: 1 }.short(), "Install");
        assert_eq!(
            Intent::OpenSettings {
                route: "/updates".into()
            }
            .label(),
            "Software Update"
        );
        // Only NotNow closes its row; the two decision intents are asks.
        for intent in every_intent() {
            assert_eq!(
                intent.closes_row(),
                matches!(intent, Intent::NotNow { .. }),
                "{intent:?}"
            );
            assert_eq!(
                intent.is_ask(),
                matches!(intent, Intent::NotNow { .. }),
                "{intent:?}"
            );
            // The accent chip marks a CONSEQUENTIAL press only: the five
            // that change the machine or the window. Every navigation and
            // every decline are quiet — `Packages` and `Software Update`
            // included, and an upgrade's `Not today` and `Skip version`.
            assert_eq!(
                intent.is_consequential(),
                matches!(
                    intent,
                    Intent::ApplyUpdate { .. }
                        | Intent::OpenSystemPane { .. }
                        | Intent::NewWindow
                        | Intent::StopPaste { .. }
                        | Intent::AgentUpgrade {
                            word: UpgradeWord::Now,
                            ..
                        }
                ),
                "{intent:?}"
            );
            assert!(
                !(intent.is_consequential() && intent.closes_row()),
                "{intent:?}: a consequential press never closes its row"
            );
        }
        assert!(
            !Intent::OpenSettings {
                route: "/packages".into()
            }
            .is_consequential()
                && !Intent::OpenSettings {
                    route: "/updates".into()
                }
                .is_consequential(),
            "Packages and Software Update are navigations: the quiet chip"
        );
        assert!(!Intent::Details.is_consequential());
    }

    /// Every intent round-trips its encoding; junk decodes to `None`.
    #[test]
    fn intents_round_trip_and_junk_is_dropped() {
        for intent in every_intent() {
            let enc = intent.encode();
            assert_eq!(Intent::decode(&enc).as_ref(), Some(&intent), "{enc}");
            assert!(!enc.contains('\u{1f}') && !enc.contains('\t'), "{enc}");
        }
        let hostile = Intent::OpenPath {
            path: "/a b/%\t\n\u{1f}c".into(),
        };
        let enc = hostile.encode();
        assert!(
            !enc.contains('\t') && !enc.contains('\n') && !enc.contains('\u{1f}'),
            "{enc}"
        );
        assert_eq!(Intent::decode(&enc), Some(hostile));
        for junk in [
            "",
            "details:x",
            "open-config-editor:abc",
            "apply-update:-1",
            "not-now:maybe",
            "teleport:home",
            "new-window:1",
        ] {
            assert_eq!(Intent::decode(junk), None, "{junk:?}");
        }
        assert_eq!(
            Intent::decode("open-config-editor:-"),
            Some(Intent::OpenConfigEditor { line: None })
        );
        // The retired admin step's intents (2026-09-24), carried by an older window,
        // decode to nothing: the loader drops them.
        for retired in ["not-now:admin-step:clt,brew", "install-elevated:clt,brew"] {
            assert_eq!(Intent::decode(retired), None, "{retired:?}");
        }
        for hold in [
            Hold::Default,
            Hold::For(Duration::from_secs(9)),
            Hold::Live {
                stale_after: Duration::from_secs(30),
            },
            Hold::Standing,
            Hold::Ask {
                for_: Duration::from_secs(600),
            },
            Hold::LogOnly,
        ] {
            assert_eq!(Hold::decode(&hold.encode()), Some(hold));
        }
        assert_eq!(Hold::decode("forever"), None);
        assert_eq!(Hold::decode("for:x"), None);
    }

    /// A BUSY meter is work with no known fraction: never with a fill —
    /// `normalized` clears the flag where a fill arrives — and its stats are
    /// clipped like any meter's.
    #[test]
    fn a_busy_meter_never_carries_a_fill() {
        let m = Meter::busy("45 MB");
        assert!(m.busy && m.fill_permille.is_none());
        assert_eq!(m.stats, "45 MB");
        let filled = Meter {
            fill_permille: Some(420),
            busy: true,
            ..Meter::default()
        }
        .normalized();
        assert!(!filled.busy, "a fill is a fraction: not busy");
        // A fill DERIVED from an amount is a fraction too.
        let counted = Meter {
            amount: Some(Amount {
                series: 1,
                done: 5,
                total: 10,
                unit: Unit::Bytes,
            }),
            busy: true,
            ..Meter::default()
        }
        .normalized();
        assert_eq!((counted.fill_permille, counted.busy), (Some(500), false));
        let long = Meter::busy("x".repeat(STATS_CAP + 10));
        assert_eq!(long.stats.chars().count(), STATS_CAP);
        let posted = Message::new(tags::UPDATE, Severity::Info, "t").meter(Meter {
            fill_permille: Some(1),
            busy: true,
            ..Meter::default()
        });
        assert!(!posted.meter.unwrap().busy);
    }

    /// THE INDICATOR IS THE METER'S STATE (design ruling 139): `in_flight`
    /// declares a busy meter, or marks a fill-less one busy, and keeps a
    /// fill; `normalized` never invents an indicator — a Live row that
    /// declared none is still (a row blocked on the person).
    #[test]
    fn in_flight_declares_busy_and_normalized_invents_nothing() {
        let live = Hold::Live {
            stale_after: Duration::from_secs(30),
        };
        let m = Message::new(tags::UPDATE, Severity::Info, "t").in_flight();
        assert_eq!(m.meter, Some(Meter::busy("")));
        let m = Message::new(tags::UPDATE, Severity::Info, "t")
            .meter(Meter {
                stats: "45 MB".into(),
                ..Meter::default()
            })
            .in_flight();
        assert_eq!(m.meter, Some(Meter::busy("45 MB")));
        let m = Message::new(tags::UPDATE, Severity::Info, "t")
            .meter(Meter {
                fill_permille: Some(300),
                ..Meter::default()
            })
            .in_flight();
        let meter = m.meter.unwrap();
        assert_eq!((meter.fill_permille, meter.busy), (Some(300), false));
        let still = Message::new(tags::UPDATE, Severity::Warn, "t")
            .hold(live)
            .normalized();
        assert_eq!(still.meter, None, "a Live row blocked on the person");
    }

    /// The builder caps everything, and `normalized` re-applies the caps to a
    /// literal.
    #[test]
    fn the_builder_and_normalized_apply_every_cap() {
        let long = "x".repeat(TITLE_CAP + 40);
        let m = Message::new(tags::SYSTEM, Severity::Info, &long)
            .lines(
                (0..DETAIL_LINES_CAP + 5)
                    .map(|i| format!("line {i} {}", "y".repeat(DETAIL_LINE_CAP))),
            )
            .action(Intent::Details)
            .action(Intent::NewWindow)
            .action(Intent::NewWindow)
            .action(Intent::NewWindow)
            .key(&"k".repeat(KEY_CAP + 3))
            .meter(Meter {
                fill_permille: Some(4000),
                stats: "s".repeat(STATS_CAP + 5),
                ..Meter::default()
            });
        assert_eq!(m.title.chars().count(), TITLE_CAP);
        assert!(m.title.ends_with('\u{2026}'));
        assert_eq!(m.detail.len(), DETAIL_LINES_CAP);
        assert!(
            m.detail
                .iter()
                .all(|l| l.chars().count() <= DETAIL_LINE_CAP)
        );
        // An empty line — typed, or empty once its controls are stripped —
        // is dropped, in the builder and in `normalized`.
        let blank = Message::new(tags::SYSTEM, Severity::Info, "t")
            .line("")
            .line("\u{7}\u{1b}")
            .line("kept")
            .line("");
        assert_eq!(blank.detail, vec!["kept".to_string()]);
        let literal_blank = Message {
            detail: vec![String::new(), "a".into(), "\u{7}".into(), "b".into()],
            ..blank.clone()
        }
        .normalized();
        assert_eq!(literal_blank.detail, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(m.actions, vec![Intent::NewWindow, Intent::NewWindow]);
        assert_eq!(m.key.as_ref().map(|k| k.chars().count()), Some(KEY_CAP));
        let meter = m.meter.clone().unwrap();
        assert_eq!(meter.fill_permille, Some(1000));
        assert_eq!(meter.stats.chars().count(), STATS_CAP);

        let literal = Message {
            tag: tags::CONFIG,
            severity: Severity::Warn,
            glyph: Glyph::FALLBACK,
            title: "a\u{1b}[31mb\u{7}".into(),
            detail: vec!["ok".into()],
            actions: vec![
                Intent::Details,
                Intent::NewWindow,
                Intent::NewWindow,
                Intent::NewWindow,
            ],
            hold: Hold::Default,
            meter: None,
            key: Some(String::new()),
            origin: Origin::Host,
            excerpt: true,
            reveal_after: None,
            finished: Some("\u{1b}Done".into()),
            loads: Loads::NONE,
            primary: None,
            retrospective: false,
        }
        .normalized();
        assert_eq!(literal.finished.as_deref(), Some("Done"));
        assert_eq!(
            literal.title, "a[31mb",
            "the ESC and BEL go; the printable bytes stay"
        );
        assert_eq!(literal.actions.len(), MAX_ACTIONS);
        assert_eq!(literal.key, None);
        assert!(
            Message::new(tags::SYSTEM, Severity::Info, "t")
                .key("")
                .key
                .is_none()
        );
        assert!(Restatement::default().is_empty());
        assert_eq!(MessageId::from_raw(0), None);
        assert_eq!(MessageId::from_raw(7).map(MessageId::raw), Some(7));
        assert_eq!(MessageId::FIRST.next().raw(), 2);
    }
}
