// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Message and exceptions-digest builders for window alerts, config warnings, and harness events.

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

/// Attention tier that warrants an on-screen row.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attention {
    Progress,
    Decision,
    Failure,
    Answer,
    Record,
}

use aterm_messages::GLASS_TITLE_CHARS;

/// Classifies `msg` into an [`Attention`] tier or returns why it cannot appear on screen.
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

pub(crate) const HOLD_LAUNCH: Duration = Duration::from_secs(120);

pub(crate) const KEY_CRASH: &str = "crash.last";

pub(crate) const KEY_LAUNCH_LOAD: &str = "config.launch-load";

pub(crate) const KEY_GPU_LOST: &str = "render.gpu-lost";

pub(crate) const KEY_A11Y_PUBLISHER: &str = "a11y.publisher";

#[cfg(any(windows, test))]
pub(crate) const KEY_BACKDROP: &str = "render.backdrop";

pub(crate) const KEY_SERIOUS_MODE: &str = "config.serious-mode";

pub(crate) const KEY_CONFIG_LANE: &str = "config.lane";

pub(crate) const KEY_FABRIC_HOLD: &str = "fabric.hold";

pub(crate) const KEY_FILE_ACCESS: &str = "privacy.fda";

pub(crate) const KEY_DEBUGGER_ACCESS: &str = "privacy.debugger";

pub(crate) const PANE_FULL_DISK_ACCESS: &str = "full-disk-access";

pub(crate) const PANE_DEVELOPER_TOOLS: &str = "developer-tools";

#[cfg(any(target_os = "macos", test))]
pub(crate) const KEY_INSTALL_POSTURE: &str = "packages.posture";

const VALIDATOR_HINT: &str = "run `aterm --window --validate-config` for the full list";

const EXCERPT_CAP: usize = 80;

/// Config warning category used to group launch or reload warnings into one message per family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ConfigFamily {
    Keybindings,
    IgnoredKeys,
    RetiredKeys,
    UnacceptedValues,
    CursorTrail,
    Fonts,
    Assets,
    Restart,
    SecureKeyboard,
}

impl ConfigFamily {
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

    /// Supersede key (`config.<family>`) for this warning family.
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

    const fn severity(self) -> Severity {
        match self {
            Self::Restart | Self::RetiredKeys => Severity::Info,
            _ => Severity::Warn,
        }
    }

    const fn hold(self) -> Hold {
        match self {
            Self::Restart | Self::RetiredKeys => Hold::LogOnly,
            _ => Hold::Default,
        }
    }

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

    /// Banner title for `n` sentences in this warning family.
    pub(crate) fn title(self, n: usize) -> String {
        let one = n == 1;
        match self {
            Self::Keybindings if one => "Keybinding skipped".to_string(),
            Self::Keybindings => format!("{n} keybindings skipped"),
            Self::IgnoredKeys if one => "Unknown setting".to_string(),
            Self::IgnoredKeys => format!("{n} unknown settings"),
            Self::RetiredKeys if one => "Retired setting".to_string(),
            Self::RetiredKeys => format!("{n} retired settings"),
            Self::UnacceptedValues if one => "Couldn't use a setting's value".to_string(),
            Self::UnacceptedValues => format!("Couldn't use {n} settings' values"),
            Self::CursorTrail if one => "Couldn't apply the cursor trail".to_string(),
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

    /// Resolution log title when a clean reload clears `n` warnings in this family.
    pub(crate) fn fixed_title(self, title: &str, n: usize) -> Option<String> {
        let one = n == 1;
        Some(match self {
            Self::IgnoredKeys => format!("{title} fixed"),
            Self::Keybindings if one => "Keybinding fixed".to_string(),
            Self::Keybindings => format!("{n} keybindings fixed"),
            Self::UnacceptedValues if one => "Setting's value fixed".to_string(),
            Self::UnacceptedValues => format!("{n} settings' values fixed"),
            Self::CursorTrail => "Cursor trail problem fixed".to_string(),
            Self::Fonts if one => "Font problem fixed".to_string(),
            Self::Fonts => format!("{n} font problems fixed"),
            Self::Assets if one => "Image problem fixed".to_string(),
            Self::Assets => format!("{n} image problems fixed"),
            Self::SecureKeyboard => "Secure Keyboard Entry changed".to_string(),
            Self::Restart | Self::RetiredKeys => return None,
        })
    }

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

/// How a previously reported config problem was resolved on reload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigFix {
    Removed,
    Applied,
}

/// Resolution words for a fixed config warning based on how the reload resolved it.
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

/// Config warnings collected during a launch or reload, grouped and deduplicated by family.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigWarnings {
    families: Vec<(ConfigFamily, Vec<String>)>,
    settings_rows: Vec<(ConfigFamily, String)>,
}

impl ConfigWarnings {
    /// Appends a warning sentence to `family`, deduplicating exact repeats.
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

    /// Appends a warning sentence to `family` that Settings also renders inline on its own row.
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

    /// Appends multiple Settings-inline warning sentences to `family`.
    pub(crate) fn extend_shown_in_settings(
        &mut self,
        family: ConfigFamily,
        sentences: impl IntoIterator<Item = String>,
    ) {
        for sentence in sentences {
            self.push_shown_in_settings(family, sentence);
        }
    }

    /// Returns whether every sentence in `family` is also shown inline in Settings.
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

    /// Supersede keys of all families whose warnings are shown inline in Settings.
    pub(crate) fn settings_shown_keys(&self) -> Vec<&'static str> {
        self.families
            .iter()
            .map(|(family, _)| *family)
            .filter(|family| self.settings_shows(*family))
            .map(ConfigFamily::key)
            .collect()
    }

    /// Appends multiple warning sentences to `family`.
    pub(crate) fn extend(
        &mut self,
        family: ConfigFamily,
        sentences: impl IntoIterator<Item = String>,
    ) {
        for sentence in sentences {
            self.push(family, sentence);
        }
    }

    /// Iterates over all collected warning sentences in family collection order.
    pub(crate) fn sentences(&self) -> impl Iterator<Item = &str> {
        self.families
            .iter()
            .flat_map(|(_, lines)| lines.iter().map(String::as_str))
    }

    /// Returns all collected warning sentences as owned strings.
    pub(crate) fn told(&self) -> Vec<String> {
        self.sentences().map(str::to_owned).collect()
    }

    /// Builds one [`Message`] per non-empty warning family in collection order.
    pub(crate) fn into_messages(self) -> Vec<Message> {
        self.into_messages_routed(false)
    }

    /// Builds warning messages, downgrading families shown inline in Settings to log records when Settings is open.
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

/// Converts a config warning [`Message`] into a log-only record with the same content.
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

fn font_excerpt(sentence: &str) -> Option<String> {
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

/// Returns the longest prefix of `sentence` within `cap` characters ending at a clause boundary.
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

fn config_family_message(family: ConfigFamily, sentences: &[String]) -> Message {
    let stripped: Vec<&str> = sentences.iter().map(|s| family.strip_prefix(s)).collect();
    let title = match (family, stripped.as_slice()) {
        (ConfigFamily::Restart, [only]) => restart_title(only),
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
    if excerpt != first && family.hold() != Hold::LogOnly {
        msg = msg.line(excerpt);
        used += 1;
    }
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
    if record { msg } else { msg.now(family.ask()) }
}

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

pub(crate) const KEY_FONT_FAMILY: &str = "config.font-family";

/// Builds the warning message when the backend rejects a configured `font_family`.
pub(crate) fn font_family_rejected(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()]).key(KEY_FONT_FAMILY)
}

/// Builds the warning message when a theme references an uninstalled font.
pub(crate) fn theme_font_refused(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()])
}

pub(crate) const CRASH_EXCERPT: &str = "a crash report was saved";

/// Builds the post-crash recovery notice from [`crate::logging::CrashEvidence`].
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

pub(crate) const KILLED_EXCERPT: &str =
    "no crash report: it ended without a crash signal or a quit";

/// Builds the notice when the previous installed app instance terminated without unwinding.
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

pub(crate) const KEEPER_SCREENS: &str = "each screen started blank and its program was asked to \
                                         redraw; the scrollback before the stop is not here";

pub(crate) const KEEPER_SCREENS_EXACT: &str =
    "each screen was restored exactly; the scrollback came back too";

/// Formats the screen-restoration summary line for a PTY keeper recovery notice.
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

