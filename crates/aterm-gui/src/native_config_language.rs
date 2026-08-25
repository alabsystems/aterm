// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded language assistance for the canonical `aterm.toml` editor.
//!
//! This module is deliberately pure. The document host owns file capabilities
//! and atomic persistence; the editor controller hands immutable source bytes
//! here after a document revision changes. Paint consumes only the bounded spans
//! projected onto visible editor rows and never parses or touches the filesystem.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

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
// The public app-inspection protocol caps action tokens at 64 bytes. Prefix +
// one-digit candidate index + slash + 128-bit identity is at most 59 bytes.
const MAX_COMPLETION_ACTION_BYTES: usize = 64;
pub(crate) const CONFIG_COMPLETION_ACTION_PREFIX: &str = "editor/config-completion/";
const COMPATIBILITY_ONLY_KEYS: &[&str] = &[
    "matrix_rain.materialize",
    "matrix_rain.ink_text",
    "matrix_rain.phosphor",
];
/// Typed compatibility metadata shared with non-editing inventory surfaces
/// such as Modified. A retired key may retain schema type information so
/// Manual can diagnose its authored value precisely, but it is never an active
/// Settings control or completion candidate.
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
    // `cursor_trail_wake_ms` USED TO BE LISTED HERE and it was wrong (found
    // 2026-08-10 while giving the cursor kitty its own Settings page). The
    // typing wake came back: `CursorGlow::wake` reads `GlowConfig::wake_persist_s`
    // on every rainbow frame — its own comment even names the Settings row — the
    // value is clamped 0..=1500 ms by `Config::cursor_trail_wake_persist_or_default`,
    // and `cursor_glow::rainbow_wake_persistence_is_a_host_dial_that_fails_off`
    // proves the plume lengthens monotonically with it and fails OFF at 0/NaN.
    // Manual was telling anyone who authored the key that it "has no effect",
    // and Settings projected it as "Compatibility only · No effect". A dial that
    // works and a UI that says it does not is the same defect as a switch that
    // does nothing, pointing the other way.
];

pub(crate) fn retired_config_key(key: &str) -> Option<&'static RetiredConfigKeyMetadata> {
    RETIRED_CONFIG_KEYS
        .iter()
        .find(|metadata| metadata.key == key)
}

pub(crate) fn is_compatibility_only_key(key: &str) -> bool {
    COMPATIBILITY_ONLY_KEYS.contains(&key)
        || retired_config_key(key).is_some()
        || key == "sparkle_words.orca"
        || key.starts_with("sparkle_words.orca.")
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
/// `windowing_behavior` — the two canonical spellings. Windows Terminal's own
/// `useNew`/`useExisting` also parse (`aterm_cli::WindowingBehavior::parse`) and
/// are offered as aliases so a ported `settings.json` habit validates.
const WINDOWING_BEHAVIORS: &[&str] = &["new_window", "attach"];
/// The five DEFERRED_CONFIG_KEYS scalars an operator writes by hand (two are
/// taught by `--help` and the `--write-config` starter). They follow the
/// `windowing_behavior` precedent: off `prefs::editable_fields`, but Manual
/// must complete them, hover them, and flag a misspelled VALUE — calling a
/// live key "unknown to this aterm build" was a false diagnostic (audit-2
/// item 9). Canonical spellings here; parser aliases below, accepted but
/// never offered.
const FONT_HINTINGS: &[&str] = &["full", "light", "native", "off"];
const FONT_SUBPIXELS: &[&str] = &["off", "rgb", "bgr"];
const RIGHT_CLICK_GESTURES: &[&str] = &["copy_paste", "off"];
const RIGHT_CLICK_ALIASES: &[&str] = &["copy-paste"];
const TAB_MENU_CHORDS: &[&str] = &["on", "menu_key", "off"];
const TAB_MENU_CHORD_ALIASES: &[&str] = &["both", "menu-key", "menu"];
const TAB_BAND_HEIGHTS: &[&str] = &["compact", "standard"];
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
    /// Worker-built lexical state at every line boundary. Completion rendering
    /// and activation use this immutable index instead of rescanning every byte
    /// before the caret on the event-loop thread.
    assist_index: ConfigAssistIndex,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ConfigAssistIndex {
    lines: Vec<AssistLineState>,
    tables: Vec<String>,
    authored: HashMap<String, AuthoredPath>,
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

    pub(crate) fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }

    /// Diagnostics are presented with blocking errors first, followed by
    /// warnings, while retaining source order inside each severity. This keeps
    /// the initial Manual status aligned with the Save gate and still makes
    /// every retained problem reachable through deterministic F8 navigation.
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
        let count = self.diagnostic_count();
        let index = index.min(count.saturating_sub(1));
        let diagnostic = self.diagnostic_at(index)?;
        let level = match diagnostic.severity {
            ConfigDiagnosticSeverity::Warning => "warning",
            ConfigDiagnosticSeverity::Error => "error",
        };
        let navigation = if count > 1 {
            format!(
                " · Problem {} of {count} · F8 next · Shift-F8 previous",
                index + 1
            )
        } else {
            " · F8 jump to problem".to_string()
        };
        let omitted = if self.omitted_diagnostics == 0 {
            String::new()
        } else {
            let noun = if self.omitted_diagnostics == 1 {
                "diagnostic"
            } else {
                "diagnostics"
            };
            format!(
                " · {} additional {noun} omitted by the bounded validator",
                self.omitted_diagnostics,
            )
        };
        Some(format!(
            "Config {level}{navigation} · Ln {}, Col {} · {}{omitted}",
            diagnostic.line, diagnostic.column, diagnostic.message,
        ))
    }

    pub(crate) fn summary(&self) -> Option<String> {
        self.summary_at(0)
    }

    /// Retain a bounded, severity-prioritized diagnostic set. A late blocking
    /// error can never disappear behind earlier warnings: it evicts the last
    /// retained warning when necessary. Omitted entries remain counted so the
    /// status line discloses truncation instead of presenting the list as
    /// complete.
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

    /// Merge filesystem-backed diagnostics produced by the capability-owning
    /// host for this exact source revision. The host completion is stale-checked
    /// before calling this; deduplication also makes a repeated completion
    /// presentation-inert. The result reports content mutation rather than a
    /// length delta because a capped warning list may evict one entry while
    /// appending another.
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
    /// Byte offsets within `insertion` selected after acceptance. Empty ranges
    /// place the caret; non-empty ranges select the editable sample while
    /// retaining its TOML quotes/delimiters.
    pub(crate) post_insert_selection: Range<usize>,
    pub(crate) display: String,
    pub(crate) help: String,
}

/// Exact immutable context from which one config-completion control was
/// rendered. Document sequence protects source bytes and the caret protects
/// the completion locus. Presentation revisions are deliberately excluded:
/// inspection and focus bookkeeping can advance them without changing the
/// completion's meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConfigCompletionContext {
    document: u64,
    document_sequence: u64,
    caret: usize,
}

impl ConfigCompletionContext {
    pub(crate) fn new(document: u64, document_sequence: u64, caret: usize) -> Self {
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

/// Bind a semantic completion action to the exact document/view generation,
/// caret, replacement range, expected bytes, insertion, and post-insert editing
/// selection that authored its visible label. A truncated SHA-256 identity
/// keeps the complete action inside the public 64-byte app-inspection limit
/// without exposing config text.
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

/// Resolve a rendered action only when every source and view-state guard still
/// matches. Recomputing assistance is safe after those checks because the
/// candidate must also have the exact range and insertion encoded by the
/// semantic control that the user activated.
#[cfg(test)]
pub(crate) fn resolve_config_completion_action(
    source: &str,
    context: ConfigCompletionContext,
    action: &str,
) -> Option<ConfigCompletionEdit> {
    let assist = assist(source, context.caret);
    resolve_config_completion_from_assist(source, context, action, assist)
}

/// Production activation path. The candidate is regenerated from the exact
/// worker-built lexical index, keeping the identity/source guards while
/// avoiding a document-prefix scan on click.
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
/// Candidate/source checks remain host-owned because the registry deliberately
/// has no document capability.
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
    let digest = digest.finalize();
    let mut identity = String::with_capacity(32);
    use std::fmt::Write as _;
    for byte in &digest[..16] {
        write!(&mut identity, "{byte:02x}").expect("writing into String cannot fail");
    }
    identity
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigAssist {
    pub(crate) help: Option<String>,
    pub(crate) completions: Vec<ConfigCompletionEdit>,
}

/// One value shape in the canonical Settings/Manual language registry.
///
/// `Scalar` entries come from the exact preference writer metadata. The other
/// shapes describe TOML-native structures that must never be squeezed through
/// a lossy one-line Settings control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigSchemaKind {
    Scalar(EditKind),
    StringList,
    /// `FontList` accepts either one comma-delimited string or a TOML string
    /// array. Manual suggests the unambiguous array spelling, while validation
    /// continues to admit both runtime-supported forms.
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

/// Shared metadata consumed by validation, completion, search, diagnostics,
/// and Modified. Adding a Manual schema entry here makes all of those surfaces
/// discover it together instead of relying on drifting allowlists.
#[derive(Clone, Debug)]
pub(crate) struct ConfigSchemaEntry {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: ConfigSchemaKind,
    placeholder: String,
    keywords: &'static [&'static str],
    /// A native scalar has a lossless `EditField` representation. Presentation
    /// policy can still keep it out of the curated ordinary Settings form.
    pub(crate) native_scalar: bool,
    /// Only exact registered Manual paths may emit a one-key reset patch.
    /// This includes a whole known dynamic map/structured list, where Reset is
    /// explicitly labeled as clearing that collection. Array members and
    /// forward-compatible paths never gain a guessed destructive action.
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

/// TOML-native schema not represented by `prefs::editable_fields`, plus every
/// known table header. Record members are included for context-aware completion
/// but are not individually resettable because their dotted paths cross an
/// array-of-tables boundary.
const MANUAL_SCHEMA: &[ManualSchemaEntry] = &[
    manual(
        "keybindings",
        "Keyboard shortcut map",
        ConfigSchemaKind::DynamicStringMap,
        &["shortcut", "chord", "action", "keyboard"],
        true,
    ),
    // A FRONT-DOOR key: read by `crates/aterm/src/main.rs` before a window
    // exists (and by the Windows jump list), never by a running window. It is
    // deliberately off `prefs::editable_fields` — see its `DEFERRED_CONFIG_KEYS`
    // rationale — but it is a first-class scalar an operator writes by hand, so
    // Manual must complete it, hover it, and flag a misspelled VALUE instead of
    // shrugging at an unknown key.
    manual(
        "windowing_behavior",
        "Where a new terminal opens",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: WINDOWING_BEHAVIORS,
        }),
        &["launch", "attach", "instance", "window", "tab", "single"],
        true,
    ),
    // The five hand-written scalars (see the consts' note): live Config
    // fields with resolvers and env aliases, deliberately off the Settings
    // pages, previously "unknown to this aterm build" in Manual.
    manual(
        "font_hinting",
        "Glyph grid-fitting (Linux/Windows)",
        ConfigSchemaKind::Scalar(EditKind::Enum {
            options: FONT_HINTINGS,
        }),
        &["hint", "grid", "autohinter", "crisp", "linux", "windows", "stem"],
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
        "net",
        "Network drive",
        ConfigSchemaKind::Table,
        &["remote", "tls", "listener"],
        false,
    ),
    manual(
        "net.connections",
        "Saved remote connections",
        ConfigSchemaKind::StructuredList,
        &["remote", "dial", "tls", "endpoint"],
        true,
    ),
    manual(
        "net.connections.name",
        "Connection name",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["remote", "dial"],
        false,
    ),
    manual(
        "net.connections.host",
        "Connection host and port",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["remote", "address", "port"],
        false,
    ),
    manual(
        "net.connections.fingerprint",
        "Connection certificate fingerprint",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["remote", "tls", "certificate"],
        false,
    ),
    manual(
        "net.connections.token_file",
        "Connection token file",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["remote", "credential", "path"],
        false,
    ),
    manual(
        "net.connections.sid",
        "Connection session id",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["remote", "session", "pin"],
        false,
    ),
    manual(
        "net.connections.expect_nonce",
        "Connection launch nonce",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["remote", "session", "rebind", "pin"],
        false,
    ),
    manual(
        "update",
        "Software update source",
        ConfigSchemaKind::Table,
        &["upgrade", "release", "github"],
        false,
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
        "Package background service",
        ConfigSchemaKind::Scalar(EditKind::Bool),
        &["atpkg", "tools", "master"],
        true,
    ),
    manual(
        "packages.account",
        "Package index account",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["atpkg", "owner", "github"],
        true,
    ),
    manual(
        "packages.channel",
        "Package release channel",
        ConfigSchemaKind::Scalar(EditKind::Text),
        &["atpkg", "stable", "release"],
        true,
    ),
    manual(
        "packages.include",
        "Included packages",
        ConfigSchemaKind::StringList,
        &["atpkg", "filter", "allow"],
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
        "matrix_rain",
        "Matrix rain",
        ConfigSchemaKind::Table,
        &["rain", "effect", "phosphor"],
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
        "sparkle_words.orca",
        "Orca words",
        ConfigSchemaKind::Table,
        &["whale", "splash", "words"],
        false,
    ),
    manual(
        "sparkle_words.orca.extra_words",
        "Additional orca words",
        ConfigSchemaKind::StringList,
        &["whale", "lexicon", "include"],
        true,
    ),
    manual(
        "sparkle_words.orca.ignore_words",
        "Ignored orca words",
        ConfigSchemaKind::StringList,
        &["whale", "lexicon", "exclude"],
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
                     placeholder,
                     ..
                 }| ConfigSchemaEntry {
                    key,
                    label,
                    // Settings' one-line controls deliberately serialize these
                    // as arrays through `prefs::typed_item`; Manual authors TOML
                    // directly, so its completion must suggest `[]`, never the
                    // temporary comma-joined control representation.
                    kind: if crate::prefs::LIST_KEYS.contains(&key) {
                        ConfigSchemaKind::StringList
                    } else if crate::prefs::manual_collection_key(key) {
                        ConfigSchemaKind::TextOrStringList
                    } else {
                        ConfigSchemaKind::Scalar(kind)
                    },
                    placeholder,
                    keywords: crate::prefs::keywords_of(key),
                    native_scalar: !crate::prefs::manual_only_key(key),
                    manual_reset_safe: false,
                },
            )
            .collect::<Vec<_>>();
        for entry in MANUAL_SCHEMA {
            if schema.iter().any(|known| known.key == entry.key) {
                continue;
            }
            schema.push(ConfigSchemaEntry {
                key: entry.key,
                label: entry.label,
                kind: entry.kind,
                placeholder: String::new(),
                keywords: entry.keywords,
                native_scalar: false,
                manual_reset_safe: entry.manual_reset_safe,
            });
        }
        schema
    })
}

/// Resolve one schema key.
///
/// Every per-keystroke Manual path (completion, validation, hover help) and the
/// Settings Modified route resolve keys one at a time, so the ~217-entry
/// registry is searched through a once-built key-sorted index rather than
/// rescanned linearly on each lookup. Keys are unique — pinned by
/// `one_schema_registry_covers_native_manual_and_table_shapes_without_duplicates`
/// — so the binary search returns exactly the entry the former first-match
/// `find` returned.
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

/// The same rank used by Settings global search, exported from the schema
/// authority so Manual labels/keywords cannot silently diverge from completion.
///
/// `entry.key` and every keyword are ASCII-lowercase by construction (pinned by
/// `config_schema_keys_and_keywords_are_lowercase`), so they are compared as they
/// are — folding them allocated a `String` each plus the collecting `Vec`, roughly
/// six throwaway allocations per entry across the whole ~217-entry schema, for
/// every character typed. Only `label` genuinely needs folding, into the caller's
/// reusable `scratch`.
pub(crate) fn config_schema_match_score(
    entry: &ConfigSchemaEntry,
    query: &str,
    scratch: &mut String,
) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    scratch.clear();
    scratch.extend(
        entry
            .label
            .chars()
            .map(|character| character.to_ascii_lowercase()),
    );
    std::iter::once(scratch.as_str())
        .chain(std::iter::once(entry.key))
        .chain(entry.keywords.iter().copied())
        .filter_map(|candidate| candidate_match_score(candidate, query))
        .min()
}

