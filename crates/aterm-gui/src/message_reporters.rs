// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONFIG BAND'S REPORTERS, AS MESSAGES — every producer that used to
//! write a line into the retired overwrite banner (`config_notice.rs`) and the
//! retired floating toast (`notice.rs`), each a pure `fn … -> Message`
//! (docs/DESIGN-unified-messages-2026-09-21.md §6, §10.3). Nothing here reads
//! a clock, a window or `App`: a site that runs before `App` exists or off the
//! loop thread queues what these build on the pre-App inbox
//! ([`crate::message_inbox::queue_message`]); a site on the loop posts it
//! (`App::post_message`).
//!
//! # The owner's attention rule (2026-09-23, design §10)
//!
//! The glass carries ONLY three things ([`attention`]): work in flight with an
//! animated indicator that says what is coming and how long (P, progress),
//! a genuine decision (D), or a failure the person must act on (F).
//! Everything else — confirmations, "done", FYI, precautionary notices,
//! disclosures, repeats — is a [`Hold::LogOnly`] RECORD that Settings ▸
//! Messages and `messages.log` keep. A glass title is a few words, verb- or
//! noun-first, with no clauses ([`aterm_messages::GLASS_TITLE_WORDS`], [`GLASS_TITLE_CHARS`]);
//! `detail[0]` is painted only when it changes what the person does
//! (`Message::no_excerpt` otherwise); every other word waits behind Details.
//!
//! # One message per config FAMILY (R1, R4)
//!
//! A launch or reload collects its sentences into [`ConfigWarnings`], one
//! bucket per [`ConfigFamily`], and each bucket becomes ONE message: a terse
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
//! the log's (`App::replace_config_set`, review 2026-09-24). The
//! restart-only family and the retired spellings are records.
//!
//! # The crash message (R2)
//!
//! The artifact's absolute path rides behind Details — the log keeps it
//! whole — with its first lines (`logging::CrashEvidence`); `Open log` opens
//! it as text (`Intent::OpenPath`, re-validated at press time). The row
//! paints its title alone.
//!
//! # A failed gesture (R9–R14)
//!
//! The floating card (`notice.rs`, retired 2026-09-23) carried a gesture's
//! failure, its decisions (Full Disk Access; the admin step was deleted
//! with the OS-installer protocols, 2026-09-24), their follow-ups and a
//! handful of disclosures. They are messages now, sorted by
//! the owner's attention rule ([`attention`], ruling 76): the glass carries
//! work in flight, very heavy system use while it lasts, and a decision or a
//! failure the person must act on — everything else is a [`Hold::LogOnly`]
//! record. A gesture the person made that did not happen is ONE shape
//! ([`gesture_failure`]): a few words for a title, the whole error behind
//! Details, a short `HOLD_GESTURE`. (R20, the toolchain offer, went with the
//! sealed seed's `seed-pending:` marker that raised it, Phase 5.)

use std::time::Duration;

use aterm_agent::supervise::policy::exception::{Exception, Urgency};
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
use crate::native_settings::SettingsRoute;

/// What earns a row on the glass (the owner's attention rule, 2026-09-23). The
/// rule as code: the tests hold every builder to it (design §10.1, ruling 76).
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
    /// folds into the log by itself after [`ANSWER_MAX`] - not a standing FYI
    /// (ruling 423). Nothing else earns it.
    Answer,
    /// Everything else: a record, never on the glass.
    Record,
}

/// Glass titles: a few words, verb- or noun-first, no clauses — ONE copy, the
/// engine's (design ruling 179), so the wire's `notice` and every builder here
/// meet the same form. Only the character cap is read here now: the one builder
/// that counted words, the admin step's `capped_title`, went with the admin row
/// (design 2026-09-22 §5.3(b)).
use aterm_messages::GLASS_TITLE_CHARS;

/// Classify `msg`, or say why it may not be on the glass: a measured level
/// off the strain row is refused; a record is a record; a confirmation, an FYI and progress with no indicator are not
/// allowed on the glass; a live row with its indicator — a fill or busy
/// (ruling 139: the indicator is the meter's state, never the hold's) — is
/// progress, and may carry a consequential capsule only when it stops that
/// row's own work (`Intent::stops_work`, ruling 232); a live row that waits on the
/// person (Warn or Error, still) is refused — it persists without saying why, so
/// the door records it and what it asked is `.now(Ask)`; an ask or a consequential capsule is a
/// decision; a warning or an error a failure.
/// Every non-record title is at most [`aterm_messages::GLASS_TITLE_WORDS`] words and
/// [`GLASS_TITLE_CHARS`] characters, carries no clause seam (` — `, `; `,
/// `: `) and does not end in a period.
#[cfg(test)]
pub(crate) fn attention(msg: &Message) -> Result<Attention, &'static str> {
    // A measured LEVEL is Progress on the strain row alone (tag `system`,
    // key `system.strain`); anywhere else it is refused (design ruling 208).
    if let Some(fault) = aterm_messages::strain::level_fault(msg) {
        return Err(fault);
    }
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
            // Work in flight is no decision (ruling 101), with ONE exception
            // (ruling 232): a capsule that stops the row's own work — `Stop
            // paste` — which the person may want while they wait. Any other
            // consequential press on a moving row is refused.
            if msg
                .actions
                .iter()
                .any(|i| i.is_consequential() && !i.stops_work())
            {
                return Err("a decision on work in flight");
            }
            Attention::Progress
        } else if msg.severity >= Severity::Warn {
            // A live row blocked on the person — the update flow's old editor
            // block — persists without saying why: the door records it
            // (`Message::admit`, owner rule 2026-09-28), so it is not a shape
            // a reporter may build. What it asked is `.now(Ask)`.
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

/// The supersede key of the launch load failure (R3).
pub(crate) const KEY_LAUNCH_LOAD: &str = "config.launch-load";

/// The supersede key of the GPU-lost row (R7): Standing until a window
/// created after the loss paints (`App::finalize_successful_present`).
pub(crate) const KEY_GPU_LOST: &str = "render.gpu-lost";

/// The supersede key of the dead accessibility publisher (R8). Its producer
/// is the panic hook of a build WITH an accessibility tree; the builder is
/// platform-neutral (D10) and this build's tests still exercise it. Never
/// gated: a seamless successor withdraws a carried row under it whatever it
/// was built with (`messages_host::describes_the_old_process`).
pub(crate) const KEY_A11Y_PUBLISHER: &str = "a11y.publisher";

/// The supersede key of the Windows backdrop family (R6): every backdrop
/// decline is one record, whichever site declined it first. The producers are
/// Windows-only; the builder is platform-neutral (D10).
#[cfg(any(windows, test))]
pub(crate) const KEY_BACKDROP: &str = "render.backdrop";

/// The supersede key of the Serious Mode write feedback (R12).
pub(crate) const KEY_SERIOUS_MODE: &str = "config.serious-mode";

/// The supersede key of the native config lane's own errors (R13).
pub(crate) const KEY_CONFIG_LANE: &str = "config.lane";

/// The supersede key of a refused or fleet-owned hold (R10).
pub(crate) const KEY_FABRIC_HOLD: &str = "fabric.hold";

/// The key the Full Disk Access question, its route words and its grant
/// share (R15–R17): one question per process, restated in place after *Open
/// Settings*, answered by *Not now* or resolved by the grant. The card's
/// policy switch resolves the whole `privacy.` prefix.
pub(crate) const KEY_FILE_ACCESS: &str = "privacy.fda";

/// The key the debugger-access records share (design
/// `docs/DESIGN-macos-debugger-access-2026-09-29.md` §3.8): each one replaces
/// the last. Records only in P1 — nothing under it is ever a live row, so the
/// FDA card's `privacy.` prefix retraction has nothing of it to resolve.
pub(crate) const KEY_DEBUGGER_ACCESS: &str = "privacy.debugger";

/// The `Intent::OpenSystemPane` pane name of the Full Disk Access list — the
/// only pane the codec carries (`messages_host::privacy_pane`).
pub(crate) const PANE_FULL_DISK_ACCESS: &str = "full-disk-access";

/// The key of the install-posture row (R21): one per launch.
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
    /// `seed_install`, `game_font`): keys aterm or atpkg still reads, or that a
    /// newer key now decides. A RECORD, never a row: nothing is broken and
    /// nothing presses — the editor marks each one, and Settings ▸ Packages
    /// already explains an Off the retired `auto_update` holds. Kept apart from
    /// [`Self::IgnoredKeys`], whose words say "no effect in this build": told
    /// that, a person who deletes `auto_update = false` turns automatic updates
    /// back on (review of the second origin/main merge, 2026-09-23; design
    /// ruling 42).
    RetiredKeys,
    /// Keys spelled right whose VALUE this build does not accept.
    UnacceptedValues,
    /// `cursor_trail_style` and the Trail Pack manifests.
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
            // at 80 columns (review 2026-09-24). "No effect", never
            // "unknown": this family also carries every key a REMOVED
            // feature left behind (`show_scene_hud`, `scene_rows`, … —
            // `RETIRED_CONFIG_KEYS`), and this build knows those precisely;
            // "no effect in this build" is true of every member (fffef97a1,
            // ruling 146).
            // Ruling 261: the config row names what is wrong in a person's
            // words — `Misspelled setting` where the validator has a near
            // miss ([`config_family_message`] reads the sentences), `Unknown
            // setting` otherwise; a key a REMOVED feature left behind is the
            // retired family's record (ruling 213), never this row.
            Self::IgnoredKeys if one => "Unknown setting".to_string(),
            Self::IgnoredKeys => format!("{n} unknown settings"),
            Self::RetiredKeys if one => "Retired setting".to_string(),
            Self::RetiredKeys => format!("{n} retired settings"),
            Self::UnacceptedValues if one => "Couldn't use a setting's value".to_string(),
            Self::UnacceptedValues => format!("Couldn't use {n} settings' values"),
            Self::CursorTrail if one => "Couldn't apply the cursor trail".to_string(),
            // A count of the family's sentences, which are trail packs and the
            // style alike — not "settings" — and short enough that a 60-column
            // row keeps its `Open aterm.toml` beside `Value not accepted`'s
            // (design ruling 217).
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
    /// it (design ruling 265): `Misspelled setting fixed`, `Font applied`.
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
            Self::CursorTrail => "Cursor trail applied".to_string(),
            Self::Fonts if one => "Font applied".to_string(),
            Self::Fonts => format!("{n} fonts applied"),
            Self::Assets if one => "Image loaded".to_string(),
            Self::Assets => format!("{n} images loaded"),
            Self::SecureKeyboard => "Secure Keyboard Entry changed".to_string(),
            Self::Restart | Self::RetiredKeys => return None,
        })
    }

    /// The first sentence's excerpt — what changes what the person does,
    /// as a fragment, never a sentence cut mid-thought (review round 2,
    /// 2026-09-23): Secure Keyboard Entry's state (the clause after its last
    /// ` — `); an unknown key as the key and its near miss (`windw_padding →
    /// window_padding?`, or the bare key), the line number behind Details;
    /// a skipped chord as `ctrl+x: no action foo`; the head of every other
    /// family's sentence at a clause seam.
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
    /// `config ` every banner line carried (the row's tag says it now), and
    /// the family word of a keybinding warning, whose subject is the chord —
    /// with its `skipping ` (the title already says skipped: `Keybinding
    /// skipped · skipping "ctrl+x"` said it twice, review 2026-09-23).
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

    /// One message per family that collected anything, in collection order.
    pub(crate) fn into_messages(self) -> Vec<Message> {
        self.families
            .into_iter()
            .filter(|(_, lines)| !lines.is_empty())
            .map(|(family, lines)| config_family_message(family, &lines))
            .collect()
    }
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

/// A font verdict's excerpt in a person's words (design ruling 306):
/// `no font named Nope` from `font_family_bold: "Nope" is not an admissible
/// font (not found)` — the validator's key, quoting and `admissible` stay
/// behind Details, where the sentence is kept whole. `None` for a sentence in
/// another shape.
fn font_excerpt(sentence: &str) -> Option<String> {
    let (head, _) = sentence.split_once(" is not an admissible font")?;
    let (_, quoted) = head.split_once('"')?;
    let name = quoted
        .rsplit_once('"')
        .map_or(quoted, |(name, _)| name)
        .trim();
    (!name.is_empty()).then(|| format!("no font named {name}"))
}

/// A TOML parser's diagnostic in a person's words for the band's excerpt
/// (design ruling 306): `line 3: missing =` where the parser names the line
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

/// R1/R4 — one family's message: the terse count title, the first
/// sentence's excerpt as `detail[0]`, every sentence whole behind it (the
/// excerpt not repeated when it IS the first sentence), and — when there are
/// more than a message holds — a last line saying how many more and where to
/// read them: a list that looks complete while it is not is worse than no
/// list. The restart-only family is a record that keeps its sentence as its
/// title when it has one.
fn config_family_message(family: ConfigFamily, sentences: &[String]) -> Message {
    let stripped: Vec<&str> = sentences.iter().map(|s| family.strip_prefix(s)).collect();
    let title = match (family, stripped.as_slice()) {
        (ConfigFamily::Restart, [only]) => restart_title(only),
        // Every unknown key has a near miss: they are misspellings (ruling 261).
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
    // How the row is logged once a clean reload resolves it (ruling 265):
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
    // The excerpt is the PAINTED line; a record is never painted, so there it
    // would only say the first sentence twice (audit 2026-09-24).
    if excerpt != first && family.hold() != Hold::LogOnly {
        msg = msg.line(excerpt);
        used += 1;
    }
    // Each sentence in its PHYSICAL rows ([`diagnostic_lines`], fffef97a1),
    // and the budget counted in rows, not sentences: one Trail Pack parse
    // error is a sentence and a four-row caret diagram, and counting it as one
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
    // `YourAction`, a flash at the warning's 45 s): asked AFTER the lines, so
    // the rows written above stay the detail and the painted excerpt.
    if record { msg } else { msg.now(family.ask()) }
}

/// A restart-only setting's record title (ruling 261): the setting in a
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
/// (`App::replace_config_messages_after_font_work`; round 44 and its review).
pub(crate) const KEY_FONT_FAMILY: &str = "config.font-family";

/// The `font_family` the backend build thread could not admit — a config
/// warning like the launch's font row, but said at the backend join rather
/// than with the launch's families, so it carries its OWN key: sharing
/// `config.fonts` would have it supersede the launch's font row and take
/// those sentences off the glass.
pub(crate) fn font_family_rejected(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()]).key(KEY_FONT_FAMILY)
}

/// The crash row's first line: plain words for what is there to look at —
/// never the report's path (design ruling 74). Not painted: the title says
/// what happened and `Open log` is the press (ruling 145).
pub(crate) const CRASH_EXCERPT: &str = "a crash report was saved";

