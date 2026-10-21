// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure `fn … -> Message` builders for config and runtime notices. Nothing here
//! reads a clock, a window or `App`: a site that runs before `App` exists or off
//! the loop thread queues what these build on the pre-App inbox
//! ([`crate::message_inbox::queue_message`]); a site on the loop posts it
//! (`App::post_message`).
//!
//! # The owner's attention rule
//!
//! The glass carries only three things ([`attention`]): work in flight with an
//! animated indicator that says what is coming and how long (P, progress),
//! a genuine decision (D), or a failure the person must act on (F).
//! Everything else — confirmations, "done", FYI, precautionary notices,
//! disclosures, repeats — is a [`Hold::LogOnly`] record that Settings ▸
//! Messages and `messages.log` keep. A glass title is a few words, verb- or
//! noun-first, with no clauses ([`aterm_messages::GLASS_TITLE_WORDS`], [`GLASS_TITLE_CHARS`]);
//! `detail[0]` is painted only when it changes what the person does
//! (`Message::no_excerpt` otherwise); every other word waits behind Details.
//!
//! # One message per config family
//!
//! A launch or reload collects its sentences into [`ConfigWarnings`], one
//! bucket per [`ConfigFamily`], and each bucket becomes one message: a terse
//! count title ([`ConfigFamily::title`], singular for one), the head of the
//! first sentence as `detail[0]` ([`excerpt_head`]), and every sentence whole
//! behind it (up to the engine's line cap; past it, the last line is the
//! roll-up that names the validator). Each family has a supersede key under
//! `config.`, so a reload replaces the family's row rather than stacking a
//! second one. A reload that re-derives every family has every live
//! `config.` row in scope; the content-equal reload, which re-derives only
//! the two text-read families, scopes to those keys
//! (`App::replace_config_messages_keyed`). Within the scope a row whose
//! family comes back in the same words stands, a gone or changed family is
//! resolved, and the same words coming back after their row left the glass
//! are a record — a reload runs on every Settings toggle, and a repeat is
//! the log's (`App::replace_config_set`). The
//! restart-only family and the retired spellings are records.
//!
//! # The crash message
//!
//! The artifact's absolute path rides behind Details — the log keeps it
//! whole — with its first lines (`logging::CrashEvidence`); `Open log` opens
//! it as text (`Intent::OpenPath`, re-validated at press time). The row
//! paints its title alone.
//!
//! # A failed gesture
//!
//! A gesture's failure, its decisions (Full Disk Access), their follow-ups and
//! disclosures are sorted by [`attention`]: a gesture the person made that did
//! not happen is [`gesture_failure`] (a few words for a title, the whole error
//! behind Details, a short `HOLD_GESTURE`).

use std::time::Duration;

use aterm_agent::supervise::policy::exception::{Exception, Urgency};
use aterm_agent::supervise::policy::notice;
use aterm_agent::supervise::policy::outlook::ExceptionKind;
use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{
    Ask, Mover, NoticeKind, Reason, Resurface, Span, StandingWhy, Then, Why,
};
use aterm_messages::{
    DETAIL_LINES_CAP, Glyph, Hold, Intent, Message, Severity, TITLE_CAP, Tag, tags,
};
#[cfg(test)]
use aterm_messages::{Decision, HOLD_GESTURE};

use crate::crash_journal::PanesWithoutFolder;
use crate::harness_host::DiskPrivate;
use crate::native_settings::SettingsRoute;

/// What earns a row on the glass (the owner's attention rule).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attention {
    /// Work in flight, with its animated indicator.
    Progress,
    /// A genuine decision: the row asks, or carries a consequential capsule.
    Decision,
    /// A failure the person must act on.
    Failure,
    /// The answer to the person's own press (`Upgrade now`): a calm notice that
    /// folds into the log by itself after [`ANSWER_MAX`].
    Answer,
    /// Everything else: a record, never on the glass.
    Record,
}

/// Glass titles: a few words, verb- or noun-first, no clauses — one copy, the
/// engine's, so the wire's `notice` and every builder here meet the same form.
use aterm_messages::GLASS_TITLE_CHARS;

/// Classify `msg`, or say why it may not be on the glass: a record is a
/// record; a confirmation, an FYI and progress with no indicator are not
/// allowed on the glass; a live row with its indicator — a fill or busy
/// (the indicator is the meter's state, never the hold's) — is
/// progress, and may carry a consequential capsule only when it stops that
/// row's own work (`Intent::stops_work`); a live row that waits on the
/// person (Warn or Error, still) is refused — it persists without saying why, so
/// the door records it and what it asked is `.now(Ask)`; an ask or a consequential capsule is a
/// decision; a warning or an error a failure.
/// Every non-record title is at most [`aterm_messages::GLASS_TITLE_WORDS`] words and
/// [`GLASS_TITLE_CHARS`] characters, carries no clause seam (` — `, `; `,
/// `: `) and does not end in a period.
#[cfg(test)]
pub(crate) fn attention(msg: &Message) -> Result<Attention, &'static str> {
    if msg.hold == Hold::LogOnly {
        return Ok(Attention::Record);
    }
    if msg.severity == Severity::Success {
        return Err("a confirmation on glass");
    }
    let indicator = msg
        .meter
        .as_ref()
        .is_some_and(|m| m.fill_permille.is_some() || m.busy);
    let class = if matches!(msg.hold, Hold::Live { .. }) {
        if indicator {
            // Work in flight is no decision, except a capsule that stops the
            // row's own work (`Stop paste`).
            if msg
                .actions
                .iter()
                .any(|i| i.is_consequential() && !i.stops_work())
            {
                return Err("a decision on work in flight");
            }
            Attention::Progress
        } else if msg.severity >= Severity::Warn {
            // A live row blocked on the person persists without saying why:
            // the door records it (`Message::admit`), so it is not a shape a
            // reporter may build. What it asked is `.now(Ask)`.
            return Err("a blocked live row that names no ask");
        } else {
            return Err("progress with no indicator");
        }
    } else if msg.is_ask() || msg.actions.iter().any(Intent::is_consequential) {
        Attention::Decision
    } else if matches!(msg.severity, Severity::Warn | Severity::Error) {
        Attention::Failure
    } else if msg.severity == Severity::Info
        && matches!(msg.hold, Hold::For(held) if held <= ANSWER_MAX)
        && msg.actions.is_empty()
    {
        Attention::Answer
    } else {
        return Err("an FYI on glass");
    };
    if let Some(fault) = aterm_messages::text::glass_title_fault(&msg.title) {
        return Err(fault);
    }
    Ok(class)
}

/// The hold of the two launch-time rows (the crash message, a launch load
/// failure): long enough to read a crash log's head or a load failure's
/// path and go and fix it, short enough that a window left open all day is
/// not wearing last week's crash.
pub(crate) const HOLD_LAUNCH: Duration = Duration::from_secs(120);

/// The supersede key of the crash message: one per launch, and a launch
/// posts at most one.
pub(crate) const KEY_CRASH: &str = "crash.last";

/// The supersede key of the launch load failure.
pub(crate) const KEY_LAUNCH_LOAD: &str = "config.launch-load";

/// The supersede key of the GPU-lost row: Standing until a window
/// created after the loss paints (`App::finalize_successful_present`).
pub(crate) const KEY_GPU_LOST: &str = "render.gpu-lost";

/// The supersede key of the dead accessibility publisher. Its producer
/// is the panic hook of a build with an accessibility tree; the builder is
/// platform-neutral and this build's tests still exercise it. Never
/// gated: a seamless successor withdraws a carried row under it whatever it
/// was built with (`messages_host::describes_the_old_process`).
pub(crate) const KEY_A11Y_PUBLISHER: &str = "a11y.publisher";

/// The supersede key of the Windows backdrop family: every backdrop
/// decline is one record, whichever site declined it first.
#[cfg(any(windows, test))]
pub(crate) const KEY_BACKDROP: &str = "render.backdrop";

/// The supersede key of the Serious Mode write feedback.
pub(crate) const KEY_SERIOUS_MODE: &str = "config.serious-mode";

/// The supersede key of the native config lane's own errors.
pub(crate) const KEY_CONFIG_LANE: &str = "config.lane";

/// The supersede key of a refused or fleet-owned hold.
pub(crate) const KEY_FABRIC_HOLD: &str = "fabric.hold";

/// The key the Full Disk Access question, its route words and its grant
/// share: one question per process, restated in place after *Open
/// Settings*, answered by *Not now* or resolved by the grant. The card's
/// policy switch resolves the whole `privacy.` prefix.
pub(crate) const KEY_FILE_ACCESS: &str = "privacy.fda";

/// The key the debugger-access records share: each one replaces
/// the last. Records only — nothing under it is ever a live row, so the
/// FDA card's `privacy.` prefix retraction has nothing of it to resolve.
pub(crate) const KEY_DEBUGGER_ACCESS: &str = "privacy.debugger";

/// The `Intent::OpenSystemPane` pane name of the Full Disk Access list
/// (`messages_host::privacy_pane`).
pub(crate) const PANE_FULL_DISK_ACCESS: &str = "full-disk-access";

/// The `Intent::OpenSystemPane` pane name of the Developer Tools list
/// (`messages_host::privacy_pane`), where the owner switches aterm on.
pub(crate) const PANE_DEVELOPER_TOOLS: &str = "developer-tools";

/// The key of the install-posture row: one per launch.
#[cfg(any(target_os = "macos", test))]
pub(crate) const KEY_INSTALL_POSTURE: &str = "packages.posture";

/// The roll-up's tail when a family has more sentences than a message
/// holds lines: where the whole list lives.
const VALIDATOR_HINT: &str = "run `aterm --window --validate-config` for the full list";

/// The widest `detail[0]` a config family's excerpt is cut to at a clause
/// seam ([`excerpt_head`]); the band shapes it further by its own width law.
const EXCERPT_CAP: usize = 80;

/// The families a launch or reload sorts its config warnings into — one
/// message each. The order is the band's reading order when several are
/// posted together: what a person typed wrong first (a chord, a key, a
/// value), then what this build cannot draw, then what waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ConfigFamily {
    /// `[keybindings]` / `[key_sequences]` rules that were skipped.
    Keybindings,
    /// Keys `aterm.toml` sets that this build does nothing with.
    IgnoredKeys,
    /// Retired and deprecated spellings (`[packages] auto_update` /
    /// `seed_install`): keys aterm or atpkg still reads, or that a
    /// newer key now decides. A record, never a row: nothing is broken and
    /// nothing presses — the editor marks each one, and Settings ▸ Packages
    /// already explains an Off the retired `auto_update` holds. Kept apart from
    /// [`Self::IgnoredKeys`], whose words say "no effect in this build": told
    /// that, a person who deletes `auto_update = false` turns automatic updates
    /// back on.
    RetiredKeys,
    /// Keys spelled right whose VALUE this build does not accept.
    UnacceptedValues,
    /// `cursor_trail_style`.
    CursorTrail,
    /// Font families, faces and variation requests that did not resolve.
    Fonts,
    /// Image assets (`wallpaper`, `cursor_nyan_sprite`) that were not admitted.
    Assets,
    /// Restart-only keys a live reload cannot apply (`columns`/`lines`, `gpu`):
    /// a RECORD — the edit waits, and Settings ▸ Advanced, Modified and Manual
    /// already mark the key beside it.
    Restart,
    /// A Secure Keyboard Entry transition the OS refused.
    SecureKeyboard,
}

impl ConfigFamily {
    /// Every family, in the band's reading order.
    pub(crate) const ALL: [Self; 9] = [
        Self::Keybindings,
        Self::IgnoredKeys,
        Self::RetiredKeys,
        Self::UnacceptedValues,
        Self::CursorTrail,
        Self::Fonts,
        Self::Assets,
        Self::Restart,
        Self::SecureKeyboard,
    ];

    /// The family's supersede key: `config.<family>`, so a reload replaces
    /// the family's live row and a clean reload resolves the `config.` prefix.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Keybindings => "config.keybindings",
            Self::IgnoredKeys => "config.ignored-keys",
            Self::RetiredKeys => "config.retired-keys",
            Self::UnacceptedValues => "config.unaccepted-values",
            Self::CursorTrail => "config.cursor-trail",
            Self::Fonts => "config.fonts",
            Self::Assets => "config.assets",
            Self::Restart => "config.restart",
            Self::SecureKeyboard => "config.ske",
        }
    }

    /// A restart-only edit is information — nothing is broken, the edit
    /// simply waits — and so is a retired spelling, which says what it still
    /// does; every other family is an edit that did not take.
    const fn severity(self) -> Severity {
        match self {
            Self::Restart | Self::RetiredKeys => Severity::Info,
            _ => Severity::Warn,
        }
    }

    /// The family's hold: a record for the two families that are information
    /// ([`Self::Restart`], [`Self::RetiredKeys`]), the engine's default for
    /// every edit to go and fix.
    const fn hold(self) -> Hold {
        match self {
            Self::Restart | Self::RetiredKeys => Hold::LogOnly,
            _ => Hold::Default,
        }
    }

    /// The ask of a family that is an edit to go and fix: why the person is
    /// needed and the one action, a flash at the warning's span
    /// ([`Span::Warn`], the 45 s [`Hold::Default`] gives a warning). The
    /// action is the row's own `Open aterm.toml`; Secure Keyboard Entry's
    /// setting is right, macOS refused it, so its step repeats the change.
    fn ask(self) -> Ask {
        let (why, step) = match self {
            Self::Keybindings => (
                "a keybinding in aterm.toml did not take",
                "Correct the keybinding in aterm.toml",
            ),
            Self::IgnoredKeys => (
                "a setting in aterm.toml has no effect in this build",
                "Correct the setting's name in aterm.toml",
            ),
            Self::UnacceptedValues => (
                "a setting's value in aterm.toml was not accepted",
                "Correct the value in aterm.toml",
            ),
            Self::CursorTrail => (
                "the cursor trail settings in aterm.toml did not take",
                "Correct the cursor trail settings in aterm.toml",
            ),
            Self::Fonts => (
                "a font named in aterm.toml did not load",
                "Name another font in aterm.toml",
            ),
            Self::Assets => (
                "an image named in aterm.toml did not load",
                "Correct the image's path in aterm.toml",
            ),
            Self::SecureKeyboard => (
                "macOS refused to change Secure Keyboard Entry",
                "Save secure_keyboard_entry in aterm.toml again",
            ),
            Self::Restart | Self::RetiredKeys => (
                "a setting in aterm.toml did not take",
                "Correct it in aterm.toml",
            ),
        };
        Ask::flash(
            Why::YourAction(Reason::new(why)),
            Then::press(Mover::open_config_editor(None), step),
            Span::Warn,
        )
    }

    /// The family's title for `n` sentences: a few words, singular for one.
    /// The restart-only family is a record and keeps its sentence (one) or
    /// its count (several).
    pub(crate) fn title(self, n: usize) -> String {
        let one = n == 1;
        match self {
            Self::Keybindings if one => "Keybinding skipped".to_string(),
            Self::Keybindings => format!("{n} keybindings skipped"),
            // `config` is the row's tag and its `Open aterm.toml` capsule
            // already: the title spends its cells on nothing else, so the
            // excerpt's near miss (`windw_padding → window_padding?`) fits
            // at 80 columns. `Misspelled setting` is used where the validator
            // has a near miss ([`config_family_message`] reads the sentences),
            // `Unknown setting` otherwise; a key a removed feature left behind
            // is the retired family's record, never this row.
            Self::IgnoredKeys if one => "Unknown setting".to_string(),
            Self::IgnoredKeys => format!("{n} unknown settings"),
            Self::RetiredKeys if one => "Retired setting".to_string(),
            Self::RetiredKeys => format!("{n} retired settings"),
            Self::UnacceptedValues if one => "Couldn't use a setting's value".to_string(),
            Self::UnacceptedValues => format!("Couldn't use {n} settings' values"),
            Self::CursorTrail if one => "Couldn't apply the cursor trail".to_string(),
            // A count of the family's sentences — not "settings" — and short
            // enough that a 60-column row keeps its `Open aterm.toml` beside
            // `Value not accepted`'s.
            Self::CursorTrail => format!("{n} errors in cursor trail settings"),
            Self::Fonts if one => "Couldn't apply the font".to_string(),
            Self::Fonts => format!("Couldn't apply {n} fonts"),
            Self::Assets if one => "Couldn't load the image".to_string(),
            Self::Assets => format!("Couldn't load {n} images"),
            Self::Restart if one => "A setting applies after restart".to_string(),
            Self::Restart => format!("{n} settings apply after restart"),
            Self::SecureKeyboard => "Couldn't change Secure Keyboard Entry".to_string(),
        }
    }

    /// The words a family's row is logged under once a clean reload resolves
    /// it: `Misspelled setting fixed`, `Font applied`.
    /// `None` for the two records, which are never resolved.
    pub(crate) fn fixed_title(self, title: &str, n: usize) -> Option<String> {
        let one = n == 1;
        Some(match self {
            // `Misspelled setting fixed`, `3 unknown settings fixed`.
            Self::IgnoredKeys => format!("{title} fixed"),
            Self::Keybindings if one => "Keybinding fixed".to_string(),
            Self::Keybindings => format!("{n} keybindings fixed"),
            Self::UnacceptedValues if one => "Setting's value fixed".to_string(),
            Self::UnacceptedValues => format!("{n} settings' values fixed"),
            // These three may be fixed by taking the setting out as well as by a
            // value that now takes, so the words declared at post hold either
            // way; a reload that saw which it was says so instead
            // ([`config_fix_words`]).
            Self::CursorTrail => "Cursor trail problem fixed".to_string(),
            Self::Fonts if one => "Font problem fixed".to_string(),
            Self::Fonts => format!("{n} font problems fixed"),
            Self::Assets if one => "Image problem fixed".to_string(),
            Self::Assets => format!("{n} image problems fixed"),
            Self::SecureKeyboard => "Secure Keyboard Entry changed".to_string(),
            Self::Restart | Self::RetiredKeys => return None,
        })
    }

    /// The first sentence's excerpt — what changes what the person does,
    /// as a fragment, never a sentence cut mid-thought: Secure Keyboard Entry's
    /// state (the clause after its last ` — `); an unknown key as the key and
    /// its near miss (`windw_padding → window_padding?`, or the bare key), the
    /// line number behind Details; a skipped chord as `ctrl+x: no action foo`;
    /// the head of every other family's sentence at a clause seam.
    fn excerpt(self, first: &str) -> String {
        match self {
            Self::SecureKeyboard => first
                .rsplit_once(" \u{2014} ")
                .map_or(first, |(_, state)| state.trim())
                .to_string(),
            Self::IgnoredKeys => unknown_key_excerpt(first)
                .unwrap_or_else(|| excerpt_head(first, EXCERPT_CAP).to_string()),
            Self::Keybindings => {
                let chord = first
                    .replace(": unknown action ", ": no action ")
                    .replace('"', "");
                excerpt_head(&chord, EXCERPT_CAP).to_string()
            }
            Self::Fonts => {
                font_excerpt(first).unwrap_or_else(|| excerpt_head(first, EXCERPT_CAP).to_string())
            }
            _ => excerpt_head(first, EXCERPT_CAP).to_string(),
        }
    }

    /// The prefixes a sentence sheds on its way into a message: the
    /// `config ` prefix (the row's tag says it), and the family word of a
    /// keybinding warning, whose subject is the chord, plus its `skipping `
    /// prefix.
    fn strip_prefix(self, sentence: &str) -> &str {
        let s = sentence.strip_prefix("config ").unwrap_or(sentence);
        match self {
            Self::Keybindings => {
                let s = s
                    .strip_prefix("keybindings: ")
                    .or_else(|| s.strip_prefix("key_sequences: "))
                    .unwrap_or(s);
                s.strip_prefix("skipping ").unwrap_or(s)
            }
            _ => s,
        }
    }
}

/// How a config problem a reload no longer finds ended, as that reload saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigFix {
    /// The setting it was about was taken out of `aterm.toml`.
    Removed,
    /// The setting is still there and now loads or applies.
    Applied,
}

/// The words a fixed problem's ✓ record takes, decided when the reload that
/// fixed it resolves it, from what that reload saw happen to each `subject` it
/// was about ("Wallpaper", "Cursor sprite", "Font", "Cursor trail"). `None` for a
/// family whose words declared at post ([`ConfigFamily::fixed_title`]) already
/// hold however it ended, or when nothing was seen.
pub(crate) fn config_fix_words(
    family: ConfigFamily,
    fixes: &[(&str, ConfigFix)],
) -> Option<String> {
    let (subject, how) = *fixes.first()?;
    let same = fixes.iter().all(|(_, other)| *other == how);
    Some(match family {
        ConfigFamily::Assets if fixes.len() == 1 => match how {
            ConfigFix::Removed => format!("{subject} removed"),
            ConfigFix::Applied => format!("{subject} loaded"),
        },
        ConfigFamily::Assets if same => match how {
            ConfigFix::Removed => format!("{} images removed", fixes.len()),
            ConfigFix::Applied => format!("{} images loaded", fixes.len()),
        },
        ConfigFamily::Assets => format!("{} image problems fixed", fixes.len()),
        ConfigFamily::Fonts | ConfigFamily::CursorTrail => match how {
            ConfigFix::Removed => format!("{subject} setting removed"),
            ConfigFix::Applied => format!("{subject} applied"),
        },
        _ => return None,
    })
}

/// The config warnings of one launch or one reload, sorted into families as
/// they are collected. Exact repeats within a family fold (a live reload
/// re-declining the same material must not list it twice). The sentences
/// are kept verbatim — [`ConfigWarnings::sentences`] is the stderr echo, in
/// the order they were collected, so a console launch reads what it always
/// read — and shed their prefixes only on the way into a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigWarnings {
    /// Families in first-push order, each with its sentences in push order.
    families: Vec<(ConfigFamily, Vec<String>)>,
    /// The sentences Settings says on the row of the setting they are about
    /// ([`Self::push_shown_in_settings`]): "Not found" on a font row, "Configured
    /// 500 → Default 12 px" on Font size, "Theme unavailable" on Color theme, a
    /// wallpaper that did not load on the Wallpaper page.
    settings_rows: Vec<(ConfigFamily, String)>,
}

impl ConfigWarnings {
    /// Add one sentence to `family`; an exact repeat within the family folds.
    pub(crate) fn push(&mut self, family: ConfigFamily, sentence: String) {
        let bucket = match self.families.iter_mut().find(|(f, _)| *f == family) {
            Some((_, lines)) => lines,
            None => {
                self.families.push((family, Vec::new()));
                &mut self.families.last_mut().expect("just pushed").1
            }
        };
        if !bucket.contains(&sentence) {
            bucket.push(sentence);
        }
    }

    /// [`Self::push`] a sentence that Settings also says on the row of the setting
    /// it is about, so while Settings is in front the family can be a record
    /// rather than a band row repeating that row ([`Self::settings_shows`]).
    pub(crate) fn push_shown_in_settings(&mut self, family: ConfigFamily, sentence: String) {
        if !self
            .settings_rows
            .iter()
            .any(|(f, line)| *f == family && *line == sentence)
        {
            self.settings_rows.push((family, sentence.clone()));
        }
        self.push(family, sentence);
    }

    /// [`Self::push_shown_in_settings`] for every sentence.
    pub(crate) fn extend_shown_in_settings(
        &mut self,
        family: ConfigFamily,
        sentences: impl IntoIterator<Item = String>,
    ) {
        for sentence in sentences {
            self.push_shown_in_settings(family, sentence);
        }
    }

    /// Whether Settings says EVERY sentence of `family` on a row of its own, so
    /// nothing goes unsaid when the family is a record instead of a band row. A
    /// family that mixes in any other sentence (a `[harness]` value, the cursor
    /// sprite) keeps its band row.
    pub(crate) fn settings_shows(&self, family: ConfigFamily) -> bool {
        self.families
            .iter()
            .find(|(f, _)| *f == family)
            .is_some_and(|(_, lines)| {
                !lines.is_empty()
                    && lines.iter().all(|line| {
                        self.settings_rows
                            .iter()
                            .any(|(f, shown)| *f == family && shown == line)
                    })
            })
    }

    /// The keys of every family [`Self::settings_shows`].
    pub(crate) fn settings_shown_keys(&self) -> Vec<&'static str> {
        self.families
            .iter()
            .map(|(family, _)| *family)
            .filter(|family| self.settings_shows(*family))
            .map(ConfigFamily::key)
            .collect()
    }

    /// Add every sentence of `family`.
    pub(crate) fn extend(
        &mut self,
        family: ConfigFamily,
        sentences: impl IntoIterator<Item = String>,
    ) {
        for sentence in sentences {
            self.push(family, sentence);
        }
    }

    /// Every sentence collected so far, verbatim, family by family in
    /// collection order — the stderr echo, and the `already_told` slice the
    /// unaccepted-value filter reads (a resolver that named the value it
    /// refused keeps its fuller sentence; the generic one stands down).
    pub(crate) fn sentences(&self) -> impl Iterator<Item = &str> {
        self.families
            .iter()
            .flat_map(|(_, lines)| lines.iter().map(String::as_str))
    }

    /// [`Self::sentences`] owned, for a filter that takes `&[String]`.
    pub(crate) fn told(&self) -> Vec<String> {
        self.sentences().map(str::to_owned).collect()
    }

    /// One message per family that collected anything, in collection order, as
    /// the band would show them with no Settings in front.
    pub(crate) fn into_messages(self) -> Vec<Message> {
        self.into_messages_routed(false)
    }

    /// [`Self::into_messages`] decided against what is on screen NOW: with Settings
    /// in front, a family Settings says on its own rows ([`Self::settings_shows`])
    /// is a record — kept in Settings ▸ Messages — not a band row that repeats the
    /// row and pushes the page down. The same decision is made again whenever
    /// Settings comes to the front (`App::record_config_rows_settings_shows`).
    pub(crate) fn into_messages_routed(self, settings_in_front: bool) -> Vec<Message> {
        let shown: Vec<ConfigFamily> = self
            .families
            .iter()
            .map(|(family, _)| *family)
            .filter(|family| self.settings_shows(*family))
            .collect();
        self.families
            .into_iter()
            .filter(|(_, lines)| !lines.is_empty())
            .map(|(family, lines)| {
                let msg = config_family_message(family, &lines);
                if settings_in_front && shown.contains(&family) {
                    config_record(&msg)
                } else {
                    msg
                }
            })
            .collect()
    }
}

/// A config problem's record, from the message it was posted as: the same title,
/// sentences and action, held in the log, without the painted excerpt
/// [`config_family_message`] puts first for the band (a shortening of the first
/// sentence). A record is never painted, so that line would only say the first
/// sentence twice in Settings ▸ Messages.
pub(crate) fn config_record(msg: &Message) -> Message {
    let mut record = msg.clone().record();
    let family = msg.key.as_deref().and_then(|key| {
        if key == KEY_FONT_FAMILY {
            Some(ConfigFamily::Fonts)
        } else {
            ConfigFamily::ALL
                .into_iter()
                .find(|family| family.key() == key)
        }
    });
    if let Some(family) = family
        && record.detail.len() >= 2
        && family.excerpt(&record.detail[1]) == record.detail[0]
        && record.detail[0] != record.detail[1]
    {
        record.detail.remove(0);
    }
    record
}

/// An unknown key's excerpt from the validator's sentence (its `config `
/// shed): `windw_padding → window_padding?` from `line 3: windw_padding —
/// did you mean "window_padding"? (…)`, and the bare key from `line 4: foo
/// is unknown to this build and has no effect` — the title already says
/// unknown, and the line number is the sentence's, behind Details. `None` for
/// a sentence in neither shape.
fn unknown_key_excerpt(sentence: &str) -> Option<String> {
    let s = sentence
        .strip_prefix("line ")
        .and_then(|rest| rest.split_once(": "))
        .filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map_or(sentence, |(_, rest)| rest);
    let bare = |key: &str| {
        let key = key.trim();
        key.strip_prefix("unknown key ")
            .unwrap_or(key)
            .trim_matches('"')
            .to_string()
    };
    if let Some((key, rest)) = s.split_once(" \u{2014} ")
        && let Some(near) = rest.strip_prefix("did you mean ")
    {
        let near = near.split('?').next()?.trim_matches('"');
        let key = bare(key);
        return (!key.is_empty() && !near.is_empty()).then(|| format!("{key} \u{2192} {near}"));
    }
    let (key, _) = s.split_once(" is unknown")?;
    let key = bare(key);
    (!key.is_empty()).then_some(key)
}

/// A font verdict's excerpt in a person's words:
/// `no font named Nope` from `font_family_bold: "Nope" is not an admissible
/// font (not found)` — the validator's key, quoting and `admissible` stay
/// behind Details, where the sentence is kept whole. `None` for a sentence in
/// another shape.
fn font_excerpt(sentence: &str) -> Option<String> {
    // The font catalog's own sentences: `font_family: "X" does not resolve
    // to a font file; …` and `emoji_font "X" was not admitted`.
    let head = [
        " is not an admissible font",
        " does not resolve to a font file",
        " was not admitted",
    ]
    .into_iter()
    .find_map(|tail| sentence.split_once(tail).map(|(head, _)| head))?;
    let (_, quoted) = head.split_once('"')?;
    let name = quoted
        .rsplit_once('"')
        .map_or(quoted, |(name, _)| name)
        .trim();
    (!name.is_empty()).then(|| format!("no font named {name}"))
}

/// A TOML parser's diagnostic in a person's words for the band's excerpt:
/// `line 3: missing =` where the parser names the line
/// and only missing tokens (`expected \`.\`, \`=\``), `a mistake on line 3`
/// where it names the line alone; `None` where it names no line — the row
/// then paints its title alone, and the parser's rows, caret diagram and
/// all, stay behind Details.
fn toml_words(rows: &[String]) -> Option<String> {
    let line = rows.iter().find_map(|row| {
        let (_, rest) = row.split_once("at line ")?;
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!n.is_empty()).then_some(n)
    })?;
    let missing = rows.iter().rev().find_map(|row| {
        let rest = row.trim().strip_prefix("expected ")?;
        let tokens: Vec<&str> = rest
            .split(", ")
            .flat_map(|t| t.split(" or "))
            .map(str::trim)
            .collect();
        tokens
            .iter()
            .all(|t| t.len() >= 3 && t.starts_with('`') && t.ends_with('`'))
            .then(|| {
                tokens
                    .iter()
                    .map(|t| t.trim_matches('`'))
                    .collect::<Vec<_>>()
                    .join(" or ")
            })
    });
    Some(match missing {
        Some(missing) => format!("line {line}: missing {missing}"),
        None => format!("a mistake on line {line}"),
    })
}

/// The longest head of `sentence` that ends at a clause seam — before a
/// parenthesised aside (` (`), a semicolon (`; `) or a dash (` — `), OUTSIDE
/// any parenthesis (a seam inside an aside cuts the aside open, not the
/// sentence) — and fits `cap` characters; the whole sentence when none does
/// (the band's width law shapes it from there). The seams are the ones the
/// config sentences use: the near-miss key keeps its `did you mean` and sheds
/// `(no effect in this build)`, a font sentence keeps its verdict and `(not
/// found)` and sheds `; ignored`; the plain unknown key has no seam and is its
/// own head.
pub(crate) fn excerpt_head(sentence: &str, cap: usize) -> &str {
    const SEAMS: [&str; 3] = [" (", "; ", " \u{2014} "];
    let mut best = None;
    let mut depth = 0usize;
    for (chars_before, (at, ch)) in sentence.char_indices().enumerate() {
        if chars_before > cap {
            break;
        }
        if at > 0 && depth == 0 && SEAMS.iter().any(|seam| sentence[at..].starts_with(seam)) {
            best = Some(at);
        }
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    best.map_or(sentence, |at| &sentence[..at])
}

/// One family's message: the terse count title, the first
/// sentence's excerpt as `detail[0]`, every sentence whole behind it (the
/// excerpt not repeated when it is the first sentence), and — when there are
/// more than a message holds — a last line saying how many more and where to
/// read them: a list that looks complete while it is not is worse than no
/// list. The restart-only family is a record that keeps its sentence as its
/// title when it has one.
fn config_family_message(family: ConfigFamily, sentences: &[String]) -> Message {
    let stripped: Vec<&str> = sentences.iter().map(|s| family.strip_prefix(s)).collect();
    let title = match (family, stripped.as_slice()) {
        (ConfigFamily::Restart, [only]) => restart_title(only),
        // Every unknown key has a near miss: they are misspellings.
        (ConfigFamily::IgnoredKeys, all)
            if all
                .iter()
                .all(|s| unknown_key_excerpt(s).is_some_and(|e| e.contains('\u{2192}'))) =>
        {
            match all.len() {
                1 => "Misspelled setting".to_string(),
                n => format!("{n} misspelled settings"),
            }
        }
        _ => family.title(stripped.len()),
    };
    // How the row is logged once a clean reload resolves it:
    // the problem, fixed — a ✓ record that leaves Problems, never the
    // warning's own title still standing in amber.
    let fixed = family.fixed_title(&title, stripped.len());
    let mut msg = Message::new(tags::CONFIG, family.severity(), title)
        .action(Intent::OpenConfigEditor { line: None })
        .key(family.key());
    let record = family.hold() == Hold::LogOnly;
    if record {
        msg = msg.record();
    }
    if let Some(fixed) = fixed {
        msg = msg.finished_as(fixed);
    }
    if family == ConfigFamily::Restart && stripped.len() == 1 {
        return msg.lines(stripped.iter().map(|s| (*s).to_string()));
    }
    let first = stripped.first().copied().unwrap_or_default();
    let excerpt = family.excerpt(first);
    let mut used = 0;
    // The excerpt is the painted line; a record is never painted, so there it
    // would only say the first sentence twice.
    if excerpt != first && family.hold() != Hold::LogOnly {
        msg = msg.line(excerpt);
        used += 1;
    }
    // Each sentence in its physical rows ([`diagnostic_lines`]),
    // and the budget counted in rows, not sentences: one TOML parse error is
    // a sentence and a four-row caret diagram, and counting it as one
    // line would let `Message::line`'s cap silently drop the roll-up below —
    // the list that looks complete while it is not. A slot is kept for the
    // roll-up whenever a sentence could still follow; a single sentence longer
    // than a whole message keeps its head and leaves that slot.
    let mut shown = 0;
    for (i, sentence) in stripped.iter().enumerate() {
        let mut rows = diagnostic_lines(sentence);
        let roll_up = usize::from(i + 1 < stripped.len());
        if used + rows.len() + roll_up > DETAIL_LINES_CAP {
            if shown > 0 {
                break;
            }
            rows.truncate(DETAIL_LINES_CAP.saturating_sub(used + roll_up));
        }
        used += rows.len();
        shown += 1;
        msg = msg.lines(rows);
    }
    if shown < stripped.len() {
        msg = msg.line(format!(
            "\u{2026} and {} more \u{2014} {VALIDATOR_HINT}",
            stripped.len() - shown
        ));
    }
    // An edit that did not take is the person's own action (the door's
    // `YourAction`, a flash at the warning's 45 s): asked after the lines, so
    // the rows written above stay the detail and the painted excerpt.
    if record { msg } else { msg.now(family.ask()) }
}

/// A restart-only setting's record title: the setting in a
/// person's words and when it applies — `GPU setting applies after restart`
/// for `gpu applies on next launch (…)` — the sentence behind it. A key this
/// map does not know reads as its words (`font_px` → `Font px`).
fn restart_title(sentence: &str) -> String {
    let key = sentence.split_whitespace().next().unwrap_or_default();
    let name = match key {
        "gpu" => "GPU".to_string(),
        "columns/lines" | "columns" | "lines" => "Window size".to_string(),
        other => {
            let words = other.replace(['_', '/'], " ");
            let mut chars = words.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().chain(chars).collect::<String>()
            })
        }
    };
    if name.is_empty() {
        ConfigFamily::Restart.title(1)
    } else {
        format!("{name} setting applies after restart")
    }
}

/// The key of the backend build's `font_family` verdict
/// ([`font_family_rejected`]). A config reload re-derives the family only
/// when its font work runs, and says it under `config.fonts`: its replace
/// takes this key in scope only when that work admitted the family
/// (`App::replace_config_messages_after_font_work`).
pub(crate) const KEY_FONT_FAMILY: &str = "config.font-family";

/// The `font_family` the backend build thread could not admit — a config
/// warning like the launch's font row, but said at the backend join rather
/// than with the launch's families, so it carries its own key: sharing
/// `config.fonts` would have it supersede the launch's font row and take
/// those sentences off the glass.
pub(crate) fn font_family_rejected(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()]).key(KEY_FONT_FAMILY)
}

/// A font a theme names that is not installed here: the font row, never the
/// config family's own verdict ([`KEY_FONT_FAMILY`]).
pub(crate) fn theme_font_refused(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()])
}

/// The crash row's first line: plain words for what is there to look at —
/// never the report's path. Not painted: the title says
/// what happened and `Open log` is the press.
pub(crate) const CRASH_EXCERPT: &str = "a crash report was saved";

/// The previous run died. The title says so in a few words
/// — `aterm crashed last time` — and the row paints it alone
/// ([`Message::no_excerpt`]): [`CRASH_EXCERPT`] is `detail[0]`, behind
/// `Details ›`; the artifact's absolute path is the next line (`crash log at
/// <path>`), then the file's head; `Open log` opens it as text.
pub(crate) fn crash_message(evidence: &crate::logging::CrashEvidence) -> Message {
    let path = evidence.path.display().to_string();
    Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
        .retrospective()
        .line(CRASH_EXCERPT)
        .line(format!("crash log at {path}"))
        .lines(evidence.head.iter().cloned())
        .no_excerpt()
        .action(Intent::OpenPath { path })
        .notice(NoticeKind::LastRun)
        .key(KEY_CRASH)
}

