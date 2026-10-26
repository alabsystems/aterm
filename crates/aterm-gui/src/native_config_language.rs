// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded language assistance for the canonical `aterm.toml` editor.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use aterm_digest::Sha256;

use crate::native_editor::{
    EditorDiagnosticSpan, EditorSyntaxClass, EditorSyntaxSpan, EditorViewportProjection,
};
use crate::prefs::{EditField, EditKind};

pub(crate) const MAX_CONFIG_ANALYSIS_BYTES: usize =
    crate::native_config_service::MAX_CONFIG_FILE_BYTES;
const MAX_SYNTAX_SPANS: usize = 16 * 1024;
const MAX_DIAGNOSTICS: usize = 32;
const MAX_COMPLETIONS: usize = 8;
const MAX_CONTEXT_LINE_BYTES: usize = 8 * 1024;
const MAX_COMPLETION_ACTION_BYTES: usize = 64;
pub(crate) const CONFIG_COMPLETION_ACTION_PREFIX: &str = "editor/config-completion/";
const COMPATIBILITY_ONLY_KEYS: &[&str] = &[
    "matrix_rain.materialize",
    "matrix_rain.ink_text",
    "matrix_rain.phosphor",
];
/// A retired key's metadata, shared with read-only inventory surfaces such as Settings ▸ Modified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetiredConfigKeyMetadata {
    pub(crate) key: &'static str,
    pub(crate) feature: &'static str,
    pub(crate) effect_label: &'static str,
}

const RETIRED_CONFIG_KEYS: &[RetiredConfigKeyMetadata] = &[
    RetiredConfigKeyMetadata {
        key: "show_hud",
        feature: "Bottom HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "show_resources_hud",
        feature: "Bottom HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "show_engine_hud",
        feature: "Bottom HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "pkg_progress_effects",
        feature: "Package progress card",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "notice_sparkle",
        feature: "Update celebration",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "show_scene_hud",
        feature: "Scene HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "scene_rows",
        feature: "Scene HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "scene_skin",
        feature: "Scene HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "scene_panels",
        feature: "Scene HUD",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "sparkle_words.feline.idle",
        feature: "Keyword Kitty idle animation",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "sparkle_words.feline.gaze",
        feature: "Keyword Kitty gaze tracking",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "sparkle_words.feline.color",
        feature: "Keyword Kitty tint control",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "sparkle_words.feline.intensity",
        feature: "Keyword Kitty opacity control",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "cursor_trail_wake_ms",
        feature: "Rainbow kitty typing wake",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "packages.channel",
        feature: "Package release channel",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "packages.include",
        feature: "Package include filter",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "presence.band",
        feature: "Presence band",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "choice_sound",
        feature: "Supervisor choice chime",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "robi",
        feature: "Robi the helper robot",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "descriptive_titles",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_provider",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_model",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_endpoint",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_token_file",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_timeout_seconds",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_proxy_mode",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_ca_file",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_interval_seconds",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_context_lines",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_include_output",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "title_summary_allow_remote",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "tab_title_format",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "window_title_format",
        feature: "Smart Titles",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "net",
        feature: "Network drive",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "net.connections",
        feature: "Network drive",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "operator.enabled",
        feature: "Embedded operator",
        effect_label: "No effect",
    },
    RetiredConfigKeyMetadata {
        key: "explain_heavy_load",
        feature: "Explain heavy load",
        effect_label: "No effect",
    },
    // THE MERGED TOP DECK WAS REJECTED (2026-10-24): 0.135.0's hidden `[sys] stats_deck_style`
    // chose its painter. A file that still sets it loads and is told the key does nothing.
    RetiredConfigKeyMetadata {
        key: "sys.stats_deck_style",
        feature: "Merged top deck",
        effect_label: "No effect",
    },
];

pub(crate) fn retired_config_key(key: &str) -> Option<&'static RetiredConfigKeyMetadata> {
    RETIRED_CONFIG_KEYS
        .iter()
        .find(|metadata| metadata.key == key)
}

pub(crate) fn is_compatibility_only_key(key: &str) -> bool {
    COMPATIBILITY_ONLY_KEYS.contains(&key) || retired_config_key(key).is_some()
}

const CURSOR_STYLE_ALIASES: &[&str] = &["beam"];
const BIDI_ALIASES: &[&str] = &["on", "off"];
const AMBIGUOUS_WIDTH_ALIASES: &[&str] = &["single", "double"];
const PREDICTIVE_ECHO_ALIASES: &[&str] = &["auto", "on", "true", "force"];
const TEXT_BLENDING_ALIASES: &[&str] = &["linear_corrected"];
const MOTION_ALIASES: &[&str] = &["reduce"];
const WINDOW_COLORSPACE_ALIASES: &[&str] = &["displayp3", "p3"];
const BACKGROUND_MATERIAL_ALIASES: &[&str] = &["underwindow", "under_window", ""];
const CUSTOM_INK_COLORWAYS: &[&str] = &["rainbow", "twotone:#RRGGBB,#RRGGBB"];
const CUSTOM_BURST_KINDS: &[&str] = &["sparkle", "nova", "supernova", "starburst", "glow"];
const CUSTOM_BURST_KIND_ALIASES: &[&str] = &["super_nova", "super-nova"];
const CUSTOM_GRAPHIC_COLLECTIONS: &[&str] = &["cats"];
const WINDOWING_BEHAVIORS: &[&str] = &["new_window", "attach"];
const FONT_HINTINGS: &[&str] = &["full", "light", "off"];
const FONT_SUBPIXELS: &[&str] = &["off", "rgb", "bgr"];
const RIGHT_CLICK_GESTURES: &[&str] = &["copy_paste", "off"];
const RIGHT_CLICK_ALIASES: &[&str] = &["copy-paste"];
const TAB_MENU_CHORDS: &[&str] = &["on", "menu_key", "off"];
const TAB_MENU_CHORD_ALIASES: &[&str] = &["both", "menu-key", "menu"];
const HARNESS_AUTOPILOT_ALIASES: &[&str] = &["forever", "infinity", "wrap-up", "wrapup", "wrap"];
const PRIVACY_WARMUPS: &[&str] = &["never", "on-request", "automatic"];
const REPAINT_NUDGE_MODES: &[&str] = &["on", "log", "off"];
const TAB_BAND_HEIGHTS: &[&str] = &["compact", "standard"];
const FLEET_ALLOWS: &[&str] = &[
    aterm_net::fleet::FleetScope::Facts.word(),
    aterm_net::fleet::FleetScope::Content.word(),
    aterm_net::fleet::DRIVE_LEVEL,
];
const UNIVERSAL_CONTROL_POLICIES: &[&str] = &["off", "leave"];
const STATS_BARS_FORMS: &[&str] = &[
    crate::stats_deck::StatsBars::Deck.word(),
    crate::stats_deck::StatsBars::Sill.word(),
    crate::stats_deck::StatsBars::Off.word(),
];
const STATS_DECK_SIZES: &[&str] = &[
    crate::stats_plan::DeckSizePref::Auto.word(),
    crate::stats_plan::DeckSizePref::Full.word(),
    crate::stats_plan::DeckSizePref::Folded.word(),
];
const WINDOWING_BEHAVIOR_ALIASES: &[&str] = &[
    "new-window",
    "newwindow",
    "new",
    "usenew",
    "use_existing",
    "use-existing",
    "useexisting",
    "existing",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigDiagnostic {
    pub(crate) bytes: Range<usize>,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) severity: ConfigDiagnosticSeverity,
    pub(crate) message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigSyntaxSpan {
    bytes: Range<usize>,
    class: EditorSyntaxClass,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigAnalysis {
    syntax: Vec<ConfigSyntaxSpan>,
    pub(crate) diagnostics: Vec<ConfigDiagnostic>,
    omitted_diagnostics: usize,
    retired_spellings: Vec<ConfigDiagnostic>,
    unaccepted_values: Vec<ConfigDiagnostic>,
    assist_index: ConfigAssistIndex,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ConfigAssistIndex {
    lines: Vec<AssistLineState>,
    tables: Vec<String>,
    authored: HashMap<String, AuthoredPath>,
    disk: Vec<aterm_agent::harness::cli::DiskFault>,
    theme_names: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AssistLineState {
    start: usize,
    table: u32,
    scope: u32,
    multiline: Option<MultilineString>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AuthoredPath {
    all: AuthoredOccurrences,
    scopes: HashMap<u32, AuthoredOccurrences>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AuthoredOccurrences {
    first_key: Range<usize>,
    count: usize,
}

impl ConfigAnalysis {
    /// Take the names of the theme files that loaded in `themes` (the catalog the host lane checks this revision against), so completion offers the themes Settings offers.
    pub(crate) fn adopt_theme_names(&mut self, themes: &crate::app_config::ThemeCatalog) {
        self.assist_index.theme_names = themes
            .ready_names()
            .filter(|name| aterm_types::scheme::builtin_theme(name).is_none())
            .map(str::to_string)
            .collect();
    }

    pub(crate) fn pending_failure(message: impl Into<String>) -> Self {
        Self {
            syntax: Vec::new(),
            diagnostics: vec![ConfigDiagnostic {
                bytes: 0..0,
                line: 1,
                column: 1,
                severity: ConfigDiagnosticSeverity::Error,
                message: message.into(),
            }],
            omitted_diagnostics: 0,
            retired_spellings: Vec::new(),
            unaccepted_values: Vec::new(),
            assist_index: ConfigAssistIndex::default(),
        }
    }

    pub(crate) fn too_large() -> Self {
        Self::pending_failure(format!(
            "file exceeds the {} KiB live-validation limit",
            MAX_CONFIG_ANALYSIS_BYTES / 1024
        ))
    }

    pub(crate) fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error)
    }

    #[cfg(test)]
    pub(crate) fn first_error(&self) -> Option<&ConfigDiagnostic> {
        self.diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error)
    }

    pub(crate) const fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }

    /// Blocking errors first, then warnings, each in source order: the initial Manual status matches the Save gate, and F8 still reaches every retained problem.
    pub(crate) fn diagnostic_at(&self, index: usize) -> Option<&ConfigDiagnostic> {
        let error_count = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error)
            .count();
        if index < error_count {
            self.diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Error)
                .nth(index)
        } else {
            self.diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
                .nth(index - error_count)
        }
    }

    pub(crate) fn summary_at(&self, index: usize) -> Option<String> {
        self.summary_forms_at(index)
            .and_then(|forms| forms.into_iter().next())
    }

    /// The diagnostic at `index` as the whole summary first, then shorter forms for a
    /// footer too small for it: without its key hint and count, then without its place
    /// too, and last the message alone (the underline in the buffer still marks the
    /// problem). Every form keeps the message whole.
    pub(crate) fn summary_forms_at(&self, index: usize) -> Option<Vec<String>> {
        let count = self.diagnostic_count();
        let index = index.min(count.saturating_sub(1));
        let diagnostic = self.diagnostic_at(index)?;
        let level = match diagnostic.severity {
            ConfigDiagnosticSeverity::Warning => "warning",
            ConfigDiagnosticSeverity::Error => "error",
        };
        let navigation = if count > 1 {
            format!(" · {} of {count} · F8 / Shift-F8", index + 1)
        } else {
            " · F8 jumps there".to_string()
        };
        let omitted = if self.omitted_diagnostics == 0 {
            String::new()
        } else {
            format!(" · {} more not shown", self.omitted_diagnostics)
        };
        let place = format!(
            "Config {level} · Ln {}, Col {} · {}",
            diagnostic.line, diagnostic.column, diagnostic.message,
        );
        Some(vec![
            format!("{place}{navigation}{omitted}"),
            place,
            format!("Config {level} · {}", diagnostic.message),
            diagnostic.message.clone(),
        ])
    }

    pub(crate) fn summary(&self) -> Option<String> {
        self.summary_at(0)
    }

    fn insert_diagnostic(&mut self, diagnostic: ConfigDiagnostic) -> bool {
        if self.diagnostics.contains(&diagnostic) {
            return false;
        }
        if self.diagnostics.len() < MAX_DIAGNOSTICS {
            self.diagnostics.push(diagnostic);
            return true;
        }
        if diagnostic.severity == ConfigDiagnosticSeverity::Error
            && let Some(index) = self
                .diagnostics
                .iter()
                .rposition(|entry| entry.severity == ConfigDiagnosticSeverity::Warning)
        {
            self.diagnostics.remove(index);
            self.diagnostics.push(diagnostic);
            self.omitted_diagnostics = self.omitted_diagnostics.saturating_add(1);
            return true;
        }
        self.omitted_diagnostics = self.omitted_diagnostics.saturating_add(1);
        true
    }

    /// Merge the host's filesystem-backed diagnostics for this exact source revision (stale-checked by the caller).
    pub(crate) fn merge_host_diagnostics(&mut self, diagnostics: Vec<ConfigDiagnostic>) -> bool {
        let mut changed = false;
        for diagnostic in diagnostics {
            changed |= self.insert_diagnostic(diagnostic);
        }
        changed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfigCompletionEdit {
    pub(crate) replacement: Range<usize>,
    pub(crate) expected: String,
    pub(crate) insertion: String,
    /// Byte offsets within `insertion` selected after acceptance.
    pub(crate) post_insert_selection: Range<usize>,
    pub(crate) display: String,
    pub(crate) help: String,
}

/// The exact context one config-completion control was rendered from: the document sequence guards the source bytes, the caret the completion locus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConfigCompletionContext {
    document: u64,
    document_sequence: u64,
    caret: usize,
}

impl ConfigCompletionContext {
    pub(crate) const fn new(document: u64, document_sequence: u64, caret: usize) -> Self {
        Self {
            document,
            document_sequence,
            caret,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedCompletionAction<'a> {
    index: usize,
    identity_hex: &'a str,
}

/// Bind a completion action to the exact document, sequence, caret, replacement range, expected bytes, insertion and post-insert selection that authored its label.
pub(crate) fn config_completion_action(
    context: ConfigCompletionContext,
    index: usize,
    completion: &ConfigCompletionEdit,
) -> String {
    let identity = completion_identity(context, index, completion);
    let action = format!("{CONFIG_COMPLETION_ACTION_PREFIX}{index}/{identity}");
    debug_assert!(action.len() <= MAX_COMPLETION_ACTION_BYTES);
    action
}

/// Resolve a rendered action only while every source and view guard still matches.
#[cfg(test)]
pub(crate) fn resolve_config_completion_action(
    source: &str,
    context: ConfigCompletionContext,
    action: &str,
) -> Option<ConfigCompletionEdit> {
    let assist = assist(source, context.caret);
    resolve_config_completion_from_assist(source, context, action, assist)
}

/// Production activation path.
pub(crate) fn resolve_config_completion_action_with_analysis(
    source: &str,
    context: ConfigCompletionContext,
    action: &str,
    analysis: &ConfigAnalysis,
) -> Option<ConfigCompletionEdit> {
    let assist = assist_with_analysis(source, context.caret, analysis);
    resolve_config_completion_from_assist(source, context, action, assist)
}

fn resolve_config_completion_from_assist(
    source: &str,
    context: ConfigCompletionContext,
    action: &str,
    assist: ConfigAssist,
) -> Option<ConfigCompletionEdit> {
    let parsed = parse_config_completion_action(action)?;
    let completion = assist.completions.into_iter().nth(parsed.index)?;
    if source.get(completion.replacement.clone()) != Some(completion.expected.as_str())
        || completion_identity(context, parsed.index, &completion) != parsed.identity_hex
    {
        return None;
    }
    Some(completion)
}

/// Structural classifier for the shared native-command authority registry.
pub(crate) fn is_config_completion_action(action: &str) -> bool {
    parse_config_completion_action(action).is_some()
}

fn parse_config_completion_action(action: &str) -> Option<ParsedCompletionAction<'_>> {
    if action.len() > MAX_COMPLETION_ACTION_BYTES {
        return None;
    }
    let mut parts = action
        .strip_prefix(CONFIG_COMPLETION_ACTION_PREFIX)?
        .split('/');
    let index = parts.next()?.parse::<usize>().ok()?;
    let identity_hex = parts.next()?;
    if parts.next().is_some()
        || index >= MAX_COMPLETIONS
        || identity_hex.len() != 32
        || !identity_hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    Some(ParsedCompletionAction {
        index,
        identity_hex,
    })
}

fn completion_identity(
    context: ConfigCompletionContext,
    index: usize,
    completion: &ConfigCompletionEdit,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"aterm-config-completion-v1\0");
    for value in [
        context.document,
        context.document_sequence,
        context.caret as u64,
        index as u64,
        completion.replacement.start as u64,
        completion.replacement.end as u64,
        completion.post_insert_selection.start as u64,
        completion.post_insert_selection.end as u64,
    ] {
        digest.update(value.to_le_bytes());
    }
    for bytes in [
        completion.expected.as_bytes(),
        completion.insertion.as_bytes(),
    ] {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    aterm_codec::hex::encode_lower(&digest.finalize()[..16])
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigAssist {
    pub(crate) help: Option<String>,
    pub(crate) completions: Vec<ConfigCompletionEdit>,
    /// The caret ends a partly typed key, table or value, one that is not yet a whole name: the moment completion is worth showing unasked.
    pub(crate) typed: bool,
    /// The help a caret resting here shows while the list waits: the key or table under it, never whichever suggestion sorts first.
    pub(crate) resting_help: Option<String>,
}

/// One value shape in the canonical Settings/Manual language registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigSchemaKind {
    Scalar(EditKind),
    StringList,
    TextOrStringList,
    DynamicStringMap,
    StructuredList,
    Table,
    Flexible,
}

impl ConfigSchemaKind {
    pub(crate) const fn is_assignable(self) -> bool {
        !matches!(
            self,
            Self::Table | Self::DynamicStringMap | Self::StructuredList
        )
    }

    const fn is_table_header(self) -> bool {
        matches!(
            self,
            Self::Table | Self::DynamicStringMap | Self::StructuredList
        )
    }

    const fn is_array_table(self) -> bool {
        matches!(self, Self::StructuredList)
    }
}

/// Shared metadata consumed by validation, completion, search, diagnostics and Settings ▸ Modified, so every surface discovers a new entry together.
#[derive(Clone, Debug)]
pub(crate) struct ConfigSchemaEntry {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: ConfigSchemaKind,
    placeholder: String,
    keywords: &'static [&'static str],
    /// A native scalar has a lossless `EditField` representation.
    pub(crate) native_scalar: bool,
    /// Only an exact registered Manual path may emit a one-key reset patch, a whole dynamic map or structured list included (Reset is labeled as clearing it).
    pub(crate) manual_reset_safe: bool,
}

#[derive(Clone, Copy)]
struct ManualSchemaEntry {
    key: &'static str,
    label: &'static str,
    kind: ConfigSchemaKind,
    keywords: &'static [&'static str],
    manual_reset_safe: bool,
}

const fn manual(
    key: &'static str,
    label: &'static str,
    kind: ConfigSchemaKind,
    keywords: &'static [&'static str],
    manual_reset_safe: bool,
) -> ManualSchemaEntry {
    ManualSchemaEntry {
        key,
        label,
        kind,
        keywords,
        manual_reset_safe,
    }
}

