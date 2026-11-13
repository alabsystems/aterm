// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell hooks (`~/.aterm/shell.d/00-atpkg.{zsh,bash,fish,ps1}`), rc wiring, and command links.

use std::fs;
use std::io;
use std::path::Path;

use crate::platform::ensure_private_dir;
use crate::store::Layout;
use crate::zdotdir::ZshDir;

/// The folders an account's rc rows ([`RC_FILES`]) live in: `$HOME`, and the one zsh reads its
/// `.zshrc` from ([`crate::zdotdir`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcHome {
    /// `$HOME`: `~/.aterm`, its ledger, and every row but zsh's.
    pub home: std::path::PathBuf,
    /// The folder zsh reads `.zshrc` from.
    pub zsh: ZshDir,
}

impl RcHome {
    /// `home`, where zsh reads `.zshrc` from `home` too.
    #[must_use]
    pub fn at(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            zsh: ZshDir::Home,
        }
    }

    /// `home` with zsh's folder as this process learns it ([`crate::zdotdir::of_process`]).
    #[must_use]
    pub fn of_process(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            zsh: crate::zdotdir::of_process(home),
        }
    }

    /// The file row `rc` names: zsh's `.zshrc` in zsh's folder, `None` while that is unknown;
    /// every other row under `$HOME`.
    #[must_use]
    pub fn path(&self, rc: &str) -> Option<std::path::PathBuf> {
        if rc == ZSHRC {
            self.zsh.dir(&self.home).map(|dir| dir.join(rc))
        } else {
            Some(self.home.join(rc))
        }
    }

    /// Row `rc` as a person reads it: `~/…` under `$HOME`, else the whole path.
    #[must_use]
    pub fn shown(&self, rc: &str) -> String {
        match self.path(rc) {
            Some(path) => match path.strip_prefix(&self.home) {
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => path.display().to_string(),
            },
            None => format!("zsh's {rc}"),
        }
    }
}

/// zsh's row in [`RC_FILES`], the one that lives in zsh's folder rather than `$HOME`.
pub(crate) const ZSHRC: &str = ".zshrc";

/// The hook file base name (`00-` sorts first).
pub const HOOK_BASENAME: &str = "00-atpkg";

/// The environment variables whose union (any one non-empty) says a process runs inside an aterm session.
pub const AGENTS_MARKERS: &[&str] = &["ATERM_AGENTS_DIR", "ATERM_CHILD", "ATERM_SESSION_ID"];

/// [`AGENTS_MARKERS`] formatted as a single POSIX parameter expansion word.
#[must_use]
pub fn agents_markers_sh() -> String {
    AGENTS_MARKERS
        .iter()
        .map(|m| format!("${{{m}-}}"))
        .collect()
}

/// Hook filename sourced by `shell` (`00-atpkg.{zsh,bash,fish,ps1}`).
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

/// Path and filename of `shell`'s hook under `<home>/.aterm/shell.d` if it exists.
#[must_use]
pub fn hook_file(home: &Path, shell: &str) -> Option<(std::path::PathBuf, &'static str)> {
    let hook = hook_for_shell(shell)?;
    let path = home.join(".aterm").join("shell.d").join(hook);
    fs::symlink_metadata(&path)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|_| (path, hook))
}

/// Whether `<home>/.aterm/shell.d` holds an atpkg hook for any shell.
#[must_use]
pub fn written(home: &Path) -> bool {
    ["zsh", "bash", "fish", "pwsh"]
        .into_iter()
        .any(|shell| hook_file(home, shell).is_some())
}

/// Whether `shell` has an rc file under `home` sourcing the atpkg hook, paired with the hook name.
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
        crate::metadata_io::read_bounded_regular_utf8(&real, 1024 * 1024)
            .is_ok_and(|body| body.contains(RC_BEGIN) || body.contains(hook))
    });
    Some((wired, hook))
}

