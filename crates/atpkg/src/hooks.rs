// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell `shell.d` hooks (§16): generate `~/.aterm/shell.d/00-atpkg.{zsh,bash,fish,ps1}` from
//! COMPILED-IN templates only — never artifact-supplied — with fish-safety (never emit a
//! POSIX `.sh`; actively delete a stray one). The `.ps1` is what wires the managed bin/ onto
//! PATH on Windows (and cross-platform pwsh), where no zsh/bash/fish runs the POSIX hooks.
//!
//! This is the ONLY thing that puts the atpkg-managed `bin/` on an interactive shell's PATH,
//! and it **APPENDS** it (append-not-prepend, mirroring [`crate::store::append_bin_to_path`])
//! so a managed tool can never shadow system `sudo`/`ssh`/`git` even on the interactive PATH.
//! The ONE exception (owner decision 2026-09-10): `<prefix>/agents/`
//! ([`crate::store::Layout::agents_dir`]), which holds nothing but the agent programs'
//! shims (`claude`, `codex` — [`crate::stub::AGENT_PROGRAMS`]), is MOVED TO THE FRONT as a
//! separate element (`ATPKG_AGENTS`), so the managed copy of a coding agent is the one that
//! runs ahead of a vendor's native install or a brew cask. `bin/` stays last; the deny-list
//! keeps its meaning, because `agents/` can only ever carry those two names.
//!
//! MOVE-TO-FRONT, NEVER SKIP-IF-PRESENT (2026-09-10). The first cut guarded the prepend with
//! "already on PATH?", and on a macOS login shell the answer was always yes: aterm's spawn had
//! put the dir first, `/etc/zprofile`'s `path_helper` then rebuilt PATH with every
//! `/etc/paths` and `/etc/paths.d` entry (`/opt/homebrew/bin` included) ahead of it, and a
//! `~/.zshrc` line `export PATH="$HOME/.local/bin:$PATH"` went in front of that. Measured on
//! m27: the dir at PATH position 14, `codex` resolving to the brew cask (0.153.4, which ships
//! no `codex-code-mode-host`, so under a code-mode-only model it could run no command at all)
//! and `claude` to `~/.local/bin` (2.1.265), while the managed 0.154.0 and 2.1.267 sat behind
//! them and `aterm pkg status` said SHADOWED. The failure was an ORDER, not a presence — the
//! reroute dir's finding, and its answer: every occurrence removed, then prepended. aterm's
//! own shell integration re-asserts `ATPKG_AGENTS` beside the reroute dir after the user's
//! rc files have run, for a line that prepends AFTER the rc block this module writes.
//! REACH: `~/.aterm/shell.d` is sourced from two places. aterm's own shell integration
//! (`aterm-shell-integration`'s zsh/bash/fish/ps1 scripts) sources it for shells running
//! INSIDE an aterm session; and [`ensure_rc_sources_hooks`] adds a marker-bounded block
//! to the user's `~/.zshrc` / `~/.bashrc` / `~/.bash_profile` / fish config so an
//! ORDINARY interactive shell — Terminal.app, iTerm, VS Code, ssh, an agent's shell —
//! gets the managed tools too. WHICH SHELLS THAT REACHES (measured 2026-09-15, macOS
//! `/bin/bash` 3.2.57, throwaway HOME): zsh reads `~/.zshrc` for every interactive
//! shell, login or not; bash reads `~/.bashrc` ONLY for an interactive NON-login shell,
//! and a LOGIN shell — what Terminal.app, iTerm and an ssh session open, what `bash -l`
//! is — reads `~/.bash_profile` instead (falling through to `~/.bash_login`, then
//! `~/.profile`, when it is absent) and never `~/.bashrc` on its own. Until 2026-09-15
//! the table held `.bashrc` alone, so a user whose login shell is bash got the block
//! written and no toolchain on PATH in any terminal they opened (audit finding);
//! `.bash_profile` is now a second bash target sourcing the same `00-atpkg.bash`.
//! `~/.profile` is deliberately NOT one — this module has never reasoned about it, and
//! it is read by `sh`/dash logins the bash/zsh hook body (`${x//p/r}`) is not written
//! for. Nothing is invented: a home with a `.bashrc` and no `.bash_profile` keeps
//! exactly that, and `aterm pkg doctor` prints the PATH line for the rest.
//! It edits only rc files that already exist and never creates one, and it never OPENS
//! one that resolves under a macOS-protected folder ([`crate::protected`]): the wiring
//! runs unattended (the six-hourly pass, the first-launch seed), and a dotfiles repo
//! under `~/Documents` or iCloud Drive reached through a symlinked `~/.zshrc` — or a
//! symlinked `~/.config` — would otherwise raise the folder consent dialog in aterm's
//! name with nobody at the screen (2026-09-12, TCC audit). Such an rc is left as it is;
//! `aterm pkg doctor`'s PATH line is the way in.
//!
//! That second half is new. Until it landed, a Terminal.app shell reached managed tools
//! only via `aterm <tool>` / `atpkg run` or a PATH line the user pasted by hand, which
//! for a product whose users run coding agents in other terminals threw away most of the
//! value of installing a toolchain
//! (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md, B9).
//! `~/.aterm/shell.d` is OUTSIDE the managed prefix, so [`refresh`] hardens both `~/.aterm`
//! and `~/.aterm/shell.d` with `ensure_private_dir` (symlink-refusing, `0700`, fail-closed)
//! BEFORE writing, and each file is written `0600` via temp + rename. Entirely best-effort:
//! a hook write NEVER fails an install.

use std::fs;
use std::io;
use std::path::Path;

use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// The hook file base name (`00-` sorts first; distinct from any `aterm-pkg`-era name).
pub const HOOK_BASENAME: &str = "00-atpkg";

/// The hook file `shell` — the basename of `$SHELL` — sources: `00-atpkg.zsh`,
/// `00-atpkg.bash`, `00-atpkg.fish`, `00-atpkg.ps1` (`pwsh`/`powershell`); NOT `sh` — the
/// bash hook's body uses `${var//pat/rep}`, which a POSIX `sh` (dash) rejects, so a
/// `$SHELL=/bin/sh` user gets the `PATH` line instead (review finding, 2026-09-16);
/// `None` for a shell atpkg writes no hook for (nushell, xonsh, cmd). One table for
/// [`rc_hook_wired`], [`hook_file`] and the remedy `doctor`/`which` print
/// ([`crate::cli::shell_remedy_command`]) — the same four files [`write_hooks`] lays.
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

/// `<home>/.aterm/shell.d/<hook>` when the hook for `shell` EXISTS there as a regular
/// file (a symlink is not ours — the writer refuses one, [`crate::platform::ensure_private_dir`]),
/// paired with the hook's file name; `None` for a shell without a hook or a hook never
/// written (an install whose hook write failed, a `HOME` atpkg has not seen). The
/// predicate behind the one remedy `doctor`/`which` name: the hook is sourced in place
/// where it stands, and only where it does not stand is a `PATH` line printed instead.
#[must_use]
pub fn hook_file(home: &Path, shell: &str) -> Option<(std::path::PathBuf, &'static str)> {
    let hook = hook_for_shell(shell)?;
    let path = home.join(".aterm").join("shell.d").join(hook);
    fs::symlink_metadata(&path)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|_| (path, hook))
}

