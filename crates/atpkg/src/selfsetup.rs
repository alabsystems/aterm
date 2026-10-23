// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Account shell setup and ownership-checked self-uninstall verbs.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The managed shell-profile block's marker PAIR.
pub const PATH_BLOCK_START: &str = "# >>> aterm ALab toolset (managed by install.sh) >>>";
/// The block's end marker.
pub const PATH_BLOCK_END: &str = "# <<< aterm ALab toolset <<<";
/// The first line of the Linux desktop entry `shell-setup --desktop` writes.
pub const DESKTOP_MARKER: &str =
    "# Managed by aterm install.sh — its uninstall removes only an entry carrying this line.";
/// Every man page basename aterm has ever shipped.
const SHIPPED_MAN_PAGES: [&str; 3] = ["aterm.1", "aterm-ctl.1", "aterm-gui.1"];
/// The icon sizes install.sh writes beside the desktop entry.
const ICON_SIZES: [&str; 5] = ["32x32", "64x64", "128x128", "256x256", "512x512"];
/// The app icon at every size the repo ships ([`ICON_SIZES`]).
#[cfg(target_os = "linux")]
pub const HICOLOR_ICONS: [(&str, &[u8]); 5] = [
    (
        "32x32",
        include_bytes!("../../../assets/linux/icons/hicolor/32x32/apps/aterm.png"),
    ),
    (
        "64x64",
        include_bytes!("../../../assets/linux/icons/hicolor/64x64/apps/aterm.png"),
    ),
    (
        "128x128",
        include_bytes!("../../../assets/linux/icons/hicolor/128x128/apps/aterm.png"),
    ),
    (
        "256x256",
        include_bytes!("../../../assets/linux/icons/hicolor/256x256/apps/aterm.png"),
    ),
    (
        "512x512",
        include_bytes!("../../../assets/linux/icons/hicolor/512x512/apps/aterm.png"),
    ),
];
/// The man pages the repo ships ([`SHIPPED_MAN_PAGES`]).
const CARRIED_MAN_PAGES: [(&str, &str, &[u8]); 1] =
    [("1", "aterm.1", include_bytes!("../../../man/aterm.1"))];
/// An aterm bundle's identifier.
const BUNDLE_ID: &str = "com.aterm.aterm";
/// A hang detector on a child run to completion.
const CHILD_HANG: Duration = Duration::from_secs(60);

/// Where install.sh lays things for this account.
#[derive(Clone, Debug)]
pub struct Places {
    /// `$HOME`.
    pub home: PathBuf,
    /// `$XDG_DATA_HOME`, else `~/.local/share`.
    pub xdg_data: PathBuf,
    /// `$XDG_CONFIG_HOME`, else `~/.config`.
    pub xdg_config: PathBuf,
    /// `ATERM_INSTALL_DIR`: THE app location of a scoped run.
    pub install_dir: Option<PathBuf>,
    /// `ATERM_BIN_DIR`, else `~/.local/bin`.
    pub bin_dir: PathBuf,
    /// `ATERM_STORE_DIR`, else `~/.local/lib/aterm/bin`.
    pub store_dir: PathBuf,
    /// `ATERM_MAN_DIR`, else `~/.local/share/man`.
    pub man_root: PathBuf,
}

impl Places {
    /// install.sh's defaults under `home`.
    #[must_use]
    pub fn defaults(home: &Path, xdg_data: Option<&Path>, xdg_config: Option<&Path>) -> Self {
        Self {
            home: home.to_path_buf(),
            xdg_data: xdg_data.map_or_else(|| home.join(".local/share"), Path::to_path_buf),
            xdg_config: xdg_config.map_or_else(|| home.join(".config"), Path::to_path_buf),
            install_dir: None,
            bin_dir: home.join(".local/bin"),
            store_dir: home.join(".local/lib/aterm/bin"),
            man_root: home.join(".local/share/man"),
        }
    }

    /// The bin dirs the sweep walks: the configured one.
    fn bin_dirs(&self) -> Vec<PathBuf> {
        let local = self.home.join(".local/bin");
        let mut dirs = vec![self.bin_dir.clone()];
        if self.bin_dir != local {
            dirs.push(local);
        }
        dirs
    }

    /// The directories the app half installs `aterm.app` into: the scope alone, when set.
    fn app_dirs(&self) -> Vec<PathBuf> {
        self.install_dir.clone().map_or_else(
            || {
                vec![
                    PathBuf::from("/Applications"),
                    self.home.join("Applications"),
                ]
            },
            |dir| vec![dir],
        )
    }

    /// aterm's support directory, where atpkg keeps its store and its decline record.
    fn support(&self) -> PathBuf {
        self.home.join("Library/Application Support/aterm")
    }
}

/// What the shell side of the environment says.
#[derive(Clone, Debug, Default)]
pub struct ShellEnv {
    /// The account's login shell ([`crate::hooks::login_shell`], [`rc_target`]).
    pub shell: Option<String>,
    /// `$PATH`.
    pub path: Option<OsString>,
    /// `$MANPATH`.
    pub manpath: Option<String>,
}

impl ShellEnv {
    fn of_process() -> Self {
        Self {
            shell: crate::hooks::login_shell().map(|shell| shell.to_string_lossy().into_owned()),
            path: std::env::var_os("PATH"),
            manpath: std::env::var("MANPATH").ok(),
        }
    }

    /// The shell's name.
    fn shell_name(&self) -> &str {
        self.shell
            .as_deref()
            .map_or("", |s| s.rsplit('/').next().unwrap_or(s))
    }

    /// The first `name` on `$PATH` that is a file.
    fn on_path(&self, name: &str) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    }
}

/// The bundle's `CFBundleIdentifier`, read as `CFBundle` reads it, or `None`.
fn bundle_identifier(app: &Path) -> Option<String> {
    aterm_containment::consent::bundle_id_of(app, &[])
}

/// Whether `app` is an aterm bundle.
fn bundle_is_ours(app: &Path) -> bool {
    bundle_identifier(app).as_deref() == Some(BUNDLE_ID)
}

/// `path`'s symlink target, as written.
fn link_target(path: &Path) -> Option<String> {
    std::fs::read_link(path)
        .ok()
        .map(|t| t.to_string_lossy().into_owned())
}

/// Whether a bin entry is a symlink this installer places.
fn bin_entry_is_ours(path: &Path, store: &Path) -> bool {
    link_target(path).is_some_and(|t| {
        t.contains("/aterm.app/Contents/MacOS/")
            || Path::new(&t).starts_with(store) && Path::new(&t) != store
    })
}

/// The aterm bundle the symlink `link` leads into, or `None`.
fn link_bundle(link: &Path) -> Option<PathBuf> {
    let t = link_target(link)?;
    let t = if t.starts_with('/') {
        t
    } else {
        format!("{}/{t}", link.parent()?.display())
    };
    let at = t.find("/aterm.app/Contents/MacOS/")?;
    Some(PathBuf::from(format!("{}/aterm.app", &t[..at])))
}

/// Whether `a` and `b` are one file (device and inode).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// `path` as the deepest directory on it that exists.
fn dir_split(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    loop {
        let mut head = PathBuf::from("/");
        let mut comps = path.components().peekable();
        while let Some(c) = comps.peek() {
            let next = match c {
                Component::RootDir | Component::Prefix(_) => {
                    comps.next();
                    continue;
                }
                Component::CurDir => {
                    comps.next();
                    continue;
                }
                other => head.join(other.as_os_str()),
            };
            if !next.is_dir() {
                break;
            }
            head = next;
            comps.next();
        }
        let head = std::fs::canonicalize(&head).ok()?;
        let mut tail: Vec<OsString> = Vec::new();
        let mut undid = false;
        let mut base = head.clone();
        for c in comps {
            match c {
                Component::Normal(n) => tail.push(n.to_os_string()),
                Component::ParentDir => {
                    undid = true;
                    if tail.pop().is_none() {
                        base = base
                            .parent()
                            .map_or_else(|| base.clone(), Path::to_path_buf);
                    }
                }
                Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            }
        }
        if undid {
            path = tail.iter().fold(base, |p, t| p.join(t));
            continue;
        }
        return Some((head, tail.iter().collect()));
    }
}

/// Whether directory `a` lies within directory `b`, by identity.
fn dir_within(a: &Path, b: &Path) -> bool {
    let (Some((mut ah, at)), Some((bh, bt))) = (dir_split(a), dir_split(b)) else {
        return false;
    };
    if bt.as_os_str().is_empty() {
        loop {
            if same_file(&ah, &bh) {
                return true;
            }
            match ah.parent() {
                Some(p) => ah = p.to_path_buf(),
                None => return false,
            }
        }
    }
    same_file(&ah, &bh) && at.starts_with(&bt)
}

/// The first line `exe args…` prints within `within`, or nothing.
fn first_line_bounded(exe: &Path, args: &[impl AsRef<OsStr>], within: Duration) -> String {
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.args(args);
    aterm_uds::deadline::output_until(&mut cmd, Instant::now() + within)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default()
}

/// Whether the regular executable file at `path` answers `--version` as aterm, asked as `run_as`
/// says ([`runs_as`]).
fn answers_as_aterm(path: &Path, run_as: RunAs) -> bool {
    let line = match run_as {
        _ if !is_executable_file(path) => return false,
        RunAs::Me => first_line_bounded(path, &["--version"], CHILD_HANG),
        RunAs::Account(uid) => first_line_bounded(
            Path::new(SUDO),
            &as_account_argv(uid, path, &["--version"]),
            CHILD_HANG,
        ),
        RunAs::Nobody => return false,
    };
    line.starts_with("aterm ")
}

