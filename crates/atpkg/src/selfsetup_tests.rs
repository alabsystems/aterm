// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `self-uninstall` and `shell-setup` over scratch accounts. Every run is SCOPED to a
//! scratch `install_dir` and a scratch HOME, so nothing here can reach this machine's
//! `/Applications` or its real profile; the stand-ins a sweep runs write only into the
//! scratch tree, by absolute path.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::*;

/// A scratch account: `<dir>/home` as HOME, its apps scoped to `<dir>/home/Applications`.
struct Account {
    dir: crate::testdir::Dir,
    places: Places,
}

impl Account {
    fn new(label: &str) -> Self {
        let dir = crate::testdir::dir(&format!("atpkg-selfsetup-{label}-"));
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let mut places = Places::defaults(&home, None, None);
        places.install_dir = Some(home.join("Applications"));
        Self { dir, places }
    }

    fn home(&self) -> &Path {
        &self.places.home
    }

    /// Sweep, capturing both channels: `(ok, stdout, stderr)`.
    fn sweep(&self, man: Option<&Path>, dry: bool) -> (bool, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ok = self_uninstall(&self.places, man, dry, &mut out, &mut err);
        (
            ok,
            String::from_utf8(out).expect("utf-8"),
            String::from_utf8(err).expect("utf-8"),
        )
    }

    fn setup(&self, env: &ShellEnv, exe: &Path, setup: &Setup) -> (bool, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ok = shell_setup(&self.places, env, exe, setup, &mut out, &mut err);
        (
            ok,
            String::from_utf8(out).expect("utf-8"),
            String::from_utf8(err).expect("utf-8"),
        )
    }
}

