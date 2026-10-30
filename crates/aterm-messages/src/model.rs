// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Message model: ids, severities, closed tag/glyph sets, intents, and the [`Message`] builder.

use std::borrow::Cow;
use std::fmt;

use crate::glass::Fnv;
use crate::need::{Ask, LaterAsk, MAX_TRANSIENT, Need, NoticeKind, Persist};
use crate::text::clip;
use crate::{
    DETAIL_LINE_CAP, DETAIL_LINES_CAP, Duration, HOLD_ERROR, HOLD_INFO, HOLD_SUCCESS, HOLD_WARN,
    KEY_CAP, MAX_ACTIONS, STATS_CAP, TITLE_CAP,
};

/// Process-monotonic, never 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(u64);

impl MessageId {
    /// The first id a fresh log mints.
    pub const FIRST: MessageId = MessageId(1);

    /// The raw number, for the wire and the log.
    #[inline]
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// An id from its raw number.
    #[inline]
    #[must_use]
    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// The id after this one (saturating at `u64::MAX`, which no process reaches).
    #[inline]
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

/// Four severities.
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
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// The severity for a wire word.
    #[inline]
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

    /// The hold a [`Hold::Default`] row of this severity takes.
    #[inline]
    #[must_use]
    pub const fn default_hold(self) -> Duration {
        match self {
            Self::Success => HOLD_SUCCESS,
            Self::Info => HOLD_INFO,
            Self::Warn => HOLD_WARN,
            Self::Error => HOLD_ERROR,
        }
    }

    /// The glyph a row of this severity carries unless the reporter picks another.
    #[inline]
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

/// A lowercase `[a-z0-9-]{1,24}` word naming the reporter family.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(Cow<'static, str>);

impl Tag {
    /// The longest tag admitted.
    pub(crate) const MAX_LEN: usize = 24;

    /// Validate a wire word into a tag.
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
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    const fn word(s: &'static str) -> Self {
        Self(Cow::Borrowed(s))
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The reporter vocabulary.
pub mod tags {
    use super::Tag;

    /// `aterm.toml` and keybinding diagnostics.
    pub const CONFIG: Tag = Tag::word("config");
    /// The previous process ended badly.
    pub const CRASH: Tag = Tag::word("crash");
    /// The `ALab` toolchain install (`atpkg`).
    pub const TOOLCHAIN: Tag = Tag::word("toolchain");
    /// Aterm's own self-update.
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
    /// An agent harness's notes (`aterm ctl appnotice harness`).
    pub const HARNESS: Tag = Tag::word("harness");
    /// Everything the host reports about itself.
    pub const SYSTEM: Tag = Tag::word("system");
    /// A script's `notice progress` row that named no tag.
    pub const SCRIPT: Tag = Tag::word("script");

    /// The fourteen, in the tag chips' fixed order on Settings ▸ Messages.
    pub const ALL: &[&Tag] = &[
        &CRASH, &CONFIG, &UPDATE, &TOOLCHAIN, &PACKAGES, &PRIVACY, &SESSION, &WINDOW, &RENDER,
        &A11Y, &FABRIC, &HARNESS, &SYSTEM, &SCRIPT,
    ];
}

/// One text-presentation glyph from the band's closed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Glyph(char);

impl Glyph {
    /// The admitted glyphs.
    pub const ALLOWED: &'static [char] = &[
        '\u{2139}', '\u{2713}', '\u{26a0}', '\u{2715}', '\u{21e3}', '\u{21bb}', '\u{2191}',
        '\u{23f8}', '\u{2726}', '!', '\u{2026}', '\u{2296}',
    ];
    const RETIRED: &'static [(char, char)] = &[('\u{00b7}', '\u{2296}')];
    /// What stands in for a glyph outside the set.
    pub const FALLBACK: Glyph = Glyph('!');

    /// A glyph from the closed set.
    #[must_use]
    pub fn new(ch: char) -> Option<Self> {
        Self::ALLOWED.contains(&ch).then_some(Self(ch))
    }

    /// The glyph, or [`Glyph::FALLBACK`] when `ch` is outside the set.
    #[must_use]
    pub fn or_fallback(ch: char) -> Self {
        Self::new(ch)
            .or_else(|| {
                Self::RETIRED
                    .iter()
                    .find(|(old, _)| *old == ch)
                    .map(|(_, now)| Self(*now))
            })
            .unwrap_or(Self::FALLBACK)
    }

    /// The character.
    #[inline]
    #[must_use]
    pub const fn ch(self) -> char {
        self.0
    }
}

/// What a `Not now` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The Full Disk Access question.
    FileAccess,
}

/// The owner's word on a live agent upgrade, pressed on its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeWord {
    /// Move the agent at its next turn end (`--now`).
    Now,
    /// Not for a day (`--defer`).
    NotToday,
    /// Stay on the running build until a newer one than this target comes (`--skip`).
    Skip,
}

impl UpgradeWord {
    /// Every word, in the order a row or a menu offers them.
    pub const ALL: [UpgradeWord; 3] = [Self::Now, Self::NotToday, Self::Skip];

    /// The codec's word: `now`, `not-today`, `skip`.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::NotToday => "not-today",
            Self::Skip => "skip",
        }
    }

    /// [`Self::as_str`] read back.
    #[inline]
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.as_str() == word)
    }

    /// The full label for the capsule, the tab menu, and a screen reader.
    #[inline]
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Now => "Upgrade now",
            Self::NotToday => "Not today",
            Self::Skip => "Skip version",
        }
    }
}

