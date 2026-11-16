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

/// Final artifacts selected by aterm's check runners when the caller did not choose a target.
pub const DEFAULT_TARGET: &str = "target/.ubd/main.noindex";

/// Relative layout for producer-selected intermediates: quiet for file events and Spotlight.
#[must_use]
pub fn target_build_relative() -> PathBuf {
    Path::new(".ubd").join(TARGET_BUILD_LEAF)
}

/// Intermediates below a chosen target; final artifacts keep the target's original path.
#[must_use]
pub fn target_build_path(target: &Path) -> PathBuf {
    target.join(target_build_relative())
}

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
/// diagnostic can report that older layout. Writers select this version's quiet [`cache_root`]
/// rather than adopting an installed shim's path.
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
    /// Trust's `targo`, which requires a lane for build commands.
    Targo,
    /// `targo-tippy`, the lint: every argument is its own, and it takes no lane.
    Tippy,
    /// An upstream `cargo`.
    Cargo,
    /// Trust's Cargo-compatible name: Cargo syntax without a required lane, Targo environment twins.
    CargoCompat,
}

impl Tool {
    /// Whether the command accepts Cargo syntax, including rustup's leading `+toolchain`.
    #[must_use]
    pub const fn is_cargo(self) -> bool {
        matches!(self, Self::Cargo | Self::CargoCompat)
    }
}

/// What [`decide`] reads of the machine.
pub struct Machine<'a> {
    /// `$HOME`.
    pub home: Option<&'a Path>,
    /// An explicit cache root; `None` selects [`cache_root`] of [`Self::home`].
    pub root: Option<&'a Path>,
    /// The working directory, as the kernel names it (no symlinks).
    pub cwd: Option<&'a Path>,
    /// One environment variable.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// A config file's text.
    pub read: &'a dyn Fn(&Path) -> Result<Option<String>, ()>,
    /// Initial manifest text: only a missing path is absent; existing non-files refuse.
    pub read_manifest: &'a dyn Fn(&Path) -> Result<Option<String>, ()>,
    /// Whether this process may write the directory (`access(2)`).
    pub writable: &'a dyn Fn(&Path) -> Option<bool>,
}

/// Environment variable carrying automatic placement across external subcommand dispatch
/// ([`loses_config`]), which may drop the outer driver's global `--config` words, and to
/// `targo trust`.
pub const BUILD_DIR_ENV: &str = "CARGO_BUILD_BUILD_DIR";

/// [`BUILD_DIR_ENV`]'s `TARGO_` twin, which Targo reads where that is unset ([`cargo_var`]).
pub const BUILD_DIR_TWIN: &str = "TARGO_BUILD_BUILD_DIR";

/// The variable spelling `build.artifact-dir`, the folder a build creates and links its final
/// artifacts into (`commands/build.rs`'s `export_dir`), apart from any target.
pub const ARTIFACT_DIR_ENV: &str = "CARGO_BUILD_ARTIFACT_DIR";

/// The short options that take no value in Targo's commands that take `-m`: `-q`, `-v`, `-h`,
/// `-r` (release), `-n` (dry run), `-w` and `-b` (`update`'s workspace and breaking), `-l`
/// (`package`'s list) and `tree`'s `-a`, `-d` and `-V`. Every other letter takes a value (`-p`,
/// `-j`, `-F`, `-m`, `-Z`, the top-level `-C`; `tree`'s `-e`, `-i` and `-f`; `vendor`'s `-s`) or
/// is refused, so a cluster's run of flags ends there (`util/command_prelude.rs`, each command's
/// `cli`). A command with a value-less letter of its own beside these (`install`'s `-f`) is
/// read with it among its own words ([`VERB_FLAG_SHORTS`], [`shorts_of`]).
const FLAG_SHORTS: &[u8] = b"qvhrnwlbadV";

/// A single-dash word read as a cluster of short options ([`shorts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shorts<'a> {
    /// The leading letters that take no value ([`FLAG_SHORTS`]).
    pub flags: &'a str,
    /// The first letter that takes one, and the value the word itself carries: the rest of the
    /// word, one leading `=` dropped. `None` when nothing follows the letter: its value is the
    /// next word.
    pub valued: Option<(char, Option<&'a str>)>,
}

impl<'a> Shorts<'a> {
    /// The value of `letter` when it is the letter that takes one: `Some(None)` when that value
    /// is the next word.
    #[must_use]
    pub fn value_of(&self, letter: char) -> Option<Option<&'a str>> {
        self.valued
            .filter(|(valued, _)| *valued == letter)
            .map(|(_, value)| value)
    }
}

/// The short options that take no value in one builtin command's own words beside
/// [`FLAG_SHORTS`], where the same letter takes one in another: `install`'s `-f` (`--force`,
/// `commands/install.rs`), which is `tree`'s `--format`. Every other command's value-less
/// letters are [`FLAG_SHORTS`] (each command's `cli`, read at Trust 16387).
const VERB_FLAG_SHORTS: &[(&str, &[u8])] = &[("install", b"f")];

/// `word` read as clap reads a cluster of short options: a run of letters that take no value
/// ([`FLAG_SHORTS`]), then at most one letter that takes a value, which is the rest of the word
/// (one leading `=` dropped) or, when nothing is left, the next word. So `-qm X`, `-mX` and
/// `-m=X` each name the manifest `X`, while in `-pm` the `m` is `-p`'s value. Whether a value
/// left to the next word is that word is [`takes_next`]'s. `None` for a word that is no cluster:
/// a long option, `--`, `-` or a positional.
#[must_use]
pub fn shorts(word: &str) -> Option<Shorts<'_>> {
    shorts_of(word, None)
}

/// `word` read as [`shorts`] reads it, among the own words of the builtin command `verb` where
/// one is named: that command's value-less letters ([`VERB_FLAG_SHORTS`]) are flags too, so
/// `install -fZunstable-options` is `-f` then `-Z unstable-options`, as clap reads it.
#[must_use]
pub fn shorts_of<'a>(word: &'a str, verb: Option<&str>) -> Option<Shorts<'a>> {
    let letters = word
        .strip_prefix('-')
        .filter(|rest| !rest.is_empty() && !rest.starts_with('-'))?;
    let own: &[u8] = verb
        .and_then(|verb| {
            VERB_FLAG_SHORTS
                .iter()
                .find(|(name, _)| *name == verb)
                .map(|(_, letters)| *letters)
        })
        .unwrap_or_default();
    let run = letters
        .bytes()
        .take_while(|letter| FLAG_SHORTS.contains(letter) || own.contains(letter))
        .count();
    let (flags, rest) = letters.split_at(run);
    let valued = rest.chars().next().map(|letter| {
        let after = &rest[letter.len_utf8()..];
        let value = after
            .strip_prefix('=')
            .or_else(|| (!after.is_empty()).then_some(after));
        (letter, value)
    });
    Some(Shorts { flags, valued })
}

/// Whether the word `next` is the value of a cluster whose letter `letter` left its value to the
/// next word ([`Shorts::valued`] `(letter, None)`), as clap reads it: `-j`'s whatever it is (it
/// takes hyphen values, `-j -1`), and another letter's unless it starts with `-` and is no lone
/// `-`. There an option whose value is optional takes none (`package -p -m X` packages the
/// default members of `X`; tree's `-i` too), and one that needs a value fails the call. A
/// character that is no ASCII letter takes nothing: Targo names no option by a digit or another
/// character (a `-1` is `--jobs -1`'s value, [`long_takes_next`]), and no shell's pattern tells a
/// non-ASCII one apart in every locale. A cluster's `-m` where it is Targo's manifest takes the next word whatever it is
/// ([`short_manifest_scope`]), as the placement reads it.
#[must_use]
pub fn takes_next(letter: char, next: &str) -> bool {
    letter.is_ascii_alphabetic() && (letter == 'j' || next == "-" || !next.starts_with('-'))
}

/// The long options of the [`TRAILING_VERBS`] that take a value besides those every reader skips
/// wherever they stand (`--config`, `--manifest-path`, `--target-dir`, `--profile`), the global
/// `--color` (which takes any word before the verb) and `--jobs` (which takes any word, as `-j`
/// does): `run`'s, `rustc`'s and `rustdoc`'s in Targo's `src/bin/cargo/commands` and
/// `util/command_prelude.rs` (Trust f57dabe4ae). `--package`, `--bin`, `--example`, `--target`,
/// `--test` and `--bench` take one optionally, the others need one.
pub const VALUED_LONGS: &[&str] = &[
    "--bench",
    "--bin",
    "--crate-type",
    "--example",
    "--features",
    "--message-format",
    "--output-format",
    "--package",
    "--print",
    "--target",
    "--test",
];

/// Whether the word `next` is the value of the long option `option` after the verb, as clap reads
/// it: `--jobs`'s whatever it is (it takes hyphen values), and `--color`'s and a
/// [`VALUED_LONGS`] option's unless it starts with `-` and is no lone `-`, as [`takes_next`]
/// reads a short option's. There an optional value takes none and a needed one fails the call.
/// Any other option takes none here.
#[must_use]
pub fn long_takes_next(option: &str, next: &str) -> bool {
    match option {
        "--jobs" => true,
        "--color" => next == "-" || !next.starts_with('-'),
        _ => VALUED_LONGS.contains(&option) && (next == "-" || !next.starts_with('-')),
    }
}

/// Targo's builtin commands (`src/bin/cargo/commands/mod.rs`'s `builtin_exec`, Trust
/// f57dabe4ae), whose names no user alias takes, and the names Targo keeps for the tools it ships
/// beside itself (`main.rs`'s `protected_trust_subcommand_binary`).
pub const BUILTIN_COMMANDS: &[&str] = &[
    "add",
    "bench",
    "build",
    "check",
    "clean",
    "config",
    "doc",
    "fetch",
    "fix",
    "generate-lockfile",
    "git-checkout",
    "help",
    "info",
    "init",
    "install",
    "locate-project",
    "login",
    "logout",
    "metadata",
    "new",
    "owner",
    "package",
    "pkgid",
    "publish",
    "read-manifest",
    "remove",
    "report",
    "run",
    "rustc",
    "rustdoc",
    "search",
    "test",
    "tree",
    "uninstall",
    "update",
    "vendor",
    "verify-project",
    "version",
    "yank",
    "trust",
    "tippy",
    "clippy",
    "fmt",
    "miri",
];

/// The user alias Targo would expand a verb into (`main.rs`'s `aliased_command`), as
/// [`targo_reading`] asks of a name no builtin command has ([`BUILTIN_COMMANDS`]): a builtin
/// alias's ([`BUILTIN_ALIASES`]) or any other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserAlias {
    /// None takes the name: a builtin alias stays Targo's (its command), and any other name is
    /// an external command, whose words are its own.
    None,
    /// One does, with these words, which Targo puts before the words after the name and reads
    /// again as a command line of its own.
    Words(Vec<String>),
    /// One may, in words the reader cannot tell.
    Unknown,
}

/// The building verbs whose clap hands their first word that is no option, and every word after
/// it, to the program (`trailing_var_arg`): `run`'s binary, `rustc`'s and `rustdoc`'s tools.
pub const TRAILING_VERBS: &[&str] = &["run", "r", "rustc", "rustdoc"];

/// The builtin verbs that take `-m` (`--manifest-path`) besides the [`BUILD_VERBS`]: the other
/// commands that call `arg_manifest_path` in Targo's `src/bin/cargo/commands` (Trust f57dabe4ae),
/// and `rm`, `remove`'s builtin alias ([`BUILTIN_ALIASES`]).
pub const MANIFEST_VERBS: &[&str] = &[
    "add",
    "fetch",
    "generate-lockfile",
    "locate-project",
    "metadata",
    "pkgid",
    "publish",
    "read-manifest",
    "remove",
    "rm",
    "report",
    "tree",
    "update",
    "vendor",
    "verify-project",
];

/// Targo's builtin aliases (`src/bin/cargo/main.rs`'s `BUILTIN_ALIASES`, Trust f57dabe4ae): no
/// command is named one, so a user alias of the name takes it over (`aliased_command` prefers
/// one), where a builtin command's name keeps the command.
pub const BUILTIN_ALIASES: &[&str] = &["b", "c", "d", "r", "t", "rm"];

/// The verbs after which a cluster's `-m` is Targo's manifest ([`short_manifest_scope`]): every
/// builtin that takes `--manifest-path` ([`BUILD_VERBS`], [`MANIFEST_VERBS`]) and `trust`.
pub fn short_manifest_verbs() -> impl Iterator<Item = &'static str> {
    BUILD_VERBS
        .iter()
        .chain(MANIFEST_VERBS)
        .copied()
        .chain(["trust"])
}

/// Whether a short cluster's `-m` ([`shorts`]) at each word of `args` names Targo's manifest:
/// after a verb [`short_manifest_verbs`] names (the lint's from its first word) and before `--`,
/// and for a [`TRAILING_VERBS`] verb only up to its first later word that is no option (a lone
/// `-` is one, as clap reads it), which with every word after it is the program's
/// ([`own_words`]). No `-m` is read after a verb that takes none, whose letters need not mean a
/// build's (`owner -rmatt` removes `matt`), after a name no builtin command has, an external
/// command's or an alias's (`lab = "run -q -p aterm-lab --"` hands `lab uname -m`'s to the
/// program; the front reads an alias's line, [`targo_line`]), nor before the verb, where clap
/// takes none. A value is skipped as clap skips it: a short option's left to the next word
/// ([`takes_next`]: `-p x`, `-vZ x`, `-qC dir`, `-j -m`, but not `-p -m`), the long options'
/// [`decide`]'s reader skips, wherever they stand, and after the verb each long option's
/// [`long_takes_next`] names (`run --package demo -m x` builds `x`'s `demo`), so each scan reads
/// the same words.
#[must_use]
pub fn short_manifest_scope(args: &[String], tool: Tool) -> Vec<bool> {
    scope_walk(args, tool).0
}

/// How many of `args`' leading words Targo reads as its own: every word before the first `--`
/// and before a [`TRAILING_VERBS`] verb's program, whose first word is the verb's first later
/// word that is no option nor an option's value (a lone `-` is one), read as
/// [`short_manifest_scope`] reads them. Words past that are the program's or a test binary's,
/// which no reader of Targo's options reads.
#[must_use]
pub fn own_words(args: &[String], tool: Tool) -> usize {
    scope_walk(args, tool).1
}

/// [`short_manifest_scope`] and [`own_words`], from one walk of `args`.
fn scope_walk(args: &[String], tool: Tool) -> (Vec<bool>, usize) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum At {
        Verb,
        Reads,
        UntilProgram,
        Never,
    }
    let mut at = if tool == Tool::Tippy {
        At::Reads
    } else {
        At::Verb
    };
    let mut scope = vec![false; args.len()];
    let mut end = args.len();
    let mut words = args.iter().map(String::as_str).enumerate().peekable();
    while let Some((index, word)) = words.next() {
        let reads = matches!(at, At::Reads | At::UntilProgram);
        scope[index] = reads;
        match word {
            "--" => {
                end = index;
                break;
            }
            "--config" | "--manifest-path" | "--target-dir" | "--profile" => {
                let _ = words.next();
            }
            "--color" | "--explain" if at == At::Verb => {
                let _ = words.next();
            }
            "-" if at == At::UntilProgram => {
                end = index;
                break;
            }
            _ if word.starts_with("--") && at != At::Verb => {
                let _ = words.next_if(|(_, next)| long_takes_next(word, next));
            }
            _ if word.starts_with('-') => {
                if let Some((letter, None)) = shorts(word).and_then(|cluster| cluster.valued) {
                    let manifest = letter == 'm' && reads;
                    let _ = words.next_if(|(_, next)| manifest || takes_next(letter, next));
                }
            }
            _ => match at {
                At::Verb if TRAILING_VERBS.contains(&word) => at = At::UntilProgram,
                At::Verb if short_manifest_verbs().any(|verb| verb == word) => at = At::Reads,
                At::Verb => at = At::Never,
                At::UntilProgram => {
                    end = index;
                    break;
                }
                At::Reads | At::Never => {}
            },
        }
    }
    (scope, end)
}

/// How many aliases deep [`reading`] expands a line: Targo refuses one whose aliases name one
/// another (and `cargo-miri` execs Targo without end on one an alias hands back to it,
/// [`miri_line`]), and past it a line is one the reader cannot tell ([`Outcome::Untold`]).
const ALIAS_DEPTH: usize = 8;

/// The names Targo keeps for the tools it ships beside itself (`main.rs`'s
/// `protected_trust_subcommand_binary`, Trust f57dabe4ae): no user alias takes one from `targo`
/// (`user_alias_allowed_for_invocation`), while one may from Trust's compat `cargo` or an
/// upstream cargo ([`Tool::Cargo`]), whose builtin commands they are not.
const PROTECTED_COMMANDS: &[&str] = &["trust", "tippy", "clippy", "fmt", "miri"];

/// The long options whose value every reader that looks for the verb skips before it: clap
/// takes `--config`, `--color` and `--explain` there and refuses the others before the verb, so
/// the readers need only agree ([`find_verb`], [`directory_arg`], [`decide`]'s and the shim's).
pub const VERB_VALUED: &[&str] = &[
    "--config",
    "--manifest-path",
    "--target-dir",
    "--color",
    "--explain",
    "--profile",
];

/// Where Targo finds a line's verb ([`find_verb`]).
enum Verb {
    /// At this word.
    At(usize),
    /// At this word, after a `--` that ends the words before it.
    Delimited(usize),
    /// Nowhere.
    Missing,
    /// After a `--`, at a word that starts with `-`, which this reader does not follow.
    Unreadable,
}

/// Where `args`' verb is, as cargo reads the line: the first word that is no flag (nor
/// `+toolchain`) and no flag's value ([`VERB_VALUED`] take one, as does a short option whose
/// value is left to the next word where clap takes it, [`takes_next`]: `-C dir`, `-Z x`, `-vZ
/// x`), or the word after a `--` that comes first, which clap hands Targo as the verb with every
/// word after it its own (Targo's `weird_check`).
fn find_verb<T: AsRef<OsStr>>(args: &[T]) -> Verb {
    let text = |a: &T| a.as_ref().to_str().unwrap_or("").to_owned();
    let mut it = args.iter().enumerate().peekable();
    while let Some((at, a)) = it.next() {
        let a = text(a);
        match a.as_str() {
            "--" => {
                return match args.get(at + 1).map(text) {
                    None => Verb::Missing,
                    Some(next) if next.starts_with('-') => Verb::Unreadable,
                    Some(_) => Verb::Delimited(at + 1),
                };
            }
            _ if VERB_VALUED.contains(&a.as_str()) => {
                it.next();
            }
            _ if a.starts_with(['-', '+']) => {
                if let Some((letter, None)) = shorts(&a).and_then(|cluster| cluster.valued) {
                    let _ = it.next_if(|(_, next)| takes_next(letter, &text(next)));
                }
            }
            _ => return Verb::At(at),
        }
    }
    Verb::Missing
}

/// The command line Targo runs for `args` (the words after the `+toolchain` Cargo's words may
/// lead with) once it has expanded the alias that takes its verb ([`targo_reading`]).
#[must_use]
pub fn targo_line(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
    miri: &Miri<'_>,
) -> Option<Vec<String>> {
    targo_reading(args, tool, alias, miri).map(|reading| reading.line)
}

/// What [`targo_reading`] reads of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The command line Targo runs.
    pub line: Vec<String>,
    /// Whether Targo answers the call with help or its version and builds nothing.
    pub query: bool,
    /// What `cargo-miri` does in the call besides the line it hands back ([`MiriCall`]).
    pub miri: MiriCall,
}

/// What `cargo-miri` reads besides the words after `miri` ([`miri_line`]), each read only where
/// the call reaches it.
pub struct Miri<'a> {
    /// One environment variable: `MIRI_SYSROOT`, `MIRI_BUILD_TEST_DEPS`.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// The target directory `cargo metadata` names for the words after `miri`, run where Targo
    /// works ([`miri_metadata_target`]); `None` where the reader cannot tell it.
    pub metadata_target: &'a dyn Fn(&[String]) -> Option<PathBuf>,
}

/// What `cargo-miri` does in a call besides the line it hands back to Targo ([`miri_line`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MiriCall {
    /// Whether it may build its sysroot first (`setup`, which runs a Targo of its own on the
    /// standard library's sources, [`miri_setup`]).
    pub setup: bool,
    /// Whether the line it hands back builds into `cargo metadata`'s target, joined with `miri`,
    /// where this reader cannot tell that target: the line names none, and its readers read
    /// Targo's own target instead.
    pub target_unread: bool,
    /// The folder whose `miri` the line's `--target-dir` names, as `cargo-miri` read it
    /// (`get_target_dir`): the first one typed, or the metadata target.
    pub base: Option<String>,
}

/// Whether `word` is a help option, as clap answers it wherever it reads the word as an option:
/// `--help`, or a cluster whose letters before any value hold `-h` (`-h`, `-vh`, `-rh`): clap
/// reacts to each letter in turn and answers help at the `h`, or fails at a letter before it,
/// and neither builds.
fn help_word(word: &str) -> bool {
    word == "--help" || shorts(word).is_some_and(|cluster| cluster.flags.contains('h'))
}

/// Whether `word` is Targo's version option before the verb: `--version`, or a cluster whose
/// letters before any value hold `-V` (`-V`, `-qV`, `-VC dir`), which clap sets as a flag.
fn version_word(word: &str) -> bool {
    word == "--version" || shorts(word).is_some_and(|cluster| cluster.flags.contains('V'))
}

/// Whether `word` is one of Targo's other options before the verb that answer the call without a
/// build (`cli.rs`'s `main`): `--list`, or `--explain` in either spelling.
fn answer_word(word: &str) -> bool {
    word == "--list" || word == "--explain" || word.starts_with("--explain=")
}

