// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-cache placement for `targo` ([`decide`], [`NOINDEX`], [`target_build_dir`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The cache root, relative to `$HOME`.
pub const CACHE_UNDER_HOME: &str = "Library/Caches/aterm/cargo-build.noindex";

/// The last component of every workspace's build directory under the cache.
pub const LEAF: &str = ".cargo-build";

/// The suffix of every build directory a tool of this repository chooses.
pub const NOINDEX: &str = ".noindex";

/// `name` named for Spotlight to skip: `tippy` is `tippy.noindex`.
#[must_use]
pub fn noindex(name: &str) -> String {
    format!("{name}{NOINDEX}")
}

/// Leaf directory for build intermediates under a custom target ([`BuildDir::Target`]).
pub const TARGET_BUILD_LEAF: &str = "build.noindex";

/// The subcommands that get a build directory.
pub const BUILD_VERBS: &[&str] = &[
    "build", "b", "check", "c", "test", "t", "run", "r", "bench", "doc", "d", "rustc", "rustdoc",
    "clippy", "tippy", "fix", "clean",
];

/// `$HOME/Library/Caches/aterm/cargo-build.noindex`.
#[must_use]
pub fn cache_root(home: &Path) -> PathBuf {
    home.join(CACHE_UNDER_HOME)
}

/// The build directory Cargo resolves for the workspace at `workspace` (absolute).
#[must_use]
pub fn build_dir(root: &Path, workspace: &Path) -> PathBuf {
    let mut s = root.as_os_str().to_owned();
    s.push(workspace.as_os_str());
    PathBuf::from(s).join(LEAF)
}

/// The `--config` value both entry points pass, as a TOML literal string.
#[must_use]
pub fn config_value(root: &Path) -> Option<String> {
    let root = root.to_str()?;
    if root
        .chars()
        .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(format!("build.build-dir='{root}{{workspace-root}}/{LEAF}'"))
}

/// Which tool reads the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Trust's `targo` (or Trust's own `cargo`, which is the same program).
    Targo,
    /// `targo-tippy`, the lint: every argument is its own, and it takes no lane.
    Tippy,
    /// An upstream `cargo`.
    Cargo,
}

/// What [`decide`] reads of the machine.
pub struct Machine<'a> {
    /// `$HOME`.
    pub home: Option<&'a Path>,
    /// The working directory, as the kernel names it (no symlinks).
    pub cwd: Option<&'a Path>,
    /// One environment variable.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// A config file's text.
    pub read: &'a dyn Fn(&Path) -> Result<Option<String>, ()>,
    /// Whether this process may write the directory (`access(2)`).
    pub writable: &'a dyn Fn(&Path) -> Option<bool>,
}

/// Environment variable carrying the build directory to `targo trust` ([`BuildDir::TrustEnv`]).
pub const BUILD_DIR_ENV: &str = "CARGO_BUILD_BUILD_DIR";

/// Where [`decide`] puts a command's build intermediates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildDir {
    /// The shared cache, `<cache><workspace>/.cargo-build`: this `--config` value first.
    Cache(String),
    /// `<target>/build.noindex` under the target directory the command builds into.
    Target(String),
    /// Build directory passed to `targo trust` via [`BUILD_DIR_ENV`].
    TrustEnv(String),
}

/// Decide the build-cache placement ([`BuildDir::Cache`], [`BuildDir::Target`]).
#[must_use]
pub fn decide(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<BuildDir> {
    let home = m
        .home
        .filter(|h| h.is_absolute() && !h.as_os_str().as_encoded_bytes().ends_with(b"/"))?;
    let cwd = m.cwd?;
    let root = cache_root(home);
    let cache = config_value(&root)?;
    if !cwd.starts_with(home) || cwd == home || cwd.starts_with(&root) || has_brace(cwd) {
        return None;
    }
    if (m.env)("PWD").is_some_and(|p| Path::new(&p) != cwd) {
        return None;
    }
    if (m.env)(BUILD_DIR_ENV).is_some() {
        return None;
    }
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    let mut line = Line::read(&args[at..], tool, home, &root)?;
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".cargo"), PathBuf::from);
    let mut target_named = false;
    let mut alias_builds = false;
    for dir in cwd
        .ancestors()
        .map(|d| d.join(".cargo"))
        .chain([cargo_home])
    {
        let named = config_scan(&dir, line.alias.as_deref(), m);
        if named.build {
            return None;
        }
        target_named |= named.target;
        alias_builds |= named.alias_builds;
    }
    if line.alias.is_some() && !alias_builds {
        return None;
    }
    let chosen = line
        .target_dir
        .take()
        .map(OsString::from)
        .or_else(|| (m.env)("CARGO_TARGET_DIR"))
        .or_else(|| (m.env)("CARGO_BUILD_TARGET_DIR"));
    let Some(target) = chosen else {
        if target_named {
            return None;
        }
        if line.cache && cache_writable(home, &root, m) {
            return Some(BuildDir::Cache(cache));
        }
        if in_noindex(cwd) {
            return None;
        }
        return Some(line.place(format!("{{workspace-root}}/target/{TARGET_BUILD_LEAF}")));
    };
    let target = target.to_str()?;
    if target.is_empty() {
        return None;
    }
    let target = if Path::new(target).is_absolute() {
        target.to_owned()
    } else {
        format!("{}/{target}", cwd.to_str()?)
    };
    if in_noindex(Path::new(&target))
        || target
            .chars()
            .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(line.place(format!("{target}/{TARGET_BUILD_LEAF}")))
}

