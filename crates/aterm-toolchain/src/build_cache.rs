// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build-cache placement for `targo` ([`decide`], [`NOINDEX`], [`target_build_dir`]).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The cache root, relative to `$HOME`. Its `*.noindex` name keeps Spotlight out, and the `.ubd`
/// folder above it (`aterm_tempfile::QUIET`) keeps its file events from fseventsd's clients and
/// history: a build writes thousands of files here (8,090 file events for one 38-package build,
/// none under `.ubd`; `docs/measured/fseventsd-ubd-2026-10-26.md`).
pub const CACHE_UNDER_HOME: &str = "Library/Caches/aterm/.ubd/cargo-build.noindex";

/// Where the cache root was until it moved under `.ubd` (2026-10-26), relative to `$HOME`. No
/// build of this version writes there. atpkg's trimmer takes what is left: each build directory
/// whole once no build holds its locks and nothing wrote it for an hour, then the emptied root
/// (`atpkg::build_cache::MOVED_AFTER`).
pub const MOVED_FROM_UNDER_HOME: &str = "Library/Caches/aterm/cargo-build.noindex";

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
    "clippy", "tippy", "fix", "clean", "package",
];

/// `$HOME/Library/Caches/aterm/.ubd/cargo-build.noindex` ([`CACHE_UNDER_HOME`]).
#[must_use]
pub fn cache_root(home: &Path) -> PathBuf {
    home.join(CACHE_UNDER_HOME)
}

/// `$HOME/Library/Caches/aterm/cargo-build.noindex`, the root no build writes any more
/// ([`MOVED_FROM_UNDER_HOME`]).
#[must_use]
pub fn moved_from(home: &Path) -> PathBuf {
    home.join(MOVED_FROM_UNDER_HOME)
}

/// The line of an atpkg `targo` shim that names its cache root under `$HOME`.
const SHIM_ROOT_LINE: &str = "__atb_c=${HOME-}/";

/// The cache root an atpkg `targo` shim whose text is `shim` builds in, for `home`: the one its
/// `__atb_c=${HOME-}/…` line names where that is [`CACHE_UNDER_HOME`] or the root before the
/// move ([`MOVED_FROM_UNDER_HOME`]); `None` for any other text (no shim, or one naming neither).
#[must_use]
pub fn root_named_by_shim(home: &Path, shim: &str) -> Option<PathBuf> {
    let named = shim
        .lines()
        .find_map(|line| line.trim().strip_prefix(SHIM_ROOT_LINE))?;
    [CACHE_UNDER_HOME, MOVED_FROM_UNDER_HOME]
        .into_iter()
        .find(|under| *under == named)
        .map(|under| home.join(under))
}

/// The cache root plain `targo` builds in now: the one the first `targo` on `path` names when it
/// is an atpkg shim ([`root_named_by_shim`]), else [`cache_root`]. A shim laid before the cache
/// moved under `.ubd` still names [`moved_from`] until an update of aterm lays the new one; a
/// build of this version that shares plain `targo`'s cache (verify's main lane, a lab's nested
/// build) follows it there, so the two never keep a copy each of one checkout's build.
#[must_use]
pub fn shim_root(home: &Path, path: &std::ffi::OsStr) -> PathBuf {
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("targo"))
        .find(|shim| shim.is_file())
        .and_then(|shim| {
            use std::io::Read as _;
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            let file = options.open(shim).ok()?;
            let meta = file.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let cap = usize::try_from(meta.len().min(64 << 10).saturating_add(1)).unwrap_or(0);
            let mut head = Vec::with_capacity(cap);
            file.take(64 << 10).read_to_end(&mut head).ok()?;
            root_named_by_shim(home, std::str::from_utf8(&head).ok()?)
        })
        .unwrap_or_else(|| cache_root(home))
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
    /// The cache root, where the caller follows plain `targo`'s ([`shim_root`]); `None` is
    /// [`cache_root`] of [`Self::home`], the one this version's shim names.
    pub root: Option<&'a Path>,
    /// The working directory, as the kernel names it (no symlinks).
    pub cwd: Option<&'a Path>,
    /// One environment variable.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// A config file's text.
    pub read: &'a dyn Fn(&Path) -> Result<Option<String>, ()>,
    /// Whether this process may write the directory (`access(2)`).
    pub writable: &'a dyn Fn(&Path) -> Option<bool>,
}

