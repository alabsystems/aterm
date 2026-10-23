// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `aterm theme`: the themes family.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::Appearance;
use aterm_types::scheme;
use aterm_types::scheme_install::{self, InstallError};

use crate::{theme_author, theme_pack_cmd};

/// The verb's usage lines.
pub(crate) const USAGE: &str = "usage: aterm theme list [--appearance dark|light] [--format text|tsv]\n       \
                                aterm theme check <file> [--strict] [--signature] [--format text|json]\n       \
                                aterm theme new <name> [--from <theme>] [--appearance dark|light] [--force]\n       \
                                aterm theme export <name> [--out <file>] [--force]\n       \
                                aterm theme schema [--format json|text]\n       \
                                aterm theme import <file> [--name <id>] [--replace]\n       \
                                aterm theme pack [<id>] [--appearance dark|light] [--format text|tsv] [--licences]\n       \
                                aterm theme install <id>... | --all [--replace]";

/// `aterm theme help`, and `--help` on any subcommand.
pub(crate) const HELP: &str = "\
aterm theme list [--appearance dark|light] [--format text|tsv]

  List every theme: the built-ins (Default first), then each <id>.conf in
  the themes folder, loaded or not, with its id (what `theme = \"<id>\"`
  takes), label, appearance, pair, source, status and description, and any
  warnings or the reason it did not load. The themes folder's path closes
  the listing, with a truncation line when the folder holds more than aterm
  reads. `aterm list-themes` is the same command.

  --appearance dark|light   Only the themes drawn for that appearance.
  --format tsv              Tab-separated rows for scripts: a header row, one
                            row per theme with a fixed column count, then a
                            `#folder` line and, if truncated, `#truncated`.

aterm theme check <file> [--strict] [--signature] [--format text|json]

  Read a theme file as the themes folder will: parse it, run the checks it
  loads under, and measure each contrast it rests on (text, each ANSI
  colour, the selection band and the text on it, the cursor). Each
  diagnostic names its line and key where it has them.

  Exit status: 0 the theme loads, 1 it loads with warnings and --strict was
  given, 2 it does not load (or a usage error).

  --strict         Warnings fail the check (exit 1).
  --signature      Also measure the stricter bar aterm's signature themes
                   (Aterm Night and Day, Hearth, Linen) clear: text 7:1; each
                   accent (ANSI 1-6, brights 9-14) 4.5:1; a tier's accents 25
                   deg apart in OkLCh hue; each bright 0.05 OkLab from its
                   normal; color8 in [3, 4.5); color0 1.15:1; color7 and
                   color15 4.5:1; the selection band 1.5:1 with text 4.5:1 on
                   it; a cursor at 3:1, OkLCh chroma 0.08 or more, within 15
                   deg of an accent's hue; accent chroma 0.07-0.17, max/min
                   at most 1.75. A missed bar is listed, and fails --strict.
  --format json    One JSON object: {document: \"aterm-theme-check\", version: 1,
                   file, id, status: ok|warnings|invalid, strict, exit_code,
                   theme: null | {display_name, description, author,
                   appearance, appearance_source: declared|measured,
                   foreground, background, selection, selection_derived},
                   diagnostics: [{severity: error|warning, code, line, key,
                   message, foreign_format}],
                   contrast: [{drawn, on, ratio, minimum, below_minimum}],
                   signature: null | [{code, message}]}. A missing value is
                   null; a version only ever gains fields.

  Diagnostic codes: errors parse, unreadable, not_utf8; warnings
  low_text_contrast, faint_selection, ansi_matches_background,
  low_cursor_contrast, appearance_mismatch (load checks), and not_conf,
  invalid_id, shadowed_by_builtin, symlink_not_followed (the file name).

