// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Brew's `Cask::TokenGenerator` (`brew generate-cask-token`):
//! simplify an app name (or an app bundle's English name) and
//! derive the cask token for it, following the Cask Cookbook
//! conventions brew encodes, pattern for pattern.

use std::path::Path;

const REMOVE_TRAILING: &str = r"(?i)(?:\s+|\bapp|\b(?:quick[\s-]*)?launcher|\b(?:for)?[\s-]*mac(?:intosh|OS)?|\b(?:for)?[\s-]*os[\s-]*x|(?:\bfor\s*)?x.?86|(?:\bfor\s*)?\bppc|(?:\bfor\s*)?\barm(?:64|v\d+)?|(?:\bfor\s*)?\d+.?bits?|\b(?:for)?[\s-]*(?:oracle|apple|sun)*[\s-]*(?:jvm|java|jre)|\bgtk|\bqt|\bwx|\bcocoa|en\s*-\s*us|[^a-z0-9]+|\b(?:version|alpha|beta|gamma|release|release.?candidate)(?:[\s.\d-]*\d[\s.\d-]*)?)\z";
const PRESERVE_TRAILING: &str = r"(?i)(?:id3|mp3|3[\s-]*d|diff3|\A[^\d]+\+)\z";

fn regex(pattern: &str) -> aterm_regex::Regex {
    aterm_regex::Regex::new(pattern).expect("static token pattern compiles")
}

/// Brew's `TokenGenerator.simplified_app_name`.
pub fn simplified_app_name(app: &str) -> String {
    let name = english_app_name(app);
    let name = if Path::new(&name).exists() {
        Path::new(&name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(name)
    } else {
        name
    };
    let name = decompose_to_ascii(&name);
    let name = regex(r"(?i)\.app\z")
        .find(&name)
        .map_or(name.clone(), |m| name[..m.start()].to_string());
    remove_trailing_strings(&name)
}

/// Brew's `TokenGenerator.token_for`; `None` when nothing
/// tokenisable remains (brew's UsageError case).
pub fn token_for(app_name: &str) -> Option<String> {
    let mut token = app_name.to_lowercase();
    for (symbol, word) in [("+", "plus"), ("@", "at")] {
        token = token.replace(symbol, &format!(" {word} "));
    }
    while token.ends_with(' ') {
        token.pop();
    }
    // Runs of spaces become single dashes, then everything
    // but [a-z0-9-] drops and dash runs collapse.
    let mut dashed = String::new();
    let mut last_space = false;
    for c in token.chars() {
        if c == ' ' {
            if !last_space {
                dashed.push('-');
            }
            last_space = true;
        } else {
            last_space = false;
            dashed.push(c);
        }
    }
    let mut collapsed = String::new();
    let mut last_dash = false;
    for c in dashed.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        if c == '-' && last_dash {
            continue;
        }
        last_dash = c == '-';
        collapsed.push(c);
    }
    let token = collapsed.trim_matches('-').to_string();
    (!token.is_empty()).then_some(token)
}

/// Brew's `TokenGenerator.warnings` (the duplicate check is
/// the caller's, as in brew).
pub fn warnings(token: &str) -> Vec<String> {
    let base = token.split('@').next().unwrap_or(token);
    if base.chars().any(|c| c.is_ascii_digit()) {
        vec![format!(
            "'{token}' contains digits. If they are a version number, remove them from the token and file name."
        )]
    } else {
        Vec::new()
    }
}

fn remove_trailing_strings(name: &str) -> String {
    let remove = regex(REMOVE_TRAILING);
    let preserve = regex(PRESERVE_TRAILING);
    let mut name = name.replace('_', " ");
    loop {
        if preserve.is_match(&name) {
            break;
        }
        // brew's lookbehind `(?<=.)`: the leftmost match
        // that starts after at least one character. The
        // search window keeps one preceding character so
        // `\b` sees the same context Ruby does (a term
        // joined to the name, as in WhatsApp, is kept).
        let cut = (1..name.len()).find(|&start| {
            name.is_char_boundary(start)
                && name.is_char_boundary(start - 1)
                && remove
                    .find(&name[start - 1..])
                    .is_some_and(|m| m.start() == 1)
        });
        match cut {
            Some(start) => name.truncate(start),
            None => break,
        }
    }
    name
}