/// Environment variable carrying automatic placement across external subcommand dispatch
/// ([`loses_config`]), which may drop the outer driver's global `--config` words, and to
/// `targo trust`.
pub const BUILD_DIR_ENV: &str = "CARGO_BUILD_BUILD_DIR";

/// Whether `args` run an external frontend, which loses the outer driver's global `--config`
/// words, so that placement must also travel in [`BUILD_DIR_ENV`]. Anything else keeps them,
/// aliases included (cargo expands one in-process), and must not get the variable: cargo hands it
/// on to the build scripts it runs, and a nested cargo on the same workspace would inherit the
/// outer build's directory and wait forever on the lock the outer build holds. A frontend has no
/// such choice, so a build script that builds its own workspace under one still waits, and an
/// alias to a frontend places the outer call alone.
#[must_use]
pub fn loses_config(args: &[String], tool: Tool) -> bool {
    tool == Tool::Tippy
        || subcommand_at(args).is_some_and(|at| matches!(args[at].as_str(), "clippy" | "tippy"))
}

/// Where `args`' subcommand is, as cargo reads the line: the first word before `--` that is no
/// flag (nor `+toolchain`) and no flag's value (`--config`, `--manifest-path`, `--target-dir`,
/// `-C`, `-Z`, `--color` and `--explain` take one). `None` for a line that names none.
fn subcommand_at<T: AsRef<OsStr>>(args: &[T]) -> Option<usize> {
    let mut it = args.iter().enumerate();
    while let Some((at, a)) = it.next() {
        let a = a.as_ref().to_str().unwrap_or("");
        match a {
            "--" => return None,
            "--config" | "--manifest-path" | "--target-dir" | "-C" | "-Z" | "--color"
            | "--explain" => {
                it.next();
            }
            _ if a.starts_with(['-', '+']) => {}
            _ => return Some(at),
        }
    }
    None
}

/// Keep global and command-local configuration in the same Cargo parser. Trust 16387's
/// command-local `--config` replaces the global list instead of appending to it. Moving the
/// global words after the verb preserves their order and precedence, including inline forms.
/// Lines whose configs are all global, including the generated test graph, stay unchanged.
#[must_use]
pub fn configs_after_subcommand<T: AsRef<OsStr> + Clone>(args: &[T]) -> Vec<T> {
    let Some(at) = subcommand_at(args) else {
        return args.to_vec();
    };
    // `trust` has its own front end before Cargo's compile command, rather than this native
    // command boundary. Its explicit lane keeps ownership of its argument grammar.
    if args[at].as_ref() == "trust" {
        return args.to_vec();
    }
    if !args[at + 1..]
        .iter()
        .take_while(|a| a.as_ref() != "--")
        .any(|a| {
            a.as_ref()
                .to_str()
                .is_some_and(|s| s == "--config" || s.starts_with("--config="))
        })
    {
        return args.to_vec();
    }
    let mut before = Vec::new();
    let mut configs = Vec::new();
    let mut prefix = args[..at].iter();
    while let Some(word) = prefix.next() {
        let text = word.as_ref().to_str().unwrap_or("");
        if text == "--config" {
            configs.push(word.clone());
            if let Some(value) = prefix.next() {
                configs.push(value.clone());
            }
        } else if text.starts_with("--config=") {
            configs.push(word.clone());
        } else {
            before.push(word.clone());
        }
    }
    before.push(args[at].clone());
    before.extend(configs);
    before.extend_from_slice(&args[at + 1..]);
    before
}

/// The test graph's flag: Cargo's unstable `-Zfeature-unification`, which lets
/// [`TEST_GRAPH_CONFIG`] resolve a build's features over the whole workspace whatever packages it
/// selects. A `-p` test build then compiles exactly the units the `--workspace` one did (`verify
/// test -p aterm-agent` after `verify test --all` held 42 units the workspace graph had not,
/// before; 0 with the test graph, and the workspace graph itself unchanged). Its presence before
/// `--` says a line already chose its feature unification.
///
/// It goes right after the subcommand ([`with_test_graph`]), never before it: Trust 16387's
/// `targo` drops a `-Zfeature-unification` it reads before the subcommand and builds the narrow
/// graph (`aterm-agent`'s test unit graph: 170 units with it there, 173 after the verb, as with
/// `--workspace`).
///
/// Only Trust's `targo` takes it (an upstream `cargo` refuses `-Z`), and the spelling is that
/// toolchain's: the Trust lock (A14) is what will pin it. The test graph never reaches a release
/// slice: `aterm_release::buildplan::cargo_build_args` is the slices' only argv source and names
/// none of it, and neither its sealed config nor the workspace's `.cargo/config.toml` carries a
/// `[resolver]` table, so a shipped binary keeps the features its own package selects.
///
/// Only aterm's own checks take it: verify's main lane over this checkout, which tests every
/// selection as `--workspace` does, and the `verify` alias. A placement never does
/// ([`with_build_dir`], and atpkg's shim block): the words change which units a build compiles,
/// so a typed `targo --unverified test -p X` under them builds and tests X against features
/// its own package never selects (in Trust, 538 units became 757, 38 of them compiler crates,
/// 2026-11-07), and a build directory must never change what is built.
pub const TEST_GRAPH_FLAG: &str = "-Zfeature-unification";