aterm theme new <name> [--from <theme>] [--appearance dark|light] [--force]

  Write <name>.conf into the themes folder, each key under a comment saying
  what it does, and print its path. Its colours come from --from (a built-in
  or a themes-folder theme that loads; with --appearance, its pair when it
  is drawn for the other appearance), else Default, or Default Light with
  --appearance light. A built-in's name is refused; an existing theme is
  kept unless --force.

aterm theme export <name> [--out <file>] [--force]

  Print a theme (a built-in or a themes-folder theme that loads) as an aterm
  theme file. --out writes it to <file> and prints the path instead; an
  existing file is kept unless --force.

aterm theme schema [--format json|text]

  Describe every key a theme file can hold, from the parser's own key
  table. JSON (the default) is {document: \"aterm-theme-schema\", version: 1,
  syntax, keys: [{name, aliases, type, required, index, constraints,
  when_omitted, default, description, example}]}; --format text is the
  same table for a person.

aterm theme import <file> [--name <id>] [--replace]

  Convert another terminal's theme into a theme in the themes folder, where
  Settings and `theme = \"<id>\"` find it: kitty, Ghostty, Alacritty (TOML),
  iTerm2 (.itermcolors), Windows Terminal (JSON) or base16/base24 (YAML),
  told apart by the file's extension, else its contents. Only a local file
  up to 256 KiB is read; no URL, include or import line is followed. Keys
  aterm has no place for are listed, not fatal.

  --name <id>   The theme's id (its file name) and display name; default the
                file's name without its extension, keeping the theme's own
                display name. A built-in's name is refused. In a Windows
                Terminal file with several schemes it picks one by name
                (else the first).
  --replace     Overwrite an existing theme with this id.

Every command but check exits 0 when done, 1 when it fails (a file that
exists or cannot be read, an unknown theme), 2 on a usage error.
";

/// What a run printed and its exit code.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) code: u8,
}

impl Outcome {
    pub(crate) fn usage_error(message: &str) -> Self {
        Self {
            stderr: format!("aterm theme: {message}\n{USAGE}\n"),
            code: 2,
            ..Self::default()
        }
    }

    fn failure(message: &str) -> Self {
        Self {
            stderr: format!("aterm theme import: {message}\n"),
            code: 1,
            ..Self::default()
        }
    }
}

/// How `aterm theme list` prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeListFormat {
    /// Aligned columns for a person (the default).
    Text,
    /// Tab-separated rows with a fixed column count, for scripts.
    Tsv,
}

/// What `aterm theme list` asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeListRequest {
    /// Only the themes drawn for this appearance (`None`: all of them).
    pub appearance: Option<Appearance>,
    /// The output form.
    pub format: ThemeListFormat,
}

/// What the theme commands need from the window library, which owns the theme catalog this crate
/// does not link.
pub trait ThemeHost {
    /// The one theme listing (`aterm theme list`).
    fn list(&self, request: &ThemeListRequest) -> String;
    /// The theme `id` names, by the catalog's lookup (built-ins first, then themes-folder files
    /// that loaded); `Err` says why there is none.
    /// # Errors
    /// No theme by that id, or its file did not load.
    fn lookup(&self, id: &str) -> Result<aterm_types::scheme::CatalogTheme, String>;
    /// The appearance a background measures as, by the rule the themes folder admits a file with.
    fn measure(&self, background: aterm_types::Rgb) -> Appearance;
    /// Whether a font family a theme names is installed here, by the window's own font resolution
    /// (never touching a display server); `None` when this host cannot tell.
    fn font_available(&self, family: &str) -> Option<bool> {
        let _ = family;
        None
    }
}

/// `aterm list-themes [<args>]` is `aterm theme list [<args>]`.
#[must_use]
pub fn list_themes_alias(first: &str, rest: &[OsString]) -> Option<Vec<OsString>> {
    (first == "list-themes").then(|| {
        std::iter::once(OsString::from("list"))
            .chain(rest.iter().cloned())
            .collect()
    })
}