const MANUAL_SCHEMA: &[ManualSchemaEntry] = &[
    manual(
        "keybindings",
        "Keyboard shortcut map",
        ConfigSchemaKind::DynamicStringMap,
        &["shortcut", "chord", "action", "keyboard"],
        true,
    ),
    manual(
        "fabric.hold_on_bridge_lost",
        "Halt sessions when the fabric bridge is lost",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["fabric", "bridge", "hold", "halt", "strict"],
        true,
    ),
    manual(
        "fabric.receipts",
        "Acknowledge senders when their mail is handled",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["fabric", "receipts", "ack", "inbox", "seen"],
        true,
    ),
    manual(
        "fabric.presence",
        "What this node's fabric presence rows publish",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: &["meta", "minimal"],
        }),
        &["fabric", "presence", "meta", "minimal", "privacy"],
        true,
    ),
    manual(
        "fabric.log",
        "Mirror the session logs' turns onto the fabric",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: &["auto", "off", "turns"],
        }),
        &["fabric", "log", "session", "turns", "mirror", "bus", "auto"],
        true,
    ),
    manual(
        "fabric.command",
        "Fabric bridge command",
        ConfigSchemaKind::TextOrStringList,
        &[
            "fabric",
            "bridge",
            "inbox",
            "post",
            "astream",
            "bus",
            "aterm-link",
        ],
        true,
    ),
    manual(
        "fabric",
        "Fabric bridge",
        ConfigSchemaKind::Table,
        &["fabric", "bridge", "inbox", "astream", "bus"],
        false,
    ),
    manual(
        "fleet",
        "Fleet listener",
        ConfigSchemaKind::Table,
        &["fleet", "survey", "network", "listener", "lan"],
        false,
    ),
    manual(
        "fleet.peers",
        "Fleet peers named by hand",
        ConfigSchemaKind::StructuredList,
        &["fleet", "survey", "peer", "host", "lan"],
        true,
    ),
    manual(
        "fleet.peers.host",
        "Fleet peer host and port",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["fleet", "peer", "host", "port"],
        false,
    ),
    manual(
        "fleet.listen",
        "Fleet listener numeric IP and port",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["fleet", "survey", "network", "listener", "port", "lan"],
        true,
    ),
    manual(
        "fleet.watch",
        "Read the fleet every minute",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["fleet", "survey", "menu bar", "status", "watch"],
        true,
    ),
    // No Reset: a one-key write would leave `drive_from` below drive (`aterm fleet allow`).
    manual(
        "fleet.allow",
        "What a fleet peer may read",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: FLEET_ALLOWS,
        }),
        &["fleet", "survey", "facts", "content", "privacy"],
        false,
    ),
    manual(
        "fleet.drive_from",
        "Fleet nodes that may drive this Mac",
        ConfigSchemaKind::StringList,
        &["fleet", "drive", "step-in", "take over", "node"],
        true,
    ),
    manual(
        "fleet.accounts_source",
        "Where this machine's fleet accounts come from",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["fleet", "accounts", "sync", "source", "roster"],
        true,
    ),
    manual(
        "agents_auto_prime",
        "Prime coding agents automatically",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["agents", "claude", "codex", "primer", "guidance"],
        true,
    ),
    manual(
        "spare_shell",
        "Warm spare shell for new tabs",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: &crate::spare::SpareMode::SPELLINGS,
        }),
        &["spare", "warm", "new tab", "prompt", "startup", "fast"],
        true,
    ),
    manual(
        "spare_shell_needs_person",
        "Keep a spare shell only while a person is present",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["spare", "warm", "headless", "person", "presence"],
        true,
    ),
    manual(
        "conpty",
        "Console host for new tabs (Windows)",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &[
            "conpty",
            "console",
            "conhost",
            "openconsole",
            "windows",
            "host",
        ],
        true,
    ),
    manual(
        "windowing_behavior",
        "Where a new terminal opens",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: WINDOWING_BEHAVIORS,
        }),
        &["launch", "attach", "instance", "window", "tab", "single"],
        true,
    ),
    manual(
        "font_hinting",
        "Glyph grid-fitting (Linux/Windows)",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: FONT_HINTINGS,
        }),
        &[
            "hint",
            "grid",
            "autohinter",
            "crisp",
            "linux",
            "windows",
            "stem",
        ],
        true,
    ),
    manual(
        "font_subpixel",
        "Linux subpixel-RGB text",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: FONT_SUBPIXELS,
        }),
        &["lcd", "cleartype", "rgb", "bgr", "linux", "antialias"],
        true,
    ),
    manual(
        "right_click",
        "Right-click gesture",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: RIGHT_CLICK_GESTURES,
        }),
        &["mouse", "paste", "copy", "context", "button"],
        true,
    ),
    manual(
        "tab_menu_chord",
        "Tab context-menu keyboard chord",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: TAB_MENU_CHORDS,
        }),
        &["menu", "shift", "f10", "keyboard", "windows", "tab"],
        true,
    ),
    manual(
        "tab_band_height",
        "Tab band height",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: TAB_BAND_HEIGHTS,
        }),
        &["strip", "band", "compact", "chrome", "tab"],
        true,
    ),
    manual(
        "key_sequences",
        "Terminal key sequence map",
        ConfigSchemaKind::DynamicStringMap,
        &["keyboard", "escape", "pty", "bytes"],
        true,
    ),
    manual(
        "palette_overrides",
        "Palette slot overrides",
        ConfigSchemaKind::DynamicStringMap,
        &["palette", "ansi", "colors", "colours", "slot", "indexed"],
        true,
    ),
    manual(
        "update",
        "Software update",
        ConfigSchemaKind::Table,
        &["upgrade", "release", "github"],
        false,
    ),
    manual(
        "update.owner",
        "Update channel owner (development)",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["upgrade", "release", "github", "development"],
        true,
    ),
    manual(
        "update.repo",
        "Update channel repository (development)",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["upgrade", "release", "github", "development"],
        true,
    ),
    manual(
        "update.require_team_id",
        "Required Team ID (development)",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["upgrade", "signing", "team", "development"],
        true,
    ),
    manual(
        "keeper",
        "PTY keeper",
        ConfigSchemaKind::Table,
        &["keeper", "crash", "recover", "pty", "session"],
        false,
    ),
    manual(
        "keeper.enabled",
        "Keep shells alive across a crash",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["keeper", "crash", "recover", "pty", "session"],
        true,
    ),
    manual(
        "session_log",
        "Session log",
        ConfigSchemaKind::Table,
        &["session", "log", "journal", "crash", "history"],
        false,
    ),
    manual(
        "session_log.journal",
        "Keep a session journal on disk",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["session", "log", "journal", "crash", "history"],
        true,
    ),
    manual(
        "session_log.clause",
        "Show what a tab is doing in its tooltip",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["session", "log", "clause", "tooltip", "tab", "now"],
        true,
    ),
    manual(
        "sys",
        "System awareness",
        ConfigSchemaKind::Table,
        &["system", "cpu", "gpu", "memory", "disk", "stats", "cores"],
        false,
    ),
    manual(
        "sys.stats_bars",
        "Stats bars: under the titlebar, in the bottom padding, or off",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: STATS_BARS_FORMS,
        }),
        &[
            "system", "cpu", "gpu", "memory", "disk", "stats", "deck", "sill", "bars", "lens",
        ],
        true,
    ),
    manual(
        "sys.stats_deck_size",
        "Stats deck size: automatic (folded), full or folded",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: STATS_DECK_SIZES,
        }),
        &[
            "system", "stats", "deck", "size", "full", "folded", "height", "rows",
        ],
        true,
    ),
    manual(
        "sys.disk_roots",
        "Folders the disk walker may size in ~/Library or a protected folder",
        ConfigSchemaKind::StringList,
        &["disk", "folders", "sizes", "library", "protected", "walker"],
        true,
    ),
    manual(
        "claude_footer",
        "Claude Code footer",
        ConfigSchemaKind::Table,
        &[
            "claude", "footer", "usage", "tokens", "card", "harness", "acts", "agents",
        ],
        false,
    ),
    manual(
        "claude_footer.usage",
        "Show Claude's working time and tokens (Σ)",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["claude", "footer", "usage", "tokens", "runtime", "sigma"],
        true,
    ),
    manual(
        "claude_footer.card",
        "Decorate Claude Code's launch card",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "claude",
            "footer",
            "card",
            "harness",
            "autopilot",
            "rainbow",
            "banner",
        ],
        true,
    ),
    manual(
        "claude_footer.acts",
        "Show what the harness just did beside its Autopilot chip",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "claude",
            "footer",
            "acts",
            "ticker",
            "harness",
            "autopilot",
            "approved",
        ],
        true,
    ),
    manual(
        "claude_footer.agents",
        "Light Claude Code's agents panel and its dots",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "claude",
            "footer",
            "agents",
            "subagents",
            "panel",
            "rainbow",
            "stuck",
            "harness",
        ],
        true,
    ),
    manual(
        "reroute",
        "Toolchain reroute",
        ConfigSchemaKind::Table,
        &["cargo", "rustc", "trust", "toolchain"],
        false,
    ),
    manual(
        "reroute.strict",
        "Toolchain reroute: refuse instead of warn",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["reroute", "strict", "refuse", "z3", "toolchain", "trust"],
        true,
    ),
    manual(
        "harness",
        "aterm harness",
        ConfigSchemaKind::Table,
        &["harness", "wrapper", "claude", "agent"],
        false,
    ),
    manual(
        "harness.claude",
        "Claude Code supervision",
        ConfigSchemaKind::Table,
        &["harness", "claude", "claude code", "agent"],
        false,
    ),
    manual(
        "harness.codex",
        "codex-alab watch",
        ConfigSchemaKind::Table,
        &["harness", "codex", "agent"],
        false,
    ),
    manual(
        "harness.agy",
        "Antigravity CLI supervision",
        ConfigSchemaKind::Table,
        &["harness", "agy", "antigravity", "agent"],
        false,
    ),
    manual(
        "harness.agy.rotate",
        "agy: rotate a launch off a low or spent model pool",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["harness", "agy", "quota", "rotate", "model", "pool"],
        true,
    ),
    manual(
        "harness.agy.low",
        "agy: the quota fraction a pool counts as low at",
        ConfigSchemaKind::Scalar(EditKind::Float),
        &["harness", "agy", "quota", "low", "threshold"],
        true,
    ),
    manual(
        "harness.agy.order",
        "agy: the models rotation tries, in order",
        ConfigSchemaKind::StringList,
        &["harness", "agy", "quota", "rotate", "model", "order"],
        true,
    ),
    manual(
        "harness.muse",
        "Muse Code supervision",
        ConfigSchemaKind::Table,
        &["harness", "muse", "muse code", "agent"],
        false,
    ),
    manual(
        "harness.headless",
        "Supervise in a headless instance",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "headless", "test"],
        true,
    ),
    manual(
        "harness.trust_roots",
        "Folders trusted without asking",
        ConfigSchemaKind::StringList,
        &["supervisor", "trust", "folder", "worktree"],
        true,
    ),
    manual(
        "harness.rm_breaker_ask",
        "Hand an unproven rm to a person",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "rm", "breaker", "approve", "strict"],
        true,
    ),
    manual(
        "harness.answer_text",
        "What a question is answered with",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["supervisor", "question", "decision", "answer"],
        true,
    ),
    manual(
        "harness.dismiss_surveys",
        "Auto-dismiss Claude Code's rating surveys",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "survey",
            "surveys",
            "dismiss surveys",
            "feedback",
            "rating",
            "satisfaction",
            "auto dismiss",
            "auto-dismiss",
        ],
        true,
    ),
    manual(
        "harness.continue",
        "Continue a turn that ended early",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "continue", "keep going"],
        true,
    ),
    manual(
        "harness.continue_text",
        "Continuation text",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["supervisor", "continue", "keep going"],
        true,
    ),
    manual(
        "harness.continue_per_hour",
        "Continuations per session per hour (0: no cap)",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["supervisor", "continue", "budget"],
        true,
    ),
    manual(
        "harness.rules_file",
        "Standing rules appended to a continuation",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["supervisor", "continue", "rules"],
        true,
    ),
    manual(
        "harness.retry_api_errors",
        "Retry after an API error or an overload",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "529",
            "overloaded",
            "retry",
            "offline",
            "network",
        ],
        true,
    ),
    manual(
        "harness.probe_api",
        "Check the API's reach during a network error",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "offline", "network", "reachable", "probe"],
        true,
    ),
    manual(
        "harness.resume_limits",
        "Continue when a usage limit resets",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "limit", "usage", "reset"],
        true,
    ),
    manual(
        "harness.limit_wait",
        "Wait out a usage limit, then continue",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "usage limit",
            "rate limit",
            "continue",
            "wait",
        ],
        true,
    ),
    manual(
        "harness.accounts",
        "Rotate Claude Code accounts at a usage limit",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: &["early", "wall", "off"],
        }),
        &[
            "supervisor",
            "account",
            "accounts",
            "usage limit",
            "rotate",
            "login",
        ],
        true,
    ),
    manual(
        "harness.logins",
        "Follow a renewed Claude Code login",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: &["renew", "off"],
        }),
        &["supervisor", "account", "login", "renew", "expire"],
        true,
    ),
    manual(
        "harness.hosts",
        "Machines to renew logins on, over ssh",
        ConfigSchemaKind::StringList,
        &["supervisor", "account", "login", "renew", "ssh"],
        true,
    ),
    manual(
        "harness.limit_reset",
        "Take a limit reset the plan offers",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "usage limit", "limit reset", "reset"],
        true,
    ),
    manual(
        "harness.limit_reset_kinds",
        "Kinds of limit reset to take, first preferred",
        ConfigSchemaKind::StringList,
        &["supervisor", "usage limit", "limit reset", "reset", "kind"],
        true,
    ),
    manual(
        "harness.usage_tap",
        "Read Claude Code's usage after each reply",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "usage",
            "usage limit",
            "status line",
            "statusline",
            "tap",
        ],
        true,
    ),
    manual(
        "harness.usage_notes",
        "Note Claude Code usage credits",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "usage credits",
            "credits",
            "usage limit",
            "accounts",
        ],
        true,
    ),
    manual(
        "harness.usage_credits",
        "Continue on usage credits",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "usage credits",
            "extra usage",
            "overage",
            "subscription",
            "consent",
        ],
        true,
    ),
    manual(
        "harness.model_fallback",
        "Model to relaunch on at a model limit",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["supervisor", "model", "limit", "opus"],
        true,
    ),
    manual(
        "harness.compact_on_context_wall",
        "Compact when the context is full",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "context", "compact"],
        true,
    ),
    manual(
        "harness.autocompact_offer",
        "Accept Claude Code's autocompact offer at this size",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &[
            "supervisor",
            "autocompact",
            "compact",
            "context",
            "tokens",
            "usage",
        ],
        true,
    ),
    manual(
        "harness.relaunch",
        "Relaunch an agent that exited on its own",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["supervisor", "relaunch", "restart", "exit"],
        true,
    ),
    manual(
        "harness.stall_term_after_s",
        "Seconds a frozen agent stands before a warning",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["supervisor", "frozen", "stall", "relaunch"],
        true,
    ),
    manual(
        "harness.stall_term",
        "Restart a frozen agent instead of warning",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "frozen",
            "stall",
            "restart",
            "signal",
            "strict",
        ],
        true,
    ),
    manual(
        "harness.memory_ceiling",
        "Memory an agent may use before aterm warns",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &[
            "supervisor",
            "memory",
            "ram",
            "ceiling",
            "warning",
            "restart",
            "relaunch",
        ],
        true,
    ),
    manual(
        "harness.facts",
        "Tell the agent of its hung background calls",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "facts",
            "background",
            "stuck",
            "hung",
            "prompt",
        ],
        true,
    ),
    manual(
        "harness.stuck_break_note",
        "Type a hung call's fact at a stuck break",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "supervisor",
            "facts",
            "background",
            "stuck",
            "hung",
            "break",
        ],
        true,
    ),
    manual(
        "harness.upgrade",
        "Restart Claude Code sessions onto a newer build",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["upgrade", "update", "claude", "restart"],
        true,
    ),
    manual(
        "harness.upgrade_ask",
        "Ask the agent before an upgrade restart",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["upgrade", "update", "claude", "ready", "strict"],
        true,
    ),
    manual(
        "harness.human_grace_s",
        "Seconds aterm waits after you type in a session",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &[
            "supervisor",
            "human",
            "keyboard",
            "grace",
            "deny",
            "countdown",
        ],
        true,
    ),
    manual(
        "ai",
        "AI CLIs",
        ConfigSchemaKind::Table,
        &[
            "ai", "cli", "claude", "codex", "agy", "muse", "grok", "review", "login",
        ],
        false,
    ),
    manual(
        "ai.reviewers",
        "AI CLIs that review, in order (default: every one used here)",
        ConfigSchemaKind::StringList,
        &["ai", "review", "reviewers", "order"],
        true,
    ),
    manual(
        "ai.review_count",
        "Reviewers per AI review",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["ai", "review", "count"],
        true,
    ),
    manual(
        "ai.review_timeout_s",
        "Seconds each AI reviewer may take",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["ai", "review", "timeout"],
        true,
    ),
    manual(
        "ai.claude",
        "Claude Code on this machine",
        ConfigSchemaKind::Table,
        &["ai", "claude", "claude code"],
        false,
    ),
    manual(
        "ai.codex",
        "Codex CLI on this machine",
        ConfigSchemaKind::Table,
        &["ai", "codex", "openai"],
        false,
    ),
    manual(
        "ai.agy",
        "Antigravity CLI on this machine",
        ConfigSchemaKind::Table,
        &["ai", "agy", "antigravity", "google"],
        false,
    ),
    manual(
        "ai.muse",
        "Muse Code on this machine",
        ConfigSchemaKind::Table,
        &["ai", "muse", "meta"],
        false,
    ),
    manual(
        "ai.grok",
        "Grok Build on this machine",
        ConfigSchemaKind::Table,
        &["ai", "grok", "grok build", "xai"],
        false,
    ),
    manual(
        "disk",
        "Disk watch",
        ConfigSchemaKind::Table,
        &["disk", "free space", "target", "cleanup"],
        false,
    ),
    manual(
        "disk.apply",
        "Let disk cleanup remove what it finds",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["disk", "cleanup", "remove", "consent"],
        true,
    ),
    manual(
        "disk.warn_free_gib",
        "Free GiB below which the disk report warns",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["disk", "free space", "warn"],
        true,
    ),
    manual(
        "disk.target_stale_days",
        "Days unbuilt before a build cache is idle",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["disk", "target", "stale", "cargo"],
        true,
    ),
    manual(
        "disk.auto_free_gib",
        "Free GiB below which caches are cleared",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["disk", "free space", "target", "cleanup", "automatic"],
        true,
    ),
    manual(
        "disk.dead_build_dirs",
        "Remove build folders whose owner is gone",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "disk",
            "cleanup",
            "automatic",
            "claude",
            "session",
            "scratch",
        ],
        true,
    ),
    manual(
        "render",
        "Render repair",
        ConfigSchemaKind::Table,
        &[
            "render",
            "repaint",
            "nudge",
            "displaced",
            "resize",
            "claude",
        ],
        false,
    ),
    manual(
        "render.repaint_nudge",
        "Repair a Claude Code tab drawn a row off",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: REPAINT_NUDGE_MODES,
        }),
        &[
            "render",
            "repaint",
            "nudge",
            "displaced",
            "resize",
            "claude",
            "row",
        ],
        true,
    ),
    manual(
        "packages",
        "Toolchain packages",
        ConfigSchemaKind::Table,
        &["atpkg", "tools", "toolchain"],
        false,
    ),
    manual(
        "packages.enabled",
        "Keep ALab tools up to date",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["atpkg", "tools", "master"],
        true,
    ),
    manual(
        "packages.account",
        "Package index account (development)",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["atpkg", "owner", "github", "development"],
        true,
    ),
    manual(
        "packages.prefix",
        "Package store prefix (development)",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["atpkg", "store", "path", "development"],
        true,
    ),
    manual(
        "packages.exclude",
        "Excluded packages",
        ConfigSchemaKind::StringList,
        &["atpkg", "filter", "deny"],
        true,
    ),
    manual(
        "packages.links",
        "Local package links",
        ConfigSchemaKind::DynamicStringMap,
        &["atpkg", "checkout", "repository", "development"],
        true,
    ),
    manual(
        "machine",
        "Machine settings",
        ConfigSchemaKind::Table,
        &["atpkg", "host", "maintenance", "universal control", "mac"],
        false,
    ),
    manual(
        "machine.universal_control",
        "Universal Control policy",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: UNIVERSAL_CONTROL_POLICIES,
        }),
        &["atpkg", "mouse", "keyboard", "sharing"],
        true,
    ),
    manual(
        "machine.rosetta",
        "Install Rosetta 2 on Apple silicon",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["atpkg", "rosetta", "intel", "x86_64", "translation", "mac"],
        true,
    ),
    manual(
        "privacy",
        "macOS consent posture",
        ConfigSchemaKind::Table,
        &["tcc", "consent", "full disk access", "macos", "security"],
        false,
    ),
    manual(
        "privacy.enabled",
        "Report macOS consent state",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["tcc", "consent", "master"],
        true,
    ),
    manual(
        "privacy.check",
        "Probe macOS access",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[
            "tcc",
            "full disk access",
            "debugger",
            "developer mode",
            "probe",
        ],
        true,
    ),
    manual(
        "privacy.notice",
        "Suggest Full Disk Access",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["tcc", "full disk access", "message", "notice"],
        true,
    ),
    manual(
        "privacy.report_attribution",
        "Attribute consent reports to sessions",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["tcc", "responsible", "session"],
        true,
    ),
    manual(
        "privacy.warmup",
        "Ask for folder access (at launch, or on request)",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: PRIVACY_WARMUPS,
        }),
        &["tcc", "folders", "prompt", "warm-up"],
        true,
    ),
    manual(
        "privacy.warmup_folders",
        "Folders the warm-up asks for",
        ConfigSchemaKind::StringList,
        &["tcc", "documents", "desktop", "downloads"],
        true,
    ),
    manual(
        "privacy.warmup_hold_ms",
        "Update hold during a warm-up",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["tcc", "warm-up", "update", "hold"],
        true,
    ),
    manual(
        "privacy.probe_interval_ms",
        "Consent re-probe interval",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["tcc", "probe", "interval"],
        true,
    ),
    manual(
        "privacy.observer",
        "Observe tccd's consent log",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["tcc", "tccd", "log", "observer"],
        true,
    ),
    manual(
        "privacy.protected_roots",
        "Protected roots",
        ConfigSchemaKind::StringList,
        &["tcc", "containment", "private", "paths"],
        true,
    ),
    manual(
        "privacy.auto_accept",
        "Answer consent dialogs (reserved; refused)",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["tcc", "consent", "reserved"],
        true,
    ),
    manual(
        "matrix_rain",
        "Matrix rain",
        ConfigSchemaKind::Table,
        &["rain", "effect", "phosphor"],
        false,
    ),
    manual(
        "output_streak",
        "Output streak",
        ConfigSchemaKind::Table,
        &["streak", "effect", "output", "prism", "rainbow"],
        false,
    ),
    manual(
        "output_streak.intensity",
        "Output streak intensity",
        ConfigSchemaKind::Scalar(EditKind::Float),
        &["streak", "prism", "amplitude"],
        true,
    ),
    manual(
        "output_streak.tail",
        "Output streak tail (cells)",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["streak", "prism", "comet", "length"],
        true,
    ),
    manual(
        "output_streak.max_streaks",
        "Output streaks at once",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["streak", "prism", "comet", "count"],
        true,
    ),
    manual(
        "output_streak.idle_secs",
        "Output streak idle drain",
        ConfigSchemaKind::Scalar(EditKind::Float),
        &["streak", "prism", "idle", "drain"],
        true,
    ),
    manual(
        "presence",
        "Presence surfaces",
        ConfigSchemaKind::Table,
        &["presence", "band", "rim", "driving", "hold", "fabric"],
        false,
    ),
    manual(
        "sparkle_words",
        "Keyword toys",
        ConfigSchemaKind::Table,
        &["sparkle", "kitty", "words", "effects"],
        false,
    ),
    manual(
        "sparkle_words.languages",
        "Sparkle word languages",
        ConfigSchemaKind::StringList,
        &["locale", "lexicon"],
        true,
    ),
    manual(
        "sparkle_words.toy_packs",
        "Keyword toy packs",
        ConfigSchemaKind::StringList,
        &["manifest", "effects", "path"],
        true,
    ),
    manual(
        "sparkle_words.deny",
        "Never-decorate words",
        ConfigSchemaKind::StringList,
        &["block", "ignore", "exclude"],
        true,
    ),
    manual(
        "sparkle_words.profanity",
        "Sparkle words",
        ConfigSchemaKind::Table,
        &["sparkle", "words", "effects"],
        false,
    ),
    manual(
        "sparkle_words.profanity.palette",
        "Sparkle palette",
        ConfigSchemaKind::StringList,
        &["color", "colour"],
        true,
    ),
    manual(
        "sparkle_words.profanity.extra_words",
        "Additional sparkle words",
        ConfigSchemaKind::StringList,
        &["lexicon", "include"],
        true,
    ),
    manual(
        "sparkle_words.profanity.ignore_words",
        "Ignored sparkle words",
        ConfigSchemaKind::StringList,
        &["lexicon", "exclude"],
        true,
    ),
    manual(
        "sparkle_words.feline",
        "Keyword kitties",
        ConfigSchemaKind::Table,
        &["cat", "kitty", "words"],
        false,
    ),
    manual(
        "sparkle_words.feline.extra_words",
        "Additional kitty words",
        ConfigSchemaKind::StringList,
        &["cat", "lexicon", "include"],
        true,
    ),
    manual(
        "sparkle_words.feline.ignore_words",
        "Ignored kitty words",
        ConfigSchemaKind::StringList,
        &["cat", "lexicon", "exclude"],
        true,
    ),
    manual(
        "sparkle_words.canine",
        "Keyword dogs",
        ConfigSchemaKind::Table,
        &["dog", "puppy", "words"],
        false,
    ),
    manual(
        "sparkle_words.canine.enabled",
        "Summon dogs for typed dog words",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["dog", "puppy", "cameo"],
        true,
    ),
    manual(
        "sparkle_words.canine.extra_words",
        "Additional dog words",
        ConfigSchemaKind::StringList,
        &["dog", "lexicon", "include"],
        true,
    ),
    manual(
        "sparkle_words.canine.ignore_words",
        "Ignored dog words",
        ConfigSchemaKind::StringList,
        &["dog", "lexicon", "exclude"],
        true,
    ),
    manual(
        "sparkle_words.tricks",
        "Kitty commands",
        ConfigSchemaKind::Table,
        &["cat", "kitty", "tricks", "commands"],
        false,
    ),
    manual(
        "sparkle_words.tricks.enabled",
        "Kitty commands (sit, jump, good kitty)",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["cat", "kitty", "tricks", "sit"],
        true,
    ),
    manual(
        "sparkle_words.tricks.ignore_words",
        "Ignored kitty commands",
        ConfigSchemaKind::StringList,
        &["cat", "tricks", "lexicon", "exclude"],
        true,
    ),
    manual(
        "sparkle_words.ink",
        "Keyword ink",
        ConfigSchemaKind::Table,
        &["shimmer", "color", "colour"],
        false,
    ),
    manual(
        "sparkle_words.emphasis",
        "Emphasis words",
        ConfigSchemaKind::Table,
        &["hype", "ink", "words"],
        false,
    ),
    manual(
        "sparkle_words.emphasis.extra_words",
        "Additional emphasis words",
        ConfigSchemaKind::StringList,
        &["hype", "lexicon", "include"],
        true,
    ),
    manual(
        "sparkle_words.emphasis.ignore_words",
        "Ignored emphasis words",
        ConfigSchemaKind::StringList,
        &["hype", "lexicon", "exclude"],
        true,
    ),
    manual(
        "sparkle_words.custom",
        "Custom keyword effects",
        ConfigSchemaKind::StructuredList,
        &["recipe", "toy", "word", "effect"],
        true,
    ),
    manual(
        "sparkle_words.custom.words",
        "Custom effect words",
        ConfigSchemaKind::StringList,
        &["recipe", "toy", "lexicon"],
        false,
    ),
    manual(
        "sparkle_words.custom.ink",
        "Custom ink effect",
        ConfigSchemaKind::Flexible,
        &["colorway", "rainbow", "twotone"],
        false,
    ),
    manual(
        "sparkle_words.custom.ink.colorway",
        "Custom ink colorway",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: CUSTOM_INK_COLORWAYS,
        }),
        &["rainbow", "twotone", "color"],
        false,
    ),
    manual(
        "sparkle_words.custom.ink.sweep_once",
        "Custom ink single sweep",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["animation", "loop"],
        false,
    ),
    manual(
        "sparkle_words.custom.burst",
        "Custom burst effect",
        ConfigSchemaKind::Table,
        &["light", "sparkle", "nova"],
        false,
    ),
    manual(
        "sparkle_words.custom.burst.kind",
        "Custom burst kind",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: CUSTOM_BURST_KINDS,
        }),
        &["sparkle", "nova", "starburst", "glow"],
        false,
    ),
    manual(
        "sparkle_words.custom.burst.chance",
        "Custom burst chance",
        ConfigSchemaKind::Scalar(EditKind::Integer),
        &["percent", "probability"],
        false,
    ),
    manual(
        "sparkle_words.custom.graphic",
        "Custom graphic effect",
        ConfigSchemaKind::Table,
        &["sprite", "image", "cat"],
        false,
    ),
    manual(
        "sparkle_words.custom.graphic.collection",
        "Custom graphic collection",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: CUSTOM_GRAPHIC_COLLECTIONS,
        }),
        &["sprite", "cats"],
        false,
    ),
    manual(
        "sparkle_words.feline.idle",
        "Keyword Kitty idle animation",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[],
        true,
    ),
    manual(
        "sparkle_words.feline.gaze",
        "Keyword Kitty gaze tracking",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[],
        true,
    ),
    manual(
        "sparkle_words.feline.color",
        "Keyword Kitty tint",
        ConfigSchemaKind::Scalar(EditKind::Color),
        &[],
        true,
    ),
    manual(
        "sparkle_words.feline.intensity",
        "Keyword Kitty opacity",
        ConfigSchemaKind::Scalar(EditKind::Float),
        &[],
        true,
    ),
    manual(
        "matrix_rain.materialize",
        "Matrix rain materialize",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[],
        true,
    ),
    manual(
        "matrix_rain.ink_text",
        "Matrix rain ink text",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[],
        true,
    ),
    manual(
        "matrix_rain.phosphor",
        "Matrix rain phosphor layer",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &[],
        true,
    ),
];

pub(crate) fn config_schema() -> &'static [ConfigSchemaEntry] {
    static SCHEMA: OnceLock<Vec<ConfigSchemaEntry>> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema = crate::prefs::editable_fields(&crate::app_config::Config::default())
            .into_iter()
            .map(
                |EditField {
                     key,
                     label,
                     kind,
                     seed,
                     placeholder,
                 }| ConfigSchemaEntry {
                    key,
                    label,
                    kind: if crate::prefs::LIST_KEYS.contains(&key) {
                        ConfigSchemaKind::StringList
                    } else if crate::prefs::manual_collection_key(key) {
                        ConfigSchemaKind::TextOrStringList
                    } else {
                        ConfigSchemaKind::Scalar(kind)
                    },
                    placeholder: if placeholder.is_empty() && matches!(kind, EditKind::Bool) {
                        seed.unwrap_or_default()
                    } else {
                        placeholder
                    },
                    keywords: crate::prefs::keywords_of(key),
                    native_scalar: !crate::prefs::manual_only_key(key),
                    manual_reset_safe: true,
                },
            )
            .collect::<Vec<_>>();
        for entry in MANUAL_SCHEMA {
            if schema.iter().any(|known| known.key == entry.key) {
                continue;
            }
            let placeholder = entry
                .key
                .strip_prefix("harness.")
                .and_then(aterm_agent::supervise::SupervisorConfig::default_shown)
                .or_else(|| {
                    entry
                        .key
                        .strip_prefix("disk.")
                        .and_then(aterm_agent::harness::disk::Config::default_shown)
                })
                .or_else(|| manual_default_shown(entry.key))
                .unwrap_or_default();
            schema.push(ConfigSchemaEntry {
                key: entry.key,
                label: entry.label,
                kind: entry.kind,
                placeholder,
                keywords: entry.keywords,
                native_scalar: false,
                manual_reset_safe: entry.manual_reset_safe,
            });
        }
        schema
    })
}

fn manual_default_shown(key: &str) -> Option<String> {
    fn word<T: PartialEq + Copy>(
        key: &str,
        default: T,
        parse: fn(&str) -> Option<T>,
    ) -> Option<String> {
        let ConfigSchemaKind::Scalar(EditKind::Enum { options }) =
            MANUAL_SCHEMA.iter().find(|entry| entry.key == key)?.kind
        else {
            return None;
        };
        options
            .iter()
            .find(|option| parse(option) == Some(default))
            .map(|option| (*option).to_string())
    }
    let config = crate::app_config::Config::default();
    Some(match key {
        "keeper.enabled" => config.keeper_enabled().to_string(),
        "session_log.journal" => config.session_log_journal().to_string(),
        "session_log.clause" => config.session_log_clause().to_string(),
        "sys.stats_bars" => word(
            key,
            config.stats_bars_or_default(),
            crate::stats_deck::StatsBars::from_word,
        )?,
        "sys.stats_deck_size" => word(
            key,
            config.stats_deck_size_or_default(),
            crate::stats_plan::DeckSizePref::from_word,
        )?,
        "agents_auto_prime" => config.agents_auto_prime_or_default().to_string(),
        "claude_footer.usage" => config.claude_footer_usage().to_string(),
        "claude_footer.card" => config.claude_footer_card().to_string(),
        "claude_footer.acts" => config.claude_footer_acts().to_string(),
        "claude_footer.agents" => config.claude_footer_agents().to_string(),
        "reroute.strict" => atpkg::config::parse_reroute("").strict().to_string(),
        #[cfg(unix)]
        "fabric.hold_on_bridge_lost" => {
            crate::fabric_launch::hold_on_bridge_lost(&config).to_string()
        }
        "render.repaint_nudge" => config.repaint_nudge_mode().word().to_string(),
        "font_hinting" => config.font_hinting_or_default(),
        "font_subpixel" => config.font_subpixel_or_default(),
        "right_click" => word(
            key,
            config.right_click_or_default(),
            crate::app_config::RightClickGesture::parse,
        )?,
        #[cfg(any(not(target_os = "macos"), test))]
        "tab_menu_chord" => word(
            key,
            config.tab_menu_chord_or_default(),
            crate::app_config::TabMenuChord::parse,
        )?,
        "tab_band_height" => word(
            key,
            config.tab_band_height_or_default(),
            crate::app_config::TabBandHeight::parse,
        )?,
        "privacy.enabled" => config.privacy_enabled().to_string(),
        "privacy.check" => config.privacy_check().to_string(),
        "privacy.notice" => config.privacy_notice().to_string(),
        "privacy.report_attribution" => config.privacy_report_attribution().to_string(),
        "privacy.warmup" => config.privacy_warmup().as_str().to_string(),
        "privacy.warmup_hold_ms" => config.privacy_warmup_hold_ms().to_string(),
        "privacy.probe_interval_ms" => config.privacy_probe_interval_ms().to_string(),
        "output_streak.enabled" => config.output_streak_enabled_or_default().to_string(),
        "output_streak.sound" => config.output_streak_sound_or_default().to_string(),
        "output_streak.intensity" => config.output_streak_intensity_or_default().to_string(),
        "output_streak.tail" => config.output_streak_tail_or_default().to_string(),
        "output_streak.max_streaks" => config.output_streak_max_streaks_or_default().to_string(),
        "output_streak.idle_secs" => config.output_streak_idle_secs_or_default().to_string(),
        "ai.review_count" => aterm_agent::ai::config::REVIEW_COUNT_DEFAULT.to_string(),
        "ai.review_timeout_s" => aterm_agent::ai::config::TIMEOUT_DEFAULT_S.to_string(),
        _ => return None,
    })
}

/// Resolve one schema key.
pub(crate) fn config_schema_entry(key: &str) -> Option<&'static ConfigSchemaEntry> {
    static INDEX: OnceLock<Vec<&'static ConfigSchemaEntry>> = OnceLock::new();
    let index = INDEX.get_or_init(|| {
        let mut entries = config_schema().iter().collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| entry.key);
        entries
    });
    index
        .binary_search_by(|entry| entry.key.cmp(key))
        .ok()
        .map(|position| index[position])
}

/// The same rank used by Settings global search, exported from the schema authority so Manual labels/keywords cannot silently diverge from completion.
pub(crate) fn config_schema_match_score(
    entry: &ConfigSchemaEntry,
    query: &str,
    reach: MatchReach,
    scratch: &mut String,
) -> Option<SearchRank> {
    if query.is_empty() {
        return Some(SearchRank::EVERY);
    }
    scratch.clear();
    scratch.extend(
        entry
            .label
            .chars()
            .map(|character| character.to_ascii_lowercase()),
    );
    query_match_rank(
        [scratch.as_str(), entry.key]
            .into_iter()
            .chain(crate::prefs::own_names_of(entry.key).into_iter().flatten()),
        entry.keywords.iter().copied(),
        query,
        reach,
    )
}

/// WHERE A SEARCH HIT LANDS, as both Settings search and Manual completion order it: the ladder's rung first ([`candidate_match_score`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SearchRank {
    /// The ladder's rung: [`WHOLE_AT_START`] … [`INSIDE_WORD_RUNG`].
    pub(crate) rung: u8,
    /// Only a synonym (a listed keyword, a value) reached the rung.
    pub(crate) synonym: bool,
}

impl SearchRank {
    /// An empty query: everything at once, in its own order.
    pub(crate) const EVERY: Self = Self::own(WHOLE_AT_START);

    /// A hit on a name of the row's own, at `rung`.
    pub(crate) const fn own(rung: u8) -> Self {
        Self {
            rung,
            synonym: false,
        }
    }

    /// A hit only a synonym reached, at `rung`.
    pub(crate) const fn synonym(rung: u8) -> Self {
        Self {
            rung,
            synonym: true,
        }
    }
}

/// Rank a row against `query` ([`SearchRank`]): its `own` names (label, key, own name) and its `synonyms` together, by [`query_match_score`].
pub(crate) fn query_match_rank<'a>(
    own: impl Iterator<Item = &'a str> + Clone,
    synonyms: impl Iterator<Item = &'a str> + Clone,
    query: &str,
    reach: MatchReach,
) -> Option<SearchRank> {
    let rung = query_match_score(own.clone().chain(synonyms), query, reach)?;
    Some(SearchRank {
        rung,
        synonym: query_match_score(own, query, reach) != Some(rung),
    })
}

const QUERY_FILLER_WORDS: [&str; 12] = [
    "a", "an", "and", "for", "in", "my", "of", "on", "or", "the", "to", "with",
];

/// Rank a candidate set (a setting's label, key and keywords) against a query of one or more words.
pub(crate) fn query_match_score<'a>(
    candidates: impl Iterator<Item = &'a str> + Clone,
    query: &str,
    reach: MatchReach,
) -> Option<u8> {
    let best = |needle: &str| {
        candidates
            .clone()
            .filter_map(|candidate| candidate_match_score(candidate, needle, reach))
            .min()
    };
    let word = |word: &str| {
        let other = word_stem(word).and_then(|stem| {
            candidates
                .clone()
                .filter_map(|candidate| word_start_rung(candidate, stem, &STEM_TAILS))
                .filter(|rung| *rung <= WHOLE_AT_WORD)
                .min()
        });
        best(word).into_iter().chain(other).min()
    };
    let mut words = query.split_whitespace();
    let first = words.next()?;
    if words.next().is_none() {
        return word(first);
    }
    let phrase = best(query.trim());
    let mut every = None;
    for part in query
        .split_whitespace()
        .filter(|part| !QUERY_FILLER_WORDS.contains(part))
    {
        match word(part) {
            Some(score) => every = Some(every.map_or(score, |worst: u8| worst.max(score))),
            None => {
                every = None;
                break;
            }
        }
    }
    match (phrase, every) {
        (Some(phrase), Some(every)) => Some(phrase.min(every)),
        (phrase, every) => phrase.or(every),
    }
}

const WORD_PIECE_FLOOR: usize = 3;

const PLURAL_TAILS: [&str; 2] = ["s", "es"];

const STEM_TAILS: [&str; 4] = ["s", "es", "ies", "y"];

fn word_stem(word: &str) -> Option<&str> {
    let stem = word
        .strip_suffix("ies")
        .or_else(|| word.strip_suffix('s').filter(|stem| !stem.ends_with('s')))
        .or_else(|| word.strip_suffix('y'))?;
    (stem.len() >= WORD_PIECE_FLOOR).then_some(stem)
}

/// The ladder's rungs ([`candidate_match_score`]), best first.
pub(crate) const WHOLE_AT_START: u8 = 0;
/// … a whole word later in it ("code" in "supervise claude code sessions") …
pub(crate) const WHOLE_AT_WORD: u8 = 1;
/// … the start of the candidate's first word ("code" in "codex") …
pub(crate) const PREFIX_AT_START: u8 = 2;
/// … the start of a later word ("cur" in "enable_cursor_trail").
pub(crate) const PREFIX_AT_WORD: u8 = 3;
/// A loose hit ([`MatchReach::Loose`]): initials only …
pub(crate) const ACRONYM_RUNG: u8 = 4;
/// … or the inside of a word ("board" in "keyboard").
pub(crate) const INSIDE_WORD_RUNG: u8 = 5;

/// `rung` as a hit inside a row's name rather than at its start: a value the row holds (a theme's name on the Theme row) is never what the row is called first, so its start ranks as a later word's.
pub(crate) const fn past_the_start(rung: u8) -> u8 {
    match rung {
        WHOLE_AT_START => WHOLE_AT_WORD,
        PREFIX_AT_START => PREFIX_AT_WORD,
        rung => rung,
    }
}

/// HOW FAR A SEARCH REACHES — one answer for a whole search, never per row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MatchReach {
    Words,
    Loose,
}

impl MatchReach {
    /// The reach of a search whose result set under [`Self::Words`] is `any_word_hit` or not.
    pub(crate) const fn of(any_word_hit: bool) -> Self {
        if any_word_hit {
            Self::Words
        } else {
            Self::Loose
        }
    }
}

/// THE search-rank ladder, shared by the config-schema authority above and by every Settings surface that filters by text: global search (`native_settings::field_match_score_in`, its page and Manual results), the theme picker's filter and the theme gallery's cards.
pub(crate) fn candidate_match_score(candidate: &str, query: &str, reach: MatchReach) -> Option<u8> {
    let tails: &[&str] = if query.len() >= WORD_PIECE_FLOOR {
        &PLURAL_TAILS
    } else {
        &[]
    };
    word_start_rung(candidate, query, tails).or_else(|| match reach {
        MatchReach::Words => None,
        MatchReach::Loose if is_acronym(query, candidate) => Some(ACRONYM_RUNG),
        MatchReach::Loose => inside_a_word(candidate, query).then_some(INSIDE_WORD_RUNG),
    })
}

fn is_acronym(query: &str, candidate: &str) -> bool {
    let wanted = query.as_bytes();
    if !(2..=3).contains(&wanted.len()) || !wanted.iter().all(u8::is_ascii_alphanumeric) {
        return false;
    }
    let mut initials = [0u8; 3];
    let mut seen = 0;
    let mut word_start = true;
    for &byte in candidate.as_bytes() {
        if byte.is_ascii_alphanumeric() {
            if word_start {
                initials.rotate_left(1);
                initials[2] = byte;
                seen += 1;
                if seen >= wanted.len() && initials[3 - wanted.len()..] == *wanted {
                    return true;
                }
            }
            word_start = false;
        } else {
            word_start = true;
        }
    }
    false
}

fn inside_a_word(candidate: &str, query: &str) -> bool {
    query.len() >= WORD_PIECE_FLOOR
        && query
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && candidate.contains(query)
}

fn word_start_rung(candidate: &str, query: &str, tails: &[&str]) -> Option<u8> {
    let (bytes, wanted) = (candidate.as_bytes(), query.as_bytes());
    let (Some(first), Some(last)) = (wanted.first(), wanted.last()) else {
        return Some(WHOLE_AT_START);
    };
    let own_boundary = !first.is_ascii_alphanumeric();
    let ends_a_word = !last.is_ascii_alphanumeric();
    let mut best: Option<u8> = None;
    let mut word_start = true;
    for (i, byte) in bytes.iter().enumerate() {
        if (word_start || own_boundary) && bytes[i..].starts_with(wanted) {
            let end = i + wanted.len();
            let left = bytes[end..]
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric())
                .count();
            let whole = ends_a_word
                || left == 0
                || tails
                    .iter()
                    .any(|tail| tail.as_bytes() == &bytes[end..end + left]);
            let rung = match (whole, i == 0) {
                (true, true) => WHOLE_AT_START,
                (true, false) => WHOLE_AT_WORD,
                (false, true) => PREFIX_AT_START,
                (false, false) => PREFIX_AT_WORD,
            };
            best = Some(best.map_or(rung, |best| best.min(rung)));
            if rung <= WHOLE_AT_WORD {
                break;
            }
        }
        word_start = !byte.is_ascii_alphanumeric();
    }
    best
}

fn contains_ascii_folded(haystack: &str, needle: &str, scratch: &mut String) -> bool {
    scratch.clear();
    scratch.extend(
        haystack
            .chars()
            .map(|character| character.to_ascii_lowercase()),
    );
    scratch.contains(needle)
}

const fn near_miss_budget(length: usize) -> usize {
    if length <= 8 { 1 } else { 2 }
}

fn osa_distance_within(
    a: &[char],
    b: &[char],
    budget: usize,
    rows: &mut [Vec<usize>; 3],
) -> Option<usize> {
    if a.is_empty() {
        return (b.len() <= budget).then_some(b.len());
    }
    let width = b.len() + 1;
    for row in rows.iter_mut() {
        row.clear();
        row.resize(width, 0);
    }
    let (mut two_back, mut one_back, mut current) = (0usize, 1usize, 2usize);
    for (column, cell) in rows[one_back].iter_mut().enumerate() {
        *cell = column;
    }
    for i in 1..=a.len() {
        rows[current][0] = i;
        let mut row_best = i;
        for j in 1..width {
            let substitution = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[one_back][j] + 1)
                .min(rows[current][j - 1] + 1)
                .min(rows[one_back][j - 1] + substitution);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[two_back][j - 2] + 1);
            }
            rows[current][j] = best;
            row_best = row_best.min(best);
        }
        if row_best > budget {
            return None;
        }
        let spent = two_back;
        two_back = one_back;
        one_back = current;
        current = spent;
    }
    let distance = rows[one_back][b.len()];
    (distance <= budget).then_some(distance)
}

fn nearest_candidate<'a>(
    query: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let folded = query
        .chars()
        .map(|character| character.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let budget = near_miss_budget(folded.len());
    let mut rows = [Vec::new(), Vec::new(), Vec::new()];
    let mut scratch = Vec::new();
    let mut best: Option<(usize, &'a str)> = None;
    for candidate in candidates {
        if candidate == query {
            continue;
        }
        scratch.clear();
        scratch.extend(
            candidate
                .chars()
                .map(|character| character.to_ascii_lowercase()),
        );
        if folded.len().abs_diff(scratch.len()) > budget {
            continue;
        }
        let Some(distance) = osa_distance_within(&folded, &scratch, budget, &mut rows) else {
            continue;
        };
        let better = best.is_none_or(|(best_distance, best_candidate)| {
            (distance, candidate.len(), candidate)
                < (best_distance, best_candidate.len(), best_candidate)
        });
        if better {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

fn suggestable_config_paths() -> &'static [&'static str] {
    static PATHS: OnceLock<Vec<&'static str>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let mut paths = Vec::new();
        for entry in config_schema() {
            if is_compatibility_only_key(entry.key) {
                continue;
            }
            paths.push(entry.key);
            let mut ancestor = entry.key;
            while let Some(cut) = ancestor.rfind('.') {
                ancestor = &ancestor[..cut];
                if !is_compatibility_only_key(ancestor) {
                    paths.push(ancestor);
                }
            }
        }
        paths.sort_unstable();
        paths.dedup();
        paths
    })
}

fn config_path_is_known(path: &str) -> bool {
    suggestable_config_paths().binary_search(&path).is_ok()
}

/// The registered key a misspelling most likely meant, or `None` when nothing is close enough.
pub(crate) fn nearest_config_key(path: &str) -> Option<&'static str> {
    let mut candidate = path;
    loop {
        if config_path_is_known(candidate) {
            return None;
        }
        if !candidate.contains('"') {
            let depth = candidate.matches('.').count();
            let nearest = nearest_candidate(
                candidate,
                suggestable_config_paths()
                    .iter()
                    .copied()
                    .filter(|known| known.matches('.').count() == depth),
            );
            if nearest.is_some() {
                return nearest;
            }
        }
        candidate = &candidate[..candidate.rfind('.')?];
    }
}