/// The test graph's `--config`: `resolver.feature-unification = "workspace"`, read under
/// [`TEST_GRAPH_FLAG`].
///
/// It goes right before the subcommand ([`with_test_graph`]), with every other `--config` of the
/// line: Trust 16387's `targo` reads the `--config` words after the subcommand IN PLACE OF those
/// before it, not beside them, so one after the verb drops the build cache's `--config` and
/// verify's recording runner ([`with_build_dir`]; `targo --config CACHE --unverified test
/// -Zfeature-unification --config RESOLVER` built in `target/`, and ran each test binary itself,
/// not through a runner named before the verb). Before the verb with them, all of them hold, and
/// the graph is the workspace's (173 units, as above).
pub const TEST_GRAPH_CONFIG: [&str; 2] = ["--config", "resolver.feature-unification=\"workspace\""];

/// `args` with the test graph around its subcommand ([`subcommand_at`]): [`TEST_GRAPH_CONFIG`]
/// right before it, [`TEST_GRAPH_FLAG`] right after it. Unchanged when it names no subcommand,
/// or already names `-Zfeature-unification` before `--` (the caller's own choice, or the test
/// graph given once already). A caller's own `--config` belongs before the subcommand too.
#[must_use]
pub fn with_test_graph(args: &[String]) -> Vec<String> {
    let chosen = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == TEST_GRAPH_FLAG);
    let Some(verb) = subcommand_at(args).filter(|_| !chosen) else {
        return args.to_vec();
    };
    let mut out = Vec::with_capacity(args.len() + 3);
    out.extend_from_slice(&args[..verb]);
    out.extend(TEST_GRAPH_CONFIG.map(str::to_owned));
    out.push(args[verb].clone());
    out.push(TEST_GRAPH_FLAG.to_owned());
    out.extend_from_slice(&args[verb + 1..]);
    out
}

/// The directory in a literal config value produced by this placement policy. Use only for
/// an automatically selected value, not to reinterpret the caller's configuration.
#[must_use]
pub fn directory_from_config(value: &str) -> Option<&str> {
    value.strip_prefix("build.build-dir='")?.strip_suffix('\'')
}

/// The directory in the leading config words [`with_build_dir`] inserted, including past
/// Cargo's optional `+toolchain`. The caller must have selected automatic placement first.
#[must_use]
pub fn directory_from_placed_args(args: &[String]) -> Option<&str> {
    let at = usize::from(args.first().is_some_and(|arg| arg.starts_with('+')));
    let [flag, value, ..] = args.get(at..)? else {
        return None;
    };
    if flag != "--config" {
        return None;
    }
    directory_from_config(value)
}

/// What `cargo run` hands the process it runs about ITSELF (`targo --unverified verify` and `lab`
/// are `run -q -p …`), which build scripts track (ring's tracks `CARGO_MANIFEST_DIR` and
/// `OUT_DIR`, the latter set for a package with a build script, as `aterm-lab` is): a nested
/// build that inherits them rebuilds at every switch between it and a typed one (18 packages up to
/// aterm-gui; `lab mac dmg`'s bench build, 7.6 s, 2026-10-21; `lab mac nux`'s `ship dev-app`
/// rebuilt ring up to aterm after a typed one over the lab's `OUT_DIR`, 2026-10-25). A name ending
/// in `*` is a prefix.
pub const CARGO_RUN_ENV: [&str; 7] = [
    "CARGO_MANIFEST_DIR",
    "CARGO_MANIFEST_PATH",
    "CARGO_PKG_*",
    "CARGO_CRATE_NAME",
    "CARGO_BIN_NAME",
    "CARGO_PRIMARY_PACKAGE",
    "OUT_DIR",
];