/// Plain-data intents the host performs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// The implicit last capsule.
    Details,
    /// Open a Settings route (`/packages`, `/updates`, `/manual`, `/appearance`, `/messages`, …).
    OpenSettings {
        /// A `SettingsRoute::path()` string.
        route: String,
    },
    /// Open `aterm.toml` in the editor, at a line when the report has one.
    OpenConfigEditor {
        /// `None` for launch warnings, which carry no line.
        line: Option<u32>,
    },
    /// Open a host-minted path under the log directory.
    OpenPath {
        /// The absolute path.
        path: String,
    },
    /// Open a system settings pane (`full-disk-access`).
    OpenSystemPane {
        /// The pane's name.
        pane: String,
    },
    /// Open a web page in the browser.
    OpenLink {
        /// The address.
        url: String,
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
    /// Drop the rest of a large paste still on its way to a session's program.
    StopPaste {
        /// The session the paste is going to.
        session: u64,
    },
    /// Legacy ordinal-only navigation retained for persisted record decoding.
    ShowTab {
        /// The tab's number in its window, 1-based.
        tab: u16,
        /// The host's id of the window the tab is in.
        window: u64,
    },
    /// Navigate to the exact session a row named.
    ShowSession {
        /// The snapshot's human tab number, never a selection index.
        tab: u16,
        /// Prefer this window if it still shows the session.
        window: u64,
        /// The lossless stable session identity, preserved through handoff.
        session: String,
    },
    /// The owner's word on the live agent upgrade in tab `tab`.
    AgentUpgrade {
        /// The tab the agent runs in (`s-<hex>`).
        tab: String,
        /// The target build the row named.
        to: String,
        /// Which word.
        word: UpgradeWord,
    },
    /// Close every tab whose shell the PTY keeper held through the last run's crash.
    EndRecovered,
    /// Open System State: a vault line's remedy (its Secrets row), and the navigation of the
    /// note that setup waits for its review.
    ReviewVaultSync,
    /// Let exactly the machine with this key, which asks to join this admin Mac's secrets vault,
    /// in. Pressed only by name: never a row's primary ([`Intent::never_default`]).
    ApproveJoin {
        /// The asking machine's key (`age1…`).
        recipient: String,
    },
    /// Allow the Mac whose request this member's desk heard as `request`: the member shows a
    /// phrase in a native sheet for a person to type on that Mac
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.1). A person's own press on this Mac's
    /// screen only ([`Intent::person_only`]), never a row's primary ([`Intent::never_default`]).
    AllowJoin {
        /// The request's handle on this member's desk.
        request: u64,
    },
    /// Hide the request this member's desk heard as `request` on this Mac.
    IgnoreJoin {
        /// The request's handle on this member's desk.
        request: u64,
    },
    /// "Allow again" the Mac this member's desk heard as `request`, which a member's removal
    /// took out (`docs/DESIGN-fluid-swarm-2026-10-28.md` F16): the phrase shows as for
    /// [`Intent::AllowJoin`], and the run restores the Mac. A person's own press on this Mac's
    /// screen only ([`Intent::person_only`]), never a row's primary ([`Intent::never_default`]).
    AllowJoinAgain {
        /// The request's handle on this member's desk.
        request: u64,
    },
    /// Sign this Mac's share of the quorum `Remove` that undoes the removals of the Mac whose
    /// join code is `remover` (F16's "Undo m7's removals?"). A person's own press only, never a
    /// row's primary.
    UndoRemovals {
        /// The remover's join code (`xxxx-xxxx-xxxx-xxxx`).
        remover: String,
    },
    /// Restart one of macOS's own services that holds far too much memory, in a tab that asks
    /// for the administrator's password once (`aterm_vitals::services::RESTARTS`).
    RestartService {
        /// The service's name (`fseventsd`), which the host checks against the restarts it knows.
        service: String,
    },
}

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

const TAB_N: [&str; 10] = [
    "Tab", "Tab 1", "Tab 2", "Tab 3", "Tab 4", "Tab 5", "Tab 6", "Tab 7", "Tab 8", "Tab 9",
];

impl Intent {
    /// A press that only silences a row or opens its words.
    #[inline]
    #[must_use]
    pub const fn is_snooze(&self) -> bool {
        matches!(
            self,
            Self::Details
                | Self::NotNow { .. }
                | Self::AgentUpgrade {
                    word: UpgradeWord::NotToday | UpgradeWord::Skip,
                    ..
                }
        )
    }

