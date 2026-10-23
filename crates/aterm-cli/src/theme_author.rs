// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Theme authoring commands (`check`, `new`, `export`, and `schema`).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;

use aterm_json::{Value, json};
use aterm_types::scheme::{
    self, APPEARANCE_VALUES, CatalogTheme, ContrastRow, MAX_THEME_TEXT_CHARS,
    MAX_USER_THEME_FILE_BYTES, ParsedTheme, THEME_KEYS, ThemeError, ThemeKey, ThemeKeyGroup,
    ThemeKeyLines, ThemeMeta, ThemeValueType, ThemeWarning,
};
use aterm_types::scheme_signature::{SignatureDefect, signature_report};
use aterm_types::{Appearance, Rgb};
use aterm_types::{scheme_import, scheme_install};

use crate::theme_cmd::{Outcome, ThemeHost, read_bounded, write_atomically};

/// Schema version of `check --format json` and `schema --format json`.
pub(crate) const DOCUMENT_VERSION: u64 = 2;

/// Exit code for `check` when a theme loads.
pub(crate) const EXIT_LOADS: u8 = 0;
/// Exit code for `check --strict` when a theme loads with warnings.
pub(crate) const EXIT_WARNINGS: u8 = 1;
/// Exit code for `check` when a theme does not load or on usage errors.
pub(crate) const EXIT_INVALID: u8 = 2;

#[derive(Default)]
struct Operands {
    positional: Vec<String>,
    values: BTreeMap<&'static str, String>,
    switches: BTreeSet<&'static str>,
}

fn operands(
    args: &[OsString],
    value_flags: &[&'static str],
    switches: &[&'static str],
) -> Result<Option<Operands>, String> {
    let mut out = Operands::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let arg = arg
            .to_str()
            .ok_or_else(|| format!("argument {:?} is not UTF-8", arg.to_string_lossy()))?;
        if matches!(arg, "--help" | "-h") {
            return Ok(None);
        }
        if !arg.starts_with('-') || arg == "-" {
            out.positional.push(arg.to_string());
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg, None),
        };
        if let Some(name) = switches.iter().find(|s| **s == flag) {
            if inline.is_some() {
                return Err(format!("{flag} takes no value"));
            }
            out.switches.insert(name);
            continue;
        }
        let Some(name) = value_flags.iter().find(|s| **s == flag) else {
            return Err(format!("unknown option {flag:?}"));
        };
        let value = match inline {
            Some(value) => value.to_string(),
            None => iter
                .next()
                .and_then(|v| v.to_str())
                .map(str::to_string)
                .ok_or_else(|| format!("{flag} needs a value"))?,
        };
        if value.is_empty() {
            return Err(format!("{flag} needs a value"));
        }
        if out.values.insert(name, value).is_some() {
            return Err(format!("{flag} is given twice"));
        }
    }
    Ok(Some(out))
}

fn one_positional(operands: &Operands, what: &str) -> Result<String, String> {
    match operands.positional.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("needs {what}")),
        _ => Err(format!("takes one {what}")),
    }
}

const fn appearance_word(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => APPEARANCE_VALUES[0],
        Appearance::Light => APPEARANCE_VALUES[1],
    }
}

fn parse_appearance(value: &str) -> Result<Appearance, String> {
    crate::theme_cmd::appearance_value(value)
}

fn hex(colour: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour.r, colour.g, colour.b)
}

fn ratio(hundredths: u32) -> f64 {
    f64::from(hundredths) / 100.0
}

fn ratio_text(hundredths: u32) -> String {
    format!("{}.{:02}:1", hundredths / 100, hundredths % 100)
}

fn json_line(value: &Value) -> String {
    let mut text = aterm_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    text.push('\n');
    text
}

fn usage(message: &str) -> Outcome {
    Outcome::usage_error(message)
}

fn help() -> Outcome {
    Outcome {
        stdout: crate::theme_cmd::HELP.to_string(),
        ..Outcome::default()
    }
}