/// Whether `path` itself (not a link's target) is a regular file someone may execute.
fn is_executable_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Who a sweep runs a program it found as ([`runs_as`]), rather than one it was started as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunAs {
    /// This process.
    Me,
    /// The home's owner, through [`SUDO`].
    Account(u32),
    /// None: root runs nothing another account could have put in its place, nor anything
    /// that would write root's files into a home not its own.
    Nobody,
}

/// What a skip says of a program [`RunAs::Nobody`] leaves unrun.
const NOT_AS_ROOT: &str = "root runs a program only as the account whose own home HOME is, or \
                           for its own HOME one no account can replace (run the uninstall as \
                           that account)";

/// [`RunAs`] for `program`, found by a sweep of `home` run as `our_uid`. Root asks as the home's
/// owner when `home` is that owner's own ([`caffeine_account`]; sudo keeps the person's HOME).
/// Otherwise it runs a program itself only for its own home, where what the program makes is
/// root's to make (in another, a root-owned store or log directory would refuse that home's
/// account later), and only one it already runs ([`ours_already`]) or no account can replace
/// ([`roots_alone`]).
fn runs_as(our_uid: u32, home: &Path, program: &Path) -> RunAs {
    if our_uid != 0 {
        return RunAs::Me;
    }
    if let Some((uid, _)) = caffeine_account(our_uid, home) {
        return RunAs::Account(uid);
    }
    let roots_home = aterm_uds::account::home_of(0).is_some_and(|own| same_file(&own, home));
    if roots_home && (ours_already(program) || roots_alone(program)) {
        RunAs::Me
    } else {
        RunAs::Nobody
    }
}

/// Whether `program` is this process's own executable, or in the bundle it runs from, spelled
/// as it resolves: a link on the way is one its owner may repoint before the exec reads the
/// path again.
fn ours_already(program: &Path) -> bool {
    let (Ok(exe), Ok(resolved)) = (
        std::env::current_exe().and_then(std::fs::canonicalize),
        std::fs::canonicalize(program),
    ) else {
        return false;
    };
    if resolved != program {
        return false;
    }
    let bundle = exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"));
    resolved == exe || bundle.is_some_and(|bundle| resolved.starts_with(bundle))
}

/// Whether no account but root can replace `program`: it and every directory above it, as
/// spelled and as its links resolve, are root's, and no group or other may write one (but a
/// sticky directory, where only an entry's owner may replace it).
fn roots_alone(program: &Path) -> bool {
    let resolved = std::fs::canonicalize(program).ok();
    [Some(program.to_path_buf()), resolved]
        .iter()
        .flatten()
        .all(|path| {
            path.ancestors()
                .filter(|at| !at.as_os_str().is_empty())
                .all(|at| {
                    std::fs::symlink_metadata(at).is_ok_and(|m| {
                        let kind = m.file_type();
                        root_only_entry(
                            m.uid(),
                            m.gid(),
                            m.mode(),
                            kind.is_dir(),
                            kind.is_symlink(),
                        )
                    })
                })
        })
}

/// Whether an entry with this owner, group and mode is one no account but root can change, for
/// [`roots_alone`]: root's, and writable by no group but root's and by no other, unless it is
/// a sticky directory (only an entry's owner may replace that entry). A link's own mode grants
/// nothing (Linux reports 0777 for every one); its parent directory, the next ancestor, decides
/// who may replace it.
const fn root_only_entry(uid: u32, gid: u32, mode: u32, dir: bool, link: bool) -> bool {
    if link {
        return uid == 0;
    }
    let sticky = dir && mode & 0o1000 != 0;
    uid == 0 && (sticky || mode & 0o002 == 0) && (sticky || gid == 0 || mode & 0o020 == 0)
}

/// sudo's exit status when it will not run a command as `uid` for root ([`TRUE`]), `None` when
/// it will.
fn sudo_refuses(uid: u32) -> Option<i32> {
    let rc = run_quietly(Path::new(SUDO), &as_account_argv(uid, Path::new(TRUE), &[]));
    (rc != 0).then_some(rc)
}

/// Whether an entry of the store directory is one the store lanes place (its `aterm` asked
/// as `run_as` says).
fn store_entry_is_ours(path: &Path, run_as: RunAs) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let meta = std::fs::symlink_metadata(path);
    match name {
        "aterm" => answers_as_aterm(path, run_as),
        ".aterm-update" => meta.is_ok_and(|m| m.is_dir()),
        n if n.starts_with(".aterm-install.") => meta.is_ok_and(|m| m.is_file()),
        _ => link_target(path).as_deref() == Some("aterm"),
    }
}

/// Remove `path` as `rm -rf` does: a symlink itself, a directory with all it holds.
fn remove(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Whether anything (a dangling symlink too) is at `path`.
fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The sweep's tally and its two channels.
struct Sweep<'w> {
    dry: bool,
    removed: usize,
    skipped: usize,
    kept: usize,
    shared_kept: usize,
    outsider: Option<PathBuf>,
    out: &'w mut dyn Write,
    err: &'w mut dyn Write,
}

impl Sweep<'_> {
    const fn act(&self) -> &'static str {
        if self.dry { "would remove" } else { "remove" }
    }

    fn say(&mut self, line: &str) {
        let _ = writeln!(self.out, "atpkg: {line}");
    }

    /// Remove what is ours, or say what a real run would.
    fn rm(&mut self, path: &Path, label: &str) {
        if self.dry {
            let act = self.act();
            self.say(&format!("{act} {label}: {}", path.display()));
        } else {
            match remove(path) {
                Ok(()) => self.say(&format!("removed {label}: {}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.say(&format!("gone already: {label}: {}", path.display()));
                    return;
                }
                Err(_) => {
                    let _ = writeln!(
                        self.err,
                        "atpkg: SKIPPED {label} (cannot remove, try sudo): {}",
                        path.display()
                    );
                    self.skipped += 1;
                    return;
                }
            }
        }
        self.removed += 1;
    }

    /// The one SKIPPED line for what is not ours.
    fn skip(&mut self, path: &str, reason: &str) {
        let _ = writeln!(self.err, "atpkg: SKIPPED {path} — {reason}");
        self.skipped += 1;
    }

    /// What is laid for the whole account.
    fn rm_shared(&mut self, path: &Path, label: &str) {
        if self.outsider.is_some() {
            self.say(&format!("left in place: {label}: {}", path.display()));
            self.kept += 1;
            self.shared_kept += 1;
        } else {
            self.rm(path, label);
        }
    }
}

/// Whether `bundle` is outside a scoped run's scope.
fn outside_scope(p: &Places, bundle: &Path) -> bool {
    let Some(scope) = &p.install_dir else {
        return false;
    };
    let entry = bundle.file_name().is_some_and(|n| n == "aterm.app")
        && bundle
            .parent()
            .is_some_and(|dir| dir_within(dir, scope) && dir_within(scope, dir));
    !entry
}

/// Whether `bundle` is another aterm than the scope's on this account.
fn other_aterm(p: &Places, bundle: &Path) -> bool {
    bundle.is_dir() && outside_scope(p, bundle) && bundle_is_ours(bundle)
}

/// The first aterm.app other than the scope's that runs on this HOME's toolchain store, or `None`.
fn scope_outsider(p: &Places) -> Option<PathBuf> {
    let mut cands = vec![p.home.join("Applications/aterm.app")];
    if home_is_the_accounts(aterm_uds::peer::our_uid(), &p.home) {
        cands.push(PathBuf::from("/Applications/aterm.app"));
    }
    for bd in p.bin_dirs() {
        for name in ["aterm", "atpkg"] {
            cands.extend(link_bundle(&bd.join(name)));
        }
    }
    cands.into_iter().find(|b| other_aterm(p, b))
}

/// The bundle whose helper step 0 may delegate to.
fn uninstall_bundle(p: &Places) -> Option<PathBuf> {
    p.app_dirs()
        .iter()
        .map(|d| d.join("aterm.app"))
        .find(|app| {
            bundle_is_ours(app)
                && crate::toolchain::is_executable_file(&app.join("Contents/MacOS/atpkg"))
        })
}

/// Where the Caffeine mug keeps the copies of the app it runs from: `<label>/<id>/X.app`
/// ([`aterm_uds::deadline::RAN`], [`sweep_caffeine_code`]).
const CAFFEINE_CODE: &str = "caffeine-code.noindex";

/// What `aterm caffeine` keeps in the support dir.
const CAFFEINE_FILES: [&str; 5] = [
    CAFFEINE_CODE,
    "caffeine",
    "caffeine.state",
    "caffeine.lock",
    "caffeine.bridge",
];

/// The line a self-uninstall adds to the toolset decline its own sweep recorded but left for an
/// aterm it kept.
const SWEPT_DECLINE_TAG: &str = "# Recorded by aterm pkg self-uninstall for an aterm it kept.";

/// The prefix of every launchd label `aterm caffeine` makes, its login plists' names.
const CAFFEINE_LABEL_PREFIX: &str = "com.aterm.aterm.caffeine";

/// Retire the going builds' startup jobs and the running mug through their own command.
fn sweep_caffeine(s: &mut Sweep<'_>, p: &Places, app_dirs: &[PathBuf]) -> Vec<PathBuf> {
    sweep_caffeine_with(
        s,
        p,
        app_dirs,
        aterm_uds::peer::our_uid(),
        || caffeine_account(aterm_uds::peer::our_uid(), &p.home),
        run_quietly,
    )
}