    /// The full capsule label.
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
                "/security" => "Security",
                _ => "Settings",
            },
            Self::OpenConfigEditor { .. } => "Open aterm.toml",
            Self::OpenPath { .. } => "Open log",
            Self::OpenSystemPane { .. } => "Open Settings",
            Self::OpenLink { .. } => "Open in browser",
            Self::ApplyUpdate { .. } => "Install now",
            Self::NotNow { .. } => "Not now",
            Self::NewWindow => "New window",
            Self::StopPaste { .. } => "Stop paste",
            Self::ShowTab { tab, .. } | Self::ShowSession { tab, .. } => {
                SHOW_TAB[usize::from(*tab).min(10) % 10]
            }
            Self::AgentUpgrade { word, .. } => word.label(),
            Self::EndRecovered => "End sessions",
            Self::ReviewVaultSync => "System State",
            Self::ApproveJoin { .. } => "Approve",
            Self::AllowJoin { .. } => "Allow",
            Self::IgnoreJoin { .. } => "Ignore",
            Self::AllowJoinAgain { .. } => "Allow again",
            Self::UndoRemovals { .. } => "Undo removals",
            Self::RestartService { .. } => "Restart",
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
                "/security" => "Security",
                _ => "Settings",
            },
            Self::OpenConfigEditor { .. } => "Edit",
            Self::OpenPath { .. } => "Log",
            Self::OpenSystemPane { .. } => "Settings",
            Self::OpenLink { .. } => "Open",
            Self::ApplyUpdate { .. } => "Install",
            Self::NotNow { .. } => "Not now",
            Self::NewWindow => "Window",
            Self::StopPaste { .. } => "Stop",
            Self::ShowTab { tab, .. } | Self::ShowSession { tab, .. } => {
                TAB_N[usize::from(*tab).min(10) % 10]
            }
            Self::AgentUpgrade { word, .. } => match word {
                UpgradeWord::Now => "Upgrade",
                UpgradeWord::NotToday => "Not now",
                UpgradeWord::Skip => "Skip",
            },
            Self::EndRecovered => "End",
            Self::ReviewVaultSync => "System",
            Self::ApproveJoin { .. } => "Approve",
            Self::AllowJoin { .. } | Self::AllowJoinAgain { .. } => "Allow",
            Self::IgnoreJoin { .. } => "Ignore",
            Self::UndoRemovals { .. } => "Undo",
            Self::RestartService { .. } => "Restart",
        }
    }

    /// An intent pressed only by name, never a row's primary or default button: letting a
    /// machine into the vault, or back in, is a person's reading of its code, not a reflex.
    #[inline]
    #[must_use]
    pub const fn never_default(&self) -> bool {
        matches!(
            self,
            Self::ApproveJoin { .. }
                | Self::AllowJoin { .. }
                | Self::AllowJoinAgain { .. }
                | Self::UndoRemovals { .. }
        )
    }

    /// An intent only a person's own input on this Mac may press: `notice act`, a fleet drive
    /// and a control client's gesture are refused it (`docs/DESIGN-fluid-swarm-2026-10-28.md`
    /// §3.3.5).
    #[inline]
    #[must_use]
    pub const fn person_only(&self) -> bool {
        matches!(
            self,
            Self::AllowJoin { .. } | Self::AllowJoinAgain { .. } | Self::UndoRemovals { .. }
        )
    }

    /// Only `NotNow` closes its row.
    #[inline]
    #[must_use]
    pub(crate) const fn closes_row(&self) -> bool {
        matches!(self, Self::NotNow { .. })
    }

    /// An intent that acts on the live row itself.
    #[inline]
    #[must_use]
    pub const fn ends_with_row(&self) -> bool {
        matches!(
            self,
            Self::NotNow { .. }
                | Self::StopPaste { .. }
                | Self::ShowTab { .. }
                | Self::ShowSession { .. }
                | Self::EndRecovered
        )
    }

    /// `NotNow`: a row carrying one is a decision row.
    #[inline]
    #[must_use]
    pub const fn is_ask(&self) -> bool {
        matches!(self, Self::NotNow { .. })
    }

    /// A consequential intent.
    #[inline]
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
                | Self::EndRecovered
                | Self::RestartService { .. }
        )
    }

    /// A decline (`Not now`, an upgrade's `Not today` or `Skip version`).
    #[inline]
    #[must_use]
    pub const fn is_decline(&self) -> bool {
        matches!(
            self,
            Self::NotNow { .. }
                | Self::AgentUpgrade {
                    word: UpgradeWord::NotToday | UpgradeWord::Skip,
                    ..
                }
        )
    }

    /// A press that stops the row's own work in flight (`StopPaste`).
    #[inline]
    #[must_use]
    pub const fn stops_work(&self) -> bool {
        matches!(self, Self::StopPaste { .. })
    }

    /// The log/carry form: `details`, `open-settings:/packages`, `open-config-editor:<line|->`.
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
            Self::OpenLink { url } => format!("open-link:{}", escape_payload(url)),
            Self::ApplyUpdate { build } => format!("apply-update:{build}"),
            Self::NotNow { decision } => match decision {
                Decision::FileAccess => "not-now:file-access".to_string(),
            },
            Self::NewWindow => "new-window".to_string(),
            Self::StopPaste { session } => format!("stop-paste:{session}"),
            Self::ShowTab { tab, window } => format!("show-tab:{tab}:{window}"),
            Self::ShowSession {
                tab,
                window,
                session,
            } => {
                format!("show-session:{tab}:{window}:{}", escape_field(session))
            }
            Self::AgentUpgrade { tab, to, word } => format!(
                "agent-upgrade:{}:{}:{}",
                word.as_str(),
                escape_field(to),
                escape_field(tab)
            ),
            Self::EndRecovered => "end-recovered".to_string(),
            Self::ReviewVaultSync => "review-vault-sync".to_string(),
            Self::ApproveJoin { recipient } => {
                format!("approve-join:{}", escape_payload(recipient))
            }
            Self::AllowJoin { request } => format!("allow-join:{request}"),
            Self::IgnoreJoin { request } => format!("ignore-join:{request}"),
            Self::AllowJoinAgain { request } => format!("allow-join-again:{request}"),
            Self::UndoRemovals { remover } => {
                format!("undo-removals:{}", escape_payload(remover))
            }
            Self::RestartService { service } => {
                format!("restart-service:{}", escape_payload(service))
            }
        }
    }

    /// The inverse of [`Intent::encode`].
    #[must_use]
    pub fn decode(s: &str) -> Option<Intent> {
        let (kind, payload) = s.split_once(':').unwrap_or((s, ""));
        match kind {
            "details" if payload.is_empty() => Some(Self::Details),
            "new-window" if payload.is_empty() => Some(Self::NewWindow),
            "end-recovered" if payload.is_empty() => Some(Self::EndRecovered),
            "review-vault-sync" if payload.is_empty() => Some(Self::ReviewVaultSync),
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
            "open-link" => {
                let url = unescape_payload(payload);
                link_host(&url)?;
                Some(Self::OpenLink { url })
            }
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
            "show-session" => {
                let (tab, rest) = payload.split_once(':')?;
                let (window, session) = rest.split_once(':')?;
                let session = unescape_payload(session);
                if session.is_empty() {
                    return None;
                }
                Some(Self::ShowSession {
                    tab: tab.parse().ok().filter(|t| *t > 0)?,
                    window: window.parse().ok()?,
                    session,
                })
            }
            "not-now" => match payload.split_once(':').unwrap_or((payload, "")) {
                ("file-access", "") => Some(Self::NotNow {
                    decision: Decision::FileAccess,
                }),
                _ => None,
            },
            "approve-join" => {
                let recipient = unescape_payload(payload);
                (recipient.starts_with("age1")
                    && recipient.bytes().all(|b| b.is_ascii_alphanumeric()))
                .then_some(Self::ApproveJoin { recipient })
            }
            "allow-join" => Some(Self::AllowJoin {
                request: payload.parse().ok()?,
            }),
            "ignore-join" => Some(Self::IgnoreJoin {
                request: payload.parse().ok()?,
            }),
            "allow-join-again" => Some(Self::AllowJoinAgain {
                request: payload.parse().ok()?,
            }),
            "undo-removals" => {
                let remover = unescape_payload(payload);
                remover_word(&remover).then_some(Self::UndoRemovals { remover })
            }
            "restart-service" => {
                let service = unescape_payload(payload);
                service_name(&service).then_some(Self::RestartService { service })
            }
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

/// Whether `name` can be a service's name an [`Intent::RestartService`] carries: a process's
/// short name (`p_comm`, at most 16 bytes) of letters, digits, `.`, `_` and `-`.
#[must_use]
pub fn service_name(name: &str) -> bool {
    (1..=16).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `word` can name the remover an [`Intent::UndoRemovals`] carries: its join code or
/// label, 1 to 64 bytes of lowercase letters, digits and `-`.
fn remover_word(word: &str) -> bool {
    (1..=64).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn escape_field(s: &str) -> String {
    escape_payload(s).replace(':', "%3A")
}

/// The longest address an [`Intent::OpenLink`] carries, in bytes.
pub const LINK_URL_CAP: usize = 512;

/// The host an [`Intent::OpenLink`] names, when `url` is an address it may carry.
#[must_use]
pub fn link_host(url: &str) -> Option<&str> {
    let printable = url
        .bytes()
        .all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'"' | b'<' | b'>' | b'`'));
    if url.len() > LINK_URL_CAP || !printable {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let host = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    let ok = host.len() <= 253 && host.contains('.') && host.split('.').all(label_ok);
    ok.then_some(host)
}

/// Every intent variant once, with a representative payload.
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
            route: "/security".into(),
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
        Intent::OpenLink {
            url: "https://chatgpt.com/security/setup?from=codex%3Aa#top".into(),
        },
        Intent::ApplyUpdate { build: 1234 },
        Intent::NotNow {
            decision: Decision::FileAccess,
        },
        Intent::NewWindow,
        Intent::EndRecovered,
        Intent::ReviewVaultSync,
        Intent::ApproveJoin {
            recipient: "age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq".into(),
        },
        Intent::AllowJoin { request: 7 },
        Intent::IgnoreJoin { request: 7 },
        Intent::AllowJoinAgain { request: 7 },
        Intent::UndoRemovals {
            remover: "abcd-efgh-ijkl-mn23".into(),
        },
        Intent::RestartService {
            service: "fseventsd".into(),
        },
        Intent::StopPaste { session: 3 },
        Intent::ShowTab { tab: 2, window: 7 },
        Intent::ShowSession {
            tab: 2,
            window: 7,
            session: "s-b5cf2faabac5ce5127bd".into(),
        },
        Intent::ShowSession {
            tab: 1,
            window: 0,
            session: "s-a:b%\t".into(),
        },
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
        Intent::AgentUpgrade {
            tab: "s-a:b".into(),
            to: "2.1.282:%\t".into(),
            word: UpgradeWord::Skip,
        },
    ]
}

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
        } else {
            out.push(c);
        }
    }
    out
}

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