/// Whether the `-Z` values among `words` (the final parse's, up to the end of Targo's own words,
/// so before any `--`) are `help` alone, which `main` answers with the `-Z` help before anything
/// else: it reads only the first value, so every one being `help` is what makes it the first
/// whatever order clap keeps a global option's values in. Every cluster here is an option: clap
/// takes no word that starts with `-` as another option's value (`--config -Zhelp` it refuses,
/// building nothing).
fn z_help(words: &[String]) -> bool {
    let mut values = Vec::new();
    let mut it = words.iter().map(String::as_str);
    while let Some(word) = it.next() {
        match shorts(word).and_then(|cluster| cluster.valued) {
            Some(('Z', Some(value))) => values.push(value),
            Some(('Z', None)) => values.extend(it.next()),
            _ => {}
        }
    }
    !values.is_empty() && values.iter().all(|value| *value == "help")
}

/// The words before the verb at `at` that are options, not an option's value, as [`find_verb`]
/// reads them: `--color -V build` gives `--color` a value clap refuses, and asks no version.
fn options_before(line: &[String], at: usize) -> impl Iterator<Item = &str> {
    let mut words = line[..at].iter().map(String::as_str).peekable();
    std::iter::from_fn(move || {
        let word = words.next()?;
        if VERB_VALUED.contains(&word) {
            let _ = words.next();
        } else if let Some((letter, None)) = shorts(word).and_then(|cluster| cluster.valued) {
            let _ = words.next_if(|next| takes_next(letter, next));
        }
        Some(word)
    })
}

/// The command [`BUILTIN_ALIASES`]' `name` names (`main.rs`'s `BUILTIN_ALIASES`).
fn builtin_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "b" => "build",
        "c" => "check",
        "d" => "doc",
        "r" => "run",
        "t" => "test",
        "rm" => "remove",
        _ => return None,
    })
}

/// Whether `verb` is a builtin command of `tool`'s, which no alias takes: every
/// [`BUILTIN_COMMANDS`] name, but for [`PROTECTED_COMMANDS`] under [`Tool::Cargo`].
#[must_use]
pub fn builtin_command(verb: &str, tool: Tool) -> bool {
    BUILTIN_COMMANDS.contains(&verb) && !(tool.is_cargo() && PROTECTED_COMMANDS.contains(&verb))
}

/// Whether the line Targo runs, `line`, may read an artifact directory (`build.artifact-dir`, in
/// a config, a `--config` value or [`ARTIFACT_DIR_ENV`]): only `build` does (`commands/build.rs`
/// sets `export_dir`, which no other command reads), so a line whose verb is another command
/// Targo runs itself ([`exec_builtin`], a builtin alias read as its command) reads none. A
/// protected tool, an external command, and a line whose verb cannot be told may run a `build`.
#[must_use]
pub fn reads_artifacts<T: AsRef<OsStr>>(line: &[T]) -> bool {
    let Some(verb) = subcommand_at(line).and_then(|at| line[at].as_ref().to_str()) else {
        return true;
    };
    let verb = builtin_alias(verb).unwrap_or(verb);
    verb == "build" || !exec_builtin(verb)
}

/// Whether `verb` is a builtin command Targo runs itself (`commands/mod.rs`'s `builtin_exec`):
/// every [`BUILTIN_COMMANDS`] name but the [`PROTECTED_COMMANDS`], which no alias takes under
/// Targo yet which it runs as external commands (`targo-tippy`, `targo-miri`), their words their
/// own ([`tool_answers`]).
fn exec_builtin(verb: &str) -> bool {
    BUILTIN_COMMANDS.contains(&verb) && !PROTECTED_COMMANDS.contains(&verb)
}

/// Whether the protected tool `verb` names answers `words`, the words after its name, without a
/// build, as each reads its own: the lint ([`tippy_answers`]); `trust` (`targo-trust`'s
/// `run_main`) with no word, or `--version`, `-V`, `help` or `--help` first; `fmt`
/// (`cargo-fmt`) with a help or version option first; and `miri` (`cargo-miri`'s `main` and
/// `phase_cargo_miri`) with a help or version option first, or `setup` or `clean` with `-h` or
/// `--help` before any `--` (a verb it hands back to Targo is that line's, [`miri_line`]). Each
/// tool answers more than this (`trust check --help`), which is left to the call's custody: the
/// reader under-reads a tool's answer, never a build.
fn tool_answers(verb: &str, words: &[String]) -> bool {
    let first = words.first().map(String::as_str);
    match verb {
        "tippy" | "clippy" => tippy_answers(words),
        "trust" => matches!(first, None | Some("--version" | "-V" | "help" | "--help")),
        "fmt" => matches!(first, Some("-h" | "--help" | "-V" | "--version")),
        "miri" => match first {
            Some("-h" | "--help" | "-V" | "--version") => true,
            Some("setup" | "clean") => miri_help(words),
            _ => false,
        },
        _ => false,
    }
}

/// Whether `cargo-miri` reads a help option among `words`, the words after `miri`: `-h` or
/// `--help` before any `--` (`has_arg_flag`).
fn miri_help(words: &[String]) -> bool {
    words
        .iter()
        .take_while(|word| word.as_str() != "--")
        .any(|word| matches!(word.as_str(), "-h" | "--help"))
}

/// The line `cargo-miri` runs Targo on for `words`, the words after `miri`, where their first is
/// a verb it hands back (`phase_cargo_miri`'s `Forward`: `run`, `r`, `test`, `t`, `nextest`, and
/// `build` where `MIRI_BUILD_TEST_DEPS` is set), with what it does besides ([`MiriCall`]):
/// `targo --unverified <verb>`, then, where it reads a help option ([`miri_help`]), every later
/// word as typed (`miri run demo -h` builds and runs `demo -h`); else `nextest`'s own verb, the
/// first `--target-dir` before `--` with `miri` joined to it (`get_target_dir`), and every later
/// word but each `--target-dir` and its value before `--` (`ArgSplitFlagValue`), so the
/// directory comes before the program's words. With no `--target-dir` there, the target is
/// `cargo metadata`'s (`miri.metadata_target`, [`miri_metadata_target`]), joined with `miri`
/// the same way, `nextest`'s too (a user alias Targo expands for it, `nextest = "test"`, builds
/// there). That target is asked only where Targo runs `targo-miri` at all (`consent`: the call
/// runs in the lane, [`in_lane`]). Where it is not asked, or the reader cannot tell
/// it ([`MiriCall::target_unread`]), the line names none, and every `--config` `cargo metadata`
/// is run with (those before `--`, `cargo_extra_flags`, each flag's values scanned apart, as
/// `ArgSplitFlagValue` scans them) past the line's own words ([`own_words`]: `run`'s
/// program's, which the build hands the program) joins them, at their end, where the readers
/// read it. The flags it adds besides (`--target`, a runner's `--config`) name no directory.
fn miri_line(words: &[String], consent: bool, miri: &Miri<'_>) -> Option<(Vec<String>, MiriCall)> {
    let verb = words.first().filter(|verb| match verb.as_str() {
        "run" | "r" | "test" | "t" | "nextest" => true,
        "build" => (miri.env)("MIRI_BUILD_TEST_DEPS").is_some(),
        _ => false,
    })?;
    let mut line = vec!["--unverified".to_owned(), verb.clone()];
    let mut rest = words[1..].iter();
    if miri_help(words) {
        line.extend(rest.cloned());
        return Some((line, MiriCall::default()));
    }
    if verb == "nextest" {
        line.extend(rest.next().cloned());
    }
    let mut target = None;
    let mut kept = Vec::new();
    while let Some(word) = rest.next() {
        if word == "--" {
            kept.push(word.clone());
            kept.extend(rest.by_ref().cloned());
        } else if word == "--target-dir" {
            let value = rest.next();
            target = target.or(value.map(String::as_str));
        } else if let Some(value) = word.strip_prefix("--target-dir=") {
            target = target.or(Some(value));
        } else {
            kept.push(word.clone());
        }
    }
    let mut call = MiriCall::default();
    let base = match target {
        Some(typed) => Some(typed.to_owned()),
        None if consent => {
            let told = (miri.metadata_target)(words)
                .and_then(|target| target.into_os_string().into_string().ok());
            call.target_unread = told.is_none();
            told
        }
        None => None,
    };
    let Some(base) = base else {
        // Every word after the verb is kept, so `line[at + 1]` is `words[at]`.
        line.extend(kept);
        let own = own_words(&line, Tool::Targo);
        let metadata: Vec<String> = metadata_configs(words)
            .filter(|&(at, _)| at + 1 >= own)
            .flat_map(|(_, value)| ["--config".to_owned(), value.to_owned()])
            .collect();
        line.splice(own..own, metadata);
        return Some((line, call));
    };
    line.push("--target-dir".to_owned());
    line.push(Path::new(&base).join("miri").to_string_lossy().into_owned());
    line.extend(kept);
    call.base = Some(base);
    Some((line, call))
}

/// The `--config` values `cargo-miri` hands `cargo metadata` from `words`, the words after
/// `miri`, each with the index of its flag: those before the first `--` no `--config` takes,
/// in either spelling (`get_arg_flag_values`); a last `--config` with no value ends the scan.
fn metadata_configs(words: &[String]) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    std::iter::from_fn(move || {
        while let Some(word) = words.get(at) {
            let flag = at;
            at += 1;
            match word.as_str() {
                "--" => return None,
                "--config" => {
                    let value = words.get(at)?;
                    at += 1;
                    return Some((flag, value.as_str()));
                }
                _ => {
                    if let Some(value) = word.strip_prefix("--config=") {
                        return Some((flag, value));
                    }
                }
            }
        }
        None
    })
}

/// Whether the lint answers `words` without a build (`tippy`'s `main`, which reads the words
/// before any `--`, whatever name runs it): a help option, a version one (`-V`, `--version`,
/// `-vV`, `-Vv`) or `--explain`.
fn tippy_answers(words: &[String]) -> bool {
    words
        .iter()
        .take_while(|word| word.as_str() != "--")
        .any(|word| {
            matches!(
                word.as_str(),
                "-h" | "--help" | "-V" | "--version" | "-vV" | "-Vv" | "--explain"
            )
        })
}

/// The `-C` words among `globals`, [`kept_globals`]' words, with their values: the mark a line
/// keeps of the directory Targo changed into.
fn directory_marks(globals: &[String]) -> Vec<String> {
    let mut marks = Vec::new();
    let mut it = globals.iter().peekable();
    while let Some(word) = it.next() {
        match word.as_str() {
            "--config" | "--color" => {
                let _ = it.next();
            }
            _ => match shorts(word).and_then(|cluster| cluster.valued) {
                Some(('C', value)) => {
                    marks.push(word.clone());
                    if value.is_none() {
                        marks.extend(it.next_if(|next| takes_next('C', next)).cloned());
                    }
                }
                Some((letter, None)) => {
                    let _ = it.next_if(|next| takes_next(letter, next));
                }
                _ => {}
            },
        }
    }
    marks
}

/// The words before an alias's name that Targo keeps once it expands the alias (`cli.rs`'s
/// `GlobalArgs`): `-v`, `-q`, `--color`, `--frozen`, `--locked`, `--offline`, `-Z` and
/// `--config`, with their values, and of a cluster its `-v` and `-q` letters and a `-Z` or `-C`
/// that ends it, with its value (`-qVC dir` keeps `-qC=dir`). A `-C` is kept too, as a mark:
/// Targo changed into its directory before it read the alias, and the readers read that
/// directory from the words typed ([`directory_arg`]). Every other word or letter (`-V`,
/// `--version`, `--list`, `--explain`, `--unverified`) is dropped, its value with it, as
/// [`find_verb`] reads the values.
fn kept_globals(words: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut it = words.iter().peekable();
    while let Some(word) = it.next() {
        let text = word.as_str();
        match text {
            "--verbose" | "--quiet" | "--frozen" | "--locked" | "--offline" => {
                kept.push(word.clone());
            }
            "--config" | "--color" => {
                kept.push(word.clone());
                kept.extend(it.next().cloned());
            }
            _ if VERB_VALUED.contains(&text) => {
                let _ = it.next();
            }
            _ if text.starts_with("--config=") || text.starts_with("--color=") => {
                kept.push(word.clone());
            }
            _ => {
                let Some(cluster) = shorts(text) else {
                    continue;
                };
                let (letter, value) = match cluster.valued {
                    Some((letter, value)) => (Some(letter), value),
                    None => (None, None),
                };
                let detached = match letter {
                    Some(letter) if value.is_none() => it.next_if(|next| takes_next(letter, next)),
                    _ => None,
                };
                let flags: String = cluster
                    .flags
                    .chars()
                    .filter(|flag| matches!(flag, 'v' | 'q'))
                    .collect();
                let valued = letter.filter(|letter| matches!(letter, 'Z' | 'C'));
                let whole = flags.len() == cluster.flags.len() && valued == letter;
                if whole {
                    kept.push(word.clone());
                    kept.extend(detached.cloned());
                    continue;
                }
                match (valued, value) {
                    (Some(letter), Some(value)) => kept.push(format!("-{flags}{letter}={value}")),
                    (Some(letter), None) => {
                        kept.push(format!("-{flags}{letter}"));
                        kept.extend(detached.cloned());
                    }
                    (None, _) if !flags.is_empty() => kept.push(format!("-{flags}")),
                    (None, _) => {}
                }
            }
        }
    }
    kept
}

/// `args` with every `-C` before the verb taken out, its value and the cluster that holds it
/// with it (`-qC dir` goes whole: its other letters are value-less flags such as `-q` and `-v`,
/// which no reader of a line's directories reads): for a reader that has already resolved the
/// directory the words typed name ([`directory_arg`]) and judges the line from there. An alias's
/// own `-C` Targo ignores: it changes directory only from the words typed.
#[must_use]
pub fn without_directory(args: &[String]) -> Vec<String> {
    let verb = match find_verb(args) {
        Verb::At(at) | Verb::Delimited(at) => at,
        Verb::Missing | Verb::Unreadable => args.len(),
    };
    let mut kept = Vec::new();
    let mut it = args[..verb].iter().peekable();
    while let Some(word) = it.next() {
        if VERB_VALUED.contains(&word.as_str()) {
            kept.push(word.clone());
            kept.extend(it.next().cloned());
            continue;
        }
        match shorts(word).and_then(|cluster| cluster.valued) {
            Some(('C', value)) => {
                if value.is_none() {
                    let _ = it.next_if(|next| takes_next('C', next));
                }
            }
            Some((letter, None)) => {
                kept.push(word.clone());
                kept.extend(it.next_if(|next| takes_next(letter, next)).cloned());
            }
            _ => kept.push(word.clone()),
        }
    }
    kept.extend_from_slice(&args[verb..]);
    kept
}

/// THE LINE TARGO RUNS for `args` (the words after the `+toolchain` Cargo's words may lead with),
/// once it has expanded the alias that takes its verb (`cli.rs`'s `expand_aliases`, Trust
/// f57dabe4ae), and whether it answers the call without a build ([`reading`]). `None` where Targo refuses the
/// call or [`reading`] cannot tell its line ([`Outcome::Refused`], [`Outcome::Untold`]).
#[must_use]
pub fn targo_reading(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
    miri: &Miri<'_>,
) -> Option<Reading> {
    match reading(args, tool, alias, miri) {
        Outcome::Read(reading) => Some(reading),
        Outcome::Refused | Outcome::Untold { .. } => None,
    }
}

/// What [`reading`] makes of a call.
enum Outcome {
    /// Targo runs this line.
    Read(Reading),
    /// Targo refuses the call: a word that starts with `-` after a `--` that comes first, or a
    /// word after the name of a builtin command it runs itself ([`exec_builtin`]) so handed,
    /// where no user alias of that name stands.
    Refused,
    /// An alias takes the verb at `at` of `line` in words the reader cannot tell
    /// ([`UserAlias::Unknown`], an alias with none, or one past [`ALIAS_DEPTH`]), or in words
    /// that with those after its name hold a `-C` clap refuses when it parses them again (a
    /// second one, one with no value or an empty one, [`directory_arg`]), where Targo builds
    /// nothing yet the readers keep the call wary; `globals` are the words Targo keeps before
    /// its name ([`kept_globals`]); `miri` what `cargo-miri` does in the call where the alias
    /// runs it, the call in the lane ([`in_lane`]): it may set up, as the alias may run it.
    Untold {
        line: Vec<String>,
        at: usize,
        globals: Vec<String>,
        miri: MiriCall,
    },
}

/// THE LINE TARGO RUNS for `args`, as [`targo_reading`] reads it. A verb no builtin command has
/// is a user alias's where `alias` says one takes it ([`UserAlias::Words`]), else a builtin
/// alias's ([`BUILTIN_ALIASES`]: `b` is `build`), else an external command's, whose words are
/// its own. An alias's line is the words before its name that Targo keeps ([`kept_globals`]:
/// `-V b` builds, as Targo drops the `-V`), the alias's words, then the words after its name,
/// read again the same way until a builtin command's name ([`builtin_command`]) or an external
/// one. Only the words typed keep their globals: a nested alias's line drops the words its outer
/// alias put before its name, as Targo discards the inner `GlobalArgs`. A `--` before the verb
/// is dropped: clap hands Targo the word after it as the verb, the words after that as its own
/// (`targo -- b -m x` builds `x`). The words read are those Targo runs: by Targo's name, the
/// hop's ([`configs_after_subcommand`]). Targo merges the `--config` values of the typed globals
/// an alias expanded past, then those of the final parse at the deepest level clap finds one: the
/// verb's own words where they hold any, else the words before it, so the line keeps no
/// `--config` of the final parse before a verb whose own words hold one. After the name of a builtin command it runs itself
/// ([`exec_builtin`]) so handed it refuses any word (`-- build --target-dir t`), unless a user
/// alias of that name stands, which it ignores (`cli.rs`'s `expand_aliases`), running the
/// command with none of them; a protected tool's name so handed is an external command's, the
/// words its own (`-- clippy --target-dir t` builds into `t`). Targo changes into a `-C`
/// directory only from the words typed
/// ([`directory_arg`]) and takes `--unverified` only from them ([`consents`]), so their readers
/// read those; an alias's own `-C` it parses again and ignores, unless clap refuses it there,
/// which leaves the line untold ([`Outcome::Untold`]).
///
/// The call is a query where Targo answers it without a build (`cli.rs`'s `main`): where clap
/// answers a help option ([`help_word`]) among the words it reads as options, before the verb in
/// any line it parses and after the final line's builtin verb among Targo's own words
/// ([`own_words`]: `xtask --help` builds where `xtask = "run -p xtask --"`, and `run serve
/// --help` builds `serve`); where that verb is `help`; where the final line has no verb; where a
/// version option ([`version_word`]), `--list` or `--explain` ([`answer_word`]) stands before
/// its verb, read among the options there, not their values ([`options_before`]); and where its
/// own `-Z` values are `help` alone ([`z_help`]), the typed globals' not among them, as `main`
/// reads the final parse's. After a protected tool's name ([`exec_builtin`]) clap reads no
/// option: there the tool's own reading answers ([`tool_answers`]), and the line `miri` hands
/// back to Targo is the line run, read again as an alias's is ([`miri_line`]: `miri run demo -h`
/// builds `demo` and runs `demo -h`), of the words typed before `miri` only a `-C`'s mark
/// ([`directory_marks`]), as Targo hands an external command its own words alone. What
/// `cargo-miri` does besides ([`MiriCall`]) is read from `miri` ([`miri_line`], [`miri_setup`]),
/// only where the call runs in the lane ([`in_lane`]), Targo refusing `miri` otherwise. The
/// lint ([`Tool::Tippy`]) has no verb: its line is its own, a query where it answers it
/// ([`tippy_answers`]).
fn reading(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
    miri: &Miri<'_>,
) -> Outcome {
    // Through the hop Targo runs the words with every `--config` before the verb moved behind it
    // where its own words hold one ([`configs_after_subcommand`]), and the line read is that one.
    let mut line = if tool == Tool::Targo {
        configs_after_subcommand(args)
    } else {
        args.to_vec()
    };
    let mut call = MiriCall::default();
    if tool == Tool::Tippy {
        let query = tippy_answers(&line);
        return Outcome::Read(Reading {
            line,
            query,
            miri: call,
        });
    }
    // The environment is read only where `miri` or an untold alias asks the lane.
    let consent = || in_lane(args, tool, miri.env);
    let mut help = false;
    let mut depth = 0;
    let mut typed: Option<Vec<String>> = None;
    loop {
        let at = match find_verb(&line) {
            Verb::At(at) => at,
            Verb::Delimited(at) => {
                if exec_builtin(&line[at]) && line.len() > at + 1 {
                    if alias(&line[at]) == UserAlias::None {
                        return Outcome::Refused;
                    }
                    line.truncate(at + 1);
                }
                line.remove(at - 1);
                at - 1
            }
            Verb::Missing => {
                return Outcome::Read(Reading {
                    line,
                    query: true,
                    miri: call,
                });
            }
            Verb::Unreadable => return Outcome::Refused,
        };
        help |= options_before(&line, at).any(help_word);
        let answered =
            help || options_before(&line, at).any(|word| version_word(word) || answer_word(word));
        // The final parse's words: past the typed globals a line expanded from an alias keeps.
        let parsed = typed.as_ref().map_or(0, Vec::len);
        let verb = line[at].as_str();
        if builtin_command(verb, tool) {
            if !exec_builtin(verb) {
                let query = answered || z_help(&line[parsed..at]);
                let miri_words = &line[at + 1..];
                call.setup |= !query && verb == "miri" && miri_setup(miri_words, consent(), miri);
                let handed = (!query && verb == "miri")
                    .then(|| miri_line(miri_words, consent(), miri))
                    .flatten();
                let Some((handed, read)) = handed else {
                    let query = query || tool_answers(verb, miri_words);
                    return Outcome::Read(Reading {
                        line,
                        query,
                        miri: call,
                    });
                };
                call.target_unread |= read.target_unread;
                call.base = read.base;
                // Targo hands an external command its own words alone, so the line `miri` runs
                // keeps of the typed globals only a `-C`'s mark, its directory the one Targo
                // changed into. A hand-back that comes round to `miri` again does so through a
                // user alias, which the depth bounds.
                let globals =
                    directory_marks(&typed.clone().unwrap_or_else(|| kept_globals(&line[..at])));
                let mut next = globals.clone();
                next.extend(handed);
                line = next;
                typed = Some(globals);
                continue;
            }
            let own = own_words(&line, tool).max(at + 1);
            let query = answered
                || verb == "help"
                || line[at + 1..own].iter().any(|word| help_word(word))
                || z_help(&line[parsed..own]);
            // Targo merges the `--config` values of the typed globals an alias expanded past
            // (`GlobalArgs`), then the one list of the final parse, which clap fills from the
            // deepest level that holds any (`fill_in_global_values`): the verb's own words where
            // they hold one, in place of the final parse's words before it (`cli.rs`'s
            // `configure_gctx`).
            if holds_config(&line[at + 1..own]) && holds_config(&line[parsed..at]) {
                line = without_configs_in(&line, parsed..at);
            }
            return Outcome::Read(Reading {
                line,
                query,
                miri: call,
            });
        }
        let globals = typed.clone().unwrap_or_else(|| kept_globals(&line[..at]));
        let words = match alias(verb) {
            UserAlias::Words(words) if !words.is_empty() && depth < ALIAS_DEPTH => words,
            UserAlias::None => match builtin_alias(verb) {
                Some(command) => vec![command.to_owned()],
                None => {
                    let query = answered || z_help(&line[parsed..at]);
                    return Outcome::Read(Reading {
                        line,
                        query,
                        miri: call,
                    });
                }
            },
            UserAlias::Words(_) | UserAlias::Unknown => {
                return Outcome::Untold {
                    line,
                    at,
                    globals,
                    miri: untold(call, consent()),
                };
            }
        };
        // Clap parses the alias's words with those after its name again, and refuses a `-C`
        // there it would refuse typed (a second one, one with no value): Targo builds nothing,
        // and the readers keep the call as wary as one whose alias is unknown.
        let mut parsed = words.clone();
        parsed.extend_from_slice(&line[at + 1..]);
        if matches!(directory_arg(&parsed), Some(None | Some(""))) {
            return Outcome::Untold {
                line,
                at,
                globals,
                miri: untold(call, consent()),
            };
        }
        let mut next = globals.clone();
        next.extend(words);
        next.extend_from_slice(&line[at + 1..]);
        line = next;
        typed = Some(globals);
        depth += 1;
    }
}