fn sweep_caffeine_with(
    s: &mut Sweep<'_>,
    p: &Places,
    app_dirs: &[PathBuf],
    our_uid: u32,
    account: impl FnOnce() -> Option<(u32, u32)>,
    mut run: impl FnMut(&Path, &[OsString]) -> i32,
) -> Vec<PathBuf> {
    let support = p.support();
    let listed = || -> Vec<PathBuf> {
        CAFFEINE_FILES
            .iter()
            .map(|name| support.join(name))
            .filter(|path| present(path))
            .collect()
    };
    let files = listed();
    let login_plist = sorted_entries(&p.home.join("Library/LaunchAgents"))
        .iter()
        .filter_map(|plist| plist.file_name().and_then(|n| n.to_str()))
        .any(|name| name.starts_with(CAFFEINE_LABEL_PREFIX) && name.ends_with(".plist"));
    if files.is_empty() && !login_plist {
        return Vec::new();
    }
    let account = account();
    let own_quit: &[&str] = &["caffeine", "quit", "own"];
    let steps: &[&[&str]] = if s.outsider.is_some() {
        &[&["caffeine", "startup", "off"], own_quit]
    } else {
        &[&["caffeine", "startup", "off"], &["caffeine", "quit"]]
    };
    let mut helpers: Vec<(PathBuf, PathBuf)> = app_dirs
        .iter()
        .map(|dir| dir.join("aterm.app"))
        .filter_map(|app| {
            let aterm = app.join("Contents/MacOS/aterm");
            (bundle_is_ours(&app) && crate::toolchain::is_executable_file(&aterm))
                .then_some((app, aterm))
        })
        .collect();
    let stored = p.store_dir.join("aterm");
    if store_left(p).is_none() && is_executable_file(&stored) {
        helpers.push((stored.clone(), stored.clone()));
    }
    let run_as = |aterm: &Path| {
        account.map_or_else(
            || runs_as(our_uid, &p.home, aterm),
            |(uid, _)| RunAs::Account(uid),
        )
    };
    let mut supported = false;
    let mut failed = Vec::new();
    // Root with no account to ask as runs a helper only as [`runs_as`] lets it, and keeps
    // every other for its steps.
    helpers.retain(|(app, aterm)| {
        if run_as(aterm) != RunAs::Nobody {
            return true;
        }
        s.skip(
            &helper_line(aterm, &["help", "caffeine"], account),
            &format!("{NOT_AS_ROOT}; {} is kept for it", app.display()),
        );
        failed.push(app.clone());
        false
    });
    if let Some((uid, _)) = account
        && !s.dry
        && !helpers.is_empty()
    {
        let rc = run(Path::new(SUDO), &as_account_argv(uid, Path::new(TRUE), &[]));
        if rc != 0 {
            for (app, aterm) in std::mem::take(&mut helpers) {
                s.skip(
                    &helper_line(&aterm, &["help", "caffeine"], account),
                    &format!(
                        "sudo could not run it as uid {uid} (exit {rc}); {} is kept for it",
                        app.display()
                    ),
                );
                failed.push(app);
            }
        }
    }
    // A file at the store's name is a helper once it answers as aterm, asked as above.
    helpers.retain(|(_, aterm)| *aterm != stored || answers_as_aterm(aterm, run_as(aterm)));
    for (app, aterm) in helpers {
        if s.dry {
            s.say(&format!(
                "would ask {}, then if supported:",
                helper_line(&aterm, &["help", "caffeine"], account)
            ));
            for args in steps {
                s.say(&format!("would run {}", helper_line(&aterm, args, account)));
            }
            continue;
        }
        let rc = run_helper(&aterm, &["help", "caffeine"], account, &mut run);
        if rc != 0 {
            if account.is_some() {
                s.skip(
                    &helper_line(&aterm, &["help", "caffeine"], account),
                    &format!(
                        "could not confirm the helper ran as its account (exit {rc}); {} is kept for it",
                        app.display()
                    ),
                );
                failed.push(app);
            }
            continue;
        }
        supported = true;
        for args in steps {
            let line = helper_line(&aterm, args, account);
            match run_helper(&aterm, args, account, &mut run) {
                0 => s.say(&format!("ran {line}")),
                2 if *args == own_quit => s.say(&format!(
                    "{line}: this build cannot quit only its own mug; one it runs keeps running"
                )),
                rc => {
                    if !failed.contains(&app) {
                        failed.push(app.clone());
                    }
                    s.skip(
                        &line,
                        &format!(
                            "it failed (exit {rc}) — run it by hand to see why; {} is kept \
                             for it",
                            app.display()
                        ),
                    );
                }
            }
        }
    }
    if s.outsider.is_some() {
        for path in files {
            s.rm_shared(&path, "Caffeine file");
        }
        return failed;
    }
    if s.dry {
        s.say("Caffeine file removal requires an acquired idle lock; the lock itself stays");
        return failed;
    }
    let files = listed();
    if files.is_empty() {
        return failed;
    }
    let lock = support.join("caffeine.lock");
    // The lock alone, with no login plist, needs no aterm to stop a mug: the hold below finds
    // the lock free (no mug, so the rendezvous stays as after any sweep) or held (a mug, kept).
    // Not for root with no account, which holds no lock in another account's directory.
    let lone_lock = (our_uid != 0 || account.is_some())
        && !login_plist
        && files.iter().all(|path| *path == lock);
    if (!supported && !lone_lock) || !failed.is_empty() {
        s.skip(
            &support.display().to_string(),
            "Caffeine stop was not confirmed; its files are kept",
        );
        return failed;
    }
    let Some(_hold) = caffeine_cleanup_hold(&lock, account, make_as_account) else {
        s.skip(
            &lock.display().to_string(),
            "Caffeine lock is busy or could not be acquired; its files are kept",
        );
        return failed;
    };
    let files = listed();
    for path in files {
        if path == lock {
            s.say(&format!(
                "left Caffeine lock rendezvous: {}",
                path.display()
            ));
            s.kept += 1;
        } else if path == support.join(CAFFEINE_CODE) {
            sweep_caffeine_code(s, &path, std::time::SystemTime::now());
        } else {
            s.rm_shared(&path, "Caffeine file");
        }
    }
    failed
}

/// Remove the mug's code directory `code` at `now` as the mug's own sweep does: all of it but a
/// copy no mug said it ran from that is younger than the first-exec hang
/// ([`aterm_uds::deadline::copy_may_go`]).
fn sweep_caffeine_code(s: &mut Sweep<'_>, code: &Path, now: std::time::SystemTime) {
    sweep_caffeine_code_with(
        s,
        code,
        now,
        &|path| std::fs::symlink_metadata(path),
        &caffeine_code_entries,
    );
}

#[derive(Default)]
struct CaffeineCodePlan {
    remove: Vec<(PathBuf, &'static str)>,
    keep: Vec<(PathBuf, Duration)>,
}

/// Admit the complete tree before removing any of it.
fn caffeine_code_plan_with(
    code: &Path,
    now: std::time::SystemTime,
    metadata: &impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
    entries: &impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) -> std::io::Result<CaffeineCodePlan> {
    let at = |path: &Path, error: std::io::Error| {
        std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
    };
    let inspect = |path: &Path| -> std::io::Result<Option<std::fs::Metadata>> {
        match metadata(path) {
            Ok(meta) => Ok(Some(meta)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(at(path, error)),
        }
    };
    let list = |path: &Path| entries(path).map_err(|error| at(path, error));
    let mut plan = CaffeineCodePlan::default();
    let Some(root) = inspect(code)? else {
        return Ok(plan);
    };
    if !root.is_dir() {
        plan.remove.push((code.to_path_buf(), "Caffeine file"));
        return Ok(plan);
    }
    let partial = |path: &Path| path.extension().is_some_and(|e| e == "partial");
    for label in list(code)? {
        let Some(meta) = inspect(&label)? else {
            continue;
        };
        if !meta.is_dir() || partial(&label) {
            plan.remove.push((label, "Caffeine file"));
            continue;
        }
        let mut copies = Vec::new();
        let mut kept = Vec::new();
        for copy in list(&label)? {
            let Some(meta) = inspect(&copy)? else {
                continue;
            };
            let left = (meta.is_dir() && !partial(&copy))
                .then(|| aterm_uds::deadline::copy_wait_left(&copy, now))
                .flatten();
            if let Some(left) = left {
                kept.push((copy, left));
            } else {
                copies.push((copy, "Caffeine copy"));
            }
        }
        if kept.is_empty() {
            plan.remove.push((label, "Caffeine file"));
        } else {
            plan.remove.extend(copies);
            plan.keep.extend(kept);
        }
    }
    if plan.keep.is_empty() {
        plan.remove.clear();
        plan.remove.push((code.to_path_buf(), "Caffeine file"));
    }
    Ok(plan)
}

fn caffeine_code_entries(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    collect_caffeine_code_entries(std::fs::read_dir(dir)?.map(|entry| entry.map(|e| e.path())))
}

fn collect_caffeine_code_entries(
    entries: impl IntoIterator<Item = std::io::Result<PathBuf>>,
) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = entries.into_iter().collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_by_key(|path| {
        let name = path.file_name().unwrap_or_default();
        (name.to_string_lossy().starts_with('.'), name.to_os_string())
    });
    Ok(paths)
}

fn sweep_caffeine_code_with(
    s: &mut Sweep<'_>,
    code: &Path,
    now: std::time::SystemTime,
    metadata: &impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
    entries: &impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) {
    let plan = match caffeine_code_plan_with(code, now, metadata, entries) {
        Ok(plan) => plan,
        Err(error) => {
            s.skip(
                &code.display().to_string(),
                &format!("cannot inspect every Caffeine copy; all kept: {error}"),
            );
            return;
        }
    };
    for (path, label) in plan.remove {
        if path == code {
            s.rm_shared(&path, label);
        } else {
            s.rm(&path, label);
        }
    }
    for (path, left) in plan.keep {
        s.skip(
            &path.display().to_string(),
            &format!(
                "no Caffeine mug said it ran from this copy, so macOS may still be assessing \
                 its first exec, and removing it now can crash syspolicyd; remove it by hand \
                 once the Mac has been awake {} more minute(s)",
                left.as_secs().div_ceil(60).max(1)
            ),
        );
    }
}

/// Hold the physical.
fn caffeine_cleanup_hold(
    path: &Path,
    account: Option<(u32, u32)>,
    make: fn(&Path, u32) -> bool,
) -> Option<crate::lock::Flock> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let owner = account.map_or_else(aterm_uds::peer::our_uid, |(uid, _)| uid);
    let made = account.is_some()
        && std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    if made && !make(path, owner) {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(account.is_none() && aterm_uds::peer::our_uid() != 0)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || opened.uid() != owner || opened.nlink() != 1 {
        return None;
    }
    if made && opened.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .ok()?;
    }
    let held = crate::lock::Flock::try_lock(file).ok()?;
    let named = std::fs::symlink_metadata(path).ok()?;
    if !named.is_file() || (named.dev(), named.ino()) != (opened.dev(), opened.ino()) {
        return None;
    }
    Some(held)
}