/// Whether the environment variable `name` is one of [`CARGO_RUN_ENV`].
#[must_use]
pub fn said_by_cargo_run(name: &str) -> bool {
    CARGO_RUN_ENV.iter().any(|p| {
        p.strip_suffix('*')
            .map_or(name == *p, |prefix| name.starts_with(prefix))
    })
}

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

/// The explicit target and manifest a managed build already chose. This is observation only:
/// creating or owning a directory belongs to the package manager, never to this resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitCache {
    pub target: PathBuf,
    pub manifest: PathBuf,
}

/// Resolve an explicit target without moving a build or calling Cargo metadata. Help, clean,
/// unknown aliases, unverified-lane refusals and custom directory configs grant no creation.
#[must_use]
pub fn explicit_cache(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<ExplicitCache> {
    let home = m.home?;
    let cwd = m.cwd?;
    if !home.is_absolute() || !cwd.is_absolute() || !cwd.starts_with(home) {
        return None;
    }
    if (m.env)(BUILD_DIR_ENV).is_some() {
        return None;
    }
    if args.iter().take_while(|a| a.as_str() != "--").any(|a| {
        matches!(
            a.as_str(),
            "-h" | "--help" | "help" | "-V" | "--version" | "clean"
        )
    }) {
        return None;
    }
    let root = m.root.map_or_else(|| cache_root(home), Path::to_path_buf);
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    let line = Line::read(&args[at..], tool, home, &root)?;
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".cargo"), PathBuf::from);
    let mut alias_builds = false;
    for dir in cwd
        .ancestors()
        .map(|p| p.join(".cargo"))
        .chain([cargo_home])
    {
        let named = config_scan(&dir, line.alias.as_deref(), m);
        if named.build {
            return None;
        }
        alias_builds |= named.alias_builds;
    }
    if line.alias.is_some() && !alias_builds {
        return None;
    }
    let absolute = |path: PathBuf| {
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        (!path
            .components()
            .any(|c| c == std::path::Component::ParentDir))
        .then(|| path.components().collect::<PathBuf>())
    };
    let target = line
        .target_dir
        .map(PathBuf::from)
        .or_else(|| (m.env)("CARGO_TARGET_DIR").map(PathBuf::from))
        .or_else(|| (m.env)("CARGO_BUILD_TARGET_DIR").map(PathBuf::from))?;
    if target.as_os_str().is_empty() {
        return None;
    }
    let manifest = match line.manifest_path {
        Some(path) => absolute(PathBuf::from(path))?,
        None => cwd
            .ancestors()
            .take_while(|p| p.starts_with(home) && *p != home)
            .take(32)
            .map(|p| p.join("Cargo.toml"))
            .find(|p| (m.read)(p).is_ok_and(|text| text.is_some()))?,
    };
    Some(ExplicitCache {
        target: absolute(target)?,
        manifest,
    })
}

/// A configuration can select a target without an argv or environment target. When this
/// observer cannot resolve that choice, its caller must retain whole-cache ownership rather
/// than letting a managed writer use an older owner's cache without observing the new owner.
#[must_use]
pub fn unresolved_directory(args: &[String], m: &Machine<'_>) -> bool {
    if (m.env)(BUILD_DIR_ENV).is_some() {
        return true;
    }
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if word.starts_with("-C") {
            return true;
        }
        let config = if word == "--config" {
            words.next()
        } else {
            word.strip_prefix("--config=")
        };
        if config.is_some_and(|value| !value.contains('=') || names_a_dir(value)) {
            return true;
        }
    }
    let (Some(home), Some(cwd)) = (m.home, m.cwd) else {
        return true;
    };
    if !cwd.starts_with(home) {
        return true;
    }
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map_or_else(|| home.join(".cargo"), PathBuf::from);
    cwd.ancestors()
        .map(|path| path.join(".cargo"))
        .chain([cargo_home])
        .any(|dir| {
            let named = config_scan(&dir, None, m);
            named.target || named.build
        })
}

