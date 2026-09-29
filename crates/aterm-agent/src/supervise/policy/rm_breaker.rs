// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Is the `rm` behind Claude Code's "Dangerous rm operation on
//! possibly-empty variable path" box safe to wave through? The resolver the
//! approval policy's `rm-breaker` rule asks ([`resolve_rm_line`]).
//!
//! **Why a resolver of its own.** The vendor draws that box in a
//! bypass-permissions session because an `rm` operand holds a `$VAR` it cannot
//! prove non-empty. In bypass the rest of the line would run unasked anyway,
//! so the one question is where each `rm` operand POINTS — never whether the
//! rest of the line is a read (the hook-era `rm_policy`, which asked that
//! and abstained on every `$`, was deleted 2026-09-25 with the hook bridge;
//! its critical-path table and its flag table live on here as
//! [`critical_path`] and [`is_known_flag`]).
//!
//! **When it decides a press.** Under `approve = "safe"`, and under the
//! retired `approve_all`, `read_outside_cwd` or `trust_dialog = false`
//! (each caps the level at `safe`): a line it proves is pressed, one it
//! does not is a person's. Under full power (`approve = "all"`, the
//! default) every rm breaker is pressed anyway, and this resolver only
//! names the rule in the ledger (`rm-breaker@…`, or `allow-once@v1` with
//! its reason as `unproven`). Under the retired `rm_breaker = false` it
//! decides nothing: the loop hands it no scratch root, and with none it
//! proves nothing at all — not even a directory the line's own `mktemp -d`
//! made ([`RmScope::roots`]).
//!
//! **What resolves.** The line is read left to right as straight-line shell,
//! with a variable table filled by literal assignments only:
//! `NAME=/abs/lit`, `NAME=$OTHER/lit` (OTHER already resolved),
//! `NAME=$(mktemp -d /abs/dir/name.XXXX)` (a fresh directory under the
//! literal template's directory). A variable the line does NOT assign is
//! not known ("Variables from the environment" below). An assignment counts
//! only where it surely runs, in this shell: first in its
//! `&&`/`||` list, not piped, and not in a list that ends in `&` (`test -d x
//! || S=/y` leaves `S` unknown; so does `S=/y rm …`, whose assignment the
//! same command's expansion never sees, and `S=/y && true &`, which bash
//! runs in a background subshell — measured). `export` expands all its
//! words before it assigns any (`export S=/x T=$S/b` makes `T` `/b`,
//! measured, both shells). A command with an assignment in front of it
//! guards nothing (`${D:?}`), and the names it assigns are unknown after
//! it: zsh expands `export`'s own words with the prefix in force (`S=/etc
//! export T=$S` sets `T=/etc`) and bash keeps it (`S=/etc export S` leaves
//! `S=/etc`) — measured.
//!
//! **A `$(mktemp -d)` that fails** leaves its variable EMPTY (measured, zsh
//! 5.9 and bash 3.2: `D=$(mktemp -d /no/such/w.XXXX)` exits 1 with `D=""`,
//! and `"$D/etc"` is then `/etc`). So an operand built on one is proven only
//! where the rm cannot run with it empty: behind `&&` (or `|`) after the
//! assignment — whose status IS the substitution's when it is the command's
//! only one (`D=$(mktemp -d) E=$(true)` exits 0) — or after `D=$(…) ||
//! exit`, or `test -n "$D" &&` / `[ -d "$D" ] &&`; or where its variable is
//! written `${D:?}` (a non-interactive shell EXITS on an empty one: measured,
//! both shells, inside `eval` too — `${D?}` does not, it tests only unset),
//! in the rm's own words or in a command that surely ran before it (`:
//! "${D:?}"`). An operand that is the variable alone (`"$D"`) is `""` when
//! it fails, which removes nothing.
//!
//! **A directory the line itself just made** ([`Eval::fresh_dir`]; the
//! coordinator's ruling of 2026-09-26). `mktemp -d` makes a NEW directory
//! under a unique name, and fails rather than reuse one, so nothing in it
//! predates the line: removing it, or anything under it, removes nothing
//! else — wherever `$TMPDIR` points, which is why no root is asked for. So
//! an operand is proven when it is `"$D"`, or lies lexically under it
//! (`"$D/x"`, `"$D"/x/y`: no `..`, no glob, no other variable), where `D`
//! holds exactly the output of one `$(mktemp -d …)` on this line (any
//! template: bare, `-t p`, relative, absolute), the rm runs only if that
//! run succeeded (the guards above) — or the operand is `"$D"` alone, which
//! is `""` when it failed and removes nothing —, and the operand is
//! double-quoted. A path UNDER `"$D"` asks, too, that EVERY use of `$D` on
//! the line be double-quoted — the directory's name is under `$TMPDIR`,
//! which may hold a space or a glob, and an unquoted use would split or glob
//! it; `"$D"` alone is the directory or `""` whatever another command did
//! with an unquoted `$D`, which changes no word of the rm's. Everything else about `$TMPDIR` stays unprovable. The rule proves
//! nothing while the rm rule is withheld (no scratch root at all:
//! [`RmScope::roots`]; owner decision D9).
//!
//! **What escalates** — everything else, by construction: a loop or any other
//! compound command, `set`/`shift`/`eval`/`read`/`local` and the other
//! builtins that change a variable, a subshell or group, a backtick, a
//! here-document, `$'…'`, a `${…}` form other than `${NAME}` and
//! `${NAME:?…}`, a positional parameter, a variable the line does not
//! assign a literal (`$HOME`, `$TMPDIR`, `$USER`, any other), `~`, a
//! relative operand and `$PWD`, an `rm` word anywhere but
//! at a command head (`xargs rm`, `sh -c 'rm …'`) — in any letter case,
//! since the default macOS volume finds `/bin/rm` for `Rm` — an unknown `rm`
//! flag, a redirect on the `rm` other than `/dev/null` or a descriptor.
//!
//! **zsh**, whose reading the Bash tool gets on macOS (measured, zsh 5.9;
//! each was read as text before): `$=V` word-splits (`/tmp/x/$=S` with
//! `S='y /etc'` is `/tmp/x/y` and `/etc`); `$~V`, `$^V`, `$+V`, a modifier on
//! a bare name (`"$S:h:h"` is `/tmp` for `S=/tmp/w/a`) and a name in
//! non-ASCII letters (`é=/x` assigns, `$é` expands) are not computed; a
//! subscript on a bare name (`$S[1,0]` is empty) and `$[…]` are arithmetic,
//! which assigns (`$A[S=0]`, `$[S=5]`), and escalate wherever they are. A
//! `:` after a bare name is a modifier only before a modifier letter
//! (`"$S: x"` is text in both shells, measured). A command this resolver
//! does not model may assign a variable it NAMES — zsh's `print -v S /etc`,
//! `print -vS`, bash's `printf -vS`, `zparseopts a:=S`, `zstyle -s c s S`,
//! `zformat -f S /etc`, zsh/stat's `stat -A S`, and arithmetic, which zsh
//! evaluates in `[ -t "1/(S=5)" ]`, in `printf %d "S=5"` (and `printf -%d
//! S=5`: zsh's `printf` takes any `-` word but `-v` and `--` as its format)
//! and in `return S=5` (measured) — so each variable a command's words name is unknown
//! after it ([`Eval::forget_named`]: a lone option cluster names only what
//! is glued after its first letter, a path word with no arithmetic operator
//! names nothing). zsh's arithmetic reads a variable's VALUE as arithmetic
//! in turn (`A=S=5; printf %d A` sets `S`, measured), so a named variable's
//! value names what it names; and under a command that evaluates a word as
//! arithmetic ([`ARITH_HEADS`]) a name the line did not assign — whose value
//! is the environment's — leaves every variable unknown. Every variable is
//! unknown after a word whose text is not known, unless the word is
//! double-quoted and the command is one that takes no word as a name or as
//! arithmetic ([`PLAIN_CONSUMERS`]) or it is data past `printf`'s
//! as-written format that reads no arithmetic (`printf '%s\n' "$x"`), or a
//! string or file operand of `test`/`[` (`[ -n "$x" ]`, `[ "$x" = y ]`;
//! not `-t`'s or `-v`'s, [`test_operands`]): an
//! unquoted unknown word, a zsh form, or a value with a glob can run code
//! in the current shell under `GLOB_SUBST` (`X='/tmp/*(e:S=/etc:)'; echo
//! $~X` sets `S`, measured). The words of a
//! [`PLAIN_CONSUMERS`] command name nothing (`echo "OUT=$OUT"` leaves `OUT`
//! known): none of them assigns a variable, in either shell — but for zsh's
//! `{S}>file`, which stores a descriptor's number in `S` under any builtin
//! (measured), so an unquoted `{NAME}` word forgets `NAME` under every
//! command. The positions it reads are the words as WRITTEN, and a word
//! the shell brace-expands moves them (`A=S=5; printf {%d,A}` is zsh's
//! `printf %d A`, `printf {-vS,%s} /etc` bash's `printf -vS %s /etc`,
//! measured), and so does a glob, which becomes the names in the working
//! directory (`printf %d *` with a file `S=5` there sets `S` in zsh, and
//! under `EXTENDED_GLOB`, which a user's `setopt` carries into Claude Code's
//! shell snapshot (2.1.284 turns it off again before every command; this
//! reading predates that, and costs only escalations), `^`, `#`, `~` and `(`
//! glob too: `[ ^-vS ]` beside files
//! `-t`, `-vS` and `S=5` is `[ -t S=5 ]` — measured, [`GLOB_CHARS`]);
//! under a command not in [`PLAIN_CONSUMERS`] such a word
//! leaves every variable unknown — unless it is data past `printf`'s
//! as-written format that reads no arithmetic, or it expands to absolute
//! paths only (it starts with `/`), which are no option, name or
//! arithmetic ([`expands_apart`]).
//!
//! **Names the shell keeps.** `$_` is the previous command's last argument
//! (EMPTY after `_=/tmp/x`, measured), and a name the shell itself sets,
//! ties or reads (`_`, `PWD` which `cd` rewrites, `HOME`, `IFS`,
//! `PATH`/`path`, `status` — read-only in zsh —, `SECONDS`, `LINES`, …: every
//! name whose assignment does not read back in zsh 5.9 or bash 3.2, measured,
//! and kept so by a test) is never assigned by a line this resolver follows; nor is a variable appended to
//! (`S+=/..`) or assigned through a subscript (`S[0]=/etc` is `S=/etc` in
//! bash, `S[1,-1]=/etc` in zsh). An unquoted `~` or `=` that starts an
//! assignment's value or follows a `:` in it is expanded (both shells:
//! `S=/a:~/b` holds the home directory; zsh: `S==ls` is `/bin/ls`), so such
//! a value is unknown.
//!
//! **Variables from the environment.** A variable the line does not assign
//! is not proven — `$HOME`, `$TMPDIR`, `$USER`, any name. The Bash tool's
//! shell inherits Claude Code's LIVE environment, which its settings `env`
//! rewrites after it starts, into which it sets names of its own for the
//! tool (a sandboxed command's `TMPDIR`), and to which the user's shell
//! startup (`~/.zshenv`) and a SessionStart hook's `CLAUDE_ENV_FILE` add:
//! no process the supervisor can read holds it. (The coordinator's ruling of
//! 2026-09-26, after a reader of Claude Code's `exec` environment was shown
//! unsound by design; the worker-environment reader the git rule uses,
//! [`super::git_config`], is not asked here for the same reason.) What such
//! a variable is proven for is nothing; what a `$(mktemp -d)`, which makes
//! its directory under `$TMPDIR`, is proven for is "A directory the line
//! itself just made". `$TMPDIR` as the SUPERVISOR has it is still one of
//! the scratch roots a literal path may sit in ([`super::approval`]'s
//! `ApprovalCtx::new`): a root, never a value.
//!
//! **The shell's startup** (scope; audit §5, measured on Claude Code
//! 2.1.284, 2026-09-28). The resolver models zsh 5.9 and bash 3.2 under
//! their DEFAULT options, with no user alias or function. Claude Code's
//! Bash tool does not run them so: every command is `source <shell
//! snapshot> && setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL && … eval
//! <command>`, and the snapshot replays the options, functions and aliases
//! of the person's `~/.zshrc` (`setopt globsubst` makes `N='~'; rm -rf $N`
//! remove the home directory; glob qualifiers are dead, `~`, `=cmd`, `*`
//! and KSH_GLOB patterns in a value are not), and `.zshenv`, bash's
//! `BASH_ENV`, the `CLAUDE_ENV_FILE` and a hook's env script run first. The
//! resolver itself still reads none of that; the approval rule reads it
//! ([`super::shell_startup`], `ApprovalCtx::shell`) and proves nothing
//! where it differs: an option outside the inert lists, or a startup it
//! cannot read, fails every line; an alias or function fails a line with
//! a word spelled as its name (`rm`, `mktemp`, `test`); each reason names
//! it. What remains unseen is a word a command outside [`ARITH_HEADS`]
//! evaluates as arithmetic (a module's builtin), reading a value the
//! environment gave a variable.
//!
//! **Why no relative operand and no `$PWD`.** The only working directory
//! the supervisor can read is the session's (its `meta cwd=`, the shell's
//! OSC 7): where Claude Code was LAUNCHED. The Bash tool keeps a working
//! directory of its own that the worker moves between calls, so a relative
//! operand or `$PWD` resolved against the launch directory can point
//! anywhere (lane B's review, 2026-09-23). The launch directory is still
//! used for what it is — the `<cwd>/target*` root and the guard that no
//! operand is it or an ancestor of it.
//!
//! **Where an operand may point.** Strictly inside a [`ScratchRoot`] (never
//! the root itself), with no glob at or above the first component under the
//! root, no `..`, no `.git`, not the cwd or an ancestor of it, and past every
//! [`critical_path`] (the filesystem root, the home directory and its
//! ancestors, `/Users/<x>`, a glob in the first component) — as written, AND
//! where the disk leads it.
//!
//! **On the disk.** `rm` follows a symbolic link in every directory of an
//! operand (and in its last component after a trailing `/`), never the
//! last component itself: with `/private/tmp/p/out -> /etc`, `rm -rf
//! /private/tmp/p/out/x` removes `/etc/x`. So the operand's directories are
//! resolved against the filesystem when the box is judged — the deepest part
//! that exists, each link on the way followed as the kernel does, the rest
//! as written — and the path they lead to must pass the same checks against
//! the roots, the cwd and the home directory resolved the same way (`/tmp`
//! is `/private/tmp`). A root is resolved only through links root owns
//! (macOS's `/tmp`, `/var`): one reached through a link the worker could have
//! made or re-pointed is not a root on the disk, so a scratch directory
//! swapped for a link to the home directory holds nothing — except the
//! `<cwd>/target*` root, which is resolved where the session cwd leads, as
//! the cwd itself is for the cwd check (a checkout opened through a link of
//! the user's keeps its build directory; a cwd re-pointed elsewhere moves
//! the cwd check with it, and the root still holds only `target*`
//! directories, past every [`critical_path`]). A glob in a
//! directory may match a link, so it escalates; a link the disk will not
//! show (a permission refused, a loop) escalates. What this cannot see: a
//! link made AFTER the box is judged — nothing else on the line may write
//! (`classify_except_rm`), but a job the worker left running can.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::supervise::classify::{GLOB_CHARS, TEST_BINARY, TEST_UNARY, glob_match, numeric_format};