/// The killed row's first line: what is — and is not — there to look at.
pub(crate) const KILLED_EXCERPT: &str =
    "no crash report: it ended without a crash signal or a quit";

/// The installed app's previous run was killed: its crash marker was left
/// empty and unlocked, so the process ended with no fatal signal and no exit
/// path at all (Force Quit, SIGKILL, running out of memory, a system watchdog,
/// power loss). A warning, not an error: aterm did not
/// fail, something ended it — but it is the same "last time" class of fact as
/// the crash row and shares its key, so a launch shows one of the two.
/// `Open log` opens `aterm.log`, whose last lines are what the run was doing.
pub(crate) fn killed_message(evidence: &crate::logging::KillEvidence) -> Message {
    let log = evidence.log.display().to_string();
    Message::new(tags::CRASH, Severity::Warn, "aterm was stopped last time")
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(KILLED_EXCERPT)
        .line("Force Quit, running out of memory or a system watchdog ends a process this way")
        .line(format!("its last lines are in {log}"))
        .no_excerpt()
        .action(Intent::OpenPath { path: log })
        .notice(NoticeKind::LastRun)
        .key(KEY_CRASH)
}

/// What the keeper's recovery row says of the reattached screens when none came back
/// from a crash-restore region (no keeper journal, or every restore fell to the META rung).
pub(crate) const KEEPER_SCREENS: &str = "each screen started blank and its program was asked to \
                                         redraw; the scrollback before the stop is not here";

/// What the row says when every reattached screen came back exactly from its region
/// (`crash_restore`), and every line of its scrollback with it (the history ring, imported
/// below it after the restore drains): no program was asked to redraw, and nothing is
/// missing.
pub(crate) const KEEPER_SCREENS_EXACT: &str =
    "each screen was restored exactly; the scrollback came back too";

/// The row's screens line for a recovery whose crash restores landed as `restores`
/// (`crash_restore::RungCounts`) and could not bring back `history_lost` lines of the
/// restored screens' scrollback (the history joins' counts, `status history_lost=` per
/// session): every screen restored exactly; every screen restored with `M` of them
/// repainted (a loss counted, so its program was asked to redraw over it); some restored
/// and the rest started blank (the META rung: no region, or a restore that failed), each
/// counted, since "each screen was restored" would be false for the blank ones; or, with
/// no screen from a region at all, [`KEEPER_SCREENS`]. Then the scrollback: back too, or
/// the lines that did not come back counted, and a blank screen's said apart (it brought
/// none).
pub(crate) fn keeper_screens_line(
    restores: crate::crash_restore::RungCounts,
    history_lost: u64,
) -> String {
    let screens = restores.screens();
    let redraws = restores.screen.saturating_add(restores.meta);
    let asked = |n: u32| {
        if n == 1 {
            "its program was asked to redraw".to_string()
        } else {
            format!("{n} programs were asked to redraw")
        }
    };
    let lines = |n: u64| {
        if n == 1 {
            "1 line".to_string()
        } else {
            format!("{n} lines")
        }
    };
    let scrollback = match (restores.meta, history_lost) {
        (0, 0) => "the scrollback came back too".to_string(),
        (0, n) => format!("{} of the scrollback did not come back", lines(n)),
        (_, 0) => "the blank ones' scrollback is not here".to_string(),
        (_, n) => format!(
            "the blank ones' scrollback is not here, and {} of the others' did not come back",
            lines(n)
        ),
    };
    if screens == 0 {
        KEEPER_SCREENS.to_string()
    } else if redraws == 0 && history_lost == 0 {
        KEEPER_SCREENS_EXACT.to_string()
    } else if redraws == 0 {
        format!("each screen was restored exactly; {scrollback}")
    } else if restores.meta == 0 {
        format!(
            "each screen was restored ({} repainted: {}); {scrollback}",
            restores.screen,
            asked(restores.screen)
        )
    } else {
        let how = if restores.screen == 0 {
            " exactly".to_string()
        } else {
            format!(" ({} repainted)", restores.screen)
        };
        format!(
            "{} restored{how} and {} started blank ({}); {scrollback}",
            counted_were(screens, "screen"),
            restores.meta,
            asked(redraws)
        )
    }
}

/// `n` and `word` as the subject of "was"/"were": `1 screen was`, `2 screens were`.
fn counted_were(n: u32, word: &str) -> String {
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    format!(
        "{} {}",
        counted(n, word),
        if n == 1 { "was" } else { "were" }
    )
}

/// The PTY keeper's recovery row: the last window
/// crashed or was killed, and the keeper kept `n` of its sessions running;
/// this launch reattached them. It takes the crash row's slot, and so carries
/// where that row's evidence is: the crash log, or the killed run's log.
/// `End sessions` hangs every reattached shell up; `refused` offers the
/// admission turned down are counted. A screen the crash restore brought back
/// exactly lands as it was; any other lands blank or with a loss counted, and
/// its program is sent a size pulse: a program that ignores it stays as it
/// landed until it next writes (`restores`, [`keeper_screens_line`]). A
/// restored screen's scrollback comes back below it, and the lines that could
/// not are counted (`history_lost`).
pub(crate) fn keeper_recovered_message(
    n: usize,
    refused: usize,
    restores: crate::crash_restore::RungCounts,
    history_lost: u64,
    crash: Option<&crate::logging::CrashEvidence>,
    killed: Option<&crate::logging::KillEvidence>,
) -> Message {
    let mut message = Message::new(tags::CRASH, Severity::Warn, "aterm stopped unexpectedly")
        .glyph(Glyph::or_fallback('\u{21bb}'))
        .line(format!(
            "{} kept running and {} reattached",
            counted(n, "session"),
            if n == 1 { "was" } else { "were" }
        ))
        .line(keeper_screens_line(restores, history_lost));
    if refused > 0 {
        message = message.line(format!(
            "{} could not be reattached and {} ended",
            counted(refused, "session"),
            if refused == 1 { "was" } else { "were" }
        ));
    }
    if let Some(evidence) = crash {
        message = message.line(format!("crash log at {}", evidence.path.display()));
    } else if let Some(evidence) = killed {
        message = message.line(format!(
            "aterm's last lines are in {}",
            evidence.log.display()
        ));
    }
    message
        .no_excerpt()
        .action(Intent::EndRecovered)
        .notice(NoticeKind::LastRun)
        .key(KEY_CRASH)
}

/// The reopened layout's loss line: what a crash journal cannot bring back.
pub(crate) const JOURNAL_LOSS: &str =
    "the programs that were running in them, and their scrollback, did not survive";

/// The supersede key of a crash journal this launch took and could not
/// reopen: one per launch.
pub(crate) const KEY_CRASH_JOURNAL: &str = "crash.journal";

/// `n` and `word`, plural when `n` is not one (`word` takes an `s`).
fn counted(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The tail of the reopened layout's line when every program that ran was an
/// agent that resumes its conversation: nothing else was lost.
pub(crate) const JOURNAL_ONLY_SCROLLBACK: &str = "only the scrollback did not survive";

/// [`JOURNAL_ONLY_SCROLLBACK`] when a pane also could not open its folder:
/// the scrollback was not all that was lost.
pub(crate) const JOURNAL_SCROLLBACK: &str = "the scrollback did not survive";

/// The reopened layout's quiet line: every leaf sat at its
/// shell's prompt, so nothing a person ran was lost.
pub(crate) const JOURNAL_NOTHING_RAN: &str =
    "nothing was running in them: only their scrollback did not survive";

/// [`JOURNAL_NOTHING_RAN`] when a pane also could not open its folder.
pub(crate) const JOURNAL_NOTHING_RAN_FOLDERLESS: &str =
    "nothing was running in them: their scrollback did not survive";

/// `names` as a person reads a list — `vim`, `vim and claude`, `vim, claude
/// and less` — at most three named and the rest counted, then `unnamed`
/// programs whose names were not known.
fn listed(names: &[String], unnamed: usize) -> String {
    const SHOWN: usize = 3;
    let mut shown: Vec<String> = names.iter().take(SHOWN).cloned().collect();
    let others = names.len().saturating_sub(SHOWN) + unnamed;
    match (shown.is_empty(), others) {
        (_, 0) => {}
        (true, 1) => shown.push("a program".to_string()),
        (true, n) => shown.push(format!("{n} programs")),
        (false, 1) => shown.push("another program".to_string()),
        (false, n) => shown.push(format!("{n} other programs")),
    }
    match shown.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Which programs were running, in how many of the layout's tabs: `Python
/// was running in 1 of 2 tabs`.
fn journal_ran_line(lost: &crate::crash_journal::Lost, tabs: usize) -> String {
    let place = match (lost.tabs == tabs, tabs) {
        (true, 1) => "its tab".to_string(),
        (true, 2) => "both tabs".to_string(),
        (true, n) => format!("all {n} tabs"),
        (false, n) => format!("{} of {n} tabs", lost.tabs),
    };
    let what = listed(&lost.named, lost.unnamed);
    let were = if lost.named.len() + lost.unnamed == 1 {
        "was"
    } else {
        "were"
    };
    format!("{what} {were} running in {place}")
}

/// The lost case's loss line: which programs were running, in how many of
/// the reopened tabs, and that they and the scrollback did not survive.
fn journal_loss_line(lost: &crate::crash_journal::Lost, tabs: usize) -> String {
    format!(
        "{} and did not survive, nor did the scrollback",
        journal_ran_line(lost, tabs)
    )
}

/// The agents that come back, in a person's words: `aterm
/// starts Claude again on its conversation in tab 2`, `… its conversations in
/// tabs 1 and 3`, `… in 4 tabs`; `tab 2 of window 1` when the layout has more
/// than one window. `resumed` is
/// [`crate::crash_journal::Reopened::resumed_agents`]'s, one entry per agent;
/// empty for none. Worded by the agents: `aterm starts Codex again
/// on its thread …`, and `aterm starts the agents again on their
/// conversations …` for both kinds. What aterm does, said as the launch
/// starts it — never a promise of how it ends; a relaunch that
/// does not take is said by its own row ([`restored_agents_not_resumed`]).
fn journal_resumed_line(
    resumed: &[((u32, u32), aterm_agent::harness::upgrade::Agent)],
    windows: usize,
) -> String {
    const SHOWN: usize = 3;
    let mut places: Vec<(u32, u32)> = Vec::new();
    for (place, _) in resumed {
        if !places.contains(place) {
            places.push(*place);
        }
    }
    let kind = resumed
        .first()
        .map(|(_, agent)| *agent)
        .filter(|agent| resumed.iter().all(|(_, other)| other == agent));
    let (who, conversations) = match kind {
        Some(agent) => {
            let (_, what) = resume_words(agent);
            let s = if resumed.len() == 1 { "" } else { "s" };
            (agent_word(agent), format!("its {what}{s}"))
        }
        None => ("the agents", "their conversations".to_string()),
    };
    let one = |(window, tab): (u32, u32)| {
        if windows > 1 {
            format!(
                "tab {} of window {}",
                u64::from(tab) + 1,
                u64::from(window) + 1
            )
        } else {
            format!("tab {}", u64::from(tab) + 1)
        }
    };
    let place = match places.as_slice() {
        [] => return String::new(),
        [only] => one(*only),
        many if many.len() > SHOWN => format!("{} tabs", many.len()),
        [init @ .., last] if windows <= 1 => format!(
            "tabs {} and {}",
            init.iter()
                .map(|(_, tab)| (u64::from(*tab) + 1).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            u64::from(last.1) + 1
        ),
        [init @ .., last] => format!(
            "{} and {}",
            init.iter().map(|p| one(*p)).collect::<Vec<_>>().join(", "),
            one(*last)
        ),
    };
    format!("aterm starts {who} again on {conversations} in {place}")
}

/// What the reopened layout brought back, and from how many journals: `2 tabs
/// in 1 window restored in their folders from its crash journal`. The panes
/// whose folder is not here
/// ([`crate::crash_journal::Reopened::panes_without_folder`]): those whose
/// folder no shell could start in are counted, whether the home folder stood
/// in for them or not; with only panes a program held, the line claims no
/// folders.
fn journal_restored_line(
    tabs: usize,
    windows: usize,
    sources: usize,
    folders: PanesWithoutFolder,
) -> String {
    let restored = format!("{} in {}", counted(tabs, "tab"), counted(windows, "window"));
    let from = match sources {
        1 => "its crash journal".to_string(),
        n => format!("{n} crash journals"),
    };
    match folders.gone {
        0 if folders == PanesWithoutFolder::NONE => format!(
            "{restored} restored in {} from {from}",
            if tabs == 1 {
                "its folder"
            } else {
                "their folders"
            }
        ),
        0 => format!("{restored} restored from {from}"),
        1 => format!("{restored} restored from {from}; 1 pane could not open its folder"),
        n => format!("{restored} restored from {from}; {n} panes could not open their folders"),
    }
}

/// Where the reopened desk sits when the last quit's windows came back too:
/// those open first, and the crashed desk's windows follow them. Nothing with
/// no quit layout.
fn journal_behind_quit_line(quit_windows: usize, windows: usize) -> Option<String> {
    if quit_windows == 0 {
        return None;
    }
    let first = quit_windows.saturating_add(1);
    let these = if windows == 1 {
        format!("this one is window {first}")
    } else {
        format!(
            "these are windows {first} to {}",
            quit_windows.saturating_add(windows)
        )
    };
    Some(format!(
        "the {} saved when aterm last quit came back too, first; {these}",
        counted(quit_windows, "window")
    ))
}

/// The reopened layout's title when programs were lost: the one program by
/// its name, else how many.
fn journal_lost_title(lost: Option<&crate::crash_journal::Lost>) -> String {
    match lost {
        Some(lost) if lost.unnamed == 0 && lost.named.len() == 1 => {
            format!("Tabs restored, {} lost", lost.named[0])
        }
        Some(lost) if lost.unnamed + lost.named.len() == 1 => {
            "Tabs restored, a program lost".to_string()
        }
        _ => "Tabs restored, programs lost".to_string(),
    }
}

/// The tag a journal's entry files under: `crash` for a
/// crash, `session` (restore) for a kill — a kill is not aterm crashing.
fn journal_tag(class: crate::crash_journal::DeathClass) -> Tag {
    if class.crashed() {
        tags::CRASH
    } else {
        tags::SESSION
    }
}

/// The previous run ended uncleanly and its crash
/// journal (`crate::crash_journal`) reopened its layout: the windows, the tabs
/// and their folders came back, the programs and their scrollback did not. It
/// takes the crash/kill row's slot (`KEY_CRASH`, one per launch) and carries
/// what that row would have: the crash log's path and head with `Open log` on
/// it, or the killed run's log (`log`, `aterm.log` in the log dir, when no
/// crash-row evidence names one — a development start's kill is no row of its
/// own). An error for a crash, a warning for a kill, like the rows it replaces.
///
/// The journal also says which leaves had a program running
/// ([`crate::crash_journal::Reopened::lost`]). When none did, a kill lost
/// nothing a person ran — every tab came back at its prompt, in its folder
/// when the restored line says so — so it is a record (`Info`,
/// [`Hold::LogOnly`]) that says so, and the band stays quiet. A crash with
/// nothing lost keeps its row: aterm failed, and the crash log is the
/// person's to open or send; its words say nothing was running. When
/// programs ran, the row names how many tabs lost one and which programs; a
/// journal without the field (`None`) reports generic loss.
///
/// `detail[0]` — the plain sentence Settings ▸ Messages sets above the
/// technical block — says what was lost or that nothing was; how many tabs
/// came back and how aterm ended follow as details. The title names the one
/// lost program by its name. A kill files under `session`, a crash under
/// `crash`.
///
/// `relaunching` is whether this launch's supervisor host relaunches the
/// agents the layout carried
/// ([`crate::harness_host::HostHandle::relaunches_restored`]). Each one it
/// brings back on its conversation
/// ([`crate::crash_journal::Reopened::resumed_agents`]) is left out of what was
/// lost and said as coming back (`aterm starts Claude again on its
/// conversation in tab 2`): when nothing else ran, only scrollback was lost, and a kill is a
/// record; when another program was lost, the warning names that one and its
/// sentence says which agent resumes. If the relaunch then fails, the host says
/// so once ([`restored_agents_not_resumed`]).
///
/// A pane whose folder no shell can start in did not come back in it:
/// `folders` counts them, and the restored line says how many
/// rather than that every tab came back in its folder
/// ([`journal_restored_line`]). A pane a program held whose folder is not
/// here is not counted, and the line then claims no folders.
/// Either way the loss sentence drops its `only`: the scrollback was not all
/// that was lost.
pub(crate) fn journal_reopened_message(
    reopened: &crate::crash_journal::Reopened,
    crash: Option<&crate::logging::CrashEvidence>,
    killed: Option<&crate::logging::KillEvidence>,
    log: Option<&std::path::Path>,
    relaunching: bool,
    folders: PanesWithoutFolder,
    quit_windows: usize,
) -> Message {
    let (windows, tabs) = reopened.counts();
    let lost = reopened.lost_given(relaunching);
    // The places are the screen's: the last quit's windows open first, so the
    // reopened desk's window 0 is window `quit_windows` on screen, and a
    // window is named whenever the screen has more than one.
    let offset = u32::try_from(quit_windows).unwrap_or(u32::MAX);
    let places: Vec<((u32, u32), aterm_agent::harness::upgrade::Agent)> = reopened
        .resumed_agents(relaunching)
        .into_iter()
        .map(|((window, tab), agent)| ((window.saturating_add(offset), tab), agent))
        .collect();
    let resumed = journal_resumed_line(&places, quit_windows.saturating_add(windows));
    let nothing_ran = lost.as_ref().is_some_and(|lost| lost.tabs == 0);
    let quiet = nothing_ran && !reopened.class.crashed();
    let severity = if quiet {
        Severity::Info
    } else if reopened.class.crashed() {
        Severity::Error
    } else {
        Severity::Warn
    };
    let mut evidence_lines = Vec::new();
    let open = if let Some(evidence) = crash {
        let path = evidence.path.display().to_string();
        evidence_lines.push(format!("crash log at {path}"));
        evidence_lines.extend(evidence.head.iter().cloned());
        Some(path)
    } else if let Some(evidence) = killed {
        let log = evidence.log.display().to_string();
        evidence_lines.push(format!("its last lines are in {log}"));
        Some(log)
    } else {
        log.map(|log| {
            let log = log.display().to_string();
            evidence_lines.push(format!("its last lines are in {log}"));
            log
        })
    };
    let title = if quiet {
        "Tabs restored after aterm stopped".to_string()
    } else if nothing_ran {
        "Tabs restored after aterm crashed".to_string()
    } else {
        journal_lost_title(lost.as_ref())
    };
    // Only the scrollback holds while every pane came back in its folder.
    let (idle_line, scrollback_line) = if folders == PanesWithoutFolder::NONE {
        (JOURNAL_NOTHING_RAN, JOURNAL_ONLY_SCROLLBACK)
    } else {
        (JOURNAL_NOTHING_RAN_FOLDERLESS, JOURNAL_SCROLLBACK)
    };
    // The agent coming back rides the loss sentence, never a detail of its
    // own — Settings ▸ Messages sets every detail after the first as
    // technical text.
    let loss = match &lost {
        Some(lost) if lost.tabs == 0 && resumed.is_empty() => idle_line.to_string(),
        Some(lost) if lost.tabs == 0 => format!("{resumed}; {scrollback_line}"),
        None if resumed.is_empty() => JOURNAL_LOSS.to_string(),
        None => format!("{JOURNAL_LOSS}; {resumed}"),
        Some(lost) if resumed.is_empty() => journal_loss_line(lost, tabs),
        Some(lost) => format!("{}; {resumed}", journal_loss_line(lost, tabs)),
    };
    let mut msg = Message::new(journal_tag(reopened.class), severity, title).line(loss);
    msg = msg
        .line(journal_restored_line(
            tabs,
            windows,
            reopened.sources.len(),
            folders,
        ))
        .lines(journal_behind_quit_line(quit_windows, windows))
        .line(reopened.class.sentence())
        .lines(evidence_lines);
    if reopened.class.crashed() {
        msg = msg.retrospective();
    }
    if let Some(path) = open {
        msg = msg.action(Intent::OpenPath { path });
    }
    let msg = msg.no_excerpt().key(KEY_CRASH);
    if quiet {
        msg.record()
    } else {
        msg.notice(NoticeKind::LastRun)
    }
}

/// A crashed run's journal this launch took and did
/// not reopen: it could not be read (a torn file after a power loss, a newer
/// build's schema, a planted link), or the brake skipped it (the run that wrote
/// it had reopened a crash journal and crashed within 90 s, or was stopped
/// within them a second time in a row). Either way that layout did
/// not come back (an unreadable one is kept aside, and the row names where),
/// and a person who expected it back is owed the reason, what it held
/// and what ran in it, and `Open log` on `log` — the crash log when
/// there is one, else `aterm.log`.
pub(crate) fn journal_note_message(
    note: &crate::crash_journal::Note,
    log: Option<&std::path::Path>,
) -> Message {
    use crate::crash_journal::Note;
    let mut msg = match note {
        Note::Unreadable {
            class, path, error, ..
        } => Message::new(
            journal_tag(*class),
            Severity::Warn,
            "Couldn't restore your last tabs",
        )
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(format!("its crash journal could not be read: {error}"))
        .line(class.sentence())
        .lines(
            path.as_ref()
                .map(|kept| format!("journal {}", kept.display())),
        )
        .notice(NoticeKind::LastRun),
        Note::Relapsed {
            class,
            windows,
            tabs,
            lost,
            ..
        } => {
            let held = format!(
                "{} in {}",
                counted(*tabs, "tab"),
                counted(*windows, "window")
            );
            let (title, why) = if class.crashed() {
                (
                    "Couldn't restore tabs after a crash",
                    format!(
                        "{held} were not restored: aterm crashed within 90 s of restoring them, \
                         so they were skipped in case they caused it"
                    ),
                )
            } else {
                (
                    "Couldn't restore tabs after two stops",
                    format!(
                        "{held} were not restored: aterm was stopped twice in a row within 90 s \
                         of restoring them, so they were skipped in case they caused it"
                    ),
                )
            };
            let ran = match lost {
                None => JOURNAL_LOSS.to_string(),
                Some(lost) if lost.tabs == 0 => "nothing was running in them".to_string(),
                Some(lost) => journal_ran_line(lost, *tabs),
            };
            Message::new(journal_tag(*class), Severity::Warn, title)
                .glyph(Glyph::or_fallback('\u{26a0}'))
                .line(why)
                .line(ran)
                .line(class.sentence())
                .notice(NoticeKind::LastRun)
        }
    };
    if let Some(log) = log {
        let log = log.display().to_string();
        msg = msg
            .line(format!("its last lines are in {log}"))
            .action(Intent::OpenPath { path: log });
    }
    msg.no_excerpt().key(KEY_CRASH_JOURNAL)
}

/// The supersede key of the row that says restored tabs' agents did not come
/// back ([`restored_agents_not_resumed`]): one per launch.
pub(crate) const KEY_RESTORED_AGENTS: &str = "harness.restored";

/// The supersede key the window posts this process's restored agents' row
/// under (`App::post_restored_row`): [`KEY_RESTORED_AGENTS`] and the process.
/// A successor's row supersedes its own earlier rows, never the row a
/// seamless update carried from the aterm before it. That row names the tabs
/// the predecessor said at its park, which this process never said.
pub(crate) fn restored_agents_key() -> String {
    format!("{KEY_RESTORED_AGENTS}.{}", std::process::id())
}

/// The supersede key of the row that says this window supervises no agent
/// ([`harness_without_socket`]): one per launch.
pub(crate) const KEY_HARNESS_NO_SOCKET: &str = "harness.no-socket";

/// What a window with no control socket cannot do, the clause
/// [`harness_without_socket`] puts after every cause. It names no agent: it
/// holds for every one the harness supervises, as the master switch's row
/// ("Supervise coding agents") does.
pub(crate) const HARNESS_BOXES_UNANSWERED: &str =
    "aterm cannot answer its coding agents' permission boxes here";

/// The fix for a launch that found no folder for its control socket: the
/// variables `aterm_uds::control_socket_dir` reads, or the flag that names a
/// socket outright.
#[cfg(not(windows))]
pub(crate) const SOCKET_DIR_UNSET_FIX: &str =
    "set XDG_RUNTIME_DIR or HOME, or pass --control-sock <path>";
/// The fix for a launch that found no folder for its control socket: the
/// variables `aterm_uds::control_socket_dir` reads, or the flag that names a
/// socket outright.
#[cfg(windows)]
pub(crate) const SOCKET_DIR_UNSET_FIX: &str = "set TMP or TEMP, or pass --control-sock <path>";

/// The fix for a control-socket folder that cannot be made private: the
/// variable `aterm_uds::control_socket_dir` reads first, or the flag.
#[cfg(not(windows))]
pub(crate) const SOCKET_DIR_UNUSABLE_FIX: &str =
    "set XDG_RUNTIME_DIR to a folder you own, or pass --control-sock <path>";
/// The fix for a control-socket folder that cannot be made private: the
/// variable `aterm_uds::control_socket_dir` reads first, or the flag.
#[cfg(windows)]
pub(crate) const SOCKET_DIR_UNUSABLE_FIX: &str =
    "set TMP to a folder you own, or pass --control-sock <path>";

/// The window's `[harness]` policy would supervise its agent sessions, but
/// it has no control socket — the one interface the
/// supervisor's workers speak — so nothing answers their permission boxes.
/// `cause` is why the launch has none (`launched with the control socket
/// off`), said with [`HARNESS_BOXES_UNANSWERED`] on the first line; `fix`,
/// the one fix that cause has, is the second line, so a long folder in the
/// cause never pushes it past the line cap.
/// A WARNING that holds for the launch window: the owner's rule is that every
/// box is answered unless they turned that off, and here they did not; the
/// `[harness]` master switch (`enabled = false`) is what says "no" and draws
/// no row.
pub(crate) fn harness_without_socket(cause: &str, fix: Option<&str>) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        "Couldn't supervise agent sessions",
    )
    .glyph(Glyph::or_fallback('\u{26a0}'))
    .line(format!("{cause}, so {HARNESS_BOXES_UNANSWERED}"))
    .lines(fix.map(str::to_string))
    .no_excerpt()
    .hold(Hold::For(HOLD_LAUNCH))
    .key(KEY_HARNESS_NO_SOCKET)
}

/// A restored tab's relaunch that was typed and did not bring its agent's
/// conversation back: a Claude that started and ended as it
/// started (`No conversation found`), never registered it, or was still
/// coming up when the host stopped waiting — and a Codex whose relaunched
/// TUI never came up in the tab (its lane's `no-resume`, and its
/// `wait:resume` while none is found), said as that ([`typed_relaunch_why`]),
/// never as one that started.
fn started_without_conversation(outcome: &aterm_agent::harness::relaunch::Outcome) -> bool {
    use aterm_agent::harness::relaunch::Outcome;
    matches!(outcome, Outcome::NotYet(step) if step == "failed:no-resume" || step == "wait:resume")
}

/// What became of `agent`'s typed relaunch that did not bring its
/// conversation back ([`started_without_conversation`]), in a person's
/// words: Claude Code started and did not pick it up; the relaunched Codex
/// did not come up in the tab.
fn typed_relaunch_why(agent: aterm_agent::harness::upgrade::Agent) -> String {
    use aterm_agent::harness::upgrade::Agent;
    match agent {
        Agent::Claude => "Claude started but did not pick its conversation up".to_string(),
        Agent::Codex => "the relaunched Codex did not come up in the tab".to_string(),
        // Its lane's words mean that too: no Muse Code came up resuming the
        // session in the tab.
        Agent::Muse => "the relaunched Muse Code did not come up in the tab".to_string(),
        // agy's too: no agy came up resuming the conversation in the tab.
        Agent::Agy => "the relaunched agy did not come up in the tab".to_string(),
        // Gemini CLI's too: no Gemini CLI came up resuming the session in
        // the tab.
        Agent::Gemini => "the relaunched Gemini CLI did not come up in the tab".to_string(),
    }
}

/// The command that brings `agent`'s conversation back in its tab, and what
/// that agent calls a conversation: Claude Code's `claude --resume` and its
/// conversation, Codex's `codex resume` and its thread (as
/// [`restored_agent_held`] words them).
fn resume_words(agent: aterm_agent::harness::upgrade::Agent) -> (&'static str, &'static str) {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => ("claude --resume", "conversation"),
        aterm_agent::harness::upgrade::Agent::Codex => ("codex resume", "thread"),
        // Muse Code's own resume verb, and what it calls one: a session.
        aterm_agent::harness::upgrade::Agent::Muse => ("muse resume", "session"),
        // agy, and its own picker of conversations (`/resume`), and what it
        // calls one: a conversation. Never a bare `agy --conversation`, which
        // agy refuses (`flag needs an argument`, exit 2): the flag takes the
        // id, which this row does not carry.
        aterm_agent::harness::upgrade::Agent::Agy => ("agy then /resume", "conversation"),
        // Gemini CLI, and its own picker of sessions (`/resume`), and what it
        // calls one: a session. Never a bare `gemini --resume`, which resumes
        // the project's newest session: the id is not on this row.
        aterm_agent::harness::upgrade::Agent::Gemini => ("gemini then /resume", "session"),
    }
}

/// Why a restored tab's agent (`agent`, Claude Code or Codex) did not come
/// back, in a person's words — never the relaunch's step word, which
/// `aterm.log` keeps. An agent that started and did not pick its
/// conversation up is said as that, never as a tab that was not ready.
fn not_resumed_why(
    agent: aterm_agent::harness::upgrade::Agent,
    outcome: &aterm_agent::harness::relaunch::Outcome,
) -> String {
    use aterm_agent::harness::relaunch::Outcome;
    let who = agent_word(agent);
    let (_, what) = resume_words(agent);
    match outcome {
        o if started_without_conversation(o) => typed_relaunch_why(agent),
        Outcome::NotYet(step) if step.starts_with("failed:relaunch:") => {
            "its prompt refused the line that starts it".to_string()
        }
        Outcome::Cannot(why) if why == "shell-gone" => "its tab's shell had ended".to_string(),
        Outcome::Left(why) if why == "not-exited" => {
            format!("the {who} that ran there before is still running, outside the tab")
        }
        Outcome::Left(why) if why == "no-conversation" => {
            format!("its {what} could not be found")
        }
        Outcome::NotYet(_) | Outcome::Busy => "its tab was not ready for it in time".to_string(),
        _ => "it could not be started there".to_string(),
    }
}

/// The agents that did not come back. The reopened layout's
/// row ([`journal_reopened_message`]) told the person that aterm starts the
/// agent again on its conversation in these tabs; the relaunch then could not
/// bring it back in each `(place, agent, outcome)` of `missed` (`place` as
/// `in tab 2`, `agent` the one that ran there). Said by the supervisor host
/// once it has tried every restored tab (`HostHandle::relaunch_restored`), each
/// row naming every tab said so far this launch (its key supersedes the last):
/// a warning — the person resumes it by hand — with `Open log` on `log`, where
/// each step is. `detail[0]`, the plain sentence Settings sets above the
/// technical block, says what happened and what to type; the per-tab reasons
/// and the log's path follow. A Codex is said as Codex, its remedy `codex
/// resume` and its thread, as the held record words it
/// ([`restored_agent_held`]); a row of both names each tab's command on its
/// line. Two panes of one tab's splits are two lines of the
/// same place, and a row of more agents than places counts panes, not tabs.
pub(crate) fn restored_agents_not_resumed(
    missed: &[(
        String,
        aterm_agent::harness::upgrade::Agent,
        aterm_agent::harness::relaunch::Outcome,
    )],
    log: Option<&std::path::Path>,
) -> Message {
    // The one agent every tab ran, where they all ran the same one.
    let kind = missed
        .first()
        .map(|(_, agent, _)| *agent)
        .filter(|agent| missed.iter().all(|(_, other, _)| other == agent));
    // Two agents of one tab's splits share its place, so a row of more agents
    // than places counts the panes.
    let places = {
        let mut places: Vec<&str> = missed.iter().map(|(place, ..)| place.as_str()).collect();
        places.sort_unstable();
        places.dedup();
        places.len()
    };
    let where_ = if places < missed.len() { "pane" } else { "tab" };
    let (title, first, step) = match (missed, kind) {
        ([(place, agent, outcome)], _) => {
            let who = agent_word(*agent);
            let (resume, what) = resume_words(*agent);
            // Only a Claude that came up is said to have started: a Codex
            // lane's same words mean its relaunched TUI never came up
            // ([`typed_relaunch_why`]).
            let started = match agent {
                aterm_agent::harness::upgrade::Agent::Claude => true,
                // Muse Code, agy and Gemini CLI: as Codex's, their lanes'
                // words mean the relaunched TUI never came up.
                aterm_agent::harness::upgrade::Agent::Codex
                | aterm_agent::harness::upgrade::Agent::Muse
                | aterm_agent::harness::upgrade::Agent::Agy
                | aterm_agent::harness::upgrade::Agent::Gemini => false,
            };
            let first = if started_without_conversation(outcome) && started {
                format!(
                    "{who} started in the tab but did not pick its {what} up after aterm \
                     stopped: type {resume} there to pick it up again"
                )
            } else {
                format!(
                    "{}, so its {what} did not come back after aterm stopped: type {resume} \
                     there to pick it up again",
                    not_resumed_why(*agent, outcome)
                )
            };
            (
                format!("Couldn't resume {who} {place}"),
                first,
                format!("Type {resume} in the tab"),
            )
        }
        (_, Some(agent)) => {
            let who = agent_word(agent);
            let (resume, what) = resume_words(agent);
            (
                format!("Couldn't resume {who} in {} {where_}s", missed.len()),
                format!(
                    "their {what}s did not come back after aterm stopped: type {resume} in \
                     each {where_} to pick one up again"
                ),
                format!("Type {resume} in each {where_}"),
            )
        }
        (_, None) => (
            format!("Couldn't resume the agents in {} {where_}s", missed.len()),
            format!(
                "their conversations did not come back after aterm stopped: type each \
                 {where_}'s resume command there, as its line says, to pick one up again"
            ),
            format!("Type each {where_}'s resume command in it"),
        ),
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Warn, title)
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(first);
    if missed.len() > 1 {
        for (place, agent, outcome) in missed {
            let why = not_resumed_why(*agent, outcome);
            msg = msg.line(if kind.is_some() {
                format!("{place}, {why}")
            } else {
                format!("{place}, {why}: type {} there", resume_words(*agent).0)
            });
        }
    }
    if let Some(log) = log {
        let log = log.display().to_string();
        msg = msg
            .line(format!("what aterm tried is in {log}"))
            .action(Intent::OpenPath { path: log });
    }
    // Asked after the lines: the remedy sentence above stays `detail[0]`.
    let (tried, reason) = match kind {
        Some(agent) => {
            let (_, what) = resume_words(agent);
            (
                format!("tried to start {} again on its {what}", agent_word(agent)),
                format!("it did not pick the {what} up"),
            )
        }
        None => (
            "tried to start each agent again on its conversation".to_string(),
            "they did not pick their conversations up".to_string(),
        ),
    };
    let ask = Ask::flash(
        Why::Spent {
            tried: Reason::new(tried),
            reason: Reason::new(reason),
        },
        Then::step(step),
        Span::Launch,
    );
    msg.no_excerpt().now(ask).key(KEY_RESTORED_AGENTS)
}

/// The supersede key of the record that says a restored tab's agent stays
/// held ([`restored_agent_held`]), one per tab: `<this>.<sid>`.
pub(crate) const KEY_RESTORED_HELD: &str = "harness.restored-held";

/// `aterm.toml` did not load at launch. `notice` is
/// `app_config::launch_config_notice`'s sentence (problem, path,
/// consequence): `aterm.toml could not be read at launch (…) — …` or
/// `aterm.toml is not a valid configuration (…) — …`. The title is terse
/// and says the file is broken, the parser's own error's first
/// row (between ` (` and `) — `) is `detail[0]`, and the consequence and the
/// whole sentence follow behind Details — the sentence in its physical rows
/// ([`diagnostic_lines`]).
///
/// The rows matter because the parenthesised error of an `Invalid` launch is
/// a `toml` parse error: a sentence, then a gutter, the source line and a
/// caret row. Handed to one `Message::line`, the engine's sanitizer strips
/// `\n`, collapsing the caret diagram into one line; splitting into physical
/// rows keeps the caret rows opening with whitespace, so the Details page
/// sets them in the monospace face with their columns intact.
pub(crate) fn launch_load_failure(notice: &str) -> Message {
    let title = if notice.starts_with("aterm.toml could not be read") {
        "Couldn't read your settings"
    } else {
        "Couldn't load your settings"
    };
    let rows = notice
        .split_once(" (")
        .and_then(|(_, rest)| rest.rsplit_once(") \u{2014} "))
        .map(|(error, _)| diagnostic_lines(error))
        .unwrap_or_default();
    // An invalid file's excerpt is the parser's finding in a person's words;
    // an unreadable one's is the system's own few words (`permission denied`).
    let error = if notice.starts_with("aterm.toml is not a valid configuration") {
        toml_words(&rows)
    } else {
        rows.into_iter().next().filter(|row| !row.is_empty())
    };
    let mut msg = Message::new(tags::CONFIG, Severity::Error, title);
    let painted = error.is_some();
    if let Some(error) = error {
        msg = msg.line(error);
    }
    // The notice's own tail says the consequence (`— every setting is
    // running at its default`): no line of its own before it.
    // Asked after the lines and the capsule: the rows above stay the
    // detail, and the ask's `Open aterm.toml` is that capsule.
    let why = if notice.starts_with("aterm.toml could not be read") {
        Why::OnlyYou(Reason::new(
            "aterm.toml could not be read, so every setting is at its default",
        ))
    } else {
        Why::YourAction(Reason::new(
            "aterm.toml did not load, so the window's settings except [harness] are at their defaults",
        ))
    };
    let ask = Ask::flash(
        why,
        Then::press(Mover::open_config_editor(None), "Fix aterm.toml"),
        Span::Launch,
    );
    let msg = msg
        .lines(diagnostic_lines(notice))
        .action(Intent::OpenConfigEditor { line: None })
        .now(ask)
        .key(KEY_LAUNCH_LOAD);
    if painted { msg } else { msg.no_excerpt() }
}