/// The accepted value a misspelling most likely meant, out of `options` (the live domain the caller prints as "must be one of").
pub(crate) fn nearest_enum_value<'a>(value: &str, options: &[&'a str]) -> Option<&'a str> {
    nearest_candidate(value.trim(), options.iter().copied())
}

fn did_you_mean(suggestion: Option<&str>) -> String {
    suggestion.map_or_else(String::new, |suggestion| {
        format!(" — did you mean {suggestion:?}?")
    })
}

/// Every key in `source` this build ignores (a misspelling, a key from a newer aterm, a retired spelling that does nothing), plus the still-read retired and deprecated spellings in their own words and the known keys whose value their reader does not use as written: every line `--validate-config` lists.
pub(crate) fn ignored_key_warnings(source: &str) -> Vec<String> {
    let Some(analysis) = key_analysis(source) else {
        return Vec::new();
    };
    let mut lines = analysis
        .diagnostics
        .iter()
        .map(key_warning_line)
        .collect::<Vec<_>>();
    lines.extend(omitted_key_line(&analysis));
    lines
}

/// [`ignored_key_warnings`] in two halves, from one walk: the band's view (`app_config::collect_key_notices`).
pub(crate) fn key_warnings(source: &str) -> KeyWarnings {
    let Some(analysis) = key_analysis(source) else {
        return KeyWarnings::default();
    };
    let mut warnings = KeyWarnings::default();
    for diagnostic in &analysis.diagnostics {
        let half = if analysis.retired_spellings.contains(diagnostic) {
            &mut warnings.retired
        } else if analysis.unaccepted_values.contains(diagnostic) {
            continue;
        } else {
            &mut warnings.ignored
        };
        half.push(key_warning_line(diagnostic));
    }
    warnings.ignored.extend(omitted_key_line(&analysis));
    warnings
}

/// The two halves [`key_warnings`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyWarnings {
    /// Keys this build does nothing with: a misspelling, a key from a newer aterm, a key below a table that does not read it (a structured list's record included).
    pub(crate) ignored: Vec<String>,
    /// Retired and deprecated spellings, and keys a removed feature left behind, each sentence saying whether the key is still read and what to rename or remove.
    pub(crate) retired: Vec<String>,
}

fn key_analysis(source: &str) -> Option<ConfigAnalysis> {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return None;
    }
    let document = source.parse::<aterm_toml::edit::DocumentMut>().ok()?;
    let mut analysis = ConfigAnalysis::default();
    let disk = aterm_agent::harness::cli::disk_faults(source);
    let walk = KeyWalk {
        source,
        document: &document,
        disk: &disk,
        readers: reader_accepted_paths(),
    };
    validate_machine_types(source, &document, &mut analysis);
    validate_disk_values(walk, &mut analysis);
    warn_compatibility_only_values(walk, &mut analysis);
    warn_unknown_values(walk, &mut analysis);
    Some(analysis)
}

fn key_warning_line(diagnostic: &ConfigDiagnostic) -> String {
    format!("line {}: {}", diagnostic.line, diagnostic.message)
}

fn omitted_key_line(analysis: &ConfigAnalysis) -> Option<String> {
    (analysis.omitted_diagnostics > 0)
        .then(|| format!("… and {} more not shown", analysis.omitted_diagnostics))
}

fn retired_but_resolving(key: &str, value: &str) -> Option<(&'static str, &'static str)> {
    let value = value.trim();
    match key {
        "font_hinting" if value == "native" => Some(("native", "light")),
        _ => None,
    }
}

/// The warning for a [`retired_but_resolving`] value of `key`, if it is one.
pub(crate) fn retired_but_resolving_note(key: &str, value: &str) -> Option<String> {
    let (retired, renders) = retired_but_resolving(key, value)?;
    Some(format!(
        "{key} = \"{retired}\" is retired and renders as \"{renders}\"; use \"{renders}\" explicitly"
    ))
}
fn unaccepted_enum_values(source: &str) -> Vec<(&'static str, String, String)> {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return Vec::new();
    }
    let Ok(document) = source.parse::<aterm_toml::edit::DocumentMut>() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for setting in config_schema() {
        let ConfigSchemaKind::Scalar(EditKind::Enum { options }) = setting.kind else {
            continue;
        };
        if is_compatibility_only_key(setting.key) {
            continue;
        }
        let Some(value) =
            dotted_item(&document, setting.key).and_then(aterm_toml::edit::Item::as_str)
        else {
            continue;
        };
        if registered_enum_value_is_valid(setting.key, value, options)
            || retired_but_resolving(setting.key, value).is_some()
        {
            continue;
        }
        found.push((
            setting.key,
            value.to_string(),
            format!(
                "{}: {value:?} is not accepted{} (expected one of: {}; {})",
                setting.key,
                did_you_mean(nearest_enum_value(value, options)),
                options.join(", "),
                match setting.key {
                    "machine.universal_control" => "Universal Control is left unchanged",
                    "render.repaint_nudge" => "the repaint nudge only logs",
                    "fleet.allow" => "the fleet listener stays off until it is one of these",
                    _ => "the default is used at load",
                },
            ),
        ));
    }
    found
}

/// The known keys whose value their reader does not use as written, minus what a line in `already_told` has explained: the key walk's ([`ConfigAnalysis::unaccepted_values`]) as `line N: …`, dropped where a told line holds it whole, then [`unaccepted_enum_values`].
pub(crate) fn unaccepted_value_warnings(source: &str, already_told: &[String]) -> Vec<String> {
    let mut lines = key_analysis(source)
        .map(|analysis| {
            analysis
                .unaccepted_values
                .iter()
                .map(key_warning_line)
                .filter(|line| !already_told.iter().any(|told| told.contains(line)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    lines.extend(
        unaccepted_enum_values(source)
            .into_iter()
            .filter(|(key, value, _)| {
                let quoted = format!("{value:?}");
                !already_told
                    .iter()
                    .any(|told| told.contains(key) && told.contains(&quoted))
            })
            .map(|(_, _, message)| message),
    );
    lines
}

pub(crate) fn analyze(source: &str) -> ConfigAnalysis {
    let mut analysis = ConfigAnalysis {
        syntax: lex_toml(source),
        diagnostics: Vec::new(),
        omitted_diagnostics: 0,
        retired_spellings: Vec::new(),
        unaccepted_values: Vec::new(),
        assist_index: build_assist_index(source),
    };
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        let mut too_large = ConfigAnalysis::too_large();
        too_large.syntax = analysis.syntax;
        return too_large;
    }

    let document = match source.parse::<aterm_toml::edit::DocumentMut>() {
        Ok(document) => document,
        Err(error) => {
            let range = parser_diagnostic_range(source, error.span());
            push_diagnostic(
                &mut analysis,
                source,
                range,
                ConfigDiagnosticSeverity::Error,
                format!("TOML syntax: {}", one_line(&error.to_string())),
            );
            return analysis;
        }
    };
    let disk = aterm_agent::harness::cli::disk_faults(source);
    let walk = KeyWalk {
        source,
        document: &document,
        disk: &disk,
        readers: reader_accepted_paths(),
    };
    validate_disk_values(walk, &mut analysis);
    analysis.assist_index.disk = unindexed_disk_faults(&disk);

    let config = match aterm_toml::from_str::<crate::app_config::Config>(source) {
        Ok(config) => config,
        Err(error) => {
            let range = parser_diagnostic_range(source, error.span());
            push_diagnostic(
                &mut analysis,
                source,
                range,
                ConfigDiagnosticSeverity::Error,
                format!("aterm schema: {}", one_line(&error.to_string())),
            );
            return analysis;
        }
    };

    validate_machine_types(source, &document, &mut analysis);
    validate_registered_values(source, &document, &mut analysis);
    warn_runtime_semantics(source, &document, &config, &mut analysis);
    warn_compatibility_only_values(walk, &mut analysis);
    warn_unknown_values(walk, &mut analysis);
    analysis
}

fn parser_diagnostic_range(source: &str, span: Option<Range<usize>>) -> Range<usize> {
    let fallback = 0..source.len().min(1);
    let mut range = span.unwrap_or(fallback);
    if range.is_empty() && range.start >= source.len() {
        let mut authored_end = source.len();
        while authored_end > 0 && matches!(source.as_bytes()[authored_end - 1], b'\n' | b'\r') {
            authored_end -= 1;
        }
        range = authored_end..authored_end;
    }
    range
}

fn warn_compatibility_only_values(walk: KeyWalk<'_>, analysis: &mut ConfigAnalysis) {
    for (key, item) in walk.document.iter() {
        let path = crate::native_config_service::join_config_key_path("", key);
        warn_compatibility_only_item(walk, item, &path, &reader_key("", key), analysis);
    }
}

fn warn_compatibility_only_item(
    walk: KeyWalk<'_>,
    item: &aterm_toml::edit::Item,
    path: &str,
    reader: &str,
    analysis: &mut ConfigAnalysis,
) {
    let at = walk.disk_at(reader);
    if at == DiskFaultAt::Here {
        return;
    }
    let authored = match item {
        aterm_toml::edit::Item::None => false,
        aterm_toml::edit::Item::Table(table) => !table.is_implicit(),
        aterm_toml::edit::Item::Value(_) | aterm_toml::edit::Item::ArrayOfTables(_) => true,
    };
    if authored && at == DiskFaultAt::Nothing && is_compatibility_only_key(path) {
        push_retired_spelling(
            analysis,
            walk.source,
            walk.range(path, reader, item),
            compatibility_only_message(path),
        );
    }
    if let Some(table) = item.as_table_like() {
        for (key, child) in table.iter() {
            let child_path = crate::native_config_service::join_config_key_path(path, key);
            warn_compatibility_only_item(
                walk,
                child,
                &child_path,
                &reader_key(reader, key),
                analysis,
            );
        }
    } else if at == DiskFaultAt::Below {
        for (index, table) in element_tables(item) {
            let element = reader_element(reader, index);
            for (key, child) in table.iter() {
                let child_path = crate::native_config_service::join_config_key_path(path, key);
                warn_compatibility_only_item(
                    walk,
                    child,
                    &child_path,
                    &reader_key(&element, key),
                    analysis,
                );
            }
        }
    }
}

/// Why `settings set|unset` refuses a key it does not write ([`settings_refusal_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsRefusal {
    CompatibilityOnly,
    HandWritten,
    Unknown,
}

/// Why `settings set|unset` refuses `key`, a key it does not write (it writes the keys Settings carries, `prefs::editable_fields`).
pub(crate) fn settings_refusal_of(key: &str) -> SettingsRefusal {
    if is_compatibility_only_key(key) {
        SettingsRefusal::CompatibilityOnly
    } else if MANUAL_SCHEMA.iter().any(|entry| entry.key == key)
        || reader_accepted_paths().contains(key)
    {
        SettingsRefusal::HandWritten
    } else {
        SettingsRefusal::Unknown
    }
}

/// What `settings set|unset` answers for a key it does not write ([`settings_refusal_of`]): a compatibility-only key its story, a hand-written one where it is written, and an unknown one that it is.
pub(crate) fn control_settings_refusal(key: &str) -> String {
    match settings_refusal_of(key) {
        SettingsRefusal::CompatibilityOnly => compatibility_only_message(key),
        SettingsRefusal::HandWritten => fleet_writer(key).map_or_else(
            || {
                format!(
                    "{key} is set in aterm.toml by hand, not through Settings: edit the file or \
                     use Manual, which completes and checks it"
                )
            },
            |writer| format!("{key} is written by `aterm fleet`, not through Settings: {writer}"),
        ),
        SettingsRefusal::Unknown => {
            format!("unknown key {key:?} (search Settings or use Manual for the complete schema)")
        }
    }
}

/// The `aterm fleet` verbs that write a `[fleet]` key the fleet keeps (a grant names the key its
/// book binds, which no hand edit has).
fn fleet_writer(key: &str) -> Option<&'static str> {
    match key {
        "fleet.allow" => Some(
            "`aterm fleet allow facts|content` sets the level, and `aterm fleet allow drive \
             --from <host|node>` grants a drive",
        ),
        "fleet.drive_from" => Some(
            "`aterm fleet allow drive --from <host|node>` writes it with the key the book binds \
             that node to, and `aterm fleet allow facts|content` takes it back",
        ),
        "fleet.listen" => Some(
            "`aterm fleet init` or `aterm fleet join` writes it (`--port <n>` names its port), \
             and `aterm fleet leave` takes it out",
        ),
        _ => None,
    }
}

pub(crate) fn compatibility_only_message(key: &str) -> String {
    if let Some(retired) = retired_config_key(key) {
        let replaced = retired_replacement(key)
            .map(|by| format!(" ({by} replaces it)"))
            .unwrap_or_default();
        return format!(
            "{key} does nothing: {} was removed{replaced}; safe to delete",
            retired.feature
        );
    }
    format!("{key} does nothing in this build; safe to delete")
}

fn retired_replacement(key: &str) -> Option<&'static str> {
    crate::message_reporters::REMOVED_FEATURES
        .iter()
        .find(|feature| {
            key == feature.table
                || key
                    .strip_prefix(feature.table)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
        .map(|feature| feature.replaced_by)
}

fn validate_machine_types(
    source: &str,
    document: &aterm_toml::edit::DocumentMut,
    analysis: &mut ConfigAnalysis,
) {
    for setting in config_schema()
        .iter()
        .filter(|setting| setting.key == "machine" || setting.key.starts_with("machine."))
    {
        let Some(item) = dotted_item(document, setting.key) else {
            continue;
        };
        let range = source_value_range(source, setting.key)
            .or_else(|| item.span())
            .unwrap_or(0..source.len().min(1));
        let machine_type_error = match setting.key {
            "machine" if item.as_table_like().is_none() => Some("machine must be a table"),
            "machine.universal_control" if item.as_str().is_none() => {
                Some("machine.universal_control must be a string (off or leave)")
            }
            "machine.rosetta" if item.as_bool().is_none() => {
                Some("machine.rosetta must be true or false")
            }
            _ => None,
        };
        if let Some(message) = machine_type_error {
            push_unaccepted_value(
                analysis,
                source,
                range,
                ConfigDiagnosticSeverity::Error,
                format!("{message}; atpkg uses the default machine settings at load"),
            );
        }
    }
}

fn validate_disk_values(walk: KeyWalk<'_>, analysis: &mut ConfigAnalysis) {
    let KeyWalk {
        source,
        document,
        disk: faults,
        ..
    } = walk;
    let root = document.as_item().as_table_like();
    for fault in faults {
        let mut at = fault.path.as_str();
        let range = loop {
            if let Some(range) = root.and_then(|root| reader_path_range(root, at)) {
                break range;
            }
            match at.rfind(['.', '[']) {
                Some(cut) if cut > 0 => at = &at[..cut],
                _ => break 0..source.len().min(1),
            }
        };
        let severity = if fault.removal_off {
            ConfigDiagnosticSeverity::Error
        } else {
            ConfigDiagnosticSeverity::Warning
        };
        push_unaccepted_value(analysis, source, range, severity, fault.note.clone());
    }
}

fn reader_path_range(table: &dyn aterm_toml::edit::TableLike, path: &str) -> Option<Range<usize>> {
    table.iter().find_map(|(key, item)| {
        let rest = path.strip_prefix(key)?;
        match rest.as_bytes().first() {
            None => item
                .span()
                .or_else(|| item.as_array_of_tables()?.iter().next()?.span()),
            Some(b'.') => reader_path_range(item.as_table_like()?, &rest[1..]),
            Some(b'[') => {
                let (index, rest) = reader_index(rest)?;
                if let Some(tables) = item.as_array_of_tables() {
                    let element = tables.iter().nth(index)?;
                    match rest.strip_prefix('.') {
                        Some(rest) => reader_path_range(element, rest),
                        None if rest.is_empty() => element.span(),
                        None => None,
                    }
                } else {
                    reader_value_range(item.as_array()?.get(index)?, rest)
                }
            }
            Some(_) => None,
        }
    })
}

fn reader_value_range(value: &aterm_toml::edit::Value, path: &str) -> Option<Range<usize>> {
    match path.as_bytes().first() {
        None => value.span(),
        Some(b'.') => reader_path_range(value.as_inline_table()?, &path[1..]),
        Some(b'[') => {
            let (index, rest) = reader_index(path)?;
            reader_value_range(value.as_array()?.get(index)?, rest)
        }
        Some(_) => None,
    }
}

fn reader_index(path: &str) -> Option<(usize, &str)> {
    let (index, rest) = path.strip_prefix('[')?.split_once(']')?;
    Some((index.parse().ok()?, rest))
}

/// What the harness's disk reader names at a path, spelled as the reader spells it ([`reader_key`], [`reader_element`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiskFaultAt {
    Nothing,
    Here,
    Below,
}

impl DiskFaultAt {
    fn of(faults: &[aterm_agent::harness::cli::DiskFault], at: &str) -> Self {
        Self::find(faults, at).map_or(Self::Nothing, |(found, _)| found)
    }

    fn find<'a>(
        faults: &'a [aterm_agent::harness::cli::DiskFault],
        at: &str,
    ) -> Option<(Self, &'a aterm_agent::harness::cli::DiskFault)> {
        let mut below = None;
        for fault in faults {
            match fault.path.strip_prefix(at) {
                Some("") => return Some((Self::Here, fault)),
                Some(rest) if rest.starts_with(['.', '[']) => {
                    below.get_or_insert((Self::Below, fault));
                }
                _ => {}
            }
        }
        below
    }
}

/// [`DiskFaultAt`] at `path`, spelled as this checker and Settings spell a key (`crate::native_config_service::join_config_key_path`: `"my theme".disk.apply`), with the fault it is about ([`DiskFaultAt::find`]).
pub(crate) fn disk_fault_at<'a>(
    faults: &'a [aterm_agent::harness::cli::DiskFault],
    path: &str,
) -> Option<(DiskFaultAt, &'a aterm_agent::harness::cli::DiskFault)> {
    DiskFaultAt::find(faults, &reader_spelling(path)?)
}

fn reader_key(parent: &str, key: &str) -> String {
    if parent.is_empty() || key.starts_with('[') {
        format!("{parent}{key}")
    } else {
        format!("{parent}.{key}")
    }
}

fn reader_element(parent: &str, index: usize) -> String {
    format!("{parent}[{index}]")
}

fn reader_spelling(path: &str) -> Option<String> {
    let keys = aterm_toml::edit::Key::parse(path).ok()?;
    Some(
        keys.iter()
            .fold(String::new(), |parent, key| reader_key(&parent, key.get())),
    )
}

/// The field list the config READER declares for the table at `path`: `T`'s own `Deserialize` is driven down `path` one key at a time, and the list serde's derive hands `deserialize_struct` at the end of it is recorded.
pub(crate) fn reader_fields<T: serde::de::DeserializeOwned>(
    path: &[&'static str],
) -> Option<&'static [&'static str]> {
    struct Reader<'p> {
        path: &'p [&'static str],
        fields: &'p mut Option<&'static [&'static str]>,
    }
    struct OneKey<'p> {
        key: Option<&'static str>,
        rest: &'p [&'static str],
        fields: &'p mut Option<&'static [&'static str]>,
    }
    fn done<E: serde::de::Error>() -> E {
        E::custom("reader probe")
    }
    impl<'de> serde::Deserializer<'de> for Reader<'_> {
        type Error = serde::de::value::Error;
        fn deserialize_any<V: serde::de::Visitor<'de>>(
            self,
            _: V,
        ) -> Result<V::Value, Self::Error> {
            Err(done())
        }
        fn deserialize_option<V: serde::de::Visitor<'de>>(
            self,
            visitor: V,
        ) -> Result<V::Value, Self::Error> {
            visitor.visit_some(self)
        }
        fn deserialize_newtype_struct<V: serde::de::Visitor<'de>>(
            self,
            _: &'static str,
            visitor: V,
        ) -> Result<V::Value, Self::Error> {
            visitor.visit_newtype_struct(self)
        }
        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, Self::Error> {
            match self.path.split_first() {
                None => {
                    *self.fields = Some(fields);
                    Err(done())
                }
                Some((key, rest)) => visitor.visit_map(OneKey {
                    key: Some(key),
                    rest,
                    fields: self.fields,
                }),
            }
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
            bytes byte_buf unit unit_struct seq tuple tuple_struct map enum
            identifier ignored_any
        }
    }
    impl<'de> serde::de::MapAccess<'de> for OneKey<'_> {
        type Error = serde::de::value::Error;
        fn next_key_seed<K: serde::de::DeserializeSeed<'de>>(
            &mut self,
            seed: K,
        ) -> Result<Option<K::Value>, Self::Error> {
            use serde::de::IntoDeserializer as _;
            self.key
                .take()
                .map(|key| seed.deserialize(key.into_deserializer()))
                .transpose()
        }
        fn next_value_seed<V: serde::de::DeserializeSeed<'de>>(
            &mut self,
            seed: V,
        ) -> Result<V::Value, Self::Error> {
            seed.deserialize(Reader {
                path: self.rest,
                fields: &mut *self.fields,
            })
        }
    }
    let mut fields = None;
    let _ = T::deserialize(Reader {
        path,
        fields: &mut fields,
    });
    fields
}

fn reader_paths<T: serde::de::DeserializeOwned>() -> Vec<String> {
    fn walk<T: serde::de::DeserializeOwned>(path: &mut Vec<&'static str>, out: &mut Vec<String>) {
        let Some(fields) = reader_fields::<T>(path) else {
            return;
        };
        for &field in fields {
            path.push(field);
            out.push(path.join("."));
            walk::<T>(path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk::<T>(&mut Vec::new(), &mut out);
    out
}

fn reader_accepted_paths() -> &'static BTreeSet<String> {
    static PATHS: OnceLock<BTreeSet<String>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let mut paths: BTreeSet<String> = reader_paths::<crate::app_config::Config>()
            .into_iter()
            .collect();
        paths.extend(
            aterm_agent::supervise::config::KEYS
                .iter()
                .map(|key| format!("harness.{key}")),
        );
        paths.extend(
            aterm_agent::harness::disk::KEYS
                .iter()
                .map(|key| format!("disk.{key}")),
        );
        paths
    })
}

#[derive(Clone, Copy)]
struct KeyWalk<'a> {
    source: &'a str,
    document: &'a aterm_toml::edit::DocumentMut,
    disk: &'a [aterm_agent::harness::cli::DiskFault],
    readers: &'a BTreeSet<String>,
}

impl KeyWalk<'_> {
    fn disk_at(&self, reader: &str) -> DiskFaultAt {
        DiskFaultAt::of(self.disk, reader)
    }

    fn range(&self, path: &str, reader: &str, item: &aterm_toml::edit::Item) -> Range<usize> {
        let found = if reader.contains(']') {
            self.document
                .as_item()
                .as_table_like()
                .and_then(|root| reader_path_range(root, reader))
        } else {
            source_value_range(self.source, path)
        };
        found
            .or_else(|| item.span())
            .unwrap_or(0..self.source.len().min(1))
    }
}

fn unindexed_disk_faults(
    faults: &[aterm_agent::harness::cli::DiskFault],
) -> Vec<aterm_agent::harness::cli::DiskFault> {
    faults
        .iter()
        .map(|fault| {
            let mut path = String::with_capacity(fault.path.len());
            let mut rest = fault.path.as_str();
            while let Some(open) = rest.find('[') {
                path.push_str(&rest[..open]);
                match reader_index(&rest[open..]) {
                    Some((_, after)) => rest = after,
                    None => {
                        path.push('[');
                        rest = &rest[open + 1..];
                    }
                }
            }
            path.push_str(rest);
            aterm_agent::harness::cli::DiskFault {
                path,
                ..fault.clone()
            }
        })
        .collect()
}

fn compatibility_only_help(path: &str, index: &ConfigAssistIndex) -> Option<String> {
    let reader = reader_spelling(path)?;
    match DiskFaultAt::find(&index.disk, &reader) {
        None => Some(compatibility_only_message(path)),
        Some((DiskFaultAt::Here, fault)) => Some(fault.note.clone()),
        Some(_) => None,
    }
}

fn element_tables(item: &aterm_toml::edit::Item) -> Vec<(usize, &dyn aterm_toml::edit::TableLike)> {
    if let Some(tables) = item.as_array_of_tables() {
        return tables
            .iter()
            .enumerate()
            .map(|(index, table)| (index, table as &dyn aterm_toml::edit::TableLike))
            .collect();
    }
    item.as_array().map_or_else(Vec::new, |values| {
        values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                let table = value.as_inline_table()?;
                Some((index, table as &dyn aterm_toml::edit::TableLike))
            })
            .collect()
    })
}

fn validate_registered_values(
    source: &str,
    document: &aterm_toml::edit::DocumentMut,
    analysis: &mut ConfigAnalysis,
) {
    for setting in config_schema() {
        if is_compatibility_only_key(setting.key) {
            continue;
        }
        let Some(item) = dotted_item(document, setting.key) else {
            continue;
        };
        let range = source_value_range(source, setting.key)
            .or_else(|| item.span())
            .unwrap_or(0..source.len().min(1));
        match setting.kind {
            ConfigSchemaKind::Scalar(EditKind::Enum { options }) => {
                let Some(value) = item.as_str() else {
                    continue;
                };
                if let Some(note) = retired_but_resolving_note(setting.key, value) {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Warning,
                        note,
                    );
                    continue;
                }
                if !registered_enum_value_is_valid(setting.key, value, options) {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!(
                            "{} must be one of: {}{}",
                            setting.key,
                            options.join(", "),
                            did_you_mean(nearest_enum_value(value, options))
                        ),
                    );
                }
            }
            ConfigSchemaKind::Scalar(EditKind::Color) => {
                let Some(value) = item.as_str() else {
                    continue;
                };
                if !crate::prefs::color_value_is_valid(setting.key, value) {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        if setting.key == crate::prefs::EDIT_CURSOR_TRAIL_COLOR {
                            format!(
                                "{} must be an RRGGBB, #RRGGBB or #RGB color, or {}",
                                setting.key,
                                crate::prefs::TRAIL_COLOR_CURSOR
                            )
                        } else {
                            format!("{} must be an RRGGBB, #RRGGBB or #RGB color", setting.key)
                        },
                    );
                }
            }
            ConfigSchemaKind::Scalar(EditKind::Float | EditKind::Integer) => {
                let number = item
                    .as_float()
                    .or_else(|| item.as_integer().map(|value| value as f64));
                let Some(number) = number else { continue };
                if item.as_integer().is_some_and(|n| n < 0)
                    && let Some(reading) = setting
                        .key
                        .strip_prefix("disk.")
                        .and_then(aterm_agent::harness::disk::Config::negative_reading)
                {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Warning,
                        format!("{} is negative; it is read as {reading}", setting.key),
                    );
                    continue;
                }
                if let Some((min, max)) = semantic_numeric_bounds(setting.key)
                    && !(min..=max).contains(&number)
                    && !runtime_semantics_owns_numeric_clamp(document, setting.key, number)
                {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Warning,
                        format!(
                            "{} is outside the supported {}–{} range and will be clamped",
                            setting.key, min, max
                        ),
                    );
                }
            }
            ConfigSchemaKind::Scalar(EditKind::Theme) => {
                match theme_names(item.as_str().unwrap_or_default()) {
                    Ok(_) => {}
                    Err(message) => push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        message,
                    ),
                }
            }
            ConfigSchemaKind::Scalar(EditKind::Bool | EditKind::Text) => {}
            ConfigSchemaKind::StringList => {
                if !item
                    .as_array()
                    .is_some_and(|array| array.iter().all(aterm_toml::edit::Value::is_str))
                {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be a list of text values", setting.key),
                    );
                }
            }
            ConfigSchemaKind::TextOrStringList => {
                let valid = item.is_str()
                    || item
                        .as_array()
                        .is_some_and(|array| array.iter().all(aterm_toml::edit::Value::is_str));
                if !valid {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be text or a list of text values", setting.key),
                    );
                }
            }
            ConfigSchemaKind::DynamicStringMap => {
                let Some(table) = item.as_table_like() else {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be a table of named text values", setting.key),
                    );
                    continue;
                };
                for (name, value) in table.iter() {
                    let member_path =
                        crate::native_config_service::join_config_key_path(setting.key, name);
                    if !value.is_str() {
                        let value_range = dynamic_map_member_value_range(source, setting.key, name)
                            .or_else(|| value.span())
                            .unwrap_or_else(|| range.clone());
                        push_diagnostic(
                            analysis,
                            source,
                            value_range,
                            ConfigDiagnosticSeverity::Error,
                            format!("{member_path} must be text"),
                        );
                    } else if setting.key == "packages.links"
                        && !package_link_target_is_valid(value.as_str().unwrap_or_default())
                    {
                        let value_range = dynamic_map_member_value_range(source, setting.key, name)
                            .or_else(|| value.span())
                            .unwrap_or_else(|| range.clone());
                        push_diagnostic(
                            analysis,
                            source,
                            value_range,
                            ConfigDiagnosticSeverity::Warning,
                            format!(
                                "{member_path} must be an absolute/~/ checkout path or a safe owner/repo slug; atpkg will ignore it"
                            ),
                        );
                    }
                }
            }
            ConfigSchemaKind::StructuredList => {
                let array_of_tables = item.as_array_of_tables().is_some();
                let inline_records = item.as_array().is_some_and(|array| {
                    array.iter().all(|value| value.as_inline_table().is_some())
                });
                if !array_of_tables && !inline_records {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be a list of records", setting.key),
                    );
                }
            }
            ConfigSchemaKind::Table => {
                // A `disk` that is not a table is told in the reader's words
                // ([`validate_disk_values`]). A `machine` that is not a table never gets
                // here: `Config` reads it, so the parse refuses it.
                if !item.is_table_like() && setting.key != "disk" {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be a table", setting.key),
                    );
                }
            }
            ConfigSchemaKind::Flexible => {}
        }
    }
}

fn package_link_target_is_valid(value: &str) -> bool {
    #[cfg(windows)]
    let home = Path::new(r"C:\Users\aterm");
    #[cfg(not(windows))]
    let home = Path::new("/home/aterm");
    !matches!(
        atpkg::config::classify_link(value, Some(home)),
        atpkg::config::LinkTarget::Invalid
    )
}

fn semantic_numeric_bounds(key: &str) -> Option<(f64, f64)> {
    match key {
        crate::prefs::EDIT_COLUMNS => Some((20.0, 500.0)),
        crate::prefs::EDIT_LINES => Some((5.0, 300.0)),
        "sparkle_words.custom.burst.chance" => Some((0.0, 100.0)),
        _ => crate::prefs::range_of(key).map(|range| (range.min, range.max)),
    }
}

fn runtime_semantics_owns_numeric_clamp(
    document: &aterm_toml::edit::DocumentMut,
    key: &str,
    number: f64,
) -> bool {
    match key {
        crate::prefs::EDIT_FONT_PX => true,
        "sparkle_words.ink.sweep_ms" => {
            number < 600.0
                && dotted_item(document, "sparkle_words.ink.loop")
                    .and_then(aterm_toml::edit::Item::as_bool)
                    .unwrap_or(false)
        }
        "matrix_rain.head_alpha" => {
            use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR};
            dotted_item(document, "matrix_rain.alpha")
                .and_then(aterm_toml::edit::Item::as_integer)
                .map_or(number < f64::from(RAIN_ALPHA_CAP), |alpha| {
                    number
                        < alpha.clamp(i64::from(RAIN_ALPHA_FLOOR), i64::from(RAIN_ALPHA_CAP)) as f64
                })
        }
        _ => false,
    }
}

/// [`analyze_host_with_assets`] with a freshly discovered theme catalog and this process's nesting fact.
#[cfg(test)]
pub(crate) fn analyze_host(source: &str, backend_gpu: bool) -> Vec<ConfigDiagnostic> {
    let Ok(config) = aterm_toml::from_str::<crate::app_config::Config>(source) else {
        return Vec::new();
    };
    let themes = crate::app_config::ThemeCatalog::discover();
    let assets = config.resolve_asset_catalog_with_themes(themes);
    analyze_host_with_assets(source, backend_gpu, &assets)
}

/// The filesystem-backed half of Manual validation, against the exact asset catalog admitted with the document generation.
pub(crate) fn analyze_host_with_assets(
    source: &str,
    backend_gpu: bool,
    assets: &crate::app_config::ConfigAssetCatalog,
) -> Vec<ConfigDiagnostic> {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return Vec::new();
    }
    let Ok(document) = source.parse::<aterm_toml::edit::DocumentMut>() else {
        return Vec::new();
    };
    let Ok(config) = aterm_toml::from_str::<crate::app_config::Config>(source) else {
        return Vec::new();
    };
    let draft_assets =
        config.resolve_asset_catalog_with_themes(std::sync::Arc::clone(&assets.themes));
    let mut analysis = ConfigAnalysis::default();
    append_semantic_warnings(
        source,
        &document,
        crate::diagnostics::config_host_semantic_warnings_with_backend_and_assets(
            &config,
            backend_gpu,
            &draft_assets,
        ),
        &mut analysis,
    );
    analysis.diagnostics
}

fn warn_runtime_semantics(
    source: &str,
    document: &aterm_toml::edit::DocumentMut,
    config: &crate::app_config::Config,
    analysis: &mut ConfigAnalysis,
) {
    append_semantic_warnings(
        source,
        document,
        crate::diagnostics::config_semantic_warnings(config),
        analysis,
    );
}

fn append_semantic_warnings(
    source: &str,
    document: &aterm_toml::edit::DocumentMut,
    warnings: impl IntoIterator<Item = crate::diagnostics::ConfigSemanticWarning>,
    analysis: &mut ConfigAnalysis,
) {
    for warning in warnings {
        let range =
            semantic_warning_range(source, document, &warning).unwrap_or(0..source.len().min(1));
        push_diagnostic(
            analysis,
            source,
            range,
            ConfigDiagnosticSeverity::Warning,
            warning.message,
        );
    }
}

