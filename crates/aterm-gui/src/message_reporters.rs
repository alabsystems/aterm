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
//! noun-first, with no clauses ([`GLASS_TITLE_WORDS`], [`GLASS_TITLE_CHARS`]);
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
//! failure, the two decisions (Full Disk Access, the admin step), their
//! follow-ups and a handful of disclosures. They are messages now, sorted by
//! the owner's attention rule ([`attention`], ruling 76): the glass carries
//! work in flight, very heavy system use while it lasts, and a decision or a
//! failure the person must act on — everything else is a [`Hold::LogOnly`]
//! record. A gesture the person made that did not happen is ONE shape
//! ([`gesture_failure`]): a few words for a title, the whole error behind
//! Details, a short `HOLD_GESTURE`. (R20, the toolchain offer, went with the
//! sealed seed's `seed-pending:` marker that raised it, Phase 5.)

use std::time::Duration;

use aterm_messages::{
    DETAIL_LINES_CAP, Decision, Glyph, HOLD_ASK, HOLD_GESTURE, Hold, Intent, Load, Message,
    Severity, TITLE_CAP, Tag, tags,
};

use crate::native_settings::SettingsRoute;

/// What earns a row on the glass (the owner's attention rule, 2026-09-23).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the rule as code is read by the tests over every builder (design §10.1, ruling 76)"
    )
)]
pub(crate) enum Attention {
    /// Work in flight, with its animated indicator.
    Progress,
    /// A genuine decision: the row asks, or carries a consequential capsule.
    Decision,
    /// A failure the person must act on.
    Failure,
    /// Everything else: a record, never on the glass.
    Record,
}

/// Glass titles: a few words, verb- or noun-first, no clauses.
pub(crate) const GLASS_TITLE_WORDS: usize = 6;

/// The WORDS of a title: whitespace tokens carrying a letter or a digit, so a
/// route's `▸` and a name's `&` spend no word (review 2026-09-24).
pub(crate) fn title_words(title: &str) -> usize {
    title
        .split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count()
}
/// …and short enough to survive beside two capsules.
pub(crate) const GLASS_TITLE_CHARS: usize = 48;

/// Classify `msg`, or say why it may not be on the glass: a record is a
/// record; a confirmation, an FYI and progress with no indicator are not
/// allowed on the glass; a live row with its indicator — a fill or busy
/// (ruling 139: the indicator is the meter's state, never the hold's) — is
/// progress; a live row that waits on the person (Warn or Error, still) is a
/// failure the person acts on; an ask or a consequential capsule is a
/// decision; a warning or an error a failure.
/// Every non-record title is at most [`GLASS_TITLE_WORDS`] words and
/// [`GLASS_TITLE_CHARS`] characters, carries no clause seam (` — `, `; `,
/// `: `) and does not end in a period.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the rule as code is read by the tests over every builder (design §10.1, ruling 76)"
    )
)]
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
            Attention::Progress
        } else if msg.severity >= Severity::Warn {
            // A live row blocked on the person — the update flow's editor
            // block — is still, and what it asks is the person's to do.
            Attention::Failure
        } else {
            return Err("progress with no indicator");
        }
    } else if msg.is_ask() || msg.actions.iter().any(Intent::is_consequential) {
        Attention::Decision
    } else if matches!(msg.severity, Severity::Warn | Severity::Error) {
        Attention::Failure
    } else {
        return Err("an FYI on glass");
    };
    let title = msg.title.as_str();
    if title_words(title) > GLASS_TITLE_WORDS {
        return Err("a glass title over six words");
    }
    if title.chars().count() > GLASS_TITLE_CHARS {
        return Err("a glass title over 48 characters");
    }
    if [" \u{2014} ", "; ", ": "]
        .iter()
        .any(|seam| title.contains(seam))
    {
        return Err("a clause in a glass title");
    }
    if title.ends_with('.') {
        return Err("a sentence for a glass title");
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
/// platform-neutral (D10) and this build's tests still exercise it.
#[cfg_attr(
    not(any(a11y_tree, test)),
    expect(
        dead_code,
        reason = "the producer is the a11y publisher's panic hook, compiled only with an accessibility tree (D10 keeps the builder platform-neutral)"
    )
)]
pub(crate) const KEY_A11Y_PUBLISHER: &str = "a11y.publisher";

/// The supersede key of the Windows backdrop family (R6): every backdrop
/// decline is one record, whichever site declined it first. The producers are
/// Windows-only; the builder is platform-neutral (D10).
#[cfg_attr(
    not(any(windows, test)),
    expect(
        dead_code,
        reason = "the producers are the Windows backdrop sites (D10 keeps the builder platform-neutral)"
    )
)]
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

/// The `Intent::OpenSystemPane` pane name of the Full Disk Access list — the
/// only pane the codec carries (`messages_host::privacy_pane`).
pub(crate) const PANE_FULL_DISK_ACCESS: &str = "full-disk-access";

/// The key the first-launch admin-step question, its live install row and
/// that install's failure share (R18/R19/R14): the install supersedes the
/// ask, and a failed install supersedes the live row.
pub(crate) const KEY_ADMIN_STEP: &str = "packages.admin-step";

/// The key of the install-posture row (R21): one per launch.
pub(crate) const KEY_INSTALL_POSTURE: &str = "packages.posture";

/// How long the admin install's LIVE row (R19) may go without its pass
/// ending before it folds `Stale`: the backstop for a finish that never
/// arrives, never the install's expected length. Apple's Command Line Tools
/// come through `softwareupdate`, a download of most of a gigabyte, and
/// Homebrew's installer follows them; the pass's own end resolves the row
/// long before this on any working network.
pub(crate) const STALE_ADMIN_INSTALL: Duration = Duration::from_mins(45);

/// The roll-up's tail when a family has more sentences than a message
/// holds lines: where the whole list lives.
const VALIDATOR_HINT: &str = "run `aterm --validate-config` for the full list";

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
            Self::IgnoredKeys if one => "Key has no effect".to_string(),
            Self::IgnoredKeys => format!("{n} keys have no effect"),
            Self::RetiredKeys if one => "Retired config key".to_string(),
            Self::RetiredKeys => format!("{n} retired config keys"),
            Self::UnacceptedValues if one => "Config value not accepted".to_string(),
            Self::UnacceptedValues => format!("{n} config values not accepted"),
            Self::CursorTrail if one => "Cursor trail not applied".to_string(),
            Self::CursorTrail => format!("{n} cursor-trail problems"),
            Self::Fonts if one => "Font not applied".to_string(),
            Self::Fonts => format!("{n} fonts not applied"),
            Self::Assets if one => "Image not loaded".to_string(),
            Self::Assets => format!("{n} images not loaded"),
            Self::Restart => format!("{n} settings apply the next time aterm starts"),
            Self::SecureKeyboard if one => "Secure Keyboard Entry refused".to_string(),
            Self::SecureKeyboard => format!("{n} Secure Keyboard Entry changes refused"),
        }
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
/// is unknown to this aterm build; …` — the title already says unknown, and
/// the line number is the sentence's, behind Details. `None` for a sentence
/// in neither shape.
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
        return (!key.is_empty() && !near.is_empty()).then(|| format!("{key} \u{2192} {near}?"));
    }
    let (key, _) = s.split_once(" is unknown")?;
    let key = bare(key);
    (!key.is_empty()).then_some(key)
}

/// The longest head of `sentence` that ends at a clause seam — before a
/// parenthesised aside (` (`), a semicolon (`; `) or a dash (` — `), OUTSIDE
/// any parenthesis (a seam inside an aside cuts the aside open, not the
/// sentence) — and fits `cap` characters; the whole sentence when none does
/// (the band's width law shapes it from there). The seams are the ones the
/// config sentences use: the near-miss key keeps its `did you mean` and sheds
/// the forward-compatibility aside, the plain unknown key keeps `is unknown to
/// this aterm build` and sheds `it will be preserved …`, a font sentence keeps
/// its verdict and `(not found)` and sheds `; ignored`.
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
        (ConfigFamily::Restart, [only]) => (*only).to_string(),
        _ => family.title(stripped.len()),
    };
    let mut msg = Message::new(tags::CONFIG, family.severity(), title)
        .action(Intent::OpenConfigEditor { line: None })
        .key(family.key())
        .hold(family.hold());
    if family == ConfigFamily::Restart && stripped.len() == 1 {
        return msg;
    }
    let first = stripped.first().copied().unwrap_or_default();
    let excerpt = family.excerpt(first);
    let mut used = 0;
    if excerpt != first {
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
    msg
}

/// The `font_family` the backend build thread could not admit — a config
/// warning like the launch's font row, but minted on another thread after the
/// launch's families were queued, so it carries its OWN key: sharing
/// `config.fonts` would have it supersede the launch's font row and take
/// those sentences off the glass.
pub(crate) fn font_family_rejected(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()]).key("config.font-family")
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
    Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(CRASH_EXCERPT)
        .line(format!("crash log at {path}"))
        .lines(evidence.head.iter().cloned())
        .no_excerpt()
        .action(Intent::OpenPath { path })
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_CRASH)
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
        "aterm.toml not readable"
    } else {
        "aterm.toml not valid"
    };
    let error = notice
        .split_once(" (")
        .and_then(|(_, rest)| rest.rsplit_once(") \u{2014} "))
        .and_then(|(error, _)| diagnostic_lines(error).into_iter().next())
        .filter(|row| !row.is_empty());
    let mut msg = Message::new(tags::CONFIG, Severity::Error, title);
    if let Some(error) = error {
        msg = msg.line(error);
    }
    msg.line("every setting is running at its default")
        .lines(diagnostic_lines(notice))
        .action(Intent::OpenConfigEditor { line: None })
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_LAUNCH_LOAD)
}

/// R5 — a key the CPU renderer cannot honour (`background_opacity`,
/// `background_material`): a DISCLOSURE, so a RECORD — the Manual already
/// marks the key inert while the CPU renderer is active — pointing at the
/// Appearance page where the renderer is chosen.
pub(crate) fn cpu_renderer_no_effect(key: &str, rest: &str) -> Message {
    Message::new(
        tags::RENDER,
        Severity::Info,
        format!("{key} has no effect on the CPU renderer"),
    )
    .line(rest)
    .action(Intent::OpenSettings {
        route: SettingsRoute::Appearance.path().to_string(),
    })
    .hold(Hold::LogOnly)
    .key(format!("render.cpu.{key}").as_str())
}