fn write(path: &Path, body: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

/// An executable stand-in (a script written at run time, never a file in the tree).
fn exe(path: &Path, body: &str) {
    write(path, body);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// An aterm bundle: its Info.plist names com.aterm.aterm, and (with `atpkg`) its atpkg is
/// the stand-in given.
fn bundle(app: &Path, atpkg: Option<&str>) {
    write(
        &app.join("Contents/Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
         <key>CFBundleIdentifier</key><string>com.aterm.aterm</string></dict></plist>\n",
    );
    if let Some(body) = atpkg {
        exe(&app.join("Contents/MacOS/atpkg"), body);
    }
}

/// A stand-in atpkg that sweeps as `uninstall --all` does — records the decline under
/// `support` when none is there — and logs to `log`. Absolute paths: the child inherits
/// this test process's HOME, which is the real one.
fn sweeping_atpkg(support: &Path, log: &Path) -> String {
    format!(
        "#!/bin/sh\necho swept >>'{}'\np='{}/pkg'\nmkdir -p \"$p\" && {{ [ -f \"$p/declined\" ] || \
         echo declined >\"$p/declined\"; }}\n",
        log.display(),
        support.display()
    )
}

fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(dir).expect("under").display().to_string());
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn zsh(path: &str) -> ShellEnv {
    ShellEnv {
        shell: Some("/bin/zsh".into()),
        path: Some(path.into()),
        manpath: None,
    }
}

/// `--dry-run` decides about what it owns — a planted managed PATH block — and leaves the
/// profile byte for byte, and the account untouched.
#[test]
fn an_uninstall_dry_run_plans_and_changes_nothing() {
    let a = Account::new("dry");
    let rc = a.home().join(".zshrc");
    let profile =
        format!("{PATH_BLOCK_START}\nexport PATH=\"$HOME/.local/bin:$PATH\"\n{PATH_BLOCK_END}\n");
    write(&rc, &profile);
    let before = tree(a.home());
    let (ok, out, err) = a.sweep(None, true);
    assert!(ok, "{out}{err}");
    assert!(
        out.contains(&format!("would remove PATH block: {}", rc.display())),
        "{out}"
    );
    assert!(out.contains("dry run complete — 1 item(s)"), "{out}");
    assert_eq!(std::fs::read_to_string(&rc).expect("rc"), profile);
    assert_eq!(tree(a.home()), before);
}

/// The sweep removes from the store only what the store lanes place there — the aterm
/// binary, its argv0 aliases, the updater's dir, a staging leftover — and keeps a bundle
/// whose toolchain sweep failed (exit 75: a pass holds the store), since the remedy it
/// prints runs that bundle's atpkg. A SKIPPED item makes the sweep's answer false.
#[test]
fn uninstall_removes_only_what_it_placed() {
    let mut a = Account::new("own");
    let store = a.dir.join("store");
    std::fs::create_dir_all(store.join(".aterm-update")).expect("mkdir");
    exe(&store.join("backup.sh"), "#!/bin/sh\necho backup\n");
    exe(&store.join("aterm"), "#!/bin/sh\necho \"aterm 0.0.0\"\n");
    write(&store.join(".aterm-install.Ab12Cd"), "");
    // argv0 aliases are known by what they are, a link to exactly `aterm`; a link to
    // anything else is not the store's.
    for alias in ["aterm-cli", "aterm-ctl", "atpkg", "aterm-gui", "aterm-next"] {
        symlink("aterm", store.join(alias)).expect("alias");
    }
    symlink("backup.sh", store.join("backup")).expect("a foreign link");
    // Asked once first, as the install that placed it did, so the system's first-exec
    // assessment is not what the probe's bound measures.
    aterm_spec::wait::first_exec(&store.join("aterm"), &["--version"]);
    let apps = a.dir.join("apps");
    bundle(&apps.join("aterm.app"), Some("#!/bin/sh\nexit 75\n"));
    a.places.install_dir = Some(apps.clone());
    a.places.store_dir = store.clone();
    let (ok, out, err) = a.sweep(None, false);
    let mut left: Vec<String> = std::fs::read_dir(&store)
        .expect("the store dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["backup", "backup.sh"], "{out}{err}");
    assert!(
        apps.join("aterm.app/Contents/MacOS/atpkg").exists(),
        "the bundle whose sweep failed is kept: {out}{err}"
    );
    assert!(
        err.contains("another aterm pkg run holds the store"),
        "{err}"
    );
    assert!(!ok, "a SKIPPED item is the non-zero exit");
}

/// A scoped sweep removes the bundle it scopes and leaves what another aterm.app on the
/// account uses — the toolchain store, the PATH links, the PATH block, the man pages. Once
/// the scoped copy is the only aterm, its sweep runs, and the decline it records goes with
/// it.
#[test]
fn a_scoped_uninstall_leaves_what_another_aterm_uses() {
    let mut a = Account::new("scoped");
    let home = a.home().to_path_buf();
    let support = home.join("Library/Application Support/aterm");
    let swept = a.dir.join("swept.log");
    let man = home.join(".local/share/man/man1/aterm.1");
    write(&man, "");
    let rc = home.join(".zshrc");
    write(
        &rc,
        format!("{PATH_BLOCK_START}\nexport PATH=\"$PATH:$HOME/.local/bin\"\n{PATH_BLOCK_END}\n"),
    );
    let atpkg = sweeping_atpkg(&support, &swept);
    bundle(&home.join("Applications/aterm.app"), Some(&atpkg));
    let links = home.join(".local/bin");
    std::fs::create_dir_all(&links).expect("mkdir");
    for tool in ["aterm", "atpkg"] {
        symlink(
            home.join(format!("Applications/aterm.app/Contents/MacOS/{tool}")),
            links.join(tool),
        )
        .expect("link");
    }
    bundle(&home.join("aterm.app"), Some(&atpkg));
    a.places.install_dir = Some(home.clone());
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(
        !swept.exists(),
        "the other aterm's store was not swept: {out}"
    );
    assert!(links.join("aterm").is_symlink() && links.join("atpkg").is_symlink());
    assert!(man.exists());
    let text = std::fs::read_to_string(&rc).expect("rc");
    assert_eq!(text.lines().filter(|l| *l == PATH_BLOCK_START).count(), 1);
    assert!(!home.join("aterm.app").exists());
    assert!(
        out.contains("another aterm on this account, uses them"),
        "{out}"
    );

    std::fs::remove_dir_all(home.join("Applications")).expect("rm");
    bundle(&home.join("aterm.app"), Some(&atpkg));
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(swept.exists(), "the only aterm's sweep ran");
    assert!(
        !support.join("pkg/declined").exists(),
        "the decline its sweep recorded goes with it"
    );
    assert!(!links.join("aterm").is_symlink() && !links.join("atpkg").is_symlink());
    assert!(!home.join("aterm.app").exists());
    assert!(!man.exists());
    assert!(
        !std::fs::read_to_string(&rc)
            .expect("rc")
            .contains(PATH_BLOCK_START)
    );
}

/// THE PATH BLOCK ROUND TRIP: `shell-setup --path` appends one marker-fenced block (the
/// hook, and the bin dir PATH lacks), once; the sweep removes exactly it and keeps the
/// rest of the profile. With no hook and none coming nothing is written; an unknown shell
/// is told, never guessed at. The plan says each of these before anything is done.
#[test]
fn the_path_block_is_written_once_and_swept_exactly() {
    let a = Account::new("path");
    let rc = a.home().join(".zshrc");
    write(&rc, "export EDITOR=vi\n");
    let env = zsh("/usr/bin:/bin");
    let path = Setup {
        path: true,
        ..Setup::default()
    };
    let exe = Path::new("/nonexistent/aterm");
    let plan = |setup: &Setup| {
        let (_, out, _) = a.setup(
            &env,
            exe,
            &Setup {
                dry_run: true,
                ..setup.clone()
            },
        );
        out
    };
    assert_eq!(
        plan(&path),
        "atpkg: PATH: no edit — no ALab toolset to put on PATH\n"
    );
    let (ok, out, _) = a.setup(&env, exe, &path);
    assert!(!ok, "no hook and none coming: no block stands: {out}");
    assert_eq!(
        std::fs::read_to_string(&rc).expect("rc"),
        "export EDITOR=vi\n"
    );

    let coming = Setup {
        toolset_coming: true,
        ..path.clone()
    };
    let bin = a.places.bin_dir.display().to_string();
    assert_eq!(
        plan(&coming),
        format!(
            "atpkg: PATH: appends ONE marker-fenced block to {} — the toolset hook + {bin}\n",
            rc.display()
        )
    );
    let (ok, out, _) = a.setup(&env, exe, &coming);
    assert!(ok, "{out}");
    assert!(out.contains("joins it after aterm's first launch"), "{out}");
    let text = std::fs::read_to_string(&rc).expect("rc");
    assert!(text.starts_with("export EDITOR=vi\n"), "{text}");
    assert_eq!(text.matches(PATH_BLOCK_START).count(), 1, "{text}");
    assert!(
        text.contains(&format!("export PATH=\"$PATH:{bin}\"")),
        "{text}"
    );
    let (ok, out, _) = a.setup(&env, exe, &coming);
    assert!(ok && out.contains("already on PATH via"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&rc).expect("rc"),
        text,
        "written once"
    );
    assert!(plan(&coming).contains("already wired"));

    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&rc).expect("rc"),
        "export EDITOR=vi\n\n"
    );

    let tcsh = ShellEnv {
        shell: Some("/bin/tcsh".into()),
        ..env.clone()
    };
    let (ok, out, _) = a.setup(&tcsh, exe, &coming);
    assert!(!ok, "{out}");
    let (_, out, _) = a.setup(
        &tcsh,
        exe,
        &Setup {
            dry_run: true,
            ..coming
        },
    );
    assert!(out.contains("unrecognised shell (/bin/tcsh)"), "{out}");
}

/// Man pages: a checkout's `*.[1-9]` pages land in their sections at 644, with a note when
/// `$MANPATH` is set and omits the tree; the sweep takes the pages aterm ships, and a page
/// the checkout ships only when it is handed that checkout, never a stranger's.
#[test]
fn man_pages_are_installed_from_a_checkout_and_swept_by_name() {
    let a = Account::new("man");
    let src = a.dir.join("checkout/man");
    write(&src.join("aterm.1"), ".TH ATERM 1\n");
    write(&src.join("aterm-extra.1"), ".TH X 1\n");
    write(&src.join("README"), "not a page\n");
    let env = ShellEnv {
        manpath: Some("/usr/share/man".into()),
        ..zsh("/usr/bin")
    };
    let man = Setup {
        man: Some(src.clone()),
        ..Setup::default()
    };
    let (ok, out, err) = a.setup(&env, Path::new("/nonexistent/aterm"), &man);
    assert!(ok, "{out}{err}");
    let section = a.places.man_root.join("man1");
    assert_eq!(tree(&section), ["aterm-extra.1", "aterm.1"]);
    let mode = std::fs::metadata(section.join("aterm.1"))
        .expect("page")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644);
    assert!(out.contains("installed 2 man page(s)"), "{out}");
    assert!(err.contains("is not in $MANPATH"), "{err}");
    write(&section.join("aterm-stranger.1"), "");

    let (ok, _, err) = a.sweep(None, false);
    assert!(!ok);
    assert!(!section.join("aterm.1").exists(), "aterm ships aterm.1");
    assert!(
        err.contains("aterm-extra.1 — not a man page this installer ships"),
        "{err}"
    );
    let (ok, _, err) = a.sweep(Some(&src), false);
    assert!(!ok, "the stranger is still SKIPPED");
    assert!(
        !section.join("aterm-extra.1").exists(),
        "the checkout ships it"
    );
    assert!(section.join("aterm-stranger.1").exists(), "{err}");
}