/// Whether `words` hold a `--config` option, in either spelling.
fn holds_config(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| word == "--config" || word.starts_with("--config="))
}

/// `line` without the `--config` options among its words in `span`, words before a verb, in
/// either spelling, a detached value with its option: Targo's globals, none of which takes a word
/// that starts with `-` as its value, so each such word is an option.
fn without_configs_in(line: &[String], span: std::ops::Range<usize>) -> Vec<String> {
    let mut kept = line[..span.start].to_vec();
    let mut words = line[span.clone()].iter();
    while let Some(word) = words.next() {
        if word == "--config" {
            let _ = words.next();
        } else if !word.starts_with("--config=") {
            kept.push(word.clone());
        }
    }
    kept.extend_from_slice(&line[span.end..]);
    kept
}

/// The lines an alias [`reading`] cannot tell may run, each judged as Targo's: the words Targo
/// keeps before its name, a building verb in its place (`build`, whose own words run to the
/// first `--`, past which its readers read nothing, and `install`, whose `--path` names the
/// package it builds), then each prefix of the words after its name, from none of them (all its
/// program's) to all. `None` where the call's line is told or Targo refuses it.
#[must_use]
pub fn untold_lines(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
    miri: &Miri<'_>,
) -> Option<Vec<Vec<String>>> {
    let Outcome::Untold {
        line, at, globals, ..
    } = reading(args, tool, alias, miri)
    else {
        return None;
    };
    let after = &line[at + 1..];
    let globals = &globals;
    Some(
        ["build", "install"]
            .into_iter()
            .flat_map(|verb| {
                (0..=after.len()).map(move |taken| {
                    let mut candidate = globals.clone();
                    candidate.push(verb.to_owned());
                    candidate.extend_from_slice(&after[..taken]);
                    candidate
                })
            })
            .collect(),
    )
}

/// What `cargo-miri` does in the call `args` besides the line it hands back ([`MiriCall`]), as
/// [`reading`] reads it: where the line is told, what its `miri` does; where an alias the reader
/// cannot tell takes the verb ([`Outcome::Untold`]), that and a setup wherever the call runs in
/// the lane ([`in_lane`]), since the alias may run `miri`; nothing where Targo refuses the call.
#[must_use]
pub fn miri_call(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
    miri: &Miri<'_>,
) -> MiriCall {
    match reading(args, tool, alias, miri) {
        Outcome::Read(reading) => reading.miri,
        Outcome::Untold { miri, .. } => miri,
        Outcome::Refused => MiriCall::default(),
    }
}

/// [`Outcome::Untold`]'s [`MiriCall`]: what the line read so far does (`call`), and a setup
/// where the call runs in the lane, since the alias the reader cannot tell may run `miri`,
/// which Targo runs only there ([`in_lane`]).
fn untold(call: MiriCall, consent: bool) -> MiriCall {
    MiriCall {
        setup: call.setup || consent,
        ..call
    }
}

/// Whether `cargo-miri` builds its sysroot for `words`, the words after `miri`, where Targo
/// runs it (`consent`: the call runs in the lane, [`in_lane`]): `setup` (`setup.rs`'s
/// `setup`, which `miri setup` never skips), or a verb it hands back ([`miri_line`]) where
/// `MIRI_SYSROOT` is unset (set, even empty, it skips the build). A help option
/// ([`miri_help`]) builds nothing.
fn miri_setup(words: &[String], consent: bool, miri: &Miri<'_>) -> bool {
    if !consent || miri_help(words) {
        return false;
    }
    let sysroot_unset = || (miri.env)("MIRI_SYSROOT").is_none();
    match words.first().map(String::as_str) {
        Some("setup") => true,
        Some("run" | "r" | "test" | "t" | "nextest") => sysroot_unset(),
        Some("build") => (miri.env)("MIRI_BUILD_TEST_DEPS").is_some() && sysroot_unset(),
        _ => false,
    }
}

/// THE TARGET `cargo metadata` NAMES for `words`, the words after `miri`, run where Targo works
/// (`m.cwd`), as `cargo-miri` runs it (`get_cargo_metadata`: `targo metadata --no-deps` with
/// every `--config` and the first `--manifest-path` before `--`, each flag scanned apart, `-m`
/// never), and as Targo's `metadata` names it (`ws.target_dir()`): `CARGO_TARGET_DIR` (or its
/// twin, [`cargo_var`]) against the working directory; else `build.target-dir`, from a
/// `--config`, then `CARGO_BUILD_TARGET_DIR` against the working directory, then the configs
/// Targo walks ([`read_configured_target`]); else the workspace root's `target`, the manifest
/// the `--manifest-path` (folded as written against the working directory; it must be a
/// `Cargo.toml` that stands) or the nearest `Targo.toml`, then `Cargo.toml`, from the working
/// directory up, the root found as `find_root` finds it ([`metadata_root`]).
///
/// `None` where the reader cannot tell it, or Targo's metadata fails, building nothing: an
/// empty target variable or config value; a `--config` value that names a target directory or
/// what this reader cannot read ([`cache_config_outputs`]), or starts with `-`, which clap takes
/// as no value; a config the walk cannot read or Targo would refuse; a later package home
/// whose config names a target ([`package_homes`]: one only Trust's main reads); a
/// `__CARGO_TEST_ROOT`, which stops Targo's walks where this reader does not follow them; a
/// `--manifest-path` Targo refuses (one that names no `Cargo.toml`, or nothing); no manifest;
/// a manifest this reader cannot read or Targo would refuse on the way to its root, or one
/// under a later package home, where the walk may stop after either; and a package the root it
/// finds does not hold as a member, or that this reader cannot tell it holds ([`member`]).
#[must_use]
pub fn miri_metadata_target(words: &[String], tool: Tool, m: &Machine<'_>) -> Option<PathBuf> {
    let cwd = m.cwd?;
    if (m.env)("__CARGO_TEST_ROOT").is_some() {
        return None;
    }
    let resolve = |value: OsString| {
        let path = PathBuf::from(value);
        (!path.as_os_str().is_empty()).then(|| cwd.join(path))
    };
    if let Some(value) = cargo_var("CARGO_TARGET_DIR", tool, m) {
        return resolve(value);
    }
    let mut seen = std::collections::BTreeSet::new();
    for (_, value) in metadata_configs(words) {
        if value.starts_with('-') {
            return None;
        }
        match cache_config_outputs(value, m, true, tool != Tool::Cargo, &mut seen) {
            Ok(outputs) if outputs.target.is_none() => {}
            _ => return None,
        }
    }
    if let Some(value) = cargo_var("CARGO_BUILD_TARGET_DIR", tool, m) {
        return resolve(value);
    }
    let homes = package_homes(tool, m);
    if let Some(target) =
        read_configured_target(cwd, homes.first().map(PathBuf::as_path), m, true).ok()?
    {
        return Some(target);
    }
    if homes.iter().skip(1).any(|home| config_dirs(home, m).target) {
        return None;
    }
    let mut flags = words
        .iter()
        .map(String::as_str)
        .take_while(|word| *word != "--");
    let mut manifest_arg = None;
    while let Some(word) = flags.next() {
        if word == "--manifest-path" {
            manifest_arg = flags.next();
            break;
        }
        if let Some(value) = word.strip_prefix("--manifest-path=") {
            manifest_arg = Some(value);
            break;
        }
    }
    let manifest = match manifest_arg {
        Some(path) => {
            let path = lexical(&cwd.join(path));
            if path.file_name() != Some(OsStr::new("Cargo.toml"))
                || !matches!(config_file(&path, m), Ok(Some(_)))
            {
                return None;
            }
            path
        }
        None => cwd
            .ancestors()
            .flat_map(|dir| ["Targo.toml", "Cargo.toml"].map(|name| dir.join(name)))
            .find_map(|path| match config_file(&path, m) {
                Ok(None) => None,
                Ok(Some(_)) => Some(Some(path)),
                Err(()) => Some(None),
            })??,
    };
    // The root walk stops after the package home, and a later one either Targo may take.
    if homes.iter().skip(1).any(|home| manifest.starts_with(home)) {
        return None;
    }
    Some(metadata_root(&manifest, homes.first().map(PathBuf::as_path), m)?.join("target"))
}

/// How a manifest places its package in a workspace (`to_workspace_config`).
enum WorkspaceConfig {
    /// A `[workspace]`: its `members` (none where unset) and `exclude`, against its folder.
    Root {
        members: Option<Vec<String>>,
        exclude: Vec<String>,
    },
    /// A package's `workspace` key, the root's folder against the manifest's, where it has one.
    Member(Option<String>),
}

impl WorkspaceConfig {
    /// The workspace config of the manifest at `path`, as Targo loads it: `None` where nothing
    /// stands there, `Some(None)` where Targo refuses it or this reader cannot read it (text it
    /// cannot read or parse as TOML 1.1; neither a package, a `project` nor a `[workspace]`;
    /// a `[workspace]` beside a package's `workspace`; a key of another type). A `[package]`
    /// beside a `[project]` is the package (`TomlManifest::package`), which Targo only warns of
    /// below the 2024 edition.
    fn read(path: &Path, m: &Machine<'_>) -> Option<Option<Self>> {
        let text = match config_file(path, m) {
            Ok(None) => return None,
            Ok(Some(text)) => text,
            Err(()) => return Some(None),
        };
        Some(Self::parse(&text))
    }

    fn parse(text: &str) -> Option<Self> {
        let manifest = config_toml(text)?;
        let strings = |value: &aterm_toml::Value| -> Option<Vec<String>> {
            value
                .as_array()?
                .iter()
                .map(|entry| entry.as_str().map(str::to_owned))
                .collect()
        };
        let package = manifest.get("package").or_else(|| manifest.get("project"));
        let pointer = match package.map(|package| package.get("workspace")) {
            Some(Some(pointer)) => Some(pointer.as_str()?.to_owned()),
            _ => None,
        };
        match (manifest.get("workspace"), pointer) {
            (Some(_), Some(_)) => None,
            (Some(workspace), None) => {
                let table = workspace.as_table()?;
                let members = match table.get("members") {
                    Some(members) => Some(strings(members)?),
                    None => None,
                };
                let exclude = match table.get("exclude") {
                    Some(exclude) => strings(exclude)?,
                    None => Vec::new(),
                };
                Some(Self::Root { members, exclude })
            }
            (None, pointer) => package.is_some().then_some(Self::Member(pointer)),
        }
    }

    /// The workspace root's folder this config names for the package whose manifest is `look`,
    /// the config's own manifest at `path` (`get_ws_root`): a root's folder where it does not
    /// exclude `look` ([`Self::Root`]'s `exclude`, a literal prefix, cancelled by a `members`
    /// one, `is_excluded`), a pointer's target folded as written (`read_root_pointer`).
    fn root(&self, path: &Path, look: &Path) -> Option<PathBuf> {
        let dir = path.parent()?;
        match self {
            Self::Root { members, exclude } => {
                let under = |entry: &String| look.starts_with(dir.join(entry));
                let excluded = exclude.iter().any(under)
                    && !members
                        .as_ref()
                        .is_some_and(|members| members.iter().any(under));
                (!excluded).then(|| dir.to_path_buf())
            }
            Self::Member(Some(pointer)) => lexical(&dir.join(pointer).join("Cargo.toml"))
                .parent()
                .map(Path::to_path_buf),
            Self::Member(None) => None,
        }
    }
}

/// The workspace root Targo finds for the manifest `manifest` (`find_root`): its own config's
/// root ([`WorkspaceConfig::root`]); else the first that each `Cargo.toml` from its folder's
/// parent up names, a pointer met first as a root is, the walk stopping before a folder that
/// ends in `target/package` and after the package home `home` (`find_root_iter`); else its own
/// folder. `None` where a manifest on the way is one Targo refuses or this reader cannot read,
/// and where a root found past its own config does not hold it as a member ([`member`]).
fn metadata_root(manifest: &Path, home: Option<&Path>, m: &Machine<'_>) -> Option<PathBuf> {
    let own = WorkspaceConfig::read(manifest, m)??;
    if let WorkspaceConfig::Root { .. } = own {
        return own.root(manifest, manifest);
    }
    if let Some(root) = own.root(manifest, manifest) {
        return member(&root, manifest, m).then_some(root);
    }
    let mut last: Option<&Path> = None;
    for dir in manifest.ancestors().skip(2) {
        if dir.ends_with("target/package") || last.is_some_and(|last| Some(last) == home) {
            break;
        }
        last = Some(dir);
        let path = dir.join("Cargo.toml");
        let Some(config) = WorkspaceConfig::read(&path, m) else {
            continue;
        };
        if let Some(root) = config?.root(&path, manifest) {
            return member(&root, manifest, m).then_some(root);
        }
    }
    manifest.parent().map(Path::to_path_buf)
}

/// How many manifests [`member`] reads following path dependencies before it stops telling.
const MEMBER_READS: usize = 256;

/// Whether this reader can tell that the manifest `manifest` is a member of the workspace whose
/// root is the folder `root` (`find_members`, which `validate`'s
/// `error_if_manifest_not_in_members` holds the package to; `false` where Targo's metadata
/// fails on it or this reader cannot tell). The root's manifest is its `Cargo.toml`, a
/// `[workspace]`. A member is a folder's `Cargo.toml` (`find_path_deps` joins that name, so a
/// `Targo.toml` is none) that the root's `exclude` does not take ([`WorkspaceConfig::root`]'s
/// reading): one a `members` entry names, literally or as a glob whose `*`, `?` and `**` match
/// as `glob` matches them a component at a time ([`glob_matches`]); or a path dependency (in
/// `dependencies`, `dev-dependencies`, `build-dependencies`, their `_` spellings and each
/// `target`'s, a `workspace = true` one through the root's `[workspace.dependencies]`) of the
/// root's package or of a member, followed from one to the next. The folders a glob names
/// ([`glob_folders`], listed on the live disk whatever reader `m` holds for manifests) are
/// members whose path dependencies are followed too (`find_members` hands each to
/// `find_path_deps`), save one `exclude` takes, which `find_path_deps` returns on before it
/// loads its manifest. A dependency outside the root's folder is not followed (Targo follows it
/// only where its own root is this one).
///
/// Members only add members, and where one Targo must load cannot be, it builds nothing: so a
/// manifest that cannot be read or parsed, and a glob whose folders cannot be listed, tell
/// nothing of the members they would name, and every other entry and dependency is still read.
/// The root's manifest is read first, then the members' in their entries' order, each package's
/// dependencies after those already queued, until [`MEMBER_READS`] manifests were read, where
/// the walk stops untold.
fn member(root: &Path, manifest: &Path, m: &Machine<'_>) -> bool {
    use aterm_toml::Value;
    let root_manifest = root.join("Cargo.toml");
    let read = |path: &Path| config_toml(&config_file(path, m).ok()??);
    let Some(root_toml) = read(&root_manifest) else {
        return false;
    };
    let Some(Value::Table(workspace)) = root_toml.get("workspace") else {
        return false;
    };
    let strings = |key: &str| -> Option<Vec<String>> {
        match workspace.get(key) {
            None => Some(Vec::new()),
            Some(value) => value
                .as_array()?
                .iter()
                .map(|entry| entry.as_str().map(str::to_owned))
                .collect(),
        }
    };
    let (Some(members), Some(exclude)) = (strings("members"), strings("exclude")) else {
        return false;
    };
    let excluded = |path: &Path| {
        exclude
            .iter()
            .any(|entry| path.starts_with(root.join(entry)))
            && !members
                .iter()
                .any(|entry| path.starts_with(root.join(entry)))
    };
    if manifest.file_name() != Some(OsStr::new("Cargo.toml")) || excluded(manifest) {
        return false;
    }
    let Some(dir) = manifest.parent() else {
        return false;
    };
    let mut queue = std::collections::VecDeque::from([root_manifest.clone()]);
    for entry in &members {
        let pattern = lexical(&root.join(entry));
        if entry.contains(['*', '?', '[']) {
            if glob_matches(&pattern, dir) {
                return true;
            }
            let folders = glob_folders(&pattern).unwrap_or_default();
            queue.extend(folders.into_iter().map(|folder| folder.join("Cargo.toml")));
        } else if pattern == dir {
            return true;
        } else {
            queue.push_back(pattern.join("Cargo.toml"));
        }
    }
    let inherited = workspace.get("dependencies");
    let mut seen: Vec<PathBuf> = Vec::new();
    while let Some(next) = queue.pop_front() {
        if seen.contains(&next) || excluded(&next) {
            continue;
        }
        if seen.len() == MEMBER_READS {
            return false;
        }
        seen.push(next.clone());
        let Some(toml) = read(&next) else {
            continue;
        };
        if toml
            .get("package")
            .or_else(|| toml.get("project"))
            .is_none()
        {
            continue;
        }
        let Some(from) = next.parent() else {
            continue;
        };
        for dep in path_dependencies(&toml, from, root, inherited) {
            if dep == manifest {
                return true;
            }
            if dep.starts_with(root) {
                queue.push_back(dep);
            }
        }
    }
    false
}

/// The manifests of the path dependencies `toml`, the manifest in the folder `from`, declares
/// ([`member`]'s tables), each `path` folded as written against `from`, a `workspace = true`
/// one's against `root` from `inherited` (the root's `[workspace.dependencies]`).
fn path_dependencies(
    toml: &aterm_toml::Value,
    from: &Path,
    root: &Path,
    inherited: Option<&aterm_toml::Value>,
) -> Vec<PathBuf> {
    const TABLES: &[&str] = &[
        "dependencies",
        "dev-dependencies",
        "dev_dependencies",
        "build-dependencies",
        "build_dependencies",
    ];
    let targets = toml
        .get("target")
        .and_then(aterm_toml::Value::as_table)
        .into_iter()
        .flat_map(|targets| targets.values());
    let mut found = Vec::new();
    for owner in std::iter::once(toml).chain(targets) {
        for table in TABLES.iter().filter_map(|name| owner.get(name)?.as_table()) {
            for (name, dep) in table {
                let path = match dep.get("path").and_then(aterm_toml::Value::as_str) {
                    Some(path) => Some(from.join(path)),
                    None if dep.get("workspace").and_then(aterm_toml::Value::as_bool)
                        == Some(true) =>
                    {
                        inherited
                            .and_then(|deps| deps.get(name)?.get("path")?.as_str())
                            .map(|path| root.join(path))
                    }
                    None => None,
                };
                found.extend(path.map(|path| lexical(&path.join("Cargo.toml"))));
            }
        }
    }
    found
}