fn counted_were(n: u32, word: &str) -> String {
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    format!(
        "{} {}",
        counted(n, word),
        if n == 1 { "was" } else { "were" }
    )
}

/// Builds the notice reporting sessions recovered from the PTY keeper after a crash or kill.
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

pub(crate) const JOURNAL_LOSS: &str =
    "the programs that were running in them, and their scrollback, did not survive";

pub(crate) const KEY_CRASH_JOURNAL: &str = "crash.journal";

fn counted(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

pub(crate) const JOURNAL_ONLY_SCROLLBACK: &str = "only the scrollback did not survive";

pub(crate) const JOURNAL_SCROLLBACK: &str = "the scrollback did not survive";

pub(crate) const JOURNAL_NOTHING_RAN: &str =
    "nothing was running in them: only their scrollback did not survive";

pub(crate) const JOURNAL_NOTHING_RAN_FOLDERLESS: &str =
    "nothing was running in them: their scrollback did not survive";

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

fn journal_loss_line(lost: &crate::crash_journal::Lost, tabs: usize) -> String {
    format!(
        "{} and did not survive, nor did the scrollback",
        journal_ran_line(lost, tabs)
    )
}

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

fn journal_tag(class: crate::crash_journal::DeathClass) -> Tag {
    if class.crashed() {
        tags::CRASH
    } else {
        tags::SESSION
    }
}

/// Builds the notice reporting windows and tabs reopened from the crash journal.
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
    let (idle_line, scrollback_line) = if folders == PanesWithoutFolder::NONE {
        (JOURNAL_NOTHING_RAN, JOURNAL_ONLY_SCROLLBACK)
    } else {
        (JOURNAL_NOTHING_RAN_FOLDERLESS, JOURNAL_SCROLLBACK)
    };
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

/// Builds the record when a crash journal could not be reopened.
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

pub(crate) const KEY_RESTORED_AGENTS: &str = "harness.restored";

/// Supersede key for this process's restored-agents notice.
pub(crate) fn restored_agents_key() -> String {
    format!("{KEY_RESTORED_AGENTS}.{}", std::process::id())
}

pub(crate) const KEY_HARNESS_NO_SOCKET: &str = "harness.no-socket";

pub(crate) const HARNESS_BOXES_UNANSWERED: &str =
    "aterm cannot answer its coding agents' permission boxes here";

#[cfg(not(windows))]
pub(crate) const SOCKET_DIR_UNSET_FIX: &str =
    "set XDG_RUNTIME_DIR or HOME, or pass --control-sock <path>";
#[cfg(windows)]
pub(crate) const SOCKET_DIR_UNSET_FIX: &str = "set TMP or TEMP, or pass --control-sock <path>";

#[cfg(not(windows))]
pub(crate) const SOCKET_DIR_UNUSABLE_FIX: &str =
    "set XDG_RUNTIME_DIR to a folder you own, or pass --control-sock <path>";
#[cfg(windows)]
pub(crate) const SOCKET_DIR_UNUSABLE_FIX: &str =
    "set TMP to a folder you own, or pass --control-sock <path>";

/// Builds the warning when `[harness]` supervision is enabled without a control socket.
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

fn started_without_conversation(outcome: &aterm_agent::harness::relaunch::Outcome) -> bool {
    use aterm_agent::harness::relaunch::Outcome;
    matches!(outcome, Outcome::NotYet(step) if step == "failed:no-resume" || step == "wait:resume")
}

fn typed_relaunch_why(agent: aterm_agent::harness::upgrade::Agent) -> String {
    use aterm_agent::harness::upgrade::Agent;
    match agent {
        Agent::Claude => "Claude started but did not pick its conversation up".to_string(),
        Agent::Codex => "the relaunched Codex did not come up in the tab".to_string(),
        Agent::Muse => "the relaunched Muse Code did not come up in the tab".to_string(),
        Agent::Agy => "the relaunched agy did not come up in the tab".to_string(),
        Agent::Gemini => "the relaunched Gemini CLI did not come up in the tab".to_string(),
    }
}

const fn resume_words(agent: aterm_agent::harness::upgrade::Agent) -> (&'static str, &'static str) {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => ("claude --resume", "conversation"),
        aterm_agent::harness::upgrade::Agent::Codex => ("codex resume", "thread"),
        aterm_agent::harness::upgrade::Agent::Muse => ("muse resume", "session"),
        aterm_agent::harness::upgrade::Agent::Agy => ("agy then /resume", "conversation"),
        aterm_agent::harness::upgrade::Agent::Gemini => ("gemini then /resume", "session"),
    }
}

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