/// `args` with the `--config` [`decide`] adds, or `None` when it adds none.
#[must_use]
pub fn with_build_dir(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<Vec<String>> {
    let (BuildDir::Cache(value) | BuildDir::Target(value)) = decide(args, tool, m)? else {
        return None;
    };
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    let mut out = Vec::with_capacity(args.len() + 2);
    out.extend_from_slice(&args[..at]);
    out.push("--config".to_owned());
    out.push(value);
    out.extend_from_slice(&args[at..]);
    Some(out)
}

/// Whether the cache, or the folder it will be made in, may be written.
fn cache_writable(home: &Path, root: &Path, m: &Machine<'_>) -> bool {
    (m.writable)(root)
        .or_else(|| root.parent().and_then(|p| (m.writable)(p)))
        .or_else(|| (m.writable)(&home.join("Library/Caches")))
        .unwrap_or(false)
}

/// Whether a folder of `p`'s is named `*.noindex`: Spotlight skips it already.
fn in_noindex(p: &Path) -> bool {
    p.components().any(|c| {
        c.as_os_str()
            .as_encoded_bytes()
            .ends_with(NOINDEX.as_bytes())
    })
}

/// Whether a path holds a `{` or `}`.
fn has_brace(p: &Path) -> bool {
    p.as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|b| matches!(b, b'{' | b'}'))
}

/// What the verb and flags half of the rule reads of a command line.
struct Line {
    /// An `--unverified` build, the lint or `clean`: the shared cache is for it.
    cache: bool,
    /// `targo trust`, which takes its build directory from the environment.
    trust: bool,
    /// A `--target-dir` before `--`.
    target_dir: Option<String>,
    /// A verb cargo does not build by itself, which builds only as an alias that does.
    alias: Option<String>,
}

impl Line {
    /// A building verb, or one an alias may make one.
    fn read(args: &[String], tool: Tool, home: &Path, root: &Path) -> Option<Self> {
        let config_ok = |v: &str| v.contains('=') && !names_a_dir(v);
        let manifest_ok = |v: &str| {
            let p = Path::new(v);
            !v.is_empty()
                && !v.contains("..")
                && !has_brace(p)
                && !p.starts_with(root)
                && (!v.starts_with('/') || p.starts_with(home))
        };
        let mut verb = (tool == Tool::Tippy).then_some("tippy");
        let mut unverified = false;
        let mut target_dir = None;
        let mut it = args.iter().map(String::as_str);
        while let Some(a) = it.next() {
            let ok = match a {
                "--" => break,
                "--unverified" => {
                    unverified = true;
                    true
                }
                "--config" => it.next().is_some_and(config_ok),
                "--manifest-path" => it.next().is_some_and(manifest_ok),
                "--target-dir" => it.next().map(|v| target_dir = Some(v.to_owned())).is_some(),
                "-Z" | "--color" | "--explain" => verb.is_some() || it.next().is_some(),
                _ => {
                    if let Some(v) = a.strip_prefix("--config=") {
                        config_ok(v)
                    } else if let Some(v) = a.strip_prefix("--manifest-path=") {
                        manifest_ok(v)
                    } else if let Some(v) = a.strip_prefix("--target-dir=") {
                        target_dir = Some(v.to_owned());
                        true
                    } else if a.starts_with("-C") {
                        false
                    } else {
                        if !a.starts_with('-') && verb.is_none() {
                            verb = Some(a);
                        }
                        true
                    }
                }
            };
            if !ok {
                return None;
            }
        }
        let lane = tool == Tool::Cargo || unverified;
        let (cache, trust, alias) = match verb {
            Some("clean" | "tippy" | "clippy") => (true, false, None),
            Some("trust") if tool == Tool::Targo => (false, true, None),
            Some(v) if BUILD_VERBS.contains(&v) && lane => (true, false, None),
            Some(v) if lane && !BUILD_VERBS.contains(&v) => (true, false, Some(v.to_owned())),
            _ => return None,
        };
        Some(Self {
            cache,
            trust,
            target_dir,
            alias,
        })
    }