/// `aterm theme …` from the front door.
pub fn main_entry(args: Vec<OsString>, host: &dyn ThemeHost) -> ExitCode {
    let outcome = run(&args, scheme::user_theme_dir().as_deref(), host);
    if let Err(e) = aterm_uds::stdout::answer(&outcome.stdout) {
        eprintln!("aterm theme: the answer was not written ({e})");
        return ExitCode::from(1);
    }
    eprint!("{}", outcome.stderr);
    ExitCode::from(outcome.code)
}

/// The verb, with the themes folder injected (`None`: none could be found).
pub(crate) fn run(args: &[OsString], theme_dir: Option<&Path>, host: &dyn ThemeHost) -> Outcome {
    let Some(sub) = args.first() else {
        return Outcome::usage_error("needs a subcommand");
    };
    let rest = args.get(1..).unwrap_or(&[]);
    match sub.to_str() {
        Some("list") => list(rest, host),
        Some("check") => theme_author::check(rest, host),
        Some("new") => theme_author::new(rest, theme_dir, host),
        Some("export") => theme_author::export(rest, host),
        Some("schema") => theme_author::schema(rest),
        Some("import") => import(rest, theme_dir),
        Some("pack") => theme_pack_cmd::pack(rest, theme_dir),
        Some("install") => theme_pack_cmd::install(rest, theme_dir),
        Some("help" | "--help" | "-h") => Outcome {
            stdout: HELP.to_string(),
            ..Outcome::default()
        },
        _ => Outcome::usage_error(&format!("unknown subcommand {:?}", sub.to_string_lossy())),
    }
}

fn parse_list_args(args: &[OsString]) -> Result<Option<ThemeListRequest>, String> {
    let mut request = ThemeListRequest {
        appearance: None,
        format: ThemeListFormat::Text,
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
            "--appearance" => request.appearance = Some(appearance_value(&value(flag)?)?),
            "--format" => request.format = format_value(&value(flag)?)?,
            other => return Err(format!("list: unknown argument {other:?}")),
        }
    }
    Ok(Some(request))
}

/// `--appearance`'s value, as `list` and `pack` read it.
pub(crate) fn appearance_value(value: &str) -> Result<Appearance, String> {
    match value {
        "dark" => Ok(Appearance::Dark),
        "light" => Ok(Appearance::Light),
        other => Err(format!("--appearance takes dark or light, not {other:?}")),
    }
}

/// `--format`'s value, as `list` and `pack` read it.
pub(crate) fn format_value(value: &str) -> Result<ThemeListFormat, String> {
    match value {
        "text" => Ok(ThemeListFormat::Text),
        "tsv" => Ok(ThemeListFormat::Tsv),
        other => Err(format!("--format takes text or tsv, not {other:?}")),
    }
}

fn list(args: &[OsString], host: &dyn ThemeHost) -> Outcome {
    match parse_list_args(args) {
        Ok(Some(request)) => Outcome {
            stdout: host.list(&request),
            ..Outcome::default()
        },
        Ok(None) => Outcome {
            stdout: HELP.to_string(),
            ..Outcome::default()
        },
        Err(message) => Outcome::usage_error(&message),
    }
}

struct ImportArgs {
    file: OsString,
    name: Option<String>,
    replace: bool,
}

fn parse_import_args(args: &[OsString]) -> Result<Option<ImportArgs>, String> {
    let mut file: Option<OsString> = None;
    let mut name: Option<String> = None;
    let mut replace = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--replace") => replace = true,
            Some("--name") => {
                let value = iter.next().ok_or("--name needs a value")?;
                name = Some(value.to_str().ok_or("--name must be UTF-8")?.to_string());
            }
            Some(flag) if flag.starts_with("--name=") => {
                name = Some(flag.trim_start_matches("--name=").to_string());
            }
            Some(flag) if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option {flag:?}"));
            }
            _ if file.is_some() => return Err("takes one file".to_string()),
            _ => file = Some(arg.clone()),
        }
    }
    let file = file.ok_or("needs a file to import")?;
    Ok(Some(ImportArgs {
        file,
        name,
        replace,
    }))
}