/// Completions are generated by the binary itself and written where each shell loads them;
/// the front door's only when the binary can generate them; the sweep removes exactly
/// those files.
#[test]
fn completions_come_from_the_binary_and_are_swept() {
    let a = Account::new("comp");
    let stand_in = a.dir.join("bin/aterm");
    exe(
        &stand_in,
        "#!/bin/sh\nif [ \"$1\" = ctl ]; then echo \"# ctl $3\"; exit 0; fi\n\
         if [ \"$1\" = --completions ]; then echo \"#compdef aterm ($2)\"; exit 0; fi\nexit 2\n",
    );
    aterm_spec::wait::first_exec(&stand_in, &["--completions", "zsh"]);
    let setup = Setup {
        completions: true,
        ..Setup::default()
    };
    let (ok, out, err) = a.setup(&zsh("/usr/bin"), &stand_in, &setup);
    assert!(ok, "{out}{err}");
    let files = completion_files(&a.places);
    for f in &files {
        assert!(f.is_file(), "{} written: {out}{err}", f.display());
    }
    assert_eq!(
        std::fs::read_to_string(a.places.xdg_data.join("zsh/site-functions/_aterm"))
            .expect("front door"),
        "#compdef aterm (zsh)\n"
    );
    assert!(
        err.contains("fpath=("),
        "zsh's loader is not in ~/.zshrc: {err}"
    );
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    for f in &files {
        assert!(!f.exists(), "{} swept", f.display());
    }
    // A binary that cannot generate the front door's: the sibling's alone.
    exe(
        &stand_in,
        "#!/bin/sh\nif [ \"$1\" = ctl ]; then echo ok; fi\n",
    );
    aterm_spec::wait::first_exec(&stand_in, &["--completions", "zsh"]);
    a.setup(&zsh("/usr/bin"), &stand_in, &setup);
    assert!(
        a.places
            .xdg_data
            .join("zsh/site-functions/_aterm-ctl")
            .is_file()
    );
    assert!(!a.places.xdg_data.join("zsh/site-functions/_aterm").exists());
}