    /// `dir` (`<target>/build.noindex`) handed over the way this command reads it.
    fn place(&self, dir: String) -> BuildDir {
        if self.trust {
            BuildDir::TrustEnv(dir)
        } else {
            BuildDir::Target(format!("build.build-dir='{dir}'"))
        }
    }
}

/// Whether a config value or line names a target or build directory.
fn names_a_dir(s: &str) -> bool {
    s.contains("target-dir") || s.contains("build-dir")
}

/// What `dir/config.toml` or `dir/config` says on lines that are not comments.
#[derive(Default)]
struct Named {
    build: bool,
    target: bool,
    alias_builds: bool,
}

/// [`Named`] for the config files in `dir`, `alias` the verb to look up.
fn config_scan(dir: &Path, alias: Option<&str>, m: &Machine<'_>) -> Named {
    let mut named = Named::default();
    for name in ["config.toml", "config"] {
        match (m.read)(&dir.join(name)) {
            Err(()) => {
                return Named {
                    build: true,
                    target: true,
                    alias_builds: false,
                };
            }
            Ok(None) => {}
            Ok(Some(text)) => {
                let mut in_alias = false;
                for line in text.lines() {
                    let x = line.trim_start_matches([' ', '\t']);
                    if x.starts_with('#') {
                        continue;
                    }
                    named.build |= line.contains("build-dir");
                    named.target |= line.contains("target-dir");
                    if x.starts_with('[') {
                        in_alias = x.starts_with("[alias]");
                    } else if let Some(verb) = alias {
                        named.alias_builds |= alias_builds(x, verb, in_alias);
                    }
                }
            }
        }
    }
    named
}

/// Whether the line `x` defines `verb` as an alias whose first word is a building verb.
fn alias_builds(x: &str, verb: &str, in_alias: bool) -> bool {
    let rest = match x.strip_prefix("alias.").and_then(|r| r.strip_prefix(verb)) {
        Some(rest) => rest,
        None if in_alias => match x.strip_prefix(verb) {
            Some(rest) => rest,
            None => return false,
        },
        None => return false,
    };
    if !rest.starts_with([' ', '\t', '=']) {
        return false;
    }
    let Some((_, value)) = x.split_once('=') else {
        return false;
    };
    let value = value.trim_start_matches([' ', '\t', '[', '"', '\'']);
    let end = value
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(value.len());
    BUILD_VERBS.contains(&&value[..end])
}

/// Whether `dir/config.toml` or `dir/config` holds a line `names` holds that is not a comment.
fn config_names(dir: &Path, m: &Machine<'_>, names: fn(&str) -> bool) -> bool {
    ["config.toml", "config"]
        .iter()
        .any(|name| match (m.read)(&dir.join(name)) {
            Err(()) => true,
            Ok(None) => false,
            Ok(Some(text)) => text
                .lines()
                .any(|line| names(line) && !line.trim_start_matches([' ', '\t']).starts_with('#')),
        })
}

/// Whether any active Cargo config contains a non-comment line matching `names`.
#[must_use]
pub fn configs_name(m: &Machine<'_>, names: fn(&str) -> bool) -> bool {
    let Some(cwd) = m.cwd else {
        return true;
    };
    if cwd
        .ancestors()
        .any(|d| config_names(&d.join(".cargo"), m, names))
    {
        return true;
    }
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| m.home.map(|h| h.join(".cargo")));
    cargo_home.is_some_and(|h| config_names(&h, m, names))
}

/// Fallback `--config` value placing intermediates in `<target>/build.noindex` ([`config_value`]).
#[must_use]
pub fn target_build_dir(target: &Path, m: &Machine<'_>) -> Option<String> {
    if !target.is_absolute()
        || (m.env)("CARGO_BUILD_BUILD_DIR").is_some()
        || configs_name(m, |line| line.contains("build-dir"))
    {
        return None;
    }
    let dir = target.join(TARGET_BUILD_LEAF);
    let dir = dir.to_str()?;
    if dir
        .chars()
        .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(format!("build.build-dir='{dir}'"))
}