/// Which capsule a press landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionIndex(pub u8);

impl ActionIndex {
    /// The implicit `Details ›` capsule.
    pub const DETAILS: ActionIndex = ActionIndex(255);

    /// `true` for the implicit capsule.
    #[inline]
    #[must_use]
    pub const fn is_details(self) -> bool {
        self.0 == Self::DETAILS.0
    }
}

/// How long a row stays and what ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The severity's hold, anchored at first glass.
    Default,
    /// A specific hold, anchored the same way.
    For(Duration),
    /// Unfinished: stays until a restate or resolve.
    Live {
        /// How long the row may go without a restate.
        stale_after: Duration,
    },
    /// Until resolve, or until read (an agent that is down, GPU lost, a11y dead).
    Standing(Persist),
    /// A decision row: ranks first.
    Ask {
        /// The question's patience.
        for_: Duration,
        /// The proof the decision row was asked for.
        seal: Persist,
    },
    /// Recorded, never on glass (managed-current repeats).
    LogOnly,
}

impl Hold {
    /// The wire/carry word: `default`, `for:<secs>`, `live:<secs>`, `standing`, `ask:<secs>`.
    #[must_use]
    pub fn encode(self) -> String {
        match self {
            Self::Default => "default".to_string(),
            Self::For(d) => format!("for:{}", d.as_secs()),
            Self::Live { stale_after } => format!("live:{}", stale_after.as_secs()),
            Self::Standing(_) => "standing".to_string(),
            Self::Ask { for_, .. } => format!("ask:{}", for_.as_secs()),
            Self::LogOnly => "log-only".to_string(),
        }
    }

    /// The inverse of [`Hold::encode`].
    #[must_use]
    pub(crate) fn decode(s: &str) -> Option<Self> {
        let (kind, secs) = s.split_once(':').unwrap_or((s, ""));
        let dur = || secs.parse::<u64>().ok().map(Duration::from_secs);
        match kind {
            "default" if secs.is_empty() => Some(Self::Default),
            "standing" if secs.is_empty() => Some(Self::Standing(Persist::mint())),
            "log-only" if secs.is_empty() => Some(Self::LogOnly),
            "for" => dur().map(Self::For),
            "live" => dur().map(|stale_after| Self::Live { stale_after }),
            "ask" => dur().map(|for_| Self::Ask {
                for_,
                seal: Persist::mint(),
            }),
            _ => None,
        }
    }