/// The folders the `members` glob `pattern` (absolute, folded) names, as `glob` lists them
/// (`expand_member_path`, whose `members_paths` keeps the folders among them): a component at a
/// time, each folder listed through its links; a wildcard component names the entries whose
/// names match it as [`glob_matches`] matches one (a leading `.` included, `glob`'s default; a
/// name that is no UTF-8 never, which `glob` skips); `**` names every folder below the folders
/// before it, each folder itself too where a component follows (it matches no components
/// there, never at the end, where `glob` yields only what lies below its base). A folder that
/// does not stand names nothing. `None` where one on the way cannot be listed, past
/// [`MEMBER_READS`] listings, for a `[` class this reader does not read, and for a wildcard
/// component whose first character is a `.`, which `glob` also expands to the folder's `.`
/// and `..`, which no listing holds.
fn glob_folders(pattern: &Path) -> Option<Vec<PathBuf>> {
    fn listed(dir: &Path, reads: &mut usize) -> Option<Vec<(Option<String>, PathBuf)>> {
        *reads += 1;
        if *reads > MEMBER_READS {
            return None;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Some(Vec::new());
            }
            Err(_) => return None,
        };
        let mut found = Vec::new();
        for entry in entries {
            let path = entry.ok()?.path();
            if std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                let name = path.file_name()?.to_str().map(str::to_owned);
                found.push((name, path));
            }
        }
        Some(found)
    }
    fn below(dir: &Path, reads: &mut usize, into: &mut Vec<PathBuf>) -> Option<()> {
        for (_, child) in listed(dir, reads)? {
            into.push(child.clone());
            below(&child, reads, into)?;
        }
        Some(())
    }
    let components: Vec<_> = pattern.components().collect();
    let mut folders = vec![PathBuf::new()];
    let mut reads = 0;
    for (at, component) in components.iter().enumerate() {
        let part = component.as_os_str().to_str();
        let mut next = Vec::new();
        match part {
            Some(part) if part.contains('[') => return None,
            Some("**") => {
                let last = at + 1 == components.len();
                for folder in &folders {
                    if !last {
                        next.push(folder.clone());
                    }
                    below(folder, &mut reads, &mut next)?;
                }
            }
            Some(part) if part.contains(['*', '?']) => {
                if part.starts_with('.') {
                    return None;
                }
                for folder in &folders {
                    for (name, child) in listed(folder, &mut reads)? {
                        if name.is_some_and(|name| glob_matches(Path::new(part), Path::new(&name)))
                        {
                            next.push(child);
                        }
                    }
                }
            }
            _ => {
                next = folders
                    .iter()
                    .map(|folder| folder.join(component))
                    .collect();
            }
        }
        folders = next;
    }
    Some(
        folders
            .into_iter()
            .filter(|folder| std::fs::metadata(folder).is_ok_and(|meta| meta.is_dir()))
            .collect(),
    )
}

/// Whether the folder `path` matches `pattern` as `glob` expands a `members` entry: a component
/// at a time, `**` any run of components (at least one at the end, where `glob` yields only
/// what lies below its base), `*` any run of characters within one, `?` one character; a
/// component `**` does not take matches only a name that is UTF-8 (`glob` skips another); a
/// `[` class this reader does not read matches nothing.
fn glob_matches(pattern: &Path, path: &Path) -> bool {
    fn within(pattern: &[char], name: &[char]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => (0..=name.len()).any(|skip| within(rest, &name[skip..])),
            Some(('?', rest)) => !name.is_empty() && within(rest, &name[1..]),
            Some(('[', _)) => false,
            Some((c, rest)) => name.first() == Some(c) && within(rest, &name[1..]),
        }
    }
    fn walk(pattern: &[String], path: &[Option<String>]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((first, rest)) if first == "**" => {
                let least = usize::from(rest.is_empty());
                (least..=path.len()).any(|skip| walk(rest, &path[skip..]))
            }
            Some((first, rest)) => path.split_first().is_some_and(|(name, more)| {
                name.as_deref().is_some_and(|name| {
                    let first: Vec<char> = first.chars().collect();
                    let name: Vec<char> = name.chars().collect();
                    within(&first, &name)
                }) && walk(rest, more)
            }),
        }
    }
    let pattern: Vec<String> = pattern
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let path: Vec<Option<String>> = path
        .components()
        .map(|c| c.as_os_str().to_str().map(str::to_owned))
        .collect();
    walk(&pattern, &path)
}

/// The variable through which Targo hands the processes it starts (a build script, a test, a
/// program `run` runs) the address of its unverified lane's authority (`process_authority.rs`'s
/// `TARGO_NESTED_UNVERIFIED_BROKER_ENV`). A Targo that inherits one it authenticates runs in that
/// lane with no `--unverified` typed (`InheritedExplicitUnverified`), `miri` among its verbs.
pub const NESTED_UNVERIFIED_ENDPOINT: &str = "TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT";

/// Whether `args` consent to the unverified lane as Targo reads them: an `--unverified` before
/// `--`, whatever word it follows (`cli.rs`'s `original_argv_has_unverified_before_delimiter`),
/// in the words typed, never an alias's, which Targo refuses to take one from.
#[must_use]
pub fn consents(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--unverified")
}

/// Whether Targo runs the call `args` in the unverified lane: the words typed consent to it
/// ([`consents`]), or, for every tool but an upstream cargo, which reads no such variable, the
/// call inherits a lane's authority ([`NESTED_UNVERIFIED_ENDPOINT`], `main.rs`'s
/// `inherited_unverified`). Targo grants an inherited address only once it authenticates it,
/// which this reader cannot do: it reads any address it finds as one that authenticates, which
/// can only judge more calls. Trust's Cargo-compatible name reads the address too and runs
/// every call outside the lanes (`authorize_trust_execution`'s `NotApplicable`), so reading it
/// there likewise only judges more calls.
#[must_use]
pub fn in_lane(args: &[String], tool: Tool, env: &dyn Fn(&str) -> Option<OsString>) -> bool {
    consents(args) || (tool != Tool::Cargo && env(NESTED_UNVERIFIED_ENDPOINT).is_some())
}

/// Whether `tool` reads each `TARGO_<X>` variable as its `CARGO_<X>` twin where `CARGO_<X>` is
/// unset (set, even empty, `CARGO_<X>` wins): Trust's Targo does
/// (`context/environment.rs`'s `add_targo_config_compatibility_twins`), the lint through the
/// Targo it runs as well, including its Cargo-compatible name; an upstream cargo reads no
/// `TARGO_` variable.
#[must_use]
pub const fn reads_twins(tool: Tool) -> bool {
    !matches!(tool, Tool::Cargo)
}

/// The value `tool` reads for the cargo variable `name` (`CARGO_<X>`): its own, or, where it is
/// unset and `tool` [`reads_twins`], its `TARGO_<X>` twin's.
#[must_use]
pub fn cargo_var(name: &str, tool: Tool, m: &Machine<'_>) -> Option<OsString> {
    (m.env)(name).or_else(|| {
        let suffix = name.strip_prefix("CARGO_").filter(|_| reads_twins(tool))?;
        (m.env)(&format!("TARGO_{suffix}"))
    })
}

/// The package homes whose config `tool` may read after the working directory's: `CARGO_HOME`
/// (against the working directory), or `~/.cargo` where it is unset or empty, and, where `tool`
/// [`reads_twins`] and `CARGO_HOME` is unset, a non-empty `TARGO_HOME` too, each with its `.`
/// and `..` folded as written ([`lexical`]), as `homedir` folds Targo's (`normalize_path`), never
/// through a link. Targo 16387 (compiler 29ebb403de, the build `trust.lock` names) takes its
/// home from the process's `CARGO_HOME` alone; Trust's main since ecd62b8507 takes it from the
/// snapshot its native brand imports twins into, so from `TARGO_HOME` instead of `~/.cargo`
/// where `CARGO_HOME` is unset (`context/mod.rs`'s `homedir`). Nothing here knows which build
/// a call's hop runs (a tree's lock, or the store's `current` for an unlocked one, may move to
/// either), so a scan that must see every config one may read reads both, and a reader that
/// must take one answer takes none where they differ.
#[must_use]
pub fn package_homes(tool: Tool, m: &Machine<'_>) -> Vec<PathBuf> {
    let at = |value: OsString| {
        lexical(
            &m.cwd
                .map_or_else(|| PathBuf::from(&value), |cwd| cwd.join(&value)),
        )
    };
    let cargo_home = (m.env)("CARGO_HOME");
    let twin = (cargo_home.is_none() && reads_twins(tool))
        .then(|| (m.env)("TARGO_HOME"))
        .flatten();
    cargo_home
        .filter(|value| !value.is_empty())
        .map(at)
        .or_else(|| m.home.map(|home| lexical(&home.join(".cargo"))))
        .into_iter()
        .chain(twin.filter(|value| !value.is_empty()).map(at))
        .collect()
}

/// The folders whose config Targo reads from `cwd`: [`config_walk`]'s, then each of `homes` that
/// is none of those nor an earlier home, by its canonical path. Targo's `walk_tree` records each
/// folder it walks by its canonical path (`seen_dir`; the path as written where it cannot be
/// resolved) and reads the home only where neither its canonical path nor its spelling is among
/// them, so a home that is a walked folder, by name or through a link, is read once. Read twice,
/// a merged array (an `alias`'s words) would hold its words twice. Each build reads one home;
/// `homes` may name both builds' homes ([`package_homes`]), and a second that resolves to the
/// first is the same folder.
fn config_folders(
    cwd: &Path,
    m: &Machine<'_>,
    homes: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut dirs = config_walk(cwd, m);
    for home in homes {
        if !dirs
            .iter()
            .any(|dir| *dir == home || canonical(dir) == canonical(&home))
        {
            dirs.push(home);
        }
    }
    dirs
}

/// The `.cargo` folders whose config Targo walks up from `from` (`walk_tree` over
/// `paths::ancestors`), nearest first: every folder's up to the root, or, where
/// `__CARGO_TEST_ROOT` names one of them, up to and including that one. Targo reads that
/// variable in every call, with `env::var`, so a value that is no UTF-8 text stops nothing, and
/// compares it with each folder by `Path` equality, its components as written. The package
/// homes ([`package_homes`]) are read after these.
#[must_use]
pub fn config_walk(from: &Path, m: &Machine<'_>) -> Vec<PathBuf> {
    let stop = (m.env)("__CARGO_TEST_ROOT")
        .and_then(|value| value.into_string().ok())
        .map(PathBuf::from);
    let mut dirs = Vec::new();
    for dir in from.ancestors() {
        dirs.push(dir.join(".cargo"));
        if stop.as_deref() == Some(dir) {
            break;
        }
    }
    dirs
}

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

/// Where `args`' subcommand is, as [`find_verb`] reads the line, before any `--`. `None` for a
/// line that names none there.
#[must_use]
pub fn subcommand_at<T: AsRef<OsStr>>(args: &[T]) -> Option<usize> {
    match find_verb(args) {
        Verb::At(at) => Some(at),
        Verb::Delimited(_) | Verb::Missing | Verb::Unreadable => None,
    }
}

/// The directory a `-C` before the verb names (`-C dir`, `-Cdir`, `-qC dir`), which Targo changes
/// into before it expands an alias, reads a config or resolves a relative path (`cli.rs`):
/// `None` where none stands there, `Some(None)` for one whose value is missing, which Targo
/// refuses, or for a second one, which clap's Set action refuses (`-C a -C b`, `-C a -qC a`).
/// The words before the verb are read as [`subcommand_at`] reads them.
#[must_use]
pub fn directory_arg(args: &[String]) -> Option<Option<&str>> {
    let mut dir = None;
    let mut it = args.iter().map(String::as_str).peekable();
    while let Some(a) = it.next() {
        match a {
            "--" => break,
            _ if VERB_VALUED.contains(&a) => {
                let _ = it.next();
            }
            _ if a.starts_with(['-', '+']) => match shorts(a).and_then(|cluster| cluster.valued) {
                Some(('C', value)) => {
                    let value = value.or_else(|| it.next_if(|next| takes_next('C', next)));
                    dir = Some(if dir.is_some() { None } else { value });
                }
                Some((letter, None)) => {
                    let _ = it.next_if(|next| takes_next(letter, next));
                }
                _ => {}
            },
            _ => break,
        }
    }
    dir
}

/// Whether the program `tool` names changes into the directory a `-C` before the verb names
/// ([`directory_arg`]) rather than refuse the call for want of its gate (`cli.rs`'s `main`,
/// before it expands an alias): its channel allows nightly features ([`nightly_channel`] says
/// so) and the gate is kept ([`directory_gated`]). `args` are the words typed, the `+toolchain`
/// of a tool that takes Cargo's words ([`Tool::is_cargo`]) first. Where the call holds no `-C`,
/// this reads the same gate.
#[must_use]
pub fn directory_honoured(args: &[String], tool: Tool, m: &Machine<'_>) -> bool {
    nightly_channel(args, tool, m) == Some(true) && directory_gated(args, tool)
}

/// Whether the program `tool` names refuses a `-C` before the verb for want of its gate, so
/// builds nothing: the gate is not kept ([`directory_gated`]), or its channel is one that allows
/// no nightly feature ([`nightly_channel`] says so). An upstream cargo whose channel this cannot
/// tell is not refused here: its `main` checks the gate before it changes into the directory and
/// reads anything else, so such a call either refuses or runs from that directory, and what it
/// builds it builds there.
#[must_use]
pub fn directory_refused(args: &[String], tool: Tool, m: &Machine<'_>) -> bool {
    nightly_channel(args, tool, m) == Some(false) || !directory_gated(args, tool)
}

/// Whether the `-Z` values clap keeps from the words typed hold `unstable-options`, the gate a
/// `-C` needs. A global option's values given after a builtin verb ([`exec_builtin`]), among its
/// own words ([`own_words`]), replace those before it (clap's `propagate_globals`:
/// `-Zunstable-options -C d build -Zx` is refused, `-C d build -Zunstable-options` honoured);
/// after an alias's or an external command's name clap parses nothing at the gate.
fn directory_gated(args: &[String], tool: Tool) -> bool {
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|a| a.starts_with('+')));
    let typed = &args[at..];
    let verb = match find_verb(typed) {
        Verb::At(verb) => verb,
        Verb::Delimited(verb) => verb - 1,
        Verb::Missing | Verb::Unreadable => typed.len(),
    };
    let own = typed
        .get(verb)
        .filter(|name| exec_builtin(name))
        .map(|name| {
            z_values(
                &typed[verb + 1..own_words(typed, tool).max(verb + 1)],
                Some(name),
            )
        })
        .unwrap_or_default();
    let kept = if own.is_empty() {
        z_values(&typed[..verb], None)
    } else {
        own
    };
    kept.contains(&"unstable-options")
}

/// Whether the channel of the program `tool` names allows nightly features (`features.rs`'s
/// `channel` and `nightly_features_allowed_on_channel`: `nightly`, `dev` or `trust`), `None`
/// where this reader cannot tell: the one `__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS` names
/// where it is set, `dev` under `RUSTC_BOOTSTRAP=1`, else the build's own. Targo's (and the
/// lint's, which runs it) is `trust`, and so is Trust's Cargo-compatible name's, the same
/// program. An upstream cargo's is told only by its `+toolchain`: one that names a nightly
/// toolchain (`+nightly`, `+nightly-2026-10-01`) allows them, a released one
/// ([`released_toolchain`]: `+stable`, `+beta-2026-10-01`, `+1.91.0`) allows none, and any
/// other, or none (the toolchain rustup picks), is untold: a custom name rustup links to any
/// cargo (`+1dev`, `+stable-mine`) tells nothing of the channel.
fn nightly_channel(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<bool> {
    let text = |name: &str| (m.env)(name).and_then(|value| value.into_string().ok());
    if let Some(channel) = text(CHANNEL_OVERRIDE) {
        return Some(matches!(channel.as_str(), "nightly" | "dev" | "trust"));
    }
    if text("RUSTC_BOOTSTRAP").as_deref() == Some("1") {
        return Some(true);
    }
    match tool {
        Tool::Targo | Tool::Tippy | Tool::CargoCompat => Some(true),
        Tool::Cargo => {
            let toolchain = args.first()?.strip_prefix('+')?;
            let named = |channel: &str| {
                toolchain == channel
                    || toolchain
                        .strip_prefix(channel)
                        .is_some_and(|rest| rest.starts_with('-'))
            };
            if named("nightly") {
                Some(true)
            } else if released_toolchain(toolchain) {
                Some(false)
            } else {
                None
            }
        }
    }
}

/// Whether `name` is a released toolchain rustup installs by that name and never links one to
/// (`rustup toolchain link` refuses it as "invalid custom toolchain name", rustup 1.29.0): the
/// channel `stable` or `beta`, or a release number (one digit, then a minor of up to three
/// digits and an optional patch of up to two, none with a leading zero, then an optional
/// `-beta` with an optional number of up to two digits), with an optional `-YYYY-MM-DD` date.
/// A name with a host triple after it (`stable-aarch64-apple-darwin`) is official too, but
/// rustup's triple reading is not repeated here, so such a name is not counted: reading less
/// as released only reads more calls as ones that may honour a `-C`.
fn released_toolchain(name: &str) -> bool {
    let number = |part: &str, most: usize| {
        !part.is_empty()
            && part.len() <= most
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let date = |part: &str| {
        let bytes = part.as_bytes();
        bytes.len() == 10
            && bytes.iter().enumerate().all(|(at, b)| match at {
                4 | 7 => *b == b'-',
                _ => b.is_ascii_digit(),
            })
    };
    let release = |part: &str| {
        let (number_part, beta) = match part.split_once("-beta") {
            Some((number_part, beta)) => (number_part, Some(beta)),
            None => (part, None),
        };
        let beta_kept = beta.is_none_or(|beta| {
            beta.is_empty() || beta.strip_prefix('.').is_some_and(|n| number(n, 2))
        });
        let mut parts = number_part.split('.');
        let major = parts.next().is_some_and(|p| number(p, 1));
        let minor = parts.next().is_some_and(|p| number(p, 3));
        let patch = parts.next().is_none_or(|p| number(p, 2));
        beta_kept && major && minor && patch && parts.next().is_none()
    };
    let channel = |part: &str| part == "stable" || part == "beta" || release(part);
    channel(name)
        || name
            .len()
            .checked_sub(11)
            .filter(|&cut| name.is_char_boundary(cut))
            .is_some_and(|cut| {
                name[cut..].starts_with('-') && date(&name[cut + 1..]) && channel(&name[..cut])
            })
}

/// The `-Z` values among `words`, read as clap reads options there: a cluster's `-Z` with its
/// value (`-Zx`, `-Z=x`, `-qZx`, `-Z x`), past every other option's value ([`VERB_VALUED`],
/// a long option after the verb as [`long_takes_next`] reads it, a short one as [`takes_next`]
/// does: `--jobs -Zx` gives `--jobs` the word). `verb` is the builtin command whose own words
/// these are, whose own value-less letters a cluster holds as flags ([`shorts_of`]), and `None`
/// for the words before the verb.
fn z_values<'a>(words: &'a [String], verb: Option<&str>) -> Vec<&'a str> {
    let mut values = Vec::new();
    let mut it = words.iter().map(String::as_str).peekable();
    while let Some(word) = it.next() {
        if VERB_VALUED.contains(&word) {
            let _ = it.next();
            continue;
        }
        if word.starts_with("--") {
            let _ = it.next_if(|next| long_takes_next(word, next));
            continue;
        }
        match shorts_of(word, verb).and_then(|cluster| cluster.valued) {
            Some(('Z', Some(value))) => values.push(value),
            Some(('Z', None)) => values.extend(it.next_if(|next| takes_next('Z', next))),
            Some((letter, None)) => {
                let _ = it.next_if(|next| takes_next(letter, next));
            }
            _ => {}
        }
    }
    values
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
    /// `<target>/.ubd/build.noindex` under the target directory the command builds into.
    Target(String),
    /// Build directory passed to `targo trust` via [`BUILD_DIR_ENV`].
    TrustEnv(String),
}

/// A final target chosen by a completely resolved producing command. A workspace default
/// requires the registrar's canonical owner; an unresolved command produces no choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalTarget {
    Explicit(PathBuf),
    WorkspaceDefault,
}

/// The outputs and manifest readings a managed build selected. This is observation only:
/// creating or owning a directory belongs to the package manager, never to this resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedOutputs {
    pub target: FinalTarget,
    /// All supported initial manifest readings; custody requires owner agreement.
    pub manifests: Vec<PathBuf>,
    /// The folder the line's target lies only inside of, owned in the target's place where it
    /// can be, the target only where it cannot: a `miri` line's target's parent
    /// ([`MiriCall::base`]). The build writes nothing outside it but a build directory selected
    /// apart ([`Self::intermediates`]), which takes miri's intermediates in place of the target.
    pub within: Option<PathBuf>,
    /// A separately selected intermediate directory; both outputs require custody.
    pub intermediates: Option<PathBuf>,
}

/// Resolve selected outputs without moving a build or calling Cargo metadata, from the line
/// Targo runs ([`targo_reading`], `alias` saying what user alias takes a verb's name). A query,
/// `clean` (at the verb: `test clean` runs the tests named so), a line whose alias cannot be
/// told, an external command, a verb that builds nothing, unverified-lane refusals and
/// unresolved output configs grant no creation. Nor does a `-C` typed before the verb: it names
/// the directory `m`'s working directory must already be, which only the caller resolves (it
/// changes into it, as Targo does, and passes the words without it, [`without_directory`]). A
/// `-C` the line Targo runs holds before its verb came from an alias's words, or was typed after
/// the name of an alias whose words name no verb: Targo parses it and ignores it (`cli.rs` reads
/// `directory` once, from the words typed, before it expands an alias), so it is dropped here
/// too. One after the verb, which clap refuses, grants none. Nor does a line `miri` hands back whose
/// target this reader cannot tell ([`MiriCall::target_unread`]); one whose target is the one
/// `miri` put there names the folder it lies in as [`SelectedOutputs::within`].
#[must_use]
pub fn selected_outputs(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<SelectedOutputs> {
    let CacheLine {
        line,
        base,
        target,
        intermediates,
    } = cache_line(args, tool, m, alias)?;
    let cwd = m.cwd?;
    // The folder whose `miri` the line `miri` hands back builds into, where that line's target is
    // the one it put there: the build writes nothing outside it but a selected build directory.
    let within = base
        .filter(|base| {
            !base.is_empty() && line.target_dir.as_deref() == Path::new(base).join("miri").to_str()
        })
        .map(PathBuf::from);
    let target = if line.trust || tool == Tool::Cargo {
        target.or_else(|| configured_target(cwd, tool, m))
    } else {
        target
    };
    let target = match target {
        Some(path) if !path.as_os_str().is_empty() => FinalTarget::Explicit(absolute(path, cwd)?),
        // cache_line completed native configuration discovery. This None therefore means
        // no final override, not an unknown graph. A separately selected intermediate is
        // the only new choice: ordinary automatic placement retains its existing path.
        None if !line.trust && tool != Tool::Cargo && intermediates.is_some() => {
            FinalTarget::WorkspaceDefault
        }
        // An empty variable Targo refuses (`target_dir`), or a line selecting neither output.
        _ => return None,
    };
    Some(SelectedOutputs {
        target,
        manifests: cache_manifests(&line, tool, m)?,
        within: within.and_then(|within| absolute(within, cwd)),
        intermediates,
    })
}

/// The `target` Targo builds a line into where nothing names another ([`workspace_target`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceTarget {
    /// Every supported initial manifest reading of the package the line builds
    /// ([`cache_manifests`]).
    pub manifests: Vec<PathBuf>,
    /// The `target` of the workspace root Targo finds for those manifests, under which it
    /// expands a placed `{workspace-root}/target/…`.
    pub target: PathBuf,
}