/// A key the CPU renderer cannot honour (`background_opacity`,
/// `background_material`): a disclosure, so a record — the Manual already
/// marks the key inert while the CPU renderer is active — pointing at the
/// Appearance page where the renderer is chosen.
pub(crate) fn cpu_renderer_no_effect(key: &str, rest: &str) -> Message {
    // The setting in a person's words; the key and why follow.
    let name = match key {
        "background_opacity" => "Transparency",
        "background_material" => "Window material",
        _ => "This setting",
    };
    Message::new(
        tags::RENDER,
        Severity::Info,
        format!("{name} needs the GPU renderer"),
    )
    .line(format!("{key} has no effect on the CPU renderer"))
    .line(rest)
    .action(Intent::OpenSettings {
        route: SettingsRoute::Appearance.path().to_string(),
    })
    .record()
    .key(format!("render.cpu.{key}").as_str())
}

/// The Windows client-area backdrop was declined (DirectComposition
/// unavailable, `hdr_glow` on, no GPU, not requested at launch): a record for
/// the family — a blank window is the GPU-lost row's, not this one's.
#[cfg(any(windows, test))]
pub(crate) fn backdrop_declined(title: &str, detail: &str) -> Message {
    // One title in the failure grammar; the site's words follow.
    Message::new(tags::RENDER, Severity::Info, "Couldn't show the backdrop")
        .line(title)
        .line(detail)
        .action(Intent::OpenConfigEditor { line: None })
        .record()
        .key(KEY_BACKDROP)
}

/// The GPU was lost and the windows created for the backdrop cannot be
/// redrawn by the CPU fallback. The remedy is a gesture (`New window`), so
/// the row stands until a window created after the loss paints, or until it
/// is read; a timer must not fold the one instruction that recovers the
/// screen. The `New window` capsule is that instruction, so the title and
/// the capsule are the row; why and the alternative ride behind Details.
#[cfg(any(windows, test))]
pub(crate) fn gpu_lost() -> Message {
    Message::new(tags::RENDER, Severity::Error, "GPU lost")
        .line("windows opened for the backdrop cannot redraw")
        .line("open a new window or restart aterm to get a visible one back")
        .no_excerpt()
        .action(Intent::NewWindow)
        .now(Ask::standing(
            StandingWhy::Spent {
                tried: Reason::new("fell back to the CPU renderer"),
                reason: Reason::new("windows made for the backdrop cannot redraw there"),
            },
            Then::press(Mover::new_window(), "Open a new window"),
        ))
        .key(KEY_GPU_LOST)
}

/// The accessibility publisher's thread died. A process gets one
/// publisher, so nothing short of a new process brings it back; the retry is
/// `detail[0]`, alone on its line (the sanctioned anchor), the reason and the
/// consequence behind it. On the glass (Standing) only when assistive
/// technology was in use — `at_in_use`: an AT client had attached to this
/// process's tree ([`crate::a11y_backend`]'s latch) — and a record
/// otherwise. Settings ▸ Messages and `messages.log` keep it either way.
#[cfg(any(a11y_tree, test))]
pub(crate) fn a11y_publisher_dead(reason: &str, at_in_use: bool) -> Message {
    let msg = Message::new(tags::A11Y, Severity::Error, "Screen reader access lost")
        .line("restart aterm to retry")
        .line(reason)
        .key(KEY_A11Y_PUBLISHER);
    if at_in_use {
        msg.now(Ask::standing(
            StandingWhy::Spent {
                tried: Reason::new("ran the accessibility publisher"),
                reason: Reason::new("it died, and only a new aterm process brings it back"),
            },
            Then::step("Quit aterm and open it again"),
        ))
    } else {
        msg.record()
    }
}

/// R9 — a presence toggle's durable write did not land: the person's toggle
/// reverted (`Couldn't save <label>`, an error) or may not have persisted
/// (`Couldn't confirm <label>`, a warning, as Serious Mode's says it).
/// `detail` is the cause. Keyed by the config leaf, so a second failure on
/// the same toggle replaces the first.
pub(crate) fn presence_not_saved(
    leaf: &str,
    label: &str,
    detail: &str,
    indeterminate: bool,
) -> Message {
    let (severity, title) = if indeterminate {
        (Severity::Warn, format!("Couldn't confirm {label}"))
    } else {
        (Severity::Error, format!("Couldn't save {label}"))
    };
    gesture_failure(
        tags::FABRIC,
        severity,
        &title,
        detail,
        Then::step("Turn the toggle again"),
    )
    .key(format!("fabric.presence-save.{leaf}").as_str())
}

/// View ▸ Stats Bars' choice did not reach aterm.toml: the write was
/// refused, conflicted with a hand edit, or could not be confirmed. The bars
/// keep the form the file holds.
pub(crate) fn stats_bars_not_saved(form: &str, detail: &str, indeterminate: bool) -> Message {
    let title = if indeterminate {
        format!("Couldn't confirm the stats bars were set to {form}")
    } else {
        format!("Couldn't set the stats bars to {form}")
    };
    gesture_failure(
        tags::FABRIC,
        if indeterminate {
            Severity::Warn
        } else {
            Severity::Error
        },
        &title,
        detail,
        Then::step("Choose it again in View \u{25B8} Stats Bars"),
    )
    .key("sys.stats-bars-save")
}

/// The harness switch's `on` (the composer top rule's switch) could not turn
/// one of aterm.toml's switches on: the write was refused, conflicted with a
/// hand edit, or could not be confirmed. The switch reads the policy in
/// force, still off; the person may press `on` again. Keyed per switch.
pub(crate) fn harness_switch_not_saved(key: &str, detail: &str, indeterminate: bool) -> Message {
    let title = if indeterminate {
        "Couldn't confirm the harness was turned on"
    } else {
        "Couldn't turn the harness on"
    };
    gesture_failure(
        tags::HARNESS,
        if indeterminate {
            Severity::Warn
        } else {
            Severity::Error
        },
        title,
        &format!("{key} = true: {detail}"),
        Then::step("Press on again"),
    )
    .key(format!("harness.switch-save.{key}").as_str())
}

/// View ▸ Deck Size's choice did not reach aterm.toml: the write was refused,
/// conflicted with a hand edit, or could not be confirmed. Every window keeps
/// the size the file holds.
pub(crate) fn deck_size_not_saved(size: &str, detail: &str, indeterminate: bool) -> Message {
    let title = if indeterminate {
        format!("Couldn't confirm the stats deck's size was set to {size}")
    } else {
        format!("Couldn't set the stats deck's size to {size}")
    };
    gesture_failure(
        tags::FABRIC,
        if indeterminate {
            Severity::Warn
        } else {
            Severity::Error
        },
        &title,
        detail,
        Then::step("Choose it again in View \u{25B8} Deck Size"),
    )
    .key("sys.deck-size-save")
}

/// A theme chosen in the command palette (Enter on a Theme row, View ▸
/// Choose Theme…) did not reach aterm.toml: the write was refused, conflicted
/// with a hand edit, or could not be confirmed. The preview it left standing
/// has reverted to the configured theme, so the screen already shows the
/// truth; this says why.
pub(crate) fn theme_not_saved(label: &str, detail: &str, indeterminate: bool) -> Message {
    let (severity, title) = if indeterminate {
        (Severity::Warn, "Couldn't confirm the theme was saved")
    } else {
        (Severity::Error, "Couldn't save the theme")
    };
    gesture_failure(
        tags::CONFIG,
        severity,
        title,
        &format!("{label}: {detail}"),
        Then::step("Choose the theme again"),
    )
    .key("config.theme-save")
}

/// The fabric bridge set `files` torn state files aside (`link set-aside`,
/// `fabric::Registry::torn_state_set_aside`): the last bridge stopped
/// uncleanly in an earlier boot (a power cut, a crash of the machine), and
/// files of its state dir that serve only sessions — which that reboot ended —
/// did not read whole, so the first bridge after it moved each, with its exact
/// bytes, into the dir's `quarantine/` and attached rather than refuse them at
/// every attach. Nothing is lost that a later boot reads, so this is a fact
/// for the owner's later look — the bytes are theirs to read or remove — never
/// an alert. One line however many cuts it covers (`fabric.state.torn`); a
/// later count restates it.
#[cfg(any(unix, test))]
pub(crate) fn torn_state_set_aside_entry(files: u64) -> Entry {
    let what = if files == 1 {
        "1 file".to_string()
    } else {
        format!("{files} files")
    };
    Entry::new(
        Family::FabricState,
        "",
        "torn",
        "Torn fabric files set aside",
        Why::OnlyYou(Reason::new(
            "aterm keeps the torn bytes for you and reads them no more",
        )),
        Then::step("Look through the quarantine folder in the state dir aterm fabric shows"),
    )
    .resurface(Resurface::CHOICE)
    .more(format!(
        "{what} found torn after an unclean shutdown moved into quarantine in the bridge's \
         state dir"
    ))
    .log_line(format!("fabric.state torn set_aside={files}"))
}

/// The bridge was lost and nothing was halted (`fabric::bridge_lost`): `n`
/// sessions the bridge governed keep running, each with
/// `status hold_advisory=fabric-lost` while no newer bridge has attached. A
/// warning the person may act on, never a row: it names what did not happen
/// and the switch that would have made it. Keyed with the person's own hold
/// messages, so a later one supersedes it.
#[cfg(any(unix, test))]
pub(crate) fn fabric_lost_advisory(n: usize) -> Message {
    let sessions = if n == 1 {
        "1 session".to_string()
    } else {
        format!("{n} sessions")
    };
    Message::new(tags::FABRIC, Severity::Warn, "Fabric bridge lost")
        .line(format!(
            "{sessions} it governed keep running, not halted (status hold_advisory=fabric-lost); \
             [fabric] hold_on_bridge_lost = true halts them"
        ))
        .record()
        .hold(Hold::LogOnly)
        .key(KEY_FABRIC_HOLD)
}

/// R10 — the person's Take Over (`on`) or Hand Back press did not move the
/// hold (a seamless update was committing the tab): the reason is `detail`.
pub(crate) fn hold_refused(on: bool, detail: &str) -> Message {
    let (title, step) = if on {
        (
            "Couldn't take the tab over",
            "Choose Take Over This Tab again",
        )
    } else {
        (
            "Couldn't hand the tab back",
            "Choose Hand Back This Tab again",
        )
    };
    gesture_failure(
        tags::FABRIC,
        Severity::Warn,
        title,
        detail,
        Then::step(step),
    )
    .key(KEY_FABRIC_HOLD)
}

/// Summarize in a New Tab's worker did not start: its tab did not open, or
/// its shell drew no prompt to type the agent's line at (`detail`).
pub(crate) fn summarize_not_started(detail: &str) -> Message {
    gesture_failure(
        tags::HARNESS,
        Severity::Warn,
        "Couldn't start the summary",
        detail,
        Then::step("Choose Summarize in a New Tab again"),
    )
    .key("harness.summarize")
}

/// The key prefix of [`stop_worker_failed`] and [`stop_worker_nothing`]:
/// one row per stopped tab's label.
pub(crate) const KEY_STOP_WORKER: &str = "harness.stop-worker";

/// The PERSON'S Stop Worker (the Overview's second ⌫, the tab menu's sheet)
/// did not signal tab `label`'s agent: `detail` is the `signal` verb's
/// refusal (no foreground program, a platform with no signals). Their own
/// gesture failed, so it is said for the gesture's short hold, with the act
/// that still ends the worker. A control client's refused stop is never this
/// row (`App::stop_worker_refused`).
pub(crate) fn stop_worker_failed(label: &str, detail: &str) -> Message {
    gesture_failure(
        tags::HARNESS,
        Severity::Warn,
        &format!("Couldn't stop {label}"),
        detail,
        Then::step("Close its tab to end it"),
    )
    .key(format!("{KEY_STOP_WORKER}.{label}").as_str())
}

/// R10 — the person's Stop Worker found nothing to stop: tab `label`'s task
/// had already ended, or its tab was gone (`detail`). They asked for what
/// already is, so it is a RECORD with no step, never a row on the glass.
pub(crate) fn stop_worker_nothing(label: &str, detail: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Info,
        format!("Nothing to stop in {label}"),
    )
    .line(detail)
    .record()
    .key(format!("{KEY_STOP_WORKER}.{label}").as_str())
}

/// R11 — an Agents menu or palette row that could not do its thing (a
/// document that would not open, a thread that would not spawn): a terse
/// title, the cause and the path behind it, keyed by the verb.
pub(crate) fn fabric_failure(verb: &str, title: &str, lines: &[&str]) -> Message {
    gesture_failure_lines(
        tags::FABRIC,
        Severity::Error,
        title,
        lines.iter().map(|line| (*line).to_string()),
        Then::step("Choose the menu item again"),
    )
    .key(format!("fabric.{verb}").as_str())
}

/// Another Mac drove a tab here over the fleet: `host` (the name
/// this Mac's book gives the node that proved itself) did `act` to `tab` — a
/// typing verb that typed, a hold that moved, a worker started, a close.
/// A record: the person granted it (`aterm fleet allow drive --from`), and
/// the tab's typed-by and its hold say the rest.
pub(crate) fn driven_over_fleet(host: &str, act: &str, tab: &str) -> Message {
    let by = crate::typed_by::fleet_credit(host);
    let title = match act {
        "turn" | "key" | "send" => format!("{by} typed into {tab} ({act})"),
        _ => format!("{by} {act} {tab}"),
    };
    Message::new(tags::FABRIC, Severity::Info, title)
        .record()
        .line("aterm fleet allow facts|content here stops it")
}

/// A command a row read into a tab ended (`App::complete_cli_tab`):
/// a record however it ended, clean or not — the output tab that opens says
/// the rest (a bus that is off, a fleet Mac that did not answer), so it is
/// never an error row. `reason` (`exited 2`) rides the record. Only a command
/// that left no output to open (`has_tab` false: it could not run, or its file
/// could not be written) is the person's gesture failing.
pub(crate) fn cli_tab_finished(verb: &str, title: &str, reason: &str, has_tab: bool) -> Message {
    let key = format!("agents.{verb}");
    if has_tab {
        let mut msg = Message::new(tags::FABRIC, Severity::Info, title).record();
        if !reason.is_empty() {
            msg = msg.line(reason).line("its output is in the new tab");
        }
        msg.key(&key)
    } else {
        gesture_failure(
            tags::FABRIC,
            Severity::Warn,
            &format!("Couldn't run {title}"),
            reason,
            Then::step("Choose it again"),
        )
        .key(&key)
    }
}

/// R12 — the Serious Mode write's feedback: the completion's sentence (`Serious
/// Mode was not changed: …`, `Serious Mode may have been written but could not
/// be verified; reload before retrying: …`, `Serious Mode was saved, but …`)
/// as a terse title and the cause behind it — the cause, or the instruction
/// to reload first, is `detail[0]`.
pub(crate) fn serious_mode_feedback(words: &str) -> Message {
    let words = words.trim_end_matches('.');
    let mut overridden = false;
    let (title, rest) = if let Some(rest) = words.strip_prefix("Serious Mode was not changed") {
        ("Couldn't change Serious Mode", rest)
    } else if let Some(rest) = words.strip_prefix("Serious Mode was saved but could not be applied")
    {
        ("Couldn't apply Serious Mode", rest)
    } else if let Some(rest) =
        words.strip_prefix("Serious Mode was saved, but a newer aterm.toml edit now controls it")
    {
        overridden = true;
        ("Couldn't apply Serious Mode", rest)
    } else {
        (
            "Couldn't confirm Serious Mode",
            words.strip_prefix("Serious Mode").unwrap_or(words),
        )
    };
    let rest = rest
        .trim_start_matches(':')
        .trim_start_matches(" because")
        .trim();
    let mut lines: Vec<String> = Vec::new();
    if let Some((head, tail)) = rest.split_once("; reload before retrying") {
        lines.push("reload before retrying".to_string());
        let head = head.trim();
        if !head.is_empty() {
            lines.push(head.to_string());
        }
        let tail = tail.trim_start_matches(':').trim();
        if !tail.is_empty() {
            lines.push(tail.to_string());
        }
    } else {
        lines.extend(
            rest.split("; ")
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string),
        );
    }
    if overridden {
        // What overrode it, first — the capsule is `Open aterm.toml`.
        lines.insert(0, "aterm.toml sets it".to_string());
    }
    // The ask's button is the row's one capsule, `Open aterm.toml`.
    gesture_failure_lines(
        tags::CONFIG,
        Severity::Warn,
        title,
        lines,
        Then::press(
            Mover::open_config_editor(None),
            "Set Serious Mode in aterm.toml",
        ),
    )
    .key(KEY_SERIOUS_MODE)
}

/// The native config lane's heads, as terse titles,
/// and whether the head is known. An unknown head is a failed settings
/// change, its whole first row the excerpt.
fn lane_title(head: &str) -> (&'static str, bool) {
    let known = |title| (title, true);
    if head.starts_with("Config observation was not valid TOML") {
        known("Couldn't read your settings")
    } else if head.starts_with("Config reconciliation failed") {
        known("Couldn't save settings changes")
    } else if head.starts_with("Manual saved aterm.toml, but")
        || head.starts_with("Config was saved, but its exact disk generation")
    {
        known("Couldn't apply saved settings")
    } else if head.starts_with("Config publication could not be verified") {
        known("Couldn't confirm settings saved")
    } else if head.starts_with("Config was NOT saved") {
        known("Couldn't save a settings change")
    } else {
        ("Couldn't change a setting", false)
    }
}

/// The native config lane's own error (`Config observation was not
/// valid TOML`, a pump that failed): a terse title by its head
/// ([`lane_title`]), the cause's first physical row as `detail[0]`, and every
/// later row — a `toml` parse error's caret diagram — behind Details.
///
/// The words are split once into their physical rows ([`diagnostic_lines`]),
/// so the band's excerpt, the Settings ▸ Messages entry and the screen
/// reader's rows are the same rows by construction, and the whole diagnostic
/// stays one message.
pub(crate) fn config_lane_error(words: &str) -> Message {
    let mut rows = diagnostic_lines(words).into_iter();
    let first = rows.next().unwrap_or_default();
    let (head, cause) = split_sentence(&first);
    let (title, known) = lane_title(head);
    // What the band paints: the parser's finding in a person's
    // words for an invalid file, the cause for a known head — and nothing for
    // a head this build does not know, whose row (`Something new went wrong:
    // …`) is someone else's words, whole behind Details.
    let rows: Vec<String> = rows.collect();
    let toml = head
        .starts_with("Config observation was not valid TOML")
        .then(|| {
            let mut all = vec![cause.to_string()];
            all.extend(rows.iter().cloned());
            toml_words(&all)
        });
    let painted = match &toml {
        Some(words) => words.is_some(),
        None => known,
    };
    let excerpt = if known { cause } else { first.as_str() };
    // What a failed reconciliation lost is what the person does next about:
    // `2 queued changes were not written` leads, the cause behind it. The head
    // says it only when requests were discarded.
    let lost = head
        .strip_prefix("Config reconciliation failed; ")
        .filter(|lost| !lost.is_empty());
    let msg = Message::new(tags::CONFIG, Severity::Warn, title);
    let msg = match lost {
        Some(lost) => msg.line(lost),
        None => msg,
    };
    let msg = match toml.flatten() {
        Some(words) => msg.line(words),
        None => msg,
    };
    let msg = rows.into_iter().fold(msg.line(excerpt), Message::line);
    let msg = msg
        .action(Intent::OpenConfigEditor { line: None })
        .key(KEY_CONFIG_LANE);
    let msg = if painted { msg } else { msg.no_excerpt() };
    // The person's own edit did not take: a flash at the warning's 45 s,
    // asked after the lines so they stay the detail. The reason is a fixed clause per head,
    // never the cause's words (`reload before retrying` is no reason).
    let why = match title {
        "Couldn't read your settings" => "aterm.toml is not valid TOML",
        "Couldn't save settings changes" => "your settings changes were not written",
        "Couldn't apply saved settings" => "your saved settings were not applied",
        "Couldn't confirm settings saved" => "your settings change could not be confirmed",
        "Couldn't save a settings change" => "your settings change was not saved",
        _ => "your settings change did not take",
    };
    let ask = Ask::flash(
        Why::YourAction(Reason::new(why)),
        Then::press(
            Mover::open_config_editor(None),
            "Fix the setting in aterm.toml",
        ),
        Span::Warn,
    );
    msg.now(ask)
}

/// One shape for "the person's own gesture failed": a failure of
/// their own action earns the glass for a short hold (`HOLD_GESTURE`, 12 s)
/// and nothing more — a title of a few words, the full error behind Details
/// (no 160-character cut: the log answers the investigator). Unkeyed unless
/// the caller keys it: a second failed press is a second row's worth of news,
/// folded as a duplicate when the words are the same.
///
/// The class is the door's transient ask: what the person just did did not
/// work (`Why::YourAction`, the title), `then` the one thing to do about it
/// (a step in words, or a button the row already carries), held for
/// [`Span::Gesture`] — the same 12 s. It is asked after the detail is
/// written, so the rows stay the detail and the painted excerpt.
fn gesture_failure(tag: Tag, severity: Severity, title: &str, detail: &str, then: Then) -> Message {
    // `sentence`, never `line`: the error is someone else's words, kept whole
    // across detail lines, not clipped at the line cap.
    let ask = gesture_ask(title, then);
    Message::new(tag, severity, title).sentence(detail).now(ask)
}

/// [`gesture_failure`] for a builder whose detail is its own lines, each as
/// [`Message::line`] (the cause and the path, Serious Mode's instruction):
/// written before the ask, so they stay the detail.
fn gesture_failure_lines(
    tag: Tag,
    severity: Severity,
    title: &str,
    lines: impl IntoIterator<Item = String>,
    then: Then,
) -> Message {
    let ask = gesture_ask(title, then);
    Message::new(tag, severity, title).lines(lines).now(ask)
}

/// The ask of a failed gesture: the title is what did not work.
fn gesture_ask(title: &str, then: Then) -> Ask {
    Ask::flash(Why::YourAction(Reason::new(title)), then, Span::Gesture)
}

/// R14 — `New Tab` did not open one (the spawn failed).
pub(crate) fn new_tab_failed(error: &str) -> Message {
    gesture_failure(
        tags::WINDOW,
        Severity::Error,
        "Couldn't open a new tab",
        error,
        Then::step("Open the tab again"),
    )
    .no_excerpt()
}

/// R14 — `New Window` did not open one (the spawn failed).
pub(crate) fn new_window_failed(error: &str) -> Message {
    gesture_failure(
        tags::WINDOW,
        Severity::Error,
        "Couldn't open a new window",
        error,
        Then::step("Open the window again"),
    )
    .no_excerpt()
}

/// A split the pane cannot hold was refused before anything spawned.
/// `reason` is the refusal's sentence (`Split refused: this pane is 20x5
/// cells; a vertical split needs at least 20x7`): the two sizes are what the
/// person acts on, so `detail[0]` is `pane 20×5, needs 20×7` and the sentence
/// rides whole behind it. A refusal in another shape is its own excerpt.
pub(crate) fn split_refused(reason: &str) -> Message {
    let (_, why) = split_sentence(reason);
    let then = || Then::step("Make the pane larger, then split it");
    match split_sizes(why) {
        Some(sizes) => {
            gesture_failure(tags::WINDOW, Severity::Error, SPLIT_FAILED, &sizes, then()).line(why)
        }
        None => gesture_failure(tags::WINDOW, Severity::Error, SPLIT_FAILED, why, then()),
    }
}

/// `pane 20×5, needs 20×7` from `this pane is 20x5 cells; a vertical split
/// needs at least 20x7`, or `None` when the sentence is not that shape.
fn split_sizes(why: &str) -> Option<String> {
    let (have, rest) = why.strip_prefix("this pane is ")?.split_once(" cells; ")?;
    let (_, need) = rest.rsplit_once("needs at least ")?;
    let size = |s: &str| {
        let (c, r) = s.trim().split_once('x')?;
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        (digits(c) && digits(r)).then(|| format!("{c}\u{00d7}{r}"))
    };
    Some(format!("pane {}, needs {}", size(have)?, size(need)?))
}

/// A split that fit could not spawn its pane.
pub(crate) fn split_failed(error: &str) -> Message {
    gesture_failure(
        tags::WINDOW,
        Severity::Error,
        SPLIT_FAILED,
        error,
        Then::step("Split the pane again"),
    )
    .no_excerpt()
}

/// A split refused or failed: one title in the failure grammar.
const SPLIT_FAILED: &str = "Couldn't split the pane";

/// Keystrokes typed while a seamless update finished overflowed the
/// pre-Commit queue and were dropped, or were typed for a new tab that never
/// started (`tab_spawn`): the person typed them, so they hear it — and
/// `detail[0]` says what to do, as an instruction, not a clause.
pub(crate) fn keystrokes_dropped(dropped: u32) -> Message {
    let what = if dropped == 1 {
        "retype the last key".to_string()
    } else {
        format!("retype the last {dropped} keys")
    };
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Keystrokes dropped",
        &what,
        Then::step("Type the dropped keys again"),
    )
}

/// The person's own key or paste did not go: the session's input
/// queue was full (the program is not reading) or its writer was gone
/// (`App::refuse_input`). A gesture that failed, so the one
/// gesture-failure shape — Error, `HOLD_GESTURE` — and a title in the failure
/// grammar; `detail[0]` says why in a few words, what to do rides whole
/// behind it. The host keys it per session, so a burst of refused keys is
/// one row. It names no `Stop paste`: that capsule is the paste row's.
pub(crate) fn input_refused(cause: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't send your input",
        cause,
        Then::step("Send it again once the program reads input"),
    )
}

/// R14 — a session restore could not create a window and stopped there. The
/// line restates the title, so it rides behind Details.
pub(crate) fn restore_stopped_early() -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't restore every tab",
        "some saved tabs were not restored",
        Then::step("Open the missing tabs again"),
    )
    .no_excerpt()
}

/// R14 — a shell handed across a seamless update could not be adopted.
pub(crate) fn shell_lost_in_update(error: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Shell lost in the update",
        error,
        Then::step("Open a new tab for its work"),
    )
    .no_excerpt()
}

/// The supersede key of the one row for the tabs that lost their shell
/// integration (`integration_notice`): one row per condition, never one per
/// tab. A tab's own record (its key waits at its next prompt) is keyed
/// `session.integration.<sid>` beneath it.
pub(crate) const KEY_INTEGRATION_LOST: &str = "session.integration";

/// The declared fix words of the lost-integration row:
/// resolved `Ok` once its last tab healed, it is logged under these words
/// with a `✓` and leaves Problems — never the warning's title still in amber.
pub(crate) const INTEGRATION_BACK: &str = "Shell integration back";

/// What opens the list of session ids on the row's last, technical lines
/// ([`integration_lost`]); [`integration_lost_sessions`] reads it back.
const INTEGRATION_SESSIONS: &str = " \u{2014} sessions ";

/// [`INTEGRATION_SESSIONS`] for a row that names one tab.
const INTEGRATION_SESSION: &str = " \u{2014} session ";

/// A lost-integration row's technical line split at its list of session
/// ids, either spelling ([`INTEGRATION_SESSIONS`], [`INTEGRATION_SESSION`]).
fn split_integration_sessions(line: &str) -> Option<(&str, &str)> {
    line.split_once(INTEGRATION_SESSIONS)
        .or_else(|| line.split_once(INTEGRATION_SESSION))
}

/// How many degraded tabs the row names a line each; the rest are counted.
const INTEGRATION_TABS_NAMED: usize = 12;

/// One tab the lost-integration row names ([`integration_lost`]): its place
/// in the person's words (`in tab 2`, `in window 2, tab 1`:
/// `App::upgrade_place`, [`tab_place`] — never the raw `s-<hex>`),
/// its session id (the technical line only), and what gives
/// its integration back.
#[derive(Clone, Debug)]
pub(crate) struct LostTab {
    /// `in tab 2`, `in window 2, tab 1`, `in its tab`.
    pub(crate) place: String,
    /// The session id, for the technical line alone.
    pub(crate) sid: String,
    /// What heals it: [`crate::integration_notice::Heal::AgentMoved`] or
    /// [`crate::integration_notice::Heal::NewTab`] (a tab whose key waits at
    /// its next prompt is a record, [`integration_rekey_waits`]).
    pub(crate) heal: crate::integration_notice::Heal,
}

/// Tabs lost their shell integration (`integration_notice`): an adopted
/// shell (a seamless update or a keeper crash recovery) came without its key,
/// so command blocks and exit codes are dark there. One Standing Warn under
/// [`KEY_INTEGRATION_LOST`] for every such tab: the title counts them, the
/// first line names them by place (past [`INTEGRATION_TABS_NAMED`], an
/// `and N more` line counts the rest), the next says what gives it back, and
/// behind them, only where the remedies differ, the agent tabs by place
/// (one shared remedy is said once), then the technical line with the
/// session ids ([`integration_lost_sessions`]). The window restates it in
/// place as tabs heal, close or change remedy, resolves it `Ok` under
/// [`INTEGRATION_BACK`] only when the last tab healed, and withdraws it when
/// the last one closed. `None` for no tab. No line asks for a restart:
/// the agent's move is the harness's act, and a new tab is not a restart.
pub(crate) fn integration_lost(tabs: &[LostTab]) -> Option<Message> {
    use crate::integration_notice::Heal;
    let first = tabs.first()?;
    let n = tabs.len();
    let bare = |place: &str| {
        atpkg::progress::sanitize_for_tty(place.strip_prefix("in ").unwrap_or(place), 64)
    };
    let agents = tabs
        .iter()
        .filter(|t| matches!(t.heal, Heal::AgentMoved { .. }))
        .count();
    // The harness types the re-keying line whenever it starts the agent anew:
    // its move onto a new build (`[harness] upgrade`) or its relaunch after a
    // crash (`[harness] relaunch`) — either switch makes the tab `AgentMoved`.
    // Tabs whose remedies differ get a line each saying which is which; one
    // shared remedy is said once, on the second line.
    let mixed = agents > 0 && agents < n;
    let (title, what, how) = if n == 1 {
        let place = atpkg::progress::sanitize_for_tty(&first.place, 64);
        let how = match first.heal {
            Heal::AgentMoved { agent } => format!(
                "it comes back when aterm next starts {agent} anew there; a new tab has it now"
            ),
            _ => "open a new tab to get it back".to_string(),
        };
        (
            "Tab lost its shell integration".to_string(),
            format!("{place}: no command blocks or exit codes there"),
            how,
        )
    } else {
        let named = tabs
            .iter()
            .take(INTEGRATION_TABS_NAMED)
            .map(|t| bare(&t.place))
            .collect::<Vec<_>>()
            .join(" \u{00b7} ");
        let how = match agents {
            0 => "open new tabs to get it back".to_string(),
            k if k == n => {
                "each comes back when aterm next starts the agent anew there; a new tab has it now"
                    .to_string()
            }
            k => format!(
                "a new tab has it now; {k} also {} back when aterm next starts the agent anew:",
                if k == 1 { "comes" } else { "come" }
            ),
        };
        (
            format!("{n} tabs lost their shell integration"),
            format!("{named} \u{2014} no command blocks or exit codes there"),
            how,
        )
    };
    // A standing row is a class (the seal of the exceptions design): an
    // adopted shell (a seamless update or a keeper crash recovery) came
    // without its key, so the one thing the owner can do is take a new tab,
    // which has it now.
    // (Classified after its lines: the door leads a row that brought no
    // detail with the reason and the action, and this one wrote its own.)
    let mut msg = Message::new(tags::SESSION, Severity::Warn, title).line(
        atpkg::progress::sanitize_for_tty(&what, aterm_messages::DETAIL_LINE_CAP),
    );
    // The count of the tabs the first line left unnamed follows it, on its
    // own line so the line cap on the names can never cut it — never after
    // the agent list, where it would read as more agent tabs.
    if n > INTEGRATION_TABS_NAMED {
        let rest = n - INTEGRATION_TABS_NAMED;
        msg = msg.line(format!(
            "and {rest} more {}",
            if rest == 1 { "tab" } else { "tabs" }
        ));
    }
    let mut msg = msg.line(how).now(Ask::standing(
        StandingWhy::Spent {
            tried: Reason::new("handed the tab's shell its integration key"),
            reason: Reason::new("the adopted shell came without it, and only a new tab has it"),
        },
        Then::step("Open a new tab"),
    ));
    // The agent tabs themselves, wherever they sit (the second line counts
    // them and ends in a colon), not the agent tabs among the first named.
    if mixed {
        let agent_tabs = tabs.iter().filter_map(|t| match t.heal {
            Heal::AgentMoved { agent } => Some((t, agent)),
            _ => None,
        });
        for (tab, agent) in agent_tabs.take(INTEGRATION_TABS_NAMED) {
            msg = msg.line(format!("{} \u{00b7} {agent}", bare(&tab.place)));
        }
        if agents > INTEGRATION_TABS_NAMED {
            let rest = agents - INTEGRATION_TABS_NAMED;
            msg = msg.line(format!(
                "and {rest} more agent {}",
                if rest == 1 { "tab" } else { "tabs" }
            ));
        }
    }
    // The session ids, whole — never one clipped at the line cap, which the
    // successor would read as a tab that is gone: as many lines as they
    // take, within the detail's line cap.
    let (mut head, sessions) = if n == 1 {
        (
            "it reads `status integration=degraded`",
            INTEGRATION_SESSION,
        )
    } else {
        (
            "each reads `status integration=degraded`",
            INTEGRATION_SESSIONS,
        )
    };
    let mut line = String::new();
    for t in tabs {
        let sid = atpkg::progress::sanitize_for_tty(&t.sid, 64);
        let open = if line.is_empty() {
            format!("{head}{sessions}")
        } else {
            String::new()
        };
        let sep = if line.is_empty() { "" } else { ", " };
        if line.chars().count() + open.chars().count() + sep.len() + sid.chars().count()
            > aterm_messages::DETAIL_LINE_CAP
        {
            if msg.detail.len() + 1 >= aterm_messages::DETAIL_LINES_CAP {
                break;
            }
            msg = msg.line(std::mem::take(&mut line));
            head = "and";
            line = format!("{head}{sessions}{sid}");
            continue;
        }
        line.push_str(&open);
        line.push_str(sep);
        line.push_str(&sid);
    }
    if !line.is_empty() {
        msg = msg.line(line);
    }
    Some(
        msg.no_excerpt()
            .finished_as(INTEGRATION_BACK)
            .key(KEY_INTEGRATION_LOST),
    )
}