/// The stand-in for the unique name `mktemp` makes: a component no deny rule
/// and no literal root can match, rendered as `<mktemp>` in a target.
const MKTEMP_NAME: &str = "\u{1}mktemp";

/// The value of a `$(mktemp -d …)` whose directory is made where the line
/// does not say — under the Bash tool's `$TMPDIR`, or relative to its
/// working directory — rendered `<mktemp -d>`. Not absolute, so no rule but
/// the fresh-directory one ([`Eval::fresh_dir`]) ever proves an operand
/// built on it.
const FRESH_DIR: &str = "\u{2}mktemp-d";

/// zsh's history modifiers (`zshexpn`, "Modifiers"): after `$NAME:` one of
/// these reshapes the value; any other character leaves `:` as text.
const ZSH_MODIFIERS: &str = "aAcefFghlpPqQrsStuwWx&";

/// A path as a reason or a target shows it: the `mktemp` stand-ins named.
fn rendered(path: &str) -> String {
    path.replace(MKTEMP_NAME, "<mktemp>")
        .replace(FRESH_DIR, "<mktemp -d>")
}

/// Names the shell itself sets, ties or reads (module header): a line that
/// assigns one is not followed, whatever it assigns — the value read back is
/// not the one written (`$_`, `PWD` after a `cd`, `SECONDS`, zsh's
/// read-only `status`), or the assignment changes what the rest of the line
/// runs or how it splits (`PATH`/`path`, `IFS`, `argv`). Every `BASH_*`,
/// `ZSH_*` and `COMP_*` name too ([`shell_name`]).
const SHELL_NAMES: &[&str] = &[
    "_",
    "PWD",
    "OLDPWD",
    "HOME",
    "IFS",
    "PATH",
    "path",
    "CDPATH",
    "cdpath",
    "FPATH",
    "fpath",
    "MANPATH",
    "manpath",
    "MAILPATH",
    "mailpath",
    "MODULE_PATH",
    "module_path",
    "argv",
    "ARGC",
    "status",
    "pipestatus",
    "PIPESTATUS",
    "RANDOM",
    "SRANDOM",
    "SECONDS",
    "EPOCHSECONDS",
    "EPOCHREALTIME",
    "LINENO",
    "HISTCMD",
    "PPID",
    "UID",
    "EUID",
    "GID",
    "EGID",
    "USERNAME",
    "ERRNO",
    "TTYIDLE",
    "SHLVL",
    "REPLY",
    "reply",
    "MATCH",
    "match",
    "MBEGIN",
    "MEND",
    "mbegin",
    "mend",
    "OPTARG",
    "OPTIND",
    "GLOBIGNORE",
    "ENV",
    "ZDOTDIR",
    "SHELLOPTS",
    "BASHOPTS",
    "BASHPID",
    "GROUPS",
    "FUNCNAME",
    "DIRSTACK",
    "dirstack",
    "options",
    "parameters",
    "commands",
    "functions",
    "aliases",
    "builtins",
    "modules",
    "nameddirs",
    "userdirs",
    "psvar",
    "PSVAR",
    "histchars",
    "signals",
    "NULLCMD",
    "READNULLCMD",
    "POSIXLY_CORRECT",
    "PS4",
    "TMPPREFIX",
    // Every other name whose assignment does not read back (measured, zsh
    // 5.9 and bash 3.2; `every_name_whose_assignment_does_not_read_back_is_kept`).
    "COLUMNS",
    "LINES",
    "FUNCNEST",
    "HISTCHARS",
    "HISTSIZE",
    "SAVEHIST",
    "KEYBOARD_HACK",
    "KEYTIMEOUT",
    "LISTMAX",
    "MAILCHECK",
    "TRY_BLOCK_ERROR",
    "TRY_BLOCK_INTERRUPT",
    "dis_aliases",
    "dis_builtins",
    "dis_functions",
    "dis_functions_source",
    "dis_galiases",
    "dis_patchars",
    "dis_reswords",
    "dis_saliases",
    "funcfiletrace",
    "funcsourcetrace",
    "funcstack",
    "functions_source",
    "functrace",
    "galiases",
    "history",
    "historywords",
    "jobdirs",
    "jobstates",
    "jobtexts",
    "keymaps",
    "patchars",
    "reswords",
    "saliases",
    "termcap",
    "terminfo",
    "usergroups",
    "widgets",
    "zsh_eval_context",
    "zsh_scheduled_events",
];

/// Whether the shell itself sets, ties or reads `name` ([`SHELL_NAMES`]).
fn shell_name(name: &str) -> bool {
    SHELL_NAMES.contains(&name)
        || ["BASH_", "ZSH_", "COMP_"]
            .iter()
            .any(|p| name.starts_with(p))
}

/// How many symbolic links [`physical`] follows for one path before it gives
/// up (the kernel's own limit is of this order: `MAXSYMLINKS` is 32 on macOS).
const MAX_LINK_HOPS: usize = 32;

/// One component of a [`ScratchRoot`] pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootComp {
    Lit(String),
    /// A `*`/`?` glob over ONE component (`target*`, `*`).
    Glob(String),
}

/// A directory pattern: the scratch roots an `rm` operand must sit strictly
/// inside, and the trust roots a folder-trust dialog's path must sit in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchRoot {
    comps: Vec<RootComp>,
    label: String,
}

impl ScratchRoot {
    /// Exactly this absolute directory. `None` for a relative path or one with
    /// a `..`.
    pub fn dir(path: &Path) -> Option<Self> {
        let comps = abs_components(&path.to_string_lossy())?;
        Some(Self {
            label: path.to_string_lossy().trim_end_matches('/').to_string(),
            comps: comps.into_iter().map(RootComp::Lit).collect(),
        })
    }

    /// Any direct child of this directory (`/tmp/<x>/`).
    pub fn children_of(path: &Path) -> Option<Self> {
        Self::glob_under(path, "*")
    }

    /// Any direct child of `path` whose name matches `glob` (`<cwd>/target*`).
    pub fn glob_under(path: &Path, glob: &str) -> Option<Self> {
        let mut root = Self::dir(path)?;
        root.comps.push(RootComp::Glob(glob.to_string()));
        root.label = format!("{}/{glob}", root.label);
        Some(root)
    }

    /// The pattern as written (`/private/tmp/*`), for a reason or a ledger row.
    #[cfg(test)]
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    fn depth(&self) -> usize {
        self.comps.len()
    }

    /// This root where the disk leads it: its literal directories resolved
    /// ([`physical`]; `/tmp/*` is `/private/tmp/*` on macOS), its glob kept.
    /// `None` when they cannot be resolved, or lead through a symbolic link
    /// root does not own — one the worker could have made or re-pointed, so
    /// a scratch directory swapped for a link to `$HOME` is no root. A root
    /// whose directories ARE the session cwd (`<cwd>/target*`) is resolved
    /// where the cwd leads (`cwd_real`, every link followed, as [`Disk`]
    /// resolves the cwd), not dropped for a link of the user's on the way.
    fn on_disk(&self, cwd: &[String], cwd_real: Option<&[String]>) -> Option<Self> {
        let lits: Vec<String> = self
            .comps
            .iter()
            .map_while(|c| match c {
                RootComp::Lit(l) => Some(l.clone()),
                RootComp::Glob(_) => None,
            })
            .collect();
        let real = match cwd_real {
            Some(real) if lits == cwd && lits.len() < self.comps.len() => real.to_vec(),
            _ => physical(&lits).ok().filter(|p| !p.foreign_link)?.comps,
        };
        let mut comps: Vec<RootComp> = real.into_iter().map(RootComp::Lit).collect();
        comps.extend(self.comps[lits.len()..].iter().cloned());
        Some(Self {
            comps,
            label: self.label.clone(),
        })
    }

    /// Whether `comps` begins with this pattern.
    fn prefixes(&self, comps: &[String]) -> bool {
        comps.len() >= self.comps.len()
            && self.comps.iter().zip(comps).all(|(p, c)| match p {
                RootComp::Lit(l) => l == c,
                RootComp::Glob(g) => glob_match(g, c),
            })
    }

    /// Whether the absolute path `comps` is this root or inside it (a trust
    /// root's test).
    pub fn holds(&self, comps: &[String]) -> bool {
        self.prefixes(comps)
    }

    /// Whether `comps` is STRICTLY inside this root with no glob in any
    /// component down to the first one under it: `rm -rf <root>/*` would
    /// empty the root, `rm -rf <root>/x/*` stays inside `x`.
    fn holds_strictly_unglobbed(&self, comps: &[String]) -> bool {
        comps.len() > self.depth()
            && self.prefixes(comps)
            && !comps[..=self.depth()].iter().any(|c| has_glob(c))
    }
}

/// What an `rm` operand is resolved against.
#[derive(Debug, Clone, Copy)]
pub struct RmScope<'a> {
    /// The session's working directory (absolute).
    pub cwd: &'a Path,
    /// The owner's home directory, for the deny table.
    pub home: Option<&'a Path>,
    /// Where an operand may point. EMPTY is the rule withheld (the retired
    /// `rm_breaker = false`, owner decision D9: the loop clears the roots):
    /// then nothing is proven, not even a directory the line's own `mktemp
    /// -d` made ([`Eval::fresh_dir`], which asks no root otherwise).
    pub roots: &'a [ScratchRoot],
}