/// Manifests for the exact producer-selected local fallback: `selected`, the placement this call
/// was handed, never a fresh [`decide`], which can name the cache once admission has made its
/// parents ([`still_selected`] holds the plan itself). They are read from the line
/// [`selected_outputs`] reads ([`cache_line`]), so a call it grants nothing grants no fallback
/// either. The registrar resolves their workspace root before preparing `target`; `None` where
/// the placement is no fallback, or where the line or a manifest cannot be told.
/// Caller-selected layouts grant no such choice.
#[must_use]
pub fn local_fallback_manifests(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    selected: &BuildDir,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<Vec<PathBuf>> {
    let expected = target_build_path(Path::new("{workspace-root}/target"));
    let expected = expected.to_str()?;
    let fallback = match selected {
        BuildDir::Target(value) => directory_from_config(value) == Some(expected),
        BuildDir::TrustEnv(value) => value == expected,
        BuildDir::Cache(_) => false,
    };
    if !fallback {
        return None;
    }
    cache_manifests(&cache_line(args, tool, m, alias)?.line, tool, m)
}

/// Whether [`decide`] still places this call in `selected`, the placement chosen before its
/// admission. A changed argument, environment or config shows; the cache's writability, the one
/// answer admission itself changes (it makes the cache's parents), is held to the answer that
/// chose `selected`: a placement other than the cache either never reads it (a target the call
/// names, the trust lane) or was chosen because the cache could not be written. [`decide`] reads
/// [`Machine::writable`] for that answer alone.
#[must_use]
pub fn still_selected(args: &[String], tool: Tool, m: &Machine<'_>, selected: &BuildDir) -> bool {
    let held = Machine {
        writable: &|_| Some(false),
        ..*m
    };
    let m = if matches!(selected, BuildDir::Cache(_)) {
        m
    } else {
        &held
    };
    decide(args, tool, m).as_ref() == Some(selected)
}

/// The manifest of the package the line [`selected_outputs`] reads builds ([`cache_line`]), and
/// the `target` of the workspace root Targo finds for it ([`workspace_root`]): the folder Targo
/// builds into where nothing names another (`Workspace::default_target_dir`), and the one a
/// local fallback's `{workspace-root}/target` expands under ([`local_fallback_manifests`]).
/// Every supported manifest reading must find the same root. `None` where the line, a manifest
/// or the root cannot be told.
#[must_use]
pub fn workspace_target(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<WorkspaceTarget> {
    let manifests = cache_manifests(&cache_line(args, tool, m, alias)?.line, tool, m)?;
    let mut roots = manifests
        .iter()
        .map(|manifest| workspace_root(manifest, tool, m));
    let root = roots.next()??;
    roots
        .all(|other| other.as_ref() == Some(&root))
        .then(|| WorkspaceTarget {
            target: root.join("target"),
            manifests,
        })
}

/// The workspace root Targo finds for the package whose manifest is `manifest` (absolute, named
/// as Targo names it), the root walk stopping after the package home ([`metadata_root`]): the
/// root whose `target` a local fallback writes ([`local_fallback_manifests`]), and the owner
/// its custody records. Each home a build may take is walked ([`package_homes`]: Targo 16387's
/// `~/.cargo` and Trust's main's `TARGO_HOME` where `CARGO_HOME` is unset), and a root they
/// find apart is none, as either build may run. `None` there, where a manifest on the way is
/// one Targo refuses or this reader cannot read, or where the root found does not hold the
/// package as a member.
#[must_use]
pub fn workspace_root(manifest: &Path, tool: Tool, m: &Machine<'_>) -> Option<PathBuf> {
    let homes = package_homes(tool, m);
    let mut roots = homes
        .iter()
        .map(|home| metadata_root(manifest, Some(home), m));
    let first = roots
        .next()
        .unwrap_or_else(|| metadata_root(manifest, None, m))?;
    roots
        .all(|root| root.as_ref() == Some(&first))
        .then_some(first)
}

/// The line [`selected_outputs`] reads, as Targo runs it, with the folder a `miri` line it hands
/// back builds under ([`MiriCall::base`]); `None` for every call [`selected_outputs`] documents as
/// granting no creation.
fn cache_line(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<CacheLine> {
    let home = m.home?;
    let cwd = m.cwd?;
    if !home.is_absolute() || !cwd.is_absolute() || !cwd.starts_with(home) {
        return None;
    }
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|a| a.starts_with('+')));
    if directory_arg(&args[at..]).is_some() {
        return None;
    }
    let metadata_target = |words: &[String]| miri_metadata_target(words, tool, m);
    let miri = Miri {
        env: m.env,
        metadata_target: &metadata_target,
    };
    let reading = targo_reading(&args[at..], tool, alias, &miri)?;
    if reading.query || reading.miri.target_unread {
        return None;
    }
    let base = reading.miri.base;
    // The lint's line is its own words, read as typed.
    let words = if tool == Tool::Tippy {
        reading.line
    } else {
        without_directory(&reading.line)
    };
    let root = m.root.map_or_else(|| cache_root(home), Path::to_path_buf);
    // Every `--config` value is [`cache_config_outputs`]'s to read, as Targo reads it: custody
    // parses the line's roles alone ([`Line::parse`]), where placement keeps its lexical config
    // rule ([`Line::read`]).
    let lane = in_lane(&args[at..], tool, m.env);
    let line = Line::parse(&words, tool, home, &root, lane)?;
    if line.alias.is_some() || line.verb_at.is_some_and(|verb| words[verb] == "clean") {
        return None;
    }
    // Its `--target-dir` is the one the target comes from: an empty one is the working directory
    // (`value_of_path` joins it to it), the project's own folder, where no build cache is made or
    // adopted.
    if line.target_dir.as_deref() == Some("") {
        return None;
    }
    // A folder Targo links final artifacts into, which no target holds, and a `-C` left among
    // Targo's own words, grant none ([`words_name_unowned`]).
    let artifacts = reads_artifacts(&words);
    if words_name_unowned(&words, tool, m) {
        return None;
    }
    // Bind the exact command/direct-environment target once. It outranks CLI config
    // target-dir, while CLI build-dir outranks its config-level environment.
    let target = line
        .target_dir
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| cargo_var("CARGO_TARGET_DIR", tool, m).map(PathBuf::from));
    // CLI config values merge in argument order. Keep the winning target's definition
    // base: inline values use cwd; a supplied file uses its parent's parent, as Targo does.
    // Every config must still be understood, even when a higher target choice overrides it.
    let mut configured = None;
    let mut intermediates = None;
    let mut seen_files = std::collections::BTreeSet::new();
    for value in &line.configs {
        let outputs = cache_config_outputs(
            value,
            m,
            target.is_none(),
            tool != Tool::Cargo,
            &mut seen_files,
        )
        .ok()?;
        // An artifact directory, where the line reads one, is a folder no target holds.
        if artifacts && outputs.artifact {
            return None;
        }
        if let Some(selected) = outputs.target {
            configured = Some(selected.base.join(selected.value));
        }
        if let Some(selected) = outputs.intermediates {
            intermediates = Some(selected.base.join(selected.value));
        }
    }
    // The proof frontend has its own command/config boundary before the inner Cargo
    // command. Its CLI-selected target needs that provenance, not this compile parser's.
    if line.trust && configured.is_some() {
        return None;
    }
    // CLI config outranks config-level environment, unlike direct CARGO_TARGET_DIR.
    let target = target
        .or(configured)
        .or_else(|| cargo_var("CARGO_BUILD_TARGET_DIR", tool, m).map(PathBuf::from));
    let intermediates =
        intermediates.or_else(|| cargo_var(BUILD_DIR_ENV, tool, m).map(PathBuf::from));
    let proof_or_upstream = line.trust || tool == Tool::Cargo;
    let (target, intermediates) = if proof_or_upstream {
        (target, intermediates)
    } else {
        resolve_discovered_outputs(cwd, tool, m, target, intermediates).ok()?
    };
    let intermediates = match intermediates {
        Some(path) => {
            // The proof frontend's separate output grammar, an unknown upstream
            // capability and Cargo's template expansion grant no writable root here.
            if proof_or_upstream
                || path.as_os_str().is_empty()
                || path.as_os_str().to_str()?.contains(['{', '}'])
            {
                return None;
            }
            Some(absolute(path, cwd)?)
        }
        None => None,
    };
    // The proof frontend and an upstream cargo keep their existing conservative discovery
    // boundary: their output grammar or include-version capability is not established by this
    // reader, so a config that names a build directory grants none. For every frontend, a config
    // the walk reads ([`config_walk`], [`package_homes`]) that names an artifact directory where
    // the line reads one, or that this reader cannot read, grants none ([`config_dirs`]).
    if config_folders(cwd, m, package_homes(tool, m))
        .iter()
        .any(|dir| {
            let named = config_dirs(dir, m);
            named.unknown || (proof_or_upstream && named.build) || (artifacts && named.artifact)
        })
    {
        return None;
    }
    Some(CacheLine {
        line,
        base,
        target,
        intermediates,
    })
}

/// What [`cache_line`] reads of a call: the line Targo runs, the folder a `miri` line it hands
/// back builds under ([`MiriCall::base`]), and the target and intermediate directories its words,
/// environment and configs select, each before [`selected_outputs`]'s own fallback.
struct CacheLine {
    line: Line,
    base: Option<String>,
    target: Option<PathBuf>,
    intermediates: Option<PathBuf>,
}

/// Whether Targo's own words ([`own_words`]: `run serve --artifact-dir out` hands it to `serve`)
/// before `--`, or the environment, name what no target holds or a directory the caller did not
/// resolve: a `-C` among them ([`directory_arg`]'s caller resolves the one typed before the verb
/// and passes the words without it), an artifact directory (`--artifact-dir` in either
/// spelling), or [`ARTIFACT_DIR_ENV`] where the line reads one ([`reads_artifacts`]: a folder Targo
/// links final artifacts into). A `--config` value is no word here.
fn words_name_unowned(args: &[String], tool: Tool, m: &Machine<'_>) -> bool {
    if reads_artifacts(args) && cargo_var(ARTIFACT_DIR_ENV, tool, m).is_some() {
        return true;
    }
    let mut words = args[..own_words(args, tool)].iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if shorts(word).is_some_and(|cluster| cluster.value_of('C').is_some())
            || word == "--artifact-dir"
            || word.starts_with("--artifact-dir=")
        {
            return true;
        }
        if word == "--config" {
            let _ = words.next();
        }
    }
    false
}

/// Merge discovered outputs below command, CLI and environment selections. Each supported
/// package home is an alternative driver behavior; only identical effective outputs agree.
fn resolve_discovered_outputs(
    cwd: &Path,
    tool: Tool,
    m: &Machine<'_>,
    target: Option<PathBuf>,
    intermediates: Option<PathBuf>,
) -> Result<(Option<PathBuf>, Option<PathBuf>), ()> {
    let homes = package_homes(tool, m);
    let homes = if homes.is_empty() {
        vec![None]
    } else {
        homes.iter().map(|p| Some(p.as_path())).collect()
    };
    let mut resolved = None;
    for home in homes {
        let outputs = discovered_config_outputs(cwd, home, m, target.is_some(), true, false)?;
        let selected = (
            target
                .clone()
                .or_else(|| outputs.target.map(|p| p.base.join(p.value))),
            intermediates
                .clone()
                .or_else(|| outputs.intermediates.map(|p| p.base.join(p.value))),
        );
        if resolved.as_ref().is_some_and(|prior| prior != &selected) {
            return Err(());
        }
        resolved = Some(selected);
    }
    resolved.ok_or(())
}

/// The outputs the configs Targo walks select ([`config_walk`], then `home`): Targo visits the
/// nearest configuration first and merges farther roots without overwriting selected fields. In
/// each folder the legacy `config` wins where one stands, else `config.toml`, read through its
/// links ([`config_file`]); each root's graph is read as Targo loads it ([`config_graph_outputs`],
/// its includes followed where `follow_includes`, else read as [`Includes::OwnTarget`] reads
/// them for an upstream cargo's target, which is then the only output read), with a fresh
/// include cycle set. A package home already visited through its canonical ancestor name is not
/// loaded twice. A target is selected (its definition base needed) only while neither
/// `target_known` nor a nearer file selected one; an outranked one is judged all the same.
///
/// Where `nearest_only`, a reader of one output (the target, or the intermediates where
/// `target_known`) parses every file up to the nearest that selects it, and stops there. Else,
/// for custody, every file is parsed while the target is open, as Targo loads it; once
/// `target_known` or a nearer file selected the target, a farther file that spells neither a
/// build directory nor an include nor an escape nor a multi-line string cannot change the
/// outputs, and is not parsed, and nor is one that spells no output, include or escape at all
/// ([`names_a_dir`]), keeping neutral configuration independent of syntax this reader does not
/// decode.
///
/// # Errors
/// A config this reader cannot read, an oversized one, and every refusal of
/// [`config_graph_outputs`].
fn discovered_config_outputs(
    cwd: &Path,
    home: Option<&Path>,
    m: &Machine<'_>,
    target_known: bool,
    follow_includes: bool,
    nearest_only: bool,
) -> Result<ConfigOutputs, ()> {
    let mut selected = ConfigOutputs::default();
    for dir in config_folders(cwd, m, home.map(Path::to_path_buf)) {
        let legacy = dir.join("config");
        let (file, text) = match config_file(&legacy, m)? {
            Some(text) => (legacy, text),
            None => {
                let file = dir.join("config.toml");
                let Some(text) = config_file(&file, m)? else {
                    continue;
                };
                (file, text)
            }
        };
        if text.len() > CONFIG_GRAPH_MAX_BYTES {
            return Err(());
        }
        let open_target = !target_known && selected.target.is_none();
        if !nearest_only && !open_target {
            // A literal with none of the output/include words or escapes cannot select an
            // output. Keep neutral configuration independent of unsupported TOML syntax.
            if !names_a_dir(&text) {
                continue;
            }
            // An already selected target makes lower target-only text irrelevant. As in
            // the conservative scan, only include/output words, key escapes or a multi-line
            // string (whose lines a scan cannot tell) can hide intermediate selection;
            // malformed unused target syntax grants no new path.
            let literal = config_text_dirs(&text);
            if !literal.build
                && !literal.include
                && !spells_by_escape(&text)
                && !holds_multiline_string(&text)
            {
                continue;
            }
        }
        let outputs = config_graph_outputs(
            text,
            Some(file),
            cwd,
            m,
            open_target,
            if follow_includes {
                Includes::Follow
            } else {
                Includes::OwnTarget
            },
            &mut std::collections::BTreeSet::new(),
        )?;
        if selected.target.is_none() {
            selected.target = outputs.target;
        }
        if selected.intermediates.is_none() {
            selected.intermediates = outputs.intermediates;
        }
        selected.artifact |= outputs.artifact;
        // A reader of one output stops at the nearest file that selects it.
        let found = if target_known {
            selected.intermediates.is_some()
        } else {
            selected.target.is_some()
        };
        if nearest_only && found {
            break;
        }
    }
    Ok(selected)
}

/// Read CLI config outputs with their definition's path bases, as Targo's `cli_args_as_table`
/// reads a `--config` value in `m.cwd`: a config file where Targo's `Path::exists` finds
/// something at that path, whatever it spells, read whole with its includes against its own
/// folder ([`config_graph_outputs`]; `file_seen` the cycle check the CLI files share), a file
/// the reader cannot read among them, or one it says is none where something stands
/// ([`stands`]), refused; else, the probe finding nothing however it fails (no such file, a name
/// longer than a file's may be), an inline dotted key, read with a cycle check of its own. Targo
/// refuses an inline text that is no one dotted key and its value ([`one_dotted_key`]: an empty
/// one, a comment, a table's header, two keys) or that sets a value to an inline table
/// ([`sets_no_inline_table`]), and text this reader cannot parse as TOML 1.1 stays unknown; each
/// is refused. Where `follow_includes` is false (an upstream cargo) a nonempty `include` is
/// refused ([`Includes::Refuse`]). This observer never changes compiler argv.
fn cache_config_outputs(
    value: &str,
    m: &Machine<'_>,
    select_target: bool,
    follow_includes: bool,
    file_seen: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<ConfigOutputs, ()> {
    let cwd = m.cwd.ok_or(())?;
    let includes = if follow_includes {
        Includes::Follow
    } else {
        Includes::Refuse
    };
    let file = cwd.join(value);
    let read = if value.is_empty() {
        Ok(None)
    } else {
        config_file(&file, m)
    };
    match read {
        Ok(Some(text)) => {
            config_graph_outputs(text, Some(file), cwd, m, select_target, includes, file_seen)
        }
        _ if !value.is_empty() && stands(&file) => Err(()),
        _ => {
            let config = config_toml(value).ok_or(())?;
            if !one_dotted_key(&config) || !sets_no_inline_table(value) {
                return Err(());
            }
            config_graph_outputs(
                value.to_owned(),
                None,
                cwd,
                m,
                select_target,
                includes,
                file_seen,
            )
        }
    }
}

/// Whether an inline `--config` value sets its key to no inline table, which Targo refuses
/// (`toml_dotted_keys`: the value after the dotted key may be no inline table): the text after
/// the first `=` outside the key's quotes opens with no `{`. A value of another kind may hold
/// tables (a list of include tables), which Targo takes.
fn sets_no_inline_table(value: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for (at, c) in value.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if c == '\\' => escaped = true,
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if matches!(c, '\'' | '"') => quote = Some(c),
            None if c == '=' => {
                return !value[at + 1..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with('{');
            }
            None => {}
        }
    }
    true
}

const CONFIG_GRAPH_MAX_READS: usize = 1024;
const CONFIG_GRAPH_MAX_BYTES: usize = 16 * 1024 * 1024;
const CONFIG_GRAPH_MAX_PENDING: usize = 2 * CONFIG_GRAPH_MAX_READS;

/// One graph's resource policy, shared by CLI output resolution and discovered-output scans.
/// Missing optional files consume attempts; text counts cumulatively before another layer runs.
#[derive(Default)]
struct ConfigGraphBudget {
    reads: usize,
    text_bytes: usize,
}

impl ConfigGraphBudget {
    fn attempt_read(&mut self) -> Result<(), ()> {
        if self.reads >= CONFIG_GRAPH_MAX_READS {
            return Err(());
        }
        self.reads += 1;
        Ok(())
    }

    fn retain_text(&mut self, text: &str) -> Result<(), ()> {
        self.text_bytes = self.text_bytes.checked_add(text.len()).ok_or(())?;
        if self.text_bytes > CONFIG_GRAPH_MAX_BYTES {
            return Err(());
        }
        Ok(())
    }
}

enum ConfigStep {
    Load(PathBuf, bool),
    Layer(String, Option<PathBuf>),
    Outputs(LayerOutputs),
}

/// The outputs a graph of configs selects, each with its definition base, and whether any of
/// them names an artifact directory.
#[derive(Default)]
struct ConfigOutputs {
    target: Option<ConfiguredPath>,
    intermediates: Option<ConfiguredPath>,
    artifact: bool,
}

/// One layer's output values, as written, each with its defining file (`None` for an inline
/// value), before the merge judges the winner.
struct LayerOutputs {
    target: Option<(Option<PathBuf>, aterm_toml::Value)>,
    intermediates: Option<(Option<PathBuf>, aterm_toml::Value)>,
    artifact: bool,
}

/// How [`config_graph_outputs`] reads a layer's nonempty `include`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Includes {
    /// Followed, as Targo always loads them.
    Follow,
    /// Not read, for an upstream cargo's target alone: it loads an `include` only from 1.93
    /// (before that under `-Zconfig-include` alone), and either way a layer's own target
    /// outranks every include it declares. So a layer with includes gives its own target, and
    /// one with none of its own is refused. No other output is read this way.
    OwnTarget,
    /// Refused: an upstream cargo carries no include-version capability proof, and an include it
    /// may load can select any output.
    Refuse,
}