/// The session ids a lost-integration row names on its technical lines
/// ([`integration_lost`]) — how a successor reads a row its parent carried
/// across a seamless update back against its own sessions
/// (`integration_notice`). Empty for a row with no such line.
pub(crate) fn integration_lost_sessions(detail: &[String]) -> Vec<String> {
    detail
        .iter()
        .filter_map(|l| split_integration_sessions(l))
        .flat_map(|(_, sids)| sids.split(", "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// A TAB WHOSE NEW KEY WAITS AT ITS NEXT PROMPT (`integration_notice`'s
/// [`crate::integration_notice::Heal::NextPrompt`]): a shell spawned with the
/// re-key channel takes the key the adoption wrote, with nothing to do — a
/// RECORD, never a row, keyed per tab beneath [`KEY_INTEGRATION_LOST`].
pub(crate) fn integration_rekey_waits(place: &str, sid: &str) -> Message {
    let place = atpkg::progress::sanitize_for_tty(place, 64);
    let sid = atpkg::progress::sanitize_for_tty(sid, 64);
    Message::new(
        tags::SESSION,
        Severity::Info,
        "Tab lost its shell integration",
    )
    .line(format!("{place}: it comes back at the shell's next prompt"))
    .line(format!("session {sid}"))
    .no_excerpt()
    .record()
    .hold(Hold::LogOnly)
    .key(&format!("{KEY_INTEGRATION_LOST}.{sid}"))
}

/// R14 — a restored tab could not start its shell.
pub(crate) fn restored_tab_failed(error: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't restore a tab",
        &format!("shell did not start: {error}"),
        Then::step("Open the tab again"),
    )
    .no_excerpt()
}

/// The supersede key of the row that says shells started in the home folder
/// because their folder was not there ([`folder_row`]): one row, however many
/// folders.
pub(crate) const KEY_FOLDER_NOT_FOUND: &str = "spawn.folder-not-found";

/// The supersede key of the row that says shells started in the home folder
/// because their folder could not be entered ([`folder_row`]).
pub(crate) const KEY_FOLDER_NOT_OPENED: &str = "spawn.folder-not-opened";

/// The supersede key of the row that says shells started in the home folder
/// because their folder, on a volume that is not local, did not answer
/// ([`folder_row`]).
pub(crate) const KEY_FOLDER_NOT_ANSWERING: &str = "spawn.folder-not-answering";

/// The supersede key of the row naming folders fresh shells started in `/`
/// for, because neither the folder nor the home folder answered
/// ([`folder_row`]).
pub(crate) const KEY_HOME_NOT_ANSWERING: &str = "spawn.home-not-answering";

/// The most folders [`folder_row`] names one per line; the rest are counted
/// on the line after them.
const FOLDERS_NAMED: usize = DETAIL_LINES_CAP - 2;

/// The one-folder row's line, before the folder.
const FOLDER_INSTEAD_OF: &str = "opened in your home folder instead of ";

/// The many-folder row's first line, before the folders.
const FOLDERS_INSTEAD: &str = "opened in your home folder instead";

/// [`FOLDER_INSTEAD_OF`] when the home folder did not answer either.
const ROOT_INSTEAD_OF: &str = "opened in / instead of ";

/// [`FOLDERS_INSTEAD`] when the home folder did not answer either.
const ROOT_INSTEAD: &str = "opened in / instead";

/// Where `fault`'s shells opened instead: the one-folder line's head and
/// the many-folder row's first line.
const fn folder_instead(fault: crate::spawn_folder::Fault) -> (&'static str, &'static str) {
    match fault {
        crate::spawn_folder::Fault::HomeUnresponsive => (ROOT_INSTEAD_OF, ROOT_INSTEAD),
        _ => (FOLDER_INSTEAD_OF, FOLDERS_INSTEAD),
    }
}

/// The supersede key of `fault`'s row.
pub(crate) const fn folder_row_key(fault: crate::spawn_folder::Fault) -> &'static str {
    match fault {
        crate::spawn_folder::Fault::Missing => KEY_FOLDER_NOT_FOUND,
        crate::spawn_folder::Fault::Shut => KEY_FOLDER_NOT_OPENED,
        crate::spawn_folder::Fault::Unresponsive => KEY_FOLDER_NOT_ANSWERING,
        crate::spawn_folder::Fault::HomeUnresponsive => KEY_HOME_NOT_ANSWERING,
    }
}

/// Fresh shells started in the home folder because the folder each was asked
/// to start in was not there, could not be entered, or did not answer from a
/// volume that is not local (`Couldn't reach`) — a restored pane's
/// saved folder, the one a New Tab inherited, a profile's
/// ([`crate::spawn_folder`]) — or in `/` because the home folder did not
/// answer either, the folder asked for or the one the PTY goes to unasked. One row per fault for every such folder while
/// it is up (`App::fold_folder_faults`, keyed [`folder_row_key`]). A failure
/// in the one grammar (`Couldn't find the folder`, `Couldn't open 3 folders`):
/// the shell is not where the person asked. `detail[0]` is where it is
/// instead, naming the one folder; several are named one per line behind it,
/// and past the line cap the rest, with any a carried row counted without
/// naming ([`crate::spawn_folder::Named::unnamed`]), are counted.
pub(crate) fn folder_row(
    fault: crate::spawn_folder::Fault,
    named: &crate::spawn_folder::Named,
) -> Message {
    debug_assert!(!named.dirs.is_empty(), "a row about no folder");
    let verb = match fault {
        crate::spawn_folder::Fault::Missing => "find",
        crate::spawn_folder::Fault::Shut => "open",
        crate::spawn_folder::Fault::Unresponsive | crate::spawn_folder::Fault::HomeUnresponsive => {
            "reach"
        }
    };
    let (instead_of, instead) = folder_instead(fault);
    let total = named.dirs.len() + named.unnamed;
    let msg = match named.dirs.as_slice() {
        [one] if total == 1 => Message::new(
            tags::WINDOW,
            Severity::Warn,
            format!("Couldn't {verb} the folder"),
        )
        .sentence(format!("{instead_of}{one}")),
        many => {
            let unnamed = total.saturating_sub(many.len().min(FOLDERS_NAMED));
            let msg = Message::new(
                tags::WINDOW,
                Severity::Warn,
                format!("Couldn't {verb} {total} folders"),
            )
            .line(instead)
            .lines(many.iter().take(FOLDERS_NAMED).cloned());
            if unnamed == 0 {
                msg
            } else {
                msg.line(format!("and {unnamed} more"))
            }
        }
    };
    msg.key(folder_row_key(fault))
}

/// The folders a [`folder_row`] names, read back from the row: how a row
/// carried across an update keeps its folders when the next one joins it
/// (the new instance's list starts empty). `None` for a row of another shape.
pub(crate) fn folder_row_named(msg: &Message) -> Option<crate::spawn_folder::Named> {
    let (instead_of, instead) =
        folder_instead(if msg.key.as_deref() == Some(KEY_HOME_NOT_ANSWERING) {
            crate::spawn_folder::Fault::HomeUnresponsive
        } else {
            crate::spawn_folder::Fault::Missing
        });
    if msg.detail.first().map(String::as_str) == Some(instead) {
        let rest = &msg.detail[1..];
        let (dirs, unnamed) = match rest.split_last() {
            Some((count, dirs)) if rest.len() == FOLDERS_NAMED + 1 => (
                dirs,
                count
                    .strip_prefix("and ")
                    .and_then(|n| n.strip_suffix(" more"))
                    .and_then(|n| n.parse().ok())?,
            ),
            _ => (rest, 0),
        };
        return Some(crate::spawn_folder::Named {
            dirs: dirs.to_vec(),
            unnamed,
        });
    }
    // The one-folder form is one sentence, split across lines only when it
    // is longer than a line: at the space before the folder and then hard
    // when the folder has no space in it, else at its spaces.
    let (head, rest) = msg.detail.split_first()?;
    let one = if head == instead_of.trim_end() && !rest.is_empty() {
        rest.concat()
    } else {
        let first = head.strip_prefix(instead_of)?;
        std::iter::once(first)
            .chain(rest.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some(crate::spawn_folder::Named {
        dirs: vec![one],
        unnamed: 0,
    })
}

/// The Open-log press refused (the path was not a regular file under the log
/// dir, or nothing could be spawned to open it): the person's gesture, so a
/// short row naming the path behind Details.
pub(crate) fn log_did_not_open(path: &str) -> Message {
    gesture_failure(
        tags::SYSTEM,
        Severity::Error,
        "Couldn't open the log",
        path,
        Then::step("Open the log from the Finder"),
    )
    .no_excerpt()
}

/// The Full Disk Access need, as an entry of the digest.
/// The card ([`crate::consent_card`]) decides when; every word here is one of
/// its consts — `FDA_WHAT`, `FDA_WHY`, `FDA_THEN`, `FDA_GRANTS`, `FDA_AFTER`,
/// `FDA_DETAIL` — imported, never retyped, so the entry and the card cannot say
/// two things.
///
/// The button is `Open Settings` ([`Mover::open_system_pane`]); a press files
/// the entry `Answered` (the owner did the one thing it asks — the card's
/// `OwnerActed` hold keeps a fresh need quiet meanwhile) and only a verified
/// grant ends it (`App::resolve_fda_need`). The entry has no `Not now`: `Dismiss
/// all` is the decline and files it for 30 days, then 90 ([`Resurface::CHOICE`]),
/// and an owner who declined the prompt never sees one (the card's
/// `decide_need` answers `Answered` from the marker). The `cause` word is the
/// only thing that makes the fact new after a dismissal: `needed` (macOS
/// showed a prompt or refused a read) or `regressed` (a verified grant reads
/// off), so a regression is news once.
///
/// The event line — what macOS just did, with the tab when it is known
/// ([`crate::consent_card::NeedWords::event_line`]) — would be a fourth
/// `more` line, and an entry holds three ([`aterm_messages::exceptions::MORE_LINES`]):
/// it is dropped here and written to `aterm.log` by the seam (`raise_fda_need`),
/// where a developer reads it. The title says which of the three happened: a
/// grant that reads off again, a read macOS refused aterm, or a prompt.
pub(crate) fn file_access_entry(words: &crate::consent_card::NeedWords) -> Entry {
    use crate::consent_card::{
        FDA_AFTER, FDA_DETAIL, FDA_GRANTS, FDA_THEN, FDA_WHAT, FDA_WHAT_DENIED, FDA_WHAT_REGRESSED,
        FDA_WHY, NeedCause,
    };
    let cause = if words.regressed {
        "regressed"
    } else {
        "needed"
    };
    let what = match (words.regressed, words.cause) {
        (true, _) => FDA_WHAT_REGRESSED,
        (false, NeedCause::ProtectedRead { .. }) => FDA_WHAT_DENIED,
        (false, NeedCause::PromptObserved { .. }) => FDA_WHAT,
    };
    Entry::new(
        Family::Privacy,
        "fda",
        cause,
        what,
        Why::OnlyYou(Reason::new(FDA_WHY)),
        Then::press(Mover::open_system_pane(PANE_FULL_DISK_ACCESS), FDA_THEN),
    )
    .resurface(Resurface::CHOICE)
    .more(FDA_GRANTS)
    .more(FDA_AFTER)
    .more(FDA_DETAIL)
}

/// The keys of the two entries [`file_access_entry`] can raise: what a verified
/// grant, or a switch turned off, ends.
pub(crate) const FDA_ENTRY_KEYS: [&str; 2] = ["privacy.fda.needed", "privacy.fda.regressed"];

/// THE PRE-DIGEST FORM of the question, a decision ROW (`Ask::question`, a
/// `Hold::Ask` of ten minutes, `Open Settings` + `Not now`, keyed
/// [`KEY_FILE_ACCESS`]). Nothing posts it any more: the four seam methods hand
/// the need to the digest ([`file_access_entry`]). It stays for tests only, as
/// the band's, the assistive-technology projection's and the Settings page's
/// fixture of a decision row with two capsules, and for the roster that holds
/// the door over a decision.
#[cfg(test)]
pub(crate) fn file_access_question(words: &crate::consent_card::NeedWords) -> Message {
    use crate::consent_card::{FDA_AFTER, FDA_DETAIL, FDA_GRANTS, FDA_THEN, FDA_TITLE, FDA_WHY};
    Message::new(tags::PRIVACY, Severity::Info, FDA_TITLE)
        .line(FDA_WHY)
        .line(words.event_line())
        .line(FDA_GRANTS)
        .line(crate::menu::privacy_settings_path_words(
            crate::menu::PrivacyPane::FullDiskAccess,
        ))
        .line(FDA_AFTER)
        .line(FDA_DETAIL)
        .no_excerpt()
        .action(Intent::OpenSystemPane {
            pane: PANE_FULL_DISK_ACCESS.to_string(),
        })
        .action(Intent::NotNow {
            decision: Decision::FileAccess,
        })
        .now(Ask::question(
            Reason::new(FDA_WHY),
            Then::press(Mover::open_system_pane(PANE_FULL_DISK_ACCESS), FDA_THEN),
        ))
        .key(KEY_FILE_ACCESS)
        // What the log says once it is answered or folds: the
        // question was asked — the grant or the decline is its own record.
        .finished_as(ASKED_FILE_ACCESS)
}

/// The Full Disk Access question's log title once it leaves the glass.
#[cfg(test)]
pub(crate) const ASKED_FILE_ACCESS: &str = "Asked for Full Disk Access";

/// The probe observed the grant. A confirmation, so a record
/// (Settings ▸ Messages and `messages.log`), never a row: the question's row
/// leaving the glass is the visible half, resolved by key before this is
/// recorded (`App::show_macos_access_granted`). It names the fact and stops
/// ([`crate::consent_card::GRANTED_CAPTION`]).
pub(crate) fn file_access_granted() -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Success,
        crate::consent_card::GRANTED_CAPTION,
    )
    .record()
    .key(KEY_FILE_ACCESS)
}

/// A consent dialog the card is not about, after the fact
/// (`App::note_observed_prompts`): the sentinel saw macOS ask the owner about
/// `what` (the music and media library, Photos, contacts, …; the words of
/// [`crate::consent_observer::service_words`]) for a program of this aterm.
/// The owner answers the dialog itself and nothing is left to do here, so it
/// is a record (Settings ▸ Messages and `messages.log`), never a row or an
/// entry: it answers "which tab made macOS ask me that?". `tab` is the 1-based
/// tab the asking program ran in, when ancestry placed it. Each dialog is its
/// own record, keyed by service (`short`, a
/// [`crate::consent_observer::short_service`] id from the same closed table).
pub(crate) fn consent_dialog_record(short: &str, what: &str, tab: Option<usize>) -> Message {
    let asker = tab.map_or_else(
        || "a program in aterm".to_owned(),
        |n| format!("a program in tab {n}"),
    );
    Message::new(tags::PRIVACY, Severity::Info, format!("Asked for {what}"))
        .line(format!("macOS asked you, for {asker}"))
        .line("System Settings ▸ Privacy & Security holds and changes the answer")
        .record()
        .key(format!("privacy.dialog.{}", short.to_ascii_lowercase()).as_str())
}

/// Debugger access records: what a
/// probe observed about macOS's debugger rule, never what an exit code said. A
/// change is the owner's own act, so a record (Settings ▸ Messages and
/// `messages.log`), never a row. Each "on" record carries the one-line undo,
/// as every surface that mentions the change does; the words stop at the
/// observation — what developer mode silences is the Security row's "Stops…"
/// line, never said here. "Was on; it
/// is off now" names no cause: a macOS upgrade, an administrator and
/// `-disable` all look the same.
pub(crate) fn debugger_access_record(record: crate::debugger_access::DebuggerRecord) -> Message {
    use crate::debugger_access::DebuggerRecord;
    let undo = format!(
        "to undo: {}",
        aterm_containment::consent::DEBUGGER_ACCESS_REVERT
    );
    let msg = match record {
        DebuggerRecord::On => Message::new(tags::PRIVACY, Severity::Success, "Debugger access on")
            .line("for the whole Mac")
            .line(&undo),
        DebuggerRecord::OnOutside => Message::new(
            tags::PRIVACY,
            Severity::Info,
            "Debugger access on, set outside aterm",
        )
        .line("for the whole Mac")
        .line(&undo),
        DebuggerRecord::Off => Message::new(tags::PRIVACY, Severity::Info, "Debugger access off"),
        DebuggerRecord::WasOnNowOff => Message::new(
            tags::PRIVACY,
            Severity::Info,
            "Debugger access was on; it is off now",
        ),
    };
    msg.record().key(KEY_DEBUGGER_ACCESS)
}

/// A harness note (`aterm ctl appnotice harness <text>`: an agent harness's acts,
/// posted from outside the window): a `harness`-tagged record, never a row.
/// The words are the wire's, sanitized and capped like any appnotice text.
pub(crate) fn harness_note(text: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Info,
        atpkg::progress::sanitize_for_tty(text, 120),
    )
    .record()
}

/// `text` led by the tab it is about (`Tab 3: …`, `Window 2, tab 1: …`), the
/// words a person finds it by; a session no tab shows (`in its tab`) leads
/// with nothing.
fn tab_led(place: &str, text: &str) -> String {
    match place.strip_prefix("in ") {
        Some(bare) if bare != "its tab" => {
            let mut chars = bare.chars();
            let cap: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            format!("{cap}: {text}")
        }
        _ => text.to_string(),
    }
}

/// What the record of a held tab says of the agent it ran
/// ([`restored_agent_held`]).
pub(crate) struct HeldAgent<'a> {
    /// The agent as a person names it (`restore::AgentRestore::name`), read
    /// only where [`Self::stopped_by_hold`]: a relaunch was owed, so its lane
    /// is known.
    pub(crate) name: &'static str,
    /// The line that resumes it on its conversation in the tab
    /// (`restore::AgentRestore::resume_line`), when one can be said.
    pub(crate) resume: Option<&'a str>,
    /// The cold restore would have relaunched it but for the hold (a host
    /// that relaunches restored agents, and a conversation to resume): the
    /// hold is said as the reason only then.
    pub(crate) stopped_by_hold: bool,
    /// The fleet's hold stood alone, which a new aterm does not put back on
    /// a fresh shell; else the owner's came back (beside a fleet's or not).
    pub(crate) fleet: bool,
    /// The owner's hold that came back was lifted before this was said (in
    /// a cold restore's drain: `App::let_go_lifted_held`): nothing is left
    /// to lift.
    pub(crate) lifted: bool,
}

/// The agent of a tab that was held when aterm ended is not relaunched. The
/// tab reopened at its shell (`place`, its session `sid`). Where the hold is
/// what kept its agent down ([`HeldAgent::stopped_by_hold`]) the record says
/// so, with the line that resumes it — the person's to type — and whose hold
/// it was; else (the harness off: nothing would have relaunched it) it says
/// only that the owner's hold came back, and a fleet's tab says nothing
/// (`None`). The lift it names is the Owner's `hold <sid> off`, whose session
/// is its argument (a selector is refused), and the Fabric menu's Lift Hold; a
/// hold the owner already lifted ([`HeldAgent::lifted`]) is said lifted, and
/// without an agent to name its record is `None` too. A `harness`-tagged
/// record: the person held the tab themselves and nothing is asked of them —
/// the reopened row (`journal_reopened_message`) already counts the agent as
/// not come back. Keyed per tab.
pub(crate) fn restored_agent_held(
    sid: &str,
    place: &str,
    agent: &HeldAgent<'_>,
) -> Option<Message> {
    let line =
        |text: &str| atpkg::progress::sanitize_for_tty(text, aterm_messages::DETAIL_LINE_CAP);
    // The Owner's lift: `hold` takes its session as the argument.
    let lift = |lead: &str| {
        format!(
            "{lead} with the Fabric menu's Lift Hold, or aterm ctl hold {sid} off, when drivers \
             may type there again"
        )
    };
    let message = if agent.stopped_by_hold {
        let name = agent.name;
        let message = Message::new(
            tags::HARNESS,
            Severity::Info,
            atpkg::progress::sanitize_for_tty(
                &tab_led(
                    place,
                    &format!(
                        "{name} was not restarted because the tab was held when aterm stopped"
                    ),
                ),
                200,
            ),
        );
        // The line alone on its row, never cut: a cut command would run
        // something nobody said.
        let message = match agent
            .resume
            .filter(|resume| resume.chars().count() <= aterm_messages::DETAIL_LINE_CAP)
        {
            Some(resume) => message
                .line("To pick its conversation up, type in the tab:")
                .line(line(resume)),
            None => message.line(line(&format!(
                "To pick its conversation up, start {} there again",
                match name {
                    "Claude" => "Claude Code",
                    other => other,
                }
            ))),
        };
        message.line(line(&if agent.fleet {
            "The fleet's hold did not carry over: a fleet hold lasts as long as the aterm that \
             took it"
                .to_string()
        } else if agent.lifted {
            "Your hold came back on the tab, and has been lifted since".to_string()
        } else {
            lift("Your hold came back on the tab: lift it")
        }))
    } else if agent.fleet || agent.lifted {
        // Nothing would have relaunched it, and nothing stands to lift.
        return None;
    } else {
        Message::new(
            tags::HARNESS,
            Severity::Info,
            atpkg::progress::sanitize_for_tty(
                &tab_led(place, "your hold on the tab came back after aterm stopped"),
                200,
            ),
        )
        .line(line(&lift("Lift it")))
    };
    Some(
        message
            .record()
            .key(format!("{KEY_RESTORED_HELD}.{sid}").as_str()),
    )
}

/// A session supervisor's information (`aterm_agent::supervise::IdleHost::
/// inform`, from the window's own supervise loop for session `sid`): a
/// `harness`-tagged record, never a row — nothing is asked of the person.
/// Codex's save-then-wait hold begins with one (a hold can last
/// days, the session idle through it). Keyed per session: the next one
/// replaces it. The text's first line is the title and each further line a
/// detail line: a frozen agent's warning names its state first and its
/// command after, and on one line the title's width cut the command off.
pub(crate) fn supervisor_note(sid: &str, place: &str, text: &str) -> Message {
    let mut lines = text.lines();
    let title = lines.next().unwrap_or_default();
    Message::new(
        tags::HARNESS,
        Severity::Info,
        atpkg::progress::sanitize_for_tty(&tab_led(place, title), 200),
    )
    .lines(lines.map(|l| atpkg::progress::sanitize_for_tty(l, aterm_messages::DETAIL_LINE_CAP)))
    .record()
    .key(format!("harness.supervisor.{sid}").as_str())
}

/// A session's Autopilot, set from the window (the chip, Settings ▸ Harness:
/// `App::set_session_autopilot`): a `harness`-tagged record, never a row — the
/// person set it, so nothing is asked of them — titled with the mode the
/// session now runs under (`view`, its effective one), its first line led by
/// the tab it is about and saying what that mode does, its second whether the
/// mode is the session's own word (`own`) or the table's default it was handed
/// back to, and, where the change dropped an armed timer (`cancelled`), a
/// third that says so (`the 18:00 landing time is cancelled`). Its last line
/// says who set it (`set_by`, composed by the caller from the stored setter:
/// `set from the chip`, `set over aterm ctl`) — on a line, never in
/// the title, which the glass title rule holds to six words. Keyed per
/// session: the next one replaces it.
pub(crate) fn autopilot_set(
    sid: &str,
    place: &str,
    view: crate::autopilot::AutopilotView,
    own: bool,
    cancelled: Option<crate::claude_lights::LandAtGlass>,
    set_by: Option<&str>,
) -> Message {
    use crate::autopilot::AutopilotView;
    let agent_off;
    let (title, what): (&str, &str) = match view {
        AutopilotView::Cruise => (
            "Autopilot set to cruise",
            "the harness keeps the work moving",
        ),
        AutopilotView::Land => (
            "Autopilot set to land",
            "the harness wraps the work up, then stops",
        ),
        AutopilotView::Off => ("Autopilot set to off", "the harness keeps its hands off"),
        AutopilotView::MasterOff => (
            "Autopilot set, harness off",
            "the harness is off in Settings, so nothing is supervised",
        ),
        AutopilotView::Unhosted => (
            "Autopilot set, harness off",
            "this aterm has no control socket, so nothing is supervised",
        ),
        AutopilotView::AgentOff(agent) => {
            agent_off = format!(
                "{} in Settings, so the harness supervises nothing here",
                crate::autopilot::agent_switch_off(agent)
            );
            ("Autopilot set, supervision off", agent_off.as_str())
        }
    };
    let whose = if own {
        "this session's own setting"
    } else {
        "back to the default, [harness] autopilot"
    };
    let message = Message::new(tags::HARNESS, Severity::Info, title)
        .line(atpkg::progress::sanitize_for_tty(
            &tab_led(place, what),
            aterm_messages::DETAIL_LINE_CAP,
        ))
        .line(whose);
    let message = match cancelled {
        Some(at) => message.line(format!(
            "the {} landing time is cancelled",
            crate::autopilot_menu::when(at)
        )),
        None => message,
    };
    set_by_line(message, set_by)
        .record()
        .key(format!("harness.autopilot.{sid}").as_str())
}

/// An Autopilot record's last line, who set it, where the caller
/// has one: sanitized and cut as every detail line is.
fn set_by_line(message: Message, set_by: Option<&str>) -> Message {
    match set_by {
        Some(set_by) => message.line(atpkg::progress::sanitize_for_tty(
            set_by,
            aterm_messages::DETAIL_LINE_CAP,
        )),
        None => message,
    }
}

/// A timed Cruise set from the window (the Autopilot menu, Settings ▸
/// Harness: `App::set_session_autopilot_timed`): a `harness` record,
/// keyed per session like [`autopilot_set`], titled with the time it lands
/// under the glass title rule (`Autopilot cruises until 18:00`; `… until Sat
/// 16:00` where it falls on another day), its line led by the tab
/// and saying what happens at the time, its last who set it (`set_by`, as
/// [`autopilot_set`]'s).
pub(crate) fn autopilot_timer_set(
    sid: &str,
    place: &str,
    until: crate::claude_lights::LandAtGlass,
    set_by: Option<&str>,
) -> Message {
    let message = Message::new(
        tags::HARNESS,
        Severity::Info,
        format!(
            "Autopilot cruises until {}",
            crate::autopilot_menu::when(until)
        ),
    )
    .line(atpkg::progress::sanitize_for_tty(
        &tab_led(
            place,
            "the harness keeps the work moving, then wraps it up at that time",
        ),
        aterm_messages::DETAIL_LINE_CAP,
    ));
    set_by_line(message, set_by)
        .record()
        .key(format!("harness.autopilot.{sid}").as_str())
}

/// What a timed Cruise's fire found the session doing:
/// what [`autopilot_timer_ended`]'s line says follows Autopilot's switch to
/// land — only work the harness carries is wrapped up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FireOver {
    /// Work the harness carries: it lands it at its next point. (Cruise
    /// takes nothing as the end, and the turn to Land starts a landing
    /// whatever the face said, so the fire never finds a task already over.)
    Work,
    /// The master switch is off: nothing is supervised.
    MasterOff,
    /// This agent's own switch is off.
    AgentOff(aterm_phase::Program),
    /// No host: this aterm has no control socket.
    Unhosted,
    /// No agent runs in the pane the timer was set on (a shell, an
    /// editor); `split`: its tab holds other panes — one may run an agent
    /// — so the line says `this pane`, not `the tab`.
    NoAgent { split: bool },
}

/// A timed Cruise's time came: the window's fire stored
/// `land` (`App::expire_autopilot_timers`). A `harness` record, never a row:
/// nothing is asked of the person, and the chip already says it. Titled
/// `Autopilot switched to land` in the
/// words the chip, the menu and its sibling records use (`autopilot: land`,
/// `Autopilot set to land`); its line says when Autopilot switched and what
/// that does to what the fire found (`over`): the work landed, or nothing to
/// land — the harness or the agent's switch off, no host, no agent in the
/// pane or the tab. Where the time passed while aterm, or only the tab, was
/// closed (a restore found it behind), its line says which instead. `clock`
/// is the time it landed at (`None`: the zone could not be read). Its last
/// line says who set it: the timer (`set_by`, `set at the 18:00 timer`).
/// Keyed per session like [`autopilot_set`]: it replaces the
/// record of the set.
pub(crate) fn autopilot_timer_ended(
    sid: &str,
    place: &str,
    clock: Option<&str>,
    passed_while: Option<crate::autopilot_timers::ClosedOver>,
    over: FireOver,
    set_by: Option<&str>,
) -> Message {
    use crate::autopilot_timers::ClosedOver;
    let time = clock.map_or_else(
        || "landing time".to_string(),
        |c| format!("{c} landing time"),
    );
    let what = match passed_while {
        Some(ClosedOver::Aterm) => {
            format!("the {time} passed while aterm was closed; land is on")
        }
        Some(ClosedOver::Tab) => format!("the {time} passed while the tab was closed; land is on"),
        None => {
            let turned = clock.map_or_else(
                || "Autopilot switched to land".to_string(),
                |c| format!("at {c} Autopilot switched to land"),
            );
            let then = match over {
                FireOver::Work => "the harness lands all of the work on the remote main branch, \
                                   then stops"
                    .to_string(),
                FireOver::MasterOff => {
                    "the harness is off in Settings, so nothing is landed".to_string()
                }
                FireOver::AgentOff(agent) => format!(
                    "{}, so nothing is landed",
                    crate::autopilot::agent_switch_off(agent)
                ),
                FireOver::Unhosted => {
                    "this aterm has no control socket, so nothing is landed".to_string()
                }
                FireOver::NoAgent { split: false } => {
                    "no agent runs in the tab, so there is nothing to land".to_string()
                }
                FireOver::NoAgent { split: true } => {
                    "no agent runs in this pane, so there is nothing to land".to_string()
                }
            };
            format!("{turned}; {then}")
        }
    };
    let message = Message::new(tags::HARNESS, Severity::Info, "Autopilot switched to land").line(
        atpkg::progress::sanitize_for_tty(&tab_led(place, &what), aterm_messages::DETAIL_LINE_CAP),
    );
    set_by_line(message, set_by)
        .record()
        .key(format!("harness.autopilot.{sid}").as_str())
}

/// A session supervisor's warning (`aterm_agent::supervise::IdleHost::warn`,
/// from the window's own supervise loop for session `sid`): a warning row on
/// the band — something the person may want to act on, nothing asked of
/// them, nothing held back. The memory ceiling says one an episode, and
/// again whenever what it says comes next changes (the Mac falling into
/// trouble or recovering, another agent now the larger): its text names the
/// footprint, the Mac's state and the tab's own resume line (`claude --resume
/// <id>`, `codex resume <thread>`), kept whole on one detail line: the
/// detail is judged as the row lays it out (400 characters, split into
/// lines at its joints), and a line that would be cut or split steps down
/// whole, else is named not at all
/// ([`aterm_agent::supervise::memory::never_cut`]). Keyed per session: the
/// next one replaces it, so the row never says `a warning only` while aterm
/// restarts the agent; it leaves on the severity's hold.
pub(crate) fn supervisor_warning(sid: &str, place: &str, text: &str) -> Message {
    let detail = aterm_agent::supervise::memory::never_cut(text, |text| {
        aterm_messages::text::split_sentence(
            &atpkg::progress::sanitize_for_tty(&tab_led(place, text), 400),
            aterm_messages::DETAIL_LINE_CAP,
        )
        .join("\n")
    });
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        "Agent memory past its ceiling",
    )
    .lines(detail.lines().map(str::to_string))
    .key(format!("harness.memory.{sid}").as_str())
}

/// The live agent upgrade's supersede key: each new waiting record replaces
/// the last one recorded under it.
pub(crate) const KEY_AGENT_UPGRADE: &str = "harness.upgrade";

/// The live agent upgrade, waiting. A record, never a
/// row — waiting for its moment is the upgrade working, and the band carries
/// only what the person acts on — so it is on Settings ▸
/// Messages, `messages.log` and `appstatus` as `kind=harness`. `product` is
/// the agent the waiting sessions run (`Claude Code`, `Codex`; `Claude Code
/// and Codex` for a mix, named in the detail under the generic title),
/// `waiting` the versions they move to, one per session; `None` when none
/// waits. One session's record is [`agent_upgrade_waiting_in`]'s.
///
/// It says there is nothing to do,
/// and never "at its next turn end" — false of a goal-mode Codex, whose next
/// turn begins within 14 ms of the last: the upgrade starts at a quiet
/// moment, the longer it waits the less it asks of one (the ladder).
pub(crate) fn agent_upgrade_waiting(product: &str, waiting: &[&str]) -> Option<Message> {
    let first = *waiting.first()?;
    let n = waiting.len();
    // A mix of products (`Claude Code and Codex`) is named in the detail:
    // the title keeps to the six-word rule with the generic word.
    let (titled, mixed) = if product.contains(" and ") {
        ("agent", Some(product))
    } else {
        (product, None)
    };
    let title = if waiting.iter().all(|v| *v == first) {
        let target = format!("{titled} {}", atpkg::progress::sanitize_for_tty(first, 24));
        if n == 1 {
            format!("{target} installs itself")
        } else {
            format!("{target} installs itself in {n} tabs")
        }
    } else {
        format!("Newer {titled} builds install themselves")
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Info, title);
    if let Some(both) = mixed {
        msg = msg.line(format!("{both} sessions"));
    }
    msg = msg.line(if n == 1 {
        "nothing to do: it starts on its own at a quiet moment in its tab"
    } else {
        "nothing to do: each starts on its own at a quiet moment in its tab"
    });
    Some(msg.no_excerpt().hold(Hold::LogOnly).key(KEY_AGENT_UPGRADE))
}