/// Resolve every `rm` on `cmd`: `Ok` with each operand's resolved path when
/// every one points strictly inside a scratch root (module header), else
/// `Err` naming the first thing that did not resolve or did not qualify.
pub fn resolve_rm_line(cmd: &str, scope: &RmScope<'_>) -> Result<Vec<String>, String> {
    let toks = lex(cmd)?;
    let unquoted = unquoted_names(&toks);
    let cmds = commands(toks)?;
    let cwd = abs_components(&scope.cwd.to_string_lossy())
        .ok_or_else(|| "the session cwd is not an absolute path".to_string())?;
    let mut ev = Eval {
        scope,
        disk: Disk::of(scope, &cwd),
        cwd,
        env: HashMap::new(),
        targets: Vec::new(),
        runs: 0,
        made: Vec::new(),
        prev: Flow::default(),
        unquoted,
    };
    for cmd in &cmds {
        ev.command(cmd)?;
    }
    if ev.targets.is_empty() {
        return Err("no rm at a command head on this line".to_string());
    }
    Ok(ev.targets)
}

// ---------------------------------------------------------------------------
// Lexing: words made of literal, variable and substitution parts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit {
        text: String,
        quoted: bool,
    },
    Var {
        name: String,
        quoted: bool,
        /// Written `${NAME:?…}`: the shell exits rather than expand it empty.
        nonempty: bool,
    },
    /// `$(…)`: its inner text, and whether it stands inside double quotes.
    Subst {
        text: String,
        quoted: bool,
    },
    /// An expansion zsh computes and this resolver does not (module header):
    /// `$=V`, `$~V`, `$^V`, `$+V`, `$V:h`, a non-ASCII name. Its source
    /// text, for a reason.
    Opaque(String),
}

type Word = Vec<Part>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Start,
    Semi,
    Newline,
    And,
    Or,
    Pipe,
    Amp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(Word),
    Op(Op),
    /// A redirect operator; `Some(target)` when the target is glued to it
    /// (`>&1`, `2>&-`), else the next word is its target.
    Redir(String, Option<String>),
}

struct Lexer {
    cs: Vec<char>,
    i: usize,
    toks: Vec<Tok>,
    word: Word,
    in_word: bool,
}

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.cs.get(self.i + k).copied()
    }

    fn lit(&mut self, c: char, quoted: bool) {
        self.in_word = true;
        if let Some(Part::Lit { text, quoted: q }) = self.word.last_mut()
            && *q == quoted
        {
            text.push(c);
            return;
        }
        self.word.push(Part::Lit {
            text: c.to_string(),
            quoted,
        });
    }

    /// The source from `start` to the cursor as one [`Part::Opaque`].
    fn opaque(&mut self, start: usize) {
        self.in_word = true;
        self.word
            .push(Part::Opaque(self.cs[start..self.i].iter().collect()));
    }

    fn end_word(&mut self) {
        if self.in_word {
            self.toks.push(Tok::Word(std::mem::take(&mut self.word)));
            self.in_word = false;
        }
    }

    fn op(&mut self, op: Op, len: usize) {
        self.end_word();
        self.toks.push(Tok::Op(op));
        self.i += len;
    }

    fn run(&mut self) -> Result<(), String> {
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' => {
                    self.end_word();
                    self.i += 1;
                }
                '\n' => self.op(Op::Newline, 1),
                ';' if self.peek(1) == Some(';') => return Err("a `;;` (case arm)".to_string()),
                ';' => self.op(Op::Semi, 1),
                '&' if self.peek(1) == Some('&') => self.op(Op::And, 2),
                '&' if self.peek(1) == Some('>') => self.redirect()?,
                '&' => self.op(Op::Amp, 1),
                '|' if self.peek(1) == Some('|') => self.op(Op::Or, 2),
                '|' if self.peek(1) == Some('&') => return Err("a `|&` pipe".to_string()),
                '|' => self.op(Op::Pipe, 1),
                '(' | ')' => return Err("a subshell or group".to_string()),
                '<' | '>' => self.redirect()?,
                '#' if !self.in_word => {
                    while self.peek(0).is_some_and(|d| d != '\n') {
                        self.i += 1;
                    }
                }
                '\'' => {
                    self.i += 1;
                    let mut any = false;
                    loop {
                        match self.peek(0) {
                            None => return Err("an unterminated ' quote".to_string()),
                            Some('\'') => break,
                            Some(d) => {
                                self.lit(d, true);
                                any = true;
                                self.i += 1;
                            }
                        }
                    }
                    self.i += 1;
                    if !any {
                        self.word.push(Part::Lit {
                            text: String::new(),
                            quoted: true,
                        });
                        self.in_word = true;
                    }
                }
                '"' => self.double_quoted()?,
                '\\' => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(n) => {
                        self.lit(n, true);
                        self.i += 2;
                    }
                    None => return Err("a trailing backslash".to_string()),
                },
                '$' => self.dollar(false)?,
                '`' => return Err("a backtick substitution".to_string()),
                _ => {
                    self.lit(c, false);
                    self.i += 1;
                }
            }
        }
        self.end_word();
        Ok(())
    }

    fn double_quoted(&mut self) -> Result<(), String> {
        self.i += 1;
        self.in_word = true;
        let before = self.word.len();
        loop {
            match self.peek(0) {
                None => return Err("an unterminated \" quote".to_string()),
                Some('"') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => match self.peek(1) {
                    Some('\n') => self.i += 2,
                    Some(n) if matches!(n, '$' | '`' | '"' | '\\') => {
                        self.lit(n, true);
                        self.i += 2;
                    }
                    Some(n) => {
                        self.lit('\\', true);
                        self.lit(n, true);
                        self.i += 2;
                    }
                    None => return Err("an unterminated \" quote".to_string()),
                },
                Some('$') => self.dollar(true)?,
                Some('`') => return Err("a backtick substitution".to_string()),
                Some(d) => {
                    self.lit(d, true);
                    self.i += 1;
                }
            }
        }
        if self.word.len() == before {
            self.word.push(Part::Lit {
                text: String::new(),
                quoted: true,
            });
        }
        Ok(())
    }

    /// At a `$`: a variable, `${NAME}` / `${NAME:?…}`, `$(…)`, or a zsh form
    /// this resolver does not compute ([`Part::Opaque`]).
    fn dollar(&mut self, quoted: bool) -> Result<(), String> {
        self.in_word = true;
        match self.peek(1) {
            Some('{') => {
                let start = self.i + 2;
                let Some(len) = self.cs[start..].iter().position(|&d| d == '}') else {
                    return Err("an unterminated ${".to_string());
                };
                let inner: String = self.cs[start..start + len].iter().collect();
                self.i = start + len + 1;
                let name_len = inner
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .count();
                let (name, rest) = inner.split_at(name_len);
                let plain = rest.is_empty()
                    || ((rest.starts_with(":?") || rest.starts_with('?'))
                        && !rest.contains(['$', '`', '\'', '"', '\\', '{']));
                if !valid_name(name) || !plain {
                    return Err(format!("a ${{{inner}}} form this resolver does not follow"));
                }
                self.word.push(Part::Var {
                    name: name.to_string(),
                    quoted,
                    // `${D?}` tests only UNSET: an empty `D` expands empty
                    // (measured, both shells).
                    nonempty: rest.starts_with(":?"),
                });
            }
            Some('(') if self.peek(2) == Some('(') => {
                return Err("an arithmetic expansion".to_string());
            }
            Some('[') => {
                return Err("a `$[…]` arithmetic expansion (it can assign: `$[S=5]`)".to_string());
            }
            // zsh's `$=V` (split), `$~V` (glob), `$^V`, `$+V`: the flags, then
            // the name, as one uncomputed part.
            Some('=' | '~' | '^' | '+') => {
                let start = self.i;
                self.i += 1;
                while self
                    .peek(0)
                    .is_some_and(|d| matches!(d, '=' | '~' | '^' | '+'))
                {
                    self.i += 1;
                }
                while self
                    .peek(0)
                    .is_some_and(|d| d.is_ascii_alphanumeric() || d == '_')
                {
                    self.i += 1;
                }
                self.opaque(start);
            }
            // zsh reads a name in the locale's letters (`é=/x` assigns, `$é`
            // expands); this resolver reads ASCII names only.
            Some(c) if !c.is_ascii() => {
                let start = self.i;
                self.i += 1;
                while self
                    .peek(0)
                    .is_some_and(|d| !d.is_ascii() || d.is_ascii_alphanumeric() || d == '_')
                {
                    self.i += 1;
                }
                self.opaque(start);
            }
            Some('(') => {
                let start = self.i + 2;
                let mut depth = 0usize;
                let mut k = start;
                let end = loop {
                    match self.cs.get(k) {
                        None => return Err("an unterminated $(".to_string()),
                        Some('(') => depth += 1,
                        Some(')') if depth == 0 => break k,
                        Some(')') => depth -= 1,
                        Some('\'' | '"' | '`' | '\\') => {
                            return Err("a quote inside a $( … )".to_string());
                        }
                        _ => {}
                    }
                    k += 1;
                };
                self.word.push(Part::Subst {
                    text: self.cs[start..end].iter().collect(),
                    quoted,
                });
                self.i = end + 1;
            }
            Some('\'' | '"') if !quoted => {
                return Err("$'…' or $\"…\" quoting".to_string());
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let start = self.i + 1;
                let len = self.cs[start..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                    .count();
                // zsh applies a subscript (`$S[1,0]` is empty) or a modifier
                // (`$S:h`) to a bare name, quoted or not (measured); bash
                // reads text. A subscript is arithmetic, which ASSIGNS
                // (`$A[S=0]` sets `S`, measured), wherever it is on the line.
                match self.cs.get(start + len) {
                    Some('[') => {
                        return Err(
                            "a zsh subscript on a bare name (`$V[…]`; its arithmetic can \
                             assign)"
                                .to_string(),
                        );
                    }
                    // Only before a modifier letter (zsh's documented set);
                    // `"$S: x"`, `$S:/c` are text (measured).
                    Some(':')
                        if self
                            .cs
                            .get(start + len + 1)
                            .is_some_and(|m| ZSH_MODIFIERS.contains(*m)) =>
                    {
                        let from = self.i;
                        self.i = start + len + 2;
                        self.opaque(from);
                        return Ok(());
                    }
                    _ => {}
                }
                self.word.push(Part::Var {
                    name: self.cs[start..start + len].iter().collect(),
                    quoted,
                    nonempty: false,
                });
                self.i = start + len;
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                // zsh subscripts a special parameter as it does a name, and
                // `$#` also takes a name (`$#A[…]`): `A=S=0; echo "$#[A]"`
                // sets `S` (measured, zsh 5.9 -f).
                let mut k = self.i + 2;
                if c == '#' {
                    while self
                        .cs
                        .get(k)
                        .is_some_and(|d| d.is_ascii_alphanumeric() || *d == '_')
                    {
                        k += 1;
                    }
                }
                if self.cs.get(k) == Some(&'[') {
                    return Err(
                        "a zsh subscript on a special parameter (`$#[…]`; its arithmetic \
                         can assign)"
                            .to_string(),
                    );
                }
                self.word.push(Part::Var {
                    name: c.to_string(),
                    quoted,
                    nonempty: false,
                });
                self.i += 2;
            }
            _ => {
                self.lit('$', quoted);
                self.i += 1;
            }
        }
        Ok(())
    }

    /// `[n]>`, `>>`, `>|`, `&>`, `>&n`, `<`: the operator, and its target when
    /// glued to a `&`. A here-document or process substitution is refused.
    fn redirect(&mut self) -> Result<(), String> {
        let mut op = String::new();
        // A descriptor number just typed is the operator's, not a word.
        if let Some(Part::Lit {
            text,
            quoted: false,
        }) = self.word.last()
            && self.word.len() == 1
            && !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit())
        {
            op.push_str(text);
            self.word.clear();
            self.in_word = false;
        }
        self.end_word();
        if self.peek(0) == Some('&') {
            op.push('&');
            self.i += 1;
        }
        let first = self.peek(0).unwrap_or('>');
        if first == '<' && matches!(self.peek(1), Some('<')) {
            return Err("a here-document".to_string());
        }
        if matches!(self.peek(1), Some('(')) {
            return Err("a process substitution".to_string());
        }
        op.push(first);
        self.i += 1;
        while let Some(d) = self.peek(0).filter(|d| matches!(d, '>' | '|')) {
            op.push(d);
            self.i += 1;
        }
        if self.peek(0) == Some('&') {
            self.i += 1;
            let mut target = String::from("&");
            while let Some(d) = self.peek(0).filter(|d| d.is_ascii_digit() || *d == '-') {
                target.push(d);
                self.i += 1;
            }
            self.toks.push(Tok::Redir(op, Some(target)));
        } else {
            self.toks.push(Tok::Redir(op, None));
        }
        Ok(())
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut lx = Lexer {
        cs: src.chars().collect(),
        i: 0,
        toks: Vec::new(),
        word: Vec::new(),
        in_word: false,
    };
    lx.run()?;
    Ok(lx.toks)
}