/// R2 — the previous run died (ruling 145). The title says so in a few words
/// — `aterm crashed last time` — and the row paints it alone
/// ([`Message::no_excerpt`]): [`CRASH_EXCERPT`] is `detail[0]`, behind
/// `Details ›`; the artifact's ABSOLUTE path is the next line (`crash log at
/// <path>`: the band printing it home-abbreviated still showed
/// `crash-signal-<pid>-<nanos>.log.seen`, an internal file name, so it is
/// never on glass), then the file's head; `Open log` opens it as text.
pub(crate) fn crash_message(evidence: &crate::logging::CrashEvidence) -> Message {
    let path = evidence.path.display().to_string();
    // An Error wears the cross (ruling 302): the red triangle read as a
    // warning in an error's ink.
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

/// R2b — the installed app's previous run was KILLED: its crash marker was left
/// empty and unlocked, so the process ended with no fatal signal and no exit
/// path at all (Force Quit, SIGKILL, running out of memory, a system watchdog,
/// power loss). Before the marker lifecycle (`crash_signal::markers`) that
/// death read exactly like a clean quit. A WARNING, not an error: aterm did not
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

/// What the keeper's recovery row says of the reattached screens.
pub(crate) const KEEPER_SCREENS: &str = "each screen started blank and its program was asked to \
                                         redraw; the scrollback before the stop is not here";

/// THE PTY KEEPER'S RECOVERY ROW (P3, design §5.3 step 7): the last window
/// crashed or was killed, and the keeper kept `n` of its sessions running;
/// this launch reattached them. It takes the crash row's slot, and so carries
/// where that row's evidence is: the crash log, or the killed run's log.
/// `End sessions` hangs every reattached shell up (Force Quit and jetsam look
/// the same, decision 4); `refused` offers the admission turned down are
/// counted. Each screen lands blank and its program is sent a size pulse: a
/// program that ignores it stays blank until it next writes.
pub(crate) fn keeper_recovered_message(
    n: usize,
    refused: usize,
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
        .line(KEEPER_SCREENS);
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
/// agent that resumes its conversation (ruling 293): nothing else was lost.
pub(crate) const JOURNAL_ONLY_SCROLLBACK: &str = "only the scrollback did not survive";

/// [`JOURNAL_ONLY_SCROLLBACK`] when a pane also could not open its folder:
/// the scrollback was not all that was lost.
pub(crate) const JOURNAL_SCROLLBACK: &str = "the scrollback did not survive";

/// The reopened layout's quiet line (D16, ruling 282): every leaf sat at its
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

/// THE AGENTS THAT COME BACK, in a person's words (P6a, ruling 293): `aterm
/// starts Claude again on its conversation in tab 2`, `… its conversations in
/// tabs 1 and 3`, `… in 4 tabs`; `tab 2 of window 1` when the layout has more
/// than one window. `resumed` is [`crate::crash_journal::Reopened::resumed`]'s,
/// one entry per agent; empty for none. What aterm DOES, said as the launch
/// starts it — never a promise of how it ends (day six, D33: `Claude resumes
/// …` stood in the log beside the row that said it did not); a relaunch that
/// does not take is said by its own row ([`restored_agents_not_resumed`]).
fn journal_resumed_line(resumed: &[(u32, u32)], windows: usize) -> String {
    const SHOWN: usize = 3;
    let mut places: Vec<(u32, u32)> = Vec::new();
    for place in resumed {
        if !places.contains(place) {
            places.push(*place);
        }
    }
    let conversations = if resumed.len() == 1 {
        "its conversation"
    } else {
        "its conversations"
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
    format!("aterm starts Claude again on {conversations} in {place}")
}

/// What the reopened layout brought back, and from how many journals: `2 tabs
/// in 1 window restored in their folders from its crash journal`. The panes
/// whose folder is not here
/// ([`crate::crash_journal::Reopened::panes_without_folder`], audit #7
/// finding 48): those whose folder no shell could start in are counted,
/// whether the home folder stood in for them or not; with only panes a
/// program held (audit #8), the line claims no folders.
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

/// Where the reopened desk sits when the last quit's windows came back too
/// (round seven of the update audit, finding 48's review): those open first,
/// and the crashed desk's windows follow them. Nothing with no quit layout.
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
/// its name (day five, D19: `a program lost` said not which), else how many.
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

/// The tag a journal's entry files under (day five, D19): `crash` for a
/// crash, `session` (restore) for a kill — a kill is not aterm crashing.
fn journal_tag(class: crate::crash_journal::DeathClass) -> Tag {
    if class.crashed() {
        tags::CRASH
    } else {
        tags::SESSION
    }
}

/// P1 of the PTY keeper — the previous run ended uncleanly and its crash
/// journal (`crate::crash_journal`) reopened its layout: the windows, the tabs
/// and their folders came back, the programs and their scrollback did not. It
/// takes the crash/kill row's slot (`KEY_CRASH`, one per launch) and carries
/// what that row would have: the crash log's path and head with `Open log` on
/// it, or the killed run's log (`log`, `aterm.log` in the log dir, when no
/// crash-row evidence names one — a development start's kill is no row of its
/// own). An ERROR for a crash, a WARNING for a kill, like the rows it replaces.
///
/// A QUIET RELAUNCH (D16, ruling 282): the journal also says which leaves had
/// a program running ([`crate::crash_journal::Reopened::lost`]). When none
/// did, a kill lost nothing a person ran — every tab came back at its prompt,
/// in its folder when the restored line says so — so it is a RECORD (`Info`,
/// [`Hold::LogOnly`]) that says so, and the band stays quiet. A crash with
/// nothing lost keeps its row: aterm failed, and the crash log is the
/// person's to open or send; its words say nothing was running. When
/// programs ran, the row names how many tabs lost one and which programs; a
/// journal from before the field (`None`) reads as it always did.
///
/// THE ANSWER IS THE SENTENCE (day five, D19): `detail[0]` — the plain
/// sentence Settings ▸ Messages sets above the technical block — says what
/// was lost or that nothing was; how many tabs came back and how aterm ended
/// follow as details. The title names the one lost program by its name. A
/// kill files under `session`, a crash under `crash`.
///
/// AN AGENT THAT COMES BACK IS NOT LOST (P6a, ruling 293): `relaunching` is
/// whether this launch's supervisor host relaunches the agents the layout
/// carried ([`crate::harness_host::HostHandle::relaunches_restored`]). Each
/// one it brings back on its conversation
/// ([`crate::crash_journal::Reopened::resumed`]) is left out of what was
/// lost and said as coming back (`aterm starts Claude again on its
/// conversation in tab 2`): when nothing else ran, only scrollback was lost, and a kill is a
/// record; when another program was lost, the warning names that one and its
/// sentence says which agent resumes (ruling 314). If the relaunch then fails, the host says
/// so once ([`restored_agents_not_resumed`]).
///
/// A PANE WHOSE FOLDER NO SHELL CAN START IN did not come back in it (audit
/// #7 finding 48): `folders` counts them, and the restored line says how many
/// rather than that every tab came back in its folder
/// ([`journal_restored_line`]). A pane a program held whose folder is not
/// here is not counted, and the line then claims no folders (audit #8).
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
    // The places are the SCREEN's (finding 48's review): the last quit's
    // windows open first, so the reopened desk's window 0 is window
    // `quit_windows` on screen, and a window is named whenever the screen has
    // more than one.
    let offset = u32::try_from(quit_windows).unwrap_or(u32::MAX);
    let places: Vec<(u32, u32)> = reopened
        .resumed(relaunching)
        .into_iter()
        .map(|(window, tab)| (window.saturating_add(offset), tab))
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
    // ONLY THE SCROLLBACK holds while every pane came back in its folder.
    let (idle_line, scrollback_line) = if folders == PanesWithoutFolder::NONE {
        (JOURNAL_NOTHING_RAN, JOURNAL_ONLY_SCROLLBACK)
    } else {
        (JOURNAL_NOTHING_RAN_FOLDERLESS, JOURNAL_SCROLLBACK)
    };
    // ONE SENTENCE (ruling 314, day eight E2): the agent coming back rides
    // the loss sentence, never a detail of its own — Settings ▸ Messages sets
    // every detail after the first as technical text, and `aterm starts
    // Claude again …` stood there in monospace.
    let loss = match &lost {
        Some(lost) if lost.tabs == 0 && resumed.is_empty() => idle_line.to_string(),
        Some(lost) if lost.tabs == 0 => format!("{resumed}; {scrollback_line}"),
        None if resumed.is_empty() => JOURNAL_LOSS.to_string(),
        None => format!("{JOURNAL_LOSS}; {resumed}"),
        Some(lost) if resumed.is_empty() => journal_loss_line(lost, tabs),
        Some(lost) => format!("{}; {resumed}", journal_loss_line(lost, tabs)),
    };
    // The mark is the severity's (ruling 302): a crash's row the cross, a
    // kill's the triangle, the quiet relaunch's record `ℹ` — never the
    // working `↻`, which read as a restore still under way.
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

/// P1 of the PTY keeper — a crashed run's journal this launch took and did
/// NOT reopen: it could not be read (a torn file after a power loss, a newer
/// build's schema, a planted link), or the brake skipped it (the run that wrote
/// it had reopened a crash journal and crashed within 90 s, or was stopped
/// within them a second time in a row — ruling 285). Either way that layout did
/// not come back (an unreadable one is kept aside, and the row names where),
/// and a person who expected it back is owed the reason, what it held
/// and what ran in it (day five, D18: `Tab restore skipped after a crash` over
/// a kill, naming nothing), and `Open log` on `log` — the crash log when
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

/// The supersede key of the row that says this window supervises no agent
/// ([`harness_without_socket`]): one per launch.
pub(crate) const KEY_HARNESS_NO_SOCKET: &str = "harness.no-socket";

/// What a window with no control socket cannot do, the clause
/// [`harness_without_socket`] puts after every cause.
pub(crate) const HARNESS_BOXES_UNANSWERED: &str =
    "aterm cannot answer Claude Code's or Codex's permission boxes here";

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

/// The window's `[harness]` policy would supervise its Claude Code and Codex
/// sessions, but it has no control socket — the one interface the
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

/// A restored tab's relaunch that was typed and whose agent started but did
/// not pick its conversation up (day six, D31): it ended as it started
/// (Claude's `No conversation found`), never registered it, or was still
/// coming up when the host stopped waiting.
fn started_without_conversation(outcome: &aterm_agent::harness::relaunch::Outcome) -> bool {
    use aterm_agent::harness::relaunch::Outcome;
    matches!(outcome, Outcome::NotYet(step) if step == "failed:no-resume" || step == "wait:resume")
}

/// Why a restored tab's agent did not come back, in a person's words — never
/// the relaunch's step word, which `aterm.log` keeps. An agent that started
/// and did not pick its conversation up is said as that, never as a tab that
/// was not ready (day six, D31: the tab was at its prompt, and Claude started
/// there and refused).
fn not_resumed_why(outcome: &aterm_agent::harness::relaunch::Outcome) -> &'static str {
    use aterm_agent::harness::relaunch::Outcome;
    match outcome {
        o if started_without_conversation(o) => {
            "Claude started but did not pick its conversation up"
        }
        Outcome::NotYet(step) if step.starts_with("failed:relaunch:") => {
            "its prompt refused the line that starts it"
        }
        Outcome::Cannot(why) if why == "shell-gone" => "its tab's shell had ended",
        Outcome::Left(why) if why == "not-exited" => {
            "the Claude that ran there before is still running, outside the tab"
        }
        Outcome::Left(why) if why == "no-conversation" => "its conversation could not be found",
        Outcome::NotYet(_) | Outcome::Busy => "its tab was not ready for it in time",
        _ => "it could not be started there",
    }
}

/// P6a, RULING 293 — THE AGENTS THAT DID NOT COME BACK. The reopened layout's
/// row ([`journal_reopened_message`]) told the person that aterm starts Claude
/// again on its conversation in these tabs; the relaunch then could not
/// bring it back in each `(place, outcome)` of `missed` (`place` as `in tab
/// 2`). Said ONCE, by the supervisor host once it has tried every restored
/// tab (`HostHandle::relaunch_restored`): a warning — the person resumes it by
/// hand — with `Open log` on `log`, where each step is. THE REMEDY IS IN THE
/// SENTENCE (day six, D32): `detail[0]`, the plain sentence Settings sets
/// above the technical block, says what happened and what to type; the
/// per-tab reasons and the log's path follow.
pub(crate) fn restored_agents_not_resumed(
    missed: &[(String, aterm_agent::harness::relaunch::Outcome)],
    log: Option<&std::path::Path>,
) -> Message {
    let (title, first) = match missed {
        [(place, outcome)] => (
            format!("Couldn't resume Claude {place}"),
            if started_without_conversation(outcome) {
                "Claude started in the tab but did not pick its conversation up after aterm \
                 stopped: type claude --resume there to pick it up again"
                    .to_string()
            } else {
                format!(
                    "{}, so its conversation did not come back after aterm stopped: type claude \
                     --resume there to pick it up again",
                    not_resumed_why(outcome)
                )
            },
        ),
        _ => (
            format!("Couldn't resume Claude in {} tabs", missed.len()),
            "their conversations did not come back after aterm stopped: type claude --resume in \
             each tab to pick one up again"
                .to_string(),
        ),
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Warn, title)
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(first);
    if missed.len() > 1 {
        for (place, outcome) in missed {
            msg = msg.line(format!("{place}, {}", not_resumed_why(outcome)));
        }
    }
    if let Some(log) = log {
        let log = log.display().to_string();
        msg = msg
            .line(format!("what aterm tried is in {log}"))
            .action(Intent::OpenPath { path: log });
    }
    // Asked after the lines: the remedy sentence above stays `detail[0]`.
    let ask = Ask::flash(
        Why::Spent {
            tried: Reason::new("tried to start Claude again on its conversation"),
            reason: Reason::new("it did not pick the conversation up"),
        },
        Then::step(if missed.len() == 1 {
            "Type claude --resume in the tab"
        } else {
            "Type claude --resume in each tab"
        }),
        Span::Launch,
    );
    msg.no_excerpt().now(ask).key(KEY_RESTORED_AGENTS)
}

/// R3 — `aterm.toml` did not load at launch. `notice` is
/// `app_config::launch_config_notice`'s sentence (problem, path,
/// consequence): `aterm.toml could not be read at launch (…) — …` or
/// `aterm.toml is not a valid configuration (…) — …`. The title is terse
/// (ruling 146) and says the file is broken, the parser's own error's first
/// row (between ` (` and `) — `) is `detail[0]`, and the consequence and the
/// whole sentence follow behind Details — the sentence in its PHYSICAL rows
/// ([`diagnostic_lines`], fffef97a1).
///
/// The rows matter because the parenthesised error of an `Invalid` launch is
/// a `toml` parse error: a sentence, then a gutter, the source line and a
/// caret row. Handed to one `Message::line`, the engine's sanitizer DELETED
/// each `\n`, so the band, the Details page and the screen reader all read
/// `…column 11  |1 | font_px =   |           ^expected a value) — …` and the
/// 240-char cut landed mid-word in the consequence. That is the multi-line
/// fix (e7dc1feee) the retired banner made; [`config_lane_error`] kept it,
/// and this reads the same way — the caret rows open with whitespace, so the
/// Details page sets them in the monospace face with their columns intact.
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
    // An invalid file's excerpt is the parser's finding in a person's words
    // (ruling 306: `expected \`=\`` reached the glass); an unreadable one's
    // is the system's own few words (`permission denied`).
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
    // detail, and the ask's `Open aterm.toml` IS that capsule.
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

/// R5 — a key the CPU renderer cannot honour (`background_opacity`,
/// `background_material`): a DISCLOSURE, so a RECORD — the Manual already
/// marks the key inert while the CPU renderer is active — pointing at the
/// Appearance page where the renderer is chosen.
pub(crate) fn cpu_renderer_no_effect(key: &str, rest: &str) -> Message {
    // The setting in a person's words (ruling 261); the key and why follow.
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

/// R6 — the Windows client-area backdrop was declined (DirectComposition
/// unavailable, `hdr_glow` on, no GPU, not requested at launch): a RECORD for
/// the family — a blank window is the GPU-lost row's, not this one's.
#[cfg(any(windows, test))]
pub(crate) fn backdrop_declined(title: &str, detail: &str) -> Message {
    // One title in the failure grammar (ruling 261); the site's words follow.
    Message::new(tags::RENDER, Severity::Info, "Couldn't show the backdrop")
        .line(title)
        .line(detail)
        .action(Intent::OpenConfigEditor { line: None })
        .record()
        .key(KEY_BACKDROP)
}

/// R7 — the GPU was lost and the windows created for the backdrop cannot be
/// redrawn by the CPU fallback. The remedy is a gesture (`New window`), so
/// the row STANDS until a window created after the loss paints, or until it
/// is read; a timer must not fold the one instruction that recovers the
/// screen. The `New window` capsule IS that instruction, so the title and
/// the capsule are the row; why and the alternative ride behind Details
/// (review 2026-09-23).
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

/// R8 — the accessibility publisher's thread died. A process gets one
/// publisher, so nothing short of a new process brings it back; the retry is
/// `detail[0]`, alone on its line (the sanctioned anchor), the reason and the
/// consequence behind it. On the glass (Standing) only when assistive
/// technology was in use — `at_in_use`: an AT client had attached to this
/// process's tree ([`crate::a11y_backend`]'s latch) — and a RECORD
/// otherwise: for everyone not using a screen reader the row was an FYI
/// about a feature they do not use (review round 2, 2026-09-23).
/// Settings ▸ Messages and `messages.log` keep it either way.
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
/// reverted (`label not saved`, an error) or may not have persisted
/// (`label may not be saved`, a warning). `detail` is the cause. Keyed by the
/// config leaf, so a second failure on the same toggle replaces the first.
pub(crate) fn presence_not_saved(
    leaf: &str,
    label: &str,
    detail: &str,
    indeterminate: bool,
) -> Message {
    let (severity, title) = if indeterminate {
        (
            Severity::Warn,
            format!("Couldn't confirm {label} was saved"),
        )
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

/// R10 — the person's Hold press met a standing FLEET hold (`HOLD_DENIED`,
/// which the bridge lifts, not this window). Pressing Hold (`on`) asked for
/// what already is — the session is held — so that is a RECORD (`Session
/// already held`); pressing Release failed, a gesture failure the person sees
/// (`Hold not lifted`). Titled with the outcome, not a state (review
/// 2026-09-24).
pub(crate) fn fleet_hold(on: bool) -> Message {
    const WHY: &str = "the fleet holds it";
    if on {
        Message::new(tags::FABRIC, Severity::Info, "Session already held")
            .line(WHY)
            .record()
            .key(KEY_FABRIC_HOLD)
    } else {
        gesture_failure(
            tags::FABRIC,
            Severity::Warn,
            "Couldn't release the hold",
            WHY,
            Then::step("Lift the hold from the fleet"),
        )
        .key(KEY_FABRIC_HOLD)
    }
}

/// THE BRIDGE WAS LOST AND NOTHING WAS HALTED (the default since the owner's
/// direction of 2026-09-29, `fabric::bridge_lost`): `n` sessions the bridge
/// governed keep running, each with `status hold_advisory=fabric-lost` while
/// no newer bridge has attached. A warning the person may act on, never a
/// row: it names what did not happen and the switch that would have made it.
/// Keyed with the person's own hold messages, so a later one supersedes it.
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

/// R10 — the bridge refused the person's Hold (`on`) or Lift Hold press
/// (`ERR …`): the reason is `detail`.
pub(crate) fn hold_refused(on: bool, detail: &str) -> Message {
    let (title, step) = if on {
        ("Couldn't hold the session", "Press Hold again")
    } else {
        ("Couldn't lift the hold", "Press Lift Hold again")
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

/// R11 — a fabric menu item that could not do its thing (a document that
/// would not open, a thread that would not spawn): a terse title, the cause
/// and the path behind it, keyed by the verb.
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

/// R11 — `aterm fabric …` ended. A clean exit is a RECORD (the output tab
/// opens anyway, and says the rest); anything else is the person's command
/// failing — `title` (`Fabric On failed`) alone on the glass, the reason
/// (`exited 2`) behind Details: an exit code is nothing the person can act
/// on, and the output tab that opens is the pointer (review 2026-09-24).
pub(crate) fn fabric_status(verb: &str, title: &str, detail: &str, ok: bool) -> Message {
    let key = format!("fabric.{verb}");
    if ok {
        Message::new(tags::FABRIC, Severity::Success, title)
            .line(detail)
            .record()
            .key(&key)
    } else {
        gesture_failure(
            tags::FABRIC,
            Severity::Error,
            title,
            detail,
            Then::step("Read the command's output in its tab"),
        )
        .no_excerpt()
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
        // What overrode it, first — the capsule is `Open aterm.toml` (review
        // 2026-09-24: the bare title did not say).
        lines.insert(0, "aterm.toml sets it".to_string());
    }
    // The ask's button IS the row's one capsule, `Open aterm.toml`.
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

/// The native config lane's heads, as terse titles (design §10.3, C22's
/// map), and whether the head is the person's own gesture failing (Robi's
/// dismissal). An unknown head is a failed settings change, its whole first
/// row the excerpt.
fn lane_title(head: &str) -> (&'static str, bool, bool) {
    let known = |title, gesture| (title, gesture, true);
    if head.starts_with("Config observation was not valid TOML") {
        known("Couldn't read your settings", false)
    } else if head.starts_with("Robi was not dismissed") {
        known("Couldn't dismiss Robi", true)
    } else if head.starts_with("Config reconciliation failed") {
        known("Couldn't save settings changes", false)
    } else if head.starts_with("Manual saved aterm.toml, but")
        || head.starts_with("Config was saved, but its exact disk generation")
    {
        known("Couldn't apply saved settings", false)
    } else if head.starts_with("Config publication could not be verified") {
        known("Couldn't confirm settings saved", false)
    } else if head.starts_with("Config was NOT saved") {
        known("Couldn't save a settings change", false)
    } else {
        ("Couldn't change a setting", false, false)
    }
}

/// R13 — the native config lane's own error (`Config observation was not
/// valid TOML`, a pump that failed, a Robi dismissal the lane refused): a
/// terse title by its head ([`lane_title`]), the cause's first physical row
/// as `detail[0]`, and every later row — a `toml` parse error's caret
/// diagram — behind Details.
///
/// A DIAGNOSTIC IS ONE PROBLEM, NOT ONE LINE (upstream e7dc1feee, ported onto
/// the message model on the origin/main merge, 2026-09-23): the words are
/// split ONCE into their physical rows ([`diagnostic_lines`]), so the band's
/// excerpt, the Settings ▸ Messages entry and the screen reader's rows are
/// the same rows by construction, and the whole diagnostic stays ONE message.
pub(crate) fn config_lane_error(words: &str) -> Message {
    let mut rows = diagnostic_lines(words).into_iter();
    let first = rows.next().unwrap_or_default();
    let (head, cause) = split_sentence(&first);
    let (title, gesture, known) = lane_title(head);
    // What the band paints (ruling 306): the parser's finding in a person's
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
    // What a failed reconciliation LOST is what the person does next about
    // (fffef97a1, ruling 146): `2 queued changes were not written` leads, the
    // cause behind it. The head says it only when requests were discarded.
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
    // The person's own edit did not take: a flash at the warning's 45 s, or
    // the gesture's 12 s for Robi's dismissal (H10/H11), asked after the
    // lines so they stay the detail. The reason is a fixed clause per head,
    // never the cause's words (`reload before retrying` is no reason).
    let why = match title {
        "Couldn't read your settings" => "aterm.toml is not valid TOML",
        "Couldn't dismiss Robi" => "Robi's dismissal was not saved",
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
        if gesture { Span::Gesture } else { Span::Warn },
    );
    msg.now(ask)
}

/// R14 — ONE SHAPE FOR "THE PERSON'S OWN GESTURE FAILED" (H11): a failure of
/// their own action earns the glass for a SHORT hold (`HOLD_GESTURE`, 12 s)
/// and nothing more — a title of a few words, the FULL error behind Details
/// (no 160-character cut: the log answers the investigator). Unkeyed unless
/// the caller keys it: a second failed press is a second row's worth of news,
/// folded as a duplicate when the words are the same.
///
/// The class is the door's TRANSIENT ASK: what the person just did did not
/// work (`Why::YourAction`, the title), `then` the one thing to do about it
/// (a step in words, or a button the row already carries), held for
/// [`Span::Gesture`] — the same 12 s. It is asked after the detail is
/// written, so the rows stay the detail and the painted excerpt.
fn gesture_failure(tag: Tag, severity: Severity, title: &str, detail: &str, then: Then) -> Message {
    // `sentence`, never `line`: the error is someone else's words, kept WHOLE
    // across detail lines (design ruling 64), not clipped at the line cap.
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

/// R14 — a split the pane cannot hold was refused BEFORE anything spawned.
/// `reason` is the refusal's sentence (`Split refused: this pane is 20x5
/// cells; a vertical split needs at least 20x7`): the two sizes are what the
/// person acts on, so `detail[0]` is `pane 20×5, needs 20×7` and the sentence
/// rides whole behind it (review round 2, 2026-09-23: the excerpt read as a
/// sentence cut mid-thought). A refusal in another shape is its own excerpt.
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

/// R14 — a split that fit could not spawn its pane.
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

/// R14 — Configure Connection… or Disconnect Session… on a tab connected to
/// no session: nothing opened, so the person hears why. Configure could not
/// do what was asked (a failure whose one step is to connect); Disconnect
/// asked for what already is, so it is an answer that the tab has none.
pub(crate) fn no_connection(configure: bool) -> Message {
    if configure {
        gesture_failure(
            tags::SESSION,
            Severity::Error,
            "Couldn't configure a connection",
            NO_CONNECTION,
            Then::step("Connect to Session\u{2026}"),
        )
    } else {
        Message::new(tags::SESSION, Severity::Info, "This tab has no connection")
            .notice(NoticeKind::Answer)
    }
}

/// The detail of [`no_connection`]'s Configure row.
pub(crate) const NO_CONNECTION: &str = "this tab has no connection";

/// A split refused or failed: one title in the failure grammar (ruling 261).
const SPLIT_FAILED: &str = "Couldn't split the pane";

/// R14 — keystrokes typed while a seamless update finished overflowed the
/// pre-Commit queue and were dropped: the person typed them, so they hear it
/// — and `detail[0]` says what to do, as an instruction, not a clause.
#[cfg(any(unix, test))]
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

/// R14 — the person's own key or paste did not go: the session's input
/// queue was full (the program is not reading) or its writer was gone
/// (ruling 270; `App::refuse_input`). A gesture that failed, so the one
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

/// The supersede key of the ONE row for the tabs that lost their shell
/// integration (`integration_notice`): one row per condition, never one per
/// tab (ruling 374's shape, the owner's "stacking" of 2026-09-28). A tab's
/// own RECORD (its key waits at its next prompt) is keyed
/// `session.integration.<sid>` beneath it.
pub(crate) const KEY_INTEGRATION_LOST: &str = "session.integration";

/// The declared FIX WORDS of the lost-integration row (rulings 265, 367):
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

/// One tab the lost-integration row names ([`integration_lost`]): its PLACE
/// in the person's words (`in tab 2`, `in window 2, tab 1`:
/// `App::upgrade_place`, [`tab_place`] — never the raw `s-<hex>`, rulings
/// 270 and 300), its session id (the technical line only), and what gives
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

/// TABS LOST THEIR SHELL INTEGRATION (2026-09-26, `integration_notice`; the
/// row's shape reworked 2026-09-29 to main's messages rulings): an adopted
/// shell (a seamless update or a keeper crash recovery) came without its key,
/// so command blocks and exit codes are dark there — until then said only by `status integration=degraded`
/// and the log. ONE Standing WARN under [`KEY_INTEGRATION_LOST`] for every
/// such tab, in the shape of an agent's one row for its stalled tabs
/// ([`agent_upgrade_stalled_group`], ruling 374): the title counts them, the
/// first line names them by place (past [`INTEGRATION_TABS_NAMED`], an
/// `and N more` line counts the rest), the next says what gives it back, and
/// behind them, only where the remedies differ, the agent tabs by place
/// (one shared remedy is said once), then the technical line with the
/// session ids ([`integration_lost_sessions`]). The window RESTATES it in
/// place as tabs heal, close or change remedy, resolves it `Ok` under
/// [`INTEGRATION_BACK`] only when the last tab healed, and withdraws it when
/// the last one closed. `None` for no tab. No line asks for a restart (the
/// reporters' guard): the agent's move is the harness's act, and a new tab
/// is not a restart.
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

/// The most folders [`folder_row`] names one per line; the rest are counted
/// on the line after them.
const FOLDERS_NAMED: usize = DETAIL_LINES_CAP - 2;

/// The one-folder row's line, before the folder.
const FOLDER_INSTEAD_OF: &str = "opened in your home folder instead of ";

/// The many-folder row's first line, before the folders.
const FOLDERS_INSTEAD: &str = "opened in your home folder instead";

/// The supersede key of `fault`'s row.
pub(crate) const fn folder_row_key(fault: crate::spawn_folder::Fault) -> &'static str {
    match fault {
        crate::spawn_folder::Fault::Missing => KEY_FOLDER_NOT_FOUND,
        crate::spawn_folder::Fault::Shut => KEY_FOLDER_NOT_OPENED,
    }
}

/// Fresh shells started in the home folder because the folder each was asked
/// to start in was not there, or could not be entered — a restored pane's
/// saved folder, the one a New Tab inherited, a profile's (audit #7 finding
/// 48; [`crate::spawn_folder`]). ONE row per fault for every such folder while
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
    };
    let total = named.dirs.len() + named.unnamed;
    let msg = match named.dirs.as_slice() {
        [one] if total == 1 => Message::new(
            tags::WINDOW,
            Severity::Warn,
            format!("Couldn't {verb} the folder"),
        )
        .sentence(format!("{FOLDER_INSTEAD_OF}{one}")),
        many => {
            let unnamed = total.saturating_sub(many.len().min(FOLDERS_NAMED));
            let msg = Message::new(
                tags::WINDOW,
                Severity::Warn,
                format!("Couldn't {verb} {total} folders"),
            )
            .line(FOLDERS_INSTEAD)
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
    if msg.detail.first().map(String::as_str) == Some(FOLDERS_INSTEAD) {
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
    let one = if head == FOLDER_INSTEAD_OF.trim_end() && !rest.is_empty() {
        rest.concat()
    } else {
        let first = head.strip_prefix(FOLDER_INSTEAD_OF)?;
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

/// R15 — THE FULL DISK ACCESS NEED, AS AN ENTRY OF THE DIGEST (owner rule,
/// 2026-09-28: an alert persists only where the owner can act, says why and
/// what, and a thing that can wait is one dismissable line, filed in the log).
/// The card ([`crate::consent_card`]) decides WHEN; every word here is one of
/// its consts — `FDA_WHAT`, `FDA_WHY`, `FDA_THEN`, `FDA_GRANTS`, `FDA_AFTER`,
/// `FDA_DETAIL` — imported, never retyped, so the entry and the card cannot say
/// two things.
///
/// The button is `Open Settings` ([`Mover::open_system_pane`]); a press files
/// the entry `Answered` (the owner did the one thing it asks — the card's
/// `OwnerActed` hold keeps a fresh need quiet meanwhile) and only a VERIFIED
/// grant ends it (`App::resolve_fda_need`). The entry has no `Not now`: `Dismiss
/// all` is the decline and files it for 30 days, then 90 ([`Resurface::CHOICE`]),
/// and an owner who pressed the old row's `Not now` never sees one (the card's
/// `decide_need` answers `Answered` from the marker). The `cause` word is the
/// only thing that makes the fact NEW after a dismissal: `needed` (macOS
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
        // What the log says once it is answered or folds (ruling 259): the
        // question was ASKED — the grant or the decline is its own record.
        .finished_as(ASKED_FILE_ACCESS)
}

/// The Full Disk Access question's log title once it leaves the glass.
#[cfg(test)]
pub(crate) const ASKED_FILE_ACCESS: &str = "Asked for Full Disk Access";

/// R17 — the probe observed the grant. A confirmation, so a RECORD
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

/// DEBUGGER ACCESS RECORDS (design §3.5, §3.8, in the R17 pattern): what a
/// probe OBSERVED about macOS's debugger rule, never what an exit code said. A
/// change is the owner's own act, so a RECORD (Settings ▸ Messages and
/// `messages.log`), never a row. Each "on" record carries the one-line undo,
/// as every surface that mentions the change does; the words stop at the
/// observation — "Should stop…" copy lives on the Security row, and nothing
/// here says what developer mode silences until spike D5 is run. "Was on; it
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

/// A HARNESS NOTE (`aterm ctl appnotice harness <text>`: an agent harness's acts,
/// posted from outside the window — nothing in aterm posts one since the second
/// harness stack and its `APPNOTICE_LANE` were deleted, 2026-09-23): a
/// `harness`-tagged RECORD, never a
/// row — design ruling 39, implemented (ruling 61). The words are the wire's,
/// sanitized and capped like any appnotice text.
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

/// A SESSION SUPERVISOR'S INFORMATION (`aterm_agent::supervise::IdleHost::
/// inform`, from the window's own supervise loop for session `sid`): a
/// `harness`-tagged RECORD, never a row — nothing is asked of the person.
/// Codex's save-then-wait hold begins with one (2026-09-28: a hold can last
/// days, the session idle through it). Keyed per session: the next one
/// replaces it. The text's FIRST LINE is the title and each further line a
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

/// A SESSION SUPERVISOR'S WARNING (`aterm_agent::supervise::IdleHost::warn`,
/// from the window's own supervise loop for session `sid`): a warning ROW on
/// the band — something the person may want to act on, nothing asked of
/// them, nothing held back. The memory ceiling says one an episode (owner
/// direction 2026-09-29: past the ceiling is a warning, not a restart), and
/// again whenever what it says comes next changes (the Mac falling into
/// trouble or recovering, another agent now the larger): its text names the
/// footprint, the Mac's state and the tab's own `claude --resume <id>`, kept
/// whole in the detail. Keyed per session: the next one replaces it, so the
/// row never says `a warning only` while aterm restarts the agent; it leaves
/// on the severity's hold.
pub(crate) fn supervisor_warning(sid: &str, place: &str, text: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        "Agent memory past its ceiling",
    )
    .sentence(atpkg::progress::sanitize_for_tty(
        &tab_led(place, text),
        400,
    ))
    .key(format!("harness.memory.{sid}").as_str())
}

/// The live agent upgrade's supersede key: each new waiting record replaces
/// the last one recorded under it.
pub(crate) const KEY_AGENT_UPGRADE: &str = "harness.upgrade";

/// THE LIVE AGENT UPGRADE, WAITING (gap audit 2026-09-24: two sessions sat one
/// and two releases behind for 8h22m and nothing said so). A RECORD, never a
/// row — waiting for its moment is the upgrade working, and the band carries
/// only what the person acts on (ruling e83d7d233) — so it is on Settings ▸
/// Messages, `messages.log` and `appstatus` as `kind=harness`. `product` is
/// the agent the waiting sessions run (`Claude Code`, `Codex`; `Claude Code
/// and Codex` for a mix, named in the detail under the generic title),
/// `waiting` the versions they move to, one per session; `None` when none
/// waits. ONE session's record is [`agent_upgrade_waiting_in`]'s.
///
/// IT SAYS THERE IS NOTHING TO DO (the owner, 2026-09-28: "do I need to do
/// something? It's not clear. I want upgrades to be applied automatically"),
/// and never "at its next turn end" — false of a goal-mode Codex, whose next
/// turn begins within 14 ms of the last: the upgrade starts at a quiet
/// moment, the longer it waits the less it asks of one (the ladder).
pub(crate) fn agent_upgrade_waiting(product: &str, waiting: &[&str]) -> Option<Message> {
    let first = *waiting.first()?;
    let n = waiting.len();
    // A MIX of products (`Claude Code and Codex`) is named in the detail:
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

/// ONE SESSION'S UPGRADE, WAITING (the owner's decision of 2026-09-28): a
/// RECORD titled by its tab — `Codex 0.158.0 installs itself in tab 1` —
/// whose sentence is WORDED BY WHAT HOLDS IT
/// ([`aterm_agent::harness::upgrade_drive::Row::waiting_line`]): where only
/// the ladder's comfort does, that there is nothing to do and when the
/// ladder stops waiting for it — `nothing to do: it starts on its own at a
/// quiet moment in the tab, and from 2:15 PM as soon as it is idle and nobody
/// has typed there for 20 seconds` (its Land rung,
/// [`aterm_agent::harness::upgrade_drive::Row::lands_by`], on the person's
/// clock; `from two hours behind` where the local clock's offset is
/// unknown); where a floor stands — a draft, a dialog, a goal or a turn of
/// this tab's in its Codex daemon, a turn in another session there, the
/// shell integration — what it is and when it moves, never the pause in the
/// person's typing (the review of 2026-09-28: it promised that of a goal
/// that held the tab). A clock time, not a countdown: said once, it stays
/// true. Then the tab and the move. No tab mark and no warning: the upgrade
/// is working (rulings 275 and 283, amended by ruling 380). `place` names the
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

/// THE LIVE AGENT UPGRADE, STALLED: a ROW, because only the person can move it
/// — it was refused, stopped the same way round after round, runs where
/// typing into the tab cannot reach, waits on what no rung of the ladder
/// passes (`blocked:*`), or has stood four hours on the ladder's last rung
/// with no pause (`overdue`, six hours behind:
/// [`aterm_agent::harness::upgrade_drive::Row::stall`]). One that will ask
/// again on its own — still asking, or resting after a round that gave up or
/// stopped once — is a RECORD with no tab mark
/// ([`aterm_agent::harness::upgrade_drive::Row::asks_on_its_own`], rulings
/// 275 and 283), and names the clock time its next round starts, so its
/// words never need restating.
/// `Standing`: it stays until the stall ends (the host resolves it) or the
/// person reads it. `detail[0]` is why; then what moves it; the tab and the
/// move ([`STALL_MOVE_LINE`]); what runs under the agent and holds it, when
/// anything does (`held by pid …`: pid, name and age, never a command); and
/// the same words spelled for a shell, always the last line (ruling 270) —
/// any shell, never the stalled tab's own (its foreground is Claude, and a
/// line typed there is a prompt). Keyed per tab ([`agent_upgrade_key`]).
///
/// THE REMEDY IS THE ONE THAT WORKS FOR THIS KIND OF STALL (review of
/// 2026-09-25: it named `--now` for every kind, and `--now` moves only two).
/// Overdue: it has stood on the ladder's last rung for hours, which `--now`
/// is too (ruling 380), so what it waits on is named — a turn running (a
/// goal that never pauses), the READY answer, a draft, a box, a hold, work
/// under the agent — and `--now` is offered only where it still moves it
/// (its own text left typed, a full queue). Blocked (`blocked:*`): no rung
/// passes it — quit it in its tab and resume it there. Gave up: `--now` asks it
/// again (at once, where its own next round is at a clock time). Held back in
/// a pane: typing into the tab cannot reach it, so no word moves it — quit it
/// in its pane and resume it there, or `--skip`. Refused or failed: the
/// harness asks it again after a rest (at a clock time, once known) — to move
/// it sooner, quit and resume it by hand, or `--skip`; stopped after the
/// agent it ended was seen gone, nothing runs to quit — `codex resume`, or
/// `claude --resume <conversation>`, in the tab takes it back. Under way and
/// not moving (`stuck:*`, 2026-09-27): no word moves it and nothing is
/// forced — the row offers none and names `--status`. (None says "restart":
/// the reporters' restart guard,
/// `no_reporter_wording_prompts_a_restart`, reads every line.)
pub(crate) fn agent_upgrade_stalled(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
    place: &str,
) -> Message {
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    // A CODEX GOAL THE MOVE PAUSED AND HAS NOT RESUMED (the owner's decision
    // of 2026-09-28: aterm pauses a goal only for a moment, and never leaves
    // it paused): a row of its own, whatever became of the move, with the one
    // hand step. No word of the upgrade's moves it. Worded as the wait it is
    // — aterm still resumes it where it can — never as a failure, and never
    // raised while aterm itself holds it as it should (`Row::with_goal`: a
    // switch open, the paused goal's last turn running; the goal-pause review
    // of 2026-09-28). One LEFT PAUSED ON PURPOSE — its thread fell into a
    // sandbox — names the relaunch that brings it out.
    let goal_row = row.stall(now);
    if matches!(goal_row.as_deref(), Some("goal-paused" | "goal-sandboxed")) {
        let tab = clean(&row.tab);
        let (title, why, step) = if goal_row.as_deref() == Some("goal-sandboxed") {
            (
                "Codex goal left paused",
                "aterm paused this tab's Codex goal to install the new Codex, and leaves it \
                 paused: its thread fell into a sandbox, where it can neither commit nor push",
                "quit Codex in the tab, resume the thread with its launch flags (codex resume \
                 --dangerously-bypass-approvals-and-sandbox <thread>), then type /goal resume",
            )
        } else {
            (
                "Codex goal still paused",
                "aterm paused this tab's Codex goal for a moment to install the new Codex, and \
                 has not been able to resume it yet",
                "type /goal resume in the tab to go on with the goal now",
            )
        };
        let (reason, hand) = if goal_row.as_deref() == Some("goal-sandboxed") {
            (
                "its thread fell into a sandbox, where it cannot commit or push",
                "Quit Codex in the tab and resume the thread with its launch flags",
            )
        } else {
            (
                "it could not resume the goal",
                "Type /goal resume in the tab",
            )
        };
        return Message::new(
            tags::HARNESS,
            Severity::Warn,
            placed_title(title, place, ""),
        )
        .line(why)
        .line(step)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .line(String::new())
        .line(format!(
            "the same in any shell: `aterm harness upgrade {tab} --status`"
        ))
        .now(Ask::standing(
            StandingWhy::Spent {
                tried: Reason::new("paused this tab's Codex goal to install the new Codex"),
                reason: Reason::new(reason),
            },
            Then::step(hand),
        ))
        .key(&agent_upgrade_key(&row.tab));
    }
    // THE ONE ROW THAT STANDS (owner rules, 2026-09-28: "there needs to be
    // some clear reason why the user must be engaged and what the user should
    // do … don't continue to persist alerts to the user that are not
    // actionable"): the agent the upgrade ended and did not bring back, or a
    // shutdown it asked for that hung. Every other upgrade wait is a RECORD —
    // the log, and after a day the digest ([`Row::attention`]).
    let interrupt = row.interrupt(now);
    // The band's excerpt: a person's words, no code quoting (ruling 270;
    // `` `/exit` ended it `` read as a command to run).
    let why = row
        .stall_words(now)
        .unwrap_or_else(|| "it is not moving".to_string())
        .replace('`', "");
    let tab = clean(&row.tab);
    let from = clean(&row.from);
    let cmd = format!("aterm harness upgrade {tab}");
    // THE REMEDY NAMES THE ROW'S OWN BUTTONS (ruling 270): since gap #21 the
    // row carries the words that move it, so the shell spelling of the same
    // words is the last, technical line — never the first way offered.
    // ONLY A BUTTON THE ENTRY HAS (day five, D21: `Skip version keeps it` on
    // an entry whose capsules were `Upgrade now` and `Not today`): the row's
    // own capsule by its label, else the tab menu's item by the menu's own
    // label (`Skip This Version`), which offers every word.
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
        // NOTHING HAS CHECKED IT (design record 2026-09-28, C10;
        // `Row::unwatched`): the host's watch has not read the record for an
        // hour past its due look, the Mac awake well past its wake grace —
        // aterm's own failure, which no word of the owner's mends. The first
        // line says whose it is, so this one offers only the skip, the one
        // act that is the owner's (never "report it": aterm has nowhere to
        // report to); the shell line names `--status`, which says when it
        // was last looked at, and by which build.
        Some(Remedy::Waits) if row.stall(now).as_deref() == Some("unwatched") => {
            (skip.clone(), "--status` or `--skip")
        }
        // A move under way that has not moved (S2 of the in-flight review,
        // 2026-09-27): the harness refuses every word while it is under way,
        // and forces nothing — the row offers no word, and names the shell's
        // view of it.
        Some(Remedy::Waits) if under_way => (
            "no word moves it while it is under way: it goes on once what it waits on ends, \
             and nothing is forced"
                .to_string(),
            "--status",
        ),
        // ITS QUESTION WAITS UNREAD BEHIND A FULL QUEUE (review of 2026-09-27:
        // this row said `Upgrade now` could not move it, and that it moves
        // "once that ends", of a limit over by every word): the agent's next
        // run takes the question, and the owner's word types it once more; so
        // does the upgrade itself once the queue has rested, a rest that grows
        // with every copy unread, up to a day (`upgrade::queue_rest`), so no
        // one interval is promised (ruling 307).
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
        // A ROUND RE-ARMED AFTER A GIVE-UP, ITS OWN WORK HOLDING IT (ruling
        // 364): a record, and `Upgrade now` moved nothing sooner there — the
        // agent had answered every notice "not yet, my work still runs". Its
        // words promised it anyway, and offered the button (round 35, D5).
        Some(Remedy::Now) if own_work_round(row, now) => (
            format!(
                "{now_word} cannot move it past its own work: it moves once that ends or the \
                 agent stops it; {skip}"
            ),
            "--skip",
        ),
        // Its own text left typed, backing off (the only other wait `--now`
        // still moves once the ladder stands at its last rung): tried again
        // at once. Never "at its next turn end" (the owner, 2026-09-28: a
        // goal-mode Codex's turns never end).
        Some(Remedy::Now) | None => (
            format!("{now_word} tries it again at its next pause; {skip}"),
            "--now` or `--skip",
        ),
        // A TURN OF THIS TAB'S OWN running in its Codex daemon, hours past
        // the ladder's last rung: a goal that never pauses (2026-09-28).
        // Nothing ends that turn for the upgrade. Only where the kernel named
        // the conversation as this tab's (`goal`, `daemon-turn`: its root's
        // turn or a subagent's it spawned) is the owner told to pause
        // anything here — never for a turn aterm could not place
        // (`daemon-busy`: another tab's goal, or a subagent of this tab's own
        // it could not name; the reviews of 2026-09-28).
        // A goal still running hours past the Land rung. Under `upgrade_ask
        // = true` (the ask flow: not `Row::quiet`) aterm pauses it there for
        // a moment, so something kept the pause off (an open save-then-wait
        // switch, a rest after a move that did not come, a person's hand);
        // under the default quiet flow no pause is made, and the goal holds
        // the move for as long as it runs.
        Some(Remedy::Waits) if row.wait == "goal" || row.wait.starts_with("goal:") => (
            if !row.quiet {
                format!(
                    "aterm pauses the goal for a moment to move it once nothing else holds the \
                     tab, and resumes it after; pausing it in this tab moves it now; {skip}"
                )
            } else {
                format!("it moves once the goal stops: `/goal pause` in this tab stops it; {skip}")
            },
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == "daemon-turn" => (
            format!("it moves as soon as this tab's turn is over; {skip}"),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == "daemon-busy" => (
            format!(
                "it moves once that turn is over, or once aterm can tell it runs in another tab \
                 (`codex agents` lists the sessions); {skip}"
            ),
            "--skip",
        ),
        // The agent's own work is never ended for it. The move comes once that
        // work ends, or once the agent stops it. After a notice, the agent is
        // asked again once half an hour has passed — at a pause of its own, a
        // break or an idle point, never between them: "every 30 minutes" was
        // said of a session a stop hook kept from pausing for seven hours
        // (2026-09-27, s-d3346: notices at 13:13 and 20:14), and of tab #1's
        // 28 silent hours (2026-09-28). Four notices end in a give-up, a stall
        // of its own. When it was last asked is the record's
        // (`Row::noticed_at`), said as a clock time, and the last notice says
        // the give-up comes next ([`asked_again_words`]). The line stays under
        // `DETAIL_LINE_CAP`, which clips. UNDER THE QUIET FLOW (`Row::quiet`,
        // the review of 2026-09-30) nothing asks an announced round again and
        // nothing gives it up, so neither is promised.
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
            // No raw wait word (round 18, day four, D3: `past its own work
            // (background)`): the shell line below names the command.
            (
                format!(
                    "{now_word} cannot move it past its own work: it moves once that ends or \
                     the agent stops it{asked}; {skip}"
                ),
                "--skip",
            )
        }
        // ATERM'S OWN FENCE FAILS UNDER THE OWNER'S `--now` (the review of
        // 2026-09-28): the word is in force and asked for the very notice
        // aterm cannot type — the why line above says so, and how often — so
        // the row offers no `Upgrade now` (pressed, it put the same row back
        // at once), says aterm tries again at the next quiet moment (as the
        // tab's waiting record says it; never "at its turn ends": a
        // goal-mode Codex's turns never end), and names what the owner can
        // still do.
        Some(Remedy::Waits) if row.fence_fails_under_now() => (
            format!(
                "{now_word} is already in force: aterm tries its notice again at the next quiet \
                 moment; {skip}"
            ),
            "--skip",
        ),
        // A USAGE LIMIT WHOSE RESET HAS PASSED ([`Row::limit_passed`], the
        // review of 2026-09-29): the wait it names is over, so "it moves once
        // that ends" would tell the owner to wait for what already ended.
        // The first line already says the reset passed. Where the reset was
        // named, the quiet upgrade moves the tab at the point its loop offers
        // then (C9), so a row that stands past that point is one it could
        // not move: no reset named, or the ask flow, whose notice holds every
        // point. The one act that moves it is a person continuing the session
        // there (design record 2026-09-28 §5).
        Some(Remedy::Waits) if row.limit_passed(now) => (
            format!(
                "type in its tab to continue the session, and it moves at its next pause; {skip}"
            ),
            "--skip",
        ),
        // A SPEND OR CREDIT WALL naming no reset (`Row::spend_held`, the
        // review of 2026-09-30): no usage window, so "it moves once that
        // ends" would wait on what never ends by itself. The owner's hold:
        // named, with what lifts it.
        Some(Remedy::Waits) if row.spend_held() => (
            format!("add credits or raise the cap, and it moves at the next pause; {skip}"),
            "--skip",
        ),
        // NOTHING HOLDS IT NOW (2026-09-29, `upgrade::NEXT_POINT`): the last
        // look found its next step due at a point nothing holds. There is no
        // wait for a word to move it past, nor one to end: "it moves once that
        // ends" named a wait that is not there. The first line already says
        // when it goes (a grouped band prints only that line per tab), so
        // this one offers only the skip.
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade::NEXT_POINT => {
            (skip.clone(), "--skip")
        }
        // ITS AGENT NOT IN FRONT IN ITS TAB, OR THE TAB UNDER ANOTHER
        // SUPERVISOR'S CLAIM (`Row::aside`, the review of 2026-09-30): no
        // worker of aterm's runs there, so none of its steps or looks comes;
        // what moves it is the hand that holds it.
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade_drive::NOT_FOREGROUND => (
            format!("it moves once it is back in front in its tab (fg there); {skip}"),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade_drive::CLAIMED => (
            format!("it moves once the supervisor holding its tab lets it go; {skip}"),
            "--skip",
        ),
        // A QUIET ROUND ITS AGENT'S OWN WORK GAVE UP ON (`Row::rests_on_own_work`,
        // round eight, #4's review): resting, its wait reads `failed`, but
        // what holds it is that work — `Upgrade now` re-arms a round the
        // quiet restart's gate holds on it all the same. Worded as the wait on
        // its own work is, the line below naming what runs under the agent;
        // it said "Upgrade now moves it at its next pause" and offered the
        // button, which moved nothing.
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
        // Under the QUIET flow the round after the rest asks nothing: it
        // moves the session at a quiet moment (round eight, #4). Only a
        // give-up a status that lags alone held reaches here — the owner's
        // `--now` reads the lag past (`upgrade::quiet_step`); one its own
        // work held is a wait (the arm above).
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
        // Where it runs, and what moves it, once (day five, D23: the why and
        // the remedy said the same thing twice, and the remedy did not
        // parse). The pane is named by what holds it; which pane of it is
        // not known to aterm — the agent's terminal is that program's.
        Some(Remedy::InItsPane) => (
            format!(
                "to move it, quit it in its {} and resume it there; {skip}",
                pane_words(row.wait.strip_prefix("terminal:").unwrap_or_default())
            ),
            "--skip",
        ),
        // Never `the harness` (day five, D24): the upgrade asks again, at
        // the clock time its next round starts — the repeating stop's too
        // (D25), or now while its re-armed round is asking.
        // A WAIT NO RUNG PASSES AND NO PAUSE ENDS (`blocked:*`, the owner's
        // decision of 2026-09-28: a warning only where something is really
        // broken): the one way it moves.
        Some(Remedy::ByHand)
            if row.stall(now).as_deref() == Some("blocked:no-shell-integration") =>
        {
            (
                format!(
                    "to move it now, quit Codex in its tab and resume it with the `codex resume` \
                     line it prints; {skip}"
                ),
                "--skip",
            )
        }
        Some(Remedy::ByHand)
            if row
                .stall(now)
                .is_some_and(|stall| stall.starts_with("blocked:")) =>
        {
            (
                format!("to move it now, quit it in its tab and resume it there; {skip}"),
                "--skip",
            )
        }
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
            format!(
                "it no longer runs in the tab and aterm will not bring it back: `{}` there \
                 takes its conversation back, or {} keeps this row down",
                match row.agent {
                    aterm_agent::harness::upgrade::Agent::Codex => "codex resume".to_string(),
                    // A Claude Code move stopped after its SIGTERM (S1 of the
                    // in-flight review, 2026-09-27): its conversation, by id.
                    aterm_agent::harness::upgrade::Agent::Claude if !row.session.is_empty() => {
                        format!("claude --resume {}", clean(&row.session))
                    }
                    aterm_agent::harness::upgrade::Agent::Claude => "claude --resume".to_string(),
                },
                UpgradeWord::Skip.label()
            ),
            "--skip",
        ),
    };
    // THE TITLE SAYS WHICH TAB AND WHETHER IT FAILED (round 18, day four,
    // D2/D4): two stalls read `Couldn't upgrade Claude` twice — the same
    // words for a move waiting on a turn end and one waiting on its own
    // work, neither of which had failed. An upgrade still waiting says so;
    // one that stopped, or that typing cannot reach, could not be done.
    // Waiting on what `Upgrade now` cannot waive, it could not be done YET.
    // (`will_ask_again`, not `asks_on_its_own`: the round goes on by itself
    // at any age, and a record that says "Couldn't upgrade" above a line
    // that says it asks again on its own is the contradiction the private
    // drive of 12.0 found for a resting round a day behind.)
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
        // A round that stopped and asks again on its own (ruling 283): the
        // upgrade working, said as such.
        (None, _) if resting => format!("{who} upgrade retries later {place}"),
        // A WAIT IS NOT A FAILURE (owner rules, 2026-09-28): `Couldn't upgrade
        // Claude in tab 1 yet` of a session that was merely in use read as a
        // failure with nothing to do. Only what did not work says `Couldn't`.
        (None, Some(Remedy::Now | Remedy::Waits)) => {
            placed_title(&format!("{who} upgrade waits"), place, "")
        }
        // The answer to the owner's own `Upgrade now`: the press hurried the
        // move, so nothing is stalled, and nothing failed either.
        (None, None) if owner_asked => placed_title(&format!("{who} upgrade waits"), place, ""),
        (None, _) => placed_title(&format!("Couldn't upgrade {who}"), place, ""),
    };
    // STILL ASKING ON ITS OWN (D5): an announced move on its own work is
    // asked again at its breaks and idle points, half an hour apart at the
    // least, and ends in a give-up, a rest and a new round — the upgrade
    // working: a record, never the glass. Past the move clock (a day behind,
    // `MOVE_BUDGET_S`, round six F7) it is one line in the digest
    // ([`Row::attention`]); one a person holds (a draft, a box, a hold: F6)
    // is never asked again, and is a record too — the person is at the tab —
    // until a day without progress (one log line) or, a draft nobody sent,
    // one digest line. Round six made both a row with a mark; the owner's
    // rule (an alert persists only where they can act) superseded that.
    //
    // UNLESS THE OWNER ASKED FOR IT (2026-09-28: "when I pressed 'update' in
    // the claude version update button, nothing seemed to happen?"). Their
    // `Upgrade now` withdrew the warn row they pressed, and the answer — it
    // cannot move past the agent's own work yet — went straight to the log in
    // the same instant, so the press read as nothing. While their word stands
    // the answer stays on the glass, calm (`Info`): what it waits on, and
    // that it moves once that ends.
    //
    // WHAT IT IS TO THE OWNER DECIDES THE SHAPE, never its age or its phase
    // (the same `draft` was a Standing row until the notice was typed and a
    // record after): a row that stands only where the owner is needed NOW and
    // one action changes it; the answer to the owner's own press is a NOTICE
    // that folds after [`ANSWER_MAX`]; a fault of aterm's own a Warn record;
    // everything else a calm record.
    let severity = if interrupt.is_some() {
        Severity::Warn
    } else {
        Severity::Info
    };
    // WHAT HOLDS IT, by pid, name and age (2026-09-27: the agent's notice
    // named the shells under it, and the owner read only "its own work
    // runs"). Never a command: those are the agent's own words. A technical
    // line after the move ([`STALL_MOVE_LINE`] stays put) and before the
    // shell spelling, which stays the last (ruling 270); none when nothing
    // holds it (an empty line is left out).
    let held = row.held_words(now).map(|words| {
        let words = atpkg::progress::sanitize_for_tty(&words, aterm_messages::DETAIL_LINE_CAP);
        format!("held by {words}")
    });
    // THE ROW THAT STANDS says why the owner and what the one action is — the
    // `Need`'s own words — and offers no snooze: `Not today` and `Skip
    // version` silence a session with no agent, and are no action.
    if let Some(need) = &interrupt {
        // THE ACTION IS THE SENTENCE AND, WHERE IT IS A COMMAND, THE COMMAND
        // ON A LINE OF ITS OWN (the second review of 2026-09-29): the capped
        // sentence never carries the launch's flags, and a command cut to fit
        // it is one the owner cannot run.
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
    // The why at a sentence's length (day five: 64 clipped `… so the
    // upgrade…` mid-sentence); the band's excerpt is clipped on its own.
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
    // record; aterm's own fault is told by ITS record (`agent_upgrade_fault`,
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

/// ATERM'S OWN FAULT IN AN UPGRADE, as a record (owner rules, 2026-09-28: an
/// alert persists only where the owner can act, and there is nothing to do
/// about a wait aterm does not classify, a notice its screen keeps refusing, a
/// record no look has visited or a relaunched agent that never idles): ONE
/// Warn line in the log, for the owner to read later as the developer — never a
/// row, a mark or a digest line. The wait log, `--status --json`, says the
/// rest; the `Need`'s own words are the two lines.
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

/// How long the answer to an owner's `Upgrade now` stays on the band: TWO
/// MINUTES, then it folds into the log. The owner's complaint was "nothing
/// seemed to happen" (2026-09-28); the press is answered on the glass, and the
/// answer is a notice of their own gesture with no action left in it - never
/// a standing row ("don't continue to persist alerts to the user that are not
/// actionable"), which half an hour of it was (the two reviews of 2026-09-29;
/// the exceptions door cuts any longer hold to this anyway). The span is the
/// engine's ([`NoticeKind::Answer`]); the tests read it here.
#[cfg(test)]
pub(crate) const ANSWER_MAX: Duration = aterm_messages::need::MAX_TRANSIENT;

/// Where a held-back agent runs, as a person names it (day five, D23): `tmux
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

/// WHEN AN ANNOUNCED MOVE WAS LAST ASKED, AND WHEN IT IS ASKED AGAIN — from
/// the record, never a promise (design record 2026-09-28, §1.4 and §3.2 C8).
/// The row said "asked again every 30 minutes" as a fixed phrase, and on tab
/// #1 nothing had asked for 28 hours: the re-ask is taken only at the
/// session's next break or idle point, and a session that offers neither is
/// asked nothing, whatever the clock says.
///
/// * `noticed_at` — when the latest notice was TYPED (`Row::noticed_at`;
///   `0`: not recorded, and no "last asked" is said). Not `clock_s`: that
///   one a usage limit's episode and a void's release move on with nothing
///   asked (review of 2026-09-28: after a limit the row named the episode's
///   end as a notice's time).
/// * `clock_s` — the re-ask clock's start (`Phase::Announced::at_s`): the
///   re-ask is due [`upgrade::REASK_S`] after it.
/// * `asks` — the notices typed this round. At [`upgrade::MAX_ASKS`] the
///   step past the clock is the give-up, not a fifth notice (the same
///   review), and the words say so.
/// * `offset` — the local zone's ([`crate::presence::local_offset_s`]):
///   times said on the clock, with the day when it is not today
///   ([`clock_on_day`]: `yesterday 10:48 AM`, the review's 29-hours-old
///   notice read as this morning); `None`: said as ages, in the why line's
///   units (`10 min ago`, `28 h ago`; [`elapsed_words`]), a notice typed
///   under a minute ago `just now`, and a re-ask due within the minute as
///   due — `in 1 min` would put it off.
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
/// 18 Sep, 2:04 PM`). A Messages row sits under the day it was POSTED, not
/// the day of a time it names, so a bare clock time more than a day old reads
/// as today's (review of 2026-09-28).
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

/// The key AN AGENT'S ONE ROW for its stalled tabs is posted under
/// ([`agent_upgrade_stalled_group`]): `harness.upgrades.claude`,
/// `harness.upgrades.codex`. Never a tab's key: [`agent_upgrade_key_tab`] reads
/// `None` for it, so the per-tab sweeps leave it alone.
pub(crate) fn agent_upgrade_group_key(agent: aterm_agent::harness::upgrade::Agent) -> String {
    let name = match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "claude",
        aterm_agent::harness::upgrade::Agent::Codex => "codex",
    };
    format!("{KEY_AGENT_UPGRADE}s.{name}")
}

/// Whether `key` is an agent's one row for its stalled tabs
/// ([`agent_upgrade_group_key`]).
pub(crate) fn agent_upgrade_group_key_is(key: &str) -> bool {
    key.strip_prefix(KEY_AGENT_UPGRADE)
        .and_then(|rest| rest.strip_prefix("s."))
        .is_some_and(|agent| matches!(agent, "claude" | "codex"))
}

/// The STALLS an agent's one row named, read back from its detail's shell
/// line ([`agent_upgrade_stalled_group`]: `… — tabs s-aaa, s-bbb (to
/// 2.1.284)`, [`group_tabs_list`]) and the lines that carry its list on
/// (`also — tabs …`): each tab (`s-<hex>`) and the build it was stalled on
/// its way to — the build said after it on its line. What the window reads a
/// row the owner saw through by, after a restart and in this process alike,
/// so the same stalls are not posted again — and what keeps a tab in its
/// agent's row across a restart. A tab with no build said after it on its
/// line is read with none, which no stall matches. The list earlier builds
/// wrote, each stall as its comparison key (`s-aaa@2.1.284`,
/// [`agent_upgrade_group_member`]), reads back the same. Empty for a detail
/// that names none. Every line's list is read (round 44: from six tabs the
/// one line passed the line cap, and the sixth stall was clipped away).
pub(crate) fn agent_upgrade_group_members(detail: &[String]) -> Vec<(String, String)> {
    let mut stalls: Vec<(String, String)> = Vec::new();
    for (_, tabs) in detail
        .iter()
        .filter_map(|line| line.rsplit_once(GROUP_TABS))
    {
        // The tabs named bare on this line so far: each takes the build said
        // after it.
        let mut bare = 0;
        for t in tabs.split(", ").filter(|t| t.starts_with("s-")) {
            if let Some((tab, to)) = t.split_once(" (to ") {
                let to = to.strip_suffix(')').unwrap_or(to);
                let from = stalls.len() - bare;
                for stall in &mut stalls[from..] {
                    stall.1 = to.to_string();
                }
                stalls.push((tab.to_string(), to.to_string()));
                bare = 0;
            } else if let Some((tab, to)) = t.split_once('@') {
                stalls.push((tab.to_string(), to.to_string()));
                bare = 0;
            } else {
                stalls.push((t.to_string(), String::new()));
                bare += 1;
            }
        }
    }
    stalls
}

/// One stall of an agent's one row as the window COMPARES it
/// ([`agent_upgrade_group_members`] read back, a row's stalls):
/// `s-aaa@2.1.284`. Never shown: pasted into `aterm harness upgrade <tab>`
/// it names no tab ([`group_tabs_list`] is what the row shows).
pub(crate) fn agent_upgrade_group_member(tab: &str, to: &str) -> String {
    format!(
        "{}@{}",
        atpkg::progress::sanitize_for_tty(tab, 64),
        atpkg::progress::sanitize_for_tty(to, 32)
    )
}

/// What opens the list of stalls on an agent's one row's last line.
const GROUP_TABS: &str = " \u{2014} tabs ";

/// A stall as an agent's one row lists it ([`group_tabs_list`]): the tab's
/// id and the build it is stalled on its way to, each made safe to show.
fn group_stall(row: &aterm_agent::harness::upgrade_drive::Row) -> (String, String) {
    (
        atpkg::progress::sanitize_for_tty(&row.tab, 64),
        atpkg::progress::sanitize_for_tty(&row.to, 32),
    )
}

/// Stalls as an agent's one row's shell line lists them: each tab's id as
/// `aterm harness upgrade <tab>` takes it, and after the last of each run of
/// tabs bound for one build, that build — `s-aaa, s-bbb (to 2.1.284)`. Until
/// 2026-09-30 each was its comparison key, `s-aaa@2.1.284`, which pasted
/// into the command beside it named no tab. A tab bound for no build is
/// named bare, and must come after every tab that has one on its line
/// ([`agent_upgrade_group_members`] gives a bare tab the next build said).
fn group_tabs_list(stalls: &[(String, String)]) -> String {
    let mut list = String::new();
    for (i, (tab, to)) in stalls.iter().enumerate() {
        if i > 0 {
            list.push_str(", ");
        }
        list.push_str(tab);
        let run_ends = stalls.get(i + 1).is_none_or(|(_, next)| next != to);
        if run_ends && !to.is_empty() {
            list.push_str(&format!(" (to {to})"));
        }
    }
    list
}

/// How many of an agent's stalled tabs its one row names a line each: the
/// rest are counted, and `--status` lists them (the detail holds
/// `DETAIL_LINES_CAP` lines).
const GROUP_TABS_NAMED: usize = 12;

/// THE ONE ROW FOR AN AGENT'S STALLED TABS (2026-09-28, the owner: "also fix
/// this stacking failures of claude upgrades!" — three stalled tabs were three
/// rows, the band's whole height, each "Couldn't upgrade Claude yet" with its
/// own buttons). `members`: each stalled tab's row and its place (`in window
/// 2, tab 1`), any order.
///
/// ONE TAB reads as its own row does ([`agent_upgrade_stalled`]), under this
/// key, its buttons the row's own word for that tab and its shell line naming
/// the stall it stands for. SEVERAL: the title counts them, in the most
/// serious of their words — one that could not be done over one that could
/// not be done yet (`Couldn't upgrade Claude in 3 tabs`, `… in 3 tabs yet`;
/// never `… upgrade waits in 3 tabs`, ruling 380); the first
/// line names the tabs, most behind first (`window 2, tab 1 for 26 h · window
/// 1, tab 2 for 7 h`), the second what the row's own buttons do, and behind
/// them each tab's own words — its place and move, why it stands, what holds
/// it — and the shell spelling with every stall, each tab's id and its target
/// ([`agent_upgrade_group_members`] reads them back). Each button is the word
/// for exactly the listed tabs that take it ([`Intent::AgentUpgradeTabs`]); a
/// tab's own menu has its own words.
pub(crate) fn agent_upgrade_stalled_group(
    members: &[(&aterm_agent::harness::upgrade_drive::Row, String)],
    now: u64,
) -> Option<Message> {
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    let (first, _) = members.first()?;
    let agent = first.agent;
    let key = agent_upgrade_group_key(agent);
    // ONE TAB LEFT reads as its own row, under the agent's key, with two
    // differences a person never reads (the review of 2026-09-28): its
    // buttons are the row's own word for that one tab (so a press answers
    // THIS row, never a tab's record behind it), and its shell line ends with
    // the stall it stands for (so the row is read back as the agent's after a
    // restart, never posted again as the tab's own).
    if let [(row, place)] = members {
        let mut msg = agent_upgrade_stalled(row, now, place);
        msg.key = Some(key);
        msg.actions = msg
            .actions
            .iter()
            .map(|intent| match intent {
                Intent::AgentUpgrade { tab, to, word } => Intent::AgentUpgradeTabs {
                    word: *word,
                    moves: vec![(tab.clone(), to.clone())],
                },
                other => other.clone(),
            })
            .collect();
        if let Some(shell) = msg.detail.last_mut() {
            shell.push_str(GROUP_TABS);
            shell.push_str(&group_tabs_list(&[group_stall(row)]));
        }
        return Some(msg);
    }
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let who = agent_word(agent);
    let n = members.len();
    let mut sorted: Vec<&(&aterm_agent::harness::upgrade_drive::Row, String)> =
        members.iter().collect();
    sorted.sort_by_key(|(row, place)| (row.behind_since, place.clone()));
    // The most serious of their words: `yet` only where every tab's own row
    // says it ([`agent_upgrade_stalled`]: a move `Upgrade now` takes, or one
    // that waits) — a round that gave up (`AskAgain`) could not be done, and
    // its tab's own row says so (round 44). (No remedy — no stall at all —
    // never stands in the row; it reads as the mildest.)
    let tier = |row: &aterm_agent::harness::upgrade_drive::Row| match row.remedy(now) {
        Some(Remedy::Now) | None => 0,
        Some(Remedy::Waits) => 1,
        Some(Remedy::AskAgain | Remedy::InItsPane | Remedy::ByHand | Remedy::ResumeInTab) => 2,
    };
    let title = match sorted.iter().map(|(row, _)| tier(row)).max().unwrap_or(0) {
        // Never `… upgrade waits in N tabs` (ruling 380, the owner's words
        // of 2026-09-28: "What is this alert about 'Codex upgrade waits in
        // tab 1'???"): a row stands only once the ladder's last rung has
        // stood for hours, and then it could not be done yet — each tab's
        // own row's words ([`agent_upgrade_stalled`]).
        0 | 1 => format!("Couldn't upgrade {who} in {n} tabs yet"),
        _ => format!("Couldn't upgrade {who} in {n} tabs"),
    };
    let bare = |place: &str| place.strip_prefix("in ").unwrap_or(place).to_string();
    let named = sorted
        .iter()
        .map(|(row, place)| match row.behind_for(now) {
            Some(b) => format!("{} for {b}", bare(place)),
            None => bare(place),
        })
        .collect::<Vec<_>>()
        .join(" \u{00b7} ");
    // Each button, for exactly the listed tabs that take its word.
    let takes = |word: UpgradeWord| -> Vec<(String, String)> {
        sorted
            .iter()
            .filter(|(row, _)| agent_upgrade_words(row, now).contains(&word))
            .map(|(row, _)| (row.tab.clone(), row.to.clone()))
            .collect()
    };
    let words: Vec<(UpgradeWord, Vec<(String, String)>)> = UpgradeWord::ALL
        .into_iter()
        .map(|word| (word, takes(word)))
        .filter(|(_, moves)| !moves.is_empty())
        .take(2)
        .collect();
    // What those buttons do, in the row's own words (ruling 288). Some of
    // the listed tabs, by count — two are `both`, never `all 2` (round 35,
    // D4) — or one by its place.
    let which = |tabs: &[&aterm_agent::harness::upgrade_drive::Row]| match tabs {
        [row] => sorted
            .iter()
            .find(|(listed, _)| listed.tab == row.tab)
            .map_or_else(|| "one".to_string(), |(_, place)| bare(place)),
        _ if tabs.len() == n && n == 2 => "both".to_string(),
        _ if tabs.len() == n => format!("all {n}"),
        _ => format!("{} of them", tabs.len()),
    };
    let mut does: Vec<String> = Vec::new();
    for (word, moves) in &words {
        let taking: Vec<&aterm_agent::harness::upgrade_drive::Row> = sorted
            .iter()
            .map(|(row, _)| *row)
            .filter(|row| moves.iter().any(|(tab, _)| *tab == row.tab))
            .collect();
        let k = taking.len();
        // The tabs `Upgrade now` cannot move, in number.
        let rest = match n - k {
            0 => String::new(),
            1 => "; the other one moves once what holds it ends".to_string(),
            m => format!("; the other {m} move once what holds them ends"),
        };
        does.push(match word {
            // At their next PAUSES, as each tab's own row says it (ruling
            // 380: never "at its next turn end" — a goal-mode Codex's turns
            // never end) — but a tab the word only asks once more is said so
            // ([`upgrade_now_asks_again`]), as its own row and the answer to
            // the press say it: `moves both at their next pauses` was false of it.
            UpgradeWord::Now => {
                let (asked, moved): (Vec<_>, Vec<_>) = taking
                    .iter()
                    .copied()
                    .partition(|row| upgrade_now_asks_again(row));
                let at = |k: usize| {
                    if k == 1 {
                        "its next pause"
                    } else {
                        "their next pauses"
                    }
                };
                let did = if asked.is_empty() {
                    format!("moves {} at {}", which(&moved), at(moved.len()))
                } else if moved.is_empty() {
                    format!("asks {} once more", which(&asked))
                } else {
                    format!(
                        "moves {} at {} and asks {} once more",
                        which(&moved),
                        at(moved.len()),
                        which(&asked)
                    )
                };
                format!("{} {did}{rest}", word.label())
            }
            UpgradeWord::NotToday => {
                format!(
                    "{} puts {} off until tomorrow",
                    word.label(),
                    which(&taking)
                )
            }
            UpgradeWord::Skip => {
                format!("{} keeps {} where they are", word.label(), which(&taking))
            }
        });
    }
    does.push("for one tab alone, use its tab's menu".to_string());
    // A RECORD: the tabs it would name are each their own record (a group row
    // was the standing claim on the owner that nobody could act on, ruling 363),
    // and nothing posts it any more (`no_tab_joins_an_agents_group_row`). Said
    // on the constructor: nothing below touches the hold or the class.
    let mut msg = Message::new(tags::HARNESS, Severity::Warn, title)
        .record()
        .line(atpkg::progress::sanitize_for_tty(&named, 160))
        .line(does.join("; "));
    for (row, place) in sorted.iter().take(GROUP_TABS_NAMED) {
        let why = row
            .stall_words(now)
            .unwrap_or_else(|| "it is not moving".to_string())
            .replace('`', "");
        let held = row
            .held_words(now)
            .map_or_else(String::new, |w| format!("; held by {w}"));
        msg = msg.line(atpkg::progress::sanitize_for_tty(
            &format!(
                "{} \u{00b7} {}: {why}{held}",
                bare(place),
                clean(&row.move_words())
            ),
            aterm_messages::DETAIL_LINE_CAP,
        ));
    }
    if n > GROUP_TABS_NAMED {
        msg = msg.line(format!(
            "and {} more \u{2014} `aterm harness upgrade --status` lists them",
            n - GROUP_TABS_NAMED
        ));
    }
    // Every stall, each tab's id and its target, on as many lines as the
    // line cap needs (round 44: one line clipped the sixth tab's away, and a
    // row the owner put down came back after a restart). `--now` only for
    // the tabs that take it, as the buttons (and each tab's own row) offer
    // it: a row of tabs no word moves past their waits named it all the
    // same, and one where only some take it offered it for every tab listed.
    let now_tabs: Vec<String> = words
        .iter()
        .filter(|(word, _)| *word == UpgradeWord::Now)
        .flat_map(|(_, moves)| {
            moves
                .iter()
                .map(|(tab, _)| atpkg::progress::sanitize_for_tty(tab, 64))
        })
        .collect();
    let shell = if now_tabs.len() == n {
        "--now` or `--skip"
    } else {
        "--skip"
    };
    // Each build said once a line: the tabs bound for one build together
    // (most behind first among them), and a tab bound for none last.
    let mut stalls: Vec<(String, String)> =
        sorted.iter().map(|(row, _)| group_stall(row)).collect();
    stalls.sort_by_key(|(_, to)| (to.is_empty(), to.clone()));
    let mut head =
        format!("the same in any shell: `aterm harness upgrade <tab> {shell}`{GROUP_TABS}");
    let mut on_line: Vec<(String, String)> = Vec::new();
    for stall in stalls {
        on_line.push(stall);
        if on_line.len() > 1
            && head.chars().count() + group_tabs_list(&on_line).chars().count()
                > aterm_messages::DETAIL_LINE_CAP
        {
            let next = on_line.pop().expect("just pushed");
            msg = msg.line(format!("{head}{}", group_tabs_list(&on_line)));
            head = format!("also{GROUP_TABS}");
            on_line = vec![next];
        }
    }
    msg = msg.line(format!("{head}{}", group_tabs_list(&on_line)));
    // Never with the list's marker: only the list is read back.
    if !now_tabs.is_empty() && now_tabs.len() < n {
        msg = msg.sentence(format!(
            "`aterm harness upgrade <tab> --now` only for {}",
            now_tabs.join(", ")
        ));
    }
    for (word, moves) in words {
        msg = msg.action(Intent::AgentUpgradeTabs { word, moves });
    }
    Some(msg.key(&key))
}

/// [`agent_upgrade_stalled`] where its tab is — and THE ROW THAT STANDS TAKES
/// THE OWNER TO ITS TAB (ruling 440). An upgrade's interrupt
/// ([`aterm_agent::harness::upgrade_drive::Row::interrupt`]: the agent it
/// ended, a shutdown that hung, a relaunch that reached no prompt, a goal left
/// paused) asks for a hand IN that tab, and with more than one window its
/// words name a window (`in window 2, tab 1`) that no chrome but the
/// connection map numbers. Where the tab was found it offers `Show tab N`
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

/// WHERE A ROW'S TAB IS, for the words of its digest entry: the place the
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

/// The kinds of digest need whose action is the same in every tab, so ONE entry
/// names every tab that has it (the owner files it once, never a line per tab).
/// Every other kind's action is done in one tab, and is that tab's entry.
const PER_KIND: [&str; 3] = ["own-work", "typed-draft", "daemon-busy"];

/// The digest kinds whose dismissal is the owner's own CHOICE (a launch they
/// made, a terminal they run it in): a filing holds 30 days, not 7.
const CHOICE_KINDS: [&str; 3] = ["no-terminal", "refused-launch", "no-integration"];

/// THE UPGRADE'S PART OF THE EXCEPTIONS DIGEST (owner rules, 2026-09-28: "there
/// needs to be some clear reason why the user must be engaged and what the user
/// should do"; "batch these exceptions in a single dismissable alert"). Every
/// row the classifier ([`aterm_agent::harness::upgrade_drive::Row::attention`])
/// calls a `Digest` becomes an [`Entry`] of [`Family::Upgrade`], keyed by the
/// KIND of the wait and — where the action is done in one tab — that tab:
///
/// * `own-work`, `typed-draft`, `daemon-busy`: ONE entry per kind, naming every
///   tab that has it (`Claude in 3 tabs is behind`); five tabs waiting on their
///   own work are one line, filed once.
/// * the rest: one entry per tab (`harness.upgrade.<tab>.suspended`).
///
/// What makes it the same fact is the key alone: a new build, a pid that came
/// and went and a tab that joined are not news (they restate the words in
/// place, at most once in ten minutes); another KIND is. The words are the
/// classifier's ([`aterm_agent::harness::upgrade_drive::Need`]: `why` and
/// `then`, capped at 90 and checked by the engine's vocabulary at
/// [`Entry::message`]); this only says WHERE, and how long a dismissal holds.
/// The rows arrive with their [`UpgradeSpot`], any order. It returns the
/// family's WHOLE answer: the host hands it to `App::except_replace`, so a kind
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
        ("daemon-busy", _) => vec!["A Codex thread holds the daemon".to_string()],
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
        ("other-client", _) => vec![
            format!("Another Codex holds Codex {place}"),
            format!("Another Codex holds Codex, {bare}"),
        ],
        ("no-integration", _) => vec![
            format!("Codex {place} lacks shell integration"),
            format!("Codex lacks shell integration, {bare}"),
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
// repaint that gave up (owner rules, 2026-09-28: an alert persists only where
// the owner can act, and says why and what; fix the cause first). Each is a
// RECORD when it begins (the log the owner reads later) and, only if it goes
// on, ONE dismissable line of the digest — never a band row, a tab mark or a
// notification.
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
/// Only this shape is ever an entry: a survey the loop is STILL TRYING to close
/// ([`aterm_agent::supervise::StuckSurvey::Unclosed`]) is a record and nothing
/// more, because a persisting "still trying" is the alert the exceptions rule
/// forbids (design 4.5): the loop's own ladder either closes it or the card
/// becomes this one.
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

/// The key of [`accounts_signed_out_entry`]: one line for every signed-out
/// account, so the fact is its identity and never the set of labels.
pub(crate) const ACCOUNTS_SIGNED_OUT_KEY: &str = "harness.accounts.signed-out";

/// The key of [`accounts_refused_entry`].
pub(crate) const ACCOUNTS_REFUSED_KEY: &str = "harness.accounts.refused";

/// The action of [`accounts_signed_out_entry`], up to the label it names.
const ACCOUNTS_LOGIN_STEP: &str = "Run aterm harness accounts login";

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
        1 => "A Claude account is signed out".to_string(),
        n => format!("{n} Claude accounts are signed out"),
    };
    let named = format!("{ACCOUNTS_LOGIN_STEP} {first}");
    let then = if named.chars().count() <= aterm_messages::need::STEP_CAP {
        named
    } else {
        format!("{ACCOUNTS_LOGIN_STEP} with the first label under Details")
    };
    let mut entry = Entry::new(
        Family::Accounts,
        "",
        "signed-out",
        &what,
        Why::OnlyYou(Reason::new(
            "A signed-out account serves no session until a browser sign-in restores it",
        )),
        Then::step(then),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT);
    for line in accounts_signed_out_lines(labels) {
        entry = entry.more(line);
    }
    Some(entry.log_line(format!("accounts need=signed-out n={}", labels.len())))
}

/// The detail of [`accounts_signed_out_entry`]: `Signed out: a, b, …` on the
/// lines an entry keeps. A label joins the line while the line stays within
/// [`ACCOUNTS_LABELS_FILL`] characters and starts the next otherwise, whole;
/// the labels the last line has no room for are counted at its end.
fn accounts_signed_out_lines(labels: &[String]) -> Vec<String> {
    let mut lines = vec![ACCOUNTS_LABELS_LEAD.to_string()];
    let mut starts = true;
    let mut named = 0;
    for label in labels {
        let line = lines.last_mut().expect("a line");
        if starts {
            line.push_str(label);
        } else if line.chars().count() + 2 + label.chars().count() <= ACCOUNTS_LABELS_FILL {
            line.push_str(", ");
            line.push_str(label);
        } else if lines.len() < aterm_messages::exceptions::MORE_LINES {
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

/// THE RESUME LOOP'S PART OF THE EXCEPTIONS DIGEST (owner rules, 2026-09-28:
/// "there needs to be some clear reason why the user must be engaged and what
/// the user should do"; a wait is never an exception). What the loop of each
/// tab found only a person can answer ([`Exception`], told through
/// `IdleHost::exception`) and that can wait for their next look (`Later`; a
/// lost login is `Now` and stays the supervisor's own escalation, never an
/// entry) becomes an [`Entry`] of [`Family::Resume`]:
///
/// * a limit, a cleared goal, a wall that will not lift: ONE entry per KIND
///   (`harness.resume.<kind>`), naming every tab that has it, since the action
///   is the owner's and the same for each; the first tab's words speak, the
///   list of tabs is a detail.
/// * a stall no reader names ([`ExceptionKind::Stalled`]), a dialog no reader
///   names ([`ExceptionKind::Dialog`]) and a context no `/compact` lowers
///   ([`ExceptionKind::Context`]): one entry per TAB
///   (`harness.resume.<tab>.stalled`, `.dialog`, `.context`), because the
///   action, typing into the tab, answering its dialog or going back a few
///   messages in it, is done in that tab.
///
/// The words are the loop's (`why` and `action`, capped and checked by the
/// engine's vocabulary at [`Entry::message`]). Returns the family's WHOLE answer
/// for `App::except_replace`: a kind no tab has any more ends.
pub(crate) fn resume_exceptions(standing: &[(String, Exception, UpgradeSpot)]) -> Vec<Entry> {
    let mut kinds: std::collections::BTreeMap<
        ExceptionKind,
        Vec<&(String, Exception, UpgradeSpot)>,
    > = std::collections::BTreeMap::new();
    let mut entries = Vec::new();
    for fact in standing {
        let (sid, exception, spot) = fact;
        if exception.when != Urgency::Later {
            continue;
        }
        if matches!(
            exception.kind,
            ExceptionKind::Stalled | ExceptionKind::Dialog | ExceptionKind::Context
        ) {
            entries.push(resume_tab_entry(sid, exception, spot));
        } else {
            kinds.entry(exception.kind).or_default().push(fact);
        }
    }
    for (kind, group) in kinds {
        let (_, first, first_spot) = group[0];
        let n = group.len();
        let head = match kind {
            ExceptionKind::Spend => "Account limit stops Claude",
            ExceptionKind::Bucket => "Model limit stops Claude",
            ExceptionKind::GoalCleared => "Claude lost its goal",
            ExceptionKind::Futile => "Limit still holds Claude",
            // A login is the supervisor's; a stall, a dialog and a context are a
            // tab's own.
            ExceptionKind::Login
            | ExceptionKind::Stalled
            | ExceptionKind::Dialog
            | ExceptionKind::Context => continue,
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
                .map(|(sid, _, _)| sid.as_str())
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
                .map(|(_, _, spot)| spot.place.strip_prefix("in ").unwrap_or(&spot.place))
                .collect();
            entry = entry.more(format!("In {}", places.join(", ")));
        }
        entries.push(entry);
    }
    entries
}

/// A tab's OWN entry (`harness.resume.<tab>.<kind>`) for what the owner does IN
/// that tab: a screen no reader names, stalled for hours (open the tab and
/// type), a dialog no reader names, stood at for an hour (answer it), or a
/// context no `/compact` lowers (go back a few messages in it).
fn resume_tab_entry(sid: &str, exception: &Exception, spot: &UpgradeSpot) -> Entry {
    let (head, tail, severity, note) = match exception.kind {
        ExceptionKind::Context => ("Context", " cannot be compacted", Severity::Info, ""),
        ExceptionKind::Dialog => (
            "Dialog",
            " stops Claude",
            Severity::Warn,
            " for over an hour",
        ),
        _ => (
            "Claude",
            " has stalled",
            Severity::Warn,
            " for over 3 hours",
        ),
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

/// The key of the record that a tab's screen no reader names has stood with its
/// own title idle (the canary's second rung: 90 minutes of background work, 30
/// of a dialog).
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

/// The digest keys of a tab's relaunch lines: `[blocked, gone]`.
pub(crate) fn relaunch_keys(sid: &str) -> [String; 2] {
    [
        digest_key(Family::Supervisor, sid, "not-back"),
        digest_key(Family::Supervisor, sid, "gone"),
    ]
}

/// The record of a relaunch's word (`text`: what happened, in the words of the
/// tab's mark it used to be). Warn, in the log; the tab is not marked.
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

/// An agent aterm cannot bring back and cannot name the command for (a launch it
/// will not repeat, no conversation known): the owner starts it again.
/// `harness.supervisor.<tab>.gone`.
pub(crate) fn relaunch_gone_entry(
    sid: &str,
    spot: &UpgradeSpot,
    reason: &str,
    restarted: bool,
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
        Then::step("Start the agent in that tab and resume its conversation"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .log_line(format!("relaunch cannot sid={sid} reason={reason}"));
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

/// THE SAME STALL, by the words of two stalled rows' details (`a`, `b`,
/// [`agent_upgrade_stalled`]) under one tab's key: the same move
/// ([`STALL_MOVE_LINE`], past its place) and the same stall — its words, an
/// overdue one's by its kind alone (`behind for …` moves with every look).
/// The place is not compared: a tab's place in its strip moves as tabs open,
/// close and move, and the key already names the tab. What keeps a stall the
/// owner already saw from being posted again after aterm restarts
/// (`upgrade_host`, the owner's report of 2026-09-27).
///
/// A ROW AN EARLIER BUILD POSTED reads the same when its words do: before
/// ruling 270 the move line held the move alone, with no place before it.
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

/// A stalled row's WHY and MOVE lines ([`agent_upgrade_stalled`]), found by
/// what they say, not where they stand (ruling 307): a refused press
/// restates the row with the refusal ABOVE the stall's lines (the band
/// paints `detail[0]`), which shifted every index, so a row the owner saw
/// through read as another stall after a restart and came back. The move is
/// the line that opens with a place (`in tab 2 · …`), the why two lines above
/// it; a row an earlier build posted, whose move line held no place, reads
/// at the fixed [`STALL_MOVE_LINE`]. `None` for a detail with neither.
fn stall_lines(d: &[String]) -> Option<(&str, &str)> {
    match d.iter().position(|l| strip_place(l).is_some()) {
        Some(at) => Some((d.get(at.checked_sub(2)?)?, &d[at])),
        None => Some((d.first()?, d.get(STALL_MOVE_LINE)?)),
    }
}

/// HOW MANY LEADING DETAIL LINES ARE THE SENTENCE (ruling 314, day eight
/// E2): Settings ▸ Messages sets them as a person's words and the rest as
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
    // An agent's one row for its stalled tabs: its tabs, and what its buttons
    // do — one tab's reads as that tab's own row.
    if key.is_some_and(agent_upgrade_group_key_is) {
        if let Some(at) = detail.iter().position(|l| strip_place(l).is_some()) {
            return at.max(1);
        }
        return 2.min(detail.len()).max(1);
    }
    // The lost-integration row ([`integration_lost`]): the tabs and the loss,
    // then what gives it back; each tab's remedy and the session ids are its
    // technical lines. The socketless harness row
    // ([`harness_without_socket`]): the cause, then its fix.
    if key == Some(KEY_INTEGRATION_LOST) || key == Some(KEY_HARNESS_NO_SOCKET) {
        // Past more tabs than the first line names, the count of the rest
        // sits between the names and the remedy.
        let counted = key == Some(KEY_INTEGRATION_LOST)
            && detail.get(1).is_some_and(|l| integration_rest_count(l));
        return (2 + usize::from(counted)).min(detail.len()).max(1);
    }
    1
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

/// A tab's PLACE in the person's words (ruling 270): `in tab 2` — its place
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
/// yet, window 2, tab 1` — a comma, since the title lint (ruling 261) allows
/// no parenthesis and no colon. Every title that fits keeps the words ruling
/// 270 and its successors gave it.
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

/// A STALL THAT ENDED WHILE ITS ROW WAS DOWN (review of 2026-09-27): the
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
/// WORDED BY WHAT HAPPENED, WITH ITS TAB (ruling 307): `Claude upgrade asks
/// again in tab 2` for a tab whose upgrade goes on (the tab still holds a
/// round), `Claude upgrade no longer waits in tab 2` for one that left —
/// never `no longer stalled`, a word the band retired, and never the stall's
/// reason in the present tense under a title saying it ended: the reason is
/// `was: …`, then the move. A restart under way that stuck and moves again
/// (ruling 327) is `Claude upgrade moves again in tab 2`: nothing asks
/// anything again, it carries on (the merge review of 2026-09-28: it read
/// `asks again`). See [`StallEnd`].
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

/// WHAT BECAME OF AN UPGRADE WHOSE STALL ENDED while its row was down
/// ([`agent_upgrade_stall_over`]'s title, ruling 307(d)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StallEnd {
    /// The tab still holds a round of the upgrade, which asks again.
    AsksAgain,
    /// A restart under way that stuck (`stuck:<what>`, ruling 327) moves
    /// again, still in flight: it carries on, and asks nothing again.
    MovesAgain,
    /// The tab left the upgrade's rows: nothing waits there now.
    Left,
}

/// The agent a row's words name: `Claude` or `Codex`.
fn agent_word(agent: aterm_agent::harness::upgrade::Agent) -> &'static str {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "Claude",
        aterm_agent::harness::upgrade::Agent::Codex => "Codex",
    }
}

/// A ROUND RE-ARMED AFTER A GIVE-UP THAT ITS AGENT'S OWN WORK HOLDS (ruling
/// 364, `Row::will_ask_again`'s pending arm — the fact without the age, for a
/// round a day behind goes on by itself too; a usage limit, the other one,
/// waits anyway): the upgrade working, and no word the owner presses moves it
/// sooner — the move comes at the end of that work (round 35, D5).
fn own_work_round(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> bool {
    row.phase == aterm_agent::harness::upgrade::Phase::Pending && row.will_ask_again(now)
}

/// THE OWNER'S WORDS THAT DO SOMETHING FOR THIS UPGRADE (gap #21), most
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
        // A goal left paused is moved by `/goal resume` alone (or, fallen
        // into a sandbox, a relaunch by hand): no word of the upgrade's is
        // offered on its row.
        || matches!(
            row.stall(now).as_deref(),
            Some("goal-paused" | "goal-sandboxed")
        )
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
    // THE WORD IN FORCE IS NOT OFFERED AGAIN (ruling 270; day three: the
    // record the owner pressed `Not today` on kept `Not today` as its
    // Primary): a deferral still running drops `Not today`, a skip of this
    // very build leaves nothing to say.
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
/// queue (typed once more, ruling 307), or its round gave up (re-armed: the
/// question typed again, and the move once the agent answers READY —
/// `upgrade_drive::Ask::Now`). Each such tab's own row says so
/// ([`agent_upgrade_stalled`]), and so must its agent's row
/// ([`agent_upgrade_stalled_group`]) and the answers to a press
/// ([`agent_upgrade_worded`], [`agent_upgrade_group_worded`]).
///
/// Never a round that gave up under the QUIET flow (`Row::quiet`, round
/// eight, #4): re-armed, it asks nothing — the quiet restart moves it at its
/// next pause, as any pending round's.
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

/// WHAT THE OWNER'S WORD DID (gap #21), in the owner's terms — a RECORD: a
/// confirmation earns no row (ruling 76). The stalled row it was pressed on
/// takes these words as it is ANSWERED (ruling 270: `ℹ`, never the `✓` of
/// work delivered or a problem fixed — putting an upgrade off is a choice,
/// and `Upgrade now` only asks), so the entry the owner pressed is the one
/// that says what happened; with no such row, it is recorded. `row` is the
/// upgrade as the word left it; `place` names its tab (`in tab 2`).
/// `detail[0]` is the sentence, the move behind it.
///
/// `asked_again`: `Upgrade now` only asked the row pressed once more
/// ([`upgrade_now_asks_again`] of the row as it was shown: its question
/// unread behind a full queue, or its round given up) — said so, never `the
/// wait for a quiet tab is waived`, of a session already idle (ruling 307),
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
        // The ladder's last rung (ruling 380): the first pause, never "when
        // its turn ends" nor "when it goes idle" — a goal-mode Codex's turns
        // never end. Under the default (`upgrade_ask = false`) no pause is
        // made for a goal, so its move comes once the goal stops.
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

/// WHAT THE OWNER'S WORD ON AN AGENT'S ONE ROW ASKED
/// ([`agent_upgrade_stalled_group`], [`Intent::AgentUpgradeTabs`]): the words
/// the row is ANSWERED with, of the `n` tabs that took the word — `Claude
/// waits until tomorrow in 3 tabs`, `Claude skips this build in 3 tabs`,
/// `Claude moves in 3 tabs` — never a `✓`: a choice is not delivered work,
/// and `Upgrade now` has moved nothing yet. What each tab's word did is that
/// tab's own record ([`agent_upgrade_worded`]).
///
/// `asked`: how many of the `n` `Upgrade now` only asked once more
/// ([`upgrade_now_asks_again`]: a question unread behind a full queue, a
/// round given up) — said so, as each tab's own record says it, never `the
/// wait for a quiet tab is waived` of a session already idle (ruling 307) nor
/// `moves at its next pause` of a round that moves only once the agent
/// agrees. `Upgrade now` moves the others at their next pauses, never "when
/// idle": a goal-mode Codex's turns never end (ruling 380).
pub(crate) fn agent_upgrade_group_worded(
    agent: aterm_agent::harness::upgrade::Agent,
    word: aterm_messages::UpgradeWord,
    n: usize,
    asked: usize,
) -> Message {
    use aterm_messages::UpgradeWord;
    let who = agent_word(agent);
    let in_tabs = |k: usize| {
        if k == 1 {
            "in 1 tab".to_string()
        } else {
            format!("in {k} tabs")
        }
    };
    let each = |k: usize| if k == 1 { "it" } else { "each" };
    let tabs = in_tabs(n);
    let asked = asked.min(n);
    let (title, then) = match word {
        UpgradeWord::Now if asked == n => (
            format!("{who} is asked again {tabs}"),
            format!(
                "the upgrade's question is typed once more; {} moves once {who} agrees",
                each(n)
            ),
        ),
        UpgradeWord::Now if asked > 0 => (
            format!("{who} moves {}", in_tabs(n - asked)),
            format!(
                "{} moves at its next pause; in {asked} more the upgrade's question is typed \
                 once more, and {} moves once {who} agrees",
                each(n - asked),
                each(asked)
            ),
        ),
        UpgradeWord::Now => (
            format!("{who} moves {tabs}"),
            format!(
                "the wait for a quiet tab is waived; {} moves at its next pause, never over \
                 typing, a draft, a dialog or running work",
                each(n)
            ),
        ),
        UpgradeWord::NotToday => (
            format!("{who} waits until tomorrow {tabs}"),
            "the upgrade tries each again from this time tomorrow".to_string(),
        ),
        UpgradeWord::Skip => (
            format!("{who} skips this build {tabs}"),
            "each stays where it is until a newer build comes".to_string(),
        ),
    };
    Message::new(tags::HARNESS, Severity::Info, title)
        .line(then)
        .record()
        .key(&agent_upgrade_group_key(agent))
}

/// AN AGENT'S ONE ROW'S STALLS ENDED WHILE THE ROW WAS DOWN (the review of
/// 2026-09-28; [`agent_upgrade_stall_over`] for one tab's row): the owner read
/// or dismissed the row, and no tab it named stalls in it any more. The log's
/// last word under the agent's key would stay the owner's read, and the same
/// stalls coming back — the same tabs overdue again, days later — would be
/// taken for the row already seen and never shown. This RECORD, under the
/// row's own key, is their end: the same stalls back are news. `detail` is
/// the row's: its title is kept as what it was, with the tabs it named.
pub(crate) fn agent_upgrade_group_over(
    agent: aterm_agent::harness::upgrade::Agent,
    title: &str,
    detail: &[String],
) -> Message {
    let who = agent_word(agent);
    let n = agent_upgrade_group_members(detail).len().max(1);
    let tabs = if n == 1 {
        "in 1 tab".to_string()
    } else {
        format!("in {n} tabs")
    };
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Info,
        format!("{who} upgrade wait ended {tabs}"),
    )
    .line(format!(
        "was: {}",
        atpkg::progress::sanitize_for_tty(title, 120)
    ));
    if let Some(first) = detail.first() {
        msg = msg.line(first.clone());
    }
    msg.record().key(&agent_upgrade_group_key(agent))
}

/// The finished words of an agent's one row whose tabs all moved (the review
/// of 2026-09-28; day five's D26 for one tab's row,
/// [`agent_upgrade_went_through`]): `Claude upgraded in 3 tabs` — or, one tab,
/// that tab's own words — the `✓` record's title in the log, never the row's
/// `⚠ Couldn't upgrade …` beside the success records.
pub(crate) fn agent_upgrade_group_went_through(
    agent: aterm_agent::harness::upgrade::Agent,
    n: usize,
    place: Option<&str>,
) -> String {
    let who = agent_word(agent);
    match (n, place) {
        (1, Some(place)) => format!("{who} upgraded {place}"),
        (1, None) => format!("{who} upgraded in 1 tab"),
        _ => format!("{who} upgraded in {n} tabs"),
    }
}

/// THE OWNER'S WORD ON AN AGENT'S ONE ROW, REFUSED FOR EVERY TAB IT WAS SENT
/// FOR (the review of 2026-09-28): nothing was written for any of the `n`
/// tabs, and why — the first tab's refusal (`why`), in the words one tab's
/// row takes ([`agent_upgrade_word_refused`]; `to`, the build the row named,
/// for a skip's title). The row pressed takes these words above its own and
/// keeps its buttons, as one tab's row does; with no live row, one
/// gesture-failure row for the press — never a row per tab stacked under it.
pub(crate) fn agent_upgrade_group_word_refused(
    agent: aterm_agent::harness::upgrade::Agent,
    word: aterm_messages::UpgradeWord,
    n: usize,
    (to, why): (&str, &str),
) -> Message {
    let tabs = if n == 1 {
        "in 1 tab".to_string()
    } else {
        format!("in {n} tabs")
    };
    let mut msg = agent_upgrade_word_refused(word, agent, ("", to), why, &tabs);
    msg.key = Some(agent_upgrade_group_key(agent));
    msg
}

/// The line an ANSWERED agent's row carries for the tabs its word was not
/// written for (`n` of them), and why — the first refusal's remedy, in the
/// words one tab's row takes ([`agent_upgrade_word_refused`]).
pub(crate) fn agent_upgrade_group_word_refused_line(
    agent: aterm_agent::harness::upgrade::Agent,
    word: aterm_messages::UpgradeWord,
    n: usize,
    why: &str,
) -> String {
    let refused = agent_upgrade_group_word_refused(agent, word, n, ("", why));
    let tabs = if n == 1 {
        "1 tab".to_string()
    } else {
        format!("{n} tabs")
    };
    match refused.detail.first() {
        Some(reason) => format!("not written for {tabs}: {reason}"),
        None => format!("not written for {tabs}"),
    }
}

/// THE OWNER'S WORD REFUSED (gap #21): nothing was written, and why — another
/// step held the lock (press it again), the upgrade moved on to a newer
/// build than the one pressed for, or `upgrade_drive::ask`'s own reason (a
/// restart under way, one stopped for good, none recorded). The person's own
/// press failed, so it is the one gesture-failure shape (ruling 270, H11):
/// Error, `HOLD_GESTURE` — a stalled row it was pressed on takes these words
/// above its own and keeps its own mark and hold.
///
/// A ROUND THAT STOPPED between the offer and the press (ruling 283) is
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
    // The tab by its place (day five, D22: `Couldn't upgrade Claude now`
    // named none). What was refused is named (ruling 307: `Couldn't put off
    // Claude` read as Claude itself being put off) — and short enough that
    // the remedy beside it is painted at 80 columns, as its siblings' is
    // (review of round 21: `Couldn't postpone Claude's upgrade in tab 2`,
    // 43 cells, painted its title alone). The tab names whose upgrade.
    let title = match word {
        UpgradeWord::Now => placed_title(&format!("Couldn't upgrade {who}"), place, " now"),
        UpgradeWord::NotToday => placed_title("Couldn't postpone the upgrade", place, ""),
        UpgradeWord::Skip => placed_title(&format!("Couldn't skip {who} {shown}"), place, ""),
    };
    // THE REMEDY FIRST, THE MECHANISM AFTER (ruling 307): at 80 columns the
    // excerpt kept `another step held the…`, and at 120 it was cut just
    // before `press it again` — the one part that changes what the person
    // does. The remedy is its own short line (the painted excerpt), the
    // mechanism the next.
    let mut again = false;
    let (reason, then, plain) = if why.starts_with("busy:") {
        again = true;
        (
            "press it again".to_string(),
            Some("another step held the upgrade's lock, so nothing was written".to_string()),
            true,
        )
    } else if let Some(now_to) = why.strip_prefix("stale:") {
        // The TARGET changed; nothing moves now (review of round 21: `it
        // moves to 2.1.283 now` promised a move the tab still waits for).
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
        // No button named (day five, D21): the entry this lands on carries
        // its own capsules, and a refusal of its own carries none.
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
    // person's words where its kind is known (round 21: those rows painted
    // nothing at any width, beside siblings that did), whole behind Details.
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

/// A writer's refusal of the owner's word in a person's words, by its kind
/// (ruling 307); `None` for one this build does not know.
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

/// The finished words of a stall row whose upgrade went through after all
/// (day five, D26): `Claude upgraded in tab 1` — the `✓` record's title in
/// the log, where the row's `⚠ Couldn't upgrade …` read as a failure beside
/// the success record.
pub(crate) fn agent_upgrade_went_through(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> String {
    format!("{} upgraded {place}", agent_word(row.agent))
}

/// THE LIVE AGENT UPGRADE, DONE: the owner's outcome line — the build the
/// session resumed on and the model its first answer named, or that the model
/// could not be confirmed ([`aterm_agent::harness::upgrade::restart_outcome`])
/// — as a RECORD. Until 2026-09-24 it went only to the ledger's JSONL, so a
/// model that changed across the move was news nobody was told.
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
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Success,
        format!("{} moved onto {}", row.agent.product(), clean(&row.to)),
    )
    .line(place);
    if !model.is_empty() {
        msg = msg.line(clean(model));
    }
    msg.hold(Hold::LogOnly)
        .key(&format!("{KEY_AGENT_UPGRADE}.done"))
}

/// R21 — THE FIRST-OPEN INSTALL DOCTOR: this copy runs from somewhere it can
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
        // Filed under `system` (ruling 309): how aterm itself is installed is
        // no ALab tool, and a person filtering for it looked there in vain.
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
// A person's Settings ▸ Packages verb (design ruling 224).
// ---------------------------------------------------------------------------

/// A Settings ▸ Packages verb a person pressed and is WAITING on — so its
/// pass takes the band's animated row whatever it weighs
/// ([`aterm_messages::Waiter::Person`], ruling 220 applied to the packages
/// lane). The row shares the lane's key ([`crate::toolchain_words::KEY_PASS`]):
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
/// on the page that pressed it (ruling 101). A removal declares the disk as
/// its load — deleting gigabytes of toolchains is the heavy part.
pub(crate) fn packages_verb_row(verb: PackagesVerb) -> Message {
    let (glyph, load) = match verb {
        PackagesVerb::Check => ('\u{21bb}', None),
        PackagesVerb::Install => ('\u{21e3}', None),
        // A drawn removal mark (ruling 304): the `·` read as a bullet.
        PackagesVerb::Remove => ('\u{2296}', Some(aterm_messages::Load::Disk)),
    };
    // The work's own load (ruling 246): a check or an install downloads, a
    // removal deletes — their words show only while other work is slowed.
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
/// `lock-waiting:`, which used to be a log line only): busy with the elapsed
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

/// R22 — the first UI-made session connection (`connections::first_use_*`):
/// a DISCLOSURE — the tab's own mark and menu already show the connection
/// and its Disconnect — so a RECORD, never a row. `text` is the connections
/// module's sentence (`⇆ Session connection created — <direction>;
/// Disconnect … undoes it`): the pictogram goes (the band's glyph column is
/// the record's), the head is the title, and each `; ` clause of the tail a
/// detail line.
pub(crate) fn session_connection_created(text: &str) -> Message {
    let text = text.strip_prefix("\u{21c6} ").unwrap_or(text);
    let (title, tail) = text.split_once(" \u{2014} ").unwrap_or((text, ""));
    Message::new(tags::SESSION, Severity::Info, title)
        .lines(tail.split("; ").map(str::to_string))
        .record()
}

/// One control character, made VISIBLE: the band spends exactly one cell per
/// char, so a control byte in a diagnostic neither draws anything nor
/// honestly occupies the cell it took. `paste_banner::sanitized` spells a
/// hostile clipboard's control bytes this way for the same reason; a config
/// diagnostic is the same hazard arriving from the other direction.
const CONTROL_MARK: char = '\u{00b7}';

/// The PHYSICAL rows of a (possibly multi-line) diagnostic — the port of the
/// retired banner's `notice_display_rows` (upstream e7dc1feee). Blank rows are
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
/// the tail after it — `Robi was not dismissed: <error>` reads as the head
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