fn import(args: &[OsString], theme_dir: Option<&Path>) -> Outcome {
    let parsed = match parse_import_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            return Outcome {
                stdout: HELP.to_string(),
                ..Outcome::default()
            };
        }
        Err(message) => return Outcome::usage_error(&message),
    };
    let path = PathBuf::from(&parsed.file);
    let shown = parsed.file.to_string_lossy().into_owned();
    let installed = match scheme_install::install_theme_file(
        &path,
        parsed.name.as_deref(),
        parsed.replace,
        theme_dir,
    ) {
        Ok(installed) => installed,
        Err(InstallError::Refused(message)) => return Outcome::usage_error(&message),
        Err(InstallError::Exists(dest)) => {
            return Outcome::failure(&format!(
                "{} already exists; pass --replace to overwrite it",
                dest.display()
            ));
        }
        Err(InstallError::Failed(message)) => return Outcome::failure(&message),
    };
    let id = &installed.id;
    let mut stdout = format!(
        "imported the {} theme {shown} as {id:?}\n  wrote {}\n",
        installed.format.label(),
        installed.dest.display()
    );
    if installed.dropped.is_empty() {
        stdout.push_str("  dropped nothing\n");
    } else {
        stdout.push_str(&format!(
            "  dropped {} key{} aterm has no place for: {}\n",
            installed.dropped.len(),
            if installed.dropped.len() == 1 {
                ""
            } else {
                "s"
            },
            installed.dropped.join(", ")
        ));
    }
    for note in &installed.notes {
        stdout.push_str(&format!("  note: {note}\n"));
    }
    stdout.push_str(&format!(
        "use it with theme = {id:?} in aterm.toml, or pick it in Settings\n"
    ));
    Outcome {
        stdout,
        ..Outcome::default()
    }
}

/// Read a theme file up to the theme bound (`scheme_install`'s reader, the one `import` uses).
pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    scheme_install::read_bounded(path)
}

