// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell hooks: `~/.aterm/shell.d/00-atpkg.{zsh,bash,fish,ps1}`, generated from compiled-in
//! templates only, never artifact-supplied. Never a POSIX `.sh` (fish sources `*.sh`); a
//! stray one is deleted. The `.ps1` serves PowerShell, where no POSIX hook runs.
//!
//! The hook appends the managed `bin/` (as [`crate::store::append_bin_to_path`] does), so a
//! managed tool never shadows system `sudo`/`ssh`/`git`. Outside aterm it is what puts `bin/`
//! on an interactive shell's PATH; inside, the spawn seam appends the same entry from the
//! first tab on ([`crate::session_bin_dir`]). `bin/` is unconditional, since the Trust
//! toolchain is the default compiler in any terminal, and written first, so a parse error in
//! the gated half cannot cost it: a shell abandons a sourced file at the error.
//!
//! The gated half applies only inside aterm (any of [`AGENTS_MARKERS`] set):
//!
//! * `<prefix>/agents/` ([`crate::store::Layout::agents_dir`]), which holds only the agent
//!   programs' shims ([`crate::stub::AGENT_PROGRAMS`]) and so cannot shadow a system tool,
//!   moves to the front (`ATPKG_AGENTS`), so the managed agent runs ahead of a vendor's
//!   install. Outside aterm the user's own install runs
//!   (docs/DESIGN-which-copy-runs-2026-08-27.md).
//! * `<prefix>/reroute` ([`crate::reroute`]) goes in front of that: its stubs only answer
//!   when they come first, and a login shell's `path_helper` and rc lines push the spawn
//!   seam's front-insert down the PATH. A window tab's shell integration re-asserts it, but
//!   the transparent-session lane has none, and the rc-sourced hook is the one thing both
//!   run after the user's startup files. So the gated arm writes `reroute:agents:…`, except
//!   under `aterm --no-reroute` ([`crate::reroute::PASSTHROUGH_ENV`]). For this dir alone a
//!   non-empty [`crate::reroute::REROUTE_DIR_ENV`] also counts as inside aterm.
//!
//! Outside aterm the false arm takes both dirs out, so a shell that inherited an aterm PATH
//! heals. Each dir is moved to the front, never skipped when present: on a macOS login
//! shell `path_helper` and rc prepends put other copies ahead, so the failure is an order,
//! not a presence. aterm's shell integration re-asserts `ATPKG_AGENTS` after the user's rc
//! files have run.
//!
//! `~/.aterm/shell.d` is sourced by aterm's shell integration inside a session, and from the
//! user's own rc through the marker-bounded block [`ensure_rc_sources_hooks`] writes (which
//! rc files, and why), so an ordinary interactive shell (Terminal.app, ssh, an agent's shell)
//! gets the tools too (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md). An rc that exists is
//! edited, and one file may be created: the rc the account's login shell reads, when it is
//! absent ([`login_rc`]; a new Mac has no `~/.zshrc`, and without it Trust is never on PATH
//! in Terminal.app). No other shell gains a file (`aterm pkg doctor` prints the PATH line for
//! those), and an rc that resolves under a macOS-protected folder ([`crate::protected`]) is
//! never opened or created: the wiring runs unattended and would raise a consent dialog with
//! nobody at the screen.
//!
//! `~/.aterm/shell.d` is outside the managed prefix, so [`refresh`] hardens `~/.aterm` and
//! `~/.aterm/shell.d` with `ensure_private_dir` (symlink-refusing, `0700`, fail-closed)
//! before writing, and each file is written `0600` via temp + rename. Best-effort: a hook
//! write never fails an install.

use std::fs;
use std::io;
use std::path::Path;

use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// The hook file base name (`00-` sorts first).
pub const HOOK_BASENAME: &str = "00-atpkg";

/// The environment variables whose union (any one non-empty) says a process runs inside an
/// aterm session, where the managed agent programs lead. A union because no single marker
/// covers every lane: the GUI exports `ATERM_CHILD` and `ATERM_SESSION_ID` but not
/// `ATERM_AGENTS_DIR`, and `aterm --session` launched from another terminal carries only
/// `ATERM_AGENTS_DIR`. `TERM_PROGRAM` is not one: any rc can reset it. `aterm claude` runs
/// the managed copy from any terminal.
///
/// The one list: every dialect's agents gate in [`hook_files`] and the agents' reroute stubs
/// ([`crate::reroute::agents_stub_body_sh`]) are rendered from it, so the hook (at shell
/// start) and the stub (when the program runs) agree.
pub const AGENTS_MARKERS: &[&str] = &["ATERM_AGENTS_DIR", "ATERM_CHILD", "ATERM_SESSION_ID"];

/// [`AGENTS_MARKERS`] as one POSIX word that is empty exactly when none is set:
/// `${ATERM_AGENTS_DIR-}${ATERM_CHILD-}…` — each defaulted, so `set -u` cannot abort the
/// hook or the stub that reads it.
#[must_use]
pub fn agents_markers_sh() -> String {
    AGENTS_MARKERS
        .iter()
        .map(|m| format!("${{{m}-}}"))
        .collect()
}

/// The hook file `shell` (the basename of `$SHELL`) sources: `00-atpkg.zsh`, `.bash`,
/// `.fish`, or `.ps1` for `pwsh`/`powershell`. Not `sh`: the bash body uses
/// `${var//pat/rep}`, which dash rejects, so a `$SHELL=/bin/sh` user gets the `PATH` line.
/// `None` for a shell with no hook (nushell, xonsh, cmd). One table for `rc_hook_wired`,
/// [`hook_file`] and the remedy `doctor`/`which` print ([`crate::cli::shell_remedy_command`]).
#[must_use]
pub fn hook_for_shell(shell: &str) -> Option<&'static str> {
    match shell {
        "zsh" => Some("00-atpkg.zsh"),
        "bash" => Some("00-atpkg.bash"),
        "fish" => Some("00-atpkg.fish"),
        "pwsh" | "powershell" => Some("00-atpkg.ps1"),
        _ => None,
    }
}

/// `<home>/.aterm/shell.d/<hook>` and the hook's file name, when the hook for `shell` exists
/// there as a regular file (a symlink is not ours); `None` for a shell without a hook or a
/// hook never written. `doctor`/`which` source the hook in place where it stands and print a
/// `PATH` line only where it does not.
#[must_use]
pub fn hook_file(home: &Path, shell: &str) -> Option<(std::path::PathBuf, &'static str)> {
    let hook = hook_for_shell(shell)?;
    let path = home.join(".aterm").join("shell.d").join(hook);
    fs::symlink_metadata(&path)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|_| (path, hook))
}

/// Whether `~/.aterm/shell.d` under `home` holds an atpkg hook for any shell ([`hook_file`]).
/// [`write_hooks`] lays every dialect or none, so one stands for all.
#[must_use]
pub fn written(home: &Path) -> bool {
    ["zsh", "bash", "fish", "pwsh"]
        .into_iter()
        .any(|shell| hook_file(home, shell).is_some())
}

/// Whether `shell` (`zsh`, `bash`, `fish`) has an rc under `home` that sources the atpkg
/// hook, paired with the hook's file name; `None` for a shell the table does not know. Read
/// off the rc itself: the marker block [`ensure_rc_sources_hooks`] writes, or a hand-written
/// line naming the hook file. An rc under a protected root is not opened (the writer's
/// consent fence), so it answers "not wired" there.
///
/// A fact, not a remedy: `exec $SHELL` inside an aterm tab loses the tab's shell
/// integration, so [`crate::cli::shell_remedy_command`] keys only on whether the hook file
/// exists ([`hook_file`]).
#[must_use]
#[cfg(all(test, unix))]
pub(crate) fn rc_hook_wired(home: &Path, shell: &str) -> Option<(bool, &'static str)> {
    let rcs: &[&str] = match shell {
        "zsh" => &[".zshrc", ".zprofile"],
        "bash" | "sh" => &[".bashrc", ".bash_profile", ".profile"],
        "fish" => &[".config/fish/config.fish"],
        _ => return None,
    };
    let hook = hook_for_shell(shell)?;
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let wired = rcs.iter().any(|rc| {
        let Ok(real) = fs::canonicalize(home.join(rc)) else {
            return false;
        };
        if crate::protected::under_protected_root(&canonical_home, &real) {
            return false;
        }
        fs::read_to_string(&real).is_ok_and(|body| body.contains(RC_BEGIN) || body.contains(hook))
    });
    Some((wired, hook))
}

/// The `(filename, content)` for each shell dialect: `bin_dir` appended, `agents_dir` moved
/// to the front and `reroute_dir` in front of that, inside aterm only (module doc). Never a
/// `.sh`: fish sources `*.sh`, which a POSIX body would break.
#[must_use]
pub fn hook_files(bin_dir: &Path, agents_dir: &Path, reroute_dir: &Path) -> Vec<(String, String)> {
    let bin = sh_quote(bin_dir);
    let agents = sh_quote(agents_dir);
    let reroute = sh_quote(reroute_dir);
    // fish is not a POSIX shell and its double quotes are not sh's, so the fish body below
    // is rendered through its own quoter ([`fish_quote`]), never this one.
    let fish_bin = fish_quote(bin_dir);
    let fish_agents = fish_quote(agents_dir);
    let fish_reroute = fish_quote(reroute_dir);
    // The `aterm --no-reroute` marker, read as the stubs read it
    // ([`crate::reroute::engaged`]: non-empty and not "0").
    let passthrough = crate::reroute::PASSTHROUGH_ENV;
    // The spawn seams' reroute handle ([`crate::reroute::REROUTE_DIR_ENV`]), read and never
    // set: one more "inside aterm" marker, for the reroute half only.
    let reroute_marker = crate::reroute::REROUTE_DIR_ENV;
    // The agents gate in each dialect, rendered from the one list ([`AGENTS_MARKERS`]).
    let markers_sh = agents_markers_sh();
    let markers_fish: String = AGENTS_MARKERS.iter().map(|m| format!("${m}")).collect();
    let markers_ps: String = AGENTS_MARKERS
        .iter()
        .map(|m| format!("$env:{m}"))
        .collect::<Vec<_>>()
        .join(" -or ");
    // POSIX (zsh + bash, down to macOS's bash 3.2): bin/ is an idempotent append; agents and
    // reroute move to the front only inside aterm. Paths are quoted for a space in
    // "Application Support". The bin half comes first in every dialect (module doc); that
    // matters most for fish and PowerShell, whose shells a stock macOS lacks to test with.
    //
    // The false arm demotes rather than merely declining to promote: it writes PATH back
    // without either dir and unsets ATPKG_AGENTS, so a shell that inherited an agents-first
    // PATH (an iTerm launched from an aterm tab) heals.
    //
    // The move frames PATH as `:$PATH:` so /opt/x never matches /opt/xy, removes every
    // `:dir:` to a fixpoint by POSIX `[ = ]` (never `[[ == ]]`, which `nocasematch` bends),
    // then strips one framing colon from each end so an empty entry (the user's "here")
    // survives. Both arms decide on the framed remainder: only `:` means nothing is left,
    // and the stripped string would drop a sole empty entry (`:dir`, `dir:`, `dir::dir`).
    //
    // `__atpkg_front` is built in two steps: agents iff a marker (ATPKG_AGENTS exported with
    // it, unset otherwise), then the reroute dir ahead of it iff a marker or a non-empty
    // `$ATERM_REROUTE_DIR`, and the passthrough marker not engaged. Only aterm's seams set
    // that handle, and each blanks an inherited one under `--no-reroute` or with nothing
    // laid, so it never widens the agents half. An empty front writes the false arm's PATH.
    let posix = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         __atpkg_bin=\"{bin}\"\n\
         case \":$PATH:\" in *\":$__atpkg_bin:\"*) ;; *) export PATH=\"$PATH:$__atpkg_bin\" ;; esac\n\
         export ATPKG_BIN=\"$__atpkg_bin\"\n\
         unset __atpkg_bin\n\
         __atpkg_agents=\"{agents}\"\n\
         __atpkg_reroute=\"{reroute}\"\n\
         __atpkg_p=\":$PATH:\"\n\
         while :; do __atpkg_q=\"${{__atpkg_p//\":$__atpkg_agents:\"/:}}\"; __atpkg_q=\"${{__atpkg_q//\":$__atpkg_reroute:\"/:}}\"; [ \"$__atpkg_q\" = \"$__atpkg_p\" ] && break; __atpkg_p=\"$__atpkg_q\"; done\n\
         __atpkg_front=\n\
         case \"{markers_sh}\" in \"\") unset ATPKG_AGENTS ;; *) __atpkg_front=\"$__atpkg_agents\"; export ATPKG_AGENTS=\"$__atpkg_agents\" ;; esac\n\
         case \"${{{passthrough}-}}\" in \"\"|0) case \"{markers_sh}${{{reroute_marker}-}}\" in \"\") ;; *) __atpkg_front=\"$__atpkg_reroute${{__atpkg_front:+:}}$__atpkg_front\" ;; esac ;; esac\n\
         case \"$__atpkg_front\" in\n\
         \"\") case \"$__atpkg_p\" in :) export PATH=\"\" ;; *) __atpkg_p=\"${{__atpkg_p#:}}\"; __atpkg_p=\"${{__atpkg_p%:}}\"; export PATH=\"$__atpkg_p\" ;; esac ;;\n\
         *) case \"$__atpkg_p\" in :) export PATH=\"$__atpkg_front\" ;; *) __atpkg_p=\"${{__atpkg_p#:}}\"; __atpkg_p=\"${{__atpkg_p%:}}\"; export PATH=\"$__atpkg_front:$__atpkg_p\" ;; esac ;;\n\
         esac\n\
         unset __atpkg_agents __atpkg_reroute __atpkg_front __atpkg_p __atpkg_q\n"
    );
    // fish: both dirs are dropped by an explicit equality loop (`string match` would read a
    // directory as a wildcard pattern; the quoted `"$__atpkg_d"` keeps an empty entry), bin/
    // appended after $PATH. The list grows by `set __atpkg_rest $__atpkg_rest …`, never
    // `set -a`, which fish 2.x lacks (the list would stay empty and PATH would be the front
    // alone); a plain config.fish may run an old fish. `__atpkg_front` is declared at top
    // level (a `set -l` inside the `if` dies with the block) and built as the POSIX one is,
    // under a nested `if` so the passthrough test keeps the `if …; or …` shape fish 2.x
    // parses. An unset fish variable expands to nothing inside the quotes.
    let fish = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         set -l __atpkg_bin \"{fish_bin}\"\n\
         if not contains $__atpkg_bin $PATH; set -gx PATH $PATH $__atpkg_bin; end\n\
         set -gx ATPKG_BIN $__atpkg_bin\n\
         set -l __atpkg_agents \"{fish_agents}\"\n\
         set -l __atpkg_reroute \"{fish_reroute}\"\n\
         set -l __atpkg_front\n\
         if test -n \"{markers_fish}\"; set __atpkg_front \"$__atpkg_agents\"; set -gx ATPKG_AGENTS $__atpkg_agents; else; set -e ATPKG_AGENTS; end\n\
         if test -z \"${passthrough}\"; or test \"${passthrough}\" = 0; if test -n \"{markers_fish}${reroute_marker}\"; set __atpkg_front \"$__atpkg_reroute\" $__atpkg_front; end; end\n\
         set -l __atpkg_rest\n\
         for __atpkg_d in $PATH; if test \"$__atpkg_d\" != \"$__atpkg_agents\"; and test \"$__atpkg_d\" != \"$__atpkg_reroute\"; set __atpkg_rest $__atpkg_rest \"$__atpkg_d\"; end; end\n\
         set -gx PATH $__atpkg_front $__atpkg_rest\n"
    );
    // PowerShell (Windows, and pwsh anywhere): the aterm PowerShell integration dot-sources
    // `~/.aterm/shell.d/*.ps1`, so this is the only thing that puts the managed dirs on its
    // PATH. The POSIX policy, split on `[System.IO.Path]::PathSeparator` so it is correct
    // wherever pwsh runs; Windows lays no reroute stubs, so that entry finds nothing there.
    // `-or` and `-and` share one precedence, left to right, so the reroute gate's grouping
    // is spelled out.
    let ps_bin = ps_quote(bin_dir);
    let ps_agents = ps_quote(agents_dir);
    let ps_reroute = ps_quote(reroute_dir);
    let powershell = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         $__atpkg_sep = [System.IO.Path]::PathSeparator\n\
         $__atpkg_bin = '{ps_bin}'\n\
         if (($env:PATH -split [regex]::Escape($__atpkg_sep)) -notcontains $__atpkg_bin) {{ $env:PATH = \"$env:PATH$__atpkg_sep$__atpkg_bin\" }}\n\
         $env:ATPKG_BIN = $__atpkg_bin\n\
         $__atpkg_agents = '{ps_agents}'\n\
         $__atpkg_reroute = '{ps_reroute}'\n\
         $__atpkg_rest = @(($env:PATH -split [regex]::Escape($__atpkg_sep)) | Where-Object {{ $_ -ne $__atpkg_agents -and $_ -ne $__atpkg_reroute }})\n\
         $__atpkg_front = @()\n\
         if ({markers_ps}) {{ $__atpkg_front = @($__atpkg_agents); $env:ATPKG_AGENTS = $__atpkg_agents }} else {{ Remove-Item Env:\\ATPKG_AGENTS -ErrorAction SilentlyContinue }}\n\
         if (({markers_ps} -or $env:{reroute_marker}) -and (-not $env:{passthrough} -or $env:{passthrough} -eq '0')) {{ $__atpkg_front = @($__atpkg_reroute) + $__atpkg_front }}\n\
         $env:PATH = ($__atpkg_front + $__atpkg_rest) -join $__atpkg_sep\n\
         Remove-Variable __atpkg_agents, __atpkg_reroute, __atpkg_front, __atpkg_bin, __atpkg_sep, __atpkg_rest\n"
    );
    vec![
        (format!("{HOOK_BASENAME}.zsh"), posix.clone()),
        (format!("{HOOK_BASENAME}.bash"), posix),
        (format!("{HOOK_BASENAME}.fish"), fish),
        (format!("{HOOK_BASENAME}.ps1"), powershell),
    ]
}

/// Whether the hook at `dest` is already `content`: a regular file (never a link), those
/// exact bytes, and — on Unix — mode `0600`, the one [`stage_hook`] lays.
pub(crate) fn hook_is_current(dest: &Path, content: &str) -> bool {
    let Ok(meta) = fs::symlink_metadata(dest) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o777 != 0o600 {
            return false;
        }
    }
    crate::metadata_io::read_bounded_regular(dest, content.len() + 1)
        .is_ok_and(|have| have == content.as_bytes())
}

/// What stood at a hook's destination before this pass replaced it, so the publish phase
/// can be undone as a whole.
enum Prev {
    /// Nothing did: an unwind takes the file this pass laid back out.
    Absent,
    /// The previous pass's hook — a regular file — moved aside to this path.
    Saved(std::path::PathBuf),
    /// Something that is not a regular file (a directory, a link, a device), left where it
    /// is: the rename that follows fails on it, and an unwind has nothing to put back.
    Foreign,
}

/// Move the regular file standing at `dest` out of the way, so a later failure in the
/// publish loop can put it back. Best-effort: anything that cannot be saved is reported as
/// [`Prev::Foreign`] and left untouched.
fn move_aside(dest: &Path) -> Prev {
    let meta = match fs::symlink_metadata(dest) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Prev::Absent,
        Err(_) => return Prev::Foreign,
    };
    if !meta.is_file() {
        return Prev::Foreign;
    }
    let Some(name) = dest.file_name().and_then(|s| s.to_str()) else {
        return Prev::Foreign;
    };
    let saved = dest.with_file_name(format!(".{name}.prev-{}", std::process::id()));
    let _ = fs::remove_file(&saved);
    match fs::rename(dest, &saved) {
        Ok(()) => Prev::Saved(saved),
        Err(_) => Prev::Foreign,
    }
}

/// Put back what stood at `dest` before this pass replaced it — or take out the file it laid
/// where nothing stood. Best-effort: an unwind has nobody to report an error to.
fn restore_prev(dest: &Path, prev: Prev) {
    match prev {
        Prev::Absent => {
            let _ = fs::remove_file(dest);
        }
        Prev::Saved(saved) => {
            let _ = fs::rename(&saved, dest);
        }
        Prev::Foreign => {}
    }
}