/// Builds the notice listing agents that could not be resumed after layout restoration.
pub(crate) fn restored_agents_not_resumed(
    missed: &[(
        String,
        aterm_agent::harness::upgrade::Agent,
        aterm_agent::harness::relaunch::Outcome,
    )],
    log: Option<&std::path::Path>,
) -> Message {
    let kind = missed
        .first()
        .map(|(_, agent, _)| *agent)
        .filter(|agent| missed.iter().all(|(_, other, _)| other == agent));
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
            let started = match agent {
                aterm_agent::harness::upgrade::Agent::Claude => true,
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

pub(crate) const KEY_RESTORED_HELD: &str = "harness.restored-held";

/// Builds the error message when `aterm.toml` fails to load at launch.
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

/// Builds a log record when a GPU-only appearance setting has no effect under the CPU renderer.
pub(crate) fn cpu_renderer_no_effect(key: &str, rest: &str) -> Message {
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

/// Builds a log record when the Windows client-area backdrop is declined.
#[cfg(any(windows, test))]
pub(crate) fn backdrop_declined(title: &str, detail: &str) -> Message {
    Message::new(tags::RENDER, Severity::Info, "Couldn't show the backdrop")
        .line(title)
        .line(detail)
        .action(Intent::OpenConfigEditor { line: None })
        .record()
        .key(KEY_BACKDROP)
}

/// Builds the alert when the GPU device is lost and backdrop windows require reopening.
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

/// Builds the notice when the accessibility publisher thread terminates.
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

/// Builds the error message when a presence toggle fails to persist to `aterm.toml`.
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

/// Builds the error message when a Stats Bars toggle fails to persist to `aterm.toml`.
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

/// Builds the error message when a harness switch toggle fails to persist to `aterm.toml`.
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

/// Builds the error message when a Deck Size change fails to persist to `aterm.toml`.
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

/// Builds the error message when a theme selection fails to persist to `aterm.toml`.
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

/// Builds the exceptions digest entry when the fabric bridge quarantines torn state files.
#[cfg(any(unix, test))]
pub(crate) fn torn_state_set_aside_entry(files: u64, told_when_empty: bool) -> Entry {
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
        Then::step(
            "Look through the quarantine folder in the state dir aterm fabric shows, then empty it",
        ),
    )
    .resurface(Resurface::CHOICE)
    .more(format!(
        "{what} found torn after an unclean shutdown moved into quarantine in the bridge's \
         state dir"
    ))
    .more(if told_when_empty {
        "This ends once the quarantine holds none of them"
    } else {
        "The bridge [fabric] command names is from an older aterm and does not say when the \
         quarantine is empty: dismiss this once it is"
    })
    .log_line(format!("fabric.state torn set_aside={files}"))
}

/// Digest key for [`torn_state_set_aside_entry`].
#[cfg(any(unix, test))]
pub(crate) fn torn_state_key() -> String {
    torn_state_set_aside_entry(1, true).key()
}

/// Builds the advisory record when the fabric bridge disconnects while sessions continue running.
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

/// Builds the error message when a Take Over or Hand Back action is refused.
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

/// Builds the error message when Summarize in a New Tab fails to start its worker.
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

pub(crate) const KEY_STOP_WORKER: &str = "harness.stop-worker";

/// Builds the error message when Stop Worker fails to signal the tab's agent.
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

/// Builds the record when Stop Worker finds no active task to stop.
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

/// Builds the error message when an Agents menu or palette command fails.
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

/// Builds the log record when a remote fleet peer drives a local tab.
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

/// Builds the completion record when a CLI command launched into a tab finishes.
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

/// Builds the feedback message for a Serious Mode config write.
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
        lines.insert(0, "aterm.toml sets it".to_string());
    }
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

/// Builds the error message when the native config observation lane fails.
pub(crate) fn config_lane_error(words: &str) -> Message {
    let mut rows = diagnostic_lines(words).into_iter();
    let first = rows.next().unwrap_or_default();
    let (head, cause) = split_sentence(&first);
    let (title, known) = lane_title(head);
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

fn gesture_failure(tag: Tag, severity: Severity, title: &str, detail: &str, then: Then) -> Message {
    let ask = gesture_ask(title, then);
    Message::new(tag, severity, title).sentence(detail).now(ask)
}

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

fn gesture_ask(title: &str, then: Then) -> Ask {
    Ask::flash(Why::YourAction(Reason::new(title)), then, Span::Gesture)
}

/// Builds the error message when `New Tab` fails to spawn a shell.
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

/// Builds the error message when `New Window` fails to spawn a shell.
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

/// Builds the feedback message when a pane split is refused for insufficient room.
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

/// Builds the error message when a pane split fails to spawn its shell.
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

const SPLIT_FAILED: &str = "Couldn't split the pane";

/// Builds the warning when keystrokes are dropped during a seamless update commit.
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

/// Builds the warning when user keystrokes or paste are refused by a full or closed session queue.
pub(crate) fn input_refused(cause: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't send your input",
        cause,
        Then::step("Send it again once the program reads input"),
    )
}

/// Builds the error message when session restore stops after failing to create a window.
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

/// Builds the error message when a shell handed across a seamless update cannot be adopted.
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

pub(crate) const KEY_INTEGRATION_LOST: &str = "session.integration";

pub(crate) const INTEGRATION_BACK: &str = "Shell integration back";

const INTEGRATION_SESSIONS: &str = " \u{2014} sessions ";

const INTEGRATION_SESSION: &str = " \u{2014} session ";

fn split_integration_sessions(line: &str) -> Option<(&str, &str)> {
    line.split_once(INTEGRATION_SESSIONS)
        .or_else(|| line.split_once(INTEGRATION_SESSION))
}

const INTEGRATION_TABS_NAMED: usize = 12;

/// Tab that lost shell integration across an update or crash recovery.
#[derive(Clone, Debug)]
pub(crate) struct LostTab {
    pub(crate) place: String,
    pub(crate) sid: String,
    pub(crate) heal: crate::integration_notice::Heal,
}

/// Builds the notice for tabs that lost shell integration, or `None` when `tabs` is empty.
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
    let mut msg = Message::new(tags::SESSION, Severity::Warn, title).line(
        atpkg::progress::sanitize_for_tty(&what, aterm_messages::DETAIL_LINE_CAP),
    );
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

/// Extracts the session IDs listed on the technical detail lines of an [`integration_lost`] row.
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

/// Builds the record for a tab whose shell-integration rekey awaits its next prompt.
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

/// Builds the error message when a restored tab fails to start its shell.
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

pub(crate) const KEY_FOLDER_NOT_FOUND: &str = "spawn.folder-not-found";

pub(crate) const KEY_FOLDER_NOT_OPENED: &str = "spawn.folder-not-opened";

pub(crate) const KEY_FOLDER_NOT_ANSWERING: &str = "spawn.folder-not-answering";

pub(crate) const KEY_HOME_NOT_ANSWERING: &str = "spawn.home-not-answering";

const FOLDERS_NAMED: usize = DETAIL_LINES_CAP - 2;

const FOLDER_INSTEAD_OF: &str = "opened in your home folder instead of ";

const FOLDERS_INSTEAD: &str = "opened in your home folder instead";

const ROOT_INSTEAD_OF: &str = "opened in / instead of ";

const ROOT_INSTEAD: &str = "opened in / instead";

const fn folder_instead(fault: crate::spawn_folder::Fault) -> (&'static str, &'static str) {
    match fault {
        crate::spawn_folder::Fault::HomeUnresponsive => (ROOT_INSTEAD_OF, ROOT_INSTEAD),
        _ => (FOLDER_INSTEAD_OF, FOLDERS_INSTEAD),
    }
}

/// Returns the supersede key for a spawn-folder [`crate::spawn_folder::Fault`].
pub(crate) const fn folder_row_key(fault: crate::spawn_folder::Fault) -> &'static str {
    match fault {
        crate::spawn_folder::Fault::Missing => KEY_FOLDER_NOT_FOUND,
        crate::spawn_folder::Fault::Shut => KEY_FOLDER_NOT_OPENED,
        crate::spawn_folder::Fault::Unresponsive => KEY_FOLDER_NOT_ANSWERING,
        crate::spawn_folder::Fault::HomeUnresponsive => KEY_HOME_NOT_ANSWERING,
    }
}

/// Builds the notice when new shells fall back to `$HOME` or `/` because their target folder failed.
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

/// Extracts the affected folders from an existing [`folder_row`] message.
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

/// Builds the error message when opening a log file fails.
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

/// Builds the interactive decision row prompting for Full Disk Access. Nothing posts it (System State's Full Disk Access row carries the need): it is a test fixture of a decision row with two capsules.
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
        .finished_as(ASKED_FILE_ACCESS)
}

#[cfg(test)]
pub(crate) const ASKED_FILE_ACCESS: &str = "Asked for Full Disk Access";

/// Builds the log record confirming Full Disk Access was observed.
pub(crate) fn file_access_granted() -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Success,
        crate::consent_card::GRANTED_CAPTION,
    )
    .record()
    .key(KEY_FILE_ACCESS)
}

/// Builds the log record when macOS presents a TCC consent dialog outside the consent card.
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

/// Builds the log record when a debugger-access probe observes a state change.
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

/// Builds the log record for an external `aterm ctl appnotice harness` note.
pub(crate) fn harness_note(text: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Info,
        atpkg::progress::sanitize_for_tty(text, 120),
    )
    .record()
}

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

/// Agent state for a held tab restored without relaunching its agent.
pub(crate) struct HeldAgent<'a> {
    pub(crate) name: &'static str,
    pub(crate) resume: Option<&'a str>,
    pub(crate) stopped_by_hold: bool,
    pub(crate) fleet: bool,
    pub(crate) lifted: bool,
}

/// Builds the notice or record when a held tab reopens at its shell without relaunching its agent.
pub(crate) fn restored_agent_held(
    sid: &str,
    place: &str,
    agent: &HeldAgent<'_>,
) -> Option<Message> {
    let line =
        |text: &str| atpkg::progress::sanitize_for_tty(text, aterm_messages::DETAIL_LINE_CAP);
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

/// Builds the log record for an informational session-supervisor note.
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

/// Builds the log record when a session's Autopilot mode is changed from the window.
pub(crate) fn autopilot_set(
    sid: &str,
    place: &str,
    view: crate::autopilot::AutopilotView,
    own: bool,
    cancelled: Option<crate::claude_lights::LandAtGlass>,
    set_by: Option<&str>,
) -> Message {
    use crate::autopilot::{AutopilotView, mode_words};
    use aterm_agent::supervise::Autopilot;
    let agent_off;
    let (title, what): (&str, &str) = match view {
        AutopilotView::Cruise => (
            "Autopilot set to cruise",
            mode_words(Autopilot::Cruise).record,
        ),
        AutopilotView::Land => ("Autopilot set to land", mode_words(Autopilot::Land).record),
        AutopilotView::Off => ("Autopilot set to off", mode_words(Autopilot::Off).record),
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

fn set_by_line(message: Message, set_by: Option<&str>) -> Message {
    match set_by {
        Some(set_by) => message.line(atpkg::progress::sanitize_for_tty(
            set_by,
            aterm_messages::DETAIL_LINE_CAP,
        )),
        None => message,
    }
}

/// Builds the log record when a timed Cruise duration is set on a session.
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
        &tab_led(place, crate::autopilot::TIMED_CRUISE_RECORD),
        aterm_messages::DETAIL_LINE_CAP,
    ));
    set_by_line(message, set_by)
        .record()
        .key(format!("harness.autopilot.{sid}").as_str())
}

/// Session state observed when a timed Cruise timer expires and switches to Land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FireOver {
    Work,
    MasterOff,
    AgentOff(aterm_phase::Program),
    Unhosted,
    NoAgent { split: bool },
}

/// Builds the log record when a timed Cruise expires and switches the session to Land.
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
                FireOver::Work => {
                    crate::autopilot::mode_words(aterm_agent::supervise::Autopilot::Land)
                        .record
                        .to_string()
                }
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

/// Builds the warning record for a session-supervisor warning.
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