/// The account a sweep of `home` asks the Caffeine helpers as, with its group.
fn caffeine_account(our_uid: u32, home: &Path) -> Option<(u32, u32)> {
    if our_uid != 0 {
        return None;
    }
    let owner = std::fs::metadata(home).ok()?;
    let own = aterm_uds::account::home_of(owner.uid())?;
    (owner.uid() != 0 && same_file(&own, home)).then(|| (owner.uid(), owner.gid()))
}

/// Whether `home` is an account's own home, so `/Applications` serves it: this process's
/// account's, or under root (sudo keeps the person's HOME) its owner's ([`caffeine_account`]).
fn home_is_the_accounts(our_uid: u32, home: &Path) -> bool {
    caffeine_account(our_uid, home).is_some()
        || aterm_uds::account::home_of(our_uid).is_some_and(|acct| same_file(&acct, home))
}

/// What runs a Caffeine helper step as `uid`.
const SUDO: &str = "/usr/bin/sudo";
/// What a root sweep asks sudo to run first, to tell sudo's refusal from a helper's.
const TRUE: &str = "/usr/bin/true";
/// What makes a missing Caffeine lock as its account ([`make_as_account`]).
const TOUCH: &str = "/usr/bin/touch";

/// Make the empty file `path` as `uid`, through [`SUDO`] ([`caffeine_cleanup_hold`]).
fn make_as_account(path: &Path, uid: u32) -> bool {
    let mut argv = as_account_argv(uid, Path::new(TOUCH), &[]);
    argv.push(path.into());
    run_quietly(Path::new(SUDO), &argv) == 0
}

/// `sudo`'s arguments for `aterm args` as `uid` ([`SUDO`]).
fn as_account_argv(uid: u32, aterm: &Path, args: &[&str]) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["-n", "-H", "-u"].into_iter().map(OsString::from).collect();
    argv.push(format!("#{uid}").into());
    argv.push("--".into());
    argv.push(aterm.into());
    argv.extend(args.iter().map(OsString::from));
    argv
}

/// Run the Caffeine helper `aterm args` quietly ([`caffeine_account`]).
fn run_helper(
    aterm: &Path,
    args: &[&str],
    account: Option<(u32, u32)>,
    run: &mut impl FnMut(&Path, &[OsString]) -> i32,
) -> i32 {
    match account {
        None => run(aterm, &args.iter().map(OsString::from).collect::<Vec<_>>()),
        Some((uid, _)) => run(Path::new(SUDO), &as_account_argv(uid, aterm, args)),
    }
}

/// How the sweep names the helper step [`run_helper`] runs.
fn helper_line(aterm: &Path, args: &[&str], account: Option<(u32, u32)>) -> String {
    let step = format!("{} {}", aterm.display(), args.join(" "));
    match account {
        None => step,
        Some((uid, _)) => format!("{SUDO} -n -H -u #{uid} -- {step}"),
    }
}

/// Run `program args`, its stdio `/dev/null`.
fn run_quietly(program: &Path, args: &[impl AsRef<OsStr>]) -> i32 {
    aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .status()
        .map_or(127, |st| st.code().unwrap_or(128))
}

/// Everything install.sh placed, swept.
pub fn self_uninstall(
    p: &Places,
    man_src: Option<&Path>,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    self_uninstall_with_caffeine(p, man_src, dry, out, err, sweep_caffeine)
}