fn semantic_warning_range(
    source: &str,
    document: &aterm_toml::edit::DocumentMut,
    warning: &crate::diagnostics::ConfigSemanticWarning,
) -> Option<Range<usize>> {
    if let Some(range) = dynamic_map_semantic_warning_range(source, document, warning) {
        return Some(range);
    }

    if let Some(index) = indexed_warning_index(&warning.message, warning.key)
        && let Some(array) =
            dotted_item(document, warning.key).and_then(aterm_toml::edit::Item::as_array)
    {
        let source_index = if warning.key == crate::prefs::EDIT_FONT_VARIATION {
            array
                .iter()
                .enumerate()
                .filter(|(_, value)| value.as_str().is_some_and(|value| !value.trim().is_empty()))
                .nth(index)
                .map(|(source_index, _)| source_index)
        } else {
            Some(index)
        };
        if let Some(range) = source_index
            .and_then(|source_index| array.get(source_index))
            .and_then(aterm_toml::edit::Value::span)
        {
            return Some(range);
        }
        if let Some(source_index) = source_index {
            let item_span =
                dotted_item(document, warning.key).and_then(aterm_toml::edit::Item::span);
            for value_range in source_value_range(source, warning.key)
                .into_iter()
                .chain(item_span)
            {
                let Some(value_source) = source.get(value_range.clone()) else {
                    continue;
                };
                let Some(entry) = delimited_value_entries(value_source, b'[', b']')
                    .and_then(|entries| entries.into_iter().nth(source_index))
                else {
                    continue;
                };
                return Some(value_range.start + entry.start..value_range.start + entry.end);
            }
        }
    }

    if let Some(record_path) = array_table_root(warning.key)
        && let Some(record) = structured_warning_record(&warning.message, record_path)
    {
        if warning.key == record_path {
            if let Some(range) = structured_record_root_range(source, record_path, record) {
                return Some(range);
            }
        } else if let Some(word_index) =
            structured_warning_word_index(&warning.message, record_path)
            && let Some(range) = structured_record_array_element_range(
                source,
                warning.key,
                record_path,
                record,
                word_index,
            )
        {
            return Some(range);
        } else if let Some(range) =
            structured_record_value_range(source, warning.key, record_path, record)
        {
            return Some(range);
        }
    }

    source_value_range(source, warning.key)
        .or_else(|| source_table_header_range(source, warning.key))
        .or_else(|| dotted_item(document, warning.key).and_then(aterm_toml::edit::Item::span))
}

fn dynamic_map_semantic_warning_range(
    source: &str,
    document: &aterm_toml::edit::DocumentMut,
    warning: &crate::diagnostics::ConfigSemanticWarning,
) -> Option<Range<usize>> {
    if !matches!(warning.key, "keybindings" | "key_sequences") {
        return None;
    }
    let table = dotted_item(document, warning.key)?.as_table_like()?;
    for (member, item) in table.iter() {
        let member_debug = format!("{member:?}");
        if warning
            .message
            .starts_with(&format!("{}[{member_debug}]:", warning.key))
        {
            return item
                .span()
                .or_else(|| dynamic_map_member_value_range(source, warning.key, member));
        }
        if warning
            .message
            .starts_with(&format!("{}: chord {member_debug} ", warning.key))
        {
            return dynamic_map_member_key_range(source, warning.key, member)
                .or_else(|| table.get_key_value(member).and_then(|(key, _)| key.span()));
        }
    }
    None
}

fn indexed_warning_index(message: &str, key: &str) -> Option<usize> {
    let suffix = message.strip_prefix(key)?.strip_prefix('[')?;
    let end = suffix.find(']')?;
    suffix[..end].parse().ok()
}

fn structured_warning_record(message: &str, record_path: &str) -> Option<usize> {
    let marker = format!("in [[{record_path}]] record ");
    let suffix = message.split_once(&marker)?.1;
    let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0)
        .then(|| suffix[..digits].parse().ok())
        .flatten()
}

fn structured_warning_word_index(message: &str, record_path: &str) -> Option<usize> {
    let marker = format!("in [[{record_path}]] record ");
    let suffix = message.split_once(&marker)?.1;
    let record_digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
    let suffix = suffix.get(record_digits..)?.strip_prefix(" word ")?;
    let word_digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
    if word_digits == 0 {
        return None;
    }
    suffix[..word_digits].parse::<usize>().ok()?.checked_sub(1)
}

pub(crate) fn registered_enum_value_is_valid(key: &str, value: &str, options: &[&str]) -> bool {
    let value = value.trim();
    if key == "machine.universal_control" {
        // Other spellings warn and Leave, so the generic case-insensitive acceptance would
        // promise a setting atpkg ignores.
        return value.is_empty() || UNIVERSAL_CONTROL_POLICIES.contains(&value);
    }
    if key == "sparkle_words.custom.ink.colorway" {
        return custom_ink_colorway_is_valid(value);
    }
    if matches!(
        key,
        crate::prefs::EDIT_CURSOR_TRAIL_STYLE | crate::prefs::EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF
    ) {
        return crate::prefs::cursor_trail_style_canonical(value).is_some();
    }
    if key == crate::prefs::EDIT_TRAIL_SOUND_STYLE {
        return crate::prefs::trail_sound_style_canonical(value).is_some();
    }
    if key == crate::prefs::EDIT_DISPLAY_FONT && value.contains('+') {
        let parts: Vec<&str> = value
            .split('+')
            .map(|part| aterm_render::display_face_canonical_id(part).unwrap_or(part.trim()))
            .collect();
        let distinct = parts
            .iter()
            .all(|part| parts.iter().filter(|other| other == &part).count() == 1);
        return parts.len() >= 2
            && parts.len() <= aterm_render::DISPLAY_FACE_MIX_MAX
            && distinct
            && parts
                .iter()
                .all(|part| crate::prefs::display_font_id_is_accepted(part));
    }
    options
        .iter()
        .chain(enum_aliases(key))
        .any(|option| option.eq_ignore_ascii_case(value))
}

fn enum_aliases(key: &str) -> &'static [&'static str] {
    match key {
        crate::prefs::EDIT_CURSOR_STYLE => CURSOR_STYLE_ALIASES,
        crate::prefs::EDIT_BIDI => BIDI_ALIASES,
        crate::prefs::EDIT_AMBIGUOUS_WIDTH => AMBIGUOUS_WIDTH_ALIASES,
        crate::prefs::EDIT_PREDICTIVE_ECHO => PREDICTIVE_ECHO_ALIASES,
        crate::prefs::EDIT_TEXT_BLENDING => TEXT_BLENDING_ALIASES,
        crate::prefs::EDIT_MOTION => MOTION_ALIASES,
        crate::prefs::EDIT_WINDOW_COLORSPACE => WINDOW_COLORSPACE_ALIASES,
        crate::prefs::EDIT_BACKGROUND_MATERIAL => BACKGROUND_MATERIAL_ALIASES,
        "sparkle_words.custom.burst.kind" => CUSTOM_BURST_KIND_ALIASES,
        "windowing_behavior" => WINDOWING_BEHAVIOR_ALIASES,
        "right_click" => RIGHT_CLICK_ALIASES,
        "tab_menu_chord" => TAB_MENU_CHORD_ALIASES,
        crate::prefs::EDIT_HARNESS_AUTOPILOT => HARNESS_AUTOPILOT_ALIASES,
        _ => &[],
    }
}

fn custom_ink_colorway_is_valid(value: &str) -> bool {
    aterm_effects::spec::custom_colorway_is_valid(value)
}

/// Parse a plain or `dark:…,light:…` theme setting into dark and light theme names.
pub(crate) fn theme_names(value: &str) -> Result<aterm_types::scheme::ThemeSelector, String> {
    let (selector, issues) = aterm_types::scheme::ThemeSelector::parse(value);
    if issues.is_empty() {
        Ok(selector)
    } else {
        Err(issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "))
    }
}

fn warn_unknown_values(walk: KeyWalk<'_>, analysis: &mut ConfigAnalysis) {
    for (key, item) in walk.document.iter() {
        let path = crate::native_config_service::join_config_key_path("", key);
        warn_unknown_item(walk, item, &path, &reader_key("", key), analysis);
    }
}

fn item_at<'a>(
    document: &'a aterm_toml::edit::DocumentMut,
    dotted: &str,
) -> Option<&'a aterm_toml::edit::Item> {
    let mut parts = dotted.split('.');
    let mut item = document.get(parts.next()?)?;
    for part in parts {
        item = item.get(part)?;
    }
    Some(item)
}

fn retired_packages_message(
    document: &aterm_toml::edit::DocumentMut,
    path: &str,
    current: &str,
    value: Option<bool>,
) -> String {
    let written = item_at(document, current);
    if path == "packages.auto_update"
        && let Some(value) = value
    {
        let enabled = written.and_then(aterm_toml::edit::Item::as_bool);
        if written.is_none() || enabled.is_some() {
            return atpkg::config::auto_update_note(value, enabled);
        }
    }
    if written.is_some() {
        format!("`{path}` is retired and `{current}` decides; remove it")
    } else {
        format!(
            "`{path}` is retired; rename it to `{current}` (the old key is still \
             read — Settings writes the new one and drops the old)"
        )
    }
}

fn warn_unknown_item(
    walk: KeyWalk<'_>,
    item: &aterm_toml::edit::Item,
    path: &str,
    reader: &str,
    analysis: &mut ConfigAnalysis,
) {
    if is_compatibility_only_key(path) {
        // The compatibility walker already reports every entry of an inert subtree; calling
        // it "unknown" too would give one key two contradictory recoveries.
        return;
    }
    let at = walk.disk_at(reader);
    if at == DiskFaultAt::Here
        || (at == DiskFaultAt::Nothing && warn_retired_spelling(walk, item, path, reader, analysis))
    {
        return;
    }
    let schema = config_schema_entry(path);
    if schema.is_some_and(|entry| entry.kind == ConfigSchemaKind::StructuredList) {
        warn_unknown_structured_records(walk, item, path, reader, analysis);
        return;
    }
    if schema.is_some_and(|entry| {
        matches!(
            entry.kind,
            ConfigSchemaKind::DynamicStringMap | ConfigSchemaKind::Flexible
        )
    }) {
        // Dynamic map keys are authored names. Flexible values have no stable
        // descendant schema outside the explicitly registered structured-list
        // records handled above.
        return;
    }
    if let Some(table) = item.as_table_like() {
        for (key, child) in table.iter() {
            let child_path = crate::native_config_service::join_config_key_path(path, key);
            warn_unknown_item(walk, child, &child_path, &reader_key(reader, key), analysis);
        }
        return;
    }
    if at == DiskFaultAt::Below {
        for (index, table) in element_tables(item) {
            let element = reader_element(reader, index);
            for (key, child) in table.iter() {
                let child_path = crate::native_config_service::join_config_key_path(path, key);
                warn_unknown_item(
                    walk,
                    child,
                    &child_path,
                    &reader_key(&element, key),
                    analysis,
                );
            }
        }
        return;
    }
    if schema.is_none() && !walk.readers.contains(path) {
        push_diagnostic(
            analysis,
            walk.source,
            walk.range(path, reader, item),
            ConfigDiagnosticSeverity::Warning,
            match nearest_config_key(path) {
                Some(near) => format!("{path} — did you mean {near:?}? (no effect in this build)"),
                None => format!("{path} is unknown to this build and has no effect"),
            },
        );
    }
}

fn warn_retired_spelling(
    walk: KeyWalk<'_>,
    item: &aterm_toml::edit::Item,
    path: &str,
    reader: &str,
    analysis: &mut ConfigAnalysis,
) -> bool {
    let source = walk.source;
    if let Some((_, current)) = crate::prefs::RETIRED_PACKAGES_SPELLINGS
        .iter()
        .find(|(retired, _)| *retired == path)
    {
        let range = walk.range(path, reader, item);
        push_retired_spelling(
            analysis,
            source,
            range.clone(),
            retired_packages_message(walk.document, path, current, item.as_bool()),
        );
        if item.as_bool().is_none() {
            push_unaccepted_value(
                analysis,
                source,
                range,
                ConfigDiagnosticSeverity::Error,
                format!("{path} must be true or false"),
            );
        }
        return true;
    }
    if path == crate::prefs::RETIRED_CURSOR_TRAIL {
        let range = walk.range(path, reader, item);
        let message = match item.as_bool() {
            Some(false) => {
                "`cursor_trail` is retired: `false` reads as `cursor_trail_style = \"off\"` \
                 and `cursor_momentum_glow = false`; write those instead (Settings drops the old \
                 key when it writes either)"
                    .to_string()
            }
            _ => "`cursor_trail` is retired; `cursor_trail_style` and `cursor_momentum_glow` \
                 decide; remove it"
                .to_string(),
        };
        push_retired_spelling(analysis, source, range.clone(), message);
        if item.as_bool().is_none() {
            push_unaccepted_value(
                analysis,
                source,
                range,
                ConfigDiagnosticSeverity::Error,
                format!("{path} must be true or false"),
            );
        }
        return true;
    }
    let one_value = item
        .as_bool()
        .map(|b| b.to_string())
        .or_else(|| item.as_str().map(str::to_string))
        .or_else(|| item.as_integer().map(|n| n.to_string()));
    if let Some(message) = path.strip_prefix("harness.").and_then(|key| {
        aterm_agent::supervise::config::retired_key_note_text(key, one_value.as_deref())
    }) {
        push_retired_spelling(analysis, source, walk.range(path, reader, item), message);
        return true;
    }
    false
}

#[derive(Clone, Copy)]
struct Record<'a> {
    path: &'a str,
    number: usize,
}

fn warn_unknown_structured_records(
    walk: KeyWalk<'_>,
    item: &aterm_toml::edit::Item,
    record_path: &str,
    reader: &str,
    analysis: &mut ConfigAnalysis,
) {
    for (index, table) in element_tables(item) {
        let record = Record {
            path: record_path,
            number: index + 1,
        };
        let element = reader_element(reader, index);
        warn_unknown_structured_table(walk, table, record_path, record, &element, analysis);
    }
}

fn warn_unknown_structured_table(
    walk: KeyWalk<'_>,
    table: &dyn aterm_toml::edit::TableLike,
    table_path: &str,
    record: Record<'_>,
    reader: &str,
    analysis: &mut ConfigAnalysis,
) {
    for (key, child) in table.iter() {
        let path = crate::native_config_service::join_config_key_path(table_path, key);
        warn_unknown_structured_item(
            walk,
            child,
            &path,
            record,
            &reader_key(reader, key),
            analysis,
        );
    }
}

fn warn_unknown_structured_item(
    walk: KeyWalk<'_>,
    item: &aterm_toml::edit::Item,
    path: &str,
    record: Record<'_>,
    reader: &str,
    analysis: &mut ConfigAnalysis,
) {
    let at = walk.disk_at(reader);
    if at == DiskFaultAt::Here {
        return;
    }
    if config_schema_entry(path).is_none() {
        if at == DiskFaultAt::Nothing {
            push_diagnostic(
                analysis,
                walk.source,
                structured_record_value_range(walk.source, path, record.path, record.number)
                    .or_else(|| item.span())
                    .unwrap_or(0..walk.source.len().min(1)),
                ConfigDiagnosticSeverity::Warning,
                format!(
                    "{path} is unknown in [[{}]] record {} and has no effect",
                    record.path, record.number
                ),
            );
        } else if let Some(table) = item.as_table_like() {
            warn_unknown_structured_table(walk, table, path, record, reader, analysis);
        }
        return;
    }

    warn_structured_registered_value(walk.source, item, path, record, analysis);
    if let Some(table) = item.as_table_like() {
        warn_unknown_structured_table(walk, table, path, record, reader, analysis);
    }
}

fn warn_structured_registered_value(
    source: &str,
    item: &aterm_toml::edit::Item,
    path: &str,
    record: Record<'_>,
    analysis: &mut ConfigAnalysis,
) {
    let Record {
        path: record_path,
        number: record,
    } = record;
    let number = item
        .as_float()
        .or_else(|| item.as_integer().map(|value| value as f64));
    if let (Some(number), Some((min, max))) = (number, semantic_numeric_bounds(path))
        && !(min..=max).contains(&number)
    {
        push_unaccepted_value(
            analysis,
            source,
            structured_record_value_range(source, path, record_path, record)
                .or_else(|| item.span())
                .unwrap_or(0..source.len().min(1)),
            ConfigDiagnosticSeverity::Warning,
            format!(
                "{path} in [[{record_path}]] record {record} is outside the supported {min}–{max} range and will be clamped"
            ),
        );
    }

    let Some(value) = item.as_str() else {
        return;
    };
    let fallback = match path {
        "sparkle_words.custom.ink" | "sparkle_words.custom.ink.colorway"
            if !custom_ink_colorway_is_valid(value) =>
        {
            Some(("rainbow", CUSTOM_INK_COLORWAYS))
        }
        "sparkle_words.custom.burst.kind"
            if !registered_enum_value_is_valid(path, value, CUSTOM_BURST_KINDS) =>
        {
            Some(("starburst", CUSTOM_BURST_KINDS))
        }
        "sparkle_words.custom.graphic.collection"
            if !registered_enum_value_is_valid(path, value, CUSTOM_GRAPHIC_COLLECTIONS) =>
        {
            Some(("cats", CUSTOM_GRAPHIC_COLLECTIONS))
        }
        _ => None,
    };
    let Some((fallback, options)) = fallback else {
        return;
    };
    push_unaccepted_value(
        analysis,
        source,
        structured_record_value_range(source, path, record_path, record)
            .or_else(|| item.span())
            .unwrap_or(0..source.len().min(1)),
        ConfigDiagnosticSeverity::Warning,
        format!(
            "{path} in [[{record_path}]] record {record} is not recognized; {fallback} will be used at runtime (supported: {})",
            options.join(", ")
        ),
    );
}

fn dotted_item<'a>(
    document: &'a aterm_toml::edit::DocumentMut,
    key: &str,
) -> Option<&'a aterm_toml::edit::Item> {
    let mut item = document.as_item();
    for segment in aterm_toml::edit::Key::parse(key).ok()? {
        item = item.as_table_like()?.get(segment.get())?;
    }
    Some(item)
}

fn canonical_key_expression(expression: &str) -> Option<String> {
    let segments = aterm_toml::edit::Key::parse(expression.trim()).ok()?;
    let mut path = String::new();
    for segment in segments {
        path = crate::native_config_service::join_config_key_path(&path, segment.get());
    }
    (!path.is_empty()).then_some(path)
}

fn join_key_expressions(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}.{child}")
    }
}

fn source_value_range(source: &str, dotted_key: &str) -> Option<Range<usize>> {
    let mut current_table = String::new();
    for (line_start, code) in toml_code_lines(source, false) {
        if let Some(table) = table_header_identity(code) {
            current_table = table;
        } else if let Some(equal) = find_unquoted(code, b'=') {
            let authored_key = canonical_key_expression(&code[..equal])?;
            let path = join_key_expressions(&current_table, &authored_key);
            if path == dotted_key {
                let (start, value) = assignment_value(code, line_start, equal);
                return delimited_source_value_range(source, start)
                    .or_else(|| Some(start..start + value.len().max(1).min(source.len() - start)));
            }
        }
    }
    None
}

fn toml_code_lines(
    source: &str,
    keep_multiline_openers: bool,
) -> impl Iterator<Item = (usize, &str)> {
    let mut multiline = None;
    let mut line_start = 0usize;
    source
        .split_inclusive('\n')
        .filter_map(move |line_with_newline| {
            let start = line_start;
            line_start = line_start.saturating_add(line_with_newline.len());
            let line = line_with_newline
                .strip_suffix('\n')
                .unwrap_or(line_with_newline);
            let started_in_multiline = multiline.is_some();
            let scan = scan_toml_fragment(line, multiline);
            multiline = scan.multiline;
            if started_in_multiline || (scan.touched_multiline && !keep_multiline_openers) {
                return None;
            }
            Some((start, &line[..scan.comment_start.unwrap_or(line.len())]))
        })
}

fn assignment_value(code: &str, line_start: usize, equal: usize) -> (usize, &str) {
    let after_equal = &code[equal + 1..];
    let leading = after_equal.len() - after_equal.trim_start().len();
    (line_start + equal + 1 + leading, after_equal.trim())
}