pub(crate) const KEY_AGENT_UPGRADE: &str = "harness.upgrade";

/// Builds the log record summarizing tabs waiting for a live agent upgrade.
pub(crate) fn agent_upgrade_waiting(product: &str, waiting: &[&str]) -> Option<Message> {
    let first = *waiting.first()?;
    let n = waiting.len();
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

/// Builds the log record for a single tab waiting for its agent upgrade.
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

/// Builds the interactive row or record when a live agent upgrade stalls.
pub(crate) fn agent_upgrade_stalled(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
    place: &str,
) -> Message {
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let interrupt = row.interrupt(now);
    let why = row
        .stall_words(now)
        .unwrap_or_else(|| "it is not moving".to_string())
        .replace('`', "");
    let tab = clean(&row.tab);
    let from = clean(&row.from);
    let cmd = format!("aterm harness upgrade {tab}");
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
        Some(Remedy::Waits) if row.stall(now).as_deref() == Some("unwatched") => {
            (skip.clone(), "--status` or `--skip")
        }
        Some(Remedy::Waits) if under_way => (
            "no word moves it while it is under way: it goes on once what it waits on ends, \
             and nothing is forced"
                .to_string(),
            "--status",
        ),
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
        Some(Remedy::Now) if own_work_round(row, now) => (
            format!(
                "{now_word} cannot move it past its own work: it moves once that ends or the \
                 agent stops it; {skip}"
            ),
            "--skip",
        ),
        Some(Remedy::Now) | None => (
            format!("{now_word} tries it again at its next pause; {skip}"),
            "--now` or `--skip",
        ),
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
            (
                format!(
                    "{now_word} cannot move it past its own work: it moves once that ends or \
                     the agent stops it{asked}; {skip}"
                ),
                "--skip",
            )
        }
        Some(Remedy::Waits) if row.fence_fails_under_now() => (
            format!(
                "{now_word} is already in force: aterm tries its notice again at the next quiet \
                 moment; {skip}"
            ),
            "--skip",
        ),
        Some(Remedy::Waits) if row.limit_passed(now) => (
            format!(
                "type in its tab to continue the session, and it moves at its next pause; {skip}"
            ),
            "--skip",
        ),
        Some(Remedy::Waits) if row.spend_held() => (
            format!("add credits or raise the cap, and it moves at the next pause; {skip}"),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == aterm_agent::harness::upgrade::NEXT_POINT => {
            (skip.clone(), "--skip")
        }
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
        Some(Remedy::InItsPane) => (
            format!(
                "to move it, quit it in its {} and resume it there; {skip}",
                pane_words(row.wait.strip_prefix("terminal:").unwrap_or_default())
            ),
            "--skip",
        ),
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
                aterm_agent::harness::upgrade::Agent::Claude if !row.session.is_empty() => {
                    Some(format!("claude --resume {}", clean(&row.session)))
                }
                aterm_agent::harness::upgrade::Agent::Claude => Some("claude --resume".to_string()),
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
    let resting = row.will_ask_again(now)
        && matches!(row.phase, aterm_agent::harness::upgrade::Phase::Failed(_));
    let owner_asked = row.request == aterm_agent::harness::upgrade::Request::Now;
    let title = match (&interrupt, row.remedy(now)) {
        (Some(need), _) if need.kind == "hung-exit" => {
            placed_title(&format!("{who} is not exiting"), place, "")
        }
        (Some(need), _) if need.kind == "relaunch-stuck" => {
            placed_title(&format!("{who} is not ready"), place, "")
        }
        (Some(_), _) => placed_title(&format!("{who} stopped"), place, ""),
        (None, _) if resting => format!("{who} upgrade retries later {place}"),
        (None, Some(Remedy::Now | Remedy::Waits)) => {
            placed_title(&format!("{who} upgrade waits"), place, "")
        }
        (None, None) if owner_asked => placed_title(&format!("{who} upgrade waits"), place, ""),
        (None, _) => placed_title(&format!("Couldn't upgrade {who}"), place, ""),
    };
    let severity = if interrupt.is_some() {
        Severity::Warn
    } else {
        Severity::Info
    };
    let held = row.held_words(now).map(|words| {
        let words = atpkg::progress::sanitize_for_tty(&words, aterm_messages::DETAIL_LINE_CAP);
        format!("held by {words}")
    });
    if let Some(need) = &interrupt {
        let mut msg = Message::new(tags::HARNESS, severity, title)
            .line(atpkg::progress::sanitize_for_tty(&need.why, 160))
            .line(atpkg::progress::sanitize_for_tty(&need.then, 160));
        if let Some(command) = &need.command {
            msg = msg.line(format!(
                "`{}`",
                atpkg::progress::sanitize_for_tty(command, 240)
            ));
        }
        return msg
            .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
            .line(format!("`{cmd} --status` shows what it waited on"))
            .now(Ask::standing(
                StandingWhy::OnlyYou(Reason::new(&need.why)),
                Then::step(&need.then),
            ))
            .key(&agent_upgrade_key(&row.tab));
    }
    let mut msg = Message::new(tags::HARNESS, severity, title)
        .line(atpkg::progress::sanitize_for_tty(&why, 160))
        .line(remedy)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .line(held.unwrap_or_default())
        .line(format!("the same in any shell: `{cmd} {shell_words}`"))
        .key(&agent_upgrade_key(&row.tab));
    if owner_asked {
        return msg.notice(NoticeKind::Answer);
    }
    for intent in agent_upgrade_capsules(row, now) {
        msg = msg.action(intent);
    }
    msg.record()
}

/// Supersede key for aterm's own upgrade fault record in `tab`.
pub(crate) fn agent_upgrade_fault_key(tab: &str) -> String {
    format!(
        "harness.upfault.{}",
        atpkg::progress::sanitize_for_tty(tab, 64)
    )
}

/// Builds the log record when aterm hits an internal fault during a tab's upgrade.
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

#[cfg(test)]
pub(crate) const ANSWER_MAX: Duration = aterm_messages::need::MAX_TRANSIENT;

fn pane_words(owner: &str) -> String {
    match owner {
        "tmux" => "tmux pane".to_string(),
        "screen" => "screen window".to_string(),
        "" | "none" | "?" | "-" => "own terminal".to_string(),
        name => format!("{} pane", atpkg::progress::sanitize_for_tty(name, 32)),
    }
}

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

fn next_round_clock(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> Option<String> {
    let secs = row.next_round_in(now).filter(|secs| *secs > 0)?;
    let offset = crate::presence::local_offset_s()?;
    Some(aterm_messages::words::clock_words(
        now.saturating_add(secs).saturating_mul(1000),
        offset,
    ))
}

/// Supersede key for a tab's stalled-upgrade row and operator response.
pub(crate) fn agent_upgrade_key(tab: &str) -> String {
    format!(
        "{KEY_AGENT_UPGRADE}.{}",
        atpkg::progress::sanitize_for_tty(tab, 64)
    )
}

/// Builds [`agent_upgrade_stalled`] annotated with navigation to `tab_num`.
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

/// Human-readable tab location and navigation intent for an exceptions digest entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpgradeSpot {
    pub(crate) place: String,
    pub(crate) nav: Option<Intent>,
}

type ExceptionGroup<'a> = Vec<(
    &'a aterm_agent::harness::upgrade_drive::Row,
    &'a UpgradeSpot,
    aterm_agent::harness::upgrade_drive::Need,
)>;

const PER_KIND: [&str; 2] = ["own-work", "typed-draft"];

const CHOICE_KINDS: [&str; 2] = ["no-terminal", "refused-launch"];

/// Builds the exceptions digest entries for stalled or blocked agent upgrades.
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

fn fitted_title(head: &str, place: &str) -> String {
    let title = placed_title(head, place, "");
    if aterm_messages::text::glass_title_fault(&title).is_none() {
        title
    } else {
        head.to_string()
    }
}

fn host_record_key(kind: &str, sid: &str) -> String {
    format!(
        "harness.host.{kind}.{}",
        atpkg::progress::sanitize_for_tty(sid, 40)
    )
}

/// Builds the warning log record when a tab's supervisor exceeds its restart budget.
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

/// Builds the exceptions digest entry when a tab's supervisor has failed for an hour.
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

fn digest_key(family: Family, sid: &str, cause: &str) -> String {
    aterm_messages::need::Fingerprint::new(family.stem(), sid, cause)
        .as_str()
        .to_string()
}