fn self_uninstall_with_caffeine(
    p: &Places,
    man_src: Option<&Path>,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
    mut caffeine: impl FnMut(&mut Sweep<'_>, &Places, &[PathBuf]) -> Vec<PathBuf>,
) -> bool {
    let outsider = p.install_dir.as_ref().and_then(|_| scope_outsider(p));
    let mut s = Sweep {
        dry,
        removed: 0,
        skipped: 0,
        kept: 0,
        shared_kept: 0,
        outsider,
        out,
        err,
    };
    let support = p.support();
    let decline = support.join("pkg/declined");
    let decline_tagged = std::fs::read_to_string(&decline)
        .is_ok_and(|text| text.lines().any(|line| line == SWEPT_DECLINE_TAG));
    let decline_before = present(&decline) && !decline_tagged;
    let app_dirs = p.app_dirs();

    let atpkg_app = uninstall_bundle(p);
    let mut swept = false;
    let mut atpkg_kept: Option<PathBuf> = None;
    match (&atpkg_app, s.outsider.is_some()) {
        (Some(_), true) => {
            s.say("left in place: the ALab toolchain store");
            s.kept += 1;
            s.shared_kept += 1;
        }
        (Some(app), false) => {
            let atpkg = app.join("Contents/MacOS/atpkg");
            if dry {
                let act = s.act();
                s.say(&format!(
                    "{act} the ALab toolchain store: {} uninstall --all",
                    atpkg.display()
                ));
                s.removed += 1;
                swept = true;
            } else {
                // Root runs the account's atpkg as the account, and none for another's home
                // or one an account could replace ([`runs_as`]).
                let run_as = runs_as(aterm_uds::peer::our_uid(), &p.home, &atpkg);
                let refused = match run_as {
                    RunAs::Me => None,
                    RunAs::Account(uid) => sudo_refuses(uid)
                        .map(|rc| format!("sudo could not run it as uid {uid} (exit {rc})")),
                    RunAs::Nobody => Some(NOT_AS_ROOT.to_string()),
                };
                let mut cmd = match run_as {
                    RunAs::Account(uid) => {
                        let mut cmd = aterm_uds::spawnfd::command(Path::new(SUDO));
                        cmd.args(as_account_argv(uid, &atpkg, &["uninstall", "--all"]));
                        cmd
                    }
                    RunAs::Me | RunAs::Nobody => {
                        let mut cmd = aterm_uds::spawnfd::command(&atpkg);
                        cmd.args(["uninstall", "--all"]);
                        cmd
                    }
                };
                let ran = match refused {
                    Some(why) => Err(why),
                    None => Ok(cmd
                        .stdin(aterm_uds::spawnfd::Stdio::null())
                        .stdout(aterm_uds::spawnfd::Stdio::null())
                        .stderr(aterm_uds::spawnfd::Stdio::null())
                        .status()
                        .map_or(127, |st| {
                            st.code().unwrap_or_else(|| {
                                128 + std::os::unix::process::ExitStatusExt::signal(&st)
                                    .unwrap_or(0)
                            })
                        })),
                };
                if ran == Ok(0) {
                    s.say("removed the ALab toolchain store");
                    swept = true;
                    s.removed += 1;
                } else if let Ok(rc @ 126..) = ran {
                    s.skip(
                        "the ALab toolchain store",
                        &format!(
                            "`{} uninstall --all` could not run (exit {rc}) — reinstall aterm, \
                             then uninstall again",
                            atpkg.display()
                        ),
                    );
                } else {
                    let why = match ran {
                        Err(why) => why,
                        Ok(rc) if rc == i32::from(crate::lock::CONTENDED_EXIT) => {
                            "another aterm pkg run holds the store — quit aterm, or let its \
                             update pass finish"
                                .to_string()
                        }
                        Ok(rc) => format!("it failed (exit {rc}) — run it by hand to see why"),
                    };
                    s.skip(
                        "the ALab toolchain store",
                        &format!(
                            "`{} uninstall --all`: {why}; {} is kept for it — then uninstall \
                             again",
                            atpkg.display(),
                            app.display()
                        ),
                    );
                    atpkg_kept = Some(app.clone());
                }
            }
        }
        (None, _) => {}
    }

    let caffeine_kept = caffeine(&mut s, p, &app_dirs);

    for dir in &app_dirs {
        for name in ["aterm.app", "aterm.app.rollback"] {
            let app = dir.join(name);
            if !app.is_dir() {
                continue;
            }
            if atpkg_kept.as_ref() == Some(&app) {
                s.skip(
                    &app.display().to_string(),
                    "kept: its atpkg is what removes the ALab toolchain store, and that removal \
                     did not finish (above)",
                );
            } else if caffeine_kept.contains(&app) {
                s.skip(
                    &app.display().to_string(),
                    "kept: its `aterm caffeine` is what stops the Caffeine mug, and that did \
                     not finish (above)",
                );
            } else if bundle_is_ours(&app) {
                s.rm(&app, "app bundle");
            } else {
                let id = bundle_identifier(&app).unwrap_or_else(|| "unreadable".to_string());
                s.skip(
                    &app.display().to_string(),
                    &format!("not an aterm bundle (CFBundleIdentifier={id})"),
                );
            }
        }
    }

    if (swept || decline_tagged) && !dry && !decline_before && decline.is_file() {
        let app_left = app_dirs.iter().any(|d| {
            let app = d.join("aterm.app");
            app.is_dir() && bundle_is_ours(&app)
        });
        if !app_left {
            let _ = std::fs::remove_file(&decline);
        } else if !decline_tagged {
            let tagged = std::fs::OpenOptions::new()
                .append(true)
                .open(&decline)
                .and_then(|mut file| writeln!(file, "{SWEPT_DECLINE_TAG}"));
            if let Err(e) = tagged {
                s.say(&format!(
                    "could not mark {} as this uninstall's: {e}",
                    decline.display()
                ));
            }
        }
    }

    for bd in p.bin_dirs() {
        let atpkg_link = bd.join("atpkg");
        if let Some(t) = link_target(&atpkg_link) {
            if t.contains("/aterm.app/Contents/MacOS/") {
                rm_link(&mut s, p, &atpkg_link, "atpkg command");
            } else {
                s.skip(
                    &atpkg_link.display().to_string(),
                    "symlink points outside an aterm bundle",
                );
            }
        }
        let bin = bd.join("aterm");
        if bin_entry_is_ours(&bin, &p.store_dir) {
            rm_link(&mut s, p, &bin, "aterm command");
        } else if let Some(t) = link_target(&bin) {
            s.skip(
                &bin.display().to_string(),
                &format!("symlink points outside an aterm bundle or store ({t})"),
            );
        } else if present(&bin) {
            s.skip(
                &bin.display().to_string(),
                "not a symlink — a hand-built binary this installer did not place",
            );
        }
    }

    sweep_store(&mut s, p, &caffeine_kept);

    for section in 1..=9 {
        let dir = p.man_root.join(format!("man{section}"));
        for page in sorted_entries(&dir) {
            let Some(base) = page
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let man_page = base.starts_with("aterm")
                && base
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| matches!(ext.as_bytes(), [b'1'..=b'9']));
            if !man_page || !page.exists() {
                continue;
            }
            if SHIPPED_MAN_PAGES.contains(&base.as_str()) {
                s.rm_shared(&page, "man page");
            } else if !man_src.is_some_and(Path::is_dir) {
                s.skip(
                    &page.display().to_string(),
                    "not a man page this installer ships (run from a checkout to sweep newer \
                     pages)",
                );
            } else if !man_src.is_some_and(|src| src.join(&base).exists()) {
                s.skip(
                    &page.display().to_string(),
                    "not a man page this repo ships",
                );
            } else {
                s.rm_shared(&page, "man page");
            }
        }
    }

    for comp in completion_files(p) {
        if comp.exists() {
            s.rm_shared(&comp, "shell completion");
        }
    }

    let desktop = p.xdg_data.join("applications/aterm.desktop");
    let mut desktop_owned = false;
    let desktop_is_link =
        std::fs::symlink_metadata(&desktop).is_ok_and(|m| m.file_type().is_symlink());
    if desktop.is_file() || desktop_is_link {
        let owned = std::fs::symlink_metadata(&desktop).is_ok_and(|m| m.is_file())
            && std::fs::read_to_string(&desktop)
                .is_ok_and(|text| text.lines().any(|l| l == DESKTOP_MARKER));
        if owned {
            desktop_owned = true;
            s.rm(&desktop, "desktop entry");
            if !dry {
                refresh_desktop_database(&ShellEnv::of_process(), &p.xdg_data, |update| {
                    runs_as(aterm_uds::peer::our_uid(), &p.home, update)
                });
            }
        } else {
            s.skip(
                &desktop.display().to_string(),
                "not a desktop entry this installer wrote (missing its marker line)",
            );
        }
    }
    for size in ICON_SIZES {
        let icon = p
            .xdg_data
            .join(format!("icons/hicolor/{size}/apps/aterm.png"));
        if !icon.exists() {
            continue;
        }
        if desktop_owned {
            s.rm(&icon, "app icon");
        } else {
            s.skip(
                &icon.display().to_string(),
                "no marker-owned aterm.desktop vouches for it",
            );
        }
    }

    if support.is_dir() {
        if dry {
            if std::fs::read_dir(&support).is_ok_and(|mut rd| rd.next().is_none()) {
                let act = s.act();
                s.say(&format!("{act} empty support dir: {}", support.display()));
                s.removed += 1;
            }
        } else if std::fs::remove_dir(&support).is_ok() {
            s.say(&format!("removed empty support dir: {}", support.display()));
            s.removed += 1;
        }
    }

    sweep_path_blocks(&mut s, p);

    if let Some(other) = s.outsider.clone()
        && s.shared_kept > 0
    {
        let n = s.shared_kept;
        s.say(&format!(
            "left in place: {n} account-wide item(s) above — {}, another aterm on this \
             account, uses them",
            other.display()
        ));
    }
    if s.removed == 0 && s.skipped == 0 && s.kept == 0 {
        s.say("nothing installed by install.sh was found — nothing to do");
        return true;
    }
    let verb = if dry { "dry run" } else { "uninstall" };
    let tail = if s.skipped > 0 {
        format!(", {} skipped", s.skipped)
    } else {
        String::new()
    };
    let removed = s.removed;
    s.say(&format!("{verb} complete — {removed} item(s){tail}"));
    let settings = p.xdg_config.join("aterm");
    if swept || (atpkg_app.is_some() && s.outsider.is_some()) {
        s.say(&format!(
            "left in place: your settings and themes in {}",
            settings.display()
        ));
    } else {
        s.say(&format!(
            "left in place: your settings and themes in {}, and any atpkg toolchain",
            settings.display()
        ));
    }
    if decline.is_file() {
        s.say(&format!(
            "left in place: the ALab toolset decline ({}) — installing aterm again installs \
             no toolset until `aterm pkg install --default-set`",
            decline.display()
        ));
    }
    s.skipped == 0
}

/// Remove a command link.
fn rm_link(s: &mut Sweep<'_>, p: &Places, link: &Path, label: &str) {
    match link_bundle(link) {
        Some(b) if other_aterm(p, &b) => {
            s.say(&format!(
                "left in place: {} — it leads into {}, another aterm than ATERM_INSTALL_DIR's",
                link.display(),
                b.display()
            ));
            s.kept += 1;
        }
        _ => s.rm(link, label),
    }
}

/// The entries of `dir`, sorted, the dot-named ones after the rest (a shell glob's order).
fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = rd.flatten().map(|e| e.file_name()).collect();
    names.sort_by_key(|n| (n.to_string_lossy().starts_with('.'), n.clone()));
    names.into_iter().map(|n| dir.join(n)).collect()
}

/// Why step 3 leaves the store as it is, when it does ([`sweep_store`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreLeft {
    /// No directory is there.
    Absent,
    /// It is a PATH directory, not a store of its own.
    PathDir,
    /// It cannot be listed.
    Unlisted,
}

/// Whether step 3 leaves the store as it is ([`StoreLeft`]).
fn store_left(p: &Places) -> Option<StoreLeft> {
    let store = &p.store_dir;
    if !store.is_dir() {
        Some(StoreLeft::Absent)
    } else if p.bin_dirs().iter().any(|bd| same_file(store, bd)) {
        Some(StoreLeft::PathDir)
    } else if std::fs::read_dir(store).is_err() {
        Some(StoreLeft::Unlisted)
    } else {
        None
    }
}

/// Step 3 of [`self_uninstall`] ([`sweep_caffeine`]).
fn sweep_store(s: &mut Sweep<'_>, p: &Places, caffeine_kept: &[PathBuf]) {
    let store = &p.store_dir;
    match store_left(p) {
        None => {}
        Some(StoreLeft::Absent) => return,
        Some(StoreLeft::PathDir) => {
            s.skip(
                &store.display().to_string(),
                "it is the PATH directory itself, not a store of its own — nothing in it was \
                 removed as the store's (ATERM_STORE_DIR needs a directory of its own)",
            );
            return;
        }
        Some(StoreLeft::Unlisted) => {
            s.skip(
                &store.display().to_string(),
                "cannot be listed, so what it holds cannot be judged — left in place",
            );
            return;
        }
    }
    let aterm = store.join("aterm");
    let run_as = runs_as(aterm_uds::peer::our_uid(), &p.home, &aterm);
    // What the store holds is judged by asking its aterm; one this sweep may not ask leaves it.
    let unasked = match run_as {
        _ if !is_executable_file(&aterm) || caffeine_kept.contains(&aterm) => None,
        RunAs::Nobody => Some(NOT_AS_ROOT.to_string()),
        RunAs::Account(uid) => {
            sudo_refuses(uid).map(|rc| format!("sudo could not run it as uid {uid} (exit {rc})"))
        }
        RunAs::Me => None,
    };
    if let Some(why) = unasked {
        s.skip(
            &store.display().to_string(),
            &format!("its aterm was not asked what it is: {why} — left in place"),
        );
        return;
    }
    let (kept, rest): (Vec<PathBuf>, Vec<PathBuf>) = sorted_entries(store)
        .into_iter()
        .partition(|entry| caffeine_kept.contains(entry));
    let (own, foreign): (Vec<PathBuf>, Vec<PathBuf>) = rest
        .into_iter()
        .partition(|entry| store_entry_is_ours(entry, run_as));
    let is_link = std::fs::symlink_metadata(store).is_ok_and(|m| m.file_type().is_symlink());
    if foreign.is_empty() && kept.is_empty() && !is_link {
        s.rm(store, "source-built toolset");
        return;
    }
    for entry in &kept {
        s.skip(
            &entry.display().to_string(),
            "kept: its `aterm caffeine` is what stops the Caffeine mug, and that did not \
             finish (above)",
        );
    }
    for entry in &own {
        s.rm(entry, "source-built toolset entry");
    }
    if !foreign.is_empty() {
        let mut shown = foreign
            .iter()
            .take(3)
            .filter_map(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        if foreign.len() > 3 {
            shown.push_str(&format!(" and {} more", foreign.len() - 3));
        }
        s.skip(
            &store.display().to_string(),
            &format!(
                "holds files this installer did not place ({shown}) — left in place with them"
            ),
        );
    }
}

/// Step 8 of [`self_uninstall`].
fn sweep_path_blocks(s: &mut Sweep<'_>, p: &Places) {
    let mut rcs: Vec<PathBuf> = Vec::new();
    for rc in [
        p.home.join(".zshrc"),
        p.home.join(".bashrc"),
        p.home.join(".bash_profile"),
        p.home.join(".bash_login"),
        p.home.join(".profile"),
        p.xdg_config.join("fish/config.fish"),
        p.home.join(".config/fish/config.fish"),
    ] {
        if !rcs.contains(&rc) {
            rcs.push(rc);
        }
    }
    for rc in rcs {
        if !rc.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&rc) else {
            continue;
        };
        if !text.contains(PATH_BLOCK_START) {
            continue;
        }
        if s.outsider.is_some() {
            s.rm_shared(&rc, "PATH block");
            continue;
        }
        if !text.contains(PATH_BLOCK_END) {
            s.skip(
                &rc.display().to_string(),
                "start marker without its end marker — delete the aterm block by hand",
            );
            continue;
        }
        if s.dry {
            let act = s.act();
            s.say(&format!("{act} PATH block: {}", rc.display()));
            s.removed += 1;
            continue;
        }
        match without_block(&text) {
            Some(kept)
                if std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&rc)
                    .and_then(|mut f| f.write_all(kept.as_bytes()))
                    .is_ok() =>
            {
                s.say(&format!("removed PATH block: {}", rc.display()));
                s.removed += 1;
            }
            _ => s.skip(
                &rc.display().to_string(),
                "could not rewrite the profile — delete the aterm block by hand",
            ),
        }
    }
}