/// Dialect `(filename, content)` pairs generated for `bin_dir`, `agents_dir`, and `reroute_dir`.
#[must_use]
pub fn hook_files(bin_dir: &Path, agents_dir: &Path, reroute_dir: &Path) -> Vec<(String, String)> {
    let bin = sh_quote(bin_dir);
    let agents = sh_quote(agents_dir);
    let reroute = sh_quote(reroute_dir);
    let fish_bin = fish_quote(bin_dir);
    let fish_agents = fish_quote(agents_dir);
    let fish_reroute = fish_quote(reroute_dir);
    let passthrough = crate::reroute::PASSTHROUGH_ENV;
    let reroute_marker = crate::reroute::REROUTE_DIR_ENV;
    let markers_sh = agents_markers_sh();
    let markers_fish: String = AGENTS_MARKERS.iter().map(|m| format!("${m}")).collect();
    let markers_ps: String = AGENTS_MARKERS
        .iter()
        .map(|m| format!("$env:{m}"))
        .collect::<Vec<_>>()
        .join(" -or ");
    let posix = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         __atpkg_bin=\"{bin}\"\n\
         case \":$PATH:\" in *\":$__atpkg_bin:\"*) ;; *) export PATH=\"$PATH:$__atpkg_bin\" ;; esac\n\
         export ATPKG_BIN=\"$__atpkg_bin\"\n\
         __atpkg_agents=\"{agents}\"\n\
         __atpkg_reroute=\"{reroute}\"\n\
         __atpkg_p=\":$PATH:\"\n\
         while :; do __atpkg_q=\"${{__atpkg_p//\":$__atpkg_agents:\"/:}}\"; __atpkg_q=\"${{__atpkg_q//\":$__atpkg_reroute:\"/:}}\"; case \"{markers_sh}\" in \"\") ;; *) __atpkg_q=\"${{__atpkg_q//\":$__atpkg_bin:\"/:}}\" ;; esac; [ \"$__atpkg_q\" = \"$__atpkg_p\" ] && break; __atpkg_p=\"$__atpkg_q\"; done\n\
         __atpkg_front=\n\
         case \"{markers_sh}\" in \"\") unset ATPKG_AGENTS ;; *) __atpkg_front=\"$__atpkg_agents:$__atpkg_bin\"; export ATPKG_AGENTS=\"$__atpkg_agents\" ;; esac\n\
         case \"${{{passthrough}-}}\" in \"\"|0) case \"{markers_sh}${{{reroute_marker}-}}\" in \"\") ;; *) __atpkg_front=\"$__atpkg_reroute${{__atpkg_front:+:}}$__atpkg_front\" ;; esac ;; esac\n\
         case \"$__atpkg_front\" in\n\
         \"\") case \"$__atpkg_p\" in :) export PATH=\"\" ;; *) __atpkg_p=\"${{__atpkg_p#:}}\"; __atpkg_p=\"${{__atpkg_p%:}}\"; export PATH=\"$__atpkg_p\" ;; esac ;;\n\
         *) case \"$__atpkg_p\" in :) export PATH=\"$__atpkg_front\" ;; *) __atpkg_p=\"${{__atpkg_p#:}}\"; __atpkg_p=\"${{__atpkg_p%:}}\"; export PATH=\"$__atpkg_front:$__atpkg_p\" ;; esac ;;\n\
         esac\n\
         unset __atpkg_bin __atpkg_agents __atpkg_reroute __atpkg_front __atpkg_p __atpkg_q\n"
    );
    let fish = format!(
        "# Generated by atpkg -- DO NOT EDIT (rewritten on every install/update).\n\
         set -l __atpkg_bin \"{fish_bin}\"\n\
         if not contains $__atpkg_bin $PATH; set -gx PATH $PATH $__atpkg_bin; end\n\
         set -gx ATPKG_BIN $__atpkg_bin\n\
         set -l __atpkg_agents \"{fish_agents}\"\n\
         set -l __atpkg_reroute \"{fish_reroute}\"\n\
         set -l __atpkg_front\n\
         if test -n \"{markers_fish}\"; set __atpkg_front \"$__atpkg_agents\" \"$__atpkg_bin\"; set -gx ATPKG_AGENTS $__atpkg_agents; else; set -e ATPKG_AGENTS; end\n\
         if test -z \"${passthrough}\"; or test \"${passthrough}\" = 0; if test -n \"{markers_fish}${reroute_marker}\"; set __atpkg_front \"$__atpkg_reroute\" $__atpkg_front; end; end\n\
         set -l __atpkg_rest\n\
         for __atpkg_d in $PATH; if test \"$__atpkg_d\" != \"$__atpkg_agents\"; and test \"$__atpkg_d\" != \"$__atpkg_reroute\"; if test -z \"{markers_fish}\"; or test \"$__atpkg_d\" != \"$__atpkg_bin\"; set __atpkg_rest $__atpkg_rest \"$__atpkg_d\"; end; end; end\n\
         set -gx PATH $__atpkg_front $__atpkg_rest\n"
    );
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
         $__atpkg_in = [bool]({markers_ps})\n\
         $__atpkg_rest = @(($env:PATH -split [regex]::Escape($__atpkg_sep)) | Where-Object {{ $_ -ne $__atpkg_agents -and $_ -ne $__atpkg_reroute -and (-not $__atpkg_in -or $_ -ne $__atpkg_bin) }})\n\
         $__atpkg_front = @()\n\
         if ($__atpkg_in) {{ $__atpkg_front = @($__atpkg_agents, $__atpkg_bin); $env:ATPKG_AGENTS = $__atpkg_agents }} else {{ Remove-Item Env:\\ATPKG_AGENTS -ErrorAction SilentlyContinue }}\n\
         if (($__atpkg_in -or $env:{reroute_marker}) -and (-not $env:{passthrough} -or $env:{passthrough} -eq '0')) {{ $__atpkg_front = @($__atpkg_reroute) + $__atpkg_front }}\n\
         $env:PATH = ($__atpkg_front + $__atpkg_rest) -join $__atpkg_sep\n\
         Remove-Variable __atpkg_agents, __atpkg_reroute, __atpkg_front, __atpkg_bin, __atpkg_sep, __atpkg_rest, __atpkg_in\n"
    );
    vec![
        (format!("{HOOK_BASENAME}.zsh"), posix.clone()),
        (format!("{HOOK_BASENAME}.bash"), posix),
        (format!("{HOOK_BASENAME}.fish"), fish),
        (format!("{HOOK_BASENAME}.ps1"), powershell),
    ]
}

/// Whether `dest` is a regular 0600 file matching `content`.
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