/// Whether `shell` — the basename of `$SHELL`: `zsh`, `bash`, `fish` — has an rc under
/// `home` that SOURCES the atpkg hook, paired with that hook's file name; `None` for a
/// shell the table does not know. "Sources" is read off the rc itself: the marker block
/// [`ensure_rc_sources_hooks`] writes, or a hand-written line naming the hook file (a
/// `.zprofile` wired by the owner counts). Read-only, and an rc under a protected root is
/// NOT opened — the consent fence the writer keeps (TCC audit, 2026-09-12) — so it answers
/// "not wired" there, which names the remedy that works either way.
///
/// A FACT, NOT A REMEDY (2026-09-16). The first cut of the agent programs' shadow row
/// keyed its remedy on this: `exec $SHELL` where an rc sources the hook, the source line
/// where none does. Measured the same day on the owner's machine, `exec $SHELL` inside an
/// aterm tab LOSES the tab's shell integration (`aterm-shell-integration`'s zsh wrapper
/// restores `ZDOTDIR` and consumes `ATERM_ORIGINAL_ZDOTDIR` before sourcing, so the
/// re-exec'd zsh never loads it; bash rides `--rcfile`, same loss), so
/// [`crate::cli::shell_remedy_command`] now names the hook source everywhere and keys
/// only on whether the hook FILE exists ([`hook_for_shell`], [`hook_file`]). This stays
/// the reader for "is the rc wired" — `doctor`'s shell-integration facts, a test's
/// witness — and answers no remedy.
#[must_use]
pub fn rc_hook_wired(home: &Path, shell: &str) -> Option<(bool, &'static str)> {
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

/// The `(filename, content)` for each shell dialect, parametrized on `bin_dir` (APPENDED)
/// and `agents_dir` (MOVED TO THE FRONT — the agent programs' shims, module doc).
/// NEVER a `.sh` (fish sources `*.sh`, which a POSIX body would break).
#[must_use]
pub fn hook_files(bin_dir: &Path, agents_dir: &Path) -> Vec<(String, String)> {
    let bin = sh_quote(bin_dir);
    let agents = sh_quote(agents_dir);
    // fish is NOT a POSIX shell and its double quotes are not sh's, so the fish body below
    // is rendered through its OWN quoter ([`fish_quote`]) — never this one.
    let fish_bin = fish_quote(bin_dir);
    let fish_agents = fish_quote(agents_dir);
    // POSIX (zsh + bash, down to macOS's bash 3.2): the agents dir is MOVED TO THE FRONT,
    // the managed bin/ an idempotent APPEND — two elements, two variables, so neither can
    // mask the other; quoted for a space in "Application Support". The move is the
    // integration's reroute idiom: frame PATH as `:$PATH:` so /opt/x never matches /opt/xy,
    // remove every `:dir:` to a fixpoint by POSIX `[ = ]` (never a `[[ == ]]` pattern,
    // which `nocasematch` bends), then strip exactly one framing colon from each end so an
    // EMPTY entry — "here", the user's — survives. Only a framed remainder of exactly `:`
    // means nothing is left; deciding on the STRIPPED string instead dropped a sole empty
    // entry (`:dir`, `dir:`, `dir::dir`) that the old prepend kept (review finding,
    // 2026-09-10).
    let posix = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         __atpkg_agents=\"{agents}\"\n\
         __atpkg_p=\":$PATH:\"\n\
         while :; do __atpkg_q=\"${{__atpkg_p//\":$__atpkg_agents:\"/:}}\"; [ \"$__atpkg_q\" = \"$__atpkg_p\" ] && break; __atpkg_p=\"$__atpkg_q\"; done\n\
         case \"$__atpkg_p\" in :) export PATH=\"$__atpkg_agents\" ;; *) __atpkg_p=\"${{__atpkg_p#:}}\"; __atpkg_p=\"${{__atpkg_p%:}}\"; export PATH=\"$__atpkg_agents:$__atpkg_p\" ;; esac\n\
         export ATPKG_AGENTS=\"$__atpkg_agents\"\n\
         unset __atpkg_agents __atpkg_p __atpkg_q\n\
         __atpkg_bin=\"{bin}\"\n\
         case \":$PATH:\" in *\":$__atpkg_bin:\"*) ;; *) export PATH=\"$PATH:$__atpkg_bin\" ;; esac\n\
         export ATPKG_BIN=\"$__atpkg_bin\"\n\
         unset __atpkg_bin\n"
    );
    // fish: the agents dir moved to the front by an explicit equality loop (`string match`
    // would read the directory as a wildcard pattern; the quoted `"$__atpkg_d"` keeps an
    // empty entry), bin/ appended after $PATH; fish `set` syntax, no POSIX export. The list
    // is grown with `set __atpkg_rest $__atpkg_rest …`, never `set -a`: fish 2.x has no
    // `-a`, the append fails there, the list stays empty, and the final `set -gx PATH` would
    // leave PATH as the agents dir alone. This file is sourced from a plain config.fish too,
    // where an old fish may run (review finding, 2026-09-10).
    let fish = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         set -l __atpkg_agents \"{fish_agents}\"\n\
         set -l __atpkg_rest\n\
         for __atpkg_d in $PATH; if test \"$__atpkg_d\" != \"$__atpkg_agents\"; set __atpkg_rest $__atpkg_rest \"$__atpkg_d\"; end; end\n\
         set -gx PATH \"$__atpkg_agents\" $__atpkg_rest\n\
         set -gx ATPKG_AGENTS $__atpkg_agents\n\
         set -l __atpkg_bin \"{fish_bin}\"\n\
         if not contains $__atpkg_bin $PATH; set -gx PATH $PATH $__atpkg_bin; end\n\
         set -gx ATPKG_BIN $__atpkg_bin\n"
    );
    // PowerShell (Windows-native, and cross-platform pwsh): the aterm PowerShell integration
    // dot-sources `~/.aterm/shell.d/*.ps1`, so this is the ONLY thing that puts the managed
    // dirs on an interactive PowerShell's PATH — on Windows there is no zsh/bash/fish to run
    // the POSIX hooks above. Same shape as the POSIX policy (agents moved to the front, bin/
    // appended, each idempotent) using `[System.IO.Path]::PathSeparator` (';' on Windows, ':'
    // on Unix), so it is correct wherever pwsh runs.
    let ps_bin = ps_quote(bin_dir);
    let ps_agents = ps_quote(agents_dir);
    let powershell = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         $__atpkg_sep = [System.IO.Path]::PathSeparator\n\
         $__atpkg_agents = '{ps_agents}'\n\
         $__atpkg_rest = @(($env:PATH -split [regex]::Escape($__atpkg_sep)) | Where-Object {{ $_ -ne $__atpkg_agents }})\n\
         $env:PATH = (@($__atpkg_agents) + $__atpkg_rest) -join $__atpkg_sep\n\
         $env:ATPKG_AGENTS = $__atpkg_agents\n\
         $__atpkg_bin = '{ps_bin}'\n\
         if (($env:PATH -split [regex]::Escape($__atpkg_sep)) -notcontains $__atpkg_bin) {{ $env:PATH = \"$env:PATH$__atpkg_sep$__atpkg_bin\" }}\n\
         $env:ATPKG_BIN = $__atpkg_bin\n\
         Remove-Variable __atpkg_agents, __atpkg_bin, __atpkg_sep, __atpkg_rest\n"
    );
    vec![
        (format!("{HOOK_BASENAME}.zsh"), posix.clone()),
        (format!("{HOOK_BASENAME}.bash"), posix),
        (format!("{HOOK_BASENAME}.fish"), fish),
        (format!("{HOOK_BASENAME}.ps1"), powershell),
    ]
}

/// What stood at a hook's destination before this pass replaced it, so the publish phase
/// can be undone as a whole.
enum Prev {
    /// Nothing did: an unwind takes the file this pass laid back out.
    Absent,
    /// The previous pass's hook — a regular file — moved aside to this path.
    Saved(std::path::PathBuf),
    /// Something that is NOT a regular file (a directory, a link, a device). It is left
    /// exactly where it is: the rename that follows then fails on a directory just as it
    /// always has, and this pass's bookkeeping displaces nothing of the user's. An unwind
    /// leaves whatever stands there alone, having nothing it could put back.
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

/// Write the three dialect hooks into `shell_d` (each `0600` via temp + rename) and delete a
/// stray POSIX `00-atpkg.sh` (fish-safety). Returns the written file names.
pub fn write_hooks(shell_d: &Path, bin_dir: &Path, agents_dir: &Path) -> io::Result<Vec<String>> {
    // EVERY TEMP FIRST, THEN EVERY RENAME (audit 2026-09-17). Written one whole file at
    // a time, a failure on the THIRD dialect left the first two carrying the new PATH
    // policy and the last two the old one — four files that are only ever correct as a
    // set, since one shell reads each and the aterm integration sources whichever it
    // finds. Staging all four before any of them is published keeps the visible set in
    // step: the ordinary failure (a full disk, a name that cannot be created) now
    // happens while nothing has been replaced, and the hooks on disk stay exactly as
    // they were for the next pass to retry.
    //
    // AND THE RENAME PHASE MOVES AS A SET TOO (review, 2026-09-19). Staging every temp
    // first made only the TEMP phase all-or-nothing: a rename that failed partway still
    // left the earlier dialects publishing the new PATH policy and the rest the old one,
    // and reported failure — on which `refresh_at`'s gate then suppressed the rc wiring
    // for ALL FOUR shells, including the ones whose own hook was on disk and correct. A
    // failure of that shape is persistent (a directory standing at a hook name), so every
    // later pass failed the same way and the rc block was never laid at all. Each
    // destination's previous file is now moved aside before it is replaced and put back
    // if a later rename fails, so a failed publish leaves `shell.d` exactly as it found
    // it and the next pass retries the hooks and the rc wiring together.
    let files = hook_files(bin_dir, agents_dir);
    let mut staged = Vec::with_capacity(files.len());
    for (name, content) in &files {
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
    let mut written = Vec::with_capacity(staged.len());
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
            // Carry nothing over: the temps that have not been renamed are this pass's
            // litter, and `shell.d` is swept by nobody.
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
    // fish-safety: a POSIX `.sh` here is fatal (fish sources *.sh too). Never emitted above;
    // actively remove a hand-dropped one.
    let stray = shell_d.join(format!("{HOOK_BASENAME}.sh"));
    if stray.exists() {
        let _ = fs::remove_file(&stray);
    }
    Ok(written)
}

/// What one hooks pass did, so a caller that PRINTS its result states what happened
/// rather than what it attempted. No variant is an error to a caller — a hooks pass never
/// fails an install — but "rewritten" is claimed only where it is true: `atpkg repair`
/// printed "shell integration rewritten (~/.aterm/shell.d + rc wiring)" unconditionally,
/// including on the paths where nothing had been touched at all.
///
/// Only [`HookPass::Rewritten`] carries rc outcomes, because the rc step runs only once
/// the hooks are on disk (THE BLOCK NEVER LEADS THE HOOKS, [`refresh_at`]) — and the list
/// is empty off unix, where no rc is wired, and on a home where none of the `RC_FILES`
/// roster exists.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HookPass {
    /// `~/.aterm` and `~/.aterm/shell.d` were hardened and the hook files were written.
    /// One [`RcOutcome`] per rc file that EXISTS, in `RC_FILES` order.
    Rewritten(Vec<(&'static str, RcOutcome)>),
    /// The directories were hardened, a hook file could not be written, and the rc step
    /// was therefore SKIPPED — a block that names a hook which is not there sources
    /// nothing, and being marker-bounded no later pass would rewrite it.
    HooksNotWritten,
    /// `$HOME` is unknown, or `~/.aterm` / `~/.aterm/shell.d` could not be made the
    /// user's own `0700` directory ([`ensure_private_dir`] refuses a symlink, a foreign
    /// owner or a group/other-writable mode): nothing was touched, no rc was read.
    NotHardened,
}

impl HookPass {
    /// What the rc step did to each rc file that exists — empty unless the hooks were
    /// written, since the rc step does not run otherwise.
    #[must_use]
    pub fn rc(&self) -> &[(&'static str, RcOutcome)] {
        match self {
            Self::Rewritten(rc) => rc,
            Self::HooksNotWritten | Self::NotHardened => &[],
        }
    }
}

/// What one pass did to one rc file that EXISTS. An absent rc has no outcome: atpkg never
/// creates one. Carried in [`HookPass`] so `atpkg repair` prints one line per rc — what it
/// wrote, what it left alone, and what it skipped and why — instead of one sentence that
/// claimed "rc wiring" whether it had just re-laid a block the user deleted, failed to
/// write the rc (a `~/.zshrc` linked into a read-only store), skipped it at the consent
/// fence, or found no rc at all. Users AND agents are sent to `repair` for other reasons —
/// a stale rustup link, a missing reroute stub (`doctor`, the manual, aterm-primer) — so
/// re-laying an opt-out must be announced, never implied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RcOutcome {
    /// The rc already carried atpkg's block; nothing was written to it.
    AlreadyWired,
    /// atpkg's block was appended to an rc that had never carried it.
    Appended,
    /// atpkg's block was laid again in an rc the user had deleted it from (recorded in
    /// `RC_LEDGER`, no marker present). Only [`refresh_rewiring_rc`] does this.
    RelaidOverOptOut,
    /// The user had deleted atpkg's block and this pass left the rc exactly as it was.
    OptOutKept,
    /// The rc resolves under a folder macOS guards with a consent dialog, so it was not
    /// even OPENED (the consent fence in `ensure_rc_sources_hooks`).
    ConsentFenced,
    /// The rc exists but is not something atpkg edits: a dangling link, not a regular
    /// file, or not UTF-8. Left untouched.
    Unreadable,
    /// The rewrite was attempted and failed; the rc is unchanged. Either the rc or its
    /// directory refused the write (a dotfile linked into a read-only store), or the
    /// temp could not be created exclusively beside it.
    WriteFailed,
}

/// Best-effort choke point called after activation ([`crate::flow`]). Hardens `~/.aterm` +
/// `~/.aterm/shell.d` (0700, symlink-refusing) then writes the hooks. NEVER propagates an
/// error — hooks are ergonomics, not trust; a failed write must not fail an install.
///
/// This is the UNATTENDED pass (install, seed, the six-hourly `update`), so its rc wiring
/// HONOURS THE OPT-OUT the block itself documents: an rc that was wired once and no longer
/// carries the block is left exactly as the user left it ([`RC_LEDGER`]).
/// [`refresh_rewiring_rc`] is the deliberate pass that writes it back.
///
/// The returned [`HookPass`] says what the pass DID. Every unattended caller discards it,
/// as it discarded the unit before; `atpkg repair` is the one caller that reports it.
pub fn refresh(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::HonorOptOut)
}

/// [`refresh`], but the rc block is written back even where the user removed it — the
/// ASKED-FOR pass, and only that: `atpkg repair`, whose own output promises it rewrote the
/// shell integration. It is the way back in after an opt-out, so opting out costs nobody a
/// hand-edited dotfile to undo.
///
/// The returned [`HookPass`] names each rc it re-laid ([`RcOutcome::RelaidOverOptOut`]),
/// so `repair` can say so with the way back out instead of re-laying it silently.
pub fn refresh_rewiring_rc(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::Rewire)
}

/// What a pass does with an rc whose block the user DELETED (the opt-out the block's own
/// text documents): an unattended pass leaves it deleted; `repair` writes it back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RcWiring {
    /// An rc recorded in [`RC_LEDGER`] that no longer carries the block is skipped.
    HonorOptOut,
    /// Every eligible rc is wired, ledger or not.
    Rewire,
}