// ---------------------------------------------------------------------------
// Simple commands, in order, with the operators on either side
// ---------------------------------------------------------------------------

struct Cmd {
    words: Vec<Word>,
    redirs: Vec<(String, Option<Word>, Option<String>)>,
    prev: Op,
    next: Op,
    /// Its `&&`/`||` list ends in `&`: bash runs the whole list in a
    /// background subshell, so an assignment in it is not the shell's after
    /// it (`S=/x && true & echo "$S"` prints nothing in bash, `/x` in zsh —
    /// measured).
    in_async_list: bool,
}

fn commands(toks: Vec<Tok>) -> Result<Vec<Cmd>, String> {
    let mut out = Vec::new();
    let mut cur = Cmd {
        words: Vec::new(),
        redirs: Vec::new(),
        prev: Op::Start,
        next: Op::Newline,
        in_async_list: false,
    };
    let mut it = toks.into_iter().peekable();
    while let Some(tok) = it.next() {
        match tok {
            Tok::Word(w) => cur.words.push(w),
            Tok::Redir(op, Some(target)) => cur.redirs.push((op, None, Some(target))),
            Tok::Redir(op, None) => match it.next() {
                Some(Tok::Word(w)) => cur.redirs.push((op, Some(w), None)),
                _ => return Err(format!("a `{op}` with no target")),
            },
            Tok::Op(op) => {
                let empty = cur.words.is_empty() && cur.redirs.is_empty();
                if empty && op != Op::Newline && op != Op::Semi {
                    return Err("an operator with no command before it".to_string());
                }
                let prev = if empty { cur.prev } else { op };
                if !empty {
                    cur.next = op;
                    out.push(std::mem::replace(
                        &mut cur,
                        Cmd {
                            words: Vec::new(),
                            redirs: Vec::new(),
                            prev,
                            next: Op::Newline,
                            in_async_list: false,
                        },
                    ));
                }
            }
        }
    }
    if !cur.words.is_empty() || !cur.redirs.is_empty() {
        if matches!(cur.prev, Op::And | Op::Or | Op::Pipe) && cur.words.is_empty() {
            return Err("an operator with no command after it".to_string());
        }
        out.push(cur);
    }
    let mut in_async_list = false;
    for c in out.iter_mut().rev() {
        match c.next {
            Op::Amp => in_async_list = true,
            Op::Semi | Op::Newline | Op::Start => in_async_list = false,
            Op::And | Op::Or | Op::Pipe => {}
        }
        c.in_async_list = in_async_list;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Straight-line evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Known {
        text: String,
        /// The `$(mktemp -d)` runs (numbered in line order) whose FAILURE
        /// would empty a part of this value (module header).
        runs: Vec<u32>,
        /// The value were every one of `runs` to fail.
        failed: String,
        /// Whether the value IS one run's output and nothing else, so it is
        /// empty as a whole when that run fails: what `${V:?}` guards.
        whole: bool,
    },
    Unknown,
    /// Known once, then unknown after the command it names (its head),
    /// which may have assigned it ([`Eval::forget_named`]).
    Forgotten(String),
}

/// Shell words that open or close a compound command.
const KEYWORDS: &[&str] = &[
    "for", "while", "until", "select", "case", "esac", "if", "then", "elif", "else", "fi", "do",
    "done", "function", "{", "}", "!", "[[", "coproc", "time",
];

/// Builtins that can change a variable, the positional parameters, what a
/// name runs, how a later word expands, or the line's own text — zsh's
/// `integer`/`float` evaluate their values as arithmetic (`integer T=A`
/// reads `A`'s value, which can assign), and `setopt`, `emulate`, bash's
/// `shopt` and a module `zmodload` loads change how every later word reads.
const VAR_BUILTINS: &[&str] = &[
    "set",
    "integer",
    "float",
    "setopt",
    "unsetopt",
    "emulate",
    "shopt",
    "zmodload",
    "shift",
    "unset",
    "declare",
    "typeset",
    "local",
    "readonly",
    "read",
    "mapfile",
    "readarray",
    "getopts",
    "eval",
    "source",
    ".",
    "exec",
    "alias",
    "unalias",
    "trap",
    "let",
    "enable",
    "hash",
    "builtin",
    "command",
];

/// What the previous simple command left for the next one's `&&`/`||`/`|`.
#[derive(Debug, Clone, Default)]
struct Flow {
    /// The runs known made whenever it ran.
    made: Vec<u32>,
    /// The runs its SUCCESS proves made: its only substitution a `$(mktemp
    /// -d)` it assigned, or `test -n "$D"` of one.
    implies: Vec<u32>,
    /// Whether it began its `&&`/`||` list.
    began_list: bool,
}

/// The disk's side of the checks ("On the disk" in the module header): the
/// session cwd, the home directory and the scratch roots, each where it
/// leads. Measured once per line.
struct Disk {
    cwd: Result<Vec<String>, String>,
    home: Option<Vec<String>>,
    roots: Vec<ScratchRoot>,
}

impl Disk {
    fn of(scope: &RmScope<'_>, cwd: &[String]) -> Self {
        let home = scope
            .home
            .and_then(|h| abs_components(&h.to_string_lossy()));
        let cwd_real = physical(cwd).map(|p| p.comps);
        let roots = scope
            .roots
            .iter()
            .filter_map(|r| r.on_disk(cwd, cwd_real.as_deref().ok()))
            .collect();
        Self {
            cwd: cwd_real,
            home: home.map(|h| physical(&h).map_or(h, |p| p.comps)),
            roots,
        }
    }
}

struct Eval<'a> {
    scope: &'a RmScope<'a>,
    cwd: Vec<String>,
    disk: Disk,
    env: HashMap<String, Val>,
    targets: Vec<String>,
    /// How many `$(mktemp -d)` runs the line has read: the next one's number.
    runs: u32,
    /// The runs known made from here on, whatever runs next: a `${D:?}` a
    /// command that surely ran expanded, and `D=$(…) || exit`.
    made: Vec<u32>,
    prev: Flow,
    /// Every name the line expands somewhere NOT inside double quotes — a
    /// bare `$D` or `${D}`, and any `$D` inside a `$(…)` or a zsh form —
    /// which [`Self::fresh_dir`] refuses ([`unquoted_names`]).
    unquoted: HashSet<String>,
}