/// Write the dialect hooks into `shell_d` (each `0600` via temp + rename) and delete a
/// stray POSIX `00-atpkg.sh`. Returns every dialect's file name now in place.
///
/// Only a hook whose bytes differ is written: a rename gives the file a new inode, and every
/// open zsh re-sources a hook whose inode moved at its next prompt.
pub fn write_hooks(
    shell_d: &Path,
    bin_dir: &Path,
    agents_dir: &Path,
    reroute_dir: &Path,
) -> io::Result<Vec<String>> {
    // Stage every temp, then rename every destination; both phases are all-or-nothing. The
    // dialects are only correct as a set, so a partial publish would leave shells on
    // different PATH policies. Each previous file is moved aside and put back if a later
    // rename fails, so a failed publish leaves `shell.d` as it was and the next pass retries
    // hooks and rc wiring together.
    let files = hook_files(bin_dir, agents_dir, reroute_dir);
    let current: Vec<String> = files
        .iter()
        .filter(|(name, content)| hook_is_current(&shell_d.join(name), content))
        .map(|(name, _)| name.clone())
        .collect();
    let mut staged = Vec::with_capacity(files.len());
    for (name, content) in files.iter().filter(|(name, _)| !current.contains(name)) {
        let dest = shell_d.join(name);
        match stage_hook(&dest, content) {
            Ok(tmp) => staged.push((tmp, dest, name.clone())),
            Err(e) => {
                for (tmp, _, _) in &staged {
                    let _ = fs::remove_file(tmp);
                }
                return Err(e);
            }
        }
    }
    let mut written = current;
    // What stood at each destination this pass has already replaced, oldest first, so a
    // rename that fails later can put every one of them back.
    let mut published: Vec<(std::path::PathBuf, Prev)> = Vec::with_capacity(staged.len());
    for (index, (tmp, dest, name)) in staged.iter().enumerate() {
        let prev = move_aside(dest);
        if let Err(e) = fs::rename(tmp, dest) {
            // This dialect never published: put its own predecessor straight back, then
            // unwind every dialect that did, newest first.
            restore_prev(dest, prev);
            for (done, was) in std::mem::take(&mut published).into_iter().rev() {
                restore_prev(&done, was);
            }
            // The temps not yet renamed are litter, and nobody sweeps `shell.d`.
            for (rest, _, _) in &staged[index..] {
                let _ = fs::remove_file(rest);
            }
            return Err(e);
        }
        published.push((dest.clone(), prev));
        written.push(name.clone());
    }
    // The whole set is published, so the predecessors it saved are this pass's litter.
    for (_, prev) in published {
        if let Prev::Saved(saved) = prev {
            let _ = fs::remove_file(&saved);
        }
    }
    // fish-safety: fish sources *.sh too, so remove a hand-dropped POSIX `.sh`.
    let stray = shell_d.join(format!("{HOOK_BASENAME}.sh"));
    if stray.exists() {
        let _ = fs::remove_file(&stray);
    }
    Ok(written)
}

/// The hooks under `home`'s `~/.aterm/shell.d` that are not as [`write_hooks`] lays them
/// for `layout` — missing, other bytes, another mode, a link in a hook's place ([`hook_is_current`])
/// — and the stray POSIX `00-atpkg.sh` it removes: what `base.shell.hooks` reads. Changes
/// nothing.
pub(crate) fn hooks_not_current(layout: &Layout, home: &Path) -> Vec<String> {
    let shell_d = home.join(".aterm").join("shell.d");
    let mut stale: Vec<String> = hook_files(
        &layout.bin_dir(),
        &layout.agents_dir(),
        &layout.reroute_dir(),
    )
    .into_iter()
    .filter(|(name, content)| !hook_is_current(&shell_d.join(name), content))
    .map(|(name, _)| name)
    .collect();
    let stray = format!("{HOOK_BASENAME}.sh");
    if shell_d.join(&stray).exists() {
        stale.push(stray);
    }
    stale
}

/// `home`'s `~/.aterm` and `~/.aterm/shell.d` that stand but not as [`lay_hooks`] leaves
/// them (`ensure_private_dir`): a symlink or a file in a directory's place, or (Unix) a
/// directory another uid owns or whose mode is not `0700`. Login shells source what is in
/// them, so `base.shell.hooks` reads these with the hooks and its apply hardens them, or
/// fails on what it may not harden. Changes nothing.
pub(crate) fn shell_dirs_not_private(home: &Path) -> Vec<std::path::PathBuf> {
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    [aterm, shell_d]
        .into_iter()
        .filter(|dir| fs::symlink_metadata(dir).is_ok_and(|meta| !laid_private(&meta)))
        .collect()
}

/// Whether `meta` (an `lstat`) is a directory as `ensure_private_dir` leaves it.
fn laid_private(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        meta.file_type().is_dir()
            && crate::platform::dir_meta_is_private(meta)
            && meta.mode() & 0o7777 == 0o700
    }
    #[cfg(not(unix))]
    {
        meta.file_type().is_dir()
    }
}

/// Harden `home`'s `~/.aterm` and `~/.aterm/shell.d` and write the hooks for `layout`
/// ([`write_hooks`]): the apply of `base.shell.hooks`, the first half of a pass
/// ([`refresh_at`]) without the rc wiring and the command links, which are items of their
/// own. `Err` names what refused.
pub(crate) fn lay_hooks(layout: &Layout, home: &Path) -> Result<(), String> {
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    for dir in [&aterm, &shell_d] {
        ensure_private_dir(dir).map_err(|e| {
            format!(
                "{} could not be made your own private (0700) directory ({e}); a symlink, a \
                 foreign owner or a group/other-writable mode there is refused",
                dir.display()
            )
        })?;
    }
    write_hooks(
        &shell_d,
        &layout.bin_dir(),
        &layout.agents_dir(),
        &layout.reroute_dir(),
    )
    .map(|_| ())
    .map_err(|e| format!("a hook in {} could not be written ({e})", shell_d.display()))
}

/// The command links [`ensure_command_links_from`] would write for the executable at `exe`
/// (canonical) under `home`, in the order it writes them: each with `None` where nothing
/// stands at its name, or with the target of the link into another `aterm.app` bundle it
/// re-points (a bundle moved from `~/Downloads` to `/Applications` leaves one dangling).
/// What `base.shell.links` reads, by the pass's own rule ([`link_act`]). Empty where this
/// executable lays none ([`command_link_source`]) or `~/.local/bin` is behind a consent
/// fence ([`command_links_dir`]). Changes nothing.
#[cfg(unix)]
pub(crate) fn command_links_to_write(
    home: &Path,
    exe: &Path,
) -> Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    let (Some(macos), Some(bin)) = (command_link_source(exe), command_links_dir(home)) else {
        return Vec::new();
    };
    let mut writes = Vec::new();
    // `amacs` follows `aterm` as the pass leaves it: re-pointed here, or as it stands.
    let mut aterm_settled = false;
    for name in COMMAND_LINKS {
        let target = macos.join(name);
        if !target.exists() {
            continue;
        }
        let link = bin.join(name);
        let settled = aterm_settled
            || fs::read_link(bin.join("aterm")).is_ok_and(|aterm| aterm == macos.join("aterm"));
        if let Some(act) = link_act(&link, &target, name, settled) {
            aterm_settled |= name == "aterm";
            writes.push((link, act));
        }
    }
    writes
}

/// The argv0 aliases [`heal_store_aliases`] would lay beside the store-installed aterm at
/// `exe` (canonical): each with nothing at its name. Empty where `exe` runs from no store.
#[cfg(unix)]
pub(crate) fn store_aliases_absent(exe: &Path) -> Vec<std::path::PathBuf> {
    store_dir_of(exe, canonical).map_or_else(Vec::new, |store| {
        aterm_types::argv0::ALIASES
            .into_iter()
            .map(|name| store.join(name))
            .filter(|alias| {
                fs::symlink_metadata(alias).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
            })
            .collect()
    })
}

/// What one hooks pass did, so a caller that prints it states what happened. No variant is
/// an error to a caller (a hooks pass never fails an install).
///
/// Only [`HookPass::Rewritten`] carries rc outcomes, since the rc step runs only once the
/// hooks are on disk ([`refresh_at`]); the list is empty off unix and where no `RC_FILES`
/// row exists.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HookPass {
    /// `~/.aterm` and `~/.aterm/shell.d` were hardened and the hook files are on disk as
    /// rendered — written, or already byte-identical and left alone ([`write_hooks`]).
    /// One [`RcOutcome`] per rc file that exists, in `RC_FILES` order.
    Rewritten(Vec<(&'static str, RcOutcome)>),
    /// The directories were hardened, a hook file could not be written, and the rc step was
    /// skipped: a block naming a missing hook sources nothing and no later pass rewrites it.
    HooksNotWritten,
    /// `$HOME` is unknown, or `~/.aterm` / `~/.aterm/shell.d` could not be made the
    /// user's own `0700` directory ([`ensure_private_dir`] refuses a symlink, a foreign
    /// owner or a group/other-writable mode): nothing was touched, no rc was read.
    NotHardened,
    /// The unattended pass ran from an app bundle that is not the release `aterm.app` (a dev
    /// bundle from `ship dev-app`, a renamed copy) and wrote nothing: the shell hooks, rc
    /// blocks and command links are the release's to lay ([`in_release_bundle`]), and a
    /// stale dev bundle would lay stale hooks. `atpkg repair` from such a bundle still writes.
    NotReleaseBundle,
}

impl HookPass {
    /// What the rc step did to each rc file that exists — empty unless the hooks were
    /// written, since the rc step does not run otherwise.
    #[must_use]
    pub fn rc(&self) -> &[(&'static str, RcOutcome)] {
        match self {
            Self::Rewritten(rc) => rc,
            Self::HooksNotWritten | Self::NotHardened | Self::NotReleaseBundle => &[],
        }
    }
}

/// What one pass did to one rc file: one that exists, or the login shell's own rc, the one
/// file a pass creates ([`login_rc`]), so `atpkg repair` prints a line per rc. People are
/// sent to `repair` for unrelated reasons too, so re-laying an opt-out is announced, never
/// implied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RcOutcome {
    /// The rc already carried atpkg's block; nothing was written to it.
    AlreadyWired,
    /// atpkg's block was appended to an rc that had never carried it.
    Appended,
    /// The login shell's rc did not exist, and atpkg created it holding only its block
    /// ([`login_rc`]); no other rc is ever created.
    Created,
    /// atpkg's block was laid again in an rc the user had deleted it from (recorded in
    /// `RC_LEDGER`, no marker present), or the login shell's rc atpkg created was made again
    /// after the user deleted the file. Only [`refresh_rewiring_rc`] does this.
    RelaidOverOptOut,
    /// The user had deleted atpkg's block (or the login shell's rc atpkg created) and this
    /// pass left it exactly as it was.
    OptOutKept,
    /// The rc resolves under a folder macOS guards with a consent dialog, so it was not
    /// even opened (the consent fence in `ensure_rc_sources_hooks`).
    ConsentFenced,
    /// The rc exists but is not something atpkg edits: a dangling link, not a regular
    /// file, or not UTF-8. Left untouched.
    Unreadable,
    /// The rewrite was attempted and failed; the rc is unchanged (the rc or its directory
    /// refused the write, or the temp could not be created exclusively beside it).
    WriteFailed,
}

/// The whole shell pass in one call: hardens `~/.aterm` + `~/.aterm/shell.d` (0700,
/// symlink-refusing), writes the hooks, wires the rc files (creating the login shell's
/// own rc when absent, [`login_rc`]) and lays the command links. NEVER propagates an
/// error — hooks are ergonomics, not trust; a failed write must not fail an install. Off
/// macOS the same pass first lays any argv0 alias missing beside a store-installed aterm
/// ([`ensure_store_aliases`]).
///
/// The provisioning lanes (the seed, the end of an install or update pass, doctor, init,
/// `repair`) run the same steps as the base relay items `base.shell.hooks`, `.rc` and
/// `.links` ([`crate::converge::repair`]), each probed and journaled; `uninstall --all`
/// runs this pass without creating an rc ([`refresh_creating_no_rc`]). The hooks name the
/// three managed directories and nothing installed in them, so they are valid before
/// anything installs.
///
/// The unattended pass, so its rc wiring honours the opt-out the block documents: an rc
/// wired once that no longer carries the block is left as the user left it
/// ([`RC_LEDGER`]). [`refresh_rewiring_rc`] writes it back.
pub fn refresh(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::HonorOptOut, account_login_rc)
}

/// [`refresh`], creating no rc: the pass `uninstall --all` runs, which provisions nothing
/// and so gives no shell a file it did not have. An rc that exists is wired as
/// [`refresh`] wires it.
pub fn refresh_creating_no_rc(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::HonorOptOut, |_| None)
}

/// [`refresh`], but the rc block is written back even where the user removed it: the
/// asked-for pass (`atpkg repair`) and only that, so opting out never costs a hand-edited
/// dotfile to undo. The returned [`HookPass`] names each rc it re-laid
/// ([`RcOutcome::RelaidOverOptOut`]), so `repair` says so.
pub fn refresh_rewiring_rc(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::Rewire, account_login_rc)
}

/// What a pass does with an rc whose block the user deleted: an unattended pass leaves it
/// deleted; `repair` writes it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RcWiring {
    /// An rc recorded in [`RC_LEDGER`] that no longer carries the block is skipped.
    HonorOptOut,
    /// Every eligible rc is wired, ledger or not.
    Rewire,
}

/// The pass, the rc it may create read from `home` by `creates` ([`account_login_rc`], or
/// none).
fn refresh_with(
    layout: &Layout,
    wiring: RcWiring,
    creates: fn(&Path) -> Option<&'static str>,
) -> HookPass {
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).ok();
    let home = aterm_types::dirs::home_dir();
    let login = home.as_deref().and_then(creates);
    refresh_as(
        layout,
        wiring,
        exe.as_deref(),
        home.as_deref(),
        HEALS_STORE,
        login,
    )
}

/// The development seam ([`aterm_types::dev_seam!`]) naming the login shell (a path, as the
/// account record spells it) a development build reads in place of the account record's:
/// a test's `chsh`, so a spawned pass can be given a login shell its host account does not
/// have. A shipped binary reads the account record whatever the environment holds.
pub const LOGIN_SHELL_SEAM: &str = "ATPKG_LOGIN_SHELL";

/// The account's login shell: the program the account record names (`getpwuid`,
/// [`aterm_uds::account::shell`]), never `$SHELL`, which a test, a wrapper or a parent
/// process may set. `None` when the record names none (and off unix).
#[must_use]
pub fn login_shell() -> Option<std::path::PathBuf> {
    if let Some(seam) = aterm_types::dev_seam!(LOGIN_SHELL_SEAM) {
        return Some(seam.into());
    }
    #[cfg(unix)]
    {
        aterm_uds::account::shell()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The [`RC_FILES`] row this account's login shell reads ([`login_rc`] of [`login_shell`],
/// fish's config under `$XDG_CONFIG_HOME`), which a pass creates when it is absent.
#[must_use]
pub fn account_login_rc(home: &Path) -> Option<&'static str> {
    let shell = login_shell()?;
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute());
    login_rc(&shell.to_string_lossy(), home, xdg_config.as_deref())
}

/// The [`RC_FILES`] row the login shell `shell` (a path or a bare name) reads, by the one
/// election `aterm pkg shell-setup --path` and `tools/install.sh` use
/// ([`crate::selfsetup::rc_target`]): `~/.zshrc` for zsh; for bash on macOS the first of
/// `~/.bash_profile`, `~/.bash_login`, `~/.profile` that is a file, else `~/.bash_profile`,
/// and `~/.bashrc` elsewhere; fish's `config.fish` under `xdg_config` (else `~/.config`).
///
/// `None` when the election picks a file that is no row (bash's `.bash_login` or
/// `.profile`, which a login bash already reads; fish under another `$XDG_CONFIG_HOME`), or
/// knows no such shell (sh, nushell): such a shell is never given a file.
#[must_use]
pub fn login_rc(shell: &str, home: &Path, xdg_config: Option<&Path>) -> Option<&'static str> {
    // `selfsetup` is a unix module, and off unix there is no rc row to elect.
    #[cfg(unix)]
    {
        let places = crate::selfsetup::Places::defaults(home, None, xdg_config);
        let env = crate::selfsetup::ShellEnv {
            shell: Some(shell.to_owned()),
            ..crate::selfsetup::ShellEnv::default()
        };
        let target = crate::selfsetup::rc_target(&places, &env)?;
        RC_FILES
            .iter()
            .map(|(rc, _)| *rc)
            .find(|rc| home.join(rc) == target)
    }
    #[cfg(not(unix))]
    {
        let _ = (shell, home, xdg_config);
        None
    }
}

/// Whether the hooks pass completes a store's argv0 aliases ([`heal_store_aliases`]): off
/// macOS only. A macOS app's aliases arrive with each update, which swaps the whole bundle,
/// and the macOS store lane is the source build, whose `install.sh` re-run lays them.
pub(crate) const HEALS_STORE: bool = cfg!(unix) && !cfg!(target_os = "macos");

/// [`refresh_with`] as the executable at `exe` (canonical) runs it, for the account whose
/// home is `home`, healing a store when `heals_store` ([`HEALS_STORE`]). All three are
/// injected so a test drives the pass's order on a scratch store on any platform.
fn refresh_as(
    layout: &Layout,
    wiring: RcWiring,
    exe: Option<&Path>,
    home: Option<&Path>,
    heals_store: bool,
    login: Option<&'static str>,
) -> HookPass {
    // The store's own argv0 aliases first: they are aterm's install, not the shell's, so
    // neither a missing $HOME nor an unhardened ~/.aterm holds them back.
    #[cfg(unix)]
    if heals_store && let Some(exe) = exe {
        heal_store_aliases(exe);
    }
    #[cfg(not(unix))]
    let _ = heals_store;
    let Some(home) = home else {
        return HookPass::NotHardened;
    };
    pass_as(layout, home, wiring, exe, login)
}

/// The store folder `tools/install.sh` lays (`~/.local/lib/aterm/bin`, or the folder
/// `ATERM_STORE_DIR` named) when the binary at `exe` runs from one: `exe`'s own folder,
/// when `exe` is named `aterm` and the `aterm-ctl` beside it is an alias of it, a link
/// that `resolve` takes to the same file as `exe`. The store holds the argv0 aliases
/// ([`aterm_types::argv0::ALIASES`]), each a link to `aterm` that the binary dispatches
/// on, so it is the folder a Linux window tab's PATH needs for them (`aterm-gui`'s
/// `spawn::linux_store_dir`) and the folder [`ensure_store_aliases`] fills: one rule, so
/// the folder on PATH is the folder the self-heal completes.
///
/// A build tree's `target/<profile>` is not a store: its `aterm-ctl` is a dev binary of
/// its own, which resolves to itself. `resolve` is injected (`fs::canonicalize` in
/// production) so the rule is tested without laying out a folder.
#[must_use]
pub fn store_dir_of(
    exe: &Path,
    resolve: impl Fn(&Path) -> Option<std::path::PathBuf>,
) -> Option<&Path> {
    let dir = exe.parent()?;
    if dir.as_os_str().is_empty() || exe.file_name()? != "aterm" {
        return None;
    }
    let binary = resolve(exe)?;
    (resolve(&dir.join("aterm-ctl"))? == binary).then_some(dir)
}

/// The self-heal of an install that predates an argv0 alias
/// (`docs/DESIGN-amacs-2026-10-19.md` §8.1): each [`aterm_types::argv0::ALIASES`] name
/// with nothing at it in the store folder `store` is laid as a link to `aterm`, the binary
/// beside it, spelled as `tools/install.sh` spells its own (`ln -sfn aterm`). The Linux
/// updater replaces only the binary, so without this a store laid before `amacs` existed
/// would never gain it short of a reinstall.
///
/// It never replaces anything: a file, a directory, or a link to anywhere (a dangling one
/// too) is left exactly as it is. Each link is made at its name by one `symlink(2)`, which
/// refuses a name something already holds, so there is no window between a check and the
/// write in which a file that appeared could be replaced. Best-effort and silent, as the
/// command links are: a store this account cannot write stays as it is. Returns the names
/// it laid.
#[cfg(unix)]
pub(crate) fn ensure_store_aliases(store: &Path) -> Vec<&'static str> {
    aterm_types::argv0::ALIASES
        .into_iter()
        .filter(|name| std::os::unix::fs::symlink("aterm", store.join(name)).is_ok())
        .collect()
}

/// [`ensure_store_aliases`] for the executable at `exe` (canonical) when it runs from a
/// store ([`store_dir_of`]); anywhere else, nothing. The pass runs it where
/// [`HEALS_STORE`] says.
#[cfg(unix)]
pub(crate) fn heal_store_aliases(exe: &Path) -> Vec<&'static str> {
    store_dir_of(exe, canonical).map_or_else(Vec::new, ensure_store_aliases)
}

/// The file `path` resolves to, links followed, or `None` where nothing is: the
/// production `resolve` of [`store_dir_of`].
#[cfg(unix)]
pub(crate) fn canonical(path: &Path) -> Option<std::path::PathBuf> {
    fs::canonicalize(path).ok()
}

