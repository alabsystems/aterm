// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `aterm theme pack` and `aterm theme install` CLI verbs.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;

use aterm_theme_pack::{PackInstall, PackState, PackTheme};
use aterm_types::Appearance;
use aterm_types::scheme::MAX_USER_THEME_FILES;
use aterm_types::scheme_install::InstallError;

use crate::theme_cmd::{Outcome, ThemeListFormat, appearance_value, format_value};

/// `aterm theme pack --help` and `aterm theme install --help`.
pub(crate) const PACK_HELP: &str = "\
aterm theme pack [<id>] [--appearance dark|light] [--format text|tsv] [--licences]

  The community theme pack: themes from permissively licensed projects,
  built into aterm and copied into the themes folder only when asked
  (`install`, Keep in Settings \u{25b8} Themes, or `aterm ctl theme commit`
  of a pack theme's preview). With no id, list them with each one's status
  in the themes folder: available, installed, modified, or blocked (by a
  file whose name differs only in case). With an id or display name, in
  any case, print that theme's file.

  --appearance dark|light   Only the pack themes drawn for that appearance.
  --format tsv              Tab-separated rows for scripts: a header row, one
                            row per theme, then a `#collection` line naming
                            the upstream repo and commit.
  --licences                Print the attribution and licence notices of
                            every third-party palette aterm bundles.

aterm theme install <id>... | --all [--replace]

  Copy pack themes, each named by id or display name in any case, into the
  themes folder as <id>.conf, where Settings and `theme = \"<id>\"` find
  them, and for each one written that has a pair print the
  `theme = \"dark:<dark>,light:<light>\"` value. A copy that differs from the
  pack's is kept unless --replace; a file whose name differs only in case
  blocks the install. An unknown id is a usage error before anything is
  written; one that left the pack names its replacement.

  --all       Every pack theme.
  --replace   Overwrite a copy that differs from the pack's.

Exit: 0 done, 1 an install failed, 2 a usage error.
";

fn help() -> Outcome {
    Outcome {
        stdout: PACK_HELP.to_string(),
        ..Outcome::default()
    }
}

const fn appearance_word(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

fn status(theme: &PackTheme, dir: Option<&Path>) -> (&'static str, Option<String>) {
    match dir.map(|dir| aterm_theme_pack::state_in(dir, theme)) {
        None | Some(PackState::Absent) => ("available", None),
        Some(PackState::Installed) => ("installed", None),
        Some(PackState::Modified) => ("modified", None),
        Some(PackState::CaseVariant(other)) => (
            "blocked",
            Some(
                other
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            ),
        ),
    }
}

struct PackArgs {
    id: Option<String>,
    appearance: Option<Appearance>,
    format: ThemeListFormat,
    licences: bool,
}

fn parse_pack_args(args: &[OsString]) -> Result<Option<PackArgs>, String> {
    let mut parsed = PackArgs {
        id: None,
        appearance: None,
        format: ThemeListFormat::Text,
        licences: false,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let arg = arg
            .to_str()
            .ok_or_else(|| format!("unknown argument {:?}", arg.to_string_lossy()))?;
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg, None),
        };
        let mut value = |flag: &str| -> Result<String, String> {
            match inline {
                Some(value) => Ok(value.to_string()),
                None => iter
                    .next()
                    .and_then(|v| v.to_str())
                    .map(str::to_string)
                    .ok_or_else(|| format!("{flag} needs a value")),
            }
        };
        match flag {
            "--help" | "-h" => return Ok(None),
            "--appearance" => parsed.appearance = Some(appearance_value(&value(flag)?)?),
            "--format" => parsed.format = format_value(&value(flag)?)?,
            "--licences" | "--licenses" if inline.is_none() => parsed.licences = true,
            "--licences" | "--licenses" => return Err(format!("pack: {flag} takes no value")),
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("pack: unknown option {other:?}"));
            }
            _ if parsed.id.is_some() => return Err("pack takes at most one id".to_string()),
            _ => parsed.id = Some(arg.to_string()),
        }
    }
    if parsed.licences && (parsed.id.is_some() || parsed.appearance.is_some()) {
        return Err("--licences takes no id or filter".to_string());
    }
    Ok(Some(parsed))
}

