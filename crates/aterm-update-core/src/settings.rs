// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Readers for `[update] enabled` and `[update] auto_apply` in `aterm.toml`, defaulting to `true`
//! where the file says nothing of them (absent, unreadable for a reason that stands, or malformed).
//! A read of `auto_apply` that hits a fault that can pass, or does not answer, holds that look
//! ([`update_auto_apply`]).

use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: u64 = 512 * 1024;

/// Canonical UI label for `[update] enabled`.
pub const CHECK_LABEL: &str = "Check for updates automatically";
/// Canonical UI label for `[update] auto_apply`.
pub const INSTALL_LABEL: &str = "Install updates automatically";

/// Status string explaining why a downloaded build is held when `[update] auto_apply` is off.
#[must_use]
pub fn install_switch_off() -> String {
    format!("\u{201c}{INSTALL_LABEL}\u{201d} is off")
}

/// Resolve the user `aterm.toml` path (`$XDG_CONFIG_HOME`, `%APPDATA%` on Windows, or `~/.config`).
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    // `aterm_types::dirs::config_home`: `$XDG_CONFIG_HOME`, or a test binary's own root.
    if let Some(x) = aterm_types::dirs::config_home() {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// What a parsed `aterm.toml` says of `[update]`'s `key`, read alone so that a typo in one key cannot
/// flip the other: `None` where it says nothing, and otherwise `Some(true)` only for `true`. Anything
/// else may be a person's `false`: a value of the wrong type (`auto_apply = "no"`), and an `update`
/// that is no table, for either key, whatever it holds (`update = false`), or an array, for the keys its
/// tables name (`[[update]]`) and whatever else it holds.
#[must_use]
pub fn update_switch_in(file: &aterm_toml::Table, key: &str) -> Option<bool> {
    let says = |value: &aterm_toml::Value| value.as_bool().unwrap_or(false);
    match file.get("update")? {
        aterm_toml::Value::Table(update) => update.get(key).map(says),
        aterm_toml::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                aterm_toml::Value::Table(update) => update.get(key).map(says),
                other => Some(says(other)),
            })
            .reduce(|one, other| one && other),
        other => Some(says(other)),
    }
}

/// Parse `[update] enabled` from `aterm.toml` text: `false` where [`update_switch_in`] reads it so, and
/// where the text is not TOML and names it or sets `update` off itself
/// ([`broken_text_names_update_enabled`]). Otherwise `true`, with a warning where the text is not TOML.
#[must_use]
pub fn parse_update_enabled(text: &str) -> bool {
    update_enabled_in(text).unwrap_or_else(|e| {
        crate::warn(&format!(
            "aterm.toml does not parse — automatic updates stay on: {e}"
        ));
        true
    })
}

/// [`parse_update_enabled`] without its warning: what `text` says of `[update] enabled`, or why
/// it says nothing.
fn update_enabled_in(text: &str) -> Result<bool, aterm_toml::Error> {
    match aterm_toml::from_str::<aterm_toml::Table>(text) {
        Ok(file) => Ok(update_switch_in(&file, "enabled") != Some(false)),
        Err(_) if broken_text_names_update_enabled(text) => Ok(false),
        Err(e) => Err(e),
    }
}

/// `line` without its comment. A `#` inside a quoted string starts none.
fn without_comment(line: &str) -> &str {
    let mut quote = None;
    for (at, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') => return &line[..at],
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            _ => {}
        }
    }
    line
}

/// The quote opened in `text` that still stands open at its end. A quote inside another's string
/// counts for nothing (`"it's"` closes).
fn open_quote(text: &str) -> Option<char> {
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            _ => {}
        }
    }
    quote
}

/// `open` brackets, after what `line` opens and closes outside its strings.
fn brackets_left_open(line: &str, mut open: usize) -> usize {
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(opened), _) if c == opened => quote = None,
            (None, '[' | '{') => open += 1,
            (None, ']' | '}') => open = open.saturating_sub(1),
            _ => {}
        }
    }
    open
}