/// One session's upgrade, waiting: a
/// record titled by its tab — `Codex 0.158.0 installs itself in tab 1` —
/// whose sentence is worded by what holds it
/// ([`aterm_agent::harness::upgrade_drive::Row::waiting_line`]): where only
/// the ladder's comfort does, that there is nothing to do and when the
/// ladder stops waiting for it — `nothing to do: it starts on its own at a
/// quiet moment in the tab, and from 2:15 PM as soon as it is idle and nobody
/// has typed there for 20 seconds` (its Land rung,
/// [`aterm_agent::harness::upgrade_drive::Row::lands_by`], on the person's
/// clock; `from two hours behind` where the local clock's offset is
/// unknown); where a floor stands — a draft, a dialog, the agent's own work
/// — what it is and when it moves, never the pause in the person's typing.
/// A clock time, not a countdown: said once, it stays
/// true. Then the tab and the move. No tab mark and no warning: the upgrade
/// is working. `place` names the
/// tab (`in tab 1`); the title drops it where it would break the title rule,
/// and the move line keeps it.
pub(crate) fn agent_upgrade_waiting_in(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> Message {
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let head = format!("{} {} installs itself", row.agent.product(), clean(&row.to));
    let placed = format!("{head} {place}");
    let title = if aterm_messages::text::glass_title_fault(&placed).is_none() {
        placed
    } else {
        head
    };
    // The Land rung's own time, past or to come: the words stay what they
    // were, so the record is written once (a time that followed the clock
    // past it would be a new record at every look). What holds it may change
    // from look to look; the host says each sentence once, and keeps a
    // floor's over the ladder's comfort (`App::record_upgrade_waiting`).
    let from = row
        .lands_by()
        .and_then(|at| {
            let offset = crate::presence::local_offset_s()?;
            Some(aterm_messages::words::clock_words(
                at.saturating_mul(1000),
                offset,
            ))
        })
        .unwrap_or_else(|| "two hours behind".to_string());
    Message::new(tags::HARNESS, Severity::Info, title)
        .line(row.waiting_line(&from))
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .no_excerpt()
        .hold(Hold::LogOnly)
        .key(KEY_AGENT_UPGRADE)
}

/// The live agent upgrade, stalled: a row when only the person can move it
/// — it was refused, stopped the same way round after round, runs where
/// typing into the tab cannot reach, or has stood four hours on the ladder's last rung
/// with no pause (`overdue`, six hours behind:
/// [`aterm_agent::harness::upgrade_drive::Row::stall`]). One that will ask
/// again on its own — still asking, or resting after a round that gave up or
/// stopped once — is a record with no tab mark
/// ([`aterm_agent::harness::upgrade_drive::Row::asks_on_its_own`]),
/// and names the clock time its next round starts, so its
/// words never need restating.
/// `Standing`: it stays until the stall ends (the host resolves it) or the
/// person reads it. `detail[0]` is why; then what moves it; the tab and the
/// move ([`STALL_MOVE_LINE`]); what runs under the agent and holds it, when
/// anything does (`held by pid …`: pid, name and age, never a command); and
/// the same words spelled for a shell, always the last line —
/// any shell, never the stalled tab's own (its foreground is Claude, and a
/// line typed there is a prompt). Keyed per tab ([`agent_upgrade_key`]).
///
/// The remedy is the one that works for this kind of stall.
/// Overdue: it has stood on the ladder's last rung for hours, which `--now`
/// is too, so what it waits on is named — a turn running, the
/// READY answer, a draft, a box, a hold, work
/// under the agent — and `--now` is offered only where it still moves it
/// (its own text left typed, a full queue). Gave up: `--now` asks it
/// again (at once, where its own next round is at a clock time). Held back in
/// a pane: typing into the tab cannot reach it, so no word moves it — quit it
/// in its pane and resume it there, or `--skip`. Refused or failed: the
/// harness asks it again after a rest (at a clock time, once known) — to move
/// it sooner, quit and resume it by hand, or `--skip`; stopped after the
/// agent it ended was seen gone, nothing runs to quit — `codex resume`, or
/// `claude --resume <conversation>`, in the tab takes it back. Under way and
/// not moving (`stuck:*`): no word moves it and nothing is
/// forced — the row offers none and names `--status`.
pub(crate) fn agent_upgrade_stalled(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
    place: &str,
) -> Message {
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    // The one row that stands: the agent the upgrade ended and did not bring
    // back, or a shutdown it asked for that hung. Every other upgrade wait is
    // a record — the log, and after a day the digest ([`Row::attention`]).
    let interrupt = row.interrupt(now);
    // The band's excerpt: a person's words, no code quoting.
    let why = row
        .stall_words(now)
        .unwrap_or_else(|| "it is not moving".to_string())
        .replace('`', "");
    let tab = clean(&row.tab);
    let from = clean(&row.from);
    let cmd = format!("aterm harness upgrade {tab}");
    // The remedy names the row's own buttons: the row carries the words that
    // move it, so the shell spelling of the same words is the last, technical
    // line — never the first way offered. The row's own capsule by its label,
    // else the tab menu's item by the menu's own label (`Skip This Version`),
    // which offers every word.
    let offered = agent_upgrade_words(row, now);
    let skip = if agent_upgrade_capsules(row, now).iter().any(|intent| {
        matches!(
            intent,
            Intent::AgentUpgrade {
                word: UpgradeWord::Skip,
                ..
            }
        )
    }) {
        format!("{} keeps it on {from}", UpgradeWord::Skip.label())
    } else if offered.contains(&UpgradeWord::Skip) {
        format!(
            "{} in the tab's menu keeps it on {from}",
            crate::session_chrome::upgrade_menu_label(UpgradeWord::Skip)
        )
    } else {
        format!("`{cmd} --skip` keeps it on {from}")
    };
    let now_word = UpgradeWord::Now.label();
    let who = agent_word(row.agent);
    let under_way = matches!(
        row.phase,
        aterm_agent::harness::upgrade::Phase::Exiting { .. }
            | aterm_agent::harness::upgrade::Phase::Relaunched { .. }
    );
    let (remedy, shell_words) = match row.remedy(now) {
        // Nothing has checked it (`Row::unwatched`): the host's watch has not
        // read the record for an hour past its due look, the Mac awake well
        // past its wake grace — aterm's own failure, which no word of the
        // owner's mends. The first line says whose it is, so this one offers
        // only the skip, the one act that is the owner's; the shell line names
        // `--status`, which says when it was last looked at, and by which
        // build.
        Some(Remedy::Waits) if row.stall(now).as_deref() == Some("unwatched") => {
            (skip.clone(), "--status` or `--skip")
        }
        // A move under way that has not moved: the harness refuses every word
        // while it is under way, and forces nothing — the row offers no word,
        // and names the shell's view of it.
        Some(Remedy::Waits) if under_way => (
            "no word moves it while it is under way: it goes on once what it waits on ends, \
             and nothing is forced"
                .to_string(),
            "--status",
        ),
        // Its question waits unread behind a full queue: the agent's next
        // run takes the question, and the owner's word types it once more; so
        // does the upgrade itself once the queue has rested, a rest that grows
        // with every copy unread, up to a day (`upgrade::queue_rest`), so no
        // one interval is promised.
        Some(Remedy::Now) if row.wait == "queued" => {
            const _: () = assert!(aterm_agent::harness::upgrade::QUEUE_REST_MAX_S == 86_400);
            (
                format!(
                    "it moves when {who} next runs: type in its tab, or {now_word} asks it once \
                     more (it also asks again by itself, waiting longer each time, up to a day); \
                     {skip}"
                ),
                "--now` or `--skip",
            )
        }
        // A round re-armed after a give-up, its own work holding it:
        // a record, and `Upgrade now` moves nothing sooner there.
        Some(Remedy::Now) if own_work_round(row, now) => (
            format!(
                "{now_word} cannot move it past its own work: it moves once that ends or the \
                 agent stops it; {skip}"
            ),
            "--skip",
        ),
        // Its own text left typed, backing off (the only other wait `--now`
        // still moves once the ladder stands at its last rung): tried again
        // at once. Never "at its next turn end" (a goal-mode Codex's turns
        // never end).
        Some(Remedy::Now) | None => (
            format!("{now_word} tries it again at its next pause; {skip}"),
            "--now` or `--skip",
        ),
        // The agent's own work is never ended for it. The move comes once that
        // work ends, or once the agent stops it. After a notice, the agent is
        // asked again once half an hour has passed — at a pause of its own, a
        // break or an idle point, never between them. Four notices end in a
        // give-up, a stall of its own. When it was last asked is the record's
        // (`Row::noticed_at`), said as a clock time, and the last notice says
        // the give-up comes next ([`asked_again_words`]). The line stays under
        // `DETAIL_LINE_CAP`, which clips. Under the quiet flow (`Row::quiet`)
        // nothing asks an announced round again and nothing gives it up, so
        // neither is promised.
        Some(Remedy::Waits) if matches!(row.wait.as_str(), "background" | "not-idle:shell") => {
            let asked = match row.phase {
                aterm_agent::harness::upgrade::Phase::Announced { at_s, asks } if !row.quiet => {
                    asked_again_words(
                        row.noticed_at,
                        at_s,
                        asks,
                        now,
                        crate::presence::local_offset_s(),
                    )
                }
                _ => String::new(),
            };
            // No raw wait word: the shell line below names the command.
            (
                format!(
                    "{now_word} cannot move it past its own work: it moves once that ends or \
                     the agent stops it{asked}; {skip}"
                ),
                "--skip",
            )
        }
        // aterm's own fence fails under the owner's `--now`:
        // the word is in force and asked for the very notice
        // aterm cannot type — the why line above says so, and how often — so
        // the row offers no `Upgrade now`, says aterm tries again at the next
        // quiet moment, and names what the owner can still do.
        Some(Remedy::Waits) if row.fence_fails_under_now() => (
            format!(
                "{now_word} is already in force: aterm tries its notice again at the next quiet \
                 moment; {skip}"
            ),
            "--skip",
        ),
        // A usage limit whose reset has passed ([`Row::limit_passed`]):
        // the wait it names is over, so "it moves once that ends" would tell
        // the owner to wait for what already ended. The first line already
        // says the reset passed. Where the reset was named, the quiet upgrade
        // moves the tab at the point its loop offers then, so a row that
        // stands past that point is one it could not move: no reset named, or
        // the ask flow, whose notice holds every point. The one act that moves
        // it is a person continuing the session there.
        Some(Remedy::Waits) if row.limit_passed(now) => (
            format!(
                "type in its tab to continue the session, and it moves at its next pause; {skip}"
            ),
            "--skip",
        ),
        // A spend or credit wall naming no reset (`Row::spend_held`):
        // no usage window, so "it moves once that ends" would wait on what
        // never ends by itself. The owner's hold: named, with what lifts it.
        Some(Remedy::Waits) if row.spend_held() => (
            format!("add credits or raise the cap, and it moves at the next pause; {skip}"),
            "--skip",
        ),
        // Nothing holds it now (`upgrade::NEXT_POINT`): the last
        // look found its next step due at a point nothing holds. The first
        // line already says when it goes, so this one offers only the skip.
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade::NEXT_POINT => {
            (skip.clone(), "--skip")
        }
        // Its agent not in front in its tab, the tab under another
        // supervisor's claim (`Row::aside`), or its Autopilot off:
        // no worker of aterm's runs there, so none of its steps or looks
        // comes; what moves it is the hand that holds it.
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade_drive::NOT_FOREGROUND => (
            format!("it moves once it is back in front in its tab (fg there); {skip}"),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade_drive::CLAIMED => (
            format!("it moves once the supervisor holding its tab lets it go; {skip}"),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade_drive::AUTOPILOT_OFF => (
            format!("it moves once its autopilot is cruise or land again; {skip}"),
            "--skip",
        ),
        // A quiet round its agent's own work gave up on (`Row::rests_on_own_work`):
        // resting, its wait reads `failed`, but what holds it is that work —
        // `Upgrade now` re-arms a round the quiet restart's gate holds on it
        // all the same. Worded as the wait on its own work is, the line below
        // naming what runs under the agent.
        Some(Remedy::Waits) if row.rests_on_own_work() => (
            format!(
                "{now_word} cannot move it past its own work: it moves once that ends or the \
                 agent stops it; {skip}"
            ),
            "--skip",
        ),
        Some(Remedy::Waits) => (
            format!("{now_word} does not move it past that wait: it moves once that ends; {skip}"),
            "--skip",
        ),
        // Under the quiet flow the round after the rest asks nothing: it
        // moves the session at a quiet moment. Only a give-up a status that
        // lags alone held reaches here — the owner's `--now` reads the lag
        // past (`upgrade::quiet_step`); one its own work held is a wait (the
        // arm above).
        Some(Remedy::AskAgain) if row.quiet => (
            match next_round_clock(row, now) {
                Some(at) => format!(
                    "it moves on its own at a quiet moment from {at}; {now_word} moves it at \
                     its next pause; {skip}"
                ),
                None => format!("{now_word} moves it at its next pause; {skip}"),
            },
            "--now` or `--skip",
        ),
        Some(Remedy::AskAgain) => (
            match next_round_clock(row, now) {
                Some(at) => format!(
                    "it asks again on its own at {at}; {now_word} asks it at its next pause; \
                     {skip}"
                ),
                None => format!("{now_word} asks it again at its next pause; {skip}"),
            },
            "--now` or `--skip",
        ),
        // Where it runs, and what moves it, once. The pane is named by what
        // holds it; which pane of it is not known to aterm — the agent's
        // terminal is that program's.
        Some(Remedy::InItsPane) => (
            format!(
                "to move it, quit it in its {} and resume it there; {skip}",
                pane_words(row.wait.strip_prefix("terminal:").unwrap_or_default())
            ),
            "--skip",
        ),
        // The upgrade asks again, at the clock time its next round starts —
        // the repeating stop's too, or now while its re-armed round is asking.
        Some(Remedy::ByHand) => {
            let when = next_round_clock(row, now).map_or_else(
                || {
                    if matches!(row.phase, aterm_agent::harness::upgrade::Phase::Failed(_)) {
                        "after a rest".to_string()
                    } else {
                        "now".to_string()
                    }
                },
                |at| format!("at {at}"),
            );
            (
                if row.repeating_stop() {
                    format!(
                        "it stopped this way {} times in a row; the upgrade asks it again {when}, \
                         resting longer each time: to move it sooner, quit it and resume it by \
                         hand; {skip}",
                        row.stop_streak
                    )
                } else {
                    format!(
                        "the upgrade asks it again {when}; to move it sooner, quit it and resume \
                         it by hand; {skip}"
                    )
                },
                "--skip",
            )
        }
        Some(Remedy::ResumeInTab) => (
            match match row.agent {
                aterm_agent::harness::upgrade::Agent::Codex => Some("codex resume".to_string()),
                // A Claude Code move stopped after its SIGTERM: its conversation, by id.
                aterm_agent::harness::upgrade::Agent::Claude if !row.session.is_empty() => {
                    Some(format!("claude --resume {}", clean(&row.session)))
                }
                aterm_agent::harness::upgrade::Agent::Claude => Some("claude --resume".to_string()),
                // Muse Code, agy and Gemini CLI: no upgrade row of theirs
                // exists (relaunch on exit only), so none is read here, and no
                // command is told, never Claude Code's.
                aterm_agent::harness::upgrade::Agent::Muse
                | aterm_agent::harness::upgrade::Agent::Agy
                | aterm_agent::harness::upgrade::Agent::Gemini => None,
            } {
                Some(command) => format!(
                    "it no longer runs in the tab and aterm will not bring it back: `{command}` \
                     there takes its conversation back, or {} keeps this row down",
                    UpgradeWord::Skip.label()
                ),
                None => format!(
                    "it no longer runs in the tab and aterm will not bring it back; {} keeps \
                     this row down",
                    UpgradeWord::Skip.label()
                ),
            },
            "--skip",
        ),
    };
    // The title says which tab and whether it failed: an upgrade still waiting
    // says so; one that stopped, or that typing cannot reach, could not be
    // done. (`will_ask_again`, not `asks_on_its_own`: the round goes on by
    // itself at any age.)
    let resting = row.will_ask_again(now)
        && matches!(row.phase, aterm_agent::harness::upgrade::Phase::Failed(_));
    let owner_asked = row.request == aterm_agent::harness::upgrade::Request::Now;
    let title = match (&interrupt, row.remedy(now)) {
        // The row that stands says what happened to the owner's agent.
        (Some(need), _) if need.kind == "hung-exit" => {
            placed_title(&format!("{who} is not exiting"), place, "")
        }
        // The upgrade restarted it and it has reached no prompt.
        (Some(need), _) if need.kind == "relaunch-stuck" => {
            placed_title(&format!("{who} is not ready"), place, "")
        }
        (Some(_), _) => placed_title(&format!("{who} stopped"), place, ""),
        // A round that stopped and asks again on its own: the upgrade working.
        (None, _) if resting => format!("{who} upgrade retries later {place}"),
        // A wait is not a failure: only what did not work says `Couldn't`.
        (None, Some(Remedy::Now | Remedy::Waits)) => {
            placed_title(&format!("{who} upgrade waits"), place, "")
        }
        // The answer to the owner's own `Upgrade now`: the press hurried the
        // move, so nothing is stalled, and nothing failed either.
        (None, None) if owner_asked => placed_title(&format!("{who} upgrade waits"), place, ""),
        (None, _) => placed_title(&format!("Couldn't upgrade {who}"), place, ""),
    };
    // An announced move on its own work is asked again at its breaks and idle
    // points, half an hour apart at the least, and ends in a give-up, a rest
    // and a new round — the upgrade working: a record, never the glass. Past
    // the move clock (`MOVE_BUDGET_S`) it is one line in the digest
    // ([`Row::attention`]); one a person holds (a draft, a box, a hold)
    // is never asked again, and is a record too until a day without progress
    // (one log line) or, a draft nobody sent, one digest line.
    //
    // While the owner's `Upgrade now` word stands, the answer stays on the
    // glass, calm (`Info`): what it waits on, and that it moves once that
    // ends.
    //
    // A row stands only where the owner is needed now and one action changes
    // it; the answer to the owner's own press is a notice that folds after
    // [`ANSWER_MAX`]; a fault of aterm's own a Warn record; everything else a
    // calm record.
    let severity = if interrupt.is_some() {
        Severity::Warn
    } else {
        Severity::Info
    };
    // What holds it, by pid, name and age. Never a command: those are the
    // agent's own words. A technical line after the move ([`STALL_MOVE_LINE`]
    // stays put) and before the shell spelling, which stays the last; none
    // when nothing holds it.
    let held = row.held_words(now).map(|words| {
        let words = atpkg::progress::sanitize_for_tty(&words, aterm_messages::DETAIL_LINE_CAP);
        format!("held by {words}")
    });
    // The row that stands says why the owner and what the one action is — the
    // `Need`'s own words — and offers no snooze: `Not today` and `Skip
    // version` silence a session with no agent, and are no action.
    if let Some(need) = &interrupt {
        // The action is the sentence and, where it is a command, the command
        // on a line of its own: the capped sentence never carries the launch's
        // flags, and a command cut to fit it is one the owner cannot run.
        let mut msg = Message::new(tags::HARNESS, severity, title)
            .line(atpkg::progress::sanitize_for_tty(&need.why, 160))
            .line(atpkg::progress::sanitize_for_tty(&need.then, 160));
        if let Some(command) = &need.command {
            msg = msg.line(format!(
                "`{}`",
                atpkg::progress::sanitize_for_tty(command, 240)
            ));
        }
        // A standing ask: the reason is the need's own sentence and the one
        // action its own step (the engine's caps are the need's caps).
        return msg
            .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
            .line(format!("`{cmd} --status` shows what it waited on"))
            .now(Ask::standing(
                StandingWhy::OnlyYou(Reason::new(&need.why)),
                Then::step(&need.then),
            ))
            .key(&agent_upgrade_key(&row.tab));
    }
    // The why at a sentence's length; the band's excerpt is clipped on its own.
    let mut msg = Message::new(tags::HARNESS, severity, title)
        .line(atpkg::progress::sanitize_for_tty(&why, 160))
        .line(remedy)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .line(held.unwrap_or_default())
        .line(format!("the same in any shell: `{cmd} {shell_words}`"))
        .key(&agent_upgrade_key(&row.tab));
    // The answer to the owner's own press names no snooze either: it answers a
    // press, and stays only for a glance (a notice of their own gesture, the
    // span of which the engine owns: [`ANSWER_MAX`]). Everything else is a
    // record; aterm's own fault is told by its record (`agent_upgrade_fault`,
    // a Warn), and the wait's own stays calm.
    if owner_asked {
        return msg.notice(NoticeKind::Answer);
    }
    for intent in agent_upgrade_capsules(row, now) {
        msg = msg.action(intent);
    }
    msg.record()
}

/// The key of the record of aterm's own fault in one tab's upgrade: its own,
/// beside the stall's ([`agent_upgrade_key`]), so a fault coming and going never
/// restates or resolves the tab's stall row.
pub(crate) fn agent_upgrade_fault_key(tab: &str) -> String {
    format!(
        "harness.upfault.{}",
        atpkg::progress::sanitize_for_tty(tab, 64)
    )
}

/// aterm's own fault in an upgrade, as a record (an alert persists only where
/// the owner can act, and there is nothing to do about a wait aterm does not
/// classify, a notice its screen keeps refusing, a record no look has visited
/// or a relaunched agent that never idles): one Warn line in the log, for the
/// owner to read later as the developer — never a row, a mark or a digest
/// line. The wait log, `--status --json`, says the rest; the `Need`'s own
/// words are the two lines.
pub(crate) fn agent_upgrade_fault(
    row: &aterm_agent::harness::upgrade_drive::Row,
    need: &aterm_agent::harness::upgrade_drive::Need,
    place: &str,
) -> Message {
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 160);
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Warn,
        placed_title(need.title, place, ""),
    )
    .line(clean(&need.why))
    .line(clean(&need.then))
    .line(format!(
        "{place} \u{00b7} {}",
        atpkg::progress::sanitize_for_tty(&row.move_words(), 64)
    ));
    if let Some(command) = &need.command {
        msg = msg.line(format!("`{}`", clean(command)));
    }
    msg.record().key(&agent_upgrade_fault_key(&row.tab))
}

/// How long the answer to an owner's `Upgrade now` stays on the band: two
/// minutes, then it folds into the log. The span is the
/// engine's ([`NoticeKind::Answer`]); the tests read it here.
#[cfg(test)]
pub(crate) const ANSWER_MAX: Duration = aterm_messages::need::MAX_TRANSIENT;

/// Where a held-back agent runs, as a person names it: `tmux
/// pane`, `screen window`, `<program> pane`, or `own terminal` when its owner
/// gave no name.
fn pane_words(owner: &str) -> String {
    match owner {
        "tmux" => "tmux pane".to_string(),
        "screen" => "screen window".to_string(),
        "" | "none" | "?" | "-" => "own terminal".to_string(),
        name => format!("{} pane", atpkg::progress::sanitize_for_tty(name, 32)),
    }
}

/// When an announced move was last asked, and when it is asked again — from
/// the record, never a promise. The re-ask is taken only at the
/// session's next break or idle point, and a session that offers neither is
/// asked nothing, whatever the clock says.
///
/// * `noticed_at` — when the latest notice was typed (`Row::noticed_at`;
///   `0`: not recorded, and no "last asked" is said). Not `clock_s`: that
///   one a usage limit's episode and a void's release move on with nothing
///   asked.
/// * `clock_s` — the re-ask clock's start (`Phase::Announced::at_s`): the
///   re-ask is due [`upgrade::REASK_S`] after it.
/// * `asks` — the notices typed this round. At [`upgrade::MAX_ASKS`] the
///   step past the clock is the give-up, not a fifth notice,
///   and the words say so.
/// * `offset` — the local zone's ([`crate::presence::local_offset_s`]):
///   times said on the clock, with the day when it is not today
///   ([`clock_on_day`]: `yesterday 10:48 AM`); `None`: said as ages, in the
///   why line's units (`10 min ago`, `28 h ago`; [`elapsed_words`]), a notice
///   typed under a minute ago `just now`, and a re-ask due within the minute
///   as due — `in 1 min` would put it off.
///
/// So: `, last asked 2:04 PM, asked again at its next break or idle point
/// from 2:34 PM` while not yet due; `, last asked yesterday 2:04 PM; asks
/// again at its next break or idle point` once due — nothing can say when a
/// point comes; `, last asked 2:04 PM (the last of 4 notices); gives up at
/// its next break or idle point` on the last.
///
/// [`upgrade::REASK_S`]: aterm_agent::harness::upgrade::REASK_S
/// [`upgrade::MAX_ASKS`]: aterm_agent::harness::upgrade::MAX_ASKS
/// [`elapsed_words`]: aterm_agent::harness::upgrade_drive::elapsed_words
fn asked_again_words(
    noticed_at: u64,
    clock_s: u64,
    asks: u32,
    now: u64,
    offset: Option<i64>,
) -> String {
    use aterm_agent::harness::upgrade::{MAX_ASKS, REASK_S};
    use aterm_agent::harness::upgrade_drive::elapsed_words;
    const POINT: &str = "at its next break or idle point";
    let when = |at: u64, ago: bool| match offset {
        Some(offset) => clock_on_day(at, now, offset),
        // Under a minute: `1 min ago` would put a notice typed seconds ago a
        // minute back.
        None if ago && now.saturating_sub(at) < 60 => "just now".to_string(),
        None if ago => format!("{} ago", elapsed_words(now.saturating_sub(at))),
        None => format!("in {}", elapsed_words(at.saturating_sub(now))),
    };
    let last = (noticed_at != 0).then(|| when(noticed_at, true));
    let due = (clock_s != 0)
        .then(|| clock_s.saturating_add(REASK_S))
        .filter(|due| now < *due && (offset.is_some() || *due >= now.saturating_add(60)))
        .map(|due| {
            if offset.is_some() {
                format!(" from {}", when(due, false))
            } else {
                format!(" {} or later", when(due, false))
            }
        });
    if asks >= MAX_ASKS {
        let last = last.map_or_else(String::new, |l| format!(", last asked {l}"));
        let from = due.unwrap_or_default();
        return format!("{last} (the last of {MAX_ASKS} notices); gives up {POINT}{from}");
    }
    match (last, due) {
        (Some(last), Some(from)) => format!(", last asked {last}, asked again {POINT}{from}"),
        (Some(last), None) => format!(", last asked {last}; asks again {POINT}"),
        (None, from) => format!(", asked again {POINT}{}", from.unwrap_or_default()),
    }
}

/// `at` (unix seconds) on the reader's clock `offset` from UTC, with its day
/// when that is not `now`'s: `2:04 PM`, `yesterday 2:04 PM`, `tomorrow 12:10
/// AM`, else the day as a Messages header says it (`Thursday 2:04 PM`, `Fri,
/// 18 Sep, 2:04 PM`). A Messages row sits under the day it was posted, not
/// the day of a time it names, so a bare clock time more than a day old reads
/// as today's.
fn clock_on_day(at: u64, now: u64, offset: i64) -> String {
    use aterm_messages::words::{clock_words, day_heading, local_day};
    let ms = |s: u64| s.saturating_mul(1000);
    let clock = clock_words(ms(at), offset);
    let (day, today) = (local_day(ms(at), offset), local_day(ms(now), offset));
    match day.saturating_sub(today) {
        0 => clock,
        -1 => format!("yesterday {clock}"),
        1 => format!("tomorrow {clock}"),
        _ => {
            let words = day_heading(today, day, true);
            if words.contains(',') {
                format!("{words}, {clock}")
            } else {
                format!("{words} {clock}")
            }
        }
    }
}

/// When a stopped round's next one starts, on the person's clock (`4:10 PM`),
/// or `None` while it is due, not stopped, or held by the owner's word
/// ([`aterm_agent::harness::upgrade_drive::Row::next_round_in`]). A clock
/// time, not a countdown: the record says it once and it stays true.
/// `None` too where the local clock's offset is unknown: no time is said
/// on a clock that may be hours off.
fn next_round_clock(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> Option<String> {
    let secs = row.next_round_in(now).filter(|secs| *secs > 0)?;
    let offset = crate::presence::local_offset_s()?;
    Some(aterm_messages::words::clock_words(
        now.saturating_add(secs).saturating_mul(1000),
        offset,
    ))
}

/// The key a tab's upgrade rows are posted under — its stalled row
/// ([`agent_upgrade_stalled`]) and the owner's word on it
/// ([`agent_upgrade_worded`]): `harness.upgrade.<tab>`, the tab id cleaned as
/// the rows print it.
pub(crate) fn agent_upgrade_key(tab: &str) -> String {
    format!(
        "{KEY_AGENT_UPGRADE}.{}",
        atpkg::progress::sanitize_for_tty(tab, 64)
    )
}

/// [`agent_upgrade_stalled`] where its tab is — and the row that stands takes
/// the owner to its tab. An upgrade's interrupt
/// ([`aterm_agent::harness::upgrade_drive::Row::interrupt`]: the agent it
/// ended, a shutdown that hung, a relaunch that reached no prompt) asks for a
/// hand in that tab, and with more than one window its words name a window
/// (`in window 2, tab 1`) that no chrome but the Overview numbers. Where the
/// tab was found it offers `Show tab N`
/// ([`UpgradeSpot::nav`]), as the digest's lines do ([`upgrade_exceptions`]).
/// Every other entry is a record or an answer, and keeps what
/// [`agent_upgrade_stalled`] gives it.
pub(crate) fn agent_upgrade_stalled_at(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
    spot: &UpgradeSpot,
) -> Message {
    let msg = agent_upgrade_stalled(row, now, &spot.place);
    match &spot.nav {
        Some(nav) if row.interrupt(now).is_some() => msg.action(nav.clone()),
        _ => msg,
    }
}

/// Where a row's tab is, for the words of its digest entry: the place the
/// upgrade's other words use (`in tab 1`, `in window 2, tab 1`) and, when the
/// tab is found, the navigation that takes the owner to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpgradeSpot {
    /// [`tab_place`], or `in its tab` when the tab was not found.
    pub(crate) place: String,
    /// `Show tab N` for that tab, when it was found.
    pub(crate) nav: Option<Intent>,
}

/// One digest fact's rows: each row, where its tab is, and its need.
type ExceptionGroup<'a> = Vec<(
    &'a aterm_agent::harness::upgrade_drive::Row,
    &'a UpgradeSpot,
    aterm_agent::harness::upgrade_drive::Need,
)>;

/// The kinds of digest need whose action is the same in every tab, so one entry
/// names every tab that has it (the owner files it once, never a line per tab).
/// Every other kind's action is done in one tab, and is that tab's entry.
const PER_KIND: [&str; 2] = ["own-work", "typed-draft"];

/// The digest kinds whose dismissal is the owner's own choice (a launch they
/// made, a terminal they run it in): a filing holds 30 days, not 7.
const CHOICE_KINDS: [&str; 2] = ["no-terminal", "refused-launch"];

/// The upgrade's part of the exceptions digest. Every
/// row the classifier ([`aterm_agent::harness::upgrade_drive::Row::attention`])
/// calls a `Digest` becomes an [`Entry`] of [`Family::Upgrade`], keyed by the
/// kind of the wait and — where the action is done in one tab — that tab:
///
/// * `own-work`, `typed-draft`: one entry per kind, naming every
///   tab that has it (`Claude in 3 tabs is behind`); five tabs waiting on their
///   own work are one line, filed once.
/// * the rest: one entry per tab (`harness.upgrade.<tab>.suspended`).
///
/// What makes it the same fact is the key alone: a new build, a pid that came
/// and went and a tab that joined are not news (they restate the words in
/// place, at most once in ten minutes); another kind is. The words are the
/// classifier's ([`aterm_agent::harness::upgrade_drive::Need`]: `why` and
/// `then`, capped at 90 and checked by the engine's vocabulary at
/// [`Entry::message`]); this only says where, and how long a dismissal holds.
/// The rows arrive with their [`UpgradeSpot`], any order. It returns the
/// family's whole answer: the host hands it to `App::except_replace`, so a kind
/// no row has any more ends.
pub(crate) fn upgrade_exceptions(
    rows: &[(&aterm_agent::harness::upgrade_drive::Row, UpgradeSpot)],
    now: u64,
) -> Vec<Entry> {
    use aterm_agent::harness::upgrade_drive::{Attention, NeedSeverity};
    let mut facts: std::collections::BTreeMap<(&'static str, String), ExceptionGroup<'_>> =
        std::collections::BTreeMap::new();
    for (row, spot) in rows {
        if let Attention::Digest(need) = row.attention(now) {
            let subject = if PER_KIND.contains(&need.kind) {
                String::new()
            } else {
                row.tab.clone()
            };
            facts
                .entry((need.kind, subject))
                .or_default()
                .push((row, spot, need));
        }
    }
    facts
        .into_iter()
        .map(|((kind, subject), mut group)| {
            // The oldest wait speaks for the fact: the same tab every look,
            // whatever order the rows come in.
            group.sort_by_key(|(row, _, _)| (row.behind_since, row.tab.clone()));
            let (first, first_spot, need) = &group[0];
            let severity = match need.severity {
                NeedSeverity::Info => Severity::Info,
                NeedSeverity::Warn => Severity::Warn,
                NeedSeverity::Error => Severity::Error,
            };
            let mut entry = Entry::new(
                Family::Upgrade,
                &subject,
                kind,
                &upgrade_exception_what(kind, &group),
                Why::OnlyYou(Reason::new(&need.why)),
                Then::step(&need.then),
            )
            .severity(severity)
            .resurface(if CHOICE_KINDS.contains(&kind) {
                Resurface::CHOICE
            } else {
                Resurface::WAIT
            });
            // One tab: take the owner to it. Several: the list is a detail.
            if group.len() == 1
                && let Some(nav) = &first_spot.nav
            {
                entry = entry.show(nav.clone());
            }
            let mut lines = first.more_lines(need, now);
            if group.len() > 1 {
                let places: Vec<&str> = group
                    .iter()
                    .map(|(_, spot, _)| spot.place.strip_prefix("in ").unwrap_or(&spot.place))
                    .collect();
                lines.insert(0, format!("In {}", places.join(", ")));
                lines.truncate(aterm_messages::exceptions::MORE_LINES);
            }
            for line in lines {
                entry = entry.more(line);
            }
            entry.log_line(
                group
                    .iter()
                    .map(|(_, _, need)| need.log_line.as_str())
                    .collect::<Vec<_>>()
                    .join(" | "),
            )
        })
        .collect()
}

/// The glass title of one digest fact: what is the matter, in the place (one
/// tab) or the number of tabs (several). Within the title rule's six words and
/// 48 characters at any window and tab number: the first form that fits.
fn upgrade_exception_what(kind: &str, group: &ExceptionGroup<'_>) -> String {
    let (row, spot, _) = &group[0];
    let n = group.len();
    let agents: std::collections::BTreeSet<&str> =
        group.iter().map(|(r, _, _)| agent_word(r.agent)).collect();
    let who = match agents.iter().copied().collect::<Vec<_>>().as_slice() {
        [only] => (*only).to_string(),
        _ => "Agents".to_string(),
    };
    let place = &spot.place;
    let bare = place.strip_prefix("in ").unwrap_or(place);
    let from = &row.from;
    let candidates: Vec<String> = match (kind, n) {
        ("own-work", 1) => vec![
            format!("{who} {place} is still on {from}"),
            format!("{who} {place} is behind"),
            format!("{who} is behind, {bare}"),
        ],
        ("own-work", _) => vec![format!("{who} in {n} tabs is behind")],
        ("typed-draft", 1) => vec![
            format!("A draft holds {who} {place}"),
            format!("A draft holds {who}, {bare}"),
        ],
        ("typed-draft", _) => vec![format!("Drafts hold {who} in {n} tabs")],
        ("suspended", _) => vec![
            format!("{who} {place} is suspended"),
            format!("{who} is suspended, {bare}"),
        ],
        ("no-terminal", _) => vec![
            format!("{who} {place} is unreachable"),
            format!("{who} is unreachable, {bare}"),
        ],
        ("refused-launch", _) => vec![
            format!("{who} {place} cannot be resumed"),
            format!("{who} cannot be resumed, {bare}"),
        ],
        ("repeating-stop", _) => vec![
            format!("The upgrade keeps stopping {place}"),
            format!("The upgrade keeps stopping, {bare}"),
        ],
        _ => vec![format!("{who} {place} is behind")],
    };
    candidates
        .into_iter()
        .find(|title| aterm_messages::text::glass_title_fault(title).is_none())
        .unwrap_or_else(|| format!("{who} is behind"))
}

// ---------------------------------------------------------------------------
// The host's own facts: the supervisor that keeps failing, a survey the loop
// cannot close, the volume aterm cannot help, the upgrade that cannot run, a
// repaint that gave up. Each is a record when it begins (the log the owner
// reads later) and, only if it goes on, one dismissable line of the digest —
// never a band row, a tab mark or a notification.
// ---------------------------------------------------------------------------

/// A glass title that names its tab where it fits ([`placed_title`]), else the
/// head alone: `Survey will not close in its tab` is a word over the rule.
fn fitted_title(head: &str, place: &str) -> String {
    let title = placed_title(head, place, "");
    if aterm_messages::text::glass_title_fault(&title).is_none() {
        title
    } else {
        head.to_string()
    }
}

/// The key of the record of a tab's supervisor failing or a survey the loop
/// cannot close: its own stem, never the digest entry's (`harness.supervisor.
/// <tab>.<cause>`), so a record and an entry never share a key.
fn host_record_key(kind: &str, sid: &str) -> String {
    format!(
        "harness.host.{kind}.{}",
        atpkg::progress::sanitize_for_tty(sid, 40)
    )
}

/// The record of a tab's supervisor failing past its budget (`why`: its last
/// failure). Warn, in the log for the developer to read; the same words a
/// keyed tab attention carried ("supervisor keeps failing; restarting in N
/// min") minus the mark.
pub(crate) fn supervisor_failing_record(sid: &str, place: &str, why: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        fitted_title("Supervisor keeps stopping", place),
    )
    .line(format!(
        "last error: {}",
        atpkg::progress::sanitize_for_tty(why, 160)
    ))
    .record()
    .key(&host_record_key("supervisor", sid))
}