    /// `true` for the holds a fold ends (Default / For / Ask).
    #[inline]
    #[must_use]
    pub const fn is_held(self) -> bool {
        matches!(self, Self::Default | Self::For(_) | Self::Ask { .. })
    }

    /// `true` for the holds that outlast a look (Standing, Ask).
    #[inline]
    #[must_use]
    pub const fn persists(self) -> bool {
        matches!(self, Self::Standing(_) | Self::Ask { .. })
    }

    /// `true` for [`Hold::Standing`].
    #[inline]
    #[must_use]
    pub const fn is_standing(self) -> bool {
        matches!(self, Self::Standing(_))
    }
}

#[cfg(any(test, debug_assertions))]
impl Hold {
    /// A standing hold.
    pub const STANDING: Hold = Hold::Standing(Persist::mint());

    /// A decision hold of `for_`.
    #[must_use]
    pub const fn asking(for_: Duration) -> Hold {
        Hold::Ask {
            for_,
            seal: Persist::mint(),
        }
    }
}

/// What the door ([`Message::admit`]) did with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Let through as it stands.
    Kept,
    /// Made a record, words kept.
    Demoted,
}

/// A row's progress: a fill.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Meter {
    /// 0..=1000, or `None` for work with no fraction.
    pub fill_permille: Option<u16>,
    /// `512 MB / 1.2 GB`.
    pub stats: String,
    /// The continuous quantity behind the fill, the eta's only input.
    pub amount: Option<Amount>,
    /// The resource the current phase loads, declared by the reporter for the phase's duration.
    pub load: Option<Load>,
    /// Work in flight with no known fraction.
    pub busy: bool,
}

impl Meter {
    /// A busy meter: work in flight with no known fraction, `stats` beside it.
    #[must_use]
    pub fn busy(stats: impl AsRef<str>) -> Self {
        Self {
            stats: clip(stats.as_ref(), STATS_CAP),
            busy: true,
            ..Self::default()
        }
    }

    /// A meter with its fill clamped, its stats sanitized, an amount with no total dropped.
    #[must_use]
    pub fn normalized(self) -> Self {
        self.normalized_measured(false)
    }

    /// [`Self::normalized`].
    #[must_use]
    pub(crate) fn normalized_measured(self, measured: bool) -> Self {
        let amount = self.amount.filter(|a| a.total > 0).map(|a| Amount {
            done: a.done.min(a.total),
            ..a
        });
        let fill_permille = self
            .fill_permille
            .map(|p| p.min(1000))
            .or_else(|| amount.map(Amount::permille));
        let counted = count_span(&self.stats).filter(|_| fill_permille.is_some());
        let fill_permille = match (counted, amount.is_some() || measured) {
            (Some((lo, _)), false) => Some(lo),
            (Some((lo, hi)), true) => {
                fill_permille.map(|p| p.clamp(lo, (hi / 10 * 10).saturating_sub(1).max(lo)))
            }
            (None, _) => fill_permille,
        };
        Self {
            fill_permille,
            stats: clip(&self.stats, STATS_CAP),
            amount,
            load: self.load,
            busy: self.busy && fill_permille.is_none(),
        }
    }
}

fn count_span(stats: &str) -> Option<(u16, u16)> {
    let (done, total) = count_of(stats)?;
    let at = |n: u64| {
        let (n, t) = (u128::from(n.min(total)), u128::from(total));
        u16::try_from((n * 1000 + t / 2) / t).unwrap_or(1000)
    };
    Some((at(done), at(done + 1)))
}

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

/// The continuous quantity behind a determinate fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Amount {
    /// Which run of work this is: [`Amount::series_of`] the deliverable's label.
    pub series: u64,
    /// How much is done.
    pub done: u64,
    /// How much the deliverable is.
    pub total: u64,
    /// What `done` and `total` count.
    pub unit: Unit,
}

impl Amount {
    /// The series id of a deliverable's label (fnv-1a).
    #[must_use]
    pub fn series_of(label: &str) -> u64 {
        let mut h = Fnv::new();
        h.str(label);
        h.finish()
    }

    /// `done / total` in permille, rounded to nearest.
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

/// What an [`Amount`] counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Bytes moved.
    Bytes,
    /// Items done (programs, files).
    Items,
    /// Steps of a fixed plan.
    Steps,
    /// [`Unit::Steps`] at a phase of the plan that holds the fill with no meter of its own.
    HeldSteps,
}

impl Unit {
    /// The wire's word for it.
    #[inline]
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Items => "items",
            Self::Steps | Self::HeldSteps => "steps",
        }
    }

    /// What the count is, for the estimator.
    #[inline]
    #[must_use]
    pub(crate) const fn count(self) -> Self {
        match self {
            Self::HeldSteps => Self::Steps,
            other => other,
        }
    }

    /// The inverse of [`Unit::word`] (the wire's `unit=`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Bytes, Self::Items, Self::Steps]
            .into_iter()
            .find(|u| u.word() == s)
    }
}

/// The resource a very heavy phase loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Load {
    /// A large download.
    Network,
    /// Extraction or linking of a large tree.
    Disk,
    /// Verification of a large payload.
    Cpu,
}

impl Load {
    /// Every load, for the slot that must fit the widest words.
    pub const ALL: [Load; 3] = [Self::Network, Self::Disk, Self::Cpu];

    /// The band's words for it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Network => "network busy",
            Self::Disk => "disk busy",
            Self::Cpu => "CPU busy",
        }
    }

    /// The one-word resource.
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Disk => "disk",
            Self::Cpu => "cpu",
        }
    }

    /// The inverse of [`Load::word`] (the wire's `load=`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.word() == s)
    }
}

