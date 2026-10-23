// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Embedded community theme pack palettes, licence notices, and installation helpers.

mod policy;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_types::Appearance;
use aterm_types::scheme::{self, ParsedTheme};
use aterm_types::scheme_import::{self, ImportFormat};
use aterm_types::scheme_install::{self, InstallError};

pub use policy::{ADMITTED_LICENCES, admitted};

/// One theme of the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackTheme {
    /// Stable theme identifier and `<id>.conf` file stem.
    pub id: &'static str,
    /// Display name shown in the theme chooser (`name =` in the theme file).
    pub name: &'static str,
    /// The theme file the pack installs, byte for byte.
    pub text: &'static str,
    /// Source file path (`upstream/<upstream>` in the pack).
    pub upstream: &'static str,
    /// The appearance it is drawn for (declared in the file).
    pub appearance: Appearance,
    /// Companion theme for the opposite appearance (pack id or built-in name).
    pub pair: Option<&'static str>,
    /// Who the palette is by.
    pub author: &'static str,
    /// The palette's project, `owner/repo` on GitHub.
    pub project: &'static str,
    /// The project's home page.
    pub project_url: &'static str,
    /// The project's licence, an SPDX id in [`ADMITTED_LICENCES`].
    pub licence: &'static str,
    /// Licence file paths (`LICENSES/…`) whose notices this theme file carries.
    pub licence_files: &'static [&'static str],
}

/// Where the pack's upstream files come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Collection {
    /// `owner/repo` on GitHub.
    pub repo: &'static str,
    /// The commit `upstream/` was taken at.
    pub commit: &'static str,
    /// The collection's own licence file under the pack folder.
    pub licence_file: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/pack.rs"));

/// Every pack theme, sorted by id ignoring ASCII case.
#[inline]
#[must_use]
pub const fn themes() -> &'static [PackTheme] {
    THEMES
}

/// Looks up a pack theme by id or display name, ignoring ASCII case.
#[inline]
#[must_use]
#[allow(clippy::manual_find)]
pub fn find(id: &str) -> Option<&'static PackTheme> {
    for theme in THEMES {
        if theme.id.eq_ignore_ascii_case(id) {
            return Some(theme);
        }
    }
    for theme in THEMES {
        if theme.name.eq_ignore_ascii_case(id) {
            return Some(theme);
        }
    }
    None
}

/// Returns the replacement theme name for a withdrawn pack theme id, ignoring ASCII case.
#[inline]
#[must_use]
pub fn withdrawn(id: &str) -> Option<&'static str> {
    for &(gone, theme) in WITHDRAWN {
        if gone.eq_ignore_ascii_case(id) {
            return Some(theme);
        }
    }
    None
}

/// The text of the licence file `file` (`LICENSES/…`), if the pack has it.
#[inline]
#[must_use]
pub fn licence_text(file: &str) -> Option<&'static str> {
    for &(name, text) in LICENCES {
        if name == file {
            return Some(text);
        }
    }
    None
}

/// Every licence file the pack embeds, `(path, text)`, sorted by path.
#[inline]
#[must_use]
pub const fn licences() -> &'static [(&'static str, &'static str)] {
    LICENCES
}

/// Where the pack's upstream files come from.
#[inline]
#[must_use]
pub const fn collection() -> &'static Collection {
    &COLLECTION
}

impl PackTheme {
    /// Return `true` when this theme declares a companion theme for the opposite appearance.
    #[inline]
    #[must_use]
    pub const fn has_pair(&self) -> bool {
        self.pair.is_some()
    }

    /// Return the number of licence files carried by this theme.
    #[inline]
    #[must_use]
    pub const fn licence_file_count(&self) -> usize {
        self.licence_files.len()
    }
}