/// R6 — the Windows client-area backdrop was declined (DirectComposition
/// unavailable, `hdr_glow` on, no GPU, not requested at launch): a RECORD for
/// the family — a blank window is the GPU-lost row's, not this one's.
#[cfg_attr(
    not(any(windows, test)),
    expect(
        dead_code,
        reason = "the producers are the Windows backdrop sites (D10 keeps the builder platform-neutral)"
    )
)]
pub(crate) fn backdrop_declined(title: &str, detail: &str) -> Message {
    Message::new(tags::RENDER, Severity::Info, title)
        .line(detail)
        .action(Intent::OpenConfigEditor { line: None })
        .hold(Hold::LogOnly)
        .key(KEY_BACKDROP)
}

/// R7 — the GPU was lost and the windows created for the backdrop cannot be
/// redrawn by the CPU fallback. The remedy is a gesture (`New window`), so
/// the row STANDS until a window created after the loss paints, or until it
/// is read; a timer must not fold the one instruction that recovers the
/// screen. The `New window` capsule IS that instruction, so the title and
/// the capsule are the row; why and the alternative ride behind Details
/// (review 2026-09-23).
#[cfg_attr(
    not(any(windows, test)),
    expect(
        dead_code,
        reason = "the producer is the Windows-only redirection-bitmap arm of the GPU-loss recovery (D10 keeps the builder platform-neutral)"
    )
)]
pub(crate) fn gpu_lost() -> Message {
    Message::new(tags::RENDER, Severity::Error, "GPU lost")
        .line("windows opened for the backdrop cannot redraw")
        .line("open a new window or restart aterm to get a visible one back")
        .no_excerpt()
        .action(Intent::NewWindow)
        .hold(Hold::Standing)
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
#[cfg_attr(
    not(any(a11y_tree, test)),
    expect(
        dead_code,
        reason = "the producer is the a11y publisher's panic hook, compiled only with an accessibility tree (D10 keeps the builder platform-neutral)"
    )
)]
pub(crate) fn a11y_publisher_dead(reason: &str, at_in_use: bool) -> Message {
    Message::new(tags::A11Y, Severity::Error, "Screen reader access lost")
        .line("restart aterm to retry")
        .line(reason)
        .line("no screen reader can see this window")
        .hold(if at_in_use {
            Hold::Standing
        } else {
            Hold::LogOnly
        })
        .key(KEY_A11Y_PUBLISHER)
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
        (Severity::Warn, format!("{label} may not be saved"))
    } else {
        (Severity::Error, format!("{label} not saved"))
    };
    gesture_failure(tags::FABRIC, severity, &title, detail)
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
            .hold(Hold::LogOnly)
            .key(KEY_FABRIC_HOLD)
    } else {
        gesture_failure(tags::FABRIC, Severity::Warn, "Hold not lifted", WHY).key(KEY_FABRIC_HOLD)
    }
}

/// R10 — the bridge refused the person's Hold press (`ERR …`): the reason is
/// `detail`.
pub(crate) fn hold_refused(detail: &str) -> Message {
    gesture_failure(tags::FABRIC, Severity::Warn, "Hold refused", detail).key(KEY_FABRIC_HOLD)
}

/// R11 — a fabric menu item that could not do its thing (a document that
/// would not open, a thread that would not spawn): a terse title, the cause
/// and the path behind it, keyed by the verb.
pub(crate) fn fabric_failure(verb: &str, title: &str, lines: &[&str]) -> Message {
    let mut msg = gesture_failure(tags::FABRIC, Severity::Error, title, "");
    for line in lines {
        msg = msg.line(*line);
    }
    msg.key(format!("fabric.{verb}").as_str())
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
            .hold(Hold::LogOnly)
            .key(&key)
    } else {
        gesture_failure(tags::FABRIC, Severity::Error, title, detail)
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
    let (title, rest) = if let Some(rest) = words.strip_prefix("Serious Mode was not changed") {
        ("Serious Mode not changed", rest)
    } else if let Some(rest) = words.strip_prefix("Serious Mode was saved but could not be applied")
    {
        ("Serious Mode not applied", rest)
    } else if let Some(rest) =
        words.strip_prefix("Serious Mode was saved, but a newer aterm.toml edit now controls it")
    {
        ("Serious Mode overridden", rest)
    } else {
        (
            "Serious Mode not verified",
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
    if title == "Serious Mode overridden" {
        // What overrode it, first — the capsule is `Open aterm.toml` (review
        // 2026-09-24: the bare title did not say).
        lines.insert(0, "aterm.toml sets it".to_string());
    }
    let mut msg = gesture_failure(tags::CONFIG, Severity::Warn, title, "");
    for line in lines {
        msg = msg.line(line);
    }
    msg.action(Intent::OpenConfigEditor { line: None })
        .key(KEY_SERIOUS_MODE)
}

/// The native config lane's heads, as terse titles (design §10.3, C22's
/// map), and whether the head is the person's own gesture failing (Robi's
/// dismissal). An unknown head is a failed settings change, its whole first
/// row the excerpt.
fn lane_title(head: &str) -> (&'static str, bool, bool) {
    let known = |title, gesture| (title, gesture, true);
    if head.starts_with("Config observation was not valid TOML") {
        known("aterm.toml syntax error", false)
    } else if head.starts_with("Robi was not dismissed") {
        known("Robi not dismissed", true)
    } else if head.starts_with("Config reconciliation failed") {
        known("Settings changes not saved", false)
    } else if head.starts_with("Manual saved aterm.toml, but")
        || head.starts_with("Config was saved, but its exact disk generation")
    {
        known("aterm.toml saved but not applied", false)
    } else if head.starts_with("Config publication could not be verified") {
        known("aterm.toml not verified", false)
    } else if head.starts_with("Config was NOT saved") {
        known("Settings change not saved", false)
    } else {
        ("Settings change failed", false, false)
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
    let msg = rows.fold(msg.line(excerpt), Message::line);
    let msg = msg
        .action(Intent::OpenConfigEditor { line: None })
        .key(KEY_CONFIG_LANE);
    if gesture {
        msg.hold(Hold::For(HOLD_GESTURE))
    } else {
        msg
    }
}

/// R14 — ONE SHAPE FOR "THE PERSON'S OWN GESTURE FAILED" (H11): a failure of
/// their own action earns the glass for a SHORT hold (`HOLD_GESTURE`, 12 s)
/// and nothing more — a title of a few words, the FULL error behind Details
/// (no 160-character cut: the log answers the investigator). Unkeyed unless
/// the caller keys it: a second failed press is a second row's worth of news,
/// folded as a duplicate when the words are the same.
fn gesture_failure(tag: Tag, severity: Severity, title: &str, detail: &str) -> Message {
    // `sentence`, never `line`: the error is someone else's words, kept WHOLE
    // across detail lines (design ruling 64), not clipped at the line cap.
    Message::new(tag, severity, title)
        .sentence(detail)
        .hold(Hold::For(HOLD_GESTURE))
}

/// R14 — `New Tab` did not open one (the spawn failed).
pub(crate) fn new_tab_failed(error: &str) -> Message {
    gesture_failure(tags::WINDOW, Severity::Error, "New tab failed", error).no_excerpt()
}

/// R14 — `New Window` did not open one (the spawn failed).
pub(crate) fn new_window_failed(error: &str) -> Message {
    gesture_failure(tags::WINDOW, Severity::Error, "New window failed", error).no_excerpt()
}

/// R14 — a split the pane cannot hold was refused BEFORE anything spawned.
/// `reason` is the refusal's sentence (`Split refused: this pane is 20x5
/// cells; a vertical split needs at least 20x7`): the two sizes are what the
/// person acts on, so `detail[0]` is `pane 20×5, needs 20×7` and the sentence
/// rides whole behind it (review round 2, 2026-09-23: the excerpt read as a
/// sentence cut mid-thought). A refusal in another shape is its own excerpt.
pub(crate) fn split_refused(reason: &str) -> Message {
    let (_, why) = split_sentence(reason);
    match split_sizes(why) {
        Some(sizes) => {
            gesture_failure(tags::WINDOW, Severity::Error, "Split refused", &sizes).line(why)
        }
        None => gesture_failure(tags::WINDOW, Severity::Error, "Split refused", why),
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
    gesture_failure(tags::WINDOW, Severity::Error, "Split failed", error).no_excerpt()
}

/// R14 — keystrokes typed while a seamless update finished overflowed the
/// pre-Commit queue and were dropped: the person typed them, so they hear it
/// — and `detail[0]` says what to do, as an instruction, not a clause.
pub(crate) fn keystrokes_dropped(dropped: u32) -> Message {
    let what = if dropped == 1 {
        "retype the last key".to_string()
    } else {
        format!("retype the last {dropped} keys")
    };
    gesture_failure(tags::SESSION, Severity::Error, "Keystrokes dropped", &what)
}

/// R14 — a session restore could not create a window and stopped there. The
/// line restates the title, so it rides behind Details.
pub(crate) fn restore_stopped_early() -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Restore stopped early",
        "some saved tabs were not restored",
    )
    .no_excerpt()
}

/// R14 — a shell handed across a seamless update could not be adopted.
pub(crate) fn shell_lost_in_update(error: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Shell lost in the update",
        &format!("a live shell was lost across the update: {error}"),
    )
    .no_excerpt()
}

/// R14 — a restored tab could not start its shell.
pub(crate) fn restored_tab_failed(error: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Restored tab failed",
        &format!("a restored tab could not start its shell: {error}"),
    )
    .no_excerpt()
}

/// R14 — the admin step's *Install* was refused before it started (an inert
/// manager, a packages verb already running): the press is never silently
/// swallowed, and the reason — which says to wait — is `detail[0]`.
pub(crate) fn admin_install_refused(message: &str) -> Message {
    gesture_failure(
        tags::PACKAGES,
        Severity::Error,
        "Admin install did not start",
        message,
    )
}

/// R14 — the admin install STARTED and its pass ended in failure: the
/// outcome of the person's own press, so it supersedes the live install row
/// (the shared [`KEY_ADMIN_STEP`]) for a gesture's short hold, the reason
/// behind Details. Settings ▸ Packages carries the rest of the story.
pub(crate) fn admin_install_failed(message: &str) -> Message {
    gesture_failure(
        tags::PACKAGES,
        Severity::Error,
        "Admin install failed",
        message,
    )
    .no_excerpt()
    .action(Intent::OpenSettings {
        route: SettingsRoute::Packages.path().to_string(),
    })
    .key(KEY_ADMIN_STEP)
}