/// A set of [`Load`]s: the kinds a row may show over its life, which size its reserved load slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Loads(u8);

impl Loads {
    /// No load.
    pub(crate) const NONE: Self = Self(0);
    /// Every load.
    pub const ALL: Self = Self(0b0111);

    const fn bit(l: Load) -> u8 {
        match l {
            Load::Network => 1,
            Load::Disk => 2,
            Load::Cpu => 4,
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

    /// The set a row reserves once it shows `l`.
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

    /// The origin for a log word.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "host" => Some(Self::Host),
            "wire" => Some(Self::Wire),
            "carried" => Some(Self::Carried),
            _ => None,
        }
    }

    /// The wire owns a message minted by it or keyed in its namespace.
    #[must_use]
    pub fn wire_owned(self, key: Option<&str>) -> bool {
        self == Self::Wire || key.is_some_and(|k| k.starts_with(crate::WIRE_KEY_PREFIX))
    }
}

/// The wall clock at ingress, minted by the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct WallStamp {
    /// Milliseconds since the Unix epoch.
    pub unix_ms: u64,
}

/// One message as a reporter posts it.
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
    /// The title's short form.
    pub short_title: Option<String>,
    /// ≤ [`DETAIL_LINES_CAP`] × [`DETAIL_LINE_CAP`], no line empty.
    pub detail: Vec<String>,
    /// ≤ [`MAX_ACTIONS`].
    pub actions: Vec<Intent>,
    /// How long it stays.
    pub hold: Hold,
    /// A live meter.
    pub meter: Option<Meter>,
    /// Supersede key: a new post with the same key replaces the live one in its slot.
    pub key: Option<String>,
    /// Where it came from.
    pub origin: Origin,
    /// Whether the band paints `detail[0]` beside the title (default true).
    pub excerpt: bool,
    /// `None`: eligible for the glass at post.
    pub reveal_after: Option<Duration>,
    /// The words the row finishes with, which its Complete echo paints in the title's place.
    pub finished: Option<String>,
    /// The loads the row may show over its life, declared by its reporter.
    pub loads: Loads,
    /// The load this work is: a download's network, a removal's disk, a build's CPU.
    pub primary: Option<Load>,
    /// A record about a previous run (`aterm crashed last time`).
    pub retrospective: bool,
    /// What the message is to the person ([`Need`]).
    pub(crate) need: Need,
}

impl Message {
    /// A message with the severity's glyph and [`Hold::Default`], not yet classified.
    #[must_use]
    pub fn new(tag: Tag, severity: Severity, title: impl AsRef<str>) -> Self {
        Self {
            tag,
            glyph: severity.default_glyph(),
            severity,
            title: clip(title.as_ref(), TITLE_CAP),
            short_title: None,
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
            need: Need::Unclassified,
        }
    }

    /// A record: the log only, never on any glass.
    #[must_use]
    pub fn record(mut self) -> Self {
        self.need = Need::Record;
        self.hold = Hold::LogOnly;
        self
    }

    /// Work in flight: a live row that ends with the work (`stale_after` is its silence cap).
    #[must_use]
    pub fn working(mut self, stale_after: Duration) -> Self {
        self.need = Need::Working;
        self.hold = Hold::Live { stale_after };
        self
    }

    /// One of the closed list of transient rows that ask nothing ([`NoticeKind`]).
    #[must_use]
    pub fn notice(mut self, kind: NoticeKind) -> Self {
        self.need = Need::Notice(kind);
        self.hold = Hold::For(kind.span());
        self
    }

    /// The owner is needed now.
    #[must_use]
    pub fn now(mut self, ask: Ask) -> Self {
        self.hold = ask.hold();
        if ask.is_standing() && self.reveal_after.is_none() {
            self.reveal_after = Some(crate::PROGRESS_GRACE);
        }
        self.lead_with(&ask.why().words(), ask.then().words());
        if let Some(mover) = ask.then().mover() {
            self.actions.retain(|a| a != mover.intent());
            self.actions.insert(0, mover.intent().clone());
            self.actions.truncate(MAX_ACTIONS);
        }
        self.need = Need::Now(ask);
        self
    }

    /// The owner is needed, but not now.
    #[must_use]
    pub fn later(mut self, ask: LaterAsk) -> Self {
        self.hold = Hold::LogOnly;
        self.lead_with(&ask.why_words(), ask.then().words());
        if let Some(mover) = ask.then().mover() {
            self.actions.retain(|a| a != mover.intent());
            self.actions.insert(0, mover.intent().clone());
            self.actions.truncate(MAX_ACTIONS);
        }
        self.key = Some(ask.fingerprint().as_str().to_string());
        self.need = Need::Later(ask);
        self
    }

    /// What the message is to the person.
    #[must_use]
    pub const fn need(&self) -> &Need {
        &self.need
    }

    /// `true` when the message keeps a band row past a glance.
    #[must_use]
    pub const fn persists(&self) -> bool {
        self.hold.persists()
    }

    /// `true` when the row shows work in flight.
    #[must_use]
    pub(crate) fn has_indicator(&self) -> bool {
        self.meter
            .as_ref()
            .is_some_and(|m| m.fill_permille.is_some() || m.busy)
    }

    fn lead_with(&mut self, why: &str, then: &str) {
        if !self.detail.is_empty() {
            return;
        }
        let why = clip(why, DETAIL_LINE_CAP);
        let then = clip(then, DETAIL_LINE_CAP);
        self.detail.retain(|l| *l != why && *l != then);
        self.detail.insert(0, then);
        self.detail.insert(0, why);
        self.detail.truncate(DETAIL_LINES_CAP);
    }