/// The desktop entry's `Exec=` is written unquoted, so only a plain absolute path may ride
/// it.
#[test]
fn only_a_plain_absolute_path_rides_an_unquoted_exec_line() {
    for (path, ok) in [
        ("/home/user/.local/bin/aterm", true),
        ("/opt/aterm-0.44.0/bin_x86+64/.aterm", true),
        (".local/bin/aterm", false),
        ("/home/user name/bin/aterm", false),
        ("/home/u\"/bin/aterm", false),
        ("/home/u%U/bin/aterm", false),
        ("/home/u;rm/bin/aterm", false),
        ("", false),
    ] {
        assert_eq!(desktop_exec_path_ok(Path::new(path)), ok, "{path:?}");
    }
}

/// `--desktop` writes the entry for `<bin dir>/aterm` with the checkout's every icon size,
/// and the sweep removes exactly that: the entry by its marker line, the icons it vouches
/// for. The marker is spelled out here as an OLDER installer wrote it — entries already on
/// disk carry this exact line, so it may never change. A foreign entry is replaced loudly,
/// and the sweep leaves one it finds without the line.
#[test]
fn the_desktop_entry_is_written_with_its_icons_and_swept_by_its_marker() {
    const ON_DISK: &str =
        "# Managed by aterm install.sh — its uninstall removes only an entry carrying this line.";
    let a = Account::new("desktop");
    let bin = a.places.bin_dir.join("aterm");
    exe(&bin, "#!/bin/sh\n");
    let icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/linux/icons/hicolor");
    let shipped = std::fs::read_dir(&icons)
        .expect("the checkout's icons")
        .count();
    assert_eq!(shipped, 5, "fixture: the checkout ships five icon sizes");
    // No PATH: nothing like update-desktop-database is run.
    let env = zsh("");
    let entry = a.places.xdg_data.join("applications/aterm.desktop");
    write(&entry, "[Desktop Entry]\nName=the X11 aterm\n");
    let setup = Setup {
        desktop: true,
        icons: Some(icons.clone()),
        ..Setup::default()
    };

    let (ok, out, err) = a.setup(
        &env,
        &bin,
        &Setup {
            dry_run: true,
            ..setup.clone()
        },
    );
    assert!(ok, "{out}{err}");
    assert!(
        out.contains(&format!(
            "desktop entry: would write {} (+ the aterm icon at 5 size(s))",
            entry.display()
        )),
        "{out}"
    );
    assert!(
        std::fs::read_to_string(&entry).is_ok_and(|t| t.contains("X11")),
        "a dry run writes nothing"
    );

    let (ok, out, err) = a.setup(&env, &bin, &setup);
    assert!(ok, "{out}{err}");
    assert!(out.contains("NOTE: replacing a pre-existing"), "{out}");
    let text = std::fs::read_to_string(&entry).expect("the entry");
    assert!(
        text.starts_with(&format!("{ON_DISK}\n[Desktop Entry]\n")),
        "{text}"
    );
    assert_eq!(DESKTOP_MARKER, ON_DISK);
    assert!(
        text.contains(&format!("\nExec={} --window\n", bin.display())),
        "{text}"
    );
    let placed: Vec<PathBuf> = ICON_SIZES
        .iter()
        .map(|size| {
            a.places
                .xdg_data
                .join(format!("icons/hicolor/{size}/apps/aterm.png"))
        })
        .collect();
    for icon in &placed {
        assert!(
            icon.metadata().is_ok_and(|m| m.len() > 0),
            "{} missing: {out}{err}",
            icon.display()
        );
    }

    // The stand-in binary is no link this installer placed; the sweep here is the entry's.
    std::fs::remove_file(&bin).expect("rm the stand-in");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(!entry.exists(), "{out}");
    assert!(placed.iter().all(|icon| !icon.exists()), "{out}");

    // An entry without the line is someone else's: left, and said.
    write(&entry, "[Desktop Entry]\nName=the X11 aterm\n");
    write(&placed[0], "png");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a SKIPPED item is the non-zero exit: {out}{err}");
    assert!(entry.exists() && placed[0].exists(), "{out}{err}");
    assert!(err.contains("missing its marker line"), "{err}");
}

