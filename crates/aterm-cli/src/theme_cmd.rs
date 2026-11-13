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

/// `aterm theme help`, and `--help` on every subcommand but `pack` and `install` (those print
/// [`theme_pack_cmd::PACK_HELP`]).
pub(crate) const HELP: &str = "\
aterm theme list [--appearance dark|light] [--format text|tsv]

  List every theme: the built-ins (Default first), then each <id>.conf in
  the themes folder, loaded or not, with its id (what `theme = \"<id>\"`
  takes), label, appearance, pair, source, status, type (the typography it
  sets) and description, and any warnings or the reason it did not load.
  The themes folder's path closes the listing, with a truncation line when
  the folder holds more than aterm reads. `aterm list-themes` is the same
  command.

  --appearance dark|light   Only the themes drawn for that appearance.
  --format tsv              Tab-separated rows for scripts: a header row, one
                            row per theme with a fixed column count, then a
                            `#folder` line and, if truncated, `#truncated`.

aterm theme check <file> [--strict] [--signature] [--format text|json]

  Read a theme file as the themes folder will: parse it, run the checks it
  loads under, and measure each contrast it rests on (text, each ANSI
  colour, the selection band and the text on it, the cursor, and each of
  accent_color, border_color and search_match_color it sets). Each
  diagnostic names its line where it has one (and, in JSON, its key).

  Exit status: 0 the theme loads, 1 it loads with warnings and --strict was
  given, 2 it does not load (or a usage error).

  --strict         Warnings fail the check (exit 1); notes never do.
  --signature      Also measure the stricter bar aterm's signature themes
                   (aterm Night and Day, Hearth, Linen) clear: text 7:1; each
                   accent (ANSI 1-6, brights 9-14) 4.5:1; a tier's accents 25
                   deg apart in OkLCh hue; each bright 0.05 OkLab from its
                   normal; color8 in [3, 4.5); color0 1.15:1; color7 and
                   color15 4.5:1; the selection band 1.5:1 with text 4.5:1 on
                   it; a cursor at 3:1, OkLCh chroma 0.08 or more, within 15
                   deg of a normal accent's (ANSI 1-6) hue; accent chroma
                   0.07-0.17, max/min at most 1.75. A missed bar is listed,
                   and fails --strict.
  --format json    One JSON object: {document: \"aterm-theme-check\", version: 2,
                   file, id, status: ok|warnings|invalid, strict, exit_code,
                   theme: null | {display_name, description, author,
                   appearance, appearance_source: declared|measured,
                   foreground, background, selection, selection_derived,
                   bold_is_bright, palette_generate, typography,
                   skipped_keys},
                   diagnostics: [{severity: error|warning|note, code, line,
                   key, message, foreign_format}],
                   contrast: [{drawn, on, ratio, minimum, below_minimum}],
                   signature: null | [{code, message}]}. A missing value is
                   null; a version only ever gains fields.

  Diagnostic codes: errors parse, unreadable, not_utf8; warnings
  low_text_contrast, faint_selection, ansi_matches_background,
  low_cursor_contrast, chrome_below_floor, search_match_like_selection,
  appearance_mismatch, skipped_keys (load checks), palette_anchors_close
  (palette_generate's derived palette), and not_conf, invalid_id,
  shadowed_by_builtin, symlink_not_followed (the file name); notes
  palette_generate, font_not_installed.

aterm theme new <name> [--from <theme>] [--appearance dark|light] [--force]

  Write <name>.conf into the themes folder, each key under a comment saying
  what it does, and print its path. Its colours, typography and
  bold_is_bright come from --from (a built-in or a themes-folder theme that
  loads), else Default, or Default Light with --appearance light. When that
  theme is drawn for the appearance --appearance does not name, its pair is
  used instead; one with no pair (every themes-folder theme) is refused. A
  built-in's name is refused; an existing theme is kept unless --force.

aterm theme export <name> [--out <file>] [--force]

  Print a theme (a built-in or a themes-folder theme that loads) as an aterm
  theme file. --out writes it to <file> and prints the path instead; an
  existing file is kept unless --force.

aterm theme schema [--format json|text]

  Describe every key a theme file can hold, from the parser's own key
  table. JSON (the default) is {document: \"aterm-theme-schema\", version: 2,
  syntax, keys: [{name, aliases, type, required, index, constraints,
  when_omitted, default, description, example, group, config_key}]};
  --format text is the same table for a person.

aterm theme import <file> [--name <id>] [--replace]

  Convert another terminal's theme (or an aterm theme file's colours) into a
  theme in the themes folder, where Settings and `theme = \"<id>\"` find it:
  kitty, Ghostty, Alacritty or WezTerm (TOML), iTerm2 (.itermcolors),
  Terminal.app (.terminal), Windows Terminal (JSON) or base16/base24 (YAML),
  told apart by the file's extension (a .toml or .plist file's contents then
  tell WezTerm from Alacritty and Terminal.app from iTerm2), else its
  contents. Only a local file up to 256 KiB is read; no URL, include or
  import line is followed. Keys aterm has no place for are listed, not
  fatal.

  --name <id>   The theme's id (its file name) and display name; default the
                file's name without its extension, keeping the theme's own
                display name. A built-in's name is refused. In a Windows
                Terminal file with several schemes it picks one by name
                (else the first).
  --replace     Overwrite an existing theme with this id.