/// `text` without the lines from an exact [`PATH_BLOCK_START`] line to the next exact
/// [`PATH_BLOCK_END`] line.
fn without_block(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut dropping = false;
    for line in text.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if bare == PATH_BLOCK_START {
            dropping = true;
        } else if bare == PATH_BLOCK_END {
            dropping = false;
        } else if !dropping {
            out.push_str(line);
        }
    }
    (!dropping).then_some(out)
}

/// The profile the managed PATH block lands in for `env`'s shell ([`crate::hooks::login_rc`]).
pub(crate) fn rc_target(p: &Places, env: &ShellEnv) -> Option<PathBuf> {
    match env.shell_name() {
        "zsh" => Some(p.home.join(".zshrc")),
        "bash" if cfg!(target_os = "macos") => Some(
            [".bash_profile", ".bash_login", ".profile"]
                .iter()
                .map(|f| p.home.join(f))
                .find(|f| f.is_file())
                .unwrap_or_else(|| p.home.join(".bash_profile")),
        ),
        "bash" => Some(p.home.join(".bashrc")),
        "fish" => Some(p.xdg_config.join("fish/config.fish")),
        _ => None,
    }
}

/// The toolset hook for `shell`, which atpkg writes ([`crate::hooks::HOOK_BASENAME`]).
fn hook_of(p: &Places, shell: &str) -> PathBuf {
    p.home.join(format!(
        ".aterm/shell.d/{}.{shell}",
        crate::hooks::HOOK_BASENAME
    ))
}

/// What `shell-setup` is asked to do.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// Wire the managed PATH block.
    pub path: bool,
    /// Write the shell completions.
    pub completions: bool,
    /// Install the man pages of this directory (a checkout's `man/`).
    pub man: Option<PathBuf>,
    /// Install the man pages this binary carries ([`Setup::man`]).
    pub own_man: bool,
    /// Write the Linux desktop entry for `<bin dir>/aterm`.
    pub desktop: bool,
    /// The hicolor icon tree to install beside the desktop entry.
    pub icons: Option<PathBuf>,
    /// The toolset hook is coming (a first launch lays it).
    pub toolset_coming: bool,
    /// Say what would be done, and do nothing.
    pub dry_run: bool,
}

/// `aterm pkg shell-setup`.
pub fn shell_setup(
    p: &Places,
    env: &ShellEnv,
    exe: &Path,
    setup: &Setup,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    let mut ok = true;
    if setup.path {
        ok = if setup.dry_run {
            plan_path(p, env, setup.toolset_coming, out);
            true
        } else {
            wire_path(p, env, setup.toolset_coming, out, err)
        };
    }
    if let Some(src) = &setup.man {
        let from = src.display().to_string();
        install_man_pages(p, env, &checkout_pages(src), &from, setup.dry_run, out, err);
    } else if setup.own_man {
        install_man_pages(
            p,
            env,
            &carried_pages(),
            "this binary",
            setup.dry_run,
            out,
            err,
        );
    }
    if setup.completions {
        install_completions(p, env, exe, setup.dry_run, out, err);
    }
    if setup.desktop {
        install_desktop_entry(p, env, setup.icons.as_deref(), setup.dry_run, out, err);
    }
    ok
}

/// The PATH line of a plan ([`wire_path`]).
fn plan_path(p: &Places, env: &ShellEnv, coming: bool, out: &mut dyn Write) {
    let line = match rc_target(p, env) {
        None => format!(
            "PATH: unrecognised shell ({}) — no profile is edited; a source hint prints \
             instead",
            env.shell.as_deref().unwrap_or("<unset>")
        ),
        Some(rc) if std::fs::read_to_string(&rc).is_ok_and(|t| t.contains(&path_block(p, env))) => {
            format!(
                "PATH: already wired — {} carries the managed block; no edit",
                rc.display()
            )
        }
        Some(_) if !(coming || hook_of(p, env.shell_name()).is_file()) => {
            "PATH: no edit — no ALab toolset to put on PATH".to_string()
        }
        Some(rc) => {
            let verb = if std::fs::read_to_string(&rc).is_ok_and(|t| t.contains(PATH_BLOCK_START)) {
                "updates the"
            } else {
                "appends ONE"
            };
            format!(
                "PATH: {verb} marker-fenced block to {} — the toolset hook + {}",
                rc.display(),
                p.bin_dir.display()
            )
        }
    };
    let _ = writeln!(out, "atpkg: {line}");
}

/// The CLI link directory leads an inherited bundle.
fn path_block(p: &Places, env: &ShellEnv) -> String {
    let hook = hook_of(p, env.shell_name());
    let body = if env.shell_name() == "fish" {
        let bin = crate::hooks::fish_quote(&p.bin_dir);
        let hook = crate::hooks::fish_quote(&hook);
        format!(
            "set -l __aterm_cli_bin \"{bin}\"\n\
             set -l __aterm_cli_rest\n\
             for __aterm_cli_entry in $PATH; if test \"$__aterm_cli_entry\" != \"$__aterm_cli_bin\"; set __aterm_cli_rest $__aterm_cli_rest \"$__aterm_cli_entry\"; end; end\n\
             set -gx PATH \"$__aterm_cli_bin\" $__aterm_cli_rest\n\
             if test -f \"{hook}\"; source \"{hook}\"; end\n"
        )
    } else {
        let bin = crate::hooks::sh_quote(&p.bin_dir);
        let hook = crate::hooks::sh_quote(&hook);
        format!(
            "__aterm_cli_bin=\"{bin}\"\n\
             __aterm_cli_path=\":$PATH:\"\n\
             while :; do __aterm_cli_next=\"${{__aterm_cli_path//\":$__aterm_cli_bin:\"/:}}\"; [ \"$__aterm_cli_next\" = \"$__aterm_cli_path\" ] && break; __aterm_cli_path=\"$__aterm_cli_next\"; done\n\
             case \"$__aterm_cli_path\" in :) export PATH=\"$__aterm_cli_bin\" ;; *) __aterm_cli_path=\"${{__aterm_cli_path#:}}\"; __aterm_cli_path=\"${{__aterm_cli_path%:}}\"; export PATH=\"$__aterm_cli_bin:$__aterm_cli_path\" ;; esac\n\
             unset __aterm_cli_bin __aterm_cli_path __aterm_cli_next\n\
             if [ -f \"{hook}\" ]; then . \"{hook}\"; fi\n"
        )
    };
    format!("{PATH_BLOCK_START}\n{body}{PATH_BLOCK_END}\n")
}

/// Replace only the exact managed section, preserving every byte around it.
fn profile_with_path_block(text: &str, block: &str) -> Result<String, &'static str> {
    let (mut start, mut end, mut offset) = (None, None, 0);
    for line in text.split_inclusive('\n') {
        match line.trim_end_matches(['\r', '\n']) {
            PATH_BLOCK_START if start.is_none() && end.is_none() => start = Some(offset),
            PATH_BLOCK_END if start.is_some() && end.is_none() => end = Some(offset + line.len()),
            PATH_BLOCK_START | PATH_BLOCK_END => {
                return Err("the profile has ambiguous PATH block markers");
            }
            _ => {}
        }
        offset += line.len();
    }
    match (start, end) {
        (Some(start), Some(end)) => Ok(format!("{}{block}{}", &text[..start], &text[end..])),
        (None, None) => {
            let separator = if text.is_empty() || text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Ok(format!("{text}{separator}{block}"))
        }
        _ => Err("the profile has an incomplete PATH block"),
    }
}