/// THE search-rank ladder, shared by the config-schema authority above and by
/// Settings global search (`native_settings::field_match_score_in`).
///
/// Exact prefix beats word prefix beats substring beats an ordered subsequence,
/// and the subsequence rung is capped at 3 characters because a long fuzzy
/// subsequence is mostly an accident (`materialize` matching unrelated labels).
/// Lower is better; the caller takes the `min` over a candidate set.
///
/// The two surfaces are supposed to rank IDENTICALLY — a key that sorts first in
/// Settings should sort first in Manual completion — and before this was shared
/// that was maintained by hand in two places.
///
/// PRECONDITION, and it is load-bearing: `query` is non-empty and already
/// ASCII-lowercased, and `candidate` is already lowercase — folded by the caller
/// into its scratch buffer, or lowercase by construction and pinned by
/// `config_schema_keys_and_keywords_are_lowercase` /
/// `editable_field_keys_are_unique_and_lowercase`. The subsequence rung folds
/// ASCII case; on already-lowercase input that is indistinguishable from an
/// exact comparison, which is why both callers can share one ladder.
pub(crate) fn candidate_match_score(candidate: &str, query: &str) -> Option<u8> {
    if candidate.starts_with(query) {
        Some(0)
    } else if candidate
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|word| word.starts_with(query))
    {
        Some(1)
    } else if candidate.contains(query) {
        Some(2)
    } else if query.chars().count() <= 3 && is_subsequence(query, candidate) {
        Some(3)
    } else {
        None
    }
}

/// `haystack.to_ascii_lowercase().contains(needle)` without the per-call
/// allocation: the fold lands in a caller-owned buffer that a sweep reuses across
/// every schema entry. `needle` must already be ASCII-lowercased by the caller,
/// exactly as the allocating form required.
fn contains_ascii_folded(haystack: &str, needle: &str, scratch: &mut String) -> bool {
    scratch.clear();
    scratch.extend(
        haystack
            .chars()
            .map(|character| character.to_ascii_lowercase()),
    );
    scratch.contains(needle)
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut haystack = haystack.chars();
    needle.chars().all(|needle| {
        haystack
            .by_ref()
            .any(|candidate| candidate.eq_ignore_ascii_case(&needle))
    })
}

pub(crate) fn analyze(source: &str) -> ConfigAnalysis {
    let mut analysis = ConfigAnalysis {
        syntax: lex_toml(source),
        diagnostics: Vec::new(),
        omitted_diagnostics: 0,
        assist_index: build_assist_index(source),
    };
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        let mut too_large = ConfigAnalysis::too_large();
        too_large.syntax = analysis.syntax;
        return too_large;
    }

    let document = match source.parse::<toml_edit::DocumentMut>() {
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

    let config = match toml::from_str::<crate::app_config::Config>(source) {
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

    validate_registered_values(source, &document, &mut analysis);
    warn_runtime_semantics(source, &document, &config, &mut analysis);
    warn_compatibility_only_values(source, &document, &mut analysis);
    warn_unknown_values(source, &document, &mut analysis);
    analysis
}

/// Parser libraries report an unterminated value at byte `source.len()`. When
/// the file ends in a newline, that byte belongs to the editor's synthetic
/// trailing blank line, not the authored line containing the unfinished value.
/// Keep the zero-width diagnostic at the end of the final authored line so its
/// status, underline, and F8 target all name the same source location.
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

fn warn_compatibility_only_values(
    source: &str,
    document: &toml_edit::DocumentMut,
    analysis: &mut ConfigAnalysis,
) {
    for (key, item) in document.iter() {
        let path = crate::native_config_service::join_config_key_path("", key);
        warn_compatibility_only_item(source, item, &path, analysis);
    }
}

fn warn_compatibility_only_item(
    source: &str,
    item: &toml_edit::Item,
    path: &str,
    analysis: &mut ConfigAnalysis,
) {
    let authored = match item {
        toml_edit::Item::None => false,
        toml_edit::Item::Table(table) => !table.is_implicit(),
        toml_edit::Item::Value(_) | toml_edit::Item::ArrayOfTables(_) => true,
    };
    if authored && is_compatibility_only_key(path) {
        push_diagnostic(
            analysis,
            source,
            source_value_range(source, path)
                .or_else(|| item.span())
                .unwrap_or(0..source.len().min(1)),
            ConfigDiagnosticSeverity::Warning,
            compatibility_only_message(path),
        );
    }
    if let Some(table) = item.as_table_like() {
        for (key, child) in table.iter() {
            let child_path = crate::native_config_service::join_config_key_path(path, key);
            warn_compatibility_only_item(source, child, &child_path, analysis);
        }
    }
}

fn compatibility_only_message(key: &str) -> String {
    if let Some(retired) = retired_config_key(key) {
        return format!(
            "{} was removed; {key} has {} (the authored value will be preserved)",
            retired.feature,
            retired.effect_label.to_ascii_lowercase()
        );
    }
    format!("{key} is compatibility-only and has no effect in this build")
}

fn validate_registered_values(
    source: &str,
    document: &toml_edit::DocumentMut,
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
                if setting.key == crate::prefs::EDIT_CURSOR_STYLE
                    && value.trim().eq_ignore_ascii_case("underline")
                {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Warning,
                        "cursor_style = \"underline\" is retired and renders as \"bar\"; use \"bar\" explicitly"
                            .to_string(),
                    );
                    continue;
                }
                if !registered_enum_value_is_valid(setting.key, value, options) {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be one of: {}", setting.key, options.join(", ")),
                    );
                }
            }
            ConfigSchemaKind::Scalar(EditKind::Color) => {
                let Some(value) = item.as_str() else {
                    continue;
                };
                if crate::app_config::parse_hex_color(value).is_none() {
                    push_diagnostic(
                        analysis,
                        source,
                        range,
                        ConfigDiagnosticSeverity::Error,
                        format!("{} must be an RRGGBB or #RRGGBB color", setting.key),
                    );
                }
            }
            ConfigSchemaKind::Scalar(EditKind::Float | EditKind::Integer) => {
                let number = item
                    .as_float()
                    .or_else(|| item.as_integer().map(|value| value as f64));
                let Some(number) = number else { continue };
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
                    // Theme files are resolved by the capability-owning host. A
                    // syntactically valid custom name is therefore neutral here:
                    // calling it missing would be a false warning for installed
                    // themes this pure language service cannot inspect.
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
                    .is_some_and(|array| array.iter().all(toml_edit::Value::is_str))
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
                        .is_some_and(|array| array.iter().all(toml_edit::Value::is_str));
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
                if !item.is_table_like() {
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
    // `classify_link` needs a home solely to classify the supported `~` and
    // `~/...` forms. This sentinel is never used for I/O; absolute-path syntax
    // is intentionally interpreted for the target platform compiling aterm.
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

/// Conditional clamps are reported by the shared runtime-semantic validator,
/// which can explain the setting that raised the floor. Suppress the generic
/// static-range warning for the same value so Manual never presents two
/// competing effective values for one authored token.
fn runtime_semantics_owns_numeric_clamp(
    document: &toml_edit::DocumentMut,
    key: &str,
    number: f64,
) -> bool {
    match key {
        crate::prefs::EDIT_FONT_PX => true,
        "sparkle_words.ink.sweep_ms" => {
            number < 600.0
                && dotted_item(document, "sparkle_words.ink.loop")
                    .and_then(toml_edit::Item::as_bool)
                    .unwrap_or(false)
        }
        "matrix_rain.head_alpha" => {
            use crate::matrix_rain::{RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR};
            dotted_item(document, "matrix_rain.alpha")
                .and_then(toml_edit::Item::as_integer)
                .map_or(number < f64::from(RAIN_ALPHA_CAP), |alpha| {
                    number
                        < alpha.clamp(i64::from(RAIN_ALPHA_FLOOR), i64::from(RAIN_ALPHA_CAP)) as f64
                })
        }
        _ => false,
    }
}

/// Filesystem-backed half of Manual validation. Callers run this away from the
/// event loop, then merge it only if the document revision still matches.
#[cfg(test)]
pub(crate) fn analyze_host(source: &str, backend_gpu: bool) -> Vec<ConfigDiagnostic> {
    analyze_host_nested(
        source,
        backend_gpu,
        crate::net_listen::launched_inside_aterm(),
    )
}

/// [`analyze_host`] with the session-NESTING fact supplied by the caller.
///
/// The listener diagnostics branch on whether this process was launched inside
/// another aterm, which is process-global environment state. A test that wants a
/// specific arm passes it here rather than mutating the environment — otherwise
/// the suite's answers depend on where the suite happens to be run from (inside
/// aterm, every listener test sees the nested no-bind arm).
#[cfg(test)]
pub(crate) fn analyze_host_nested(
    source: &str,
    backend_gpu: bool,
    nested: bool,
) -> Vec<ConfigDiagnostic> {
    let Ok(config) = toml::from_str::<crate::app_config::Config>(source) else {
        return Vec::new();
    };
    let themes = crate::app_config::ThemeCatalog::discover();
    let assets = config.resolve_asset_catalog_with_themes(themes);
    analyze_host_with_assets(source, backend_gpu, &assets, nested)
}

/// Manual's host lane uses the exact asset catalog admitted with its document
/// generation. The legacy wrapper above is reserved for explicit CLI/tests that
/// do not already own a versioned snapshot.
pub(crate) fn analyze_host_with_assets(
    source: &str,
    backend_gpu: bool,
    assets: &crate::app_config::ConfigAssetCatalog,
    nested: bool,
) -> Vec<ConfigDiagnostic> {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return Vec::new();
    }
    let Ok(document) = source.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let Ok(config) = toml::from_str::<crate::app_config::Config>(source) else {
        return Vec::new();
    };
    // Draft-authored Trail/rainbow kitty paths must be validated against the draft, while
    // custom themes stay pinned to the exact parsed catalog admitted by the live
    // service. This runs only on the host worker (or explicit CLI/test wrapper).
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
            nested,
        ),
        &mut analysis,
    );
    analysis.diagnostics
}

/// Surface the same deterministic warnings as `--validate-config` without
/// importing its filesystem-backed font/theme/pack probes into the editor's
/// event-loop analysis path.
fn warn_runtime_semantics(
    source: &str,
    document: &toml_edit::DocumentMut,
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
    document: &toml_edit::DocumentMut,
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
    document: &toml_edit::DocumentMut,
    warning: &crate::diagnostics::ConfigSemanticWarning,
) -> Option<Range<usize>> {
    if let Some(range) = dynamic_map_semantic_warning_range(source, document, warning) {
        return Some(range);
    }

    if let Some(index) = indexed_warning_index(&warning.message, warning.key)
        && let Some(array) = dotted_item(document, warning.key).and_then(toml_edit::Item::as_array)
    {
        let source_index = if warning.key == crate::prefs::EDIT_FONT_VARIATION {
            // FontList trims and drops blank array entries while deserializing.
            // Match the warning's retained-entry ordinal so a preceding blank
            // string cannot shift the underline onto the wrong source token.
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
            .and_then(toml_edit::Value::span)
        {
            return Some(range);
        }
        // `toml_edit` does not promise child spans for every array reached
        // through synthesized tables. Recover the exact lexical element from
        // the already-valid right-hand side instead of widening to the array.
        if let Some(source_index) = source_index {
            let item_span = dotted_item(document, warning.key).and_then(toml_edit::Item::span);
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
        .or_else(|| dotted_item(document, warning.key).and_then(toml_edit::Item::span))
}

/// A semantic warning for a dynamic string map owns either the concrete member
/// key (bad/shadowed chord) or its concrete value (bad byte sequence/action).
/// The warning text is generated from the same map member using `Debug`, so an
/// exact prefix match is unambiguous even when a chord contains `]`, quotes, or
/// another member name.
fn dynamic_map_semantic_warning_range(
    source: &str,
    document: &toml_edit::DocumentMut,
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
    if key == "sparkle_words.custom.ink.colorway" {
        return custom_ink_colorway_is_valid(value);
    }
    if key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE {
        return crate::prefs::cursor_trail_style_canonical(value).is_some()
            || value
                .strip_prefix("pack:")
                .is_some_and(|id| !id.trim().is_empty());
    }
    // The typing-sound picker's domain is the synth's own roster + aliases
    // (`SoundVoice::parse`), through the same canonicaliser the Settings row
    // and `--validate-config` use.
    if key == crate::prefs::EDIT_TRAIL_SOUND_STYLE {
        return crate::prefs::trail_sound_style_canonical(value).is_some();
    }
    // `display_font` also accepts a MIX — 2..=3 distinct bundled ids joined by
    // `+` ("pixel+engraved"), authored by the Text & Fonts toggles. The same
    // domain the typed TOML writer enforces (`prefs::typed_item`).
    //
    // Parts are compared CANONICALIZED, so a legacy spelling is neither
    // rejected (a mix that was valid before the rename must stay valid) nor a
    // way to name one face twice ("pixel+minecraft" is a duplicate).
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
        // `trail_sound_style` aliases live on the synth (`SoundVoice::ALIASES`)
        // and are honoured by the early return in
        // `registered_enum_value_is_valid`, like the cursor-trail table.
        crate::prefs::EDIT_WINDOW_COLORSPACE => WINDOW_COLORSPACE_ALIASES,
        crate::prefs::EDIT_BACKGROUND_MATERIAL => BACKGROUND_MATERIAL_ALIASES,
        // The pre-rename face ids. Accepted, never OFFERED — see
        // `prefs::LEGACY_DISPLAY_FONT_IDS`.
        crate::prefs::EDIT_DISPLAY_FONT => crate::prefs::LEGACY_DISPLAY_FONT_IDS,
        "sparkle_words.custom.burst.kind" => CUSTOM_BURST_KIND_ALIASES,
        // Windows Terminal's own value spellings, plus the hyphen/compact forms
        // `aterm_cli::WindowingBehavior::parse` accepts. Accepted, never OFFERED
        // — completion suggests the two canonical names.
        "windowing_behavior" => WINDOWING_BEHAVIOR_ALIASES,
        "right_click" => RIGHT_CLICK_ALIASES,
        "tab_menu_chord" => TAB_MENU_CHORD_ALIASES,
        _ => &[],
    }
}

fn custom_ink_colorway_is_valid(value: &str) -> bool {
    aterm_effects::spec::custom_colorway_is_valid(value)
}

/// Parse the same plain or `dark:…,light:…` shape as Config's runtime
/// resolver, while rejecting segments that the permissive fallback would
/// silently ignore. Names are intentionally not classified as built-in versus
/// custom because user theme files are resolved by the capability-owning host.
pub(crate) fn theme_names(value: &str) -> Result<Vec<&str>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("theme must name a built-in or user theme".to_string());
    }
    let is_split = value.split(',').any(|segment| {
        segment.split_once(':').is_some_and(|(mode, _)| {
            matches!(mode.trim().to_ascii_lowercase().as_str(), "dark" | "light")
        })
    });
    if !is_split {
        return Ok(vec![value]);
    }

    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for segment in value.split(',') {
        let (mode, name) = segment
            .split_once(':')
            .ok_or_else(|| format!("theme split segment {segment:?} needs dark: or light:"))?;
        let mode = mode.trim().to_ascii_lowercase();
        if !matches!(mode.as_str(), "dark" | "light") {
            return Err(format!("theme split mode {mode:?} must be dark or light"));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("theme split mode {mode} needs a theme name"));
        }
        if !seen.insert(mode.clone()) {
            return Err(format!("theme split mode {mode} appears more than once"));
        }
        names.push(name);
    }
    Ok(names)
}

fn warn_unknown_values(
    source: &str,
    document: &toml_edit::DocumentMut,
    analysis: &mut ConfigAnalysis,
) {
    for (key, item) in document.iter() {
        let path = crate::native_config_service::join_config_key_path("", key);
        warn_unknown_item(source, item, &path, analysis);
    }
}