fn refresh_with(layout: &Layout, wiring: RcWiring) -> HookPass {
    let Some(home) = aterm_types::dirs::home_dir() else {
        return HookPass::NotHardened;
    };
    pass_at(layout, &home, wiring)
}

/// One whole pass against a known `home`, as [`refresh_with`] runs it — so a test in
/// another module (`cli`'s repair report, `doctor`'s rc lines) can drive the real pass on
/// a synthetic home instead of the account running the tests.
pub(crate) fn pass_at(layout: &Layout, home: &Path, wiring: RcWiring) -> HookPass {
    refresh_at(home, &layout.bin_dir(), &layout.agents_dir(), wiring)
}

/// [`refresh_with`] over an explicit `home` and the two managed directories, so a test
/// can drive a whole pass under a throwaway home.
///
/// THE BLOCK NEVER LEADS THE HOOKS (audit 2026-09-17). The rc block is the only thing
/// that makes a hook reachable from an ORDINARY shell, and it is written into a file
/// atpkg does not own — so it may only go in once the hook it names is actually on
/// disk. [`write_hooks`]'s result was discarded here, so a pass whose hook write failed
/// (a full disk, a `shell.d` entry that could not be replaced) still appended a block
/// naming a file that was never written. The block is marker-bounded, so no later pass
/// rewrites it: it would have sourced nothing for as long as it stood, and — before the
/// status-neutral line below — left `$?` = 1 in every shell that read it. The wiring
/// now runs only when the hooks are there, and the next pass retries the two together.
fn refresh_at(home: &Path, bin_dir: &Path, agents_dir: &Path, wiring: RcWiring) -> HookPass {
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    if ensure_private_dir(&aterm).is_err() || ensure_private_dir(&shell_d).is_err() {
        return HookPass::NotHardened;
    }
    let hooks_written = write_hooks(&shell_d, bin_dir, agents_dir).is_ok();
    #[cfg(unix)]
    let rc = if hooks_written {
        ensure_rc_sources_hooks(home, wiring)
    } else {
        Vec::new()
    };
    #[cfg(not(unix))]
    let rc = {
        let _ = wiring;
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
pub(crate) const RC_BEGIN: &str = "# >>> atpkg shell integration >>>";
pub(crate) const RC_END: &str = "# <<< atpkg shell integration <<<";

/// The rc files atpkg wires: each row is a path relative to `$HOME` and the `shell.d` hook
/// it sources — the ONE roster, so the pass that writes the block and the report that names
/// its state ([`rc_wiring`]) can never disagree about which files are atpkg's. Each is the
/// startup file of a shell whose hook dialect exists in [`hook_files`], and atpkg edits one
/// only when it ALREADY EXISTS.
///
/// A shell that reads none of these four is not wired by atpkg: fish with
/// `$XDG_CONFIG_HOME` set reads a different `config.fish`, and a login bash on a home with
/// no `~/.bash_profile` falls through to `~/.bash_login` and then `~/.profile`, neither of
/// which is a row (`ensure_rc_sources_hooks`'s doc says why). `aterm pkg doctor`'s PATH
/// line is the way in for those.
#[cfg(unix)]
pub(crate) const RC_FILES: [(&str, &str); 4] = [
    (".zshrc", "00-atpkg.zsh"),
    (".bashrc", "00-atpkg.bash"),
    // The LOGIN bash rc ([`ensure_rc_sources_hooks`]): the same hook, reached by the
    // shell Terminal.app actually opens.
    (".bash_profile", "00-atpkg.bash"),
    (".config/fish/config.fish", "00-atpkg.fish"),
];

/// `~/.aterm/rc-wired` — the rc files this machine has wired, one canonical path per
/// line. It is what makes the block's OWN SENTENCE true.
///
/// The block says "delete this block to opt out", and until 2026-09-16 the only
/// idempotency check was the marker itself: a pass that read an rc with no [`RC_BEGIN`]
/// in it appended the block again. [`refresh`] runs unconditionally from install, seed,
/// `repair` and the six-hourly background `update`, so the documented opt-out lasted only
/// until the next pass — hours — and a dotfiles repo showed the same diff coming back
/// forever, the user's own edit to their own file silently reverted (audit finding,
/// 2026-09-16). Nothing else recorded the opt-out: no config key, no tombstone.
///
/// So a wired rc is RECORDED here, and a recorded rc that no longer carries the block is
/// read as the user's opt-out and skipped. A pass also records an rc it finds ALREADY
/// wired, so a machine wired by an older build gets its entry on the first pass after the
/// upgrade and the FIRST deletion there is honoured too.
///
/// State, not trust, and cheap to lose: a wiped `~/.aterm` costs one re-add, and two
/// passes racing cost at most one lost entry (each writes the union of what it read and
/// what it wired). [`refresh_rewiring_rc`] ignores it on purpose — that is the way back.
#[cfg(unix)]
const RC_LEDGER: &str = "rc-wired";

#[cfg(unix)]
fn rc_ledger_path(home: &Path) -> std::path::PathBuf {
    home.join(".aterm").join(RC_LEDGER)
}

/// The recorded rc paths, or an empty set when the ledger is absent or unreadable — a
/// missing ledger means "nothing has been wired yet", which is what a fresh home is.
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

/// Record `real` as wired, best-effort (0600, temp + rename, the discipline every other
/// file here is written with). Already recorded ⇒ no write at all, so the ordinary pass —
/// every rc already in the ledger — touches nothing.
///
/// A path containing a NEWLINE cannot be one line of a line-per-path file, so it is left
/// out rather than written as two entries that would each mean something else: such an rc
/// keeps the old behaviour (wired again by the next pass) and the rest of the ledger stays
/// exactly what it says it is.
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

/// Source the `shell.d` hooks from the user's own rc, so the managed toolchain is on
/// PATH in EVERY shell — Terminal.app, iTerm, VS Code, ssh, an agent's shell — not only
/// inside an aterm session.
///
/// Until this existed, `shell.d` was written on every install and sourced only by
/// aterm's own integration, so `trustc` was not found anywhere else and `doctor` could
/// only print a PATH line for the user to paste by hand. For a product whose users run
/// coding agents in other terminals that is most of the value of installing a toolchain
/// (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md, B9).
///
/// Deliberately conservative about touching a file it does not own:
/// * only rc files that ALREADY EXIST are edited — atpkg never creates a shell's rc;
/// * the block is bounded by [`RC_BEGIN`]/[`RC_END`] so it is idempotent (present ⇒
///   nothing happens) and a user can delete it in one motion — and a deletion STAYS
///   deleted, because a wired rc is recorded in [`RC_LEDGER`] and a recorded rc that no
///   longer carries the block is this pass's evidence that the user opted out;
/// * it SOURCES `shell.d` rather than inlining a PATH, so the path stays correct after
///   the prefix moves and there is exactly one generator;
/// * the hook path is written DOUBLE-QUOTED, so a home with a space (`/Users//Jane Doe`)
///   sources the hook instead of erroring at every shell start;
/// * the sourced hook appends the managed `bin/`, never prepends it, so a managed tool
///   cannot shadow system `sudo`/`ssh`/`git` (the agents dir it prepends can only ever
///   carry `claude`/`codex` — module doc);
/// * every step is best-effort — wiring PATH must never fail an install.
///
/// bash gets TWO targets, `~/.bashrc` and `~/.bash_profile`, because bash reads them for
/// DIFFERENT shells (measured 2026-09-15, macOS `/bin/bash` 3.2.57, a throwaway HOME
/// holding all three of `.bash_profile`/`.bashrc`/`.profile`, each echoing its name):
/// `bash -l -i` — a Terminal.app or iTerm tab, an ssh login, `bash -l` — printed
/// `.bash_profile` alone; `bash -i` printed `.bashrc` alone; with `.bash_profile`
/// removed the login shell fell through to `.profile`; with only `.bashrc` left it
/// printed nothing. Until then the table held `.bashrc` alone, so a bash-login user got
/// the block AND no toolchain on PATH in any terminal they opened (audit finding,
/// 2026-09-15). Both rows source the same `00-atpkg.bash`; a `.bash_profile` that itself
/// sources `.bashrc` runs it twice, which the hook is built for (bin/ presence-guarded,
/// agents/ move-to-front — the real-shell test pins a second source as a no-op). The
/// never-create rule stands: a home with a `.bashrc` and no `.bash_profile` gets nothing
/// invented (many users source `.bashrc` from a `.bash_profile` of their own, and a
/// file we invent is ours forever). `~/.bash_login` and `~/.profile` are NOT rows — this
/// module has never reasoned about `.profile`, and it is read by `sh`/dash logins the
/// bash/zsh hook body (`${x//p/r}`) is not written for; `aterm pkg doctor`'s PATH line
/// is the way in for those.
///
/// Returns one [`RcOutcome`] per row that EXISTS, in [`RC_FILES`] order, so `atpkg repair`
/// can print what happened to each rc instead of one sentence about all of them. An absent
/// row is not in the list at all: atpkg never creates an rc, so there is nothing to report
/// but "not there", which the caller says once by naming the roster.
#[cfg(unix)]
fn ensure_rc_sources_hooks(home: &Path, wiring: RcWiring) -> Vec<(&'static str, RcOutcome)> {
    // Both sides of the protected-root comparison canonical: `$TMPDIR` and a home
    // reached through a link both spell `/private/…` once resolved.
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    // Read once for the whole pass: the four rows share it, and each write folds its own
    // entry back in ([`record_rc_wired`]).
    let mut ledger = read_rc_ledger(home);
    let mut outcomes: Vec<(&'static str, RcOutcome)> = Vec::new();
    // Each row is independent — an absent one is skipped below, never created.
    for (rc, hook) in RC_FILES {
        let rc_path = home.join(rc);
        // Never CREATE an rc: a shell the user does not use should not gain one, and a
        // file we invent is a file we own forever. `lstat` and `realpath` are
        // metadata-only (design §1.5) and gated nowhere, so finding out where the rc
        // really lives costs no consent.
        if fs::symlink_metadata(&rc_path).is_err() {
            continue;
        }
        // Write to the FILE the rc names, never over a link: a symlinked rc (stow,
        // yadm/chezmoi, home-manager, a dotfiles repo) renamed over became a detached
        // 0644 copy the dotfile never saw (2026-09-12, audit K12). Resolved for a
        // REGULAR rc too: `~/.config` is itself a link in the same setups, and the file
        // it leads to is the one every call below would open. A dangling link, or
        // anything that is not a regular file, is left alone.
        let Ok(real) = fs::canonicalize(&rc_path) else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        // THE CONSENT FENCE (2026-09-12, TCC audit). This runs on the six-hourly pass
        // and at the first-launch seed, with nobody at the screen. A dotfile that
        // resolves under `~/Documents`, iCloud Drive or a mounted volume is opened by
        // the read below, and that open is exactly what raises the macOS folder dialog
        // — in aterm's name, at a moment nobody chose, parking this pass on a modal
        // that has no timeout. The rc stays unwired; the owner can wire it by hand from
        // a shell (a deliberate touch, which may prompt), and `aterm pkg doctor` prints
        // the PATH line either way.
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
            // Already wired; the hook body itself is what gets refreshed. Record it —
            // including an rc wired by a build that predates the ledger — so that the
            // NEXT deletion is read as the opt-out the block promises.
            record_rc_wired(home, &real, &mut ledger);
            outcomes.push((rc, RcOutcome::AlreadyWired));
            continue;
        }
        // THE OPT-OUT, HONOURED. This rc carried the block once and does not now, so the
        // user deleted it exactly as the block told them to. An unattended pass — an
        // install, a seed, the six-hourly `update` — must not undo a user's edit to their
        // own dotfile; `atpkg repair` ([`refresh_rewiring_rc`]) is the pass that asks for
        // it back — and it SAYS which rc it re-laid, so the deletion is never undone
        // silently ([`RcOutcome::RelaidOverOptOut`]).
        let opted_out = ledger.contains(real.to_string_lossy().as_ref());
        if wiring == RcWiring::HonorOptOut && opted_out {
            outcomes.push((rc, RcOutcome::OptOutKept));
            continue;
        }
        let hook_path = home.join(".aterm").join("shell.d").join(hook);
        // DOUBLE-QUOTED, both interpolations. `sh_quote`'s contract is a double-quoted
        // context (its doc), and this line was the one place that interpolated it bare:
        // a HOME with a space — `/Users//Jane Doe`, ordinary on Linux and in custom
        // setups — word-split the path, so `[` got too many arguments, the hook was
        // NEVER sourced (the entire purpose of the block), and zsh/bash printed that
        // error at EVERY interactive shell start. The block is marker-bounded, so a bad
        // line is never rewritten by a later pass — it has to be born right (2026-09-16
        // audit; the real-shell test below replays it in bash and zsh).
        //
        // STATUS-NEUTRAL, both dialects (audit 2026-09-17). This line is the LAST thing
        // many rc files run, so the status it leaves is the `$?` the user's first prompt
        // reads. `[ -f "<hook>" ] && . "<hook>"` is an `&&` list: with the hook ABSENT —
        // an install whose hook write failed, a cleared `~/.aterm`, a home restored
        // without it — the test fails, nothing is sourced, and the list exits 1.
        // Measured in both shells on 2026-09-17: the `&&` line over a missing hook left
        // `rc=1` in bash and zsh, the `if` form `rc=0`. Every prompt that renders the
        // last status (a `git`-aware theme, a `PS1` carrying `$?`, starship) then opened
        // showing a failure with nothing to blame. `if …; then …; fi` reports the
        // SOURCED hook's own status and 0 when there is nothing to source, which is the
        // truth in both cases.
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
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&format!(
            "\n{RC_BEGIN}\n\
             # Puts the ALab toolchain on PATH. Managed by atpkg; delete this block to opt\n\
             # out -- it stays deleted; `aterm pkg repair` puts it back.\n\
             {line}\n\
             {RC_END}\n"
        ));
        // Same temp+rename discipline the hook files use: a reader never sees a
        // half-written rc, and a failure leaves the original untouched. The temp sits
        // beside the REAL file (same filesystem, so the rename replaces it and not the
        // link) and carries its mode, so a 0600 rc stays 0600. A directory we cannot
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
        let written = create_rc_temp(&tmp)
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

/// Create the rc rewrite's temp file: exclusively, and born `0600`. It is filled with the
/// WHOLE rc before the rc's own mode is applied, so a umask-default (usually `0644`)
/// temp left a `0600` rc's contents readable by other users until that chmod
/// (2026-09-12, review of audit K12). The real mode is set just before the rename.
#[cfg(unix)]
fn create_rc_temp(tmp: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

/// The login profiles `tools/install.sh`'s `path_block_rc_target` can elect for bash on
/// macOS that atpkg has NO row for, relative to `$HOME`. Login bash reads the first of
/// `~/.bash_profile`, `~/.bash_login`, `~/.profile` that exists; the first IS one of
/// [`RC_FILES`], and these two are the fall-through atpkg never wires (the never-create
/// rule, and `.profile` is read by `sh`/dash logins the bash hook body is not written for).
///
/// atpkg never EDITS them. [`rc_wiring`] READS them, so `doctor` can see install.sh's own
/// `# >>> aterm ALab toolset` block there: a Mac whose bash was wired by install.sh into
/// `~/.bash_login` would otherwise read as "nothing sources shell.d".
#[cfg(unix)]
pub(crate) const INSTALL_SH_PROFILES: [&str; 2] = [".bash_login", ".profile"];

/// The rc-wiring state of one rc file that exists, for `doctor`.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RcState {
    /// The rc carries atpkg's marker block ([`RC_BEGIN`]).
    Wired,
    /// The rc sources `~/.aterm/shell.d` through a line that is NOT atpkg's block:
    /// `tools/install.sh`'s own marker pair (`# >>> aterm ALab toolset (managed by
    /// install.sh) >>>`, its `wire_shell_path`), or a line the user wrote. The toolchain
    /// reaches that shell; atpkg simply does not own the line. Measured by CONTENT
    /// ([`sources_shell_d`]), so doctor never reports "nothing sources shell.d" from the
    /// mere absence of atpkg's marker — a fault it would not have measured.
    SourcedElsewhere,
    /// atpkg wired this rc once ([`RC_LEDGER`]) and the block is gone: the user opted out.
    /// Left alone by every pass except `atpkg repair`.
    OptedOut,
    /// The rc exists, no [`RC_LEDGER`] entry records atpkg wiring it, and nothing else
    /// sources `shell.d` from it either — so no pass has run since it appeared.
    Unwired,
    /// The rc resolves under a folder macOS guards with a consent dialog, so no pass will
    /// open it ([`ensure_rc_sources_hooks`]'s consent fence) — and neither does this
    /// report, which is why the state is about the path, not the contents. THE ONE STATE
    /// THE USER CANNOT DIAGNOSE THEMSELVES: the rc looks ordinary, every pass leaves it
    /// untouched, and nothing said why.
    ConsentFenced,
}

/// How [`rc_wiring`] resolves one rc: exactly the steps [`ensure_rc_sources_hooks`] takes
/// before it would write, so the report can never describe a file the pass would treat
/// differently. `None` means there is nothing to report — the rc is absent, or it is not a
/// regular UTF-8 file, which is one no pass will ever edit.
#[cfg(unix)]
enum RcRead {
    /// The resolved target and its contents.
    Body(std::path::PathBuf, String),
    /// The rc resolves under a macOS-guarded root and was NOT opened.
    Fenced,
}

#[cfg(unix)]
fn read_rc_for_report(canonical_home: &Path, rc_path: &Path) -> Option<RcRead> {
    // Never create, and never invent a row for a file that is not there.
    fs::symlink_metadata(rc_path).ok()?;
    let real = fs::canonicalize(rc_path).ok()?;
    // The consent fence, before the open — doctor must not raise the dialog the
    // unattended pass refuses to raise.
    if crate::protected::under_protected_root(canonical_home, &real) {
        return Some(RcRead::Fenced);
    }
    if !fs::metadata(&real).ok()?.is_file() {
        return None;
    }
    let body = fs::read_to_string(&real).ok()?;
    Some(RcRead::Body(real, body))
}

/// The rc-wiring state of each [`RC_FILES`] row that EXISTS under `home`, in roster order,
/// then each [`INSTALL_SH_PROFILES`] entry that sources `shell.d`.
///
/// An absent rc is not listed: atpkg never creates one, so there is nothing to say about it
/// beyond "not there" — and an rc no pass would edit (not a regular file, not UTF-8) is not
/// listed either. Content is read BEFORE the ledger: an rc that sources `shell.d` through a
/// foreign line ([`RcState::SourcedElsewhere`]) reaches the toolchain whether or not
/// atpkg's own block was deleted from it. The install.sh profiles are read-only probes —
/// atpkg never wires them, so the only state they can report is `SourcedElsewhere`, and one
/// that does not source `shell.d` is not listed at all.
///
/// These six names ([`rc_files_read`]) are every file the report is measured on, which is
/// why doctor names them instead of making a claim about the machine: `$ZDOTDIR`, `/etc`
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

/// Every file [`rc_wiring`] reads, relative to `$HOME`, in the order it reports them: the
/// four atpkg wires, then install.sh's two bash login fall-throughs. doctor lists exactly
/// these when none of them sources `shell.d`, so its "none of …" line is a measurement of
/// named files rather than a verdict on every shell the machine can start.
#[cfg(unix)]
pub(crate) fn rc_files_read() -> impl Iterator<Item = &'static str> {
    RC_FILES
        .iter()
        .map(|(rc, _)| *rc)
        .chain(INSTALL_SH_PROFILES)
}

/// Whether `rc`'s contents source `~/.aterm/shell.d` through some line other than atpkg's
/// own block: any non-comment line naming a path under `/.aterm/shell.d/` (`~/`, `$HOME/`,
/// or spelled out). `tools/install.sh`'s `wire_shell_path` writes exactly such a line under
/// its own marker pair, and a user may write one by hand. A commented-out line does not
/// count: it sources nothing.
#[cfg(unix)]
fn sources_shell_d(rc: &str) -> bool {
    rc.lines()
        .map(str::trim_start)
        .any(|l| !l.starts_with('#') && l.contains("/.aterm/shell.d/"))
}

/// Put `aterm` and `atpkg` in `~/.local/bin` when we are running from an app bundle.
///
/// The rc wiring is [`ensure_rc_sources_hooks`]'s (the marker-bounded block in an
/// EXISTING `~/.zshrc` / `~/.bashrc` / `~/.bash_profile` / fish config that sources
/// `shell.d`); this is the OTHER half of reaching the toolchain from outside aterm: the
/// `aterm <tool>` / `atpkg run` route. On the DMG install route those two commands did
/// not exist on any PATH — the binaries live only inside
/// `/Applications/aterm.app/Contents/MacOS`, and only `tools/install.sh` ever linked
/// them out. So the fallback the docs named was itself unreachable for anyone who
/// dragged the app to Applications, which README offers as a first-class option: the
/// ten programs installed, and nothing could reach them from outside aterm (2026-08-20
/// round-8 audit). (Until 2026-09-10 this comment claimed the module "deliberately does
/// NOT write into `~/.zshrc`" — untrue since the rc wiring landed; `aterm help pkg`
/// said the same and was corrected with it.)
///
/// Deliberately narrow: symlinks only, into the same `~/.local/bin` that
/// `tools/install.sh` uses, and NEVER over anything that is not already ours — a
/// real file there is someone's own build. No dotfile is touched HERE; `atpkg doctor`
/// still prints the rc line for putting the managed `bin/` itself on PATH.
///
/// Unix-only: the whole body is bundle-shaped (`Contents/MacOS`, `~/.local/bin`,
/// POSIX symlinks), so on Windows it could only ever return early — and
/// `std::os::unix::fs::symlink` does not exist there to compile against.
#[cfg(unix)]
fn ensure_command_links(home: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Ok(exe) = exe.canonicalize() else {
        return;
    };
    // Only from inside a bundle: a source build's target/release is the developer's
    // own tree, and linking out of it would outlive the checkout.
    if !exe.parent().is_some_and(|d| d.ends_with("Contents/MacOS")) {
        return;
    }
    // AND NOT FROM A TRANSLOCATED ONE. Gatekeeper runs a quarantined download from a
    // read-only, randomly-named mount that disappears when the app quits, so a link
    // into it is dangling by the next login — pointing at a path that will never
    // exist again, on a machine whose real install is elsewhere
    // (2026-08-20 round-9 audit).
    if exe.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .starts_with("AppTranslocation")
    }) {
        return;
    }
    let Some(macos) = exe.parent() else {
        return;
    };
    let Some(bin) = command_links_dir(home) else {
        return;
    };
    if fs::create_dir_all(&bin).is_err() {
        return;
    }
    for name in ["aterm", "atpkg"] {
        let target = macos.join(name);
        if !target.exists() {
            continue;
        }
        let link = bin.join(name);
        match fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                // Ours to repoint only if it already points into an app bundle;
                // anything else belongs to whoever put it there.
                let ours = fs::read_link(&link).ok().is_some_and(|old| {
                    old.to_string_lossy().contains("/aterm.app/Contents/MacOS/")
                });
                if !ours || fs::read_link(&link).is_ok_and(|old| old == target) {
                    continue;
                }
                let _ = fs::remove_file(&link);
            }
            // A real file is a hand-built binary; leave it.
            Ok(_) => continue,
            Err(_) => {}
        }
        let _ = std::os::unix::fs::symlink(&target, &link);
    }
}