/// The Open-log press refused (the path was not a regular file under the log
/// dir, or nothing could be spawned to open it): the person's gesture, so a
/// short row naming the path behind Details.
pub(crate) fn log_did_not_open(path: &str) -> Message {
    gesture_failure(tags::SYSTEM, Severity::Error, "Log did not open", path).no_excerpt()
}

/// R15 — THE FULL DISK ACCESS QUESTION (`consent_card`'s decision; the row
/// the card posts once and watches by id). A DECISION row: it ranks first
/// and holds for `HOLD_ASK` (ten minutes) until a capsule answers it. Info,
/// not Warn — a quiet question, never an alarm (the copy fence forbids alarm
/// words). The title is the ask ([`crate::consent_card::FDA_TITLE`]) and the
/// row paints it alone: [`crate::consent_card::FDA_DETAIL`]'s hedge, the
/// route in words and what aterm does next wait behind Details (review round
/// 2, 2026-09-23: a status title with a hedge for its excerpt never said
/// what it asked).
pub(crate) fn file_access_question() -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Info,
        crate::consent_card::FDA_TITLE,
    )
    .line(crate::consent_card::FDA_DETAIL)
    .line(crate::menu::privacy_settings_path_words(
        crate::menu::PrivacyPane::FullDiskAccess,
    ))
    .line("aterm keeps checking; nothing here grants anything")
    .no_excerpt()
    .action(Intent::OpenSystemPane {
        pane: PANE_FULL_DISK_ACCESS.to_string(),
    })
    .action(Intent::NotNow {
        decision: Decision::FileAccess,
    })
    .hold(Hold::Ask { for_: HOLD_ASK })
    .key(KEY_FILE_ACCESS)
}

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
    .hold(Hold::LogOnly)
    .key(KEY_FILE_ACCESS)
}

/// A program the admin step installs, by the name a person knows it by —
/// the title's words; the vendor's whole line (`admin_vendor_line`) rides
/// behind Details.
fn admin_program_name(name: &str) -> &str {
    match name {
        "clt" => "Command Line Tools",
        "brew" => "Homebrew",
        other => other,
    }
}

/// `clt`, `brew` → `Command Line Tools and Homebrew`; three or more →
/// `A, B and C` (door order).
fn admin_program_list(names: &[String]) -> String {
    let named: Vec<&str> = names.iter().map(|n| admin_program_name(n)).collect();
    match named.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        Some((last, _)) => (*last).to_string(),
        None => String::new(),
    }
}

/// A title naming `names` programs when there are at most two and the
/// words fit the glass title's cap, else `fallback` (the count).
fn capped_title(names: usize, text: String, fallback: impl FnOnce() -> String) -> String {
    if names <= 2
        && title_words(&text) <= GLASS_TITLE_WORDS
        && text.chars().count() <= GLASS_TITLE_CHARS
    {
        text
    } else {
        fallback()
    }
}

/// R18 — THE FIRST-LAUNCH ADMIN STEP: one or more index programs need an
/// administrator (`needs admin — run: aterm pkg install <name>`) and the
/// unattended pass can never supply the password. A DECISION row — *Install*
/// runs the `--elevate=osascript` door (macOS's own dialog), *Not now*
/// records a dismissal for this exact set — held for `HOLD_ASK`. The TITLE
/// names what the decision installs — `Install Command Line Tools and
/// Homebrew` (or `Install 3 programs` past the cap): at 80 columns an excerpt
/// asked the person to approve an unnamed install (review 2026-09-23). What
/// Install does and each vendor's installer ride behind Details.
pub(crate) fn admin_step(names: &[String]) -> Message {
    let title = capped_title(
        names.len(),
        format!("Install {}", admin_program_list(names)),
        || format!("Install {} programs", names.len()),
    );
    Message::new(tags::PACKAGES, Severity::Info, title)
        .line("Install opens macOS's own password dialog")
        .lines(
            names
                .iter()
                .map(|n| crate::packages_screen::admin_vendor_line(n)),
        )
        .action(Intent::InstallElevated {
            names: names.to_vec(),
        })
        .action(Intent::NotNow {
            decision: Decision::AdminStep {
                names: names.to_vec(),
            },
        })
        .no_excerpt()
        .hold(Hold::Ask { for_: HOLD_ASK })
        .key(KEY_ADMIN_STEP)
}

/// The admin install's title: what is coming, by name, in the words its own
/// question asked — `Installing Command Line Tools and Homebrew` — or, past
/// two names or the glass title's cap, the count (`Installing 3 programs`).
fn admin_install_title(names: &[String]) -> String {
    capped_title(
        names.len(),
        format!("Installing {}", admin_program_list(names)),
        || format!("Installing {} programs", names.len()),
    )
}