fn warn_unknown_item(
    source: &str,
    item: &toml_edit::Item,
    path: &str,
    analysis: &mut ConfigAnalysis,
) {
    if is_compatibility_only_key(path) {
        // The compatibility walker already reports every concrete entry in an
        // inert subtree. Calling it "unknown" as well would give one authored
        // Orca key two contradictory recovery stories.
        return;
    }
    if path == crate::prefs::LEGACY_EDIT_DISPLAY_FONT {
        // DEPRECATED, not unknown. The key still parses and still applies (a
        // serde alias on `Config::display_font`); "unknown to this build" would
        // be a lie that reads like data loss. Say what to rename it to instead,
        // and return so the key gets exactly one story.
        let range = source_value_range(source, path)
            .or_else(|| item.span())
            .unwrap_or(0..source.len().min(1));
        push_diagnostic(
            analysis,
            source,
            range.clone(),
            ConfigDiagnosticSeverity::Warning,
            format!(
                "`{path}` is deprecated; rename it to `{current}` (the old key still \
                 applies — the faces are named for the letterform now, not a game)",
                current = crate::prefs::EDIT_DISPLAY_FONT
            ),
        );
        // The VALUE is still checked against the live domain. A deprecated key
        // is not an unvalidated one: without this, `game_font = "dooom"` would
        // silently lose the "must be one of" error it used to get, and the
        // rename would have made a typo harder to notice than before.
        if let Some(value) = item.as_str()
            && !registered_enum_value_is_valid(
                crate::prefs::EDIT_DISPLAY_FONT,
                value,
                crate::prefs::DISPLAY_FONT_OPTIONS,
            )
        {
            push_diagnostic(
                analysis,
                source,
                range,
                ConfigDiagnosticSeverity::Error,
                format!(
                    "{path} must be one of: {}",
                    crate::prefs::DISPLAY_FONT_OPTIONS.join(", ")
                ),
            );
        }
        return;
    }
    let schema = config_schema_entry(path);
    if schema.is_some_and(|entry| entry.kind == ConfigSchemaKind::StructuredList) {
        warn_unknown_structured_records(source, item, path, analysis);
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
            warn_unknown_item(source, child, &child_path, analysis);
        }
        return;
    }
    if schema.is_none() {
        push_diagnostic(
            analysis,
            source,
            source_value_range(source, path)
                .or_else(|| item.span())
                .unwrap_or(0..source.len().min(1)),
            ConfigDiagnosticSeverity::Warning,
            format!(
                "{path} is unknown to this aterm build; it will be preserved for forward compatibility"
            ),
        );
    }
}

fn warn_unknown_structured_records(
    source: &str,
    item: &toml_edit::Item,
    record_path: &str,
    analysis: &mut ConfigAnalysis,
) {
    if let Some(records) = item.as_array_of_tables() {
        for (record, table) in records.iter().enumerate() {
            warn_unknown_structured_table(
                source,
                table,
                record_path,
                record_path,
                record + 1,
                analysis,
            );
        }
        return;
    }
    let Some(records) = item.as_array() else {
        return;
    };
    for (record, value) in records.iter().enumerate() {
        let Some(table) = value.as_inline_table() else {
            continue;
        };
        warn_unknown_structured_table(
            source,
            table,
            record_path,
            record_path,
            record + 1,
            analysis,
        );
    }
}

fn warn_unknown_structured_table(
    source: &str,
    table: &dyn toml_edit::TableLike,
    table_path: &str,
    record_path: &str,
    record: usize,
    analysis: &mut ConfigAnalysis,
) {
    for (key, child) in table.iter() {
        let path = crate::native_config_service::join_config_key_path(table_path, key);
        warn_unknown_structured_item(source, child, &path, record_path, record, analysis);
    }
}

fn warn_unknown_structured_item(
    source: &str,
    item: &toml_edit::Item,
    path: &str,
    record_path: &str,
    record: usize,
    analysis: &mut ConfigAnalysis,
) {
    if config_schema_entry(path).is_none() {
        push_diagnostic(
            analysis,
            source,
            structured_record_value_range(source, path, record_path, record)
                .or_else(|| item.span())
                .unwrap_or(0..source.len().min(1)),
            ConfigDiagnosticSeverity::Warning,
            format!(
                "{path} is unknown in [[{record_path}]] record {record}; it will be preserved for forward compatibility"
            ),
        );
        return;
    }

    warn_structured_registered_value(source, item, path, record_path, record, analysis);
    if let Some(table) = item.as_table_like() {
        warn_unknown_structured_table(source, table, path, record_path, record, analysis);
    }
}

fn warn_structured_registered_value(
    source: &str,
    item: &toml_edit::Item,
    path: &str,
    record_path: &str,
    record: usize,
    analysis: &mut ConfigAnalysis,
) {
    let number = item
        .as_float()
        .or_else(|| item.as_integer().map(|value| value as f64));
    if let (Some(number), Some((min, max))) = (number, semantic_numeric_bounds(path))
        && !(min..=max).contains(&number)
    {
        push_diagnostic(
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
    push_diagnostic(
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

fn dotted_item<'a>(document: &'a toml_edit::DocumentMut, key: &str) -> Option<&'a toml_edit::Item> {
    let mut item = document.as_item();
    for segment in toml_edit::Key::parse(key).ok()? {
        item = item.as_table_like()?.get(segment.get())?;
    }
    Some(item)
}

/// Canonical semantic identity for one authored TOML key expression. Quoted
/// segments that contain dots remain quoted, while quote-style differences
/// around an otherwise bare segment collapse to the same TOML key.
fn canonical_key_expression(expression: &str) -> Option<String> {
    let segments = toml_edit::Key::parse(expression.trim()).ok()?;
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

/// Locate the concrete right-hand-side token for a scalar authored either as a
/// top-level/dotted assignment or under a table header. `toml_edit` represents
/// children of explicit tables through synthesized parent items whose span can
/// be the opening `[`, so diagnostics must recover the source token rather than
/// underline an unrelated header byte.
fn source_value_range(source: &str, dotted_key: &str) -> Option<Range<usize>> {
    let mut current_table = String::new();
    let mut multiline = None;
    let mut line_start = 0usize;
    for line_with_newline in source.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        multiline = scan.multiline;
        if started_in_multiline || scan.touched_multiline {
            line_start = line_start.saturating_add(line_with_newline.len());
            continue;
        }
        let code_end = scan.comment_start.unwrap_or(line.len());
        let code = &line[..code_end];
        if let Some(table) = table_header_identity(code) {
            current_table = table;
        } else if let Some(equal) = find_unquoted(code, b'=') {
            let authored_key = canonical_key_expression(&code[..equal])?;
            let path = join_key_expressions(&current_table, &authored_key);
            if path == dotted_key {
                let after_equal = &code[equal + 1..];
                let leading = after_equal.len() - after_equal.trim_start().len();
                let value = after_equal.trim();
                let start = line_start + equal + 1 + leading;
                return delimited_source_value_range(source, start)
                    .or_else(|| Some(start..start + value.len().max(1).min(source.len() - start)));
            }
        }
        line_start = line_start.saturating_add(line_with_newline.len());
    }
    None
}

/// Expand an array or inline-table value through its matching delimiter,
/// ignoring delimiters inside TOML strings and comments. Assignment discovery
/// above is line-oriented so it can continue tracking table identity, while
/// the value itself may legitimately span many lines.
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

/// Locate the concrete header token for a table that has no scalar right-hand
/// side. Host capability diagnostics (for example an otherwise empty
/// `[packages]`) still need a stable source address instead of falling back to
/// byte zero.
fn source_table_header_range(source: &str, dotted_key: &str) -> Option<Range<usize>> {
    let mut multiline = None;
    let mut line_start = 0usize;
    for line_with_newline in source.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        multiline = scan.multiline;
        if !started_in_multiline && !scan.touched_multiline {
            let code_end = scan.comment_start.unwrap_or(line.len());
            let code = &line[..code_end];
            if table_header_identity(code).as_deref() == Some(dotted_key) {
                let trimmed = trim_ascii_range(code, 0..code.len())?;
                return Some(line_start + trimmed.start..line_start + trimmed.end);
            }
        }
        line_start = line_start.saturating_add(line_with_newline.len());
    }
    None
}

/// Locate one whole structured-list record. Array-table records map to their
/// `[[path]]` header; inline-list records map to the corresponding `{ ... }`
/// token. This is used only when the record itself is inert and no leaf value
/// can honestly own the warning.
fn structured_record_root_range(
    source: &str,
    record_path: &str,
    record: usize,
) -> Option<Range<usize>> {
    let mut current_table = String::new();
    let mut current_record = 0usize;
    let mut multiline = None;
    let mut line_start = 0usize;
    for line_with_newline in source.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        multiline = scan.multiline;
        if started_in_multiline || scan.touched_multiline {
            line_start = line_start.saturating_add(line_with_newline.len());
            continue;
        }
        let code_end = scan.comment_start.unwrap_or(line.len());
        let code = &line[..code_end];
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
                let after_equal = &code[equal + 1..];
                let leading = after_equal.len() - after_equal.trim_start().len();
                let value_source = after_equal.trim();
                let value_start = line_start + equal + 1 + leading;
                let range = delimited_value_entries(value_source, b'[', b']')?
                    .into_iter()
                    .nth(record.saturating_sub(1))?;
                return Some(value_start + range.start..value_start + range.end);
            }
        }
        line_start = line_start.saturating_add(line_with_newline.len());
    }
    None
}

/// Locate a concrete member value inside one structured-list record.
///
/// `toml_edit` deliberately synthesizes table items while traversing an array
/// of tables.  A nested inline member such as `burst.chance` can therefore
/// inherit the span of the record's opening `[[` rather than the authored
/// `101`.  Re-scan only the bounded, already-valid TOML and parse each matching
/// right-hand side in isolation so diagnostics point at the actual leaf token.
fn structured_record_value_range(
    source: &str,
    dotted_key: &str,
    record_path: &str,
    record: usize,
) -> Option<Range<usize>> {
    let mut current_table = String::new();
    let mut current_record = 0usize;
    let mut active_record = false;
    let mut multiline = None;
    let mut line_start = 0usize;

    for line_with_newline in source.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        multiline = scan.multiline;
        if started_in_multiline || scan.touched_multiline {
            line_start = line_start.saturating_add(line_with_newline.len());
            continue;
        }

        let code_end = scan.comment_start.unwrap_or(line.len());
        let code = &line[..code_end];
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
            let after_equal = &code[equal + 1..];
            let leading = after_equal.len() - after_equal.trim_start().len();
            let value_source = after_equal.trim();
            let value_start = line_start + equal + 1 + leading;

            // The alternate inline-list representation keeps every record in
            // one assignment: `sparkle_words.custom = [{ ... }, { ... }]`.
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
        line_start = line_start.saturating_add(line_with_newline.len());
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

/// Resolve a dotted descendant within an inline table without trusting
/// `toml_edit`'s synthesized child spans. The containing document has already
/// parsed successfully; this bounded lexical pass exists solely to preserve
/// the exact byte address of the authored leaf.
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

/// Host handoff locator for Settings → Manual. It deliberately exposes only
/// a byte range inside already-authorized source text; filesystem resolution
/// remains entirely outside the language service.
pub(crate) fn config_key_source_range(source: &str, dotted_key: &str) -> Option<Range<usize>> {
    source_value_range(source, dotted_key)
}

/// `toml_edit` does not retain a child span for entries reached through an
/// implicit table synthesized by a `[table.header]`. Recover the concrete
/// value token from the bounded source so a dynamic-map type error underlines
/// the offending member rather than the table's opening bracket.
fn dynamic_map_member_value_range(
    source: &str,
    table_path: &str,
    member: &str,
) -> Option<Range<usize>> {
    let mut current = String::new();
    let mut multiline = None;
    let mut line_start = 0usize;
    let member = crate::native_config_service::join_config_key_path("", member);
    for line_with_newline in source.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        multiline = scan.multiline;
        if started_in_multiline || scan.touched_multiline {
            line_start = line_start.saturating_add(line_with_newline.len());
            continue;
        }
        let code_end = scan.comment_start.unwrap_or(line.len());
        let code = &line[..code_end];
        if let Some(table) = table_header_identity(code) {
            current = table;
        } else if current == table_path
            && let Some(equal) = find_unquoted(code, b'=')
        {
            let authored_key = canonical_key_expression(&code[..equal])?;
            if authored_key == member {
                let after_equal = &code[equal + 1..];
                let leading = after_equal.len() - after_equal.trim_start().len();
                let value = after_equal.trim();
                let start = line_start + equal + 1 + leading;
                return Some(start..start + value.len().max(1).min(source.len() - start));
            }
        }
        line_start = line_start.saturating_add(line_with_newline.len());
    }
    None
}