/// [`pass_at`] as the executable at `exe` runs it: the unattended pass
/// ([`RcWiring::HonorOptOut`]) from a bundle that is not the release `aterm.app` writes
/// nothing ([`HookPass::NotReleaseBundle`]); the asked-for pass ([`RcWiring::Rewire`],
/// `atpkg repair`) runs from anywhere.
fn pass_as(
    layout: &Layout,
    home: &Path,
    wiring: RcWiring,
    exe: Option<&Path>,
    login: Option<&'static str>,
) -> HookPass {
    if wiring == RcWiring::HonorOptOut && exe.is_some_and(is_non_release_bundle_exe) {
        return HookPass::NotReleaseBundle;
    }
    pass_at(layout, home, wiring, login)
}

/// One whole pass against a known `home`, as [`refresh_with`] runs it, so a test in another
/// module (`cli`'s repair report, `doctor`'s rc lines) can drive it on a synthetic home.
/// `login` is the login shell's rc row, which the pass creates when absent ([`login_rc`]).
pub(crate) fn pass_at(
    layout: &Layout,
    home: &Path,
    wiring: RcWiring,
    login: Option<&'static str>,
) -> HookPass {
    refresh_at(
        home,
        &layout.bin_dir(),
        &layout.agents_dir(),
        &layout.reroute_dir(),
        wiring,
        login,
    )
}

/// [`refresh_with`] over an explicit `home` and the three managed directories.
///
/// The block never leads the hooks: it goes into a file atpkg does not own, and a
/// marker-bounded block naming a hook that was never written sources nothing while no later
/// pass rewrites it. So the wiring runs only once the hooks are on disk, and the next pass
/// retries the two together.
fn refresh_at(
    home: &Path,
    bin_dir: &Path,
    agents_dir: &Path,
    reroute_dir: &Path,
    wiring: RcWiring,
    login: Option<&'static str>,
) -> HookPass {
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    if ensure_private_dir(&aterm).is_err() || ensure_private_dir(&shell_d).is_err() {
        return HookPass::NotHardened;
    }
    let hooks_written = write_hooks(&shell_d, bin_dir, agents_dir, reroute_dir).is_ok();
    #[cfg(unix)]
    let rc = if hooks_written {
        ensure_rc_sources_hooks(home, wiring, login)
    } else {
        Vec::new()
    };
    #[cfg(not(unix))]
    let rc = {
        let _ = (wiring, login);
        Vec::new()
    };
    #[cfg(unix)]
    ensure_command_links(home);
    if hooks_written {
        HookPass::Rewritten(rc)
    } else {
        HookPass::HooksNotWritten
    }
}

/// The markers bounding atpkg's block in a user's shell rc. Present so the block can be
/// found, refreshed and removed exactly — never matched by content, which drifts.
/// `pub(crate)` so callers outside this module — the repair report's pin test, `doctor`'s rc
/// lines — read the marker the code actually writes rather than a copy of it that can drift.
#[cfg(unix)]
pub(crate) const RC_BEGIN: &str = "# >>> atpkg shell integration >>>";
#[cfg(unix)]
pub(crate) const RC_END: &str = "# <<< atpkg shell integration <<<";

/// The rc files atpkg wires: each row is a path relative to `$HOME` and the `shell.d` hook
/// it sources. The one roster, so the pass that writes the block and the report of its
/// state ([`rc_wiring`]) agree. atpkg edits one that exists, and creates only the one the
/// account's login shell reads when it is absent ([`login_rc`]).
///
/// A shell that reads none of these is not wired: fish with `$XDG_CONFIG_HOME` set reads a
/// different `config.fish`, and a login bash with `~/.bash_login` or `~/.profile` and no
/// `~/.bash_profile` reads those. `aterm pkg doctor`'s PATH line is the way in for those.
#[cfg(unix)]
pub(crate) const RC_FILES: [(&str, &str); 4] = [
    (".zshrc", "00-atpkg.zsh"),
    (".bashrc", "00-atpkg.bash"),
    // The login bash rc: the same hook, reached by the shell Terminal.app opens.
    (".bash_profile", "00-atpkg.bash"),
    (".config/fish/config.fish", "00-atpkg.fish"),
];

/// `~/.aterm/rc-wired`: the rc files this machine has wired, one canonical path per line.
///
/// The block says "delete this block to opt out", but the rc pass runs from install, every
/// launch's seed and the six-hourly `update`, so with the marker as the only check the
/// opt-out would last until the next pass. A wired rc is recorded here, and a recorded rc that no
/// longer carries the block is the user's opt-out and is skipped. A pass also records an rc
/// it finds already wired, so the first deletion on a machine wired before the ledger is
/// honoured too.
///
/// State, not trust, and cheap to lose: a wiped `~/.aterm` costs one re-add, and two passes
/// racing cost at most one lost entry (each writes the union of what it read and what it
/// wired). [`refresh_rewiring_rc`] ignores it on purpose: that is the way back.
#[cfg(unix)]
const RC_LEDGER: &str = "rc-wired";

#[cfg(unix)]
fn rc_ledger_path(home: &Path) -> std::path::PathBuf {
    home.join(".aterm").join(RC_LEDGER)
}

/// The recorded rc paths; empty when the ledger is absent or unreadable, which is what a
/// fresh home is.
#[cfg(unix)]
fn read_rc_ledger(home: &Path) -> std::collections::BTreeSet<String> {
    fs::read_to_string(rc_ledger_path(home))
        .map(|body| {
            body.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Record `real` as wired, best-effort (0600, temp + rename). Already recorded means no
/// write, so the ordinary pass touches nothing. A path containing a newline cannot be one
/// line of the ledger, so it is left out: such an rc is wired again by the next pass.
#[cfg(unix)]
fn record_rc_wired(home: &Path, real: &Path, ledger: &mut std::collections::BTreeSet<String>) {
    let entry = real.to_string_lossy().into_owned();
    if entry.contains('\n') {
        return;
    }
    if !ledger.insert(entry) {
        return; // already on disk
    }
    // `~/.aterm` is ours, and [`refresh`] has already hardened it; this covers a caller
    // that reaches the wiring directly.
    if ensure_private_dir(&home.join(".aterm")).is_err() {
        return;
    }
    let body: String = ledger.iter().map(|path| format!("{path}\n")).collect();
    let _ = atomic_write(&rc_ledger_path(home), &body);
}

/// Source the `shell.d` hooks from the user's own rc, so the managed toolchain is on PATH in
/// every shell (Terminal.app, iTerm, VS Code, ssh, an agent's shell), not only inside an
/// aterm session.
///
/// Conservative about a file it does not own:
/// * rc files that exist are edited, and exactly one may be created: `login`, the row the
///   account's login shell reads ([`login_rc`]), when it is absent and its directory
///   exists. A shell the user does not log in with never gains a file, and the created rc is
///   recorded like a wired one, so deleting it is an opt-out too;
/// * the block is bounded by [`RC_BEGIN`]/[`RC_END`], so it is idempotent and a user can
///   delete it in one motion, and a deletion stays deleted ([`RC_LEDGER`]);
/// * it sources `shell.d` rather than inlining a PATH, so there is one generator and the
///   path survives a prefix move;
/// * the hook path is double-quoted, so a home with a space sources the hook;
/// * every step is best-effort: wiring PATH never fails an install.
///
/// bash gets two targets because it reads them for different shells (macOS `/bin/bash`
/// 3.2.57): a login shell (a Terminal.app tab, ssh, `bash -l`) reads `.bash_profile` alone,
/// `bash -i` reads `.bashrc` alone, and with no `.bash_profile` a login falls through to
/// `.profile`. Both source the same `00-atpkg.bash`; a `.bash_profile` that sources
/// `.bashrc` runs it twice, which the hook tolerates (bin/ presence-guarded, the front dirs
/// moved, never duplicated). A home with a `.bashrc` and no `.bash_profile` gains a
/// `.bash_profile` only when the login shell is bash on macOS and no `.bash_login` or
/// `.profile` stands. `~/.bash_login` and `~/.profile` are not rows: `sh`/dash logins read
/// `.profile`, and the hook body (`${x//p/r}`) is not written for them.
///
/// Returns one [`RcOutcome`] per row that exists, and for `login` when it was absent and a
/// pass created it or left it so, in [`RC_FILES`] order.
#[cfg(unix)]
pub(crate) fn ensure_rc_sources_hooks(
    home: &Path,
    wiring: RcWiring,
    login: Option<&'static str>,
) -> Vec<(&'static str, RcOutcome)> {
    // Both sides of the protected-root comparison canonical: `$TMPDIR` and a home
    // reached through a link both spell `/private/…` once resolved.
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    // Read once for the whole pass; each write folds its own entry back in.
    let mut ledger = read_rc_ledger(home);
    let mut outcomes: Vec<(&'static str, RcOutcome)> = Vec::new();
    for (rc, hook) in RC_FILES {
        let rc_path = home.join(rc);
        // Create only the login shell's rc: a shell the user does not log in with should not
        // gain one. `lstat` and `realpath` are metadata-only and gated nowhere, so finding
        // where the rc really lives costs no consent.
        if fs::symlink_metadata(&rc_path).is_err() {
            if login == Some(rc)
                && let Some(outcome) =
                    create_login_rc(home, &canonical_home, rc, hook, wiring, &mut ledger)
            {
                outcomes.push((rc, outcome));
            }
            continue;
        }
        // Write to the file the rc names, never over a link: renaming over a symlinked rc
        // (stow, yadm/chezmoi, home-manager) leaves a detached copy the dotfile never sees.
        // Resolved for a regular rc too, since `~/.config` is itself a link in such setups.
        // A dangling link, or anything that is not a regular file, is left alone.
        let Ok(real) = fs::canonicalize(&rc_path) else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        // The consent fence: this runs on the six-hourly pass and the first-launch seed,
        // with nobody at the screen. Opening a dotfile that resolves under `~/Documents`,
        // iCloud Drive or a mounted volume raises the macOS folder dialog in aterm's name
        // and parks the pass on a modal with no timeout. The rc stays unwired; the owner can
        // wire it by hand, and `aterm pkg doctor` prints the PATH line either way.
        if crate::protected::under_protected_root(&canonical_home, &real) {
            outcomes.push((rc, RcOutcome::ConsentFenced));
            continue;
        }
        let Ok(meta) = fs::metadata(&real) else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        if !meta.is_file() {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        }
        let Ok(existing) = fs::read_to_string(&real) else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        if existing.contains(RC_BEGIN) {
            // Already wired; the hook body is what gets refreshed. Record it (including an
            // rc wired before the ledger existed) so the next deletion reads as the opt-out.
            record_rc_wired(home, &real, &mut ledger);
            outcomes.push((rc, RcOutcome::AlreadyWired));
            continue;
        }
        // The opt-out, honoured: this rc carried the block once and does not now, so the
        // user deleted it as the block told them to. An unattended pass leaves it; `atpkg
        // repair` ([`refresh_rewiring_rc`]) puts it back and says so
        // ([`RcOutcome::RelaidOverOptOut`]).
        let opted_out = ledger.contains(real.to_string_lossy().as_ref());
        if wiring == RcWiring::HonorOptOut && opted_out {
            outcomes.push((rc, RcOutcome::OptOutKept));
            continue;
        }
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push('\n');
        next.push_str(&rc_block(home, hook));
        // Temp + rename: a reader never sees a half-written rc, and a failure leaves the
        // original untouched. The temp sits beside the real file (same filesystem, so the
        // rename replaces it and not the link) and carries its mode. A directory we cannot
        // write (a link into the read-only /nix/store) fails here and changes nothing.
        let Some(name) = real.file_name() else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        let tmp = real.with_file_name(format!(
            ".{}.atpkg-{}.tmp",
            name.to_string_lossy().trim_start_matches('.'),
            std::process::id()
        ));
        let _ = fs::remove_file(&tmp);
        let written = crate::platform::create_private_new(&tmp)
            .and_then(|mut f| io::Write::write_all(&mut f, next.as_bytes()))
            .and_then(|()| fs::set_permissions(&tmp, meta.permissions()))
            .and_then(|()| fs::rename(&tmp, &real));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
            outcomes.push((rc, RcOutcome::WriteFailed));
        } else {
            record_rc_wired(home, &real, &mut ledger);
            outcomes.push((
                rc,
                if opted_out {
                    RcOutcome::RelaidOverOptOut
                } else {
                    RcOutcome::Appended
                },
            ));
        }
    }
    outcomes
}

/// atpkg's block for an rc that sources `hook` from `home`'s `shell.d`, markers included.
#[cfg(unix)]
fn rc_block(home: &Path, hook: &str) -> String {
    let hook_path = home.join(".aterm").join("shell.d").join(hook);
    // Double-quoted, both interpolations (`sh_quote`'s contract): bare, a HOME with a
    // space word-splits and the hook is never sourced. The block is marker-bounded, so
    // no later pass rewrites a bad line; it has to be born right.
    //
    // Status-neutral in both dialects: this is the last line many rc files run, so its
    // status is the `$?` the first prompt reads. `[ -f … ] && . …` exits 1 when the hook
    // is absent; `if …; then …; fi` reports the hook's own status, and 0 when there is
    // nothing to source.
    let line = if hook.ends_with(".fish") {
        format!(
            "if test -f \"{p}\"; source \"{p}\"; end",
            p = fish_quote(&hook_path)
        )
    } else {
        format!(
            "if [ -f \"{p}\" ]; then . \"{p}\"; fi",
            p = sh_quote(&hook_path)
        )
    };
    format!(
        "{RC_BEGIN}\n\
         # Puts the ALab toolchain on PATH. Managed by atpkg; delete this block to opt\n\
         # out -- it stays deleted; `aterm pkg repair` puts it back.\n\
         {line}\n\
         {RC_END}\n"
    )
}

/// Where the absent login rc `rc` would be created: its directory resolved (a `~/.config`
/// that is a link counts as its target) and the file name joined on, so the consent fence
/// and the ledger read the path the file will have. `None` when the directory is missing:
/// a pass creates one file, never a directory for it.
#[cfg(unix)]
fn login_rc_target(home: &Path, rc: &str) -> Option<std::path::PathBuf> {
    let path = home.join(rc);
    let name = path.file_name()?;
    let dir = fs::canonicalize(path.parent()?).ok()?;
    fs::metadata(&dir).ok()?.is_dir().then(|| dir.join(name))
}

/// What a pass does about the absent login rc `rc` ([`login_rc`]); `None` when it can be
/// neither created nor reported (its directory is missing).
///
/// Under the consent fence it is not created ([`RcOutcome::ConsentFenced`]). One the
/// ledger records was created (or wired) once and is gone, so the user deleted it: the
/// unattended pass leaves it absent ([`RcOutcome::OptOutKept`]) and `repair` creates it
/// again ([`RcOutcome::RelaidOverOptOut`]). Otherwise it is created holding only the
/// block, exclusively (`O_EXCL`, so a file that appeared since the `lstat` is never
/// replaced; such a file is wired by the next pass) and `0600`, then recorded.
#[cfg(unix)]
fn create_login_rc(
    home: &Path,
    canonical_home: &Path,
    rc: &'static str,
    hook: &str,
    wiring: RcWiring,
    ledger: &mut std::collections::BTreeSet<String>,
) -> Option<RcOutcome> {
    let real = login_rc_target(home, rc)?;
    if crate::protected::under_protected_root(canonical_home, &real) {
        return Some(RcOutcome::ConsentFenced);
    }
    let opted_out = ledger.contains(real.to_string_lossy().as_ref());
    if wiring == RcWiring::HonorOptOut && opted_out {
        return Some(RcOutcome::OptOutKept);
    }
    let body = rc_block(home, hook);
    let mut file = match crate::platform::create_private_new(&real) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return None,
        Err(_) => return Some(RcOutcome::WriteFailed),
    };
    // A file this call created, so a failed write takes it back rather than leave a shell a
    // half block to parse.
    if io::Write::write_all(&mut file, body.as_bytes()).is_err() {
        drop(file);
        let _ = fs::remove_file(&real);
        return Some(RcOutcome::WriteFailed);
    }
    record_rc_wired(home, &real, ledger);
    Some(if opted_out {
        RcOutcome::RelaidOverOptOut
    } else {
        RcOutcome::Created
    })
}

/// The absent login rc `rc`'s state for a report, as [`create_login_rc`] would treat it:
/// [`RcState::Unwired`] when a pass creates it, [`RcState::OptedOut`] when the user deleted
/// the one a pass created, [`RcState::ConsentFenced`] under the fence. `None` when `rc`
/// stands (the report reads it as any rc) or cannot be created (no directory for it).
#[cfg(unix)]
pub(crate) fn absent_login_rc(home: &Path, rc: &str) -> Option<RcState> {
    if fs::symlink_metadata(home.join(rc)).is_ok() {
        return None;
    }
    let real = login_rc_target(home, rc)?;
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    Some(
        if crate::protected::under_protected_root(&canonical_home, &real) {
            RcState::ConsentFenced
        } else if read_rc_ledger(home).contains(real.to_string_lossy().as_ref()) {
            RcState::OptedOut
        } else {
            RcState::Unwired
        },
    )
}

/// The bash login profiles `tools/install.sh`'s `path_block_rc_target` can elect that atpkg
/// has no row for, relative to `$HOME`: login bash reads the first of `~/.bash_profile`,
/// `~/.bash_login`, `~/.profile` that exists, and these are the fall-through. atpkg never
/// edits them; [`rc_wiring`] reads them so `doctor` sees install.sh's block there.
#[cfg(unix)]
pub(crate) const INSTALL_SH_PROFILES: [&str; 2] = [".bash_login", ".profile"];

/// The rc-wiring state of one rc file that exists, for `doctor`.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RcState {
    /// The rc carries atpkg's marker block ([`RC_BEGIN`]).
    Wired,
    /// The rc sources `~/.aterm/shell.d` through a line that is NOT this block: the PATH
    /// block `aterm pkg shell-setup --path` writes ([`crate::selfsetup::PATH_BLOCK_START`]),
    /// or a line the user wrote. The toolchain
    /// reaches that shell; atpkg simply does not own the line. Measured by CONTENT
    /// ([`sources_shell_d`]), so doctor never reports "nothing sources shell.d" from the
    /// mere absence of atpkg's marker — a fault it would not have measured.
    SourcedElsewhere,
    /// atpkg wired this rc once ([`RC_LEDGER`]) and the block is gone: the user opted out.
    /// Left alone by every pass except `atpkg repair`.
    OptedOut,
    /// The rc exists, no [`RC_LEDGER`] entry records atpkg wiring it, and nothing else
    /// sources `shell.d` from it: no pass has run since it appeared.
    Unwired,
    /// The rc resolves under a folder macOS guards with a consent dialog, so no pass opens it
    /// ([`ensure_rc_sources_hooks`]'s consent fence), and neither does this report. A user
    /// cannot diagnose it alone: the rc looks ordinary and every pass leaves it untouched.
    ConsentFenced,
}

/// How [`rc_wiring`] resolves one rc: the steps [`ensure_rc_sources_hooks`] takes before it
/// would write, so the report never describes a file the pass would treat differently.
/// `None`: the rc is absent, or not a regular UTF-8 file, which no pass edits.
#[cfg(unix)]
enum RcRead {
    /// The resolved target and its contents.
    Body(std::path::PathBuf, String),
    /// The rc resolves under a macOS-guarded root and was not opened.
    Fenced,
}

#[cfg(unix)]
fn read_rc_for_report(canonical_home: &Path, rc_path: &Path) -> Option<RcRead> {
    // Never create, and never invent a row for a file that is not there.
    fs::symlink_metadata(rc_path).ok()?;
    let real = fs::canonicalize(rc_path).ok()?;
    // The consent fence, before the open: doctor must not raise the dialog the unattended
    // pass refuses to raise.
    if crate::protected::under_protected_root(canonical_home, &real) {
        return Some(RcRead::Fenced);
    }
    if !fs::metadata(&real).ok()?.is_file() {
        return None;
    }
    let body = fs::read_to_string(&real).ok()?;
    Some(RcRead::Body(real, body))
}

/// The rc-wiring state of each [`RC_FILES`] row that exists under `home`, in roster order,
/// then each [`INSTALL_SH_PROFILES`] entry that sources `shell.d`.
///
/// An absent rc, or one no pass would edit (not a regular file, not UTF-8), is not listed.
/// Content is read before the ledger: an rc that sources `shell.d` through a foreign line
/// reaches the toolchain whether or not atpkg's block was deleted from it. The install.sh
/// profiles are read-only probes, listed only as `SourcedElsewhere`.
///
/// These six names ([`rc_files_read`]) are every file the report reads: `$ZDOTDIR`, `/etc`
/// and fish under `$XDG_CONFIG_HOME` are neither wired nor read.
#[cfg(unix)]
pub(crate) fn rc_wiring(home: &Path) -> Vec<(&'static str, RcState)> {
    // Both sides canonical, as the pass compares them.
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let ledger = read_rc_ledger(home);
    let mut wiring: Vec<(&'static str, RcState)> = RC_FILES
        .iter()
        .filter_map(|(rc, _)| {
            let state = match read_rc_for_report(&canonical_home, &home.join(rc))? {
                RcRead::Fenced => RcState::ConsentFenced,
                RcRead::Body(real, body) => {
                    if body.contains(RC_BEGIN) {
                        RcState::Wired
                    } else if sources_shell_d(&body) {
                        RcState::SourcedElsewhere
                    } else if ledger.contains(real.to_string_lossy().as_ref()) {
                        RcState::OptedOut
                    } else {
                        RcState::Unwired
                    }
                }
            };
            Some((*rc, state))
        })
        .collect();
    wiring.extend(INSTALL_SH_PROFILES.into_iter().filter_map(|rc| {
        match read_rc_for_report(&canonical_home, &home.join(rc))? {
            RcRead::Body(_, body) => {
                sources_shell_d(&body).then_some((rc, RcState::SourcedElsewhere))
            }
            RcRead::Fenced => None,
        }
    }));
    wiring
}

/// Every file [`rc_wiring`] reads, relative to `$HOME`, in report order: the four atpkg
/// wires, then install.sh's two bash login fall-throughs. doctor names exactly these when
/// none sources `shell.d`, so its line measures named files, not every shell.
#[cfg(unix)]
pub(crate) fn rc_files_read() -> impl Iterator<Item = &'static str> {
    RC_FILES
        .iter()
        .map(|(rc, _)| *rc)
        .chain(INSTALL_SH_PROFILES)
}

/// Whether `rc` sources `~/.aterm/shell.d` through a line other than atpkg's block: any
/// non-comment line naming a path under `/.aterm/shell.d/`, as `tools/install.sh`'s
/// `wire_shell_path` writes or a user may by hand.
#[cfg(unix)]
fn sources_shell_d(rc: &str) -> bool {
    rc.lines()
        .map(str::trim_start)
        .any(|l| !l.starts_with('#') && l.contains("/.aterm/shell.d/"))
}

/// Put `aterm`, `atpkg` and `amacs` in `~/.local/bin` when we are running from an app
/// bundle ([`COMMAND_LINKS`]).
///
/// The rc wiring is [`ensure_rc_sources_hooks`]'s (the marker-bounded block in an
/// existing `~/.zshrc` / `~/.bashrc` / `~/.bash_profile` / fish config that sources
/// `shell.d`); this is the other half of reaching the toolchain from outside aterm: the
/// `aterm <tool>` / `atpkg run` route, and `amacs` for an `$EDITOR` in Terminal.app or an
/// ssh session into the Mac. On the DMG install route the binaries live only inside
/// `/Applications/aterm.app/Contents/MacOS`, so without these links none of the commands
/// is on any PATH outside aterm.
///
/// Symlinks only, into the `~/.local/bin` `tools/install.sh` uses, and never over anything
/// not already ours: a real file there is left for `repair` ([`move_aside_copied_commands`]).
///
/// Only from the release bundle: "ours" is a link into an `aterm.app` bundle
/// ([`in_release_bundle`]), and this pass never writes a link it would refuse to repoint, so
/// a dev bundle (`aterm (dev).app`) cannot take the names from the release. Aiming `aterm`
/// at a dev build is `targo --unverified ship dev-app`'s stated choice (`--no-link` declines
/// it), never a side effect of launching one. Unix-only: the body is bundle-shaped
/// (`Contents/MacOS`, POSIX symlinks).
#[cfg(unix)]
fn ensure_command_links(home: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Ok(exe) = exe.canonicalize() else {
        return;
    };
    ensure_command_links_from(home, &exe);
}

/// Whether `path` lies inside a release-named `aterm.app` bundle's `Contents/MacOS`: the one
/// spelling of "ours" for the unattended writers of shared user state, both for an existing
/// command link's target and for the executable that writes the links
/// ([`ensure_command_links`]) or, unattended, the shell hooks ([`refresh`]). A dev bundle
/// (`aterm (dev).app`), a renamed copy and a source tree fail it; so does a release copy
/// the user renamed, which renaming back fixes.
fn in_release_bundle(path: &Path) -> bool {
    path.to_string_lossy()
        .contains("/aterm.app/Contents/MacOS/")
}

/// Whether the executable at `exe` (canonical) runs from an app bundle that is not the
/// release `aterm.app`, the case [`refresh`] and [`ensure_command_links`] stand aside for.
/// A source build (`target/release/aterm`) is not a bundle.
pub(crate) fn is_non_release_bundle_exe(exe: &Path) -> bool {
    exe.parent().is_some_and(|d| d.ends_with("Contents/MacOS")) && !in_release_bundle(exe)
}

/// Whether this process runs from an app bundle that is not the release `aterm.app`
/// ([`is_non_release_bundle_exe`]). Public so the window's agent primer applies the same
/// rule: a stale dev bundle must not rewrite `~/.claude/CLAUDE.md` and the skills. An
/// executable that cannot be resolved answers `false`.
#[must_use]
pub fn runs_from_non_release_bundle() -> bool {
    std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .is_ok_and(|exe| is_non_release_bundle_exe(&exe))
}

/// [`ensure_command_links`] for the executable at `exe` (already canonical).
#[cfg(unix)]
pub(crate) fn ensure_command_links_from(home: &Path, exe: &Path) {
    let Some(macos) = command_link_source(exe) else {
        return;
    };
    let Some(bin) = command_links_dir(home) else {
        return;
    };
    if fs::create_dir_all(&bin).is_err() {
        return;
    }
    for name in COMMAND_LINKS {
        let target = macos.join(name);
        if !target.exists() {
            continue;
        }
        let link = bin.join(name);
        // Read as it stands now: `aterm` was settled a moment ago.
        let aterm_settled =
            fs::read_link(bin.join("aterm")).is_ok_and(|aterm| aterm == macos.join("aterm"));
        match link_act(&link, &target, name, aterm_settled) {
            None => continue,
            Some(Some(_)) => {
                let _ = fs::remove_file(&link);
            }
            Some(None) => {}
        }
        let _ = std::os::unix::fs::symlink(&target, &link);
    }
}

/// What the command-link pass does at `link` for its `target` (`name` one of
/// [`COMMAND_LINKS`]), `aterm_settled` whether `~/.local/bin/aterm` names this bundle's
/// `aterm` once the pass has written it: `None` leaves it; `Some(None)` lays a link where
/// nothing stands; `Some(Some(old))` re-points a link to `old`. One rule for the pass
/// ([`ensure_command_links_from`]) and for the read of what it would do
/// ([`command_links_to_write`]).
///
/// Ours to re-point only if it already points into an `aterm.app` bundle; anything else —
/// a dev bundle `ship dev-app` aimed `aterm` at, someone's own build — belongs to whoever
/// put it there. TWO exceptions into another bundle's `Contents/MacOS`. `atpkg`: no tool
/// states that choice (`ship dev-app` links `aterm` and `amacs`), so such a link is the
/// leftover of this pass before it was release-only — a dev bundle's launch took it — and
/// it goes back to the release. `amacs` FOLLOWS `aterm`: the editor client and the window
/// it hands a file to are one binary, so once `aterm` here runs this release again
/// (`tools/install.sh` relinks only `aterm` when a dev build rejoins the release), the
/// editor comes back with it; while `aterm` is aimed at a dev bundle, so is `amacs`. Not a
/// link — an older install's copy, or someone's own build — is never touched by this pass;
/// `doctor` names it and `repair` moves it aside.
#[cfg(unix)]
fn link_act(
    link: &Path,
    target: &Path,
    name: &str,
    aterm_settled: bool,
) -> Option<Option<std::path::PathBuf>> {
    match fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let old = fs::read_link(link).ok()?;
            let in_a_bundle =
                |old: &Path| old.parent().is_some_and(|d| d.ends_with("Contents/MacOS"));
            let ours = in_release_bundle(&old)
                || (name == "atpkg" && in_a_bundle(&old))
                || (name == "amacs" && in_a_bundle(&old) && aterm_settled);
            (ours && old != target).then_some(Some(old))
        }
        Ok(_) => None,
        Err(_) => Some(None),
    }
}

