// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Brew's user aliases (`brew alias`/`unalias`): small shell
//! scripts in the shared alias directory, symlinked as
//! `bin/brew-<name>` in the prefix. The directory, script
//! format, reserved names and error texts follow brew's
//! `Aliases` so aliases written by either tool read back in
//! the other. Invocation re-dispatches a plain
//! `brew <words> $*` script natively; anything richer is a
//! user script abrew does not execute.

use std::path::PathBuf;

use super::prefix::Prefix;

/// Brew's alias directory: `~/.config/brew-aliases` when it
/// exists, else `~/.brew-aliases` (created there by init).
pub fn aliases_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    let config = home.join(".config/brew-aliases");
    if config.exists() {
        return config.canonicalize().ok().or(Some(config));
    }
    let legacy = home.join(".brew-aliases");
    if legacy.exists() {
        return legacy.canonicalize().ok().or(Some(legacy));
    }
    Some(legacy)
}

fn init() -> Result<PathBuf, String> {
    let dir = aliases_dir().ok_or_else(|| "abrew: $HOME is not set".to_string())?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("abrew: cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn script_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn script_path(dir: &std::path::Path, name: &str) -> PathBuf {
    dir.join(script_name(name))
}

fn link_alias(prefix: &Prefix, dir: &std::path::Path, name: &str) {
    let link = prefix.bin().join(format!("brew-{name}"));
    if link
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        let _ = std::fs::remove_file(&link);
    }
    let _ = std::fs::create_dir_all(prefix.bin());
    let target = script_path(dir, name);
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(&target, &link);
    }
    #[cfg(not(unix))]
    {
        let _ = &target;
    }
}

/// Whether `<name>` collides with an existing command file on
/// PATH outside the alias directory (brew's `cmd_exists?`).
fn cmd_exists(dir: &std::path::Path, name: &str) -> bool {
    let aliases_real = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for file in [format!("brew-{name}.rb"), format!("brew-{name}")] {
        for base in std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default()
        {
            let candidate = base.join(&file);
            if candidate.is_file()
                && candidate
                    .canonicalize()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d != aliases_real))
                    .unwrap_or(true)
            {
                return true;
            }
        }
    }
    false
}

fn bash_path() -> String {
    for base in std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let candidate = base.join("bash");
        if candidate.is_file() {
            return candidate.display().to_string();
        }
    }
    "/bin/bash".to_string()
}

/// `brew alias [<name>|<name>=<command>]` (without `--edit`,
/// which stays refused at admission).
pub fn add_or_show(prefix: &Prefix, operand: Option<&str>) -> Result<(), String> {
    let dir = init()?;
    let Some(raw) = operand else {
        show(prefix, &dir, &[]);
        return Ok(());
    };
    let (name, command) = match raw.split_once('=') {
        Some((name, command)) => (name.trim(), Some(command)),
        None => (raw.trim(), None),
    };
    let Some(command) = command else {
        show(prefix, &dir, &[name]);
        return Ok(());
    };
    // Aliases.add, then Alias#write's checks, in brew's order.
    if script_path(&dir, name).exists() {
        return Err(format!("Error: alias 'brew {name}' already exists!"));
    }
    if super::command_names(true).contains(&name) || matches!(name, "alias" | "unalias") {
        return Err(format!("Error: '{name}' is a reserved command. Sorry."));
    }
    if cmd_exists(&dir, name) {
        return Err(format!("Error: 'brew {name}' already exists. Sorry."));
    }
    let command = match command.strip_prefix(['!', '%']) {
        Some(rest) => rest.to_string(),
        None => format!("brew {command}"),
    };
    let content = format!(
        "#! {}\n# alias: brew {name}\n#:  * `{name}` [args...]\n#:    `brew {name}` is an alias for `{command}`\n{command} $*\n\n",
        bash_path()
    );
    let script = script_path(&dir, name);
    std::fs::write(&script, content)
        .map_err(|e| format!("abrew: cannot write {}: {e}", script.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o744));
    }
    link_alias(prefix, &dir, name);
    Ok(())
}

fn show(prefix: &Prefix, dir: &std::path::Path, only: &[&str]) {
    for (name, command) in each(dir, only) {
        println!("brew alias {name}='{command}'");
        let link = prefix.bin().join(format!("brew-{name}"));
        if link.symlink_metadata().is_err() {
            link_alias(prefix, dir, &name);
        }
    }
}

/// Every recorded alias as `(name, command)`, in filename
/// order, following brew's script parsing.
fn each(dir: &std::path::Path, only: &[&str]) -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            continue;
        }
        let echoed = parse_script(&path);
        if let Some((name, command)) = echoed
            && (only.is_empty() || only.contains(&name.as_str()))
        {
            out.push((name, command));
        }
    }
    out
}

fn parse_script(path: &std::path::Path) -> Option<(String, String)> {
    if path.to_string_lossy().ends_with('~') {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let _shebang = lines.next();
    let meta = lines.next().unwrap_or("");
    let name = meta
        .strip_prefix("# alias: brew ")
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .or_else(|| path.file_name()?.to_str().map(str::to_owned))?;
    let first = lines.find(|l| !l.starts_with('#') && !l.trim().is_empty())?;
    let mut command = first.trim_end().to_string();
    if let Some(stripped) = command.strip_suffix(" $*") {
        command = stripped.to_string();
    }
    if let Some(stripped) = command.strip_prefix("brew ") {
        command = stripped.to_string();
    } else {
        command = format!("!{command}");
    }
    Some((name, command))
}

/// `brew unalias <name>…`: brew stops at the first failure.
pub fn remove(prefix: &Prefix, names: &[String]) -> Result<(), String> {
    let dir = init()?;
    for name in names {
        let name = name.trim();
        let link = prefix.bin().join(format!("brew-{name}"));
        let valid = link
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .is_some_and(|d| d == dir.canonicalize().unwrap_or_else(|_| dir.clone()));
        if !valid {
            return Err(format!("Error: 'brew {name}' is not aliased to anything."));
        }
        let _ = std::fs::remove_file(script_path(&dir, name));
        let _ = std::fs::remove_file(&link);
    }
    Ok(())
}

/// The argv words an alias script expands to when it has the
/// generated `brew <words> $*` form; `None` for richer
/// scripts abrew does not execute.
pub fn expansion(name: &str) -> Option<Vec<String>> {
    let dir = aliases_dir()?;
    let (_, command) = each(&dir, &[name]).into_iter().next()?;
    if command.starts_with('!') {
        return None;
    }
    Some(command.split_whitespace().map(str::to_owned).collect())
}
#[cfg(test)]
mod tests {
    use super::{each, parse_script, script_name};

    #[test]
    fn script_round_trip_matches_brew_shape() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("ll");
        std::fs::write(
            &script,
            "#! /bin/bash\n# alias: brew ll\n#:  * `ll` [args...]\n#:    `brew ll` is an alias for `brew list --versions`\nbrew list --versions $*\n\n",
        )
        .unwrap();
        assert_eq!(
            parse_script(&script),
            Some(("ll".to_string(), "list --versions".to_string()))
        );
        std::fs::write(
            tmp.path().join("bang"),
            "#! /bin/bash\n# alias: brew bang\necho hi $*\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("old~"),
            "#! /bin/bash\n# alias: brew old\necho old\n",
        )
        .unwrap();
        assert_eq!(
            each(tmp.path(), &[]),
            vec![
                ("bang".to_string(), "!echo hi".to_string()),
                ("ll".to_string(), "list --versions".to_string()),
            ]
        );
        assert_eq!(script_name("my-cmd@v2"), "my_cmd_v2");
    }
}