/// Encode a theme pack entry summary (`id_hash`, `text_len`, `licence_count`) into 16 LE bytes.
#[inline]
#[must_use]
pub const fn encode_theme_descriptor_le(
    id_hash: u64,
    text_len: u32,
    licence_count: u32,
) -> [u8; 16] {
    [
        id_hash as u8,
        (id_hash >> 8) as u8,
        (id_hash >> 16) as u8,
        (id_hash >> 24) as u8,
        (id_hash >> 32) as u8,
        (id_hash >> 40) as u8,
        (id_hash >> 48) as u8,
        (id_hash >> 56) as u8,
        text_len as u8,
        (text_len >> 8) as u8,
        (text_len >> 16) as u8,
        (text_len >> 24) as u8,
        licence_count as u8,
        (licence_count >> 8) as u8,
        (licence_count >> 16) as u8,
        (licence_count >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a theme pack entry summary (`id_hash`, `text_len`, `licence_count`).
#[inline]
#[must_use]
pub const fn decode_theme_descriptor_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let id_hash = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let text_len = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let licence_count = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (id_hash, text_len, licence_count)
}

/// Returns the embedded `LICENSES/<spdx>-<repo>.txt` path for a built-in port's source project.
#[inline]
#[must_use]
pub fn port_licence_file(project: &str) -> Option<&'static str> {
    for &(name, file) in PORTS {
        if name == project {
            return Some(file);
        }
    }
    None
}

/// The Apache-2.0 short notice a copy carries in place of the whole licence.
const APACHE_NOTICE: [&str; 6] = [
    "Licensed under the Apache License, Version 2.0 (the \"License\"); you may not",
    "use this file except in compliance with the License. You may obtain a copy of",
    "the License at http://www.apache.org/licenses/LICENSE-2.0 ; aterm prints it",
    "with `aterm theme pack --licences`. Unless required by applicable law or agreed",
    "to in writing, it is distributed on an \"AS IS\" BASIS, WITHOUT WARRANTIES OR",
    "CONDITIONS OF ANY KIND, either express or implied.",
];

/// `Copyright [yyyy] [name of copyright owner]` template.
fn copyright_lines(licence: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for raw in licence.lines() {
        let line = raw.trim();
        if line.starts_with("Copyright") && !line.contains("[yyyy]") && !line.contains("[name of") {
            out.push(line);
        }
    }
    out
}

/// the copyright lines and the short notice.
fn notice_lines(spdx: &str, licence: &str) -> Vec<String> {
    let mut out = Vec::new();
    if policy::notice_is_whole_text(spdx) {
        for line in licence.lines() {
            out.push(line.to_string());
        }
        return out;
    }
    for line in copyright_lines(licence) {
        out.push(line.to_string());
    }
    for line in APACHE_NOTICE {
        out.push(line.to_string());
    }
    out
}

/// Generates the pack `.conf` file for `theme` from `upstream_text` and `licences`.
///
/// # Errors
/// The upstream file does not convert, or the result does not round-trip.
pub fn derive(
    theme: &PackTheme,
    upstream_text: &str,
    licences: &[&str],
    collection_licence: &str,
) -> Result<String, String> {
    let mut report = match scheme_import::convert(ImportFormat::Ghostty, upstream_text, None) {
        Ok(r) => r,
        Err(error) => return Err(format!("{}: {error}", theme.upstream)),
    };
    report.scheme.name = theme.name.to_string();
    report.author = Some(theme.author.to_string());
    report.description = Some(format!(
        "{} from {} ({}), via iTerm2-Color-Schemes",
        theme.upstream, theme.project, theme.licence
    ));
    report.scheme.appearance = theme.appearance;
    report.appearance_declared = true;

    let commit = COLLECTION.commit.get(..12).unwrap_or(COLLECTION.commit);
    let dropped = if report.dropped.is_empty() {
        "nothing".to_string()
    } else {
        report.dropped.join(", ")
    };
    let spdx = if theme.licence == "MIT" {
        "MIT".to_string()
    } else {
        format!("{} AND MIT", theme.licence)
    };
    let mut header = vec![
        format!(
            "{}: aterm community theme pack (contrib/themes/pack).",
            theme.id
        ),
        format!(
            "Palette: {} by {}, {}",
            theme.upstream, theme.author, theme.project_url
        ),
        format!(
            "Converted by aterm from iTerm2-Color-Schemes ghostty/{} at {commit}",
            theme.upstream
        ),
        format!(
            "  (https://github.com/{}); dropped: {dropped}.",
            COLLECTION.repo
        ),
        String::new(),
        format!("SPDX-License-Identifier: {spdx}"),
    ];
    for licence in licences {
        header.push(String::new());
        let lines = notice_lines(theme.licence, licence);
        if !policy::notice_is_whole_text(theme.licence) && copyright_lines(licence).is_empty() {
            header.push(format!("Copyright {}", theme.author));
        }
        header.extend(lines);
    }
    header.push(String::new());
    header.push("The iTerm2-Color-Schemes collection:".to_string());
    for line in collection_licence.lines() {
        header.push(line.to_string());
    }
    let mut header_refs = Vec::with_capacity(header.len());
    for line in &header {
        header_refs.push(line.as_str());
    }
    let text = report.to_native_conf(&header_refs);

    let expected = ParsedTheme::colours_only(
        report.scheme.clone(),
        true,
        report.description.clone(),
        report.author.clone(),
    );
    match scheme::parse_theme_str(&text) {
        Ok(read_back) if read_back == expected => Ok(text),
        Ok(_) => Err(format!(
            "{}: the file does not read back unchanged",
            theme.id
        )),
        Err(error) => Err(format!(
            "{}: the file does not read back ({error})",
            theme.id
        )),
    }
}

/// Returns whether `text` contains every non-blank notice line required for `licence` under `spdx`.
#[must_use]
pub fn carries_notice(text: &str, spdx: &str, licence: &str) -> bool {
    let mut comments = text.lines();
    for raw in notice_lines(spdx, licence) {
        let wanted = raw.trim_end();
        if wanted.trim().is_empty() {
            continue;
        }
        let mut found = false;
        for line in &mut comments {
            if let Some(comment) = line.strip_prefix("# ")
                && comment == wanted
            {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }
    true
}

/// Where a pack theme stands in a themes folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackState {
    /// No `<id>.conf`.
    Absent,
    /// `<id>.conf` matches the pack's file (or an earlier release's `name = <id>` copy).
    Installed,
    /// `<id>.conf` exists with different contents or is not a regular file.
    Modified,
    /// A file exists whose name differs from `<id>.conf` only in ASCII case.
    CaseVariant(PathBuf),
}

/// Where `theme` stands in the themes folder `dir`.
#[inline]
#[must_use]
pub fn state_in(dir: &Path, theme: &PackTheme) -> PackState {
    standing(dir, theme).0
}

/// [`state_in`], and whether `<id>.conf` is the pack's file byte for byte.
fn standing(dir: &Path, theme: &PackTheme) -> (PackState, bool) {
    if let Some(other) = scheme_install::case_variant(dir, theme.id) {
        return (PackState::CaseVariant(other), false);
    }
    let path = dir.join(format!("{}.conf", theme.id));
    match std::fs::symlink_metadata(&path) {
        Err(_) => (PackState::Absent, false),
        Ok(meta) if !meta.is_file() => (PackState::Modified, false),
        Ok(_) => match scheme_install::read_bounded(&path) {
            Ok(bytes) if bytes == theme.text.as_bytes() => (PackState::Installed, true),
            Ok(bytes) if earlier_copy(&bytes, theme) => (PackState::Installed, false),
            _ => (PackState::Modified, false),
        },
    }
}

/// comment too, is an edit.
fn earlier_copy(bytes: &[u8], theme: &PackTheme) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    if theme.name == theme.id {
        return false;
    }
    let shown = format!("\nname = {}\n", theme.name);
    let Some(at) = theme.text.find(&shown) else {
        return false;
    };
    let (head, tail) = (&theme.text[..=at], &theme.text[at + shown.len() - 1..]);
    text.strip_prefix(head)
        .and_then(|rest| rest.strip_suffix(tail))
        .is_some_and(|line| line == format!("name = {}", theme.id))
}

/// What [`install`] did.
#[derive(Debug, Clone, Eq)]
pub enum PackInstall {
    /// Wrote the pack's file here.
    Wrote(PathBuf),
    /// The pack's file was already here, unchanged.
    Unchanged(PathBuf),
}

impl PartialEq for PackInstall {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Wrote(a), Self::Wrote(b)) | (Self::Unchanged(a), Self::Unchanged(b)) => a == b,
            _ => false,
        }
    }
}