/// Decide the build-cache placement ([`BuildDir::Cache`], [`BuildDir::Target`]).
#[must_use]
pub fn decide(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<BuildDir> {
    let home = m
        .home
        .filter(|h| h.is_absolute() && !h.as_os_str().as_encoded_bytes().ends_with(b"/"))?;
    let cwd = m.cwd?;
    let root = m.root.map_or_else(|| cache_root(home), Path::to_path_buf);
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

/// `args` with the `--config` [`decide`] adds, or `None` when it adds none. That pair leads (a
/// shim's front strips it there), and every other word stays as typed: the pair says where the
/// build's intermediates go, never which units it builds, so the call resolves exactly the unit
/// graph it resolves unplaced (features included: no [`TEST_GRAPH_CONFIG`] or
/// [`TEST_GRAPH_FLAG`] is added, whatever the verb or profile).
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
    /// The last explicit manifest, before `--`, independent of the launcher's cwd.
    manifest_path: Option<String>,
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
        let mut manifest_path = None;
        let mut it = args.iter().map(String::as_str);
        while let Some(a) = it.next() {
            let ok = match a {
                "--" => break,
                "--unverified" => {
                    unverified = true;
                    true
                }
                "--config" => it.next().is_some_and(config_ok),
                "--manifest-path" => it.next().is_some_and(|v| {
                    manifest_path = Some(v.to_owned());
                    manifest_ok(v)
                }),
                "--target-dir" => it.next().map(|v| target_dir = Some(v.to_owned())).is_some(),
                "-Z" | "--color" | "--explain" => verb.is_some() || it.next().is_some(),
                _ => {
                    if let Some(v) = a.strip_prefix("--config=") {
                        config_ok(v)
                    } else if let Some(v) = a.strip_prefix("--manifest-path=") {
                        manifest_path = Some(v.to_owned());
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
            manifest_path,
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
                        let head = x.split('#').next().unwrap_or(x).trim_end();
                        in_alias = !head.starts_with("[[")
                            && head
                                .strip_prefix('[')
                                .and_then(|h| h.strip_suffix(']'))
                                .is_some_and(|h| h.trim() == "alias");
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
    use std::io::Read as _;
    const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(libc::ENXIO) => return Ok(None),
        Err(_) => return Err(()),
    };
    let meta = match file.metadata() {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Ok(None),
        Err(_) => return Err(()),
    };
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(());
    }
    let mut buf = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(MAX_CONFIG_BYTES.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| ())?;
    if (buf.len() as u64) > MAX_CONFIG_BYTES {
        return Err(());
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_an_external_frontend_loses_the_config_words() {
        let words = |line: &str| {
            line.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        for line in [
            "tippy",
            "clippy -p demo",
            "--unverified tippy",
            "+trust --config build.jobs=1 clippy",
            "--color always tippy",
        ] {
            assert!(loses_config(&words(line), Tool::Cargo), "{line}");
        }
        for line in [
            "",
            "--unverified",
            "--unverified build",
            "--config build.jobs=1 test -q",
            "+trust --target-dir tippy check",
            "-- tippy",
            "fix",
            "clean",
            "--unverified verify",
            "an-alias",
        ] {
            assert!(!loses_config(&words(line), Tool::Cargo), "{line}");
        }
        assert!(loses_config(&words("-p demo"), Tool::Tippy));
    }

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn global_and_command_configs_share_one_parser_without_moving_program_words() {
        let words = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            configs_after_subcommand(&words(
                "--config a=1 --color always --unverified test --config b=2"
            )),
            words("--color always --unverified test --config a=1 --config b=2")
        );
        assert_eq!(
            configs_after_subcommand(&words("--config=a=1 --unverified test --config b=2")),
            words("--unverified test --config=a=1 --config b=2")
        );
        for line in [
            "--config a=1 --unverified test -p x",
            "--config a=1 --unverified --config b=2 test -Zfeature-unification",
            "--config a=1",
            "--config a=1 --unverified run -- --config b=2",
            "--config a=1 trust check --config b=2",
        ] {
            assert_eq!(configs_after_subcommand(&words(line)), words(line));
        }
    }

    /// The test graph's `--config` goes right before the subcommand and its flag right after it,
    /// past every flag and flag value before it, once. NEGATIVE CONTROL: a line naming no
    /// subcommand before `--`, or one that chose its unification already, is left as it is.
    #[test]
    fn the_test_graph_wraps_the_subcommand() {
        let words = |line: &str| {
            line.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let c = TEST_GRAPH_CONFIG.join(" ");
        let f = TEST_GRAPH_FLAG;
        for (line, want) in [
            (
                "--unverified test -p x",
                format!("--unverified {c} test {f} -p x"),
            ),
            (
                "--config build.jobs=2 --unverified build -p x",
                format!("--config build.jobs=2 --unverified {c} build {f} -p x"),
            ),
            (
                "-Z unstable-options --color always run -q -- -x",
                format!("-Z unstable-options --color always {c} run {f} -q -- -x"),
            ),
            (
                "+trust --manifest-path m/Cargo.toml metadata --no-deps",
                format!("+trust --manifest-path m/Cargo.toml {c} metadata {f} --no-deps"),
            ),
            (
                "test -- -Zfeature-unification",
                format!("{c} test {f} -- -Zfeature-unification"),
            ),
        ] {
            assert_eq!(with_test_graph(&words(line)), words(&want), "{line}");
        }
        for line in [
            "",
            "--unverified",
            "--config build.jobs=2",
            "-- test",
            "--unverified test -Zfeature-unification -p x",
        ] {
            assert_eq!(with_test_graph(&words(line)), words(line), "{line}");
        }
    }

    /// The shared cache sits in a quiet folder fseventsd drops, still named `*.noindex`, and so
    /// does every build directory under it. NEGATIVE CONTROL: the root it moved from, the same
    /// name without the folder, is no quiet place.
    #[test]
    fn the_cache_root_is_quiet_and_noindex() {
        let home = Path::new("/Users//u");
        let root = cache_root(home);
        assert!(aterm_tempfile::is_quiet(&root), "{root:?}");
        assert!(in_noindex(&root), "{root:?}");
        assert_eq!(
            root.parent().and_then(Path::file_name),
            Some(std::ffi::OsStr::new(aterm_tempfile::QUIET))
        );
        let leaf = build_dir(&root, Path::new("/Users//u/Developer/aterm"));
        assert!(
            leaf.to_str().is_some_and(|p| p.contains("/.ubd/")),
            "{leaf:?}"
        );
        let old = moved_from(home);
        assert!(!aterm_tempfile::is_quiet(&old), "{old:?}");
        assert_eq!(old.file_name(), root.file_name());
        assert_ne!(old, root);
    }

    /// A shim's `__atb_c` line names the root it builds in: this version's quiet one, or the
    /// one before the move. NEGATIVE CONTROL: any other root, no such line, and no shim on the
    /// path leave the quiet root.
    #[test]
    fn the_root_plain_targo_builds_in_is_the_one_its_shim_names() {
        let home = Path::new("/Users//u");
        let shim = |under: &str| {
            format!("#!/bin/sh\n# atpkg shim\n__atb_t=\n__atb_c=${{HOME-}}/{under}\n")
        };
        assert_eq!(
            root_named_by_shim(home, &shim(MOVED_FROM_UNDER_HOME)),
            Some(moved_from(home))
        );
        assert_eq!(
            root_named_by_shim(home, &shim(CACHE_UNDER_HOME)),
            Some(cache_root(home))
        );
        assert_eq!(root_named_by_shim(home, &shim("Library/elsewhere")), None);
        assert_eq!(
            root_named_by_shim(home, "#!/bin/sh\nexec targo \"$@\"\n"),
            None
        );
        assert_eq!(shim_root(home, std::ffi::OsStr::new("")), cache_root(home));
    }

    #[test]
    fn only_what_cargo_run_says_about_its_child_is_dropped() {
        for name in [
            "CARGO_MANIFEST_DIR",
            "CARGO_PKG_NAME",
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_BIN_NAME",
            "OUT_DIR",
        ] {
            assert!(said_by_cargo_run(name), "{name}");
        }
        // The caller's own layout and toolchain choices, and a near miss, stay.
        for name in [
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            BUILD_DIR_ENV,
            "CARGO_PKG",
            "XCARGO_PKG_NAME",
            "OUT_DIRS",
        ] {
            assert!(!said_by_cargo_run(name), "{name}");
        }
    }

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
            root: None,
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

        let dir = aterm_tempfile::Builder::new()
            .prefix("atc-read-config-")
            .tempdir()
            .expect("scratch dir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "target-dir = 'x'\n").unwrap();
        assert_eq!(read_config(&cfg), Ok(Some("target-dir = 'x'\n".to_owned())));
        std::fs::write(&cfg, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        assert_eq!(
            read_config(&cfg),
            Err(()),
            "oversized config must fail closed"
        );
        #[cfg(unix)]
        {
            let fifo = dir.path().join("fifo.toml");
            let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
            assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
            assert_eq!(
                read_config(&fifo),
                Ok(None),
                "a FIFO config must not hang or count as a regular file"
            );
        }
    }
}