/// A piped install has no checkout, so the binary writes what it carries: `--own-man` its
/// man page, and `--desktop` with no `--icons` (on Linux) the icon at every shipped size —
/// byte for byte the repo's, at the paths the sweep takes them from. A dry run names
/// the source and writes nothing; a checkout's `--man` wins over `--own-man`.
#[test]
fn a_piped_install_takes_the_man_page_and_icons_the_binary_carries() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let a = Account::new("carried");
    let bin = a.places.bin_dir.join("aterm");
    exe(&bin, "#!/bin/sh\n");
    let env = zsh("");
    let setup = Setup {
        own_man: true,
        desktop: true,
        ..Setup::default()
    };
    let page = a.places.man_root.join("man1/aterm.1");
    let icons: Vec<PathBuf> = ICON_SIZES
        .iter()
        .map(|size| {
            a.places
                .xdg_data
                .join(format!("icons/hicolor/{size}/apps/aterm.png"))
        })
        .collect();

    let (ok, out, err) = a.setup(
        &env,
        &bin,
        &Setup {
            dry_run: true,
            ..setup.clone()
        },
    );
    assert!(ok, "{out}{err}");
    assert!(
        out.contains("man pages: would install 1 page(s) from this binary"),
        "{out}"
    );
    assert!(!page.exists() && !a.places.xdg_data.exists(), "{out}");

    let (ok, out, err) = a.setup(&env, &bin, &setup);
    assert!(ok, "{out}{err}");
    assert!(out.contains("installed 1 man page(s)"), "{out}");
    assert_eq!(
        std::fs::read(&page).expect("the page"),
        std::fs::read(repo.join("man/aterm.1")).expect("the repo's page")
    );
    assert_eq!(
        std::fs::metadata(&page).expect("page").permissions().mode() & 0o777,
        0o644
    );
    #[cfg(target_os = "linux")]
    {
        let shipped = tree(&repo.join("assets/linux/icons/hicolor"))
            .into_iter()
            .filter(|p| !p.contains('/'))
            .collect::<Vec<_>>();
        let mut sizes = ICON_SIZES.map(str::to_string).to_vec();
        sizes.sort();
        assert_eq!(sizes, shipped, "the sweep knows every shipped size");
        assert_eq!(HICOLOR_ICONS.map(|(size, _)| size), ICON_SIZES);
        for (size, icon) in ICON_SIZES.iter().zip(&icons) {
            assert_eq!(
                std::fs::read(icon).expect("an icon"),
                std::fs::read(
                    repo.join(format!("assets/linux/icons/hicolor/{size}/apps/aterm.png"))
                )
                .expect("the repo's icon"),
                "{size}"
            );
        }
        assert!(out.contains("(+ the aterm icon at 5 size(s)"), "{out}");
    }
    #[cfg(not(target_os = "linux"))]
    assert!(icons.iter().all(|icon| !icon.exists()), "{out}");

    // The sweep takes them all: the page by name, the icons by the entry's marker.
    std::fs::remove_file(&bin).expect("rm the stand-in");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(!page.exists(), "{out}");
    assert!(icons.iter().all(|icon| !icon.exists()), "{out}");

    // A checkout's pages win over the carried ones.
    let src = a.dir.join("checkout/man");
    write(&src.join("aterm.1"), ".TH ATERM 1 checkout\n");
    let (ok, out, err) = a.setup(
        &env,
        &bin,
        &Setup {
            man: Some(src),
            own_man: true,
            ..Setup::default()
        },
    );
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&page).expect("the page"),
        ".TH ATERM 1 checkout\n"
    );
}