/// Write a theme file atomically through `scheme_install`'s one write path;
/// a taken `dest` without `replace` is refused naming `overwrite_flag`.
pub(crate) fn write_atomically(
    dir: &Path,
    dest: &Path,
    id: &str,
    bytes: &[u8],
    replace: bool,
    overwrite_flag: &str,
) -> Result<(), String> {
    scheme_install::write_atomically(dir, dest, id, bytes, replace).map_err(|error| match error {
        InstallError::Exists(dest) => format!(
            "{} already exists; pass {overwrite_flag} to overwrite it",
            dest.display()
        ),
        InstallError::Refused(message) | InstallError::Failed(message) => message,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use aterm_types::scheme_import::{self, MAX_IMPORT_BYTES};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cli-theme-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn never_lists(_: &ThemeListRequest) -> String {
        panic!("only `list` lists")
    }

    fn echo(request: &ThemeListRequest) -> String {
        format!("{request:?}")
    }

    /// A test host: the listing is `.0`, the catalog is the built-ins, and
    /// the classifier is the window's one dark/light rule.
    pub(crate) struct Host(pub(crate) fn(&ThemeListRequest) -> String);

    impl ThemeHost for Host {
        fn list(&self, request: &ThemeListRequest) -> String {
            (self.0)(request)
        }

        fn lookup(&self, id: &str) -> Result<aterm_types::scheme::CatalogTheme, String> {
            aterm_types::scheme::CatalogTheme::builtin(id)
                .ok_or_else(|| "no built-in theme by that name".to_string())
        }

        fn measure(&self, background: aterm_types::Rgb) -> Appearance {
            if aterm_messages::ink::bg_is_light([background.r, background.g, background.b]) {
                Appearance::Light
            } else {
                Appearance::Dark
            }
        }
    }

    pub(crate) const NEVER_LISTS: Host = Host(never_lists);
    const ECHO: Host = Host(echo);

    fn listed(argv: &[&str]) -> Outcome {
        run(&args(argv), None, &ECHO)
    }

    #[test]
    fn list_passes_its_filter_and_format_to_the_listing() {
        let plain = listed(&["list"]);
        assert_eq!(plain.code, 0, "{plain:?}");
        let expect = |appearance, format| format!("{:?}", ThemeListRequest { appearance, format });
        assert_eq!(plain.stdout, expect(None, ThemeListFormat::Text));
        for argv in [
            vec!["list", "--appearance", "light", "--format", "tsv"],
            vec!["list", "--format=tsv", "--appearance=light"],
        ] {
            let out = listed(&argv);
            assert_eq!(out.code, 0, "{argv:?}: {out:?}");
            assert_eq!(
                out.stdout,
                expect(Some(Appearance::Light), ThemeListFormat::Tsv),
                "{argv:?}"
            );
        }
        let dark = listed(&["list", "--appearance", "dark", "--format", "text"]);
        assert_eq!(
            dark.stdout,
            expect(Some(Appearance::Dark), ThemeListFormat::Text)
        );
    }

    #[test]
    fn list_refuses_what_it_does_not_take() {
        for argv in [
            vec!["list", "--json"],
            vec!["list", "extra"],
            vec!["list", "--appearance", "blue"],
            vec!["list", "--appearance"],
            vec!["list", "--format", "csv"],
            vec!["list", "--format="],
        ] {
            let out = run(&args(&argv), None, &NEVER_LISTS);
            assert_eq!(out.code, 2, "{argv:?}: {out:?}");
            assert!(out.stderr.contains(USAGE), "{argv:?}");
            assert!(out.stdout.is_empty(), "{argv:?}");
        }
    }

    #[test]
    fn list_themes_is_the_theme_list_spelling() {
        let rest = args(&["--format", "tsv"]);
        let aliased = list_themes_alias("list-themes", &rest).expect("the alias");
        assert_eq!(aliased, args(&["list", "--format", "tsv"]));
        assert_eq!(
            run(&aliased, None, &ECHO).stdout,
            listed(&["list", "--format", "tsv"]).stdout
        );
        for other in ["theme", "list", "list-fonts", ""] {
            assert!(list_themes_alias(other, &rest).is_none(), "{other:?}");
        }
    }

    const KITTY: &str = "## name: Harbour\nforeground #c0caf5\nbackground #1a1b26\n\
                         url_color #73daca\ncolor1 #f7768e\n";

    #[test]
    fn import_writes_a_native_theme_that_reads_back() {
        let root = scratch("import");
        let source = root.join("harbour.conf");
        std::fs::write(&source, KITTY).unwrap();
        let themes = root.join("themes");
        let out = run(
            &args(&["import", source.to_str().unwrap()]),
            Some(&themes),
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 0, "{out:?}");
        assert!(out.stdout.contains("url_color"), "{}", out.stdout);
        let written = std::fs::read_to_string(themes.join("harbour.conf")).unwrap();
        let parsed = scheme::parse_theme_str(&written).unwrap();
        let converted = scheme_import::convert(scheme_import::ImportFormat::Kitty, KITTY, None)
            .unwrap()
            .scheme;
        assert_eq!(parsed.scheme, converted);
        assert_eq!(parsed.scheme.name, "Harbour");
        let entries: Vec<_> = std::fs::read_dir(&themes).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn printable(text: &str) -> bool {
        !text.chars().any(|c| c.is_control() && c != '\n')
    }

    #[test]
    fn skipped_key_names_reach_the_terminal_escaped() {
        let root = scratch("skipped-escaped");
        for (file, key, listed) in [
            ("evil.conf", "\x1b]0;PWNED\x07", "\\u{1b}]0;pwned\\u{7}"),
            ("plain.conf", "future_key", "future_key"),
        ] {
            let source = root.join(file);
            std::fs::write(
                &source,
                format!(
                    "name = Evil\nschema = 999\nbackground = #000000\n\
                     foreground = #ffffff\n{key} = 1\n"
                ),
            )
            .unwrap();
            let path = source.to_str().unwrap();
            let checked = run(&args(&["check", path]), None, &NEVER_LISTS);
            let imported = run(
                &args(&["import", path, "--replace"]),
                Some(&root.join("themes")),
                &NEVER_LISTS,
            );
            for out in [&checked, &imported] {
                let text = format!("{}{}", out.stdout, out.stderr);
                assert!(printable(&text), "{file}: {text:?}");
                assert!(text.contains(listed), "{file}: {text:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_theme_is_kept_unless_replace() {
        let root = scratch("replace");
        let source = root.join("harbour.conf");
        std::fs::write(&source, KITTY).unwrap();
        let themes = root.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let dest = themes.join("mine.conf");
        std::fs::write(&dest, "old").unwrap();
        let src = source.to_str().unwrap();
        let refused = run(
            &args(&["import", src, "--name", "mine"]),
            Some(&themes),
            &NEVER_LISTS,
        );
        assert_eq!(refused.code, 1, "{refused:?}");
        assert!(refused.stderr.contains("--replace"));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        let replaced = run(
            &args(&["import", src, "--name=mine", "--replace"]),
            Some(&themes),
            &NEVER_LISTS,
        );
        assert_eq!(replaced.code, 0, "{replaced:?}");
        let parsed = scheme::parse_theme_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(parsed.scheme.name, "mine");
        assert_eq!(std::fs::read_dir(&themes).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn urls_builtin_names_and_bad_ids_are_refused_before_reading() {
        let root = scratch("refuse");
        let themes = root.join("themes");
        for argv in [
            vec!["import", "https://example.com/theme.conf"],
            vec!["import", "/nonexistent/Nord.conf"],
            vec!["import", "/nonexistent/x.conf", "--name", "default"],
            vec!["import", "/nonexistent/x.conf", "--name", "dark:x"],
            vec!["import", "/nonexistent/x.conf", "--name", "a/b"],
            vec!["import"],
            vec!["import", "a", "b"],
            vec!["import", "a", "--bogus"],
            vec!["frobnicate"],
            vec![],
        ] {
            let out = run(&args(&argv), Some(&themes), &NEVER_LISTS);
            assert_eq!(out.code, 2, "{argv:?}: {out:?}");
            assert!(out.stderr.contains(USAGE), "{argv:?}");
        }
        assert!(!themes.exists(), "a refusal must write nothing");
        let out = run(
            &args(&["import", "/nonexistent/x.conf"]),
            Some(&themes),
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 1, "{out:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversize_and_unknown_files_fail_and_write_nothing() {
        let root = scratch("oversize");
        let themes = root.join("themes");
        let big = root.join("big.conf");
        std::fs::write(&big, vec![b'\n'; MAX_IMPORT_BYTES + 1]).unwrap();
        let out = run(
            &args(&["import", big.to_str().unwrap()]),
            Some(&themes),
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 1, "{out:?}");
        assert!(out.stderr.contains("at most"), "{}", out.stderr);
        let prose = root.join("prose.txt");
        std::fs::write(&prose, "hello there\n").unwrap();
        let out = run(
            &args(&["import", prose.to_str().unwrap()]),
            Some(&themes),
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 1, "{out:?}");
        assert!(!themes.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn help_answers_on_stdout() {
        for argv in [
            vec!["help"],
            vec!["--help"],
            vec!["import", "--help"],
            vec!["list", "--help"],
        ] {
            let out = run(&args(&argv), None, &NEVER_LISTS);
            assert_eq!(out.code, 0);
            assert!(out.stdout.contains("aterm theme import"), "{argv:?}");
            assert!(out.stdout.contains("aterm theme list"), "{argv:?}");
        }
    }
}