/// Append or upgrade only the managed profile block, keeping the user's other text.
fn wire_path(
    p: &Places,
    env: &ShellEnv,
    coming: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    let shell = env.shell_name();
    let Some(rc) = rc_target(p, env) else {
        if hook_of(p, "bash").is_file() {
            let _ = writeln!(
                err,
                "atpkg: NOTE: source ~/.aterm/shell.d/{}.* from your {} profile to get the ALab \
                 toolset on PATH",
                crate::hooks::HOOK_BASENAME,
                if shell.is_empty() { "shell" } else { shell }
            );
        }
        return false;
    };
    let hook = hook_of(p, shell);
    if !hook.is_file() && !coming {
        let _ = writeln!(out, "atpkg: PATH: no edit — no ALab toolset to put on PATH");
        return false;
    }
    let text = match std::fs::read_to_string(&rc) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            let _ = writeln!(err, "atpkg: cannot read {}: {e}", rc.display());
            return false;
        }
    };
    let block = path_block(p, env);
    let updated = match profile_with_path_block(&text, &block) {
        Ok(updated) => updated,
        Err(why) => {
            let _ = writeln!(err, "atpkg: {}: {why}; no edit", rc.display());
            return false;
        }
    };
    if updated == text {
        let _ = writeln!(
            out,
            "atpkg: ALab toolset already on PATH via {}",
            rc.display()
        );
        return true;
    }
    let bin = p.bin_dir.display();
    let destination = if rc.is_symlink() {
        match rc.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                let _ = writeln!(err, "atpkg: cannot resolve {}: {e}", rc.display());
                return false;
            }
        }
    } else {
        rc.clone()
    };
    if let Some(parent) = destination.parent() {
        let _ = made_dir(parent);
    }
    let tmp = destination.with_file_name(format!(".aterm-profile.{}", std::process::id()));
    let wrote = (|| {
        let mode = destination
            .metadata()
            .map(|m| m.mode() & 0o777)
            .unwrap_or(0o600);
        let mut file = crate::platform::open_create_new_write(&tmp, mode)?;
        let written = file
            .write_all(updated.as_bytes())
            .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)));
        drop(file);
        written
            .and_then(|()| std::fs::rename(&tmp, &destination))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
    })();
    if wrote.is_err() {
        let _ = writeln!(
            err,
            "atpkg: NOTE: could not write {} — add this line yourself for the ALab toolset on \
             PATH:\n{block}",
            rc.display()
        );
        return false;
    }
    let rc_s = rc.display();
    let said = if hook.is_file() {
        format!("added the ALab toolset and {bin} to PATH in {rc_s} — open a new shell")
    } else {
        format!(
            "added {bin} to PATH in {rc_s} — open a new shell; the ALab toolset joins it after \
             aterm's first launch"
        )
    };
    let _ = writeln!(out, "atpkg: {said}");
    true
}

/// A man page to install.
type Page = (String, OsString, Cow<'static, [u8]>);

/// `src`'s `*.[1-9]` pages (a checkout's `man/`).
fn checkout_pages(src: &Path) -> Vec<Page> {
    sorted_entries(src)
        .into_iter()
        .filter_map(|page| {
            let section = page
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| matches!(e.as_bytes(), [b'1'..=b'9']))?
                .to_string();
            let name = page.file_name()?.to_os_string();
            if !page.is_file() {
                return None;
            }
            let bytes = std::fs::read(&page).ok()?;
            Some((section, name, Cow::Owned(bytes)))
        })
        .collect()
}

/// The pages this binary carries ([`CARRIED_MAN_PAGES`]).
fn carried_pages() -> Vec<Page> {
    CARRIED_MAN_PAGES
        .iter()
        .map(|&(section, name, page)| (section.to_string(), name.into(), Cow::Borrowed(page)))
        .collect()
}

/// `dir` and its missing ancestors, made as [`create_dir_all_unshared`] makes them.
fn made_dir(dir: &Path) -> std::io::Result<()> {
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755)
}

/// `bytes` as `dir/name`, mode 644, `dir` made first ([`made_dir`]).
fn write_644(dir: &Path, name: &OsStr, bytes: &[u8]) -> std::io::Result<()> {
    let dest = dir.join(name);
    made_dir(dir)
        .and_then(|()| std::fs::write(&dest, bytes))
        .and_then(|()| std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)))
}

/// Write `pages` (from `from`, as a plan names it) into the man tree's section directories (mode
/// 644), and say when `$MANPATH` is set and omits the tree.
fn install_man_pages(
    p: &Places,
    env: &ShellEnv,
    pages: &[Page],
    from: &str,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    if dry {
        if !pages.is_empty() {
            let _ = writeln!(
                out,
                "atpkg: man pages: would install {} page(s) from {from} -> {}",
                pages.len(),
                p.man_root.display()
            );
        }
        return;
    }
    let mut n = 0usize;
    for (section, name, bytes) in pages {
        let dir = p.man_root.join(format!("man{section}"));
        if write_644(&dir, name, bytes).is_ok() {
            n += 1;
        }
    }
    if n == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "atpkg: installed {n} man page(s) -> {} (e.g. man aterm)",
        p.man_root.display()
    );
    if let Some(manpath) = env.manpath.as_deref().filter(|m| !m.is_empty())
        && !manpath
            .split(':')
            .any(|entry| Path::new(entry) == p.man_root)
    {
        let _ = writeln!(
            err,
            "atpkg: NOTE: {} is not in $MANPATH — add it to run `man aterm`",
            p.man_root.display()
        );
    }
}

/// The completion files `shell-setup` writes and the sweep removes: `(shell, front door, path)`.
fn completion_entries(p: &Places) -> [(&'static str, bool, PathBuf); 6] {
    let zsh = p.xdg_data.join("zsh/site-functions");
    let bash = p.xdg_data.join("bash-completion/completions");
    let fish = p.xdg_config.join("fish/completions");
    [
        ("bash", false, bash.join("aterm-ctl")),
        ("zsh", false, zsh.join("_aterm-ctl")),
        ("fish", false, fish.join("aterm-ctl.fish")),
        ("bash", true, bash.join("aterm")),
        ("zsh", true, zsh.join("_aterm")),
        ("fish", true, fish.join("aterm.fish")),
    ]
}

/// The paths of [`completion_entries`].
fn completion_files(p: &Places) -> Vec<PathBuf> {
    completion_entries(p)
        .into_iter()
        .map(|(_, _, f)| f)
        .collect()
}

/// Publish generated, non-secret setup text through a file this call created.
fn write_setup_file(tmp: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = crate::platform::open_create_new_write(tmp, 0o666)?;
    let written = file.write_all(bytes);
    drop(file);
    written
        .and_then(|()| std::fs::rename(tmp, dest))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(tmp);
        })
}

/// Generate the completions with `exe` and write each where its shell loads it from.
fn install_completions(
    p: &Places,
    env: &ShellEnv,
    exe: &Path,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let zsh_dir = p.xdg_data.join("zsh/site-functions");
    let bash_loader = [
        "/opt/homebrew/etc/profile.d/bash_completion.sh",
        "/usr/local/etc/profile.d/bash_completion.sh",
        "/etc/profile.d/bash_completion.sh",
        "/usr/share/bash-completion/bash_completion",
    ]
    .iter()
    .any(|f| Path::new(f).is_file());
    let tilde = zsh_dir
        .strip_prefix(&p.home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_default();
    let zsh_loader = std::fs::read_to_string(p.home.join(".zshrc")).is_ok_and(|rc| {
        rc.contains(&zsh_dir.display().to_string()) || (!tilde.is_empty() && rc.contains(&tilde))
    });
    let fish_loader = env.on_path("fish").is_some();
    let front_door =
        first_line_bounded(exe, &["--completions", "zsh"], CHILD_HANG).contains("#compdef aterm");
    let mut wrote = (false, false, false);
    for (shell, front, dest) in completion_entries(p) {
        if front && !front_door {
            continue;
        }
        if dry {
            let _ = writeln!(out, "atpkg: completions: {shell} -> {}", dest.display());
            continue;
        }
        let Some(dir) = dest.parent() else {
            continue;
        };
        if made_dir(dir).is_err() {
            continue;
        }
        let args: &[&str] = if front {
            &["--completions", shell]
        } else {
            &["ctl", "--completions", shell]
        };
        let mut cmd = aterm_uds::spawnfd::command(exe);
        cmd.args(args);
        let Ok(run) = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + CHILD_HANG)
        else {
            continue;
        };
        if run.stdout.is_empty() {
            continue;
        }
        let tmp = dir.join(format!(
            ".{}.tmp.{}",
            dest.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id()
        ));
        if write_setup_file(&tmp, &dest, &run.stdout).is_err() {
            continue;
        }
        match shell {
            "bash" => wrote.0 = true,
            "zsh" => wrote.1 = true,
            _ => wrote.2 = true,
        }
    }
    if dry {
        return;
    }
    let claimed: Vec<&str> = [
        ("bash", wrote.0 && bash_loader),
        ("zsh", wrote.1 && zsh_loader),
        ("fish", wrote.2 && fish_loader),
    ]
    .iter()
    .filter(|(_, yes)| *yes)
    .map(|(name, _)| *name)
    .collect();
    if !claimed.is_empty() {
        let _ = writeln!(
            out,
            "atpkg: installed shell completions ({}) — restart your shell to load them",
            claimed.join(", ")
        );
    }
    if wrote.1 && !zsh_loader {
        let z = zsh_dir.display();
        let _ = writeln!(
            err,
            "atpkg: NOTE: zsh completions landed in {z}, which ~/.zshrc does not put on \
             $fpath — add:\n  fpath=(\"{z}\" $fpath)\n  autoload -Uz compinit && compinit"
        );
    }
}