/// Installs `theme` as `<id>.conf` into `dir`, replacing modified files only when `replace` is set.
///
/// # Errors
/// Returns [`InstallError`] if `dir` is missing, a case-variant or modified file blocks the write,
/// `theme` names a built-in, or the file fails to parse or write.
pub fn install(
    theme: &PackTheme,
    dir: Option<&Path>,
    replace: bool,
) -> Result<PackInstall, InstallError> {
    if let Err(why) = scheme::validate_user_theme_name(theme.id) {
        return Err(InstallError::Refused(format!(
            "{:?} cannot be a theme id: {why}",
            theme.id
        )));
    }
    let Some(dir) = dir else {
        return Err(InstallError::Failed(
            "no themes folder: neither XDG_CONFIG_HOME, APPDATA nor HOME is set".to_string(),
        ));
    };
    for builtin in scheme::builtin_names() {
        if builtin.eq_ignore_ascii_case(theme.id) {
            return Err(InstallError::Refused(format!(
                "{:?} is the built-in theme {builtin:?}",
                theme.id
            )));
        }
    }
    let dest = dir.join(format!("{}.conf", theme.id));
    match standing(dir, theme) {
        (PackState::CaseVariant(other), _) => {
            return Err(InstallError::Failed(
                scheme_install::case_collision_message(&other, theme.id),
            ));
        }
        (PackState::Installed, exact) if exact || !replace => {
            return Ok(PackInstall::Unchanged(dest));
        }
        (PackState::Installed | PackState::Absent | PackState::Modified, _) => {}
    }
    if scheme::parse_theme_str(theme.text).is_err() {
        return Err(InstallError::Failed(format!(
            "the pack copy of {} does not parse",
            theme.id
        )));
    }
    #[allow(clippy::question_mark)]
    if let Err(err) =
        scheme_install::write_atomically(dir, &dest, theme.id, theme.text.as_bytes(), replace)
    {
        return Err(err);
    }
    Ok(PackInstall::Wrote(dest))
}