/// The names the command-link pass lays in `~/.local/bin`, in the order it lays them.
/// `amacs`, aterm's editor (`docs/DESIGN-amacs-2026-10-19.md` §8.1), is owned as `aterm`
/// is — laid where nothing is, repointed from a link into an `aterm.app`, and a file there
/// is `repair`'s to move — and follows the `aterm` link this pass has just settled out of
/// another bundle ([`ensure_command_links_from`]), so it comes after `aterm`. A bundle from
/// before amacs has no `Contents/MacOS/amacs`, so it lays no link to one.
#[cfg(unix)]
pub(crate) const COMMAND_LINKS: [&str; 3] = ["aterm", "atpkg", "amacs"];

/// The `Contents/MacOS` an executable at `exe` (canonical) lays the command links from, or
/// `None` where it may not lay them.
#[cfg(unix)]
pub(crate) fn command_link_source(exe: &Path) -> Option<&Path> {
    command_link_check(exe).ok()
}

/// [`command_link_source`], with the reason, in words `repair` prints, when this executable
/// may not lay the links.
#[cfg(unix)]
pub(crate) fn command_link_check(exe: &Path) -> Result<&Path, &'static str> {
    // Only from inside a bundle: a source build's target/release is the developer's
    // own tree, and linking out of it would outlive the checkout.
    let macos = exe
        .parent()
        .filter(|d| d.ends_with("Contents/MacOS"))
        .ok_or("this atpkg is not inside an app bundle")?;
    // Only from the release bundle: never write a link this pass would refuse to repoint
    // ([`ensure_command_links`]).
    if !in_release_bundle(exe) {
        return Err("this atpkg is not the release aterm.app's (a dev or renamed copy)");
    }
    // Not from a translocated copy: Gatekeeper runs a quarantined download from a
    // read-only, randomly-named mount that disappears when the app quits.
    if exe.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .starts_with("AppTranslocation")
    }) {
        return Err(
            "this aterm.app runs from Gatekeeper's temporary copy of a download — move it to \
             /Applications and open it from there",
        );
    }
    // Not from a mounted volume: an app run in place from a disk image or a removable disk
    // goes away with the eject, and a link into it dangles.
    if exe.starts_with("/Volumes") {
        return Err(
            "this aterm.app resolves onto a disk image or a mounted volume (under /Volumes) \
             — copy the app itself, not a link to it, into /Applications and run it from \
             there",
        );
    }
    Ok(macos)
}

/// Names an older install may have left in `~/.local/bin` that no pass links:
/// `aterm ctl` replaces `aterm-ctl`.
#[cfg(unix)]
const RETIRED_COMMANDS: [&str; 1] = ["aterm-ctl"];

/// The names this platform's install links into `~/.local/bin`: on macOS the app lays each
/// of [`COMMAND_LINKS`]; elsewhere `tools/install.sh` lays `aterm` alone, and there only when
/// `ATERM_BIN_DIR` does not name another folder.
#[cfg(all(unix, target_os = "macos"))]
const INSTALLED_COMMANDS: &[&str] = &COMMAND_LINKS;
#[cfg(all(unix, not(target_os = "macos")))]
const INSTALLED_COMMANDS: &[&str] = &["aterm"];

/// The command paths in `~/.local/bin` that hold a regular file instead of a link (an older
/// install's copy, or someone's own build) under a name this platform's install links
/// ([`INSTALLED_COMMANDS`]) or a retired one ([`RETIRED_COMMANDS`]). It never updates, and it
/// runs instead of the installed aterm wherever `~/.local/bin` leads PATH. No unattended
/// pass touches it; `doctor` names it, and on macOS `repair` moves it aside
/// ([`move_aside_copied_commands`]).
#[cfg(unix)]
pub(crate) fn copied_command_links(home: &Path) -> Vec<std::path::PathBuf> {
    let Some(bin) = command_links_dir(home) else {
        return Vec::new();
    };
    INSTALLED_COMMANDS
        .iter()
        .chain(RETIRED_COMMANDS.iter())
        .map(|name| bin.join(name))
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file()))
        .collect()
}

/// How to run `repair` from the installed app (a copied `~/.local/bin/aterm` is what
/// `aterm pkg repair` would run): this process's own `atpkg` when it is the release
/// bundle's, else the `atpkg` of an `aterm.app` in `/Applications` or `~/Applications`, else
/// a description.
#[cfg(all(unix, target_os = "macos"))]
pub(crate) fn repair_from_app_hint() -> String {
    let own = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .ok()
        .and_then(|exe| command_link_source(&exe).map(|macos| macos.join("atpkg")));
    let installed = || installed_app_atpkg(&installed_app_dirs(aterm_types::dirs::home_dir()));
    repair_hint_for(own.or_else(installed).as_deref())
}

/// Where an installed `aterm.app` is looked for, in order: the system folder first.
#[cfg(all(unix, target_os = "macos"))]
fn installed_app_dirs(home: Option<std::path::PathBuf>) -> [Option<std::path::PathBuf>; 2] {
    [
        Some(std::path::PathBuf::from("/Applications")),
        home.map(|home| home.join("Applications")),
    ]
}

/// The `atpkg` of the first `aterm.app` in `apps_dirs` that would do the job: its folder
/// resolved (a link onto a mounted volume would refuse it), and the `atpkg` name kept,
/// since in the bundle it is an alias link to `aterm` and the one binary picks its tool by
/// that name.
#[cfg(all(unix, target_os = "macos"))]
fn installed_app_atpkg(apps_dirs: &[Option<std::path::PathBuf>]) -> Option<std::path::PathBuf> {
    apps_dirs
        .iter()
        .flatten()
        .filter_map(|apps| apps.join("aterm.app/Contents/MacOS").canonicalize().ok())
        .find(|macos| {
            macos.join("atpkg").is_file() && command_link_source(&macos.join("aterm")).is_some()
        })
        .map(|macos| macos.join("atpkg"))
}

/// [`repair_from_app_hint`]'s words for the `atpkg` it found, if any. Pure.
#[cfg(all(unix, target_os = "macos"))]
fn repair_hint_for(atpkg: Option<&Path>) -> String {
    atpkg.map_or_else(
        || {
            "`atpkg repair` from aterm.app installed in /Applications or ~/Applications \
             (a real copy, not one on a disk image or mounted volume)"
                .to_string()
        },
        |atpkg| {
            format!(
                "`{} repair`",
                crate::stub::sh_single_quote(&atpkg.to_string_lossy())
            )
        },
    )
}

/// What replaces the copy at `path` — a [`copied_command_links`] entry — said to the owner:
/// on macOS the installed app's `repair`, which moves it aside and links the app in place of
/// a [`COMMAND_LINKS`] name (a retired name gets nothing in its place). Elsewhere no atpkg
/// lays these links and the installer's own may live in another folder (`ATERM_BIN_DIR`), so
/// the copy is moved aside by hand — a retired `aterm-ctl` too, whose place `aterm ctl` takes.
#[cfg(unix)]
pub(crate) fn copied_command_remedy(path: &Path) -> String {
    let retired = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| RETIRED_COMMANDS.contains(&name));
    #[cfg(target_os = "macos")]
    {
        if retired {
            format!(
                "run {} — it moves this file aside; `aterm ctl` replaces it",
                repair_from_app_hint()
            )
        } else {
            format!(
                "run {} — it moves this file aside and links the app in its place",
                repair_from_app_hint()
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if retired {
            "move it aside — `aterm ctl` replaces it".to_string()
        } else {
            "move it aside, and install aterm again if its link belongs here".to_string()
        }
    }
}

/// One file `repair` moved out of a command path.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MovedAside {
    pub(crate) from: std::path::PathBuf,
    pub(crate) to: std::path::PathBuf,
    /// Whether the app was linked in its place (a retired name gets no link).
    pub(crate) linked: bool,
}

/// What `repair`'s command-link step did: each file it moved and each it could not, with
/// why.
#[cfg(unix)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AsideOutcome {
    pub(crate) moved: Vec<MovedAside>,
    pub(crate) failed: Vec<(std::path::PathBuf, String)>,
}

/// `atpkg repair`'s half of the command links: each [`copied_command_links`] path is moved to
/// `<name>.moved-aside-<unix secs>` beside it, and a name the pass links ([`COMMAND_LINKS`])
/// gets the release bundle linked in its place; a retired name is only moved. The asked-for
/// pass may displace someone's own build, so it moves rather than deletes and says where the
/// file went. `Err` says why the executable at `exe` does not lay the links
/// ([`command_link_check`]); `home` and `exe` are the account's and the running pass's, `secs`
/// the suffix's time.
#[cfg(unix)]
pub(crate) fn move_aside_copied_commands_at(
    home: &Path,
    exe: &Path,
    secs: u64,
) -> Result<AsideOutcome, &'static str> {
    let macos = command_link_check(exe)?;
    let mut outcome = AsideOutcome::default();
    for from in copied_command_links(home) {
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let link_to = COMMAND_LINKS.contains(&name).then(|| macos.join(name));
        if let Some(target) = link_to.as_ref().filter(|target| !target.exists()) {
            outcome.failed.push((
                from,
                format!("this app has no {} to link in its place", target.display()),
            ));
            continue;
        }
        let mut aside = from.clone().into_os_string();
        aside.push(format!(".moved-aside-{secs}"));
        let aside = std::path::PathBuf::from(aside);
        match rename_file_aside(&from, &aside, link_to.as_deref()) {
            Ok(()) => outcome.moved.push(MovedAside {
                from,
                to: aside,
                linked: link_to.is_some(),
            }),
            Err(why) => outcome.failed.push((from, why)),
        }
    }
    Ok(outcome)
}

/// Give the file at `from` the name `aside`, then put a link to `link_to` at `from` (or leave
/// nothing there). The file keeps a name at every step: `hard_link` refuses an existing
/// `aside`, so an earlier move is never overwritten; the link is made under a temporary name
/// and renamed over `from`; `from` must still be the same inode before it is replaced; and on
/// any failure `aside` is dropped, leaving `from` as it was. `Err` says why nothing changed.
#[cfg(unix)]
fn rename_file_aside(from: &Path, aside: &Path, link_to: Option<&Path>) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let inode = |path: &Path| fs::symlink_metadata(path).map(|m| (m.dev(), m.ino())).ok();
    fs::hard_link(from, aside).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            format!("{} already exists", aside.display())
        } else {
            format!("it could not be given a second name ({e})")
        }
    })?;
    let replaced = if inode(from).is_none() || inode(from) != inode(aside) {
        Err("it changed while it was being moved".to_string())
    } else {
        match link_to {
            Some(target) => {
                let mut tmp = from.as_os_str().to_owned();
                tmp.push(format!(".link-{}", std::process::id()));
                let tmp = std::path::PathBuf::from(tmp);
                let _ = fs::remove_file(&tmp);
                let linked = std::os::unix::fs::symlink(target, &tmp)
                    .and_then(|()| fs::rename(&tmp, from))
                    .map_err(|e| {
                        format!("the link to the app could not be put in its place ({e})")
                    });
                if linked.is_err() {
                    let _ = fs::remove_file(&tmp);
                }
                linked
            }
            None => fs::remove_file(from)
                .map_err(|e| format!("it could not be removed from its old name ({e})")),
        }
    };
    if replaced.is_err() {
        let _ = fs::remove_file(aside);
    }
    replaced
}

/// Where the [`COMMAND_LINKS`] go — `~/.local/bin` — or `None` when that
/// directory RESOLVES under a root macOS guards with a consent dialog (a `~/.local` that
/// is itself a link into a dotfiles repo under `~/Documents`): creating the directory,
/// or a link inside it, is a gated write, and [`ensure_command_links`] runs unattended
/// (the same fence as [`ensure_rc_sources_hooks`]; `crate::protected`).
///
/// The nearest existing ancestor decides where a create would land, and `canonicalize` is
/// metadata-only, so this touches nothing consent-gated. A home that cannot be resolved at
/// all yields the plain path.
#[cfg(unix)]
pub(crate) fn command_links_dir(home: &Path) -> Option<std::path::PathBuf> {
    let bin = home.join(".local/bin");
    let mut probe = bin.as_path();
    let real = loop {
        match fs::canonicalize(probe) {
            Ok(real) => break real.join(bin.strip_prefix(probe).unwrap_or(Path::new(""))),
            Err(_) => match probe.parent() {
                Some(parent) if parent.starts_with(home) => probe = parent,
                _ => return Some(bin),
            },
        }
    };
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if crate::protected::under_protected_root(&canonical_home, &real) {
        None
    } else {
        Some(bin)
    }
}