aterm theme pack [<id>] [--appearance dark|light] [--format text|tsv] [--licences]
aterm theme install <id>... | --all [--replace]

  The community theme pack built into aterm, and copying its themes into
  the themes folder: `aterm theme pack --help` describes both.

list, new, export, schema and import exit 0 when done, 1 when they fail (a
file that exists or cannot be read, an unknown theme), 2 on a usage error;
pack and install exit as `aterm theme pack --help` says.
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

/// `aterm theme …` from the front door, its answer spelled for the shell that reads each stream
/// (the CLI's `cli_text`).
pub fn main_entry(args: Vec<OsString>, host: &dyn ThemeHost) -> ExitCode {
    let outcome = run(&args, scheme::user_theme_dir().as_deref(), host);
    ExitCode::from(delivered(
        &outcome,
        &|text| aterm_uds::stdout::answer(&crate::cli_text::for_stdout(text)),
        &|text| eprint!("{}", crate::cli_text::for_stderr(text)),
    ))
}

/// Write `outcome` to `stdout` and `stderr`: its stderr goes out even when its stdout could not
/// be written. The exit status is its code, or 1, with the failure's line on `stderr`, when the
/// stdout was not written.
fn delivered(
    outcome: &Outcome,
    stdout: &dyn Fn(&str) -> std::io::Result<()>,
    stderr: &dyn Fn(&str),
) -> u8 {
    let written = stdout(&outcome.stdout);
    stderr(&outcome.stderr);
    match written {
        Ok(()) => outcome.code,
        Err(e) => {
            stderr(&format!("aterm theme: the answer was not written ({e})\n"));
            1
        }
    }
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
    let mut format_given = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let arg = arg
            .to_str()
            .ok_or_else(|| format!("unknown argument {:?}", arg.to_string_lossy()))?;
        if matches!(arg, "--help" | "-h") {
            return Ok(None);
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg, None),
        };
        let mut value = |flag: &str| -> Result<String, String> {
            match inline {
                Some(value) => Ok(value.to_string()),
                None => {
                    let raw = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
                    raw.to_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("argument {:?} is not UTF-8", raw.to_string_lossy()))
                }
            }
        };
        match flag {
            "--appearance" => {
                if request.appearance.is_some() {
                    return Err("list: --appearance is given twice".to_string());
                }
                request.appearance = Some(appearance_value(&value(flag)?)?);
            }
            "--format" => {
                if format_given {
                    return Err("list: --format is given twice".to_string());
                }
                request.format = format_value(&value(flag)?)?;
                format_given = true;
            }
            other => return Err(format!("list: unknown argument {other:?}")),
        }
    }
    Ok(Some(request))
}

/// `--appearance`'s value, as `list`, `new` and `pack` read it.
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
                if name.is_some() {
                    return Err("--name is given twice".to_string());
                }
                let value = iter.next().ok_or("--name needs a value")?;
                name = Some(value.to_str().ok_or("--name must be UTF-8")?.to_string());
            }
            Some(flag) if flag.starts_with("--name=") => {
                if name.is_some() {
                    return Err("--name is given twice".to_string());
                }
                name = flag.strip_prefix("--name=").map(str::to_string);
            }
            Some(flag) if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("unknown option {flag:?}"));
            }
            None if arg.as_encoded_bytes().starts_with(b"--name=") => {
                if name.is_some() {
                    return Err("--name is given twice".to_string());
                }
                return Err("--name must be UTF-8".to_string());
            }
            None if arg.as_encoded_bytes().starts_with(b"-")
                && arg.as_encoded_bytes().len() > 1 =>
            {
                return Err(format!("unknown option {:?}", arg.to_string_lossy()));
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
#[path = "theme_cmd_tests.rs"]
pub(crate) mod tests;