/// Digest key for [`supervisor_failing_entry`].
pub(crate) fn supervisor_failing_key(sid: &str) -> String {
    digest_key(Family::Supervisor, sid, "failing")
}

/// Digest key for [`occupancy_prompt_entry`]: `harness.occupancy.aterm-toml.prompt`.
pub(crate) fn occupancy_prompt_key() -> String {
    digest_key(Family::Occupancy, "aterm-toml", "prompt")
}

/// THE OWNER'S `occupancy = "prompt"` IN FORCE (docs/DESIGN-harness-occupancy-2026-10-20.md §1.1).
pub(crate) fn occupancy_prompt_entry() -> Entry {
    Entry::new(
        Family::Occupancy,
        "aterm-toml",
        "prompt",
        "Harness types occupancy lines",
        Why::Configured {
            key: Reason::new("[harness] occupancy = \"prompt\""),
            reason: Reason::new("the harness types occupancy lines into the agents' sessions"),
        },
        Then::step("Set occupancy to observe in aterm.toml to stop them"),
    )
    .resurface(Resurface::CHOICE)
    .show(Intent::OpenConfigEditor { line: None })
    .more(
        "aterm.toml's root [harness] table says occupancy = \"prompt\": at a drawn break, with \
         every guard clear and paced, the harness types wait-nudge@v1 and hold-room@v1 lines \
         into a waiting agent's session. aterm ctl @<sid> occupancy why says what it judged.",
    )
    .log_line("occupancy mode=prompt")
}

/// Digest key for [`occupancy_hold_entry`]: `harness.occupancy.<sid>.hold`.
pub(crate) fn occupancy_hold_key(sid: &str) -> String {
    digest_key(Family::Occupancy, sid, "hold")
}

/// A TAB'S OCCUPANCY LINES SPENT FOR ITS HOLD, under `prompt` (design §6, `occupancy.hold`): `spent` names each rule whose cap is spent.
pub(crate) fn occupancy_hold_entry(
    sid: &str,
    spent: &[aterm_agent::supervise::occupancy::RuleId],
) -> Entry {
    use aterm_agent::supervise::occupancy::said::cap_words;
    let caps: Vec<String> = spent.iter().map(|rule| cap_words(*rule)).collect();
    let entry = Entry::new(
        Family::Occupancy,
        sid,
        "hold",
        "A tab's occupancy lines are spent",
        Why::Spent {
            tried: Reason::new("typed its occupancy lines"),
            reason: Reason::new(format!(
                "{} spent, and the tab still holds",
                caps.join(" and ")
            )),
        },
        Then::step(format!(
            "Look at the tab's wait with aterm ctl @{sid} occupancy why"
        )),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .log_line(format!(
        "occupancy hold sid={sid} spent={}",
        spent
            .iter()
            .map(|rule| rule.name())
            .collect::<Vec<_>>()
            .join(",")
    ));
    caps.iter()
        .fold(entry, |entry, cap| entry.more(format!("{cap} spent")))
}

/// Digest key for [`occupancy_ready_idle_entry`]: `harness.occupancy.<sid>.ready-idle`.
pub(crate) fn occupancy_ready_idle_key(sid: &str) -> String {
    digest_key(Family::Occupancy, sid, "ready-idle")
}

/// A TAB'S DUE LINE STOOD K LOOKS UNTYPED, under `prompt` (the slice-2 plan's D2, the K Entry):
/// `idle` names its rule, the guard holding it at the last look and when it was first due; `at_ms`
/// (unix) is the told word's. Every word it fills in is the tab's sid, the closed rule list's, the
/// closed guard list's or a span the loop's clock measured.
pub(crate) fn occupancy_ready_idle_entry(
    sid: &str,
    idle: &aterm_agent::supervise::occupancy::SaidReadyIdle,
    at_ms: i64,
) -> Entry {
    use aterm_agent::supervise::occupancy::said::span_words;
    let rule = idle.rule.name();
    let span = span_words(at_ms.saturating_sub(idle.since_ms));
    let held = idle.guard.map_or_else(
        || String::from("no guard held it, and it went untyped"),
        |guard| format!("{} held it at the last look", guard.word()),
    );
    Entry::new(
        Family::Occupancy,
        sid,
        "ready-idle",
        "A tab's occupancy line stands untyped",
        Why::Spent {
            tried: Reason::new(format!("kept {rule} due for {span}")),
            reason: Reason::new(&held),
        },
        Then::step(format!(
            "Look at the tab's wait with aterm ctl @{sid} occupancy why"
        )),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .more(format!(
        "The harness judged {rule} due on this tab for {span} and typed nothing: {held}."
    ))
    .log_line(format!(
        "occupancy ready-idle sid={sid} rule={rule} guard={} due_ms={}",
        idle.guard.map_or("none", |guard| guard.word()),
        at_ms.saturating_sub(idle.since_ms).max(0)
    ))
}

/// A TAB'S DECLARED WAKE STOOD K LOOKS READY AND UNTOLD, under `prompt` (the slice-2 plan's
/// 2.5, the wake K): on the same key as a hold rule's ([`occupancy_ready_idle_key`]), which the
/// exceptions host gives the rule's first. Every word it fills in is the tab's sid, the wake's
/// ordinal, the closed guard list's (or `no-loop`, where no loop told the window a word) or a span
/// the window's clock measured; no word of the agent's declaration.
pub(crate) fn occupancy_wake_idle_entry(
    sid: &str,
    idle: &crate::occupancy_wakes::WakeIdle,
) -> Entry {
    use aterm_agent::supervise::occupancy::said::span_words;
    let ord = idle.ord;
    let span = span_words(idle.at_ms.saturating_sub(idle.since_ms));
    let guard = match (idle.looped, idle.guard) {
        (false, _) => "no-loop",
        (true, Some(guard)) => guard.word(),
        (true, None) => "none",
    };
    let held = match (idle.looped, idle.guard) {
        (false, _) => String::from("no loop reads this tab's wakes"),
        (true, Some(guard)) => format!("{} held its line at the last look", guard.word()),
        (true, None) => String::from("no guard held it, and nothing told it"),
    };
    Entry::new(
        Family::Occupancy,
        sid,
        "ready-idle",
        "A tab's declared wake stands untold",
        Why::Spent {
            tried: Reason::new(format!("kept wake #{ord} ready for {span}")),
            reason: Reason::new(&held),
        },
        Then::step(format!("Look at the tab's wakes with aterm ctl @{sid} waits why")),
    )
    .severity(Severity::Info)
    .resurface(Resurface::WAIT)
    .more(format!(
        "The tab's agent declared wake #{ord}; its condition held {span} ago and nothing told it: {held}."
    ))
    .log_line(format!(
        "occupancy ready-idle sid={sid} wake=#{ord} guard={guard} due_ms={}",
        idle.at_ms.saturating_sub(idle.since_ms).max(0)
    ))
}

/// Retired `aterm.toml` feature table and its replacement guidance.
pub(crate) struct RemovedFeature {
    pub(crate) table: &'static str,
    pub(crate) what: &'static str,
    pub(crate) instead: &'static str,
    pub(crate) said: &'static str,
    pub(crate) replaced_by: &'static str,
    pub(crate) left: Option<fn(&aterm_toml::Value) -> Vec<String>>,
}

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

const LEFT_PATH_CHARS: usize = 120;

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

/// Digest key for `feature`'s retired-setting entry.
pub(crate) fn removed_setting_key(feature: &RemovedFeature) -> String {
    digest_key(Family::Removed, feature.table, "replaced")
}

/// Remediation step text for removing `feature`'s table from `aterm.toml`.
pub(crate) fn removed_setting_step(feature: &RemovedFeature) -> String {
    format!("Delete the [{}] table from aterm.toml", feature.table)
}

/// Builds the exceptions digest entry when `aterm.toml` still configures a retired `feature`.
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

const fn survey_cause(kind: aterm_agent::supervise::StuckSurvey) -> &'static str {
    match kind {
        aterm_agent::supervise::StuckSurvey::Withheld => "survey-card",
        aterm_agent::supervise::StuckSurvey::Unclosed => "survey-open",
    }
}

/// Builds the warning log record when a rating survey card stands in a supervised tab.
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

/// Builds the exceptions digest entry when a rating survey card has stood for an hour.
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

/// Digest key for [`survey_entry`].
pub(crate) fn survey_key(sid: &str) -> String {
    digest_key(
        Family::Supervisor,
        sid,
        survey_cause(aterm_agent::supervise::StuckSurvey::Withheld),
    )
}

pub(crate) const DISK_LOW_KEY: &str = "disk.low";

const LARGEST: &str = "Largest it cannot reclaim: ";
const AND_MORE: &str = " more, and aterm harness disk lists what is left now";

pub(crate) const DISK_LOW_HOG_CHARS: usize =
    aterm_messages::DETAIL_LINE_CAP - LARGEST.len() - "; 9".len() - AND_MORE.len();

const _: () = assert!(aterm_agent::harness::disk::MAX_HOGS - 2 < 10);

/// Builds the warning log record when the home volume stays below the free-space floor after cleanup.
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

/// Builds the exceptions digest entry when the home volume remains low after automatic cleanup.
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

pub(crate) const SESSION_JOURNAL_PAUSED_KEY: &str = "disk.session-journal-paused";

/// Builds the exceptions digest entry when the session journal is paused due to disk errors.
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

/// Builds the exceptions digest entry when the session journal stops for the current run.
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

pub(crate) const DISK_LOW_CLEANUP_OFF_KEY: &str = "disk.low-cleanup-off";

/// Builds the warning log record when disk space is low while automatic cleanup is disabled.
pub(crate) fn disk_low_cleanup_off_record(free_gib: u64, floor_gib: u64) -> Message {
    Message::new(tags::SYSTEM, Severity::Warn, "Disk space is low")
        .line(format!(
            "{free_gib} GiB free on the home volume, below the {floor_gib} GiB cleanup floor; no \
             cleanup ran, and aterm frees nothing until it can read the [disk] settings"
        ))
        .record()
        .key("system.disk.low")
}

/// Builds the exceptions digest entry when disk space is low and automatic cleanup was disabled by a config fault.
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

/// Builds the digest entry reporting bytes reclaimed by an automatic disk-watch pass.
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

pub(crate) const DISK_FULL_KEY: &str = "system.disk.full";

const DISK_FULL_SENTENCE: usize = 3;

/// Reason the home volume remains full after or without an automatic disk-cleanup pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiskFullCause {
    Spent,
    Unrecorded,
    Unsupervised { whole: bool },
    CleanupOff { fault: bool },
    Inodes,
    Private(DiskPrivate),
}