/// R19 — the admin install was accepted and is RUNNING (ruling 148): work in
/// flight and very heavy system use (macOS's own installer loads disk and
/// CPU for minutes), so a LIVE row, BUSY (ruling 139) with the system load
/// declared, naming what is coming, until the packages pass that carries it
/// ends: the pass's outcome resolves it (`App::finish_admin_install`), a
/// failure superseding it with [`admin_install_failed`]. It supersedes the
/// question by the shared key. `STALE_ADMIN_INSTALL` is only the backstop for
/// an end that never arrives. Its ONE detail (ruling 62) is the one thing the
/// person must do — the password macOS's own dialog asks for (2026-09-23) —
/// painted, because it changes what the person does (ruling 77). No
/// capsule: work in flight is not a decision (ruling 101), and a `Packages`
/// capsule cut the title to `Installing Com…` at 60 columns (review
/// 2026-09-24); the failure keeps it.
pub(crate) fn admin_install_started(names: &[String]) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, admin_install_title(names))
        .glyph(Glyph::or_fallback('\u{21e3}'))
        .line("enter your password in the macOS dialog")
        .meter(aterm_messages::Meter {
            load: Some(Load::System),
            ..aterm_messages::Meter::busy("")
        })
        .hold(Hold::Live {
            stale_after: STALE_ADMIN_INSTALL,
        })
        .key(KEY_ADMIN_STEP)
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
    .hold(Hold::LogOnly)
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
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(
        dead_code,
        reason = "the producer is the macOS first-open doctor in `resumed` (D10 keeps the builder platform-neutral)"
    )
)]
pub(crate) fn install_posture(
    posture: aterm_update::which_copy::InstallPosture,
) -> Option<Message> {
    use aterm_update::which_copy::InstallPosture;
    let remedy = posture.remedy()?;
    let where_ = match posture {
        InstallPosture::MountedImage => "running from the disk image",
        InstallPosture::Translocated => "running from a temporary copy",
        InstallPosture::Installed | InstallPosture::NotABundle => return None,
    };
    Some(
        Message::new(tags::PACKAGES, Severity::Warn, "Move aterm to Applications")
            .line(where_)
            .line(remedy)
            .line(posture.summary())
            .hold(Hold::For(HOLD_ASK))
            .key(KEY_INSTALL_POSTURE),
    )
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
        .hold(Hold::LogOnly)
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
mod tests {
    use super::*;

    fn sentences(msg: &Message) -> Vec<String> {
        std::iter::once(msg.title.clone())
            .chain(msg.detail.iter().cloned())
            .collect()
    }

    /// Every message a launch or reload can post, from fixtures that stand
    /// in for the real producers' words.
    fn every_reporter_message() -> Vec<Message> {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        warns.push(
            ConfigFamily::Keybindings,
            "config key_sequences: skipping \"ctrl+a b\": bad chord".into(),
        );
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\" — did you mean \"window_padding\"?"
                .into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "columns/lines applies on next launch (resize the window to change size now)".into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
        );
        warns.push(
            ConfigFamily::Fonts,
            "config font_family_bold: \"Nope\" is not an admissible font (not found); ignored"
                .into(),
        );
        crate::app_config::collect_key_notices(
            &mut warns,
            "game_font = \"chunky\"\n[packages]\nenabled = true\nauto_update = false\n\
             seed_install = true\n",
        );
        let mut all = warns.into_messages();
        all.push(crash_message(&crate::logging::CrashEvidence {
            path: std::path::PathBuf::from("/Users//_an/Library/Logs/aterm/crash-1-1.log.seen"),
            head: vec!["aterm-gui 0.1.0 crashed at unix 1.000".into()],
        }));
        all.push(launch_load_failure(
            "aterm.toml is not a valid configuration (expected `=` at line 3) — every \
             setting is running at its default. Fix /home/ana/.config/aterm.toml and it loads on \
             the next change.",
        ));
        all.push(cpu_renderer_no_effect(
            "background_opacity",
            "it has no translucent present path, so the window stays solid",
        ));
        all.push(backdrop_declined(
            "background_material is styling the title bar only",
            "this display stack refused the DirectComposition backdrop swapchain",
        ));
        all.push(gpu_lost());
        all.push(a11y_publisher_dead("Server GUID mismatch", true));
        all.push(a11y_publisher_dead("Server GUID mismatch", false));
        all.push(presence_not_saved(
            "presence.band",
            "Presence Band",
            "the write was refused",
            false,
        ));
        all.push(presence_not_saved(
            "presence.rim",
            "Presence Rim",
            "the write could not be verified",
            true,
        ));
        all.push(fleet_hold(true));
        all.push(fleet_hold(false));
        all.push(hold_refused("no session"));
        all.push(fabric_failure(
            "document",
            "Document did not open",
            &["no such file", "~/x.md"],
        ));
        all.push(fabric_failure(
            "on",
            "Fabric On did not start",
            &["cannot spawn its thread"],
        ));
        all.push(fabric_status("on", "aterm fabric on: done.", "", true));
        all.push(fabric_status("off", "Fabric Off failed", "exited 2", false));
        all.push(serious_mode_feedback(
            "Serious Mode was not changed: aterm.toml changed first",
        ));
        all.push(serious_mode_feedback(
            "Serious Mode may have been written but could not be verified; reload before \
             retrying: the disk generation moved",
        ));
        for head in [
            "Config observation was not valid TOML: expected `]`",
            "Robi was not dismissed: the settings lane dropped the request",
            "Config reconciliation failed; queued changes were not written: io",
            "Manual saved aterm.toml, but its exact generation could not be admitted: x",
            "Config publication could not be verified: x",
            "Config was NOT saved: x",
            "Something new went wrong: x",
        ] {
            all.push(config_lane_error(head));
        }
        all.push(log_did_not_open("/x/y.log"));
        all.push(font_family_rejected(
            "font_family: \"Nope\" is not an admissible font",
        ));
        // R14–R22, the retired toast's reporters.
        all.extend(gesture_failures());
        all.push(file_access_question());
        all.push(file_access_granted());
        let both = ["clt".to_string(), "brew".to_string()];
        all.push(admin_step(&both));
        all.push(admin_install_started(&both));
        all.push(admin_install_failed("User canceled. (-128)"));
        for posture in [
            aterm_update::which_copy::InstallPosture::MountedImage,
            aterm_update::which_copy::InstallPosture::Translocated,
        ] {
            all.push(install_posture(posture).expect("a posture to fix"));
        }
        for kind in [
            crate::connections::ConnectedSpawnKind::Controlled,
            crate::connections::ConnectedSpawnKind::Controller,
        ] {
            all.push(session_connection_created(
                &crate::connections::first_use_notice_text(kind),
            ));
        }
        all.push(session_connection_created(
            &crate::connections::first_use_connect_notice_text(true, false),
        ));
        all
    }

    /// A GESTURE FAILURE KEEPS THE WHOLE ERROR (design ruling 64): a 600-char
    /// error — longer than one detail line — is every word behind Details, in
    /// order, never clipped at the line cap.
    #[test]
    fn a_gesture_failure_keeps_the_whole_error() {
        let error = format!(
            "posix_spawn failed: {} (see https://example.invalid/aterm/spawn for the \
             limits); errno 35",
            "resource temporarily unavailable ".repeat(16)
        );
        assert!(error.chars().count() > aterm_messages::DETAIL_LINE_CAP);
        let m = new_tab_failed(&error);
        assert!(m.detail.len() > 1, "{:?}", m.detail);
        let normalized: String = error.split_whitespace().collect::<Vec<_>>().join(" ");
        let rejoined: String = m
            .detail
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(rejoined, normalized, "every word, in order");
        assert!(
            m.detail
                .iter()
                .all(|l| l.chars().count() <= aterm_messages::DETAIL_LINE_CAP)
        );
    }

    /// Every R14 row, from the sentences their sites really pass.
    fn gesture_failures() -> Vec<Message> {
        vec![
            new_tab_failed("fork: Resource temporarily unavailable (os error 35)"),
            new_window_failed("the pty could not be opened"),
            split_refused(
                "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
            ),
            split_refused("Split refused: this pane's size could not be measured"),
            split_failed("no event loop to host the new pane"),
            keystrokes_dropped(3),
            restore_stopped_early(),
            shell_lost_in_update("descriptor 9 was closed"),
            restored_tab_failed("exec failed"),
            admin_install_refused("a packages operation is already running"),
        ]
    }

    /// THE COPY OF THE ONE QUESTION THIS FEATURE ASKS UNPROMPTED (the fence
    /// that held the retired card's caption, `notice.rs`, moved onto the
    /// words themselves — design §7.1). Three fences at once: the owner's
    /// restart-phrase ruling (`tools/grep_guard.sh` B10/B12), the honesty
    /// rule that no coverage or scope claim may ship while §7 S1 and S4 are
    /// unrun, and the ruling that mitigating this annoyance is acceptable —
    /// so it may never read as elimination. The title and `detail[0]` are
    /// held under 75 characters together so the ask survives beside its two
    /// capsules on an ordinary window, and the primary capsule's label opens
    /// a pane without reading as granting anything.
    #[test]
    fn the_file_access_message_is_honest_and_fence_clean() {
        use crate::consent_card::{FDA_DETAIL, FDA_TITLE};
        let msg = file_access_question();
        assert_eq!(msg.title, FDA_TITLE, "the row carries the const");
        assert_eq!(msg.detail[0], FDA_DETAIL);
        for text in std::iter::once(&msg.title).chain(msg.detail.iter()) {
            let lower = text.to_ascii_lowercase();
            // 1. The restart-phrase fence, in the shapes B12 scans for.
            for banned in [
                "restart",
                "relaunch",
                "re-launch",
                "reopen",
                "re-open",
                "quit and",
                "next launch",
                "reboot",
                "launch aterm again",
            ] {
                assert!(!lower.contains(banned), "{banned:?} in {text:?}");
            }
            // 2. No coverage claim and no scope sentence: which folders a
            //    grant reaches is S4's measurement and how far it reaches is
            //    S1's, and NEITHER has been run.
            for unmeasured in [
                "cover",
                "documents",
                "desktop",
                "downloads",
                "volume",
                "icloud",
                "retires",
                "this process",
                "new processes",
                "every folder",
                "all folders",
            ] {
                assert!(!lower.contains(unmeasured), "{unmeasured:?} in {text:?}");
            }
            // 3. Mitigate, never eliminate (owner's ruling).
            for promise in [
                "never again",
                "no more",
                "eliminat",
                "not be interrupted",
                "never be interrupted",
                "without interruption",
                "stops the",
                "ends the",
            ] {
                assert!(!lower.contains(promise), "{promise:?} in {text:?}");
            }
        }
        // 4. The observed access, with the owner's already-enabled switch
        //    explicitly possible: a failed probe does not read the switch.
        assert!(FDA_DETAIL.contains("Full Disk Access"));
        assert!(FDA_DETAIL.contains("may already be enabled"));
        assert!(
            FDA_TITLE.chars().count() + FDA_DETAIL.chars().count() <= 75,
            "short enough to survive beside two capsules on an ordinary window"
        );
        assert!(
            FDA_TITLE.ends_with('?') && !msg.excerpt,
            "the title is the ask and the row paints it alone; the hedge is behind Details"
        );
        // 5. A decision: the primary opens a pane — its label grants nothing —
        //    and Not now answers; an Ask hold, the shared key, a quiet tone.
        assert_eq!(
            msg.actions,
            [
                Intent::OpenSystemPane {
                    pane: PANE_FULL_DISK_ACCESS.to_string()
                },
                Intent::NotNow {
                    decision: Decision::FileAccess
                }
            ]
        );
        let label = msg.actions[0].label().to_ascii_lowercase();
        for grant in ["grant", "allow", "enable", "turn on"] {
            assert!(!label.contains(grant), "{label:?} reads as granting");
        }
        assert_eq!(msg.actions[0].label(), "Open Settings");
        assert_eq!(msg.hold, Hold::Ask { for_: HOLD_ASK });
        assert_eq!(msg.key.as_deref(), Some(KEY_FILE_ACCESS));
        assert_eq!(msg.severity, Severity::Info, "a question, never an alarm");
        assert_eq!(msg.tag, tags::PRIVACY);
        // 6. The route in words is behind Details, whole.
        assert!(
            msg.detail.iter().any(|l| l
                == crate::menu::privacy_settings_path_words(
                    crate::menu::PrivacyPane::FullDiskAccess
                )),
            "{:?}",
            msg.detail
        );
    }

    /// R14 UNDER THE OWNER'S ATTENTION RULE (2026-09-23): a failure of the
    /// person's own gesture is a row — Error, a short `HOLD_GESTURE` — with a
    /// title of a few words and the WHOLE error behind Details, never cut.
    /// No key: each is an event of its own.
    #[test]
    fn a_failed_gesture_is_a_short_row_with_the_whole_error_behind_details() {
        let long = "x".repeat(200);
        let msg = new_tab_failed(&long);
        assert_eq!(msg.title, "New tab failed");
        assert_eq!(
            msg.detail,
            std::slice::from_ref(&long),
            "the error whole, not cut at 160"
        );
        assert!(
            !msg.excerpt,
            "a spawn's error does not change what the person does"
        );
        for msg in gesture_failures() {
            assert_eq!(msg.severity, Severity::Error, "{msg:?}");
            assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "{msg:?}");
            assert!(msg.key.is_none(), "an event, not a slot: {msg:?}");
            assert!(
                msg.title.split_whitespace().count() <= 5,
                "a few words: {:?}",
                msg.title
            );
            assert!(!msg.title.contains(':'), "no clause: {:?}", msg.title);
            assert!(!msg.detail.is_empty(), "the reason is behind Details");
        }
        let refused = split_refused(
            "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
        );
        assert_eq!(refused.title, "Split refused");
        assert_eq!(
            refused.detail,
            [
                "pane 20\u{00d7}5, needs 20\u{00d7}7",
                "this pane is 20x5 cells; a vertical split needs at least 20x7"
            ],
            "the sizes are the excerpt; the sentence rides whole behind them"
        );
        assert_eq!(
            split_refused("Split refused: this pane's size could not be measured").detail,
            ["this pane's size could not be measured"],
            "another shape is its own excerpt"
        );
        assert_eq!(keystrokes_dropped(3).title, "Keystrokes dropped");
        assert_eq!(
            keystrokes_dropped(3).detail[0],
            "retype the last 3 keys",
            "what to do is the excerpt"
        );
        assert_eq!(keystrokes_dropped(1).detail[0], "retype the last key");
        assert!(keystrokes_dropped(3).excerpt);
        assert!(
            !restore_stopped_early().excerpt,
            "its line restates the title: behind Details"
        );
        // The install's own failure supersedes its live row by key, and
        // points at the page with the rest of the story.
        let failed = admin_install_failed("User canceled.");
        assert_eq!(failed.key.as_deref(), Some(KEY_ADMIN_STEP));
        assert_eq!(failed.hold, Hold::For(HOLD_GESTURE));
        assert_eq!(
            failed.actions,
            [Intent::OpenSettings {
                route: "/packages".to_string()
            }]
        );
    }

    /// THE REST OF THE RETIRED TOAST UNDER THE ATTENTION RULE: the two
    /// questions are decision rows; the admin install is LIVE work in flight
    /// naming what is coming; a crippled install is a row only when there is
    /// something to fix; the grant and the connection disclosure are records that
    /// never touch the glass.
    #[test]
    fn decisions_and_work_in_flight_are_rows_and_the_rest_are_records() {
        let both = ["clt".to_string(), "brew".to_string()];
        // R18 — a decision naming what it installs in its TITLE.
        let ask = admin_step(&both);
        assert_eq!(ask.title, "Install Command Line Tools and Homebrew");
        assert!(
            !ask.excerpt,
            "the title and the capsules alone on the glass"
        );
        assert_eq!(ask.detail[0], "Install opens macOS's own password dialog");
        assert!(
            ask.detail[1].starts_with("Apple Command Line Tools"),
            "{:?}",
            ask.detail
        );
        assert_eq!(admin_step(&both[..1]).title, "Install Command Line Tools");
        let three = ["clt".to_string(), "brew".to_string(), "xquartz".to_string()];
        assert_eq!(admin_step(&three).title, "Install 3 programs");
        assert_eq!(
            ask.actions,
            [
                Intent::InstallElevated {
                    names: both.to_vec()
                },
                Intent::NotNow {
                    decision: Decision::AdminStep {
                        names: both.to_vec()
                    }
                }
            ]
        );
        assert_eq!(ask.hold, Hold::Ask { for_: HOLD_ASK });
        assert_eq!(ask.key.as_deref(), Some(KEY_ADMIN_STEP));
        // R19 — live work in flight, naming the goal; supersedes the ask.
        let started = admin_install_started(&both);
        assert_eq!(started.title, "Installing Command Line Tools and Homebrew");
        assert_eq!(
            started.hold,
            Hold::Live {
                stale_after: STALE_ADMIN_INSTALL
            }
        );
        assert_eq!(started.key.as_deref(), Some(KEY_ADMIN_STEP));
        let meter = started.meter.as_ref().expect("the comet");
        assert_eq!(meter.fill_permille, None, "indeterminate");
        assert!(meter.busy, "work in flight declares busy (ruling 139)");
        assert_eq!(meter.load, Some(Load::System), "macOS's installer is heavy");
        assert!(
            started.excerpt,
            "the password prompt changes what the person does"
        );
        assert_eq!(
            started.detail,
            ["enter your password in the macOS dialog"],
            "the one thing the person must do is the only detail (ruling 62)"
        );
        assert!(
            started.actions.is_empty(),
            "work in flight is not a decision: no capsule to cut the title (ruling 101)"
        );
        assert_eq!(
            admin_install_started(&both[1..]).title,
            "Installing Homebrew"
        );
        // R21 — a row only where the person has something to fix.
        use aterm_update::which_copy::InstallPosture;
        assert!(install_posture(InstallPosture::Installed).is_none());
        assert!(install_posture(InstallPosture::NotABundle).is_none());
        let dmg = install_posture(InstallPosture::MountedImage).expect("a fix");
        assert_eq!(
            dmg.title, "Move aterm to Applications",
            "the instruction is the title"
        );
        assert_eq!(dmg.detail[0], "running from the disk image");
        assert!(
            dmg.detail[1].starts_with("Drag aterm to Applications and open it from there")
                && dmg.detail[1].contains("cannot update itself"),
            "the remedy whole: {:?}",
            dmg.detail
        );
        assert_eq!(dmg.detail[2], InstallPosture::MountedImage.summary());
        assert_eq!(dmg.severity, Severity::Warn);
        assert!(
            dmg.actions.is_empty(),
            "no page of aterm's performs the fix"
        );
        assert_eq!(dmg.key.as_deref(), Some(KEY_INSTALL_POSTURE));
        let moved = install_posture(InstallPosture::Translocated).expect("a fix");
        assert_eq!(moved.title, "Move aterm to Applications");
        assert_eq!(moved.detail[0], "running from a temporary copy");
        // R17, R22 — records (R20 went with the sealed seed, Phase 5).
        for record in [
            file_access_granted(),
            session_connection_created(&crate::connections::first_use_notice_text(
                crate::connections::ConnectedSpawnKind::Controlled,
            )),
        ] {
            assert_eq!(record.hold, Hold::LogOnly, "{record:?}");
        }
        assert_eq!(file_access_granted().key.as_deref(), Some(KEY_FILE_ACCESS));
        let connected = session_connection_created(&crate::connections::first_use_notice_text(
            crate::connections::ConnectedSpawnKind::Controlled,
        ));
        assert_eq!(connected.title, "Session connection created");
        assert_eq!(connected.tag, tags::SESSION);
        assert!(
            connected.detail[0].starts_with("this session can now type into"),
            "{:?}",
            connected.detail
        );
        assert!(
            connected.detail[1].starts_with("Disconnect"),
            "{:?}",
            connected.detail
        );
    }

    /// The artifact's path is a detail line (whole, absolute) behind Details,
    /// never the title — the title is the fact, the path is where to look —
    /// and never on the glass (design ruling 74: `detail[0]` says a report
    /// was saved, in words, and even that is not painted, ruling 145); the
    /// head follows it; `Open log` carries the same path.
    #[test]
    fn the_crash_row_names_the_artifact_in_its_detail_not_its_title() {
        let path = "/Users//_an/Library/Logs/aterm/crash-signal-1-1.log.seen";
        let msg = crash_message(&crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(path),
            head: vec!["aterm-gui 0.1.0 crashed".into(), "fatal signal 11".into()],
        });
        assert_eq!(msg.tag, tags::CRASH);
        assert_eq!(msg.severity, Severity::Error);
        assert_eq!(msg.title, "aterm crashed last time");
        assert!(!msg.title.contains('/'), "the title names no path");
        assert!(!msg.excerpt, "the row paints its title alone");
        assert_eq!(msg.detail[0], CRASH_EXCERPT, "the first line is words");
        assert!(
            !msg.detail[0].contains('/') && !msg.detail[0].contains(".log"),
            "the first line names no file: {}",
            msg.detail[0]
        );
        assert_eq!(msg.detail[1], format!("crash log at {path}"));
        assert_eq!(
            &msg.detail[2..],
            ["aterm-gui 0.1.0 crashed", "fatal signal 11"]
        );
        assert_eq!(
            msg.actions,
            [Intent::OpenPath {
                path: path.to_string()
            }]
        );
        assert_eq!(msg.hold, Hold::For(HOLD_LAUNCH));
        assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
    }

    /// One message per family: a TERSE title — singular for one sentence, a
    /// count for several — the first sentence's head as `detail[0]`, every
    /// sentence whole behind it (the excerpt not repeated when it IS the
    /// sentence), a repeat folded, past the line cap the roll-up, and the
    /// family's key and the editor capsule on every one (design §10.3 C1–C9,
    /// §10.5 H9; this replaces the ruling-29 head tests).
    #[test]
    fn config_family_titles_are_terse_and_the_first_sentence_is_the_excerpt() {
        let mut warns = ConfigWarnings::default();
        assert!(
            warns.clone().into_messages().is_empty(),
            "nothing collected, nothing posted"
        );
        warns.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        warns.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\"".into(),
        );
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 9: unknown key \"colums\"".into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
        );
        assert_eq!(
            warns.sentences().collect::<Vec<_>>(),
            [
                "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"",
                "config line 3: unknown key \"windw_padding\"",
                "config line 9: unknown key \"colums\"",
                "gpu applies on next launch (the renderer backend is chosen at startup)",
            ],
            "the echo is verbatim and in collection order, repeats folded"
        );
        assert_eq!(warns.told().len(), 4);
        let msgs = warns.into_messages();
        assert_eq!(msgs.len(), 3, "one per family");
        let kb = &msgs[0];
        assert_eq!(kb.title, "Keybinding skipped");
        assert_eq!(
            kb.detail,
            [
                "ctrl+x: no action foo",
                "\"ctrl+x\": unknown action \"foo\""
            ],
            "the chord is the subject, unquoted; the title already says skipped"
        );
        assert_eq!(kb.severity, Severity::Warn);
        assert_eq!(kb.key.as_deref(), Some("config.keybindings"));
        assert_eq!(kb.actions, [Intent::OpenConfigEditor { line: None }]);
        let keys = &msgs[1];
        assert_eq!(keys.title, "2 keys have no effect");
        assert_eq!(
            keys.detail,
            [
                "line 3: unknown key \"windw_padding\"",
                "line 9: unknown key \"colums\""
            ]
        );
        let restart = &msgs[2];
        assert_eq!(restart.hold, Hold::LogOnly, "a waiting edit is a record");
        assert!(restart.title.starts_with("gpu applies"));
        assert_eq!(restart.key.as_deref(), Some("config.restart"));

        // Every family's title, both forms.
        for (family, one, many) in [
            (
                ConfigFamily::Keybindings,
                "Keybinding skipped",
                "3 keybindings skipped",
            ),
            (
                ConfigFamily::IgnoredKeys,
                "Key has no effect",
                "3 keys have no effect",
            ),
            (
                ConfigFamily::RetiredKeys,
                "Retired config key",
                "3 retired config keys",
            ),
            (
                ConfigFamily::UnacceptedValues,
                "Config value not accepted",
                "3 config values not accepted",
            ),
            (
                ConfigFamily::CursorTrail,
                "Cursor trail not applied",
                "3 cursor-trail problems",
            ),
            (
                ConfigFamily::Fonts,
                "Font not applied",
                "3 fonts not applied",
            ),
            (
                ConfigFamily::Assets,
                "Image not loaded",
                "3 images not loaded",
            ),
            (
                ConfigFamily::SecureKeyboard,
                "Secure Keyboard Entry refused",
                "3 Secure Keyboard Entry changes refused",
            ),
        ] {
            assert_eq!(family.title(1), one, "{family:?}");
            assert_eq!(family.title(3), many, "{family:?}");
        }

        // A font verdict's head sheds `; ignored`, and the sentence rides
        // whole behind it.
        let face = "f".repeat(20);
        let mut one = ConfigWarnings::default();
        one.push(
            ConfigFamily::Fonts,
            format!(
                "config font_family: \"{face}\" is not an admissible font (not found); ignored"
            ),
        );
        let msg = one.into_messages().remove(0);
        assert_eq!(msg.title, "Font not applied");
        assert_eq!(
            msg.detail[0],
            format!("font_family: \"{face}\" is not an admissible font (not found)")
        );
        assert_eq!(
            msg.detail[1],
            format!("font_family: \"{face}\" is not an admissible font (not found); ignored")
        );
        let font = font_family_rejected("font_family: \"Nope\" is not an admissible font");
        assert_eq!(font.title, "Font not applied");
        assert_eq!(
            font.detail,
            ["font_family: \"Nope\" is not an admissible font"]
        );
        assert_eq!(font.key.as_deref(), Some("config.font-family"));

        // Past the engine's line cap the last line is the roll-up.
        let mut many = ConfigWarnings::default();
        many.extend(
            ConfigFamily::IgnoredKeys,
            (0..(DETAIL_LINES_CAP + 5)).map(|i| format!("config line {i}: unknown key \"k{i}\"")),
        );
        let msg = many.into_messages().remove(0);
        assert_eq!(
            msg.title,
            format!("{} keys have no effect", DETAIL_LINES_CAP + 5)
        );
        assert_eq!(msg.detail.len(), DETAIL_LINES_CAP);
        assert_eq!(
            msg.detail[DETAIL_LINES_CAP - 1],
            format!("\u{2026} and 6 more \u{2014} {VALIDATOR_HINT}")
        );
        assert_eq!(
            msg.detail[DETAIL_LINES_CAP - 2],
            format!(
                "line {}: unknown key \"k{}\"",
                DETAIL_LINES_CAP - 2,
                DETAIL_LINES_CAP - 2
            )
        );
    }

    /// A RETIRED SPELLING IS NOT AN UNKNOWN KEY (review of the second
    /// origin/main merge, 2026-09-23; design ruling 42). atpkg still reads
    /// `[packages] auto_update = false` as `enabled = false`, and
    /// `seed_install` as `auto_install` while that is unset. They are their
    /// own family: a record that never reaches the glass, whose words say
    /// what they are. The sentences are the real ones
    /// (`app_config::collect_key_notices`, the launch's and every reload's
    /// seam), so the pin cannot drift from the validator's words.
    #[test]
    fn a_retired_spelling_is_a_record_and_never_counted_as_an_unknown_key() {
        let src = "[packages]\nauto_update = false\nseed_install = false\n";
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(&mut warns, src);
        let msgs = warns.into_messages();
        assert_eq!(
            msgs.len(),
            1,
            "one family, and it is not the ignored keys: {msgs:?}"
        );
        let retired = &msgs[0];
        // Both of the ignored-key family's wordings are false for a spelling that
        // is still READ: refusing only the old one would let this pass vacuously
        // the day the family was reworded.
        assert!(
            !retired.title.contains("nknown")
                && !retired.title.contains("not known to this build")
                && !retired.title.contains("no effect"),
            "a working opt-out called unknown or inert: {:?}",
            retired.title
        );
        assert_eq!(retired.title, "2 retired config keys");
        assert_eq!(retired.key.as_deref(), Some("config.retired-keys"));
        assert_eq!(retired.hold, Hold::LogOnly, "a record, never on glass");
        assert_eq!(retired.severity, Severity::Info, "nothing is broken");
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("it is read as `enabled = false`")),
            "{:?}",
            retired.detail
        );
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("the old key is still read")),
            "{:?}",
            retired.detail
        );

        // Beside a typo, the deprecated `game_font` and the retired
        // `channel`: the typo and `channel` — which selects nothing — are the
        // keys that do nothing, one row; `game_font` and `auto_update`, which
        // still apply, are the record.
        let src = "game_font = \"chunky\"\nwindw_padding = 20\n\
                   [packages]\nenabled = true\nauto_update = false\nchannel = \"stable\"\n";
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(&mut warns, src);
        assert_eq!(
            warns.sentences().count(),
            4,
            "the stderr echo still says every one: {:?}",
            warns.told()
        );
        let msgs = warns.into_messages();
        assert_eq!(msgs.len(), 2, "{msgs:?}");
        let ignored = &msgs[0];
        assert_eq!(ignored.key.as_deref(), Some("config.ignored-keys"));
        assert_eq!(ignored.hold, Hold::Default);
        assert_eq!(ignored.severity, Severity::Warn);
        assert_eq!(ignored.title, "2 keys have no effect");
        assert!(ignored.detail.iter().any(|l| l.contains("windw_padding")));
        assert!(
            ignored
                .detail
                .iter()
                .any(|l| l.contains("packages.channel has no effect"))
        );
        assert!(
            ignored
                .detail
                .iter()
                .all(|l| !l.contains("game_font") && !l.contains("auto_update")),
            "{:?}",
            ignored.detail
        );
        let retired = &msgs[1];
        assert_eq!(retired.key.as_deref(), Some("config.retired-keys"));
        assert_eq!(retired.hold, Hold::LogOnly);
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("`game_font` is deprecated"))
        );
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("it still keeps automatic updates off")),
            "{:?}",
            retired.detail
        );

        // `--validate-config` still lists every key that needs an edit.
        let all = crate::native_config_language::ignored_key_warnings(src);
        assert_eq!(all.len(), 4, "{all:?}");
    }

    /// THE COMMONEST CONFIG WARNING — one misspelled key: the title is terse
    /// and the near-miss spelling is the excerpt (the head up to the
    /// forward-compatibility aside), with the whole sentence behind Details.
    /// The sentence is the real one — `app_config::ignored_key_notices` over
    /// the text a live edit would save — so the fixture cannot drift from the
    /// validator's words.
    #[test]
    fn a_lone_ignored_key_warning_titles_its_head_and_keeps_the_sentence_whole() {
        let notices = crate::app_config::ignored_key_notices("windw_padding = 20\n");
        assert_eq!(notices.len(), 1, "{notices:?}");
        let whole = notices[0].strip_prefix("config ").expect("the prefix");
        let mut warns = ConfigWarnings::default();
        warns.extend(ConfigFamily::IgnoredKeys, notices.clone());
        let msg = warns.into_messages().remove(0);
        assert_eq!(msg.title, "Key has no effect");
        assert_eq!(
            msg.detail[0], "windw_padding \u{2192} window_padding?",
            "the key and its near miss; the line number is behind Details"
        );
        assert_eq!(msg.detail[1], whole, "the sentence survives whole");
        assert_eq!(msg.key.as_deref(), Some("config.ignored-keys"));

        // The plain form is the bare key.
        assert_eq!(
            unknown_key_excerpt(
                "line 4: foo_bar is unknown to this aterm build; it will be preserved for \
                 forward compatibility"
            )
            .as_deref(),
            Some("foo_bar")
        );
        assert_eq!(
            unknown_key_excerpt("line 3: unknown key \"windw_padding\"").as_deref(),
            None,
            "neither shape: the head at a seam instead"
        );
        // A plain form sheds its forward-compatibility clause at the
        // semicolon; the head is the LONGEST that fits outside a parenthesis.
        let key = "k".repeat(40);
        let plain = format!(
            "line 4: {key} is unknown to this aterm build; it will be preserved for forward \
             compatibility"
        );
        assert_eq!(
            excerpt_head(&plain, EXCERPT_CAP),
            format!("line 4: {key} is unknown to this aterm build")
        );
        // A sentence with no seam inside the cap is its own excerpt.
        let wide = "x".repeat(EXCERPT_CAP + 1);
        let long = format!("{wide} (aside); tail");
        assert_eq!(excerpt_head(&long, EXCERPT_CAP), long);
        assert_eq!(excerpt_head(" (aside) only", EXCERPT_CAP), " (aside) only");
    }

    /// Every title a reporter here mints fits the engine's title cap
    /// without being clipped: a clipped title reads as a cut sentence.
    #[test]
    fn every_reporter_title_fits_the_title_cap() {
        for msg in every_reporter_message() {
            assert!(
                msg.title.chars().count() <= TITLE_CAP,
                "title over the cap: {:?}",
                msg.title
            );
            assert!(!msg.title.ends_with('\u{2026}'), "clipped: {:?}", msg.title);
            assert!(!msg.title.is_empty());
            assert!(
                msg.detail.iter().all(|l| !l.is_empty()),
                "no empty detail line: {msg:?}"
            );
        }
    }

    /// The value-level twin of `update_words::no_update_lane_source_prompts_a_
    /// restart` over the reporters' words: no sentence asks for a restart
    /// unless it carries one of the sanctioned anchors on the same line —
    /// the restart-only keys (`columns/lines`, `gpu applies`), the GPU-lost
    /// remedy (`open a new window or restart`), the dead publisher's retry,
    /// the Windows backdrop.
    #[test]
    fn no_reporter_wording_prompts_a_restart() {
        let needles = ["restart", "relaunch", "reopen", "next launch", "reboot"];
        let anchors = [
            "columns/lines",
            "gpu applies",
            "open a new window or restart",
            "restart aterm to retry",
            "backdrop",
        ];
        let mut checked = 0usize;
        for msg in every_reporter_message() {
            for line in sentences(&msg) {
                checked += 1;
                let lower = line.to_lowercase();
                let prompts = needles.iter().any(|n| lower.contains(n));
                let anchored = anchors.iter().any(|a| lower.contains(a));
                assert!(!prompts || anchored, "asks for a restart: {line:?}");
            }
        }
        assert!(checked > 30, "the guard covers the reporters: {checked}");
    }

    /// The feedback rows: a terse title, the cause (or what to do) as
    /// `detail[0]`, a gesture's short hold; the pins that read the old
    /// sentences as titles ("The GPU was lost", "accessibility OFF", "has no
    /// effect on the CPU renderer" on glass) are re-pinned here.
    #[test]
    fn feedback_sentences_split_into_a_title_and_a_cause() {
        let msg = serious_mode_feedback(
            "Serious Mode was not changed: save conflict; current aterm.toml could not be admitted: x",
        );
        assert_eq!(msg.title, "Serious Mode not changed");
        assert_eq!(
            msg.detail,
            [
                "save conflict",
                "current aterm.toml could not be admitted: x"
            ]
        );
        assert_eq!(msg.key.as_deref(), Some(KEY_SERIOUS_MODE));
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE));
        let conflict = serious_mode_feedback(
            "Serious Mode was not changed because aterm.toml changed first; its current value was kept.",
        );
        assert_eq!(conflict.detail[0], "aterm.toml changed first");
        let unverified = serious_mode_feedback(
            "Serious Mode may have been written but could not be verified; reload before retrying: gen moved",
        );
        assert_eq!(unverified.title, "Serious Mode not verified");
        assert_eq!(unverified.detail[0], "reload before retrying");
        let msg = presence_not_saved(
            "presence.rim",
            "Presence Rim",
            "aterm.toml changed first; its current value was kept",
            false,
        );
        assert_eq!(msg.title, "Presence Rim not saved");
        assert_eq!(msg.severity, Severity::Error);
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE));
        assert_eq!(
            msg.key.as_deref(),
            Some("fabric.presence-save.presence.rim")
        );
        let maybe = presence_not_saved("presence.band", "Presence Band", "io", true);
        assert_eq!(maybe.title, "Presence Band may not be saved");
        assert_eq!(maybe.severity, Severity::Warn);
        let msg =
            config_lane_error("Robi was not dismissed: the settings lane dropped the request");
        assert_eq!(msg.title, "Robi not dismissed");
        assert_eq!(msg.detail, ["the settings lane dropped the request"]);
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "Robi's is a gesture");
        let unknown = config_lane_error("Something new went wrong: the cause");
        assert_eq!(unknown.title, "Settings change failed");
        assert_eq!(unknown.detail, ["Something new went wrong: the cause"]);
        let msg = launch_load_failure(
            "aterm.toml could not be read at launch (permission denied (os error 13)) \u{2014} every setting is running at its default.",
        );
        assert_eq!(msg.title, "aterm.toml not readable");
        assert_eq!(msg.detail[0], "permission denied (os error 13)");
        assert_eq!(msg.detail[1], "every setting is running at its default");
        assert_eq!(msg.hold, Hold::For(HOLD_LAUNCH));
        assert_eq!(msg.key.as_deref(), Some(KEY_LAUNCH_LOAD));
        let invalid = launch_load_failure(
            "aterm.toml is not a valid configuration (expected `=` at line 3) \u{2014} every setting is running at its default.",
        );
        assert_eq!(invalid.title, "aterm.toml not valid");
        assert_eq!(invalid.detail[0], "expected `=` at line 3");
        let standing = gpu_lost();
        assert_eq!(standing.title, "GPU lost");
        assert_eq!(
            standing.detail,
            [
                "windows opened for the backdrop cannot redraw",
                "open a new window or restart aterm to get a visible one back"
            ]
        );
        assert_eq!(standing.hold, Hold::Standing);
        assert_eq!(standing.actions, [Intent::NewWindow]);
        let dead = a11y_publisher_dead("bus gone", true);
        assert_eq!(dead.title, "Screen reader access lost");
        assert_eq!(dead.hold, Hold::Standing, "a screen reader was attached");
        assert_eq!(
            a11y_publisher_dead("bus gone", false).hold,
            Hold::LogOnly,
            "nobody was listening: a record, not an FYI on the glass"
        );
        assert_eq!(
            dead.detail,
            [
                "restart aterm to retry",
                "bus gone",
                "no screen reader can see this window"
            ],
            "the retry alone on its line, the reason behind it"
        );
        let cpu = cpu_renderer_no_effect("background_material", "no compositor");
        assert_eq!(cpu.key.as_deref(), Some("render.cpu.background_material"));
        assert_eq!(cpu.hold, Hold::LogOnly, "a disclosure is a record");
        assert_eq!(
            cpu.actions,
            [Intent::OpenSettings {
                route: "/appearance".to_string()
            }]
        );
        // A Hold press on a held session asked for what already is: a
        // record. A Release the fleet refused failed: a gesture row, titled
        // with the outcome.
        let held = fleet_hold(true);
        assert_eq!(held.title, "Session already held");
        assert_eq!(held.hold, Hold::LogOnly);
        assert_eq!(held.detail, ["the fleet holds it"]);
        let kept = fleet_hold(false);
        assert_eq!(kept.title, "Hold not lifted");
        assert_eq!(kept.severity, Severity::Warn);
        assert_eq!(kept.hold, Hold::For(HOLD_GESTURE));
        assert_eq!(kept.detail, ["the fleet holds it"]);
        assert!(kept.excerpt);
        assert_eq!(kept.key.as_deref(), Some(KEY_FABRIC_HOLD));
        assert_eq!(hold_refused("x").title, "Hold refused");
        assert_eq!(hold_refused("x").key.as_deref(), Some(KEY_FABRIC_HOLD));
        let failed = fabric_status("off", "Fabric Off failed", "exited 2", false);
        assert_eq!(failed.severity, Severity::Error);
        assert_eq!(failed.detail, ["exited 2"]);
        assert!(!failed.excerpt, "an exit code rides behind Details");
        let overridden = serious_mode_feedback(
            "Serious Mode was saved, but a newer aterm.toml edit now controls it.",
        );
        assert_eq!(overridden.title, "Serious Mode overridden");
        assert_eq!(
            overridden.detail,
            ["aterm.toml sets it"],
            "what overrode it"
        );
        let done = fabric_status("off", "aterm fabric off: done.", "", true);
        assert_eq!(done.hold, Hold::LogOnly, "a clean exit is a record");
        assert_eq!(log_did_not_open("/x/y.log").title, "Log did not open");
        assert_eq!(log_did_not_open("/x/y.log").hold, Hold::For(HOLD_GESTURE));
    }

    /// A DIAGNOSTIC IS ONE PROBLEM, NOT ONE LINE (upstream e7dc1feee's
    /// `a_multi_line_diagnostic_is_painted_as_the_rows_it_actually_has`, ported
    /// onto R13). A rejected `aterm.toml` reaches the config lane as a `toml`
    /// parse error with its caret diagram: one message, a terse title, the
    /// parser's first row as the excerpt and its caret diagram behind Details.
    /// The error string comes from the REAL parser, not a literal, so the test
    /// cannot pass because the shape it assumes has changed.
    #[test]
    fn a_multi_line_diagnostic_is_one_message_with_the_rows_it_actually_has() {
        let error = "x = [bad\n"
            .parse::<aterm_toml::edit::DocumentMut>()
            .expect_err("a malformed document")
            .to_string();
        assert!(
            error.contains('\n'),
            "this test is about a multi-line diagnostic: {error:?}"
        );
        let msg = config_lane_error(&format!("Config observation was not valid TOML: {error}"));
        // ONE PROBLEM, ONE MESSAGE: a terse title, and the diagnostic's rows
        // are its detail lines — several, not one.
        assert_eq!(msg.title, "aterm.toml syntax error");
        assert_eq!(msg.key.as_deref(), Some(KEY_CONFIG_LANE));
        assert!(msg.detail.len() > 1, "{:?}", msg.detail);
        // NO CONTROL CHARACTER REACHES A LINE, and no line is blank.
        for line in &msg.detail {
            assert!(
                line.chars().all(|ch| !ch.is_control()),
                "a control character took a cell: {line:?}"
            );
            assert!(!line.trim().is_empty(), "a blank row: {:?}", msg.detail);
        }
        // The diagnostic's own opening sentence and its caret row are DIFFERENT
        // lines, the caret row kept whole with its alignment.
        assert!(
            msg.detail[0].starts_with("TOML parse error"),
            "{:?}",
            msg.detail
        );
        let caret = error
            .lines()
            .find(|row| row.contains('^'))
            .expect("the parser's caret row")
            .trim_end();
        let at = msg
            .detail
            .iter()
            .position(|line| line == caret)
            .unwrap_or_else(|| panic!("the caret row {caret:?} is no line: {:?}", msg.detail));
        assert!(at > 0, "the caret must not share a line with the sentence");
        // A control byte that survives the split is spelled, not dropped.
        let hostile = config_lane_error("Config observation was not valid TOML: bad\u{1b}[31m\tx");
        assert_eq!(hostile.detail, ["bad\u{00b7}[31m\u{00b7}x"]);
        // A one-line sentence: the terse title and its cause.
        let one = config_lane_error("Robi was not dismissed: the lane dropped it");
        assert_eq!(
            (one.title.as_str(), one.detail.as_slice()),
            (
                "Robi not dismissed",
                ["the lane dropped it".to_string()].as_slice()
            )
        );
        // Every head of design §10.3 C22's map.
        for (head, title) in [
            (
                "Config observation was not valid TOML",
                "aterm.toml syntax error",
            ),
            ("Robi was not dismissed", "Robi not dismissed"),
            (
                "Config reconciliation failed; queued changes were not written",
                "Settings changes not saved",
            ),
            (
                "Manual saved aterm.toml, but its exact generation could not be admitted",
                "aterm.toml saved but not applied",
            ),
            (
                "Config was saved, but its exact disk generation could not be admitted",
                "aterm.toml saved but not applied",
            ),
            (
                "Config publication could not be verified",
                "aterm.toml not verified",
            ),
            ("Config was NOT saved", "Settings change not saved"),
        ] {
            assert_eq!(
                config_lane_error(&format!("{head}: the cause")).title,
                title,
                "{head}"
            );
        }
    }

    /// THE OWNER'S RULE, OVER EVERY BUILDER (design §10.1, ruling 76): every
    /// message a reporter here posts is progress, a decision, a failure or a
    /// record — never a confirmation, an FYI or progress with no indicator on
    /// the glass — and every glass title is terse.
    #[test]
    fn every_glass_message_earns_its_row() {
        let mut classes = std::collections::BTreeMap::new();
        for msg in every_reporter_message() {
            let class = attention(&msg)
                .unwrap_or_else(|why| panic!("{why}: {:?} ({:?})", msg.title, msg.hold));
            *classes.entry(format!("{class:?}")).or_insert(0usize) += 1;
        }
        for class in ["Progress", "Decision", "Failure", "Record"] {
            assert!(
                classes.contains_key(class),
                "the fixture exercises every class: {classes:?}"
            );
        }
        // The rule refuses what it must.
        let fyi = Message::new(tags::SYSTEM, Severity::Info, "Something happened");
        assert_eq!(attention(&fyi), Err("an FYI on glass"));
        let done = Message::new(tags::SYSTEM, Severity::Success, "Done");
        assert_eq!(attention(&done), Err("a confirmation on glass"));
        let blind = Message::new(tags::SYSTEM, Severity::Info, "Working").hold(Hold::Live {
            stale_after: Duration::from_secs(30),
        });
        assert_eq!(attention(&blind), Err("progress with no indicator"));
        let clause = Message::new(tags::SYSTEM, Severity::Warn, "It broke: here is why");
        assert_eq!(attention(&clause), Err("a clause in a glass title"));
        let long = Message::new(
            tags::SYSTEM,
            Severity::Warn,
            "one two three four five six seven",
        );
        assert_eq!(attention(&long), Err("a glass title over six words"));
        let sentence = Message::new(tags::SYSTEM, Severity::Warn, "It broke.");
        assert_eq!(attention(&sentence), Err("a sentence for a glass title"));
        // Words, not tokens: a route's `▸` and a name's `&` spend none.
        assert_eq!(
            title_words("Open Privacy & Security \u{25b8} Full Disk Access"),
            6
        );
    }

    /// The restart-only family and the renderer's disclosures are RECORDS
    /// (design §10.3 C8, C13, C14): nothing broke, nothing presses.
    #[test]
    fn the_restart_family_and_the_render_notes_are_records() {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::Restart,
            "columns/lines applies on next launch (resize the window to change size now)".into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
        );
        let restart = warns.into_messages().remove(0);
        assert_eq!(restart.hold, Hold::LogOnly);
        assert_eq!(attention(&restart), Ok(Attention::Record));
        for record in [
            cpu_renderer_no_effect("background_opacity", "no translucent present path"),
            backdrop_declined(
                "background_material is styling the title bar only",
                "this display stack refused the DirectComposition backdrop swapchain",
            ),
        ] {
            assert_eq!(record.hold, Hold::LogOnly, "{}", record.title);
        }
    }

    /// C9's DEFECT FIX: the Secure Keyboard Entry refusal was one sentence with
    /// an 18-space run where a line continuation was lost; it is one clean
    /// sentence now, and its excerpt is the state the person set.
    #[test]
    fn secure_keyboard_refusal_has_no_space_run() {
        // The literal as the compiler reads it: a `\` line continuation eats
        // the newline and the next line's indentation.
        let src = include_str!("app_config.rs");
        let at = src
            .find("\"secure_keyboard_entry: the OS refused the change")
            .expect("the refusal's literal");
        let end = src[at + 1..].find("\",").expect("its end") + at + 1;
        let mut literal = String::new();
        let mut rest = &src[at + 1..end];
        while let Some(cut) = rest.find("\\\n") {
            literal.push_str(&rest[..cut]);
            rest = rest[cut + 2..].trim_start();
        }
        literal.push_str(rest);
        assert!(
            literal.contains("\\u{2014} Secure Keyboard Entry is NOT"),
            "{literal:?}"
        );
        assert!(
            !literal.contains("  "),
            "a space run in the sentence: {literal:?}"
        );
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::SecureKeyboard,
            "secure_keyboard_entry: the OS refused the change (OSStatus -25293) \u{2014} \
             Secure Keyboard Entry is NOT on"
                .into(),
        );
        let msg = warns.into_messages().remove(0);
        assert_eq!(msg.title, "Secure Keyboard Entry refused");
        assert_eq!(msg.detail[0], "Secure Keyboard Entry is NOT on");
        assert!(msg.detail[1].contains("OSStatus -25293"));
        assert!(
            msg.detail.iter().all(|l| !l.contains("  ")),
            "{:?}",
            msg.detail
        );
    }

    /// R19 COMPLETES IN ITS FINISHED FORM (design ruling 154): `Installed
    /// Command Line Tools and Homebrew`, `Installed 3 programs` — with nothing
    /// else on the row moving at 60, 80, 120 and 160 (the password excerpt
    /// included).
    #[test]
    fn the_admin_install_completes_in_its_finished_form() {
        use crate::message_band::assert_completes_in_place as completes;
        let two = ["clt".to_string(), "brew".to_string()];
        completes(
            &admin_install_started(&two),
            "Installed Command Line Tools and Homebrew",
        );
        let three = ["clt".to_string(), "brew".to_string(), "xquartz".to_string()];
        completes(&admin_install_started(&three), "Installed 3 programs");
        completes(
            &admin_install_started(&["brew".to_string()]),
            "Installed Homebrew",
        );
    }

    /// §10.2 #14: the admin install's title names what is coming while the
    /// names fit the glass title's cap, and counts them when they would not.
    #[test]
    fn the_admin_install_title_falls_back_under_the_cap() {
        let two = ["clt".to_string(), "brew".to_string()];
        assert_eq!(
            admin_install_started(&two).title,
            "Installing Command Line Tools and Homebrew"
        );
        let three = ["clt".to_string(), "brew".to_string(), "xquartz".to_string()];
        let msg = admin_install_started(&three);
        assert_eq!(msg.title, "Installing 3 programs");
        assert_eq!(attention(&msg), Ok(Attention::Progress));
        assert_eq!(
            admin_install_started(&["brew".to_string()]).title,
            "Installing Homebrew"
        );
    }

    /// A glued row is the sentence and the diagram sharing one line — the shape
    /// the engine's `\n`-deleting sanitizer produced: `…column 11  |1 | font_px =`.
    fn glued(line: &str) -> bool {
        line.contains("column") && line.contains('|')
    }

    /// THE LAUNCH PATH KEEPS THE ROWS TOO. An `aterm.toml` that is invalid AT
    /// LAUNCH — every setting running at its default, the highest-stakes case —
    /// went through one `Message::line`, so the band, the Details page and the
    /// screen reader all read the caret diagram glued inline and the 240-char cut
    /// fell mid-word in the consequence. The lane path kept e7dc1feee's split;
    /// this one lost it when the banner became messages. Real parser, real notice.
    #[test]
    fn an_invalid_aterm_toml_at_launch_keeps_its_caret_diagram_in_rows() {
        let path = std::path::Path::new("/home/someone/.config/aterm/aterm.toml");
        let error = aterm_toml::from_str::<crate::app_config::Config>("font_px = \n")
            .err()
            .expect("a malformed aterm.toml");
        let notice = crate::app_config::launch_config_notice(
            path,
            crate::app_config::LaunchConfigProblem::Invalid(&error),
        );
        assert!(
            notice.contains('\n'),
            "this is about a multi-line notice: {notice:?}"
        );

        let msg = launch_load_failure(&notice);
        assert_eq!(
            msg.title, "aterm.toml not valid",
            "terse, and says it is broken"
        );
        assert!(msg.detail.len() > 1, "{:?}", msg.detail);
        for line in &msg.detail {
            assert!(!glued(line), "the diagram is glued to the words: {line:?}");
            assert!(line.chars().all(|ch| !ch.is_control()), "{line:?}");
        }
        assert!(
            msg.detail
                .iter()
                .any(|line| line.starts_with(char::is_whitespace) && line.contains('^')),
            "the caret row keeps its leading whitespace, so Details sets it as a diagram: {:?}",
            msg.detail
        );
        assert!(
            msg.detail
                .last()
                .is_some_and(|line| line.ends_with("and it loads on the next change.")),
            "the consequence arrives whole, not cut mid-word: {:?}",
            msg.detail
        );
        // A one-line problem is unchanged: the notice is its only line.
        let unreadable = crate::app_config::launch_config_notice(
            path,
            crate::app_config::LaunchConfigProblem::Unreadable(&"permission denied"),
        );
        let unreadable_msg = launch_load_failure(&unreadable);
        assert_eq!(unreadable_msg.title, "aterm.toml not readable");
        assert_eq!(
            unreadable_msg.detail.last(),
            Some(&unreadable),
            "the notice is its own last line: {:?}",
            unreadable_msg.detail
        );
    }

    /// A TRAIL PACK THAT DOES NOT PARSE KEEPS ITS ROWS, alone or in a crowd. Its
    /// sentence embeds `aterm_toml`'s caret diagram, and the config families
    /// handed it to one `Message::line` too. In a count, rows are what the
    /// budget spends: counting one diagram as one line let the engine's cap drop
    /// the roll-up, and a list that looks complete while it is not is the
    /// defect the roll-up exists to prevent.
    #[test]
    fn a_trail_pack_parse_error_keeps_its_rows_and_the_roll_up_survives() {
        let error = aterm_toml::from_str::<aterm_toml::Value>("trail = [\n")
            .expect_err("a malformed manifest")
            .to_string();
        assert!(error.contains('\n'), "{error:?}");
        let sentence = format!(
            "cursor_trail_packs[0] \"sparkle.toml\" invalid (TOML schema error: {error}); skipping"
        );

        // Alone: the family's short title (ruling 146), the head as the
        // excerpt, and every row of the sentence behind it.
        let lone =
            config_family_message(ConfigFamily::CursorTrail, std::slice::from_ref(&sentence));
        assert_eq!(lone.title, "Cursor trail not applied");
        assert_eq!(
            lone.detail[0],
            "cursor_trail_packs[0] \"sparkle.toml\" invalid"
        );
        assert!(lone.detail.len() > 2, "{:?}", lone.detail);
        for line in &lone.detail {
            assert!(!glued(line), "the diagram is glued to the words: {line:?}");
        }
        assert!(
            lone.detail
                .iter()
                .any(|line| line.trim_start().starts_with('|') && line.contains('^')),
            "the caret row is a line of its own: {:?}",
            lone.detail
        );

        // Enough of them that their ROWS overflow a message, though their count
        // (eight sentences) would not have.
        let many: Vec<String> = (0..8).map(|_| sentence.clone()).collect();
        let msg = config_family_message(ConfigFamily::CursorTrail, &many);
        assert!(msg.detail.len() <= DETAIL_LINES_CAP, "{}", msg.detail.len());
        assert!(
            msg.detail
                .last()
                .is_some_and(|line| line.starts_with("\u{2026} and ")),
            "the roll-up must survive the rows it rolls up: {:?}",
            msg.detail.last()
        );
        for line in &msg.detail {
            assert!(!glued(line), "{line:?}");
        }
    }

    /// TWO REMOVED KEYS ARE NOT "UNKNOWN". A removed feature's keys ride the
    /// ignored-key family, and its COUNT title said "not known to this build" —
    /// so the owner's `show_scene_hud`, joined by one more Scene key, put the
    /// forward-compatibility story back in the title of a row whose every line
    /// says "was removed" (3164e20c0 fixed the sentences; the count reinstated
    /// the contradiction one level up).
    #[test]
    fn two_removed_keys_are_counted_as_having_no_effect_not_as_unknown() {
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(
            &mut warns,
            "show_scene_hud = true\nscene_rows = 3\n",
        );
        let msgs = warns.into_messages();
        let ignored = msgs
            .iter()
            .find(|m| m.key.as_deref() == Some("config.ignored-keys"))
            .unwrap_or_else(|| panic!("the ignored-key family: {msgs:?}"));
        assert_eq!(ignored.title, "2 keys have no effect");
        assert!(!ignored.title.contains("nknown"), "{:?}", ignored.title);
        assert!(
            ignored
                .detail
                .iter()
                .filter(|line| line.contains("Scene HUD was removed"))
                .count()
                >= 2,
            "{:?}",
            ignored.detail
        );
    }
}