/// The names `toks` expand outside double quotes ([`Eval::unquoted`]): a
/// [`Part::Var`] not quoted, and every `$NAME`, `${NAME` or zsh `$=NAME`-style
/// reference inside a [`Part::Subst`] or [`Part::Opaque`], whose quoting is
/// not read.
fn unquoted_names(toks: &[Tok]) -> HashSet<String> {
    let mut out = HashSet::new();
    let scan = |out: &mut HashSet<String>, text: &str| {
        let cs: Vec<char> = text.chars().collect();
        for (i, c) in cs.iter().enumerate() {
            if *c != '$' {
                continue;
            }
            let mut k = i + 1;
            while cs
                .get(k)
                .is_some_and(|d| matches!(d, '{' | '=' | '~' | '^' | '+' | '#'))
            {
                k += 1;
            }
            let name: String = cs[k..]
                .iter()
                .take_while(|d| d.is_ascii_alphanumeric() || **d == '_')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    };
    for tok in toks {
        let Tok::Word(w) = tok else {
            continue;
        };
        for p in w {
            match p {
                Part::Var {
                    name,
                    quoted: false,
                    ..
                } => {
                    out.insert(name.clone());
                }
                Part::Subst { text: t, .. } | Part::Opaque(t) => scan(&mut out, t),
                Part::Var { .. } | Part::Lit { .. } => {}
            }
        }
    }
    out
}

fn valid_name(name: &str) -> bool {
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The word as literal text when every part is literal.
fn literal(word: &Word) -> Option<String> {
    let mut s = String::new();
    for p in word {
        match p {
            Part::Lit { text, .. } => s.push_str(text),
            _ => return None,
        }
    }
    Some(s)
}

/// `NAME=` at the head of an unquoted first part: `(NAME, the value's parts)`.
fn assignment(word: &Word) -> Option<(String, Word)> {
    let Some(Part::Lit {
        text,
        quoted: false,
    }) = word.first()
    else {
        return None;
    };
    let (name, rest) = text.split_once('=')?;
    if !valid_name(name) {
        return None;
    }
    let mut value = Vec::new();
    if !rest.is_empty() {
        value.push(Part::Lit {
            text: rest.to_string(),
            quoted: false,
        });
    }
    value.extend(word[1..].iter().cloned());
    Some((name.to_string(), value))
}

/// An assignment [`assignment`] does not read, at a command's head: an
/// append (`S+=/..`) or a subscript (`S[0]=/etc`, which is `S=/etc` in bash;
/// `S[1,-1]=/etc`, which is `S=/etc` in zsh). The form, for a reason.
fn assignment_like(word: &Word) -> Option<String> {
    let Some(Part::Lit {
        text,
        quoted: false,
    }) = word.first()
    else {
        return None;
    };
    let name_len = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    let (name, rest) = text.split_at(name_len);
    (valid_name(name) && (rest.starts_with("+=") || rest.starts_with('['))).then(|| clip(text, 40))
}

/// Whether an unquoted piece `t` of an assignment's value, after `before`,
/// holds a `~` or `=` the shell expands there: at the value's start or after
/// a `:` (module header).
fn value_expands(before: &str, t: &str) -> bool {
    let mut prev = before.chars().last();
    for c in t.chars() {
        if matches!(c, '~' | '=') && matches!(prev, None | Some(':')) {
            return true;
        }
        prev = Some(c);
    }
    false
}

/// Commands that take no word as a variable's name or as arithmetic,
/// whatever it holds: a double-quoted word the resolver cannot compute
/// under one of these names nothing ([`Eval::forget_named`]; zsh's
/// `{S}>file` redirect assigns `S` under a builtin one, and is read apart). Not `print`,
/// `printf` (zsh evaluates a `%d` argument: `printf %d "S=5"` sets `S`,
/// measured), `test`/`[` (`-t` evaluates in zsh), `stat` (zsh/stat's `-A`),
/// `read`, `wait`, or any other builtin that can. A program run from the
/// disk cannot assign the shell's variables at all: the read-only ones the
/// classifier accepts are here (measured no builtin in zsh 5.9 or bash
/// 3.2), but not `env` or `xargs`, which run a command, nor `stat`.
const PLAIN_CONSUMERS: &[&str] = &[
    "echo",
    "ls",
    "du",
    "cat",
    "date",
    "wc",
    "head",
    "tail",
    "file",
    "mkdir",
    "touch",
    "pwd",
    "true",
    "false",
    ":",
    "grep",
    "rg",
    "find",
    "sort",
    "uniq",
    "cut",
    "basename",
    "dirname",
    "readlink",
    "realpath",
    "diff",
    "cmp",
    "tree",
    "which",
    "git",
    "egrep",
    "fgrep",
    "mdfind",
    "sed",
    "awk",
    "tr",
    "jq",
    "comm",
    "nl",
    "md5",
    "shasum",
    "sha256sum",
    "column",
    "paste",
    "seq",
    "strings",
    "fold",
    "expr",
    "uname",
    "whoami",
    "id",
    "sw_vers",
    "hostname",
    "df",
    "uptime",
    "sysctl",
    "ps",
    "lsof",
    "pgrep",
    "sleep",
];

/// Commands zsh evaluates a word of as arithmetic, which assigns and reads
/// a variable's value as arithmetic in turn (measured, zsh 5.9: `printf %d
/// "S=5"`, `print -f %d "S=5"`, `[ -t "1/(S=5)" ]`, `return S=5` each set
/// `S`; `[ 1 -eq S=5 ]`, `printf %s S=5`, `kill`, `ulimit`, `umask`,
/// `wait` do not). Which of their words is [`arith_words`]; bash 3.2 reads
/// none of these as arithmetic (measured).
const ARITH_HEADS: &[&str] = &["printf", "print", "test", "[", "return"];

/// How an [`ARITH_HEADS`] command reads its words (numbered from the
/// command's head, 0), `words` as they expand and `parts` as written.
#[derive(Debug, Default)]
struct Reading {
    /// The words it evaluates as arithmetic: `test`/`[`'s word after `-t`;
    /// `printf`'s arguments after its format (past `-v NAME` and `--`)
    /// unless the format says otherwise ([`numeric_format`]) — zsh takes a
    /// `-` word other than `-v` as the format —, and every word after a
    /// `-vNAME` word, which bash reads as `-v NAME`; every
    /// word of `print -f` (`printf`'s own reading) and of `return`.
    arith: Vec<usize>,
    /// The first word from which a word it does not evaluate names nothing:
    /// `printf`'s format, when every word before it is `-v NAME` or `--`
    /// and every word up to it stands as written ([`as_written`]) — only
    /// `-v NAME`, an option, assigns. Past the words when there is no such
    /// word.
    mute_from: usize,
}

fn arith_words(head: &str, words: &[String], parts: &[Word]) -> Reading {
    let none = Reading {
        arith: Vec::new(),
        mute_from: words.len(),
    };
    if !ARITH_HEADS.contains(&head) {
        return none;
    }
    match head {
        "test" | "[" => Reading {
            arith: (2..words.len()).filter(|&k| words[k - 1] == "-t").collect(),
            ..none
        },
        "printf" => {
            let mut k = 1;
            while let Some(w) = words.get(k) {
                if w == "--" {
                    k += 1;
                    break;
                }
                if !w.starts_with('-') || w == "-" {
                    break;
                }
                if w.starts_with("-v") && w != "-v" {
                    // zsh's `printf` knows only `-v`: any other `-` word is
                    // its FORMAT (`printf -vS %d S=5` prints it — measured,
                    // zsh 5.9), but bash reads `-vS` as `-v S` and assigns
                    // `S`. No word after it is muted, and every one is read
                    // as arithmetic.
                    return Reading {
                        arith: (k + 1..words.len()).collect(),
                        ..none
                    };
                }
                if w != "-v" {
                    // Any other `-` word is zsh's FORMAT (`printf -%d S=5`
                    // sets `S`; `printf '--- %s ---\n' S=5` does not —
                    // measured, zsh 5.9), which bash refuses as an unknown
                    // option, assigning nothing (bash 3.2 knows only `-v`,
                    // and stops at a cluster's first unknown letter).
                    break;
                }
                k += 2;
            }
            let args: Vec<usize> = (k + 1..words.len()).collect();
            // A word before the format the shell may split, remove, glob or
            // brace-expand moves the format (`E=; printf $E -v S /etc` is
            // `printf -v S /etc`): read as before, every argument arithmetic.
            if k >= words.len() || !(1..=k).all(|j| parts.get(j).is_some_and(as_written)) {
                return Reading {
                    arith: args,
                    ..none
                };
            }
            Reading {
                arith: if numeric_format(&words[k]) {
                    args
                } else {
                    Vec::new()
                },
                mute_from: k,
            }
        }
        // `print -f` is `printf`; without it `print` reads no arithmetic
        // (`print -C S=5 a` leaves `S`, measured). An option's own word does
        // not end its options (`print -u 2 -f %d S=5` sets `S`, measured),
        // so any `-` word with an `f` before `--` counts.
        "print"
            if words[1..]
                .iter()
                .take_while(|w| w.as_str() != "--")
                .any(|w| w.starts_with('-') && w.contains('f')) =>
        {
            Reading {
                arith: (1..words.len()).collect(),
                ..none
            }
        }
        "return" => Reading {
            arith: (1..words.len()).collect(),
            ..none
        },
        _ => none,
    }
}

/// The words `test`/`[` read as a string or a file operand (numbered from
/// the command's head, 0), by POSIX's rules for 1, 2 and 3 arguments, every
/// operator a literal: `[ X ]`, `[ ! X ]`, `[ -n X ]`, `[ X = Y ]`, `[ ! -d X
/// ]`. A double-quoted word there names nothing whatever its value
/// ([`Eval::forget_named`]): measured, zsh 5.9 and bash 3.2, the values
/// `-t`, `S=5`, `-v`, `-vS`, `path[$(cmd)]`, `(`, `)`, `!`, `=`, `-a`,
/// `-o`, `-t S=5` and `1/(S=5)` in `[ -n "$x" ]`, `[ -z "$x" ]`, `[ "$x"
/// ]`, `[ ! "$x" ]`, `[ ! -n "$x" ]`, `[ -d "$x" ]`, `[ "$x" = "$y" ]`, `[
/// "$x" != "$y" ]`, `[ "$x" -eq "$y" ]` and `test "$x" = "$y"` assign
/// nothing and run nothing. `-t`'s operand is arithmetic ([`arith_words`]),
/// and `-v` and `-R` name a variable (zsh's `[ -v "$x" ]` evaluates the
/// subscript in `x`, measured): no operand. Four words or more: none. Nor
/// any when a word holds `"$@"`, which is as many words as there are
/// positional parameters: `set -- -t S=0; [ "$@" ]` sets `S` (measured,
/// zsh 5.9 -f).
fn test_operands(head: &str, words: &[Word]) -> Vec<usize> {
    let args = match (head, words.get(1..).unwrap_or_default()) {
        ("test", args) => args,
        ("[", [args @ .., close]) if literal(close).as_deref() == Some("]") => args,
        _ => return Vec::new(),
    };
    if args
        .iter()
        .flatten()
        .any(|p| matches!(p, Part::Var { name, .. } if name == "@"))
    {
        return Vec::new();
    }
    let is = |k: usize, set: &[&str]| {
        args.get(k)
            .and_then(literal)
            .is_some_and(|t| t != "-t" && set.contains(&t.as_str()))
    };
    let at: &[usize] = match args.len() {
        1 => &[0],
        2 if is(0, &["!"]) || is(0, TEST_UNARY) => &[1],
        3 if is(1, TEST_BINARY) => &[0, 2],
        3 if is(0, &["!"]) && is(1, TEST_UNARY) => &[2],
        _ => &[],
    };
    at.iter().map(|k| k + 1).collect()
}

/// Whether the shell passes `word` on as exactly the one word it spells:
/// quoted text, quoted variables, and unquoted text with no glob, no brace,
/// no `~` (a leading one is the home directory, and `EXTENDED_GLOB` reads
/// any other) and no leading `=` (zsh's `=cmd`). An unquoted variable may split
/// (bash) or vanish when empty (both shells).
fn as_written(word: &Word) -> bool {
    word.iter().enumerate().all(|(k, p)| match p {
        Part::Lit { quoted: true, .. } | Part::Var { quoted: true, .. } => true,
        Part::Lit {
            text,
            quoted: false,
        } => {
            !has_glob(text)
                && !text.contains(['{', '}', '~'])
                && !(k == 0 && text.starts_with(['~', '=']))
        }
        Part::Var { .. } | Part::Subst { .. } | Part::Opaque(_) => false,
    })
}

/// Whether the shell may make other words of `word`: an unquoted `{`,
/// which brace-expands (`{%d,A}` is `%d A`, `-{vS,-}` is `-vS --`; both
/// shells, measured), or an unquoted glob, which becomes the names in the
/// working directory (`printf %d *` with a file `S=5` there sets `S` in
/// zsh, and so does `[ ^-vS ]` under `EXTENDED_GLOB` beside files `-t` and
/// `S=5` — measured). A `~` that starts the word is a directory, one path.
fn expands_apart(word: &Word) -> bool {
    word.iter().enumerate().any(|(k, p)| match p {
        Part::Lit {
            text,
            quoted: false,
        } => {
            // `EXTENDED_GLOB`'s `~` changes the word's text (`-t~y` is `-t`
            // beside a file `-t`); a `~` that starts the word does not.
            let tilde = text.chars().skip(usize::from(k == 0)).any(|c| c == '~');
            text.contains('{') || has_glob(text) || tilde
        }
        _ => false,
    })
}

/// A value's text with the `mktemp` stand-ins blanked: they name nothing.
fn unstand(text: &str) -> String {
    text.replace(MKTEMP_NAME, " ").replace(FRESH_DIR, " ")
}

/// Whether some word or substitution on a command carries an `rm` token
/// that is not the command's own head.
fn mentions_rm(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')))
        .map(str::to_ascii_lowercase)
        .any(|w| w == "rm" || w.ends_with("/rm"))
}

/// `rm` by any letter case: the default macOS volume is case-insensitive,
/// so `Rm` and `/BIN/RM` run `/bin/rm` (measured: bash's `type Rm`).
fn is_rm_head(h: &str) -> bool {
    matches!(
        h.to_ascii_lowercase().as_str(),
        "rm" | "/bin/rm" | "/usr/bin/rm"
    )
}

/// `exit` or `exit <n>`, nothing else on the command.
fn is_exit(cmd: &Cmd) -> bool {
    let words: Vec<Option<String>> = cmd.words.iter().map(literal).collect();
    match words.as_slice() {
        [Some(h)] => h == "exit",
        [Some(h), Some(n)] => h == "exit" && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// A known value unknown after the command `head` ([`Val::Forgotten`]); an
/// unknown one keeps its own reason.
fn forget(v: &mut Val, head: &str) {
    if matches!(v, Val::Known { .. }) {
        *v = Val::Forgotten(clip(head, 20));
    }
}

/// `a` with every member of `b` it lacks.
fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = a.to_vec();
    out.extend(b.iter().filter(|r| !a.contains(r)));
    out
}

/// One `rm` operand, expanded.
struct Operand {
    text: String,
    glob: bool,
    /// Each `$(mktemp -d)` run the operand is built on, and the variable it
    /// came through.
    runs: Vec<(u32, String)>,
    /// The operand were those runs to fail.
    failed: String,
    /// It is one variable and nothing else, whose value is one run's output:
    /// `""` (or no word at all) when that run fails, which removes nothing.
    alone: bool,
}

impl Eval<'_> {
    /// One simple command, and what it leaves for the next ([`Flow`]).
    fn command(&mut self, cmd: &Cmd) -> Result<(), String> {
        // The runs known made whenever this command runs: after `&&`, all
        // the previous one's and all its success proves; after `|`, the
        // previous one's (it runs with it); after `||`, `;` or `&`, none —
        // the list before may have failed on a `mktemp`.
        let made = match cmd.prev {
            Op::Start | Op::Semi | Op::Newline | Op::Amp | Op::Or => Vec::new(),
            Op::And => union(&self.prev.made, &self.prev.implies),
            Op::Pipe => self.prev.made.clone(),
        };
        let began_list = matches!(cmd.prev, Op::Start | Op::Semi | Op::Newline | Op::Amp);
        // It surely runs, in this shell: first in its list, neither piped
        // (a pipeline's commands run in subshells) nor in a list that is
        // backgrounded ([`Cmd::in_async_list`]).
        let surely = began_list && cmd.next != Op::Pipe && !cmd.in_async_list;
        // `D=$(mktemp -d) || exit`: past this list the assignment succeeded.
        if cmd.prev == Op::Or
            && self.prev.began_list
            && matches!(cmd.next, Op::Semi | Op::Newline)
            && is_exit(cmd)
        {
            self.made = union(&self.made, &self.prev.implies);
        }
        let (implies, guarded) = self.simple(cmd, &made, surely)?;
        if surely {
            // Its `${D:?}` expanded, or the shell exited: that `D` was not
            // empty, and its run made, from here on.
            self.made = union(&self.made, &guarded);
        }
        self.prev = Flow {
            made,
            implies,
            began_list,
        };
        Ok(())
    }

    /// The runs the `${V:?}` among `parts` guard, `V` as the line has it
    /// now: each `V` whose value is one run's output (`Val::Known::whole`).
    fn guarded_runs<'p>(&self, parts: impl Iterator<Item = &'p Part>) -> Vec<u32> {
        let mut out = Vec::new();
        for p in parts {
            if let Part::Var {
                name,
                nonempty: true,
                ..
            } = p
                && let Val::Known {
                    runs, whole: true, ..
                } = self.lookup(name)
            {
                out = union(&out, &runs);
            }
        }
        out
    }

    /// [`Self::command`]'s reading of the command itself: `Ok` with the runs
    /// its success proves made, and the runs its `${V:?}` words guard — each
    /// read where the shell expands it: an assignment's after the ones
    /// before it on the command, every other word before the command
    /// assigns anything (`E=${D:?} D=…` guards the OLD `D`; `export`
    /// expands all its words first — measured, both shells).
    fn simple(
        &mut self,
        cmd: &Cmd,
        made: &[u32],
        surely: bool,
    ) -> Result<(Vec<u32>, Vec<u32>), String> {
        let n_assign = cmd
            .words
            .iter()
            .take_while(|w| assignment(w).is_some())
            .count();
        let head = cmd.words.get(n_assign).map(literal);
        if let Some(Some(h)) = &head {
            if KEYWORDS.contains(&h.as_str()) {
                return Err(format!("a compound command (`{h}`)"));
            }
            if VAR_BUILTINS.contains(&h.as_str()) {
                return Err(format!("`{h}` can change what a later rm means"));
            }
        }
        if let Some(form) = cmd.words.get(n_assign).and_then(assignment_like) {
            return Err(format!(
                "`{form}` assigns a variable in a way this resolver does not follow"
            ));
        }
        // Any `rm` a word or a substitution hands to another program.
        for (k, w) in cmd.words.iter().enumerate() {
            for p in w {
                let text = match p {
                    Part::Lit { text, .. } | Part::Subst { text, .. } => text,
                    Part::Var { .. } | Part::Opaque(_) => continue,
                };
                let head = k == n_assign && literal(w).as_deref().is_some_and(is_rm_head);
                if !head && mentions_rm(text) {
                    return Err(format!(
                        "an rm this resolver cannot see run (in `{}`)",
                        clip(text, 60)
                    ));
                }
            }
        }
        let names: Vec<String> = cmd.words[..n_assign]
            .iter()
            .filter_map(|w| assignment(w).map(|(n, _)| n))
            .collect();
        if let Some(name) = names.iter().find(|n| shell_name(n)) {
            return Err(format!(
                "{name} is assigned, a name the shell itself sets or reads"
            ));
        }
        if n_assign == cmd.words.len() {
            // An assignment-only command. It surely runs only when it starts
            // its `&&`/`||` list and is neither piped nor backgrounded. Its
            // status is its LAST substitution's, so with one substitution
            // its success proves that run made.
            let substs = cmd
                .words
                .iter()
                .flatten()
                .filter(|p| matches!(p, Part::Subst { .. }))
                .count();
            let mut implies = Vec::new();
            let mut guarded = Vec::new();
            for w in &cmd.words {
                let (name, value) = assignment(w).ok_or("an assignment")?;
                guarded = union(&guarded, &self.guarded_runs(value.iter()));
                let first = self.runs;
                let val = if surely {
                    self.value(&value)
                } else {
                    Val::Unknown
                };
                if substs == 1 && self.runs > first {
                    implies.push(first);
                }
                self.env.insert(name, val);
            }
            return Ok((implies, guarded));
        }
        // A command with an assignment in front of it guards nothing
        // (module header): zsh expands `export`'s own words with the prefix
        // in force, so a `${D:?}` there tests the prefix, not the `D` a copy
        // of it (`E="$D"`) still holds.
        let guarded = if names.is_empty() {
            self.guarded_runs(cmd.words.iter().flatten())
        } else {
            Vec::new()
        };
        let rest = &cmd.words[n_assign..];
        let Some(Some(head)) = head else {
            return Err("a command named by an expansion".to_string());
        };
        if head == "export" {
            // `S=/etc export T=$S`: zsh gives `T` the prefix's `S`, bash keeps
            // the prefix — neither is this line's reading. Every name the
            // command assigns is unknown after it.
            let prefixed = !names.is_empty();
            self.export(rest, surely && !prefixed)?;
            for name in names {
                self.env.insert(name, Val::Unknown);
            }
            return Ok((Vec::new(), guarded));
        }
        if is_rm_head(&head) {
            if !names.is_empty() {
                return Err("an assignment on the rm command itself".to_string());
            }
            // Every word is expanded before rm runs, so a `${D:?}` anywhere
            // on it guards `D` in every operand.
            self.rm(cmd, &rest[1..], &union(made, &guarded))?;
            return Ok((Vec::new(), guarded));
        }
        // `NAME=v cmd`: not seen by this command's own expansions, and not
        // known after it.
        for name in names {
            self.env.insert(name, Val::Unknown);
        }
        let implies = self.test_implies(&head, &rest[1..]);
        self.forget_named(cmd, &head);
        Ok((implies, guarded))
    }

    /// What a command this resolver does not model may do to the line's
    /// variables (module header, "zsh"): a builtin assigns a variable NAMED
    /// in its words — `print -v S /etc`, `print -vS`, bash `printf -vS`,
    /// `zparseopts a:=S`, `zstyle -s c s S`, `zformat -f S /etc`, `stat -A
    /// S` — or evaluates a word as arithmetic, which assigns (`[ -t
    /// "1/(S=5)" ]`, zsh `printf %d "S=5"`), so every variable a word names,
    /// as the word expands, is unknown after it. Two kinds of word name
    /// nothing but what they say: a lone option cluster (`-d`, `-sh`) names
    /// only what is glued after its first letter (`-vS` → `S`), and a path
    /// word (a `/`, and none of `=`, `(`, `[`, `++`, `--`: no arithmetic can
    /// assign in it) names nothing. Every variable is unknown after a word
    /// whose text is not known — unless the word is double-quoted and the
    /// command is a [`PLAIN_CONSUMERS`] one, or the word is data past
    /// `printf`'s as-written format that reads no arithmetic: an unquoted
    /// unknown word, a zsh form, or an unquoted value with a glob can run
    /// code in this shell
    /// (a glob qualifier under `GLOB_SUBST`), and any other command may read
    /// the unknown word as a name. A [`PLAIN_CONSUMERS`] command's words
    /// name nothing: it assigns no variable. A word an [`ARITH_HEADS`]
    /// command evaluates as arithmetic ([`arith_words`]) is read as zsh
    /// reads it ([`Self::forget_arith`]). Whatever the command, an unquoted
    /// `{NAME}` word forgets `NAME`: zsh's `{S}>file` stores the number of
    /// the descriptor it opens in `S`, in this shell under a builtin (`echo
    /// hi {S}>/dev/null`, `: {S}</dev/null`, `printf '%s' x {S}>/dev/null`;
    /// a blank before the `>` too — measured, zsh 5.9).
    fn forget_named(&mut self, cmd: &Cmd, head: &str) {
        for w in &cmd.words {
            if let [
                Part::Lit {
                    text,
                    quoted: false,
                },
            ] = w.as_slice()
                && let Some(name) = text.strip_prefix('{').and_then(|t| t.strip_suffix('}'))
                && let Some(v) = self.env.get_mut(name)
            {
                forget(v, head);
            }
        }
        let plain = PLAIN_CONSUMERS.contains(&head);
        let redirs = cmd.redirs.iter().filter_map(|(_, w, _)| w.as_ref());
        let mut words: Vec<String> = Vec::new();
        let mut unknown = false;
        // The words whose text is not known only through a double-quoted
        // variable or substitution: one word each, whatever its value.
        let mut quoted_unknown: Vec<usize> = Vec::new();
        for (k, w) in cmd.words.iter().chain(redirs).enumerate() {
            let mut text = String::new();
            for p in w {
                match p {
                    Part::Lit { text: t, .. } => text.push_str(t),
                    Part::Var { name, quoted, .. } => match self.lookup(name) {
                        // The `mktemp` stand-ins name no variable.
                        Val::Known { text: t, .. } => {
                            unknown |= !quoted && has_glob(&t);
                            text.push_str(&unstand(&t));
                        }
                        Val::Unknown | Val::Forgotten(_) if *quoted => {
                            if !plain {
                                quoted_unknown.push(k);
                            }
                        }
                        Val::Unknown | Val::Forgotten(_) => unknown = true,
                    },
                    Part::Subst { quoted: true, .. } => {
                        if !plain {
                            quoted_unknown.push(k);
                        }
                    }
                    Part::Subst { quoted: false, .. } | Part::Opaque(_) => unknown = true,
                }
            }
            words.push(text);
        }
        if unknown {
            self.forget_all(head);
            return;
        }
        if plain {
            return;
        }
        let at = cmd
            .words
            .iter()
            .take_while(|w| assignment(w).is_some())
            .count();
        let reading = arith_words(head, &words[at..cmd.words.len()], &cmd.words[at..]);
        // A double-quoted unknown word names nothing where the command
        // reads it as plain data: past `printf`'s as-written format when
        // that format reads no arithmetic (`printf '%s\n' "$x"` — its `-v`
        // and `--` stand before the format, and zsh evaluates an argument
        // only under a numeric conversion, [`numeric_format`]), and as a
        // string or file operand of `test`/`[` ([`test_operands`]).
        // Anywhere else its value may be a name, an option or arithmetic.
        let operands = test_operands(head, &cmd.words[at..]);
        let data = |k: usize| {
            k > at
                && k < cmd.words.len()
                && (k - at > reading.mute_from || operands.contains(&(k - at)))
                && !reading.arith.contains(&(k - at))
        };
        if !quoted_unknown.iter().all(|&k| data(k)) {
            self.forget_all(head);
            return;
        }
        // The positions read below are the words AS WRITTEN; a word the
        // shell makes more than one word of moves every word after it — an
        // argument into `printf`'s format, a format into an option, a name
        // into `-t`'s operand (`A=S=5; printf {%d,A}` sets `S` in zsh, and
        // `printf {-vS,%s} /etc` in bash). Every variable is unknown after
        // one, unless it is a data word past `printf`'s as-written format
        // that reads no arithmetic — or its every word is an absolute path
        // (it expands to `/…` and holds no blank): no option, no name, no
        // arithmetic (`printf %d /tmp/w/*` beside a file `S=5` there leaves
        // `S`, measured, both shells).
        let moved = (at + 1..cmd.words.len()).any(|k| {
            expands_apart(&cmd.words[k])
                && !(words[k].starts_with('/') && !words[k].contains(char::is_whitespace))
                && (k - at < reading.mute_from || reading.arith.contains(&(k - at)))
        });
        if moved {
            self.forget_all(head);
            return;
        }
        for (k, t) in words.iter().enumerate() {
            if k > at && k < cmd.words.len() && reading.arith.contains(&(k - at)) {
                if self.forget_arith(t, head) {
                    return;
                }
                continue;
            }
            if k > at && k < cmd.words.len() && k - at >= reading.mute_from {
                continue;
            }
            let cluster = t.len() > 1
                && t.starts_with('-')
                && t[1..].bytes().all(|b| b.is_ascii_alphabetic());
            let path = t.contains('/')
                && !t.contains(['=', '(', '['])
                && !t.contains("++")
                && !t.contains("--");
            if path {
                continue;
            }
            let names: Vec<&str> = if cluster {
                // What is glued after the first option letter: `-vS` → `S`,
                // `-rvS` → `vS`, `S`.
                (2..t.len()).map(|k| &t[k..]).collect()
            } else {
                t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .collect()
            };
            for name in names {
                if let Some(v) = self.env.get_mut(name) {
                    forget(v, head);
                }
            }
        }
    }

    /// Every variable unknown after the command `head`.
    fn forget_all(&mut self, head: &str) {
        for v in self.env.values_mut() {
            forget(v, head);
        }
    }

    /// A word zsh evaluates as arithmetic, `text` as it expands: every name
    /// it reads is unknown after it, and zsh reads a variable's value as
    /// arithmetic in turn (`A=S=5; printf %d A` sets `S`, measured), so so is
    /// every name that value reads, and on. Text that starts with `/` reads
    /// nothing: the parse stops at its first character (`printf %d /A` does
    /// not read `A`; `1/A` and `x/A` do — measured). A name whose value the
    /// line does not know — the environment's, or one it forgot — can read
    /// any name: every variable is unknown then, and `true` says so.
    fn forget_arith(&mut self, text: &str, head: &str) -> bool {
        let mut todo = vec![text.to_string()];
        let mut seen = HashSet::new();
        while let Some(t) = todo.pop() {
            if t.starts_with('/') {
                continue;
            }
            for name in t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                if !valid_name(name) || !seen.insert(name.to_string()) {
                    continue;
                }
                match self.lookup(name) {
                    Val::Known { text: v, .. } => {
                        todo.push(unstand(&v));
                        if let Some(v) = self.env.get_mut(name) {
                            forget(v, head);
                        }
                    }
                    Val::Unknown | Val::Forgotten(_) => {
                        self.forget_all(head);
                        return true;
                    }
                }
            }
        }
        false
    }

    /// `test -n "$D"`, `test -d "$D"`, `test -e "$D"` and their `[ … ]`
    /// forms, `D` one run's output: their success proves the run made. The
    /// operand must be quoted — `test -n` with no operand is TRUE (measured,
    /// both shells), and that is what an unquoted empty `$D` leaves.
    fn test_implies(&self, head: &str, args: &[Word]) -> Vec<u32> {
        let args = match (head, args) {
            ("test", args) => args,
            ("[", [args @ .., close]) if literal(close).as_deref() == Some("]") => args,
            _ => return Vec::new(),
        };
        let [flag, word] = args else {
            return Vec::new();
        };
        if !matches!(literal(flag).as_deref(), Some("-n" | "-d" | "-e")) {
            return Vec::new();
        }
        let mut vars = word
            .iter()
            .filter(|p| !matches!(p, Part::Lit { text, .. } if text.is_empty()));
        match (vars.next(), vars.next()) {
            (
                Some(Part::Var {
                    name, quoted: true, ..
                }),
                None,
            ) => match self.lookup(name) {
                Val::Known {
                    runs, whole: true, ..
                } => runs,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// `export NAME=value` is an assignment under the same rule; `export NAME`
    /// changes nothing this resolver reads; anything else escalates. Its
    /// status is `export`'s, whatever a substitution in it did, and it
    /// expands all its words before it assigns any: `export S=/x T=$S/b`
    /// makes `T` `/b` (measured, both shells).
    fn export(&mut self, rest: &[Word], surely: bool) -> Result<(), String> {
        let mut bound = Vec::new();
        for w in &rest[1..] {
            if let Some((name, value)) = assignment(w) {
                if shell_name(&name) {
                    return Err(format!(
                        "{name} is assigned, a name the shell itself sets or reads"
                    ));
                }
                let val = if surely {
                    self.value(&value)
                } else {
                    Val::Unknown
                };
                bound.push((name, val));
            } else if !literal(w).is_some_and(|l| valid_name(&l)) {
                return Err("an `export` this resolver does not follow".to_string());
            }
        }
        self.env.extend(bound);
        Ok(())
    }

    /// A variable's value as this line assigned it; a name it did not
    /// assign is not known (module header, "Variables from the environment").
    fn lookup(&self, name: &str) -> Val {
        self.env.get(name).cloned().unwrap_or(Val::Unknown)
    }

    /// An assignment's value: no word splitting or globbing; a `~` or `=`
    /// the shell expands there unresolved ([`value_expands`]).
    fn value(&mut self, parts: &Word) -> Val {
        let mut text = String::new();
        let mut failed = String::new();
        let mut runs = Vec::new();
        let mut pieces = 0usize;
        let mut whole_piece = false;
        for p in parts {
            match p {
                Part::Lit { text: t, quoted } => {
                    if !quoted && value_expands(&text, t) {
                        return Val::Unknown;
                    }
                    text.push_str(t);
                    failed.push_str(t);
                    pieces += usize::from(!t.is_empty());
                }
                Part::Var { name, nonempty, .. } => match self.lookup(name) {
                    Val::Known {
                        text: t,
                        runs: r,
                        failed: f,
                        whole: w,
                    } => {
                        pieces += 1;
                        text.push_str(&t);
                        if *nonempty && w {
                            // `${D:?}`: the shell exits rather than go on
                            // with `D` empty.
                            failed.push_str(&t);
                        } else {
                            failed.push_str(&f);
                            runs = union(&runs, &r);
                            whole_piece = w;
                        }
                    }
                    Val::Unknown | Val::Forgotten(_) => return Val::Unknown,
                },
                Part::Subst { text: s, .. } => match mktemp_dir(s) {
                    Some(t) => {
                        pieces += 1;
                        text.push_str(&t);
                        runs.push(self.runs);
                        self.runs += 1;
                        whole_piece = true;
                    }
                    None => return Val::Unknown,
                },
                Part::Opaque(_) => return Val::Unknown,
            }
        }
        Val::Known {
            text,
            whole: pieces == 1 && whole_piece && runs.len() == 1,
            runs,
            failed,
        }
    }

    fn rm(&mut self, cmd: &Cmd, args: &[Word], made: &[u32]) -> Result<(), String> {
        for (op, word, glued) in &cmd.redirs {
            let target = match (word, glued) {
                (_, Some(g)) => g.clone(),
                (Some(w), None) => literal(w).unwrap_or_default(),
                (None, None) => String::new(),
            };
            let harmless = op.ends_with('>')
                && (target == "/dev/null"
                    || target.strip_prefix('&').is_some_and(|fd| {
                        fd == "-" || (!fd.is_empty() && fd.bytes().all(|b| b.is_ascii_digit()))
                    }));
            if !harmless {
                return Err(format!("a `{op}` redirect on the rm"));
            }
        }
        let made = union(made, &self.made);
        let mut after_dd = false;
        let mut operands = 0usize;
        for w in args {
            let lit = literal(w);
            if !after_dd && let Some(l) = lit.as_deref() {
                if l == "--" {
                    after_dd = true;
                    continue;
                }
                if l.starts_with('-') && l != "-" {
                    if l == "--no-preserve-root" {
                        return Err("rm --no-preserve-root".to_string());
                    }
                    if !is_known_flag(l) {
                        return Err(format!("an rm flag this resolver does not know: {l}"));
                    }
                    continue;
                }
            }
            // A directory the line itself just made: proven wherever it is.
            let fresh = self.fresh_dir(w, &made);
            if let Some(Ok(target)) = fresh {
                self.targets.push(target);
                operands += 1;
                continue;
            }
            let op = self.operand(w)?;
            if !after_dd && op.text.starts_with('-') {
                return Err("an rm operand that expands to a flag".to_string());
            }
            if let Some((_, var)) = op.runs.iter().find(|(r, _)| !made.contains(r))
                && !op.alone
            {
                let failed = if op.failed.is_empty() {
                    "empty".to_string()
                } else {
                    clip(&op.failed, 60)
                };
                let fix = format!(
                    "run the rm behind `&&` after the assignment, or write \"${{{var}:?}}\""
                );
                return Err(if op.glob {
                    format!(
                        "a glob behind a $(mktemp) variable (were mktemp to fail, ${var} is \
                         empty and the operand is {failed}); {fix}"
                    )
                } else {
                    format!(
                        "${var} is a `$(mktemp -d)` that can fail, leaving it empty, and the \
                         rm then removes {failed}; {fix}"
                    )
                });
            }
            // Built on a `mktemp -d` made where the line does not say: only
            // the fresh-directory rule could prove it — wherever in the
            // operand the directory's name sits (`"$S/$D"` is not under `$S`
            // if `$TMPDIR` climbs with `..`).
            if op.text.contains(FRESH_DIR) {
                return Err(match fresh {
                    Some(Err(why)) => why,
                    _ => format!(
                        "{}: a path built on a directory `mktemp -d` makes is proven only as \
                         \"$D\" or a path under it",
                        rendered(&op.text)
                    ),
                });
            }
            let target = self.check(&op.text)?;
            self.targets.push(target);
            operands += 1;
        }
        if operands == 0 {
            return Err("an rm with no operand".to_string());
        }
        Ok(())
    }

    /// THE FRESH-DIRECTORY RULE (module header, "A directory the line itself
    /// just made"): `Some(Ok(target))` when `word` is `"$D"` or lies
    /// lexically under it, `D` exactly one `$(mktemp -d …)` run's output,
    /// that run proven made where the rm runs (`made`, or the operand's own
    /// `${D:?}`) unless the operand is `"$D"` alone, the operand quoted and,
    /// for a path under `$D`, every use of `$D` on the line double-quoted,
    /// and the rule not withheld ([`RmScope::roots`]);
    /// `Some(Err(why))` when `word` starts with such a `D` and one of those
    /// fails; `None` when it does not start with one.
    fn fresh_dir(&self, word: &Word, made: &[u32]) -> Option<Result<String, String>> {
        let (
            Part::Var {
                name,
                quoted,
                nonempty,
            },
            rest,
        ) = word.split_first()?
        else {
            return None;
        };
        let Val::Known {
            text,
            runs,
            whole: true,
            ..
        } = self.lookup(name)
        else {
            return None;
        };
        let run = *runs.first()?;
        let refused = |why: &str| {
            Some(Err(format!(
                "${name} is a directory `mktemp -d` makes, proven only as \"${name}\" or a path \
                 under it: {why}"
            )))
        };
        if self.scope.roots.is_empty() {
            return refused("the rm rule is withheld (no scratch root), so nothing is proven");
        }
        let unquoted = || {
            refused(&format!(
                "every use of ${name} on the line must be double-quoted (its directory's name \
                 may hold a space or a glob)"
            ))
        };
        if !quoted {
            return unquoted();
        }
        let mut under = String::new();
        for p in rest {
            let Part::Lit { text: t, quoted } = p else {
                return refused("another expansion follows it");
            };
            if has_glob(t) || (!quoted && t.contains(['{', '}'])) {
                return refused("a glob or a brace follows it");
            }
            under.push_str(t);
        }
        // `"$D"` alone is the directory, or `""`, whatever an unquoted `$D`
        // did elsewhere; a path under it asks that none split or glob it.
        if !under.is_empty() && self.unquoted.contains(name.as_str()) {
            return unquoted();
        }
        if !(under.is_empty() || under.starts_with('/')) {
            return refused(
                "text glued to its name names a sibling of the directory, not a path in it",
            );
        }
        if under.split('/').any(|c| c == "..") {
            return refused("a .. under it climbs out of it");
        }
        // `"$D"` alone is `""` when the run failed, which removes nothing;
        // anything under it would then be under `/`.
        if !under.is_empty() && !nonempty && !made.contains(&run) {
            return refused(&format!(
                "the rm may run with the `mktemp` failed and ${name} empty (run it behind `&&` \
                 after the assignment, or write \"${{{name}:?}}\")"
            ));
        }
        Some(Ok(rendered(&format!("{text}{under}"))))
    }

    /// An operand after expansion ([`Operand`]).
    fn operand(&self, word: &Word) -> Result<Operand, String> {
        let mut op = Operand {
            text: String::new(),
            glob: false,
            runs: Vec::new(),
            failed: String::new(),
            alone: false,
        };
        let mut pieces = 0usize;
        let mut whole_var = false;
        for (k, p) in word.iter().enumerate() {
            match p {
                Part::Lit { text: t, quoted } => {
                    if !quoted {
                        if k == 0 && t.starts_with('~') {
                            return Err("~ in an rm operand".to_string());
                        }
                        if t.contains(['{', '}']) {
                            return Err("brace expansion in an rm operand".to_string());
                        }
                    }
                    // A quoted `*` is a literal name; judged as a glob
                    // anyway (a tie breaks toward escalating).
                    op.glob |= has_glob(t);
                    op.text.push_str(t);
                    op.failed.push_str(t);
                    pieces += usize::from(!t.is_empty());
                }
                Part::Var {
                    name,
                    quoted,
                    nonempty,
                } => {
                    if name == "HOME" {
                        return Err("$HOME in an rm operand".to_string());
                    }
                    if !valid_name(name) || name == "_" {
                        return Err(format!("${name} (a positional or special parameter)"));
                    }
                    match self.lookup(name) {
                        Val::Known {
                            text: t,
                            runs,
                            failed,
                            whole,
                        } => {
                            if !quoted && (t.chars().any(char::is_whitespace) || has_glob(&t)) {
                                return Err(format!(
                                    "${name} is split or globbed unquoted (its value holds a space or a glob)"
                                ));
                            }
                            pieces += 1;
                            op.glob |= has_glob(&t);
                            op.text.push_str(&t);
                            if *nonempty && whole {
                                op.failed.push_str(&t);
                            } else {
                                op.failed.push_str(&failed);
                                op.runs.extend(runs.into_iter().map(|r| (r, name.clone())));
                                whole_var = whole;
                            }
                        }
                        Val::Forgotten(by) => {
                            return Err(format!(
                                "${name} is not known after `{by}`, which may assign it (it \
                                 names or evaluates it, or holds a word this resolver cannot \
                                 read)"
                            ));
                        }
                        Val::Unknown if name == "PWD" => {
                            return Err("$PWD (the Bash tool's working directory is not \
                                        known to the supervisor)"
                                .to_string());
                        }
                        Val::Unknown if self.env.contains_key(name) => {
                            return Err(format!("${name} is not assigned a literal on this line"));
                        }
                        Val::Unknown => {
                            return Err(format!(
                                "${name} is not assigned on this line (a value from the \
                                 environment is not proven)"
                            ));
                        }
                    }
                }
                Part::Subst { .. } => {
                    return Err("a command substitution in an rm operand".to_string());
                }
                Part::Opaque(src) => {
                    return Err(format!(
                        "`{}` in an rm operand, an expansion zsh computes that this resolver \
                         does not",
                        clip(src, 20)
                    ));
                }
            }
        }
        op.alone = pieces == 1 && whole_var;
        Ok(op)
    }

    /// Where the operand points, and whether it may point there: as written,
    /// then on the disk ([`Self::on_disk`]).
    fn check(&self, text: &str) -> Result<String, String> {
        if text.is_empty() {
            return Err("an empty rm operand".to_string());
        }
        if !text.starts_with('/') {
            return Err(format!(
                "a relative rm operand ({}; the Bash tool's working directory is not known \
                 to the supervisor)",
                clip(text, 60)
            ));
        }
        let mut comps = Vec::new();
        for c in text.split('/') {
            match c {
                "" | "." => {}
                ".." => return Err("a .. component in an rm operand".to_string()),
                c if c.starts_with('.') && has_glob(c) => {
                    return Err("a glob that can match .. in an rm operand".to_string());
                }
                c => comps.push(c.to_string()),
            }
        }
        let shown = rendered(&join(&comps));
        let home = self
            .scope
            .home
            .and_then(|h| abs_components(&h.to_string_lossy()));
        inside(&comps, &shown, &self.cwd, home.as_deref(), self.scope.roots)?;
        // A trailing `/` (or `/.`) makes rm follow the last component too.
        let follow_last = text
            .rsplit('/')
            .next()
            .is_some_and(|last| last.is_empty() || last == ".");
        self.on_disk(&comps, follow_last, &shown)?;
        Ok(shown)
    }

    /// The operand's directories resolved on the disk ([`physical`]), and the
    /// path they lead to checked again, against the cwd, the home directory
    /// and the roots as the disk has them ([`Disk`]).
    fn on_disk(&self, comps: &[String], follow_last: bool, shown: &str) -> Result<(), String> {
        let dirs = if follow_last {
            comps.len()
        } else {
            comps.len().saturating_sub(1)
        };
        if let Some(c) = comps[..dirs].iter().find(|c| has_glob(c)) {
            return Err(format!(
                "{shown}: a glob in one of its directories ({}) can match a symbolic link",
                clip(c, 30)
            ));
        }
        let real = physical(&comps[..dirs]).map_err(|e| format!("{shown}: {e}"))?;
        let mut real = real.comps;
        real.extend(comps[dirs..].iter().cloned());
        let led = if real == comps {
            format!("{shown} (on the disk)")
        } else {
            format!(
                "{shown} leads through a symbolic link to {}",
                rendered(&join(&real))
            )
        };
        let cwd = self
            .disk
            .cwd
            .as_deref()
            .map_err(|e| format!("{shown}: the session cwd on the disk: {e}"))?;
        inside(
            &real,
            &led,
            cwd,
            self.disk.home.as_deref(),
            &self.disk.roots,
        )
    }
}

/// Whether `comps` is past every [`critical_path`] (with `home`), is not
/// `cwd` or an ancestor of it, and is strictly inside one of `roots`;
/// `shown` names it in the reason.
fn inside(
    comps: &[String],
    shown: &str,
    cwd: &[String],
    home: Option<&[String]>,
    roots: &[ScratchRoot],
) -> Result<(), String> {
    if let Some(name) = critical_path(comps, home) {
        return Err(format!("{shown}: {name}"));
    }
    // ASCII-case-insensitively: the default macOS volume is, and a
    // differently-cased spelling of the cwd is the cwd.
    if comps.len() <= cwd.len()
        && comps
            .iter()
            .zip(cwd)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        return Err(format!("{shown} is the session cwd or an ancestor of it"));
    }
    if roots.iter().any(|r| r.holds_strictly_unglobbed(comps)) {
        Ok(())
    } else {
        Err(format!("{shown} is not strictly inside a scratch root"))
    }
}

/// An absolute path as the disk resolves it ([`physical`]).
struct OnDisk {
    comps: Vec<String>,
    /// Whether a symbolic link on the way is not root's: the worker could
    /// have made it, or re-pointed it.
    foreign_link: bool,
}

/// Where the absolute path `comps` leads on the disk, walked as the kernel
/// walks it: each component that is a symbolic link replaced by its
/// target (an absolute target from `/`, a relative one from the link's
/// directory, a `..` in it the physical parent) until one does not exist,
/// and from there the rest as written — nothing below a missing directory
/// can be a link yet. `Err` when a link's target climbs out of a missing
/// directory with `..`, when the links do not end ([`MAX_LINK_HOPS`]), or
/// when the disk refuses the look (a permission, a file where a directory
/// should be): a path the resolver cannot see is not proven.
///
/// Not [`super::approval`]'s `real_components`, the Read rule's resolver,
/// for three reasons: that one asks `canonicalize`, which answers where a
/// path leads but not WHOSE links led it there, and a root reached through
/// a link the worker could re-point is no root here ([`OnDisk::foreign_link`],
/// [`ScratchRoot::on_disk`]); it follows the LAST component too, as a Read
/// does, where rm removes a link itself and follows only the directories
/// above it (the caller passes those); and it gives up on a link that leads
/// nowhere, where this one walks on as written to where a directory made
/// there later would put the rm (a missing directory, then only a `..`
/// that climbs out of one, is refused).
fn physical(comps: &[String]) -> Result<OnDisk, String> {
    let mut todo: VecDeque<OsString> = comps.iter().map(OsString::from).collect();
    let mut real = PathBuf::from("/");
    let mut hops = 0usize;
    let mut foreign_link = false;
    while let Some(c) = todo.pop_front() {
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." {
            real.pop();
            continue;
        }
        let path = real.join(&c);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_LINK_HOPS {
                    return Err(format!(
                        "{} leads through more than {MAX_LINK_HOPS} symbolic links",
                        path.display()
                    ));
                }
                foreign_link |= !owned_by_root(&m);
                let to = std::fs::read_link(&path).map_err(|e| {
                    format!("the symbolic link {} cannot be read ({e})", path.display())
                })?;
                if to.has_root() {
                    real = PathBuf::from("/");
                }
                for part in to.components().rev() {
                    match part {
                        Component::Normal(n) => todo.push_front(n.to_os_string()),
                        Component::ParentDir => todo.push_front(OsString::from("..")),
                        Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
                    }
                }
            }
            Ok(_) => real.push(&c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                real.push(&c);
                while let Some(rest) = todo.pop_front() {
                    if rest == ".." {
                        return Err(format!(
                            "{} does not exist, and a symbolic link's `..` climbs out of it",
                            path.display()
                        ));
                    }
                    if !rest.is_empty() && rest != "." {
                        real.push(&rest);
                    }
                }
            }
            Err(e) => {
                return Err(format!(
                    "{} cannot be examined on the disk ({e})",
                    path.display()
                ));
            }
        }
    }
    let comps = real
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok(OnDisk {
        comps,
        foreign_link,
    })
}