/// A tab's supervisor that has kept stopping for an hour
/// ([`aterm_messages::need::Horizon::SUPERVISOR_RESTARTS`]): one line per tab,
/// `harness.supervisor.<tab>.failing`. What the owner can do is read the
/// error; aterm cannot mend a fault it keeps hitting.
pub(crate) fn supervisor_failing_entry(sid: &str, spot: &UpgradeSpot, why: &str) -> Entry {
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        "failing",
        &fitted_title("Supervisor keeps stopping", &spot.place),
        Why::OnlyYou(Reason::new(
            "the tab's supervisor has kept stopping for over an hour, whatever aterm does",
        )),
        Then::step("Read the last supervisor error in aterm.log"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(format!(
        "last error: {}",
        atpkg::progress::sanitize_for_tty(why, 160)
    ))
    .log_line(format!("supervisor-failing sid={sid} for over an hour"));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// The digest key `family` files `(sid, cause)` under: what an entry's own
/// [`Entry::key`] is, spelled without building the entry (for its end).
fn digest_key(family: Family, sid: &str, cause: &str) -> String {
    aterm_messages::need::Fingerprint::new(family.stem(), sid, cause)
        .as_str()
        .to_string()
}

/// The digest key of [`supervisor_failing_entry`], for its end.
pub(crate) fn supervisor_failing_key(sid: &str) -> String {
    digest_key(Family::Supervisor, sid, "failing")
}

/// A feature removed while an `aterm.toml` may still configure it, and what
/// now does its work: the retired-key notice the person is told once, in the
/// messages ([`removed_setting_entry`]).
pub(crate) struct RemovedFeature {
    /// The top-level table that configured it: any spelling of it in
    /// `aterm.toml` (a header, an array of tables below it, an inline table, a
    /// dotted key) raises the entry.
    pub(crate) table: &'static str,
    /// The entry's title.
    pub(crate) what: &'static str,
    /// What replaced the feature, the entry's reason beside the table it
    /// names, on every platform.
    pub(crate) instead: &'static str,
    /// How the replacement is reached, behind Details.
    pub(crate) said: &'static str,
    /// What replaces the feature, as Manual's retired-key line names it.
    pub(crate) replaced_by: &'static str,
    /// What the feature left on this machine that nothing reads now, read
    /// off its table, and how to delete it: Details lines after
    /// [`Self::said`] (`None`: it left nothing). The table is the only
    /// record of where these are, so the entry names them while the table is
    /// still there; aterm deletes none of them.
    pub(crate) left: Option<fn(&aterm_toml::Value) -> Vec<String>>,
}

/// The removed features a config load looks for. The network
/// drive (`[net] listen|cert|key`, `[[net.connections]]`, the `dial` verbs)
/// was replaced by the fleet, and a machine that still has the keys has a
/// closed port it should hear about once. The embedded operator
/// (`[operator] enabled`) was replaced by a manager session. Each entry's one
/// step is what ends it, the table deleted from `aterm.toml`; what replaced
/// the feature is its reason and its Details.
pub(crate) const REMOVED_FEATURES: &[RemovedFeature] = &[
    RemovedFeature {
        table: "net",
        what: "Network drive removed",
        instead: "the fleet replaced the network drive",
        said: "[net] and [[net.connections]] open no port and dial nothing now. The fleet \
               replaced them: aterm fleet init on one machine, aterm fleet join on each \
               other, then aterm ctl survey --fleet",
        replaced_by: "the fleet, `aterm fleet init` on one machine and `aterm fleet join` on \
                      each other,",
        left: Some(net_left),
    },
    RemovedFeature {
        table: "operator",
        what: "Embedded operator removed",
        instead: "a manager session replaced the embedded operator",
        said: "[operator] starts nothing now. A manager session replaced the embedded \
               operator: it reads every session with aterm ctl survey and starts workers \
               with aterm ctl @self spawn agent=claude",
        replaced_by: "a manager session, which `aterm help workers` describes,",
        left: None,
    },
];

/// The longest path [`net_left`] names, in characters.
const LEFT_PATH_CHARS: usize = 120;

/// What the network drive left (`[net]`'s [`RemovedFeature::left`]): the
/// persistent drive token it kept in the folder of `[net] key`
/// (`aterm-network-drive.token`), each saved connection's dial token — a
/// Keychain item on macOS (service `aterm-net-drive`, account the
/// connection's name), else `net/<name>.token` beside aterm.toml, which
/// `dial-token` wrote whatever the connection said, and on either any
/// `token_file` it names — and the TLS key and certificate. Nothing reads
/// them now, but an aterm too old to have lost the drive, still serving
/// `[net] listen`, takes its own drive token as the Owner, so the lines say
/// to delete them, each folder named once ([`named_by_folder`]): the first
/// the drive token and every `token_file`, the second the Keychain's items
/// on macOS (the key and certificate join the first there) and elsewhere the
/// conventional files, then the key and certificate. A line is still clipped
/// at its cap. A table that names none of them leaves no line.
fn net_left(net: &aterm_toml::Value) -> Vec<String> {
    let text = |v: Option<&aterm_toml::Value>| {
        v.and_then(aterm_toml::Value::as_str)
            .map(|s| atpkg::progress::sanitize_for_tty(s, LEFT_PATH_CHARS))
    };
    let key = text(net.get("key"));
    let mut tokens: Vec<String> = key
        .iter()
        .map(|key| {
            std::path::Path::new(key)
                .with_file_name("aterm-network-drive.token")
                .display()
                .to_string()
        })
        .collect();
    let mut names = Vec::new();
    for conn in net
        .get("connections")
        .and_then(aterm_toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        tokens.extend(text(conn.get("token_file")));
        names.extend(text(conn.get("name")));
    }
    let tls: Vec<String> = key.into_iter().chain(text(net.get("cert"))).collect();
    let grouped = |files: &[String]| (!files.is_empty()).then(|| named_by_folder(files));
    let (first, second): (Vec<String>, Vec<String>) = if cfg!(target_os = "macos") {
        tokens.extend(tls);
        let keychain = (!names.is_empty()).then(|| {
            format!(
                "its Keychain items: security delete-generic-password -s aterm-net-drive -a \
                 <name>, for {}",
                names.join(", ")
            )
        });
        (
            grouped(&tokens).into_iter().collect(),
            keychain.into_iter().collect(),
        )
    } else {
        let conventional = (!names.is_empty()).then(|| {
            let files: Vec<String> = names.iter().map(|n| format!("{n}.token")).collect();
            format!("in net beside aterm.toml: {}", files.join(", "))
        });
        (
            grouped(&tokens).into_iter().collect(),
            conventional.into_iter().chain(grouped(&tls)).collect(),
        )
    };
    let (first, second) = if first.is_empty() {
        (second, Vec::new())
    } else {
        (first, second)
    };
    let mut lines = Vec::new();
    if !first.is_empty() {
        lines.push(format!(
            "Delete what it left, which an older aterm serving [net] still accepts: {}",
            first.join("; ")
        ));
    }
    if !second.is_empty() {
        lines.push(format!("and {}", second.join("; ")));
    }
    lines
}

/// `files` in order, each folder and each file named once: a folder holding
/// one of them is its whole path, one holding more is `in <folder>: <name>,
/// <name>`, groups joined by `; ` in the order their first file came.
fn named_by_folder(files: &[String]) -> String {
    let mut folders: Vec<(String, Vec<String>)> = Vec::new();
    for file in files {
        let path = std::path::Path::new(file);
        let (folder, name) = match (path.parent(), path.file_name()) {
            (Some(folder), Some(name)) if !folder.as_os_str().is_empty() => (
                folder.display().to_string(),
                name.to_string_lossy().into_owned(),
            ),
            _ => (String::new(), file.clone()),
        };
        match folders.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, names)) if names.contains(&name) => {}
            Some((_, names)) => names.push(name),
            None => folders.push((folder, vec![name])),
        }
    }
    folders
        .iter()
        .map(|(folder, names)| match names.as_slice() {
            _ if folder.is_empty() => names.join(", "),
            [name] => std::path::Path::new(folder)
                .join(name)
                .display()
                .to_string(),
            _ => format!("in {folder}: {}", names.join(", ")),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The digest key of `feature`'s entry, for its end.
pub(crate) fn removed_setting_key(feature: &RemovedFeature) -> String {
    digest_key(Family::Removed, feature.table, "replaced")
}

/// The one step of `feature`'s entry, the one that ends it: the table
/// deleted from `aterm.toml`.
pub(crate) fn removed_setting_step(feature: &RemovedFeature) -> String {
    format!("Delete the [{}] table from aterm.toml", feature.table)
}

/// The entry for `feature`, whose settings an `aterm.toml` still holds from
/// `line` on (1-based; `None`: the editor opens at the top), naming what the
/// feature `left` ([`RemovedFeature::left`]) behind Details. Its one step is
/// the deletion that ends it ([`removed_setting_step`]), and its navigation
/// opens the file there, where that step is done: aterm does not delete the
/// person's table for them.
pub(crate) fn removed_setting_entry(
    feature: &RemovedFeature,
    line: Option<u32>,
    left: &[String],
) -> Entry {
    let entry = Entry::new(
        Family::Removed,
        feature.table,
        "replaced",
        feature.what,
        Why::Configured {
            key: Reason::new(format!("[{}]", feature.table)),
            reason: Reason::new(feature.instead),
        },
        Then::step(removed_setting_step(feature)),
    )
    .resurface(Resurface::CHOICE)
    .show(Intent::OpenConfigEditor { line })
    .more(feature.said)
    .log_line(format!("removed-setting table={}", feature.table));
    left.iter().fold(entry, |entry, line| entry.more(line))
}

/// The cause word of a stuck survey in the digest's key.
fn survey_cause(kind: aterm_agent::supervise::StuckSurvey) -> &'static str {
    match kind {
        aterm_agent::supervise::StuckSurvey::Withheld => "survey-card",
        aterm_agent::supervise::StuckSurvey::Unclosed => "survey-open",
    }
}

/// The record of a rating survey the loop cannot close (`text`: the loop's own
/// sentence). Warn, never a tab mark: the card blocks nothing the worker does.
pub(crate) fn survey_record(
    kind: aterm_agent::supervise::StuckSurvey,
    sid: &str,
    place: &str,
    text: &str,
) -> Message {
    let head = match kind {
        aterm_agent::supervise::StuckSurvey::Withheld => "Rating card open",
        aterm_agent::supervise::StuckSurvey::Unclosed => "Survey will not close",
    };
    Message::new(tags::HARNESS, Severity::Warn, fitted_title(head, place))
        .line(atpkg::progress::sanitize_for_tty(text, 160))
        .record()
        .key(&host_record_key(survey_cause(kind), sid))
}

/// A rating card the harness will not press, that has stood in a tab for an
/// hour: the owner presses `0` in it. `harness.supervisor.<tab>.survey-card`.
/// Only this shape is ever an entry: a survey the loop is still trying to close
/// ([`aterm_agent::supervise::StuckSurvey::Unclosed`]) is a record and nothing
/// more, because a persisting "still trying" is the alert the exceptions rule
/// forbids: the loop's own ladder either closes it or the card becomes this one.
pub(crate) fn survey_entry(sid: &str, spot: &UpgradeSpot, text: &str) -> Entry {
    let kind = aterm_agent::supervise::StuckSurvey::Withheld;
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        survey_cause(kind),
        &fitted_title("Rating card open", &spot.place),
        Why::OnlyYou(Reason::new(
            "aterm does not answer this kind of rating card itself",
        )),
        Then::step("Press 0 in that tab to dismiss it"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .more(atpkg::progress::sanitize_for_tty(text, 160))
    .log_line(format!(
        "survey {} sid={sid} for over an hour",
        survey_cause(kind)
    ));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// The digest key of [`survey_entry`], for its end.
pub(crate) fn survey_key(sid: &str) -> String {
    digest_key(
        Family::Supervisor,
        sid,
        survey_cause(aterm_agent::supervise::StuckSurvey::Withheld),
    )
}

/// The key of the disk line.
pub(crate) const DISK_LOW_KEY: &str = "disk.low";

/// The words before a hog on the entry's lines ([`disk_low_entry`]); the
/// record's are fewer.
const LARGEST: &str = "Largest it cannot reclaim: ";
/// The words after the entry's second hog, after the count of the rest.
const AND_MORE: &str = " more, and aterm harness disk lists what is left now";

/// The most characters of a hog line a `disk.low` detail line carries
/// whole: the detail cap ([`aterm_messages::DETAIL_LINE_CAP`], which clips a
/// line at its end) less the most words around one — the entry's second,
/// with the count of the rest (one digit). A hog fitted to it
/// (`Hog::line_within`) keeps what holds the space and why.
pub(crate) const DISK_LOW_HOG_CHARS: usize =
    aterm_messages::DETAIL_LINE_CAP - LARGEST.len() - "; 9".len() - AND_MORE.len();

// The count of the rest is one digit.
const _: () = assert!(aterm_agent::harness::disk::MAX_HOGS - 2 < 10);

/// The record that the home volume stays short of the automatic floor after
/// two passes of aterm reclaiming what it may, with the largest build directories it cannot
/// reclaim (`hogs`, each a line naming its size and what frees it). A volume
/// short while no pass runs, because a `[disk]` value aterm cannot read turned
/// every removal off, is [`disk_low_cleanup_off_record`]'s.
pub(crate) fn disk_low_record(free_gib: u64, floor_gib: u64, hogs: &[String]) -> Message {
    Message::new(tags::SYSTEM, Severity::Warn, "Disk space is low")
        .line(format!(
            "{free_gib} GiB free on the home volume, below the {floor_gib} GiB floor after \
             two passes of aterm reclaiming what it may"
        ))
        .lines(hogs.iter().map(|h| format!("aterm cannot reclaim: {h}")))
        .record()
        .key("system.disk.low")
}

/// The volume stayed below the floor through two passes, whatever they
/// freed: what is left to free is the owner's (`disk.low`) — and the largest build
/// directories aterm cannot reclaim say where it is (`hogs`, biggest first:
/// the first two are shown, and the second counts the rest this pass found and
/// points at `aterm harness disk`, which surveys afresh and may list others —
/// not on a line of its own, since an entry keeps only three detail lines).
/// None found adds nothing: the passes already took what aterm may reclaim.
/// Only passes that ran open this line; a volume short while a `[disk]` value
/// aterm cannot read keeps every pass from running is
/// [`disk_low_cleanup_off_entry`]'s.
pub(crate) fn disk_low_entry(free_gib: u64, floor_gib: u64, hogs: &[String]) -> Entry {
    let mut entry = Entry::new(
        Family::Disk,
        "",
        "low",
        "Disk space is low",
        Why::OnlyYou(Reason::new(
            "aterm reclaimed what it may in two passes and the home volume is still short",
        )),
        Then::step("Free up space on the home volume"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(format!(
        "{free_gib} GiB free; aterm reclaims below {floor_gib} GiB"
    ));
    let rest = hogs.len().saturating_sub(2);
    for (i, hog) in hogs.iter().take(2).enumerate() {
        entry = entry.more(if i == 1 && rest > 0 {
            format!("{LARGEST}{hog}; {rest}{AND_MORE}")
        } else {
            format!("{LARGEST}{hog}")
        });
    }
    entry.log_line(format!(
        "disk-low free_gib={free_gib} floor_gib={floor_gib} hogs={}",
        hogs.len()
    ))
}

/// The key of the session journal's paused line ([`session_journal_paused_entry`]).
pub(crate) const SESSION_JOURNAL_PAUSED_KEY: &str = "disk.session-journal-paused";

/// The session journal (`[session_log] journal`, phase 2B) is paused because
/// `why` (the disk is full, or failed a write) and retries on its own: a fact
/// for later, cleared at its first good write. No shell is touched; what
/// sessions do meanwhile reads later as a counted gap.
pub(crate) fn session_journal_paused_entry(why: &str) -> Entry {
    Entry::new(
        Family::Disk,
        "",
        "session-journal-paused",
        "The session journal is paused",
        Why::OnlyYou(Reason::new(
            "only you can free space on the volume; until then what sessions do is counted, \
             not written",
        )),
        Then::step("Free up space on the home volume"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(format!("It paused because {why}"))
    .log_line(format!("session-journal paused: {why}"))
}

/// The session journal stopped for this run because `why` (its folder or a
/// file refused, a filesystem without file locks, the disk failing, its
/// writer's fault): sessions run on, and the next launch tries again.
pub(crate) fn session_journal_stopped_entry(why: &str) -> Entry {
    Entry::new(
        Family::Disk,
        "",
        "session-journal-stopped",
        "The session journal stopped",
        Why::OnlyYou(Reason::new(
            "sessions run on; log and brief answer for this run's sessions from memory",
        )),
        Then::step("Check the session-log folder in the logs folder"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(format!(
        "It stopped because {why}; the next launch starts it again"
    ))
    .log_line(format!("session-journal stopped: {why}"))
}

/// The key of the low-disk line a volume short while cleanup is off by fault
/// opens ([`disk_low_cleanup_off_entry`]). It is not [`DISK_LOW_KEY`]: this
/// line also ends when the settings read again, and the window can tell an
/// adopted line's form only by its key; and the owner's dismissal of one form
/// does not hold the other, whose action differs.
pub(crate) const DISK_LOW_CLEANUP_OFF_KEY: &str = "disk.low-cleanup-off";

/// The record that the home volume is below the automatic floor (`floor_gib`:
/// aterm.toml's `auto_free_gib`, or its default where the file writes none)
/// while a `[disk]` value aterm cannot read turns every removal off: no pass
/// ran, and aterm frees nothing until the settings read again.
pub(crate) fn disk_low_cleanup_off_record(free_gib: u64, floor_gib: u64) -> Message {
    Message::new(tags::SYSTEM, Severity::Warn, "Disk space is low")
        .line(format!(
            "{free_gib} GiB free on the home volume, below the {floor_gib} GiB cleanup floor; no \
             cleanup ran, and aterm frees nothing until it can read the [disk] settings"
        ))
        .record()
        .key("system.disk.low")
}

/// The volume stayed below the automatic floor (`floor_gib`: aterm.toml's
/// `auto_free_gib`, or its default where the file writes none) for two looks
/// while a `[disk]` value aterm cannot read turned every removal off
/// (`disk.low-cleanup-off`): no pass ran and none will until the owner fixes
/// the value, so the action is the settings' (`Fix aterm.toml`, as on
/// [`disk_settings_entry`]'s line), and the detail gives the figures. It names
/// no hogs: no search runs while removal is off.
pub(crate) fn disk_low_cleanup_off_entry(free_gib: u64, floor_gib: u64) -> Entry {
    Entry::new(
        Family::Disk,
        "",
        "low-cleanup-off",
        "Disk space is low",
        Why::YourAction(Reason::new(
            "no cleanup ran: aterm frees nothing until it can read the [disk] settings in \
             aterm.toml",
        )),
        Then::press(Mover::open_config_editor(None), "Fix aterm.toml"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(format!(
        "{free_gib} GiB free, below the {floor_gib} GiB cleanup floor"
    ))
    .log_line(format!(
        "disk-low-cleanup-off free_gib={free_gib} floor_gib={floor_gib}"
    ))
}

/// A FACT FOR LATER: a pass of the disk watch freed `bytes` below the floor
/// (`went`: the largest of what went, biggest first; the first two shown).
/// Nothing is asked now — the build caches it removed are rebuilt by the next
/// build — but what was removed is the owner's to know, and `aterm harness
/// disk` and the ledger say the rest.
pub(crate) fn disk_freed_entry(bytes: u64, went: &[String]) -> Entry {
    let freed = aterm_agent::harness::disk::human_bytes(bytes);
    let mut entry = Entry::new(
        Family::Disk,
        "",
        "freed",
        "aterm freed disk space",
        Why::Spent {
            tried: Reason::new(format!("freed {freed} of rebuildable build caches")),
            reason: Reason::new("the home volume ran short; the next build rebuilds them"),
        },
        Then::step("Read aterm harness disk for what went and what is left"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT);
    for w in went.iter().take(2) {
        entry = entry.more(format!("Went: {w}"));
    }
    entry.log_line(format!("disk-freed bytes={bytes} rows={}", went.len()))
}

/// The key of the disk-full alert ([`disk_full`]).
pub(crate) const DISK_FULL_KEY: &str = "system.disk.full";

/// The disk-full alert's leading lines that are its sentence
/// ([`sentence_lines`]): the step, the cause, what to expect.
const DISK_FULL_SENTENCE: usize = 3;

/// What holds the disk-full alert ([`disk_full`]) up: what aterm did about
/// the full home volume, or why it did nothing. Each case says only what is
/// true of it, so the alert states its fact once and never says aterm "can
/// free nothing more" where it was kept from trying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiskFullCause {
    /// A pass took every build cache aterm may remove.
    Spent,
    /// A pass was kept from removing a build cache: its write-ahead record
    /// could not be written on the full volume, and aterm removes nothing
    /// without one (`disk::Refusal::Unjournalled`), so it removed no more.
    Unrecorded,
    /// No session is supervised, so a pass reclaims only aterm's own build
    /// cache, and only while `[disk] dead_build_dirs` lets a whole build
    /// directory go (`whole`).
    Unsupervised { whole: bool },
    /// Every removal is off: `[disk] auto_free_gib` is 0, as the owner wrote
    /// it or (`fault`) because a `[disk]` value aterm cannot read turned it
    /// off.
    CleanupOff { fault: bool },
    /// The volume is out of inodes, not bytes: no build cache's size is the
    /// cause.
    Inodes,
    /// This process is not the owner's disk watch, by the rule named (a
    /// private state root, a scratch home, ...): it measures and removes
    /// nothing.
    Private(DiskPrivate),
}

impl DiskFullCause {
    /// The log's words for the cause, settings named as aterm.toml spells
    /// them (the log is read beside the file; the glass is not).
    pub(crate) fn log_words(self) -> &'static str {
        match self {
            Self::Spent => {
                "aterm reclaimed every build cache it may and nothing more is safe to remove"
            }
            Self::Unrecorded => {
                "the disk ledger took no write-ahead row, so the pass removed no build cache \
                 without one"
            }
            Self::Unsupervised { whole: true } => {
                "no supervised session: only aterm's own build cache is reclaimed"
            }
            Self::Unsupervised { whole: false } => {
                "no supervised session, and [disk] dead_build_dirs is off: no whole build cache \
                 is reclaimed"
            }
            Self::CleanupOff { fault: false } => {
                "the automatic removal is off ([disk] auto_free_gib is 0)"
            }
            Self::CleanupOff { fault: true } => {
                "the automatic removal is off (a [disk] value aterm cannot read)"
            }
            Self::Inodes => {
                "the volume is out of inodes, not bytes: no build cache aterm may remove is the \
                 cause"
            }
            Self::Private(rule) => rule.why(),
        }
    }

    /// The alert's reason and its one step: `free` the bytes left (`None`:
    /// not measured), `freed` what aterm freed this episode, `hogs` whether
    /// the alert lists the largest folders it could not reclaim. The step is
    /// the band's excerpt beside `Disk full`, short enough to stand whole in a
    /// narrow window; the bytes left lead every reason but the
    /// inodes', the first line of Details. Every reason is the owner's to act
    /// on ([`StandingWhy::OnlyYou`]: aterm deletes none of their files), which
    /// lets the figure lead where `Spent`'s words open on what aterm did.
    fn ask(self, free: Option<u64>, freed: u64, hogs: bool) -> (StandingWhy, Then) {
        let human = aterm_agent::harness::disk::human_bytes;
        let left = free.map_or_else(
            || "little space left".to_owned(),
            |f| format!("{} left", human(f)),
        );
        let reason = match self {
            Self::Spent if freed > 0 => format!(
                "{left}; aterm already deleted every unused build cache ({})",
                human(freed)
            ),
            Self::Spent => format!("{left}; aterm found no unused build cache to delete"),
            Self::Unrecorded if freed > 0 => format!(
                "{left}; aterm removed {}, then could not log a removal on the full disk",
                human(freed)
            ),
            Self::Unrecorded => {
                format!(
                    "{left}; aterm removed nothing: it could not log a removal on the full disk"
                )
            }
            Self::Unsupervised { whole: true } => format!(
                "{left}; aterm removes only its own build cache while no session is supervised"
            ),
            Self::Unsupervised { whole: false } => {
                format!("{left}; aterm.toml keeps aterm from removing build caches")
            }
            Self::CleanupOff { fault: false } => {
                format!("{left}; automatic cleanup is off in aterm.toml")
            }
            Self::CleanupOff { fault: true } => {
                format!("{left}; a disk setting aterm cannot read turned cleanup off")
            }
            Self::Inodes => {
                "the home volume has room for no more files: it is out of inodes, not bytes"
                    .to_owned()
            }
            // The owner's own window can be the one that removes nothing
            // (its home's disk unnamed, its account home unread): it is
            // told so, never called a private aterm.
            Self::Private(rule) => match rule {
                DiskPrivate::StateRoot | DiskPrivate::ScratchHome => {
                    format!("{left}; a private aterm like this one removes nothing")
                }
                DiskPrivate::DeviceUnnamed => {
                    format!("{left}; aterm cannot identify this home's disk, so it removes nothing")
                }
                DiskPrivate::NoAccountHome => {
                    format!("{left}; aterm cannot read your account's home, so it removes nothing")
                }
            },
        };
        let why = StandingWhy::OnlyYou(Reason::new(reason));
        let then = if self == Self::Inodes {
            "Delete folders full of small files you no longer need"
        } else if hogs {
            "Free up space, starting with the largest folder in Details"
        } else {
            "Free up space: aterm harness disk lists what fills it"
        };
        (why, Then::step(then))
    }

    /// What ends the alert, as a figure the owner can free up
    /// to, and what aterm does then. The watch ends it once the volume is
    /// out of its emergency (`thresholds`: [`Thresholds::emergency`] free
    /// bytes, or [`Thresholds::files_emergency`] free files where the volume
    /// is out of inodes), and resumes the tabs whose agents the full disk
    /// stopped (`harness_host::disk_owed`).
    ///
    /// [`Thresholds::emergency`]: aterm_agent::harness::disk::Thresholds::emergency
    /// [`Thresholds::files_emergency`]: aterm_agent::harness::disk::Thresholds::files_emergency
    fn expect(self, thresholds: &aterm_agent::harness::disk::Thresholds) -> String {
        let free = if self == Self::Inodes {
            format!(
                "there is room for {} more files",
                grouped(thresholds.files_emergency)
            )
        } else {
            format!(
                "{} is free",
                aterm_agent::harness::disk::human_bytes(thresholds.emergency)
            )
        };
        format!("Once {free}, this alert ends and aterm resumes the tabs the full disk stopped.")
    }
}

/// `n` with its thousands grouped (`50,000`), as a person reads a count.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The home volume is full and only the owner can free it — the one disk
/// alert on the glass: writes are failing or about to (agents' tool calls,
/// their transcripts), and aterm already took what it may or was kept from
/// taking anything (`cause`, which `freed` this episode and `free` the bytes
/// left figure in). One action changes the outcome, so it stands
/// ([`Ask::standing`]) until the volume is out of its emergency. The step
/// leads, as a tab's notice's does ([`resume_ask`]): it is the band's
/// excerpt beside `Disk full`, which the band's width law keeps whole before
/// the rest of the row; the cause follows, stated once, then the largest
/// folders aterm could not reclaim (`hogs`, each fitted to its line), each
/// with what frees it. Between them, what
/// to expect: the free space that ends the alert (`thresholds`), and that
/// aterm then resumes the tabs it stopped ([`DiskFullCause::expect`]).
pub(crate) fn disk_full(
    free: Option<u64>,
    freed: u64,
    hogs: &[String],
    cause: DiskFullCause,
    thresholds: &aterm_agent::harness::disk::Thresholds,
) -> Message {
    let (why, then) = cause.ask(free, freed, !hogs.is_empty());
    let cause_words = Why::from(why.clone()).words();
    Message::new(tags::SYSTEM, Severity::Error, "Disk full")
        .line(then.words())
        .line(cause_words)
        .line(cause.expect(thresholds))
        .now(Ask::standing(why, then))
        .lines(hogs.iter().take(3).map(|h| format!("Largest: {h}")))
        .key(DISK_FULL_KEY)
}

/// The key of the program-check alert ([`program_check_stuck`]).
pub(crate) const PROGRAM_CHECK_STUCK_KEY: &str = "system.program-check.stuck";

/// The program-check alert's leading lines that are its sentence
/// ([`sentence_lines`]): the step, the cause, what it does.
const PROGRAM_CHECK_SENTENCE: usize = 3;

/// MACOS'S PROGRAM CHECK IS STUCK and only a restart ends it
/// ([`crate::program_check_watch`]): `syspolicyd`, which decides whether each
/// new program may start, has stood at the kernel's thread limit (`workers`
/// threads, against the constrained `limit`) for five minutes, because it and
/// `tccd` each wait on the other. No new program starts on the Mac, and the
/// window server can die at a privacy check, closing every window
/// (2026-10-20, 4.5 days after it stuck). Logging out did not clear it; a
/// restart did. One action changes the outcome, so it stands
/// ([`Ask::standing`]) until the check is no longer stuck. The step leads, as
/// [`disk_full`]'s does, and its cut keeps the restart; the pool's figures
/// and where the stuck check came from are its technical lines.
pub(crate) fn program_check_stuck(workers: u64, limit: Option<u64>) -> Message {
    let why = StandingWhy::OnlyYou(Reason::new(
        "macOS's program check is deadlocked, and only a restart of the Mac ends it",
    ));
    let then = Then::step("Restart the Mac once your work is saved").or_short("Restart the Mac");
    let cause_words = Why::from(why.clone()).words();
    let limit = limit.map_or_else(|| "its limit".to_owned(), |l| format!("a limit of {l}"));
    Message::new(
        tags::SYSTEM,
        Severity::Error,
        "macOS's program check is stuck",
    )
    .line(then.words())
    .line(cause_words)
    .line(
        "No new program can start on this Mac, and a privacy check can freeze the screen \
             and close every window. Logging out does not clear it.",
    )
    .now(Ask::standing(why, then))
    .line(format!(
        "syspolicyd has stood at the kernel's thread limit for five minutes ({workers} pool \
             threads against {limit}), and it and tccd each hold what the other needs."
    ))
    .line(
        "Builds and tests in a tab kept across an aterm update make macOS check aterm's \
             earlier copy at every program they start. Start them from a new tab.",
    )
    .key(PROGRAM_CHECK_STUCK_KEY)
}

/// The key of the line that a `[disk]` setting aterm cannot read turned
/// every removal off ([`disk_settings_entry`]).
pub(crate) const DISK_SETTINGS_KEY: &str = "disk.settings";

/// The words after the count of the notes [`disk_settings_entry`] does not
/// show.
const NOTES_MORE: &str = " more, and aterm harness disk prints each";

/// aterm.toml does not say what the owner meant by a `[disk]` removal knob
/// (a value of another type, a `disk` that is not a table, a knob below
/// another table's header, a file that cannot be read or parsed), so every
/// removal is off until it is fixed (`disk.settings`): the disk watch frees
/// nothing however short the volume, and only the owner can say what the
/// value meant. `notes` are the reader's own, which quote no value: those of
/// the values that turned removal off, or a file's one note where none of its
/// values can be read (the host picks them, `harness_host::disk_settings_note`).
/// Up to [`aterm_messages::exceptions::MORE_LINES`] are shown, and where there
/// are more the last line counts the rest and points at `aterm harness disk`,
/// which prints every one.
pub(crate) fn disk_settings_entry(notes: &[String]) -> Entry {
    use aterm_messages::exceptions::MORE_LINES;
    let mut entry = Entry::new(
        Family::Disk,
        "",
        "settings",
        "Disk cleanup is off",
        Why::YourAction(Reason::new(
            "aterm cannot read the [disk] settings in aterm.toml, so it frees no disk space",
        )),
        Then::press(Mover::open_config_editor(None), "Fix aterm.toml"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT);
    let shown = if notes.len() > MORE_LINES {
        MORE_LINES - 1
    } else {
        notes.len()
    };
    for note in &notes[..shown] {
        entry = entry.more(note);
    }
    if shown < notes.len() {
        entry = entry.more(format!("{}{NOTES_MORE}", notes.len() - shown));
    }
    entry.log_line(format!("disk-settings-off notes={}", notes.len()))
}

/// The key of the line saying aterm is not on the Developer Tools list
/// ([`devtools_entry`]).
pub(crate) const DEVTOOLS_KEY: &str = "privacy.devtools.not-granted";

/// macOS blocked account automation on a permission the owner controls.
/// The worker keeps retrying after the permission is granted.
pub(crate) fn workstation_accounts_entry() -> Entry {
    Entry::new(
        Family::Privacy,
        "workstation",
        "account-signout",
        "macOS blocked account automation",
        Why::OnlyYou(Reason::new("macOS requires your permission to automate account settings")),
        Then::step("Allow account automation in System Settings"),
    )
    .resurface(Resurface::CHOICE)
    .more("System Settings > Privacy & Security > Accessibility: allow aterm")
    .more("System Settings > Privacy & Security > Automation: allow aterm to control Messages and FaceTime")
    .more("Account automation retries after permission is granted; aterm signs out the accounts")
}

/// First-run setup has not verified every setting. The worker keeps retrying;
/// the owner can inspect its current result without restarting setup.
pub(crate) fn workstation_setup_failure_entry() -> Entry {
    Entry::new(
        Family::Privacy,
        "workstation",
        "settings-incomplete",
        "Mac setup needs attention",
        Why::OnlyYou(Reason::new(
            "background setup has not verified every workstation setting",
        )),
        Then::step("Inspect aterm doctor --check"),
    )
    .resurface(Resurface::CHOICE)
    .more("Automatic setup retries while aterm remains open")
    .more("aterm doctor --check optionally inspects settings; no rerun is needed to resume setup")
}

/// aterm is not on macOS's Developer Tools list, MEASURED
/// (`aterm_containment::consent::devtool_grant` over the switch's row and
/// tccd's answers): every new program aterm starts
/// waits for macOS's first-run check, so builds and tests wait. The switch is
/// the owner's — macOS asks for their password, and aterm never presses it.
/// A digest line, never a band row, in `atpkg::devtools`' words (one
/// spelling with the helper panel, Settings ▸ Security and `aterm doctor`):
///
/// * its title and its step follow what the list holds of aterm
///   (`atpkg::devtools::Step`, as the panel, the card and doctor do): one
///   action ([`atpkg::devtools::Step::action`]) — the drag for an aterm the
///   list does not hold, with the route through `+` riding the details beside
///   the path; aterm's switch for a row switched off; and, for a row on while
///   macOS still checks, a look at the row with the causes in the details — a
///   step with ` or ` fails `Step::fault`, and an entry that fails it is
///   recorded, not raised;
/// * its press opens the Developer Tools pane, and the message center files
///   the line `Answered` for it (`MessageCenter::act_digest`: the owner did the
///   one action), quiet for 30 days, then 90 ([`Resurface::CHOICE`]), as a
///   dismissal is; a reading that finds aterm on the list ends the line, or
///   forgets its filing (`App::except_clear`), so aterm leaving the list later
///   is news again. On a list read empty the page waits for `spctl
///   developer-mode enable-terminal`, so the line is filed before the page is
///   known to open: when it then does not, the line comes back
///   (`App::settle_devtools_waiting`);
/// * its second capsule, `Security`, opens aterm's own Settings ▸ Security,
///   where the list's state stays readable after the line is put away. A
///   navigation: pressing it leaves the line.
pub(crate) fn devtools_entry(step: atpkg::devtools::Step) -> Entry {
    use atpkg::devtools::Step;
    let skips = aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check;
    let (title, why) = match step {
        Step::Add => (
            "aterm is not in Developer Tools",
            "only you can add aterm to the Developer Tools list in System Settings",
        ),
        Step::SwitchOn => (
            "aterm is off in Developer Tools",
            "only you can switch on aterm's row in Developer Tools in System Settings",
        ),
        Step::ListedOn => (
            "macOS still checks programs aterm starts",
            "only you can change aterm's row in the Developer Tools list in System Settings",
        ),
    };
    let path = crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::DeveloperTools);
    let entry = Entry::new(
        Family::Privacy,
        "devtools",
        "not-granted",
        title,
        Why::OnlyYou(Reason::new(why)),
        Then::press(Mover::open_system_pane(PANE_DEVELOPER_TOOLS), step.action()),
    )
    .show(Intent::OpenSettings {
        route: SettingsRoute::Security.path().to_string(),
    })
    .severity(Severity::Warn)
    .resurface(Resurface::CHOICE)
    .more(atpkg::devtools::why(skips));
    let entry = match step {
        Step::Add => entry.more(format!("{path} ({})", atpkg::devtools::PLUS_ROUTE)),
        Step::SwitchOn => entry.more(path),
        Step::ListedOn => entry.more(atpkg::devtools::HELPER_LISTED_ON).more(path),
    };
    entry
        .more(
            "Once it is on, aterm has macOS check it again: when aterm reads the switch on, or \
             when you come back to aterm from the page. Programs started before that may still wait.",
        )
        .log_line("devtools not-granted")
}

/// The live upgrade cannot run (its state folder cannot hold the lock): the
/// host's fact, an EVENT of [`Family::UpgradeHost`]
/// (`harness.upgrade-host.cannot-run`), in the classifier's own words; the
/// folder to mend (`dir`) is under Details, the step only in the log line.
pub(crate) fn upgrade_cannot_run_entry(step: &str, dir: Option<&std::path::Path>) -> Entry {
    let need = aterm_agent::harness::upgrade_drive::Need::upgrade_cannot_run(step);
    let entry = Entry::new(
        Family::UpgradeHost,
        "",
        "cannot-run",
        need.title,
        Why::OnlyYou(Reason::new(&need.why)),
        Then::step(&need.then),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(need.log_line);
    match dir {
        Some(dir) => entry.more(atpkg::progress::sanitize_for_tty(
            &dir.display().to_string(),
            aterm_messages::DETAIL_LINE_CAP,
        )),
        None => entry,
    }
}

/// The key of [`upgrade_cannot_run_entry`].
pub(crate) const UPGRADE_CANNOT_RUN_KEY: &str = "harness.upgrade-host.cannot-run";

/// The folder the upgrades' records are kept in cannot be listed whole, so
/// the upgrades the window shows stay at the last look that listed it: the
/// host's fact, an EVENT of [`Family::UpgradeHost`]
/// (`harness.upgrade-host.records-unlisted`), in the classifier's words; the
/// folder to mend (`dir`) is under Details.
pub(crate) fn upgrade_records_unlisted_entry(dir: &std::path::Path) -> Entry {
    let need = aterm_agent::harness::upgrade_drive::Need::upgrade_records_unlisted();
    Entry::new(
        Family::UpgradeHost,
        "",
        "records-unlisted",
        need.title,
        Why::OnlyYou(Reason::new(&need.why)),
        Then::step(&need.then),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(need.log_line)
    .more(atpkg::progress::sanitize_for_tty(
        &dir.display().to_string(),
        aterm_messages::DETAIL_LINE_CAP,
    ))
}

/// The key of [`upgrade_records_unlisted_entry`].
pub(crate) const UPGRADE_RECORDS_UNLISTED_KEY: &str = "harness.upgrade-host.records-unlisted";

/// Upgrade records the owner's view lists but cannot read keep their tabs as
/// they stand, so those tabs' upgrade rows and marks are not renewed: the
/// host's fact, an EVENT of [`Family::UpgradeHost`]
/// (`harness.upgrade-host.records-unreadable`), in the classifier's words;
/// the files to mend (`files`) are under Details, as many as it holds.
pub(crate) fn upgrade_records_unreadable_entry(files: &[std::path::PathBuf]) -> Entry {
    let need = aterm_agent::harness::upgrade_drive::Need::upgrade_records_unreadable();
    files.iter().fold(
        Entry::new(
            Family::UpgradeHost,
            "",
            "records-unreadable",
            need.title,
            Why::OnlyYou(Reason::new(&need.why)),
            Then::step(&need.then),
        )
        .severity(Severity::Warn)
        .resurface(Resurface::WAIT)
        .log_line(need.log_line),
        |entry, file| {
            entry.more(atpkg::progress::sanitize_for_tty(
                &file.display().to_string(),
                aterm_messages::DETAIL_LINE_CAP,
            ))
        },
    )
}

/// The key of [`upgrade_records_unreadable_entry`].
pub(crate) const UPGRADE_RECORDS_UNREADABLE_KEY: &str = "harness.upgrade-host.records-unreadable";

/// The key of [`accounts_signed_out_entry`]: one line for every signed-out
/// account, so the fact is its identity and never the set of labels.
pub(crate) const ACCOUNTS_SIGNED_OUT_KEY: &str = "harness.accounts.signed-out";

/// The key of [`accounts_refused_entry`].
pub(crate) const ACCOUNTS_REFUSED_KEY: &str = "harness.accounts.refused";

/// What the first detail line of [`accounts_signed_out_entry`] opens with.
const ACCOUNTS_LABELS_LEAD: &str = "Signed out: ";

/// A detail line of [`accounts_signed_out_entry`] takes another label while it
/// stays within this many characters: under the engine's line cap by the room
/// the count of the labels left unnamed needs.
const ACCOUNTS_LABELS_FILL: usize = 200;

/// Claude Code accounts the roster lists that are signed out (`labels`, in
/// roster order: `aterm_agent::harness::accounts::signed_out`), or `None` for
/// none: ONE line of [`Family::Accounts`], however many there are.
///
/// A label is the owner's own word, so it stands only where it cannot fault
/// the entry: the reason's vocabulary is matched by substring (`soon-1` there
/// would fault the entry, and the owner would be shown nothing), so the title
/// counts the accounts and the reason names none. The action names the FIRST,
/// the one command to run next; a label the action's cap would cut is not
/// named in it (a cut label is another account's command), and the action
/// points at the detail, which names them ([`accounts_signed_out_lines`]).
pub(crate) fn accounts_signed_out_entry(labels: &[String]) -> Option<Entry> {
    let first = labels.first()?;
    let what = match labels.len() {
        1 => notice::ACCOUNT_SIGNED_OUT.title.to_string(),
        n => format!("{n} Claude accounts are signed out"),
    };
    let filled = notice::ACCOUNT_SIGNED_OUT.fill(Some(first), None);
    let mut entry = Entry::new(
        Family::Accounts,
        "",
        "signed-out",
        &what,
        Why::OnlyYou(Reason::new(&filled.happened)),
        notice_then(&filled.todo, filled.short),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    // Its words move only with the roster (an account signed out or back in),
    // never every look: each is shown as it moves, as its notice is.
    .restated_at_once();
    // The labels first, then what aterm did and what follows, as the
    // detail's room allows ([`notice_tail`]).
    for line in accounts_signed_out_lines(labels) {
        entry = entry.more(line);
    }
    entry = notice_tail(entry, filled);
    Some(entry.log_line(format!("accounts need=signed-out n={}", labels.len())))
}

/// The detail of [`accounts_signed_out_entry`]: `Signed out: a, b, …` on the
/// lines an entry keeps ([`labels_lines`]).
fn accounts_signed_out_lines(labels: &[String]) -> Vec<String> {
    labels_lines(ACCOUNTS_LABELS_LEAD, labels, NOTICE_LABEL_LINES)
}

/// The label lines of an accounts notice's entry: what aterm did and what
/// follows take the rest of the lines an entry keeps.
const NOTICE_LABEL_LINES: usize = aterm_messages::exceptions::MORE_LINES - 2;

/// An accounts notice's entry closed by what aterm did and what follows —
/// or, where its step points at Details for its command
/// ([`notice::COMMAND_UNDER_DETAILS`]), by that command, whole, and what
/// follows: the one thing to do outranks what aterm did in the lines an
/// entry keeps.
fn notice_tail(entry: Entry, filled: notice::Filled) -> Entry {
    match filled.command {
        Some(command) => entry.more(command).more(filled.expect),
        None => entry.more(filled.did).more(filled.expect),
    }
}

/// `lead` and the labels, `a, b, …`, on at most `room` of the lines an entry
/// keeps. A label joins the line while the line stays within
/// [`ACCOUNTS_LABELS_FILL`] characters and starts the next otherwise, whole;
/// the labels the last line has no room for are counted at its end.
fn labels_lines(lead: &str, labels: &[String], room: usize) -> Vec<String> {
    let mut lines = vec![lead.to_string()];
    let mut starts = true;
    let mut named = 0;
    for label in labels {
        let line = lines.last_mut().expect("a line");
        if starts {
            line.push_str(label);
        } else if line.chars().count() + 2 + label.chars().count() <= ACCOUNTS_LABELS_FILL {
            line.push_str(", ");
            line.push_str(label);
        } else if lines.len() < room.min(aterm_messages::exceptions::MORE_LINES) {
            lines.push(label.clone());
        } else {
            break;
        }
        starts = false;
        named += 1;
    }
    if named < labels.len() {
        let line = lines.last_mut().expect("a line");
        line.push_str(&format!(" and {} more", labels.len() - named));
    }
    lines
}

/// THE ACCOUNT THE ACTION OF A SIGNED-OUT LINE IS FOR, read back from that
/// line's `detail` (the reason, the action, then
/// [`accounts_signed_out_lines`]): the first label it names, whether the
/// action could name it or points at the detail. A label holds neither a
/// blank nor a comma, so the read is exact. `None`: not this entry's words.
pub(crate) fn accounts_signed_out_first(detail: &[String]) -> Option<&str> {
    detail
        .get(2)?
        .strip_prefix(ACCOUNTS_LABELS_LEAD)?
        .split([',', ' '])
        .next()
        .filter(|label| !label.is_empty())
}

/// The key of [`accounts_credits_entry`]: one line for every account usage
/// credits carry.
pub(crate) const ACCOUNTS_CREDITS_KEY: &str = "harness.accounts.credits";

/// The key of [`accounts_window_full_entry`].
pub(crate) const ACCOUNTS_WINDOW_FULL_KEY: &str = "harness.accounts.window-full";

/// Usage credits carry a Claude account's sessions past a limit (`labels`, in
/// roster order: `aterm_agent::harness::accounts::credits_noted`, an episode
/// that began on a day no other did), or `None` for none: one line of
/// [`Family::Accounts`] (once per account per day). Credits
/// are work going on, paid for: nothing waits on the owner, and the one thing
/// only they do is the cap. Under `[harness] usage_credits = false`, the
/// default, the harness types none of its own turns
/// while they carry a session (aterm-agent's credits hold), so what they pay
/// for is the turn in flight and a person's own turns. The labels are the
/// owner's own words, so they are in the detail, never the reason.
pub(crate) fn accounts_credits_entry(labels: &[String]) -> Option<Entry> {
    labels.first()?;
    Some(
        Entry::new(
            Family::Accounts,
            "",
            "credits",
            "Claude is running on usage credits",
            Why::OnlyYou(Reason::new(
                "Credits are billed to you, and only you set their monthly cap",
            )),
            Then::step("Review the usage credit cap in claude.ai settings"),
        )
        .severity(Severity::Info)
        .resurface(Resurface::WAIT)
        .more(format!("On credits: {}", labels.join(", ")))
        .log_line(format!("accounts need=credits n={}", labels.len())),
    )
}

/// A Claude account's week is nearly used and its credits are off (`full`, in
/// roster order, each with the vendor's word for why its credits are off:
/// `aterm_agent::harness::accounts::windows_full`), or `None` for none: one
/// line of [`Family::Accounts`]. A session that reaches
/// the limit then stops until the week resets, a day or more off; credits
/// would carry it, and only the owner turns them on — or raises their cap,
/// where the vendor said they ran out (`out_of_credits`). Under `[harness]
/// usage_credits = false` (`usage_credits`; the default) the harness keeps the
/// session on the subscription until that reset whatever the account allows,
/// so the step names the key with the credits.
pub(crate) fn accounts_window_full_entry(
    full: &[(String, String)],
    usage_credits: bool,
) -> Option<Entry> {
    full.first()?;
    let spent = full.iter().any(|(_, why)| why == "out_of_credits");
    let then = match (usage_credits, spent) {
        (true, true) => "Raise the usage credit cap in claude.ai settings",
        (true, false) => "Turn on usage credits in claude.ai settings",
        (false, true) => {
            "Set [harness] usage_credits = true and raise the usage credit cap in claude.ai"
        }
        (false, false) => {
            "Set [harness] usage_credits = true and turn on usage credits in claude.ai"
        }
    };
    let why = if usage_credits {
        "With credits off, a session that reaches the limit stops until the week resets"
    } else {
        "aterm keeps sessions on your plan: one at the limit stops until the week resets"
    };
    let labels: Vec<&str> = full.iter().map(|(label, _)| label.as_str()).collect();
    Some(
        Entry::new(
            Family::Accounts,
            "",
            "window-full",
            "Weekly Claude limit nearly used",
            Why::OnlyYou(Reason::new(why)),
            Then::step(then),
        )
        .severity(Severity::Info)
        .resurface(Resurface::WAIT)
        .more(format!("Nearly used: {}", labels.join(", ")))
        .log_line(format!(
            "accounts need=window-full n={} spent={spent}",
            full.len()
        )),
    )
}

/// The key of [`accounts_due_entry`]: one line for every due login.
pub(crate) const ACCOUNTS_DUE_KEY: &str = "harness.accounts.due";

/// The key of [`accounts_none_soon_entry`].
pub(crate) const ACCOUNTS_NONE_SOON_KEY: &str = "harness.accounts.none-soon";

/// The key of [`accounts_wrong_entry`].
pub(crate) const ACCOUNTS_WRONG_KEY: &str = "harness.accounts.wrong-account";

/// The one step of the due lines: the whole batch, every machine's.
const ACCOUNTS_RENEW_DUE: &str = "Run aterm harness accounts renew --due";

/// THE MACHINE'S LOGINS DUE FOR RENEWAL (`labels`, in roster order:
/// `aterm_agent::harness::accounts::renew_due` — forecast to end within five
/// days, or ended), or `None` for none: ONE Info line of
/// [`Family::Accounts`], named in its detail, whose one step renews them all
/// in one sitting (and every other machine's, under `[harness]
/// hosts`). A fact worth a look, never an ask: nothing has stopped yet
/// (a session stopped on a lost login is its tab's ask, [`resume_ask`]).
pub(crate) fn accounts_due_entry(labels: &[String]) -> Option<Entry> {
    labels.first()?;
    let what = match labels.len() {
        1 => "Claude login due for renewal".to_string(),
        n => format!("{n} Claude logins due for renewal"),
    };
    let mut entry = Entry::new(
        Family::Accounts,
        "",
        "due",
        &what,
        Why::OnlyYou(Reason::new(
            "A login ends on a date Anthropic sets, and only a sign-in in your browser renews it",
        )),
        Then::step(ACCOUNTS_RENEW_DUE),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT);
    for line in labels_lines("Due: ", labels, aterm_messages::exceptions::MORE_LINES) {
        entry = entry.more(line);
    }
    Some(entry.log_line(format!("accounts need=due n={}", labels.len())))
}

/// NO RENEWABLE LOGIN IS LEFT WORKING WITHIN A DAY
/// (`aterm_agent::harness::accounts::none_working_within`): every login this
/// machine's sessions can be relaunched onto is ended or forecast to end by
/// then. A Warn line, never an ask: nothing has stopped yet.
pub(crate) fn accounts_none_soon_entry() -> Entry {
    Entry::new(
        Family::Accounts,
        "",
        "none-soon",
        "No Claude login lasts past tomorrow",
        Why::OnlyYou(Reason::new(
            "Every Claude login on this machine ends within a day, and only you can renew one",
        )),
        Then::step(ACCOUNTS_RENEW_DUE),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line("accounts need=none-working-within-a-day")
}

/// Renewals that signed in another account (`labels`:
/// `aterm_agent::harness::accounts::wrong_accounts`), or `None`: nothing
/// live changed, and the row's login renews only in the account's own
/// browser profile. Named by label alone: the row's email is never shown.
pub(crate) fn accounts_wrong_entry(labels: &[String]) -> Option<Entry> {
    let first = labels.first()?;
    let filled = notice::RENEWAL_WRONG_ACCOUNT.fill(Some(first), None);
    let mut entry = Entry::new(
        Family::Accounts,
        "",
        "wrong-account",
        notice::RENEWAL_WRONG_ACCOUNT.title,
        Why::OnlyYou(Reason::new(&filled.happened)),
        notice_then(&filled.todo, filled.short),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    // Its words move only with the renewals it names: shown as they move.
    .restated_at_once();
    for line in labels_lines("Wrong account: ", labels, NOTICE_LABEL_LINES) {
        entry = entry.more(line);
    }
    entry = notice_tail(entry, filled);
    Some(entry.log_line(format!("accounts need=wrong-account n={}", labels.len())))
}

/// The key of the machine's one renew ask an earlier build raised for every
/// tab stopped on an ended login: no build raises it now (a lost login is its
/// tab's notice, [`resume_ask`]), and the resume family's settle withdraws a
/// row of it an earlier process left.
pub(crate) const RENEW_ASK_KEY: &str = "harness.accounts.renew-ask";

/// The roster of Claude Code accounts does not admit as written
/// ([`Family::Accounts`], `harness.accounts.refused`): no account can be named
/// and no `accounts login` runs until the file is mended, so this line stands
/// in the signed-out one's place. WHY it is refused is neither said here nor
/// logged: that reason names the file's path, and `aterm harness accounts`
/// prints it at the owner's own terminal.
pub(crate) fn accounts_refused_entry() -> Entry {
    Entry::new(
        Family::Accounts,
        "",
        "refused",
        "The Claude accounts list is refused",
        Why::OnlyYou(Reason::new(format!(
            "{} is refused as written, so no session changes account until it is fixed",
            aterm_agent::harness::accounts::ACCOUNTS_FILE
        ))),
        Then::step("Run aterm harness accounts to read why it is refused"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line("accounts need=roster-refused")
}

/// The key of [`accounts_logins_unread_entry`].
pub(crate) const ACCOUNTS_LOGINS_UNREAD_KEY: &str = "harness.accounts.logins-unread";

/// This machine's Claude logins cannot be read ([`Family::Accounts`],
/// `harness.accounts.logins-unread`): the roster admits, but `logins.json` —
/// which binds each row to the login a renewal made it — is torn, another
/// build's, or unreadable, so which login is whose is not known and no
/// session changes account until it reads again. It stands in the
/// signed-out line's place, as the roster's refusal does, and names its own
/// file. Why it cannot be read is
/// neither said here nor logged: that reason names the file's path, and
/// `aterm harness accounts` prints it at the owner's own terminal.
pub(crate) fn accounts_logins_unread_entry() -> Entry {
    Entry::new(
        Family::Accounts,
        "",
        "logins-unread",
        "Claude account logins cannot be read",
        Why::OnlyYou(Reason::new(format!(
            "{} cannot be read, so no session changes account until it can",
            aterm_agent::harness::accounts::LOGINS_FILE
        ))),
        Then::step("Run aterm harness accounts to read why"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line("accounts need=logins-unread")
}

/// The key of [`usage_tap_silent_entry`].
pub(crate) const USAGE_TAP_SILENT_KEY: &str = "harness.accounts.no-tap";

/// A SESSION aterm PUT ITS USAGE TAP ON WROTE NO READING after a turn it
/// served (`aterm_agent::harness::tap::Silence`): ONE line of
/// [`Family::Accounts`] for the machine, whatever the number of tabs, raised
/// at most once a day while `[harness] usage_tap` is on. A fact worth a look
/// later: a reading moves no session, and nothing waits on one.
pub(crate) fn usage_tap_silent_entry() -> Entry {
    Entry::new(
        Family::Accounts,
        "",
        "no-tap",
        "A Claude usage tap is silent",
        Why::OnlyYou(Reason::new(
            "Claude Code ran no status line for a tapped session, so its usage figures go unread",
        )),
        Then::step("Run aterm harness tap --help for what stops a tap"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .more("An untrusted folder, disableAllHooks, a managed statusLine or a moved aterm stops it")
    .log_line("accounts need=no-tap")
}

/// A Claude Code whose registry row reports a build its image does not name
/// ([`crate::session_timeline::RegistryDrift`]): the file
/// it runs from sits in a folder named for one build, and the program says it
/// is another. A fact for later, never a gate (the card's decoration does not
/// read it): an event of [`Family::UpgradeHost`]
/// (`harness.upgrade-host.<tab>.build-drift`), raised once per image and
/// reported build. The step checks what the store holds, or puts the vendor's
/// own install back.
pub(crate) fn claude_build_drift_entry(
    sid: &str,
    spot: &UpgradeSpot,
    drift: &crate::session_timeline::RegistryDrift,
) -> Entry {
    let (folder, then) = match drift.source {
        atpkg::image::ImageSource::Native => {
            ("installer", "Reinstall Claude Code with its own installer")
        }
        _ => ("store", "Run aterm pkg verify claude"),
    };
    let mut entry = Entry::new(
        Family::UpgradeHost,
        sid,
        "build-drift",
        "Claude Code reports another build",
        Why::OnlyYou(Reason::new(format!(
            "it runs from the {} {folder} folder but says it is {}",
            drift.image, drift.reported
        ))),
        Then::step(then),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line(format!(
        "claude build-drift sid={sid} image={} source={} reported={}",
        drift.image,
        drift.source.word(),
        drift.reported
    ))
    .more(format!("The Claude Code {}", spot.place));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// A repaint the render stream gave up on: the tab may be drawn one row off
/// (`render.<tab>.unrepaired`). `bare` is the tab's place past its "in " (`tab
/// 3`, `window 2, tab 1`); the first title that fits the glass rule is used.
pub(crate) fn render_unrepaired_entry(
    sid: &str,
    spot: &UpgradeSpot,
    why: aterm_core::terminal::repaint_nudge::GiveUp,
) -> Entry {
    let bare = spot.place.strip_prefix("in ").unwrap_or(&spot.place);
    let mut chars = bare.chars();
    let cap: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    let what = [
        format!("{cap} may be one row off"),
        "A tab may be one row off".to_string(),
    ]
    .into_iter()
    .find(|title| aterm_messages::text::glass_title_fault(title).is_none())
    .unwrap_or_else(|| "A tab may be off".to_string());
    let mut entry = Entry::new(
        Family::Render,
        sid,
        "unrepaired",
        &what,
        Why::OnlyYou(Reason::new(render_unrepaired_reason(why))),
        Then::step("Drag the window edge one row and back"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line(format!("render unrepaired sid={sid}"));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// Why a repaint gave up, one clause per cause the render stream tells apart.
fn render_unrepaired_reason(why: aterm_core::terminal::repaint_nudge::GiveUp) -> &'static str {
    use aterm_core::terminal::repaint_nudge::{Gate, GiveUp};
    match why {
        GiveUp::NoAnswer => "the program in the tab did not redraw when aterm asked",
        GiveUp::Deferred(Gate::Selection) => {
            "text stayed selected, so aterm did not redraw the tab"
        }
        GiveUp::Deferred(Gate::Lease) => {
            "another program was driving the tab, so aterm did not redraw it"
        }
        GiveUp::Deferred(Gate::Busy) => "the program kept drawing, so aterm did not redraw the tab",
        GiveUp::Budget => "aterm already tried to redraw this tab three times this hour",
    }
}

/// The key of [`render_unrepaired_entry`], for its end.
pub(crate) fn render_unrepaired_key(sid: &str) -> String {
    digest_key(Family::Render, sid, "unrepaired")
}

/// The key of [`palette_refused_entry`], for its end: a config generation that
/// allows palette sets again clears it (`App::apply_prepared_config_generation`).
pub(crate) fn palette_refused_key() -> String {
    digest_key(Family::Terminal, "", "palette-refused")
}

/// A program set palette colours (OSC 4, numeric OSC 21) and the owner's own
/// explicit `allow_palette_reconfigure = false` refused them (`terminal.palette-refused`,
/// an EVENT of [`Family::Terminal`]): the commonest theming tools (base16-shell,
/// pywal, `theme.sh`) then do nothing with no sign why. One line for every tab,
/// raised at a session's first refusal, never on screen; the gate stays off
/// until the owner turns it on, so a dismissal is their choice
/// ([`Resurface::CHOICE`]), and turning it on ends the line. The action names
/// the Settings row by that row's own label, so the two cannot drift.
pub(crate) fn palette_refused_entry() -> Entry {
    Entry::new(
        Family::Terminal,
        "",
        "palette-refused",
        "Program colours were refused",
        Why::Configured {
            key: Reason::new("allow_palette_reconfigure = false"),
            reason: Reason::new("a program tried to set terminal palette colours"),
        },
        Then::press(
            Mover::open_settings(SettingsRoute::Security.path()),
            format!(
                "Turn on {}",
                crate::prefs::security_label(crate::prefs::EDIT_ALLOW_PALETTE_RECONFIGURE)
            ),
        ),
    )
    .severity(Severity::Info)
    .resurface(Resurface::CHOICE)
    .more(
        "You turned this off in aterm.toml. Theme tools such as base16-shell, pywal and \
         theme.sh set colours this way (OSC 4).",
    )
    .log_line("terminal palette-refused key=allow_palette_reconfigure")
}

/// The API outage (`harness.api-outage.unreachable`, the whole answer of the
/// level [`Family::ApiOutage`]): `tabs` tabs' agents cannot reach their API,
/// the first since `since_hhmm` (local time, when the zone is known). Every
/// tab on the same network meets it at once, so it is one entry naming the
/// count — never a row or a notification per tab — and the outage ending
/// sweeps it. The count and the time move in place; the fact's identity is the
/// cause alone.
pub(crate) fn api_outage_entry(tabs: usize, since_hhmm: Option<&str>) -> Entry {
    // The reason names the tabs it counts, never the whole Mac: one tab of
    // twelve waiting is not every agent cut off.
    let (what, why) = if tabs == 1 {
        (
            "1 tab can't reach the API".to_string(),
            "the network keeps this agent from its API server",
        )
    } else {
        (
            format!("{tabs} tabs can't reach the API"),
            "the network keeps these agents from their API server",
        )
    };
    let mut entry = Entry::new(
        Family::ApiOutage,
        "",
        "unreachable",
        &what,
        Why::OnlyYou(Reason::new(why)),
        Then::step("Check this Mac's network connection"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    // The count moves only as tabs join and leave the wait, and the tabs
    // of one outage meet it within seconds of each other: each count is
    // shown as it moves, never held at the first tab's for ten minutes.
    .restated_at_once()
    .log_line(format!("api-outage tabs={tabs}"));
    if let Some(at) = since_hhmm {
        entry = entry.more(format!("since {at}"));
    }
    entry
}

/// A long hold (`harness.long-hold.<sid>.taken-over`, one entry of the level
/// [`Family::LongHold`]): the
/// person took tab `label` (session `sid`) over `mins` minutes ago and still
/// holds it, so its harness, its manager (`manager`, when the task names one
/// here) and its scripts are refused (`ERR halted … origin=person`). A fact
/// for the person's next look, never a row: the one act is theirs, Hand Back.
pub(crate) fn long_hold_entry(sid: &str, label: &str, mins: u64, manager: Option<&str>) -> Entry {
    // A glass title: the label's first words, so it stays six words and 48
    // characters whatever the tab is called.
    let short: String = label
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let short: String = short.chars().take(20).collect();
    let what = if short.is_empty() {
        format!("A tab held for {mins} min")
    } else {
        format!("You've held {short} for {mins} min")
    };
    let why = match manager {
        Some(_) => "only you type there, so its harness and its manager are refused",
        None => "only you type there, so its harness and its scripts are refused",
    };
    let mut entry = Entry::new(
        Family::LongHold,
        sid,
        "taken-over",
        &what,
        Why::OnlyYou(Reason::new(why)),
        Then::step("Hand the tab back when you are done with it"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line(format!("long-hold sid={sid} mins={mins}"));
    if let Some(manager) = manager {
        entry = entry.more(format!("worker of {manager}"));
    }
    entry
}

/// A client's long hold (`harness.long-hold.<sid>.client-held`, of the level
/// [`Family::LongHold`]): an Owner client's `hold … on` has stood on tab
/// `label` for `mins` minutes with no hold of the person's beside it. It
/// halts the instance's app verbs as the person's does (`hold_cost!`), and
/// nothing else names a forgotten one; Hand Back lifts it at the window, as
/// the client's own `hold off` would. `reason` is the hold's reason in words.
pub(crate) fn client_long_hold_entry(
    sid: &str,
    label: &str,
    mins: u64,
    reason: &str,
    manager: Option<&str>,
) -> Entry {
    let short: String = label
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let short: String = short.chars().take(20).collect();
    let what = if short.is_empty() {
        format!("A client held a tab {mins} min")
    } else {
        format!("A client held {short} {mins} min")
    };
    let mut entry = Entry::new(
        Family::LongHold,
        sid,
        "client-held",
        &what,
        Why::OnlyYou(Reason::new(
            "while any tab is held, no agent here can use its tab and pane verbs",
        )),
        Then::step("Hand the tab back to lift its hold"),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line(format!("long-hold sid={sid} mins={mins} by=client"))
    .more(format!("held by an Owner client: {reason}"));
    if let Some(manager) = manager {
        entry = entry.more(format!("worker of {manager}"));
    }
    entry
}

/// The resume loop's part of the exceptions digest. What the loop of each
/// tab found only a person can answer ([`Exception`], told through
/// `IdleHost::exception`) and that can wait for their next look (`Later`; a
/// lost login is `Now`: the loop's own escalation where the point escalated
/// it, else the owner's ask on the glass, [`resume_ask`], which the digest
/// does not repeat) becomes an [`Entry`] of [`Family::Resume`]:
///
/// * a limit, a cleared goal, a wall that will not lift: one entry per kind
///   (`harness.resume.<kind>`), naming every tab that has it, since the action
///   is the owner's and the same for each; the first tab's words speak, the
///   list of tabs is a detail.
/// * a stall no reader names ([`ExceptionKind::Stalled`]), a dialog no reader
///   names ([`ExceptionKind::Dialog`]), a context no `/compact` lowers
///   ([`ExceptionKind::Context`]) and a request the API refuses the same way
///   each time ([`ExceptionKind::Refused`]): one entry per tab
///   (`harness.resume.<tab>.stalled`, `.dialog`, `.context`, `.refused`),
///   because the action, typing into the tab, answering its dialog, going
///   back a few messages in it or changing what its request sends, is done
///   in that tab.
///
/// The words are the loop's (`why` and `action`, capped and checked by the
/// engine's vocabulary at [`Entry::message`]); a tab's own stall names the
/// agent its worker supervises. Returns the family's whole answer
/// for `App::except_replace`: a kind no tab has any more ends.
pub(crate) fn resume_exceptions(
    standing: &[(String, Exception, aterm_phase::Program, UpgradeSpot)],
) -> Vec<Entry> {
    let mut kinds: std::collections::BTreeMap<
        ExceptionKind,
        Vec<&(String, Exception, aterm_phase::Program, UpgradeSpot)>,
    > = std::collections::BTreeMap::new();
    let mut entries = Vec::new();
    for fact in standing {
        let (sid, exception, agent, spot) = fact;
        if exception.when != Urgency::Later {
            continue;
        }
        if matches!(
            exception.kind,
            ExceptionKind::Stalled
                | ExceptionKind::Dialog
                | ExceptionKind::Context
                | ExceptionKind::Refused
        ) {
            entries.push(resume_tab_entry(sid, exception, *agent, spot));
        } else {
            kinds.entry(exception.kind).or_default().push(fact);
        }
    }
    for (kind, group) in kinds {
        let (_, first, _, first_spot) = group[0];
        let n = group.len();
        let head = match kind {
            ExceptionKind::Spend => "Account limit stops Claude",
            ExceptionKind::Bucket => "Model limit stops Claude",
            ExceptionKind::GoalCleared => "Claude lost its goal",
            ExceptionKind::Futile => "Limit still holds Claude",
            // A login is `Now` (the loop's escalation, or the glass's ask); a
            // stall, a dialog, a context and a refused request are a tab's own.
            ExceptionKind::Login
            | ExceptionKind::Stalled
            | ExceptionKind::Dialog
            | ExceptionKind::Context
            | ExceptionKind::Refused => continue,
        };
        let what = if n == 1 {
            fitted_title(head, &first_spot.place)
        } else {
            let many = format!("{head} in {n} tabs");
            if aterm_messages::text::glass_title_fault(&many).is_none() {
                many
            } else {
                head.to_string()
            }
        };
        let mut entry = Entry::new(
            Family::Resume,
            "",
            kind.word(),
            &what,
            Why::OnlyYou(Reason::new(&first.why)),
            Then::step(&first.action),
        )
        .severity(Severity::Info)
        .resurface(Resurface::WAIT)
        .log_line(format!(
            "resume {} tabs={}",
            kind.word(),
            group
                .iter()
                .map(|(sid, _, _, _)| sid.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
        if n == 1 {
            if let Some(nav) = &first_spot.nav {
                entry = entry.show(nav.clone());
            }
        } else {
            let places: Vec<&str> = group
                .iter()
                .map(|(_, _, _, spot)| spot.place.strip_prefix("in ").unwrap_or(&spot.place))
                .collect();
            entry = entry.more(format!("In {}", places.join(", ")));
        }
        entries.push(entry);
    }
    entries
}

/// The stem of [`resume_ask_key`]: never [`Family::Resume`]'s, whose keys are
/// the digest's.
pub(crate) const RESUME_ASK_KEY: &str = "harness.resume-ask";

/// The key of a tab's [`resume_ask`]: `harness.resume-ask.<tab>`, the session
/// id cleaned as the rows print it.
pub(crate) fn resume_ask_key(sid: &str) -> String {
    format!(
        "{RESUME_ASK_KEY}.{}",
        atpkg::progress::sanitize_for_tty(sid, 64)
    )
}

/// The key of a tab's [`resume_ask`] for `notice` (the four-part notice it
/// says, by its [`Notice::code`]): `harness.resume-ask.<tab>.<code>`, or the
/// tab's own [`resume_ask_key`] for an exception with none. Another notice is
/// another key, so a notice the owner read is not said again while it stands
/// ([`aterm_messages::need::STANDING_QUIET`]) and a new one is news. The
/// code is two letters: the engine caps a key at 48 characters, and a tab's
/// id takes most of them.
pub(crate) fn notice_ask_key(sid: &str, notice: Option<&str>) -> String {
    match notice {
        Some(code) => format!("{}.{code}", resume_ask_key(sid)),
        None => resume_ask_key(sid),
    }
}

/// The resume loop's `Now` that no escalation of its own carries
/// ([`Exception::escalated`] false, [`Urgency::Now`]): a lost login aterm
/// carried as far as it can — `/login` typed, the method picked — whose one
/// step left is the owner's (the browser sign-in, a sign-in cancelled or
/// stopped, a renewed login the tab cannot move onto by itself). The owner is
/// needed now, aterm cannot settle it, and one step changes the outcome, so
/// it is a standing ask on the glass, keyed per tab and
/// notice ([`notice_ask_key`]); it stands until the loop says the point no
/// longer holds it — the login back, the agent working, the loop or its
/// worker ended.
///
/// The four parts: the one thing
/// to do now (the band's excerpt, under the title that already says what
/// happened), what happened, what aterm already did, and what to expect
/// after — one line each ([`notice`]), all four the expanded row's sentence
/// ([`sentence_lines`]), and after them, where the step starts what outlasts
/// the notice, how to end it (`accounts use`'s pin). The step leads
/// where every other ask's reason does. A row an older build
/// carries over is adopted by its second line (`Ask::adopted`): an older
/// build's step, or this build's happening for the moment until the
/// reporter re-asserts the ask. An exception with no notice says its action
/// and its why. Where the tab was found the row offers `Show tab N`
/// ([`UpgradeSpot::nav`]), as the digest's line for the tab does
/// ([`resume_tab_entry`]): the step is taken there.
///
/// A notice declares its fix words ([`notice::NoticeText::back`], the tab's
/// place fitted on: [`notice::BACK`] for a lost or renewed login,
/// [`notice::SWITCHED`] for a usage limit, where no login was lost): the
/// login back, or a move onto another account landed, resolves it, or closes
/// its record, `✓` in them (`App::close_notice_story`); a row withdrawn as
/// its point moves on keeps its own words.
pub(crate) fn resume_ask(sid: &str, spot: &UpgradeSpot, exception: &Exception) -> Message {
    let clean = |line: &str| atpkg::progress::sanitize_for_tty(line, 240);
    let notice = exception.notice.as_ref();
    let title = notice.map_or("Claude needs you", |n| n.title);
    // The band's title where the whole one does not fit: the notice's short
    // form, no tab in it (`Show tab N` names the tab), else the title with
    // its place left off ([`notice::NoticeText::short_title`]).
    let short_title = notice.map_or(title, |n| n.short_title);
    let mut message = Message::new(
        tags::HARNESS,
        Severity::Warn,
        fitted_title(title, &spot.place),
    )
    .short_title(short_title)
    .line(clean(&exception.action))
    .line(clean(&exception.why));
    if let Some(n) = notice {
        message = message
            .line(clean(&n.did))
            .line(clean(&n.expect))
            .finished_as(fitted_title(n.back, &spot.place));
        // The command a step points at Details for, whole
        // ([`notice::COMMAND_UNDER_DETAILS`]).
        if let Some(command) = &n.command {
            message = message.line(clean(command));
        }
        // How to end what the step starts, where it outlasts the notice
        // ([`notice::NoticeText::undo`]: the pin `accounts use` sets).
        if let Some(undo) = &n.undo {
            message = message.line(clean(undo));
        }
    }
    if let Some(nav) = &spot.nav {
        message = message.action(nav.clone());
    }
    message
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new(&exception.why)),
            notice_then(&exception.action, notice.and_then(|n| n.short)),
        ))
        .key(&notice_ask_key(sid, notice.map(|n| n.code)))
}

/// A notice's one step, with the words the band shows in its place where it
/// cannot show it whole ([`notice::Filled::short`]): a step that runs a
/// command is shown whole or replaced, never cut — a cut label is another
/// account's command.
fn notice_then(step: &str, short: Option<&str>) -> Then {
    let then = Then::step(step);
    match short {
        Some(short) => then.or_short(short),
        None => then,
    }
}

/// A tab's OWN entry (`harness.resume.<tab>.<kind>`) for what the owner does IN
/// that tab: a screen no reader names, or the agent's own background work,
/// stalled for hours, or a background call the jobs reading says made no
/// progress (open the tab and type, or tell the agent; the title names
/// `agent`, [`resume_agent_word`]), a dialog no reader names, stood at for an hour
/// (answer it), a context no `/compact` lowers (go back a few messages in
/// it), or a request the API refuses (the remedy its notice names:
/// `/rewind`, `/model`, …).
fn resume_tab_entry(
    sid: &str,
    exception: &Exception,
    agent: aterm_phase::Program,
    spot: &UpgradeSpot,
) -> Entry {
    let (head, tail, severity, note) = match exception.kind {
        ExceptionKind::Context => ("Context", " cannot be compacted", Severity::Info, ""),
        ExceptionKind::Refused => ("Claude's request", " is refused", Severity::Info, ""),
        ExceptionKind::Dialog => (
            "Dialog",
            " stops Claude",
            Severity::Warn,
            " for over an hour",
        ),
        // No length: today's canary raises at six units (3 h), one whose
        // jobs reading named the call (`Exception::no_progress`) one unit
        // past its `STALLED`, and the kind does not say which.
        _ => (resume_agent_word(agent), " has stalled", Severity::Warn, ""),
    };
    let title = placed_title(head, &spot.place, tail);
    let title = if aterm_messages::text::glass_title_fault(&title).is_none() {
        title
    } else {
        format!("{head}{tail}")
    };
    let mut entry = Entry::new(
        Family::Resume,
        sid,
        exception.kind.word(),
        &title,
        Why::OnlyYou(Reason::new(&exception.why)),
        Then::step(&exception.action),
    )
    .severity(severity)
    .resurface(Resurface::WAIT)
    .log_line(format!("resume {} sid={sid}{note}", exception.kind.word()));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// The agent a tab's own stall names: `Claude` as the digest's other lines
/// name it, any other agent by its product name ([`aterm_phase::Program::product`]:
/// `Antigravity CLI`, `Muse Code`), and a program no reader names `The agent`.
fn resume_agent_word(agent: aterm_phase::Program) -> &'static str {
    match agent {
        aterm_phase::Program::Claude => "Claude",
        aterm_phase::Program::Generic => "The agent",
        aterm_phase::Program::Codex
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Gemini => agent.product(),
    }
}

/// The key of the record that a tab's screen no reader names has stood with its
/// own title idle, or that its agent's own background work has run under words
/// that stood still (the canary's second rung: 90 minutes of background work,
/// 30 of a dialog).
fn stall_record_key(sid: &str) -> String {
    host_record_key("stalled", sid)
}

/// The record at the canary's second rung: aterm's own step is stuck (a wait
/// or a dialog this build does not classify), Warn, in the log for the
/// developer. Never a mark on the tab, and not yet a line of the digest
/// (`minutes`: how long). Its words fit both flavours of the canary.
pub(crate) fn stall_unread_record(sid: &str, place: &str, minutes: u64) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        fitted_title("Claude has stalled", place),
    )
    .line(format!(
        "Claude's own title has said idle for {minutes} min over a screen this build does not \
         recognise"
    ))
    .record()
    .key(&stall_record_key(sid))
}

/// The record at the canary's second rung on `agent`'s OWN background work
/// (agy's, Muse Code's: `StallSeen::OwnWork`): its turn ended, and the work
/// its reader names has run `minutes` under words that stood still. No title
/// was read and the screen is read, so nothing here says otherwise: Warn, in
/// the log, under the same key as [`stall_unread_record`] (one record per
/// episode). Never a mark on the tab, and not yet a line of the digest.
pub(crate) fn stall_on_work_record(
    sid: &str,
    place: &str,
    agent: aterm_phase::Program,
    minutes: u64,
) -> Message {
    let name = resume_agent_word(agent);
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        fitted_title(&format!("{name} has stalled"), place),
    )
    .line(format!(
        "{name}'s turn ended, and its own background work has run {minutes} min under words \
         that stood still"
    ))
    .record()
    .key(&stall_record_key(sid))
}

/// The record at the canary's second rung where the jobs reading explains the
/// screen (`StallSeen::NoProgress`, the hung-call design's R1): a background
/// call of `agent`'s own, process `pid`, has made no progress for `minutes`.
/// The screen is read and the kernel named the call, so nothing here says
/// otherwise: Warn, in the log, under the same key as [`stall_unread_record`]
/// (one record per episode). Never a mark on the tab, and not yet a line of
/// the digest. The pid and the minutes are its only variable words.
pub(crate) fn stall_no_progress_record(
    sid: &str,
    place: &str,
    agent: aterm_phase::Program,
    pid: u32,
    minutes: u64,
) -> Message {
    let name = resume_agent_word(agent);
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        fitted_title(&format!("{name} has stalled"), place),
    )
    .line(format!(
        "{name} waits on a background call (pid {pid}) that has made no progress for {minutes} min"
    ))
    .record()
    .key(&stall_record_key(sid))
}

/// The digest keys of a tab's relaunch lines: `[blocked, gone, lost]` — the
/// words of the agent's relaunch, which this process's host holds (its
/// adoption sweep ends an earlier process's). What a restart did not bring
/// back is a fact of its own beside them ([`relaunch_not_restored_key`]).
pub(crate) fn relaunch_keys(sid: &str) -> [String; 3] {
    [
        digest_key(Family::Supervisor, sid, "not-back"),
        digest_key(Family::Supervisor, sid, "gone"),
        digest_key(Family::Supervisor, sid, "lost"),
    ]
}

/// The digest key of [`relaunch_not_restored_entry`]:
/// `harness.supervisor.<tab>.not-restored`.
pub(crate) fn relaunch_not_restored_key(sid: &str) -> String {
    digest_key(Family::Supervisor, sid, "not-restored")
}

/// Something a restart lost: the agent came back, Claude Code
/// itself reported that something the session ran did not,
/// and nothing was typed to restore it — the conversation's
/// task was finished, or nobody had asked it anything
/// (`relaunch::restore_look`'s `done:lost`). Only the owner knows whether it
/// still matters: a line of the digest, in aterm's own words (the vendor's
/// row is never quoted). `harness.supervisor.<tab>.not-restored`, its own
/// key, never [`relaunch_keys`]' `lost` (an agent that cannot come back):
/// the agent's running again (`Ended`), its conversation held again
/// (`Back`) and a newer word of the tab end nothing here — what it lost
/// stays lost. It ends with its tab (the line shows the tab and asks a look
/// at it), and a seamless successor keeps it: a fact, not a word the earlier
/// process's host held.
pub(crate) fn relaunch_not_restored_entry(sid: &str, spot: &UpgradeSpot) -> Entry {
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        "not-restored",
        &fitted_title("Something did not come back", &spot.place),
        Why::OnlyYou(Reason::new(
            "Claude Code said something this session ran in the background did not come back",
        )),
        Then::step("Look at the tab and restore what it relied on"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(format!("relaunch not-restored sid={sid}"));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// The record of a relaunch's word (`text`: what happened).
/// Warn, in the log; the tab is not marked.
pub(crate) fn relaunch_record(sid: &str, place: &str, text: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        fitted_title("Agent is not back", place),
    )
    .line(atpkg::progress::sanitize_for_tty(text, 160))
    .record()
    .key(&host_record_key("relaunch", sid))
}

/// A relaunch that has kept failing for an hour for a cause only a person clears
/// (`cause`: [`super::harness_host`]'s `blocked_cause`): the owner clears it.
/// `harness.supervisor.<tab>.not-back`. Ends when the agent runs in the tab.
pub(crate) fn relaunch_blocked_entry(
    sid: &str,
    spot: &UpgradeSpot,
    cause: &str,
    restarted: bool,
) -> Entry {
    let (why, then) = match cause {
        "held" => (
            "the tab is held, so aterm cannot put the agent back in it",
            "Release the hold on that tab",
        ),
        "shell-prompt" => (
            "a job in the tab keeps the shell from its prompt, so the agent cannot be put back",
            "End the job running in that tab",
        ),
        _ => (
            "the conversation is open in another tab, and two copies would corrupt it",
            "Close the conversation in its other tab",
        ),
    };
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        "not-back",
        &fitted_title("Agent is not back", &spot.place),
        Why::OnlyYou(Reason::new(why)),
        Then::step(then),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(format!(
        "relaunch blocked sid={sid} cause={cause} restarted={restarted} for over an hour"
    ));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// A relaunch that keeps failing with no command the tab's mark could name:
/// the owner starts the agent, or clears what holds it (`cause`, as
/// [`relaunch_blocked_entry`] words it: a conversation open in another tab
/// is closed there, never started here too). `harness.supervisor.<tab>.not-back`.
pub(crate) fn relaunch_failing_entry(
    sid: &str,
    spot: &UpgradeSpot,
    cause: Option<&str>,
    restarted: bool,
) -> Entry {
    if let Some(cause) = cause {
        return relaunch_blocked_entry(sid, spot, cause, restarted);
    }
    let lead = if restarted {
        "aterm ended the agent and has not brought it back yet"
    } else {
        "the agent exited and its relaunch keeps failing"
    };
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        "not-back",
        &fitted_title("Agent is not back", &spot.place),
        Why::OnlyYou(Reason::new(format!("{lead}; aterm still tries"))),
        Then::step("Start the agent in that tab and resume its conversation"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(format!("relaunch failing sid={sid} restarted={restarted}"));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// An agent aterm cannot bring back and cannot name the command for (a launch it
/// will not repeat, no conversation known, a Codex): the owner starts it again
/// — in that tab, or, for an agent that ran as its tab's own program
/// (`new_tab`: no shell is left there), in a new one — or, where resuming it
/// is no remedy (`then`, `harness_host::cannot_then`: a resume that would
/// load a copy missing turns), does what that says.
/// `harness.supervisor.<tab>.gone`.
pub(crate) fn relaunch_gone_entry(
    sid: &str,
    spot: &UpgradeSpot,
    reason: &str,
    restarted: bool,
    new_tab: bool,
    then: Option<&str>,
) -> Entry {
    let lead = if restarted {
        "aterm ended the agent and cannot bring it back"
    } else {
        "aterm cannot bring the agent back"
    };
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        "gone",
        &fitted_title("Agent is gone", &spot.place),
        Why::OnlyYou(Reason::new(format!("{lead}: {reason}"))),
        Then::step(then.unwrap_or(if new_tab {
            "Start the agent in a new tab and resume its conversation"
        } else {
            "Start the agent in that tab and resume its conversation"
        })),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(format!("relaunch cannot sid={sid} reason={reason}"));
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// An agent aterm cannot bring back whose conversation is known, said where
/// the tab's mark does not say it (no command whole within the mark, or a
/// person working in the tab on another conversation):
/// the owner resumes it, by the command (its details) when there is one —
/// or, where resuming it is no remedy (`then`, `harness_host::cannot_then`:
/// a resume that would load a copy missing turns, a session with no turn),
/// does what that says. `harness.supervisor.<tab>.lost`. Ends when that
/// conversation is held again, in any tab, when a newer word of the tab
/// takes its place, or when the tab closes (it names a command to run there,
/// and shows it) — never when some program merely starts in the tab.
pub(crate) fn relaunch_lost_entry(
    sid: &str,
    spot: &UpgradeSpot,
    reason: &str,
    restarted: bool,
    command: Option<&str>,
    then: Option<&str>,
) -> Entry {
    let lead = if restarted {
        "aterm ended the agent and cannot bring it back"
    } else {
        "aterm cannot bring the agent back"
    };
    let then = match (then, command) {
        (Some(step), _) => Then::step(step),
        (None, Some(_)) => {
            Then::step("Run the command below in that tab to resume its conversation")
        }
        (None, None) => Then::step("Start the agent in that tab and resume its conversation"),
    };
    let mut entry = Entry::new(
        Family::Supervisor,
        sid,
        "lost",
        &fitted_title("Agent is gone", &spot.place),
        Why::OnlyYou(Reason::new(format!("{lead}: {reason}"))),
        then,
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(format!("relaunch lost sid={sid} reason={reason}"));
    if let Some(command) = command {
        entry = entry.more(command);
    }
    if let Some(nav) = &spot.nav {
        entry = entry.show(nav.clone());
    }
    entry
}

/// The tab a key names when it is one tab's [`agent_upgrade_key`] — this
/// build's or an earlier one's, which keyed the same way — or `None`: another
/// family's key, the waiting record's ([`KEY_AGENT_UPGRADE`]), the finished
/// moves' (`harness.upgrade.done`).
pub(crate) fn agent_upgrade_key_tab(key: &str) -> Option<&str> {
    key.strip_prefix(KEY_AGENT_UPGRADE)?
        .strip_prefix('.')
        .filter(|tab| !tab.is_empty() && *tab != "done")
}

/// The line of a stalled row's detail that names the tab and the move
/// ([`agent_upgrade_stalled`]: `in tab 2 · Claude Code 2.1.280 → 2.1.283`).
pub(crate) const STALL_MOVE_LINE: usize = 2;

/// The same stall, by the words of two stalled rows' details (`a`, `b`,
/// [`agent_upgrade_stalled`]) under one tab's key: the same move
/// ([`STALL_MOVE_LINE`], past its place) and the same stall — its words, an
/// overdue one's by its kind alone (`behind for …` moves with every look).
/// The place is not compared: a tab's place in its strip moves as tabs open,
/// close and move, and the key already names the tab. Keeps a stall the
/// owner already saw from being posted again after aterm restarts
/// (`upgrade_host`).
///
/// A row an earlier build posted reads the same when its words do (where the
/// move line held the move alone, with no place before it).
pub(crate) fn same_stall(a: &[String], b: &[String]) -> bool {
    let words = |d: &[String]| -> Option<(String, String)> {
        let (why, line) = stall_lines(d)?;
        let why = if why.starts_with("behind for ") {
            "overdue".to_string()
        } else {
            why.to_string()
        };
        let moved = strip_place(line).unwrap_or(line);
        Some((why, moved.to_string()))
    };
    matches!((words(a), words(b)), (Some(x), Some(y)) if x == y)
}

/// A stalled row's why and move lines ([`agent_upgrade_stalled`]), found by
/// what they say, not where they stand: a refused press
/// restates the row with the refusal above the stall's lines (the band
/// paints `detail[0]`), which shifts every index. The move is
/// the line that opens with a place (`in tab 2 · …`), the why two lines above
/// it; a row an earlier build posted, whose move line held no place, reads
/// at the fixed [`STALL_MOVE_LINE`]. `None` for a detail with neither.
fn stall_lines(d: &[String]) -> Option<(&str, &str)> {
    match d.iter().position(|l| strip_place(l).is_some()) {
        Some(at) => Some((d.get(at.checked_sub(2)?)?, &d[at])),
        None => Some((d.first()?, d.get(STALL_MOVE_LINE)?)),
    }
}

/// How many leading detail lines are the sentence:
/// Settings ▸ Messages sets them as a person's words and the rest as
/// technical lines. One for every record but a tab's upgrade row
/// ([`agent_upgrade_stalled`]), whose why and next step — `it asks again on
/// its own at 12:55 AM; Upgrade now asks it at its next pause; …` — stand
/// above its move line (found by its place, as [`stall_lines`] finds it, so a
/// refused press's restated row keeps its refusal, why and remedy together).
/// A record a key does not name, or one with no move line, keeps one.
pub(crate) fn sentence_lines(key: Option<&str>, detail: &[String]) -> usize {
    if key.and_then(agent_upgrade_key_tab).is_some()
        && let Some(at) = detail.iter().position(|l| strip_place(l).is_some())
    {
        return at.max(1);
    }
    // The lost-integration row ([`integration_lost`]): the tabs and the loss,
    // then what gives it back; each tab's remedy and the session ids are its
    // technical lines. The socketless harness row
    // ([`harness_without_socket`]): the cause, then its fix.
    // A lost login's ask ([`resume_ask`]; an earlier build's machine renew
    // ask, [`RENEW_ASK_KEY`]) and the two accounts notices
    // ([`accounts_signed_out_entry`], [`accounts_wrong_entry`]): every line is
    // the owner's sentence — the step and what happened (a tab's ask leads
    // with the step), what aterm did, what follows (or the accounts it
    // names).
    if key.is_some_and(is_notice_key) {
        return detail.len().max(1);
    }
    // The disk-full alert ([`disk_full`]): its step, its cause and what to
    // expect are the owner's sentence, as a notice's four parts are; the
    // largest folders under them are its technical lines.
    if key == Some(DISK_FULL_KEY) {
        return DISK_FULL_SENTENCE.min(detail.len()).max(1);
    }
    // The program-check alert ([`program_check_stuck`]) reads the same way:
    // its step, its cause and what it does; the pool's figures under them.
    if key == Some(PROGRAM_CHECK_STUCK_KEY) {
        return PROGRAM_CHECK_SENTENCE.min(detail.len()).max(1);
    }
    if key == Some(KEY_INTEGRATION_LOST) || key == Some(KEY_HARNESS_NO_SOCKET) {
        // Past more tabs than the first line names, the count of the rest
        // sits between the names and the remedy.
        let counted = key == Some(KEY_INTEGRATION_LOST)
            && detail.get(1).is_some_and(|l| integration_rest_count(l));
        return (2 + usize::from(counted)).min(detail.len()).max(1);
    }
    1
}

/// Whether `key` is a notice's: a tab's ask ([`resume_ask_key`] and the keys
/// under it), an earlier build's machine renew ask ([`RENEW_ASK_KEY`]), or an
/// accounts notice ([`ACCOUNTS_SIGNED_OUT_KEY`], [`ACCOUNTS_WRONG_KEY`]).
fn is_notice_key(key: &str) -> bool {
    key == RENEW_ASK_KEY
        || key == ACCOUNTS_SIGNED_OUT_KEY
        || key == ACCOUNTS_WRONG_KEY
        || key.starts_with(&format!("{RESUME_ASK_KEY}."))
}

/// Whether `line` is the lost-integration row's `and N more tabs` count
/// ([`integration_lost`]); a row carried from an older build may have the
/// bare `and N more`.
fn integration_rest_count(line: &str) -> bool {
    line.strip_prefix("and ")
        .and_then(|l| {
            let l = l
                .strip_suffix(" tabs")
                .or_else(|| l.strip_suffix(" tab"))
                .unwrap_or(l);
            l.strip_suffix(" more")
        })
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// `line` past a PLACE at its head and ` · ` — `in tab 2`, `in window 2,
/// tab 1`, `in its tab` (`upgrade_host`'s `upgrade_place`, [`tab_place`]) —
/// or `None` when it opens with none.
fn strip_place(line: &str) -> Option<&str> {
    let (place, rest) = line.split_once(" \u{00b7} ")?;
    let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    let tab = |p: &str| p.strip_prefix("tab ").is_some_and(number);
    let placed = place == "in its tab"
        || place.strip_prefix("in ").is_some_and(|p| {
            tab(p)
                || p.strip_prefix("window ")
                    .and_then(|p| p.split_once(", "))
                    .is_some_and(|(w, p)| number(w) && tab(p))
        });
    placed.then_some(rest)
}

/// A tab's PLACE in the person's words: `in tab 2` — its place
/// in its window's strip, counted from 1 — or, where the owner has more than
/// one window (`window`, counted from 1), `in window 2, tab 1`: tabs are
/// counted per window, and the band is every window's, so two tabs first in
/// their own windows would read the same. Never a session id.
pub(crate) fn tab_place(window: Option<usize>, tab: usize) -> String {
    match window {
        Some(window) => format!("in window {window}, tab {tab}"),
        None => format!("in tab {tab}"),
    }
}

/// A GLASS TITLE THAT NAMES ITS TAB: `{head} {place}{tail}` (`Couldn't
/// upgrade Claude in tab 1 yet`) — or, where that would break the glass title
/// rule (six words, 48 characters: [`aterm_messages::text::glass_title_fault`])
/// because the place names a window too (`in window 2, tab 1` is a word
/// longer), `{head}{tail}, {place past its "in "}`: `Couldn't upgrade Claude
/// yet, window 2, tab 1` — a comma, since the title lint allows
/// no parenthesis and no colon.
fn placed_title(head: &str, place: &str, tail: &str) -> String {
    let title = format!("{head} {place}{tail}");
    match place.strip_prefix("in ") {
        Some(bare)
            if bare.starts_with("window ")
                && aterm_messages::text::glass_title_fault(&title).is_some() =>
        {
            format!("{head}{tail}, {bare}")
        }
        _ => title,
    }
}

/// A STALL THAT ENDED WHILE ITS ROW WAS DOWN: the
/// person read or dismissed the stalled row ([`agent_upgrade_stalled`]) and
/// the stall has since ended — the upgrade moved on, finished, or its tab
/// went. A row no longer live cannot be resolved, so the log kept the
/// person's read or dismissal as the stall's last word, and when the same
/// stall came back — an overdue upgrade quieted by `--now` reading overdue
/// once more, a refusal a later round meets again after its first notice
/// went — `upgrade_host` took it for the row the person had already seen and
/// posted nothing. This RECORD, under the row's own `key`, is the stall's
/// end: the same stall coming back is news. `detail` is the stalled row's:
/// its first line (why) and its tab and move are kept.
///
/// Worded by what happened, with its tab: `Claude upgrade asks
/// again in tab 2` for a tab whose upgrade goes on (the tab still holds a
/// round), `Claude upgrade no longer waits in tab 2` for one that left —
/// never `no longer stalled`, a word the band retired, and never the stall's
/// reason in the present tense under a title saying it ended: the reason is
/// `was: …`, then the move. A restart under way that stuck and moves again
/// is `Claude upgrade moves again in tab 2`: nothing asks
/// anything again, it carries on. See [`StallEnd`].
pub(crate) fn agent_upgrade_stall_over(
    agent: aterm_agent::harness::upgrade::Agent,
    key: &str,
    detail: &[String],
    place: &str,
    end: StallEnd,
) -> Message {
    let who = agent_word(agent);
    let title = match end {
        StallEnd::AsksAgain => format!("{who} upgrade asks again {place}"),
        StallEnd::MovesAgain => format!("{who} upgrade moves again {place}"),
        StallEnd::Left => format!("{who} upgrade no longer waits {place}"),
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Info, title);
    if let Some((why, moved)) = stall_lines(detail) {
        msg = msg.line(format!("was: {why}")).line(moved);
    }
    msg.record().key(key)
}

/// What became of an upgrade whose stall ended while its row was down
/// ([`agent_upgrade_stall_over`]'s title).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StallEnd {
    /// The tab still holds a round of the upgrade, which asks again.
    AsksAgain,
    /// A restart under way that stuck (`stuck:<what>`) moves
    /// again, still in flight: it carries on, and asks nothing again.
    MovesAgain,
    /// The tab left the upgrade's rows: nothing waits there now.
    Left,
}

/// The agent a row's words name: `Claude` or `Codex`; Muse Code, agy and
/// Gemini CLI (whose relaunch rows are read) by its own name.
fn agent_word(agent: aterm_agent::harness::upgrade::Agent) -> &'static str {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "Claude",
        aterm_agent::harness::upgrade::Agent::Codex => "Codex",
        // Muse Code, agy and Gemini CLI: their own names (`Muse Code`,
        // `Antigravity CLI`, `Gemini CLI`).
        aterm_agent::harness::upgrade::Agent::Muse
        | aterm_agent::harness::upgrade::Agent::Agy
        | aterm_agent::harness::upgrade::Agent::Gemini => agent.product(),
    }
}

/// A round re-armed after a give-up that its agent's own work holds
/// (`Row::will_ask_again`'s pending arm — the fact without the age, for a
/// round a day behind goes on by itself too; a usage limit, the other one,
/// waits anyway): the upgrade working, and no word the owner presses moves it
/// sooner — the move comes at the end of that work.
fn own_work_round(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> bool {
    row.phase == aterm_agent::harness::upgrade::Phase::Pending && row.will_ask_again(now)
}

/// The owner's words that do something for this upgrade, most
/// useful first — the tab menu offers them all, a row its first two (two
/// authored capsules: `MAX_ACTIONS`). None while a restart is under way or
/// done: `upgrade_drive::ask` refuses a word then. `Upgrade now` only where
/// `--now` moves it: a healthy wait (the settling window and the attended
/// guard waived), an overdue one waiting on a turn end
/// ([`Remedy::Now`](aterm_agent::harness::upgrade_drive::Remedy::Now)), one
/// that gave up (asked again) — never one waiting on what `--now` does not
/// waive, one in a pane typing cannot reach, or one refused or failed for
/// good, where it would be recorded and move nothing, or be refused — nor
/// one the owner's `--now` already hurries. `Not today` and `Skip version`
/// hold every kind (a day; until a newer build).
pub(crate) fn agent_upgrade_words(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
) -> Vec<aterm_messages::UpgradeWord> {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    if !matches!(
        row.phase,
        Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
    ) || row.tab.is_empty()
        || row.to.is_empty()
    {
        return Vec::new();
    }
    let now_moves = match row.remedy(now) {
        // Not stalled: `--now` waives the quiet window — unless the owner's
        // `--now` is already in force, where pressing it again would only
        // arm a new round (a fresh READY marker) for a move already hurried.
        None => {
            !matches!(row.phase, Phase::Failed(_))
                && row.request != aterm_agent::harness::upgrade::Request::Now
        }
        Some(Remedy::Now) => !own_work_round(row, now),
        Some(Remedy::AskAgain) => true,
        Some(Remedy::Waits | Remedy::InItsPane | Remedy::ByHand | Remedy::ResumeInTab) => false,
    };
    // The word in force is not offered again: a deferral still running drops
    // `Not today`, a skip of this very build leaves nothing to say.
    use aterm_agent::harness::upgrade::{Request, Version};
    let deferred = match &row.request {
        Request::DeferUntil(until) => now < *until,
        Request::Skip(v) if Version::parse(v) == Version::parse(&row.to) => return Vec::new(),
        Request::Skip(_) | Request::None | Request::Now => false,
    };
    let mut words = Vec::with_capacity(3);
    if now_moves {
        words.push(UpgradeWord::Now);
    }
    if !deferred {
        words.push(UpgradeWord::NotToday);
    }
    words.push(UpgradeWord::Skip);
    words
}

/// Whether `Upgrade now` on this row only ASKS the agent once more, rather
/// than moving it at its next pause: its question waits unread behind a full
/// queue (typed once more), or its round gave up (re-armed: the
/// question typed again, and the move once the agent answers READY —
/// `upgrade_drive::Ask::Now`). Each such tab's own row says so
/// ([`agent_upgrade_stalled`]), and so must the answer to a press
/// ([`agent_upgrade_worded`]).
///
/// Never a round that gave up under the quiet flow (`Row::quiet`): re-armed,
/// it asks nothing — the quiet restart moves it at its next pause, as any
/// pending round's.
pub(crate) fn upgrade_now_asks_again(row: &aterm_agent::harness::upgrade_drive::Row) -> bool {
    row.wait == "queued"
        || (!row.quiet
            && matches!(
                &row.phase,
                aterm_agent::harness::upgrade::Phase::Failed(why)
                    if why == aterm_agent::harness::upgrade::GAVE_UP
            ))
}

/// A row's capsules: the first two of [`agent_upgrade_words`], for this
/// tab and the build the row names.
pub(crate) fn agent_upgrade_capsules(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
) -> Vec<Intent> {
    agent_upgrade_words(row, now)
        .into_iter()
        .take(2)
        .map(|word| Intent::AgentUpgrade {
            tab: row.tab.clone(),
            to: row.to.clone(),
            word,
        })
        .collect()
}

/// What the owner's word did, in the owner's terms — a RECORD: a
/// confirmation earns no row. The stalled row it was pressed on
/// takes these words as it is ANSWERED (`ℹ`, never the `✓` of
/// work delivered or a problem fixed — putting an upgrade off is a choice,
/// and `Upgrade now` only asks), so the entry the owner pressed is the one
/// that says what happened; with no such row, it is recorded. `row` is the
/// upgrade as the word left it; `place` names its tab (`in tab 2`).
/// `detail[0]` is the sentence, the move behind it.
///
/// `asked_again`: `Upgrade now` only asked the row pressed once more
/// ([`upgrade_now_asks_again`] of the row as it was shown: its question
/// unread behind a full queue, or its round given up) — said so, never `the
/// wait for a quiet tab is waived`, of a session already idle,
/// nor `moves at its next pause`, of a round that moves only once the agent
/// agrees.
pub(crate) fn agent_upgrade_worded(
    row: &aterm_agent::harness::upgrade_drive::Row,
    word: aterm_messages::UpgradeWord,
    place: &str,
    asked_again: bool,
) -> Message {
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let who = agent_word(row.agent);
    let (from, to) = (clean(&row.from), clean(&row.to));
    let (title, then) = match word {
        UpgradeWord::Now if asked_again => (
            format!("{who} is asked again {place}"),
            format!("the upgrade's question is typed once more; it moves once {who} agrees"),
        ),
        // The first pause, never "when its turn ends" nor "when it goes idle"
        // — a goal-mode Codex's turns never end. Under the default
        // (`upgrade_ask = false`) no pause is made for a goal, so its move
        // comes once the goal stops.
        UpgradeWord::Now => (
            format!("{who} moves at its next pause"),
            "the wait for a quiet tab is waived; it never moves over typing, a draft, a dialog \
             or running work"
                .to_string(),
        ),
        UpgradeWord::NotToday => (
            format!("{who} stays on {from} until tomorrow"),
            "the upgrade tries again from this time tomorrow".to_string(),
        ),
        UpgradeWord::Skip => (
            format!("{who} skips {to}"),
            format!("it stays on {from} until a build newer than {to} comes"),
        ),
    };
    Message::new(tags::HARNESS, Severity::Info, title)
        .line(then)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .hold(Hold::LogOnly)
        .key(&agent_upgrade_key(&row.tab))
}

/// The owner's word refused: nothing was written, and why — another
/// step held the lock (press it again), the upgrade moved on to a newer
/// build than the one pressed for, or `upgrade_drive::ask`'s own reason (a
/// restart under way, one stopped for good, none recorded). The person's own
/// press failed, so it is the one gesture-failure shape:
/// Error, `HOLD_GESTURE` — a stalled row it was pressed on takes these words
/// above its own and keeps its own mark and hold.
///
/// A round that stopped between the offer and the press is
/// worded here from the writer's typed refusal
/// ([`aterm_agent::harness::upgrade_drive::refused_stopped`]): the tab by its
/// place, when its next round starts on the person's clock — never the CLI's
/// sentence with its raw session id, flags and stop word, nor a button the
/// entry may not carry. Any other refusal is the writer's own sentence: kept whole,
/// but behind Details (`no_excerpt`), so the band paints only the title.
pub(crate) fn agent_upgrade_word_refused(
    word: aterm_messages::UpgradeWord,
    agent: aterm_agent::harness::upgrade::Agent,
    (tab, to): (&str, &str),
    why: &str,
    place: &str,
) -> Message {
    use aterm_messages::UpgradeWord;
    let who = agent_word(agent);
    let shown = atpkg::progress::sanitize_for_tty(to, 32);
    // The tab by its place, and what was refused, short enough that the remedy
    // beside it is painted at 80 columns. The tab names whose upgrade.
    let title = match word {
        UpgradeWord::Now => placed_title(&format!("Couldn't upgrade {who}"), place, " now"),
        UpgradeWord::NotToday => placed_title("Couldn't postpone the upgrade", place, ""),
        UpgradeWord::Skip => placed_title(&format!("Couldn't skip {who} {shown}"), place, ""),
    };
    // The remedy is its own short line (the painted excerpt), the mechanism
    // the next, so at 80 columns the remedy is never truncated behind the
    // mechanism.
    let mut again = false;
    let (reason, then, plain) = if why.starts_with("busy:") {
        again = true;
        (
            "press it again".to_string(),
            Some("another step held the upgrade's lock, so nothing was written".to_string()),
            true,
        )
    } else if let Some(now_to) = why.strip_prefix("stale:") {
        // The target changed; nothing moves now.
        let now_to = atpkg::progress::sanitize_for_tty(now_to, 32);
        (
            format!("a newer build: {now_to}"),
            Some(format!(
                "the upgrade is to {now_to} now, so nothing was written for {shown}"
            )),
            true,
        )
    } else if let Some((at, _)) = aterm_agent::harness::upgrade_drive::refused_stopped(why) {
        let when = match crate::presence::local_offset_s() {
            Some(offset) if at > 0 => format!(
                "at {}",
                aterm_messages::words::clock_words(at.saturating_mul(1000), offset)
            ),
            _ => "soon".to_string(),
        };
        // No button named: the entry this lands on carries its own capsules,
        // and a refusal of its own carries none.
        (
            format!("retries {when}"),
            Some("the upgrade stopped before the press reached it".to_string()),
            true,
        )
    } else {
        (format!("nothing was written: {why}"), None, false)
    };
    let mut msg = gesture_failure(
        tags::HARNESS,
        Severity::Error,
        &title,
        &atpkg::progress::sanitize_for_tty(&reason, 600),
        Then::step("Press the word again on the upgrade's row"),
    );
    if let Some(then) = then {
        msg = msg.line(then);
    }
    // A writer's own sentence names the raw tab and a CLI's flags: in a
    // person's words where its kind is known, whole behind Details.
    let msg = if plain {
        msg
    } else {
        match refusal_words(why) {
            Some(words) => {
                let mut msg = msg;
                msg.detail.insert(0, words.to_string());
                msg
            }
            None => msg.no_excerpt(),
        }
    };
    // Another step held the lock: the press itself is the remedy, so the row
    // carries the very capsule that was pressed.
    if again && !tab.is_empty() && !to.is_empty() {
        msg.action(Intent::AgentUpgrade {
            tab: tab.to_string(),
            to: to.to_string(),
            word,
        })
    } else {
        msg
    }
}

/// A writer's refusal of the owner's word in a person's words, by its kind;
/// `None` for one this build does not know.
fn refusal_words(why: &str) -> Option<&'static str> {
    if why.contains("is already under way") {
        Some("its restart is already under way")
    } else if why.contains("stopped after its `/exit`") {
        Some("it already quit: codex resume in the tab takes it back")
    } else if why.starts_with("no upgrade is recorded") {
        Some("no upgrade waits in that tab now")
    } else if why.contains("stopped for good") || why.contains("re-arms only one that gave up") {
        Some("its upgrade stopped and was not asked again")
    } else {
        None
    }
}

/// The finished words of a stall row whose upgrade went through after all:
/// `Claude upgraded in tab 1` — the `✓` record's title in the log, where the
/// row's `⚠ Couldn't upgrade …` read as a failure beside the success record.
pub(crate) fn agent_upgrade_went_through(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> String {
    format!("{} upgraded {place}", agent_word(row.agent))
}

/// How long after its relaunch a move may finish and its record still read as
/// the move's own news ([`moved_line`]): ten minutes, well past a move's
/// adoption, first idle point and model wait.
const MOVED_SAID_AFTER_S: u64 = 10 * 60;

/// When a move happened (`moved_at`, its relaunch line typed), as its record's
/// line — `moved at 2:04 PM`, `moved yesterday 2:04 PM`, `moved Thursday 2:04
/// PM` — on the reader's clock `offset` from UTC, against the day it finished
/// (`done_at`), which is the record's. `None` when the move finished within
/// [`MOVED_SAID_AFTER_S`] of it (the record's own instant says it), when
/// either instant is unknown, and where the clock's offset is: no time is said
/// on a clock that may be hours off.
pub(crate) fn moved_line(moved_at: u64, done_at: u64, offset: Option<i64>) -> Option<String> {
    if moved_at == 0 || done_at < moved_at.saturating_add(MOVED_SAID_AFTER_S) {
        return None;
    }
    let when = clock_on_day(moved_at, done_at, offset?);
    Some(if when.starts_with(|c: char| c.is_ascii_digit()) {
        format!("moved at {when}")
    } else {
        format!("moved {when}")
    })
}

/// THE LIVE AGENT UPGRADE, DONE: the owner's outcome line — the build the
/// session resumed on and the model its first answer named, or that the model
/// could not be confirmed ([`aterm_agent::harness::upgrade::restart_outcome`])
/// — as a RECORD.
///
/// A move that FINISHED WELL AFTER IT HAPPENED says when it happened
/// ([`moved_line`]): a Claude Code move is done at its session's first idle
/// point after the relaunch, and the record is written then.
pub(crate) fn agent_upgrade_done(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> Message {
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 160);
    // The outcome opens `claude restarted on <to> · model …` (a Codex move's
    // `codex on <to> · <what came back>`); the title says where it moved, so
    // the detail carries the half after it.
    let model = row
        .outcome
        .split_once(" · ")
        .map_or(row.outcome.as_str(), |(_, rest)| rest);
    let moved = moved_line(row.moved_at, row.done_at, crate::presence::local_offset_s());
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Success,
        format!("{} moved onto {}", row.agent.product(), clean(&row.to)),
    )
    .line(place)
    .lines(moved);
    if !model.is_empty() {
        msg = msg.line(clean(model));
    }
    msg.hold(Hold::LogOnly)
        .key(&format!("{KEY_AGENT_UPGRADE}.done"))
}

/// The first-open install doctor: this copy runs from somewhere it can
/// neither update itself from nor put `aterm` on a new shell's PATH from,
/// and only the person can fix that. A row, then — a failure they must act
/// on — and `None` for every posture with nothing to fix (a healthy install,
/// a plain binary), whose `remedy()` is `None` too. Warn, because both
/// consequences are silent otherwise; no capsule, because the fix is a drag
/// in the Finder that no page of aterm's performs. The instruction IS the
/// title; `detail[0]` says where it runs; the remedy whole and
/// `aterm_update`'s own summary follow behind Details.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn install_posture(
    posture: aterm_update::which_copy::InstallPosture,
) -> Option<Message> {
    use aterm_update::which_copy::InstallPosture;
    let remedy = posture.remedy()?;
    let where_ = match posture {
        InstallPosture::MountedImage => "running from a mounted volume",
        InstallPosture::Translocated => "running from a temporary copy",
        InstallPosture::Installed | InstallPosture::NotABundle => return None,
    };
    Some(
        // Filed under `system`: how aterm itself is installed is no ALab tool.
        Message::new(tags::SYSTEM, Severity::Warn, "Move aterm to Applications")
            .line(where_)
            .line(remedy)
            .now(Ask::flash(
                Why::OnlyYou(Reason::new(format!(
                    "aterm is {where_}, so it cannot update itself"
                ))),
                Then::step("Drag aterm into Applications"),
                Span::Ten,
            ))
            .key(KEY_INSTALL_POSTURE),
    )
}

// ---------------------------------------------------------------------------
// A person's Settings ▸ Packages verb.
// ---------------------------------------------------------------------------

/// A Settings ▸ Packages verb a person pressed and is WAITING on — so its
/// pass takes the band's animated row whatever it weighs
/// ([`aterm_messages::Waiter::Person`]). The row shares the lane's key ([`crate::toolchain_words::KEY_PASS`]):
/// once the pass writes its plan, the tailer's reads restate it with the fill,
/// the ETA and `3 of 10 programs`, exactly as a first run's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesVerb {
    /// `Check & Update Now` (`atpkg update`).
    Check,
    /// `Install ALab Tools Now` (`atpkg install --default-set`).
    Install,
    /// `Remove ALab Tools` (`atpkg uninstall --all`).
    Remove,
}

impl PackagesVerb {
    /// The row's title while the verb's own work runs, before (or without) a
    /// plan: what it is doing, in the lane's one noun.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Check => "Checking ALab tools",
            Self::Install => crate::toolchain_words::INSTALLING_ALAB,
            Self::Remove => "Removing ALab tools",
        }
    }

    /// The words its Complete echo shows when the verb ends well.
    pub(crate) const fn finished(self) -> &'static str {
        match self {
            Self::Check => crate::toolchain_words::PACKAGES_FINISHED,
            Self::Install => crate::toolchain_words::PACKAGES_INSTALLED,
            Self::Remove => "ALab tools removed",
        }
    }

    /// The words its Complete echo shows once the pass planned work — an
    /// update the Check found, the set an Install lays down.
    pub(crate) const fn finished_work(self) -> &'static str {
        match self {
            Self::Check => "ALab tools updated",
            Self::Install | Self::Remove => self.finished(),
        }
    }
}

/// The title while a person's verb is queued behind another `atpkg` pass at
/// the store lock (up to its 30-minute bound): what they are waiting for.
pub(crate) const WAITING_FOR_ALAB: &str = "Waiting for another ALab update";

/// A person's verb row before its pass plans anything: BUSY (no fraction is
/// known yet; the band adds the elapsed clock past ten seconds), keyed to the
/// lane's pass, revealed after [`aterm_messages::PROGRESS_GRACE`] so a no-op
/// Check (1–4 s, measured in `packages.log`) never reaches the glass, and
/// ending in its [`PackagesVerb::finished`] words. No capsule: the person is
/// on the page that pressed it. A removal declares the disk as
/// its load — deleting gigabytes of toolchains is the heavy part.
pub(crate) fn packages_verb_row(verb: PackagesVerb) -> Message {
    let (glyph, load) = match verb {
        PackagesVerb::Check => ('\u{21bb}', None),
        PackagesVerb::Install => ('\u{21e3}', None),
        PackagesVerb::Remove => ('\u{2296}', Some(aterm_messages::Load::Disk)),
    };
    // The work's own load: a check or an install downloads, a removal deletes
    // — their words never show.
    let primary = match verb {
        PackagesVerb::Check | PackagesVerb::Install => aterm_messages::Load::Network,
        PackagesVerb::Remove => aterm_messages::Load::Disk,
    };
    Message::new(tags::TOOLCHAIN, Severity::Info, verb.title())
        .glyph(Glyph::or_fallback(glyph))
        .loads(crate::toolchain_words::PASS_LOADS)
        .primary_load(primary)
        .no_excerpt()
        .meter(aterm_messages::Meter {
            load,
            ..aterm_messages::Meter::busy("")
        })
        .working(aterm_messages::STALE_ANNOUNCE)
        .key(crate::toolchain_words::KEY_PASS)
        .finished_as(verb.finished())
        .reveal_after(aterm_messages::PROGRESS_GRACE)
}

/// A person's verb QUEUED at the store lock behind another pass (atpkg's
/// `lock-waiting:`): busy with the elapsed
/// clock, held for the wait's own bound and a margin, and restated back to
/// [`packages_verb_row`] when the lock is taken. A holder whose plan the
/// tailer reads restates it with that pass's fill — the work they wait
/// behind, and how long it has left.
pub(crate) fn packages_waiting_row(verb: PackagesVerb) -> Message {
    Message::new(tags::TOOLCHAIN, Severity::Info, WAITING_FOR_ALAB)
        .glyph(Glyph::or_fallback('\u{23f8}'))
        .loads(crate::toolchain_words::PASS_LOADS)
        .no_excerpt()
        .meter(aterm_messages::Meter::busy(""))
        .working(Duration::from_secs(crate::ATPKG_WAIT_LOCK_SECS) + Duration::from_mins(5))
        .key(crate::toolchain_words::KEY_PASS)
        .finished_as(verb.finished())
        .reveal_after(aterm_messages::PROGRESS_GRACE)
}

/// One control character, made VISIBLE: the band spends exactly one cell per
/// char, so a control byte in a diagnostic neither draws anything nor
/// honestly occupies the cell it took. `paste_banner::sanitized` spells a
/// hostile clipboard's control bytes this way for the same reason; a config
/// diagnostic is the same hazard arriving from the other direction.
const CONTROL_MARK: char = '\u{00b7}';

/// The PHYSICAL rows of a (possibly multi-line) diagnostic. Blank rows are
/// dropped (a `toml` diagram opens on a bare gutter line, and a row spent on
/// nothing is a row the caret loses), trailing whitespace goes with them, and
/// any control character that survives the split becomes [`CONTROL_MARK`].
/// Leading whitespace is KEPT: the caret row is aligned under the column it
/// points at. Every diagnostic yields at least one row.
fn diagnostic_lines(words: &str) -> Vec<String> {
    let mut rows: Vec<String> = words
        .lines()
        .map(|row| {
            row.trim_end()
                .chars()
                .map(|ch| if ch.is_control() { CONTROL_MARK } else { ch })
                .collect::<String>()
        })
        .filter(|row| !row.trim().is_empty())
        .collect();
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// A feedback sentence's head and tail: the head before the first `: ` and
/// the tail after it — `Config was NOT saved: <error>` reads as the head
/// and the cause — or the whole sentence and nothing when it carries no
/// colon (`Message::line` drops an empty line).
fn split_sentence(words: &str) -> (&str, &str) {
    words.split_once(": ").unwrap_or((words, ""))
}

// A glass title fits well inside the engine's title cap: the cap bounds a
// record's words, the glass rule a row's.
const _: () = assert!(GLASS_TITLE_CHARS < TITLE_CAP);

#[cfg(test)]
#[path = "message_reporters_tests.rs"]
mod tests;