/// Formats the full attribution and licence text for all bundled third-party palettes.
#[must_use]
pub fn notices() -> String {
    let mut out = String::new();
    out.push_str(
        "# Colour theme attribution\n\n\
         aterm's built-in theme ports and its community theme pack\n\
         (contrib/themes/pack/) use colour palettes from the projects below, each\n\
         under its own licence. Every pack theme file carries its notices too.\n\
         This file is generated (aterm_theme_pack::notices); do not edit it.\n\n\
         ## The community theme pack\n\n",
    );
    let _ = writeln!(
        out,
        "The pack's files are converted from the ghostty/ folder of\n\
         https://github.com/{} at commit\n{}, under the MIT licence ({}).\n\
         Each palette belongs to its author:\n",
        COLLECTION.repo, COLLECTION.commit, COLLECTION.licence_file
    );
    let mut keyed_projects: Vec<(String, &str)> = Vec::with_capacity(THEMES.len());
    for theme in THEMES {
        keyed_projects.push((theme.project.to_ascii_lowercase(), theme.project));
    }
    keyed_projects.sort_unstable();
    let mut projects: Vec<&str> = Vec::with_capacity(keyed_projects.len());
    for (_, project) in keyed_projects {
        if projects.last() != Some(&project) {
            projects.push(project);
        }
    }
    for project in projects {
        let mut first: Option<&PackTheme> = None;
        let mut ids: Vec<&str> = Vec::new();
        for t in THEMES {
            if t.project == project {
                if first.is_none() {
                    first = Some(t);
                }
                ids.push(t.id);
            }
        }
        let Some(first) = first else {
            continue;
        };
        let _ = writeln!(
            out,
            "- {project} by {} ({}; {}): {}\n  {}",
            first.author,
            first.licence,
            first.licence_files.join(", "),
            ids.join(", "),
            first.project_url
        );
    }
    out.push_str("\n## Built-in theme ports\n\n");
    let mut sources: Vec<(scheme::BuiltinSource, Vec<&str>)> = Vec::new();
    for builtin in scheme::builtin_theme_list() {
        let Some(source) = builtin.source else {
            continue;
        };
        let mut pushed = false;
        for (s, names) in &mut sources {
            if s.project == source.project {
                names.push(builtin.name);
                pushed = true;
                break;
            }
        }
        if !pushed {
            sources.push((source, vec![builtin.name]));
        }
    }
    for (source, names) in &sources {
        let _ = writeln!(
            out,
            "- {} ({}; {}): {}\n  {}",
            source.project,
            source.licence,
            port_licence_file(source.project).unwrap_or("no licence file"),
            names.join(", "),
            source.url
        );
    }
    out.push_str("\n## Licence texts\n");
    for (file, text) in LICENCES {
        let _ = write!(out, "\n### {file}\n\n");
        for line in text.lines() {
            if line.trim().is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "    {}", line.trim_end());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