/// What stood at a hook's destination before this pass replaced it.
enum Prev {
    /// Nothing did: an unwind takes the file this pass laid back out.
    Absent,
    /// The previous hook file or link, moved aside without following it.
    Saved(std::path::PathBuf),
}

fn move_aside(dest: &Path, saved: &Path) -> io::Result<Prev> {
    let meta = match fs::symlink_metadata(dest) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Prev::Absent),
        Err(e) => return Err(e),
    };
    if !meta.is_file() && !meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a hook file or link", dest.display()),
        ));
    }
    fs::rename(dest, saved)?;
    Ok(Prev::Saved(saved.to_path_buf()))
}

fn restore_prev(dest: &Path, prev: Prev) -> io::Result<()> {
    match prev {
        Prev::Absent => match fs::remove_file(dest) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        },
        Prev::Saved(saved) => fs::rename(&saved, dest),
    }
}

/// Write dialect hooks into `shell_d` (0600 via temp + rename) and remove any stray `00-atpkg.sh`.
pub fn write_hooks(
    shell_d: &Path,
    bin_dir: &Path,
    agents_dir: &Path,
    reroute_dir: &Path,
) -> io::Result<Vec<String>> {
    HookPublication::prepare(shell_d, bin_dir, agents_dir, reroute_dir)?.publish()
}

fn hook_lock_file(shell_d: &Path) -> io::Result<fs::File> {
    let path = shell_d.join("hooks.lock");
    crate::index_probe::open_private(&path, true).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is not a writable regular non-link lock file",
                path.display()
            ),
        )
    })
}

struct HookPublication {
    // Keep the rendezvous file in place: unlinking it would split concurrent locks.
    _lock: crate::lock::Flock,
    directory: Option<aterm_tempfile::TempDir>,
    shell_d: std::path::PathBuf,
    staged: Vec<(std::path::PathBuf, std::path::PathBuf, String)>,
    current: Vec<String>,
}

impl HookPublication {
    fn prepare(
        shell_d: &Path,
        bin_dir: &Path,
        agents_dir: &Path,
        reroute_dir: &Path,
    ) -> io::Result<Self> {
        Self::prepare_with(shell_d, bin_dir, agents_dir, reroute_dir, stage_hook)
    }

    fn prepare_with(
        shell_d: &Path,
        bin_dir: &Path,
        agents_dir: &Path,
        reroute_dir: &Path,
        mut stage: impl FnMut(&Path, &str) -> io::Result<std::path::PathBuf>,
    ) -> io::Result<Self> {
        let lock = crate::lock::Flock::lock_within(
            hook_lock_file(shell_d)?,
            std::time::Duration::from_secs(60),
            &shell_d.join("hooks.lock"),
        )?;
        let files = hook_files(bin_dir, agents_dir, reroute_dir);
        let current: Vec<String> = files
            .iter()
            .filter(|(name, content)| hook_is_current(&shell_d.join(name), content))
            .map(|(name, _)| name.clone())
            .collect();
        let directory = if current.len() == files.len() {
            None
        } else {
            Some(
                aterm_tempfile::Builder::new()
                    .prefix("hook-publication-")
                    .suffix(".noindex")
                    .tempdir_in(shell_d)?,
            )
        };
        let mut staged = Vec::with_capacity(files.len());
        for (name, content) in files.iter().filter(|(name, _)| !current.contains(name)) {
            let dest = shell_d.join(name);
            let tmp = stage(
                &directory
                    .as_ref()
                    .expect("changed hooks own staging")
                    .path()
                    .join(name),
                content,
            )?;
            staged.push((tmp, dest, name.clone()));
        }
        Ok(Self {
            _lock: lock,
            directory,
            shell_d: shell_d.to_path_buf(),
            staged,
            current,
        })
    }

    fn publish(mut self) -> io::Result<Vec<String>> {
        let mut written = std::mem::take(&mut self.current);
        let mut published: Vec<(std::path::PathBuf, Prev)> = Vec::with_capacity(self.staged.len());
        for (tmp, dest, name) in std::mem::take(&mut self.staged) {
            let saved = self
                .directory
                .as_ref()
                .expect("staged hooks own their directory")
                .path()
                .join(format!("previous-{name}"));
            let prev = match move_aside(&dest, &saved) {
                Ok(prev) => prev,
                Err(e) => return Err(self.rollback(published, e)),
            };
            if let Err(e) = fs::rename(tmp, &dest) {
                published.push((dest, prev));
                return Err(self.rollback(published, e));
            }
            published.push((dest, prev));
            written.push(name);
        }
        let stray = self.shell_d.join(format!("{HOOK_BASENAME}.sh"));
        if stray.exists() {
            let _ = fs::remove_file(&stray);
        }
        Ok(written)
    }

    fn rollback(
        &mut self,
        published: Vec<(std::path::PathBuf, Prev)>,
        cause: io::Error,
    ) -> io::Error {
        let failures: Vec<String> = published
            .into_iter()
            .rev()
            .filter_map(|(dest, previous)| {
                restore_prev(&dest, previous)
                    .err()
                    .map(|e| format!("{}: {e}", dest.display()))
            })
            .collect();
        if failures.is_empty() {
            return cause;
        }
        let kept = self
            .directory
            .take()
            .expect("failed publication owns staging")
            .keep();
        io::Error::other(format!(
            "hook publication failed: {cause}; rollback failed: {}; saved files retained at {}",
            failures.join("; "),
            kept.display()
        ))
    }
}