/// Resolve output settings in Cargo's merge order without recursive traversal, as Targo loads a
/// config (`_load_file`, `load_includes`): a file's own value over a later include's, over an
/// earlier one's, each include read against the declaring file's folder (cwd for inline values,
/// [`include_file`]) as TOML 1.1 ([`config_toml`]), an optional one where nothing stands skipped
/// ([`stands`]). Each output keeps its definition base, the folder above the one its defining file
/// is in (`Definition::root`). Targo's merge (`merge_helper`) lets a later string, integer or
/// boolean replace an earlier one, so only the winning value is judged among those, and it must
/// be a non-empty string with no control character; a list or a table meets any other value with
/// an error, and a float or a date Targo cannot load, so one anywhere in the graph is refused.
/// A target is returned only where `select_target`, though it is judged either way: Targo's
/// `build_config` loads the whole `build` table, an outranked target with it. Only a returned
/// value's definition base is taken, as Targo's `Definition::root` is asked only where a path is
/// resolved. A nonempty `include` is read as `includes` says ([`Includes`]).
///
/// # Errors
/// A repeated path (Targo's cycle check, `file_seen` for CLI files, a fresh set for inline
/// values), unreadable input, an include [`config_includes`] refuses or that names nothing
/// though not optional, a `build` that is no table, a value Targo cannot merge or load, a
/// winning value of no non-empty string, a returned value's defining file with no grandparent,
/// an include [`Includes`] refuses, and a graph past the read, text or pending-work bounds.
fn config_graph_outputs(
    text: String,
    file: Option<PathBuf>,
    cwd: &Path,
    m: &Machine<'_>,
    select_target: bool,
    includes: Includes,
    file_seen: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<ConfigOutputs, ()> {
    // Bound adversarial acyclic graphs as well as cycles. Optional missing files consume
    // attempts, and aggregate text is checked before retaining another layer.
    let mut budget = ConfigGraphBudget::default();
    budget.retain_text(&text)?;
    if file.is_some() {
        budget.attempt_read()?;
    }
    let mut inline_seen = std::collections::BTreeSet::new();
    // CLI file arguments share Targo's seen set; inline graphs each get a fresh one.
    // Supplied root paths retain their spelling, while included paths are normalized.
    let seen = if let Some(file) = &file {
        if file_seen.len() >= CONFIG_GRAPH_MAX_READS || !file_seen.insert(file.clone()) {
            return Err(());
        }
        file_seen
    } else {
        &mut inline_seen
    };
    let mut pending = vec![ConfigStep::Layer(text, file)];
    let mut target = None;
    let mut intermediates = None;
    let mut artifact = false;
    while let Some(step) = pending.pop() {
        match step {
            ConfigStep::Outputs(outputs) => {
                if outputs.target.is_some() {
                    target = outputs.target;
                }
                if outputs.intermediates.is_some() {
                    intermediates = outputs.intermediates;
                }
                artifact |= outputs.artifact;
            }
            ConfigStep::Load(file, optional) => {
                if seen.contains(&file) {
                    return Err(());
                }
                budget.attempt_read()?;
                match config_file(&file, m) {
                    Ok(None) if optional && !stands(&file) => {}
                    Ok(Some(text))
                        if seen.len() < CONFIG_GRAPH_MAX_READS && seen.insert(file.clone()) =>
                    {
                        budget.retain_text(&text)?;
                        pending.push(ConfigStep::Layer(text, Some(file)));
                    }
                    _ => return Err(()),
                }
            }
            ConfigStep::Layer(text, file) => {
                let config = config_toml(&text).ok_or(())?;
                let layer_target = config_layer_value(&config, "target-dir")?;
                let layer_intermediates = config_layer_value(&config, "build-dir")?;
                let layer_artifact = config_layer_value(&config, "artifact-dir")?.is_some();
                let own_target = layer_target.is_some();
                if own_target || layer_intermediates.is_some() || layer_artifact {
                    pending.push(ConfigStep::Outputs(LayerOutputs {
                        target: layer_target.map(|value| (file.clone(), value)),
                        intermediates: layer_intermediates.map(|value| (file.clone(), value)),
                        artifact: layer_artifact,
                    }));
                }
                let base = file.as_deref().and_then(Path::parent).unwrap_or(cwd);
                let layer_includes = config_includes(&config)?;
                if !layer_includes.is_empty() {
                    match includes {
                        Includes::Follow => {}
                        Includes::OwnTarget if own_target => continue,
                        Includes::OwnTarget | Includes::Refuse => return Err(()),
                    }
                }
                // Refuse an oversized layer before allocating its joined paths. The output
                // selection, if any, already counts toward the pending bound.
                if pending.len().saturating_add(layer_includes.len()) > CONFIG_GRAPH_MAX_PENDING {
                    return Err(());
                }
                for include in layer_includes.iter().rev() {
                    pending.push(ConfigStep::Load(
                        include_file(base, include),
                        include.optional,
                    ));
                }
            }
        }
        if pending.len() > CONFIG_GRAPH_MAX_PENDING {
            return Err(());
        }
    }
    // Only the winning value is judged: a non-empty string with no control character. Only a
    // returned one takes its base: Targo's `Definition::root` unwraps the defining file's
    // grandparent where it resolves the path, before it asks whether the value is absolute.
    let winner = |selected: Option<(Option<PathBuf>, aterm_toml::Value)>, returned: bool| {
        let Some((file, value)) = selected else {
            return Ok(None);
        };
        let aterm_toml::Value::String(value) = value else {
            return Err(());
        };
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(());
        }
        if !returned {
            return Ok(None);
        }
        let base = match file {
            Some(file) => file
                .parent()
                .and_then(Path::parent)
                .ok_or(())?
                .to_path_buf(),
            None => cwd.to_path_buf(),
        };
        Ok(Some(ConfiguredPath { base, value }))
    };
    Ok(ConfigOutputs {
        target: winner(target, select_target)?,
        intermediates: winner(intermediates, true)?,
        artifact,
    })
}

/// One layer's `build.<field>` as written, where Targo's merge may replace it (a string, an
/// integer or a boolean). The graph resolver handles includes and records each output's
/// definition base.
///
/// # Errors
/// A `build` that is no table, which Targo cannot merge or read, and a value that is a list, a
/// table, a float or a date, which Targo cannot merge or load.
fn config_layer_value(
    config: &aterm_toml::Value,
    field: &str,
) -> Result<Option<aterm_toml::Value>, ()> {
    let Some(build) = config.get("build") else {
        return Ok(None);
    };
    let build = build.as_table().ok_or(())?;
    match build.get(field) {
        None => Ok(None),
        Some(
            value @ (aterm_toml::Value::String(_)
            | aterm_toml::Value::Integer(_)
            | aterm_toml::Value::Boolean(_)),
        ) => Ok(Some(value.clone())),
        Some(_) => Err(()),
    }
}

fn absolute(path: PathBuf, cwd: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    (!path
        .components()
        .any(|c| c == std::path::Component::ParentDir))
    .then(|| path.components().collect::<PathBuf>())
}

/// An initial manifest observation. Only absence allows searching a parent directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialManifest {
    Absent,
    Readable(PathBuf),
    Unreadable(PathBuf),
}

/// Initial package discovery within managed ancestry. Native Targo prefers its filename
/// within each directory; compatibility drivers may read Cargo.toml alone. Workspace
/// ancestor and package.workspace lookup are separate operations with their own grammar.
#[must_use]
pub fn initial_manifest(m: &Machine<'_>, native: bool) -> InitialManifest {
    let (Some(home), Some(cwd)) = (m.home, m.cwd) else {
        return InitialManifest::Absent;
    };
    let names: &[&str] = if native {
        &["Targo.toml", "Cargo.toml"]
    } else {
        &["Cargo.toml"]
    };
    for directory in cwd
        .ancestors()
        .take_while(|p| p.starts_with(home) && *p != home)
    {
        for name in names {
            let path = directory.join(name);
            match (m.read_manifest)(&path) {
                Ok(None) => {}
                Ok(Some(_)) => return InitialManifest::Readable(path),
                Err(_) => return InitialManifest::Unreadable(path),
            }
        }
    }
    InitialManifest::Absent
}

fn manifest_modes(tool: Tool, line: Option<&Line>) -> &'static [bool] {
    match tool {
        Tool::Cargo => &[false],
        Tool::Targo if line.is_some_and(|line| !line.trust && line.alias.is_none()) => &[true],
        _ => &[true, false],
    }
}

/// Initial discovery observations for custody, on an already expanded command line.
/// Unknown command/proof dispatch retains both supported grammars. The same policy
/// selects manifest readings before an output may acquire an owner.
#[must_use]
pub fn initial_manifest_readings(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
) -> Vec<InitialManifest> {
    let line = m.home.and_then(|home| {
        let root = m.root.map_or_else(|| cache_root(home), Path::to_path_buf);
        Line::parse(args, tool, home, &root, consents(args))
    });
    manifest_modes(tool, line.as_ref())
        .iter()
        .map(|&native| initial_manifest(m, native))
        .collect()
}

fn cache_manifests(line: &Line, tool: Tool, m: &Machine<'_>) -> Option<Vec<PathBuf>> {
    if let Some(path) = line.manifest_path.as_deref() {
        return Some(vec![absolute(PathBuf::from(path), m.cwd?)?]);
    }
    let mut manifests = Vec::new();
    for &native in manifest_modes(tool, Some(line)) {
        let InitialManifest::Readable(path) = initial_manifest(m, native) else {
            return None;
        };
        if !manifests.contains(&path) {
            manifests.push(path);
        }
    }
    Some(manifests)
}

/// The target the configs name, as [`read_configured_target`] reads it: the nearest
/// configuration's, its includes followed for every tool but an upstream cargo (which reads
/// `include` only from 1.93, before that under `-Zconfig-include` alone). Parse TOML rather
/// than mistaking a comment or an alias argument for a target selection. The home read is the
/// first of [`package_homes`]: `CARGO_HOME`, or `~/.cargo`, which the walk already passes as
/// the `.cargo` of an ancestor of every working directory under HOME. A `TARGO_HOME` after it
/// can name a target only where none of those does, one Targo 16387 would not build into and
/// Trust's main would, so there is none to take (and [`configs_unresolved`] owes the call custody).
fn configured_target(cwd: &Path, tool: Tool, m: &Machine<'_>) -> Option<PathBuf> {
    let homes = package_homes(tool, m);
    read_configured_target(
        cwd,
        homes.first().map(PathBuf::as_path),
        m,
        tool != Tool::Cargo,
    )
    .ok()
    .flatten()
}

/// Cargo's selected final target, without inventing a workspace's default directory.
/// Environment paths are relative to `cwd`; a configuration's path is relative to the folder
/// above the one its defining file is in (`Definition::root`: the directory containing `.cargo`
/// for a file there). A caller with no explicit selection chooses its own workspace default.
/// This reads configuration only and creates nothing.
///
/// # Errors
/// A selected target is empty; a Cargo configuration Targo would refuse, or this reader cannot
/// read ([`read_configured_target`]): a file it cannot read or parse as TOML 1.1, a `build` that
/// is no table, an `include` Targo refuses (no list, an entry or path of another shape, one
/// that names nothing though not optional, a file loaded twice); or a selected target-dir that
/// is a list, a table, a float or a date anywhere in its file's chain of includes (which Targo
/// cannot merge or load), is no string, is empty, holds a control character, or sits in a file
/// with no grandparent to resolve it against.
pub fn cargo_target_hint(cwd: &Path) -> Result<Option<PathBuf>, String> {
    target_hint_at(cwd, Tool::Cargo)
}

/// Targo's selected final target, including its `TARGO_` environment twins.
/// Different targets in the package homes supported Targo versions read are ambiguous,
/// not permission for a helper to replace the caller's selection with its own default.
///
/// # Errors
/// As [`cargo_target_hint`], or the supported package homes select different targets.
pub fn targo_target_hint(cwd: &Path) -> Result<Option<PathBuf>, String> {
    target_hint_at(cwd, Tool::Targo)
}

fn target_hint_at(cwd: &Path, tool: Tool) -> Result<Option<PathBuf>, String> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty());
    #[cfg(windows)]
    let home = home.or_else(|| std::env::var_os("USERPROFILE").filter(|home| !home.is_empty()));
    let home = home.map(PathBuf::from);
    let machine = Machine {
        home: home.as_deref(),
        root: None,
        cwd: Some(cwd),
        env: &|name| std::env::var_os(name),
        read: &read_config,
        read_manifest: &read_manifest,
        writable: &|_| None,
    };
    target_hint(&machine, tool)
}

/// [`cargo_target_hint`] using a producer's captured environment and configuration reader.
/// This reads configuration only and creates no directories.
///
/// # Errors
/// As [`cargo_target_hint`], or the machine has no working directory.
pub fn cargo_target_hint_with(machine: &Machine<'_>) -> Result<Option<PathBuf>, String> {
    target_hint(machine, Tool::Cargo)
}

fn target_hint(m: &Machine<'_>, tool: Tool) -> Result<Option<PathBuf>, String> {
    let cwd = m
        .cwd
        .ok_or("Cargo target lookup has no working directory")?;
    for name in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        if let Some(value) = cargo_var(name, tool, m) {
            let path = PathBuf::from(value);
            if path.as_os_str().is_empty() {
                return Err(format!("{name} is empty"));
            }
            return Ok(Some(if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }));
        }
    }
    let cargo_home = cargo_home_hint_with(m)?;
    let target = read_configured_target(cwd, cargo_home.as_deref(), m, true)?;
    if !reads_twins(tool) {
        return Ok(target);
    }
    for other in package_homes(tool, m) {
        if Some(&other) == cargo_home.as_ref() {
            continue;
        }
        if read_configured_target(cwd, Some(&other), m, true)? != target {
            return Err("supported Targo package homes select different target directories".into());
        }
    }
    Ok(target)
}

/// Cargo's home from a producer snapshot, as Targo folds it ([`package_homes`]): a non-empty
/// `CARGO_HOME` against the actual working directory, else a non-empty home's `.cargo`, its `.`
/// and `..` folded as written.
///
/// # Errors
/// The machine has no working directory.
pub fn cargo_home_hint_with(m: &Machine<'_>) -> Result<Option<PathBuf>, String> {
    let cwd = m.cwd.ok_or("Cargo home lookup has no working directory")?;
    Ok((m.env)("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(|path| lexical(&cwd.join(path)))
        .or_else(|| {
            m.home
                .filter(|home| !home.as_os_str().is_empty())
                .map(|home| lexical(&home.join(".cargo")))
        }))
}

/// The `build.target-dir` the configs Targo walks select ([`discovered_config_outputs`]: the
/// nearest file's, each read as Targo loads it, its includes merged where `follow`; Targo always
/// loads them, an upstream cargo may not), joined to its defining file's `Definition::root`. The
/// files farther than the nearest one that names it are not read: one Targo cannot merge with it
/// (a list there) fails Targo's load, so such a call is one this reader grants a directory Targo
/// never uses, never one whose directory it misses.
///
/// # Errors
/// A config this reader cannot read or Targo would refuse ([`config_graph_outputs`]): a file it
/// cannot read or parse as TOML 1.1, a `build` that is no table, an `include` Targo refuses or
/// one that names nothing though not optional, a file loaded twice, a value Targo cannot merge
/// or load, and a winning target that is no non-empty string, holds a control character
/// (refused, as the shim refuses to place under one, though this reader decodes it as Targo
/// does), or sits in a file with no grandparent (where Targo's `Definition::root` unwraps one,
/// absolute value or not, and panics).
fn read_configured_target(
    cwd: &Path,
    cargo_home: Option<&Path>,
    m: &Machine<'_>,
    follow: bool,
) -> Result<Option<PathBuf>, String> {
    let outputs = discovered_config_outputs(cwd, cargo_home, m, false, follow, true)
        .map_err(|()| configs_refused("target-dir", cwd))?;
    Ok(outputs
        .target
        .map(|selected| selected.base.join(selected.value)))
}

/// Why a walk of the configs Targo loads from `cwd` selects no `field` a reader can take.
fn configs_refused(field: &str, cwd: &Path) -> String {
    format!(
        "a Cargo configuration Targo loads from {} cannot be read, or selects a {field} Targo \
         would refuse",
        cwd.display()
    )
}

/// A directory a config selects: its value as written and its defining file's
/// `Definition::root` (the working directory for an inline `--config` value's), which a
/// relative value is resolved against.
struct ConfiguredPath {
    base: PathBuf,
    value: String,
}

/// A caller's intermediate-directory choice. The forwarded value preserves Cargo templates;
/// a fully named path is available only when no Cargo-computed template remains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CargoBuildDirHint {
    value: OsString,
    path: Option<PathBuf>,
}

impl CargoBuildDirHint {
    /// The command-local value for `CARGO_BUILD_BUILD_DIR`.
    #[must_use]
    pub fn value(&self) -> &OsStr {
        &self.value
    }

    /// The absolute lexical path, when Cargo need not compute a template to name it.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// Capture the caller's intermediate directory using Cargo environment/config precedence:
/// [`BUILD_DIR_ENV`], else the `build.build-dir` the configs Targo walks select, their includes
/// followed ([`discovered_config_outputs`]), the home read [`cargo_home_hint_with`]'s.
/// Config-relative paths retain their original base when forwarded through the environment.
/// Valid templates remain Cargo's to expand; this grants no directory or cleanup authority.
///
/// # Errors
/// The machine has no working directory; the variable is no UTF-8 text; the selection is
/// empty, holds an unmatched brace or an unknown template variable; or a configuration this
/// reader cannot read or Targo would refuse ([`read_configured_target`]'s refusals).
pub fn cargo_build_dir_hint_with(m: &Machine<'_>) -> Result<Option<CargoBuildDirHint>, String> {
    let cwd = m
        .cwd
        .ok_or("Cargo build-directory lookup has no working directory")?;
    let (base, value) = match (m.env)(BUILD_DIR_ENV) {
        Some(value) => (
            cwd.to_path_buf(),
            value
                .into_string()
                .map_err(|_| "Cargo build-dir is not UTF-8")?,
        ),
        None => {
            let home = cargo_home_hint_with(m)?;
            let Some(selected) =
                discovered_config_outputs(cwd, home.as_deref(), m, true, true, true)
                    .map_err(|()| configs_refused("build-dir", cwd))?
                    .intermediates
            else {
                return Ok(None);
            };
            (selected.base, selected.value)
        }
    };
    if value.is_empty() {
        return Err("Cargo build-dir is empty".into());
    }
    let mut rest = value.as_str();
    let mut templated = false;
    while let Some(at) = rest.find(['{', '}']) {
        let suffix = &rest[at..];
        if !suffix.starts_with('{') {
            return Err("Cargo build-dir has an unmatched closing brace".into());
        }
        let Some(end) = suffix.find('}') else {
            return Err("Cargo build-dir has an unmatched opening brace".into());
        };
        if !matches!(
            &suffix[1..end],
            "workspace-root" | "cargo-cache-home" | "workspace-path-hash"
        ) {
            return Err("Cargo build-dir has an unknown template variable".into());
        }
        templated = true;
        rest = &suffix[end + 1..];
    }
    // Cargo expands templates before applying the config base. Leading absolute-producing
    // templates therefore keep their spelling; a hash stays relative to that original base.
    let named = if value.starts_with("{workspace-root}") || value.starts_with("{cargo-cache-home}")
    {
        PathBuf::from(&value)
    } else {
        base.join(&value)
    };
    let path = (!templated && named.is_absolute()).then(|| named.clone());
    Ok(Some(CargoBuildDirHint {
        value: named.into_os_string(),
        path,
    }))
}

/// A configuration can select a target without an argv or environment target. When this
/// observer cannot resolve that choice, its caller must retain whole-cache ownership rather
/// than letting a managed writer use an older owner's cache without observing the new owner.
/// Only Targo's own words are read ([`own_words`]): `run serve --config app.toml` hands its
/// `--config` to `serve`.
#[must_use]
pub fn unresolved_directory(args: &[String], tool: Tool, m: &Machine<'_>) -> bool {
    unresolved_words(args, tool, m)
        || m.cwd.is_none_or(|cwd| {
            m.home.is_none_or(|home| !cwd.starts_with(home))
                || configs_unresolved(cwd, tool, m, reads_artifacts(args))
        })
}

/// [`unresolved_directory`]'s reading of `args` and the environment: a build or artifact
/// directory there (`--artifact-dir` in either spelling; [`ARTIFACT_DIR_ENV`] where the line
/// reads one, [`reads_artifacts`]: a folder Targo links final artifacts into, which no target
/// holds), a `-C`, or a `--config` value ([`cache_config_outputs`], read against the directory
/// Targo works in, `m.cwd`) that names a build directory, an artifact directory where the line
/// reads one, a target directory no typed `--target-dir` ([`typed_target_dir`]) or
/// `CARGO_TARGET_DIR` outranks, or what this reader cannot read. `m.read` must fail, never say
/// no file stands, where Targo finds one it loads, a folder, a FIFO or a device included (atpkg's
/// front reader fails on each): the configs a walk finds are read through it alone, while a
/// `--config` value's file and the files it includes, where Targo asks whether something stands
/// itself, are unread where the reader says none and something does ([`stands`]).
#[must_use]
pub fn unresolved_words(args: &[String], tool: Tool, m: &Machine<'_>) -> bool {
    words_unresolved(args, tool, m, typed_target_dir(args, tool).is_some())
}

/// [`unresolved_words`], `typed_target` whether the line's own words name a `--target-dir` (an
/// empty one is the working directory, `value_of_path` joining it to it), read by the reader its
/// caller takes the target from. Targo takes the target from that word, then `CARGO_TARGET_DIR`
/// (or its twin, [`cargo_var`]), then `build.target-dir`, where a `--config` value outranks
/// `CARGO_BUILD_TARGET_DIR` and every config file (`context/mod.rs`'s `target_dir`;
/// `Definition`'s priority, `Cli` over `Environment`), so a `--config` target directory moves the
/// build only where neither of the first two stands. Every `--config` value among Targo's own
/// words is read, before the verb and after it, a superset of the values Targo merges (the hop
/// hands it one list, a direct call keeps the later one; the line `miri` runs holds those its
/// `cargo metadata` reads where it names no target, [`miri_line`]), with one cycle check across
/// them all (`cli_args_as_table`'s `seen`).
fn words_unresolved(args: &[String], tool: Tool, m: &Machine<'_>, typed_target: bool) -> bool {
    if cargo_var(BUILD_DIR_ENV, tool, m).is_some() || words_name_unowned(args, tool, m) {
        return true;
    }
    let artifacts = reads_artifacts(args);
    let select_target = !typed_target && cargo_var("CARGO_TARGET_DIR", tool, m).is_none();
    let mut seen = std::collections::BTreeSet::new();
    let mut words = args[..own_words(args, tool)].iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        let config = if word == "--config" {
            words.next()
        } else {
            word.strip_prefix("--config=")
        };
        if let Some(value) = config {
            match cache_config_outputs(value, m, select_target, tool != Tool::Cargo, &mut seen) {
                Ok(outputs)
                    if outputs.target.is_none()
                        && outputs.intermediates.is_none()
                        && !(artifacts && outputs.artifact) => {}
                _ => return true,
            }
        }
    }
    false
}