/// Whether root owns this link: then no one else made it or can re-point it
/// (macOS's `/tmp -> private/tmp`, `/var -> private/var`).
#[cfg(unix)]
fn owned_by_root(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    m.uid() == 0
}

#[cfg(not(unix))]
fn owned_by_root(_: &std::fs::Metadata) -> bool {
    false
}

/// `mktemp -d [-q] [-t PREFIX]` → [`FRESH_DIR`] (under the Bash tool's
/// `$TMPDIR`, which is never proven: module header); `mktemp -d [-q]
/// /abs/dir/name.XXXX` → `/abs/dir/<mktemp>`; a relative template →
/// [`FRESH_DIR`] (under the Bash tool's working directory). Anything else
/// (a file — no `-d` —, `-p DIR`, an expansion) is not resolved.
fn mktemp_dir(subst: &str) -> Option<String> {
    let words: Vec<&str> = subst.split_whitespace().collect();
    if words.first() != Some(&"mktemp") {
        return None;
    }
    let mut dir_flag = false;
    let mut template: Option<&str> = None;
    let mut k = 1;
    while let Some(w) = words.get(k).copied() {
        match w {
            "-d" | "--directory" => dir_flag = true,
            "-q" | "--quiet" => {}
            "-dq" | "-qd" => dir_flag = true,
            "-t" => {
                let prefix = words.get(k + 1)?;
                if prefix.contains(['/', '$']) {
                    return None;
                }
                k += 1;
            }
            w if !w.starts_with('-') && template.is_none() => template = Some(w),
            _ => return None,
        }
        k += 1;
    }
    if !dir_flag {
        return None;
    }
    match template {
        None => Some(FRESH_DIR.to_string()),
        Some(t) => {
            if t.contains(['$', '`', '~', '*', '?', '[']) || !t.ends_with("XXX") {
                return None;
            }
            match t.rsplit_once('/').filter(|_| t.starts_with('/')) {
                Some((dir, _)) => {
                    abs_components(dir)?;
                    Some(format!("{dir}/{MKTEMP_NAME}"))
                }
                None => Some(FRESH_DIR.to_string()),
            }
        }
    }
}