impl DiskFullCause {
    /// Log description of the disk-full cause using `aterm.toml` setting names.
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

/// Builds the on-screen alert when the home volume is critically full.
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

pub(crate) const PROGRAM_CHECK_STUCK_KEY: &str = "system.program-check.stuck";

const PROGRAM_CHECK_SENTENCE: usize = 3;

/// Builds the on-screen alert when macOS `syspolicyd` is wedged and blocking execs.
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
        "Most new programs cannot start on this Mac, and a privacy check can freeze the \
             screen and close every window. Logging out does not clear it.",
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

pub(crate) const DISK_SETTINGS_KEY: &str = "disk.settings";

const NOTES_MORE: &str = " more, and aterm harness disk prints each";

/// Builds the exceptions digest entry when invalid `[disk]` settings disable automatic cleanup.
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

/// The key every record of macOS's program check's backed-up episodes takes (leftover §6.5's `program-check.` stem).
pub(crate) const PROGRAM_CHECK_EPISODE_KEY: &str = "program-check.episode";

/// A backed-up episode of macOS's program check opening or draining, as the view that saw it says ([`crate::vitals_host::held_edge`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldEdge {
    /// It opened at this reading.
    Opened(aterm_vitals::jobs::HeldEpisode),
    /// It closed: its second reading in a row found nothing held.
    Drained(aterm_vitals::jobs::HeldEpisode),
}

/// Builds the record of a backed-up episode of macOS's program check opening or draining (leftover §6.5): a fact for later, never an Entry or a Now (§6.7). The opening says when aterm started none of the held programs (`aterm`, how many aterm started, is 0): aterm's grant does not reach them (§5.1). The drain's length is the longer of the episode's own span and its longest wait: an episode opens a reading after its first program was seen held, so a short one can span no time while a program waited.
pub(crate) fn program_check_episode(edge: HeldEdge, aterm: u32) -> Message {
    let programs = |n: u32| if n == 1 { "program" } else { "programs" };
    let age = aterm_uds::process::held_age;
    match edge {
        HeldEdge::Opened(episode) => {
            let opened = Message::new(
                tags::SYSTEM,
                Severity::Info,
                format!(
                    "macOS's program check is holding {} {}",
                    episode.peak,
                    programs(episode.peak)
                ),
            )
            .line(format!(
                "the oldest for {}; only this user's programs are counted",
                age(episode.oldest_ms)
            ));
            match (aterm, episode.peak) {
                (0, 1) => {
                    opened.line("it started outside aterm, so aterm's grant does not reach it")
                }
                (0, _) => {
                    opened.line("they started outside aterm, so aterm's grant does not reach them")
                }
                _ => opened,
            }
        }
        HeldEdge::Drained(episode) => Message::new(
            tags::SYSTEM,
            Severity::Info,
            "macOS's program check caught up",
        )
        .line(format!(
            "drained after {}, at most {} {} held at once",
            age(episode.for_ms.max(episode.oldest_ms)),
            episode.peak,
            programs(episode.peak)
        )),
    }
    .record()
    .key(PROGRAM_CHECK_EPISODE_KEY)
}

/// The key of the record naming this user's programs that have not started for a reason other than macOS's program check (leftover §5.1: Rosetta's translation, or a volume not proven local).
pub(crate) const NOT_STARTED_KEY: &str = "program-check.not-started";

/// Builds the record of the programs `rows` names that have not started, each with what keeps it (leftover §5.1, §6.5): "translating (Rosetta)", which aterm's Developer Tools grant does not reach, or "has not started" with nothing read to say why. It is posted when a view first names a row, so it gives no age: a row's age counts from that first sighting (`sys jobs` shows it growing). A fact for later, never an Entry or a Now: no step of aterm's moves either.
pub(crate) fn not_started_record(
    rows: &[(aterm_vitals::jobs::StartWait, &aterm_vitals::jobs::Named)],
) -> Message {
    use aterm_vitals::jobs::StartWait;
    let n = rows.len();
    let title = if n == 1 {
        "1 program has not started".to_owned()
    } else {
        format!("{n} programs have not started")
    };
    let line = |(wait, named): &(StartWait, &aterm_vitals::jobs::Named)| {
        let who = format!(
            "pid {} {}",
            named.pid,
            aterm_vitals::jobs::wire::exe_word(&named.name)
        );
        match wait {
            StartWait::Translating => format!(
                "{who}: translating (Rosetta), which aterm's Developer Tools grant does not reach"
            ),
            StartWait::NotStarted | StartWait::ProgramCheck => format!(
                "{who}: it has not started, and its program is on a volume aterm cannot prove \
                 local, so nothing read says why"
            ),
        }
    };
    Message::new(tags::SYSTEM, Severity::Info, title)
        .lines(rows.iter().map(line))
        .record()
        .key(NOT_STARTED_KEY)
}

/// Builds the exceptions digest entry when the live upgrade host cannot lock its state directory.
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

pub(crate) const UPGRADE_CANNOT_RUN_KEY: &str = "harness.upgrade-host.cannot-run";

/// Builds the exceptions digest entry when the upgrade records directory cannot be listed.
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

pub(crate) const UPGRADE_RECORDS_UNLISTED_KEY: &str = "harness.upgrade-host.records-unlisted";

/// Builds the exceptions digest entry when individual upgrade record files cannot be read.
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

pub(crate) const UPGRADE_RECORDS_UNREADABLE_KEY: &str = "harness.upgrade-host.records-unreadable";

pub(crate) const ACCOUNTS_SIGNED_OUT_KEY: &str = "harness.accounts.signed-out";

pub(crate) const ACCOUNTS_REFUSED_KEY: &str = "harness.accounts.refused";

const ACCOUNTS_LABELS_LEAD: &str = "Signed out: ";

const ACCOUNTS_LABELS_FILL: usize = 200;

/// Builds the exceptions digest entry listing signed-out Claude Code accounts.
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
    .restated_at_once();
    for line in accounts_signed_out_lines(labels) {
        entry = entry.more(line);
    }
    entry = notice_tail(entry, filled);
    Some(entry.log_line(format!("accounts need=signed-out n={}", labels.len())))
}