/// `dir_within` judges directories by identity: a symlinked spelling is the same place,
/// a directory still to be created lies within the one that will hold it, and `..` below
/// one still to be created undoes that component.
#[test]
fn dir_within_judges_by_identity() {
    let a = Account::new("within");
    let real = a.dir.join("real");
    std::fs::create_dir_all(real.join("sub")).expect("mkdir");
    symlink(&real, a.dir.join("alias")).expect("link");
    assert!(dir_within(&a.dir.join("alias/sub"), &real));
    assert!(dir_within(&real, &a.dir.join("alias")));
    assert!(dir_within(&real.join("new/deeper"), &real));
    assert!(dir_within(
        &a.dir.join("real/new/../sub"),
        &real.join("sub")
    ));
    assert!(!dir_within(&real, &real.join("sub")));
    assert!(!dir_within(&a.dir.join("elsewhere"), &real));
}

/// Step 0's actual admission is exercised without running self-uninstall or any
/// helper. Executable-mode files hold inert text; only the bounded plist reader runs.
#[test]
fn an_uninstall_helper_is_selected_only_from_an_identified_aterm_bundle() {
    let mut refused = Vec::new();
    for foreign in [false, true] {
        let account = Account::new("helper-identity");
        let app = account
            .places
            .install_dir
            .as_ref()
            .unwrap()
            .join("aterm.app");
        bundle(&app, Some("not executed by this test\n"));
        assert_eq!(
            uninstall_bundle(&account.places),
            Some(app.clone()),
            "known bundle plus helper is actually admitted"
        );
        let plist = app.join("Contents/Info.plist");
        if foreign {
            write(
                &plist,
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>com.example.other</string>\
                 </dict></plist>\n",
            );
        } else {
            std::fs::remove_file(&plist).expect("remove this test's plist");
        }
        let helper = app.join("Contents/MacOS/atpkg");
        assert!(crate::toolchain::is_executable_file(&helper));
        refused.push(uninstall_bundle(&account.places).is_none());
        assert_eq!(
            std::fs::read_to_string(&helper).unwrap(),
            "not executed by this test\n",
            "admission touches no helper bytes"
        );
    }
    assert_eq!(refused, [true, true], "missing and foreign identities");
}

/// The actual desktop writer gets an admitted, non-executed command file and an empty
/// injected PATH. Its staging name may already be somebody else's file or link: neither
/// the entry nor that person's bytes may be published, truncated or removed.
#[test]
fn a_desktop_write_preserves_a_preexisting_staging_file_or_link() {
    let mut preserved = Vec::new();
    for linked in [false, true] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atpkg-setup-stage-")
            .tempdir_in("/tmp")
            .expect("exclusive scratch");
        let p = Places::defaults(dir.path(), None, None);
        let bin = p.bin_dir.join("aterm");
        exe(&bin, "not executed by this test\n");
        assert!(
            desktop_exec_path_ok(&bin),
            "the fixture admits the Exec path"
        );
        let env = ShellEnv::default(); // no desktop-database command can be found
        let (mut out, mut err) = (Vec::new(), Vec::new());
        install_desktop_entry(&p, &env, None, false, &mut out, &mut err);
        let app_dir = p.xdg_data.join("applications");
        let entry = app_dir.join("aterm.desktop");
        assert!(
            std::fs::read_to_string(&entry)
                .expect("fresh positive")
                .lines()
                .any(|line| line == DESKTOP_MARKER),
            "the ordinary writer was actually admitted"
        );
        std::fs::remove_file(&entry).expect("remove this test's positive output");
        let stage = app_dir.join(format!(".aterm.desktop.tmp.{}", std::process::id()));
        let other = dir.path().join("personal-notes");
        let sentinel = b"preexisting work\n";
        std::fs::write(&other, sentinel).expect("owned target");
        if linked {
            symlink(&other, &stage).expect("owned planted link");
        } else {
            std::fs::write(&stage, sentinel).expect("owned planted file");
        }
        out.clear();
        err.clear();
        install_desktop_entry(&p, &env, None, false, &mut out, &mut err);
        let stage_kept = if linked {
            std::fs::read_link(&stage).is_ok_and(|target| target == other)
        } else {
            std::fs::read(&stage).is_ok_and(|bytes| bytes == sentinel)
        };
        preserved.push(
            stage_kept
                && std::fs::read(&other).is_ok_and(|bytes| bytes == sentinel)
                && !entry.exists()
                && String::from_utf8_lossy(&err).contains("SKIPPED the desktop entry"),
        );
    }
    assert_eq!(
        preserved,
        [true, true],
        "regular-file and symlink staging collisions"
    );
}