/// Whether a character belongs to a word of text that is not TOML: a key's or a value's.
fn in_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-')
}

/// The word `text` starts with, and the rest.
fn first_word(text: &str) -> (&str, &str) {
    text.split_at(text.find(|c: char| !in_word(c)).unwrap_or(text.len()))
}

/// Whether what follows a key, after its separators and any whitespace (a pasted no-break space is what
/// broke some files), sets anything but `true`. The word counts in either case, and what comes after it
/// is a slip (`True`, `true;`, `true // note`, the next key after a missing comma) unless that is `false`
/// (`true false`), which may be a person's `false` beside the `true` they meant to replace.
fn value_says_off(after: &str) -> bool {
    let value = after.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '=' | ':'));
    let (first, rest) = first_word(value);
    let (next, _) = first_word(rest.trim_start());
    !first.eq_ignore_ascii_case("true") || next.eq_ignore_ascii_case("false")
}

/// Whether a line of text that is not TOML, with no comment, names `key` with any value but `true`
/// ([`value_says_off`]), however it separates them (`enabled: true`, `enabled true`), each key of an
/// inline table read alone. A longer word does not name it
/// (`enabled-fork`), and neither does a quoted string with no `=` or `:` after it, which is a value
/// (`repo = "enabled"`). A quote that never closes on the line hides no key: it is the line's mistake.
fn line_says_off(line: &str, key: &str) -> bool {
    line.match_indices(key).any(|(at, _)| {
        let (before, after) = (&line[..at], &line[at + key.len()..]);
        if before.ends_with(in_word) || after.starts_with(in_word) {
            return false;
        }
        let after = if let Some(quote) = open_quote(before) {
            let after = if after.contains(quote) {
                match after.strip_prefix(quote) {
                    Some(after) => after,
                    None => return false,
                }
            } else {
                after.strip_prefix(['"', '\'']).unwrap_or(after)
            };
            if !after.trim_start().starts_with(['=', ':']) {
                return false;
            }
            after
        } else {
            after
        };
        value_says_off(after)
    })
}

/// Whether text that is not TOML, above its first table, sets `update` itself to anything but `true`,
/// an inline table, or one of those or nothing in an array, as [`update_switch_in`] reads text that is
/// TOML: `update = false` said nothing here, so a typo on another line turned a hold into an install.
/// With no separator only the word `false` counts, since `update notes` is prose. It reads the line that
/// sets `update`: an array's later rows say nothing.
fn broken_text_sets_update_off(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|line| without_comment(line).trim())
        .take_while(|line| !line.starts_with('['))
        .any(|line| {
            let name = line.trim_start_matches(['"', '\'']);
            let Some(rest) = name.strip_prefix("update") else {
                return false;
            };
            let rest = rest.trim_start_matches(['"', '\'']);
            let spaced = rest.trim_start();
            let Some(value) = spaced.strip_prefix(['=', ':']) else {
                return spaced.len() < rest.len() && spaced == "false";
            };
            let value = value
                .trim()
                .trim_start_matches('[')
                .trim_end_matches([']', ','])
                .trim();
            !(value.is_empty() || value == "true" || value.starts_with('{'))
        })
}