fn no_pack_theme(id: &str) -> String {
    match aterm_theme_pack::withdrawn(id) {
        Some(theme) if aterm_theme_pack::find(theme).is_some() => format!(
            "{id:?} left the pack; the pack theme {theme:?} replaces it: \
             aterm theme install {theme:?}"
        ),
        Some(theme) => format!(
            "{id:?} left the pack; the built-in theme {theme:?} replaces it: \
             theme = {theme:?} in aterm.toml"
        ),
        None => format!("no pack theme {id:?}; `aterm theme pack` lists them"),
    }
}

fn unknown(id: &str) -> Outcome {
    Outcome::usage_error(&no_pack_theme(id))
}

/// `aterm theme pack …`.
pub(crate) fn pack(args: &[OsString], theme_dir: Option<&Path>) -> Outcome {
    let parsed = match parse_pack_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return help(),
        Err(message) => return Outcome::usage_error(&message),
    };
    if parsed.licences {
        return Outcome {
            stdout: aterm_theme_pack::notices(),
            ..Outcome::default()
        };
    }
    if let Some(id) = &parsed.id {
        return match aterm_theme_pack::find(id) {
            Some(theme) => Outcome {
                stdout: theme.text.to_string(),
                ..Outcome::default()
            },
            None => unknown(id),
        };
    }
    let rows: Vec<&PackTheme> = aterm_theme_pack::themes()
        .iter()
        .filter(|theme| parsed.appearance.is_none_or(|a| a == theme.appearance))
        .collect();
    let collection = aterm_theme_pack::collection();
    let stdout = match parsed.format {
        ThemeListFormat::Tsv => {
            let mut out = String::from(
                "id\tappearance\tpair\tproject\tproject_url\tlicence\tupstream\tstatus\n",
            );
            for theme in &rows {
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    theme.id,
                    appearance_word(theme.appearance),
                    theme.pair.unwrap_or("-"),
                    theme.project,
                    theme.project_url,
                    theme.licence,
                    theme.upstream,
                    status(theme, theme_dir).0
                );
            }
            let _ = writeln!(
                out,
                "#collection\t{}\t{}",
                collection.repo, collection.commit
            );
            out
        }
        ThemeListFormat::Text => {
            let table: Vec<[String; 6]> = rows
                .iter()
                .map(|theme| {
                    let (word, blocker) = status(theme, theme_dir);
                    [
                        theme.id.to_string(),
                        appearance_word(theme.appearance).to_string(),
                        theme.pair.unwrap_or("-").to_string(),
                        theme.project.to_string(),
                        theme.licence.to_string(),
                        blocker.map_or_else(|| word.to_string(), |file| format!("{word}: {file}")),
                    ]
                })
                .collect();
            let header = ["ID", "APPEARANCE", "PAIR", "FROM", "LICENCE", "STATUS"];
            let mut widths = header.map(|h| h.chars().count());
            for row in &table {
                for (width, cell) in widths.iter_mut().zip(row) {
                    *width = (*width).max(cell.chars().count());
                }
            }
            let mut out = String::new();
            let mut line = |cells: [&str; 6]| {
                let mut text = String::new();
                for (index, cell) in cells.iter().enumerate() {
                    if index + 1 == cells.len() {
                        text.push_str(cell);
                    } else {
                        let pad = widths[index] + 2 - cell.chars().count();
                        text.push_str(cell);
                        text.push_str(&" ".repeat(pad));
                    }
                }
                out.push_str(text.trim_end());
                out.push('\n');
            };
            line(header);
            for row in &table {
                line(row.each_ref().map(String::as_str));
            }
            let commit = collection.commit.get(..12).unwrap_or(collection.commit);
            let _ = writeln!(
                out,
                "\n{} themes from permissively licensed projects, via {} @ {commit}.",
                rows.len(),
                collection.repo
            );
            out.push_str(
                "install: aterm theme install <id> (or --all) \u{b7} licences: aterm theme pack --licences\n",
            );
            let _ = writeln!(
                out,
                "themes folder: {}",
                theme_dir.map_or_else(|| "none".to_string(), |d| d.display().to_string())
            );
            out
        }
    };
    Outcome {
        stdout,
        ..Outcome::default()
    }
}