    /// The door: make `hold` and `need` agree, and say what it did ([`Admission`]).
    #[must_use]
    pub(crate) fn admit(&mut self) -> Admission {
        let demote = |m: &mut Self| {
            m.hold = Hold::LogOnly;
            m.need = Need::Record;
            Admission::Demoted
        };
        match &self.need {
            Need::Record => {
                self.hold = Hold::LogOnly;
                Admission::Kept
            }
            Need::Later(ask) => {
                self.hold = Hold::LogOnly;
                if ask.is_complete() {
                    Admission::Kept
                } else {
                    self.need = Need::Record;
                    Admission::Demoted
                }
            }
            Need::Working => {
                if !matches!(self.hold, Hold::Live { .. }) || !self.has_indicator() {
                    return demote(self);
                }
                Admission::Kept
            }
            Need::Notice(kind) => {
                self.hold = Hold::For(kind.span());
                Admission::Kept
            }
            Need::Now(ask) => {
                let keyed = !ask.is_standing() || ask.is_adopted() || self.key.is_some();
                let declines_only =
                    ask.hold().persists() && !ask.is_adopted() && self.declines_only();
                if ask.is_complete() && keyed && !declines_only {
                    if !ask.is_adopted() {
                        self.hold = ask.hold();
                    }
                    Admission::Kept
                } else {
                    demote(self)
                }
            }
            Need::Unclassified => {
                let carried = self.origin == Origin::Carried;
                match self.hold {
                    Hold::Standing(_) | Hold::Ask { .. } if carried => {
                        self.need =
                            Need::Now(Ask::adopted(self.detail.get(1).map_or("", String::as_str)));
                        Admission::Kept
                    }
                    Hold::Standing(_) | Hold::Ask { .. } => demote(self),
                    Hold::For(d) if d > MAX_TRANSIENT && !carried => {
                        self.hold = Hold::For(MAX_TRANSIENT);
                        Admission::Demoted
                    }
                    Hold::Live { .. }
                        if !carried && self.severity >= Severity::Warn && !self.has_indicator() =>
                    {
                        demote(self)
                    }
                    _ => Admission::Kept,
                }
            }
        }
    }

    fn declines_only(&self) -> bool {
        !self.actions.is_empty() && self.actions.iter().all(Intent::is_snooze)
    }

    /// The loads this row may show over its life.
    #[must_use]
    pub fn loads(mut self, loads: impl IntoIterator<Item = Load>) -> Self {
        self.loads = loads.into_iter().collect();
        self
    }

    /// The load this work is ([`Message::primary`]).
    #[must_use]
    pub const fn primary_load(mut self, l: Load) -> Self {
        self.primary = Some(l);
        self.loads = self.loads.with(l);
        self
    }

    /// A record about a previous run ([`Message::retrospective`]).
    #[must_use]
    pub const fn retrospective(mut self) -> Self {
        self.retrospective = true;
        self
    }

    /// The band paints the title alone.
    #[must_use]
    pub const fn no_excerpt(mut self) -> Self {
        self.excerpt = false;
        self
    }

    /// The words the row finishes with, where the closed participle table would read wrong.
    #[must_use]
    pub fn finished_as(mut self, words: impl AsRef<str>) -> Self {
        let w = clip(words.as_ref(), TITLE_CAP);
        self.finished = (!w.is_empty()).then_some(w);
        self
    }

    /// The title's short form for the band.
    #[must_use]
    pub fn short_title(mut self, words: impl AsRef<str>) -> Self {
        let w = clip(words.as_ref(), TITLE_CAP);
        self.short_title = (!w.is_empty()).then_some(w);
        self
    }

    /// The title a Complete echo paints.
    #[must_use]
    pub fn finished_title(&self) -> String {
        self.finished_words().unwrap_or_else(|| self.title.clone())
    }

    /// The row's words in the past tense, when there are any.
    #[must_use]
    pub fn finished_words(&self) -> Option<String> {
        match &self.finished {
            Some(w) => Some(w.clone()),
            None => crate::words::finished_form(&self.title),
        }
    }

    /// Not on the glass until `d` after the post (the progress grace).
    #[must_use]
    pub const fn reveal_after(mut self, d: Duration) -> Self {
        self.reveal_after = Some(d);
        self
    }

    /// Declares the row's indicator.
    #[must_use]
    pub fn in_flight(mut self) -> Self {
        let m = self.meter.get_or_insert_default();
        m.busy = m.fill_permille.is_none();
        self
    }

    /// One detail line (sanitized, capped; dropped past the line cap).
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

    /// A sentence received from elsewhere (an updater's error, a pass's cause).
    #[must_use]
    pub fn sentence(self, s: impl AsRef<str>) -> Self {
        self.lines(crate::text::split_sentence(s.as_ref(), DETAIL_LINE_CAP))
    }

    /// An authored intent.
    #[must_use]
    pub fn action(mut self, i: Intent) -> Self {
        if i != Intent::Details && self.actions.len() < MAX_ACTIONS {
            self.actions.push(i);
        }
        self
    }

    /// The hold (a persistent one cannot be written outside this crate).
    #[must_use]
    pub fn hold(mut self, h: Hold) -> Self {
        self.hold = h;
        #[cfg(any(test, debug_assertions))]
        if h.persists() && matches!(self.need, Need::Unclassified) {
            self.need = Need::Now(Ask::adopted("test"));
        }
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
    pub const fn glyph(mut self, g: Glyph) -> Self {
        self.glyph = g;
        self
    }

    /// The origin.
    #[must_use]
    pub(crate) const fn origin(mut self, o: Origin) -> Self {
        self.origin = o;
        self
    }

    /// Applied by `post`: sanitize and cap title/detail/key/stats, drop empty detail lines.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.title = clip(&self.title, TITLE_CAP);
        self.short_title = self
            .short_title
            .as_deref()
            .map(|w| clip(w, TITLE_CAP))
            .filter(|w| !w.is_empty());
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

    /// `true` for a decision row.
    #[must_use]
    pub fn is_ask(&self) -> bool {
        matches!(self.hold, Hold::Ask { .. }) || self.actions.iter().any(Intent::is_ask)
    }
}

/// An in-place change to a live row.
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
    pub finished: Option<Option<String>>,
    /// The new title's short form ([`Message::short_title`]; `Some(None)` clears it).
    pub short_title: Option<Option<String>>,
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

