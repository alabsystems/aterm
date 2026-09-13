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
//! to the user's `~/.zshrc` / `~/.bashrc` / fish config so an ORDINARY interactive shell
//! — Terminal.app, iTerm, VS Code, ssh, an agent's shell — gets the managed tools too.
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

/// The `(filename, content)` for each shell dialect, parametrized on `bin_dir` (APPENDED)
/// and `agents_dir` (MOVED TO THE FRONT — the agent programs' shims, module doc).
/// NEVER a `.sh` (fish sources `*.sh`, which a POSIX body would break).
#[must_use]
pub fn hook_files(bin_dir: &Path, agents_dir: &Path) -> Vec<(String, String)> {
    let bin = sh_quote(bin_dir);
    let agents = sh_quote(agents_dir);
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
         set -l __atpkg_agents \"{agents}\"\n\
         set -l __atpkg_rest\n\
         for __atpkg_d in $PATH; if test \"$__atpkg_d\" != \"$__atpkg_agents\"; set __atpkg_rest $__atpkg_rest \"$__atpkg_d\"; end; end\n\
         set -gx PATH \"$__atpkg_agents\" $__atpkg_rest\n\
         set -gx ATPKG_AGENTS $__atpkg_agents\n\
         set -l __atpkg_bin \"{bin}\"\n\
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

/// Write the three dialect hooks into `shell_d` (each `0600` via temp + rename) and delete a
/// stray POSIX `00-atpkg.sh` (fish-safety). Returns the written file names.
pub fn write_hooks(shell_d: &Path, bin_dir: &Path, agents_dir: &Path) -> io::Result<Vec<String>> {
    let mut written = Vec::new();
    for (name, content) in hook_files(bin_dir, agents_dir) {
        atomic_write(&shell_d.join(&name), &content)?;
        written.push(name);
    }
    // fish-safety: a POSIX `.sh` here is fatal (fish sources *.sh too). Never emitted above;
    // actively remove a hand-dropped one.
    let stray = shell_d.join(format!("{HOOK_BASENAME}.sh"));
    if stray.exists() {
        let _ = fs::remove_file(&stray);
    }
    Ok(written)
}

/// Best-effort choke point called after activation ([`crate::flow`]). Hardens `~/.aterm` +
/// `~/.aterm/shell.d` (0700, symlink-refusing) then writes the hooks. NEVER propagates an
/// error — hooks are ergonomics, not trust; a failed write must not fail an install.
pub fn refresh(layout: &Layout) {
    let Some(home) = aterm_types::dirs::home_dir() else {
        return;
    };
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    if ensure_private_dir(&aterm).is_err() || ensure_private_dir(&shell_d).is_err() {
        return;
    }
    let _ = write_hooks(&shell_d, &layout.bin_dir(), &layout.agents_dir());
    #[cfg(unix)]
    ensure_rc_sources_hooks(&home);
    #[cfg(unix)]
    ensure_command_links(&home);
}

/// The markers bounding atpkg's block in a user's shell rc. Present so the block can be
/// found, refreshed and removed exactly — never matched by content, which drifts.
const RC_BEGIN: &str = "# >>> atpkg shell integration >>>";
const RC_END: &str = "# <<< atpkg shell integration <<<";

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
///   nothing happens) and a user can delete it in one motion;
/// * it SOURCES `shell.d` rather than inlining a PATH, so the path stays correct after
///   the prefix moves and there is exactly one generator;
/// * the sourced hook appends the managed `bin/`, never prepends it, so a managed tool
///   cannot shadow system `sudo`/`ssh`/`git` (the agents dir it prepends can only ever
///   carry `claude`/`codex` — module doc);
/// * every step is best-effort — wiring PATH must never fail an install.
#[cfg(unix)]
fn ensure_rc_sources_hooks(home: &Path) {
    // Both sides of the protected-root comparison canonical: `$TMPDIR` and a home
    // reached through a link both spell `/private/…` once resolved.
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    for (rc, hook) in [
        (".zshrc", "00-atpkg.zsh"),
        (".bashrc", "00-atpkg.bash"),
        (".config/fish/config.fish", "00-atpkg.fish"),
    ] {
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
            continue;
        }
        let Ok(meta) = fs::metadata(&real) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(existing) = fs::read_to_string(&real) else {
            continue;
        };
        if existing.contains(RC_BEGIN) {
            continue; // already wired; the hook body itself is what gets refreshed
        }
        let hook_path = home.join(".aterm").join("shell.d").join(hook);
        let line = if hook.ends_with(".fish") {
            format!("test -f {p}; and source {p}", p = sh_quote(&hook_path))
        } else {
            format!("[ -f {p} ] && . {p}", p = sh_quote(&hook_path))
        };
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&format!(
            "\n{RC_BEGIN}\n\
             # Puts the ALab toolchain on PATH. Managed by atpkg; delete this block to opt out.\n\
             {line}\n\
             {RC_END}\n"
        ));
        // Same temp+rename discipline the hook files use: a reader never sees a
        // half-written rc, and a failure leaves the original untouched. The temp sits
        // beside the REAL file (same filesystem, so the rename replaces it and not the
        // link) and carries its mode, so a 0600 rc stays 0600. A directory we cannot
        // write (a link into the read-only /nix/store) fails here and changes nothing.
        let Some(name) = real.file_name() else {
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
        }
    }
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