fn parse_install_args(
    args: &[OsString],
) -> Result<Option<(Vec<&'static PackTheme>, bool)>, String> {
    let mut ids: Vec<String> = Vec::new();
    let mut all = false;
    let mut replace = false;
    for arg in args {
        let arg = arg
            .to_str()
            .ok_or_else(|| format!("unknown argument {:?}", arg.to_string_lossy()))?;
        match arg {
            "--help" | "-h" => return Ok(None),
            "--all" => all = true,
            "--replace" => replace = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("install: unknown option {other:?}"));
            }
            id => ids.push(id.to_string()),
        }
    }
    let themes: Vec<&'static PackTheme> = match (all, ids.is_empty()) {
        (true, false) => return Err("install takes ids or --all, not both".to_string()),
        (false, true) => return Err("install needs a pack theme id, or --all".to_string()),
        (true, true) => aterm_theme_pack::themes().iter().collect(),
        (false, false) => {
            let mut themes = Vec::new();
            for id in &ids {
                let theme = aterm_theme_pack::find(id).ok_or_else(|| no_pack_theme(id))?;
                if !themes.iter().any(|t: &&PackTheme| t.id == theme.id) {
                    themes.push(theme);
                }
            }
            themes
        }
    };
    Ok(Some((themes, replace)))
}

fn split_value(theme: &PackTheme, pair: &str) -> String {
    let (dark, light) = match theme.appearance {
        Appearance::Dark => (theme.id, pair),
        Appearance::Light => (pair, theme.id),
    };
    format!("dark:{dark},light:{light}")
}