    #[test]
    fn tags_and_glyphs_are_closed_sets() {
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

    #[test]
    fn labels_are_one_table() {
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
            assert!(
                !(intent.is_consequential() && intent.closes_row()),
                "{intent:?}: a consequential press never closes its row"
            );
            assert!(
                !(intent.is_decline() && intent.is_consequential()),
                "{intent:?}"
            );
        }
        let page = Intent::OpenSettings {
            route: "/security".into(),
        };
        let pane = Intent::OpenSystemPane {
            pane: "developer-tools".into(),
        };
        assert_eq!((page.label(), page.short()), ("Security", "Security"));
        assert_ne!(page.label(), pane.label());
        assert_ne!(page.short(), pane.short());
        assert!(Intent::ApplyUpdate { build: 1 }.is_consequential());
        assert!(
            !Intent::OpenSettings {
                route: "/packages".into()
            }
            .is_consequential()
                && !Intent::Details.is_consequential(),
            "navigations are the quiet chip"
        );
    }

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
            "review-vault-sync:1",
            "show-session:0:1:s-x",
            "show-session:1:no:s-x",
            "show-session:1:1:",
            "show-session:1:1",
            "show-session:65536:1:s-x",
            "restart-service:",
            "restart-service:a b",
            "restart-service:../x",
            "restart-service:seventeen-letters",
        ] {
            assert_eq!(Intent::decode(junk), None, "{junk:?}");
        }
        let restart = Intent::RestartService {
            service: "fseventsd".into(),
        };
        assert_eq!(restart.encode(), "restart-service:fseventsd");
        assert!(restart.is_consequential() && !restart.ends_with_row() && !restart.is_snooze());
        assert_eq!(
            Intent::decode("open-config-editor:-"),
            Some(Intent::OpenConfigEditor { line: None })
        );
        assert_eq!(
            Intent::decode("show-tab:2:7"),
            Some(Intent::ShowTab { tab: 2, window: 7 })
        );
        assert_eq!(
            Intent::decode("review-vault-sync"),
            Some(Intent::ReviewVaultSync),
            "records already written name it so"
        );
        let stable = Intent::ShowSession {
            tab: 2,
            window: 7,
            session: "s-a:b%\t".into(),
        };
        assert_eq!(Intent::decode(&stable.encode()), Some(stable));
        for retired in ["not-now:admin-step:clt,brew", "install-elevated:clt,brew"] {
            assert_eq!(Intent::decode(retired), None, "{retired:?}");
        }
        for hold in [
            Hold::Default,
            Hold::For(Duration::from_secs(9)),
            Hold::Live {
                stale_after: Duration::from_secs(30),
            },
            Hold::STANDING,
            Hold::asking(Duration::from_secs(600)),
            Hold::LogOnly,
        ] {
            assert_eq!(Hold::decode(&hold.encode()), Some(hold));
        }
        assert_eq!(Hold::decode("forever"), None);
        assert_eq!(Hold::decode("for:x"), None);
    }

    #[test]
    fn an_open_link_decodes_only_an_https_page() {
        for url in [
            "https://chatgpt.com",
            "https://chatgpt.com/",
            "https://auth.openai.com/setup?step=2&x=a:b#mfa",
            "https://xn--bcher-kva.example/%7Euser",
        ] {
            let link = Intent::OpenLink { url: url.into() };
            assert_eq!(Intent::decode(&link.encode()), Some(link), "{url}");
        }
        assert_eq!(
            link_host("https://Auth.OpenAI.com/x"),
            Some("Auth.OpenAI.com")
        );
        let long = format!("https://example.com/{}", "a".repeat(LINK_URL_CAP));
        let refused = [
            "http://chatgpt.com/",
            "HTTPS://chatgpt.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "mailto:someone@example.com",
            "https:chatgpt.com",
            "https:///path",
            "https://chatgpt.com:443/",
            "https://user@chatgpt.com/",
            "https://user:pw@chatgpt.com/",
            "https://good.com\\@evil.com/",
            "https://[::1]/",
            "https://localhost/",
            "https://chatgpt.com./",
            "https://.chatgpt.com/",
            "https://chat..gpt.com/",
            "https://-chatgpt.com/",
            "https://chat_gpt.com/",
            "https://chatgpt.com/a b",
            "https://chatgpt.com/\u{7f}",
            "https://chatgpt.com/\u{e9}",
            "https://chatgpt.com/\"x",
            long.as_str(),
        ];
        for url in refused {
            assert_eq!(link_host(url), None, "{url:?}");
            let forged = format!("open-link:{}", escape_payload(url));
            assert_eq!(Intent::decode(&forged), None, "{forged:?}");
        }
        assert_eq!(Intent::decode("open-link:"), None);
        assert_eq!(Intent::decode("open-link"), None);
        let link = Intent::OpenLink {
            url: "https://chatgpt.com/".into(),
        };
        assert_eq!((link.label(), link.short()), ("Open in browser", "Open"));
        assert!(!link.is_consequential() && !link.is_snooze() && !link.ends_with_row());
    }

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
            short_title: Some("\u{1b}Short".into()),
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
            need: Need::Unclassified,
        }
        .normalized();
        assert_eq!(literal.finished.as_deref(), Some("Done"));
        assert_eq!(literal.short_title.as_deref(), Some("Short"));
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
        assert_eq!(MessageId::from_raw(0), None);
        assert_eq!(MessageId::from_raw(7).map(MessageId::raw), Some(7));
        assert_eq!(MessageId::FIRST.next().raw(), 2);
    }
}