/// [`Machine::read`] on the real filesystem: a regular file read whole; anything else is no file.
///
/// # Errors
/// A file that exists and cannot be read, which [`decide`] counts as a choice.
#[expect(
    clippy::result_unit_err,
    reason = "Machine::read's shape: a file that cannot be read is a choice, whatever the error"
)]
pub fn read_config(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => std::fs::read(path)
            .map(|b| Some(String::from_utf8_lossy(&b).into_owned()))
            .map_err(|_| ()),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// `unix`, a fixture's absolute path, as this platform spells one (`C:\w\repo` on
    /// Windows, where `/w/repo` is not absolute).
    fn native(unix: &str) -> String {
        if cfg!(windows) && unix.starts_with('/') {
            format!("C:{}", unix.replace('/', "\\"))
        } else {
            unix.to_owned()
        }
    }

    /// A machine whose environment is `env` and whose config files are `files`, every
    /// absolute path spelled as this platform spells it ([`native`]).
    fn decide(
        target: &str,
        cwd: Option<&str>,
        env: &[(&str, &str)],
        files: &[(&str, &str)],
    ) -> Option<String> {
        let env: BTreeMap<String, OsString> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(native(v))))
            .collect();
        let files: BTreeMap<PathBuf, Result<String, ()>> = files
            .iter()
            .map(|(p, text)| {
                let text = if *text == "<unreadable>" {
                    Err(())
                } else {
                    Ok((*text).to_owned())
                };
                (PathBuf::from(native(p)), text)
            })
            .collect();
        let lookup = |k: &str| env.get(k).cloned();
        let read = |p: &Path| files.get(p).cloned().transpose();
        let writable = |_: &Path| Some(true);
        let cwd = cwd.map(native);
        let home = native("/h");
        let m = Machine {
            home: Some(Path::new(&home)),
            cwd: cwd.as_deref().map(Path::new),
            env: &lookup,
            read: &read,
            writable: &writable,
        };
        target_build_dir(Path::new(&native(target)), &m)
    }

    /// A build pinned to a target directory keeps its intermediates in
    /// `<target>/build.noindex`; a caller's own build directory, an unknown working
    /// directory and a path the literal cannot hold leave the layout alone.
    #[test]
    fn a_pinned_target_keeps_its_intermediates_out_of_spotlight() {
        let w = Some("/w/repo");
        assert_eq!(
            decide("/w/repo/target", w, &[], &[]),
            Some(format!(
                "build.build-dir='{}'",
                native("/w/repo/target/build.noindex")
            ))
        );
        assert!(
            decide(
                "/t",
                w,
                &[],
                &[("/w/repo/.cargo/config.toml", "[build]\ntarget-dir = 'x'\n")]
            )
            .is_some()
        );
        assert!(
            decide(
                "/t",
                w,
                &[],
                &[("/w/.cargo/config.toml", "# build-dir = 'x'\n")]
            )
            .is_some()
        );
        for (why, got) in [
            ("relative", decide("target", w, &[], &[])),
            ("no cwd", decide("/t", None, &[], &[])),
            (
                "env",
                decide("/t", w, &[("CARGO_BUILD_BUILD_DIR", "")], &[]),
            ),
            (
                "ancestor config",
                decide(
                    "/t",
                    w,
                    &[],
                    &[("/w/.cargo/config.toml", "[build]\nbuild-dir = 'x'\n")],
                ),
            ),
            (
                "home config",
                decide("/t", w, &[], &[("/h/.cargo/config", "build.build-dir='x'")]),
            ),
            (
                "CARGO_HOME config",
                decide(
                    "/t",
                    w,
                    &[("CARGO_HOME", "/ch")],
                    &[("/ch/config.toml", "  build-dir = 'x'")],
                ),
            ),
            (
                "unreadable config",
                decide("/t", w, &[], &[("/w/repo/.cargo/config", "<unreadable>")]),
            ),
            ("quote", decide("/t'q", w, &[], &[])),
            ("brace", decide("/t{workspace-root}", w, &[], &[])),
            ("control", decide("/t\nx", w, &[], &[])),
        ] {
            assert_eq!(got, None, "{why}");
        }
        assert!(
            decide(
                "/t",
                w,
                &[("CARGO_HOME", "/ch")],
                &[("/h/.cargo/config.toml", "build-dir = 'x'")]
            )
            .is_some()
        );
    }
}