/// Whether text that is not TOML has a line naming `[update] enabled` ([`line_says_off`]), which may be
/// a person's `false`. `enabled` is a key of many tables, so unlike [`broken_text_names_auto_apply`] only
/// a line under an `[update]` header counts, a broken or quoted one included, one starting with `update`
/// (`update.enabled`, `update = { enabled = false }`), and one inside a value such a line left open
/// (`update = {` alone on its line). While a bracket stands open a line starting `[` is an array's row,
/// not a header, unless it names `update`, since a broken file may never close that bracket. A
/// `[update.enabled]` header names it too, unless it goes on to set `true`, and so does `update` set off
/// itself ([`broken_text_sets_update_off`]).
#[must_use]
pub fn broken_text_names_update_enabled(text: &str) -> bool {
    if broken_text_sets_update_off(text) {
        return true;
    }
    let (mut in_update, mut in_update_value, mut open) = (false, false, 0);
    for line in text.trim_start_matches('\u{feff}').lines() {
        let mut line = without_comment(line).trim();
        if line.starts_with('[') || line.starts_with("update]") {
            let header = line.trim_start_matches('[');
            let (name, rest) = header.split_once(']').unwrap_or((header, ""));
            let end = name
                .find(|c: char| !(c.is_alphanumeric() || "_-.\"' \t".contains(c)))
                .unwrap_or(name.len());
            let (name, value) = name.split_at(end);
            let name: String = name.chars().filter(|c| !"\"' \t".contains(*c)).collect();
            if name == "update.enabled" && value_says_off(value) {
                return true;
            }
            if name == "update" || open == 0 {
                in_update = name == "update";
                open = 0;
                line = rest.trim_start_matches(']').trim();
            }
        }
        let names_update = line.trim_start_matches(['"', '\'']).starts_with("update");
        if (in_update || in_update_value || names_update) && line_says_off(line, "enabled") {
            return true;
        }
        open = brackets_left_open(line, open);
        in_update_value = open > 0 && (in_update_value || names_update);
    }
    false
}

/// [`read_config_text`], with any error reading as no text.
#[must_use]
pub fn read_config_file(path: &Path) -> Option<String> {
    read_config_text(path).ok().flatten()
}

/// Whether `error`, from reading the config file, can pass by itself. Only an error about the file
/// itself stands until someone changes something, and reads as a file with no text: no permission, a
/// loop of links, a name too long, a path that is no file. Any other says nothing of what the file
/// holds (a volume that stalled or went away, a network down, a process short of memory or
/// descriptors), so a person's `auto_apply = false` may be behind it.
#[must_use]
pub fn read_error_is_passing(error: &std::io::Error) -> bool {
    use std::io::ErrorKind::{InvalidInput, IsADirectory, NotADirectory, PermissionDenied};
    if matches!(
        error.kind(),
        PermissionDenied | NotADirectory | IsADirectory | InvalidInput
    ) {
        return false;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        return ![
            libc::ELOOP,
            libc::ENAMETOOLONG,
            libc::EOPNOTSUPP,
            libc::ENOTSUP,
            libc::EILSEQ,
            libc::EOVERFLOW,
        ]
        .contains(&code);
    }
    true
}