/// `aterm theme install …`.
pub(crate) fn install(args: &[OsString], theme_dir: Option<&Path>) -> Outcome {
    let (themes, replace) = match parse_install_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return help(),
        Err(message) => return Outcome::usage_error(&message),
    };
    let mut out = Outcome::default();
    let (mut wrote, mut had, mut kept, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for theme in &themes {
        match aterm_theme_pack::install(theme, theme_dir, replace) {
            Ok(PackInstall::Wrote(path)) => {
                wrote += 1;
                let _ = writeln!(
                    out.stdout,
                    "installed {} (from {}, {})\n  wrote {}",
                    theme.id,
                    theme.project,
                    theme.licence,
                    path.display()
                );
                if let Some(pair) = theme.pair {
                    let missing = aterm_theme_pack::find(pair).is_some_and(|other| {
                        !themes.iter().any(|t| t.id == other.id)
                            && theme_dir.is_none_or(|dir| {
                                aterm_theme_pack::state_in(dir, other) != PackState::Installed
                            })
                    });
                    let _ = writeln!(
                        out.stdout,
                        "  pairs with {pair}: theme = \"{}\"{}",
                        split_value(theme, pair),
                        if missing {
                            format!(" (aterm theme install \"{pair}\")")
                        } else {
                            String::new()
                        }
                    );
                }
            }
            Ok(PackInstall::Unchanged(_)) => {
                had += 1;
                let _ = writeln!(out.stdout, "{} is already installed (unchanged)", theme.id);
            }
            Err(InstallError::Exists(path)) => {
                kept += 1;
                let _ = writeln!(
                    out.stdout,
                    "kept {}: it differs from the pack copy; pass --replace to overwrite it",
                    path.display()
                );
            }
            Err(InstallError::Refused(message) | InstallError::Failed(message)) => {
                failed += 1;
                let _ = writeln!(out.stderr, "aterm theme install: {}: {message}", theme.id);
            }
        }
    }
    if themes.len() == 1 && failed == 0 {
        let _ = writeln!(
            out.stdout,
            "use it with theme = \"{}\" in aterm.toml, or pick it in Settings \u{25b8} Themes",
            themes[0].id
        );
    } else if themes.len() > 1 {
        let _ = writeln!(
            out.stdout,
            "installed {wrote} \u{b7} already had {had} \u{b7} kept {kept}{}",
            if failed > 0 {
                format!(" \u{b7} failed {failed}")
            } else {
                String::new()
            }
        );
    }
    if wrote > 0
        && let Some(dir) = theme_dir
    {
        let count = std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    Path::new(&entry.file_name())
                        .extension()
                        .is_some_and(|ext| ext == "conf")
                })
                .count()
        });
        if count > MAX_USER_THEME_FILES {
            let _ = writeln!(
                out.stderr,
                "aterm theme install: warning: the themes folder holds {count} .conf entries; \
                 aterm reads at most {MAX_USER_THEME_FILES} theme files, lowest names first \
                 (aterm theme list says if any were left out)"
            );
        }
    }
    out.code = u8::from(failed > 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_cmd::run;
    use crate::theme_cmd::tests::NEVER_LISTS;

    fn scratch(tag: &str) -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-cli-pack-{tag}-"))
            .tempdir()
            .expect("owned scratch dir")
    }

    fn theme(argv: &[&str], dir: Option<&Path>) -> Outcome {
        let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
        run(&args, dir, &NEVER_LISTS)
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn pack_lists_every_theme_as_tsv() {
        let out = theme(&["pack", "--format", "tsv"], None);
        assert_eq!(out.code, 0, "{out:?}");
        let lines: Vec<&str> = out.stdout.lines().collect();
        let count = aterm_theme_pack::themes().len();
        assert_eq!(lines.len(), count + 2, "{}", out.stdout);
        for line in &lines[..=count] {
            assert_eq!(line.split('\t').count(), 8, "{line}");
        }
        let trailer: Vec<&str> = lines[count + 1].split('\t').collect();
        assert_eq!(trailer[0], "#collection");
        assert_eq!(trailer[2], aterm_theme_pack::collection().commit);

        let light = theme(&["pack", "--appearance=light", "--format=tsv"], None);
        let rows: Vec<&str> = light
            .stdout
            .lines()
            .skip(1)
            .filter(|l| !l.starts_with('#'))
            .collect();
        assert!(!rows.is_empty() && rows.len() < count);
        assert!(
            rows.iter()
                .all(|row| row.split('\t').nth(1) == Some("light"))
        );
        let text = theme(&["pack"], None);
        assert_eq!(text.code, 0);
        assert!(text.stdout.lines().count() > count);
    }

    #[test]
    fn pack_prints_a_theme_and_the_licences() {
        let kanagawa = aterm_theme_pack::find("Kanagawa Wave").expect("in the pack");
        for id in ["Kanagawa Wave", "kanagawa wave"] {
            let out = theme(&["pack", id], None);
            assert_eq!(out.code, 0);
            assert_eq!(out.stdout, kanagawa.text);
        }
        for argv in [
            vec!["pack", "Nope"],
            vec!["pack", "a", "b"],
            vec!["pack", "--licences", "Ayu"],
            vec!["pack", "--appearance", "blue"],
            vec!["pack", "--bogus"],
        ] {
            assert_eq!(theme(&argv, None).code, 2, "{argv:?}");
        }
        for flag in ["--licences", "--licenses"] {
            let out = theme(&["pack", flag], None);
            assert_eq!(out.code, 0);
            for (file, text) in aterm_theme_pack::licences() {
                let first = text.lines().find(|l| !l.trim().is_empty()).unwrap();
                assert!(out.stdout.contains(first.trim_end()), "{file}");
            }
        }
    }

    #[test]
    fn install_writes_keeps_and_replaces() {
        let owned = scratch("install");
        let dir = owned.path().to_path_buf();
        let iceberg = aterm_theme_pack::find("Iceberg Dark").unwrap();
        let dest = dir.join("Iceberg Dark.conf");
        let out = theme(&["install", "Iceberg Dark"], Some(&dir));
        assert_eq!(out.code, 0, "{out:?}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), iceberg.text);
        assert!(
            out.stdout
                .contains("theme = \"dark:Iceberg Dark,light:Iceberg Light\""),
            "{}",
            out.stdout
        );
        assert_eq!(files(&dir), ["Iceberg Dark.conf"]);
        let again = theme(&["install", "iceberg dark"], Some(&dir));
        assert_eq!(again.code, 0);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), iceberg.text);

        std::fs::write(&dest, "mine").unwrap();
        let kept = theme(&["install", "Iceberg Dark"], Some(&dir));
        assert_eq!(kept.code, 0, "{kept:?}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "mine");
        let replaced = theme(&["install", "Iceberg Dark", "--replace"], Some(&dir));
        assert_eq!(replaced.code, 0);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), iceberg.text);
    }

    #[test]
    fn install_takes_the_shown_name_and_keeps_an_earlier_copy() {
        let owned = scratch("names");
        let dir = owned.path().to_path_buf();
        let renamed = aterm_theme_pack::themes()
            .iter()
            .find(|theme| theme.name != theme.id)
            .expect("premise: a theme shown under another name");
        let dest = dir.join(format!("{}.conf", renamed.id));
        let out = theme(&["install", renamed.name], Some(&dir));
        assert_eq!(out.code, 0, "{out:?}");
        assert_eq!(files(&dir), [format!("{}.conf", renamed.id)]);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), renamed.text);
        let earlier = renamed.text.replacen(
            &format!("\nname = {}\n", renamed.name),
            &format!("\nname = {}\n", renamed.id),
            1,
        );
        assert_ne!(earlier, renamed.text, "premise: the name line differs");
        std::fs::write(&dest, &earlier).unwrap();
        let listed = theme(&["pack", "--format", "tsv"], Some(&dir));
        let row = listed
            .stdout
            .lines()
            .find(|line| line.split('\t').next() == Some(renamed.id))
            .expect("its row");
        assert_eq!(row.rsplit('\t').next(), Some("installed"), "{row}");
        for argv in [vec!["install", renamed.id], vec!["install", "--all"]] {
            let out = theme(&argv, Some(&dir));
            assert_eq!(out.code, 0, "{argv:?}: {out:?}");
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), earlier, "{argv:?}");
        }
        assert_eq!(
            files(&dir)
                .iter()
                .filter(|file| file.starts_with(&format!("{}.", renamed.id))
                    || file.starts_with(&format!("{}.", renamed.name)))
                .count(),
            1,
            "{:?}",
            files(&dir)
        );

        let gone = theme(&["install", "GitHub Light Default"], Some(&dir));
        assert_eq!(gone.code, 2, "{gone:?}");
        let standing = aterm_theme_pack::withdrawn("GitHub Light Default").expect("withdrawn");
        assert!(
            gone.stderr.contains(&format!("{standing:?}")),
            "{}",
            gone.stderr
        );
        let unknown = theme(&["install", "Nope"], Some(&dir));
        assert_eq!(unknown.code, 2);
        assert!(
            !unknown.stderr.contains(&format!("{standing:?}")),
            "{}",
            unknown.stderr
        );
    }

    #[test]
    fn install_all_and_refusals() {
        let owned = scratch("all");
        let dir = owned.path().to_path_buf();
        for argv in [
            vec!["install", "Nope", "Iceberg Dark"],
            vec!["install", "--all", "Ayu"],
            vec!["install"],
            vec!["install", "--bogus"],
        ] {
            assert_eq!(theme(&argv, Some(&dir)).code, 2, "{argv:?}");
        }
        assert!(files(&dir).is_empty(), "a refusal wrote {:?}", files(&dir));
        let out = theme(&["install", "--all"], Some(&dir));
        assert_eq!(out.code, 0, "{out:?}");
        let mut want: Vec<String> = aterm_theme_pack::themes()
            .iter()
            .map(|t| format!("{}.conf", t.id))
            .collect();
        want.sort();
        assert_eq!(files(&dir), want);
    }

    #[test]
    fn install_fails_without_a_folder_and_warns_past_the_cap() {
        assert_eq!(theme(&["install", "Ayu"], None).code, 1);
        let owned = scratch("cap");
        let dir = owned.path().to_path_buf();
        for n in 0..MAX_USER_THEME_FILES - 1 {
            std::fs::write(dir.join(format!("f{n}.conf")), "").unwrap();
        }
        let under = theme(&["install", "Ayu"], Some(&dir));
        assert_eq!(under.code, 0);
        assert!(under.stderr.is_empty(), "{}", under.stderr);
        let over = theme(&["install", "Snazzy"], Some(&dir));
        assert_eq!(over.code, 0);
        assert!(
            over.stderr
                .contains(&format!("{}", MAX_USER_THEME_FILES + 1)),
            "{}",
            over.stderr
        );
    }
}