/// Render `bin_dir` for a double-quoted POSIX shell string, escaping the four
/// metacharacters a double-quoted context still interprets. A HOME-derived path won't
/// contain them, but never interpolate raw.
///
/// POSIX only — zsh, bash, and the `if [ -f "…" ]` rc line. fish has [`fish_quote`]; this
/// quoter's backtick is wrong there.
pub(crate) fn sh_quote(bin_dir: &Path) -> String {
    let mut out = String::new();
    for ch in bin_dir.to_string_lossy().chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Render `p` for a double-quoted fish string: the fish hook body and the fish rc line.
///
/// Not [`sh_quote`]: fish is not a POSIX shell. Between double quotes it recognises only
/// `\\`, `\"` and `\$` (and a backslash-newline continuation), keeps a backslash before any
/// other character literally, and has no backtick command substitution at all. sh's quoter
/// also escapes a backtick, so a prefix or a `$HOME` holding one became a path of a
/// different name (`/opt/a`b` emitted as `/opt/a\`b`): the hook put a directory that does
/// not exist on PATH, the rc line's `test -f` never found the hook, and the marker-bounded
/// block sourced nothing for ever.
pub(crate) fn fish_quote(p: &Path) -> String {
    let mut out = String::new();
    for ch in p.to_string_lossy().chars() {
        if matches!(ch, '\\' | '"' | '$') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Render `bin_dir` for a single-quoted PowerShell string literal: only the single quote is
/// special there (doubled to escape), so a Windows path's backslashes pass through literally.
fn ps_quote(bin_dir: &Path) -> String {
    bin_dir.to_string_lossy().replace('\'', "''")
}

/// Write `content` to `dest` atomically (temp `0600` + rename), so a reader never sees a
/// half-written hook.
#[cfg(unix)]
fn atomic_write(dest: &Path, content: &str) -> io::Result<()> {
    let tmp = stage_hook(dest, content)?;
    fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Write `content` to `dest`'s temp sibling, born `0600` and hardened, and hand back the
/// temp path for the caller to rename into place. The temp is removed on every error arm,
/// since nobody sweeps `~/.aterm/shell.d`. Not `fs::write`: `create_new` + `0600` gives the
/// mode at birth (never world-readable before a chmod) and refuses a name another file holds.
fn stage_hook(dest: &Path, content: &str) -> io::Result<std::path::PathBuf> {
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path has no file name"))?;
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path has no parent"))?;
    let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    // A temp of our own name left by a dead process with this pid is ours to clear; anything
    // that is not a plain file we can remove stays, and `create_new` then refuses.
    let _ = fs::remove_file(&tmp);
    let staged = crate::platform::create_private_new(&tmp)
        .and_then(|mut f| io::Write::write_all(&mut f, content.as_bytes()))
        .and_then(|()| crate::platform::harden_file(&tmp));
    match staged {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// The four files `write_hooks` lays are exactly what `hook_for_shell` names per shell,
    /// and `hook_file` answers only for a hook that exists as a regular file.
    #[cfg(unix)]
    #[test]
    fn hook_for_shell_and_hook_file_read_the_files_write_hooks_lays() {
        let home = tmp("hookfile");
        assert_eq!(hook_for_shell("zsh"), Some("00-atpkg.zsh"));
        assert_eq!(hook_for_shell("bash"), Some("00-atpkg.bash"));
        // `sh` gets no hook: the bash hook is not POSIX sh (`${var//pat/rep}`).
        assert_eq!(hook_for_shell("sh"), None);
        assert_eq!(hook_for_shell("fish"), Some("00-atpkg.fish"));
        assert_eq!(hook_for_shell("pwsh"), Some("00-atpkg.ps1"));
        assert_eq!(hook_for_shell("powershell"), Some("00-atpkg.ps1"));
        assert_eq!(hook_for_shell("nu"), None);
        // Nothing written: no hook file for any shell.
        for sh in ["zsh", "bash", "fish", "pwsh", "nu"] {
            assert_eq!(hook_file(&home, sh), None, "{sh}");
        }
        let shell_d = home.join(".aterm/shell.d");
        fs::create_dir_all(&shell_d).unwrap();
        let written = write_hooks(
            &shell_d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        )
        .unwrap();
        for sh in ["zsh", "bash", "fish", "pwsh"] {
            let (path, hook) = hook_file(&home, sh).unwrap_or_else(|| panic!("{sh}"));
            assert_eq!(path, shell_d.join(hook));
            assert!(
                written.contains(&hook.to_string()),
                "{hook} is a file write_hooks lays"
            );
        }
        assert_eq!(hook_file(&home, "nu"), None);
        // `sh` never gets the bash hook: its body is not POSIX sh.
        assert_eq!(hook_file(&home, "sh"), None);
        // A symlink in the hook's place is not the hook.
        fs::remove_file(shell_d.join("00-atpkg.zsh")).unwrap();
        std::os::unix::fs::symlink(shell_d.join("00-atpkg.bash"), shell_d.join("00-atpkg.zsh"))
            .unwrap();
        assert_eq!(hook_file(&home, "zsh"), None);
        let _ = fs::remove_dir_all(&home);
    }

    fn tmp(label: &str) -> crate::testdir::Dir {
        // The command-link checks refuse anything on a mounted volume, so a TMPDIR under
        // /Volumes would refuse these tests' own bundles: build them on the system volume.
        let base = std::env::temp_dir();
        let base = if base
            .canonicalize()
            .is_ok_and(|resolved| resolved.starts_with("/Volumes"))
        {
            PathBuf::from("/tmp")
        } else {
            base
        };
        crate::testdir::dir_in(&base, &format!("atpkg-hooks-{label}-"))
    }

    /// The rc block goes in exactly once, sources the hook, and is removable.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_is_idempotent_and_only_touches_existing_rc_files() {
        use std::os::unix::fs::MetadataExt as _;
        let home = tmp("rcwire");
        let zshrc = home.join(".zshrc");
        std::fs::write(&zshrc, "# pre-existing user content\nexport FOO=1\n").unwrap();
        // .bashrc deliberately absent, and no login rc named: atpkg must not create an rc.

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        let once = std::fs::read_to_string(&zshrc).unwrap();
        let ino_once = fs::metadata(&zshrc).unwrap().ino();
        assert!(
            once.contains(RC_BEGIN) && once.contains(RC_END),
            "block written"
        );
        assert!(
            once.contains(".aterm/shell.d/00-atpkg.zsh"),
            "it must SOURCE the generated hook, not inline a PATH that can go stale"
        );
        assert!(
            once.starts_with("# pre-existing user content"),
            "the user's own content must be preserved, and preserved FIRST"
        );
        assert!(!home.join(".bashrc").exists(), "no rc was invented");

        // Idempotent: a second pass stacks no second block and rewrites nothing (same inode).
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        let twice = std::fs::read_to_string(&zshrc).unwrap();
        assert_eq!(twice, once, "a second pass must be a no-op");
        assert_eq!(twice.matches(RC_BEGIN).count(), 1, "exactly one block");
        assert_eq!(
            fs::metadata(&zshrc).unwrap().ino(),
            ino_once,
            "the rc was not rewritten: same inode, no temp+rename"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The opt-out the block documents lasts: a recorded rc without the block is left alone
    /// by every unattended pass, `repair` ([`refresh_rewiring_rc`]) writes it back, and after
    /// that the block can be deleted for good again.
    #[cfg(unix)]
    #[test]
    fn a_deleted_rc_block_stays_deleted_and_repair_is_the_way_back() {
        let home = tmp("rcoptout");
        let zshrc = home.join(".zshrc");
        let user = "# pre-existing user content\nexport FOO=1\n";
        fs::write(&zshrc, user).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        assert!(
            fs::read_to_string(&zshrc).unwrap().contains(RC_BEGIN),
            "wired on the first pass, as before"
        );
        let real = fs::canonicalize(&zshrc).unwrap();
        assert!(
            read_rc_ledger(&home).contains(real.to_string_lossy().as_ref()),
            "the wiring is recorded: {:?}",
            read_rc_ledger(&home)
        );
        assert_eq!(
            fs::metadata(rc_ledger_path(&home))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the ledger is private, like every other file this module writes"
        );

        // The user does exactly what the block tells them to.
        fs::write(&zshrc, user).unwrap();
        for pass in 0..3 {
            ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
            assert_eq!(
                fs::read_to_string(&zshrc).unwrap(),
                user,
                "pass {pass}: an unattended pass must not undo the user's own edit"
            );
        }

        // `atpkg repair` is the pass the user asks for, so it wires the rc again.
        ensure_rc_sources_hooks(&home, RcWiring::Rewire, None);
        assert!(
            fs::read_to_string(&zshrc).unwrap().contains(RC_BEGIN),
            "repair puts the block back"
        );

        // …and the opt-out is still available afterwards.
        fs::write(&zshrc, user).unwrap();
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        assert_eq!(
            fs::read_to_string(&zshrc).unwrap(),
            user,
            "a second opt-out is honoured just like the first"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// The one file a pass creates (design init-system §4.2): the login shell's absent rc,
    /// holding only the block, private, recorded. No other shell's rc appears, even where
    /// its directory stands. Deleting the created file is an opt-out the unattended pass
    /// keeps and `repair` lifts.
    #[cfg(unix)]
    #[test]
    fn the_login_shell_s_absent_rc_is_created_and_no_other_shell_gains_one() {
        let home = tmp("rclogin");
        // fish's directory stands, so a pass that created a file for any shell could.
        fs::create_dir_all(home.join(".config/fish")).unwrap();
        let login = login_rc("/bin/zsh", &home, None);
        assert_eq!(login, Some(".zshrc"));
        let zshrc = home.join(".zshrc");

        assert_eq!(
            ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, login),
            [(".zshrc", RcOutcome::Created)]
        );
        let body = fs::read_to_string(&zshrc).unwrap();
        assert!(body.starts_with(RC_BEGIN), "only the block: {body:?}");
        assert!(body.contains(".aterm/shell.d/00-atpkg.zsh"), "{body:?}");
        assert_eq!(body.matches(RC_BEGIN).count(), 1);
        assert_eq!(
            fs::metadata(&zshrc).unwrap().permissions().mode() & 0o777,
            0o600,
            "born private"
        );
        let real = fs::canonicalize(&zshrc).unwrap();
        assert!(
            read_rc_ledger(&home).contains(real.to_string_lossy().as_ref()),
            "recorded like a wired rc"
        );
        for other in [".bashrc", ".bash_profile", ".config/fish/config.fish"] {
            assert!(
                fs::symlink_metadata(home.join(other)).is_err(),
                "{other} appeared for a shell this account does not log in with"
            );
        }

        // The next pass finds it wired and writes nothing.
        assert_eq!(
            ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, login),
            [(".zshrc", RcOutcome::AlreadyWired)]
        );
        assert_eq!(fs::read_to_string(&zshrc).unwrap(), body);

        // The user deletes the file: every unattended pass leaves it gone, and says so.
        fs::remove_file(&zshrc).unwrap();
        assert_eq!(absent_login_rc(&home, ".zshrc"), Some(RcState::OptedOut));
        for pass in 0..2 {
            assert_eq!(
                ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, login),
                [(".zshrc", RcOutcome::OptOutKept)],
                "pass {pass}"
            );
            assert!(fs::symlink_metadata(&zshrc).is_err(), "pass {pass}");
        }
        // `repair` makes it again.
        assert_eq!(
            ensure_rc_sources_hooks(&home, RcWiring::Rewire, login),
            [(".zshrc", RcOutcome::RelaidOverOptOut)]
        );
        assert_eq!(fs::read_to_string(&zshrc).unwrap(), body);
        assert_eq!(absent_login_rc(&home, ".zshrc"), None, "it stands again");
    }

    /// The login rc is selfsetup's `rc_target` election, and only a file atpkg wires: a
    /// shell it does not know, and an election that picks a file no row names, get nothing.
    #[cfg(unix)]
    #[test]
    fn the_login_rc_is_the_rc_target_election_s_and_only_a_row() {
        let home = tmp("rcelect");
        assert_eq!(login_rc("/bin/zsh", &home, None), Some(".zshrc"));
        assert_eq!(login_rc("zsh", &home, None), Some(".zshrc"));
        assert_eq!(
            login_rc("/opt/homebrew/bin/fish", &home, None),
            Some(".config/fish/config.fish")
        );
        assert_eq!(
            login_rc("/opt/homebrew/bin/fish", &home, Some(&home.join(".config"))),
            Some(".config/fish/config.fish")
        );
        // fish under another XDG_CONFIG_HOME reads a config.fish no row names.
        assert_eq!(
            login_rc("/opt/homebrew/bin/fish", &home, Some(&home.join("xdg"))),
            None
        );
        for unknown in ["/bin/sh", "/bin/dash", "/usr/bin/nu", "/usr/bin/false", ""] {
            assert_eq!(login_rc(unknown, &home, None), None, "{unknown:?}");
        }
        if cfg!(target_os = "macos") {
            assert_eq!(login_rc("/bin/bash", &home, None), Some(".bash_profile"));
            // A login bash that reads `~/.profile` is left to it.
            fs::write(home.join(".profile"), "").unwrap();
            assert_eq!(login_rc("/bin/bash", &home, None), None);
        } else {
            assert_eq!(login_rc("/bin/bash", &home, None), Some(".bashrc"));
        }
    }

    /// The created rc keeps the consent fence: a login rc whose directory resolves under a
    /// guarded folder is neither created nor opened, and a missing directory is never made.
    #[cfg(unix)]
    #[test]
    fn a_login_rc_under_a_guarded_folder_or_with_no_directory_is_not_created() {
        let home = tmp("rcloginfence");
        let login = login_rc("/usr/local/bin/fish", &home, None);
        assert_eq!(login, Some(".config/fish/config.fish"));
        // No ~/.config/fish: nothing is created, and no directory either.
        assert_eq!(ensure_rc_sources_hooks(&home, RcWiring::Rewire, login), []);
        assert!(fs::symlink_metadata(home.join(".config")).is_err());
        assert_eq!(absent_login_rc(&home, ".config/fish/config.fish"), None);

        // ~/.config is a link into ~/Documents (a dotfiles checkout there).
        fs::create_dir_all(home.join("Documents/dotfiles/fish")).unwrap();
        std::os::unix::fs::symlink(home.join("Documents/dotfiles"), home.join(".config")).unwrap();
        assert_eq!(
            ensure_rc_sources_hooks(&home, RcWiring::Rewire, login),
            [(".config/fish/config.fish", RcOutcome::ConsentFenced)]
        );
        assert!(!home.join("Documents/dotfiles/fish/config.fish").exists());
        assert_eq!(
            absent_login_rc(&home, ".config/fish/config.fish"),
            Some(RcState::ConsentFenced)
        );
    }

    /// A machine wired before the ledger existed opts out on its first deletion: a pass
    /// records an rc it finds already wired, rewriting nothing.
    #[cfg(unix)]
    #[test]
    fn an_rc_wired_before_the_ledger_existed_is_recorded_by_the_next_pass() {
        let home = tmp("rcpreledger");
        let bashrc = home.join(".bashrc");
        let user = "export FOO=1\n";
        let hook = home.join(".aterm").join("shell.d").join("00-atpkg.bash");
        let pre = format!(
            "{user}\n{RC_BEGIN}\n[ -f \"{p}\" ] && . \"{p}\"\n{RC_END}\n",
            p = hook.display()
        );
        fs::write(&bashrc, &pre).unwrap();
        assert!(
            !rc_ledger_path(&home).exists(),
            "the pre-2026-09-16 shape: wired, with nothing recorded"
        );

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        assert_eq!(
            fs::read_to_string(&bashrc).unwrap(),
            pre,
            "an already-wired rc is still never rewritten"
        );
        assert!(
            rc_ledger_path(&home).is_file(),
            "the pass records what it found wired"
        );

        // The first deletion on such a machine is honoured.
        fs::write(&bashrc, user).unwrap();
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        assert_eq!(
            fs::read_to_string(&bashrc).unwrap(),
            user,
            "the first deletion sticks on a machine wired by an older build"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// The `shell` a real-shell test here runs, as `aterm_spec::host::test_shell` finds
    /// it (the resolver aterm-shell-integration's live tests use too), or `None` with
    /// `shell` added to `absent`. bash must be found on a unix host.
    #[cfg(unix)]
    fn real_shell<'a>(shell: &'a str, absent: &mut Vec<&'a str>) -> Option<PathBuf> {
        let found = aterm_spec::host::test_shell(shell);
        if found.is_none() {
            assert_ne!(shell, "bash", "bash must be found on a unix host");
            absent.push(shell);
        }
        found
    }

    /// A HOME with a SPACE still gets the toolchain, and no shell prints an error.
    ///
    /// The rc line interpolates the hook path through [`sh_quote`], whose contract is a
    /// DOUBLE-QUOTED context — so the interpolation has to carry the quotes, and until
    /// 2026-09-16 it did not. With `HOME=/Users//Jane Doe` the line word-split:
    /// `zsh:[:1: too many arguments`, bash's `[: binary operator expected`, at EVERY
    /// interactive shell start, and the hook never sourced — which is the whole point of
    /// the block. Marker-bounded, so no later pass would have repaired it. Replayed in
    /// the real shells, fish's included, running the generated line exactly as the
    /// user's rc runs it, each in a scratch HOME (`aterm_spec::host::shell_command`).
    #[cfg(unix)]
    #[test]
    fn rc_wiring_quotes_the_hook_path_so_a_home_with_a_space_still_sources_it() {
        let root = tmp("rcspace");
        let home = root.join("Jane Doe");
        let shell_d = home.join(".aterm").join("shell.d");
        fs::create_dir_all(&shell_d).unwrap();
        for (rc, hook) in [
            (".zshrc", "00-atpkg.zsh"),
            (".bashrc", "00-atpkg.bash"),
            (".config/fish/config.fish", "00-atpkg.fish"),
        ] {
            let rc_path = home.join(rc);
            fs::create_dir_all(rc_path.parent().unwrap()).unwrap();
            fs::write(&rc_path, "# pre-existing user content\n").unwrap();
            let body = if hook.ends_with(".fish") {
                "set -g ATPKG_RC_TEST sourced\n"
            } else {
                "ATPKG_RC_TEST=sourced\n"
            };
            fs::write(shell_d.join(hook), body).unwrap();
        }

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);

        // The generated source line for one dialect, read from inside the markers.
        let line_of = |rc: &str| -> String {
            let body = fs::read_to_string(home.join(rc)).unwrap();
            let (_, after) = body.split_once(RC_BEGIN).expect("block written");
            let (block, _) = after.split_once(RC_END).expect("block closed");
            block
                .lines()
                .find(|l| l.contains("00-atpkg."))
                .unwrap_or_else(|| panic!("{rc}: no source line in the block"))
                .to_owned()
        };

        // Presence is decided by the resolver alone (the idiom of the other real-shell
        // tests): a shell it finds nowhere ends the test COULD NOT RUN.
        let mut absent = Vec::new();
        for (shell, args, rc) in [
            ("bash", &["--noprofile", "--norc", "-c"][..], ".bashrc"),
            ("zsh", &["-f", "-c"][..], ".zshrc"),
            (
                "fish",
                &["--no-config", "-c"][..],
                ".config/fish/config.fish",
            ),
        ] {
            let Some(program) = real_shell(shell, &mut absent) else {
                continue;
            };
            let line = line_of(rc);
            // The scratch HOME is this test's own home; `--no-config` keeps fish from
            // sourcing the rc itself, so the line runs once, as written.
            let out = aterm_spec::host::shell_command(&program, &home)
                .args(args)
                .arg(format!("{line}\nprintf 'X=%s\\n' \"$ATPKG_RC_TEST\""))
                .output()
                .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stderr.is_empty(),
                "{shell}: a home with a space must not error at shell start: exit {:?}, stderr={stderr:?}, line={line:?}",
                out.status.code()
            );
            assert_eq!(
                stdout.trim_end(),
                "X=sourced",
                "{shell}: the hook must actually be sourced; line={line:?}"
            );
        }

        // And as text, where it is a fact of the generator whatever the host: BOTH
        // interpolations quoted (`test -f "..."; and source "..."`).
        let fish = line_of(".config/fish/config.fish");
        let fish_quoted = format!("\"{}\"", shell_d.join("00-atpkg.fish").display());
        assert_eq!(
            fish.matches(fish_quoted.as_str()).count(),
            2,
            "fish: both interpolations must be double-quoted, got {fish:?}"
        );

        let _ = fs::remove_dir_all(&root);
        aterm_spec::host::could_not_run_without(&absent);
    }

    /// The block's line leaves `$?` = 0 when the hook is missing, replayed in the real shells
    /// over a `shell.d` that holds no hook.
    #[cfg(unix)]
    #[test]
    fn the_rc_line_is_status_neutral_when_the_hook_is_missing() {
        let home = tmp("rcstatus");
        // The rc files exist; the hooks deliberately do not.
        for rc in [".zshrc", ".bashrc", ".config/fish/config.fish"] {
            let rc_path = home.join(rc);
            fs::create_dir_all(rc_path.parent().unwrap()).unwrap();
            fs::write(&rc_path, "# pre-existing user content\n").unwrap();
        }
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);

        let line_of = |rc: &str| -> String {
            let body = fs::read_to_string(home.join(rc)).unwrap();
            let (_, after) = body.split_once(RC_BEGIN).expect("block written");
            let (block, _) = after.split_once(RC_END).expect("block closed");
            block
                .lines()
                .find(|l| l.contains("00-atpkg."))
                .unwrap_or_else(|| panic!("{rc}: no source line in the block"))
                .to_owned()
        };

        let mut absent = Vec::new();
        for (shell, args, rc) in [
            ("bash", &["--noprofile", "--norc", "-c"][..], ".bashrc"),
            ("zsh", &["-f", "-c"][..], ".zshrc"),
            (
                "fish",
                &["--no-config", "-c"][..],
                ".config/fish/config.fish",
            ),
        ] {
            let Some(program) = real_shell(shell, &mut absent) else {
                continue;
            };
            let line = line_of(rc);
            let out = aterm_spec::host::shell_command(&program, &home)
                .args(args)
                .arg(&line)
                .output()
                .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
            assert_eq!(
                out.status.code(),
                Some(0),
                "{shell}: an ABSENT hook must still leave $? = 0 — this line is the last \
                 thing the rc runs; line={line:?}"
            );
        }

        // And as text: the status-neutral form is the generator's, whatever the host.
        let fish = line_of(".config/fish/config.fish");
        assert!(
            fish.starts_with("if test -f ") && fish.ends_with("; end"),
            "fish: the line must be the status-neutral `if …; end` form, got {fish:?}"
        );

        let _ = fs::remove_dir_all(&home);
        aterm_spec::host::could_not_run_without(&absent);
    }

    /// A pass whose hook write failed wires no rc: a block naming a missing hook sources
    /// nothing and is never rewritten.
    #[cfg(unix)]
    #[test]
    fn a_failed_hook_write_wires_no_rc_block() {
        let home = tmp("rcnohook");
        let zshrc = home.join(".zshrc");
        let user = "# pre-existing user content\nexport FOO=1\n";
        fs::write(&zshrc, user).unwrap();
        let shell_d = home.join(".aterm").join("shell.d");
        fs::create_dir_all(&shell_d).unwrap();
        // A directory stands where the hook writer must create its temp file, so the first
        // dialect's write fails.
        let blocked = shell_d.join(format!(".{HOOK_BASENAME}.zsh.tmp-{}", std::process::id()));
        fs::create_dir_all(&blocked).unwrap();

        refresh_at(
            &home,
            Path::new("/opt/atpkg-fixture/bin"),
            Path::new("/opt/atpkg-fixture/agents"),
            Path::new("/opt/atpkg-fixture/reroute"),
            RcWiring::HonorOptOut,
            None,
        );

        assert!(
            !shell_d.join("00-atpkg.zsh").is_file(),
            "the hook write must have failed for this test to mean anything"
        );
        assert_eq!(
            fs::read_to_string(&zshrc).unwrap(),
            user,
            "no block may be wired when the hook it names was never written"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// A symlinked rc (stow, yadm/chezmoi, home-manager) gets the block written through the
    /// link and stays a link with its mode; a dangling link is left alone.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_writes_through_a_symlinked_rc_and_keeps_it_a_link_and_its_mode() {
        let home = tmp("rcsymlink");
        let dot = home.join("dotfiles");
        fs::create_dir_all(&dot).unwrap();
        let target = dot.join("zshrc");
        fs::write(&target, "export FOO=1\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = home.join(".zshrc");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // A dangling link is refused, never replaced by a file of our own.
        let gone = dot.join("bashrc-not-checked-out");
        std::os::unix::fs::symlink(&gone, home.join(".bashrc")).unwrap();
        // A regular 0600 rc keeps its mode across the rewrite.
        let fish = home.join(".config/fish/config.fish");
        fs::create_dir_all(fish.parent().unwrap()).unwrap();
        fs::write(&fish, "set -x FOO 1\n").unwrap();
        fs::set_permissions(&fish, fs::Permissions::from_mode(0o600)).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the rc must stay a symlink"
        );
        assert_eq!(fs::read_link(&link).unwrap(), target);
        let through = fs::read_to_string(&target).unwrap();
        assert!(
            through.contains(RC_BEGIN),
            "the block lands in the dotfile: {through}"
        );
        assert!(through.starts_with("export FOO=1\n"), "{through}");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600,
            "a 0600 rc must not widen"
        );
        assert!(
            fs::symlink_metadata(home.join(".bashrc"))
                .unwrap()
                .file_type()
                .is_symlink()
                && !gone.exists(),
            "a dangling rc link is left exactly as it was"
        );
        assert!(fs::read_to_string(&fish).unwrap().contains(RC_BEGIN));
        assert_eq!(
            fs::metadata(&fish).unwrap().permissions().mode() & 0o777,
            0o600,
            "a regular 0600 rc must not widen either"
        );

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        assert_eq!(
            fs::read_to_string(&target)
                .unwrap()
                .matches(RC_BEGIN)
                .count(),
            1,
            "exactly one block through the link"
        );
        for dir in [&home, &dot, &fish.parent().unwrap().to_path_buf()] {
            let strays: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("atpkg-") && n.ends_with(".tmp"))
                .collect();
            assert!(
                strays.is_empty(),
                "temp files left in {}: {strays:?}",
                dir.display()
            );
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// A bash login shell reads `~/.bash_profile`, not `~/.bashrc`: a home with
    /// `.bash_profile` gets the block, sourcing the same `00-atpkg.bash`; a home with both
    /// gets it in both; and `.bashrc` is never invented beside a lone `.bash_profile`.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_reaches_a_bash_login_shell_through_an_existing_bash_profile() {
        let home = tmp("bashprofile");
        let profile = home.join(".bash_profile");
        fs::write(&profile, "# login-only content\nexport FOO=1").unwrap(); // no final newline

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        let wired = fs::read_to_string(&profile).unwrap();
        assert!(
            wired.contains(RC_BEGIN) && wired.contains(RC_END),
            "block written into .bash_profile: {wired}"
        );
        assert!(
            wired.contains(".aterm/shell.d/00-atpkg.bash"),
            "it sources the bash hook, the same one .bashrc sources: {wired}"
        );
        assert!(
            wired.starts_with("# login-only content\nexport FOO=1\n\n"),
            "the user's content stays first and gains the newline it lacked: {wired}"
        );
        assert!(
            !home.join(".bashrc").exists(),
            ".bashrc must not be invented beside a lone .bash_profile"
        );

        // Both present: both wired, each with exactly one block and the same hook.
        let bashrc = home.join(".bashrc");
        fs::write(&bashrc, "export BAR=1\n").unwrap();
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);
        for rc in [&profile, &bashrc] {
            let body = fs::read_to_string(rc).unwrap();
            assert_eq!(
                body.matches(RC_BEGIN).count(),
                1,
                "exactly one block in {}",
                rc.display()
            );
            assert!(body.contains("00-atpkg.bash"), "{}", rc.display());
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// The consent fence: an rc that resolves under a protected root (through a symlinked rc,
    /// or a symlinked parent of a regular rc) is left exactly as it was, while an rc beside
    /// it that resolves somewhere ordinary is still wired.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_never_opens_an_rc_that_resolves_under_a_protected_folder() {
        let home = tmp("rcprotected");
        let docs = home.join("Documents").join("dotfiles");
        fs::create_dir_all(docs.join("config").join("fish")).unwrap();
        // ~/.zshrc -> ~/Documents/dotfiles/zshrc
        let zshrc = docs.join("zshrc");
        fs::write(&zshrc, "export FOO=1\n").unwrap();
        std::os::unix::fs::symlink(&zshrc, home.join(".zshrc")).unwrap();
        // ~/.config -> ~/Documents/dotfiles/config, with a regular config.fish inside
        // it: the rc path itself is not a link, its parent is.
        let fish = docs.join("config").join("fish").join("config.fish");
        fs::write(&fish, "set -x FOO 1\n").unwrap();
        std::os::unix::fs::symlink(docs.join("config"), home.join(".config")).unwrap();
        // ~/.bashrc -> ~/dotfiles/bashrc: a link into an ordinary directory, the control.
        let plain = home.join("dotfiles");
        fs::create_dir_all(&plain).unwrap();
        let bashrc = plain.join("bashrc");
        fs::write(&bashrc, "export BAR=1\n").unwrap();
        std::os::unix::fs::symlink(&bashrc, home.join(".bashrc")).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);

        assert_eq!(
            fs::read_to_string(&zshrc).unwrap(),
            "export FOO=1\n",
            "an rc reached through a link into Documents is left exactly as it was"
        );
        assert_eq!(
            fs::read_to_string(&fish).unwrap(),
            "set -x FOO 1\n",
            "a regular rc under a symlinked ~/.config that resolves into Documents too"
        );
        assert!(
            fs::read_to_string(&bashrc).unwrap().contains(RC_BEGIN),
            "the control: a link into an ordinary directory is still wired"
        );
        // Nothing was written anywhere under the protected root — not even a temp.
        let mut stack = vec![home.join("Documents")];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                assert!(!name.contains("atpkg"), "wrote {name:?} under Documents");
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                }
            }
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// The same fence for the command links: a `~/.local` linked under a protected root is
    /// refused.
    #[cfg(unix)]
    #[test]
    fn command_links_stay_out_of_a_protected_local_bin() {
        let home = tmp("cmdlinks");
        // Ordinary: nothing exists yet — the directory would be created under $HOME.
        assert_eq!(command_links_dir(&home), Some(home.join(".local/bin")));
        // Ordinary: ~/.local is a real directory.
        fs::create_dir_all(home.join(".local")).unwrap();
        assert_eq!(command_links_dir(&home), Some(home.join(".local/bin")));
        fs::remove_dir_all(home.join(".local")).unwrap();
        // ~/.local -> ~/Documents/dotlocal: a create under it would land in Documents.
        let dot = home.join("Documents").join("dotlocal");
        fs::create_dir_all(&dot).unwrap();
        std::os::unix::fs::symlink(&dot, home.join(".local")).unwrap();
        assert_eq!(
            command_links_dir(&home),
            None,
            "refused: resolves under Documents"
        );
        // …and once bin/ exists inside it, still refused.
        fs::create_dir_all(dot.join("bin")).unwrap();
        assert_eq!(command_links_dir(&home), None);
        let _ = fs::remove_dir_all(&home);
    }

    /// The PATH names are the release bundle's to write: from the dev bundle nothing is
    /// planted and a release link is left alone; from the release bundle an absent link is
    /// planted, a link into an older release location is repointed, and a link
    /// `ship dev-app` aimed at the dev bundle is left alone.
    #[cfg(unix)]
    #[test]
    fn command_links_are_written_only_from_the_release_bundle() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tmp("cmdlink-owner");
        let root = scratch.canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let bundle = |app: &str| -> PathBuf {
            let macos = root.join("Applications").join(app).join("Contents/MacOS");
            fs::create_dir_all(&macos).unwrap();
            for name in ["aterm", "atpkg"] {
                let p = macos.join(name);
                fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
            macos
        };
        let release = bundle("aterm.app");
        let dev = bundle("aterm (dev).app");
        let old_release = root.join("Old").join("aterm.app").join("Contents/MacOS");
        let bin = home.join(".local/bin");
        let target = |name: &str| fs::read_link(bin.join(name)).ok();
        let set = |name: &str, to: &Path| {
            let _ = fs::remove_file(bin.join(name));
            std::os::unix::fs::symlink(to, bin.join(name)).unwrap();
        };
        // 1. From the dev bundle, with nothing at the link path: nothing is planted.
        ensure_command_links_from(&home, &dev.join("aterm"));
        assert_eq!(target("aterm"), None, "a dev bundle plants no `aterm`");
        assert_eq!(target("atpkg"), None, "a dev bundle plants no `atpkg`");

        // 2. From the release bundle: both are planted into it.
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("aterm"), Some(release.join("aterm")));
        assert_eq!(target("atpkg"), Some(release.join("atpkg")));

        // 3. The release owns them, then the dev bundle runs: untouched.
        ensure_command_links_from(&home, &dev.join("aterm"));
        assert_eq!(
            target("aterm"),
            Some(release.join("aterm")),
            "a dev bundle's launch must not take the release's `aterm`"
        );
        assert_eq!(
            target("atpkg"),
            Some(release.join("atpkg")),
            "a dev bundle's launch must not take the release's `atpkg`"
        );

        // 4. Moves still track: a link into an older `aterm.app` location is repointed.
        set("aterm", &old_release.join("aterm"));
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("aterm"), Some(release.join("aterm")));

        // 5. The stated choice stands: `ship dev-app` aimed `aterm` at the dev bundle,
        //    and the release leaves it there.
        set("aterm", &dev.join("aterm"));
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("aterm"), Some(dev.join("aterm")));

        // 6. The leftover heals: an `atpkg` into the dev bundle is no tool's choice, so the
        //    release takes it back, while the `aterm` beside it stays.
        set("atpkg", &dev.join("atpkg"));
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("atpkg"), Some(release.join("atpkg")));
        assert_eq!(target("aterm"), Some(dev.join("aterm")));

        assert!(in_release_bundle(&release.join("aterm")));
        assert!(!in_release_bundle(&dev.join("aterm")));
        assert!(is_non_release_bundle_exe(&dev.join("aterm")));
        assert!(!is_non_release_bundle_exe(&release.join("aterm")));
        assert!(
            !is_non_release_bundle_exe(&root.join("target/release/aterm")),
            "a source build is not a bundle, and is not caught"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// `amacs` JOINS THE COMMAND LINKS (`docs/DESIGN-amacs-2026-10-19.md` §8.1), owned as
    /// `aterm` is. Driven through the real pass on bundle trees of its own, laid as a bundle
    /// is (aliases are links to `aterm`): a release from before amacs lays nothing at the
    /// name; the release with it points `~/.local/bin/amacs` into itself; a dev bundle's
    /// launch takes nothing; `amacs` follows `aterm` (aimed at a dev bundle together, it
    /// stays; once `aterm` runs the release again, it comes back), while a link to a build
    /// outside any bundle stays; and a file there is never touched unattended, while on
    /// macOS `doctor` names it and `repair` moves it aside and links the app in its place.
    #[cfg(unix)]
    #[test]
    fn command_links_lay_amacs_on_macos() {
        use std::os::unix::fs::symlink;
        let scratch = tmp("cmdlink-amacs");
        let root = scratch.canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let bundle = |app: &str, aliases: &[&str]| -> PathBuf {
            let macos = root.join(app).join("Contents/MacOS");
            fs::create_dir_all(&macos).unwrap();
            let aterm = macos.join("aterm");
            fs::write(&aterm, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).unwrap();
            for alias in aliases {
                symlink("aterm", macos.join(alias)).unwrap();
            }
            macos
        };
        let before = bundle("Old/aterm.app", &["atpkg"]);
        let release = bundle("Applications/aterm.app", &["atpkg", "amacs"]);
        let dev = bundle("Applications/aterm (dev).app", &["atpkg", "amacs"]);
        let bin = home.join(".local/bin");
        let target = |name: &str| fs::read_link(bin.join(name)).ok();

        // 1. A release from before amacs: no `Contents/MacOS/amacs`, no link to one.
        ensure_command_links_from(&home, &before.join("aterm"));
        assert_eq!(target("atpkg"), Some(before.join("atpkg")));
        assert_eq!(
            target("amacs"),
            None,
            "a bundle without amacs lays no link to it"
        );

        // 2. The release with amacs: all three point into it, the other two repointed from
        //    the older release location, and `amacs` runs the app's one binary.
        ensure_command_links_from(&home, &release.join("aterm"));
        for name in COMMAND_LINKS {
            assert_eq!(target(name), Some(release.join(name)), "{name}");
        }
        assert_eq!(
            fs::canonicalize(bin.join("amacs")).unwrap(),
            release.join("aterm")
        );

        // 3. A dev bundle's launch takes nothing from the release.
        ensure_command_links_from(&home, &dev.join("aterm"));
        assert_eq!(target("amacs"), Some(release.join("amacs")));

        // 4. `amacs` follows `aterm`. `ship dev-app` aims the pair at the dev bundle, and
        //    the release leaves both: the dev window's tabs run the dev editor client.
        let aim = |name: &str, at: &Path| {
            fs::remove_file(bin.join(name)).unwrap();
            symlink(at, bin.join(name)).unwrap();
        };
        aim("aterm", &dev.join("aterm"));
        aim("amacs", &dev.join("amacs"));
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("aterm"), Some(dev.join("aterm")));
        assert_eq!(target("amacs"), Some(dev.join("amacs")));
        //    Rejoining the release (`tools/install.sh` relinks `aterm` alone): the next
        //    pass brings the editor back with it.
        aim("aterm", &release.join("aterm"));
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("amacs"), Some(release.join("amacs")));
        //    NEGATIVE CONTROL: an `amacs` aimed at someone's own build, outside any bundle,
        //    stays with `aterm` on the release.
        let own = root.join("src/amacs/target/debug/amacs");
        aim("amacs", &own);
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(target("amacs"), Some(own));

        // 5. A file at the name: left byte-identical by the unattended pass. The installed
        //    commands follow COMMAND_LINKS on macOS only, so only there does doctor name it
        //    and repair move it aside.
        fs::remove_file(bin.join("amacs")).unwrap();
        fs::write(bin.join("amacs"), b"someone's own").unwrap();
        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(fs::read(bin.join("amacs")).unwrap(), b"someone's own");
        assert_eq!(
            copied_command_links(&home),
            if cfg!(target_os = "macos") {
                vec![bin.join("amacs")]
            } else {
                Vec::new()
            }
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                move_aside_copied_commands_at(&home, &release.join("aterm"), 5),
                Ok(AsideOutcome {
                    moved: vec![MovedAside {
                        from: bin.join("amacs"),
                        to: bin.join("amacs.moved-aside-5"),
                        linked: true,
                    }],
                    failed: Vec::new(),
                })
            );
            assert_eq!(target("amacs"), Some(release.join("amacs")));
            assert_eq!(
                fs::read(bin.join("amacs.moved-aside-5")).unwrap(),
                b"someone's own",
                "moved, not deleted"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// THE STORE SELF-HEAL (`docs/DESIGN-amacs-2026-10-19.md` §8.1). A store install.sh laid
    /// before a name existed gains it beside the binary, spelled as install.sh spells its
    /// own (a link to `aterm`), and a second pass lays nothing. Whatever already holds a
    /// name stays exactly as it was: a file, a link to someone's own build, a dangling link.
    /// NEGATIVE CONTROLS: a build tree (its `aterm-ctl` is a dev binary of its own), a
    /// binary under another name, and a store without its `aterm-ctl` alias are not stores,
    /// and nothing is laid in them.
    #[cfg(unix)]
    #[test]
    fn store_aliases_self_heal_lays_missing_and_never_replaces_a_file() {
        use std::os::unix::fs::symlink;
        let scratch = tmp("store-aliases");
        let root = scratch.canonicalize().unwrap();
        let resolve = |path: &Path| fs::canonicalize(path).ok();
        let binary = |dir: &Path, name: &str| -> PathBuf {
            fs::create_dir_all(dir).unwrap();
            let exe = dir.join(name);
            fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
            exe
        };
        let store = root.join("home/.local/lib/aterm/bin");
        let aterm = binary(&store, "aterm");
        symlink("aterm", store.join("aterm-ctl")).unwrap();
        fs::write(store.join("atpkg"), b"someone's own").unwrap();
        symlink("/opt/their/aterm-gui", store.join("aterm-gui")).unwrap();
        symlink("gone", store.join("abrew")).unwrap();
        let held = ["aterm-ctl", "atpkg", "aterm-gui", "abrew"];

        assert_eq!(store_dir_of(&aterm, resolve), Some(store.as_path()));
        let laid = heal_store_aliases(&aterm);
        let missing: Vec<&str> = aterm_types::argv0::ALIASES
            .into_iter()
            .filter(|name| !held.contains(name))
            .collect();
        assert_eq!(laid, missing);
        assert!(laid.contains(&"amacs"), "the editor's alias is laid");
        for name in &laid {
            assert_eq!(fs::read_link(store.join(name)).unwrap(), Path::new("aterm"));
            assert_eq!(fs::canonicalize(store.join(name)).unwrap(), aterm, "{name}");
        }
        assert_eq!(
            fs::read(store.join("atpkg")).unwrap(),
            b"someone's own",
            "a file is never replaced"
        );
        assert_eq!(
            fs::read_link(store.join("aterm-gui")).unwrap(),
            Path::new("/opt/their/aterm-gui")
        );
        assert_eq!(
            fs::read_link(store.join("abrew")).unwrap(),
            Path::new("gone")
        );
        assert!(
            heal_store_aliases(&aterm).is_empty(),
            "a second pass lays nothing"
        );

        let tree = root.join("checkout/target/debug");
        let dev = binary(&tree, "aterm");
        binary(&tree, "aterm-ctl");
        let renamed_dir = root.join("renamed");
        let renamed = binary(&renamed_dir, "aterm-next");
        symlink("aterm-next", renamed_dir.join("aterm-ctl")).unwrap();
        let bare_dir = root.join("bare");
        let bare = binary(&bare_dir, "aterm");
        for (exe, dir) in [(dev, tree), (renamed, renamed_dir), (bare, bare_dir)] {
            assert_eq!(store_dir_of(&exe, resolve), None, "{}", exe.display());
            assert!(heal_store_aliases(&exe).is_empty(), "{}", exe.display());
            assert!(
                fs::symlink_metadata(dir.join("amacs")).is_err(),
                "nothing laid beside {}",
                exe.display()
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// THE HOOKS PASS RUNS THE HEAL, AND FIRST: through the pass every caller takes
    /// ([`refresh_as`], which `refresh` and `refresh_rewiring_rc` reach), a store's missing
    /// alias is laid even with no $HOME, where the pass then hardens and writes nothing. With
    /// the heal off, as [`HEALS_STORE`] has it on macOS, the same pass lays nothing.
    #[cfg(unix)]
    #[test]
    fn the_hooks_pass_heals_a_store_before_it_needs_a_home() {
        use std::os::unix::fs::symlink;
        let scratch = tmp("store-pass");
        let root = scratch.canonicalize().unwrap();
        let layout = Layout {
            prefix: root.join("pkg"),
        };
        let store = root.join("lib/aterm/bin");
        fs::create_dir_all(&store).unwrap();
        let aterm = store.join("aterm");
        fs::write(&aterm, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("aterm", store.join("aterm-ctl")).unwrap();
        let amacs = store.join("amacs");

        for wiring in [RcWiring::HonorOptOut, RcWiring::Rewire] {
            let pass = refresh_as(&layout, wiring, Some(&aterm), None, false, None);
            assert_eq!(pass, HookPass::NotHardened);
            assert!(
                fs::symlink_metadata(&amacs).is_err(),
                "heal off: nothing laid"
            );
        }
        let pass = refresh_as(
            &layout,
            RcWiring::HonorOptOut,
            Some(&aterm),
            None,
            true,
            None,
        );
        assert_eq!(pass, HookPass::NotHardened, "no $HOME: no hook is written");
        assert_eq!(fs::read_link(&amacs).unwrap(), Path::new("aterm"));
        assert!(
            !layout.prefix.exists(),
            "the heal writes the store and nothing else"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A copied binary at a link path stays until the owner asks: the unattended pass leaves
    /// it byte-identical and [`copied_command_links`] names it for `doctor`. `repair` from the
    /// release bundle renames it aside, never over an earlier one, and links the app. From a
    /// dev bundle it moves nothing, because nothing would take the name.
    #[cfg(unix)]
    #[test]
    fn a_copied_command_is_left_alone_unattended_and_moved_aside_only_by_repair() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tmp("cmdlink-copied");
        let root = scratch.canonicalize().unwrap();
        let home = root.join("home");
        let bin = home.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let bundle = |app: &str| -> PathBuf {
            let macos = root.join("Applications").join(app).join("Contents/MacOS");
            fs::create_dir_all(&macos).unwrap();
            for name in ["aterm", "atpkg"] {
                let p = macos.join(name);
                fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            }
            macos
        };
        let release = bundle("aterm.app");
        let dev = bundle("aterm (dev).app");
        let copied = bin.join("aterm");
        let bytes = b"#!/bin/sh\necho 'aterm 0.15.0'\n";
        fs::write(&copied, bytes).unwrap();
        let is_file = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());

        ensure_command_links_from(&home, &release.join("aterm"));
        assert_eq!(
            fs::read(&copied).unwrap(),
            bytes,
            "the unattended pass never touches it"
        );
        assert_eq!(
            fs::read_link(bin.join("atpkg")).ok(),
            Some(release.join("atpkg")),
            "the name beside it is still linked"
        );
        assert_eq!(copied_command_links(&home), vec![copied.clone()]);

        assert!(
            move_aside_copied_commands_at(&home, &dev.join("aterm"), 7).is_err(),
            "a dev bundle does not lay the links"
        );
        assert!(
            is_file(&copied),
            "from a dev bundle nothing would take the name"
        );

        let earlier = bin.join("aterm.moved-aside-7");
        fs::write(&earlier, b"earlier").unwrap();
        let refused = move_aside_copied_commands_at(&home, &release.join("aterm"), 7).unwrap();
        assert!(refused.moved.is_empty());
        assert_eq!(refused.failed.len(), 1);
        assert_eq!(
            fs::read(&earlier).unwrap(),
            b"earlier",
            "an earlier move is never overwritten"
        );
        assert!(is_file(&copied));

        let aside = bin.join("aterm.moved-aside-9");
        let ctl = bin.join("aterm-ctl");
        fs::write(&ctl, b"old ctl").unwrap();
        assert_eq!(
            move_aside_copied_commands_at(&home, &release.join("aterm"), 9),
            Ok(AsideOutcome {
                moved: vec![
                    MovedAside {
                        from: copied.clone(),
                        to: aside.clone(),
                        linked: true,
                    },
                    MovedAside {
                        from: ctl.clone(),
                        to: bin.join("aterm-ctl.moved-aside-9"),
                        linked: false,
                    },
                ],
                failed: Vec::new(),
            })
        );
        assert!(
            fs::symlink_metadata(&ctl).is_err(),
            "a retired name is moved and nothing is linked in its place"
        );
        assert_eq!(fs::read(&aside).unwrap(), bytes, "renamed, not deleted");
        assert_eq!(fs::read_link(&copied).ok(), Some(release.join("aterm")));
        assert!(copied_command_links(&home).is_empty());
        assert!(
            move_aside_copied_commands_at(&home, &release.join("aterm"), 10)
                == Ok(AsideOutcome::default()),
            "a second repair has nothing to move"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A move that cannot finish changes nothing: with the temporary link's name taken by a
    /// directory, the file keeps its name and no second name is left behind.
    #[cfg(unix)]
    #[test]
    fn a_move_aside_that_cannot_finish_changes_nothing() {
        let scratch = tmp("aside-rollback");
        let dir = scratch.canonicalize().unwrap();
        let from = dir.join("aterm");
        let aside = dir.join("aterm.moved-aside-1");
        fs::write(&from, b"a build of someone's own").unwrap();
        let target = dir.join("target");
        fs::write(&target, b"").unwrap();
        let blocker = dir.join(format!("aterm.link-{}", std::process::id()));
        fs::create_dir_all(blocker.join("inside")).unwrap();

        assert!(rename_file_aside(&from, &aside, Some(&target)).is_err());
        assert_eq!(fs::read(&from).unwrap(), b"a build of someone's own");
        assert!(
            fs::symlink_metadata(&aside).is_err(),
            "no second name left behind"
        );

        fs::remove_dir_all(&blocker).unwrap();
        rename_file_aside(&from, &aside, Some(&target)).unwrap();
        assert_eq!(fs::read_link(&from).unwrap(), target);
        assert_eq!(fs::read(&aside).unwrap(), b"a build of someone's own");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A retired name is moved with nothing linked in its place; when its old name cannot be
    /// removed (a read-only folder), the second name is dropped and the file stays put.
    #[cfg(unix)]
    #[test]
    fn a_retired_name_that_cannot_leave_its_folder_stays_put() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::host::could_not_run_as_root("a folder whose mode lacks write");
        /// Puts the locked folder's mode back however the test ends.
        struct Unlock(PathBuf);
        impl Drop for Unlock {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }
        let scratch = tmp("aside-retired");
        let dir = scratch.canonicalize().unwrap();
        let locked = dir.join("locked");
        let open = dir.join("open");
        fs::create_dir_all(&locked).unwrap();
        fs::create_dir_all(&open).unwrap();
        let from = locked.join("aterm-ctl");
        let aside = open.join("aterm-ctl.moved-aside-1");
        fs::write(&from, b"an old aterm-ctl").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let unlock = Unlock(locked.clone());

        let moved = rename_file_aside(&from, &aside, None);
        drop(unlock);
        assert!(moved.is_err());
        assert_eq!(fs::read(&from).unwrap(), b"an old aterm-ctl");
        assert!(
            fs::symlink_metadata(&aside).is_err(),
            "no second name left behind"
        );

        rename_file_aside(&from, &aside, None).unwrap();
        assert!(
            fs::symlink_metadata(&from).is_err(),
            "moved, nothing in its place"
        );
        assert_eq!(fs::read(&aside).unwrap(), b"an old aterm-ctl");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The remedy for a copied command on macOS names the installed app's own `atpkg`:
    /// the alias, from the first app it would act from (never a translocated one), quoted
    /// as one shell word.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_remedy_names_the_installed_app_it_would_act_from() {
        // The installed-app fallback keeps the `atpkg` alias name, and skips an
        // app it would not act from.
        let scratch = tmp("hint-installed");
        let root = scratch.canonicalize().unwrap();
        let macos = root.join("Apps/aterm.app/Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(macos.join("aterm"), b"").unwrap();
        std::os::unix::fs::symlink("aterm", macos.join("atpkg")).unwrap();
        // `Moved/aterm.app` resolves into a translocated mount, which atpkg
        // refuses to act from: it is skipped for the one it would act from.
        let mount = root.join("AppTranslocation/X/d/aterm.app/Contents/MacOS");
        fs::create_dir_all(&mount).unwrap();
        fs::write(mount.join("aterm"), b"").unwrap();
        std::os::unix::fs::symlink("aterm", mount.join("atpkg")).unwrap();
        fs::create_dir_all(root.join("Moved")).unwrap();
        std::os::unix::fs::symlink(
            root.join("AppTranslocation/X/d/aterm.app"),
            root.join("Moved/aterm.app"),
        )
        .unwrap();
        assert_eq!(installed_app_atpkg(&[Some(root.join("Moved"))]), None);
        assert_eq!(
            installed_app_atpkg(&[None, Some(root.join("Moved")), Some(root.join("Apps"))]),
            Some(macos.join("atpkg")),
            "the alias, not the binary it links to, from the app it would act from"
        );
        assert_eq!(installed_app_atpkg(&[Some(root.join("Nowhere"))]), None);
        // Two it would act from: the first listed wins.
        let second = root.join("Second/aterm.app/Contents/MacOS");
        fs::create_dir_all(&second).unwrap();
        fs::write(second.join("aterm"), b"").unwrap();
        std::os::unix::fs::symlink("aterm", second.join("atpkg")).unwrap();
        assert_eq!(
            installed_app_atpkg(&[Some(root.join("Second")), Some(root.join("Apps"))]),
            Some(second.join("atpkg"))
        );
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            repair_hint_for(Some(Path::new(
                "/Bob's Apps/aterm.app/Contents/MacOS/atpkg"
            ))),
            "`'/Bob'\\''s Apps/aterm.app/Contents/MacOS/atpkg' repair`"
        );
        assert_eq!(
            installed_app_dirs(Some(PathBuf::from("/Users//u"))),
            [
                Some(PathBuf::from("/Applications")),
                Some(PathBuf::from("/Users//u/Applications"))
            ],
            "the system folder first"
        );
    }

    /// The links are laid only from a bundle that stays: not a translocated copy and
    /// not one run from a mounted disk image or removable disk. A remedy names the app's
    /// own atpkg as one shell word, whatever its folder is called.
    #[cfg(all(unix, target_os = "macos"))]
    #[test]
    fn links_come_only_from_a_bundle_that_stays_and_the_remedy_is_one_word() {
        for gone in [
            "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
            "/private/var/folders/x/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
            "/Apps/aterm (dev).app/Contents/MacOS/aterm",
            "/src/target/release/aterm",
        ] {
            assert_eq!(command_link_source(Path::new(gone)), None, "{gone}");
            assert!(command_link_check(Path::new(gone)).is_err(), "{gone}");
        }
        assert!(
            command_link_source(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"))
                .is_some()
        );
    }

    /// The shell hooks are the release bundle's to lay unattended: from a dev bundle the
    /// unattended pass writes nothing, from the release bundle it writes, and `repair` writes
    /// from either.
    #[cfg(unix)]
    #[test]
    fn an_unattended_hooks_pass_from_a_dev_bundle_writes_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = tmp("hooks-owner");
        let root = scratch.canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let layout = Layout {
            prefix: root.join("pkg"),
        };
        let dev = root.join("Applications/aterm (dev).app/Contents/MacOS/aterm");
        let release = root.join("Applications/aterm.app/Contents/MacOS/aterm");
        let hook = home.join(".aterm/shell.d/00-atpkg.zsh");

        let pass = pass_as(&layout, &home, RcWiring::HonorOptOut, Some(&dev), None);
        assert_eq!(pass, HookPass::NotReleaseBundle);
        assert!(pass.rc().is_empty());
        assert!(
            !home.join(".aterm").exists(),
            "the unattended pass from a dev bundle touches nothing under $HOME"
        );

        let pass = pass_as(&layout, &home, RcWiring::HonorOptOut, Some(&release), None);
        assert!(matches!(pass, HookPass::Rewritten(_)), "{pass:?}");
        assert!(hook.exists(), "the release bundle lays the hook");

        fs::remove_file(&hook).unwrap();
        let pass = pass_as(&layout, &home, RcWiring::Rewire, Some(&dev), None);
        assert!(matches!(pass, HookPass::Rewritten(_)), "{pass:?}");
        assert!(
            hook.exists(),
            "repair is the asked-for pass and writes from anywhere"
        );

        let pass = pass_as(&layout, &home, RcWiring::HonorOptOut, None, None);
        assert!(
            matches!(pass, HookPass::Rewritten(_)),
            "an unresolvable executable never stops a pass: {pass:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn powershell_dialect_appends_bin_and_moves_agents_to_the_front_via_platform_separator() {
        let files = hook_files(
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        );
        let (_, ps) = files.iter().find(|(n, _)| n.ends_with(".ps1")).unwrap();
        assert!(
            ps.contains("[System.IO.Path]::PathSeparator"),
            "OS-correct separator"
        );
        assert!(
            ps.contains("$env:PATH = \"$env:PATH$__atpkg_sep$__atpkg_bin\""),
            "bin/ is appended, never prepended"
        );
        assert!(
            ps.contains("Where-Object { $_ -ne $__atpkg_agents -and $_ -ne $__atpkg_reroute }")
                && ps.contains("{ $__atpkg_front = @($__atpkg_agents); $env:ATPKG_AGENTS")
                && ps.contains("$env:PATH = ($__atpkg_front + $__atpkg_rest) -join $__atpkg_sep\n"),
            "agents/ is MOVED TO THE FRONT (the one exception): removed, then prepended"
        );
        // reroute/ ahead of it: any agents marker or the seams' handle, grouped explicitly,
        // and never under --no-reroute's marker.
        assert!(
            ps.contains(&format!(
                "if (($env:ATERM_AGENTS_DIR -or $env:ATERM_CHILD -or $env:ATERM_SESSION_ID -or $env:{r}) -and (-not $env:{p} -or $env:{p} -eq '0')) {{ $__atpkg_front = @($__atpkg_reroute) + $__atpkg_front }}",
                r = crate::reroute::REROUTE_DIR_ENV,
                p = crate::reroute::PASSTHROUGH_ENV
            )),
            "reroute/ leads agents/ inside aterm unless --no-reroute's marker is engaged: {ps}"
        );
        assert!(
            !ps.contains(&format!("$env:{} =", crate::reroute::REROUTE_DIR_ENV)),
            "the handle is read, never set: {ps}"
        );
        assert!(
            ps.contains("-notcontains $__atpkg_bin")
                && !ps.contains("-notcontains $__atpkg_agents"),
            "bin/ keeps its presence guard; agents/ has none — an order, not a presence"
        );
        assert!(ps.contains("$env:ATPKG_AGENTS = $__atpkg_agents"));
        assert!(!ps.contains("export "), "no POSIX export in a .ps1");
    }

    #[test]
    fn powershell_single_quotes_a_windows_path_so_backslashes_survive() {
        let files = hook_files(
            Path::new(r"C:\Users\x\AppData\Local\aterm\pkg\bin"),
            Path::new(r"C:\Users\x\AppData\Local\aterm\pkg\agents"),
            Path::new(r"C:\Users\x\AppData\Local\aterm\pkg\reroute"),
        );
        let (_, ps) = files.iter().find(|(n, _)| n.ends_with(".ps1")).unwrap();
        assert!(
            ps.contains(r"$__atpkg_bin = 'C:\Users\x\AppData\Local\aterm\pkg\bin'"),
            "a single-quoted PS literal keeps backslashes verbatim"
        );
        assert!(ps.contains(r"$__atpkg_agents = 'C:\Users\x\AppData\Local\aterm\pkg\agents'"));
        assert!(ps.contains(r"$__atpkg_reroute = 'C:\Users\x\AppData\Local\aterm\pkg\reroute'"));
    }

    #[test]
    fn fish_dialect_appends_bin_moves_agents_to_the_front_and_avoids_posix_export() {
        let files = hook_files(
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        );
        let (_, fish) = files.iter().find(|(n, _)| n.ends_with(".fish")).unwrap();
        assert!(
            fish.contains("set -gx PATH $PATH $__atpkg_bin"),
            "fish append"
        );
        assert!(
            fish.contains("set -gx PATH $__atpkg_front $__atpkg_rest\n")
                && fish.contains("set -l __atpkg_front\n")
                && fish.contains(&format!(
                    "if test -n \"$ATERM_AGENTS_DIR$ATERM_CHILD$ATERM_SESSION_ID\"; set __atpkg_front \"$__atpkg_agents\"; set -gx ATPKG_AGENTS $__atpkg_agents; else; set -e ATPKG_AGENTS; end\n\
                     if test -z \"${p}\"; or test \"${p}\" = 0; if test -n \"$ATERM_AGENTS_DIR$ATERM_CHILD$ATERM_SESSION_ID${r}\"; set __atpkg_front \"$__atpkg_reroute\" $__atpkg_front; end; end\n",
                    p = crate::reroute::PASSTHROUGH_ENV,
                    r = crate::reroute::REROUTE_DIR_ENV
                ))
                && !fish.contains(&format!("set -gx {}", crate::reroute::REROUTE_DIR_ENV))
                && fish.contains("if test \"$__atpkg_d\" != \"$__atpkg_agents\"; and test \"$__atpkg_d\" != \"$__atpkg_reroute\";"),
            "fish moves the agents dir (and the reroute dir ahead of it) to the front by equality"
        );
        assert!(fish.contains("set -gx ATPKG_AGENTS $__atpkg_agents"));
        assert!(
            fish.contains("set __atpkg_rest $__atpkg_rest \"$__atpkg_d\"")
                && !fish.contains("set -a"),
            "no `set -a`: fish 2.x lacks it, and a failed append would set PATH to the agents dir alone"
        );
        assert_eq!(
            fish.matches("if not contains").count(),
            1,
            "one presence guard, bin/'s"
        );
        assert!(!fish.contains("export "), "no POSIX export in fish");
    }

    /// The four hooks move as a set, or not at all: a staging failure on the third publishes
    /// none.
    #[cfg(unix)]
    #[test]
    fn a_failed_hook_write_replaces_none_of_the_dialects() {
        let d = tmp("writeset");
        // A directory stands where the third dialect's temp must be created.
        let blocked = d.join(format!(".{HOOK_BASENAME}.fish.tmp-{}", std::process::id()));
        fs::create_dir_all(&blocked).unwrap();

        let out = write_hooks(
            &d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        );
        assert!(
            out.is_err(),
            "the blocked dialect must fail the whole write"
        );
        for name in [
            "00-atpkg.zsh",
            "00-atpkg.bash",
            "00-atpkg.fish",
            "00-atpkg.ps1",
        ] {
            assert!(
                !d.join(name).exists(),
                "{name} must not be published when a sibling dialect could not be written"
            );
        }

        let _ = fs::remove_dir_all(&d);
    }

    /// No temp is left behind on any error arm, here a failed rename onto a directory after
    /// the temp was written and hardened.
    #[cfg(unix)]
    #[test]
    fn a_hook_write_that_fails_leaves_no_temp_behind() {
        let d = tmp("writetmp");
        // The rename target is a non-empty directory, so `rename` cannot replace it.
        let dest = d.join("00-atpkg.ps1");
        fs::create_dir_all(dest.join("occupied")).unwrap();

        let out = write_hooks(
            &d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        );
        assert!(out.is_err(), "a rename onto a directory must fail");

        let litter: Vec<String> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| n.starts_with(".00-atpkg.") && n.contains(".tmp-"))
            .collect();
        assert!(
            litter.is_empty(),
            "a failed hook write must leave no temp behind, found {litter:?}"
        );

        let _ = fs::remove_dir_all(&d);
    }

    /// The publish phase moves as a set too: when the last dialect's rename fails, the three
    /// already published over a previous pass's hooks are all put back, and neither a temp
    /// nor a saved predecessor is left.
    #[cfg(unix)]
    #[test]
    fn a_failed_publish_puts_back_every_dialect_it_had_already_replaced() {
        let d = tmp("publishset");
        // A previous pass's hooks stand for the three dialects that publish first.
        let old = "# the previous pass's hook\n";
        for name in ["00-atpkg.zsh", "00-atpkg.bash", "00-atpkg.fish"] {
            fs::write(d.join(name), old).unwrap();
        }
        // The last dialect's destination is a non-empty directory, so its rename fails with
        // the first three already renamed into place.
        fs::create_dir_all(d.join("00-atpkg.ps1").join("occupied")).unwrap();

        let out = write_hooks(
            &d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        );
        assert!(out.is_err(), "a rename onto a directory must fail the pass");

        for name in ["00-atpkg.zsh", "00-atpkg.bash", "00-atpkg.fish"] {
            assert_eq!(
                fs::read_to_string(d.join(name)).unwrap(),
                old,
                "{name} must carry the PREVIOUS pass's hook again — the four files are only \
                 ever correct as a set, and a partial publish is what suppressed the rc \
                 wiring for every shell"
            );
        }
        // And the unwind leaves neither a temp nor a saved predecessor behind.
        let litter: Vec<String> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| n.contains(".tmp-") || n.contains(".prev-"))
            .collect();
        assert!(
            litter.is_empty(),
            "a failed publish must leave no temp and no saved predecessor, found {litter:?}"
        );

        let _ = fs::remove_dir_all(&d);
    }

    /// fish is not a POSIX shell, and its double quotes are not sh's.
    ///
    /// The fish hook body and rc line were both rendered through `sh_quote`, which escapes a
    /// backtick. Between double quotes fish recognises only `\\`, `\"` and `\$`, keeps a
    /// backslash before anything else literally, and has no backtick command substitution at
    /// all — so a prefix or a `$HOME` holding a backtick was emitted with a backslash in
    /// front of it, naming a directory that does not exist.
    ///
    /// The quoter is held to those documented rules here, and to fish itself in
    /// [`the_fish_hook_names_a_hostile_path_verbatim_in_a_real_fish`].
    #[test]
    fn fish_quoting_escapes_fishs_metacharacters_and_leaves_a_backtick_alone() {
        let hostile = PathBuf::from("/opt/a`b/c$d/e\"f/g\\h/i j");
        assert_eq!(
            fish_quote(&hostile),
            "/opt/a`b/c\\$d/e\\\"f/g\\\\h/i j",
            "fish: escape backslash, double quote and dollar -- and nothing else"
        );
        assert_eq!(
            sh_quote(&hostile),
            "/opt/a\\`b/c\\$d/e\\\"f/g\\\\h/i j",
            "sh: the backtick IS special in a POSIX double-quoted string, and stays escaped"
        );

        let files = hook_files(&hostile, &hostile, &hostile);
        let body = |ext: &str| -> String {
            files
                .iter()
                .find(|(n, _)| n.ends_with(ext))
                .map(|(_, b)| b.clone())
                .unwrap_or_else(|| panic!("hook_files writes no {ext} hook"))
        };
        let fish = body(".fish");
        assert!(
            fish.contains("set -l __atpkg_bin \"/opt/a`b/c\\$d/"),
            "the fish body must carry the backtick unescaped and the dollar escaped: {fish:?}"
        );
        assert!(
            !fish.contains("a\\`b"),
            "a backtick must never be backslash-escaped in a fish string: {fish:?}"
        );
        // The POSIX body still escapes the backtick.
        assert!(
            body(".zsh").contains("a\\`b"),
            "the POSIX body must still escape the backtick"
        );
    }

    /// The fish quoter judged by fish: the hook body, rendered for a bin, agents and
    /// reroute dir under a path holding every character fish's double quotes treat
    /// specially (`\\`, `"`, `$`) and the ones they do not (a backtick, a space), is
    /// sourced by a real fish, which must name each directory byte for byte. A quoter that
    /// backslash-escaped the backtick (sh's rule) names a directory that does not exist
    /// (`a\\`b`), and one that left the dollar bare expands it. The rc line, over a
    /// `$HOME` of the same characters, must source its hook. COULD NOT RUN where the
    /// resolver finds no fish.
    #[cfg(unix)]
    #[test]
    fn the_fish_hook_names_a_hostile_path_verbatim_in_a_real_fish() {
        let root = tmp("fishhostile");
        let hostile = root
            .join("a`b")
            .join("c$d")
            .join("e\"f")
            .join("g\\h")
            .join("i j");
        let (bin, agents, reroute) = (
            hostile.join("bin"),
            hostile.join("agents"),
            hostile.join("reroute"),
        );
        for dir in [&bin, &agents, &reroute] {
            fs::create_dir_all(dir).unwrap();
        }
        let files = hook_files(&bin, &agents, &reroute);
        let hook = root.join("hook.fish");
        let body = &files.iter().find(|(n, _)| n.ends_with(".fish")).unwrap().1;
        fs::write(&hook, body).unwrap();
        let mut absent = Vec::new();
        if let Some(fish) = real_shell("fish", &mut absent) {
            let home = root.join("home");
            fs::create_dir_all(&home).unwrap();
            let out = aterm_spec::host::shell_command(&fish, &home)
                .args([
                    "--no-config",
                    "-c",
                    "source \"$ATPKG_TEST_HOOK\"; printf 'BIN=%s\\n' \"$ATPKG_BIN\"; \
                     printf 'AGENTS=%s\\n' \"$ATPKG_AGENTS\"; printf 'PATH=%s\\n' $PATH",
                ])
                .env("PATH", "/usr/bin:/bin")
                .env("ATERM_SESSION_ID", "atpkg-hook-test")
                .env_remove(crate::reroute::PASSTHROUGH_ENV)
                .env("ATPKG_TEST_HOOK", &hook)
                .output()
                .expect("fish runs");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stderr.is_empty(),
                "fish: exit {:?}, stderr={stderr:?}",
                out.status.code()
            );
            let lines: Vec<&str> = stdout.lines().collect();
            let path: Vec<&str> = lines
                .iter()
                .filter_map(|l| l.strip_prefix("PATH="))
                .collect();
            assert_eq!(
                lines.first().copied(),
                Some(format!("BIN={}", bin.display()).as_str()),
                "fish must name the bin dir verbatim: {stdout:?}"
            );
            assert!(
                lines.contains(&format!("AGENTS={}", agents.display()).as_str()),
                "fish must name the agents dir verbatim: {stdout:?}"
            );
            assert_eq!(
                path.last().copied(),
                Some(bin.to_str().unwrap()),
                "bin is appended last, verbatim: {stdout:?}"
            );
            assert_eq!(
                path.first().copied(),
                Some(reroute.to_str().unwrap()),
                "the reroute dir leads, verbatim: {stdout:?}"
            );

            // The rc line over a hostile $HOME sources its hook.
            let rc_home = hostile.join("home");
            let rc = rc_home.join(".config/fish/config.fish");
            fs::create_dir_all(rc.parent().unwrap()).unwrap();
            fs::write(&rc, "# pre-existing user content\n").unwrap();
            let shell_d = rc_home.join(".aterm").join("shell.d");
            fs::create_dir_all(&shell_d).unwrap();
            fs::write(
                shell_d.join(format!("{HOOK_BASENAME}.fish")),
                "set -g ATPKG_RC_TEST sourced\n",
            )
            .unwrap();
            ensure_rc_sources_hooks(&rc_home, RcWiring::HonorOptOut, None);
            let written = fs::read_to_string(&rc).unwrap();
            let line = written
                .lines()
                .find(|l| l.contains("00-atpkg.fish"))
                .unwrap_or_else(|| panic!("no fish source line in {written:?}"));
            let out = aterm_spec::host::shell_command(&fish, &home)
                .args(["--no-config", "-c"])
                .arg(format!("{line}\nprintf 'X=%s\\n' \"$ATPKG_RC_TEST\""))
                .output()
                .expect("fish runs");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim_end(),
                "X=sourced",
                "the fish rc line must source its hook from a hostile HOME: line={line:?}, \
                 stderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = fs::remove_dir_all(&root);
        aterm_spec::host::could_not_run_without(&absent);
    }

    /// AND THE FISH RC LINE TOO: a `$HOME` with a backtick still names the hook verbatim.
    #[cfg(unix)]
    #[test]
    fn the_fish_rc_line_carries_a_backtick_home_unescaped() {
        let home = crate::testdir::dir("atpkg-hooks-fish`tick-");
        let rc = home.join(".config/fish/config.fish");
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        fs::write(&rc, "# pre-existing user content\n").unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut, None);

        let written = fs::read_to_string(&rc).unwrap();
        let line = written
            .lines()
            .find(|l| l.contains("00-atpkg.fish"))
            .unwrap_or_else(|| panic!("no fish source line in {written:?}"))
            .to_owned();
        let hook = home.join(".aterm").join("shell.d").join("00-atpkg.fish");
        assert!(
            line.contains(&format!("\"{}\"", hook.display())),
            "the fish rc line must name the hook path verbatim, got {line:?}"
        );
        assert!(
            !line.contains("\\`"),
            "a backtick must never be backslash-escaped in the fish rc line, got {line:?}"
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn write_hooks_writes_all_dialects_and_removes_stray_sh() {
        let d = tmp("write");
        // Plant a stray POSIX .sh that fish would choke on.
        fs::write(d.join("00-atpkg.sh"), b"echo stray\n").unwrap();
        let written = write_hooks(
            &d,
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        )
        .unwrap();
        assert_eq!(written.len(), 4);
        for name in [
            "00-atpkg.zsh",
            "00-atpkg.bash",
            "00-atpkg.fish",
            "00-atpkg.ps1",
        ] {
            assert!(d.join(name).is_file(), "{name} written");
        }
        assert!(
            !d.join("00-atpkg.sh").exists(),
            "stray .sh removed (fish-safety)"
        );
        let body = fs::read_to_string(d.join("00-atpkg.zsh")).unwrap();
        assert!(body.contains("/p/bin") && body.contains("/p/agents"));
        // 0600 — Unix-only mode check.
        #[cfg(unix)]
        {
            let mode = fs::metadata(d.join("00-atpkg.zsh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// A hook that is already current is never rewritten (same inode), while one whose bytes
    /// or mode drifted is.
    #[cfg(unix)]
    #[test]
    fn write_hooks_leaves_identical_hooks_untouched() {
        use std::os::unix::fs::MetadataExt as _;
        let d = tmp("identical");
        let (bin, agents, reroute) = (
            Path::new("/p/bin"),
            Path::new("/p/agents"),
            Path::new("/p/reroute"),
        );
        write_hooks(&d, bin, agents, reroute).unwrap();
        let inode = |name: &str| fs::metadata(d.join(name)).unwrap().ino();
        let names = [
            "00-atpkg.zsh",
            "00-atpkg.bash",
            "00-atpkg.fish",
            "00-atpkg.ps1",
        ];
        let before: Vec<u64> = names.iter().map(|n| inode(n)).collect();
        let again = write_hooks(&d, bin, agents, reroute).unwrap();
        assert_eq!(again.len(), 4, "every dialect is still in place: {again:?}");
        let after: Vec<u64> = names.iter().map(|n| inode(n)).collect();
        assert_eq!(before, after, "no hook was rewritten");
        // One drifted in content, one in mode: those two, and only those, are rewritten.
        fs::write(d.join("00-atpkg.zsh"), "# edited by hand\n").unwrap();
        fs::set_permissions(d.join("00-atpkg.fish"), fs::Permissions::from_mode(0o644)).unwrap();
        write_hooks(&d, bin, agents, reroute).unwrap();
        for (name, was) in names.iter().zip(&before) {
            let drifted = *name == "00-atpkg.zsh" || *name == "00-atpkg.fish";
            assert_eq!(inode(name) != *was, drifted, "{name}");
        }
        assert_eq!(
            fs::metadata(d.join("00-atpkg.fish"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// THE MEASURED FAILURE, REPLAYED IN REAL SHELLS (module doc). The inherited PATH is the
    /// m27 login shell's shape: `~/.local/bin` first, `/opt/homebrew/bin` ahead of the agents
    /// dir, the agents dir listed twice, and an EMPTY entry the user owns — and, since
    /// 2026-09-27, the reroute dir demoted behind them too (the 2026-09-24 TTY-session
    /// measurement). Sourcing the generated hook inside aterm must leave the reroute dir
    /// FIRST and the agents dir SECOND, each once, keep every other entry — the empty one
    /// included — in order, append bin/ last, and change nothing on a second source.
    ///
    /// EVERY DIALECT a person's shell sources is executed, each by the shell
    /// `aterm_spec::host::test_shell` finds (`$ATERM_TEST_<SHELL>`, its usual install
    /// path, then `$PATH`: the one resolver aterm-shell-integration's live tests use too):
    /// bash (macOS's 3.2 at /bin/bash) must be found; a missing zsh, fish or pwsh ends the
    /// test COULD NOT RUN once the others ran, never a pass that ran fewer dialects than it
    /// names. Each shell runs with a scratch HOME/XDG root, so a shell that writes
    /// first-run state (pwsh, fish) writes it under this test's temp dir.
    #[cfg(unix)]
    #[test]
    fn every_hook_dialect_orders_path_in_its_real_shell() {
        let root = tmp("realsh");
        let agents = root.join("Application Support/pkg/agents");
        let reroute = root.join("Application Support/pkg/reroute");
        let bin = root.join("Application Support/pkg/bin");
        let files = hook_files(&bin, &agents, &reroute);
        let (a, r, b) = (
            agents.to_str().unwrap(),
            reroute.to_str().unwrap(),
            bin.to_str().unwrap(),
        );
        let inherited =
            format!("/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin:{r}:{a}::/bin:{a}:{r}");
        let want = format!("{r}:{a}:/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin::/bin:{b}");
        let posix_cmd =
            "PATH=\"$ATPKG_TEST_PATH\"; . \"$ATPKG_TEST_HOOK\"; printf 'PATH=%s\\n' \"$PATH\"";
        let fish_cmd = "set -gx PATH (string split -- : \"$ATPKG_TEST_PATH\"); \
                        source \"$ATPKG_TEST_HOOK\"; printf 'PATH=%s\\n' (string join : $PATH)";
        // pwsh pays ~0.15 CPU-s of runtime start per process, so its eight cases share ONE:
        // each runs in a child scope (`& { }`, where the hook's variables die) on the
        // environment the process started with, restored in full first, so no case sees
        // what an earlier sourcing left in the only state the hook reads or writes.
        let pwsh_cmd = "$env0 = [Environment]::GetEnvironmentVariables(); \
                        foreach ($i in 0..([int]$env:ATPKG_TEST_CASES - 1)) { \
                        foreach ($k in @([Environment]::GetEnvironmentVariables().Keys)) { \
                        if (-not $env0.Contains($k)) { [Environment]::SetEnvironmentVariable($k, $null) } }; \
                        foreach ($k in $env0.Keys) { [Environment]::SetEnvironmentVariable($k, $env0[$k]) }; \
                        & { $env:PATH = [Environment]::GetEnvironmentVariable(\"ATPKG_TEST_PATH_$i\"); \
                        . $env:ATPKG_TEST_HOOK; \
                        [Console]::Out.Write('PATH=' + $env:PATH + [char]10) } }";
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let mut absent = Vec::new();
        for (name, args, ext, cmd) in [
            (
                "bash",
                &["--noprofile", "--norc", "-c"][..],
                "bash",
                posix_cmd,
            ),
            ("zsh", &["-f", "-c"][..], "zsh", posix_cmd),
            ("fish", &["--no-config", "-c"][..], "fish", fish_cmd),
            (
                "pwsh",
                &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"][..],
                "ps1",
                pwsh_cmd,
            ),
        ] {
            let hook = root.join(format!("hook.{ext}"));
            let body = &files.iter().find(|(n, _)| n.ends_with(ext)).unwrap().1;
            fs::write(&hook, body).unwrap();
            // Presence is decided ONCE, by the resolver alone: only a shell it finds
            // nowhere is absent. Every run of a found shell must exit 0 and print its
            // PATH line, or the test fails with the shell's own words (review finding
            // 2026-09-10: a fatal zsh error used to read as "zsh not installed"); a
            // found shell that will not start fails too.
            let Some(shell) = real_shell(name, &mut absent) else {
                continue;
            };
            // One process: its exit status and its PATH lines, one per case it was given.
            let spawn = |paths: &[&str]| -> Vec<String> {
                // A scratch HOME and XDG root (pwsh and fish write first-run state), and
                // no inherited $BASH_ENV, which non-interactive bash sources even under
                // --noprofile --norc and which would re-order PATH around the hook.
                let mut command = aterm_spec::host::shell_command(&shell, &home);
                command
                    .args(args)
                    .arg(cmd)
                    .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
                    .env("POWERSHELL_UPDATECHECK", "Off")
                    // This fixture tests an aterm session, not a foreign shell
                    // where the production hook must demote managed agents.
                    .env("ATERM_SESSION_ID", "atpkg-hook-test")
                    // Nor an `aterm --no-reroute` one, whatever session runs the suite.
                    .env_remove(crate::reroute::PASSTHROUGH_ENV)
                    .env("ATPKG_TEST_HOOK", &hook);
                if let [path] = paths {
                    command.env("ATPKG_TEST_PATH", path);
                } else {
                    command.env("ATPKG_TEST_CASES", paths.len().to_string());
                    for (i, path) in paths.iter().enumerate() {
                        command.env(format!("ATPKG_TEST_PATH_{i}"), path);
                    }
                }
                let out = command
                    .output()
                    .unwrap_or_else(|error| panic!("{name} ({shell:?}): spawn: {error}"));
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success(),
                    "{name}: exit {:?}; stdout={stdout:?} stderr={:?}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                );
                let lines: Vec<String> = stdout
                    .lines()
                    .map(|line| {
                        line.strip_prefix("PATH=")
                            .unwrap_or_else(|| panic!("{name}: not a PATH line: {stdout:?}"))
                            .to_owned()
                    })
                    .collect();
                assert_eq!(lines.len(), paths.len(), "{name}: one PATH line per case");
                lines
            };
            // fish spells an empty PATH entry `.` once PATH is set: the same directory, in
            // its place, in fish's spelling.
            let spelled = |path: String| -> String {
                if name != "fish" {
                    return path;
                }
                path.split(':')
                    .map(|entry| if entry.is_empty() { "." } else { entry })
                    .collect::<Vec<_>>()
                    .join(":")
            };
            let want = spelled(want.clone());
            // The inherited PATH, then the hook's own answer sourced again (a nested shell),
            // then the edges: a sole EMPTY entry beside the agents dir survives (review
            // finding 2026-09-10), and a PATH of only the managed dirs stays exactly those.
            let mut cases = vec![
                (
                    inherited.clone(),
                    want.clone(),
                    "agents first and once, the rest in order, bin last",
                ),
                (want.clone(), want.clone(), "idempotent"),
            ];
            for (path, expect) in [
                (format!(":{a}"), format!("{r}:{a}::{b}")),
                (format!("{a}:"), format!("{r}:{a}::{b}")),
                (format!("{a}::{a}"), format!("{r}:{a}::{b}")),
                (format!("{r}::{a}"), format!("{r}:{a}::{b}")),
                (a.to_string(), format!("{r}:{a}:{b}")),
                (r.to_string(), format!("{r}:{a}:{b}")),
            ] {
                cases.push((path, spelled(expect), "an edge"));
            }
            let paths: Vec<&str> = cases.iter().map(|(path, _, _)| path.as_str()).collect();
            let got = if name == "pwsh" {
                spawn(&paths)
            } else {
                paths.iter().flat_map(|path| spawn(&[path])).collect()
            };
            for ((path, expect, what), got) in cases.iter().zip(got) {
                assert_eq!(&got, expect, "{name}: {what}: {path:?}");
            }
        }
        // OUTSIDE ATERM (no agents marker), in bash: the agents and reroute dirs are taken
        // out of an inherited PATH and bin/ is still appended; the seams' reroute handle
        // alone fronts the reroute dir only.
        let hook = root.join("hook.bash");
        let bash = aterm_spec::host::test_shell("bash").expect("bash on a unix host");
        let outside = |handle: Option<&Path>| -> String {
            let mut command = aterm_spec::host::shell_command(&bash, &home);
            command
                .args(["--noprofile", "--norc", "-c", posix_cmd])
                .env_remove(crate::reroute::PASSTHROUGH_ENV)
                .env_remove(crate::reroute::REROUTE_DIR_ENV)
                .env("ATPKG_TEST_HOOK", &hook)
                .env("ATPKG_TEST_PATH", &inherited);
            for marker in AGENTS_MARKERS {
                command.env_remove(marker);
            }
            if let Some(handle) = handle {
                command.env(crate::reroute::REROUTE_DIR_ENV, handle);
            }
            let out = command.output().expect("bash runs");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .trim_end()
                .strip_prefix("PATH=")
                .expect("a PATH line")
                .to_owned()
        };
        let rest = "/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin::/bin";
        assert_eq!(
            outside(None),
            format!("{rest}:{b}"),
            "both demoted outside aterm"
        );
        assert_eq!(
            outside(Some(&reroute)),
            format!("{r}:{rest}:{b}"),
            "the reroute handle alone fronts the reroute dir"
        );
        let _ = fs::remove_dir_all(&root);
        aterm_spec::host::could_not_run_without(&absent);
    }
}