fn accounts_signed_out_lines(labels: &[String]) -> Vec<String> {
    labels_lines(ACCOUNTS_LABELS_LEAD, labels, NOTICE_LABEL_LINES)
}

const NOTICE_LABEL_LINES: usize = aterm_messages::exceptions::MORE_LINES - 2;

fn notice_tail(entry: Entry, filled: notice::Filled) -> Entry {
    match filled.command {
        Some(command) => entry.more(command).more(filled.expect),
        None => entry.more(filled.did).more(filled.expect),
    }
}

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

/// Extracts the first signed-out account label targeted by an [`accounts_signed_out_entry`] action.
pub(crate) fn accounts_signed_out_first(detail: &[String]) -> Option<&str> {
    detail
        .get(2)?
        .strip_prefix(ACCOUNTS_LABELS_LEAD)?
        .split([',', ' '])
        .next()
        .filter(|label| !label.is_empty())
}

pub(crate) const ACCOUNTS_CREDITS_KEY: &str = "harness.accounts.credits";

pub(crate) const ACCOUNTS_WINDOW_FULL_KEY: &str = "harness.accounts.window-full";

/// Builds the exceptions digest entry when usage credits are carrying Claude Code sessions.
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

/// Builds the exceptions digest entry when an account's weekly quota is nearly full with credits off.
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

pub(crate) const ACCOUNTS_DUE_KEY: &str = "harness.accounts.due";

pub(crate) const ACCOUNTS_NONE_SOON_KEY: &str = "harness.accounts.none-soon";

pub(crate) const ACCOUNTS_WRONG_KEY: &str = "harness.accounts.wrong-account";

const ACCOUNTS_RENEW_DUE: &str = "Run aterm harness accounts renew --due";

/// Builds the exceptions digest entry for Claude Code logins due for renewal.
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

/// Builds the exceptions digest entry when no renewable login remains working within 24 hours.
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

/// Builds the exceptions digest entry when a login renewal authenticated the wrong account.
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
    .restated_at_once();
    for line in labels_lines("Wrong account: ", labels, NOTICE_LABEL_LINES) {
        entry = entry.more(line);
    }
    entry = notice_tail(entry, filled);
    Some(entry.log_line(format!("accounts need=wrong-account n={}", labels.len())))
}

pub(crate) const RENEW_ASK_KEY: &str = "harness.accounts.renew-ask";

/// Builds the exceptions digest entry when `accounts.toml` fails admission.
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

pub(crate) const ACCOUNTS_LOGINS_UNREAD_KEY: &str = "harness.accounts.logins-unread";

/// Builds the exceptions digest entry when `logins.json` cannot be read.
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

pub(crate) const USAGE_TAP_SILENT_KEY: &str = "harness.accounts.no-tap";

/// Builds the exceptions digest entry when the usage tap writes no reading after a served turn.
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

/// Builds the exceptions digest entry when a Claude Code session file reports a build that mismatches its binary.
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

/// Builds the exceptions digest entry when the render stream gives up on repairing a tab repaint.
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

const fn render_unrepaired_reason(
    why: aterm_core::terminal::repaint_nudge::GiveUp,
) -> &'static str {
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

/// Digest key for [`render_unrepaired_entry`].
pub(crate) fn render_unrepaired_key(sid: &str) -> String {
    digest_key(Family::Render, sid, "unrepaired")
}

/// Digest key for [`palette_refused_entry`].
pub(crate) fn palette_refused_key() -> String {
    digest_key(Family::Terminal, "", "palette-refused")
}

/// Builds the exceptions digest entry when `allow_palette_reconfigure = false` blocks an OSC palette change.
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

/// Builds the exceptions digest entry when multiple tabs cannot reach the AI vendor API.
pub(crate) fn api_outage_entry(tabs: usize, since_hhmm: Option<&str>) -> Entry {
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
    .restated_at_once()
    .log_line(format!("api-outage tabs={tabs}"));
    if let Some(at) = since_hhmm {
        entry = entry.more(format!("since {at}"));
    }
    entry
}

/// Builds the exceptions digest entry when a human hold on a supervised tab has stood for an hour.
pub(crate) fn long_hold_entry(sid: &str, label: &str, mins: u64, manager: Option<&str>) -> Entry {
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

/// Builds the exceptions digest entry when a control-socket client hold has stood for an hour.
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

/// Builds the exceptions digest entries for supervised tabs blocked on human-only resume exceptions.
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
        let who = resume_limit_agent(&group);
        let (named, bare) = match kind {
            ExceptionKind::Spend => (
                who.map(|w| format!("Account limit stops {w}")),
                "Account limit reached",
            ),
            ExceptionKind::Bucket => (
                who.map(|w| format!("Model limit stops {w}")),
                "Model limit reached",
            ),
            ExceptionKind::GoalCleared => (None, "Claude lost its goal"),
            ExceptionKind::Futile => (
                who.map(|w| format!("Limit still holds {w}")),
                "Limit still holds",
            ),
            ExceptionKind::Login
            | ExceptionKind::Stalled
            | ExceptionKind::Dialog
            | ExceptionKind::Context
            | ExceptionKind::Refused => continue,
        };
        let placed = |head: &str| {
            if n == 1 {
                placed_title(head, &first_spot.place, "")
            } else {
                format!("{head} in {n} tabs")
            }
        };
        let fits = |t: &String| aterm_messages::text::glass_title_fault(t).is_none();
        let what = named
            .iter()
            .map(String::as_str)
            .chain([bare])
            .map(placed)
            .find(fits)
            .or_else(|| named.clone().filter(fits))
            .unwrap_or_else(|| bare.to_string());
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

pub(crate) const RESUME_ASK_KEY: &str = "harness.resume-ask";

/// Supersede key (`harness.resume-ask.<tab>`) for a tab's [`resume_ask`] row.
pub(crate) fn resume_ask_key(sid: &str) -> String {
    format!(
        "{RESUME_ASK_KEY}.{}",
        atpkg::progress::sanitize_for_tty(sid, 64)
    )
}

/// Supersede key for a tab's [`resume_ask`] row scoped to an optional `NEEDS YOU` notice code.
pub(crate) fn notice_ask_key(sid: &str, notice: Option<&str>) -> String {
    match notice {
        Some(code) => format!("{}.{code}", resume_ask_key(sid)),
        None => resume_ask_key(sid),
    }
}

/// Builds the immediate on-screen decision row for an unescalated [`Urgency::Now`] resume exception.
pub(crate) fn resume_ask(sid: &str, spot: &UpgradeSpot, exception: &Exception) -> Message {
    let clean = |line: &str| atpkg::progress::sanitize_for_tty(line, 240);
    let notice = exception.notice.as_ref();
    let title = notice.map_or("Claude needs you", |n| n.title);
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
        if let Some(command) = &n.command {
            message = message.line(clean(command));
        }
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

fn notice_then(step: &str, short: Option<&str>) -> Then {
    let then = Then::step(step);
    match short {
        Some(short) => then.or_short(short),
        None => then,
    }
}

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

const fn resume_agent_word(agent: aterm_phase::Program) -> &'static str {
    match agent {
        aterm_phase::Program::Claude => "Claude",
        aterm_phase::Program::Generic => "The agent",
        aterm_phase::Program::Codex
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse
        | aterm_phase::Program::Gemini => agent.product(),
    }
}

fn resume_limit_agent(
    group: &[&(String, Exception, aterm_phase::Program, UpgradeSpot)],
) -> Option<&'static str> {
    let mut agents = group.iter().map(|(_, _, agent, _)| *agent);
    let first = agents.next()?;
    (first != aterm_phase::Program::Generic && agents.all(|a| a == first))
        .then(|| resume_agent_word(first))
}

fn stall_record_key(sid: &str) -> String {
    host_record_key("stalled", sid)
}

/// Builds the warning log record when a supervised tab stalls on an unclassified state.
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

/// Builds the warning log record when an agent stalls waiting on its own background work.
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

/// Builds the warning log record when a supervised tab's child command stops making progress.
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

/// Digest keys (`[blocked, gone, lost]`) for a tab's agent relaunch entries.
pub(crate) fn relaunch_keys(sid: &str) -> [String; 3] {
    [
        digest_key(Family::Supervisor, sid, "not-back"),
        digest_key(Family::Supervisor, sid, "gone"),
        digest_key(Family::Supervisor, sid, "lost"),
    ]
}