/// The components of an absolute path, `.` and empty ones dropped; `None` for
/// a relative path or one with a `..`.
pub(crate) fn abs_components(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix('/')?;
    let mut out = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => out.push(c.to_string()),
        }
    }
    Some(out)
}

/// BSD and GNU `rm` flags that only change HOW the named operands are
/// removed. `-W` (BSD undelete) and `--help`/`--version` are not deletions and
/// are left unrecognised, and `--no-preserve-root` is refused by name before
/// this is asked.
fn is_known_flag(word: &str) -> bool {
    if let Some(long) = word.strip_prefix("--") {
        return matches!(
            long,
            "force"
                | "recursive"
                | "dir"
                | "verbose"
                | "one-file-system"
                | "preserve-root"
                | "preserve-root=all"
                | "interactive"
                | "interactive=never"
                | "interactive=once"
                | "interactive=always"
        );
    }
    match word.strip_prefix('-') {
        Some(cluster) if !cluster.is_empty() => cluster.chars().all(|c| "dfiIPrRvx".contains(c)),
        _ => false,
    }
}

/// Whether `s` holds a character by which zsh globs a word into more names
/// ([`GLOB_CHARS`]: `*`, `?`, `[`, and `EXTENDED_GLOB`'s `^`, `#` and `(` —
/// `rm -rf /tmp/w/^x` removes every name there but `x`).
fn has_glob(s: &str) -> bool {
    s.contains(GLOB_CHARS)
}