/// The last `--target-dir` among Targo's own words ([`own_words`]: `run serve --target-dir out`
/// hands it to `serve`), before `--`, as typed (empty included), the values of `--config`,
/// `--manifest-path`, `--color` and `--explain`, and a short option's value left to the next word
/// where clap takes it ([`takes_next`]), skipped; `None` for none.
#[must_use]
pub fn typed_target_dir(args: &[String], tool: Tool) -> Option<&str> {
    let mut target = None;
    let mut words = args[..own_words(args, tool)]
        .iter()
        .map(String::as_str)
        .peekable();
    while let Some(word) = words.next() {
        match word {
            "--" => break,
            "--target-dir" => target = words.next(),
            "--manifest-path" | "--config" | "--color" | "--explain" => {
                let _ = words.next();
            }
            _ => {
                if let Some(path) = word.strip_prefix("--target-dir=") {
                    target = Some(path);
                } else if let Some((letter, None)) = shorts(word).and_then(|c| c.valued) {
                    let _ = words.next_if(|next| takes_next(letter, next));
                }
            }
        }
    }
    target
}

/// Whether parsed TOML has the shape of the one dotted key and its value Targo takes inline
/// (`toml_dotted_keys`): each table from the root down holds one key, the last a value that is
/// no table. An inline table's keys have the shape too, which Targo refuses: its caller tells
/// them apart by their text ([`sets_no_inline_table`]).
fn one_dotted_key(config: &aterm_toml::Value) -> bool {
    let mut value = config;
    while let aterm_toml::Value::Table(table) = value {
        let mut entries = table.values();
        match (entries.next(), entries.next()) {
            (Some(next), None) => value = next,
            _ => return false,
        }
    }
    true
}

/// [`unresolved_directory`]'s reading of the configs Targo reads walking up from `root`
/// (`walk_tree`: each parent's `.cargo` [`config_walk`] reads, `..` walked as written, then the
/// package homes): whether one names a target or build directory, an artifact directory where
/// the call reads one (`artifacts`, [`reads_artifacts`]), or one it cannot tell. A working
/// directory outside HOME [`unresolved_directory`] takes for one that may.
#[must_use]
pub fn configs_unresolved(root: &Path, tool: Tool, m: &Machine<'_>, artifacts: bool) -> bool {
    config_walk(root, m)
        .into_iter()
        .chain(package_homes(tool, m))
        .any(|dir| {
            let named = config_dirs(&dir, m);
            named.target || named.build || (artifacts && named.artifact)
        })
}

/// [`configs_unresolved`]'s walk from `root` read for a call whose alias words this reader cannot
/// tell, which may be any line a config's text holds: whether a config's line that is no comment
/// spells a directory's key wherever it stands ([`Named::mentioned`]: an alias's words among
/// them), or one this reader cannot read.
#[must_use]
pub fn configs_mention_dirs(root: &Path, tool: Tool, m: &Machine<'_>) -> bool {
    config_walk(root, m)
        .into_iter()
        .chain(package_homes(tool, m))
        .any(|dir| {
            let named = config_dirs(&dir, m);
            named.mentioned || named.unknown
        })
}

/// [`configs_unresolved`]'s walk from `root` read for a build whose target and build directory
/// outrank every config (`cargo-miri`'s sysroot build): whether a config names an artifact
/// directory, into which that build copies its output, or is one this reader cannot read.
#[must_use]
pub fn configs_name_artifacts(root: &Path, tool: Tool, m: &Machine<'_>) -> bool {
    config_walk(root, m)
        .into_iter()
        .chain(package_homes(tool, m))
        .any(|dir| {
            let named = config_dirs(&dir, m);
            named.artifact || named.unknown
        })
}

/// Whether the configs Targo reads ([`config_walk`], [`package_homes`]) may be others than the
/// ones the shim's block reads, which walks every ancestor and reads each home through the
/// kernel: a `__CARGO_TEST_ROOT` is set (Targo's walk stops there, the block's never does; any
/// value, as the block cannot tell a UTF-8 one), or a home read has a `..` part (Targo folds it as
/// written, the kernel through a link). Neither half places such a call, so the two agree.
fn walks_unlike_the_block(tool: Tool, m: &Machine<'_>) -> bool {
    let climbs = |name: &str| {
        (m.env)(name).is_some_and(|value| {
            Path::new(&value)
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        })
    };
    (m.env)("__CARGO_TEST_ROOT").is_some()
        || climbs("CARGO_HOME")
        || ((m.env)("CARGO_HOME").is_none() && reads_twins(tool) && climbs("TARGO_HOME"))
}

/// Decide the build-cache placement ([`BuildDir::Cache`], [`BuildDir::Target`]). A call whose
/// words typed before the verb hold one `-C` is judged as the same call typed from the directory
/// Targo changes into ([`directory_placed`]: its configs, package homes and relative targets
/// read there, whatever the launcher's folder, which Targo reads nothing from but the `-C`
/// itself), or left as typed where that directory cannot be told; any other call is judged from
/// the launcher's folder, which must be under HOME, not HOME itself, outside the shared cache,
/// brace-free and the `$PWD` it inherited, where one is set.
#[must_use]
pub fn decide(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<BuildDir> {
    let home = m
        .home
        .filter(|h| h.is_absolute() && !h.as_os_str().as_encoded_bytes().ends_with(b"/"))?;
    let cwd = m.cwd?;
    let root = m.root.map_or_else(|| cache_root(home), Path::to_path_buf);
    let cache = config_value(&root)?;
    // Where the call is judged from when no `-C` moves it: a launcher this reader places from.
    let launcher_placed = cwd.starts_with(home)
        && cwd != home
        && !cwd.starts_with(&root)
        && !has_brace(cwd)
        && (m.env)("PWD").is_none_or(|p| Path::new(&p) == cwd);
    if cargo_var(BUILD_DIR_ENV, tool, m).is_some() || walks_unlike_the_block(tool, m) {
        return None;
    }
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|a| a.starts_with('+')));
    // Placement reads no user alias's words, as the shim reads none: a builtin alias's `-m` is
    // read even where one may take its name, which can only leave the call as typed.
    let mut line = Line::read(&args[at..], tool, home, &root, consents(&args[at..]))?;
    let moved_to;
    let moved;
    let (cwd, m) = match line.directory.take() {
        None if launcher_placed => (cwd, m),
        None => return None,
        Some(dir) => {
            moved_to = directory_placed(&dir, &line, tool, cwd, home, &root, m)?;
            moved = Machine {
                cwd: Some(&moved_to),
                ..*m
            };
            (moved_to.as_path(), &moved)
        }
    };
    let mut target_named = false;
    let mut alias_builds = false;
    let mut configured = false;
    let query_alias = line
        .verb_at
        .and_then(|verb| args.get(at + verb))
        .filter(|verb| !builtin_command(verb, tool));
    // Every build reads the walk and the first package home (`~/.cargo` is walked from every
    // folder under HOME, which `cwd` is); a later home only Trust's main reads, so an alias it
    // defines builds for that build and is no command of Targo 16387's: it places no call.
    let homes = package_homes(tool, m)
        .into_iter()
        .enumerate()
        .map(|(at, dir)| (dir, at == 0));
    for (dir, every_build) in config_walk(cwd, m)
        .into_iter()
        .map(|dir| (dir, true))
        .chain(homes)
    {
        let named = config_scan(
            &dir,
            line.alias.as_deref().or(query_alias.map(String::as_str)),
            m,
        );
        if named.build || named.unread {
            return None;
        }
        target_named |= named.target;
        alias_builds |= named.alias_builds && every_build;
        configured |= named.present;
    }
    // A configured short alias may hand its help words to a program. Only a builtin
    // or an unconfigured alias has the literal query meaning at this placement boundary.
    if line.query && (query_alias.is_none() || !configured) {
        return None;
    }
    if line.alias.is_some() && !alias_builds {
        return None;
    }
    let chosen = line
        .target_dir
        .take()
        .map(OsString::from)
        .or_else(|| cargo_var("CARGO_TARGET_DIR", tool, m))
        .or_else(|| cargo_var("CARGO_BUILD_TARGET_DIR", tool, m));
    let Some(target) = chosen else {
        if line.cache && cache_writable(home, &root, m) {
            // A configured final target does not change the workspace identity of the
            // shared intermediates. Leave that final target to Cargo to resolve.
            return Some(BuildDir::Cache(cache));
        }
        if target_named {
            return None;
        }
        return Some(
            line.place(
                target_build_path(Path::new("{workspace-root}/target"))
                    .to_str()?
                    .to_owned(),
            ),
        );
    };
    let target = target.to_str()?;
    // An empty typed target is the working directory itself, an empty variable one Targo
    // refuses: neither is placed.
    if target.is_empty() {
        return None;
    }
    // Joined with the host's separator: a Windows `\\?\` path reads `/` as a name's letter.
    let sep = std::path::MAIN_SEPARATOR;
    let target = if Path::new(target).is_absolute() {
        target.to_owned()
    } else {
        format!("{}{sep}{target}", cwd.to_str()?)
    };
    if target
        .chars()
        .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    // Preserve the target's emitted spelling, as the shell does. Custody compares path
    // components, so redundant separators and `.` do not change the derived directory.
    Some(line.place(format!(
        "{target}{sep}{}",
        target_build_relative().display()
    )))
}

/// The variable that names the channel Targo reads in place of its own (`features.rs`'s `channel`).
const CHANNEL_OVERRIDE: &str = "__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS";

/// The directory [`decide`] judges a call from whose words typed before the verb hold the one
/// `-C` `dir` ([`Line::directory`]), launched at `cwd`: the folder Targo changes into before it
/// reads a config, an alias or a relative path, read back as Targo reads it ([`changed_into`]:
/// a link's target, its `..`, a trailing `/` and the data volume's firmlink folded). `None`, the
/// call left as typed, wherever Targo would refuse the `-C` or this reader cannot tell that it
/// honours it, as the shim's block cannot: the call is neither Targo's own nor its
/// Cargo-compatible name's, which reads a `-C` as Targo does (the lint's words are its own, and
/// an upstream cargo's `-C` is declined even where [`nightly_channel`] reads its channel and the
/// observer honours it), the gate is not held before the verb (`-Z
/// unstable-options`, [`Line::gate`]) or a `-Z` after the verb may replace it
/// ([`Line::z_after`]), a channel is named in Targo's place ([`CHANNEL_OVERRIDE`], set at all);
/// and wherever the directory is empty, cannot be changed into, is HOME itself or outside it,
/// lies in the shared cache `root`, or holds a brace, as [`decide`] leaves a launcher's folder
/// there.
fn directory_placed(
    dir: &str,
    line: &Line,
    tool: Tool,
    cwd: &Path,
    home: &Path,
    root: &Path,
    m: &Machine<'_>,
) -> Option<PathBuf> {
    if !matches!(tool, Tool::Targo | Tool::CargoCompat)
        || !line.gate
        || line.z_after
        || (m.env)(CHANNEL_OVERRIDE).is_some()
        || dir.is_empty()
    {
        return None;
    }
    let moved = changed_into(&cwd.join(dir))?;
    if !moved.starts_with(home) || moved == home || moved.starts_with(root) || has_brace(&moved) {
        return None;
    }
    Some(moved)
}

/// The folder a process that changes into `dir` (absolute) reads back as its own, as Targo reads
/// its own once it has (`cli.rs`'s `set_current_dir`, then `reload_cwd`'s `getcwd`), or `None`
/// where it cannot change into `dir`: nothing there, no folder, or one this user may not search.
/// On macOS the kernel's own name for the folder, opened for search alone as changing into it
/// needs (`O_SEARCH`), asked for its path (`F_GETPATH`), the name `getcwd` gives a process in
/// it: a folder named through the data volume's firmlink (`/System/Volumes/Data/Users/<me>`) is
/// read back by its root spelling (`/Users/<me>`), where `realpath` keeps the spelling given.
/// Elsewhere `canonicalize`'s, a link's target with its `..` folded, once `.` inside it reads
/// as a folder.
#[must_use]
pub fn changed_into(dir: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        // `<sys/fcntl.h>`'s `O_SEARCH` (`O_EXEC | O_DIRECTORY`), which the generated libc does
        // not carry: a folder this user may search, whatever it lets them read.
        const O_SEARCH: libc::c_int = 0x4000_0000 | libc::O_DIRECTORY;
        let folder = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | O_SEARCH)
            .open(dir)
            .ok()?;
        let mut named = vec![0_u8; usize::try_from(libc::PATH_MAX).ok()?];
        // SAFETY: a live descriptor and a buffer of `PATH_MAX` bytes (`MAXPATHLEN`), which
        // `F_GETPATH` fills with a NUL-terminated path.
        if unsafe { libc::fcntl(folder.as_raw_fd(), libc::F_GETPATH, named.as_mut_ptr()) } < 0 {
            return None;
        }
        let end = named.iter().position(|&b| b == 0)?;
        Some(PathBuf::from(OsStr::from_bytes(&named[..end])))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let moved = std::fs::canonicalize(dir).ok()?;
        std::fs::metadata(moved.join("."))
            .is_ok_and(|meta| meta.is_dir())
            .then_some(moved)
    }
}

/// `args` with the `--config` [`decide`] adds, or `None` when it adds none. That pair leads (a
/// shim's front strips it there), and every other word stays as typed: the pair says where the
/// build's intermediates go, never which units it builds, so the call resolves exactly the unit
/// graph it resolves unplaced (features included: no [`TEST_GRAPH_CONFIG`] or
/// [`TEST_GRAPH_FLAG`] is added, whatever the verb or profile).
#[must_use]
pub fn with_build_dir(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<Vec<String>> {
    placed_build_dir(args, tool, m).map(|(_, args)| args)
}

/// The same placement as [`with_build_dir`], retaining the exact decision whose arguments
/// were constructed. Admission can validate a selected plan without reading config twice.
#[must_use]
pub fn placed_build_dir(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
) -> Option<(BuildDir, Vec<String>)> {
    let selected = decide(args, tool, m)?;
    let (BuildDir::Cache(value) | BuildDir::Target(value)) = &selected else {
        return None;
    };
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|a| a.starts_with('+')));
    let mut out = Vec::with_capacity(args.len() + 2);
    out.extend_from_slice(&args[..at]);
    out.push("--config".to_owned());
    out.push(value.clone());
    out.extend_from_slice(&args[at..]);
    Some((selected, out))
}

/// Whether the cache, or the folder it will be made in, may be written.
fn cache_writable(home: &Path, root: &Path, m: &Machine<'_>) -> bool {
    (m.writable)(root)
        .or_else(|| root.parent().and_then(|p| (m.writable)(p)))
        .or_else(|| {
            home.join("Library/Caches")
                .ancestors()
                .take_while(|path| path.starts_with(home))
                .find_map(m.writable)
        })
        .unwrap_or(false)
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
    /// A literal non-building answer among the frontend's own words.
    query: bool,
    /// An `--unverified` build, the lint or `clean`: the shared cache is for it.
    cache: bool,
    /// `targo trust`, which takes its build directory from the environment.
    trust: bool,
    /// A `--target-dir` before `--`.
    target_dir: Option<String>,
    /// The last explicit manifest (`--manifest-path`, or `-m` where it is Targo's word,
    /// [`short_manifest_scope`]), before `--`, independent of the launcher's cwd.
    manifest_path: Option<String>,
    /// A verb cargo does not build by itself, which builds only as an alias that does.
    alias: Option<String>,
    /// CLI config values among the frontend's own words, in their original order.
    configs: Vec<String>,
    /// Where the verb is in the line read, when one was.
    verb_at: Option<usize>,
    /// The value of the one `-C` before the verb, as typed (an empty one included): the
    /// directory Targo changes into before it reads anything else ([`decide`] resolves it).
    directory: Option<String>,
    /// Whether a `-Z` value before the verb is `unstable-options`, the gate Targo's `-C` needs.
    gate: bool,
    /// Whether a `-Z` stands among Targo's own words after the verb, where clap's values replace
    /// those before it (`propagate_globals`), so the gate before it may not be the one kept.
    z_after: bool,
}

impl Line {
    /// A building verb, or one an alias may make one, `unverified` whether the call runs in the
    /// lane ([`in_lane`]; placement reads the words typed alone, [`consents`], as the shim's
    /// block does), each `--config` value one placement takes ([`placed_config`]).
    fn read(
        args: &[String],
        tool: Tool,
        home: &Path,
        root: &Path,
        unverified: bool,
    ) -> Option<Self> {
        let line = Self::parse(args, tool, home, root, unverified)?;
        line.configs
            .iter()
            .all(|v| placed_config(v))
            .then_some(line)
    }

    /// The argument grammar of [`Self::read`] with no config policy: custody reads each
    /// `--config` value itself ([`cache_config_outputs`]) against the fully selected explicit
    /// target, where placement keeps its lexical rule.
    fn parse(
        args: &[String],
        tool: Tool,
        home: &Path,
        root: &Path,
        unverified: bool,
    ) -> Option<Self> {
        let manifest_ok = |v: &str| {
            let p = Path::new(v);
            !v.is_empty()
                && !v.contains("..")
                && !v.contains("//")
                && !v.contains("/./")
                && !v.ends_with("/.")
                && !has_brace(p)
                && !p.starts_with(root)
                && (!v.starts_with('/') || p.starts_with(home))
        };
        let mut verb = (tool == Tool::Tippy).then_some("tippy");
        let mut verb_at = None;
        let mut target_dir = None;
        let mut manifest_path = None;
        let mut configs = Vec::new();
        let mut query = false;
        let mut global_query = false;
        let mut directory = None;
        let mut directories = 0_usize;
        let mut gate = false;
        let mut z_after = false;
        let mut direct_help = false;
        let (scope, own) = scope_walk(args, tool);
        // A `--target-dir` after a name a user alias may take may be its program's (`xtask =
        // "run -p xtask --"`), where a build directory placed under it would be no target's:
        // such a call is left as typed.
        let named = |verb: Option<&str>| verb.is_some_and(|verb| !builtin_command(verb, tool));
        // Targo's own words: a program's are its own (`run serve --target-dir out`).
        let mut words = args[..own]
            .iter()
            .map(String::as_str)
            .enumerate()
            .peekable();
        while let Some((at, a)) = words.next() {
            if help_word(a) {
                direct_help |= verb_at.is_some_and(|v| at == v + 1)
                    && verb.is_some_and(|verb| builtin_command(verb, tool));
            }
            if help_word(a) && (verb.is_none() || verb.is_some_and(exec_builtin)) {
                query = true;
                global_query |= verb.is_none();
            }
            // A flag's value is the next word.
            let mut it = (&mut words).map(|(_, v)| v);
            let ok = match a {
                "--" => break,
                "--help" | "-h" | "--version" | "-V" => {
                    query = true;
                    global_query |= verb.is_none();
                    true
                }
                "--config" => it.next().map(|v| configs.push(v.to_owned())).is_some(),
                "--manifest-path" => it.next().is_some_and(|v| {
                    manifest_path = Some(v.to_owned());
                    manifest_ok(v)
                }),
                "--target-dir" => {
                    !named(verb) && it.next().map(|v| target_dir = Some(v.to_owned())).is_some()
                }
                "--profile" => it.next().is_some(),
                "--color" | "--explain" if verb.is_none() => it.next().is_some(),
                _ => {
                    if let Some(v) = a.strip_prefix("--config=") {
                        configs.push(v.to_owned());
                        true
                    } else if let Some(v) = a.strip_prefix("--manifest-path=") {
                        manifest_path = Some(v.to_owned());
                        manifest_ok(v)
                    } else if let Some(v) = a.strip_prefix("--target-dir=") {
                        target_dir = Some(v.to_owned());
                        !named(verb)
                    } else if a.starts_with("--") && verb.is_some() {
                        // After the verb a long option takes the word clap gives it
                        // ([`long_takes_next`]), as the scope reads it: `--jobs` any.
                        let _ = words.next_if(|(_, next)| long_takes_next(a, next));
                        true
                    } else if let Some(cluster) = shorts(a) {
                        match cluster.valued {
                            // The first directory before the verb, in each spelling clap reads
                            // (`-C d`, `-Cd`, `-C=d`, `-qC d`, `-qCd`), is recorded; a second,
                            // one after the verb, which clap refuses, and one whose value is
                            // missing (`-C -q`) leave the call as typed.
                            Some(('C', value)) => {
                                directories += 1;
                                let value = value.or_else(|| {
                                    words
                                        .next_if(|(_, next)| takes_next('C', next))
                                        .map(|(_, next)| next)
                                });
                                directory = value.map(str::to_owned);
                                verb.is_none() && directories == 1 && value.is_some()
                            }
                            // A `-Z` value, the gate's before the verb, and one after it.
                            Some(('Z', value)) => {
                                let value = value.or_else(|| {
                                    words
                                        .next_if(|(_, next)| takes_next('Z', next))
                                        .map(|(_, next)| next)
                                });
                                if verb.is_none() {
                                    gate |= value == Some("unstable-options");
                                } else {
                                    z_after = true;
                                }
                                true
                            }
                            // `--manifest-path`, in each spelling clap reads, where it is
                            // Targo's word.
                            Some(('m', value)) if scope[at] => {
                                value.or_else(|| it.next()).is_some_and(|v| {
                                    manifest_path = Some(v.to_owned());
                                    manifest_ok(v)
                                })
                            }
                            // Any other letter's value left to the next word is that word where
                            // clap takes it ([`takes_next`]: `-vZ x`, `-p x`, not `-p -m`). One
                            // the line ends without leaves no verb before it, or is a program's.
                            Some((letter, None)) => {
                                let _ = words.next_if(|(_, next)| takes_next(letter, next));
                                true
                            }
                            _ => true,
                        }
                    } else {
                        if !a.starts_with('-') && verb.is_none() {
                            verb = Some(a);
                            verb_at = Some(at);
                        }
                        true
                    }
                }
            };
            if !ok {
                return None;
            }
        }
        let query = direct_help
            || global_query
            || match tool {
                Tool::Tippy => tippy_answers(args),
                _ if verb
                    .is_some_and(|verb| exec_builtin(verb) || builtin_alias(verb).is_some()) =>
                {
                    query
                }
                Tool::Targo => verb_at.is_some_and(|at| tool_answers(&args[at], &args[at + 1..])),
                _ => false,
            };
        let lane = tool.is_cargo() || unverified;
        let (cache, trust, alias) = match verb {
            Some("help") => return None,
            Some("clean" | "tippy" | "clippy") => (true, false, None),
            Some("trust") if tool == Tool::Targo => (false, true, None),
            Some(v) if BUILD_VERBS.contains(&v) && lane => (true, false, None),
            Some(v) if lane && !BUILD_VERBS.contains(&v) => (true, false, Some(v.to_owned())),
            _ => return None,
        };
        Some(Self {
            query,
            cache,
            trust,
            target_dir,
            manifest_path,
            alias,
            configs,
            verb_at,
            directory,
            gate,
            z_after,
        })
    }