/// Digest key for [`relaunch_not_restored_entry`].
pub(crate) fn relaunch_not_restored_key(sid: &str) -> String {
    digest_key(Family::Supervisor, sid, "not-restored")
}

/// Builds the exceptions digest entry when a resumed Claude Code build reports unrestored session state.
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

/// Builds the warning log record for an agent relaunch event.
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

/// Builds the exceptions digest entry when an agent relaunch has been blocked for an hour.
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

/// Builds the exceptions digest entry when an agent relaunch repeatedly fails without a recovery command.
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

/// Builds the exceptions digest entry when an exited agent cannot be automatically relaunched.
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

/// Builds the exceptions digest entry when an exited agent with a known conversation cannot be relaunched.
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

/// Extracts the tab identifier from a per-tab [`agent_upgrade_key`].
pub(crate) fn agent_upgrade_key_tab(key: &str) -> Option<&str> {
    key.strip_prefix(KEY_AGENT_UPGRADE)?
        .strip_prefix('.')
        .filter(|tab| !tab.is_empty() && *tab != "done")
}

pub(crate) const STALL_MOVE_LINE: usize = 2;

/// Returns whether two stalled-upgrade detail slices describe the same move and wait reason.
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

fn stall_lines(d: &[String]) -> Option<(&str, &str)> {
    match d.iter().position(|l| strip_place(l).is_some()) {
        Some(at) => Some((d.get(at.checked_sub(2)?)?, &d[at])),
        None => Some((d.first()?, d.get(STALL_MOVE_LINE)?)),
    }
}

/// Returns how many leading detail lines of a message form its prose sentence.
pub(crate) fn sentence_lines(key: Option<&str>, detail: &[String]) -> usize {
    if key.and_then(agent_upgrade_key_tab).is_some()
        && let Some(at) = detail.iter().position(|l| strip_place(l).is_some())
    {
        return at.max(1);
    }
    if key.is_some_and(is_notice_key) {
        return detail.len().max(1);
    }
    if key == Some(DISK_FULL_KEY) {
        return DISK_FULL_SENTENCE.min(detail.len()).max(1);
    }
    if key == Some(PROGRAM_CHECK_STUCK_KEY) {
        return PROGRAM_CHECK_SENTENCE.min(detail.len()).max(1);
    }
    if key == Some(KEY_INTEGRATION_LOST) || key == Some(KEY_HARNESS_NO_SOCKET) {
        let counted = key == Some(KEY_INTEGRATION_LOST)
            && detail.get(1).is_some_and(|l| integration_rest_count(l));
        return (2 + usize::from(counted)).min(detail.len()).max(1);
    }
    1
}

fn is_notice_key(key: &str) -> bool {
    key == RENEW_ASK_KEY
        || key == ACCOUNTS_SIGNED_OUT_KEY
        || key == ACCOUNTS_WRONG_KEY
        || key.starts_with(&format!("{RESUME_ASK_KEY}."))
}

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

/// Formats a 1-based tab position (`in tab 2` or `in window 1, tab 2`).
pub(crate) fn tab_place(window: Option<usize>, tab: usize) -> String {
    match window {
        Some(window) => format!("in window {window}, tab {tab}"),
        None => format!("in tab {tab}"),
    }
}

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

/// Builds the log record when a stalled upgrade resolves after its row was dismissed.
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

/// Outcome of a stalled upgrade that resolved while its row was dismissed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StallEnd {
    AsksAgain,
    MovesAgain,
    Left,
}

const fn agent_word(agent: aterm_agent::harness::upgrade::Agent) -> &'static str {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "Claude",
        aterm_agent::harness::upgrade::Agent::Codex => "Codex",
        aterm_agent::harness::upgrade::Agent::Muse
        | aterm_agent::harness::upgrade::Agent::Agy
        | aterm_agent::harness::upgrade::Agent::Gemini => agent.product(),
    }
}

fn own_work_round(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> bool {
    row.phase == aterm_agent::harness::upgrade::Phase::Pending && row.will_ask_again(now)
}

/// Returns the ordered operator actions available for a tab's upgrade row.
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
        None => {
            !matches!(row.phase, Phase::Failed(_))
                && row.request != aterm_agent::harness::upgrade::Request::Now
        }
        Some(Remedy::Now) => !own_work_round(row, now),
        Some(Remedy::AskAgain) => true,
        Some(Remedy::Waits | Remedy::InItsPane | Remedy::ByHand | Remedy::ResumeInTab) => false,
    };
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

/// Returns whether pressing `Upgrade now` on `row` re-asks the agent rather than restarting at its next pause.
pub(crate) fn upgrade_now_asks_again(row: &aterm_agent::harness::upgrade_drive::Row) -> bool {
    row.wait == "queued"
        || (!row.quiet
            && matches!(
                &row.phase,
                aterm_agent::harness::upgrade::Phase::Failed(why)
                    if why == aterm_agent::harness::upgrade::GAVE_UP
            ))
}

/// Returns the primary two action capsules for an upgrade row.
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

/// Builds the log record confirming an operator's upgrade action (`Upgrade now`, `Defer`, `Skip`).
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

/// Builds the feedback message when an operator's upgrade action is refused.
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
    let title = match word {
        UpgradeWord::Now => placed_title(&format!("Couldn't upgrade {who}"), place, " now"),
        UpgradeWord::NotToday => placed_title("Couldn't postpone the upgrade", place, ""),
        UpgradeWord::Skip => placed_title(&format!("Couldn't skip {who} {shown}"), place, ""),
    };
    let mut again = false;
    let (reason, then, plain) = if why.starts_with("busy:") {
        again = true;
        (
            "press it again".to_string(),
            Some("another step held the upgrade's lock, so nothing was written".to_string()),
            true,
        )
    } else if let Some(now_to) = why.strip_prefix("stale:") {
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

/// Formats the resolution log title when a stalled upgrade completes (`<Product> upgraded in <place>`).
pub(crate) fn agent_upgrade_went_through(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> String {
    format!("{} upgraded {place}", agent_word(row.agent))
}

const MOVED_SAID_AFTER_S: u64 = 10 * 60;

/// Formats the `moved at ...` detail line for a completed upgrade record.
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

/// Builds the completion log record when a live agent upgrade finishes.
pub(crate) fn agent_upgrade_done(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> Message {
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 160);
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

/// Builds the first-open install posture message when aterm runs from an non-updatable location.
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

/// Settings ▸ Packages action initiated by the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesVerb {
    Check,
    Install,
    Remove,
}

impl PackagesVerb {
    /// Banner title while the package action is running.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Check => "Checking ALab tools",
            Self::Install => crate::toolchain_words::INSTALLING_ALAB,
            Self::Remove => "Removing ALab tools",
        }
    }

    /// Completion title when the package action finishes with no planned changes.
    pub(crate) const fn finished(self) -> &'static str {
        match self {
            Self::Check => crate::toolchain_words::PACKAGES_FINISHED,
            Self::Install => crate::toolchain_words::PACKAGES_INSTALLED,
            Self::Remove => "ALab tools removed",
        }
    }

    /// Completion title when the package action finishes after applying planned work.
    pub(crate) const fn finished_work(self) -> &'static str {
        match self {
            Self::Check => "ALab tools updated",
            Self::Install | Self::Remove => self.finished(),
        }
    }
}

pub(crate) const WAITING_FOR_ALAB: &str = "Waiting for another ALab update";

/// Builds the busy banner row shown before a user-initiated package action computes its plan.
pub(crate) fn packages_verb_row(verb: PackagesVerb) -> Message {
    let (glyph, load) = match verb {
        PackagesVerb::Check => ('\u{21bb}', None),
        PackagesVerb::Install => ('\u{21e3}', None),
        PackagesVerb::Remove => ('\u{2296}', Some(aterm_messages::Load::Disk)),
    };
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

/// Builds the busy banner row shown while a user-initiated package action waits on the store lock.
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

const CONTROL_MARK: char = '\u{00b7}';

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

fn split_sentence(words: &str) -> (&str, &str) {
    words.split_once(": ").unwrap_or((words, ""))
}

const _: () = assert!(GLASS_TITLE_CHARS < TITLE_CAP);

#[cfg(test)]
#[path = "message_reporters_tests.rs"]
mod tests;