/// Hook filenames under `<home>/.aterm/shell.d` that differ from what [`write_hooks`] lays.
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

/// Existing `~/.aterm` and `~/.aterm/shell.d` directories under `home` that are not private (0700).
pub(crate) fn shell_dirs_not_private(home: &Path) -> Vec<std::path::PathBuf> {
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    [aterm, shell_d]
        .into_iter()
        .filter(|dir| fs::symlink_metadata(dir).is_ok_and(|meta| !laid_private(&meta)))
        .collect()
}

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

/// Harden `~/.aterm` and `~/.aterm/shell.d` under `home` and write the hooks for `layout`.
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

/// Command symlinks [`ensure_command_links_from`] would update under `<home>/.local/bin`.
#[cfg(unix)]
pub(crate) fn command_links_to_write(
    home: &Path,
    exe: &Path,
) -> Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    let (Some(macos), Some(bin)) = (command_link_source(exe), command_links_dir(home)) else {
        return Vec::new();
    };
    let mut writes = Vec::new();
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

/// Missing argv0 store aliases beside the canonical store executable `exe`.
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

/// Outcome of a shell hooks and rc pass.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HookPass {
    /// `~/.aterm` and `~/.aterm/shell.d` were hardened and the hook files are on disk as
    /// rendered — written, or already byte-identical and left alone ([`write_hooks`]).
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
    /// stale dev bundle would lay stale hooks.
    NotReleaseBundle,
}

impl HookPass {
    /// Per-rc outcomes from the pass.
    #[must_use]
    pub fn rc(&self) -> &[(&'static str, RcOutcome)] {
        match self {
            Self::Rewritten(rc) => rc,
            Self::HooksNotWritten | Self::NotHardened | Self::NotReleaseBundle => &[],
        }
    }
}

/// Outcome of a pass on a single rc file.
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
    /// after the user deleted the file.
    RelaidOverOptOut,
    /// The user had deleted atpkg's block (or the login shell's rc atpkg created) and this
    /// pass left it exactly as it was.
    OptOutKept,
    /// The rc resolves under a folder macOS guards with a consent dialog, so it was not
    /// even opened (the consent fence in `ensure_rc_sources_hooks`).
    ConsentFenced,
    /// The rc exists but is not something atpkg edits: a dangling link, not a regular
    /// file, or not UTF-8, or its persisted wiring history cannot be read safely.
    Unreadable,
    /// The rewrite was attempted and failed; the rc is unchanged (the rc or its directory
    /// refused the write, or the temp could not be created exclusively beside it).
    WriteFailed,
    /// zsh's row: which folder zsh reads `.zshrc` from could not be learned
    /// ([`ZshDir::Unknown`]), so no file was opened for it.
    ZshFolderUnknown,
}

/// Harden `~/.aterm/shell.d`, write hooks, wire rc files, and lay `~/.local/bin` command links.
pub fn refresh(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::HonorOptOut, account_login_rc)
}

/// Run [`refresh`] without creating a missing login rc file.
pub fn refresh_creating_no_rc(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::HonorOptOut, |_| None)
}

/// Run [`refresh`], restoring the rc block even where the user removed it.
pub fn refresh_rewiring_rc(layout: &Layout) -> HookPass {
    refresh_with(layout, RcWiring::Rewire, account_login_rc)
}

/// Policy for handling an rc file whose hook block was deleted by the user.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RcWiring {
    /// An rc recorded in [`RC_LEDGER`] that no longer carries the block is skipped.
    HonorOptOut,
    /// Every eligible rc is wired, ledger or not.
    Rewire,
}

fn refresh_with(
    layout: &Layout,
    wiring: RcWiring,
    creates: fn(&RcHome) -> Option<&'static str>,
) -> HookPass {
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).ok();
    let rcs = aterm_types::dirs::home_dir().map(|home| RcHome::of_process(&home));
    let login = rcs.as_ref().and_then(creates);
    refresh_as(
        layout,
        wiring,
        exe.as_deref(),
        rcs.as_ref(),
        HEALS_STORE,
        login,
    )
}

/// The development seam ([`aterm_types::dev_seam!`]) naming the login shell (a path, as the
/// account record spells it) a development build reads in place of the account record's:
/// a test's `chsh`, so a spawned pass can be given a login shell its host account does not
/// have.
pub const LOGIN_SHELL_SEAM: &str = "ATPKG_LOGIN_SHELL";

/// The account's login shell from the system account record.
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

/// The [`RC_FILES`] entry read by this account's login shell.
#[must_use]
pub fn account_login_rc(rcs: &RcHome) -> Option<&'static str> {
    let shell = login_shell()?;
    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute());
    login_rc(&shell.to_string_lossy(), rcs, xdg_config.as_deref())
}