fn delimited_source_value_range(source: &str, start: usize) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let close = match bytes.get(start)? {
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let mut closes = vec![close];
    let mut multiline = None;
    let mut single = None;
    let mut escaped = false;
    let mut in_comment = false;
    let mut index = start + 1;

    while index < bytes.len() {
        if in_comment {
            if bytes[index] == b'\n' {
                in_comment = false;
            }
            index += 1;
            continue;
        }
        if let Some(active) = multiline {
            let delimiter = match active {
                MultilineString::Basic => b'"',
                MultilineString::Literal => b'\'',
            };
            if active == MultilineString::Basic && bytes[index] == b'\\' {
                index = (index + 2).min(bytes.len());
                continue;
            }
            if bytes[index..].starts_with(&[delimiter; 3]) {
                multiline = None;
                index += 3;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(active) = single {
            let byte = bytes[index];
            index += 1;
            if active == b'"' && byte == b'\\' && !escaped {
                escaped = true;
                continue;
            }
            if byte == active && !escaped {
                single = None;
            }
            escaped = false;
            continue;
        }

        if bytes[index] == b'#' {
            in_comment = true;
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"\"\"\"") {
            multiline = Some(MultilineString::Basic);
            index += 3;
            continue;
        }
        if bytes[index..].starts_with(b"'''") {
            multiline = Some(MultilineString::Literal);
            index += 3;
            continue;
        }
        if matches!(bytes[index], b'\'' | b'"') {
            single = Some(bytes[index]);
            escaped = false;
            index += 1;
            continue;
        }

        match bytes[index] {
            b'[' => closes.push(b']'),
            b'{' => closes.push(b'}'),
            byte if closes.last() == Some(&byte) => {
                closes.pop();
                if closes.is_empty() {
                    return Some(start..index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn source_table_header_range(source: &str, dotted_key: &str) -> Option<Range<usize>> {
    for (line_start, code) in toml_code_lines(source, false) {
        if table_header_identity(code).as_deref() == Some(dotted_key) {
            let trimmed = trim_ascii_range(code, 0..code.len())?;
            return Some(line_start + trimmed.start..line_start + trimmed.end);
        }
    }
    None
}

fn structured_record_root_range(
    source: &str,
    record_path: &str,
    record: usize,
) -> Option<Range<usize>> {
    let mut current_table = String::new();
    let mut current_record = 0usize;
    for (line_start, code) in toml_code_lines(source, false) {
        if let Some(table) = table_header_identity(code) {
            if code.trim_start().starts_with("[[") && table == record_path {
                current_record = current_record.saturating_add(1);
                if current_record == record {
                    let trimmed = trim_ascii_range(code, 0..code.len())?;
                    return Some(line_start + trimmed.start..line_start + trimmed.end);
                }
            }
            current_table = table;
        } else if let Some(equal) = find_unquoted(code, b'=') {
            let authored_key = canonical_key_expression(&code[..equal])?;
            let assignment_path = join_key_expressions(&current_table, &authored_key);
            if assignment_path == record_path {
                let (value_start, value_source) = assignment_value(code, line_start, equal);
                let range = delimited_value_entries(value_source, b'[', b']')?
                    .into_iter()
                    .nth(record.saturating_sub(1))?;
                return Some(value_start + range.start..value_start + range.end);
            }
        }
    }
    None
}

fn structured_record_value_range(
    source: &str,
    dotted_key: &str,
    record_path: &str,
    record: usize,
) -> Option<Range<usize>> {
    let mut current_table = String::new();
    let mut current_record = 0usize;
    let mut active_record = false;

    for (line_start, code) in toml_code_lines(source, false) {
        if let Some(table) = table_header_identity(code) {
            let is_array = code.trim_start().starts_with("[[");
            if is_array && table == record_path {
                current_record = current_record.saturating_add(1);
                active_record = current_record == record;
            } else if is_array {
                active_record = false;
            } else {
                active_record = current_record == record
                    && (table == record_path
                        || table
                            .strip_prefix(record_path)
                            .is_some_and(|suffix| suffix.starts_with('.')));
            }
            current_table = table;
        } else if let Some(equal) = find_unquoted(code, b'=') {
            let authored_key = canonical_key_expression(&code[..equal])?;
            let assignment_path = join_key_expressions(&current_table, &authored_key);
            let (value_start, value_source) = assignment_value(code, line_start, equal);

            if assignment_path == record_path {
                let suffix = dotted_key.strip_prefix(record_path)?.strip_prefix('.')?;
                if let Some(range) = parsed_value_descendant_range(
                    value_source,
                    value_start,
                    Some(record.saturating_sub(1)),
                    suffix,
                ) {
                    return Some(range);
                }
            }

            if active_record {
                if assignment_path == dotted_key {
                    return Some(value_start..value_start + value_source.len().max(1));
                }
                if let Some(suffix) = dotted_key
                    .strip_prefix(&assignment_path)
                    .and_then(|suffix| suffix.strip_prefix('.'))
                    && let Some(range) =
                        parsed_value_descendant_range(value_source, value_start, None, suffix)
                {
                    return Some(range);
                }
            }
        }
    }
    None
}

fn structured_record_array_element_range(
    source: &str,
    dotted_key: &str,
    record_path: &str,
    record: usize,
    index: usize,
) -> Option<Range<usize>> {
    let value_range = structured_record_value_range(source, dotted_key, record_path, record)?;
    let value_range =
        delimited_source_value_range(source, value_range.start).unwrap_or(value_range);
    let value_source = source.get(value_range.clone())?;
    let entry = delimited_value_entries(value_source, b'[', b']')?
        .into_iter()
        .nth(index)?;
    Some(value_range.start + entry.start..value_range.start + entry.end)
}

fn parsed_value_descendant_range(
    value_source: &str,
    value_start: usize,
    array_index: Option<usize>,
    dotted_suffix: &str,
) -> Option<Range<usize>> {
    let record_range = if let Some(index) = array_index {
        delimited_value_entries(value_source, b'[', b']')?
            .into_iter()
            .nth(index)?
    } else {
        0..value_source.len()
    };
    let relative =
        inline_table_descendant_range(&value_source[record_range.clone()], dotted_suffix)?;
    Some(
        value_start + record_range.start + relative.start
            ..value_start + record_range.start + relative.end,
    )
}

fn inline_table_descendant_range(value: &str, dotted_suffix: &str) -> Option<Range<usize>> {
    for entry in delimited_value_entries(value, b'{', b'}')? {
        let entry_source = &value[entry.clone()];
        let equal = find_top_level_byte(entry_source, b'=')?;
        let authored_key = canonical_key_expression(&entry_source[..equal])?;
        let value_in_entry = trim_ascii_range(entry_source, equal + 1..entry_source.len())?;
        let value_range = entry.start + value_in_entry.start..entry.start + value_in_entry.end;
        if authored_key == dotted_suffix {
            return Some(value_range);
        }
        if let Some(descendant) = dotted_suffix
            .strip_prefix(&authored_key)
            .and_then(|suffix| suffix.strip_prefix('.'))
            && let Some(nested) =
                inline_table_descendant_range(&value[value_range.clone()], descendant)
        {
            return Some(value_range.start + nested.start..value_range.start + nested.end);
        }
    }
    None
}

fn delimited_value_entries(value: &str, open: u8, close: u8) -> Option<Vec<Range<usize>>> {
    let outer = trim_ascii_range(value, 0..value.len())?;
    let bytes = value.as_bytes();
    if bytes.get(outer.start) != Some(&open) || bytes.get(outer.end.checked_sub(1)?) != Some(&close)
    {
        return None;
    }
    let inner = outer.start + 1..outer.end - 1;
    let mut entries = Vec::new();
    let mut entry_start = inner.start;
    let mut index = inner.start;
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    while index < inner.end {
        let byte = bytes[index];
        if let Some(active) = quote {
            if active == b'"' && byte == b'\\' && !escaped {
                escaped = true;
                index += 1;
                continue;
            }
            if byte == active && !escaped {
                quote = None;
            }
            escaped = false;
        } else {
            match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' | b'{' => depth = depth.saturating_add(1),
                b']' | b'}' => depth = depth.saturating_sub(1),
                b',' if depth == 0 => {
                    if let Some(entry) = trim_ascii_range(value, entry_start..index) {
                        entries.push(entry);
                    }
                    entry_start = index + 1;
                }
                _ => {}
            }
        }
        index += 1;
    }
    if let Some(entry) = trim_ascii_range(value, entry_start..inner.end) {
        entries.push(entry);
    }
    Some(entries)
}

fn find_top_level_byte(value: &str, needle: u8) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if let Some(active) = quote {
            if active == b'"' && byte == b'\\' && !escaped {
                escaped = true;
                continue;
            }
            if byte == active && !escaped {
                quote = None;
            }
            escaped = false;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'[' | b'{' => depth = depth.saturating_add(1),
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ if byte == needle && depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn trim_ascii_range(value: &str, range: Range<usize>) -> Option<Range<usize>> {
    let bytes = value.as_bytes();
    let mut start = range.start;
    let mut end = range.end;
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start < end).then_some(start..end)
}

/// Host handoff locator for Settings → Manual.
pub(crate) fn config_key_source_range(source: &str, dotted_key: &str) -> Option<Range<usize>> {
    source_value_range(source, dotted_key)
}

fn dynamic_map_member_value_range(
    source: &str,
    table_path: &str,
    member: &str,
) -> Option<Range<usize>> {
    let mut current = String::new();
    let member = crate::native_config_service::join_config_key_path("", member);
    for (line_start, code) in toml_code_lines(source, false) {
        if let Some(table) = table_header_identity(code) {
            current = table;
        } else if current == table_path
            && let Some(equal) = find_unquoted(code, b'=')
        {
            let authored_key = canonical_key_expression(&code[..equal])?;
            if authored_key == member {
                let (start, value) = assignment_value(code, line_start, equal);
                return Some(start..start + value.len().max(1).min(source.len() - start));
            }
        }
    }
    None
}

fn dynamic_map_member_key_range(
    source: &str,
    table_path: &str,
    member: &str,
) -> Option<Range<usize>> {
    let mut current = String::new();
    let member = crate::native_config_service::join_config_key_path("", member);
    for (line_start, code) in toml_code_lines(source, true) {
        if let Some(table) = table_header_identity(code) {
            current = table;
        } else if current == table_path
            && let Some(equal) = find_unquoted(code, b'=')
            && canonical_key_expression(&code[..equal]).as_deref() == Some(member.as_str())
        {
            let key = trim_ascii_range(code, 0..equal)?;
            return Some(line_start + key.start..line_start + key.end);
        }
    }
    None
}

fn push_diagnostic(
    analysis: &mut ConfigAnalysis,
    source: &str,
    bytes: Range<usize>,
    severity: ConfigDiagnosticSeverity,
    message: String,
) {
    let _ = analysis.insert_diagnostic(located_diagnostic(source, bytes, severity, message));
}

fn push_retired_spelling(
    analysis: &mut ConfigAnalysis,
    source: &str,
    bytes: Range<usize>,
    message: String,
) {
    let diagnostic = located_diagnostic(source, bytes, ConfigDiagnosticSeverity::Warning, message);
    analysis.retired_spellings.push(diagnostic.clone());
    let _ = analysis.insert_diagnostic(diagnostic);
}

fn push_unaccepted_value(
    analysis: &mut ConfigAnalysis,
    source: &str,
    bytes: Range<usize>,
    severity: ConfigDiagnosticSeverity,
    message: String,
) {
    let diagnostic = located_diagnostic(source, bytes, severity, message);
    if !analysis.unaccepted_values.contains(&diagnostic) {
        analysis.unaccepted_values.push(diagnostic.clone());
    }
    let _ = analysis.insert_diagnostic(diagnostic);
}

fn located_diagnostic(
    source: &str,
    bytes: Range<usize>,
    severity: ConfigDiagnosticSeverity,
    message: String,
) -> ConfigDiagnostic {
    let start = bytes.start.min(source.len());
    let end = bytes.end.min(source.len()).max(start);
    let start = floor_char_boundary(source, start);
    let end = floor_char_boundary(source, end).max(start);
    let before = &source[..start];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = before
        .rsplit_once('\n')
        .map_or(before, |(_, tail)| tail)
        .chars()
        .count()
        + 1;
    ConfigDiagnostic {
        bytes: start..end,
        line,
        column,
        severity,
        message,
    }
}

#[cfg(test)]
pub(crate) fn assist(source: &str, caret: usize) -> ConfigAssist {
    let mut index = build_assist_index(source);
    index.disk = unindexed_disk_faults(&aterm_agent::harness::cli::disk_faults(source));
    assist_indexed(source, caret, &index)
}

/// Production completion path.
pub(crate) fn assist_with_analysis(
    source: &str,
    caret: usize,
    analysis: &ConfigAnalysis,
) -> ConfigAssist {
    assist_indexed(source, caret, &analysis.assist_index)
}

fn assist_indexed(source: &str, caret: usize, index: &ConfigAssistIndex) -> ConfigAssist {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return ConfigAssist::default();
    }
    let caret = floor_char_boundary(source, caret.min(source.len()));
    let line_start = source[..caret]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    if caret.saturating_sub(line_start) > MAX_CONTEXT_LINE_BYTES {
        return ConfigAssist::default();
    }
    let line_end = source[caret..]
        .find('\n')
        .map_or(source.len(), |position| caret + position);
    if line_end.saturating_sub(line_start) > MAX_CONTEXT_LINE_BYTES {
        return ConfigAssist::default();
    }
    let line = &source[line_start..line_end];
    let caret_in_line = caret - line_start;
    let Some(lexical) = indexed_assist_lexical_context(index, line_start, &line[..caret_in_line])
    else {
        return ConfigAssist::default();
    };
    if lexical.blocked {
        return ConfigAssist::default();
    }
    let code_end = find_unquoted(line, b'#').unwrap_or(line.len());
    if caret_in_line > code_end {
        return ConfigAssist::default();
    }
    let code = &line[..code_end];
    if code.trim_start().starts_with('[') {
        return table_assist(
            source,
            line_start,
            code,
            caret_in_line,
            &lexical.table,
            lexical.scope,
            index,
        );
    }
    let table = lexical.table;
    let before_caret = &code[..caret_in_line.min(code.len())];
    if let Some(equal) = find_unquoted(before_caret, b'=') {
        value_assist(
            source,
            line_start,
            code,
            equal,
            caret_in_line,
            &table,
            index,
        )
    } else {
        key_assist(
            source,
            line_start,
            code,
            caret_in_line,
            &table,
            lexical.scope,
            index,
        )
    }
}

fn authored_path_elsewhere(
    index: &ConfigAssistIndex,
    path: &str,
    current_key: &Range<usize>,
    scope: u32,
) -> bool {
    let Some(authored) = index.authored.get(path) else {
        return false;
    };
    let occurrences = if scope == 0 {
        Some(&authored.all)
    } else {
        authored.scopes.get(&scope)
    };
    occurrences
        .is_some_and(|occurrences| occurrences.count > 1 || occurrences.first_key != *current_key)
}

fn record_authored_path(
    index: &mut ConfigAssistIndex,
    path: String,
    key: Range<usize>,
    scope: u32,
) {
    let authored = index.authored.entry(path).or_default();
    authored.all.record(&key);
    authored.scopes.entry(scope).or_default().record(&key);
}

impl AuthoredOccurrences {
    fn record(&mut self, key: &Range<usize>) {
        if self.count == 0 {
            self.first_key = key.clone();
        }
        self.count = self.count.saturating_add(1);
    }
}

fn table_assist(
    source: &str,
    line_start: usize,
    code: &str,
    caret_in_line: usize,
    current_table: &str,
    scope: u32,
    index: &ConfigAssistIndex,
) -> ConfigAssist {
    let leading = code.len().saturating_sub(code.trim_start().len());
    let token_end = code.trim_end().len().max(leading);
    let token = &code[leading..token_end];
    let prefix_end = caret_in_line.clamp(leading, token_end);
    let prefix_token = &code[leading..prefix_end];
    let wants_array_table = prefix_token.trim_start().starts_with("[[");
    let prefix = prefix_token
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_ascii_lowercase();
    let replacement = line_start + leading..line_start + token_end;
    let current_array_root = (scope != 0)
        .then(|| array_table_root(current_table))
        .flatten();
    let mut scratch = String::new();
    let mut matches = config_schema()
        .iter()
        .filter(|entry| !is_compatibility_only_key(entry.key))
        .filter(|entry| entry.kind.is_table_header())
        .filter(|entry| !wants_array_table || entry.kind.is_array_table())
        .filter(|entry| {
            if entry.kind.is_array_table() {
                return true;
            }
            let candidate_scope = if current_array_root.is_some()
                && current_array_root == array_table_root(entry.key)
            {
                scope
            } else {
                0
            };
            !authored_path_elsewhere(index, entry.key, &replacement, candidate_scope)
        })
        .filter(|entry| {
            prefix.is_empty()
                || entry.key.starts_with(prefix.as_str())
                || contains_ascii_folded(entry.label, &prefix, &mut scratch)
                || entry
                    .keywords
                    .iter()
                    .any(|keyword| keyword.starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|entry| {
        (
            !entry.key.starts_with(prefix.as_str()),
            entry.key.len(),
            entry.key,
        )
    });
    let normalized_token = token.trim_matches(['[', ']']).trim();
    let exact = matches
        .iter()
        .find(|entry| entry.key.eq_ignore_ascii_case(normalized_token))
        .map(|entry| setting_help(entry));
    let completions = matches
        .into_iter()
        .take(MAX_COMPLETIONS)
        .map(|entry| {
            let insertion = if entry.kind.is_array_table() {
                format!("[[{}]]", entry.key)
            } else {
                format!("[{}]", entry.key)
            };
            let caret = insertion.len();
            ConfigCompletionEdit {
                replacement: replacement.clone(),
                expected: source[replacement.clone()].to_string(),
                display: format!("{insertion} — {}", entry.label),
                insertion,
                post_insert_selection: caret..caret,
                help: setting_help(entry),
            }
        })
        .collect::<Vec<_>>();
    let typed = !prefix.is_empty() && exact.is_none() && !completions.is_empty();
    ConfigAssist {
        resting_help: exact.clone(),
        help: exact.or_else(|| {
            completions
                .first()
                .map(|completion| completion.help.clone())
        }),
        completions,
        typed,
    }
}

fn key_assist(
    source: &str,
    line_start: usize,
    code: &str,
    caret_in_line: usize,
    table: &str,
    scope: u32,
    index: &ConfigAssistIndex,
) -> ConfigAssist {
    let key_region_end = find_unquoted(code, b'=').unwrap_or(code.len());
    let key_region = &code[..key_region_end];
    let leading = key_region
        .len()
        .saturating_sub(key_region.trim_start().len());
    let token_end = key_region.trim_end().len().max(leading);
    let replacement = line_start + leading..line_start + token_end;
    let prefix_end = caret_in_line.clamp(leading, token_end);
    let prefix = code[leading..prefix_end].trim_end();
    let token = &code[leading..token_end];
    let normalized = prefix.to_ascii_lowercase();
    let assignment_exists = key_region_end < code.len();
    let mut scratch = String::new();
    let mut matches = config_schema()
        .iter()
        .filter(|setting| !is_compatibility_only_key(setting.key))
        .filter(|setting| setting.kind.is_assignable())
        .filter_map(|setting| {
            let local = local_key(setting.key, table)?;
            if authored_path_elsewhere(index, setting.key, &replacement, scope) {
                return None;
            }
            (normalized.is_empty()
                || local.starts_with(normalized.as_str())
                || contains_ascii_folded(setting.label, &normalized, &mut scratch)
                || setting
                    .keywords
                    .iter()
                    .any(|keyword| keyword.starts_with(&normalized)))
            .then_some((setting, local))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(setting, local)| {
        (
            !local.starts_with(normalized.as_str()),
            local.len(),
            setting.key,
        )
    });
    let exact = matches
        .iter()
        .find(|(_, local)| local.eq_ignore_ascii_case(token))
        .map(|(setting, _)| setting_help(setting));
    let completions = matches
        .into_iter()
        .take(MAX_COMPLETIONS)
        .map(|(setting, local)| {
            let (insertion, post_insert_selection) = if assignment_exists {
                let insertion = local.to_string();
                let caret = insertion.len();
                (insertion, caret..caret)
            } else {
                let value = example_value(setting);
                let prefix = format!("{local} = ");
                let selection = editable_value_selection(&value);
                let value_start = prefix.len();
                (
                    format!("{prefix}{value}"),
                    value_start + selection.start..value_start + selection.end,
                )
            };
            ConfigCompletionEdit {
                replacement: replacement.clone(),
                expected: source[replacement.clone()].to_string(),
                insertion,
                post_insert_selection,
                display: format!("{local} — {}", setting.label),
                help: setting_help(setting),
            }
        })
        .collect::<Vec<_>>();
    let typed = !normalized.is_empty() && exact.is_none() && !completions.is_empty();
    let resting_help = exact.clone().or_else(|| table_context_help(table, index));
    ConfigAssist {
        help: exact
            .or_else(|| {
                completions
                    .first()
                    .map(|completion| completion.help.clone())
            })
            .or_else(|| table_context_help(table, index)),
        completions,
        typed,
        resting_help,
    }
}

fn value_assist(
    source: &str,
    line_start: usize,
    code: &str,
    equal: usize,
    caret_in_line: usize,
    table: &str,
    index: &ConfigAssistIndex,
) -> ConfigAssist {
    let Some(local) = canonical_key_expression(&code[..equal]) else {
        return ConfigAssist::default();
    };
    let full_key = join_key_expressions(table, &local);
    if is_compatibility_only_key(&full_key) {
        let help = compatibility_only_help(&full_key, index);
        return ConfigAssist {
            resting_help: help.clone(),
            help,
            completions: Vec::new(),
            typed: false,
        };
    }
    let Some(setting) = config_schema_entry(&full_key) else {
        let help = table_context_help(table, index);
        return ConfigAssist {
            resting_help: help.clone(),
            help,
            completions: Vec::new(),
            typed: false,
        };
    };
    let value_start = equal
        + 1
        + code[equal + 1..]
            .len()
            .saturating_sub(code[equal + 1..].trim_start().len());
    let value_end = code.trim_end().len().max(value_start);
    let replacement = line_start + value_start..line_start + value_end;
    let literals = match setting.kind {
        ConfigSchemaKind::Scalar(EditKind::Bool) => {
            vec!["true".to_string(), "false".to_string()]
        }
        ConfigSchemaKind::Scalar(EditKind::Enum { options }) => options
            .iter()
            .map(|option| format!("\"{option}\""))
            .collect(),
        ConfigSchemaKind::Scalar(EditKind::Theme) => theme_value_literals(index),
        ConfigSchemaKind::StringList | ConfigSchemaKind::TextOrStringList => {
            vec!["[]".to_string()]
        }
        ConfigSchemaKind::Scalar(
            EditKind::Float | EditKind::Integer | EditKind::Text | EditKind::Color,
        )
        | ConfigSchemaKind::DynamicStringMap
        | ConfigSchemaKind::StructuredList
        | ConfigSchemaKind::Table
        | ConfigSchemaKind::Flexible => Vec::new(),
    };
    let prefix_end = caret_in_line.clamp(value_start, value_end);
    let normalized = code[value_start..prefix_end].trim().to_ascii_lowercase();
    let whole_value = code[value_start..value_end].trim();
    let finished = literals
        .iter()
        .any(|literal| literal.eq_ignore_ascii_case(whole_value));
    let completions = literals
        .into_iter()
        .filter(|literal| {
            normalized.is_empty() || literal.to_ascii_lowercase().starts_with(&normalized)
        })
        .take(MAX_COMPLETIONS)
        .map(|literal| {
            let display = if full_key == "sparkle_words.feline.style" && literal == "\"paw\"" {
                "\"paw\" — legacy ink-only; no paw graphic".to_string()
            } else if full_key == "theme" && literal.starts_with("\"dark:") {
                format!("{literal} — follow the system's dark or light appearance")
            } else {
                literal.clone()
            };
            ConfigCompletionEdit {
                replacement: replacement.clone(),
                expected: source[replacement.clone()].to_string(),
                post_insert_selection: editable_value_selection(&literal),
                insertion: literal,
                display,
                help: setting_help(setting),
            }
        })
        .collect::<Vec<_>>();
    ConfigAssist {
        typed: !normalized.is_empty() && !finished && !completions.is_empty(),
        help: Some(setting_help(setting)),
        resting_help: Some(setting_help(setting)),
        completions,
    }
}

fn theme_value_literals(index: &ConfigAssistIndex) -> Vec<String> {
    let builtins = aterm_types::scheme::builtin_names();
    let dark = builtins.first().copied().unwrap_or("Default");
    let light = aterm_types::scheme::builtin_pair(dark).unwrap_or(dark);
    let mut literals: Vec<String> = builtins
        .iter()
        .map(|name| (*name).to_string())
        .chain(index.theme_names.iter().cloned())
        .map(|name| format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    literals.insert(
        literals.len().min(1),
        format!("\"dark:{dark},light:{light}\""),
    );
    literals
}

fn table_context_help(table: &str, index: &ConfigAssistIndex) -> Option<String> {
    if is_compatibility_only_key(table) {
        return compatibility_only_help(table, index);
    }
    (!table.is_empty())
        .then(|| config_schema_entry(table))
        .flatten()
        .map(setting_help)
}

fn local_key<'a>(key: &'a str, table: &str) -> Option<&'a str> {
    if table.is_empty() {
        return (!key.contains('.')).then_some(key);
    }
    let local = key.strip_prefix(table)?.strip_prefix('.')?;
    (!local.contains('.')).then_some(local)
}

/// The list of records `key` is a member of (`sparkle_words.custom` for `sparkle_words.custom.graphic.collection`), or `None` for a key that is not inside one.
pub(crate) fn config_key_record_root(key: &str) -> Option<&'static str> {
    array_table_root(key).filter(|root| *root != key)
}

fn array_table_root(path: &str) -> Option<&'static str> {
    config_schema()
        .iter()
        .filter(|entry| entry.kind.is_array_table())
        .filter(|entry| {
            path == entry.key
                || path
                    .strip_prefix(entry.key)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
        .max_by_key(|entry| entry.key.len())
        .map(|entry| entry.key)
}

/// Manual's help for `key`, as its hover and completion show it.
#[cfg(test)]
pub(crate) fn setting_help_for(key: &str) -> Option<String> {
    config_schema_entry(key).map(setting_help)
}

fn setting_help(setting: &ConfigSchemaEntry) -> String {
    let kind = if setting.key == "sparkle_words.feline.style" {
        "\"cat\" renders the cat graphic; \"paw\" is legacy ink-only and renders no paw graphic"
            .to_string()
    } else if setting.key == "packages.account" {
        "GitHub owner slug (letters, digits, dot, underscore, or hyphen); a development build only"
            .to_string()
    } else if setting.key == "packages.prefix" {
        "absolute or ~/ store path; a development build only (a shipped build installs nothing \
         automatically while this names another store)"
            .to_string()
    } else if setting.key == "packages.links" {
        "named absolute/~/ checkout paths (dev links), or owner/repo slugs (a development build only)"
            .to_string()
    } else if setting.key == "harness.usage_notes" {
        "true / false; with no accounts listed, the one Claude Code account's usage credits and \
         its own warnings that a window fills each get a digest line"
            .to_string()
    } else if setting.key == crate::prefs::EDIT_CURSOR_TRAIL_COLOR {
        format!(
            "RRGGBB, #RRGGBB or #RGB, or {} for the cursor's color",
            crate::prefs::TRAIL_COLOR_CURSOR
        )
    } else if setting.key == "harness.trust_roots" {
        "absolute or ~/ folder paths, a trailing * any suffix of the last name; read only under \
         approve = \"safe\", which answers Claude Code's trust dialog in these folders alone \
         (and a Claude Code Read in them too; aterm answers no Codex box); none by \
         default, so under approve = \"safe\" every trust dialog goes to a person until you list \
         your own"
            .to_string()
    } else {
        match setting.kind {
            ConfigSchemaKind::Scalar(EditKind::Float) => "number".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Integer) => "integer".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Bool) => "true / false".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Text) => "text".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Theme) => "color theme".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Color) => "RRGGBB, #RRGGBB or #RGB".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Enum { options }) => options.join(" / "),
            ConfigSchemaKind::StringList => "list of text values".to_string(),
            ConfigSchemaKind::TextOrStringList => "text or list of text values".to_string(),
            ConfigSchemaKind::DynamicStringMap => "table of named text values".to_string(),
            ConfigSchemaKind::StructuredList => "array of tables".to_string(),
            ConfigSchemaKind::Table => "table".to_string(),
            ConfigSchemaKind::Flexible => "text or inline table".to_string(),
        }
    };
    let unit = crate::prefs::numeric_unit(setting.key);
    let range = semantic_numeric_bounds(setting.key)
        .map(|(min, max)| format!(" · range {min}–{max}{}", unit.map_or("", |unit| unit.many)));
    let placeholder = setting
        .placeholder
        .trim()
        .strip_suffix(" (default)")
        .unwrap_or(setting.placeholder.trim())
        .trim();
    let default = if placeholder.is_empty() || placeholder.eq_ignore_ascii_case("default") {
        String::new()
    } else if placeholder
        .split_whitespace()
        .any(|word| word.eq_ignore_ascii_case("default"))
    {
        format!(" · {placeholder}")
    } else if let Ok(value) = placeholder.parse::<f64>() {
        format!(
            " · default {placeholder}{}",
            unit.map_or("", |unit| unit.after(value))
        )
    } else {
        format!(" · default {placeholder}")
    };
    let timing = if setting.key == crate::prefs::EDIT_MACHINE_UNIVERSAL_CONTROL {
        format!(
            " · off: {}; with Keep ALab tools up to date off, {}; leave: when saved",
            crate::prefs::ApplicationTiming::NextToolsCheck.words(),
            crate::prefs::ApplicationTiming::DailyLaunchApply.clause()
        )
    } else {
        crate::prefs::application_timing(setting.key)
            .map(|timing| format!(" · {}", timing.words()))
            .unwrap_or_default()
    };
    let environment = crate::prefs::environment_precedence(setting.key)
        .map(|note| format!(" · {note}"))
        .unwrap_or_default();
    let constraint = match setting.key {
        crate::prefs::EDIT_MINIMUM_CONTRAST => {
            " · text on a see-through background keeps 4.5:1 contrast"
        }
        crate::prefs::EDIT_BACKGROUND_OPACITY => {
            " · macOS GPU rendering only · text keeps 4.5:1 contrast"
        }
        crate::prefs::EDIT_BACKGROUND_MATERIAL => {
            " · GPU renderer only · macOS requires background_opacity < 1; Windows maps to Mica / Mica Alt / Acrylic"
        }
        crate::prefs::EDIT_WINDOW_COLORSPACE => {
            " · SDR-only on the macOS GPU window layer; HDR/f16 surfaces force ExtendedLinearSrgb and ignore this tag"
        }
        crate::prefs::EDIT_FONT_PX => {
            " · an out-of-range/non-finite value is ignored, not clamped; resolution falls through to the next valid precedence source"
        }
        crate::prefs::EDIT_FONT_VARIATION => {
            " · each entry is tag=value with a 1–4 byte OpenType axis tag; malformed entries are ignored; a font named at one of its instances (SF Mono Light) ignores it"
        }
        crate::prefs::EDIT_FONT_WEIGHT_DARK_NUDGE => {
            " · a font named at one of its instances (SF Mono Light) keeps that weight in a dark theme"
        }
        crate::prefs::EDIT_FONT_WEIGHT => {
            " · applies only when the selected font provides a wght axis; static fonts, and a font named at one of its instances (SF Mono Light), ignore it"
        }
        crate::prefs::EDIT_FONT_FEATURES => {
            " · tokens are tag / +tag / -tag / tag=unsigned-value with 1–4 byte ASCII tags; malformed tokens are ignored"
        }
        crate::prefs::EDIT_FONT_THICKEN => {
            " · macOS CoreText only; parsed and preserved but inert on other platforms"
        }
        "privacy.auto_accept" => {
            " · RESERVED and refused: aterm never answers a macOS consent dialog, so no value of this key does anything; grant Full Disk Access instead (Settings \u{25b8} Security) · it does not answer macOS password dialogs either; for the debugger password, see Debugger access on the same page (Settings \u{25b8} Security)"
        }
        crate::prefs::EDIT_MOTION => crate::prefs::motion_auto_help(),
        crate::prefs::EDIT_SECURE_KEYBOARD_ENTRY => {
            " · macOS only: while aterm is in front, other apps cannot read your keystrokes, and their global hotkeys pause"
        }
        crate::prefs::EDIT_TRAIL_SOUNDS
        | crate::prefs::EDIT_TRAIL_SOUND_VOLUME
        | crate::prefs::EDIT_TONE_MELODY
        | crate::prefs::EDIT_TRAIL_SOUND_BED => {
            " · audio playback is macOS-only; parsed and preserved but inert on other platforms"
        }
        crate::prefs::EDIT_TRAIL_SOUND_RIFF => {
            " · quiets only the held-key sing-along song; its ribbon, star shower and dancing cat keep running · subordinate to trail_sounds and trail_sound_volume · audio playback is macOS-only"
        }
        crate::prefs::EDIT_BELL_SOUND => {
            " · the OS alert sound (macOS NSBeep / Windows MessageBeep / the X11 bell, which the window manager plays as the desktop's bell), so trail_sound_volume does not scale it · false keeps the visual bell flash and the urgent-window request · Wayland and other platforms emit no beep"
        }
        crate::prefs::EDIT_SPARKLE_BONK | crate::prefs::EDIT_SPARKLE_BONK_DETONATION => {
            " · scaled by trail_sound_volume and silent with the window unfocused, reduced motion, or the sparkle-words master off · audio playback is macOS-only"
        }
        crate::prefs::EDIT_CURSOR_TRAIL_BLOOM
        | crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH
        | crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS
        | crate::prefs::EDIT_CURSOR_FIRE_SHIMMER
        | crate::prefs::EDIT_HDR_GLOW
        | crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST => {
            " · GPU renderer only; parsed and preserved but inert while the CPU renderer is active"
        }
        crate::prefs::EDIT_CONFIRM_MULTILINE_PASTE => {
            " · asks before pasting text with a line break into a program that does not use bracketed paste"
        }
        crate::prefs::EDIT_PREDICTIVE_ECHO_EDITING => {
            " · true: a bare ←, → or end-of-line Backspace is shown before its echo too; in adaptive a Backspace once the line has shown it echoes, an arrow once the shell has also echoed an arrow exactly; always shows them at once · false: those keys wait for the echo · inert while predictive_echo is off"
        }
        crate::prefs::EDIT_OPTION_AS_META => {
            " · true: Option/Alt acts as Meta in shells and editors · false: it types your keyboard layout's characters (é, ü, …)"
        }
        crate::prefs::EDIT_ALLOW_NOTIFICATIONS => {
            " · desktop delivery is implemented on macOS and Windows; parsed but inert on other platforms"
        }
        crate::prefs::EDIT_DESKTOP_ALERTS => {
            " · aterm's OWN alerts (the menu bar's escalations, macOS permissions, update health) as system notifications — macOS, run from the app, only as aterm itself: once macOS allows it (turning Notify Me on asks, as does the first notice while this is on), none until then or once it says no, none from one in your Desktop, Documents or Downloads folder (where aterm does not read its name) until it moves to Applications, and none from one started through a link to it (such as ~/.local/bin/aterm, where aterm does not read its identity either) until aterm.app itself is opened; not run from the app, through osascript (shown as Script Editor); Windows a notification-area balloon (not update health); none on other platforms · off keeps them in the message band, the menu bar and messages.log · program notifications (OSC 9/99/777) are allow_notifications'"
        }
        crate::prefs::EDIT_ALLOW_OSC52_QUERY => {
            if cfg!(target_os = "linux") {
                " · the OSC 52 clipboard query · an authorized query reads back only the clipboard selections aterm itself owns (a foreign app's X11 selection is not readable today); rate/budget gates still apply"
            } else {
                " · the OSC 52 clipboard query · an authorized query reads the SYSTEM clipboard — including text copied in other apps — which is what remote vim/tmux clipboard sync needs; rate/budget gates still apply"
            }
        }
        crate::prefs::EDIT_ALLOW_WINDOW_OPS => {
            if cfg!(target_os = "linux") {
                " · XTWINOPS (CSI t) · manipulations (iconify, maximize, fullscreen, resize — move stays denied) apply to the window; the window-title, text-grid-size, text-area-pixels and cell-size reports are answered, while window/screen position and screen-size reports remain unanswered"
            } else {
                " · XTWINOPS (CSI t) · answers the window-title, text-grid-size, text-area-pixels and cell-size reports only; window changes (iconify, maximize, fullscreen, resize, move) and position and screen-size reports are ignored on this platform"
            }
        }
        crate::prefs::EDIT_ALLOW_PALETTE_RECONFIGURE => {
            " · OSC 4 and OSC 21 · the colors a program sets last for the tab's life and are never saved; OSC 104, RIS, Reset Terminal or turning this off put the theme's back · false refuses a program's change to one of the 256 indexed colors and files one notice; reading them, and the named foreground, background and cursor colors, are unaffected · minimum_contrast keeps any recolored text legible"
        }
        crate::prefs::EDIT_SEARCH_HISTORY_LINES => {
            " · Find searches only this many of the newest lines; 0 searches only the screen"
        }
        crate::prefs::EDIT_PACKAGES_ENABLED => {
            " · the one switch for the background package service (the retired auto_update is read as it), read live by the window (off stands it down within seconds, on resumes it); a published public index is picked up within minutes, and a full signed check runs every six hours across every aterm on the machine; explicit package commands and Check & Update Now remain available when the trust-root gate is open"
        }
        crate::prefs::EDIT_PACKAGES_AUTO_INSTALL => {
            " · the one install consent (the retired seed_install is read as it while this is unset): the first-run fill and new members of the signed set; uninstall, exclude and packages.enabled = false always win, and it cannot bypass the package trust-root gate"
        }
        crate::prefs::EDIT_UPDATE_ENABLED => {
            " · off stops the background checks only — Check for Updates, `aterm update check` and installing a downloaded build still run; read at launch · the native updater runs on macOS and Linux"
        }
        crate::prefs::EDIT_REROUTE_ANNOUNCE => {
            " · false silences the line a rerouted cargo/rustc/tlc/z3 (or a clippy/rustfmt/rustdoc/lean whose Trust tool is missing) prints before it runs upstream; the tool still runs, and a direct substitution still says what it ran · a refusal is not a line: inside a Trust source checkout a cargo/rustc that is not Trust is refused either way"
        }
        "reroute.strict" => {
            " · default false: every rerouted name warns in one line and runs — except a cargo/rustc that is not Trust inside a Trust source checkout, which is refused · true restores the refusals — z3 exits 2, a missing tippy/trustfmt/trustdoc/clean exits 127 instead of running upstream, a stub that cannot reach atpkg exits 2 · read at each run; a stub's own copy is re-laid at the next session spawn"
        }
        "update.owner" | "update.repo" => {
            " · a development build only; a shipped build reads its compiled channel · safe GitHub slug only; invalid/blank values fall through · the native updater runs on macOS and Linux"
        }
        "update.require_team_id" => {
            " · a development build only; a shipped build's compiled Team ID always wins"
        }
        "fabric.hold_on_bridge_lost" => " · off: a lost bridge posts an advisory and halts nothing",
        "harness.rm_breaker_ask" => {
            " · read only under approve = \"safe\", at Claude Code's rm circuit breaker · off: an rm the safe rules cannot prove is approved, and its reason kept as a warning"
        }
        "harness.probe_api" => {
            " · a resolve, a connect and a TLS handshake to api.anthropic.com, no request sent, so a wait at an unreachable API ends within about a minute of it being back · off: the wait follows its time ladder alone"
        }
        "harness.limit_wait" => {
            " · at Claude Code's usage-limit dialog, chooses its Wait here row, never a spending row or Stop · off: the dialog is yours"
        }
        "harness.autocompact_offer" => {
            " · at Claude Code's \"We recommend compacting earlier\" offer, chooses Autocompact at this size (300k to 900k), never the keep row or Esc · it changes Claude Code's own autocompact setting for that model (/autocompact switches back) · off: the offer is yours"
        }
        "harness.accounts" => {
            " · at a usage limit, a Claude Code session is relaunched under the next listed account; \"early\" also moves it ahead of the limit and types /usage to sample it; \"off\" moves no session · the one key for every CLI whose roster this machine keeps beside aterm.toml (accounts-codex.toml, accounts-grok.toml, accounts-kimi.toml): those pools are listed, pinned, synced and renewed (aterm harness accounts --cli <id>), and no session of theirs is moved in this build"
        }
        "harness.logins" => {
            " · a login listed with an email is signed in again by aterm itself when it ends or comes due, and a Claude Code session on it is relaunched onto the new login; aterm types no /login over it, and where nothing relaunches the tab (relaunch or resume_limits off, a restart it cannot make) the tab waits, its record naming why · a lost login listed nowhere is signed in where it stands: aterm types /login once and the browser sign-in is yours · off: aterm signs no listed login in by itself and no session follows a renewal, so a lost login gets /login once · in every CLI's roster: a window renews a due login of another CLI's roster in the background where a headless browser is installed and the row names an email (on macOS, Grok Build's and Codex's sign-in runs in a sandbox that keeps it from opening a browser on the desk, and not at all where that sandbox is missing), and off stops that too (aterm harness accounts renew --cli <id> by hand still runs)"
        }
        "harness.hosts" => {
            " · read by aterm harness accounts renew --due alone · each is host or host:/abs/path/to/aterm"
        }
        "harness.limit_reset" => {
            " · Claude Code only, through the usage-limit dialog's own reset row and its confirmation (keys only, no command typed) · only the one reset on offer, unless limit_reset_kinds names the kinds it may spend (which kinds: aterm harness accounts --help) · needs resume_limits · built, off by default, and never run against a real wall"
        }
        "harness.limit_reset_kinds" => {
            " · 5-hour, weekly or full, first preferred · the row of the first listed kind on offer is taken, whatever else is offered, never an unlisted kind or a Fable reset · empty: only the one reset on offer"
        }
        "harness.usage_tap" => {
            " · through Claude Code's own status line on the relaunch lines aterm types, for this machine's own login only (its default or lone one, or, where accounts.toml lists two or more accounts, one it lists or one a renewal moved a row away from); not where your own statusLine, apiKeyHelper, credential or --settings is set · its readings give the footer its weekly figure and the account stats their recorder, time the continuation at a usage limit to the reset they read, and, where accounts.toml lists two or more accounts, feed accounts = \"early\", which moves a session to the next account ahead of its limit · on by default"
        }
        "harness.usage_credits" => {
            " · Claude Code only · on: a consent box that names usage credits is taken (`Continue with <model>` is kept by Claude Code for good for that model and account; `Continue with usage credits` saves nothing and carries the session onto credits already on) and a model limit that asks for it is continued into it · off by default: the harness keeps sessions on the subscription; such a box is cancelled with Esc, a session credits carry gets no turn of the harness's own until the reset their notice names (then one turn, which another window may still bill to credits), and no session is moved onto a model billed to credits · nothing is ever bought either way"
        }
        "harness.model_fallback" => {
            " · Claude Code only, for that session alone · none: wait for the reset"
        }
        "harness.stall_term_after_s" => {
            " · with stall_term on, the agent is restarted instead · 0: never"
        }
        "harness.stall_term" => {
            " · restarted as aterm ctl restart does, for its relaunch, once it has stood stall_term_after_s"
        }
        "harness.memory_ceiling" => {
            " · auto, off or a number of GiB · the agent is restarted only when the Mac is short of memory and it holds the most"
        }
        "harness.facts" => {
            " · once each time a call stops making progress, on a prompt aterm types anyway, only when it fits whole · off: none"
        }
        "harness.stuck_break_note" => {
            " · off by default · on: one line of aterm's own at a break of the agent's background work, once each time a call stops making progress"
        }
        "harness.upgrade_ask" => {
            " · asks the agent to reach a stopping point and answer READY first · off: a session moves only when it is idle"
        }
        "harness.human_grace_s" => {
            " · the supervisor types nothing into that session for this long, short of an untouched box's auto-deny: that box is answered 15 s before its countdown runs out"
        }
        "disk.apply" => {
            " · the consent aterm harness disk --apply <class> needs before it removes anything"
        }
        "disk.target_stale_days" => {
            " · a cargo profile unbuilt this long counts as idle: its incremental cache goes first, whole (below the floor, recent ones follow, least recently used first; none built in the last 10 min, even at 0)"
        }
        "disk.auto_free_gib" => {
            " · left unwritten, the floor is 5% of the volume (at most 32 GiB) when that is more; a value written here is the floor · below it, cargo profiles' incremental caches are reclaimed automatically: idle ones, then least recently used first until 5 GiB above it, or until the bytes freed cover the shortfall"
        }
        "disk.dead_build_dirs" => {
            " · below the floor, a whole build directory whose owner is gone goes too: an ended Claude Code session's scratch build (no running Claude Code names the session, nothing of it written for target_stale_days, at least a day) or the cells cache of a removed checkout, or aterm's Rust build cache of a removed workspace (unwritten 10 min), after the idle caches and before any live checkout's · false: only its incremental caches"
        }
        "render.repaint_nudge" => {
            " · on: tell the tab its terminal is one row shorter, then the true size; log: only log what that would do; off: do nothing"
        }
        "claude_footer.usage" => {
            " · default true: the Claude Code footer shows `Σ` — Claude's working time and the session's token totals, all models merged (`Σ 7h52m ↑395M ↓2.0M`); false hides it, and a limit wall (`⧗`) shows either way · read live"
        }
        "claude_footer.acts" => {
            " · default true: right of the Autopilot chip, what the harness just did shows for 3 s in the chip's ink (`✓ approved`, `↻ kept going`, `? answered`, `⧗ resumed`, `⤓ landing`, `⤓ nudged`, `✗ declined`), at most once every 15 s, with no animation; false withholds it, and the chip's title still lists the last three acts with their times · serious mode withholds it too · read live"
        }
        "claude_footer.agents" => {
            " · default true: while the harness's theme is on a Claude Code session (its composer border drawn), the agents panel under the footer wears the theme's rainbow — each row's name in its stripe at Claude's own lightness, a workflow's bar along the whole palette — and each agent's dot tells its work: hue = which agent, brightness and glow = how much work, saturation = how fresh, full hue with a dim steady glow = in its own tool call (inside that call's timeout plus 30 s, read from its transcript), a grey breath = stuck (no work for 150 s with nothing of its own in flight), a still amber ring = awaiting approval, Claude's own green or red = done or failed — glass only; false leaves the panel as Claude drew it · serious mode keeps the colours and drops the glow · read live"
        }
        "claude_footer.card" => {
            " · default true: while the harness carries a Claude Code session (Autopilot cruise or land, its claim this window's), its launch card wears a rainbow border, a pixel-art kitty riding a rainbow with stars, and the line `✦ enhanced by aterm harness <harness version> · autopilot ∞ cruise`, and under Autopilot off the same card in white (`· autopilot ○ off`) — glass only; false leaves the card bare · read live"
        }
        "keeper.enabled" => {
            " · read at launch; on, every terminal is registered with this build's PTY keeper (`aterm keeper start` runs one) and a launch after a crash or kill reattaches the shells it held · macOS only"
        }
        "fleet.peers.host" => {
            " · host[:port] or [IPv6]:port, the port 7100 when none is written; listed and checked by `aterm fleet peers`"
        }
        "session_log.journal" => {
            " · on (the default): a record of what each session did under the logs folder's `session-log/`: each command's first word as typed (`detail=`, so a secret typed at a prompt and run is kept too) and the foreground program's name (`prog=`), outcomes and durations, agent turns and their tools' names, the session's folder (`cwd=`), its meta values (an attention text only as a fingerprint), mail's sender, recipient and offsets; never a command's arguments, an OSC title, a mail's body or an agent's transcript text · files 0600 in a 0700 folder, the oldest deleted first to keep its own near 64 MiB and 2048 files (files a running instance still writes can hold it above that), so `log @<sid>` and `brief @<sid>` answer for a session from an earlier run or a crash · off: aterm keeps no journal, creates no file and deletes nothing · read at launch; follows restore_session; a window's only · macOS and Linux"
        }
        "sys.stats_bars" => {
            " · default deck: a table under the titlebar with one row per resource, CPU, GPU, memory, disk, network, tokens and processes, plus the cores: each row's last 10 minutes as a heat strip (cold to hot, 0 to 100% of that resource's max, a hollow cell where nothing was read), its % now, a Global ⊇ aterm ⊇ Tab bar on its capacity and the amount with its unit; Folded, or Full where `stats_deck_size` says so · sill: thin lanes in the window's bottom padding, which takes no terminal row (needs 8 pt of padding) · off: no bars, and the terminal gets the Deck's rows back · rest the pointer on a row, or press ⌘I, for the close-up · View ▸ Stats Bars picks one · read live"
        }
        "sys.stats_deck_size" => {
            " · default auto: the Deck stays Folded, two blocks of four rows in 62 pt, however tall the window · full: always Full, a header line and a row per resource in about 100 pt, which takes whole terminal rows · folded: always Folded · decided when a window opens, resizes, changes display or the font changes, never by what the deck shows · View ▸ Deck Size picks one · read live"
        }
        "session_log.clause" => {
            " · on (the default): one more line after `activity:`, `now:` and what the session's log says the tab is doing (`now: running shell`, `now: approval · bash`, `now: limit · resets 14:05`; what needs you first), in the menu with how long it has held; the harness's own moments for 30 s with their author (`now: restarting · by the harness`); never a label another client wrote; the tab's name is unchanged · off: a tab's tooltip and context menu are as they were · read live"
        }
        "update.auto_apply" => {
            " · off, a downloaded build installs only when you ask — Update to Latest Now on macOS, `aterm update apply` on Linux · the native updater runs on macOS and Linux"
        }
        "sparkle_words.lexicon" => {
            " · loaded on the host; unreadable or rejected files are skipped while built-ins remain active"
        }
        "sparkle_words.profanity.palette" => {
            " · each color must be RRGGBB, #RRGGBB or #RGB; malformed entries are skipped"
        }
        "matrix_rain.hue" => " · matrix / theme / #RRGGBB; invalid values use stock matrix green",
        "sparkle_words.ink.sweep_ms" => " · loop = true raises the effective minimum to 600 ms",
        "matrix_rain.head_alpha" => " · effective minimum follows the resolved matrix_rain.alpha",
        crate::prefs::EDIT_WINDOW_PADDING_TOP => " · effective maximum follows window_padding",
        crate::prefs::EDIT_SCROLLBACK => " · 0 means unlimited scrollback",
        _ => "",
    };
    let facts = format!(
        "{kind}{default}{}{timing}{environment}{constraint}",
        range.unwrap_or_default()
    );
    format!("{} · {} · {facts}", setting.label, setting.key)
}

/// A key's DOCUMENTED default as a TOML literal.
pub(crate) fn config_key_default_literal(key: &str) -> Option<String> {
    let setting = config_schema_entry(key)?;
    let shown = setting.placeholder.trim();
    let shown = shown.strip_suffix(" (default)").unwrap_or(shown);
    if setting.kind == ConfigSchemaKind::Scalar(EditKind::Text) {
        return (!shown.is_empty()).then(|| aterm_toml::edit::Value::from(shown).to_string());
    }
    let token = shown.split(" (").next()?.trim();
    match setting.kind {
        ConfigSchemaKind::Scalar(EditKind::Bool) => {
            matches!(token, "true" | "false").then(|| token.to_string())
        }
        ConfigSchemaKind::Scalar(EditKind::Integer) => {
            token.parse::<i64>().ok().map(|value| value.to_string())
        }
        ConfigSchemaKind::Scalar(EditKind::Float) => token
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| crate::prefs::written_number(value).to_string()),
        ConfigSchemaKind::Scalar(EditKind::Enum { options }) => options
            .iter()
            .find(|option| option.eq_ignore_ascii_case(token))
            .map(|option| format!("\"{option}\"")),
        _ => None,
    }
}

/// The value completion would offer for `key` as a TOML literal (`"builtin"`, `true`, `[]`), for a key that takes one.
pub(crate) fn config_key_example_literal(key: &str) -> Option<String> {
    let setting = config_schema_entry(key)?;
    (setting.kind.is_assignable() && !matches!(setting.kind, ConfigSchemaKind::Flexible))
        .then(|| example_value(setting))
}

fn example_value(setting: &ConfigSchemaEntry) -> String {
    match setting.kind {
        ConfigSchemaKind::Scalar(EditKind::Bool) => "true".to_string(),
        ConfigSchemaKind::Scalar(EditKind::Enum { options }) => {
            format!("\"{}\"", options.first().copied().unwrap_or(""))
        }
        ConfigSchemaKind::Scalar(EditKind::Theme) => "\"Default\"".to_string(),
        ConfigSchemaKind::Scalar(EditKind::Color) => "\"#ffffff\"".to_string(),
        ConfigSchemaKind::Scalar(EditKind::Text) => "\"\"".to_string(),
        ConfigSchemaKind::Scalar(EditKind::Float | EditKind::Integer) => match setting.key {
            crate::prefs::EDIT_COLUMNS => "80".to_string(),
            crate::prefs::EDIT_LINES => "24".to_string(),
            _ => crate::prefs::range_of(setting.key)
                .map_or_else(|| "0".to_string(), |range| range.min.to_string()),
        },
        ConfigSchemaKind::StringList | ConfigSchemaKind::TextOrStringList => "[]".to_string(),
        ConfigSchemaKind::Flexible if setting.key == "sparkle_words.custom.ink" => {
            "\"rainbow\"".to_string()
        }
        ConfigSchemaKind::Flexible => "{}".to_string(),
        ConfigSchemaKind::DynamicStringMap
        | ConfigSchemaKind::StructuredList
        | ConfigSchemaKind::Table => "{}".to_string(),
    }
}

const fn editable_value_selection(value: &str) -> Range<usize> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && matches!(
            (bytes.first(), bytes.last()),
            (Some(b'"'), Some(b'"'))
                | (Some(b'\''), Some(b'\''))
                | (Some(b'['), Some(b']'))
                | (Some(b'{'), Some(b'}'))
        )
    {
        1..value.len() - 1
    } else {
        0..value.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultilineString {
    Basic,
    Literal,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TomlFragmentScan {
    multiline: Option<MultilineString>,
    comment_start: Option<usize>,
    touched_multiline: bool,
}

fn scan_toml_fragment(value: &str, mut multiline: Option<MultilineString>) -> TomlFragmentScan {
    let bytes = value.as_bytes();
    let mut index = 0usize;
    let mut single = None;
    let mut escaped = false;
    let mut touched_multiline = multiline.is_some();

    while index < bytes.len() {
        if let Some(active) = multiline {
            let delimiter = match active {
                MultilineString::Basic => b'"',
                MultilineString::Literal => b'\'',
            };
            if active == MultilineString::Basic && bytes[index] == b'\\' {
                index = (index + 2).min(bytes.len());
                continue;
            }
            if bytes[index..].starts_with(&[delimiter; 3]) {
                multiline = None;
                index += 3;
                continue;
            }
            index += 1;
            continue;
        }

        if let Some(active) = single {
            let byte = bytes[index];
            index += 1;
            if active == b'"' && byte == b'\\' && !escaped {
                escaped = true;
                continue;
            }
            if byte == active && !escaped {
                single = None;
            }
            escaped = false;
            continue;
        }

        if bytes[index] == b'#' {
            return TomlFragmentScan {
                multiline,
                comment_start: Some(index),
                touched_multiline,
            };
        }
        if bytes[index..].starts_with(b"\"\"\"") {
            multiline = Some(MultilineString::Basic);
            touched_multiline = true;
            index += 3;
            continue;
        }
        if bytes[index..].starts_with(b"'''") {
            multiline = Some(MultilineString::Literal);
            touched_multiline = true;
            index += 3;
            continue;
        }
        if matches!(bytes[index], b'\'' | b'"') {
            single = Some(bytes[index]);
            escaped = false;
        }
        index += 1;
    }

    TomlFragmentScan {
        multiline,
        comment_start: None,
        touched_multiline,
    }
}

fn table_header_identity(code: &str) -> Option<String> {
    let trimmed = code.trim();
    let name = trimmed
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
        .or_else(|| {
            trimmed
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
        })?;
    canonical_key_expression(name)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AssistLexicalContext {
    table: String,
    scope: u32,
    blocked: bool,
}

fn build_assist_index(source: &str) -> ConfigAssistIndex {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return ConfigAssistIndex::default();
    }
    let mut index = ConfigAssistIndex {
        lines: Vec::new(),
        tables: vec![String::new()],
        authored: HashMap::new(),
        disk: Vec::new(),
        theme_names: Vec::new(),
    };
    let mut table_ids = BTreeMap::from([(String::new(), 0_u32)]);
    let mut table = 0_u32;
    let mut scope = 0_u32;
    let mut next_scope = 1_u32;
    let mut multiline = None;
    let mut start = 0usize;
    for line_with_newline in source.split_inclusive('\n') {
        index.lines.push(AssistLineState {
            start,
            table,
            scope,
            multiline,
        });
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        if !started_in_multiline {
            let code = &line[..scan.comment_start.unwrap_or(line.len())];
            if !scan.touched_multiline
                && let Some(header) = table_header_identity(code)
            {
                let leading = code.len().saturating_sub(code.trim_start().len());
                let token_end = code.trim_end().len().max(leading);
                let array_table = code[leading..token_end].starts_with("[[");
                let current_table = index.tables.get(table as usize).map_or("", String::as_str);
                let current_array_root = (scope != 0)
                    .then(|| array_table_root(current_table))
                    .flatten();
                scope = if array_table {
                    let allocated = next_scope;
                    next_scope = next_scope
                        .checked_add(1)
                        .expect("bounded config array-table scope count fits u32");
                    allocated
                } else if current_array_root.is_some()
                    && current_array_root == array_table_root(&header)
                {
                    scope
                } else {
                    0
                };
                record_authored_path(
                    &mut index,
                    header.clone(),
                    start + leading..start + token_end,
                    scope,
                );
                table = if let Some(id) = table_ids.get(&header) {
                    *id
                } else {
                    let id = u32::try_from(index.tables.len())
                        .expect("bounded config table count fits u32");
                    index.tables.push(header.clone());
                    table_ids.insert(header, id);
                    id
                };
            } else if let Some(equal) = find_unquoted(code, b'=')
                && let Some(local) = canonical_key_expression(&code[..equal])
            {
                let leading = code[..equal]
                    .len()
                    .saturating_sub(code[..equal].trim_start().len());
                let token_end = code[..equal].trim_end().len().max(leading);
                let table_path = index.tables.get(table as usize).map_or("", String::as_str);
                let path = join_key_expressions(table_path, &local);
                record_authored_path(&mut index, path, start + leading..start + token_end, scope);
            }
        }
        multiline = scan.multiline;
        start = start.saturating_add(line_with_newline.len());
    }
    if index.lines.is_empty() || source.ends_with('\n') {
        index.lines.push(AssistLineState {
            start,
            table,
            scope,
            multiline,
        });
    }
    index
}

fn indexed_assist_lexical_context(
    index: &ConfigAssistIndex,
    line_start: usize,
    current_line_prefix: &str,
) -> Option<AssistLexicalContext> {
    let state = index
        .lines
        .binary_search_by_key(&line_start, |state| state.start)
        .ok()
        .and_then(|position| index.lines.get(position))?;
    let table = index.tables.get(state.table as usize)?.clone();
    let started_in_multiline = state.multiline.is_some();
    let scan = scan_toml_fragment(current_line_prefix, state.multiline);
    Some(AssistLexicalContext {
        table,
        scope: state.scope,
        blocked: started_in_multiline
            || scan.touched_multiline
            || scan.multiline.is_some()
            || scan.comment_start.is_some(),
    })
}

pub(crate) fn decorate_projection(
    projection: &mut EditorViewportProjection,
    analysis: &ConfigAnalysis,
) {
    let mut cursor = 0usize;
    for index in 0..projection.lines.len() {
        let window_start = projection.lines[index].source.start;
        debug_assert!(
            index == 0 || projection.lines[index - 1].source.end < window_start,
            "decorate_projection requires one projected window per source line"
        );
        while cursor < analysis.syntax.len() && analysis.syntax[cursor].bytes.end <= window_start {
            cursor += 1;
        }
        let scan_end = projection
            .lines
            .get(index + 1)
            .map_or(usize::MAX, |next| next.source.start);
        let line = &mut projection.lines[index];
        for span in &analysis.syntax[cursor..] {
            if span.bytes.start >= scan_end {
                break;
            }
            if let Some(bytes) = relative_intersection(&line.source, &span.bytes) {
                line.syntax.push(EditorSyntaxSpan {
                    bytes,
                    class: span.class,
                });
            }
        }
        for diagnostic in &analysis.diagnostics {
            if let Some(bytes) =
                diagnostic_intersection(&line.source, &diagnostic.bytes, &line.text)
            {
                line.diagnostics.push(EditorDiagnosticSpan {
                    bytes,
                    error: diagnostic.severity == ConfigDiagnosticSeverity::Error,
                });
            }
        }
    }
}

fn relative_intersection(line: &Range<usize>, span: &Range<usize>) -> Option<Range<usize>> {
    let start = line.start.max(span.start);
    let end = line.end.min(span.end);
    (start < end).then(|| start - line.start..end - line.start)
}

fn diagnostic_intersection(
    line: &Range<usize>,
    diagnostic: &Range<usize>,
    text: &str,
) -> Option<Range<usize>> {
    if let Some(range) = relative_intersection(line, diagnostic) {
        return Some(range);
    }
    if diagnostic.start < line.start || diagnostic.start > line.end {
        return None;
    }
    let mut start = diagnostic.start.saturating_sub(line.start).min(text.len());
    start = floor_char_boundary(text, start);
    if start == text.len() && start > 0 {
        start = text[..start]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
    }
    let end = text[start..]
        .chars()
        .next()
        .map_or(start, |character| start + character.len_utf8());
    Some(start..end)
}

fn lex_toml(source: &str) -> Vec<ConfigSyntaxSpan> {
    let limit = floor_char_boundary(source, source.len().min(MAX_CONFIG_ANALYSIS_BYTES));
    let mut spans = Vec::new();
    let mut line_start = 0usize;
    while line_start < limit && spans.len() < MAX_SYNTAX_SPANS {
        let remaining = &source[line_start..limit];
        let line_len = remaining.find('\n').unwrap_or(remaining.len());
        let line = &remaining[..line_len];
        lex_line(line, line_start, &mut spans);
        line_start = line_start.saturating_add(line_len).saturating_add(1);
    }
    spans.truncate(MAX_SYNTAX_SPANS);
    spans
}

fn lex_line(line: &str, base: usize, spans: &mut Vec<ConfigSyntaxSpan>) {
    let comment = find_unquoted(line, b'#');
    let code_end = comment.unwrap_or(line.len());
    if let Some(comment) = comment {
        push_syntax(
            spans,
            base + comment..base + line.len(),
            EditorSyntaxClass::Comment,
        );
    }
    let code = &line[..code_end];
    let leading = code.len().saturating_sub(code.trim_start().len());
    let trimmed = code[leading..].trim_end();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        push_syntax(
            spans,
            base + leading..base + leading + trimmed.len(),
            EditorSyntaxClass::Table,
        );
        return;
    }
    let Some(equal) = find_unquoted(code, b'=') else {
        return;
    };
    let key_start = leading;
    let key_end = code[..equal].trim_end().len();
    if key_start < key_end {
        push_syntax(
            spans,
            base + key_start..base + key_end,
            EditorSyntaxClass::Key,
        );
    }
    lex_value(&code[equal + 1..], base + equal + 1, spans);
}

fn lex_value(value: &str, base: usize, spans: &mut Vec<ConfigSyntaxSpan>) {
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && spans.len() < MAX_SYNTAX_SPANS {
        match bytes[index] {
            b'\'' | b'"' => {
                let quote = bytes[index];
                let start = index;
                index += 1;
                let mut escaped = false;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if quote == b'"' && byte == b'\\' && !escaped {
                        escaped = true;
                        continue;
                    }
                    if byte == quote && !escaped {
                        break;
                    }
                    escaped = false;
                }
                push_syntax(spans, base + start..base + index, EditorSyntaxClass::String);
            }
            byte if byte.is_ascii_digit() || matches!(byte, b'+' | b'-') => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric()
                        || matches!(bytes[index], b'_' | b'.' | b'+' | b'-' | b':'))
                {
                    index += 1;
                }
                push_syntax(spans, base + start..base + index, EditorSyntaxClass::Number);
            }
            byte if byte.is_ascii_alphabetic() => {
                let start = index;
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                    index += 1;
                }
                if matches!(&value[start..index], "true" | "false") {
                    push_syntax(
                        spans,
                        base + start..base + index,
                        EditorSyntaxClass::Boolean,
                    );
                }
            }
            _ => index += 1,
        }
    }
}