/// Whether `path` may ride the desktop entry's `Exec=` line UNQUOTED: absolute.
fn desktop_exec_path_ok(path: &Path) -> bool {
    path.to_str().is_some_and(|p| {
        p.starts_with('/')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b))
    })
}

/// Tell launchers a desktop entry came or went now rather than at their next rescan, through
/// the program `$PATH` names when `run_as` says this process runs it ([`runs_as`]: under sudo,
/// `$PATH` is the person's).
fn refresh_desktop_database(env: &ShellEnv, xdg_data: &Path, run_as: impl Fn(&Path) -> RunAs) {
    if let Some(update) = env.on_path("update-desktop-database")
        && run_as(&update) == RunAs::Me
    {
        let mut cmd = aterm_uds::spawnfd::command(update);
        cmd.arg(xdg_data.join("applications"));
        let _ = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + CHILD_HANG);
    }
}

/// An icon to install.
type Icon = (String, Cow<'static, [u8]>);

/// The `<size>/apps/aterm.png` icons of the hicolor tree `dir` (a checkout's).
fn checkout_icons(dir: &Path) -> Vec<Icon> {
    sorted_entries(dir)
        .into_iter()
        .filter_map(|size| {
            let png = size.join("apps/aterm.png");
            let name = size.file_name()?.to_str()?.to_string();
            if !png.is_file() {
                return None;
            }
            let bytes = std::fs::read(&png).ok()?;
            Some((name, Cow::Owned(bytes)))
        })
        .collect()
}

/// The icons this binary carries ([`HICOLOR_ICONS`]).
fn carried_icons() -> Vec<Icon> {
    #[cfg(target_os = "linux")]
    let carried = HICOLOR_ICONS
        .iter()
        .map(|&(size, png)| (size.to_string(), Cow::Borrowed(png)))
        .collect();
    #[cfg(not(target_os = "linux"))]
    let carried = Vec::new();
    carried
}

/// The Linux desktop identity ([`DESKTOP_MARKER`]).
fn install_desktop_entry(
    p: &Places,
    env: &ShellEnv,
    icons: Option<&Path>,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let bin = p.bin_dir.join("aterm");
    if !std::fs::metadata(&bin).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
        return;
    }
    if !desktop_exec_path_ok(&bin) {
        let _ = writeln!(
            err,
            "atpkg: SKIPPED the desktop entry: {} contains characters an unquoted desktop \
             Exec= line cannot carry safely (set ATERM_BIN_DIR to a plain path)",
            bin.display()
        );
        return;
    }
    let app_dir = p.xdg_data.join("applications");
    let entry = app_dir.join("aterm.desktop");
    let sources = icons.map_or_else(carried_icons, checkout_icons);
    if dry {
        let _ = writeln!(
            out,
            "atpkg: desktop entry: would write {} (+ the aterm icon at {} size(s))",
            entry.display(),
            sources.len()
        );
        return;
    }
    if entry.is_file()
        && !std::fs::read_to_string(&entry)
            .is_ok_and(|text| text.lines().any(|l| l == DESKTOP_MARKER))
    {
        let _ = writeln!(
            out,
            "atpkg: NOTE: replacing a pre-existing {} this installer did not write (no backup \
             is kept)",
            entry.display()
        );
    }
    let b = bin.display();
    let body = format!(
        "{DESKTOP_MARKER}\n[Desktop Entry]\nType=Application\nName=aterm\nGenericName=Terminal\n\
         Comment=The batteries-included terminal for AI\nTryExec={b}\nExec={b} --window\n\
         Icon=aterm\nTerminal=false\nCategories=System;TerminalEmulator;\n\
         Keywords=terminal;shell;console;command line;\nStartupNotify=true\n\
         StartupWMClass=aterm\n"
    );
    let tmp = app_dir.join(format!(".aterm.desktop.tmp.{}", std::process::id()));
    if made_dir(&app_dir)
        .and_then(|()| write_setup_file(&tmp, &entry, body.as_bytes()))
        .is_err()
    {
        let _ = writeln!(
            err,
            "atpkg: SKIPPED the desktop entry: could not write {}",
            entry.display()
        );
        return;
    }
    let mut placed = 0usize;
    for (size, png) in &sources {
        let dir = p.xdg_data.join(format!("icons/hicolor/{size}/apps"));
        if write_644(&dir, OsStr::new("aterm.png"), png).is_ok() {
            placed += 1;
        }
    }
    refresh_desktop_database(env, &p.xdg_data, |_| RunAs::Me);
    if placed > 0 {
        let _ = writeln!(
            out,
            "atpkg: installed the desktop entry -> {} (+ the aterm icon at {placed} size(s) \
             under {})",
            entry.display(),
            p.xdg_data.join("icons/hicolor").display()
        );
    } else {
        let _ = writeln!(
            out,
            "atpkg: installed the desktop entry -> {}",
            entry.display()
        );
        let _ = writeln!(
            err,
            "atpkg: NOTE: no aterm icon was written under {} — the launcher entry works, \
             with a generic icon",
            p.xdg_data.join("icons/hicolor").display()
        );
    }
}

/// The places of this process: `$HOME`.
fn process_places() -> Option<Places> {
    let home = aterm_types::dirs::home_dir()?;
    let env_dir = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    Some(Places::defaults(
        &home,
        env_dir("XDG_DATA_HOME").as_deref(),
        env_dir("XDG_CONFIG_HOME").as_deref(),
    ))
}

/// A location flag's value.
fn take_dir(
    flag: &str,
    it: &mut std::slice::Iter<'_, String>,
    verb: &str,
) -> Result<PathBuf, ExitCode> {
    match it.next() {
        Some(v) if !v.is_empty() && !v.starts_with('-') => Ok(PathBuf::from(v)),
        _ => Err(crate::cli::usage_error(
            verb,
            Some(&format!("{flag} needs a directory")),
        )),
    }
}

/// `aterm pkg self-uninstall [--dry-run] [--install-dir <dir>] [--bin-dir <dir>] [--store-dir
/// <dir>] [--man-root <dir>] [--man <dir>]`.
pub fn self_uninstall_verb(rest: &[String]) -> ExitCode {
    const VERB: &str = "self-uninstall";
    let Some(mut places) = process_places() else {
        eprintln!("atpkg {VERB}: HOME is unset — nothing to find what install.sh placed under");
        return ExitCode::from(2);
    };
    let (mut dry, mut man) = (false, None);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let parsed = match arg.as_str() {
            "--dry-run" => {
                dry = true;
                Ok(())
            }
            "--install-dir" => take_dir(arg, &mut it, VERB).map(|d| places.install_dir = Some(d)),
            "--bin-dir" => take_dir(arg, &mut it, VERB).map(|d| places.bin_dir = d),
            "--store-dir" => take_dir(arg, &mut it, VERB).map(|d| places.store_dir = d),
            "--man-root" => take_dir(arg, &mut it, VERB).map(|d| places.man_root = d),
            "--man" => take_dir(arg, &mut it, VERB).map(|d| man = Some(d)),
            other => Err(crate::cli::usage_error(
                VERB,
                Some(&format!("unknown argument {other:?}")),
            )),
        };
        if let Err(code) = parsed {
            return code;
        }
    }
    let ok = self_uninstall(
        &places,
        man.as_deref(),
        dry,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `aterm pkg shell-setup [--path] [--completions] [--man <dir> | --own-man] [--desktop [--icons
/// <dir>]] [--toolset-coming] [--dry-run] [--bin-dir <dir>] [--man-root <dir>]`.
pub fn shell_setup_verb(rest: &[String]) -> ExitCode {
    const VERB: &str = "shell-setup";
    let Some(mut places) = process_places() else {
        eprintln!("atpkg {VERB}: HOME is unset — no profile to wire");
        return ExitCode::from(2);
    };
    let mut setup = Setup::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let parsed = match arg.as_str() {
            "--path" => {
                setup.path = true;
                Ok(())
            }
            "--completions" => {
                setup.completions = true;
                Ok(())
            }
            "--toolset-coming" => {
                setup.toolset_coming = true;
                Ok(())
            }
            "--dry-run" => {
                setup.dry_run = true;
                Ok(())
            }
            "--desktop" => {
                setup.desktop = true;
                Ok(())
            }
            "--man" => take_dir(arg, &mut it, VERB).map(|d| setup.man = Some(d)),
            "--own-man" => {
                setup.own_man = true;
                Ok(())
            }
            "--icons" => take_dir(arg, &mut it, VERB).map(|d| setup.icons = Some(d)),
            "--bin-dir" => take_dir(arg, &mut it, VERB).map(|d| places.bin_dir = d),
            "--man-root" => take_dir(arg, &mut it, VERB).map(|d| places.man_root = d),
            other => Err(crate::cli::usage_error(
                VERB,
                Some(&format!("unknown argument {other:?}")),
            )),
        };
        if let Err(code) = parsed {
            return code;
        }
    }
    if !setup.path && !setup.completions && setup.man.is_none() && !setup.own_man && !setup.desktop
    {
        setup.path = true;
        setup.completions = true;
    }
    if setup.desktop && !cfg!(target_os = "linux") {
        println!("atpkg: desktop entry: skipped — an XDG desktop entry is a Linux desktop's");
        setup.desktop = false;
    }
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
    else {
        eprintln!("atpkg {VERB}: this binary's own path cannot be read");
        return ExitCode::from(1);
    };
    let ok = shell_setup(
        &places,
        &ShellEnv::of_process(),
        &exe,
        &setup,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
#[path = "selfsetup_tests.rs"]
mod tests;