/// The [`RC_FILES`] entry read by login shell `shell`: none for zsh while zsh's folder is
/// unknown.
#[must_use]
pub fn login_rc(shell: &str, rcs: &RcHome, xdg_config: Option<&Path>) -> Option<&'static str> {
    #[cfg(unix)]
    {
        let mut places = crate::selfsetup::Places::defaults(&rcs.home, None, xdg_config);
        places.zdotdir = rcs.zsh.clone();
        let env = crate::selfsetup::ShellEnv {
            shell: Some(shell.to_owned()),
            ..crate::selfsetup::ShellEnv::default()
        };
        let target = crate::selfsetup::rc_target(&places, &env)?;
        RC_FILES
            .iter()
            .map(|(rc, _)| *rc)
            .find(|rc| rcs.path(rc).as_ref() == Some(&target))
    }
    #[cfg(not(unix))]
    {
        let _ = (shell, rcs, xdg_config);
        None
    }
}

/// Whether the hooks pass completes a store's argv0 aliases ([`heal_store_aliases`]): off
/// macOS only.
pub(crate) const HEALS_STORE: bool = cfg!(unix) && !cfg!(target_os = "macos");

fn refresh_as(
    layout: &Layout,
    wiring: RcWiring,
    exe: Option<&Path>,
    rcs: Option<&RcHome>,
    heals_store: bool,
    login: Option<&'static str>,
) -> HookPass {
    #[cfg(unix)]
    if heals_store && let Some(exe) = exe {
        heal_store_aliases(exe);
    }
    #[cfg(not(unix))]
    let _ = heals_store;
    let Some(rcs) = rcs else {
        return HookPass::NotHardened;
    };
    pass_as(layout, rcs, wiring, exe, login)
}

/// Store directory containing `exe` when `exe` is a store install (`~/.local/lib/aterm/bin`).
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

/// Create any missing [`aterm_types::argv0::ALIASES`] symlinks in `store`.
#[cfg(unix)]
pub(crate) fn ensure_store_aliases(store: &Path) -> Vec<&'static str> {
    aterm_types::argv0::ALIASES
        .into_iter()
        .filter(|name| std::os::unix::fs::symlink("aterm", store.join(name)).is_ok())
        .collect()
}

/// Run [`ensure_store_aliases`] when canonical `exe` lives in a store directory.
#[cfg(unix)]
pub(crate) fn heal_store_aliases(exe: &Path) -> Vec<&'static str> {
    store_dir_of(exe, canonical).map_or_else(Vec::new, ensure_store_aliases)
}

/// Canonicalize `path`, returning `None` on failure.
#[cfg(unix)]
pub(crate) fn canonical(path: &Path) -> Option<std::path::PathBuf> {
    fs::canonicalize(path).ok()
}

fn pass_as(
    layout: &Layout,
    rcs: &RcHome,
    wiring: RcWiring,
    exe: Option<&Path>,
    login: Option<&'static str>,
) -> HookPass {
    if wiring == RcWiring::HonorOptOut && exe.is_some_and(is_non_release_bundle_exe) {
        return HookPass::NotReleaseBundle;
    }
    pass_at(layout, rcs, wiring, login)
}

/// Run a full hook and rc pass against `rcs` with `wiring`.
pub(crate) fn pass_at(
    layout: &Layout,
    rcs: &RcHome,
    wiring: RcWiring,
    login: Option<&'static str>,
) -> HookPass {
    refresh_at(
        rcs,
        &layout.bin_dir(),
        &layout.agents_dir(),
        &layout.reroute_dir(),
        wiring,
        login,
    )
}