fn failure(verb: &str, message: &str) -> Outcome {
    Outcome {
        stderr: format!("aterm theme {verb}: {message}\n"),
        code: 1,
        ..Outcome::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    const fn word(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Diagnostic {
    severity: Severity,
    code: &'static str,
    line: Option<usize>,
    key: Option<String>,
    message: String,
    foreign_format: Option<&'static str>,
}

impl Diagnostic {
    const fn error(code: &'static str, message: String) -> Self {
        Self {
            severity: Severity::Error,
            code,
            line: None,
            key: None,
            message,
            foreign_format: None,
        }
    }
}

struct Loaded {
    meta: ThemeMeta,
    scheme: aterm_types::ColorScheme,
    contrast: Vec<ContrastRow>,
    typography: scheme::ThemeTypography,
    bold_is_bright: Option<bool>,
}

struct CheckReport {
    file: String,
    id: Option<String>,
    loaded: Option<Loaded>,
    diagnostics: Vec<Diagnostic>,
    signature: Option<Vec<SignatureDefect>>,
}

impl CheckReport {
    fn warnings(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    fn status(&self) -> &'static str {
        match (&self.loaded, self.warnings()) {
            (None, _) => "invalid",
            (Some(_), 0) => "ok",
            (Some(_), _) => "warnings",
        }
    }

    fn exit_code(&self, strict: bool) -> u8 {
        let missed = self.signature.as_ref().map_or(0, Vec::len);
        match (&self.loaded, self.warnings() + missed) {
            (None, _) => EXIT_INVALID,
            (Some(_), n) if strict && n > 0 => EXIT_WARNINGS,
            (Some(_), _) => EXIT_LOADS,
        }
    }
}

fn warning_diagnostic(warning: ThemeWarning, lines: &ThemeKeyLines) -> Diagnostic {
    let (code, key, index) = match warning {
        ThemeWarning::LowTextContrast { .. } => ("low_text_contrast", ThemeKey::Foreground, None),
        ThemeWarning::FaintSelection { .. } => ("faint_selection", ThemeKey::Selection, None),
        ThemeWarning::AnsiMatchesBackground { slot } => {
            ("ansi_matches_background", ThemeKey::Palette, Some(slot))
        }
        ThemeWarning::LowCursorContrast { .. } => ("low_cursor_contrast", ThemeKey::Cursor, None),
        ThemeWarning::ChromeBelowFloor { key, .. } => ("chrome_below_floor", key, None),
        ThemeWarning::SearchMatchLikeSelection => (
            "search_match_like_selection",
            ThemeKey::SearchMatchColor,
            None,
        ),
        ThemeWarning::AppearanceMismatch { .. } => {
            ("appearance_mismatch", ThemeKey::Appearance, None)
        }
        ThemeWarning::SkippedNewerKeys { .. } => ("skipped_keys", ThemeKey::Schema, None),
    };
    Diagnostic {
        severity: Severity::Warning,
        code,
        line: lines.line(key, index),
        key: Some(scheme::theme_key_spec(key).spelling(index)),
        message: warning.to_string(),
        foreign_format: None,
    }
}

fn id_diagnostics(path: &Path) -> (Option<String>, Vec<Diagnostic>) {
    let warn = |code, message: String| Diagnostic {
        severity: Severity::Warning,
        ..Diagnostic::error(code, message)
    };
    let mut out = Vec::new();
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        out.push(warn(
            "symlink_not_followed",
            "the themes folder does not follow symlinks; copy the file in".to_string(),
        ));
    }
    if path.extension().and_then(|e| e.to_str()) != Some("conf") {
        out.push(warn(
            "not_conf",
            "the themes folder reads only files named <id>.conf".to_string(),
        ));
    }
    let Some(id) = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
    else {
        out.push(warn(
            "invalid_id",
            "the file name is not UTF-8, so no config can name it".to_string(),
        ));
        return (None, out);
    };
    if let Err(why) = scheme::validate_user_theme_name(&id) {
        out.push(warn(
            "invalid_id",
            format!("{id:?} cannot be a theme id: {why}"),
        ));
    } else if let Some(builtin) = scheme::builtin_theme(&id) {
        out.push(warn(
            "shadowed_by_builtin",
            format!(
                "{id:?} is the built-in theme {:?}, which always wins; rename the file",
                builtin.name
            ),
        ));
    }
    (Some(id), out)
}

fn check_file(path: &Path, host: &dyn ThemeHost) -> CheckReport {
    let file = path.display().to_string();
    let (id, id_warnings) = id_diagnostics(path);
    let invalid = |diagnostic: Diagnostic| CheckReport {
        file: file.clone(),
        id: id.clone(),
        loaded: None,
        diagnostics: vec![diagnostic],
        signature: None,
    };
    let bytes = match read_bounded(path) {
        Ok(bytes) => bytes,
        Err(message) => return invalid(Diagnostic::error("unreadable", message)),
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return invalid(Diagnostic::error(
            "not_utf8",
            "the file is not UTF-8 text".to_string(),
        ));
    };
    let (mut parsed, lines) = match scheme::parse_theme_located(&text) {
        Ok(parsed) => parsed,
        Err(error) => {
            let ThemeError::Parse {
                line,
                ref reason,
                ref key,
                foreign_format,
            } = error;
            return invalid(Diagnostic {
                severity: Severity::Error,
                code: "parse",
                line: (line > 0).then_some(line),
                key: key.clone(),
                message: match foreign_format {
                    Some(_) => error.to_string(),
                    None => reason.clone(),
                },
                foreign_format: foreign_format.map(scheme::ForeignFormat::label),
            });
        }
    };
    let measured = host.measure(parsed.scheme.background);
    let meta = ThemeMeta::admit(&mut parsed, id.as_deref().unwrap_or_default(), measured);
    let mut diagnostics: Vec<Diagnostic> = meta
        .warnings
        .iter()
        .map(|warning| {
            let mut diagnostic = warning_diagnostic(*warning, &lines);
            if matches!(warning, ThemeWarning::SkippedNewerKeys { .. }) {
                diagnostic.message =
                    format!("{}: {}", diagnostic.message, meta.skipped_keys.join(", "));
            }
            diagnostic
        })
        .collect();
    diagnostics.extend(font_notes(&parsed.typography, &lines, host));
    diagnostics.extend(palette_notes(&parsed.scheme, &lines));
    diagnostics.extend(id_warnings);
    CheckReport {
        file,
        id,
        loaded: Some(Loaded {
            contrast: scheme::contrast_report(&parsed.scheme),
            scheme: parsed.scheme,
            meta,
            typography: parsed.typography,
            bold_is_bright: parsed.bold_is_bright,
        }),
        diagnostics,
        signature: None,
    }
}

const PALETTE_ANCHOR_MIN_SPAN: f64 = 0.5;

fn palette_notes(scheme: &aterm_types::ColorScheme, lines: &ThemeKeyLines) -> Vec<Diagnostic> {
    if !scheme.palette_generate {
        return Vec::new();
    }
    let (dark, light) = aterm_types::scheme_palette256::palette_anchors(
        &scheme.ansi,
        scheme.foreground,
        scheme.background,
    );
    let line = lines.line(ThemeKey::PaletteGenerate, None);
    let key = Some(
        scheme::theme_key_spec(ThemeKey::PaletteGenerate)
            .name
            .to_string(),
    );
    let mut out = vec![Diagnostic {
        severity: Severity::Note,
        code: "palette_generate",
        line,
        key: key.clone(),
        message: format!(
            "slots 16-255 are derived: black {} and white {} anchor the cube and the grey \
             ramp, color9-color14 colour its corners",
            hex(dark),
            hex(light)
        ),
        foreign_format: None,
    }];
    let span = aterm_types::scheme_signature::oklab(light).0
        - aterm_types::scheme_signature::oklab(dark).0;
    if span < PALETTE_ANCHOR_MIN_SPAN {
        out.push(Diagnostic {
            severity: Severity::Warning,
            code: "palette_anchors_close",
            line,
            key,
            message: format!(
                "the derived palette's black {} and white {} are {span:.2} apart in OkLab \
                 lightness (under {PALETTE_ANCHOR_MIN_SPAN}): its 256-colour cube and grey ramp \
                 will be hard to tell apart",
                hex(dark),
                hex(light)
            ),
            foreign_format: None,
        });
    }
    out
}

fn font_notes(
    typography: &scheme::ThemeTypography,
    lines: &ThemeKeyLines,
    host: &dyn ThemeHost,
) -> Vec<Diagnostic> {
    let mut notes = Vec::new();
    for spec in THEME_KEYS.iter().filter(|spec| {
        matches!(
            spec.value,
            ThemeValueType::FontName | ThemeValueType::FontNameList
        )
    }) {
        let names: Vec<String> = match typography.value_text(spec.key) {
            None => continue,
            Some(text) if spec.value == ThemeValueType::FontNameList => text
                .split(',')
                .map(|name| name.trim().to_string())
                .collect(),
            Some(text) => vec![text],
        };
        for name in names {
            if host.font_available(&name) == Some(false) {
                let fallback = if spec.key == ThemeKey::FontFamily {
                    "aterm uses your font_family, else its default"
                } else {
                    "aterm uses your setting for it, else its own choice"
                };
                notes.push(Diagnostic {
                    severity: Severity::Note,
                    code: "font_not_installed",
                    line: lines.line(spec.key, None),
                    key: Some(spec.name.to_string()),
                    message: format!("{name} is not installed here; {fallback}"),
                    foreign_format: None,
                });
            }
        }
    }
    notes
}

fn check_json(report: &CheckReport, strict: bool) -> Value {
    let diagnostics: Vec<Value> = report
        .diagnostics
        .iter()
        .map(|d| {
            json!({
                "severity": d.severity.word(),
                "code": d.code,
                "line": d.line,
                "key": d.key,
                "message": d.message,
                "foreign_format": d.foreign_format,
            })
        })
        .collect();
    let (theme, contrast) = match &report.loaded {
        None => (Value::Null, Vec::new()),
        Some(loaded) => {
            let meta = &loaded.meta;
            let theme = json!({
                "display_name": meta.display_name,
                "description": meta.description,
                "author": meta.author,
                "appearance": appearance_word(meta.appearance),
                "appearance_source": match meta.appearance_source {
                    scheme::AppearanceSource::Declared => "declared",
                    scheme::AppearanceSource::Measured => "measured",
                },
                "foreground": hex(loaded.scheme.foreground),
                "background": hex(loaded.scheme.background),
                "selection": hex(loaded.scheme.selection_band()),
                "selection_derived": loaded.scheme.selection.is_none(),
                "bold_is_bright": loaded.bold_is_bright,
                "palette_generate": loaded.scheme.palette_generate,
                "typography": typography_json(&loaded.typography),
                "skipped_keys": meta.skipped_keys,
            });
            let contrast = loaded
                .contrast
                .iter()
                .map(|row| {
                    let (drawn, on) = row.pair.keys();
                    json!({
                        "drawn": drawn,
                        "on": on,
                        "ratio": ratio(row.hundredths),
                        "minimum": row.minimum.map(ratio),
                        "below_minimum": row.below_minimum(),
                    })
                })
                .collect();
            (theme, contrast)
        }
    };
    json!({
        "document": "aterm-theme-check",
        "version": DOCUMENT_VERSION,
        "file": report.file,
        "id": report.id,
        "status": report.status(),
        "strict": strict,
        "exit_code": report.exit_code(strict),
        "theme": theme,
        "diagnostics": diagnostics,
        "contrast": contrast,
        "signature": report.signature.as_ref().map(|defects| {
            defects
                .iter()
                .map(|d| json!({"code": d.code, "message": d.message}))
                .collect::<Vec<Value>>()
        }),
    })
}

fn typography_json(typography: &scheme::ThemeTypography) -> Value {
    let mut map = aterm_json::Map::new();
    for spec in THEME_KEYS
        .iter()
        .filter(|spec| spec.group == ThemeKeyGroup::Typography)
    {
        if let Some(value) = typography.value_text(spec.key) {
            map.insert(spec.name.to_string(), Value::from(value));
        }
    }
    Value::Object(map)
}

fn check_text(report: &CheckReport) -> String {
    let mut out = String::new();
    for d in &report.diagnostics {
        let severity = d.severity.word();
        match d.line {
            Some(line) => out.push_str(&format!("{}:{line}: ", report.file)),
            None => out.push_str(&format!("{}: ", report.file)),
        }
        out.push_str(&format!("{severity}[{}]: {}\n", d.code, d.message));
    }
    if let Some(loaded) = &report.loaded {
        if !loaded.typography.is_empty() {
            out.push_str("typography (aterm.toml keys of the same name win):\n");
            for spec in THEME_KEYS
                .iter()
                .filter(|spec| spec.group == ThemeKeyGroup::Typography)
            {
                if let Some(value) = loaded.typography.value_text(spec.key) {
                    out.push_str(&format!("  {:<24}{value}\n", spec.name));
                }
            }
        }
        out.push_str("contrast (WCAG; text needs 3:1 to read, a selection 1.3:1 to be seen):\n");
        for row in &loaded.contrast {
            let (drawn, on) = row.pair.keys();
            let pair = format!("{drawn} on {on}");
            let verdict = match (row.minimum, row.below_minimum()) {
                (None, _) => String::new(),
                (Some(min), false) => format!("  ok (at least {})", ratio_text(min)),
                (Some(min), true) => format!("  LOW (below {})", ratio_text(min)),
            };
            out.push_str(&format!(
                "  {pair:<34}{:>9}{verdict}\n",
                ratio_text(row.hundredths)
            ));
        }
    }
    if let Some(defects) = &report.signature {
        if defects.is_empty() {
            out.push_str("signature bar: every bar cleared\n");
        } else {
            out.push_str("signature bars missed (aterm's signature themes clear them all):\n");
            for d in defects {
                out.push_str(&format!("  missed[{}]: {}\n", d.code, d.message));
            }
        }
    }
    let summary = match (&report.loaded, report.warnings()) {
        (None, _) => "does not load".to_string(),
        (Some(_), 0) => "loads".to_string(),
        (Some(_), 1) => "loads, with 1 warning".to_string(),
        (Some(_), n) => format!("loads, with {n} warnings"),
    };
    out.push_str(&format!("{}: {summary}\n", report.file));
    out
}

/// `aterm theme check <file> [--strict] [--signature] [--format text|json]`.
pub(crate) fn check(args: &[OsString], host: &dyn ThemeHost) -> Outcome {
    let operands = match operands(args, &["--format"], &["--strict", "--signature"]) {
        Ok(Some(operands)) => operands,
        Ok(None) => return help(),
        Err(message) => return usage(&format!("check: {message}")),
    };
    let file = match one_positional(&operands, "theme file") {
        Ok(file) => file,
        Err(message) => return usage(&format!("check: {message}")),
    };
    let json = match operands.values.get("--format").map(String::as_str) {
        None | Some("text") => false,
        Some("json") => true,
        Some(other) => return usage(&format!("--format takes text or json, not {other:?}")),
    };
    let strict = operands.switches.contains("--strict");
    let mut report = check_file(Path::new(&file), host);
    if operands.switches.contains("--signature") {
        report.signature = report
            .loaded
            .as_ref()
            .map(|loaded| signature_report(&loaded.scheme));
    }
    Outcome {
        stdout: if json {
            json_line(&check_json(&report, strict))
        } else {
            check_text(&report)
        },
        stderr: String::new(),
        code: report.exit_code(strict),
    }
}

/// `aterm theme new <name> [--from <theme>] [--appearance dark|light] [--force]`.
pub(crate) fn new(args: &[OsString], theme_dir: Option<&Path>, host: &dyn ThemeHost) -> Outcome {
    let operands = match operands(args, &["--from", "--appearance"], &["--force"]) {
        Ok(Some(operands)) => operands,
        Ok(None) => return help(),
        Err(message) => return usage(&format!("new: {message}")),
    };
    let id = match one_positional(&operands, "theme name") {
        Ok(id) => id,
        Err(message) => return usage(&format!("new: {message}")),
    };
    if let Err(why) = scheme::validate_user_theme_name(&id) {
        return usage(&format!("{id:?} cannot be a theme name: {why}"));
    }
    if let Some(builtin) = scheme::builtin_theme(&id) {
        return usage(&format!(
            "{id:?} is the built-in theme {:?}; pick another name",
            builtin.name
        ));
    }
    if id.chars().count() > MAX_THEME_TEXT_CHARS {
        return usage(&format!(
            "a theme name is at most {MAX_THEME_TEXT_CHARS} characters"
        ));
    }
    let wanted = match operands.values.get("--appearance") {
        Some(value) => match parse_appearance(value) {
            Ok(appearance) => Some(appearance),
            Err(message) => return usage(&message),
        },
        None => None,
    };
    let force = operands.switches.contains("--force");

    let from = operands.values.get("--from").cloned().unwrap_or_else(|| {
        match wanted {
            Some(Appearance::Light) => "Default Light",
            _ => "Default",
        }
        .to_string()
    });
    let mut seed = match host.lookup(&from) {
        Ok(seed) => seed,
        Err(why) => return failure("new", &format!("--from {from:?}: {why}")),
    };
    if let Some(wanted) = wanted
        && seed.meta.appearance != wanted
    {
        let Some(pair) = seed.meta.pair.clone() else {
            return usage(&format!(
                "{:?} is drawn for {} and has no {} pair; pick a {} theme with --from",
                seed.id,
                appearance_word(seed.meta.appearance),
                appearance_word(wanted),
                appearance_word(wanted),
            ));
        };
        seed = match host.lookup(&pair) {
            Ok(paired) => paired,
            Err(why) => return failure("new", &format!("{pair:?}: {why}")),
        };
    }

    let Some(dir) = theme_dir else {
        return failure(
            "new",
            "no themes folder: neither XDG_CONFIG_HOME, APPDATA nor HOME is set",
        );
    };
    if let Some(other) = scheme_install::case_variant(dir, &id) {
        return failure("new", &scheme_install::case_collision_message(&other, &id));
    }

    let mut scheme = seed.theme.scheme.clone();
    scheme.name = scheme_import::sanitize_text(&id).unwrap_or_default();
    scheme.appearance = seed.meta.appearance;
    let dest = dir.join(format!("{id}.conf"));
    let header = [
        format!("{id}: an aterm theme, started from {:?}.", seed.id),
        "Edit the colours below; a running aterm reloads this file when it changes.".to_string(),
        format!("Use it with theme = {id:?} in aterm.toml, or pick it in Settings."),
        format!(
            "Check it with: aterm theme check {:?} --strict",
            dest.display().to_string()
        ),
        "Every key is listed by: aterm theme schema --format text".to_string(),
    ];
    let header: Vec<&str> = header.iter().map(String::as_str).collect();
    let mut expected = ParsedTheme::colours_only(scheme, true, None, None);
    expected.typography = seed.theme.typography.clone();
    expected.bold_is_bright = seed.theme.bold_is_bright;
    let text = scheme_import::to_native_conf_package_commented(&expected, &header);
    match scheme::parse_theme_str(&text) {
        Ok(read_back) if read_back == expected => {}
        _ => {
            return failure(
                "new",
                "the theme does not read back unchanged; nothing was written",
            );
        }
    }
    if let Err(message) = write_atomically(dir, &dest, &id, text.as_bytes(), force, "--force") {
        return failure("new", &message);
    }
    Outcome {
        stdout: format!("{}\n", dest.display()),
        ..Outcome::default()
    }
}

/// The native file for a catalog theme, as `export` writes it.
pub(crate) fn export_text(theme: &CatalogTheme) -> String {
    let source = if scheme::builtin_theme(&theme.id).is_some() {
        format!("the built-in theme {:?}", theme.id)
    } else {
        format!("the themes-folder theme {:?}", theme.id)
    };
    let header = format!("Exported by `aterm theme export` from {source}.");
    scheme_import::to_native_conf_package(&theme.theme, &[&header])
}

/// `aterm theme export <name> [--out <file>] [--force]`.
pub(crate) fn export(args: &[OsString], host: &dyn ThemeHost) -> Outcome {
    let operands = match operands(args, &["--out"], &["--force"]) {
        Ok(Some(operands)) => operands,
        Ok(None) => return help(),
        Err(message) => return usage(&format!("export: {message}")),
    };
    let id = match one_positional(&operands, "theme name") {
        Ok(id) => id,
        Err(message) => return usage(&format!("export: {message}")),
    };
    let theme = match host.lookup(&id) {
        Ok(theme) => theme,
        Err(why) => return failure("export", &format!("{id:?}: {why}")),
    };
    let text = export_text(&theme);
    let Some(out) = operands.values.get("--out") else {
        return Outcome {
            stdout: text,
            ..Outcome::default()
        };
    };
    let path = Path::new(out);
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if let Err(message) = write_atomically(
        dir,
        path,
        &theme.id,
        text.as_bytes(),
        operands.switches.contains("--force"),
        "--force",
    ) {
        return failure("export", &message);
    }
    Outcome {
        stdout: format!("{out}\n"),
        ..Outcome::default()
    }
}

fn constraints(value: ThemeValueType) -> Value {
    match value {
        ThemeValueType::Colour => json!({
            "forms": ["#RRGGBB", "RRGGBB", "#RGB"],
            "case_insensitive": true,
        }),
        ThemeValueType::Text => json!({
            "max_chars": MAX_THEME_TEXT_CHARS,
            "control_characters": false,
            "inline_comment_starts_at": [" #", " ;"],
        }),
        ThemeValueType::Appearance => json!({
            "values": APPEARANCE_VALUES,
            "case_insensitive": true,
        }),
        ThemeValueType::FontName => font_name_constraints(),
        ThemeValueType::FontNameList => json!({
            "separator": ",",
            "min_entries": 1,
            "max_entries": scheme::MAX_THEME_FONT_LIST,
            "empty_entry": false,
            "entry": font_name_constraints(),
        }),
        ThemeValueType::Flag => json!({
            "values": ["true", "false"],
            "case_insensitive": true,
        }),
        ThemeValueType::Milli { min, max } => json!({
            "min": f64::from(min) / 1000.0,
            "max": f64::from(max) / 1000.0,
            "max_fraction_digits": 3,
            "sign_exponent_inf_nan": false,
        }),
        ThemeValueType::Integer { min, max } => json!({
            "min": min,
            "max": max,
        }),
        ThemeValueType::FeatureList => json!({
            "separators": [",", " "],
            "min_entries": 1,
            "max_entries": scheme::MAX_THEME_FEATURES,
            "forms": ["tag", "+tag", "-tag", "tag=N"],
            "tag": "1-4 ASCII letters or digits",
            "max_value": 65_535,
        }),
        ThemeValueType::SchemaVersion => json!({
            "min": 1,
            "max": u16::MAX,
            "current": scheme::THEME_SCHEMA_VERSION,
        }),
    }
}

fn font_name_constraints() -> Value {
    json!({
        "max_chars": scheme::MAX_THEME_FONT_NAME_CHARS,
        "control_characters": false,
        "not_a_path": {
            "contains": ["/", "\\"],
            "starts_with": ["~", "."],
            "drive_letter": true,
            "file_extensions": [".ttf", ".otf", ".ttc", ".otc", ".dfont", ".woff", ".woff2"],
        },
        "display_faces": "display:<id>[+<id>] names aterm's built-in display faces",
    })
}

/// `aterm theme schema --format json`: every key of [`THEME_KEYS`].
pub(crate) fn schema_json() -> Value {
    let defaults = aterm_types::ColorScheme::default();
    let keys: Vec<Value> = THEME_KEYS
        .iter()
        .map(|spec| {
            let indexed = spec.indexed > 0;
            let pattern = |name: &str| {
                if indexed {
                    format!("{name}<N>")
                } else {
                    name.to_string()
                }
            };
            let index = indexed.then(|| json!({"min": 0, "max": spec.indexed - 1}));
            let default = match spec.key {
                ThemeKey::Palette => Value::from(
                    defaults
                        .ansi
                        .iter()
                        .map(|c| Value::from(hex(*c)))
                        .collect::<Vec<_>>(),
                ),
                _ => Value::Null,
            };
            json!({
                "name": pattern(spec.name),
                "aliases": spec.aliases.iter().map(|a| pattern(a)).collect::<Vec<_>>(),
                "type": spec.value.label(),
                "required": spec.required,
                "index": index,
                "constraints": constraints(spec.value),
                "when_omitted": spec.when_omitted,
                "default": default,
                "description": spec.description,
                "example": spec.example,
                "group": spec.group.label(),
                "config_key": spec.config_key,
            })
        })
        .collect();
    json!({
        "document": "aterm-theme-schema",
        "version": DOCUMENT_VERSION,
        "syntax": {
            "line": "key = value",
            "comment_line_prefixes": ["#", ";"],
            "inline_comment": "whitespace then # or ; ends the value (a leading # is a colour)",
            "keys_case_insensitive": true,
            "unknown_key": "error",
            "duplicate_key": "error (a key and its aliases are one key)",
            "max_file_bytes": MAX_USER_THEME_FILE_BYTES,
            "file_name": "<id>.conf in the themes folder; <id> is what theme = \"<id>\" names",
            "schema_gate": {
                "current": scheme::THEME_SCHEMA_VERSION,
                "newer": "unknown keys are skipped and listed; known keys stay strict",
            },
            "config_wins": "an aterm.toml key named by config_key wins over the theme's value",
        },
        "keys": keys,
    })
}

fn schema_text() -> String {
    let mut out = String::from(
        "aterm theme file: one `key = value` per line. A line starting with `#` or `;`\n\
         is a comment, as is whitespace then `#` or `;` after a value.\n\
         Keys are case-insensitive; an unknown or repeated key is an error.\n\n",
    );
    let mut group = None;
    for spec in &THEME_KEYS {
        if group != Some(spec.group) {
            group = Some(spec.group);
            out.push_str(match spec.group {
                ThemeKeyGroup::Identity => "-- identity --\n\n",
                ThemeKeyGroup::Colour => "-- colour --\n\n",
                ThemeKeyGroup::Chrome => "-- chrome (optional) --\n\n",
                ThemeKeyGroup::Typography => {
                    "-- typography (optional; aterm.toml keys of the same name win) --\n\n"
                }
            });
        }
        let suffix = if spec.indexed > 0 { "<N>" } else { "" };
        out.push_str(&format!("{}{suffix}", spec.name));
        for alias in spec.aliases {
            out.push_str(&format!(", {alias}{suffix}"));
        }
        out.push_str(&format!(
            "  ({}, {})\n",
            spec.value.label(),
            if spec.required {
                "required"
            } else {
                "optional"
            }
        ));
        out.push_str(&format!("    {}\n", spec.description));
        if spec.indexed > 0 {
            out.push_str(&format!("    N is 0 to {}.\n", spec.indexed - 1));
        }
        match spec.value {
            ThemeValueType::Colour => out.push_str("    #RRGGBB (the # optional) or #RGB.\n"),
            ThemeValueType::Text => out.push_str(&format!(
                "    At most {MAX_THEME_TEXT_CHARS} characters, no control characters.\n"
            )),
            ThemeValueType::Appearance => {
                out.push_str(&format!("    One of: {}.\n", APPEARANCE_VALUES.join(", ")));
            }
            ThemeValueType::FontName => out.push_str(
                "    A family name, never a file or a path (no / \\ ~ or font file extension).\n",
            ),
            ThemeValueType::FontNameList => out.push_str(&format!(
                "    Family names separated by commas, 1 to {}.\n",
                scheme::MAX_THEME_FONT_LIST
            )),
            ThemeValueType::Flag => out.push_str("    true or false.\n"),
            ThemeValueType::Milli { min, max } => out.push_str(&format!(
                "    A number from {} to {}, at most 3 decimals.\n",
                aterm_types::scheme_typography::milli_text(min),
                aterm_types::scheme_typography::milli_text(max)
            )),
            ThemeValueType::Integer { min, max } => {
                out.push_str(&format!("    A whole number from {min} to {max}.\n"));
            }
            ThemeValueType::FeatureList => out.push_str(&format!(
                "    OpenType features (tag, +tag, -tag, tag=N), comma or space separated, 1 to {}.\n",
                scheme::MAX_THEME_FEATURES
            )),
            ThemeValueType::SchemaVersion => out.push_str(&format!(
                "    A whole number from 1; this aterm reads schema {} in full.\n",
                scheme::THEME_SCHEMA_VERSION
            )),
        }
        if let Some(key) = spec.config_key {
            out.push_str(&format!("    aterm.toml key: {key} (wins when set).\n"));
        }
        if !spec.required {
            out.push_str(&format!("    Omitted: {}.\n", spec.when_omitted));
        }
        out.push_str(&format!(
            "    Example: {}{} = {}\n\n",
            spec.name,
            if spec.indexed > 0 { "1" } else { "" },
            spec.example
        ));
    }
    out
}

/// `aterm theme schema [--format json|text]`.
pub(crate) fn schema(args: &[OsString]) -> Outcome {
    let operands = match operands(args, &["--format"], &[]) {
        Ok(Some(operands)) => operands,
        Ok(None) => return help(),
        Err(message) => return usage(&format!("schema: {message}")),
    };
    if !operands.positional.is_empty() {
        return usage("schema takes no operands");
    }
    let stdout = match operands.values.get("--format").map(String::as_str) {
        None | Some("json") => json_line(&schema_json()),
        Some("text") => schema_text(),
        Some(other) => return usage(&format!("--format takes json or text, not {other:?}")),
    };
    Outcome {
        stdout,
        ..Outcome::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_cmd::run;
    use crate::theme_cmd::tests::NEVER_LISTS;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cli-theme-author-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn theme(argv: &[&str], dir: Option<&Path>) -> Outcome {
        run(&args(argv), dir, &NEVER_LISTS)
    }

    fn json_of(out: &Outcome) -> Value {
        aterm_json::from_str(&out.stdout).expect("one JSON document")
    }

    fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
        value
            .get(key)
            .unwrap_or_else(|| panic!("no {key:?} in {value:?}"))
    }

    fn codes(doc: &Value) -> Vec<String> {
        field(doc, "diagnostics")
            .as_array()
            .expect("diagnostics")
            .iter()
            .map(|d| field(d, "code").as_str().unwrap_or_default().to_string())
            .collect()
    }

    const GOOD: &str = "name = Harbour\nappearance = dark\nforeground = #e0e0e0\n\
                        background = #101418\ncursor = #7aa2f7\nselection = #2a3a50\n";

    #[test]
    fn check_passes_a_good_theme() {
        let dir = scratch("good");
        let file = dir.join("harbour.conf");
        std::fs::write(&file, GOOD).unwrap();
        let path = file.to_str().unwrap();
        for argv in [
            vec!["check", path],
            vec!["check", path, "--strict"],
            vec!["check", "--format=json", path],
        ] {
            let out = theme(&argv, None);
            assert_eq!(out.code, EXIT_LOADS, "{argv:?}: {out:?}");
        }
        let out = theme(&["check", path, "--format", "json", "--strict"], None);
        let doc = json_of(&out);
        assert_eq!(field(&doc, "status").as_str(), Some("ok"));
        assert_eq!(
            field(&doc, "exit_code").as_u64(),
            Some(u64::from(EXIT_LOADS))
        );
        assert_eq!(field(&doc, "id").as_str(), Some("harbour"));
        assert!(codes(&doc).is_empty(), "{doc:?}");
        let theme_doc = field(&doc, "theme");
        assert_eq!(field(theme_doc, "display_name").as_str(), Some("Harbour"));
        assert_eq!(
            field(theme_doc, "appearance_source").as_str(),
            Some("declared")
        );
        let contrast = field(&doc, "contrast").as_array().unwrap();
        assert_eq!(contrast.len(), 20);
        assert!(
            contrast
                .iter()
                .all(|row| field(row, "below_minimum").as_bool() == Some(false))
        );
        let ansi = contrast
            .iter()
            .filter(|row| {
                field(row, "drawn")
                    .as_str()
                    .is_some_and(|k| k.starts_with("color"))
            })
            .count();
        assert_eq!(ansi, 16);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_refuses_an_invalid_theme() {
        let dir = scratch("invalid");
        let bad = dir.join("bad.conf");
        std::fs::write(
            &bad,
            "foreground = #ffffff\nbackground = #000000\nwidth = 80\n",
        )
        .unwrap();
        let out = theme(&["check", bad.to_str().unwrap(), "--format", "json"], None);
        assert_eq!(out.code, EXIT_INVALID, "{out:?}");
        let doc = json_of(&out);
        assert_eq!(field(&doc, "status").as_str(), Some("invalid"));
        assert_eq!(field(&doc, "theme"), &Value::Null);
        let diagnostic = &field(&doc, "diagnostics").as_array().unwrap()[0];
        assert_eq!(field(diagnostic, "severity").as_str(), Some("error"));
        assert_eq!(field(diagnostic, "code").as_str(), Some("parse"));
        assert_eq!(field(diagnostic, "line").as_u64(), Some(3));
        assert_eq!(field(diagnostic, "key").as_str(), Some("width"));
        let text = theme(&["check", bad.to_str().unwrap()], None);
        assert_eq!(text.code, EXIT_INVALID);

        let kitty = dir.join("night.conf");
        std::fs::write(&kitty, "foreground #c0caf5\nbackground #1a1b26\n").unwrap();
        let doc = json_of(&theme(
            &["check", kitty.to_str().unwrap(), "--format=json"],
            None,
        ));
        let diagnostic = &field(&doc, "diagnostics").as_array().unwrap()[0];
        assert_eq!(field(diagnostic, "foreign_format").as_str(), Some("kitty"));

        let missing = theme(&["check", dir.join("absent.conf").to_str().unwrap()], None);
        assert_eq!(missing.code, EXIT_INVALID);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_reports_warnings_and_strict_fails_on_them() {
        let dir = scratch("warn");
        let file = dir.join("murky.conf");
        std::fs::write(
            &file,
            "background = #202020\nforeground = #303030\ncolor4 = #202020\nappearance = light\n",
        )
        .unwrap();
        let path = file.to_str().unwrap();
        assert_eq!(theme(&["check", path], None).code, EXIT_LOADS);
        let strict = theme(&["check", path, "--strict", "--format", "json"], None);
        assert_eq!(strict.code, EXIT_WARNINGS, "{strict:?}");
        let doc = json_of(&strict);
        assert_eq!(field(&doc, "status").as_str(), Some("warnings"));
        let found = codes(&doc);
        for code in [
            "low_text_contrast",
            "ansi_matches_background",
            "appearance_mismatch",
        ] {
            assert!(found.iter().any(|c| c == code), "{code}: {found:?}");
        }
        let at = |code: &str| {
            field(&doc, "diagnostics")
                .as_array()
                .unwrap()
                .iter()
                .find(|d| field(d, "code").as_str() == Some(code))
                .map(|d| {
                    (
                        field(d, "line").as_u64(),
                        field(d, "key").as_str().map(str::to_string),
                    )
                })
                .unwrap()
        };
        assert_eq!(
            at("low_text_contrast"),
            (Some(2), Some("foreground".into()))
        );
        assert_eq!(
            at("ansi_matches_background"),
            (Some(3), Some("color4".into()))
        );
        assert_eq!(
            at("appearance_mismatch"),
            (Some(4), Some("appearance".into()))
        );
        let low = field(&doc, "contrast")
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                field(row, "drawn").as_str() == Some("foreground")
                    && field(row, "on").as_str() == Some("background")
            })
            .unwrap();
        assert_eq!(field(low, "below_minimum").as_bool(), Some(true));
        assert_eq!(
            theme(&["check", path, "--strict"], None).code,
            EXIT_WARNINGS
        );

        let chrome = dir.join("chrome.conf");
        let with_accent = |accent: &str| {
            format!("background = #1a1b26\nforeground = #c0caf5\naccent_color = {accent}\n")
        };
        std::fs::write(&chrome, with_accent("#1a1b26")).unwrap();
        let path = chrome.to_str().unwrap();
        let strict = theme(&["check", path, "--strict", "--format", "json"], None);
        assert_eq!(strict.code, EXIT_WARNINGS, "{strict:?}");
        let doc = json_of(&strict);
        let diagnostic = field(&doc, "diagnostics")
            .as_array()
            .unwrap()
            .iter()
            .find(|d| field(d, "code").as_str() == Some("chrome_below_floor"))
            .expect("the chrome warning");
        assert_eq!(field(diagnostic, "line").as_u64(), Some(3));
        assert_eq!(field(diagnostic, "key").as_str(), Some("accent_color"));
        std::fs::write(&chrome, with_accent("#7aa2f7")).unwrap();
        assert_eq!(theme(&["check", path, "--strict"], None).code, EXIT_LOADS);

        let shadowed = dir.join("Nord.conf");
        std::fs::write(&shadowed, GOOD).unwrap();
        let doc = json_of(&theme(
            &["check", shadowed.to_str().unwrap(), "--format=json"],
            None,
        ));
        assert_eq!(codes(&doc), ["shadowed_by_builtin"]);
        #[cfg(unix)]
        {
            let real = dir.join("real.conf");
            std::fs::write(&real, GOOD).unwrap();
            let linked = dir.join("linked.conf");
            std::os::unix::fs::symlink(&real, &linked).unwrap();
            let doc = json_of(&theme(
                &["check", linked.to_str().unwrap(), "--format=json"],
                None,
            ));
            assert!(codes(&doc).contains(&"symlink_not_followed".to_string()));
            let doc = json_of(&theme(
                &["check", real.to_str().unwrap(), "--format=json"],
                None,
            ));
            assert!(!codes(&doc).contains(&"symlink_not_followed".to_string()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn authoring_usage_errors_exit_2() {
        for argv in [
            vec!["check"],
            vec!["check", "a", "b"],
            vec!["check", "a", "--format", "yaml"],
            vec!["check", "a", "--strict=yes"],
            vec!["new"],
            vec!["new", "x", "--appearance", "dim"],
            vec!["new", "Dracula"],
            vec!["new", "dark:x"],
            vec!["export"],
            vec!["export", "Nord", "--bogus"],
            vec!["schema", "extra"],
            vec!["schema", "--format", "yaml"],
        ] {
            let out = theme(&argv, None);
            assert_eq!(out.code, 2, "{argv:?}: {out:?}");
            assert!(out.stdout.is_empty(), "{argv:?}");
        }
    }

    #[test]
    fn new_writes_a_theme_that_round_trips() {
        let dir = scratch("new").join("themes");
        let read = |out: &Outcome| {
            assert_eq!(out.code, 0, "{out:?}");
            let path = PathBuf::from(out.stdout.trim_end());
            assert_eq!(path.parent(), Some(dir.as_path()));
            scheme::parse_theme_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        let seeded = |name: &str| CatalogTheme::builtin(name).unwrap().theme.scheme;

        let plain = read(&theme(&["new", "Harbour"], Some(&dir)));
        let mut want = seeded("Default");
        want.name = "Harbour".into();
        assert_eq!(plain.scheme, want);
        assert!(plain.appearance_declared);

        let light = read(&theme(
            &["new", "Paper", "--appearance", "light"],
            Some(&dir),
        ));
        assert_eq!(light.scheme.background, seeded("Default Light").background);
        assert_eq!(light.scheme.appearance, Appearance::Light);

        let paired = read(&theme(
            &[
                "new",
                "Camp",
                "--from",
                "gruvbox dark",
                "--appearance=light",
            ],
            Some(&dir),
        ));
        assert_eq!(paired.scheme.ansi, seeded("Gruvbox Light").ansi);

        let again = theme(&["new", "Harbour", "--from", "Nord"], Some(&dir));
        assert_eq!(again.code, 1, "{again:?}");
        let kept = std::fs::read_to_string(dir.join("Harbour.conf")).unwrap();
        assert_eq!(scheme::parse_scheme_str(&kept).unwrap(), want);
        let forced = read(&theme(
            &["new", "Harbour", "--from", "Nord", "--force"],
            Some(&dir),
        ));
        assert_eq!(forced.scheme.ansi, seeded("Nord").ansi);

        let path = dir.join("Harbour.conf");
        assert_ne!(
            theme(&["check", path.to_str().unwrap()], None).code,
            EXIT_INVALID
        );
        let missing = theme(&["new", "Other", "--from", "No Such Theme"], Some(&dir));
        assert_eq!(missing.code, 1);
        assert!(!dir.join("Other.conf").exists());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn export_reparses_equal_and_out_respects_force() {
        let out = theme(&["export", "Nord"], None);
        assert_eq!(out.code, 0, "{out:?}");
        let parsed = scheme::parse_theme_str(&out.stdout).expect("Nord");
        assert_eq!(parsed, CatalogTheme::builtin("Nord").unwrap().theme);
        let dir = scratch("export");
        let dest = dir.join("nord.conf");
        let dest_arg = dest.to_str().unwrap();
        let out = theme(&["export", "Nord", "--out", dest_arg], None);
        assert_eq!(out.code, 0, "{out:?}");
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            theme(&["export", "Nord"], None).stdout
        );
        assert_eq!(
            theme(&["export", "Dracula", "--out", dest_arg], None).code,
            1
        );
        let forced = theme(&["export", "Dracula", "--out", dest_arg, "--force"], None);
        assert_eq!(forced.code, 0);
        assert_eq!(
            scheme::parse_scheme_str(&std::fs::read_to_string(&dest).unwrap()).unwrap(),
            scheme::builtin("Dracula").unwrap()
        );
        assert_eq!(theme(&["export", "No Such Theme"], None).code, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_forced_export_replaces_the_link_and_preserves_its_target() {
        let dir = scratch("export-link");
        let foreign = dir.join("foreign.conf");
        let original = b"a separate file that this export does not own\n";
        std::fs::write(&foreign, original).unwrap();
        let dest = dir.join("nord.conf");
        std::os::unix::fs::symlink(&foreign, &dest).unwrap();
        let path = dest.to_str().unwrap();

        let refused = theme(&["export", "Nord", "--out", path], None);
        assert_eq!(refused.code, 1, "{refused:?}");
        assert!(std::fs::symlink_metadata(&dest).unwrap().is_symlink());
        assert_eq!(std::fs::read(&foreign).unwrap(), original);

        let forced = theme(&["export", "Nord", "--out", path, "--force"], None);
        assert_eq!(forced.code, 0, "{forced:?}");
        assert_eq!(std::fs::read(&foreign).unwrap(), original);
        assert!(std::fs::symlink_metadata(&dest).unwrap().is_file());
        let exported = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(
            scheme::parse_theme_str(&exported).unwrap(),
            CatalogTheme::builtin("Nord").unwrap().theme
        );
        let names: BTreeSet<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_signature_measures_the_signature_bar() {
        let dir = scratch("signature");
        let check = |name: &str, extra: &[&str]| {
            let path = dir.join(format!("{}.conf", name.replace(' ', "-")));
            std::fs::write(&path, theme(&["export", name], None).stdout).unwrap();
            let mut argv = vec!["check", path.to_str().unwrap(), "--format=json"];
            argv.extend_from_slice(extra);
            let out = theme(&argv, None);
            let doc = json_of(&out);
            (out.code, field(&doc, "signature").clone())
        };
        let (code, night) = check("Aterm Night", &["--signature", "--strict"]);
        assert_eq!(
            (code, night.as_array().map(Vec::len)),
            (EXIT_LOADS, Some(0))
        );
        let (code, nord) = check("Nord", &["--signature"]);
        assert_eq!(code, EXIT_LOADS);
        let missed = nord.as_array().expect("an array");
        assert!(!missed.is_empty());
        assert!(
            missed
                .iter()
                .all(|row| field(row, "code").as_str().is_some())
        );
        assert_eq!(check("Nord", &["--signature", "--strict"]).0, EXIT_WARNINGS);
        assert_eq!(check("Nord", &[]).1, Value::Null);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_built_from_the_schema_examples_parses() {
        let out = theme(&["schema"], None);
        assert_eq!(out.code, 0, "{out:?}");
        let doc = json_of(&out);
        assert_eq!(field(&doc, "version").as_u64(), Some(DOCUMENT_VERSION));
        let keys = field(&doc, "keys").as_array().unwrap();
        let build_as = |skip: Option<&str>, spell: Option<(&str, &str)>| {
            let mut text = String::new();
            for key in keys {
                let canonical = field(key, "name").as_str().unwrap();
                if Some(canonical) == skip {
                    continue;
                }
                let name = match spell {
                    Some((key, alias)) if key == canonical => alias,
                    _ => canonical,
                };
                let example = field(key, "example").as_str().unwrap();
                match field(key, "index") {
                    Value::Null => text.push_str(&format!("{name} = {example}\n")),
                    index => {
                        let min = field(index, "min").as_u64().unwrap();
                        let max = field(index, "max").as_u64().unwrap();
                        for n in min..=max {
                            let key = name.replace("<N>", &n.to_string());
                            text.push_str(&format!("{key} = {example}\n"));
                        }
                    }
                }
            }
            text
        };
        let build = |skip: Option<&str>| build_as(skip, None);
        let full = scheme::parse_theme_str(&build(None)).expect("the schema's examples parse");
        assert!(full.description.is_some() && full.author.is_some());
        for key in keys {
            let name = field(key, "name").as_str().unwrap();
            let required = field(key, "required").as_bool().unwrap();
            let without = scheme::parse_theme_str(&build(Some(name)));
            assert_eq!(without.is_err(), required, "{name}: {without:?}");
            for alias in field(key, "aliases").as_array().unwrap() {
                let alias = alias.as_str().unwrap();
                let aliased = scheme::parse_theme_str(&build_as(None, Some((name, alias))));
                assert_eq!(aliased.as_ref(), Ok(&full), "{alias}");
            }
        }
        for (key, spec) in keys.iter().zip(THEME_KEYS.iter()) {
            assert_eq!(field(key, "group").as_str(), Some(spec.group.label()));
            assert_eq!(
                spec.group == ThemeKeyGroup::Chrome,
                matches!(
                    spec.key,
                    ThemeKey::AccentColor
                        | ThemeKey::ActiveTabColor
                        | ThemeKey::BorderColor
                        | ThemeKey::SearchMatchColor
                ),
                "{}",
                spec.name
            );
        }
        let text = theme(&["schema", "--format", "text"], None);
        assert_eq!(text.code, 0);
        assert!(!text.stdout.is_empty());
    }

    struct TypeHost(CatalogTheme);

    impl ThemeHost for TypeHost {
        fn list(&self, _: &crate::theme_cmd::ThemeListRequest) -> String {
            String::new()
        }
        fn lookup(&self, id: &str) -> Result<CatalogTheme, String> {
            if id.eq_ignore_ascii_case(&self.0.id) {
                return Ok(self.0.clone());
            }
            CatalogTheme::builtin(id).ok_or_else(|| "no theme by that name".to_string())
        }
        fn measure(&self, _: Rgb) -> Appearance {
            Appearance::Dark
        }
        fn font_available(&self, family: &str) -> Option<bool> {
            Some(!family.contains("Zz"))
        }
    }

    const STUDIO: &str = "name = Studio\nappearance = dark\nforeground = #e0e0e0\n\
                          background = #101418\nbold_is_bright = false\n\
                          font_family = Zz No Such Font\nline_height = 1.15\n\
                          fallback_fonts = Menlo, Zz Other\nfont_features = calt, -liga\n";

    fn type_host() -> TypeHost {
        let mut parsed = scheme::parse_theme_str(STUDIO).expect("studio parses");
        let meta = ThemeMeta::admit(&mut parsed, "Studio", Appearance::Dark);
        TypeHost(CatalogTheme {
            id: "Studio".to_string(),
            theme: parsed,
            meta,
        })
    }

    #[test]
    fn schema_names_each_keys_group_and_config_key() {
        let doc = json_of(&theme(&["schema"], None));
        let keys = field(&doc, "keys").as_array().unwrap();
        assert_eq!(keys.len(), THEME_KEYS.len());
        for (key, spec) in keys.iter().zip(THEME_KEYS.iter()) {
            assert_eq!(field(key, "group").as_str(), Some(spec.group.label()));
            assert_eq!(field(key, "config_key").as_str(), spec.config_key);
            assert_ne!(field(key, "constraints"), &Value::Null, "{}", spec.name);
        }
        let gate = field(field(&doc, "syntax"), "schema_gate");
        assert_eq!(
            field(gate, "current").as_u64(),
            Some(u64::from(scheme::THEME_SCHEMA_VERSION))
        );
        let text = theme(&["schema", "--format", "text"], None).stdout;
        for spec in THEME_KEYS
            .iter()
            .filter(|s| s.group == ThemeKeyGroup::Typography)
        {
            assert!(text.contains(spec.name), "{}", spec.name);
        }
    }

    #[test]
    fn check_notes_a_font_that_is_not_installed() {
        let host = type_host();
        let dir = scratch("type-check");
        let file = dir.join("Studio.conf");
        std::fs::write(&file, STUDIO).unwrap();
        let path = file.to_str().unwrap();
        let out = run(
            &args(&["check", path, "--strict", "--format", "json"]),
            None,
            &host,
        );
        assert_eq!(out.code, EXIT_LOADS, "{out:?}");
        let doc = json_of(&out);
        let notes: Vec<&Value> = field(&doc, "diagnostics")
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| field(d, "severity").as_str() == Some("note"))
            .collect();
        assert_eq!(notes.len(), 2, "{doc:?}");
        assert!(
            notes
                .iter()
                .all(|d| field(d, "code").as_str() == Some("font_not_installed"))
        );
        let family = notes
            .iter()
            .find(|d| field(d, "key").as_str() == Some("font_family"))
            .expect("the family's note");
        assert_eq!(field(family, "line").as_u64(), Some(6));
        let typography = field(field(&doc, "theme"), "typography");
        assert_eq!(field(typography, "line_height").as_str(), Some("1.15"));
        let text = run(&args(&["check", path]), None, &host).stdout;
        assert!(text.contains("note[font_not_installed]"), "{text}");
        std::fs::write(&file, format!("{STUDIO}line_height = big\n")).unwrap();
        let bad = run(&args(&["check", path, "--strict"]), None, &host);
        assert_eq!(bad.code, EXIT_INVALID);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn theme_check_warns_on_close_palette_anchors() {
        let dir = scratch("palette-anchors");
        let file = dir.join("Murk.conf");
        let path = file.to_str().unwrap().to_string();
        let check = |text: &str| {
            std::fs::write(&file, text).unwrap();
            let out = theme(&["check", &path, "--format", "json"], None);
            codes(&json_of(&out))
        };
        let murk = "foreground = #9a9a9a\nbackground = #6a6a6a\ncolor0 = #707070\n\
                    color7 = #8a8a8a\ncolor15 = #909090\npalette_generate = true\n";
        let found = check(murk);
        assert!(
            found.iter().any(|c| c == "palette_anchors_close"),
            "{found:?}"
        );
        assert!(found.iter().any(|c| c == "palette_generate"), "{found:?}");
        let wide = "foreground = #f0f0f0\nbackground = #101010\ncolor0 = #202020\n\
                    color15 = #f8f8f8\npalette_generate = true\n";
        let found = check(wide);
        assert!(
            !found.iter().any(|c| c == "palette_anchors_close"),
            "{found:?}"
        );
        assert!(found.iter().any(|c| c == "palette_generate"), "{found:?}");
        let off = murk.replace("palette_generate = true\n", "");
        let found = check(&off);
        assert!(
            !found
                .iter()
                .any(|c| c == "palette_generate" || c == "palette_anchors_close"),
            "{found:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_and_new_carry_typography() {
        let host = type_host();
        let out = run(&args(&["export", "Studio"]), None, &host);
        assert_eq!(out.code, 0, "{out:?}");
        let parsed = scheme::parse_theme_str(&out.stdout).expect("exported");
        assert_eq!(parsed.typography, host.0.theme.typography);
        assert_eq!(parsed.bold_is_bright, Some(false));
        let dir = scratch("type-new").join("themes");
        let made = run(
            &args(&["new", "Desk", "--from", "Studio"]),
            Some(&dir),
            &host,
        );
        assert_eq!(made.code, 0, "{made:?}");
        let written = std::fs::read_to_string(dir.join("Desk.conf")).unwrap();
        let back = scheme::parse_theme_str(&written).expect("new parses");
        assert_eq!(back.typography, host.0.theme.typography);
        assert_eq!(back.bold_is_bright, Some(false));
        let plain = run(
            &args(&["new", "Plain", "--from", "Nord"]),
            Some(&dir),
            &host,
        );
        assert_eq!(plain.code, 0);
        let plain = std::fs::read_to_string(dir.join("Plain.conf")).unwrap();
        assert!(
            scheme::parse_theme_str(&plain)
                .unwrap()
                .typography
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    fn contrib_themes() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/themes/")
    }

    fn strict_check(path: &Path) -> (Outcome, Value) {
        let path = path.to_str().expect("a UTF-8 path");
        let out = theme(&["check", path, "--strict", "--format", "json"], None);
        let doc = json_of(&out);
        (out, doc)
    }

    #[test]
    fn contrib_themes_pass_the_strict_check() {
        let dir = contrib_themes();
        let mut checked = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("contrib/themes exists") {
            let path = entry.expect("a readable entry").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name == "README.md" || name == "GUIDE.md" {
                continue;
            }
            if name == "pack" {
                assert!(path.is_dir(), "contrib/themes/pack is the pack's folder");
                continue;
            }
            assert_eq!(
                path.extension().and_then(|e| e.to_str()),
                Some("conf"),
                "{name}: contrib/themes holds one .conf per theme"
            );
            let (out, doc) = strict_check(&path);
            assert_eq!(out.code, EXIT_LOADS, "{name}: {}", out.stdout);
            assert!(codes(&doc).is_empty(), "{name}: {doc:?}");
            let identity = field(&doc, "theme");
            for key in ["description", "author"] {
                assert!(field(identity, key).as_str().is_some(), "{name}: no {key}");
            }
            assert_eq!(
                field(identity, "appearance_source").as_str(),
                Some("declared"),
                "{name}: declare appearance"
            );
            checked.push(path);
        }
        assert!(
            checked.iter().any(|p| p.ends_with("TEMPLATE.conf")),
            "{checked:?}"
        );

        let scratch = scratch("contrib-control");
        let murky = scratch.join("murky.conf");
        let text = std::fs::read_to_string(&checked[0]).expect("a contributed theme");
        let background = scheme::parse_theme_str(&text)
            .expect("it loads")
            .scheme
            .background;
        let sunk: String = text
            .lines()
            .map(|line| {
                let key = line.split('=').next().unwrap_or_default().trim();
                if key.eq_ignore_ascii_case("foreground") || key.eq_ignore_ascii_case("fg") {
                    format!("{key} = {}", hex(background))
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&murky, sunk).unwrap();
        let (out, doc) = strict_check(&murky);
        assert_eq!(out.code, EXIT_WARNINGS, "{}", out.stdout);
        assert!(
            codes(&doc).contains(&"low_text_contrast".to_string()),
            "{doc:?}"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn pack_themes_pass_the_strict_check() {
        let dir = scratch("pack-strict");
        for theme in aterm_theme_pack::themes() {
            let path = dir.join(format!("{}.conf", theme.id));
            std::fs::write(&path, theme.text).unwrap();
            let (out, doc) = strict_check(&path);
            let id = theme.id;
            assert_eq!(out.code, EXIT_LOADS, "{id}: {}", out.stdout);
            assert!(codes(&doc).is_empty(), "{id}: {doc:?}");
            let identity = field(&doc, "theme");
            for key in ["description", "author"] {
                assert!(field(identity, key).as_str().is_some(), "{id}: no {key}");
            }
            assert_eq!(
                field(identity, "appearance_source").as_str(),
                Some("declared"),
                "{id}"
            );
            let want = match theme.appearance {
                Appearance::Dark => "dark",
                Appearance::Light => "light",
            };
            assert_eq!(field(identity, "appearance").as_str(), Some(want), "{id}");
        }
        let theme = &aterm_theme_pack::themes()[0];
        let background = scheme::parse_theme_str(theme.text)
            .expect("it loads")
            .scheme
            .background;
        let sunk: String = theme
            .text
            .lines()
            .map(|line| {
                if line.starts_with("foreground =") {
                    format!("foreground = {}", hex(background))
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(
            sunk.trim_end(),
            theme.text.trim_end(),
            "the control changed the text"
        );
        let murky = dir.join("murky.conf");
        std::fs::write(&murky, sunk).unwrap();
        let (out, doc) = strict_check(&murky);
        assert_eq!(out.code, EXIT_WARNINGS, "{}", out.stdout);
        assert!(
            codes(&doc).contains(&"low_text_contrast".to_string()),
            "{doc:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