/// Lexically recover the concrete key token for a member of an explicit table.
/// `toml_edit::Key::span` may inherit the synthesized parent table's header
/// span, so it is not a trustworthy first choice for `[keybindings]` and
/// `[key_sequences]` children.
fn dynamic_map_member_key_range(
    source: &str,
    table_path: &str,
    member: &str,
) -> Option<Range<usize>> {
    let mut current = String::new();
    let mut multiline = None;
    let mut line_start = 0usize;
    let member = crate::native_config_service::join_config_key_path("", member);
    for line_with_newline in source.split_inclusive('\n') {
        let line = line_with_newline
            .strip_suffix('\n')
            .unwrap_or(line_with_newline);
        let started_in_multiline = multiline.is_some();
        let scan = scan_toml_fragment(line, multiline);
        multiline = scan.multiline;
        // The opening line of a multiline string still has an ordinary,
        // source-addressable key before `=`. Only continuation lines are
        // unavailable as assignments.
        if started_in_multiline {
            line_start = line_start.saturating_add(line_with_newline.len());
            continue;
        }
        let code_end = scan.comment_start.unwrap_or(line.len());
        let code = &line[..code_end];
        if let Some(table) = table_header_identity(code) {
            current = table;
        } else if current == table_path
            && let Some(equal) = find_unquoted(code, b'=')
            && canonical_key_expression(&code[..equal]).as_deref() == Some(member.as_str())
        {
            let key = trim_ascii_range(code, 0..equal)?;
            return Some(line_start + key.start..line_start + key.end);
        }
        line_start = line_start.saturating_add(line_with_newline.len());
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
    let _ = analysis.insert_diagnostic(ConfigDiagnostic {
        bytes: start..end,
        line,
        column,
        severity,
        message,
    });
}

#[cfg(test)]
pub(crate) fn assist(source: &str, caret: usize) -> ConfigAssist {
    let index = build_assist_index(source);
    assist_indexed(source, caret, &index)
}

/// Production completion path. `analysis` belongs to the exact document
/// revision being rendered, and its lexical index was built on the background
/// validation worker. The event-loop cost is one line lookup plus the bounded
/// current-line fragment, independent of document length.
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
        value_assist(source, line_start, code, equal, caret_in_line, &table)
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
        // Schema keys and keywords are ASCII-lowercase by construction (pinned by
        // `config_schema_keys_and_keywords_are_lowercase`) and `prefix` was folded
        // by the caller, so only the label needs folding — and it folds into a
        // reused buffer. This runs over the whole ~217-entry schema for every
        // character typed into the Manual editor.
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
    // `sort_by_key` re-evaluates its key closure on EVERY comparison, so the
    // `to_ascii_lowercase()` this used to do was ~2 allocations per comparison,
    // O(n log n) of them — and a no-op, since the key is already lowercase.
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
    ConfigAssist {
        help: exact.or_else(|| {
            completions
                .first()
                .map(|completion| completion.help.clone())
        }),
        completions,
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
        // `local` is a slice of the (lowercase) schema key and the keywords are
        // lowercase too, so only the label folds — into a reused buffer. See the
        // twin in `table_assist`.
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
    // No allocation in the sort key: `sort_by_key` calls this on every comparison
    // (~1,260 for the ~170 top-level settings), and the fold it used to do was a
    // no-op on an already-lowercase key.
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
    ConfigAssist {
        help: exact
            .or_else(|| {
                completions
                    .first()
                    .map(|completion| completion.help.clone())
            })
            .or_else(|| table_context_help(table)),
        completions,
    }
}

fn value_assist(
    source: &str,
    line_start: usize,
    code: &str,
    equal: usize,
    caret_in_line: usize,
    table: &str,
) -> ConfigAssist {
    let Some(local) = canonical_key_expression(&code[..equal]) else {
        return ConfigAssist::default();
    };
    let full_key = join_key_expressions(table, &local);
    if is_compatibility_only_key(&full_key) {
        return ConfigAssist {
            help: Some(compatibility_only_message(&full_key)),
            completions: Vec::new(),
        };
    }
    let Some(setting) = config_schema_entry(&full_key) else {
        return ConfigAssist {
            help: table_context_help(table),
            completions: Vec::new(),
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
        ConfigSchemaKind::Scalar(EditKind::Theme) => aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| format!("\"{name}\""))
            .collect(),
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
    let completions = literals
        .into_iter()
        .filter(|literal| {
            normalized.is_empty() || literal.to_ascii_lowercase().starts_with(&normalized)
        })
        .take(MAX_COMPLETIONS)
        .map(|literal| {
            let display = if full_key == "sparkle_words.feline.style" && literal == "\"paw\"" {
                "\"paw\" — legacy ink-only; no paw graphic".to_string()
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
        .collect();
    ConfigAssist {
        help: Some(setting_help(setting)),
        completions,
    }
}

fn table_context_help(table: &str) -> Option<String> {
    if is_compatibility_only_key(table) {
        return Some(compatibility_only_message(table));
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

fn setting_help(setting: &ConfigSchemaEntry) -> String {
    let kind = if setting.key == "sparkle_words.feline.style" {
        "\"cat\" renders the cat graphic; \"paw\" is legacy ink-only and renders no paw graphic"
            .to_string()
    } else if setting.key == "packages.account" {
        "GitHub owner slug (letters, digits, dot, underscore, or hyphen)".to_string()
    } else if setting.key == "packages.links" {
        "named absolute/~/ checkout paths or owner/repo slugs".to_string()
    } else {
        match setting.kind {
            ConfigSchemaKind::Scalar(EditKind::Float) => "number".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Integer) => "integer".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Bool) => "true / false".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Text) => "text".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Theme) => "color theme".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Color) => "RRGGBB or #RRGGBB".to_string(),
            ConfigSchemaKind::Scalar(EditKind::Enum { options }) => options.join(" / "),
            ConfigSchemaKind::StringList => "list of text values".to_string(),
            ConfigSchemaKind::TextOrStringList => "text or list of text values".to_string(),
            ConfigSchemaKind::DynamicStringMap => "table of named text values".to_string(),
            ConfigSchemaKind::StructuredList => "array of tables".to_string(),
            ConfigSchemaKind::Table => "table".to_string(),
            ConfigSchemaKind::Flexible => "text or inline table".to_string(),
        }
    };
    let range =
        semantic_numeric_bounds(setting.key).map(|(min, max)| format!(" · range {min}–{max}"));
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
    } else {
        format!(" · default {placeholder}")
    };
    let timing = crate::prefs::application_timing(setting.key)
        .map(|note| format!(" · {note}"))
        .unwrap_or_default();
    let environment = crate::prefs::environment_precedence(setting.key)
        .map(|note| format!(" · {note}"))
        .unwrap_or_default();
    let constraint = match setting.key {
        crate::prefs::EDIT_MINIMUM_CONTRAST => {
            " · translucent backgrounds enforce at least 4.5:1 text contrast"
        }
        crate::prefs::EDIT_BACKGROUND_OPACITY => {
            " · macOS GPU window path only; CPU and non-macOS GPU grids stay solid · translucent backgrounds enforce at least 4.5:1 text contrast"
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
            " · each entry is tag=value with a 1–4 byte OpenType axis tag; malformed entries are ignored"
        }
        crate::prefs::EDIT_FONT_WEIGHT => {
            " · applies only when the selected font provides a wght axis; static fonts ignore it"
        }
        crate::prefs::EDIT_FONT_FEATURES => {
            " · tokens are tag / +tag / -tag / tag=unsigned-value with 1–4 byte ASCII tags; malformed tokens are ignored"
        }
        crate::prefs::EDIT_FONT_THICKEN => {
            " · macOS CoreText only; parsed and preserved but inert on other platforms"
        }
        crate::prefs::EDIT_MOTION => crate::prefs::motion_auto_help(),
        crate::prefs::EDIT_ROBI => {
            " · an invited guest: off until you turn him on · once enabled, typing robi or robot makes him greet you · hidden under reduced motion or serious mode"
        }
        crate::prefs::EDIT_SECURE_KEYBOARD_ENTRY => {
            " · macOS only: blocks other processes from observing keystrokes (EnableSecureEventInput — the guard iTerm2 offers under this name) · held only while aterm is frontmost, per Apple's TN2150 fairness guidance, so other apps' global hotkeys and clipboard managers are suppressed only then · applied at launch and on every save"
        }
        crate::prefs::EDIT_NOTICE_SPARKLE => {
            " · decorative only: the post-update card wears a hue-cycling badge and a ring of twinkling sparkles · reduced motion keeps the colour and holds it still · no other notice is affected"
        }
        // Every SYNTH voice shares the one macOS-only output path, so they share
        // one platform caveat. Grown with the Sound menu: these keys are now
        // reachable from Settings, so Manual must state the same limit the panel
        // discloses rather than only doing so for the master and the volume.
        crate::prefs::EDIT_TRAIL_SOUNDS
        | crate::prefs::EDIT_TRAIL_SOUND_VOLUME
        | crate::prefs::EDIT_TONE_MELODY
        | crate::prefs::EDIT_TRAIL_SOUND_BED => {
            " · audio playback is macOS-only; parsed and preserved but inert on other platforms"
        }
        crate::prefs::EDIT_TRAIL_SOUND_RIFF => {
            " · quiets only the held-key sing-along song; its ribbon, star shower and dancing cat keep running · subordinate to trail_sounds and trail_sound_volume · audio playback is macOS-only"
        }
        // The BEL is the one sound trail_sound_volume does NOT reach — say so
        // where a Manual author is looking for the level control.
        crate::prefs::EDIT_BELL_SOUND => {
            " · the OS alert sound (macOS NSBeep / Windows MessageBeep), so trail_sound_volume does not scale it · false keeps the visual bell flash and the urgent-window request · other platforms emit no beep"
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
            " · prompts only for unbracketed multiline paste; bracketed paste bypasses the dialog · live on every platform: the macOS sheet, the Windows dialog, and the Linux in-window banner"
        }
        crate::prefs::EDIT_OPTION_AS_META => {
            " · true sends ESC-prefixed Meta on every platform; false forwards OS-composed text when available while non-text Alt chords remain encoded"
        }
        crate::prefs::EDIT_ALLOW_NOTIFICATIONS => {
            " · desktop delivery is implemented on macOS and Windows; parsed but inert on other platforms"
        }
        crate::prefs::EDIT_ALLOW_OSC52_QUERY => {
            if cfg!(target_os = "linux") {
                " · an authorized query reads back only the clipboard selections aterm itself owns (a foreign app's X11 selection is not readable today); rate/budget gates still apply"
            } else {
                " · an authorized query reads the SYSTEM clipboard — including text copied in other apps — which is what remote vim/tmux clipboard sync needs; rate/budget gates still apply"
            }
        }
        crate::prefs::EDIT_ALLOW_WINDOW_OPS => {
            if cfg!(target_os = "linux") {
                " · manipulations (iconify, maximize, fullscreen, resize — move stays denied) apply to the window; reports beyond the window-title and text-grid-size fallbacks remain unanswered"
            } else {
                " · the GUI answers only XTWINOPS window-title and text-grid-size fallback reports; host manipulation and most state/geometry requests are ignored"
            }
        }
        crate::prefs::EDIT_SEARCH_HISTORY_LINES => {
            " · 0 searches only the live screen; a bounded index can report partial results for older retained history"
        }
        crate::prefs::EDIT_PACKAGES_ENABLED => {
            " · gates only the background package service; explicit package commands remain available when the trust-root gate is open"
        }
        crate::prefs::EDIT_PACKAGES_AUTO_UPDATE => {
            " · both packages.enabled and packages.auto_update must be true; the interval environment variable changes cadence only"
        }
        crate::prefs::EDIT_PACKAGES_AUTO_INSTALL => {
            " · takes effect on an update/package operation and cannot bypass $ATPKG_DISABLE or the package trust-root gate"
        }
        "packages.channel" => " · a blank value is treated as unset and resolves to stable",
        "update.owner" | "update.repo" => {
            " · safe GitHub slug only; invalid/blank values fall through · in-app updates are macOS-only"
        }
        "update.auto_apply" => " · in-app update application is macOS-only",
        "sparkle_words.lexicon" => {
            " · loaded on the host; unreadable or rejected files are skipped while built-ins remain active"
        }
        "sparkle_words.profanity.palette" => {
            " · each color must be RRGGBB or #RRGGBB; malformed entries are skipped"
        }
        "matrix_rain.hue" => " · matrix / theme / #RRGGBB; invalid values use stock matrix green",
        "net.connections.name" => " · nonempty [A-Za-z0-9_-] and unique; the first duplicate wins",
        "net.connections.host" => {
            " · must be host:port (or bracketed IPv6:port) with a nonzero port"
        }
        "net.connections.fingerprint" => {
            " · 64 hex SHA-256 digits, optionally prefixed sha256:; invalid pins refuse dialing"
        }
        "net.connections.sid" => {
            " · stored for compatibility but currently inert; the shipping rebind check is nonce-only"
        }
        "net.connections.expect_nonce" => {
            " · currently fails closed because the shipping wire cannot verify a remote launch nonce"
        }
        "sparkle_words.ink.sweep_ms" => " · loop = true raises the effective minimum to 600 ms",
        "matrix_rain.head_alpha" => " · effective minimum follows the resolved matrix_rain.alpha",
        crate::prefs::EDIT_WINDOW_PADDING_TOP => " · effective maximum follows window_padding",
        crate::prefs::EDIT_SCROLLBACK => " · 0 means unlimited scrollback",
        _ => "",
    };
    format!(
        "{} · {} · {}{}{}{}{}{}",
        setting.label,
        setting.key,
        kind,
        default,
        range.unwrap_or_default(),
        timing,
        environment,
        constraint,
    )
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

fn editable_value_selection(value: &str) -> Range<usize> {
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

/// Scan a bounded TOML fragment while carrying only the lexical state that may
/// cross a line boundary. Single-line strings and comments always terminate on
/// the current line; triple-basic and triple-literal strings do not. This is a
/// lexical pass rather than a parser on purpose: Manual assistance remains
/// useful while the line under construction is temporarily invalid TOML.
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
                // In a multiline basic string an escaped quote cannot begin
                // the closing delimiter. A trailing continuation backslash is
                // consumed here; lexical string state intentionally persists.
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

/// Build immutable line-entry state with one forward, byte-bounded pass on the
/// config-analysis worker. Fake headers and comments inside multiline strings
/// never enter the table context. Repeated table names share one interned value
/// so a large document does not clone its active table once per line.
fn build_assist_index(source: &str) -> ConfigAssistIndex {
    if source.len() > MAX_CONFIG_ANALYSIS_BYTES {
        return ConfigAssistIndex::default();
    }
    let mut index = ConfigAssistIndex {
        lines: Vec::new(),
        tables: vec![String::new()],
        authored: HashMap::new(),
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
                    // The source cap is far below u32::MAX bytes, hence below
                    // u32::MAX distinct non-empty table headers.
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

/// Resolve one caret from the worker-authored line state, then scan only its
/// already-size-capped current-line prefix. Assistance is inert on every line
/// touched by a multiline string.
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
    // Windowed, not reordered. `lex_toml` lexes one source line at a time, so
    // every span lies inside a single source line and `analysis.syntax` is a
    // run of per-source-line blocks in strictly increasing line order (only the
    // order *within* a block is scrambled: `lex_line` pushes the comment span
    // ahead of the key/value spans it follows). `project_viewport_with` emits
    // one projected line per source line, also strictly increasing. Those two
    // facts let each line scan a bounded window while pushing exactly the same
    // subsequence in exactly the same order a full rescan would — push order is
    // observable, both in paint prim order and in the compiled-UI fingerprint.
    let mut cursor = 0usize;
    for index in 0..projection.lines.len() {
        let window_start = projection.lines[index].source.start;
        // The one-window-per-source-line precondition, checked where it is spent:
        // consecutive windows are separated by at least the newline between their
        // source lines. A soft-wrap projection would put two windows *inside* one
        // source line and quietly invalidate the `scan_end` terminator below.
        // `viewport_projection_emits_one_window_per_source_line` pins the producer.
        debug_assert!(
            index == 0 || projection.lines[index - 1].source.end < window_start,
            "decorate_projection requires one projected window per source line"
        );
        // Every later window starts further right, so a span that already ends
        // at or before this one's start is dead for the whole rest of the
        // projection. Ends are not monotone inside a block, so this can stall
        // early — that only ever means scanning more, never scanning less.
        while cursor < analysis.syntax.len() && analysis.syntax[cursor].bytes.end <= window_start {
            cursor += 1;
        }
        // The next projected line begins on the next source line, so every span
        // of *this* source line starts before it: stopping there cannot drop a
        // span that intersects this window. Using the window's own end instead
        // would be wrong — on a horizontally sliced line the comment span sits
        // ahead of the key span it starts after, so an end-of-window terminator
        // would truncate the block before reaching the key.
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
        // Diagnostics stay a full sweep: `diagnostic_intersection` has a
        // zero-width clamp fallback that is not a pure intersection, and
        // MAX_DIAGNOSTICS keeps the set tiny.
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
    // The analysis budget is byte-based, but every slice still has to end on a
    // UTF-8 boundary. An oversized config can place a multibyte scalar across
    // the exact cap; rounding down keeps highlighting bounded and panic-free.
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
mod tests {
    use super::*;

    /// The completion and search paths compare schema keys and keywords against an
    /// already-lowercased prefix WITHOUT folding them (they are lowercase by
    /// construction, and folding them allocated per entry per keystroke). Pin the
    /// invariant so a capitalized key fails the build instead of silently
    /// disappearing from Manual completion and global search.
    #[test]
    fn config_schema_keys_and_keywords_are_lowercase() {
        for entry in config_schema() {
            assert_eq!(
                entry.key,
                entry.key.to_ascii_lowercase(),
                "schema key must be lowercase"
            );
            for keyword in entry.keywords {
                assert_eq!(
                    *keyword,
                    keyword.to_ascii_lowercase(),
                    "keyword of {} must be lowercase",
                    entry.key
                );
            }
        }
    }

    /// Every rung of the shared ladder, pinned by example. It had no direct
    /// coverage while it was written out twice; both copies could have drifted a
    /// rung and only a user would have noticed the ordering change.
    #[test]
    fn candidate_match_score_ranks_each_rung() {
        // 0 exact prefix, 1 word prefix, 2 substring, 3 short subsequence, None.
        assert_eq!(candidate_match_score("cursor_trail", "cur"), Some(0));
        assert_eq!(candidate_match_score("enable_cursor_trail", "cur"), Some(1));
        assert_eq!(candidate_match_score("thecursorthing", "cur"), Some(2));
        assert_eq!(candidate_match_score("colour_under_rail", "cur"), Some(3));
        assert_eq!(candidate_match_score("nothing_here", "cur"), None);
        // The subsequence rung is capped at 3 chars: a long fuzzy match is mostly
        // accidental, so it must NOT rank rather than rank last.
        assert_eq!(candidate_match_score("colour_under_railing", "curl"), None);
        // Lower wins, so a caller's `min` over a candidate set picks the best rung.
        assert!(candidate_match_score("cursor", "cur") < candidate_match_score("my_cursor", "cur"));
    }

    /// The precondition that lets Settings global search and the schema authority
    /// share ONE ladder: on already-lowercase input, the ladder's ASCII-case-folding
    /// subsequence rung is indistinguishable from an exact comparison. Settings used
    /// to carry its own case-SENSITIVE `is_subsequence`; sharing was only sound
    /// because every candidate on both sides is lowercase. Pin that equivalence, so
    /// if anyone ever feeds mixed case the difference surfaces here.
    #[test]
    fn subsequence_rung_agrees_with_exact_comparison_on_lowercase_input() {
        fn case_sensitive_subsequence(needle: &str, haystack: &str) -> bool {
            let mut needle = needle.chars();
            let mut wanted = needle.next();
            for character in haystack.chars() {
                if Some(character) == wanted {
                    wanted = needle.next();
                    if wanted.is_none() {
                        return true;
                    }
                }
            }
            wanted.is_none()
        }
        for entry in config_schema() {
            for candidate in std::iter::once(entry.key).chain(entry.keywords.iter().copied()) {
                for query in ["cur", "ab", "z", "trl"] {
                    assert_eq!(
                        is_subsequence(query, candidate),
                        case_sensitive_subsequence(query, candidate),
                        "case-folding and exact subsequence disagree on \
                         lowercase candidate {candidate:?} for {query:?}"
                    );
                }
            }
        }
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

    /// MIGRATION: the pre-rename display-face spellings LOAD. A config written
    /// before "Game Fonts" became "Display Faces" must not turn into an error,
    /// or a shipped key would have been deleted out from under the people who
    /// used it — so `game_font` stays valid, every retired id stays valid
    /// (including `mariokart`, whose face was removed with no substitute), and
    /// the only consequence is a warning naming the current spelling.
    ///
    /// "Deprecated" and "unknown" must not BOTH be said: the key still applies,
    /// and "unknown to this build; preserved for forward compatibility" reads
    /// like the setting stopped working.
    #[test]
    fn the_deprecated_display_font_key_warns_with_the_new_spelling_and_never_errors() {
        for value in crate::prefs::LEGACY_DISPLAY_FONT_IDS
            .iter()
            .copied()
            .chain(["pixel", "minecraft+zelda", "pixel+engraved"])
        {
            let source = format!("{} = {value:?}\n", crate::prefs::LEGACY_EDIT_DISPLAY_FONT);
            let analysis = analyze(&source);
            assert!(
                !analysis.has_errors(),
                "a config valid before the rename must still load ({value:?}): {:?}",
                analysis.diagnostics
            );
            let messages: Vec<&str> = analysis
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect();
            assert!(
                messages
                    .iter()
                    .any(|message| message.contains("is deprecated")
                        && message.contains(crate::prefs::EDIT_DISPLAY_FONT)),
                "the deprecation must name the new key ({value:?}): {messages:?}"
            );
            assert!(
                messages.iter().all(|message| !message.contains("unknown")),
                "a deprecated key is not an unknown key ({value:?}): {messages:?}"
            );
        }
        // The current key with a legacy VALUE: accepted, because the same
        // migration promise covers the ids, not just the key name.
        for value in crate::prefs::LEGACY_DISPLAY_FONT_IDS {
            let source = format!("{} = {value:?}\n", crate::prefs::EDIT_DISPLAY_FONT);
            assert!(
                !analyze(&source).has_errors(),
                "legacy id {value:?} must remain accepted under the current key"
            );
        }
        // …and a genuine typo is still an ERROR under either spelling. A
        // deprecated key is not an unvalidated one.
        for key in [
            crate::prefs::EDIT_DISPLAY_FONT,
            crate::prefs::LEGACY_EDIT_DISPLAY_FONT,
        ] {
            for bad in ["dooom", "pixel+dooom", "pixel+minecraft"] {
                assert!(
                    analyze(&format!("{key} = {bad:?}\n")).has_errors(),
                    "{bad:?} must not pass validation under {key}"
                );
            }
        }
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
        let syntax = analyze("theme = \"Nord\n");
        assert!(syntax.has_errors());
        assert!(
            syntax
                .first_error()
                .unwrap()
                .message
                .contains("TOML syntax")
        );

        let schema = analyze("font_px = \"large\"\n");
        assert!(schema.has_errors());
        assert!(
            schema
                .first_error()
                .unwrap()
                .message
                .contains("aterm schema")
        );

        let metadata = analyze("window_theme = \"sepia\"\n");
        assert!(metadata.has_errors());
        assert!(
            metadata
                .first_error()
                .unwrap()
                .message
                .contains("must be one of")
        );

        let malformed_theme = analyze("theme = \"dark:Dracula,sepia\"\n");
        assert!(malformed_theme.has_errors());
        assert!(
            malformed_theme
                .first_error()
                .unwrap()
                .message
                .contains("needs dark: or light:")
        );
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
        assert!(compatibility.diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("compatibility-only")
                && diagnostic.message.contains("has no effect")
        }));
    }

    #[test]
    fn color_assistance_names_the_exact_runtime_six_digit_domain() {
        for source in ["foreground = \"#12abEF\"\n", "foreground = \"12abEF\"\n"] {
            let analysis = analyze(source);
            assert!(
                !analysis.has_errors(),
                "runtime-valid six-digit color must be accepted: {:?}",
                analysis.diagnostics
            );
        }

        let short = analyze("foreground = \"#abc\"\n");
        assert!(
            short.has_errors(),
            "three-digit colors are not a runtime spelling"
        );
        assert!(
            short
                .first_error()
                .is_some_and(|diagnostic| diagnostic.message.contains("RRGGBB or #RRGGBB"))
        );
        let foreground = config_schema_entry(crate::prefs::EDIT_FOREGROUND).unwrap();
        let help = setting_help(foreground);
        assert!(help.contains("RRGGBB or #RRGGBB"), "{help}");
        assert!(!help.contains("#RGB or"), "{help}");
    }

    #[test]
    fn orca_subtree_round_trips_with_warnings_and_is_never_completed() {
        let source = r#"[sparkle_words.orca]
enabled = true
extra_words = ["whale"]
ignore_words = ["skip"]
future_splash = true
"#;
        let document = source.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            document.to_string(),
            source,
            "compatibility syntax round-trips"
        );
        assert!(
            toml::from_str::<crate::app_config::Config>(source).is_ok(),
            "the runtime parser keeps accepting suspended Orca configuration"
        );

        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        let messages = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        for key in [
            "sparkle_words.orca",
            "sparkle_words.orca.enabled",
            "sparkle_words.orca.extra_words",
            "sparkle_words.orca.ignore_words",
            "sparkle_words.orca.future_splash",
        ] {
            assert!(is_compatibility_only_key(key));
            assert!(
                messages.iter().any(|message| {
                    message.contains(key)
                        && message.contains("compatibility-only")
                        && message.contains("has no effect")
                }),
                "missing compatibility diagnostic for {key}: {messages:?}"
            );
        }
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("is unknown")),
            "one inert key must not also receive an unknown-key story: {messages:?}"
        );

        let table_source = "[sparkle_words.or";
        let table = assist(table_source, table_source.len());
        assert!(
            table
                .completions
                .iter()
                .all(|completion| !completion.insertion.contains("orca")),
            "the compatibility-only table must not be suggested: {table:?}"
        );

        let key_source = "[sparkle_words.orca]\nextra";
        let key = assist(key_source, key_source.len());
        assert!(key.completions.is_empty(), "Orca keys are inert: {key:?}");
        assert!(
            key.help
                .as_deref()
                .is_some_and(|help| help.contains("compatibility-only"))
        );

        let value_source = "[sparkle_words.orca]\nenabled = ";
        let value = assist(value_source, value_source.len());
        assert!(
            value.completions.is_empty(),
            "Orca values must not suggest active runtime choices: {value:?}"
        );
        assert!(
            value
                .help
                .as_deref()
                .is_some_and(|help| help.contains("compatibility-only"))
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
        let config = toml::from_str::<crate::app_config::Config>(source)
            .expect("retired feline keys remain loadable for compatibility");
        let feline = config
            .sparkle_words
            .as_ref()
            .and_then(|sparkle| sparkle.feline.as_ref())
            .expect("feline table parsed");
        assert_eq!(feline.idle, Some(false));
        assert_eq!(feline.gaze, Some(false));
        assert_eq!(feline.color.as_deref(), Some("#112233"));
        assert_eq!(feline.intensity, Some(0.25));

        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        let expected = [
            (
                "sparkle_words.feline.idle",
                "Keyword Kitty idle animation was removed; sparkle_words.feline.idle has no effect (the authored value will be preserved)",
            ),
            (
                "sparkle_words.feline.gaze",
                "Keyword Kitty gaze tracking was removed; sparkle_words.feline.gaze has no effect (the authored value will be preserved)",
            ),
            (
                "sparkle_words.feline.color",
                "Keyword Kitty tint control was removed; sparkle_words.feline.color has no effect (the authored value will be preserved)",
            ),
            (
                "sparkle_words.feline.intensity",
                "Keyword Kitty opacity control was removed; sparkle_words.feline.intensity has no effect (the authored value will be preserved)",
            ),
        ];
        assert_eq!(analysis.diagnostics.len(), expected.len());
        for (key, message) in expected {
            assert!(crate::prefs::manual_only_key(key));
            assert!(is_compatibility_only_key(key));
            assert_eq!(
                retired_config_key(key).map(|entry| entry.effect_label),
                Some("No effect")
            );
            assert_eq!(
                config_schema_entry(key).map(|entry| entry.native_scalar),
                Some(false),
                "retired scalar metadata must not create a native control"
            );
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message == message),
                "missing exact compatibility diagnostic for {key}: {:?}",
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
            assistance.help.as_deref(),
            Some(
                "Keyword Kitty idle animation was removed; sparkle_words.feline.idle has no effect (the authored value will be preserved)"
            )
        );
    }

    #[test]
    fn legacy_paw_mode_is_manual_only_and_completion_names_its_real_effect() {
        const KEY: &str = "sparkle_words.feline.style";
        assert!(crate::prefs::manual_only_key(KEY));
        assert!(!is_compatibility_only_key(KEY));
        assert_eq!(
            config_schema_entry(KEY).map(|entry| entry.native_scalar),
            Some(false),
            "paw has a real ink-only effect, but no honest native choice surface"
        );
        let source = "[sparkle_words.feline]\nstyle = \"paw\"\n";
        assert!(analyze(source).diagnostics.is_empty());

        let value_source = "[sparkle_words.feline]\nstyle = ";
        let assistance = assist(value_source, value_source.len());
        let paw = assistance
            .completions
            .iter()
            .find(|completion| completion.insertion == "\"paw\"")
            .expect("Manual keeps the compatibility value available");
        assert_eq!(paw.display, "\"paw\" — legacy ink-only; no paw graphic");
        for help in [assistance.help.as_deref(), Some(paw.help.as_str())] {
            assert!(help.is_some_and(|help| {
                help.contains(
                    "\"cat\" renders the cat graphic; \"paw\" is legacy ink-only and renders no paw graphic",
                )
            }));
        }
    }

    #[test]
    fn retired_bottom_hud_keys_are_preserved_but_never_presented_as_active_settings() {
        let keys = ["show_hud", "show_resources_hud", "show_engine_hud"];
        let source = "show_hud = true\nshow_resources_hud = false\nshow_engine_hud = true\n";
        let document = source.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            document.to_string(),
            source,
            "Manual must not destructively rewrite retired configuration"
        );
        assert!(
            toml::from_str::<crate::app_config::Config>(source).is_ok(),
            "retired compatibility keys remain loadable while their values are inert"
        );

        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        assert_eq!(
            analysis.diagnostics.len(),
            keys.len(),
            "each authored retired key gets one precise warning"
        );
        let active_fields = crate::prefs::editable_fields(&crate::app_config::Config::default());
        for key in keys {
            assert!(is_compatibility_only_key(key));
            let retired = retired_config_key(key).expect("retired HUD metadata");
            assert_eq!(retired.key, key);
            assert_eq!(retired.feature, "Bottom HUD");
            assert_eq!(retired.effect_label, "No effect");
            assert!(
                config_schema_entry(key).is_none(),
                "retired HUD keys are not active Manual schema entries"
            );
            assert!(
                active_fields.iter().all(|field| field.key != key),
                "retired HUD keys must stay out of Settings and Advanced"
            );

            let matching = analysis
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.message.contains(key))
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "{key}: {:?}", analysis.diagnostics);
            assert_eq!(matching[0].severity, ConfigDiagnosticSeverity::Warning);
            assert!(matching[0].message.contains("Bottom HUD was removed"));
            assert!(matching[0].message.contains("has no effect"));
            assert!(matching[0].message.contains("will be preserved"));
            assert!(!matching[0].message.contains("unknown"));
            assert!(!matching[0].message.contains("forward compatibility"));

            let value_source = format!("{key} = ");
            let value = assist(&value_source, value_source.len());
            assert!(
                value.completions.is_empty(),
                "retired values have no active runtime choices: {value:?}"
            );
            assert!(
                value.help.as_deref().is_some_and(|help| {
                    help.contains("Bottom HUD was removed") && help.contains("has no effect")
                }),
                "retired value help must tell the same precise story: {value:?}"
            );
        }

        let key_source = "show_";
        let key_assist = assist(key_source, key_source.len());
        assert!(
            key_assist.completions.iter().all(|completion| {
                keys.iter()
                    .all(|key| !completion.insertion.starts_with(key))
            }),
            "retired Bottom HUD keys must never be suggested: {key_assist:?}"
        );
    }

    #[test]
    fn manual_rejects_non_text_dynamic_map_values_even_when_gui_serde_ignores_them() {
        let source = "[packages.links]\nay = 5\n";
        assert!(
            toml::from_str::<crate::app_config::Config>(source).is_ok(),
            "the GUI intentionally delegates packages.links to atpkg"
        );
        let analysis = analyze(source);
        assert!(analysis.has_errors());
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .contains("packages.links.ay must be text")
            })
            .expect("the delegated atpkg map still has a shared string-value schema");
        assert_eq!(&source[diagnostic.bytes.clone()], "5");

        for valid in [
            "[packages.links]\nay = \"~/ay\"\n",
            "[packages]\nlinks = { ay = \"~/ay\" }\n",
        ] {
            assert!(
                !analyze(valid).has_errors(),
                "valid dynamic string map: {valid:?}"
            );
        }
    }

    #[test]
    fn package_account_and_link_targets_share_the_runtime_classifiers() {
        let source = r#"[packages]
account = "bad owner"
[packages.links]
bad = "a b"
repo = "alabsystems/aterm"
home = "~/aterm"
"#;
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "runtime fallbacks remain saveable");
        let account = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("packages.account"))
            .expect("invalid package account is visible");
        assert_eq!(account.severity, ConfigDiagnosticSeverity::Warning);
        assert_eq!(&source[account.bytes.clone()], "\"bad owner\"");
        let link = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("packages.links.bad"))
            .expect("invalid package link target is visible");
        assert_eq!(link.severity, ConfigDiagnosticSeverity::Warning);
        assert_eq!(&source[link.bytes.clone()], "\"a b\"");
        assert!(
            analysis.diagnostics.iter().all(|diagnostic| !diagnostic
                .message
                .contains("packages.links.repo")
                && !diagnostic.message.contains("packages.links.home")),
            "the exact atpkg classifier accepts sanctioned repo and ~/ checkout shapes"
        );
        assert!(
            setting_help(config_schema_entry("packages.account").unwrap())
                .contains("GitHub owner slug")
        );
        assert!(
            setting_help(config_schema_entry("packages.links").unwrap()).contains("owner/repo")
        );
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
                    && diagnostic.message.contains("does not resolve")
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
    // NOT nested: the listener preflight arms branch on whether this process was
    // launched inside another aterm, and the whole suite is normally run FROM one.
    // Supplying the fact keeps the assertion hermetic instead of asserting whatever
    // the ambient environment happens to make true.
    fn listener_host_preflight_warnings_address_each_effective_source_token() {
        const CERT: &[u8] = include_bytes!("../../aterm-net/src/testdata/cert.der");
        const KEY: &[u8] = include_bytes!("../../aterm-net/src/testdata/key.pkcs8.der");
        let root = std::env::temp_dir().join(format!(
            "aterm-manual-listener-preflight-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cert = root.join("cert.der");
        let key = root.join("key.der");
        let missing_cert = root.join("missing-cert.der");
        let missing_key = root.join("missing-key.der");
        std::fs::write(&cert, CERT).unwrap();
        std::fs::write(&key, KEY).unwrap();
        let cert_token = format!("{:?}", cert.to_string_lossy());
        let key_token = format!("{:?}", key.to_string_lossy());

        let source = format!(
            "[net]\nlisten = \"not a bind address\"\ncert = {cert_token}\nkey = {key_token}\n"
        );
        let diagnostics = analyze_host_nested(&source, true, false);
        let invalid_address = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .contains("not a numeric IP:port bind address")
            })
            .unwrap_or_else(|| panic!("missing address preflight warning: {diagnostics:?}"));
        assert_eq!(
            &source[invalid_address.bytes.clone()],
            "\"not a bind address\""
        );

        let missing_cert_token = format!("{:?}", missing_cert.to_string_lossy());
        let source = format!(
            "[net]\nlisten = \"127.0.0.1:7100\"\ncert = {missing_cert_token}\nkey = {key_token}\n"
        );
        let diagnostics = analyze_host_nested(&source, true, false);
        let unreadable_cert = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.message.contains("certificate from net.cert")
                    && diagnostic.message.contains("unreadable")
            })
            .unwrap_or_else(|| panic!("missing certificate preflight warning: {diagnostics:?}"));
        assert_eq!(&source[unreadable_cert.bytes.clone()], missing_cert_token);

        let missing_key_token = format!("{:?}", missing_key.to_string_lossy());
        let source = format!(
            "[net]\nlisten = \"127.0.0.1:7100\"\ncert = {cert_token}\nkey = {missing_key_token}\n"
        );
        let diagnostics = analyze_host_nested(&source, true, false);
        let unreadable_key = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.message.contains("private key from net.key")
                    && diagnostic.message.contains("unreadable")
            })
            .unwrap_or_else(|| panic!("missing key preflight warning: {diagnostics:?}"));
        assert_eq!(&source[unreadable_key.bytes.clone()], missing_key_token);

        std::fs::write(&cert, b"malformed certificate").unwrap();
        let source =
            format!("[net]\nlisten = \"127.0.0.1:7100\"\ncert = {cert_token}\nkey = {key_token}\n");
        let diagnostics = analyze_host_nested(&source, true, false);
        let mut pair_tokens = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("certificate/key pair"))
            .map(|diagnostic| source[diagnostic.bytes.clone()].to_string())
            .collect::<Vec<_>>();
        pair_tokens.sort();
        let mut expected = vec![cert_token.clone(), key_token.clone()];
        expected.sort();
        assert_eq!(pair_tokens, expected);

        std::fs::write(&cert, CERT).unwrap();
        let diagnostics = analyze_host_nested(&source, true, false);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.message.starts_with("network listener")),
            "valid address/cert/key must preflight cleanly: {diagnostics:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn merged_lexicon_host_warnings_address_external_path_and_exact_custom_word() {
        // libtest names the thread after the test's FULL PATH, and `:` is not a
        // legal Windows path character — sanitized, not dropped, because the name
        // is what keeps the directory unique per test (same fix as
        // `diagnostics::tests::host_semantics_…`).
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
        let messages = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        for expected in ["palette[1]", "shift+entr", "no_such_action"] {
            assert!(
                messages.iter().any(|message| message.contains(expected)),
                "Manual must share the runtime diagnostic for {expected}: {messages:?}"
            );
        }
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
        );
        let mut expected_spans = vec![
            ("chord \"shift+entr\" invalid", "\"shift+entr\""),
            ("key_sequences[\"ctrl+x\"]: value invalid", "'\\q'"),
            ("unknown action \"no_such_action\"", "\"no_such_action\""),
        ];
        // The built-in-conflict warning follows the Cmd/Super suite gate
        // (`HARDCODED_SUPER_CHORDS`): on Linux the suite is compiled off, so a
        // cmd chord conflicts with nothing and the diagnostic must NOT appear.
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
        let actual = enums.iter().map(|(key, _)| *key).collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            "ambiguous_width",
            "background_material",
            "bidi",
            "cursor_style",
            "cursor_trail_style",
            "display_font",
            "motion",
            "predictive_echo",
            "sparkle_words.feline.style",
            "sparkle_words.profanity.style",
            "tab_title_format",
            "text_blending",
            "title_summary_provider",
            "title_summary_proxy_mode",
            "trail_sound_style",
            "window_colorspace",
            "window_theme",
            "window_title_format",
            // Manual-only (off `prefs::editable_fields` — see the
            // DEFERRED_CONFIG_KEYS rationale), but real enums an operator types
            // into aterm.toml, so they get the same domain coverage as the
            // rest. Calling any of them "unknown to this aterm build" was the
            // audit-2 item-9 false diagnostic.
            "windowing_behavior",
            "font_hinting",
            "font_subpixel",
            "right_click",
            "tab_menu_chord",
            "tab_band_height",
        ]);
        assert_eq!(actual, expected, "new enum needs language-domain coverage");

        let source_for = |key: &str, value: &str| format!("{key} = {value:?}\n");
        for (key, options) in &enums {
            for option in *options {
                let source = source_for(key, option);
                let analysis = analyze(&source);
                assert!(
                    !analysis.has_errors(),
                    "canonical {key}={option:?}: {:?}",
                    analysis.diagnostics
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
            // Every pre-rename face id, including the one whose face was
            // deleted: a config that was valid before the rename must still
            // LOAD without an error, whatever it renders with.
            (
                crate::prefs::EDIT_DISPLAY_FONT,
                crate::prefs::LEGACY_DISPLAY_FONT_IDS,
            ),
            // Windows Terminal's `useNew`/`useExisting` and the compact
            // spellings — every one `aterm_cli::WindowingBehavior::parse`
            // accepts must LOAD without an error, or Manual would red-flag a
            // value the front door happily honours.
            ("windowing_behavior", WINDOWING_BEHAVIOR_ALIASES),
            // The hand-written scalars' parser aliases (`RightClickGesture::
            // parse`, `TabMenuChord::parse`): accepted, never offered.
            ("right_click", RIGHT_CLICK_ALIASES),
            ("tab_menu_chord", TAB_MENU_CHORD_ALIASES),
        ] {
            for alias in aliases {
                let source = source_for(key, alias);
                let analysis = analyze(&source);
                assert!(
                    !analysis.has_errors(),
                    "runtime alias {key}={alias:?}: {:?}",
                    analysis.diagnostics
                );
            }
        }
        for &(alias, _) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            let source = source_for(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, alias);
            let analysis = analyze(&source);
            assert!(
                !analysis.has_errors(),
                "trail alias {alias:?}: {:?}",
                analysis.diagnostics
            );
        }
        // The typing-sound picker's aliases live on the synth's own roster.
        for &(alias, _) in aterm_effects::trail_sound::SoundVoice::ALIASES {
            let source = source_for(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
            let analysis = analyze(&source);
            assert!(
                !analysis.has_errors(),
                "typing-sound alias {alias:?}: {:?}",
                analysis.diagnostics
            );
        }
        for pack in ["pack:synthwave", "pack: local-pack "] {
            let source = source_for(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, pack);
            assert!(!analyze(&source).has_errors(), "valid trail pack {pack:?}");
        }
        for pack in ["pack:", "pack:   "] {
            let source = source_for(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, pack);
            assert!(analyze(&source).has_errors(), "empty trail pack {pack:?}");
        }
    }

    #[test]
    fn retired_underline_cursor_is_never_false_green_or_suggested_as_an_alias() {
        assert!(!CURSOR_STYLE_ALIASES.contains(&"underline"));
        let analysis = analyze("cursor_style = \"underline\"\n");
        assert!(
            !analysis.has_errors(),
            "the compatibility fallback remains loadable"
        );
        let warnings = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            warnings,
            [
                "cursor_style = \"underline\" is retired and renders as \"bar\"; use \"bar\" explicitly"
            ]
        );
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
        assert!(
            messages
                .iter()
                .any(|message| message.contains("sparkle_words.enabld is unknown"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("sparkle_words.feline.gzae is unknown"))
        );

        let allowed = [
            r#"[keybindings]
"cmd+shift+t" = "new_tab"
[key_sequences]
"shift+enter" = '\e[13;2u'
"#,
            r#"[packages]
enabled = true
account = "alabsystems"
channel = "stable"
include = ["ay"]
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
            r#"[[net.connections]]
name = "work"
host = "work.example:7100"
fingerprint = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
token_file = "~/.config/aterm/token"
"#,
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

[[net.connections]]
name = "one"
host = "one.example:7100"
fingerprint = "sha256:0101010101010101010101010101010101010101010101010101010101010101"

[[net.connections]]
name = "two"
host = "two.example:7100"
fingerprint = "sha256:0202020202020202020202020202020202020202020202020202020202020202"
token_flie = "~/.config/aterm/token"
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
        assert_eq!(unknown.len(), 2, "{:?}", analysis.diagnostics);
        assert!(unknown.iter().any(|diagnostic| {
            diagnostic.message.contains(
                "sparkle_words.custom.wrods is unknown in [[sparkle_words.custom]] record 1",
            )
        }));
        assert!(unknown.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("net.connections.token_flie is unknown in [[net.connections]] record 2")
        }));
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

[[net.connections]]
name = "one"
host = "one.example:7100"
fingerprint = "sha256:0101010101010101010101010101010101010101010101010101010101010101"

[[net.connections]]
name = "two"
host = "two.example:7100"
fingerprint = "sha256:0202020202020202020202020202020202020202020202020202020202020202"
token_file = "~/.config/aterm/token"
"##;
        let clean = analyze(clean_repeats);
        assert!(
            clean.diagnostics.is_empty(),
            "valid repeated records stay clean: {:?}",
            clean.diagnostics
        );
    }

    #[test]
    fn custom_burst_chance_help_and_structured_diagnostics_match_runtime_clamp() {
        const KEY: &str = "sparkle_words.custom.burst.chance";
        assert_eq!(semantic_numeric_bounds(KEY), Some((0.0, 100.0)));
        let help = setting_help(config_schema_entry(KEY).expect("burst chance schema"));
        assert!(help.contains("range 0–100"), "{help}");

        for chance in [0, 100] {
            let source = format!(
                "[[sparkle_words.custom]]\nwords = [\"zap\"]\nburst = {{ kind = \"starburst\", chance = {chance} }}\n"
            );
            let analysis = analyze(&source);
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .all(|diagnostic| !diagnostic.message.contains("outside the supported")),
                "valid {chance}% chance: {:?}",
                analysis.diagnostics
            );
        }

        let source = "[[sparkle_words.custom]]\nwords = [\"zap\"]\nburst = { kind = \"starburst\", chance = 101 }\n";
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "runtime clamp remains recoverable");
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.message.contains(KEY)
                    && diagnostic.message.contains("record 1")
                    && diagnostic.message.contains("0–100")
                    && diagnostic.message.contains("will be clamped")
            })
            .unwrap_or_else(|| panic!("missing burst chance warning: {:?}", analysis.diagnostics));
        assert_eq!(&source[diagnostic.bytes.clone()], "101");
    }

    #[test]
    fn dependent_runtime_clamps_have_exact_help_ranges_and_single_diagnostics() {
        let sweep_help = setting_help(
            config_schema_entry("sparkle_words.ink.sweep_ms").expect("ink sweep schema"),
        );
        assert!(
            sweep_help.contains("loop = true raises the effective minimum to 600 ms"),
            "{sweep_help}"
        );
        let head_help =
            setting_help(config_schema_entry("matrix_rain.head_alpha").expect("rain head schema"));
        assert!(
            head_help.contains("effective minimum follows the resolved matrix_rain.alpha"),
            "{head_help}"
        );

        let source = "[sparkle_words.ink]\nloop = true\nsweep_ms = 400\n\
[matrix_rain]\nalpha = 120\nhead_alpha = 20\n";
        let analysis = analyze(source);
        assert!(!analysis.has_errors());
        assert_eq!(
            analysis.diagnostics.len(),
            2,
            "each dependent clamp has one recovery story: {:?}",
            analysis.diagnostics
        );
        for (key, token, effective) in [
            ("sparkle_words.ink.sweep_ms", "400", "effectively 600 ms"),
            ("matrix_rain.head_alpha", "20", "effectively 120"),
        ] {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.starts_with(key))
                .unwrap_or_else(|| {
                    panic!(
                        "missing dependent clamp for {key}: {:?}",
                        analysis.diagnostics
                    )
                });
            assert_eq!(&source[diagnostic.bytes.clone()], token);
            assert!(
                diagnostic.message.contains(effective),
                "{}",
                diagnostic.message
            );
        }

        let boundaries = "[sparkle_words.ink]\nloop = true\nsweep_ms = 600\n\
[matrix_rain]\nalpha = 120\nhead_alpha = 120\n";
        let clean = analyze(boundaries);
        assert!(
            clean.diagnostics.is_empty(),
            "exact effective floors stay clean: {:?}",
            clean.diagnostics
        );
    }

    #[test]
    fn runtime_fallback_diagnostics_address_exact_array_and_record_tokens() {
        let fingerprint = "00".repeat(32);
        let source = format!(
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
burst = {{ kind = "glow", chance = 0 }}

[[net.connections]]
name = "bad/name"
host = "bad.example:7100"
fingerprint = "wrong"
sid = "legacy"

[[net.connections]]
name = "work"
host = " "
fingerprint = "{fingerprint}"

[[net.connections]]
name = "work"
host = "two.example:7100"
fingerprint = "{fingerprint}"
expect_nonce = "pin"
"##
        );
        let analysis = analyze(&source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);

        for (message_fragment, token) in [
            ("font_px is", "201"),
            ("font_variation[0]", "\"bad-axis\""),
            ("font_features[1]", "\"toolong\""),
            ("update.owner value", "\"bad/owner\""),
            ("matrix_rain.hue value", "\"blue\""),
            ("sparkle_words.profanity.palette[1]", "\"bad-color\""),
            ("record 1 has no nonblank word", "[\" \"]"),
            ("record 2 is inert", "0"),
            ("record 1 must be a nonempty", "\"bad/name\""),
            ("record 1 must be 64 hexadecimal", "\"wrong\""),
            ("record 1 is stored but currently inert", "\"legacy\""),
            ("record 2 is blank", "\" \""),
            ("record 3 duplicates", "\"work\""),
            ("record 3 currently makes dialing fail closed", "\"pin\""),
        ] {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.contains(message_fragment))
                .unwrap_or_else(|| {
                    panic!("missing {message_fragment:?}: {:?}", analysis.diagnostics)
                });
            assert_eq!(
                &source[diagnostic.bytes.clone()],
                token,
                "{}",
                diagnostic.message
            );
        }

        let inert_header = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("record 1 is inert"))
            .expect("inert record warning");
        assert_eq!(
            &source[inert_header.bytes.clone()],
            "[[sparkle_words.custom]]"
        );

        let inline = "sparkle_words.custom = [{ words = [\"quiet\"] }]\n";
        let inline_analysis = analyze(inline);
        let inert = inline_analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("record 1 is inert"))
            .expect("inline inert record warning");
        assert_eq!(&inline[inert.bytes.clone()], "{ words = [\"quiet\"] }");

        let multiline = "font_features = [\n  \"+ss01\",\n  \"toolong\",\n]\n";
        let multiline_analysis = analyze(multiline);
        let invalid_feature = multiline_analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("font_features[1]"))
            .expect("multiline feature warning");
        assert_eq!(&multiline[invalid_feature.bytes.clone()], "\"toolong\"");

        let missing_words = "[[sparkle_words.custom]]\nink = { colorway = \"rainbow\" }\n";
        let missing_words_analysis = analyze(missing_words);
        let no_words = missing_words_analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("has no nonblank word"))
            .expect("missing words warning");
        assert_eq!(
            &missing_words[no_words.bytes.clone()],
            "[[sparkle_words.custom]]"
        );
    }

    #[test]
    fn inert_or_partial_security_capabilities_warn_on_the_authored_true_token() {
        let source = "allow_osc52_query = true\nallow_window_ops = true\n";
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);

        // The window-ops phrasing is per-platform: Linux wires the manipulation
        // half (frame audit #4), so its honest residue is the unanswered
        // geometry reports; elsewhere the pre-wiring statement stands.
        let window_ops_phrase = if cfg!(target_os = "linux") {
            "remain unanswered"
        } else {
            "most state/geometry requests"
        };
        // The OSC 52 caveat states the widest grant per platform: the system
        // clipboard off-Linux (what the Query arm answers with), the
        // own-selections bound on X11.
        let osc52_phrase = if cfg!(target_os = "linux") {
            "selections aterm itself owns"
        } else {
            "system clipboard"
        };
        for message in [osc52_phrase, window_ops_phrase] {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.contains(message))
                .unwrap_or_else(|| panic!("missing {message:?}: {:?}", analysis.diagnostics));
            assert_eq!(&source[diagnostic.bytes.clone()], "true");
        }
    }

    #[test]
    fn manual_help_discloses_fallback_platform_and_package_precedence() {
        let help_for = |key: &str| setting_help(config_schema_entry(key).unwrap());
        assert!(help_for("font_px").contains("ignored, not clamped"));
        assert!(help_for("font_variation").contains("tag=value"));
        assert!(help_for("font_features").contains("tag=unsigned-value"));
        assert!(help_for("font_thicken").contains("macOS CoreText only"));
        assert!(help_for("trail_sounds").contains("audio playback is macOS-only"));
        assert!(help_for("columns").contains("default 80"));
        assert!(help_for("lines").contains("default 24"));
        assert!(
            help_for("confirm_multiline_paste")
                .contains("live on every platform")
        );
        assert!(
            help_for("allow_notifications")
                .contains("desktop delivery is implemented on macOS and Windows")
        );
        assert!(help_for("option_as_meta").contains("ESC-prefixed Meta on every platform"));
        assert!(help_for("update.owner").contains("in-app updates are macOS-only"));
        assert!(help_for("update.auto_apply").contains("macOS-only"));
        assert!(help_for(crate::prefs::EDIT_MOTION).contains(crate::prefs::motion_auto_help()));
        // The query help is platform-split and states the WIDEST grant: the
        // system clipboard off-Linux (what the Query arm actually answers
        // with), the own-selections bound on X11. "cannot return clipboard
        // contents" was two generations stale — the arm answers since the
        // OSC 52 read landed.
        assert!(help_for(crate::prefs::EDIT_ALLOW_OSC52_QUERY).contains(
            if cfg!(target_os = "linux") {
                "selections aterm itself owns"
            } else {
                "reads the SYSTEM clipboard"
            }
        ));
        assert!(
            help_for(crate::prefs::EDIT_ALLOW_WINDOW_OPS)
                .contains("window-title and text-grid-size")
        );
        assert!(help_for("matrix_rain.hue").contains("matrix / theme / #RRGGBB"));
        assert!(help_for("packages.channel").contains("resolves to stable"));
        let auto = help_for(crate::prefs::EDIT_PACKAGES_AUTO_UPDATE);
        assert!(auto.contains("both packages.enabled and packages.auto_update"));
        assert!(auto.contains("$ATPKG_UPDATE_INTERVAL_SECS"));
        assert!(auto.contains("0 runs once"));
        assert!(
            help_for(crate::prefs::EDIT_PACKAGES_AUTO_INSTALL)
                .contains("cannot bypass $ATPKG_DISABLE")
        );

        let source = "trail_sounds = true\ntrail_sound_volume = 0.4\n\
confirm_multiline_paste = true\nallow_notifications = true\n";
        let document = source.parse::<toml_edit::DocumentMut>().unwrap();
        let config = toml::from_str::<crate::app_config::Config>(source).unwrap();
        let mut analysis = ConfigAnalysis::default();
        append_semantic_warnings(
            source,
            &document,
            crate::diagnostics::config_backend_capability_warnings(
                &config,
                true,
                crate::diagnostics::ConfigCapabilityPlatform::Unsupported,
            ),
            &mut analysis,
        );
        // `confirm_multiline_paste` earns NO platform warning any more: the
        // confirm is live everywhere (Linux's in-window paste_banner included).
        assert!(
            !analysis
                .diagnostics
                .iter()
                .any(|warning| warning.message.starts_with("confirm_multiline_paste")),
            "a live-everywhere key must not carry a platform warning"
        );
        for (key, token) in [
            ("trail_sounds", "true"),
            ("trail_sound_volume", "0.4"),
            ("allow_notifications", "true"),
        ] {
            let warning = analysis
                .diagnostics
                .iter()
                .find(|warning| warning.message.starts_with(key))
                .unwrap_or_else(|| panic!("missing platform warning for {key}"));
            assert_eq!(&source[warning.bytes.clone()], token);
        }
    }

    #[test]
    fn gpu_post_effect_help_and_cpu_warnings_are_source_addressed() {
        let keys = [
            crate::prefs::EDIT_CURSOR_TRAIL_BLOOM,
            crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH,
            crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
            crate::prefs::EDIT_CURSOR_FIRE_SHIMMER,
            crate::prefs::EDIT_HDR_GLOW,
            crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST,
        ];
        for key in keys {
            let help = setting_help(config_schema_entry(key).unwrap());
            assert!(help.contains("GPU renderer only"), "{key}: {help}");
            assert!(help.contains("CPU renderer"), "{key}: {help}");
        }

        let source = "cursor_trail_bloom = false\n\
cursor_trail_bloom_strength = 1.25\n\
cursor_trail_bloom_radius = 3.5\n\
cursor_fire_shimmer = false\n\
hdr_glow = true\n\
cursor_glow_sdr_boost = 0.4\n";
        let document = source.parse::<toml_edit::DocumentMut>().unwrap();
        let config = toml::from_str::<crate::app_config::Config>(source).unwrap();
        let warnings = crate::diagnostics::config_backend_capability_warnings(
            &config,
            false,
            crate::diagnostics::ConfigCapabilityPlatform::MacOs,
        );
        assert_eq!(warnings.len(), keys.len(), "{warnings:?}");

        let mut analysis = ConfigAnalysis::default();
        append_semantic_warnings(source, &document, warnings, &mut analysis);
        for (key, token) in [
            (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "false"),
            (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH, "1.25"),
            (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS, "3.5"),
            (crate::prefs::EDIT_CURSOR_FIRE_SHIMMER, "false"),
            (crate::prefs::EDIT_HDR_GLOW, "true"),
            (crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST, "0.4"),
        ] {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.starts_with(key))
                .unwrap_or_else(|| panic!("missing CPU capability diagnostic for {key}"));
            assert_eq!(&source[diagnostic.bytes.clone()], token, "{key}");
            assert!(diagnostic.message.contains("requires the GPU renderer"));
        }

        assert!(
            crate::diagnostics::config_backend_capability_warnings(
                &config,
                true,
                crate::diagnostics::ConfigCapabilityPlatform::MacOs,
            )
            .is_empty(),
            "every authored post-effect has a GPU consumer"
        );
    }

    #[test]
    fn compositor_capability_help_and_warnings_are_backend_truthful_and_source_addressed() {
        let help_for = |key: &str| setting_help(config_schema_entry(key).unwrap());
        let opacity_help = help_for(crate::prefs::EDIT_BACKGROUND_OPACITY);
        assert!(
            opacity_help.contains("macOS GPU window path only"),
            "{opacity_help}"
        );
        assert!(
            opacity_help.contains("non-macOS GPU grids stay solid"),
            "{opacity_help}"
        );
        let material_help = help_for(crate::prefs::EDIT_BACKGROUND_MATERIAL);
        assert!(
            material_help.contains("GPU renderer only"),
            "{material_help}"
        );
        assert!(
            material_help.contains("background_opacity < 1"),
            "{material_help}"
        );
        assert!(material_help.contains("Mica Alt"), "{material_help}");
        let colorspace_help = help_for(crate::prefs::EDIT_WINDOW_COLORSPACE);
        assert!(
            colorspace_help.contains("macOS GPU window layer"),
            "{colorspace_help}"
        );
        assert!(colorspace_help.contains("SDR-only"), "{colorspace_help}");
        assert!(
            colorspace_help.contains("ExtendedLinearSrgb"),
            "{colorspace_help}"
        );

        let source = "background_opacity = 0.7\nbackground_material = \"hud\"\n\
window_colorspace = \"display-p3\"\n";
        let document = source.parse::<toml_edit::DocumentMut>().unwrap();
        let config = toml::from_str::<crate::app_config::Config>(source).unwrap();
        let mut analysis = ConfigAnalysis::default();
        append_semantic_warnings(
            source,
            &document,
            crate::diagnostics::config_backend_capability_warnings(
                &config,
                false,
                crate::diagnostics::ConfigCapabilityPlatform::MacOs,
            ),
            &mut analysis,
        );
        assert_eq!(analysis.diagnostics.len(), 3, "{:?}", analysis.diagnostics);
        for (key, token) in [
            (crate::prefs::EDIT_BACKGROUND_OPACITY, "0.7"),
            (crate::prefs::EDIT_BACKGROUND_MATERIAL, "\"hud\""),
            (crate::prefs::EDIT_WINDOW_COLORSPACE, "\"display-p3\""),
        ] {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.starts_with(key))
                .unwrap_or_else(|| panic!("missing capability diagnostic for {key}"));
            assert_eq!(&source[diagnostic.bytes.clone()], token);
            assert!(
                diagnostic.message.contains("no effect")
                    || diagnostic.message.contains("stays solid")
            );
        }

        assert!(
            crate::diagnostics::config_backend_capability_warnings(
                &config,
                true,
                crate::diagnostics::ConfigCapabilityPlatform::MacOs,
            )
            .is_empty(),
            "all three values have consumers on a translucent macOS GPU window"
        );
        let windows = crate::diagnostics::config_backend_capability_warnings(
            &config,
            true,
            crate::diagnostics::ConfigCapabilityPlatform::Windows,
        );
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

        let opaque: crate::app_config::Config =
            toml::from_str("background_material = \"sidebar\"\n").unwrap();
        let warnings = crate::diagnostics::config_backend_capability_warnings(
            &opaque,
            true,
            crate::diagnostics::ConfigCapabilityPlatform::MacOs,
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].key, crate::prefs::EDIT_BACKGROUND_MATERIAL);
        assert!(
            warnings[0]
                .message
                .contains("background_opacity resolves to 1")
        );
        assert!(
            crate::diagnostics::config_backend_capability_warnings(
                &opaque,
                true,
                crate::diagnostics::ConfigCapabilityPlatform::Windows,
            )
            .is_empty(),
            "Windows DWM material is independent of background opacity"
        );
        let unsupported = crate::diagnostics::config_backend_capability_warnings(
            &config,
            true,
            crate::diagnostics::ConfigCapabilityPlatform::Unsupported,
        );
        assert!(
            unsupported
                .iter()
                .any(|warning| { warning.key == crate::prefs::EDIT_BACKGROUND_MATERIAL })
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
    fn diagnostic_cap_evicts_a_warning_for_a_late_blocking_error() {
        let mut analysis = ConfigAnalysis::default();
        for index in 0..MAX_DIAGNOSTICS {
            push_diagnostic(
                &mut analysis,
                "x",
                0..1,
                ConfigDiagnosticSeverity::Warning,
                format!("warning {index}"),
            );
        }
        push_diagnostic(
            &mut analysis,
            "x",
            0..1,
            ConfigDiagnosticSeverity::Error,
            "late blocking error".to_string(),
        );
        assert_eq!(analysis.diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(analysis.has_errors());
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "late blocking error")
        );
        assert_eq!(analysis.omitted_diagnostics, 1);
        assert!(
            analysis
                .summary()
                .unwrap()
                .contains("1 additional diagnostic omitted")
        );
        analysis.omitted_diagnostics = 2;
        assert!(
            analysis
                .summary()
                .unwrap()
                .contains("2 additional diagnostics omitted")
        );
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
    fn literal_dotted_keys_are_unknown_and_cannot_impersonate_nested_schema_paths() {
        let source = r#""packages.include" = ["literal"]

[packages]
include = ["nested"]
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
                .any(|message| message.contains(r#""packages.include" is unknown"#)),
            "the literal segment must retain an unknown, forward-compatible identity: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("packages.include is unknown")),
            "the nested registered path must not be diagnosed as unknown: {messages:?}"
        );
        assert!(config_schema_entry(r#""packages.include""#).is_none());
        assert!(
            config_schema_entry("packages.include").is_some_and(|entry| entry.manual_reset_safe)
        );

        let literal_value = r#""packages.include" = "#;
        let literal_assist = assist(literal_value, literal_value.len());
        assert!(literal_assist.completions.is_empty());
        assert!(literal_assist.help.is_none());

        let quoted_nested_value = "[packages]\n\"include\" = ";
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
    fn typography_diagnostics_use_the_full_runtime_domains() {
        let analysis = analyze("font_px = 100\nfont_weight = 950\n");
        assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
        assert!(
            analysis
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("outside the supported")),
            "valid runtime values must not receive clamp warnings: {:?}",
            analysis.diagnostics
        );
        assert_eq!(
            semantic_numeric_bounds(crate::prefs::EDIT_FONT_PX),
            Some((6.0, 200.0))
        );
        assert_eq!(
            semantic_numeric_bounds(crate::prefs::EDIT_FONT_WEIGHT),
            Some((1.0, 1000.0))
        );
    }

    #[test]
    fn one_schema_registry_covers_native_manual_and_table_shapes_without_duplicates() {
        let schema = config_schema();
        let keys = schema
            .iter()
            .map(|entry| entry.key)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys.len(),
            schema.len(),
            "config schema keys must be unique"
        );

        for field in crate::prefs::editable_fields(&crate::app_config::Config::default()) {
            let entry = config_schema_entry(field.key).expect("native field is registered");
            let expected_kind = if crate::prefs::LIST_KEYS.contains(&field.key) {
                ConfigSchemaKind::StringList
            } else if crate::prefs::manual_collection_key(field.key) {
                ConfigSchemaKind::TextOrStringList
            } else {
                ConfigSchemaKind::Scalar(field.kind)
            };
            assert_eq!(entry.kind, expected_kind, "{}", field.key);
            assert_eq!(
                entry.native_scalar,
                !crate::prefs::manual_only_key(field.key),
                "{} has a lossless native scalar representation",
                field.key
            );
        }
        for (key, kind, reset_safe) in [
            ("packages.include", ConfigSchemaKind::StringList, true),
            ("packages.links", ConfigSchemaKind::DynamicStringMap, true),
            ("keybindings", ConfigSchemaKind::DynamicStringMap, true),
            ("key_sequences", ConfigSchemaKind::DynamicStringMap, true),
            (
                "sparkle_words.custom",
                ConfigSchemaKind::StructuredList,
                true,
            ),
            ("net.connections", ConfigSchemaKind::StructuredList, true),
        ] {
            let entry = config_schema_entry(key).unwrap();
            assert_eq!(entry.kind, kind, "{key}");
            assert_eq!(entry.manual_reset_safe, reset_safe, "{key}");
        }

        // Every dotted native scalar has a registered table-header prefix, so
        // table completion cannot lag the value/diagnostic registry.
        for entry in schema.iter().filter(|entry| entry.native_scalar) {
            let mut prefix = String::new();
            let segments = entry.key.split('.').collect::<Vec<_>>();
            for segment in segments.iter().take(segments.len().saturating_sub(1)) {
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(segment);
                assert!(
                    config_schema_entry(&prefix).is_some_and(|table| table.kind.is_table_header()),
                    "{} needs registered table header {prefix}",
                    entry.key
                );
            }
        }
    }

    #[test]
    fn manual_table_key_and_value_assistance_share_the_schema_registry() {
        let table = assist("[pack", 5);
        assert!(table.completions.iter().any(|completion| {
            completion.insertion == "[packages]" && completion.help.contains("Toolchain packages")
        }));

        let array_table_source = "[[sparkle_words.cus";
        let array_table = assist(array_table_source, array_table_source.len());
        assert!(array_table.completions.iter().any(|completion| {
            completion.insertion == "[[sparkle_words.custom]]"
                && completion.help.contains("array of tables")
        }));

        let package_source = "[packages]\ninc";
        let package_key = assist(package_source, package_source.len());
        assert!(package_key.completions.iter().any(|completion| {
            completion.insertion == "include = []"
                && completion.help.contains("list of text values")
        }));

        let bool_source = "[packages]\nenabled = ";
        let package_value = assist(bool_source, bool_source.len());
        assert_eq!(
            package_value
                .completions
                .iter()
                .map(|completion| completion.insertion.as_str())
                .collect::<Vec<_>>(),
            ["true", "false"]
        );

        let connection_source = "[[net.connections]]\nfin";
        let connection = assist(connection_source, connection_source.len());
        assert!(connection.completions.iter().any(|completion| {
            completion.insertion == "fingerprint = \"\""
                && completion.help.contains("certificate fingerprint")
        }));

        let map_context = assist("[keybindings]\n", "[keybindings]\n".len());
        assert!(
            map_context
                .help
                .as_deref()
                .is_some_and(|help| help.contains("table of named text values"))
        );
    }

    #[test]
    fn vec_string_fields_complete_as_valid_toml_arrays_not_settings_control_text() {
        for key in crate::prefs::LIST_KEYS {
            let assistance = assist(key, key.len());
            let completion = assistance
                .completions
                .iter()
                .find(|completion| completion.insertion == format!("{key} = []"))
                .unwrap_or_else(|| panic!("{key} must complete as a TOML array"));
            assert!(completion.help.contains("list of text values"), "{key}");
            let mut completed = (*key).to_string();
            completed.replace_range(completion.replacement.clone(), &completion.insertion);
            completed.push('\n');
            let analysis = analyze(&completed);
            assert!(
                !analysis.has_errors(),
                "{key} completion must deserialize: {:?}",
                analysis.diagnostics
            );
        }
        for font_list in [
            crate::prefs::EDIT_FALLBACK_FONTS,
            crate::prefs::EDIT_FONT_VARIATION,
        ] {
            assert!(matches!(
                config_schema_entry(font_list).unwrap().kind,
                ConfigSchemaKind::TextOrStringList
            ));
            let help = setting_help(config_schema_entry(font_list).expect("FontList schema"));
            assert!(help.contains("text or list of text values"), "{help}");
        }
    }

    #[test]
    fn every_offered_non_record_key_and_value_completion_is_error_free_after_acceptance() {
        for setting in config_schema().iter().filter(|setting| {
            setting.kind.is_assignable()
                && !is_compatibility_only_key(setting.key)
                && array_table_root(setting.key).is_none()
        }) {
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

            let key_source = format!("{base}{local}");
            let key_assist = assist(&key_source, key_source.len());
            let key_completion = key_assist
                .completions
                .iter()
                .find(|completion| completion.insertion.starts_with(&format!("{local} = ")))
                .unwrap_or_else(|| {
                    panic!(
                        "registered key {} must be offered from its exact prefix: {:?}",
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
    fn every_offered_structured_record_member_completion_is_error_free_after_acceptance() {
        for setting in config_schema().iter().filter(|setting| {
            setting.kind.is_assignable()
                && !is_compatibility_only_key(setting.key)
                && array_table_root(setting.key).is_some()
        }) {
            let record_root = array_table_root(setting.key).unwrap();
            let relative = setting
                .key
                .strip_prefix(record_root)
                .and_then(|suffix| suffix.strip_prefix('.'))
                .expect("record member path");
            let (parent, local) = relative
                .rsplit_once('.')
                .map_or(("", relative), |(parent, local)| (parent, local));
            let mut base = format!("[[{record_root}]]\n");
            if record_root == "net.connections" {
                for (required, value) in [
                    ("name", "one"),
                    ("host", "one.example:7100"),
                    ("fingerprint", "sha256:01"),
                ] {
                    if local != required {
                        base.push_str(&format!("{required} = {value:?}\n"));
                    }
                }
            } else {
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
            }

            let key_source = format!("{base}{local}");
            let key_assist = assist(&key_source, key_source.len());
            let key_completion = key_assist
                .completions
                .iter()
                .find(|completion| completion.insertion.starts_with(&format!("{local} = ")))
                .unwrap_or_else(|| {
                    panic!(
                        "record key {} must be offered from its exact prefix: {:?}",
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
    fn grid_size_completions_match_runtime_defaults_and_warn_before_runtime_clamps() {
        for (key, inserted, min, max) in [
            (crate::prefs::EDIT_COLUMNS, 80_u16, 20_u16, 500_u16),
            (crate::prefs::EDIT_LINES, 24_u16, 5_u16, 300_u16),
        ] {
            let completion = assist(key, key.len())
                .completions
                .into_iter()
                .find(|completion| completion.insertion == format!("{key} = {inserted}"))
                .unwrap_or_else(|| panic!("{key} must complete to its runtime default"));
            assert_eq!(
                &completion.insertion[completion.post_insert_selection.clone()],
                inserted.to_string()
            );
            let mut completed = key.to_string();
            completed.replace_range(completion.replacement, &completion.insertion);
            let analysis = analyze(&completed);
            assert!(
                analysis.diagnostics.is_empty(),
                "default completion must be clean: {:?}",
                analysis.diagnostics
            );
            let config: crate::app_config::Config = toml::from_str(&completed).unwrap();
            let authored = if key == crate::prefs::EDIT_COLUMNS {
                config.columns.unwrap()
            } else {
                config.lines.unwrap()
            };
            assert_eq!(authored, inserted);
            assert_eq!(authored.clamp(min, max), inserted);

            for outside in [min - 1, max + 1] {
                let source = format!("{key} = {outside}\n");
                let outside_analysis = analyze(&source);
                assert!(!outside_analysis.has_errors());
                assert!(outside_analysis.diagnostics.iter().any(|diagnostic| {
                    diagnostic.message.contains(key)
                        && diagnostic.message.contains("will be clamped")
                }));
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
        assert!(window.help.contains("System appearance"));
        assert!(window.help.contains("auto / light / dark"));

        let nested_source = "[sparkle_words]\nena";
        let nested = assist(nested_source, nested_source.len());
        assert!(
            nested
                .completions
                .iter()
                .any(|candidate| candidate.insertion == "enabled = true")
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
    fn completion_suppresses_authored_scalar_and_table_paths_but_accepts_valid_edits() {
        let top_level = "theme = \"Nord\"\n";
        assert!(!analyze(top_level).has_errors());
        let top_assist = assist(top_level, top_level.len());
        assert!(
            top_assist
                .completions
                .iter()
                .all(|completion| !completion.insertion.starts_with("theme = "))
        );
        let top_candidate = top_assist
            .completions
            .first()
            .expect("another top-level scalar completion");
        let mut completed_top = top_level.to_string();
        completed_top.replace_range(top_candidate.replacement.clone(), &top_candidate.insertion);
        assert!(
            !analyze(&completed_top).has_errors(),
            "accepted top-level completion must remain a valid config: {completed_top:?}"
        );

        let nested = "[packages]\nenabled = true\n";
        assert!(!analyze(nested).has_errors());
        let nested_assist = assist(nested, nested.len());
        assert!(
            nested_assist
                .completions
                .iter()
                .all(|completion| !completion.insertion.starts_with("enabled = "))
        );
        let nested_candidate = nested_assist
            .completions
            .iter()
            .find(|completion| completion.insertion == "include = []")
            .expect("another packages completion");
        let mut completed_nested = nested.to_string();
        completed_nested.replace_range(
            nested_candidate.replacement.clone(),
            &nested_candidate.insertion,
        );
        assert!(
            !analyze(&completed_nested).has_errors(),
            "accepted nested completion must remain a valid config: {completed_nested:?}"
        );

        let table_base = "theme = \"Nord\"\n";
        assert!(!analyze(table_base).has_errors());
        let table_source = format!("{table_base}\n[");
        let table_assist = assist(&table_source, table_source.len());
        let table_candidate = table_assist
            .completions
            .iter()
            .find(|completion| completion.insertion == "[packages]")
            .expect("un-authored table completion");
        let mut completed_table = table_source.clone();
        completed_table.replace_range(
            table_candidate.replacement.clone(),
            &table_candidate.insertion,
        );
        assert!(
            !analyze(&completed_table).has_errors(),
            "accepted table completion must remain a valid config: {completed_table:?}"
        );

        let authored_table = format!("{table_base}\n[packages]\n");
        let duplicate_table = format!("{authored_table}\n[");
        assert!(
            assist(&duplicate_table, duplicate_table.len())
                .completions
                .iter()
                .all(|completion| completion.insertion != "[packages]")
        );
    }

    #[test]
    fn completion_keeps_the_current_token_and_repeated_array_table_editable() {
        let scalar = "window_theme = \"dark\"\n";
        let scalar_assist = assist(scalar, 3);
        let scalar_candidate = scalar_assist
            .completions
            .iter()
            .find(|completion| completion.insertion == "window_theme")
            .expect("the assignment currently being edited remains completable");
        let mut completed_scalar = scalar.to_string();
        completed_scalar.replace_range(
            scalar_candidate.replacement.clone(),
            &scalar_candidate.insertion,
        );
        assert!(!analyze(&completed_scalar).has_errors());

        let header = "[packages]\n";
        let header_assist = assist(header, "[pack".len());
        let header_candidate = header_assist
            .completions
            .iter()
            .find(|completion| completion.insertion == "[packages]")
            .expect("the table header currently being edited remains completable");
        let mut completed_header = header.to_string();
        completed_header.replace_range(
            header_candidate.replacement.clone(),
            &header_candidate.insertion,
        );
        assert!(!analyze(&completed_header).has_errors());

        let first_record = r#"[[net.connections]]
name = "one"
host = "one.example:7100"
fingerprint = "sha256:01"
"#;
        assert!(!analyze(first_record).has_errors());
        let repeated_source = format!(
            "{first_record}\n[[\nname = \"two\"\nhost = \"two.example:7100\"\nfingerprint = \"sha256:02\"\n"
        );
        let repeated_caret = first_record.len() + "\n[[".len();
        let repeated_assist = assist(&repeated_source, repeated_caret);
        let repeated_candidate = repeated_assist
            .completions
            .iter()
            .find(|completion| completion.insertion == "[[net.connections]]")
            .expect("an authored array-table path remains repeatable");
        let mut completed_repeated = repeated_source.clone();
        completed_repeated.replace_range(
            repeated_candidate.replacement.clone(),
            &repeated_candidate.insertion,
        );
        assert!(
            !analyze(&completed_repeated).has_errors(),
            "accepted repeated array table must remain a valid config: {completed_repeated:?}"
        );

        let nested_repeated = r#"[[sparkle_words.custom]]
words = ["one"]
[sparkle_words.custom.burst]
kind = "nova"

[[sparkle_words.custom]]
words = ["two"]

[sparkle_words.custom.b"#;
        let nested_assist = assist(nested_repeated, nested_repeated.len());
        let nested_candidate = nested_assist
            .completions
            .iter()
            .find(|completion| completion.insertion == "[sparkle_words.custom.burst]")
            .expect("a nested table may repeat in a different array-table record");
        let mut completed_nested = nested_repeated.to_string();
        completed_nested.replace_range(
            nested_candidate.replacement.clone(),
            &nested_candidate.insertion,
        );
        assert!(
            !analyze(&completed_nested).has_errors(),
            "nested table completion must retain per-record array scope: {completed_nested:?}"
        );
    }

    #[test]
    fn assist_is_inert_inside_multiline_strings_and_ignores_fake_context() {
        for source in [
            r#"[sparkle_words.profanity]
note = """
[packages] # not a table
# not a comment
include =
"""
sty"#,
            r#"[sparkle_words.profanity]
note = '''
[packages] # not a table
# not a comment
include =
'''
sty"#,
        ] {
            for marker in ["[packages]", "# not a comment", "include ="] {
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
                    .all(|completion| !completion.insertion.starts_with("include = "))
            );
        }
    }

    #[test]
    fn escaped_triple_quotes_do_not_end_a_multiline_basic_string() {
        let source = r#"[sparkle_words.profanity]
note = """
escaped delimiter: \"""
[packages]
include =
"""
sty"#;
        let fake_include = source.find("include =").unwrap() + "include =".len();
        assert_eq!(
            assist(source, fake_include),
            ConfigAssist::default(),
            "an escaped first quote cannot begin the closing delimiter"
        );
        let after = assist(source, source.len());
        assert!(
            after
                .completions
                .iter()
                .any(|completion| completion.insertion.starts_with("style = "))
        );
    }

    #[test]
    fn worker_built_assist_index_matches_standalone_language_results() {
        for source in [
            "",
            "win",
            "[packages]\ninc",
            "[sparkle_words.profanity]\nnote = \"\"\"\n[packages]\n\"\"\"\nsty",
            "[keybindings]\n\"literal.key\" = \"value\"\n",
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
    fn worker_index_covers_large_document_line_boundaries_once() {
        let mut source = "[packages]\n".to_string();
        while source.len() < 400 * 1024 {
            source.push_str("# bounded filler\n");
        }
        source.push_str("inc");
        let analysis = analyze(&source);
        assert_eq!(
            analysis.assist_index.lines.len(),
            source.bytes().filter(|byte| *byte == b'\n').count() + 1
        );
        let indexed = assist_with_analysis(&source, source.len(), &analysis);
        assert!(
            indexed
                .completions
                .iter()
                .any(|completion| completion.insertion == "include = []")
        );
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
    fn setting_help_omits_empty_or_duplicated_default_wording() {
        for setting in config_schema() {
            let help = setting_help(setting);
            let lower = help.to_ascii_lowercase();
            assert!(!lower.contains("default default"), "{help}");
            assert!(!lower.contains("default  "), "{help}");
            assert!(!lower.ends_with("default "), "{help}");
        }

        let help_for = |key: &str| setting_help(config_schema_entry(key).unwrap());
        assert!(!help_for("cursor_blink").contains(" · default"));
        assert!(!help_for("tab_strip_rows").contains(" · default"));
        assert_eq!(
            help_for("window_theme")
                .to_ascii_lowercase()
                .matches("default")
                .count(),
            1
        );
        for key in ["columns", "lines", "gpu"] {
            let timing = crate::prefs::application_timing(key)
                .unwrap_or_else(|| panic!("missing application timing for {key}"));
            assert!(
                help_for(key).contains(timing),
                "Manual help must use the shared application timing for {key}: {timing}"
            );
        }
        assert!(!help_for("font_px").contains("Applies next launch"));
        for key in [
            crate::prefs::EDIT_COLUMNS,
            crate::prefs::EDIT_LINES,
            crate::prefs::EDIT_GPU,
            crate::prefs::EDIT_FONT_PX,
            crate::prefs::EDIT_FONT_FAMILY,
            crate::prefs::EDIT_TAB_STRIP_ROWS,
            crate::prefs::EDIT_STEM_GAMMA,
            crate::prefs::EDIT_SHELL,
            "net.listen",
            "net.cert",
            "net.key",
            "update.owner",
            "update.repo",
            "update.auto_apply",
            crate::prefs::EDIT_FALLBACK_FONTS,
            crate::prefs::EDIT_SYMBOL_FONT,
            crate::prefs::EDIT_EMOJI_FONT,
        ] {
            let note = crate::prefs::environment_precedence(key)
                .unwrap_or_else(|| panic!("missing environment precedence for {key}"));
            assert!(
                help_for(key).contains(note),
                "Manual help must disclose environment precedence for {key}"
            );
        }
        assert!(help_for(crate::prefs::EDIT_FONT_WEIGHT).contains("provides a wght axis"));
        assert!(
            help_for(crate::prefs::EDIT_SEARCH_HISTORY_LINES)
                .contains("0 searches only the live screen")
        );
        assert!(
            help_for(crate::prefs::EDIT_CONFIRM_MULTILINE_PASTE)
                .contains("bracketed paste bypasses the dialog")
        );
    }

    #[test]
    fn scrollback_completion_explains_the_unlimited_zero_value() {
        let source = "scrollback";
        let completion = assist(source, source.len())
            .completions
            .into_iter()
            .find(|completion| completion.insertion.starts_with("scrollback_lines = "))
            .expect("scrollback-lines completion");
        assert!(completion.help.contains("0 means unlimited scrollback"));
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
    }

    #[test]
    fn oversized_utf8_split_at_analysis_cap_is_panic_free() {
        let source = format!("{}é", "x".repeat(MAX_CONFIG_ANALYSIS_BYTES - 1));
        assert!(source.len() > MAX_CONFIG_ANALYSIS_BYTES);
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

    /// A config document whose every line is one of the shapes that can trip the
    /// windowed decoration: a bare comment, a table header, keys with a trailing
    /// comment (the span `lex_line` pushes AHEAD of the key it follows), a
    /// multibyte value, an empty line, and a line long enough that the projection
    /// must slice it horizontally instead of starting at column zero.
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

    /// Every projection shape the config editor can hand `decorate_projection`:
    /// each scroll anchor and caret position, across the geometry envelope that
    /// actually changes the window — the row budget, and the column budget that
    /// turns on horizontal slicing.
    fn for_each_config_projection(
        source: &str,
        mut inspect: impl FnMut(&EditorViewportProjection),
    ) {
        let mut store = crate::document_store::DocumentStore::new();
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

    /// The precondition `decorate_projection`'s windowed scan spends, written as a
    /// predicate so the pin below can prove itself with a negative control:
    /// consecutive rows are consecutive SOURCE lines, and are therefore separated
    /// by at least the newline between them.
    fn one_window_per_source_line(projection: &EditorViewportProjection) -> bool {
        projection.lines.windows(2).all(|pair| {
            pair[1].number == pair[0].number + 1 && pair[0].source.end < pair[1].source.start
        })
    }

    /// One projected row over an arbitrary byte window of `source` — the shape a
    /// soft-wrap projection would emit, which the real projector never does.
    fn projected_window(
        source: &str,
        number: usize,
        bytes: Range<usize>,
    ) -> crate::native_editor::EditorViewportLine {
        crate::native_editor::EditorViewportLine {
            number,
            column_start: bytes.start,
            text: source[bytes.clone()].to_string(),
            source: bytes,
            selections: Vec::new(),
            carets: Vec::new(),
            syntax: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// The cross-module precondition `decorate_projection`'s windowed scan spends:
    /// `project_viewport_with` emits exactly ONE window per source line, so the
    /// next projected row starts after every span of this one and is a sound scan
    /// terminator. Soft wrap — two windows inside one source line — would land
    /// `lines[i + 1].source.start` mid-line, where the comment span `lex_line`
    /// pushes ahead of the key span it follows trips the `break` and the key
    /// highlighting silently vanishes on wrapped rows with a trailing comment.
    /// Pin the producer here so that feature breaks THIS test, loudly, instead of
    /// the decoration, invisibly.
    #[test]
    fn viewport_projection_emits_one_window_per_source_line() {
        let source = projection_pin_source();
        let mut sliced_windows = 0usize;
        for_each_config_projection(&source, |projection| {
            for line in &projection.lines {
                assert!(
                    !line.text.contains('\n'),
                    "a projected window must stay inside one source line: {line:?}"
                );
                // A window that does not begin right after a newline began mid
                // source line: that is the horizontal slice, the only shape where
                // a span can start left of the window it belongs to.
                if line.source.start > 0 && source.as_bytes()[line.source.start - 1] != b'\n' {
                    sliced_windows += 1;
                }
            }
            assert!(
                one_window_per_source_line(projection),
                "decorate_projection stops each row's scan at `lines[i + 1].source.start`, \
                 which is only a sound terminator while every source line owns exactly one \
                 window: {projection:?}"
            );
        });
        assert!(
            sliced_windows > 0,
            "negative control: the grid must actually produce horizontally sliced \
             windows, otherwise it never exercises the mid-line case"
        );
        // Negative control for the predicate itself: the shape soft wrap would
        // emit — two windows inside source line 1 — must be REJECTED, otherwise
        // the pin above could never fail for the reason it was written.
        let wrapped = EditorViewportProjection {
            first_line: 1,
            total_lines: 1,
            lines: vec![
                projected_window(&source, 1, 0..7),
                projected_window(&source, 1, 7..14),
            ],
        };
        assert!(
            !one_window_per_source_line(&wrapped),
            "a two-window-per-source-line projection must not satisfy the precondition"
        );
    }

    /// The windowed scan has to push exactly the subsequence a full rescan would,
    /// in exactly the same order — push order is observable both in paint prim
    /// order and in the compiled-UI fingerprint. Oracle the optimization against
    /// the naive sweep over the whole projection grid, so a terminator or cursor
    /// change that drops (or reorders) a span fails here.
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
}