/// A failure after exclusive creation only cleans this call's stage; the destination
/// directory and its contents survive. This control is POSTFIX-only, with no old RED claim.
#[test]
fn a_failed_setup_publish_cleans_only_its_own_stage() {
    let dir = aterm_tempfile::tempdir().expect("exclusive scratch");
    let stage = dir.path().join("stage");
    let dest = dir.path().join("destination");
    std::fs::create_dir(&dest).expect("owned destination directory");
    let kept = dest.join("notes");
    std::fs::write(&kept, b"keep").expect("owned notes");
    assert!(write_setup_file(&stage, &dest, b"generated").is_err());
    assert!(
        !stage.exists(),
        "only the file this call created was removed"
    );
    assert_eq!(std::fs::read(&kept).unwrap(), b"keep");
}

/// The env that turns [`probe_shell_setup_under_an_open_umask_shares_no_directory`] on:
/// the scratch directory its account lives in.
const OPEN_UMASK_ENV: &str = "ATPKG_SELFSETUP_OPEN_UMASK_DIR";
/// That probe's name, as `--exact` wants it.
const OPEN_UMASK_PROBE: &str =
    "selfsetup::tests::probe_shell_setup_under_an_open_umask_shares_no_directory";

/// The child half of [`shell_setup_under_an_open_umask_shares_no_directory`]: an account
/// as `tools/install.sh` leaves it before shell setup (a private HOME, `~/.local/bin` made
/// `0755`, no `~/.local/share`), then shell setup's man page, desktop entry and icons under
/// umask `002`. Every directory it made is closed to group and other
/// writes, `~/.local/share` (the atpkg prefix's ancestor) `0755`. Outside that child (the
/// env unset) it does nothing: the umask is the whole process's.
#[test]
fn probe_shell_setup_under_an_open_umask_shares_no_directory() {
    let Some(dir) = std::env::var_os(OPEN_UMASK_ENV) else {
        return;
    };
    // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
    // process runs this one test (`--exact`), so no other create sees the mask.
    unsafe { libc::umask(0o022) };
    let home = PathBuf::from(dir).join("home");
    std::fs::create_dir(&home).expect("mkdir home");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let mut places = Places::defaults(&home, None, None);
    places.install_dir = Some(home.join("Applications"));
    let bin = places.bin_dir.join("aterm");
    exe(&bin, "#!/bin/sh\n");
    let before = tree(&home);
    // SAFETY: as above.
    unsafe { libc::umask(0o002) };
    let setup = Setup {
        own_man: true,
        desktop: true,
        ..Setup::default()
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert!(
        shell_setup(&places, &zsh(""), &bin, &setup, &mut out, &mut err),
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    // Octal text, so a failure reads `775`, not `509`.
    let mode = |rel: &str| {
        let meta = std::fs::symlink_metadata(home.join(rel)).expect("made");
        format!("{:o}", meta.permissions().mode() & 0o7777)
    };
    let made: Vec<String> = tree(&home)
        .into_iter()
        .filter(|rel| !before.contains(rel) && home.join(rel).is_dir())
        .collect();
    assert!(
        made.iter().any(|rel| rel == ".local/share/man/man1"),
        "the man page's directories were made: {made:?}"
    );
    #[cfg(target_os = "linux")]
    assert!(
        made.iter().any(|rel| rel == ".local/share/applications"),
        "the desktop entry's directory was made: {made:?}"
    );
    assert_eq!(mode(".local/share"), "755", "the prefix's ancestor");
    let shared: Vec<String> = made
        .iter()
        .map(|rel| format!("{rel} {}", mode(rel)))
        .filter(|row| !row.ends_with(" 755"))
        .collect();
    assert!(shared.is_empty(), "made other than 755: {shared:?}");
}

/// Shell setup is the first writer under `~/.local/share` on a fresh Linux account
/// (`tools/install.sh` runs it before the toolchain lands), and it made its directories
/// with `create_dir_all`'s `0777 & ~umask`: under umask `002` (a user-private-group
/// login's default) `~/.local/share` came out `0775`, a group-writable directory above the
/// atpkg prefix, so `targo trust` refused every Trust toolchain installed below it. The
/// umask is process-wide, so the setup runs in a child of this test binary that runs only
/// [`probe_shell_setup_under_an_open_umask_shares_no_directory`].
#[test]
fn shell_setup_under_an_open_umask_shares_no_directory() {
    let scratch = crate::testdir::dir("atpkg-selfsetup-umask-");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
        .env(OPEN_UMASK_ENV, scratch.as_os_str())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("run the probe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "shell setup under umask 002: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The Caffeine mug's leftovers are swept through the going bundle's own aterm — asked
/// `help caffeine` first, then `caffeine startup off` and `caffeine quit` — and its files in
/// the support dir go after a free lock is held (the rendezvous stays); a dry run runs no
/// helper. Refused or unsupported helpers and busy/missing locks preserve the files.
#[test]
fn an_uninstall_quits_the_caffeine_mug_and_removes_its_files() {
    let a = Account::new("caffeine");
    let home = a.home().to_path_buf();
    let log = a.dir.join("asked.log");
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    // Absolute paths: the child inherits this test process's HOME, which is the real one.
    exe(
        &aterm,
        &format!("#!/bin/sh\necho \"$*\" >>'{}'\n", log.display()),
    );
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(
        !log.exists(),
        "no mug ran here, so no aterm was asked: {out}"
    );

    let a = Account::new("caffeine-left");
    let home = a.home().to_path_buf();
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    exe(
        &aterm,
        &format!("#!/bin/sh\necho \"$*\" >>'{}'\n", log.display()),
    );
    let support = home.join("Library/Application Support/aterm");
    write(&support.join("caffeine.state"), "mode=off\n");
    write(&support.join("caffeine.lock"), "");
    write(&support.join("caffeine/l.plist"), "<plist/>");
    write(
        &support.join("caffeine-code.noindex/l/aterm.app/Contents/Info.plist"),
        "<plist/>",
    );
    let (ok, out, err) = a.sweep(None, true);
    assert!(ok, "{out}{err}");
    for step in ["caffeine startup off", "caffeine quit"] {
        assert!(
            out.contains(&format!("would run {} {step}", aterm.display())),
            "{out}"
        );
    }
    assert!(
        support.join("caffeine.state").exists(),
        "a dry run keeps it"
    );
    assert!(!log.exists(), "a dry run starts no helper");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&log).expect("asked"),
        "help caffeine\ncaffeine startup off\ncaffeine quit\n"
    );
    for name in ["caffeine-code.noindex", "caffeine", "caffeine.state"] {
        assert!(!present(&support.join(name)), "{name}: {out}");
    }
    assert!(
        support.join("caffeine.lock").is_file(),
        "the rendezvous stays"
    );
    assert!(!app.exists(), "the bundle went after its aterm was asked");

    for kind in ["quit-fails", "unsupported", "busy", "missing"] {
        let a = Account::new(kind);
        let support = a.home().join("Library/Application Support/aterm");
        let app = a.home().join("Applications/aterm.app");
        bundle(&app, None);
        let refuse = match kind {
            "quit-fails" => "caffeine quit",
            "unsupported" => "help caffeine",
            _ => "never",
        };
        exe(
            &app.join("Contents/MacOS/aterm"),
            &format!("#!/bin/sh\n[ \"$*\" = '{refuse}' ] && exit 1\nexit 0\n"),
        );
        write(&support.join("caffeine.state"), "mode=off\n");
        write(&support.join("caffeine-code.noindex/keep"), "copy bytes");
        let lock = support.join("caffeine.lock");
        if kind != "missing" {
            write(&lock, "");
        }
        let held = (kind == "busy").then(|| {
            crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold")
        });
        let before = tree(&support);
        let (ok, out, err) = a.sweep(None, false);
        assert!(
            !ok,
            "unconfirmed cleanup must report a partial result: {kind}: {out}{err}"
        );
        assert_eq!(tree(&support), before, "{kind}");
        assert_eq!(
            std::fs::read_to_string(support.join("caffeine.state")).expect("state"),
            "mode=off\n"
        );
        assert_eq!(
            std::fs::read_to_string(support.join("caffeine-code.noindex/keep")).expect("copy"),
            "copy bytes"
        );
        drop(held);
    }
}
