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
    /// Whether this process may write the directory (`access(2)`).
    pub writable: &'a dyn Fn(&Path) -> Option<bool>,
}

/// Environment variable carrying automatic placement across external subcommand dispatch
/// ([`loses_config`]), which may drop the outer driver's global `--config` words, and to
/// `targo trust`.
pub const BUILD_DIR_ENV: &str = "CARGO_BUILD_BUILD_DIR";

/// [`BUILD_DIR_ENV`]'s `TARGO_` twin, which Targo reads where that is unset ([`cargo_var`]).
pub const BUILD_DIR_TWIN: &str = "TARGO_BUILD_BUILD_DIR";

/// The short options that take no value in Targo's commands that take `-m`: `-q`, `-v`, `-h`,
/// `-r` (release), `-n` (dry run), `-w` and `-b` (`update`'s workspace and breaking), `-l`
/// (`package`'s list) and `tree`'s `-a`, `-d` and `-V`. Every other letter takes a value (`-p`,
/// `-j`, `-F`, `-m`, `-Z`, the top-level `-C`; `tree`'s `-e`, `-i` and `-f`; `vendor`'s `-s`) or
/// is refused, so a cluster's run of flags ends there (`util/command_prelude.rs`, each command's
/// `cli`).
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

/// `word` read as clap reads a cluster of short options: a run of letters that take no value
/// ([`FLAG_SHORTS`]), then at most one letter that takes a value, which is the rest of the word
/// (one leading `=` dropped) or, when nothing is left, the next word. So `-qm X`, `-mX` and
/// `-m=X` each name the manifest `X`, while in `-pm` the `m` is `-p`'s value. Whether a value
/// left to the next word is that word is [`takes_next`]'s. `None` for a word that is no cluster:
/// a long option, `--`, `-` or a positional.
#[must_use]
pub fn shorts(word: &str) -> Option<Shorts<'_>> {
    let letters = word
        .strip_prefix('-')
        .filter(|rest| !rest.is_empty() && !rest.starts_with('-'))?;
    let run = letters
        .bytes()
        .take_while(|letter| FLAG_SHORTS.contains(letter))
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