/// The paths no operand may name whatever root holds it, compared
/// ASCII-case-insensitively (the default macOS volume is): `Some(its name)`
/// for the filesystem root, the home directory, an ancestor of it or a glob
/// directly inside it, `/Users` or `/home` or a direct child of either
/// (somebody's home), a glob in the first component (`/*`, `/u*/x`), and any
/// `.git` component.
fn critical_path(comps: &[String], home: Option<&[String]>) -> Option<&'static str> {
    let prefix_of = |a: &[String], b: &[String]| {
        a.len() <= b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
    };
    if comps.is_empty() {
        Some("filesystem root")
    } else if home.is_some_and(|h| {
        prefix_of(comps, h)
            || (comps.len() == h.len() + 1
                && prefix_of(h, comps)
                && comps.last().is_some_and(|c| has_glob(c)))
    }) {
        Some("home")
    } else if comps.len() <= 2
        && (comps[0].eq_ignore_ascii_case("Users") || comps[0].eq_ignore_ascii_case("home"))
    {
        Some("/Users")
    } else if has_glob(&comps[0]) {
        Some("glob at root")
    } else if comps.iter().any(|c| c.eq_ignore_ascii_case(".git")) {
        Some("component .git")
    } else {
        None
    }
}

fn join(comps: &[String]) -> String {
    format!("/{}", comps.join("/"))
}

/// `s` cut to its first `n` characters, `…` where it was cut: the policy's
/// one clip — an operand quoted in a reason (60), and a decline's reason, its
/// quoted invocation and a refusal's label ([`super::approval`]).
pub(super) fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
#[path = "rm_breaker_tests.rs"]
mod tests;