    /// `dir` (`<target>/.ubd/build.noindex`) handed over the way this command reads it.
    fn place(&self, dir: String) -> BuildDir {
        if self.trust {
            BuildDir::TrustEnv(dir)
        } else {
            BuildDir::Target(format!("build.build-dir='{dir}'"))
        }
    }
}

/// Whether placement takes a `--config` value for a dotted key that names no directory: it holds
/// an `=`, as every dotted key does, and spells none ([`names_a_dir`]). The shim's block reads the
/// same words (`__atb_ok`).
fn placed_config(value: &str) -> bool {
    value.contains('=') && !names_a_dir(value)
}

/// Whether placement reads a `--config` value as one that may name a target or build directory:
/// one that spells either, an `include`, whose file Targo loads as a config of its own
/// (`cli_args_as_table`), or a backslash, through which a quoted key's escape may spell any of them
/// (`build."target\u002ddir"`). The shim's block reads the same words.
fn names_a_dir(s: &str) -> bool {
    s.contains("target-dir") || s.contains("build-dir") || s.contains("include") || s.contains('\\')
}

/// What `dir/config.toml` or `dir/config` says: to placement ([`config_scan`]), what its lines
/// that are not comments spell, as the shim's block reads them; to [`config_dirs`], what its
/// `build` table names as Targo parses it, and what its lines mention.
#[derive(Default)]
struct Named {
    /// A config exists, so a builtin alias cannot be assumed unconfigured.
    present: bool,
    build: bool,
    target: bool,
    /// `build.artifact-dir`, read by [`config_dirs`] alone: placement never moves that folder.
    artifact: bool,
    /// A line that is no comment spells a directory's key (`target-dir`, `build-dir` or
    /// `artifact-dir`) wherever it stands, set by [`config_dirs`] alone: in the `build` table, or
    /// in an alias's words (`build-dev = "run --target-dir target/tools …"`), another string or a
    /// comment after a value, none of which names a directory Targo builds into. Only a call
    /// whose alias words this reader cannot tell reads it ([`configs_mention_dirs`]).
    mentioned: bool,
    alias_builds: bool,
    /// A line names what no line scan can read, so placement leaves the call as typed: it spells
    /// `include`, whose files Targo loads as part of the config ([`config_dirs`] reads them),
    /// holds a multi-line string ([`holds_multiline_string`]), after which a line that starts
    /// with `#` may close it and go on to name a directory, or holds an escape that can spell a
    /// key no line spells ([`spells_by_escape`]). The shim's block cannot read any of them.
    unread: bool,
    /// A config this reader cannot read, which may name any directory ([`Self::both`]).
    unknown: bool,
}

impl Named {
    /// What a config this reader cannot read may name: a target and a build directory, and
    /// whatever else it may say ([`Self::unknown`]).
    fn both(&mut self) {
        self.build = true;
        self.target = true;
        self.unknown = true;
    }
}

/// What a config's lines that are not comments spell ([`config_text_dirs`]).
#[derive(Default)]
struct Literal {
    build: bool,
    target: bool,
    artifact: bool,
    include: bool,
}

/// Literal output/include markers on non-comment lines. Escaped keys and multi-line strings
/// still need TOML decoding; these markers alone grant no selected directory or graph validity.
fn config_text_dirs(text: &str) -> Literal {
    let mut literal = Literal::default();
    for line in text.lines() {
        if line.trim_start_matches([' ', '\t']).starts_with('#') {
            continue;
        }
        literal.build |= line.contains("build-dir");
        literal.target |= line.contains("target-dir");
        literal.artifact |= line.contains("artifact-dir");
        literal.include |= line.contains("include");
    }
    literal
}

/// [`Named`] for the config files in `dir`, `alias` the verb to look up, each read through its
/// links ([`config_file`]), as Targo and the shim's block (`[ -f ]`) read it.
fn config_scan(dir: &Path, alias: Option<&str>, m: &Machine<'_>) -> Named {
    let mut named = Named::default();
    for name in ["config.toml", "config"] {
        match config_file(&dir.join(name), m) {
            Err(()) => {
                return Named {
                    present: true,
                    build: true,
                    target: true,
                    mentioned: false,
                    artifact: false,
                    alias_builds: false,
                    unread: false,
                    unknown: true,
                };
            }
            Ok(None) => {}
            Ok(Some(text)) => {
                named.present = true;
                let mut in_alias = false;
                for line in text.lines() {
                    let x = line.trim_start_matches([' ', '\t']);
                    if x.starts_with('#') {
                        continue;
                    }
                    named.build |= line.contains("build-dir");
                    named.target |= line.contains("target-dir");
                    named.unread |= line.contains("include")
                        || holds_multiline_string(line)
                        || spells_by_escape(line);
                    if x.starts_with('[') {
                        // The table header as the shim's block reads it: `[alias]` as spelled
                        // there, so a spaced `[ alias ]`, which Targo reads too, leaves an
                        // alias's call as typed in both halves.
                        in_alias = x.starts_with("[alias]");
                    } else if let Some(value) =
                        alias.and_then(|verb| alias_value(x, verb, in_alias))
                    {
                        named.alias_builds |= alias_builds(value);
                    }
                }
            }
        }
    }
    named
}

/// The directories the config file in `dir` that Targo loads names as Targo loads it
/// (`context/mod.rs`'s `get_file_path`, then `_load_file`: the legacy `config` where one stands,
/// else `config.toml`), with the files its `include` names, an `artifact-dir` among them
/// ([`scan_config_graph_dirs`]): each file, and the files it includes, read through its links as
/// Targo reads it ([`config_file`]), and read as TOML 1.1 ([`config_toml`], as Targo parses it)
/// for its `build` table ([`parsed_dirs`]) wherever a line that is no comment spells a
/// directory's key (which an alias's words or a comment after a value may spell too, naming
/// nothing: [`Named::mentioned`] keeps that a line spelled one), an include, an escape that can
/// spell any character ([`spells_by_escape`]) or a multi-line string
/// ([`holds_multiline_string`]), since a quoted key's escape may spell `include` or a directory's
/// key where no line does, and a line that starts with `#` and closes a multi-line string may go
/// on to name one: outside such a string, a line that starts with `#` is a comment, and a file
/// none of whose other lines spells any of these names nothing. Each config file a walk finds
/// starts its own cycle check (`load_file`'s fresh set), shared by the files it includes. Text
/// this reader cannot parse, which Targo refuses to load, names what its lines spell where they
/// tell (no include, escape or multi-line string). A file that holds one of those and that this
/// reader cannot parse, and an include it cannot follow, name both: text it cannot parse, an
/// `include` that is no list, an entry of another type, a path that is no `.toml` or holds a
/// glob, a brace or a control character ([`config_includes`]), one that names nothing though not
/// optional, or a file the walk loads twice (Targo's cycle check spans it, so a file two includes
/// name is one). So does a file the reader fails on, which it must where Targo finds one it loads
/// (a folder, a FIFO or a device included, [`unresolved_words`]): the walk reads each file through
/// the reader alone. Targo refuses all but the first and a FIFO's or device's, and builds nothing
/// then. A bounded iterative walk shares the CLI graph's read-attempt, aggregate-text and
/// pending-work limits; exceeding a limit names both.
fn config_dirs(dir: &Path, m: &Machine<'_>) -> Named {
    let mut named = Named::default();
    // Targo loads one file per folder (`get_file_path`): the legacy `config` wherever it opens,
    // and the `config.toml` beside it never. One this reader fails on names both, whichever.
    if !scan_config_graph_dirs(&dir.join("config"), m, &mut named) {
        scan_config_graph_dirs(&dir.join("config.toml"), m, &mut named);
    }
    named
}

/// Add the output markers of the one configuration graph rooted at `file` to `named`
/// ([`config_dirs`]) without recursive traversal; whether a file stands there, as the reader
/// says (one it fails on stands). A missing root names nothing; a missing include names nothing
/// where it is optional. Each root gets a fresh cycle set and resource budget; any incomplete
/// graph names both and needs no further reads.
fn scan_config_graph_dirs(file: &Path, m: &Machine<'_>, named: &mut Named) -> bool {
    let mut budget = ConfigGraphBudget::default();
    let mut seen = std::collections::BTreeSet::new();
    let mut pending = vec![(file.to_path_buf(), false)];
    let mut root = true;
    while let Some((file, optional)) = pending.pop() {
        let is_root = std::mem::replace(&mut root, false);
        if budget.attempt_read().is_err() {
            named.both();
            return true;
        }
        let text = match config_file(&file, m) {
            Ok(Some(text)) => text,
            Ok(None) if is_root => return false,
            Ok(None) if optional => continue,
            Ok(None) | Err(()) => {
                named.both();
                return true;
            }
        };
        // Every file a walk loads is one it has seen, as Targo's `_load_file` records it before
        // reading what it includes.
        if !seen.insert(file.clone()) || budget.retain_text(&text).is_err() {
            named.both();
            return true;
        }
        // A line that spells a directory's key is a reason to parse the file, never a reading of
        // it: Targo builds into what the `build` table names ([`parsed_dirs`]), and the line it
        // runs reads an alias's words itself ([`targo_line`]), so an alias that names a target
        // (`build-dev = "run --target-dir target/tools …"`) names none for another call.
        let literal = config_text_dirs(&text);
        let mentioned = literal.build || literal.target || literal.artifact;
        named.mentioned |= mentioned;
        let hidden = literal.include || spells_by_escape(&text) || holds_multiline_string(&text);
        if !mentioned && !hidden {
            continue;
        }
        let Some(config) = config_toml(&text) else {
            if hidden {
                named.both();
                return true;
            }
            // Text the reader cannot parse, which Targo refuses to load, whose lines tell: what
            // they spell, as before it was parsed.
            named.build |= literal.build;
            named.target |= literal.target;
            named.artifact |= literal.artifact;
            continue;
        };
        parsed_dirs(&config, named);
        let Ok(includes) = config_includes(&config) else {
            named.both();
            return true;
        };
        if pending.len().saturating_add(includes.len()) > CONFIG_GRAPH_MAX_PENDING {
            named.both();
            return true;
        }
        let base = file.parent().unwrap_or(Path::new("/"));
        for include in includes.iter().rev() {
            pending.push((include_file(base, include), include.optional));
        }
    }
    true
}

/// Whether something stands at `path` as Targo's `Path::exists` asks it, through its links: a
/// reader that says no file stands there, where one does, is not read as saying none.
fn stands(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// The directories parsed TOML names under `build` ([`Named`]). A `build` that is no table,
/// which Targo refuses, names both.
fn parsed_dirs(config: &aterm_toml::Value, named: &mut Named) {
    match config.get("build") {
        None => {}
        Some(aterm_toml::Value::Table(build)) => {
            named.build |= build.get("build-dir").is_some();
            named.target |= build.get("target-dir").is_some();
            named.artifact |= build.get("artifact-dir").is_some();
        }
        Some(_) => named.both(),
    }
}

/// Cargo config or manifest text parsed as Targo parses it (`load_layer`'s `toml` 1.1.5, the
/// `--config` value `toml_dotted_keys` reads with `toml_edit` 0.25, and a manifest's
/// `parse_document`, `toml` 1.1.5's `DeTable::parse`, all TOML 1.1 with `toml_datetime` 1.1.1's
/// times): every reader of a config Targo loads, or of a manifest on the way to its workspace
/// root (`WorkspaceConfig::read`, `member`), reads it with this one; `None` where it cannot.
/// `aterm_toml` folds a multi-line string's CRLF to LF where `toml` keeps it, so text holding a
/// carriage return whose value holds a string with a line feed is not read: the string may not be
/// the one Targo reads.
#[must_use]
pub fn config_toml(text: &str) -> Option<aterm_toml::Value> {
    let value: aterm_toml::Value = aterm_toml::from_str(text).ok()?;
    (!(text.contains('\r') && holds_line_feed(&value))).then_some(value)
}

/// Whether a string of `value` holds a line feed.
fn holds_line_feed(value: &aterm_toml::Value) -> bool {
    match value {
        aterm_toml::Value::String(text) => text.contains('\n'),
        aterm_toml::Value::Array(values) => values.iter().any(holds_line_feed),
        aterm_toml::Value::Table(table) => table.values().any(holds_line_feed),
        _ => false,
    }
}

/// One entry of a config's `include`, as Targo's `include_paths` reads it: its path as written,
/// and whether it is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    /// The path, relative to the including file's folder (or the working directory, for a
    /// `--config` value's) unless absolute.
    pub path: String,
    /// `optional = true`: skipped where nothing stands at the path.
    pub optional: bool,
}

/// The includes the parsed config `config` names, in order, as Targo's `include_paths` reads
/// them, checked whole before any is loaded; none where it has no `include`. Each entry is a
/// string or a table with a string `path` and, if any, a boolean `optional` (another key is
/// ignored, as Targo ignores it); its path ends in `.toml` and holds no glob character
/// (`*?[]`) and no brace.
///
/// # Errors
/// What Targo refuses: an `include` that is no list, an entry of another shape, and a path that
/// does not end in `.toml` or holds a glob character or a brace. Also a path holding a control
/// character, which Targo loads and this reader refuses, as it refuses a target holding one.
#[expect(
    clippy::result_unit_err,
    reason = "an include Targo refuses is one this reader cannot follow, whatever the shape"
)]
pub fn config_includes(config: &aterm_toml::Value) -> Result<Vec<Include>, ()> {
    let Some(include) = config.get("include") else {
        return Ok(Vec::new());
    };
    let items = include.as_array().ok_or(())?;
    items
        .iter()
        .map(|item| {
            let (path, optional) = match item {
                aterm_toml::Value::String(path) => (path, false),
                aterm_toml::Value::Table(table) => match (table.get("path"), table.get("optional"))
                {
                    (Some(aterm_toml::Value::String(path)), None) => (path, false),
                    (
                        Some(aterm_toml::Value::String(path)),
                        Some(aterm_toml::Value::Boolean(optional)),
                    ) => (path, *optional),
                    _ => return Err(()),
                },
                _ => return Err(()),
            };
            let refused = Path::new(path)
                .extension()
                .is_none_or(|extension| extension != "toml")
                || path.contains(['*', '?', '[', ']', '{', '}'])
                || path.chars().any(char::is_control);
            if refused {
                return Err(());
            }
            Ok(Include {
                path: path.clone(),
                optional,
            })
        })
        .collect()
}

/// The file `include` names, against `base` (the including file's folder, or the working
/// directory for a `--config` value's), its `.` and `..` folded as written, as Targo's
/// `ConfigInclude::resolve_path` folds it (`normalize_path`, a `..` past the root staying
/// there).
#[must_use]
pub fn include_file(base: &Path, include: &Include) -> PathBuf {
    lexical(&base.join(&include.path))
}

/// Whether TOML text holds a multi-line string (`"""` or `'''`), the one place a line that starts
/// with `#` is no comment: a line that closes the string there (`#""", target-dir = …` in an
/// inline table) goes on to name keys a line scan skips. Without one, every such line is a
/// comment (a lone CR, which would end one early, is a newline TOML refuses, and Targo with it).
fn holds_multiline_string(text: &str) -> bool {
    text.contains("\"\"\"") || text.contains("'''")
}

/// Whether TOML text holds an escape that can spell any character, and so a key no line spells
/// (`"target\u002ddir"`): `\u`, `\U` or TOML 1.1's `\x`. The others (`\"`, `\\`, TOML 1.1's
/// `\e`) spell no character a key's name needs.
#[must_use]
pub fn spells_by_escape(text: &str) -> bool {
    ["\\u", "\\U", "\\x"]
        .iter()
        .any(|escape| text.contains(escape))
}

/// The text of the config file at `file` as Targo reads it, through its links: the reader a
/// [`Machine`] carries may refuse a link (atpkg's does), so one is read at its canonical path,
/// and a link that names nothing is a file that does not stand (an optional include of it is
/// skipped, as Targo skips one whose path does not exist).
///
/// # Errors
/// A file that cannot be read, a file the reader fails on that is no such link among them.
#[must_use = "a config file's text is what it names"]
#[expect(
    clippy::result_unit_err,
    reason = "Machine::read's shape: a file that cannot be read is a choice, whatever the error"
)]
pub fn config_file(file: &Path, m: &Machine<'_>) -> Result<Option<String>, ()> {
    (m.read)(file).or_else(|()| match std::fs::canonicalize(file) {
        Ok(real) if real != file => (m.read)(&real),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata(file)
                    .is_ok_and(|meta| meta.file_type().is_symlink()) =>
        {
            Ok(None)
        }
        _ => Err(()),
    })
}

/// `path` with its `.` and `..` folded as written, as Targo's `normalize_path` folds an
/// include's path and its package home: a `..` past the root stays there, and one that leads a
/// relative path, or follows another such, is kept.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.ends_with(Component::ParentDir) || (!out.pop() && !out.has_root()) {
                    out.push(Component::ParentDir);
                }
            }
            component => out.push(component.as_os_str()),
        }
    }
    out
}

/// The value the line `x` gives the alias `verb` (`alias.<verb> = …`, or `<verb> = …` in an
/// `[alias]` table), as the shim's block reads it; `None` for a line that defines none.
fn alias_value<'x>(x: &'x str, verb: &str, in_alias: bool) -> Option<&'x str> {
    // An empty verb is no alias's: Targo runs it as no command, and the block's verb never is one.
    if verb.is_empty() {
        return None;
    }
    let rest = match x.strip_prefix("alias.").and_then(|r| r.strip_prefix(verb)) {
        Some(rest) => rest,
        None if in_alias => x.strip_prefix(verb)?,
        None => return None,
    };
    if !rest.starts_with([' ', '\t', '=']) {
        return None;
    }
    x.split_once('=').map(|(_, value)| value)
}

/// Whether an alias's value ([`alias_value`]) starts with a building verb.
fn alias_builds(value: &str) -> bool {
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

/// Whether any Cargo config Targo may read contains a non-comment line matching `names`: each
/// the walk reads ([`config_walk`]), then each package home's ([`package_homes`], whichever
/// home a Targo takes).
#[must_use]
pub fn configs_name(m: &Machine<'_>, names: fn(&str) -> bool) -> bool {
    let Some(cwd) = m.cwd else {
        return true;
    };
    config_walk(cwd, m)
        .into_iter()
        .chain(package_homes(Tool::Targo, m))
        .any(|dir| config_names(&dir, m, names))
}

/// Fallback `--config` value placing intermediates in `<target>/.ubd/build.noindex` ([`config_value`]).
#[must_use]
pub fn target_build_dir(target: &Path, m: &Machine<'_>) -> Option<String> {
    if !target.is_absolute()
        || (m.env)("CARGO_BUILD_BUILD_DIR").is_some()
        || configs_name(m, |line| line.contains("build-dir"))
    {
        return None;
    }
    target_build_config(target)
}

/// The generated target recipe, independent of mutable caller configuration. Native admission
/// recognizes its earlier selection before rechecking whether it remains eligible.
#[must_use]
pub fn target_build_config(target: &Path) -> Option<String> {
    if !target.is_absolute() {
        return None;
    }
    let dir = target_build_path(target);
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
    read_input(path, false)
}

/// Read the initial manifest selected by the driver, which tests existence before reading.
/// Unlike configuration discovery, an existing non-regular file cannot be skipped.
///
/// # Errors
/// An existing path is not a readable, bounded, regular UTF-8 manifest.
#[expect(
    clippy::result_unit_err,
    reason = "Machine's input observation distinguishes absence from refusal"
)]
pub fn read_manifest(path: &Path) -> Result<Option<String>, ()> {
    read_input(path, true)
}

fn read_input(path: &Path, manifest: bool) -> Result<Option<String>, ()> {
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
        Err(e) if !manifest && e.raw_os_error() == Some(libc::ENXIO) => return Ok(None),
        Err(_) => return Err(()),
    };
    let meta = match file.metadata() {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) if !manifest => return Ok(None),
        Ok(_) | Err(_) => return Err(()),
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
    if manifest {
        String::from_utf8(buf).map(Some).map_err(|_| ())
    } else {
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }
}

#[cfg(test)]
#[path = "build_cache_tests.rs"]
mod tests;