/// Crude Latin decomposition standing in for Ruby's NFKD
/// plus ASCII filtering (brew calls its own version crude).
fn decompose_to_ascii(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| if "·‧・･".contains(c) { '-' } else { c })
        .collect();
    if name.is_ascii() {
        return name;
    }
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii() {
            out.push(c);
            continue;
        }
        let base: &str = match c {
            'À'..='Å' | 'Ā'..='Ą' => "A",
            'à'..='å' | 'ā'..='ą' => "a",
            'Ç' | 'Ć'..='Č' => "C",
            'ç' | 'ć'..='č' => "c",
            'Ď' => "D",
            'ď' => "d",
            'È'..='Ë' | 'Ē'..='Ę' => "E",
            'è'..='ë' | 'ē'..='ę' => "e",
            'Ĝ'..='Ğ' => "G",
            'ĝ'..='ğ' => "g",
            'Ĥ' => "H",
            'ĥ' => "h",
            'Ì'..='Ï' | 'Ĩ'..='İ' => "I",
            'ì'..='ï' => "i",
            'Ĵ' => "J",
            'ĵ' => "j",
            'Ķ' => "K",
            'ķ' => "k",
            'Ĺ'..='Ŀ' => "L",
            'ĺ'..='ŀ' => "l",
            'Ñ' | 'Ń'..='Ň' => "N",
            'ñ' | 'ń'..='ň' => "n",
            'Ò'..='Ö' | 'Ō'..='Ő' => "O",
            'ò'..='ö' | 'ō'..='ő' => "o",
            'Ŕ'..='Ř' => "R",
            'ŕ'..='ř' => "r",
            'Ś'..='Ş' => "S",
            'ś'..='ş' => "s",
            'Š' => "S",
            'š' => "s",
            'Ţ'..='Ť' => "T",
            'ţ'..='ť' => "t",
            'Ù'..='Ü' | 'Ũ'..='Ų' => "U",
            'ù'..='ü' | 'ũ'..='ų' => "u",
            'Ý' => "Y",
            'ý' | 'ÿ' => "y",
            '\u{fb00}' => "ff",
            '\u{fb01}' => "fi",
            '\u{fb02}' => "fl",
            '\u{fb03}' => "ffi",
            '\u{fb04}' => "ffl",
            '\u{fb05}' | '\u{fb06}' => "st",
            'Ź'..='Ž' => "Z",
            'ź'..='ž' => "z",
            _ => "",
        };
        out.push_str(base);
    }
    out
}

fn english_app_name(app: &str) -> String {
    if app.is_ascii() {
        return app.to_string();
    }
    let path = Path::new(app);
    if !path.exists() {
        return app.to_string();
    }
    let candidates = [
        bundle_info_string(path, "CFBundleDisplayName"),
        bundle_info_string(path, "CFBundleName"),
        localized_app_name(path),
        bundle_info_string(path, "CFBundleExecutable"),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|c| c.is_ascii())
        .unwrap_or_else(|| app.to_string())
}

fn bundle_info_string(app_path: &Path, key: &str) -> Option<String> {
    let plist = app_path.join("Contents/Info.plist");
    if !plist.is_file() {
        return None;
    }
    let out = super::fetch::run_capture(
        "/usr/libexec/PlistBuddy",
        &["-c", &format!("Print {key}"), path_str(&plist)?],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn path_str(path: &Path) -> Option<&str> {
    path.to_str()
}

fn localized_app_name(app_path: &Path) -> Option<String> {
    let mut strings = app_path.join("Contents/Resources/en.lproj/InfoPlist.strings");
    if !strings.exists() {
        strings = app_path.join("Contents/Resources/English.lproj/InfoPlist.strings");
    }
    let bytes = std::fs::read(&strings).ok()?;
    let units: Vec<u16> = (0..bytes.len() / 2)
        .map(|i| u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}');
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            if let Some(rest) = line.strip_prefix(key) {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=')
                    && let Some(v) = rest
                        .trim_start()
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix("\";"))
                {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{simplified_app_name, token_for};

    fn token(name: &str) -> Option<String> {
        token_for(&simplified_app_name(name))
    }

    #[test]
    fn token_battery_matches_brew() {
        // Each row checked against brew 7.0.12 output.
        for (input, expected) in [
            ("Google Chrome", "google-chrome"),
            ("WhatsApp", "whatsapp"),
            ("PlayOnMac", "playonmac"),
            ("My App 2", "my-app-2"),
            ("Super Launcher", "super"),
            ("Frobnicator for Mac", "frobnicator"),
            ("Tool for macOS", "tool"),
            ("x86 Tool 64-bit", "x86-tool"),
            ("MP3 Gain 3", "mp3-gain-3"),
            ("Café Münster.app", "cafe-munster"),
            ("Easy MP3 Alarm Clock", "easy-mp3-alarm-clock"),
            ("Straße", "strae"),
            ("Łódź", "odz"),
            ("þing", "ing"),
            ("Æsop", "sop"),
            ("ﬁle", "file"),
        ] {
            assert_eq!(token(input).as_deref(), Some(expected), "{input}");
        }
        assert_eq!(token("  "), None);
    }
}