fn push_syntax(spans: &mut Vec<ConfigSyntaxSpan>, bytes: Range<usize>, class: EditorSyntaxClass) {
    if bytes.start < bytes.end && spans.len() < MAX_SYNTAX_SPANS {
        spans.push(ConfigSyntaxSpan { bytes, class });
    }
}

fn find_unquoted(value: &str, needle: u8) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if let Some(active) = quote {
            if active == b'"' && byte == b'\\' && !escaped {
                escaped = true;
                continue;
            }
            if byte == active && !escaped {
                quote = None;
            }
            escaped = false;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == needle {
            return Some(index);
        }
    }
    None
}

fn floor_char_boundary(value: &str, mut byte: usize) -> usize {
    byte = byte.min(value.len());
    while byte > 0 && !value.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn settings_set_names_a_hand_written_key_as_one() {
        for key in ["conpty", "windowing_behavior", "fleet.peers"] {
            let refusal = control_settings_refusal(key);
            assert_eq!(
                refusal,
                format!(
                    "{key} is set in aterm.toml by hand, not through Settings: edit the file or \
                     use Manual, which completes and checks it"
                )
            );
        }
        for (key, verb) in [
            ("fleet.allow", "`aterm fleet allow facts|content`"),
            (
                "fleet.drive_from",
                "`aterm fleet allow drive --from <host|node>`",
            ),
            ("fleet.listen", "`aterm fleet init` or `aterm fleet join`"),
        ] {
            let refusal = control_settings_refusal(key);
            assert!(
                refusal.starts_with(&format!("{key} is written by `aterm fleet`"))
                    && refusal.contains(verb)
                    && !refusal.contains("by hand"),
                "{refusal}"
            );
        }
        assert_eq!(
            control_settings_refusal("title_summary_provider"),
            compatibility_only_message("title_summary_provider")
        );
        assert_eq!(
            control_settings_refusal("not_a_real_setting"),
            "unknown key \"not_a_real_setting\" (search Settings or use Manual for the complete \
             schema)"
        );
    }

    #[test]
    fn settings_set_never_calls_a_key_a_reader_takes_unknown() {
        assert!(
            !MANUAL_SCHEMA
                .iter()
                .any(|entry| entry.key == "harness.occupancy"),
            "no Manual row names it"
        );
        assert_eq!(
            settings_refusal_of("harness.occupancy"),
            SettingsRefusal::HandWritten
        );
        for key in reader_accepted_paths() {
            assert_ne!(settings_refusal_of(key), SettingsRefusal::Unknown, "{key}");
        }
        assert_eq!(
            settings_refusal_of("not_a_real_setting"),
            SettingsRefusal::Unknown
        );
    }

    #[test]
    fn candidate_match_score_ranks_each_rung() {
        use MatchReach::{Loose, Words};
        let words = |candidate: &str, query: &str| candidate_match_score(candidate, query, Words);
        let loose = |candidate: &str, query: &str| candidate_match_score(candidate, query, Loose);
        assert_eq!(words("font size", "font"), Some(WHOLE_AT_START));
        assert_eq!(words("display font", "font"), Some(WHOLE_AT_WORD));
        assert_eq!(words("cursor_trail", "cur"), Some(PREFIX_AT_START));
        assert_eq!(words("enable_cursor_trail", "cur"), Some(PREFIX_AT_WORD));
        assert_eq!(words("nothing_here", "cur"), None);
        assert!(
            words("supervise claude code sessions", "code") < words("codex", "code"),
            "a whole word mid-name before a prefix at the start"
        );
        assert!(words("codex", "code") < words("watch codex-alab sessions", "code"));
        assert_eq!(words("codex code", "code"), Some(WHOLE_AT_WORD));
        assert_eq!(words("tabs", "tab"), Some(WHOLE_AT_START));
        assert_eq!(words("text boxes", "box"), Some(WHOLE_AT_WORD));
        assert_eq!(words("tables", "tab"), Some(PREFIX_AT_START));
        assert_eq!(words("bus", "bu"), Some(PREFIX_AT_START));
        assert_eq!(
            words("terminal bell sound", "bell sound"),
            Some(WHOLE_AT_WORD)
        );
        assert_eq!(
            words("cursor_trail_style", "trail_style"),
            Some(WHOLE_AT_WORD)
        );
        assert_eq!(words("cursor_trail_style", "_style"), Some(WHOLE_AT_WORD));
        assert_eq!(words("cursor_trail_style", "ail_style"), None);
        assert_eq!(words("thecursorthing", "cur"), None);
        assert_eq!(
            words("allow programs to show desktop notifications", "cat"),
            None
        );
        assert_eq!(words("indicator", "cat"), None);
        assert_eq!(words("feline", "line"), None);
        assert_eq!(words("line height", "lh"), None);
        assert_eq!(words("color and theme", "cat"), None);
        assert_eq!(loose("colour_under_rail", "cur"), Some(ACRONYM_RUNG));
        assert_eq!(loose("line height", "lh"), Some(ACRONYM_RUNG));
        assert_eq!(loose("color and theme", "cat"), Some(ACRONYM_RUNG));
        assert_eq!(loose("a a b", "aab"), Some(ACRONYM_RUNG));
        assert_eq!(loose("keyboard & input", "board"), Some(INSIDE_WORD_RUNG));
        assert_eq!(loose("wallpaper", "paper"), Some(INSIDE_WORD_RUNG));
        assert_eq!(loose("feline", "line"), Some(INSIDE_WORD_RUNG));
        assert_eq!(loose("enable_cursor_trail", "cur"), Some(PREFIX_AT_WORD));
        assert_eq!(loose("clear_tab", "cat"), None);
        assert_eq!(
            loose("show claude's working time and tokens (σ)", "cat"),
            None
        );
        assert_eq!(loose("colour_under_rail_link", "curl"), None);
        assert_eq!(loose("keyboard", "yb"), None);
        assert_eq!(loose("nothing_here", "zzz"), None);
        assert!(words("cursor", "cur") < words("my_cursor", "cur"));
        let model = |candidate: &str, query: &str| -> Option<u8> {
            if query.is_empty() {
                return Some(WHOLE_AT_START);
            }
            let bytes = candidate.as_bytes();
            let alnum = |byte: &u8| byte.is_ascii_alphanumeric();
            candidate
                .match_indices(query)
                .filter(|(i, _)| !alnum(&query.as_bytes()[0]) || *i == 0 || !alnum(&bytes[i - 1]))
                .map(|(i, _)| {
                    let end = i + query.len();
                    let rest: String = candidate[end..]
                        .chars()
                        .take_while(char::is_ascii_alphanumeric)
                        .collect();
                    let whole = !alnum(query.as_bytes().last().unwrap_or(&b'_'))
                        || rest.is_empty()
                        || (query.len() >= 3 && (rest == "s" || rest == "es"));
                    match (whole, i == 0) {
                        (true, true) => WHOLE_AT_START,
                        (true, false) => WHOLE_AT_WORD,
                        (false, true) => PREFIX_AT_START,
                        (false, false) => PREFIX_AT_WORD,
                    }
                })
                .min()
        };
        let candidates = [
            "",
            "_",
            "a",
            "cursor",
            "cursors",
            "enable_cursor_trail",
            "x-cur",
            "é_cur",
            "écur",
            "a1_2b",
            "über_font",
            "font.size",
            "cur_",
            "日本cur",
            "a  b",
            "trail__cur9",
            "terminal bell sound",
            "cur curl cur",
        ];
        let queries = [
            "",
            "c",
            "cur",
            "cur9",
            "1",
            "2b",
            "b",
            "_cur",
            "é",
            "cur_",
            "font.",
            "ü",
            "日",
            " b",
            "bell sound",
            "l sound",
            "cursor",
        ];
        for candidate in candidates.into_iter().chain(
            config_schema()
                .iter()
                .flat_map(|entry| std::iter::once(entry.key).chain(entry.keywords.iter().copied())),
        ) {
            for query in queries {
                assert_eq!(
                    words(candidate, query),
                    model(candidate, query),
                    "{candidate:?} / {query:?}"
                );
            }
        }
    }

    #[test]
    fn an_own_name_hit_ranks_before_a_synonym_hit_on_the_same_rung() {
        let rank = |own: &[&str], synonyms: &[&str], query: &str| {
            query_match_rank(
                own.iter().copied(),
                synonyms.iter().copied(),
                query,
                MatchReach::Words,
            )
        };
        let named = rank(&["supervise codex sessions", "codex"], &["agent"], "codex");
        let listed = rank(
            &["keep alab tools up to date"],
            &["codex", "claude"],
            "codex",
        );
        assert_eq!(named, Some(SearchRank::own(WHOLE_AT_START)));
        assert_eq!(listed, Some(SearchRank::synonym(WHOLE_AT_START)));
        assert!(named < listed);
        assert!(
            rank(&["x"], &["codex"], "codex") < rank(&["supervise codex sessions"], &[], "codex")
        );
        assert_eq!(
            rank(&["dark sky"], &["mode"], "dark mode"),
            Some(SearchRank::synonym(WHOLE_AT_START))
        );
        assert_eq!(rank(&["dark sky"], &["mode"], "sun"), None);
        assert!(
            rank(&["supervise claude code sessions"], &[], "claude code")
                < rank(
                    &["keep alab tools up to date"],
                    &["claude", "codex"],
                    "claude code"
                )
        );
    }

    #[test]
    fn query_match_score_takes_every_word() {
        let set = ["window appearance", "window_theme", "dark", "light", "mode"];
        let score = |query: &str| query_match_score(set.iter().copied(), query, MatchReach::Words);
        assert_eq!(score("dark mode"), Some(WHOLE_AT_START));
        assert_eq!(
            score("mode dark"),
            Some(WHOLE_AT_START),
            "word order does not matter"
        );
        assert_eq!(score("dark sky"), None, "every word must match");
        assert_eq!(
            score("the dark mode"),
            Some(WHOLE_AT_START),
            "filler words need not"
        );
        assert_eq!(score("appearance of the window"), Some(WHOLE_AT_WORD));
        assert_eq!(score("dark appear"), Some(PREFIX_AT_WORD));
        assert_eq!(score("dark ppear"), None);
        assert_eq!(score("window app"), Some(PREFIX_AT_START));
        let words = |set: &[&str], query: &str| {
            query_match_score(set.iter().copied(), query, MatchReach::Words)
        };
        let kitties = ["keyword kitties", "sparkle_words.feline.enabled"];
        assert_eq!(words(&kitties, "kitty"), Some(WHOLE_AT_WORD));
        assert_eq!(words(&["smart title"], "titles"), Some(WHOLE_AT_WORD));
        assert_eq!(words(&["copy and trim"], "cats"), None);
        assert_eq!(words(&["user"], "series"), None);
        assert_eq!(words(&["serial port"], "series"), None);
        assert_eq!(
            query_match_score(["keyboard & input"].into_iter(), "board", MatchReach::Loose),
            Some(INSIDE_WORD_RUNG)
        );
        assert_eq!(
            query_match_score(["keyboard & input"].into_iter(), "board", MatchReach::Words),
            None
        );
    }

    fn highlighted<'a>(
        source: &'a str,
        analysis: &ConfigAnalysis,
        class: EditorSyntaxClass,
    ) -> Vec<&'a str> {
        analysis
            .syntax
            .iter()
            .filter(|span| span.class == class)
            .map(|span| &source[span.bytes.clone()])
            .collect()
    }

    #[test]
    fn native_toml_lexer_projects_keys_values_tables_and_comments() {
        let source = "theme = \"Nord\" # terminal colors\n[sparkle_words]\nenabled = true\n";
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        assert_eq!(
            highlighted(source, &analysis, EditorSyntaxClass::Key),
            ["theme", "enabled"]
        );
        assert_eq!(
            highlighted(source, &analysis, EditorSyntaxClass::String),
            ["\"Nord\""]
        );
        assert_eq!(
            highlighted(source, &analysis, EditorSyntaxClass::Table),
            ["[sparkle_words]"]
        );
        assert_eq!(
            highlighted(source, &analysis, EditorSyntaxClass::Boolean),
            ["true"]
        );
        assert_eq!(
            highlighted(source, &analysis, EditorSyntaxClass::Comment),
            ["# terminal colors"]
        );
    }

    #[test]
    fn a_retired_packages_key_is_never_told_to_rename_onto_a_written_key() {
        let message = |source: &str| -> String {
            analyze(source)
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .find(|m| m.contains("auto_update") || m.contains("seed_install"))
                .unwrap_or_else(|| panic!("a retired-key diagnostic for {source:?}"))
        };
        let both = message("[packages]\nenabled = true\nauto_update = false\n");
        assert!(!both.contains("rename"), "{both}");
        assert!(
            both.contains("remove it and set `enabled = false`"),
            "{both}"
        );
        let alone = message("[packages]\nauto_update = false\n");
        assert!(alone.contains("rename it to `enabled`"), "{alone}");
        let seed_both = message("[packages]\nauto_install = true\nseed_install = false\n");
        assert!(!seed_both.contains("rename"), "{seed_both}");
        assert!(seed_both.contains("remove it"), "{seed_both}");
        let seed_alone = message("[packages]\nseed_install = false\n");
        assert!(
            seed_alone.contains("rename it to `packages.auto_install`"),
            "{seed_alone}"
        );
    }

    #[test]
    fn unterminated_parser_diagnostic_targets_the_authored_line_before_trailing_newline() {
        let source = "# Manual\nfont_px = [ \n";
        let analysis = analyze(source);
        let diagnostic = analysis.first_error().expect("syntax diagnostic");
        let authored_end = source.len() - 1;

        assert_eq!(diagnostic.bytes, authored_end..authored_end);
        assert_eq!((diagnostic.line, diagnostic.column), (2, 13));
        assert!(
            analysis
                .summary()
                .is_some_and(|summary| summary.contains("Ln 2, Col 13"))
        );
    }

    #[test]
    fn syntax_schema_and_metadata_errors_block_saves_but_unknown_keys_only_warn() {
        for blocking in [
            "theme = \"Nord\n",
            "font_px = \"large\"\n",
            "window_theme = \"sepia\"\n",
            "theme = \"dark:Dracula,sepia\"\n",
            "foreground = \"#abcd\"\n",
        ] {
            assert!(analyze(blocking).has_errors(), "{blocking:?}");
        }
        for color in [
            "foreground = \"#12abEF\"\n",
            "foreground = \"12abEF\"\n",
            "foreground = \"#abc\"\n",
        ] {
            assert!(!analyze(color).has_errors(), "{color:?}");
        }
        let custom_theme = analyze("theme = \"My Private Theme\"\n");
        assert!(!custom_theme.has_errors());
        assert!(custom_theme.diagnostics.is_empty());
        assert!(
            analyze("theme = \"dark:Dracula,light:GitHub Light\"\n")
                .diagnostics
                .is_empty()
        );
        assert!(
            analyze("theme = \"dark:My Dark Theme,light:My Light Theme\"\n")
                .diagnostics
                .is_empty()
        );

        let forward = analyze("future_setting = 1\n");
        assert!(!forward.has_errors());
        assert_eq!(
            forward.diagnostics[0].severity,
            ConfigDiagnosticSeverity::Warning
        );

        let compatibility = analyze("matrix_rain.materialize = true\n");
        assert!(!compatibility.has_errors());
        assert_eq!(
            compatibility.diagnostics[0].severity,
            ConfigDiagnosticSeverity::Warning
        );
    }

    #[test]
    fn retired_feline_controls_are_preserved_diagnosed_and_never_completed() {
        let source = r##"[sparkle_words.feline]
idle = false
gaze = false
color = "#112233"
intensity = 0.25
"##;
        aterm_toml::from_str::<crate::app_config::Config>(source)
            .expect("retired feline keys remain loadable for compatibility");

        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        let expected = [
            "sparkle_words.feline.idle",
            "sparkle_words.feline.gaze",
            "sparkle_words.feline.color",
            "sparkle_words.feline.intensity",
        ];
        assert_eq!(analysis.diagnostics.len(), expected.len());
        for key in expected {
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.starts_with(&format!("{key} "))),
                "missing compatibility diagnostic for {key}: {:?}",
                analysis.diagnostics
            );
        }

        let key_source = "[sparkle_words.feline]\nid";
        assert!(
            assist(key_source, key_source.len())
                .completions
                .iter()
                .all(|completion| !completion.insertion.starts_with("idle =")),
            "retired keys must not be offered as active configuration"
        );
        let value_source = "[sparkle_words.feline]\nidle = ";
        let assistance = assist(value_source, value_source.len());
        assert!(assistance.completions.is_empty());
        assert_eq!(
            assistance.help,
            Some(compatibility_only_message("sparkle_words.feline.idle"))
        );
    }

    #[test]
    fn the_repaint_nudge_words_are_the_engines() {
        use aterm_core::terminal::repaint_nudge::Mode;
        let read = |word: &str| -> Mode {
            let toml = format!("[render]\nrepaint_nudge = \"{word}\"\n");
            aterm_toml::from_str::<crate::app_config::Config>(&toml)
                .unwrap()
                .repaint_nudge_mode()
        };
        for &word in REPAINT_NUDGE_MODES {
            assert_eq!(read(word).word(), word, "the Manual offers {word:?}");
        }
        assert_eq!(
            read("whenever"),
            Mode::Log,
            "a word it cannot read never acts"
        );
    }

    #[test]
    fn a_reader_accepted_key_without_a_schema_row_is_never_unknown() {
        #[derive(serde::Deserialize)]
        #[expect(dead_code, reason = "only the reader's field list is read")]
        struct Reader {
            privacy: Option<ReaderPrivacy>,
        }
        #[derive(serde::Deserialize)]
        #[expect(dead_code, reason = "only the reader's field list is read")]
        struct ReaderPrivacy {
            no_schema_row: Option<bool>,
        }
        assert!(config_schema_entry("privacy.no_schema_row").is_none());
        let readers: BTreeSet<String> = reader_paths::<Reader>().into_iter().collect();
        let unknown = |source: &str| {
            let document = source.parse::<aterm_toml::edit::DocumentMut>().unwrap();
            let mut analysis = ConfigAnalysis::default();
            let walk = KeyWalk {
                source,
                document: &document,
                disk: &[],
                readers: &readers,
            };
            warn_unknown_values(walk, &mut analysis);
            analysis.diagnostics
        };
        let accepted = unknown("[privacy]\nno_schema_row = true\n");
        assert!(accepted.is_empty(), "{accepted:?}");
        let typo = unknown("[privacy]\nno_schema_rwo = true\n");
        assert!(
            typo.iter()
                .any(|diagnostic| diagnostic.message.contains("privacy.no_schema_rwo")),
            "{typo:?}"
        );
        for path in [
            "agents_auto_prime",
            "privacy.auto_accept",
            "harness.headless",
            "disk.auto_free_gib",
        ] {
            assert!(reader_accepted_paths().contains(path), "{path}");
        }
        assert!(!reader_accepted_paths().contains("privacy.no_schema_row"));
        let fabric = analyze(
            "[fabric]\ncommand = \"aterm-link serve\"\nreceipts = true\npresence = \"minimal\"\n",
        );
        assert!(fabric.diagnostics.is_empty(), "{:?}", fabric.diagnostics);
        let argv = analyze("[fabric]\ncommand = [\"/a b/aterm\", \"link\", \"serve\"]\n");
        assert!(argv.diagnostics.is_empty(), "{:?}", argv.diagnostics);
        let wrong = analyze("[fabric]\ncommand = 3\n");
        assert!(
            wrong
                .diagnostics
                .iter()
                .any(|d| d.message.contains(aterm_types::fabric_command::EXPECTED)),
            "{:?}",
            wrong.diagnostics
        );
    }

    #[test]
    fn the_operator_switch_is_a_key_a_removed_feature_left() {
        for (source, line) in [
            ("[operator]\nenabled = true\n", 2),
            ("operator.enabled = false\n", 1),
        ] {
            let keys = key_warnings(source);
            assert!(keys.ignored.is_empty(), "{source:?}: {keys:?}");
            assert_eq!(
                keys.retired,
                vec![format!(
                    "line {line}: {}",
                    compatibility_only_message("operator.enabled")
                )],
                "{source:?}"
            );
            assert!(
                keys.retired[0].contains("was removed"),
                "{source:?}: {keys:?}"
            );
        }
    }

    #[test]
    fn every_retired_top_level_key_says_it_was_removed_and_never_that_it_is_from_the_future() {
        let top_level: Vec<&str> = RETIRED_CONFIG_KEYS
            .iter()
            .map(|entry| entry.key)
            .filter(|key| !key.contains('.'))
            .collect();
        assert!(
            top_level.len() >= 8,
            "the sweep should cover every un-nested retired key: {top_level:?}"
        );
        for key in top_level {
            let source = format!("{key} = true\n");
            let analysis = analyze(&source);
            assert!(!analysis.has_errors(), "{key}: {:?}", analysis.diagnostics);
            let [diagnostic] = analysis.diagnostics.as_slice() else {
                panic!("{key} gets exactly one story: {:?}", analysis.diagnostics);
            };
            assert_eq!(diagnostic.severity, ConfigDiagnosticSeverity::Warning);
            let message = &diagnostic.message;
            assert_eq!(message, &compatibility_only_message(key));
            assert!(!message.contains("unknown"), "{key}: {message}");
            assert!(
                !message.contains("forward compatibility"),
                "{key}: {message}"
            );
            let value_source = format!("{key} = ");
            let value = assist(&value_source, value_source.len());
            assert!(
                value.completions.is_empty(),
                "{key}: retired values have no active runtime choices: {value:?}"
            );
            assert_eq!(
                value.help,
                Some(compatibility_only_message(key)),
                "{key}: the caret help tells the diagnostic's story"
            );
            let prefix = &key[..key.len() - 2];
            assert!(
                assist(prefix, prefix.len())
                    .completions
                    .iter()
                    .all(|completion| !completion.insertion.starts_with(key)),
                "{key}: a retired key is never completed"
            );
        }
    }

    #[test]
    fn host_analysis_resolves_assets_off_the_pure_language_path() {
        let source = concat!(
            "theme = \"Manual Theme That Cannot Exist 7F1C\"\n",
            "font_family_bold = \"Manual Font That Cannot Exist 7F1C\"\n",
            "cursor_nyan_sprite = \"/aterm/no-such-manual-nyan-7f1c.png\"\n",
        );
        let pure = analyze(source);
        assert!(
            pure.diagnostics.is_empty(),
            "custom theme names are neutral until the host resolves them"
        );
        let host = analyze_host(source, true);
        let missing = host
            .iter()
            .find(|diagnostic| {
                diagnostic.message.starts_with("theme:")
                    && diagnostic.message.contains("is unavailable")
            })
            .expect("host reports the loader's theme fallback")
            .clone();
        assert_eq!(missing.severity, ConfigDiagnosticSeverity::Warning);
        assert_eq!(
            &source[missing.bytes.clone()],
            "\"Manual Theme That Cannot Exist 7F1C\""
        );
        for expected in ["font_family_bold", "cursor_nyan_sprite"] {
            assert!(
                host.iter()
                    .any(|diagnostic| diagnostic.message.contains(expected)),
                "host diagnostics must mirror the runtime fallback for {expected}: {host:?}"
            );
        }

        let mut merged = pure;
        assert!(merged.merge_host_diagnostics(host.clone()));
        assert!(!merged.merge_host_diagnostics(host));
        assert_eq!(
            merged
                .diagnostics
                .iter()
                .filter(|diagnostic| *diagnostic == &missing)
                .count(),
            1,
            "replayed host completions cannot duplicate editor diagnostics"
        );
    }

    #[test]
    fn merged_lexicon_host_warnings_address_external_path_and_exact_custom_word() {
        let thread = std::thread::current();
        let root = std::env::temp_dir().join(format!(
            "aterm-manual-merged-lexicon-{}-{}",
            std::process::id(),
            thread.name().unwrap_or("test").replace(':', "-")
        ));
        std::fs::create_dir_all(&root).unwrap();
        let lexicon = root.join("conflict.toml");
        std::fs::write(
            &lexicon,
            "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"abc猫\"]\n",
        )
        .unwrap();
        let path_token = format!("{:?}", lexicon.to_string_lossy());
        let source = format!(
            "[sparkle_words]\nlexicon = {path_token}\n\
[sparkle_words.feline]\ncjk_single_char = false\n\
[[sparkle_words.custom]]\nwords = [\"犬\", \"mix猫\"]\nink = \"rainbow\"\n"
        );
        let host = analyze_host(&source, true);
        for (message, token) in [
            (
                "sparkle_words.lexicon merged-layer conflict",
                path_token.as_str(),
            ),
            ("record 1 word 1", "\"犬\""),
            ("record 1 word 2", "\"mix猫\""),
        ] {
            let diagnostic = host
                .iter()
                .find(|diagnostic| diagnostic.message.contains(message))
                .unwrap_or_else(|| panic!("missing {message:?}: {host:?}"));
            assert_eq!(&source[diagnostic.bytes.clone()], token, "{message}");
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manual_surfaces_the_canonical_keybinding_sequence_and_palette_warnings() {
        let source = r##"palette = ["#102030", "nope"]
[key_sequences]
"shift+entr" = "hi"
"ctrl+x" = '\q'
"cmd+c" = "shadow"
[keybindings]
"cmd+k" = "no_such_action"
"##;
        let analysis = analyze(source);
        assert!(
            !analysis.has_errors(),
            "warnings do not make TOML unsaveable"
        );
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
        );
        let mut expected_spans = vec![
            ("palette[1]", "\"nope\""),
            ("chord \"shift+entr\" invalid", "\"shift+entr\""),
            ("key_sequences[\"ctrl+x\"]: value invalid", "'\\q'"),
            ("unknown action \"no_such_action\"", "\"no_such_action\""),
        ];
        if crate::app_input::HARDCODED_SUPER_CHORDS {
            expected_spans.push(("chord \"cmd+c\" conflicts with built-in", "\"cmd+c\""));
        } else {
            assert!(
                !analysis
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("conflicts with built-in")),
                "no built-in conflict with the suite gated off: {:?}",
                analysis.diagnostics
            );
        }
        for (message, token) in expected_spans {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.contains(message))
                .unwrap_or_else(|| panic!("missing {message:?}: {:?}", analysis.diagnostics));
            assert_eq!(&source[diagnostic.bytes.clone()], token, "{message}");
        }
    }

    #[test]
    fn the_autocompact_offer_key_takes_its_readers_spellings() {
        for value in ["\"400k\"", "\"off\"", "false", "true", "500", "600000"] {
            let source = format!("[harness]\nautocompact_offer = {value}\n");
            let analysis = analyze(&source);
            assert!(
                !analysis.has_errors(),
                "{value}: {:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn every_registered_enum_accepts_its_runtime_domain_and_aliases() {
        let enums = config_schema()
            .iter()
            .filter(|setting| !setting.key.starts_with("sparkle_words.custom."))
            .filter_map(|setting| match setting.kind {
                ConfigSchemaKind::Scalar(EditKind::Enum { options }) => {
                    Some((setting.key, options))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let source_for = |key: &str, value: &str| format!("{key} = {value:?}\n");
        let admitted = |source: &str, what: &str| {
            let analysis = analyze(source);
            assert!(!analysis.has_errors(), "{what}: {:?}", analysis.diagnostics);
            let band = unaccepted_value_warnings(source, &[]);
            assert!(band.is_empty(), "{what}: {band:?}");
        };
        for (key, options) in &enums {
            for option in *options {
                admitted(
                    &source_for(key, option),
                    &format!("canonical {key}={option:?}"),
                );
            }
            let source = source_for(key, "definitely-not-a-real-enum-value");
            assert!(analyze(&source).has_errors(), "invalid {key} must fail");
        }

        for (key, aliases) in [
            (crate::prefs::EDIT_CURSOR_STYLE, CURSOR_STYLE_ALIASES),
            (crate::prefs::EDIT_BIDI, BIDI_ALIASES),
            (crate::prefs::EDIT_AMBIGUOUS_WIDTH, AMBIGUOUS_WIDTH_ALIASES),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, PREDICTIVE_ECHO_ALIASES),
            (crate::prefs::EDIT_TEXT_BLENDING, TEXT_BLENDING_ALIASES),
            (crate::prefs::EDIT_MOTION, MOTION_ALIASES),
            (
                crate::prefs::EDIT_WINDOW_COLORSPACE,
                WINDOW_COLORSPACE_ALIASES,
            ),
            (
                crate::prefs::EDIT_BACKGROUND_MATERIAL,
                BACKGROUND_MATERIAL_ALIASES,
            ),
            ("windowing_behavior", WINDOWING_BEHAVIOR_ALIASES),
            ("right_click", RIGHT_CLICK_ALIASES),
            ("tab_menu_chord", TAB_MENU_CHORD_ALIASES),
            (
                crate::prefs::EDIT_HARNESS_AUTOPILOT,
                HARNESS_AUTOPILOT_ALIASES,
            ),
        ] {
            for alias in aliases {
                admitted(&source_for(key, alias), &format!("alias {key}={alias:?}"));
            }
        }
        for &(alias, _) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            let source = source_for(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, alias);
            admitted(&source, &format!("trail alias {alias:?}"));
        }
        for &(alias, _) in aterm_effects::trail_sound::SoundVoice::ALIASES {
            let source = source_for(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
            admitted(&source, &format!("typing-sound alias {alias:?}"));
        }
    }

    #[test]
    fn machine_policy_domain_matches_atpkg_and_bad_types_remain_errors() {
        use atpkg::config::{MachineConfig, UniversalControlPolicy};
        for (value, policy) in [
            ("off", UniversalControlPolicy::Off),
            ("leave", UniversalControlPolicy::Leave),
            (" off ", UniversalControlPolicy::Off),
            (" leave ", UniversalControlPolicy::Leave),
            ("", UniversalControlPolicy::Off),
            ("  ", UniversalControlPolicy::Off),
        ] {
            let fields = format!("universal_control = {value:?}\n");
            let machine: MachineConfig = aterm_toml::from_str(&fields).unwrap();
            assert_eq!(machine.universal_control(), policy, "{value:?}");
            let source = format!("[machine]\n{fields}");
            assert!(analyze(&source).diagnostics.is_empty(), "{value:?}");
            assert!(unaccepted_value_warnings(&source, &[]).is_empty());
            assert!(ignored_key_warnings(&source).is_empty(), "{value:?}");
        }
        for value in ["OFF", "Leave", "leav"] {
            let fields = format!("universal_control = {value:?}\n");
            let machine: MachineConfig = aterm_toml::from_str(&fields).unwrap();
            assert_eq!(machine.universal_control(), UniversalControlPolicy::Leave);
            let source = format!("[machine]\n{fields}");
            assert!(analyze(&source).has_errors(), "unsupported {value:?}");
            let warnings = unaccepted_value_warnings(&source, &[]).join("\n");
            assert!(warnings.contains("machine.universal_control"), "{warnings}");
        }
        let fields = "universal_control = true\n";
        assert!(aterm_toml::from_str::<MachineConfig>(fields).is_err());
        let source = format!("[machine]\n{fields}");
        assert!(analyze(&source).has_errors());
        assert!(!crate::app_config::unaccepted_value_notices(&source, &[]).is_empty());
        assert!(
            crate::app_config::ignored_key_notices(&source).is_empty(),
            "{source:?}"
        );
        assert!(analyze("machine = false\n").has_errors());
        assert!(!ignored_key_warnings("machine = false\n").is_empty());
        for value in ["true", "false"] {
            let source = format!("[machine]\nrosetta = {value}\n");
            assert!(analyze(&source).diagnostics.is_empty(), "{value}");
            assert!(ignored_key_warnings(&source).is_empty(), "{value}");
            assert_eq!(
                atpkg::config::parse_machine(&source).rosetta(),
                value == "true"
            );
        }
        let source = "[machine]\nrosetta = \"yes\"\n";
        assert!(analyze(source).has_errors(), "{source:?}");
        assert!(!atpkg::config::parse_machine(source).rosetta());
    }

    #[test]
    fn retired_native_hinting_renders_as_light_and_is_never_offered() {
        assert!(!FONT_HINTINGS.contains(&"native"));
        let analysis = analyze("font_hinting = \"native\"\n");
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        let warnings = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("\"light\""), "{warnings:?}");
        assert!(unaccepted_enum_values("font_hinting = \"native\"\n").is_empty());
        assert!(analyze("font_hinting = \"bytecode\"\n").has_errors());
    }

    #[test]
    fn nested_unknown_scalars_warn_but_manual_maps_lists_and_custom_tables_do_not() {
        let typo =
            analyze("[sparkle_words]\nenabld = false\n[sparkle_words.feline]\ngzae = true\n");
        assert!(!typo.has_errors());
        let messages = typo
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        let named_as_unknown = |path: &str| {
            messages
                .iter()
                .any(|message| message.starts_with(path) && message.contains("no effect"))
        };
        assert!(named_as_unknown("sparkle_words.enabld"), "{messages:?}");
        assert!(
            named_as_unknown("sparkle_words.feline.gzae"),
            "{messages:?}"
        );

        let allowed = [
            r#"[keybindings]
"cmd+shift+k" = "new_tab"
[key_sequences]
"shift+enter" = '\e[13;2u'
"#,
            r#"[packages]
enabled = true
account = "alabsystems"
exclude = ["trust"]
[packages.links]
ay = "~/ay"
"#,
            r##"[sparkle_words]
languages = ["en"]
toy_packs = ["~/toy.toml"]
deny = ["plain"]
[sparkle_words.profanity]
palette = ["#ffffff"]
extra_words = ["zap"]
ignore_words = ["skip"]
[sparkle_words.feline]
extra_words = ["kit"]
ignore_words = ["skip"]
[sparkle_words.emphasis]
extra_words = ["wow"]
ignore_words = ["skip"]
"##,
            r#"[[sparkle_words.custom]]
words = ["ultrathink"]
ink = { colorway = "rainbow", sweep_once = true }
burst = { kind = "starburst", chance = 10 }
graphic = { collection = "cats" }
"#,
        ];
        for source in allowed {
            let analysis = analyze(source);
            assert!(
                analysis.diagnostics.is_empty(),
                "manual-native structure must be allowed: {:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn structured_record_typos_warn_per_record_without_blocking_or_hiding_repeats() {
        let source = r#"[[sparkle_words.custom]]
wrods = ["typo"]
ink = { colorway = "rainbow" }

[[sparkle_words.custom]]
words = ["valid"]
burst = { kind = "nova" }
"#;
        let analysis = analyze(source);
        assert!(
            !analysis.has_errors(),
            "unknown record members remain forward-compatible warnings: {:?}",
            analysis.diagnostics
        );
        let unknown = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.contains(" is unknown in [["))
            .collect::<Vec<_>>();
        assert_eq!(unknown.len(), 1, "{:?}", analysis.diagnostics);
        assert!(
            unknown[0].message.contains("sparkle_words.custom.wrods")
                && unknown[0].message.contains("record 1"),
            "{:?}",
            unknown[0].message
        );
        assert!(
            unknown
                .iter()
                .all(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
        );

        let clean_repeats = r##"[[sparkle_words.custom]]
words = ["one"]
ink = { colorway = "twotone:#112233,#445566" }

[[sparkle_words.custom]]
words = ["two"]
burst = { kind = "super-nova", chance = 10 }
graphic = { collection = "cats" }
"##;
        let clean = analyze(clean_repeats);
        assert!(
            clean.diagnostics.is_empty(),
            "valid repeated records stay clean: {:?}",
            clean.diagnostics
        );
    }

    #[test]
    fn runtime_fallback_diagnostics_address_their_authored_tokens() {
        type Case = (
            &'static str,
            bool,
            &'static [(&'static str, &'static str)],
            bool,
        );
        let cases: &[Case] = &[
            (
                r##"font_px = 201
font_variation = ["", "bad-axis"]
font_features = ["+ss01", "toolong"]

[update]
owner = "bad/owner"

[matrix_rain]
hue = "blue"

[sparkle_words.profanity]
palette = ["#112233", "bad-color"]

[[sparkle_words.custom]]
words = [" "]

[[sparkle_words.custom]]
words = ["quiet"]
burst = { kind = "glow", chance = 0 }
"##,
                false,
                &[
                    ("font_px: 201.0 is not accepted", "201"),
                    ("font_variation[0]", "\"bad-axis\""),
                    ("font_features[1]", "\"toolong\""),
                    ("update.owner: \"bad/owner\"", "\"bad/owner\""),
                    ("matrix_rain.hue value", "\"blue\""),
                    ("sparkle_words.profanity.palette[1]", "\"bad-color\""),
                    ("record 1 has no nonblank word", "[\" \"]"),
                    ("record 1 is inert", "[[sparkle_words.custom]]"),
                    ("record 2 is inert", "0"),
                ],
                false,
            ),
            (
                "sparkle_words.custom = [{ words = [\"quiet\"] }]\n",
                false,
                &[("record 1 is inert", "{ words = [\"quiet\"] }")],
                false,
            ),
            (
                "font_features = [\n  \"+ss01\",\n  \"toolong\",\n]\n",
                false,
                &[("font_features[1]", "\"toolong\"")],
                false,
            ),
            (
                "[[sparkle_words.custom]]\nink = { colorway = \"rainbow\" }\n",
                false,
                &[("has no nonblank word", "[[sparkle_words.custom]]")],
                false,
            ),
            (
                "[packages]\naccount = \"bad owner\"\n[packages.links]\nbad = \"a b\"\n",
                false,
                &[
                    ("packages.account", "\"bad owner\""),
                    ("packages.links.bad", "\"a b\""),
                ],
                false,
            ),
            (
                "[packages.links]\nay = 5\n",
                true,
                &[("packages.links.ay must be text", "5")],
                false,
            ),
            (
                "[[sparkle_words.custom]]\nwords = [\"zap\"]\n\
                 burst = { kind = \"starburst\", chance = 101 }\n",
                false,
                &[("sparkle_words.custom.burst.chance", "101")],
                false,
            ),
            (
                "[sparkle_words.ink]\nloop = true\nsweep_ms = 400\n\
                 [matrix_rain]\nalpha = 120\nhead_alpha = 20\n",
                false,
                &[("effectively 600 ms", "400"), ("effectively 120", "20")],
                true,
            ),
            (
                "allow_osc52_query = true\nallow_window_ops = true\n",
                false,
                &[("allow_osc52_query", "true"), ("allow_window_ops", "true")],
                true,
            ),
        ];
        for &(source, blocks, expected, exactly) in cases {
            let analysis = analyze(source);
            assert_eq!(
                analysis.has_errors(),
                blocks,
                "{source:?}: {:?}",
                analysis.diagnostics
            );
            if exactly {
                assert_eq!(
                    analysis.diagnostics.len(),
                    expected.len(),
                    "{source:?}: one story each: {:?}",
                    analysis.diagnostics
                );
            }
            for (fragment, token) in expected {
                let diagnostic = analysis
                    .diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.message.contains(fragment))
                    .unwrap_or_else(|| panic!("missing {fragment:?}: {:?}", analysis.diagnostics));
                assert_eq!(
                    &source[diagnostic.bytes.clone()],
                    *token,
                    "{}",
                    diagnostic.message
                );
            }
        }
        for clean in [
            "[packages.links]\nrepo = \"alabsystems/aterm\"\nhome = \"~/aterm\"\n",
            "[packages.links]\nay = \"~/ay\"\n",
            "[packages]\nlinks = { ay = \"~/ay\" }\n",
            "[[sparkle_words.custom]]\nwords = [\"zap\"]\n\
             burst = { kind = \"starburst\", chance = 100 }\n",
            "[sparkle_words.ink]\nloop = true\nsweep_ms = 600\n\
             [matrix_rain]\nalpha = 120\nhead_alpha = 120\n",
        ] {
            let analysis = analyze(clean);
            assert!(
                analysis.diagnostics.is_empty(),
                "{clean:?}: {:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn capability_warnings_are_backend_truthful_and_source_addressed() {
        use crate::diagnostics::{
            ConfigCapabilityPlatform as Platform, config_backend_capability_warnings as warnings,
        };
        let config_of =
            |source: &str| aterm_toml::from_str::<crate::app_config::Config>(source).unwrap();
        let backend = "background_opacity = 0.7\nbackground_material = \"hud\"\n\
window_colorspace = \"display-p3\"\n\
cursor_trail_bloom = false\n\
cursor_trail_bloom_strength = 1.25\n\
cursor_trail_bloom_radius = 3.5\n\
cursor_fire_shimmer = false\n\
hdr_glow = true\n\
cursor_glow_sdr_boost = 0.4\n";
        let platform = "trail_sounds = true\ntrail_sound_volume = 0.4\n\
confirm_multiline_paste = true\nallow_notifications = true\n";
        for (source, gpu, on, expected) in [
            (
                backend,
                false,
                Platform::MacOs,
                &[
                    (crate::prefs::EDIT_BACKGROUND_OPACITY, "0.7"),
                    (crate::prefs::EDIT_BACKGROUND_MATERIAL, "\"hud\""),
                    (crate::prefs::EDIT_WINDOW_COLORSPACE, "\"display-p3\""),
                    (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "false"),
                    (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH, "1.25"),
                    (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS, "3.5"),
                    (crate::prefs::EDIT_CURSOR_FIRE_SHIMMER, "false"),
                    (crate::prefs::EDIT_HDR_GLOW, "true"),
                    (crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST, "0.4"),
                ][..],
            ),
            (
                platform,
                true,
                Platform::Unsupported,
                &[
                    ("trail_sounds", "true"),
                    ("trail_sound_volume", "0.4"),
                    ("allow_notifications", "true"),
                ][..],
            ),
        ] {
            let document = source.parse::<aterm_toml::edit::DocumentMut>().unwrap();
            let mut analysis = ConfigAnalysis::default();
            append_semantic_warnings(
                source,
                &document,
                warnings(&config_of(source), gpu, on),
                &mut analysis,
            );
            assert_eq!(
                analysis.diagnostics.len(),
                expected.len(),
                "{:?}",
                analysis.diagnostics
            );
            for (key, token) in expected {
                let diagnostic = analysis
                    .diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.message.starts_with(key))
                    .unwrap_or_else(|| panic!("missing capability diagnostic for {key}"));
                assert_eq!(&source[diagnostic.bytes.clone()], *token, "{key}");
            }
        }
        assert!(
            warnings(&config_of(backend), true, Platform::MacOs).is_empty(),
            "every value has a consumer on a translucent macOS GPU window"
        );
        let windows = warnings(&config_of(backend), true, Platform::Windows);
        assert!(
            windows
                .iter()
                .any(|warning| warning.key == crate::prefs::EDIT_BACKGROUND_OPACITY),
            "Windows GPU must disclose that per-pixel grid opacity is inert"
        );
        assert!(
            windows
                .iter()
                .all(|warning| warning.key != crate::prefs::EDIT_BACKGROUND_MATERIAL),
            "Windows GPU consumes materials as DWM backdrops without translucency"
        );
        assert!(
            windows
                .iter()
                .any(|warning| { warning.key == crate::prefs::EDIT_WINDOW_COLORSPACE })
        );

        let opaque = warnings(
            &config_of("background_material = \"sidebar\"\n"),
            true,
            Platform::MacOs,
        );
        assert_eq!(opaque.len(), 1);
        assert_eq!(opaque[0].key, crate::prefs::EDIT_BACKGROUND_MATERIAL);
        assert!(
            warnings(
                &config_of("background_material = \"sidebar\"\n"),
                true,
                Platform::Windows,
            )
            .is_empty(),
            "Windows DWM material is independent of background opacity"
        );
        assert!(
            warnings(&config_of(backend), true, Platform::Unsupported)
                .iter()
                .any(|warning| warning.key == crate::prefs::EDIT_BACKGROUND_MATERIAL)
        );
    }

    #[test]
    fn custom_effect_fallback_values_warn_and_offer_runtime_canonical_completions() {
        let source = r#"[[sparkle_words.custom]]
words = ["one"]
ink = { colorway = "sepia" }
burst = { kind = "flashbang" }
graphic = { collection = "dogs" }

[[sparkle_words.custom]]
words = ["two"]
ink = { colorway = "twotone:#112233,#445566,#778899" }
"#;
        let analysis = analyze(source);
        assert!(!analysis.has_errors());
        for (path, fallback) in [
            ("sparkle_words.custom.ink.colorway", "rainbow"),
            ("sparkle_words.custom.burst.kind", "starburst"),
            ("sparkle_words.custom.graphic.collection", "cats"),
        ] {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.starts_with(path))
                .unwrap_or_else(|| {
                    panic!(
                        "missing fallback warning for {path}: {:?}",
                        analysis.diagnostics
                    )
                });
            assert_eq!(diagnostic.severity, ConfigDiagnosticSeverity::Warning);
            assert!(
                diagnostic.message.contains(fallback),
                "{}",
                diagnostic.message
            );
            assert!(
                diagnostic.message.contains("record 1"),
                "{}",
                diagnostic.message
            );
        }
        let extra_twotone = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .starts_with("sparkle_words.custom.ink.colorway")
                    && diagnostic.message.contains("record 2")
            })
            .expect("three-color twotone warning");
        assert_eq!(
            &source[extra_twotone.bytes.clone()],
            "\"twotone:#112233,#445566,#778899\""
        );

        for (source, expected) in [
            ("[sparkle_words.custom.ink]\ncolorway = \"", "\"rainbow\""),
            ("[sparkle_words.custom.burst]\nkind = \"n", "\"nova\""),
            (
                "[sparkle_words.custom.graphic]\ncollection = \"",
                "\"cats\"",
            ),
        ] {
            assert!(
                assist(source, source.len())
                    .completions
                    .iter()
                    .any(|completion| completion.insertion == expected),
                "missing {expected} completion in {source:?}"
            );
        }
    }

    #[test]
    fn saturated_range_warnings_cannot_hide_invalid_package_link_type() {
        let mut source = String::new();
        let mut authored = 0usize;
        for setting in config_schema() {
            let ConfigSchemaKind::Scalar(kind @ (EditKind::Float | EditKind::Integer)) =
                setting.kind
            else {
                continue;
            };
            let Some(range) = crate::prefs::range_of(setting.key) else {
                continue;
            };
            let outside = range.max + range.step.max(1.0);
            let value = if matches!(kind, EditKind::Integer) {
                format!("{outside:.0}")
            } else {
                outside.to_string()
            };
            source.push_str(&format!("{} = {value}\n", setting.key));
            authored += 1;
            if authored == MAX_DIAGNOSTICS {
                break;
            }
        }
        assert_eq!(authored, MAX_DIAGNOSTICS, "range-warning fixture drifted");
        source.push_str("[packages.links]\nay = 1\n");

        let analysis = analyze(&source);
        assert!(
            analysis.has_errors(),
            "late schema error must block Manual Save"
        );
        assert!(analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == ConfigDiagnosticSeverity::Error
                && diagnostic
                    .message
                    .contains("packages.links.ay must be text")
        }));
        assert!(analysis.omitted_diagnostics > 0);
    }

    #[test]
    fn key_lines_past_the_cap_end_in_one_count() {
        let source = (0..MAX_DIAGNOSTICS + 3)
            .map(|i| format!("qqq_unknown_{i} = 1\n"))
            .collect::<String>();
        let lines = ignored_key_warnings(&source);
        assert_eq!(lines.len(), MAX_DIAGNOSTICS + 1, "{lines:?}");
    }

    #[test]
    fn literal_dotted_keys_are_unknown_and_cannot_impersonate_nested_schema_paths() {
        let source = r#""packages.exclude" = ["literal"]

[packages]
exclude = ["nested"]
"#;
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        let messages = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message.contains(r#""packages.exclude" is unknown"#)),
            "the literal segment must retain an unknown, forward-compatible identity: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("packages.exclude is unknown")),
            "the nested registered path must not be diagnosed as unknown: {messages:?}"
        );

        let literal_value = r#""packages.exclude" = "#;
        let literal_assist = assist(literal_value, literal_value.len());
        assert!(literal_assist.completions.is_empty());
        assert!(literal_assist.help.is_none());

        let quoted_nested_value = "[packages]\n\"exclude\" = ";
        let nested_assist = assist(quoted_nested_value, quoted_nested_value.len());
        assert!(nested_assist.completions.iter().any(|completion| {
            completion.insertion == "[]" && completion.help.contains("list of text values")
        }));
    }

    #[test]
    fn diagnostics_ignore_literal_and_multiline_lookalikes_when_locating_nested_values() {
        let source = r#"future_note = """
[matrix_rain] # fake header
fps = 5 # fake value
"""
"matrix_rain.fps" = 7

[matrix_rain]
fps = 999
"#;
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        let range_warning = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.message.contains("matrix_rain.fps")
                    && diagnostic.message.contains("outside the supported")
            })
            .expect("the real nested value receives its registered range warning");
        assert_eq!(
            &source[range_warning.bytes.clone()],
            "999",
            "neither a fake multiline assignment nor a literal dotted segment may steal the diagnostic range"
        );
        assert!(analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains(r#""matrix_rain.fps" is unknown"#)
        }));
    }

    #[test]
    fn every_offered_key_and_value_completion_is_error_free_after_acceptance() {
        for setting in config_schema().iter().filter(|setting| {
            setting.kind.is_assignable() && !is_compatibility_only_key(setting.key)
        }) {
            let (base, local) = match array_table_root(setting.key) {
                None => {
                    let (table, local) = setting
                        .key
                        .rsplit_once('.')
                        .map_or(("", setting.key), |(table, local)| (table, local));
                    let base = if table.is_empty() {
                        String::new()
                    } else {
                        format!("[{table}]\n")
                    };
                    assert!(
                        !analyze(&base).has_errors(),
                        "completion base for {} must be valid",
                        setting.key
                    );
                    (base, local)
                }
                Some(record_root) => {
                    let relative = setting
                        .key
                        .strip_prefix(record_root)
                        .and_then(|suffix| suffix.strip_prefix('.'))
                        .expect("record member path");
                    let (parent, local) = relative
                        .rsplit_once('.')
                        .map_or(("", relative), |(parent, local)| (parent, local));
                    let mut base = format!("[[{record_root}]]\n");
                    if local != "words" && parent.is_empty() {
                        base.push_str("words = []\n");
                    }
                    if !parent.is_empty() {
                        base.push_str("words = []\n");
                        base.push_str(&format!("[{record_root}.{parent}]\n"));
                        if relative == "ink.sweep_once" {
                            base.push_str("colorway = \"rainbow\"\n");
                        }
                    }
                    (base, local)
                }
            };

            let key_source = format!("{base}{local}");
            let key_assist = assist(&key_source, key_source.len());
            let key_completion = key_assist
                .completions
                .iter()
                .find(|completion| completion.insertion.starts_with(&format!("{local} = ")))
                .unwrap_or_else(|| {
                    panic!(
                        "{} must be offered from its exact prefix: {:?}",
                        setting.key, key_assist
                    )
                });
            let mut completed = key_source.clone();
            completed.replace_range(
                key_completion.replacement.clone(),
                &key_completion.insertion,
            );
            let completed_analysis = analyze(&completed);
            assert!(
                !completed_analysis.has_errors(),
                "{} key completion produced errors in {completed:?}: {:?}",
                setting.key,
                completed_analysis.diagnostics
            );

            let value_source = format!("{base}{local} = ");
            for value_completion in assist(&value_source, value_source.len()).completions {
                let mut completed_value = value_source.clone();
                completed_value.replace_range(
                    value_completion.replacement.clone(),
                    &value_completion.insertion,
                );
                let value_analysis = analyze(&completed_value);
                assert!(
                    !value_analysis.has_errors(),
                    "{} value completion {:?} produced errors: {:?}",
                    setting.key,
                    value_completion.insertion,
                    value_analysis.diagnostics
                );
            }
        }
    }

    #[test]
    fn contextual_assist_is_derived_from_registered_metadata() {
        let top = assist("win", 3);
        let window = top
            .completions
            .iter()
            .find(|candidate| candidate.insertion.starts_with("window_theme ="))
            .expect("window-theme completion");
        assert!(!window.help.is_empty());

        let nested_source = "[sparkle_words]\nena";
        let nested = assist(nested_source, nested_source.len());
        assert!(
            nested
                .completions
                .iter()
                .any(|candidate| candidate.insertion == "enabled = true")
        );

        let bool_source = "[packages]\nenabled = ";
        assert_eq!(
            assist(bool_source, bool_source.len())
                .completions
                .iter()
                .map(|completion| completion.insertion.as_str())
                .collect::<Vec<_>>(),
            ["true", "false"]
        );

        let value_source = "window_theme = \"d";
        let values = assist(value_source, value_source.len());
        assert!(
            values
                .completions
                .iter()
                .any(|candidate| candidate.insertion == "\"dark\"")
        );

        let themes = aterm_types::scheme::builtin_names();
        assert!(themes.len() > MAX_COMPLETIONS);
        let later_theme = themes[MAX_COMPLETIONS];
        let theme_source = format!("theme = \"{later_theme}");
        let theme_values = assist(&theme_source, theme_source.len());
        assert!(
            theme_values
                .completions
                .iter()
                .any(|candidate| { candidate.insertion == format!("\"{later_theme}\"") })
        );
    }

    #[test]
    fn theme_completion_offers_catalog_themes_and_the_split_form() {
        let source = "theme = \"";
        let mut catalog_less = analyze(source);
        let offered = |analysis: &ConfigAnalysis| {
            assist_with_analysis(source, source.len(), analysis)
                .completions
                .into_iter()
                .map(|completion| completion.insertion)
                .collect::<Vec<_>>()
        };
        let before = offered(&catalog_less);
        assert!(!before.iter().any(|insertion| insertion == "\"harbour\""));
        assert!(
            before.contains(&"\"dark:Default,light:Default Light\"".to_string()),
            "{before:?}"
        );
        let themes = crate::app_config::ThemeCatalog::from_schemes([(
            "harbour".to_string(),
            aterm_types::scheme::builtin("Nord").unwrap(),
        )]);
        catalog_less.adopt_theme_names(&themes);
        let typed = "theme = \"har";
        let after = assist_with_analysis(typed, typed.len(), &catalog_less)
            .completions
            .into_iter()
            .map(|completion| completion.insertion)
            .collect::<Vec<_>>();
        assert_eq!(after, ["\"harbour\""]);
    }

    fn accept_completion(source: &str, caret: usize, insertion: &str) -> String {
        let assistance = assist(source, caret);
        let candidate = assistance
            .completions
            .iter()
            .find(|completion| completion.insertion == insertion)
            .unwrap_or_else(|| panic!("{insertion:?} offered at {caret} in {source:?}"));
        let mut completed = source.to_string();
        completed.replace_range(candidate.replacement.clone(), &candidate.insertion);
        assert!(
            !analyze(&completed).has_errors(),
            "accepting {insertion:?} must leave a valid config: {completed:?}"
        );
        completed
    }

    #[test]
    fn completion_suppresses_authored_paths_but_keeps_the_edited_token_and_records_repeatable() {
        let top_level = "theme = \"Nord\"\n";
        let top = assist(top_level, top_level.len());
        assert!(
            top.completions
                .iter()
                .all(|completion| !completion.insertion.starts_with("theme = "))
        );
        let other = top.completions.first().expect("another top-level scalar");
        accept_completion(top_level, top_level.len(), &other.insertion);

        let nested = "[packages]\nenabled = true\n";
        assert!(
            assist(nested, nested.len())
                .completions
                .iter()
                .all(|completion| !completion.insertion.starts_with("enabled = "))
        );
        accept_completion(nested, nested.len(), "exclude = []");

        let table = format!("{top_level}\n[pa");
        accept_completion(&table, table.len(), "[packages]");
        let duplicate_table = format!("{top_level}\n[packages]\n\n[");
        assert!(
            assist(&duplicate_table, duplicate_table.len())
                .completions
                .iter()
                .all(|completion| completion.insertion != "[packages]")
        );

        accept_completion("[packages]\n", "[pack".len(), "[packages]");

        let first_record = "[[sparkle_words.custom]]\nwords = [\"one\"]\nink = \"rainbow\"\n";
        let repeated = format!("{first_record}\n[[\nwords = [\"two\"]\nink = \"rainbow\"\n");
        accept_completion(
            &repeated,
            first_record.len() + "\n[[".len(),
            "[[sparkle_words.custom]]",
        );
        let nested_repeated = r#"[[sparkle_words.custom]]
words = ["one"]
[sparkle_words.custom.burst]
kind = "nova"

[[sparkle_words.custom]]
words = ["two"]

[sparkle_words.custom.b"#;
        accept_completion(
            nested_repeated,
            nested_repeated.len(),
            "[sparkle_words.custom.burst]",
        );
    }

    #[test]
    fn assist_is_inert_inside_multiline_strings_and_ignores_fake_context() {
        let all = ["[packages]", "# not a comment", "exclude ="];
        for (source, markers) in [
            (
                r#"[sparkle_words.profanity]
note = """
[packages] # not a table
# not a comment
exclude =
"""
sty"#,
                &all[..],
            ),
            (
                r#"[sparkle_words.profanity]
note = '''
[packages] # not a table
# not a comment
exclude =
'''
sty"#,
                &all[..],
            ),
            (
                r#"[sparkle_words.profanity]
note = """
escaped delimiter: \"""
[packages]
exclude =
"""
sty"#,
                &all[2..],
            ),
        ] {
            for marker in markers {
                let caret = source.find(marker).unwrap() + marker.len();
                assert_eq!(
                    assist(source, caret),
                    ConfigAssist::default(),
                    "assistance must be inert at {marker:?} inside a multiline string"
                );
            }

            let after = assist(source, source.len());
            assert!(
                after
                    .completions
                    .iter()
                    .any(|completion| completion.insertion.starts_with("style = ")),
                "the fake [packages] header must not replace the real profanity table: {after:?}"
            );
            assert!(
                after
                    .completions
                    .iter()
                    .all(|completion| !completion.insertion.starts_with("exclude = "))
            );
        }
    }

    #[test]
    fn worker_built_assist_index_matches_standalone_language_results() {
        for source in [
            "",
            "win",
            "[packages]\ninc",
            "[sparkle_words.profanity]\nnote = \"\"\"\n[packages]\n\"\"\"\nsty",
            "[keybindings]\n\"literal.key\" = \"value\"\n",
            "[[foo]]\ndisk.apply = false\n",
        ] {
            let analysis = analyze(source);
            for caret in (0..=source.len()).filter(|caret| source.is_char_boundary(*caret)) {
                assert_eq!(
                    assist_with_analysis(source, caret, &analysis),
                    assist(source, caret),
                    "indexed assistance diverged at byte {caret} in {source:?}"
                );
            }
        }
    }

    #[test]
    fn completion_replaces_the_full_key_or_value_token_from_a_mid_token_caret() {
        let source = "window_theme = \"dark\" # keep this comment\n";

        let key_assistance = assist(source, 3);
        let key = key_assistance
            .completions
            .iter()
            .find(|candidate| candidate.display.starts_with("window_theme —"))
            .expect("mid-key completion");
        assert_eq!(&source[key.replacement.clone()], "window_theme");
        assert_eq!(key.expected, "window_theme");
        assert_eq!(key.insertion, "window_theme");
        let mut completed_key = source.to_string();
        completed_key.replace_range(key.replacement.clone(), &key.insertion);
        assert_eq!(completed_key, source);

        let value_start = source.find("\"dark\"").unwrap();
        let value_assistance = assist(source, value_start + 2);
        let value = value_assistance
            .completions
            .iter()
            .find(|candidate| candidate.insertion == "\"dark\"")
            .expect("mid-value completion");
        assert_eq!(&source[value.replacement.clone()], "\"dark\"");
        assert_eq!(value.expected, "\"dark\"");
        let mut completed_value = source.to_string();
        completed_value.replace_range(value.replacement.clone(), &value.insertion);
        assert_eq!(completed_value, source);
    }

    #[test]
    fn completion_actions_bind_document_caret_range_and_candidate() {
        let source = "win";
        let assist = assist(source, source.len());
        let (index, completion) = assist
            .completions
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.insertion.starts_with("window_theme ="))
            .expect("window-theme completion");
        let context = ConfigCompletionContext::new(7, 11, source.len());
        let action = config_completion_action(context, index, completion);
        assert!(is_config_completion_action(&action));
        assert!(action.len() <= MAX_COMPLETION_ACTION_BYTES);
        let wire = format!("app/v1 view 1 editor/config-completion/{index} {action}");
        assert!(
            aterm_types::app_inspection::parse_act(&wire).is_ok(),
            "bound completion action must fit the public control protocol"
        );
        assert_eq!(
            resolve_config_completion_action(source, context, &action),
            Some(completion.clone())
        );

        for stale in [
            ConfigCompletionContext::new(8, 11, source.len()),
            ConfigCompletionContext::new(7, 12, source.len()),
            ConfigCompletionContext::new(7, 11, source.len() - 1),
        ] {
            assert_eq!(
                resolve_config_completion_action(source, stale, &action),
                None
            );
        }

        let mut wrong_range = completion.clone();
        wrong_range.replacement.end -= 1;
        let wrong_range_action = config_completion_action(context, index, &wrong_range);
        assert_eq!(
            resolve_config_completion_action(source, context, &wrong_range_action),
            None
        );

        let mut wrong_candidate = completion.clone();
        wrong_candidate.insertion.push('x');
        let wrong_candidate_action = config_completion_action(context, index, &wrong_candidate);
        assert_eq!(
            resolve_config_completion_action(source, context, &wrong_candidate_action),
            None
        );

        let mut wrong_selection = completion.clone();
        wrong_selection.post_insert_selection = 0..0;
        let wrong_selection_action = config_completion_action(context, index, &wrong_selection);
        assert_eq!(
            resolve_config_completion_action(source, context, &wrong_selection_action),
            None,
            "post-insert editing intent is part of the stale-checked identity"
        );
    }

    #[test]
    fn analysis_and_context_are_strictly_bounded() {
        let source = "x".repeat(MAX_CONFIG_ANALYSIS_BYTES + 1);
        let analysis = analyze(&source);
        assert!(analysis.has_errors());
        assert!(analysis.syntax.len() <= MAX_SYNTAX_SPANS);
        assert_eq!(assist(&source, source.len()), ConfigAssist::default());

        let source = format!("{}é", "x".repeat(MAX_CONFIG_ANALYSIS_BYTES - 1));
        assert!(!source.is_char_boundary(MAX_CONFIG_ANALYSIS_BYTES));
        let analysis = analyze(&source);
        assert!(analysis.has_errors());
        assert!(analysis.syntax.iter().all(|span| {
            source.is_char_boundary(span.bytes.start) && source.is_char_boundary(span.bytes.end)
        }));
    }

    #[test]
    fn decoration_projects_only_visible_syntax_and_diagnostics() {
        let source = "font_px = \"large\"\n";
        let analysis = analyze(source);
        let mut projection = EditorViewportProjection {
            first_line: 1,
            total_lines: 1,
            lines: vec![crate::native_editor::EditorViewportLine {
                number: 1,
                source: 0..source.trim_end_matches('\n').len(),
                column_start: 0,
                text: source.trim_end_matches('\n').to_string(),
                selections: Vec::new(),
                carets: Vec::new(),
                syntax: Vec::new(),
                diagnostics: Vec::new(),
            }],
        };

        decorate_projection(&mut projection, &analysis);
        let line = &projection.lines[0];
        assert!(line.syntax.iter().any(|span| {
            span.class == EditorSyntaxClass::Key && &line.text[span.bytes.clone()] == "font_px"
        }));
        assert!(line.diagnostics.iter().any(|diagnostic| diagnostic.error));
        assert!(
            line.syntax.len() <= analysis.syntax.len()
                && line.diagnostics.len() <= analysis.diagnostics.len()
        );
    }

    fn projection_pin_source() -> String {
        let wide = "wide ".repeat(80);
        format!(
            "# aterm config\n\
             [window]\n\
             font_px = 18   # trailing comment after a key\n\
             title = \"α terminal ✨\"   # multibyte value\n\
             \n\
             opacity = 0.85\n\
             notes = \"{wide}\"   # forces a horizontally sliced window\n\
             [window.padding]\n\
             left = 4\n"
        )
    }

    fn for_each_config_projection(
        source: &str,
        mut inspect: impl FnMut(&EditorViewportProjection),
    ) {
        let mut store = crate::test_document_store();
        let document = store.open("mem://config-projection".to_string(), source.to_string());
        let mut workspace = crate::native_editor::EditorWorkspace::new();
        let mut view = workspace
            .attach(
                &mut store,
                document,
                crate::document_store::DocumentViewId(91),
            )
            .expect("attaching a freshly opened in-memory document cannot fail");
        for anchor in (0..=source.len()).step_by(23) {
            for caret in (0..=source.len()).step_by(37) {
                view.viewport_anchor = anchor;
                view.selections = vec![crate::native_editor::Selection::caret(caret)];
                for (rows, columns) in
                    [(1usize, 4usize), (3, 12), (7, 40), (usize::MAX, usize::MAX)]
                {
                    inspect(&crate::native_editor::project_viewport(
                        source, &view, rows, columns,
                    ));
                }
            }
        }
    }

    #[test]
    fn windowed_decoration_matches_a_full_sweep() {
        let source = projection_pin_source();
        let analysis = analyze(&source);
        assert!(
            !analysis.syntax.is_empty(),
            "the pinned source must actually lex to spans"
        );
        for_each_config_projection(&source, |projection| {
            let mut decorated = projection.clone();
            decorate_projection(&mut decorated, &analysis);
            for (line, original) in decorated.lines.iter().zip(&projection.lines) {
                let swept: Vec<EditorSyntaxSpan> = analysis
                    .syntax
                    .iter()
                    .filter_map(|span| {
                        relative_intersection(&original.source, &span.bytes).map(|bytes| {
                            EditorSyntaxSpan {
                                bytes,
                                class: span.class,
                            }
                        })
                    })
                    .collect();
                assert_eq!(
                    line.syntax, swept,
                    "windowed scan diverged from the full sweep on {:?}",
                    original.source
                );
            }
        });
    }

    fn only_message(source: &str) -> String {
        let analysis = analyze(source);
        assert_eq!(
            analysis.diagnostics.len(),
            1,
            "expected exactly one diagnostic for {source:?}: {:?}",
            analysis.diagnostics
        );
        analysis.diagnostics[0].message.clone()
    }

    #[test]
    fn a_mistyped_key_is_told_the_spelling_that_works() {
        for (source, intended) in [
            ("cursor_trail_stlye = 1\n", "cursor_trail_style"),
            ("colums = 1\n", "columns"),
            ("scrollback_lnies = 1\n", "scrollback_lines"),
            ("Columns = 100\n", "columns"),
            ("[keybindigns]\n\"ctrl+t\" = \"new_tab\"\n", "keybindings"),
            ("cursor_style = \"blok\"\n", "block"),
        ] {
            let message = only_message(source);
            assert!(
                message.contains(&format!("did you mean {intended:?}")),
                "{source:?} must name {intended}: {message}"
            );
        }
    }

    #[test]
    fn a_suggestion_is_always_a_key_the_loader_accepts_and_never_a_working_one() {
        let suggestable = suggestable_config_paths();
        for dead in RETIRED_CONFIG_KEYS
            .iter()
            .map(|retired| retired.key)
            .chain(COMPATIBILITY_ONLY_KEYS.iter().copied())
        {
            assert!(
                !suggestable.contains(&dead),
                "{dead} must not be suggestable"
            );
        }
        for entry in config_schema() {
            assert_eq!(
                nearest_config_key(entry.key),
                None,
                "{} is registered and must never be corrected",
                entry.key
            );
        }
    }

    #[test]
    fn the_network_drives_leftovers_are_told_once_as_removed() {
        for (text, key) in [
            (
                "[net]\nlisten = \"0.0.0.0:7100\"\ncert = \"c.der\"\nkey = \"k.der\"\n",
                "net",
            ),
            (
                "[[net.connections]]\nname = \"box\"\nhost = \"box:7100\"\n",
                "net.connections",
            ),
        ] {
            let warnings = key_warnings(text);
            assert!(warnings.ignored.is_empty(), "{text:?}: {warnings:?}");
            assert_eq!(warnings.retired.len(), 1, "{text:?}: {warnings:?}");
            let line = &warnings.retired[0];
            assert!(
                line.starts_with(&format!(
                    "line 1: {key} does nothing: Network drive was removed"
                )),
                "{line}"
            );
            assert!(line.contains("aterm fleet join"), "{line}");
        }
    }

    #[test]
    fn the_near_miss_budget_scales_with_how_much_of_the_name_is_left() {
        assert_eq!(
            nearest_config_key("gpx"),
            Some("gpu"),
            "one edit, short key"
        );
        assert_eq!(nearest_config_key("xzq"), None, "two edits, short key");
        assert_eq!(
            nearest_config_key("scrolback_lnes"),
            Some("scrollback_lines"),
            "two edits, long key"
        );
        assert_eq!(nearest_config_key("theme_of_everything"), None);
        assert_eq!(
            nearest_enum_value("wildly-different", &["block", "bar"]),
            None
        );
    }

    #[test]
    fn every_registered_enum_key_is_graded_on_its_spelling_and_on_its_value() {
        let mut graded = 0;
        for setting in config_schema() {
            let ConfigSchemaKind::Scalar(EditKind::Enum { .. }) = setting.kind else {
                continue;
            };
            if is_compatibility_only_key(setting.key) {
                continue;
            }
            if setting.key.starts_with("sparkle_words.custom.") {
                continue;
            }
            graded += 1;
            let mistyped = format!("{}zq = \"x\"\n", setting.key);
            assert!(
                ignored_key_warnings(&mistyped)
                    .iter()
                    .any(|line| line.contains(setting.key)),
                "a mistyped {} must be reported: {:?}",
                setting.key,
                ignored_key_warnings(&mistyped)
            );
            let refused = format!("{} = \"zq not a value\"\n", setting.key);
            assert!(
                unaccepted_value_warnings(&refused, &[])
                    .iter()
                    .any(|line| line.starts_with(setting.key)),
                "a value {} does not accept must be reported: {:?}",
                setting.key,
                unaccepted_value_warnings(&refused, &[])
            );
        }
        assert!(graded > 20, "the registry sweep found only {graded} keys");
    }

    #[test]
    fn a_line_that_already_named_the_value_suppresses_the_generic_one_but_a_platform_note_does_not()
    {
        let source = "window_theme = \"drak\"\n";
        assert!(
            unaccepted_value_warnings(
                source,
                &["window_theme: expected auto|light|dark, got \"drak\"; using auto".to_string()],
            )
            .is_empty(),
            "a line naming the refused value has told this story"
        );
        assert_eq!(
            unaccepted_value_warnings(
                source,
                &["window_theme has no effect on this platform".to_string()],
            )
            .len(),
            1,
            "a platform note never said the value was wrong"
        );
    }

    #[test]
    fn a_removed_features_key_is_a_record_and_a_typo_is_not() {
        let warned =
            key_warnings("show_hud = true\ncolums = 100\n[packages]\nchannel = \"beta\"\n");
        assert!(
            warned
                .retired
                .iter()
                .any(|l| l.contains("show_hud does nothing")),
            "{warned:?}"
        );
        assert!(
            warned
                .retired
                .iter()
                .any(|l| l.contains("packages.channel")),
            "{warned:?}"
        );
        assert!(
            warned
                .ignored
                .iter()
                .all(|l| !l.contains("show_hud") && !l.contains("channel")),
            "{warned:?}"
        );
        assert!(
            warned.ignored.iter().any(|l| l.contains("colums")),
            "{warned:?}"
        );
        let all = ignored_key_warnings("theme = \"Nord\"\ncolums = 100\nshow_hud = true\n");
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(
            all.iter()
                .any(|line| line.starts_with("line 2:") && line.contains("\"columns\"")),
            "{all:?}"
        );
        assert!(
            all.iter().any(|line| line.starts_with("line 3:")),
            "{all:?}"
        );
        assert!(ignored_key_warnings("theme = \"Nord\"\n").is_empty());
        assert!(unaccepted_value_warnings("columns = 100\n", &[]).is_empty());
        assert!(ignored_key_warnings("theme = \"Nord\n").is_empty());
        assert!(unaccepted_value_warnings("theme = \"Nord\n", &[]).is_empty());
    }

    #[test]
    fn every_ai_key_is_known_to_the_config_language() {
        let fleet = if cfg!(windows) {
            "C:/dotfiles/ai-fleet.toml"
        } else {
            "/dotfiles/ai-fleet.toml"
        };
        let mut text = format!(
            "[ai]\nenabled = true\npreferred = \"codex\"\nreviewers = [\"codex\"]\n\
             review_count = 2\nreview_timeout_s = 120\nmachine = \"m7\"\n\
             fleet = \"{fleet}\"\nreviews = true\n\
             review_permissions = \"inherit\"\n\
             fleet_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n",
        );
        for id in aterm_agent::ai::connector::ids() {
            text.push_str(&format!("[ai.{id}]\nuse = false\n"));
        }
        let (_, notes) = aterm_agent::ai::config::read(&text);
        assert!(notes.is_empty(), "the AI reader: {notes:?}");
        assert!(
            ignored_key_warnings(&text).is_empty(),
            "{:?}",
            ignored_key_warnings(&text)
        );
        for key in ["ai.reviewers", "ai.review_count", "ai.review_timeout_s"] {
            assert!(
                crate::prefs::nested_leaf(key).is_none(),
                "{key} has no Settings row"
            );
            assert!(
                MANUAL_SCHEMA.iter().any(|e| e.key == key),
                "{key} is a Manual row"
            );
        }
        let mut rows: Vec<String> = [
            crate::prefs::EDIT_AI_ENABLED,
            crate::prefs::EDIT_AI_PREFERRED,
            crate::prefs::EDIT_AI_REVIEWS,
            crate::prefs::EDIT_AI_REVIEW_PERMISSIONS,
            crate::prefs::EDIT_AI_MACHINE,
            crate::prefs::EDIT_AI_FLEET,
            crate::prefs::EDIT_AI_FLEET_KEY,
        ]
        .map(str::to_string)
        .to_vec();
        rows.extend(
            aterm_agent::ai::connector::ids()
                .into_iter()
                .map(|id| format!("ai.{id}.use")),
        );
        for key in &rows {
            assert!(
                crate::prefs::nested_leaf(key).is_some()
                    && !MANUAL_SCHEMA.iter().any(|e| e.key == key.as_str()),
                "{key} is a Settings row: one registration, in NESTED_LEAVES"
            );
            assert!(
                config_schema().iter().any(|e| e.key == key.as_str()),
                "{key} is not served by config_schema()"
            );
        }
        for wrong in [
            "[ai]\nreviewer = [\"codex\"]\n",
            "[ai.nonesuch]\nuse = true\n",
        ] {
            assert!(
                !aterm_agent::ai::config::read(wrong).1.is_empty(),
                "{wrong}"
            );
            assert!(!ignored_key_warnings(wrong).is_empty(), "{wrong}");
        }
    }

    #[test]
    fn a_disk_value_the_reader_cannot_read_is_told_once_in_its_words() {
        use crate::message_reporters::{ConfigFamily, ConfigWarnings};
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let file = dir.path().join("aterm.toml");
        let reader = |text: &str| {
            std::fs::write(&file, text).unwrap();
            aterm_agent::harness::cli::disk_config_read(Some(&file))
        };
        let record = |name: &str| {
            format!("[[sparkle_words.custom]]\nwords = [\"{name}\"]\nink = \"rainbow\"\n")
        };
        let cases: Vec<(String, usize)> = vec![
            ("disk = 5\n".into(), 1),
            ("[[disk]]\napply = false\n".into(), 1),
            ("disk = { apply = \"yes\" }\n".into(), 1),
            ("[disk]\napply = \"yes\"\n".into(), 2),
            ("[disk]\nauto_free_gib = 20.0\n".into(), 2),
            ("[disk]\ntarget_stale_days = -1.0\n".into(), 2),
            ("[harness]\ndisk.auto_free_gib = 0\n".into(), 2),
            (format!("{}disk.auto_free_gib = 0\n", record("box")), 4),
            (
                format!("{}{}disk.apply = false\n", record("a"), record("b")),
                7,
            ),
            ("[[foo]]\ndisk.apply = false\n".into(), 2),
            ("[harness.approve_all]\ndisk.apply = false\n".into(), 2),
        ];
        for (text, line) in &cases {
            let reading = reader(text);
            let off = reading.config;
            assert!(
                !off.apply && off.auto_free_gib == 0 && !off.dead_build_dirs,
                "{text:?}: {reading:?}"
            );
            let [note] = reading.notes.as_slice() else {
                panic!("{text:?}: {reading:?}");
            };
            let told = format!("line {line}: {note}");
            let diagnostics = analyze(text).diagnostics;
            assert!(
                matches!(diagnostics.as_slice(), [d] if d.severity == ConfigDiagnosticSeverity::Error
                    && d.line == *line
                    && &d.message == note),
                "{text:?}: {diagnostics:?}"
            );
            assert_eq!(
                crate::diagnostics::validate_config_text(text),
                Ok(vec![told.clone()]),
                "{text:?}"
            );
            let keys = key_warnings(text);
            assert!(
                keys.ignored.is_empty() && keys.retired.is_empty(),
                "{text:?}: {keys:?}"
            );
            assert_eq!(unaccepted_value_warnings(text, &[]), [told], "{text:?}");
            let mut warns = ConfigWarnings::default();
            crate::app_config::collect_key_notices(&mut warns, text);
            let unaccepted = crate::app_config::unaccepted_value_notices(text, &warns.told());
            warns.extend(ConfigFamily::UnacceptedValues, unaccepted);
            let rows = warns
                .into_messages()
                .into_iter()
                .map(|m| m.key)
                .collect::<Vec<_>>();
            assert_eq!(
                rows,
                [Some(ConfigFamily::UnacceptedValues.key().to_string())],
                "{text:?}"
            );
        }

        let text = "[a.disk.b.disk]\napply = false\n";
        assert!(reader(text).notes.is_empty(), "{:?}", reader(text));
        let keys = key_warnings(text);
        assert!(
            matches!(keys.ignored.as_slice(), [l] if l.contains("a.disk.b.disk.apply")),
            "{keys:?}"
        );
        assert!(unaccepted_value_warnings(text, &[]).is_empty());
        let text = "[harness]\ndisk.warn_free_gib = 5\n";
        let reading = reader(text);
        assert_eq!(
            reading.config,
            aterm_agent::harness::disk::Config::default()
        );
        let diagnostics = analyze(text).diagnostics;
        assert!(
            !diagnostics.is_empty()
                && diagnostics
                    .iter()
                    .all(|d| d.severity == ConfigDiagnosticSeverity::Warning
                        && reading.notes.contains(&d.message)),
            "{diagnostics:?} vs {:?}",
            reading.notes
        );
        let said = aterm_agent::harness::disk::Config::negative_reading("target_stale_days")
            .expect("a floor");
        let whole = analyze("[disk]\ntarget_stale_days = -1\n").diagnostics;
        assert!(
            whole
                .iter()
                .any(|d| d.message.ends_with(&said)
                    && d.severity == ConfigDiagnosticSeverity::Warning),
            "{whole:?}"
        );
        assert!(
            !whole
                .iter()
                .any(|d| d.severity == ConfigDiagnosticSeverity::Error),
            "{whole:?}"
        );
    }

    #[test]
    fn no_walker_calls_a_table_that_holds_a_disk_off_switch_one_to_delete() {
        let record = |name: &str| {
            format!("[[sparkle_words.custom]]\nwords = [\"{name}\"]\nink = \"rainbow\"\n")
        };
        let cases: Vec<(String, usize, (usize, &str))> = vec![
            (
                "[harness.approve_all]\nbogus = 1\ndisk.apply = false\n".into(),
                3,
                (2, "harness.approve_all.bogus"),
            ),
            (
                "[[foo]]\nbogus = 1\ndisk.apply = false\n".into(),
                3,
                (2, "foo.bogus"),
            ),
            (
                "[harness.approve_all.x]\nbogus = 1\ndisk.apply = false\n".into(),
                3,
                (2, "harness.approve_all.x.bogus"),
            ),
            (
                format!("{}bogus = 1\ndisk.apply = false\n", record("box")),
                5,
                (4, "sparkle_words.custom.bogus"),
            ),
            (
                format!(
                    "{}bogus = 1\n{}disk.apply = false\n",
                    record("a"),
                    record("b")
                ),
                8,
                (4, "sparkle_words.custom.bogus"),
            ),
            (
                format!(
                    "{}[sparkle_words.custom.extra]\nbogus = 1\ndisk.apply = false\n",
                    record("box")
                ),
                6,
                (5, "sparkle_words.custom.extra.bogus"),
            ),
        ];
        for (text, line, (told_line, told_key)) in &cases {
            let [fault] = aterm_agent::harness::cli::disk_faults(text)
                .try_into()
                .unwrap_or_else(|faults: Vec<_>| panic!("{text:?}: {faults:?}"));
            assert!(fault.removal_off, "{text:?}: {fault:?}");
            let reader_line = format!("line {line}: {}", fault.note);
            let lines = ignored_key_warnings(text);
            assert_eq!(
                lines.iter().filter(|l| **l == reader_line).count(),
                1,
                "{text:?}: {lines:?}"
            );
            let told = format!("line {told_line}: {told_key} ");
            assert!(
                matches!(
                    lines
                        .iter()
                        .filter(|l| **l != reader_line)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    [l] if l.starts_with(&told)
                ),
                "{text:?}: {lines:?}"
            );
            assert_eq!(
                unaccepted_value_warnings(text, &[]),
                std::slice::from_ref(&reader_line)
            );
            let diagnostics = analyze(text).diagnostics;
            assert_eq!(diagnostics.len(), lines.len(), "{text:?}: {diagnostics:?}");
            assert!(
                diagnostics
                    .iter()
                    .all(|d| (d.severity == ConfigDiagnosticSeverity::Error)
                        == (d.message == fault.note)),
                "{text:?}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn a_known_keys_value_error_is_never_a_key_with_no_effect() {
        let custom = "[[sparkle_words.custom]]\nwords = [\"one\"]\n";
        for (text, retired) in [
            (
                "windw_padding = 20\n[packages]\nauto_update = \"yes\"\n".to_string(),
                true,
            ),
            (
                format!("windw_padding = 20\n{custom}burst = {{ kind = \"blorp\" }}\n"),
                false,
            ),
            (
                format!("windw_padding = 20\n{custom}burst = {{ chance = 500 }}\n"),
                false,
            ),
        ] {
            let said = key_analysis(&text)
                .expect("the text parses")
                .unaccepted_values
                .iter()
                .map(key_warning_line)
                .collect::<Vec<_>>();
            assert_eq!(said.len(), 1, "{text:?}: {said:?}");
            let keys = key_warnings(&text);
            assert!(
                matches!(keys.ignored.as_slice(), [l] if l.starts_with("line 1: windw_padding")),
                "{text:?}: {keys:?}"
            );
            assert_eq!(
                keys.retired.len(),
                usize::from(retired),
                "{text:?}: {keys:?}"
            );
            let unaccepted = unaccepted_value_warnings(&text, &[]);
            for line in &said {
                assert!(
                    !keys.ignored.contains(line) && !keys.retired.contains(line),
                    "{text:?}: {keys:?}"
                );
                assert_eq!(
                    unaccepted.iter().filter(|l| *l == line).count(),
                    1,
                    "{text:?}: {unaccepted:?}"
                );
            }
        }
    }

    #[test]
    fn a_retired_harness_key_is_told_the_limit_it_still_is() {
        for key in aterm_agent::supervise::config::RETIRED_KEYS {
            for value in [false, true] {
                let warned = key_warnings(&format!("[harness]\n{key} = {value}\n"));
                assert!(warned.ignored.is_empty(), "{key}: {:?}", warned.ignored);
                let said = aterm_agent::supervise::config::retired_key_note(key, Some(value))
                    .expect("a retired key");
                assert!(
                    warned.retired.iter().any(|l| l.contains(&said)),
                    "{key} = {value}: {:?}",
                    warned.retired
                );
            }
        }
        let typo = key_warnings("[harness]\napprove_al = false\n");
        assert!(typo.retired.is_empty(), "{:?}", typo.retired);
        assert!(
            typo.ignored.iter().any(|l| l.contains("approve_al")),
            "{:?}",
            typo.ignored
        );
    }
}