fn refresh_at(
    rcs: &RcHome,
    bin_dir: &Path,
    agents_dir: &Path,
    reroute_dir: &Path,
    wiring: RcWiring,
    login: Option<&'static str>,
) -> HookPass {
    let home = rcs.home.as_path();
    let aterm = home.join(".aterm");
    let shell_d = aterm.join("shell.d");
    if ensure_private_dir(&aterm).is_err() || ensure_private_dir(&shell_d).is_err() {
        return HookPass::NotHardened;
    }
    let hooks_written = write_hooks(&shell_d, bin_dir, agents_dir, reroute_dir).is_ok();
    #[cfg(unix)]
    let rc = if hooks_written {
        ensure_rc_sources_hooks(rcs, wiring, login)
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

/// The markers bounding atpkg's block in a user's shell rc, so the block is found, refreshed
/// and removed exactly, never matched by content.
#[cfg(unix)]
pub(crate) const RC_BEGIN: &str = "# >>> atpkg shell integration >>>";
#[cfg(unix)]
pub(crate) const RC_END: &str = "# <<< atpkg shell integration <<<";

/// The rc files atpkg wires: each row is a path relative to `$HOME` (zsh's, [`ZSHRC`], relative
/// to the folder zsh reads it from: [`RcHome::path`]) and the `shell.d` hook it sources.
#[cfg(unix)]
pub(crate) const RC_FILES: [(&str, &str); 4] = [
    (".zshrc", "00-atpkg.zsh"),
    (".bashrc", "00-atpkg.bash"),
    (".bash_profile", "00-atpkg.bash"),
    (".config/fish/config.fish", "00-atpkg.fish"),
];

/// `~/.aterm/rc-wired`: the rc files this machine has wired, one canonical path per line.
#[cfg(unix)]
const RC_LEDGER: &str = "rc-wired";

/// Canonical path records, including newline, admitted by both the reader and writer.
#[cfg(unix)]
const RC_HISTORY_LINE_BYTES: usize = 4096;

#[cfg(unix)]
fn rc_ledger_path(home: &Path) -> std::path::PathBuf {
    home.join(".aterm").join(RC_LEDGER)
}

/// The recorded rc paths. Missing is fresh; refused evidence is unknown and holds wiring,
/// never permission to restore a hook the person may have removed. Record IO is bounded;
/// the exact set grows with distinct historical paths.
#[cfg(unix)]
fn read_rc_ledger(home: &Path) -> io::Result<std::collections::BTreeSet<String>> {
    let mut paths = std::collections::BTreeSet::new();
    match crate::metadata_io::read_regular_lines(
        &rc_ledger_path(home),
        RC_HISTORY_LINE_BYTES,
        |line| {
            let path = line.trim();
            if path.len() >= RC_HISTORY_LINE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "rc history path exceeds its record bound",
                ));
            }
            if !path.is_empty() {
                paths.insert(path.to_owned());
            }
            Ok(true)
        },
    ) {
        Ok(()) => Ok(paths),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn rc_history_entry(real: &Path) -> Option<String> {
    let entry = real.to_string_lossy();
    (!entry.is_empty()
        && entry.trim() == entry.as_ref()
        && !entry.contains(['\n', '\r'])
        && entry.len() < RC_HISTORY_LINE_BYTES)
        .then(|| entry.into_owned())
}

/// Record `real` as wired, best-effort, through the workspace's careful writer
/// ([`aterm_uds::atomic_file::write`]: a `0600` temp durably flushed, renamed over the ledger,
/// its directory flushed). Already recorded means no write, so the ordinary pass touches
/// nothing.
#[cfg(unix)]
fn record_rc_wired(home: &Path, real: &Path, ledger: &mut std::collections::BTreeSet<String>) {
    let Some(entry) = rc_history_entry(real) else {
        return;
    };
    if !ledger.insert(entry) {
        return;
    }
    if ensure_private_dir(&home.join(".aterm")).is_err() {
        return;
    }
    let body: String = ledger.iter().map(|path| format!("{path}\n")).collect();
    let _ = aterm_uds::atomic_file::write(&rc_ledger_path(home), body.as_bytes(), 0o600);
}

/// Ensure the existing rc files of `rcs` source the `shell.d` hooks.
#[cfg(unix)]
pub(crate) fn ensure_rc_sources_hooks(
    rcs: &RcHome,
    wiring: RcWiring,
    login: Option<&'static str>,
) -> Vec<(&'static str, RcOutcome)> {
    let home = rcs.home.as_path();
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let Ok(mut ledger) = read_rc_ledger(home) else {
        return RC_FILES
            .iter()
            .map(|(rc, _)| (*rc, RcOutcome::Unreadable))
            .collect();
    };
    let mut outcomes: Vec<(&'static str, RcOutcome)> = Vec::new();
    for (rc, hook) in RC_FILES {
        let Some(rc_path) = rcs.path(rc) else {
            outcomes.push((rc, RcOutcome::ZshFolderUnknown));
            continue;
        };
        let (real, metadata) = match crate::protected::resolve_path(
            &canonical_home,
            &rc_path,
            |path| fs::symlink_metadata(path),
            |path| fs::read_link(path),
        ) {
            Ok(resolved) => resolved,
            Err(crate::protected::PathRefusal::ConsentFenced) => {
                outcomes.push((rc, RcOutcome::ConsentFenced));
                continue;
            }
            Err(_) => {
                outcomes.push((rc, RcOutcome::Unreadable));
                continue;
            }
        };
        if metadata.is_none() {
            if !fs::symlink_metadata(&rc_path)
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            {
                // An existing dangling leaf link is not an invitation to create its target.
                outcomes.push((rc, RcOutcome::Unreadable));
                continue;
            }
            if login == Some(rc)
                && let Some(outcome) = create_login_rc(home, &real, hook, wiring, &mut ledger)
            {
                outcomes.push((rc, outcome));
            }
            continue;
        }
        if rc_history_entry(&real).is_none() {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        }
        let Ok(existing) = crate::metadata_io::read_bounded_regular_utf8(&real, 1024 * 1024) else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        if existing.contains(RC_BEGIN) {
            record_rc_wired(home, &real, &mut ledger);
            outcomes.push((rc, RcOutcome::AlreadyWired));
            continue;
        }
        let opted_out = ledger.contains(real.to_string_lossy().as_ref());
        if wiring == RcWiring::HonorOptOut && opted_out {
            outcomes.push((rc, RcOutcome::OptOutKept));
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&real) else {
            outcomes.push((rc, RcOutcome::Unreadable));
            continue;
        };
        let mut next = existing;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push('\n');
        next.push_str(&rc_block(home, hook));
        let written = aterm_uds::atomic_file::write(
            &real,
            next.as_bytes(),
            std::os::unix::fs::PermissionsExt::mode(&meta.permissions()),
        );
        if written.is_err() {
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

/// An admitted, resolved login rc path whose parent directory already exists. A missing
/// directory is never made by the RC pass.
#[cfg(unix)]
fn login_rc_target(path: &Path) -> Option<std::path::PathBuf> {
    fs::symlink_metadata(path.parent()?)
        .ok()?
        .is_dir()
        .then(|| path.to_path_buf())
}

/// What a pass does about the absent login rc at `path` ([`login_rc`]); `None` when it can be
/// neither created nor reported (its directory is missing).
/// The caller has already resolved and admitted `path` through the consent walker.
#[cfg(unix)]
fn create_login_rc(
    home: &Path,
    path: &Path,
    hook: &str,
    wiring: RcWiring,
    ledger: &mut std::collections::BTreeSet<String>,
) -> Option<RcOutcome> {
    let real = login_rc_target(path)?;
    if rc_history_entry(&real).is_none() {
        return Some(RcOutcome::Unreadable);
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

/// Report state for the absent login rc file row `rc` of `rcs`.
#[cfg(unix)]
pub(crate) fn absent_login_rc(rcs: &RcHome, rc: &str) -> Option<RcState> {
    let path = rcs.path(rc)?;
    let home = rcs.home.as_path();
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let real = match crate::protected::resolve_path(
        &canonical_home,
        &path,
        |path| fs::symlink_metadata(path),
        |path| fs::read_link(path),
    ) {
        Err(crate::protected::PathRefusal::ConsentFenced) => return Some(RcState::ConsentFenced),
        Ok((real, None)) => real,
        _ => return None,
    };
    if !fs::symlink_metadata(&path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound) {
        return None;
    }
    let real = login_rc_target(&real)?;
    Some(match read_rc_ledger(home) {
        Ok(ledger) if ledger.contains(real.to_string_lossy().as_ref()) => RcState::OptedOut,
        Ok(_) => RcState::Unwired,
        Err(_) => RcState::HistoryUnreadable,
    })
}

/// The bash login profiles `tools/install.sh`'s `path_block_rc_target` can elect that atpkg
/// has no row for, relative to `$HOME`: login bash reads the first of `~/.bash_profile`,
/// `~/.bash_login`, `~/.profile` that exists, and these are the fall-through.
#[cfg(unix)]
pub(crate) const INSTALL_SH_PROFILES: [&str; 2] = [".bash_login", ".profile"];

/// Wiring state of an existing rc file for `doctor`.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RcState {
    /// The rc carries atpkg's marker block ([`RC_BEGIN`]).
    Wired,
    /// The rc sources `~/.aterm/shell.d` through a line that is NOT this block: the PATH
    /// block `aterm pkg shell-setup --path` writes ([`crate::selfsetup::PATH_BLOCK_START`]),
    /// or a line the user wrote.
    SourcedElsewhere,
    /// atpkg wired this rc once ([`RC_LEDGER`]) and the block is gone: the user opted out.
    OptedOut,
    /// The rc exists, no [`RC_LEDGER`] entry records atpkg wiring it, and nothing else
    /// sources `shell.d` from it: no pass has run since it appeared.
    Unwired,
    /// Persisted history is unreadable; automatic wiring is held without inferring a choice.
    HistoryUnreadable,
    /// The rc resolves under a folder macOS guards with a consent dialog, so no pass opens it
    /// ([`ensure_rc_sources_hooks`]'s consent fence), and neither does this report.
    ConsentFenced,
    /// zsh's row, whose folder could not be learned ([`ZshDir::Unknown`]): no pass opens a
    /// `.zshrc` for it.
    ZshFolderUnknown,
}

/// How [`rc_wiring`] resolves one rc: the steps [`ensure_rc_sources_hooks`] takes before it
/// would write, so the report never describes a file the pass would treat differently.
#[cfg(unix)]
enum RcRead {
    /// The resolved target and its contents.
    Body(std::path::PathBuf, String),
    /// The rc resolves under a macOS-guarded root and was not opened.
    Fenced,
}

#[cfg(unix)]
fn read_rc_for_report(canonical_home: &Path, rc_path: &Path) -> Option<RcRead> {
    let real = match crate::protected::resolve_path(
        canonical_home,
        rc_path,
        |path| fs::symlink_metadata(path),
        |path| fs::read_link(path),
    ) {
        Err(crate::protected::PathRefusal::ConsentFenced) => return Some(RcRead::Fenced),
        Ok((real, Some(metadata))) if metadata.is_file() => real,
        _ => return None,
    };
    let body = crate::metadata_io::read_bounded_regular_utf8(&real, 1024 * 1024).ok()?;
    Some(RcRead::Body(real, body))
}

/// Wiring state of every existing rc file of `rcs` in report order, and zsh's row while its
/// folder is unknown.
#[cfg(unix)]
pub(crate) fn rc_wiring(rcs: &RcHome) -> Vec<(&'static str, RcState)> {
    let home = rcs.home.as_path();
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let Ok(ledger) = read_rc_ledger(home) else {
        return RC_FILES
            .iter()
            .map(|(rc, _)| (*rc, RcState::HistoryUnreadable))
            .collect();
    };
    let mut wiring: Vec<(&'static str, RcState)> = RC_FILES
        .iter()
        .filter_map(|(rc, _)| {
            let Some(path) = rcs.path(rc) else {
                return Some((*rc, RcState::ZshFolderUnknown));
            };
            let state = match read_rc_for_report(&canonical_home, &path)? {
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

/// The rows [`rc_wiring`] inspects ([`RcHome::path`] places each).
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

/// Put `aterm`, `atpkg`, `amacs` and `agh` in `~/.local/bin` when we are running from an app
/// bundle ([`COMMAND_LINKS`]).
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

fn in_release_bundle(path: &Path) -> bool {
    path.to_string_lossy()
        .contains("/aterm.app/Contents/MacOS/")
}

/// Whether canonical `exe` runs from an app bundle other than release `aterm.app`.
pub(crate) fn is_non_release_bundle_exe(exe: &Path) -> bool {
    exe.parent().is_some_and(|d| d.ends_with("Contents/MacOS")) && !in_release_bundle(exe)
}

/// Whether this process runs from a non-release app bundle.
#[must_use]
pub fn runs_from_non_release_bundle() -> bool {
    std::env::current_exe()
        .and_then(|e| e.canonicalize())
        .is_ok_and(|exe| is_non_release_bundle_exe(&exe))
}

/// Ensure `~/.local/bin` command links point to canonical `exe`.
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
/// nothing stands; `Some(Some(old))` re-points a link to `old`.
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
                || (matches!(name, "amacs" | "agh") && in_a_bundle(&old) && aterm_settled);
            (ours && old != target).then_some(Some(old))
        }
        Ok(_) => None,
        Err(_) => Some(None),
    }
}

/// The names the command-link pass lays in `~/.local/bin`, in the order it lays them.
/// `agh`, aterm's GitHub client, is owned as `aterm` is and comes last; a bundle from before
/// agh has no `Contents/MacOS/agh`, so it lays no link to one.
#[cfg(unix)]
pub(crate) const COMMAND_LINKS: [&str; 4] = ["aterm", "atpkg", "amacs", "agh"];

/// `Contents/MacOS` directory from which canonical `exe` may lay command links.
#[cfg(unix)]
pub(crate) fn command_link_source(exe: &Path) -> Option<&Path> {
    command_link_check(exe).ok()
}

/// Whether canonical `exe` is the release `aterm.app`'s where it is installed, which
/// [`command_link_check`] admits: not a dev or renamed copy, Gatekeeper's temporary copy or a
/// mounted volume.
#[cfg(unix)]
#[must_use]
pub fn is_installed_release_exe(exe: &Path) -> bool {
    command_link_check(exe).is_ok()
}

/// [`command_link_source`] with a user-facing refusal reason when `exe` may not lay links.
#[cfg(unix)]
pub(crate) fn command_link_check(exe: &Path) -> Result<&Path, &'static str> {
    let macos = exe
        .parent()
        .filter(|d| d.ends_with("Contents/MacOS"))
        .ok_or("this atpkg is not inside an app bundle")?;
    if !in_release_bundle(exe) {
        return Err("this atpkg is not the release aterm.app's (a dev or renamed copy)");
    }
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

/// Command paths in `<home>/.local/bin` that are regular files instead of symlinks.
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

/// Command hint for running `repair` from the installed app.
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

/// User-facing remedy for a copied command binary at `path`.
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

/// Record of a regular file moved out of a command link path by `repair`.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MovedAside {
    pub(crate) from: std::path::PathBuf,
    pub(crate) to: std::path::PathBuf,
    /// Whether the app was linked in its place (a retired name gets no link).
    pub(crate) linked: bool,
}

/// Files moved aside and errors encountered during `repair`'s command-link step.
#[cfg(unix)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AsideOutcome {
    pub(crate) moved: Vec<MovedAside>,
    pub(crate) failed: Vec<(std::path::PathBuf, String)>,
}

/// Move copied binaries in `<home>/.local/bin` aside to `<name>.moved-aside`.
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
/// nothing there).
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
                std::os::unix::fs::symlink(target, &tmp)
                    .and_then(|()| {
                        let renamed = fs::rename(&tmp, from);
                        if renamed.is_err() {
                            // Only remove a temporary link this call successfully created.
                            let _ = fs::remove_file(&tmp);
                        }
                        renamed
                    })
                    .map_err(|e| format!("the link to the app could not be put in its place ({e})"))
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

/// Return `<home>/.local/bin` unless it resolves under a TCC-guarded directory.
#[cfg(unix)]
pub(crate) fn command_links_dir(home: &Path) -> Option<std::path::PathBuf> {
    let bin = home.join(".local/bin");
    let canonical_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let (_, metadata) = crate::protected::resolve_path(
        &canonical_home,
        &bin,
        |path| fs::symlink_metadata(path),
        |path| fs::read_link(path),
    )
    .ok()?;
    metadata.is_none_or(|meta| meta.is_dir()).then_some(bin)
}

/// Escape `bin_dir` for a double-quoted POSIX shell string.
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

/// Escape `p` for a double-quoted fish string.
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

fn ps_quote(bin_dir: &Path) -> String {
    bin_dir.to_string_lossy().replace('\'', "''")
}

fn stage_hook(tmp: &Path, content: &str) -> io::Result<std::path::PathBuf> {
    let staged = crate::platform::create_private_new(tmp).and_then(|mut f| {
        io::Write::write_all(&mut f, content.as_bytes())?;
        crate::platform::set_mode_on(&f, 0o600)
    });
    match staged {
        Ok(()) => Ok(tmp.to_path_buf()),
        Err(e) => {
            let _ = fs::remove_file(tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