/// The command line Targo runs for `args` (the words after an upstream cargo's `+toolchain`)
/// once it has expanded the alias that takes its verb ([`targo_reading`]).
#[must_use]
pub fn targo_line(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<Vec<String>> {
    targo_reading(args, tool, alias).map(|reading| reading.line)
}

/// What [`targo_reading`] reads of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The command line Targo runs.
    pub line: Vec<String>,
    /// Whether Targo answers the call with help or its version and builds nothing.
    pub query: bool,
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
/// a verb it hands back (`phase_cargo_miri`'s `Forward`: `run`, `r`, `test`, `t`, `nextest`):
/// `targo --unverified <verb>`, then, where it reads a help option ([`miri_help`]), every later
/// word as typed (`miri run demo -h` builds and runs `demo -h`); else `nextest`'s own verb, the
/// first `--target-dir` before `--` with `miri` joined to it (`get_target_dir`, which takes
/// Targo's own target where none is typed, left here to the line's readers), and every later
/// word but each `--target-dir` and its value before `--` (`ArgSplitFlagValue`), so the
/// directory comes before the program's words. The flags it adds besides (`--target`, a
/// runner's `--config`) name no directory.
fn miri_line(words: &[String]) -> Option<Vec<String>> {
    let verb = words
        .first()
        .filter(|verb| matches!(verb.as_str(), "run" | "r" | "test" | "t" | "nextest"))?;
    let mut line = vec!["--unverified".to_owned(), verb.clone()];
    let mut rest = words[1..].iter();
    if miri_help(words) {
        line.extend(rest.cloned());
        return Some(line);
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
    if let Some(target) = target {
        line.push("--target-dir".to_owned());
        line.push(
            Path::new(target)
                .join("miri")
                .to_string_lossy()
                .into_owned(),
        );
    }
    line.extend(kept);
    Some(line)
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

/// THE LINE TARGO RUNS for `args` (the words after an upstream cargo's `+toolchain`), once it has
/// expanded the alias that takes its verb (`cli.rs`'s `expand_aliases`, Trust f57dabe4ae), and
/// whether it answers the call without a build ([`reading`]). `None` where Targo refuses the
/// call or [`reading`] cannot tell its line ([`Outcome::Refused`], [`Outcome::Untold`]).
#[must_use]
pub fn targo_reading(
    args: &[String],
    tool: Tool,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<Reading> {
    match reading(args, tool, alias) {
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
    /// ([`UserAlias::Unknown`], an alias with none, or one past [`ALIAS_DEPTH`]); `globals` are
    /// the words Targo keeps before its name ([`kept_globals`]).
    Untold {
        line: Vec<String>,
        at: usize,
        globals: Vec<String>,
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
/// (`targo -- b -m x` builds `x`). After the name of a builtin command it runs itself
/// ([`exec_builtin`]) so handed it refuses any word (`-- build --target-dir t`), unless a user
/// alias of that name stands, which it ignores (`cli.rs`'s `expand_aliases`), running the
/// command with none of them; a protected tool's name so handed is an external command's, the
/// words its own (`-- clippy --target-dir t` builds into `t`). Targo changes into a `-C`
/// directory only from the words typed
/// ([`directory_arg`]) and takes `--unverified` only from them ([`consents`]), so their readers
/// read those.
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
/// ([`directory_marks`]), as Targo hands an external command its own words alone. The lint
/// ([`Tool::Tippy`])
/// has no verb: its line is its own, a query where it answers it ([`tippy_answers`]).
fn reading(args: &[String], tool: Tool, alias: &dyn Fn(&str) -> UserAlias) -> Outcome {
    let mut line = args.to_vec();
    if tool == Tool::Tippy {
        let query = tippy_answers(&line);
        return Outcome::Read(Reading { line, query });
    }
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
            Verb::Missing => return Outcome::Read(Reading { line, query: true }),
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
                let handed = (!query && verb == "miri")
                    .then(|| miri_line(&line[at + 1..]))
                    .flatten();
                let Some(handed) = handed else {
                    let query = query || tool_answers(verb, &line[at + 1..]);
                    return Outcome::Read(Reading { line, query });
                };
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
            return Outcome::Read(Reading { line, query });
        }
        let globals = typed.clone().unwrap_or_else(|| kept_globals(&line[..at]));
        let words = match alias(verb) {
            UserAlias::Words(words) if !words.is_empty() && depth < ALIAS_DEPTH => words,
            UserAlias::None => match builtin_alias(verb) {
                Some(command) => vec![command.to_owned()],
                None => {
                    let query = answered || z_help(&line[parsed..at]);
                    return Outcome::Read(Reading { line, query });
                }
            },
            UserAlias::Words(_) | UserAlias::Unknown => {
                return Outcome::Untold { line, at, globals };
            }
        };
        let mut next = globals.clone();
        next.extend(words);
        next.extend_from_slice(&line[at + 1..]);
        line = next;
        typed = Some(globals);
        depth += 1;
    }
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
) -> Option<Vec<Vec<String>>> {
    let Outcome::Untold { line, at, globals } = reading(args, tool, alias) else {
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

/// Whether `args` consent to the unverified lane as Targo reads them: an `--unverified` before
/// `--`, whatever word it follows (`cli.rs`'s `original_argv_has_unverified_before_delimiter`),
/// in the words typed, never an alias's, which Targo refuses to take one from.
#[must_use]
pub fn consents(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--unverified")
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
/// [`reads_twins`] and `CARGO_HOME` is unset, a non-empty `TARGO_HOME` too. The store's Targo
/// (Trust 29ebb403de) takes its home from `CARGO_HOME` alone, Trust's main from `TARGO_HOME`
/// instead of `~/.cargo` where `CARGO_HOME` is unset (`context/mod.rs`'s `homedir`), so a scan
/// that must see the config either may read reads both.
#[must_use]
pub fn package_homes(tool: Tool, m: &Machine<'_>) -> Vec<PathBuf> {
    let at = |value: OsString| {
        m.cwd
            .map_or_else(|| PathBuf::from(&value), |cwd| cwd.join(&value))
    };
    let cargo_home = (m.env)("CARGO_HOME");
    let twin = (cargo_home.is_none() && reads_twins(tool))
        .then(|| (m.env)("TARGO_HOME"))
        .flatten();
    cargo_home
        .filter(|value| !value.is_empty())
        .map(at)
        .or_else(|| m.home.map(|home| home.join(".cargo")))
        .into_iter()
        .chain(twin.filter(|value| !value.is_empty()).map(at))
        .collect()
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
/// refuses. The words before the verb are read as [`subcommand_at`] reads them.
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
                    dir = Some(value.or_else(|| it.next_if(|next| takes_next('C', next))));
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

/// The explicit outputs and manifest a managed build already chose. This is observation only:
/// creating or owning a directory belongs to the package manager, never to this resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitCache {
    pub target: PathBuf,
    pub manifest: PathBuf,
    /// A separately selected intermediate directory; both outputs require custody.
    pub intermediates: Option<PathBuf>,
}

/// Resolve an explicit target without moving a build or calling Cargo metadata, from the line
/// Targo runs ([`targo_reading`], `alias` saying what user alias takes a verb's name). A query,
/// `clean` (at the verb: `test clean` runs the tests named so), a line whose alias cannot be
/// told, an external command, a verb that builds nothing, unverified-lane refusals and
/// unresolved output configs grant no creation.
#[must_use]
pub fn explicit_cache(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<ExplicitCache> {
    let (line, target, intermediates) = cache_line(args, tool, m, alias)?;
    let cwd = m.cwd?;
    let target = target.or_else(|| configured_target(cwd, tool, m))?;
    if target.as_os_str().is_empty() {
        return None;
    }
    Some(ExplicitCache {
        target: absolute(target, cwd)?,
        manifest: cache_manifest(&line, m)?,
        intermediates,
    })
}

/// Manifest for the exact producer-selected local fallback. The registrar resolves its
/// workspace root before preparing `target`; caller-selected layouts grant no such choice.
#[must_use]
pub fn local_fallback_manifest(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<PathBuf> {
    let expected = target_build_path(Path::new("{workspace-root}/target"));
    let expected = expected.to_str()?;
    let selected = match decide(args, tool, m)? {
        BuildDir::Target(value) => directory_from_config(&value) == Some(expected),
        BuildDir::TrustEnv(value) => value == expected,
        BuildDir::Cache(_) => false,
    };
    if !selected {
        return None;
    }
    cache_manifest(&cache_line(args, tool, m, alias)?.0, m)
}

fn cache_line(
    args: &[String],
    tool: Tool,
    m: &Machine<'_>,
    alias: &dyn Fn(&str) -> UserAlias,
) -> Option<(Line, Option<PathBuf>, Option<PathBuf>)> {
    let home = m.home?;
    let cwd = m.cwd?;
    if !home.is_absolute() || !cwd.is_absolute() || !cwd.starts_with(home) {
        return None;
    }
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|a| a.starts_with('+')));
    let reading = targo_reading(&args[at..], tool, alias)?;
    if reading.query {
        return None;
    }
    let words = reading.line;
    let root = m.root.map_or_else(|| cache_root(home), Path::to_path_buf);
    let line = Line::parse(&words, tool, home, &root, consents(&args[at..]))?;
    if line.alias.is_some() || line.verb_at.is_some_and(|verb| words[verb] == "clean") {
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
    let intermediates = match intermediates {
        Some(path) => {
            // The proof frontend's separate output grammar, an unknown upstream
            // capability and Cargo's template expansion grant no writable root here.
            if line.trust
                || tool == Tool::Cargo
                || path.as_os_str().is_empty()
                || path.as_os_str().to_str()?.contains(['{', '}'])
            {
                return None;
            }
            Some(absolute(path, cwd)?)
        }
        None => None,
    };
    // Discovered build-directory graphs remain unresolved here; they need their own
    // definition provenance before either output can grant custody.
    if cwd
        .ancestors()
        .map(|p| p.join(".cargo"))
        .chain(package_homes(tool, m))
        .any(|dir| config_dirs(&dir, m).build)
    {
        return None;
    }
    Some((line, target, intermediates))
}

/// Read CLI config outputs with their definition's path bases. Cargo asks whether the path
/// exists before interpreting inline TOML, even when its name contains `=`. Unreadable or
/// unsupported input stays unknown; this observer never changes compiler argv.
fn cache_config_outputs(
    value: &str,
    m: &Machine<'_>,
    select_target: bool,
    follow_includes: bool,
    file_seen: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<ConfigOutputs, ()> {
    let cwd = m.cwd.ok_or(())?;
    let file = cwd.join(value);
    let (text, from_file) = match config_file(&file, m)? {
        Some(text) => (text, true),
        None if value.contains('=') => (value.to_owned(), false),
        None => return Err(()),
    };
    config_graph_outputs(
        text,
        from_file.then_some(file),
        cwd,
        m,
        select_target,
        follow_includes,
        file_seen,
    )
}

enum ConfigStep {
    Load(PathBuf, bool),
    Layer(String, Option<PathBuf>),
    Outputs(ConfigOutputs),
}

#[derive(Default)]
struct ConfigOutputs {
    target: Option<ConfiguredPath>,
    intermediates: Option<ConfiguredPath>,
}

/// Resolve output settings in Cargo's merge order without recursive traversal. Includes use
/// the declaring file's parent (cwd for inline values); each output keeps its definition base.
/// Repeated paths and unreadable input grant no custody. Each selected output retains its base.
fn config_graph_outputs(
    text: String,
    file: Option<PathBuf>,
    cwd: &Path,
    m: &Machine<'_>,
    select_target: bool,
    follow_includes: bool,
    file_seen: &mut std::collections::BTreeSet<PathBuf>,
) -> Result<ConfigOutputs, ()> {
    // Bound adversarial acyclic graphs as well as cycles. Optional missing files consume
    // attempts, and aggregate text is checked before retaining another layer.
    const MAX_READS: usize = 1024;
    const MAX_BYTES: usize = 16 * 1024 * 1024;
    const MAX_PENDING: usize = 2 * MAX_READS;
    let mut text_bytes = text.len();
    if text_bytes > MAX_BYTES {
        return Err(());
    }
    let mut reads = usize::from(file.is_some());
    let mut inline_seen = std::collections::BTreeSet::new();
    // CLI file arguments share Targo's seen set; inline graphs each get a fresh one.
    // Supplied root paths retain their spelling, while included paths are normalized.
    let seen = if let Some(file) = &file {
        if file_seen.len() >= MAX_READS || !file_seen.insert(file.clone()) {
            return Err(());
        }
        file_seen
    } else {
        &mut inline_seen
    };
    let mut pending = vec![ConfigStep::Layer(text, file)];
    let mut selected = ConfigOutputs::default();
    while let Some(step) = pending.pop() {
        match step {
            ConfigStep::Outputs(outputs) => {
                if outputs.target.is_some() {
                    selected.target = outputs.target;
                }
                if outputs.intermediates.is_some() {
                    selected.intermediates = outputs.intermediates;
                }
            }
            ConfigStep::Load(file, optional) => {
                if reads >= MAX_READS || seen.contains(&file) {
                    return Err(());
                }
                reads += 1;
                match config_file(&file, m)? {
                    None if optional => {}
                    Some(text) if seen.len() < MAX_READS && seen.insert(file.clone()) => {
                        text_bytes = text_bytes.checked_add(text.len()).ok_or(())?;
                        if text_bytes > MAX_BYTES {
                            return Err(());
                        }
                        pending.push(ConfigStep::Layer(text, Some(file)))
                    }
                    _ => return Err(()),
                }
            }
            ConfigStep::Layer(text, file) => {
                let config: aterm_toml::Value = aterm_toml::from_str(&text).map_err(|_| ())?;
                let target = config_layer_path(&config, "target-dir")?.filter(|_| select_target);
                let intermediates = config_layer_path(&config, "build-dir")?;
                if target.is_some() || intermediates.is_some() {
                    let base = match &file {
                        Some(file) => file
                            .parent()
                            .and_then(Path::parent)
                            .ok_or(())?
                            .to_path_buf(),
                        None => cwd.to_path_buf(),
                    };
                    pending.push(ConfigStep::Outputs(ConfigOutputs {
                        target: target.map(|value| ConfiguredPath {
                            base: base.clone(),
                            value,
                        }),
                        intermediates: intermediates.map(|value| ConfiguredPath { base, value }),
                    }));
                }
                let base = file.as_deref().and_then(Path::parent).unwrap_or(cwd);
                let includes = config_includes(&config)?;
                // Generic upstream Cargo carries no include-version capability proof, as
                // with alias discovery. Known Trust fronts may follow nonempty graphs.
                if !follow_includes && !includes.is_empty() {
                    return Err(());
                }
                for (path, optional) in includes.into_iter().rev() {
                    pending.push(ConfigStep::Load(lexical(&base.join(path)), optional));
                }
            }
        }
        if pending.len() > MAX_PENDING {
            return Err(());
        }
    }
    Ok(selected)
}

/// Decode one output key. Malformed tables and unsupported values remain unresolved;
/// the graph resolver handles includes and records each output's definition base.
fn config_layer_path(config: &aterm_toml::Value, field: &str) -> Result<Option<String>, ()> {
    let Some(build) = config.get("build") else {
        return Ok(None);
    };
    if build.as_table().is_none() {
        return Err(());
    }
    build
        .get(field)
        .map(|target| {
            target
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or(())
        })
        .transpose()
}

/// Cargo's string/table include entries, shared by custody and inherited config scans.
fn config_includes(config: &aterm_toml::Value) -> Result<Vec<(&str, bool)>, ()> {
    let Some(include) = config.get("include") else {
        return Ok(Vec::new());
    };
    include
        .as_array()
        .ok_or(())?
        .iter()
        .map(|item| {
            let (path, optional) = match item {
                aterm_toml::Value::String(path) => (path.as_str(), false),
                aterm_toml::Value::Table(table) => match (table.get("path"), table.get("optional"))
                {
                    (Some(aterm_toml::Value::String(path)), None) => (path.as_str(), false),
                    (
                        Some(aterm_toml::Value::String(path)),
                        Some(aterm_toml::Value::Boolean(optional)),
                    ) => (path.as_str(), *optional),
                    _ => return Err(()),
                },
                _ => return Err(()),
            };
            if Path::new(path)
                .extension()
                .is_none_or(|extension| extension != "toml")
                || path.contains(['*', '?', '[', ']', '{', '}'])
            {
                return Err(());
            }
            Ok((path, optional))
        })
        .collect()
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

fn cache_manifest(line: &Line, m: &Machine<'_>) -> Option<PathBuf> {
    let (home, cwd) = (m.home?, m.cwd?);
    Some(match line.manifest_path.as_deref() {
        Some(path) => absolute(PathBuf::from(path), cwd)?,
        None => cwd
            .ancestors()
            .take_while(|p| p.starts_with(home) && *p != home)
            .take(32)
            .map(|p| p.join("Cargo.toml"))
            .find(|p| (m.read)(p).is_ok_and(|text| text.is_some()))?,
    })
}

/// Cargo resolves a configured path against the directory containing `.cargo`, with the
/// nearest configuration winning. The legacy `config` wins when both spellings exist.
/// Parse TOML rather than mistaking a comment or an alias argument for a target selection.
/// The home read is the first of [`package_homes`]: `CARGO_HOME`, or `~/.cargo`, which the walk
/// already passes as the `.cargo` of an ancestor of every working directory under HOME. A
/// `TARGO_HOME` after it can name a target only where none of those does, one the store's Targo
/// would not build into, so there is none then (and [`configs_unresolved`] owes the call
/// custody).
fn configured_target(cwd: &Path, tool: Tool, m: &Machine<'_>) -> Option<PathBuf> {
    let homes = package_homes(tool, m);
    read_configured_target(cwd, homes.first().map(PathBuf::as_path), m.read)
        .ok()
        .flatten()
}

/// Cargo's selected final target, without inventing a workspace's default directory.
/// Environment paths are relative to `cwd`; configuration paths are relative to the
/// directory containing `.cargo`. A caller with no explicit selection chooses its own
/// workspace default. This reads configuration only and creates nothing.
///
/// # Errors
/// A selected target is empty, a Cargo configuration cannot be read or parsed, or its
/// unresolved `include` could select a target.
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
    let target = read_configured_target(cwd, cargo_home.as_deref(), m.read)?;
    if !reads_twins(tool) {
        return Ok(target);
    }
    for other in package_homes(tool, m) {
        if Some(&other) == cargo_home.as_ref() {
            continue;
        }
        if read_configured_target(cwd, Some(&other), m.read)? != target {
            return Err("supported Targo package homes select different target directories".into());
        }
    }
    Ok(target)
}

/// Cargo's home from a producer snapshot, relative to its actual working directory.
///
/// # Errors
/// The machine has no working directory.
pub fn cargo_home_hint_with(m: &Machine<'_>) -> Result<Option<PathBuf>, String> {
    let cwd = m.cwd.ok_or("Cargo home lookup has no working directory")?;
    Ok((m.env)("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .or_else(|| {
            m.home
                .filter(|home| !home.as_os_str().is_empty())
                .map(|home| home.join(".cargo"))
        }))
}

fn read_configured_target(
    cwd: &Path,
    cargo_home: Option<&Path>,
    read: &dyn Fn(&Path) -> Result<Option<String>, ()>,
) -> Result<Option<PathBuf>, String> {
    Ok(read_configured_path(cwd, cargo_home, read, "target-dir")?
        .map(|selected| selected.base.join(selected.value)))
}

struct ConfiguredPath {
    base: PathBuf,
    value: String,
}

fn read_configured_path(
    cwd: &Path,
    cargo_home: Option<&Path>,
    read: &dyn Fn(&Path) -> Result<Option<String>, ()>,
    field: &str,
) -> Result<Option<ConfiguredPath>, String> {
    for dir in cwd
        .ancestors()
        .map(|p| p.join(".cargo"))
        .chain(cargo_home.map(Path::to_path_buf))
    {
        let read = |path: &Path| read(path).map_err(|()| format!("read {}", path.display()));
        let text = match read(&dir.join("config"))? {
            Some(text) => Some(text),
            None => read(&dir.join("config.toml"))?,
        };
        let Some(text) = text else { continue };
        let config: aterm_toml::Value = aterm_toml::from_str(&text)
            .map_err(|_| format!("parse Cargo configuration in {}", dir.display()))?;
        if config
            .get("build")
            .is_some_and(|build| build.as_table().is_none())
        {
            return Err(format!(
                "Cargo build configuration in {} is not a table",
                dir.display()
            ));
        }
        if let Some(selected) = config.get("build").and_then(|build| build.get(field)) {
            let value = selected
                .as_str()
                .ok_or_else(|| format!("Cargo {field} in {} is not a string", dir.display()))?;
            if value.is_empty() {
                return Err(format!("Cargo {field} in {} is empty", dir.display()));
            }
            return Ok(Some(ConfiguredPath {
                base: dir
                    .parent()
                    .ok_or("Cargo configuration has no parent")?
                    .to_path_buf(),
                value: value.to_owned(),
            }));
        }
        if config.get("include").is_some() {
            return Err(format!(
                "Cargo {field} lookup cannot resolve includes in {}",
                dir.display()
            ));
        }
    }
    Ok(None)
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

/// Capture the caller's intermediate directory using Cargo environment/config precedence.
/// Config-relative paths retain their original base when forwarded through the environment.
/// Valid templates remain Cargo's to expand; this grants no directory or cleanup authority.
///
/// # Errors
/// The selection is empty, malformed or unreadable, or an unresolved include could select it.
pub fn cargo_build_dir_hint_with(m: &Machine<'_>) -> Result<Option<CargoBuildDirHint>, String> {
    let cwd = m
        .cwd
        .ok_or("Cargo build-directory lookup has no working directory")?;
    let selected = match (m.env)(BUILD_DIR_ENV) {
        Some(value) => ConfiguredPath {
            base: cwd.to_path_buf(),
            value: value
                .into_string()
                .map_err(|_| "Cargo build-dir is not UTF-8")?,
        },
        None => {
            let home = cargo_home_hint_with(m)?;
            let Some(selected) = read_configured_path(cwd, home.as_deref(), m.read, "build-dir")?
            else {
                return Ok(None);
            };
            selected
        }
    };
    if selected.value.is_empty() {
        return Err("Cargo build-dir is empty".into());
    }
    let mut rest = selected.value.as_str();
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
    let named = if selected.value.starts_with("{workspace-root}")
        || selected.value.starts_with("{cargo-cache-home}")
    {
        PathBuf::from(&selected.value)
    } else {
        selected.base.join(&selected.value)
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
            m.home.is_none_or(|home| !cwd.starts_with(home)) || configs_unresolved(cwd, tool, m)
        })
}

/// [`unresolved_directory`]'s reading of `args` and the environment: a build directory there, a
/// `-C`, or a `--config` value that names a file or may name a directory, each file read against
/// the directory Targo works in (`m.cwd`).
#[must_use]
pub fn unresolved_words(args: &[String], tool: Tool, m: &Machine<'_>) -> bool {
    if cargo_var(BUILD_DIR_ENV, tool, m).is_some() {
        return true;
    }
    let mut words = args[..own_words(args, tool)].iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if shorts(word).is_some_and(|cluster| cluster.value_of('C').is_some()) {
            return true;
        }
        let config = if word == "--config" {
            words.next()
        } else {
            word.strip_prefix("--config=")
        };
        // A value is a config file's path where something stands there, whatever it spells
        // (`context/mod.rs`'s `cli_args_as_table` asks the path first), else a dotted key.
        let names_a_file = |value: &str| {
            m.cwd
                .is_none_or(|cwd| !matches!((m.read)(&cwd.join(value)), Ok(None)))
        };
        if config
            .is_some_and(|value| !value.contains('=') || names_a_dir(value) || names_a_file(value))
        {
            return true;
        }
    }
    false
}

/// [`unresolved_directory`]'s reading of the configs Targo reads walking up from `root`
/// (`walk_tree`: each parent's `.cargo`, `..` walked as written, then the package homes): whether
/// one names a target or build directory, or one it cannot tell. A working directory outside HOME
/// [`unresolved_directory`] takes for one that may.
#[must_use]
pub fn configs_unresolved(root: &Path, tool: Tool, m: &Machine<'_>) -> bool {
    root.ancestors()
        .map(|path| path.join(".cargo"))
        .chain(package_homes(tool, m))
        .any(|dir| {
            let named = config_dirs(&dir, m);
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
    if cargo_var(BUILD_DIR_ENV, tool, m).is_some() {
        return None;
    }
    let at = usize::from(tool.is_cargo() && args.first().is_some_and(|a| a.starts_with('+')));
    // Placement reads no user alias's words, as the shim reads none: a builtin alias's `-m` is
    // read even where one may take its name, which can only leave the call as typed.
    let mut line = Line::read(&args[at..], tool, home, &root, consents(&args[at..]))?;
    let mut target_named = false;
    let mut alias_builds = false;
    let mut configured = false;
    let query_alias = line
        .verb_at
        .and_then(|verb| args.get(at + verb))
        .filter(|verb| !builtin_command(verb, tool));
    for dir in cwd
        .ancestors()
        .map(|d| d.join(".cargo"))
        .chain(package_homes(tool, m))
    {
        let named = config_scan(
            &dir,
            line.alias.as_deref().or(query_alias.map(String::as_str)),
            m,
        );
        if named.build || named.include {
            return None;
        }
        target_named |= named.target;
        alias_builds |= named.alias_builds;
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
        .or_else(|| (m.writable)(&home.join("Library/Caches")))
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
}

impl Line {
    /// A building verb, or one an alias may make one, `unverified` whether the words typed
    /// consent to the lane ([`consents`]).
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
            .all(|v| v.contains('=') && !names_a_dir(v))
            .then_some(line)
    }

    /// Parse argument roles before applying output policy. Placement retains its lexical
    /// config rule; custody judges config against the fully selected explicit target.
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
                            // Another directory, in a cluster too (`-qC x`), leaves the call as
                            // typed.
                            Some(('C', _)) => false,
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

/// Whether a `--config` value may name a target or build directory: one that spells either, an
/// `include`, whose file Targo loads as a config of its own (`cli_args_as_table`), or a
/// backslash, through which a quoted key's escape may spell any of them
/// (`build."target\u002ddir"`). The shim's block reads the same words.
fn names_a_dir(s: &str) -> bool {
    s.contains("target-dir") || s.contains("build-dir") || s.contains("include") || s.contains('\\')
}

/// What `dir/config.toml` or `dir/config` says on lines that are not comments.
#[derive(Default)]
struct Named {
    /// A config exists, so a builtin alias cannot be assumed unconfigured.
    present: bool,
    build: bool,
    target: bool,
    alias_builds: bool,
    /// A line spells `include`: Targo loads the files it names as part of the config
    /// ([`config_dirs`] reads them), which the shim's block cannot, so placement leaves the
    /// call as typed.
    include: bool,
}

/// [`Named`] for the config files in `dir`, `alias` the verb to look up.
fn config_scan(dir: &Path, alias: Option<&str>, m: &Machine<'_>) -> Named {
    let mut named = Named::default();
    for name in ["config.toml", "config"] {
        match (m.read)(&dir.join(name)) {
            Err(()) => {
                return Named {
                    present: true,
                    build: true,
                    target: true,
                    alias_builds: false,
                    include: false,
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
                    named.include |= line.contains("include");
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

/// The directories the config files in `dir` name as Targo loads them (`context/mod.rs`'s
/// `_load_file`), each with the files its `include` names: [`config_scan`]'s reading of each
/// file's lines, the files it includes read the same way, each through its links as Targo reads
/// it ([`config_file`]), and a file whose text holds a backslash read as TOML as well, since a
/// quoted key's escape may spell `include`, `target-dir` or `build-dir` where no line does. Each config file a walk finds starts
/// its own cycle check (`load_file`'s fresh set), shared by the files it includes. An include
/// this reader cannot follow names both: text it cannot parse, an `include` that is no list, an
/// entry of another type, a path that is no `.toml` or holds a glob or a brace, one that names
/// nothing though not optional, or a file the walk loads twice (Targo's cycle check spans it,
/// so a file two includes name is one). Targo refuses all but the first, and builds nothing
/// then. The cycle check bounds the walk as it bounds Targo's: each file loads once, and a path
/// that loops through a link stops at the kernel's link limit, a read this reader cannot make.
fn config_dirs(dir: &Path, m: &Machine<'_>) -> Named {
    let mut named = Named::default();
    for name in ["config.toml", "config"] {
        let _ = included_dirs(&dir.join(name), m, &mut Vec::new(), &mut named);
    }
    named
}

/// [`config_dirs`] for the one file `file`, its findings added to `named`; `None` where nothing
/// stands at `file`.
fn included_dirs(
    file: &Path,
    m: &Machine<'_>,
    seen: &mut Vec<PathBuf>,
    named: &mut Named,
) -> Option<()> {
    let both = |named: &mut Named| {
        named.build = true;
        named.target = true;
    };
    let text = match config_file(file, m) {
        Err(()) => {
            both(named);
            return Some(());
        }
        Ok(None) => return None,
        Ok(Some(text)) => text,
    };
    // Every file a walk loads is one it has seen, as Targo's `_load_file` records it before
    // reading what it includes.
    if seen.iter().any(|loaded| loaded == file) {
        both(named);
        return Some(());
    }
    seen.push(file.to_path_buf());
    let escaped = spells_by_escape(&text);
    let mut includes = false;
    for line in text.lines() {
        if line.trim_start_matches([' ', '\t']).starts_with('#') {
            continue;
        }
        named.build |= line.contains("build-dir");
        named.target |= line.contains("target-dir");
        includes |= line.contains("include");
    }
    if !includes && !escaped {
        return Some(());
    }
    let Ok(config) = aterm_toml::from_str::<aterm_toml::Value>(&text) else {
        both(named);
        return Some(());
    };
    let build = config.get("build");
    named.build |= build.and_then(|build| build.get("build-dir")).is_some();
    named.target |= build.and_then(|build| build.get("target-dir")).is_some();
    let Ok(includes) = config_includes(&config) else {
        both(named);
        return Some(());
    };
    for (path, optional) in includes {
        let full = lexical(&file.parent().unwrap_or(Path::new("/")).join(path));
        if included_dirs(&full, m, seen, named).is_none() && !optional {
            both(named);
        }
    }
    Some(())
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
/// include's path.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
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
#[path = "build_cache_tests.rs"]
mod tests;