/// Where the `aterm`/`atpkg` command links go — `~/.local/bin` — or `None` when that
/// directory RESOLVES under a root macOS guards with a consent dialog (a `~/.local` that
/// is itself a link into a dotfiles repo under `~/Documents`): creating the directory,
/// or a link inside it, is a gated write, and [`ensure_command_links`] runs unattended
/// (the same fence as [`ensure_rc_sources_hooks`]; `crate::protected`).
///
/// The nearest EXISTING ancestor decides where a create would land, and `canonicalize`
/// is metadata-only (design §1.5), so this touches nothing consent-gated itself. A home
/// that cannot be resolved at all yields the plain path: there is nothing to refuse
/// against, and `create_dir_all` answers for itself.
#[cfg(unix)]
fn command_links_dir(home: &Path) -> Option<std::path::PathBuf> {
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
/// POSIX ONLY — zsh, bash, and the `if [ -f "…" ]` rc line. fish has [`fish_quote`]; this
/// quoter's backtick is WRONG there.
fn sh_quote(bin_dir: &Path) -> String {
    let mut out = String::new();
    for ch in bin_dir.to_string_lossy().chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Render `p` for a double-quoted **fish** string — the fish hook body and the fish rc line.
///
/// NOT [`sh_quote`] (review, 2026-09-19). fish is not a POSIX shell and its double quotes
/// are not sh's: between them fish recognises only `\\`, `\"` and `\$` (and a
/// backslash-newline continuation), and a backslash before ANY other character is kept
/// LITERALLY. fish also has no backtick command substitution at all — `(cmd)` is its
/// spelling, and inside double quotes only `$(cmd)`, which escaping `$` already covers. So
/// sh's quoter, which also escapes a backtick, turned a prefix or a `$HOME` holding one into
/// a path of a DIFFERENT name: `/opt/a`b` was emitted as `/opt/a\`b`, which fish reads with
/// the backslash in it. The hook then put a directory that does not exist on PATH and
/// `ATPKG_BIN`/`ATPKG_AGENTS` named it, and the rc line's `test -f` never found the hook, so
/// the block sourced nothing — and, being marker-bounded, no later pass would rewrite it.
fn fish_quote(p: &Path) -> String {
    let mut out = String::new();
    for ch in p.to_string_lossy().chars() {
        if matches!(ch, '\\' | '"' | '$') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Render `bin_dir` for a SINGLE-quoted PowerShell string literal: only the single quote is
/// special there (doubled to escape), so a Windows path's backslashes pass through literally
/// (no `C:\Users` → `C:<tab>sers` surprise) — the reason the `.ps1` uses `'...'`, not `"..."`.
fn ps_quote(bin_dir: &Path) -> String {
    bin_dir.to_string_lossy().replace('\'', "''")
}

/// Write `content` to `dest` atomically (temp `0600` + rename), so a reader never sees a
/// half-written hook.
fn atomic_write(dest: &Path, content: &str) -> io::Result<()> {
    let tmp = stage_hook(dest, content)?;
    fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Write `content` to `dest`'s temp sibling, born `0600` and hardened, and hand back the
/// temp path for the caller to rename into place. The temp is removed on EVERY error arm.
///
/// TWO FIXES OVER THE OLD `fs::write` (audit 2026-09-17). It left the temp behind
/// whenever `harden_file` or the rename failed — `~/.aterm/shell.d` is swept by nobody,
/// so each failed pass added one more `.00-atpkg.zsh.tmp-<pid>` that the fish and zsh
/// glob loops in aterm's own integration then had to skip forever. And `fs::write`
/// creates at the umask default, usually `0644`, so the hook was world-readable between
/// the write and the chmod; `create_new` + `mode(0o600)` gives it the mode at birth,
/// which is what [`create_rc_temp`] already does for the rc rewrite, and refuses to
/// write through a name something else already holds.
fn stage_hook(dest: &Path, content: &str) -> io::Result<std::path::PathBuf> {
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path has no file name"))?;
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path has no parent"))?;
    let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    // A temp of OUR name left by a dead process with this pid is ours to clear; anything
    // that is not a plain file we can remove stays, and `create_new` then refuses.
    let _ = fs::remove_file(&tmp);
    let staged = create_hook_temp(&tmp)
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

/// Create a hook temp EXCLUSIVELY and, on Unix, born `0600` — [`create_rc_temp`]'s twin
/// for the files under `shell.d`. Windows has no mode to ask for at creation;
/// [`crate::platform::harden_file`] is the no-op there and the directory is already
/// private ([`ensure_private_dir`]).
#[cfg(unix)]
fn create_hook_temp(tmp: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
}

#[cfg(not(unix))]
fn create_hook_temp(tmp: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// THE CHECKED REMEDY (review, 2026-09-16): `rc_hook_wired` reads the rc itself —
    /// the marker block, or a hand-written line naming the hook — never a ledger, and
    /// knows the three shell families and nothing else.
    #[test]
    fn rc_hook_wired_reads_the_rc_for_the_shell_family() {
        let home = std::env::temp_dir().join(format!("atpkg-rcwired-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".config/fish")).unwrap();
        // Nothing wired anywhere: every known shell says so, an unknown one says nothing.
        assert_eq!(rc_hook_wired(&home, "zsh"), Some((false, "00-atpkg.zsh")));
        assert_eq!(rc_hook_wired(&home, "bash"), Some((false, "00-atpkg.bash")));
        assert_eq!(rc_hook_wired(&home, "fish"), Some((false, "00-atpkg.fish")));
        assert_eq!(rc_hook_wired(&home, "nu"), None);
        assert_eq!(rc_hook_wired(&home, ""), None);
        // The marker block in .zshrc: zsh wired, bash still not.
        fs::write(
            home.join(".zshrc"),
            format!("x=1\n{RC_BEGIN}\n. hook\n{RC_END}\n"),
        )
        .unwrap();
        assert_eq!(rc_hook_wired(&home, "zsh"), Some((true, "00-atpkg.zsh")));
        assert_eq!(rc_hook_wired(&home, "bash"), Some((false, "00-atpkg.bash")));
        // A hand-written source line (no markers) in the LOGIN rc counts for bash…
        fs::write(
            home.join(".bash_profile"),
            "[ -f ~/.aterm/shell.d/00-atpkg.bash ] && . ~/.aterm/shell.d/00-atpkg.bash\n",
        )
        .unwrap();
        assert_eq!(rc_hook_wired(&home, "bash"), Some((true, "00-atpkg.bash")));
        // …and an rc that exists but names no hook does not.
        fs::write(home.join(".config/fish/config.fish"), "set -x FOO 1\n").unwrap();
        assert_eq!(rc_hook_wired(&home, "fish"), Some((false, "00-atpkg.fish")));
        let _ = fs::remove_dir_all(&home);
    }

    /// ONE TABLE FOR THE HOOK NAME (2026-09-16): the four files `write_hooks` lays are
    /// exactly what `hook_for_shell` names per shell family, and `hook_file` answers only
    /// for a hook that EXISTS as a regular file — a symlink under `shell.d` is not ours.
    #[cfg(unix)]
    #[test]
    fn hook_for_shell_and_hook_file_read_the_files_write_hooks_lays() {
        let home = tmp("hookfile");
        assert_eq!(hook_for_shell("zsh"), Some("00-atpkg.zsh"));
        assert_eq!(hook_for_shell("bash"), Some("00-atpkg.bash"));
        // `sh` gets no hook: the bash hook is not POSIX sh (`${var//pat/rep}`), so the
        // remedy for a `/bin/sh` user is the `PATH` line, never a file sh cannot source.
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
        let written = write_hooks(&shell_d, Path::new("/p/bin"), Path::new("/p/agents")).unwrap();
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

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-hooks-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The rc block goes in exactly once, sources the hook, and is removable.
    ///
    /// Before this, `shell.d` was written on every install and sourced only by aterm's
    /// own integration, so the managed toolchain was invisible in Terminal.app, iTerm,
    /// VS Code and any agent shell — most of the value of installing a toolchain
    /// (docs/AUDIT-nux-first-open-toolchain-2026-08-31.md, B9).
    #[cfg(unix)]
    #[test]
    fn rc_wiring_is_idempotent_and_only_touches_existing_rc_files() {
        let home = tmp("rcwire");
        let zshrc = home.join(".zshrc");
        std::fs::write(&zshrc, "# pre-existing user content\nexport FOO=1\n").unwrap();
        // .bashrc deliberately absent: atpkg must not CREATE an rc for a shell the
        // user does not use — a file we invent is a file we own forever.

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        let once = std::fs::read_to_string(&zshrc).unwrap();
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

        // Idempotent: a second refresh must not stack a second block.
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        let twice = std::fs::read_to_string(&zshrc).unwrap();
        assert_eq!(twice, once, "a second pass must be a no-op");
        assert_eq!(twice.matches(RC_BEGIN).count(), 1, "exactly one block");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// THE OPT-OUT THE BLOCK DOCUMENTS ACTUALLY LASTS — and `repair` is the way back.
    ///
    /// The block carries the sentence "delete this block to opt out", and until
    /// 2026-09-16 the only idempotency check was [`RC_BEGIN`] itself: a user who
    /// deleted the block as instructed got it back on the next pass — an install, a
    /// seed, `repair`, or the six-hourly background `update`, so within hours — with
    /// their own edit to their own dotfile silently reverted and a dotfiles repo
    /// showing the same diff forever (audit finding). [`RC_LEDGER`] records the wiring,
    /// so a recorded rc that no longer carries the block is read as the opt-out it is.
    /// The other half is that opting out must not be a trap: the deliberate pass
    /// ([`refresh_rewiring_rc`], which `atpkg repair` runs) writes it back, and after
    /// that the block is deletable-for-good again.
    #[cfg(unix)]
    #[test]
    fn a_deleted_rc_block_stays_deleted_and_repair_is_the_way_back() {
        let home = tmp("rcoptout");
        let zshrc = home.join(".zshrc");
        let user = "# pre-existing user content\nexport FOO=1\n";
        fs::write(&zshrc, user).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
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
            ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
            assert_eq!(
                fs::read_to_string(&zshrc).unwrap(),
                user,
                "pass {pass}: an unattended pass must not undo the user's own edit"
            );
        }

        // `atpkg repair` is the pass the user asks for, so it wires the rc again.
        ensure_rc_sources_hooks(&home, RcWiring::Rewire);
        assert!(
            fs::read_to_string(&zshrc).unwrap().contains(RC_BEGIN),
            "repair puts the block back"
        );

        // …and the opt-out is still available afterwards.
        fs::write(&zshrc, user).unwrap();
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        assert_eq!(
            fs::read_to_string(&zshrc).unwrap(),
            user,
            "a second opt-out is honoured just like the first"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// A machine wired by a build from BEFORE the ledger opts out on its first deletion.
    ///
    /// Recording only at the moment of writing would have made every existing install
    /// spend its first deletion re-learning what it already knew: the block would come
    /// back once, and only the second deletion would stick. So a pass records an rc it
    /// finds already wired — it rewrites nothing, it only remembers.
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

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        assert_eq!(
            fs::read_to_string(&bashrc).unwrap(),
            pre,
            "an already-wired rc is still never rewritten"
        );
        assert!(
            rc_ledger_path(&home).is_file(),
            "the pass records what it found wired"
        );

        // The FIRST deletion on such a machine is honoured.
        fs::write(&bashrc, user).unwrap();
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        assert_eq!(
            fs::read_to_string(&bashrc).unwrap(),
            user,
            "the first deletion sticks on a machine wired by an older build"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// A HOME with a SPACE still gets the toolchain, and no shell prints an error.
    ///
    /// The rc line interpolates the hook path through [`sh_quote`], whose contract is a
    /// DOUBLE-QUOTED context — so the interpolation has to carry the quotes, and until
    /// 2026-09-16 it did not. With `HOME=/Users//Jane Doe` the line word-split:
    /// `zsh:[:1: too many arguments`, bash's `[: binary operator expected`, at EVERY
    /// interactive shell start, and the hook never sourced — which is the whole point of
    /// the block. Marker-bounded, so no later pass would have repaired it. Replayed in
    /// the real shells, running the generated line exactly as the user's rc runs it.
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
            fs::write(shell_d.join(hook), "ATPKG_RC_TEST=sourced\n").unwrap();
        }

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);

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

        // Presence is decided by the spawn alone (the idiom of the other real-shell
        // test): only a shell that cannot be started is skipped.
        for (shell, args, rc) in [
            ("bash", &["--noprofile", "--norc", "-c"][..], ".bashrc"),
            ("zsh", &["-f", "-c"][..], ".zshrc"),
        ] {
            match std::process::Command::new(shell)
                .args(args)
                .arg("true")
                .output()
            {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    assert_ne!(shell, "bash", "bash must be runnable on a unix host");
                    continue; // zsh not installed here
                }
                Err(error) => panic!("{shell}: cannot start: {error}"),
                Ok(_) => {}
            }
            let line = line_of(rc);
            let out = std::process::Command::new(shell)
                .args(args)
                .arg(format!("{line}\nprintf 'X=%s\\n' \"$ATPKG_RC_TEST\""))
                // Non-interactive bash sources $BASH_ENV even under --noprofile --norc.
                .env_remove("BASH_ENV")
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

        // fish is not installed on every host, so its line is checked as text: BOTH
        // interpolations quoted (`test -f "..."; and source "..."`).
        let fish = line_of(".config/fish/config.fish");
        let fish_quoted = format!("\"{}\"", shell_d.join("00-atpkg.fish").display());
        assert_eq!(
            fish.matches(fish_quoted.as_str()).count(),
            2,
            "fish: both interpolations must be double-quoted, got {fish:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// THE BLOCK MUST NOT LEAVE `$?` = 1 IN EVERY NEW SHELL (audit 2026-09-17).
    ///
    /// The block is the last thing many rc files run, so the status its line leaves is
    /// the `$?` the user's first prompt reads. `[ -f "<hook>" ] && . "<hook>"` is an
    /// `&&` list: with the hook ABSENT — an install whose hook write failed, a cleared
    /// `~/.aterm`, a home restored without it — the test fails, nothing is sourced, and
    /// the list exits 1. Every prompt that renders the last status (a `git`-aware
    /// theme, a `PS1` carrying `$?`, starship) then opened showing a failure with
    /// nothing to blame. Replayed in the real shells, running the generated line
    /// exactly as the user's rc runs it, over a `shell.d` that holds no hook at all.
    #[cfg(unix)]
    #[test]
    fn the_rc_line_is_status_neutral_when_the_hook_is_missing() {
        let home = tmp("rcstatus");
        // The rc files exist; the hooks deliberately DO NOT.
        for rc in [".zshrc", ".bashrc", ".config/fish/config.fish"] {
            let rc_path = home.join(rc);
            fs::create_dir_all(rc_path.parent().unwrap()).unwrap();
            fs::write(&rc_path, "# pre-existing user content\n").unwrap();
        }
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);

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

        for (shell, args, rc) in [
            ("bash", &["--noprofile", "--norc", "-c"][..], ".bashrc"),
            ("zsh", &["-f", "-c"][..], ".zshrc"),
        ] {
            match std::process::Command::new(shell)
                .args(args)
                .arg("true")
                .output()
            {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    assert_ne!(shell, "bash", "bash must be runnable on a unix host");
                    continue; // zsh not installed here
                }
                Err(error) => panic!("{shell}: cannot start: {error}"),
                Ok(_) => {}
            }
            let line = line_of(rc);
            let out = std::process::Command::new(shell)
                .args(args)
                .arg(&line)
                // Non-interactive bash sources $BASH_ENV even under --noprofile --norc.
                .env_remove("BASH_ENV")
                .output()
                .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
            assert_eq!(
                out.status.code(),
                Some(0),
                "{shell}: an ABSENT hook must still leave $? = 0 — this line is the last \
                 thing the rc runs; line={line:?}"
            );
        }

        // fish is not installed on every host, so its line is checked as text.
        let fish = line_of(".config/fish/config.fish");
        assert!(
            fish.starts_with("if test -f ") && fish.ends_with("; end"),
            "fish: the line must be the status-neutral `if …; end` form, got {fish:?}"
        );

        let _ = fs::remove_dir_all(&home);
    }

    /// A PASS WHOSE HOOK WRITE FAILED WIRES NO RC (audit 2026-09-17).
    ///
    /// The block names ONE file. Writing it when that file is not there leaves an rc
    /// sourcing nothing — and, because the block is marker-bounded, no later pass ever
    /// rewrites the line, so it stays wrong for as long as it stands. The hook write is
    /// made to fail the way a real one does (the temp path it must create is occupied),
    /// and the rc must come out exactly as the user left it.
    #[cfg(unix)]
    #[test]
    fn a_failed_hook_write_wires_no_rc_block() {
        let home = tmp("rcnohook");
        let zshrc = home.join(".zshrc");
        let user = "# pre-existing user content\nexport FOO=1\n";
        fs::write(&zshrc, user).unwrap();
        let shell_d = home.join(".aterm").join("shell.d");
        fs::create_dir_all(&shell_d).unwrap();
        // A DIRECTORY stands where the hook writer must create its temp file, so the
        // very first dialect's write fails and `write_hooks` returns Err.
        let blocked = shell_d.join(format!(".{HOOK_BASENAME}.zsh.tmp-{}", std::process::id()));
        fs::create_dir_all(&blocked).unwrap();

        refresh_at(
            &home,
            Path::new("/opt/atpkg-fixture/bin"),
            Path::new("/opt/atpkg-fixture/agents"),
            RcWiring::HonorOptOut,
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

    /// A symlinked rc (GNU stow, yadm/chezmoi symlink mode, home-manager, any dotfiles
    /// repo) gets the block written THROUGH the link, and stays a link. Until 2026-09-12
    /// the temp file was renamed over the rc path, which replaces the LINK: `~/.zshrc`
    /// became a detached regular 0644 copy, the dotfile never got the block, and later
    /// edits to the dotfiles repo silently stopped reaching the shell (audit K12).
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

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
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

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
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

    /// THE BASH LOGIN SHELL. Measured 2026-09-15 (macOS `/bin/bash` 3.2.57): a login
    /// shell — what Terminal.app, iTerm and ssh open — reads `~/.bash_profile` and NOT
    /// `~/.bashrc`, which only an interactive non-login shell reads. Until this row
    /// existed a bash-login user had the block in `.bashrc` and no toolchain on PATH in
    /// any terminal they opened (audit finding). A home with `.bash_profile` gets the
    /// block, sourcing the SAME `00-atpkg.bash` as `.bashrc`; a home with both gets it
    /// in both; and `.bashrc` is never invented beside a lone `.bash_profile`.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_reaches_a_bash_login_shell_through_an_existing_bash_profile() {
        let home = tmp("bashprofile");
        let profile = home.join(".bash_profile");
        fs::write(&profile, "# login-only content\nexport FOO=1").unwrap(); // no final newline

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
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
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
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

    /// The never-create rule, for the login rc in particular. A home with only a
    /// `.bashrc` is the common Linux shape and a legitimate macOS one (a hand-written
    /// `.bash_profile` that sources `.bashrc` is the usual fix, but its absence is the
    /// user's to fill): `.bashrc` is wired as before and NO `.bash_profile` appears. A
    /// home with neither gains no file at all — not an rc, not a temp.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_never_invents_a_bash_profile_or_a_bashrc() {
        let home = tmp("noinvent");
        // Neither: the pass must leave an empty home empty.
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        let after: Vec<_> = fs::read_dir(&home)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(after.is_empty(), "invented in an empty home: {after:?}");

        // Only `.bashrc`: wired exactly as before this row existed, nothing beside it.
        let bashrc = home.join(".bashrc");
        fs::write(&bashrc, "export FOO=1\n").unwrap();
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        assert!(fs::read_to_string(&bashrc).unwrap().contains(RC_BEGIN));
        assert!(
            fs::symlink_metadata(home.join(".bash_profile")).is_err(),
            "a .bash_profile must never be invented beside a lone .bashrc"
        );
        assert!(
            fs::symlink_metadata(home.join(".profile")).is_err()
                && fs::symlink_metadata(home.join(".bash_login")).is_err(),
            "nor any other login rc"
        );
        let _ = fs::remove_dir_all(&home);
    }

    /// A second pass over a wired `.bash_profile` REWRITES NOTHING — not "writes the same
    /// bytes again": the file keeps its inode (a temp+rename would give it a new one), its
    /// content and its mode, and leaves no temp behind. The block is found by its
    /// markers, never by content, so this holds after the hook line itself changes.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_of_bash_profile_is_idempotent_and_a_second_pass_rewrites_nothing() {
        use std::os::unix::fs::MetadataExt as _;
        let home = tmp("bashprofile-idem");
        let profile = home.join(".bash_profile");
        fs::write(&profile, "export FOO=1\n").unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o600)).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        let once = fs::read_to_string(&profile).unwrap();
        let meta_once = fs::metadata(&profile).unwrap();
        assert_eq!(once.matches(RC_BEGIN).count(), 1);
        assert_eq!(meta_once.permissions().mode() & 0o777, 0o600, "mode kept");

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        let twice = fs::read_to_string(&profile).unwrap();
        let meta_twice = fs::metadata(&profile).unwrap();
        assert_eq!(twice, once, "a second pass changes no byte");
        assert_eq!(twice.matches(RC_BEGIN).count(), 1, "exactly one block");
        assert_eq!(
            meta_twice.ino(),
            meta_once.ino(),
            "the file was not rewritten: same inode, no temp+rename"
        );
        assert_eq!(meta_twice.permissions().mode() & 0o777, 0o600);
        let strays: Vec<_> = fs::read_dir(&home)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("atpkg-") && n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "temp files left: {strays:?}");
        let _ = fs::remove_dir_all(&home);
    }

    /// A symlinked `.bash_profile` (stow, yadm/chezmoi, home-manager) is edited IN PLACE:
    /// the block lands in the dotfile the link names, the link stays a link, and the
    /// dotfile keeps its mode — the same guarantee audit K12 (2026-09-12) won for
    /// `.zshrc`, pinned here for the row added 2026-09-15 so it cannot regress alone.
    #[cfg(unix)]
    #[test]
    fn rc_wiring_writes_through_a_symlinked_bash_profile_and_keeps_it_a_link() {
        let home = tmp("bashprofile-symlink");
        let dot = home.join("dotfiles");
        fs::create_dir_all(&dot).unwrap();
        let target = dot.join("bash_profile");
        fs::write(&target, "export FOO=1\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = home.join(".bash_profile");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            ".bash_profile must stay a symlink"
        );
        assert_eq!(
            fs::read_link(&link).unwrap(),
            target,
            "and point where it did"
        );
        let through = fs::read_to_string(&target).unwrap();
        assert!(
            through.contains(RC_BEGIN) && through.contains("00-atpkg.bash"),
            "the block lands in the dotfile: {through}"
        );
        assert!(through.starts_with("export FOO=1\n"), "{through}");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600,
            "a 0600 dotfile must not widen"
        );
        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);
        assert_eq!(
            fs::read_to_string(&target)
                .unwrap()
                .matches(RC_BEGIN)
                .count(),
            1,
            "exactly one block through the link"
        );
        for dir in [&home, &dot] {
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

    /// THE CONSENT FENCE. The wiring runs on the six-hourly pass and the first-launch
    /// seed, and a dotfiles repo commonly lives under `~/Documents` (or iCloud Drive)
    /// with `~/.zshrc` — or the whole of `~/.config` — symlinked into it. Opening that
    /// file from an unattended pass raises the macOS folder dialog in aterm's name with
    /// nobody at the screen (2026-09-12, TCC audit). So an rc that RESOLVES under a
    /// protected root is left exactly as it was — through a symlinked rc, and through a
    /// symlinked parent of a regular rc — while an rc beside it that resolves somewhere
    /// ordinary is still wired.
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
        // ~/.config -> ~/Documents/dotfiles/config, with a REGULAR config.fish inside
        // it: the rc path itself is not a link, its parent is.
        let fish = docs.join("config").join("fish").join("config.fish");
        fs::write(&fish, "set -x FOO 1\n").unwrap();
        std::os::unix::fs::symlink(docs.join("config"), home.join(".config")).unwrap();
        // ~/.bashrc -> ~/dotfiles/bashrc: a link into an ORDINARY directory, the control.
        let plain = home.join("dotfiles");
        fs::create_dir_all(&plain).unwrap();
        let bashrc = plain.join("bashrc");
        fs::write(&bashrc, "export BAR=1\n").unwrap();
        std::os::unix::fs::symlink(&bashrc, home.join(".bashrc")).unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);

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

    /// The same fence for the command links: `~/.local/bin` is created and written on
    /// every pass, and a `~/.local` that is itself a link into a dotfiles repo under a
    /// protected root would make that create the gated write.
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

    /// The rc rewrite's temp holds the WHOLE rc before its real mode is applied, so it is
    /// born 0600: created with the umask default (0644 under the usual 022) it left a
    /// 0600 rc's contents readable by other users until the chmod that followed
    /// (2026-09-12, review of audit K12). Observed at creation, before any write or chmod.
    #[cfg(unix)]
    #[test]
    fn rc_temp_is_never_created_wider_than_0600() {
        let dir = tmp("rctemp");
        let path = dir.join(".zshrc.atpkg-1.tmp");
        let f = create_rc_temp(&path).unwrap();
        assert_eq!(
            f.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "the temp must be private from the instant it exists"
        );
        drop(f);
        assert!(
            create_rc_temp(&path).is_err(),
            "exclusive: an existing temp is never opened and truncated"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hook_files_cover_exactly_zsh_bash_fish_ps1_and_never_sh() {
        let files = hook_files(Path::new("/p/bin"), Path::new("/p/agents"));
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "00-atpkg.zsh",
                "00-atpkg.bash",
                "00-atpkg.fish",
                "00-atpkg.ps1"
            ]
        );
        assert!(
            !names.iter().any(|n| n.ends_with(".sh")),
            "never a POSIX .sh (fish-safety)"
        );
    }

    #[test]
    fn powershell_dialect_appends_bin_and_moves_agents_to_the_front_via_platform_separator() {
        let files = hook_files(Path::new("/p/bin"), Path::new("/p/agents"));
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
            ps.contains("Where-Object { $_ -ne $__atpkg_agents }")
                && ps.contains(
                    "$env:PATH = (@($__atpkg_agents) + $__atpkg_rest) -join $__atpkg_sep"
                ),
            "agents/ is MOVED TO THE FRONT (the one exception): removed, then prepended"
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
        );
        let (_, ps) = files.iter().find(|(n, _)| n.ends_with(".ps1")).unwrap();
        assert!(
            ps.contains(r"$__atpkg_bin = 'C:\Users\x\AppData\Local\aterm\pkg\bin'"),
            "a single-quoted PS literal keeps backslashes verbatim"
        );
        assert!(ps.contains(r"$__atpkg_agents = 'C:\Users\x\AppData\Local\aterm\pkg\agents'"));
    }

    /// The PATH policy in one test: the managed `bin/` is APPENDED (never ahead of a
    /// system `sudo`/`ssh`/`git`), and the agents dir — the one exception, owner
    /// decision 2026-09-10 — is MOVED TO THE FRONT as its own element, with no presence
    /// guard (module doc: an order failure), exported as `ATPKG_AGENTS` beside `ATPKG_BIN`.
    #[test]
    fn posix_dialects_append_bin_and_move_only_the_agents_dir_to_the_front() {
        let files = hook_files(Path::new("/p/bin"), Path::new("/p/agents"));
        for (name, body) in &files {
            if name.ends_with(".zsh") || name.ends_with(".bash") {
                assert!(
                    body.contains("export PATH=\"$PATH:$__atpkg_bin\""),
                    "bin/ appends"
                );
                assert!(
                    !body.contains("$__atpkg_bin:$PATH"),
                    "bin/ is never prepended"
                );
                assert!(
                    body.contains("export PATH=\"$__atpkg_agents:$__atpkg_p\"")
                        && body.contains("in :) export PATH=\"$__atpkg_agents\" ;;"),
                    "agents/ goes to the front"
                );
                assert!(
                    !body.contains("*\":$__atpkg_agents:\"*) ;;"),
                    "agents/ has no skip-if-present guard"
                );
                assert!(
                    body.contains("export ATPKG_AGENTS=\"$__atpkg_agents\"")
                        && body.contains("export ATPKG_BIN=\"$__atpkg_bin\""),
                    "both exported"
                );
                assert_eq!(
                    body.matches("case \":$PATH:\"").count(),
                    1,
                    "one presence guard, bin/'s"
                );
                assert!(
                    body.find("__atpkg_agents").unwrap() < body.find("__atpkg_bin").unwrap(),
                    "the agents element is set up first"
                );
                assert!(body.contains("\"/p/agents\"") && body.contains("\"/p/bin\""));
            }
        }
    }

    #[test]
    fn fish_dialect_appends_bin_moves_agents_to_the_front_and_avoids_posix_export() {
        let files = hook_files(Path::new("/p/bin"), Path::new("/p/agents"));
        let (_, fish) = files.iter().find(|(n, _)| n.ends_with(".fish")).unwrap();
        assert!(
            fish.contains("set -gx PATH $PATH $__atpkg_bin"),
            "fish append"
        );
        assert!(
            fish.contains("set -gx PATH \"$__atpkg_agents\" $__atpkg_rest")
                && fish.contains("if test \"$__atpkg_d\" != \"$__atpkg_agents\""),
            "fish moves the agents dir to the front by equality"
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

    /// THE FOUR HOOKS MOVE AS A SET, OR NOT AT ALL (audit 2026-09-17).
    ///
    /// One shell reads each dialect and aterm's own integration sources whichever it
    /// finds, so the four files are only ever correct together. Written one whole file
    /// at a time, a failure on the third left `00-atpkg.zsh` and `00-atpkg.bash`
    /// carrying the new PATH policy and `00-atpkg.fish`/`.ps1` the old one, with nothing
    /// to put them back. Staging every temp before publishing any of them means the
    /// ordinary failure happens while nothing has been replaced.
    #[cfg(unix)]
    #[test]
    fn a_failed_hook_write_replaces_none_of_the_dialects() {
        let d = tmp("writeset");
        // A DIRECTORY stands where the THIRD dialect's temp must be created, so its
        // staging fails after the first two have been staged.
        let blocked = d.join(format!(".{HOOK_BASENAME}.fish.tmp-{}", std::process::id()));
        fs::create_dir_all(&blocked).unwrap();

        let out = write_hooks(&d, Path::new("/p/bin"), Path::new("/p/agents"));
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

    /// NO TEMP IS LEFT BEHIND, ON ANY ERROR ARM (audit 2026-09-17).
    ///
    /// `~/.aterm/shell.d` is swept by nobody, and the fish and zsh integration scripts
    /// glob the directory — so every failed pass used to add one more
    /// `.00-atpkg.<ext>.tmp-<pid>` for them to skip, forever. Here the LAST dialect's
    /// destination is a directory, so its rename is what fails, with its temp already
    /// written and hardened: exactly the arm that leaked.
    #[cfg(unix)]
    #[test]
    fn a_hook_write_that_fails_leaves_no_temp_behind() {
        let d = tmp("writetmp");
        // The rename target is a non-empty directory, so `rename` cannot replace it.
        let dest = d.join("00-atpkg.ps1");
        fs::create_dir_all(dest.join("occupied")).unwrap();

        let out = write_hooks(&d, Path::new("/p/bin"), Path::new("/p/agents"));
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

    /// AND THE PUBLISH PHASE MOVES AS A SET TOO (review, 2026-09-19).
    ///
    /// Staging every temp first made only the TEMP phase all-or-nothing. A rename that
    /// failed partway still left the earlier dialects carrying the NEW policy and the rest
    /// the old one — and reported failure, on which `refresh_at`'s gate suppressed the rc
    /// wiring for all four shells, including the ones whose own hook was on disk and
    /// correct. That failure is persistent (a directory standing at a hook name), so every
    /// later pass failed the same way and the rc block was never laid at all. Here the LAST
    /// dialect's rename is the one that fails, with three already published over a previous
    /// pass's hooks: every one of them must come back.
    #[cfg(unix)]
    #[test]
    fn a_failed_publish_puts_back_every_dialect_it_had_already_replaced() {
        let d = tmp("publishset");
        // A PREVIOUS PASS'S HOOKS stand for the three dialects that publish first.
        let old = "# the previous pass's hook\n";
        for name in ["00-atpkg.zsh", "00-atpkg.bash", "00-atpkg.fish"] {
            fs::write(d.join(name), old).unwrap();
        }
        // The LAST dialect's destination is a NON-EMPTY DIRECTORY, so its rename fails with
        // the first three already renamed into place.
        fs::create_dir_all(d.join("00-atpkg.ps1").join("occupied")).unwrap();

        let out = write_hooks(&d, Path::new("/p/bin"), Path::new("/p/agents"));
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

    /// FISH IS NOT A POSIX SHELL, AND ITS DOUBLE QUOTES ARE NOT SH'S (review, 2026-09-19).
    ///
    /// The fish hook body and the fish rc line were both rendered through `sh_quote`, which
    /// escapes a BACKTICK. Between double quotes fish recognises only `\\`, `\"` and `\$`
    /// (and a backslash-newline); a backslash before anything else is kept LITERALLY, and
    /// fish has no backtick command substitution at all. So a prefix or a `$HOME` holding a
    /// backtick was emitted with a backslash in front of it, which fish reads as a directory
    /// of a DIFFERENT name: the hook put a path that does not exist on PATH.
    ///
    /// fish is not installed on this host, so the quoter is measured directly against those
    /// documented rules rather than by running it.
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

        let files = hook_files(&hostile, &hostile);
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
        // The POSIX body is unchanged: its quoter is still right for zsh and bash.
        assert!(
            body(".zsh").contains("a\\`b"),
            "the POSIX body must still escape the backtick"
        );
    }

    /// AND THE FISH RC LINE TOO: a `$HOME` with a backtick still names the hook verbatim.
    #[cfg(unix)]
    #[test]
    fn the_fish_rc_line_carries_a_backtick_home_unescaped() {
        let home =
            std::env::temp_dir().join(format!("atpkg-hooks-fish`tick-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let rc = home.join(".config/fish/config.fish");
        fs::create_dir_all(rc.parent().unwrap()).unwrap();
        fs::write(&rc, "# pre-existing user content\n").unwrap();

        ensure_rc_sources_hooks(&home, RcWiring::HonorOptOut);

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
        let written = write_hooks(&d, Path::new("/p/bin"), Path::new("/p/agents")).unwrap();
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

    /// THE MEASURED FAILURE, REPLAYED IN REAL SHELLS (module doc). The inherited PATH is the
    /// m27 login shell's shape: `~/.local/bin` first, `/opt/homebrew/bin` ahead of the agents
    /// dir, the agents dir listed twice, and an EMPTY entry the user owns. Sourcing the
    /// generated hook must leave the agents dir FIRST and once, keep every other entry — the
    /// empty one included — in order, append bin/ last, and change nothing on a second source.
    /// bash is macOS's 3.2 when that is what `bash` is; zsh is skipped where it is absent.
    #[cfg(unix)]
    #[test]
    fn posix_hook_moves_the_agents_dir_to_the_front_in_real_shells() {
        let root = tmp("realsh");
        let agents = root.join("Application Support/pkg/agents");
        let bin = root.join("Application Support/pkg/bin");
        let files = hook_files(&bin, &agents);
        let (a, b) = (agents.to_str().unwrap(), bin.to_str().unwrap());
        let inherited = format!("/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin:{a}::/bin:{a}");
        let want = format!("{a}:/Users//u/.local/bin:/usr/bin:/opt/homebrew/bin::/bin:{b}");
        for (shell, args, ext) in [
            ("bash", &["--noprofile", "--norc", "-c"][..], "bash"),
            ("zsh", &["-f", "-c"][..], "zsh"),
        ] {
            let hook = root.join(format!("hook.{ext}"));
            let body = &files.iter().find(|(n, _)| n.ends_with(ext)).unwrap().1;
            fs::write(&hook, body).unwrap();
            // Presence is decided ONCE, by the spawn alone: only a shell that cannot
            // be started is skipped. Every run after that must exit 0 and print its
            // PATH line, or the test fails with the shell's own words (review finding
            // 2026-09-10: a fatal zsh error used to read as "zsh not installed").
            match std::process::Command::new(shell)
                .args(args)
                .arg("true")
                .output()
            {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    assert_ne!(shell, "bash", "bash must be runnable on a unix host");
                    continue; // zsh not installed here
                }
                Err(error) => panic!("{shell}: cannot start: {error}"),
                Ok(_) => {}
            }
            let run = |path: &str| -> String {
                let out = std::process::Command::new(shell)
                    .args(args)
                    .arg("PATH=\"$ATPKG_TEST_PATH\"; . \"$ATPKG_TEST_HOOK\"; printf 'PATH=%s\\n' \"$PATH\"")
                    // Non-interactive bash sources $BASH_ENV even under --noprofile
                    // --norc; an inherited one would re-order PATH around the hook.
                    .env_remove("BASH_ENV")
                    .env("ATPKG_TEST_HOOK", &hook)
                    .env("ATPKG_TEST_PATH", path)
                    .output()
                    .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success(),
                    "{shell}: exit {:?}; stdout={stdout:?} stderr={:?}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                );
                stdout
                    .trim_end()
                    .strip_prefix("PATH=")
                    .unwrap_or_else(|| panic!("{shell}: no PATH line: {stdout:?}"))
                    .to_owned()
            };
            let got = run(&inherited);
            assert_eq!(
                got, want,
                "{shell}: agents first and once, the rest in order, bin last"
            );
            assert_eq!(run(&got), want, "{shell}: idempotent");
            // A sole EMPTY entry beside the agents dir survives (review finding
            // 2026-09-10), and a PATH of only the agents dir stays exactly that.
            for (path, expect) in [
                (format!(":{a}"), format!("{a}::{b}")),
                (format!("{a}:"), format!("{a}::{b}")),
                (format!("{a}::{a}"), format!("{a}::{b}")),
                (a.to_string(), format!("{a}:{b}")),
            ] {
                assert_eq!(run(&path), expect, "{shell}: {path:?}");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bin_dir_with_spaces_is_quoted() {
        let files = hook_files(
            Path::new("/Users//x/Library/Application Support/aterm/pkg/bin"),
            Path::new("/Users//x/Library/Application Support/aterm/pkg/agents"),
        );
        let (_, zsh) = files.iter().find(|(n, _)| n.ends_with(".zsh")).unwrap();
        assert!(
            zsh.contains("\"/Users//x/Library/Application Support/aterm/pkg/bin\"")
                && zsh.contains("\"/Users//x/Library/Application Support/aterm/pkg/agents\""),
            "a path with a space stays one double-quoted PATH element"
        );
    }
}