/// Put `aterm` and `atpkg` in `~/.local/bin` when we are running from an app bundle.
///
/// The rc wiring is [`ensure_rc_sources_hooks`]'s (the marker-bounded block in an
/// EXISTING `~/.zshrc` / `~/.bashrc` / fish config that sources `shell.d`); this is the
/// OTHER half of reaching the toolchain from outside aterm: the `aterm <tool>` /
/// `atpkg run` route. On the DMG install route those two commands did not exist on any
/// PATH — the binaries live only inside `/Applications/aterm.app/Contents/MacOS`, and
/// only `tools/install.sh` ever linked them out. So the fallback the docs named was
/// itself unreachable for anyone who dragged the app to Applications, which README
/// offers as a first-class option: the ten programs installed, and nothing could reach
/// them from outside aterm (2026-08-20 round-8 audit). (Until 2026-09-10 this comment
/// claimed the module "deliberately does NOT write into `~/.zshrc`" — untrue since
/// the rc wiring landed; `aterm help pkg` said the same and was corrected with it.)
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

/// Render `bin_dir` for a double-quoted shell string, escaping the four metacharacters a
/// double-quoted context still interprets. A HOME-derived path won't contain them, but never
/// interpolate raw.
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

/// Render `bin_dir` for a SINGLE-quoted PowerShell string literal: only the single quote is
/// special there (doubled to escape), so a Windows path's backslashes pass through literally
/// (no `C:\Users` → `C:<tab>sers` surprise) — the reason the `.ps1` uses `'...'`, not `"..."`.
fn ps_quote(bin_dir: &Path) -> String {
    bin_dir.to_string_lossy().replace('\'', "''")
}

/// Write `content` to `dest` atomically (temp `0600` + rename), so a reader never sees a
/// half-written hook.
fn atomic_write(dest: &Path, content: &str) -> io::Result<()> {
    let name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path has no file name"))?;
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path has no parent"))?;
    let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    fs::write(&tmp, content.as_bytes())?;
    crate::platform::harden_file(&tmp)?;
    fs::rename(&tmp, dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

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

        ensure_rc_sources_hooks(&home);
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
        ensure_rc_sources_hooks(&home);
        let twice = std::fs::read_to_string(&zshrc).unwrap();
        assert_eq!(twice, once, "a second pass must be a no-op");
        assert_eq!(twice.matches(RC_BEGIN).count(), 1, "exactly one block");

        let _ = std::fs::remove_dir_all(&home);
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

        ensure_rc_sources_hooks(&home);
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

        ensure_rc_sources_hooks(&home);
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

        ensure_rc_sources_hooks(&home);

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