/// The text of the config file at `path`, bounded, with no rule on its owner or mode: `Ok(None)` if
/// absent, non-regular (a socket at that name included) or oversized. Bytes that are not UTF-8 are
/// replaced, so a stray byte in a comment hides no setting. Only a read that can hold an update back
/// takes text this way, since anyone able to write such a file wrote it.
/// # Errors
/// The open or the read failed for any other reason than an absent file.
pub fn read_config_text(path: &Path) -> std::io::Result<Option<String>> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match opts.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // A socket, or a device with nothing behind it, cannot be opened: a name that is no file.
        #[cfg(unix)]
        Err(error)
            if matches!(error.raw_os_error(), Some(libc::ENXIO | libc::EOPNOTSUPP))
                && std::fs::metadata(path).is_ok_and(|meta| !meta.is_file()) =>
        {
            return Ok(None);
        }
        // Windows opens no folder for reading (std asks no FILE_FLAG_BACKUP_SEMANTICS): a folder
        // at the name, or a link (junction, symlink) the open followed to one, is refused as
        // access denied, and is a name that is no file. The look follows links as the open did;
        // a file whose ACL denies reading still reads as a file and stays an error.
        #[cfg(windows)]
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && std::fs::metadata(path).is_ok_and(|meta| meta.is_dir()) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(MAX_CONFIG_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Read [`parse_update_enabled`] from `path` (`true` if absent, non-regular, oversized, or on any read
/// error, one that can pass included; [`update_enabled_at_launch`] keeps no answer from one that can
/// pass).
#[must_use]
pub fn update_enabled_at(path: &Path) -> bool {
    read_config_file(path).is_none_or(|text| parse_update_enabled(&text))
}

/// Whether text that is not TOML has a line naming `auto_apply` ([`line_says_off`]), whatever table it
/// would sit in, or sets `update` off itself ([`broken_text_sets_update_off`]), either of which may be
/// a person's `false`.
#[must_use]
pub fn broken_text_names_auto_apply(text: &str) -> bool {
    broken_text_sets_update_off(text)
        || text
            .lines()
            .any(|line| line_says_off(without_comment(line), "auto_apply"))
}

/// Parse `[update] auto_apply` from `aterm.toml` text: `false` where [`update_switch_in`] reads it so,
/// and where the text is not TOML and names it or sets `update` off itself
/// ([`broken_text_names_auto_apply`]). Otherwise `true`.
#[must_use]
pub fn parse_update_auto_apply(text: &str) -> bool {
    aterm_toml::from_str::<aterm_toml::Table>(text).map_or_else(
        |_| !broken_text_names_auto_apply(text),
        |file| update_switch_in(&file, "auto_apply") != Some(false),
    )
}

/// How long [`update_auto_apply`] waits on the config file. Far past a small file's read on a loaded
/// Mac, and past it only a file that does not answer (a followed symlink onto a volume that stopped,
/// a dialog nobody answers).
const AUTO_APPLY_READ_WITHIN: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The helper threads [`update_auto_apply`] reads on; while one stays out the next is refused at once.
static AUTO_APPLY_READS: aterm_uds::deadline::Gate =
    aterm_uds::deadline::Gate::new("aterm-update-config-read");

/// Read [`parse_update_auto_apply`] from the user config: `true` if absent, oversized, or unreadable
/// for a reason that stands. A read that hits a fault that can pass ([`read_error_is_passing`]), or
/// does not answer, answers `false` for this look and the next asks again: a person's `false` may be
/// behind it, and installing over that is the worse mistake.
#[must_use]
pub fn update_auto_apply() -> bool {
    let path = config_path();
    auto_apply_within(&AUTO_APPLY_READS, AUTO_APPLY_READ_WITHIN, move || {
        path.map_or(Ok(None), |path| read_config_text(&path))
    })
}

/// [`parse_update_auto_apply`] of what `read` answers within `within` on a helper of `gate`.
fn auto_apply_within(
    gate: &'static aterm_uds::deadline::Gate,
    within: std::time::Duration,
    read: impl FnOnce() -> std::io::Result<Option<String>> + Send + 'static,
) -> bool {
    match gate.run(within, within, read) {
        Ok(Ok(text)) => text.is_none_or(|text| parse_update_auto_apply(&text)),
        Ok(Err(error)) if read_error_is_passing(&error) => {
            crate::warn(&format!(
                "[update] auto_apply could not be read ({error}): this look leaves the update and \
                 the next asks again"
            ));
            false
        }
        Ok(Err(_)) => true,
        Err(refused) => {
            crate::warn(&format!(
                "[update] auto_apply did not answer within {} s ({refused:?}): this look leaves \
                 the update and the next asks again",
                within.as_secs()
            ));
            false
        }
    }
}

/// `[update] enabled` as this process launched with it: read by the process's first call
/// (which warns of a config that does not parse) and kept until the next launch, whatever the file
/// says later. `None` where that read hit a fault that can pass ([`read_error_is_passing`]): that
/// says nothing of a person's `false`, so nothing is kept, and later calls answer `None` with no
/// read until [`update_enabled_at_launch_read`] keeps an answer. Only the first call reads, so a
/// volume that stalls before it fails costs a launch one stall, not one per caller.
#[must_use]
pub fn update_enabled_at_launch() -> Option<bool> {
    kept_or_first_read(&LAUNCH_ENABLED, &LAUNCH_READ, read_enabled_at_launch)
}

/// [`update_enabled_at_launch`], reading the file again while nothing is kept: for a thread that
/// waits on the answer, never one a person waits on.
#[must_use]
pub fn update_enabled_at_launch_read() -> Option<bool> {
    kept_or_read(&LAUNCH_ENABLED, read_enabled_at_launch)
}

/// What [`update_enabled_at_launch`] has kept so far, with no file read: `None` until a read
/// answered.
#[must_use]
pub fn update_enabled_kept() -> Option<bool> {
    LAUNCH_ENABLED.get().copied()
}

/// The answer [`update_enabled_at_launch`] keeps.
static LAUNCH_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether [`update_enabled_at_launch`] has made its one read.
static LAUNCH_READ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// [`update_enabled_at_launch`], with a launch read that has not answered yet taken as off for this
/// call: a person's `false` may be behind it.
#[must_use]
pub fn update_enabled() -> bool {
    update_enabled_at_launch().unwrap_or(false)
}

/// One read of `[update] enabled` for [`update_enabled_at_launch`]: on where there is no config.
fn read_enabled_at_launch() -> Option<bool> {
    config_path().map_or(Some(true), |path| {
        launch_enabled_of(read_config_text(&path))
    })
}

/// What `cell` keeps, or else what `read` says, kept in `cell` once it says anything.
fn kept_or_read(
    cell: &std::sync::OnceLock<bool>,
    read: impl FnOnce() -> Option<bool>,
) -> Option<bool> {
    if let Some(kept) = cell.get() {
        return Some(*kept);
    }
    let read = read()?;
    Some(*cell.get_or_init(|| read))
}

/// [`kept_or_read`] for the first call that finds `read_once` unset, which sets it; `None` with no
/// read for every later call while `cell` keeps nothing.
fn kept_or_first_read(
    cell: &std::sync::OnceLock<bool>,
    read_once: &std::sync::atomic::AtomicBool,
    read: impl FnOnce() -> Option<bool>,
) -> Option<bool> {
    if let Some(kept) = cell.get() {
        return Some(*kept);
    }
    if read_once.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return None;
    }
    kept_or_read(cell, read)
}

/// What one read of the config file says of `[update] enabled` at launch
/// ([`update_enabled_at_launch`]): as [`saved_enabled_of`], with the parser's warning.
fn launch_enabled_of(read: std::io::Result<Option<String>>) -> Option<bool> {
    match read {
        Ok(text) => Some(text.is_none_or(|text| parse_update_enabled(&text))),
        Err(error) => (!read_error_is_passing(&error)).then_some(true),
    }
}

/// `[update] enabled` as the user config holds it now, read again at each call
/// ([`update_enabled_saved_at`] of [`config_path`]): a long-lived process compares it with
/// [`update_enabled_at_launch`] to learn that the setting changed since it started (no answer on
/// either side is no change). `None` where the read hit
/// a fault that can pass ([`read_error_is_passing`]), which says nothing of the setting: it is no
/// change, and the next read asks again.
#[must_use]
pub fn update_enabled_saved() -> Option<bool> {
    update_enabled_saved_at(config_path().as_deref())
}

/// [`update_enabled_at`] of `path` (`Some(true)` with none), but quiet: a config that does not
/// parse and says nothing of the key reads as on without a warning, since a caller that reads it again
/// and again would repeat the one warning [`update_enabled_at_launch`]'s kept read gave. `None` as
/// [`update_enabled_saved`] says.
fn update_enabled_saved_at(path: Option<&Path>) -> Option<bool> {
    path.map_or(Some(true), |path| saved_enabled_of(read_config_text(path)))
}

/// What one read of the config file says of `[update] enabled` ([`update_enabled_saved_at`]).
fn saved_enabled_of(read: std::io::Result<Option<String>>) -> Option<bool> {
    match read {
        Ok(text) => Some(text.is_none_or(|text| update_enabled_in(&text).unwrap_or(true))),
        Err(error) => (!read_error_is_passing(&error)).then_some(true),
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
