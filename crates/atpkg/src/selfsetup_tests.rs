// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `self-uninstall` and `shell-setup` over scratch accounts.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::*;

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

fn exe(path: &Path, body: &str) {
    write(path, body);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

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

#[test]
fn uninstall_removes_only_what_it_placed() {
    let mut a = Account::new("own");
    let store = a.dir.join("store");
    std::fs::create_dir_all(store.join(".aterm-update")).expect("mkdir");
    exe(&store.join("backup.sh"), "#!/bin/sh\necho backup\n");
    exe(&store.join("aterm"), "#!/bin/sh\necho \"aterm 0.0.0\"\n");
    write(&store.join(".aterm-install.Ab12Cd"), "");
    for alias in ["aterm-cli", "aterm-ctl", "atpkg", "aterm-gui", "aterm-next"] {
        symlink("aterm", store.join(alias)).expect("alias");
    }
    symlink("backup.sh", store.join("backup")).expect("a foreign link");
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

#[test]
fn shell_setup_selects_the_cli_before_an_inherited_bundle_and_preserves_the_profile() {
    let fish = aterm_spec::host::need_tool("fish");
    for shell in [
        Path::new("/bin/zsh"),
        Path::new("/bin/bash"),
        fish.as_path(),
    ] {
        let is_fish = shell.file_name().is_some_and(|name| name == "fish");
        let mut account = Account::new("selected-cli");
        account.places.bin_dir = account.home().join("CLI $selected `app` 'links'");
        let bin = &account.places.bin_dir;
        let release = account.home().join("aterm.app/Contents/MacOS");
        let dev = account.home().join("aterm (dev).app/Contents/MacOS");
        exe(
            &release.join("aterm"),
            "#!/bin/sh\nprintf '%s\\n' release\n",
        );
        exe(&dev.join("aterm"), "#!/bin/sh\nprintf '%s\\n' selected\n");
        std::fs::create_dir_all(bin).unwrap();
        symlink(dev.join("aterm"), bin.join("aterm")).unwrap();
        let package = account.home().join("package/bin");
        let agents = account.home().join("package/agents");
        let reroute = account.home().join("package/reroute");
        for (name, body) in crate::hooks::hook_files(&package, &agents, &reroute) {
            write(&account.home().join(".aterm/shell.d").join(name), body);
        }
        let inherited = format!(
            "{}:/usr/bin::/bin:{}:{}",
            release.display(),
            bin.display(),
            bin.display()
        );
        let env = ShellEnv {
            shell: Some(shell.to_string_lossy().into_owned()),
            path: Some(inherited.clone().into()),
            manpath: None,
        };
        let rc = rc_target(&account.places, &env).unwrap();
        let target = account.home().join("profiles/interactive");
        let prefix = if is_fish {
            "# user prefix\nset -gx UNCHANGED kept\n"
        } else {
            "# user prefix\nexport UNCHANGED=kept\n"
        };
        let suffix = "# user suffix\n";
        write(
            &target,
            format!(
                "{prefix}{PATH_BLOCK_START}\n# older append-only CLI block\n{PATH_BLOCK_END}\n{suffix}"
            ),
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::create_dir_all(rc.parent().unwrap()).unwrap();
        symlink(&target, &rc).unwrap();
        let setup = Setup {
            path: true,
            ..Setup::default()
        };
        let (ok, out, err) = account.setup(&env, &dev.join("aterm"), &setup);
        assert!(ok, "{out}{err}");
        let updated = std::fs::read_to_string(&target).unwrap();
        assert!(rc.is_symlink(), "the profile's link belongs to the user");
        assert!(updated.starts_with(prefix) && updated.ends_with(suffix));
        assert_eq!(target.metadata().unwrap().mode() & 0o777, 0o600);
        assert!(account.setup(&env, &dev.join("aterm"), &setup).0);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), updated);

        let mut command = aterm_uds::spawnfd::command(shell);
        let args = if is_fish {
            [
                "--no-config",
                "-c",
                "aterm --version; source \"$PROFILE\"; source \"$PROFILE\"; aterm init --help; printf '%s\\n' (string join : $PATH) \"$UNCHANGED\"",
            ]
        } else {
            [
                "-f",
                "-c",
                "aterm --version; . \"$PROFILE\"; . \"$PROFILE\"; aterm init --help; printf '%s\\n' \"$PATH\" \"$UNCHANGED\"",
            ]
        };
        command.args(args).env_clear();
        for (key, value) in aterm_spec::host::hermetic_env(account.home()) {
            command.env(key, value);
        }
        command
            .env("PATH", &inherited)
            .env("PROFILE", &rc)
            .env("ATERM_CHILD", "1");
        let output = aterm_uds::deadline::output_until(
            &mut command,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "release\nselected\n{}:{}:{}:{}:/usr/bin:{}:/bin:{}\nkept\n",
                reroute.display(),
                agents.display(),
                bin.display(),
                release.display(),
                if is_fish { "." } else { "" },
                package.display()
            )
        );
        let incomplete = format!("{prefix}{PATH_BLOCK_START}\n# interrupted old setup\n");
        write(&target, &incomplete);
        let (ok, _, _) = account.setup(&env, &dev.join("aterm"), &setup);
        assert!(!ok);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), incomplete);
    }
}

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
        text.contains(&format!("__aterm_cli_bin=\"{bin}\"")),
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

    std::fs::remove_file(&bin).expect("rm the stand-in");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(!entry.exists(), "{out}");
    assert!(placed.iter().all(|icon| !icon.exists()), "{out}");

    write(&entry, "[Desktop Entry]\nName=the X11 aterm\n");
    write(&placed[0], "png");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a SKIPPED item is the non-zero exit: {out}{err}");
    assert!(entry.exists() && placed[0].exists(), "{out}{err}");
    assert!(err.contains("missing its marker line"), "{err}");
}

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

    std::fs::remove_file(&bin).expect("rm the stand-in");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(!page.exists(), "{out}");
    assert!(icons.iter().all(|icon| !icon.exists()), "{out}");

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
        let env = ShellEnv::default();
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

const OPEN_UMASK_ENV: &str = "ATPKG_SELFSETUP_OPEN_UMASK_DIR";
const OPEN_UMASK_PROBE: &str =
    "selfsetup::tests::probe_shell_setup_under_an_open_umask_shares_no_directory";

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

#[test]
fn an_uninstall_quits_the_caffeine_mug_and_removes_its_files() {
    let a = Account::new("caffeine");
    let home = a.home().to_path_buf();
    let log = a.dir.join("asked.log");
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
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
    write(&support.join("caffeine.bridge"), "4242\n");
    write(&support.join("caffeine/l.plist"), "<plist/>");
    write(
        &support.join("caffeine-code.noindex/l/1-2/aterm.app/Contents/Info.plist"),
        "<plist/>",
    );
    write(&support.join("caffeine-code.noindex/l/1-2/ran"), "");
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
    for name in [
        "caffeine-code.noindex",
        "caffeine",
        "caffeine.state",
        "caffeine.bridge",
    ] {
        assert!(!present(&support.join(name)), "{name}: {out}");
    }
    assert!(
        support.join("caffeine.lock").is_file(),
        "the rendezvous stays"
    );
    assert!(!app.exists(), "the bundle went after its aterm was asked");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(out.contains("left Caffeine lock rendezvous"), "{out}");
    assert!(support.join("caffeine.lock").is_file());
    let lock = support.join("caffeine.lock");
    let held =
        crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a held lock is a running mug: {out}{err}");
    assert!(err.contains("Caffeine lock is busy"), "{err}");
    drop(held);
    let plist = home.join("Library/LaunchAgents/com.aterm.aterm.caffeine.plist");
    write(&plist, "<plist/>");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a login plist starts a mug: {out}{err}");
    assert!(err.contains("Caffeine stop was not confirmed"), "{err}");
    assert!(plist.is_file() && lock.is_file());

    let a = Account::new("caffeine-quit-acts");
    let home = a.home().to_path_buf();
    let support = home.join("Library/Application Support/aterm");
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    exe(
        &app.join("Contents/MacOS/aterm"),
        &format!(
            "#!/bin/sh\n[ \"$*\" = 'caffeine quit' ] || exit 0\nrm -rf '{code}'\n\
             echo mode=off >'{state}'\n",
            code = support.join("caffeine-code.noindex").display(),
            state = support.join("caffeine.state").display()
        ),
    );
    write(
        &support.join("caffeine-code.noindex/l/aterm.app/Contents/Info.plist"),
        "<plist/>",
    );
    write(&support.join("caffeine/l.plist"), "<plist/>");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    for name in ["caffeine-code.noindex", "caffeine", "caffeine.state"] {
        assert!(!present(&support.join(name)), "{name}: {out}");
    }
    assert!(
        support.join("caffeine.lock").is_file(),
        "the rendezvous made for the cleanup stays: {out}"
    );

    for kind in ["quit-fails", "unsupported", "busy", "lock-only-quit-fails"] {
        let a = Account::new(kind);
        let support = a.home().join("Library/Application Support/aterm");
        let app = a.home().join("Applications/aterm.app");
        bundle(&app, None);
        let refuse = match kind {
            "quit-fails" | "lock-only-quit-fails" => "caffeine quit",
            "unsupported" => "help caffeine",
            _ => "never",
        };
        exe(
            &app.join("Contents/MacOS/aterm"),
            &format!("#!/bin/sh\n[ \"$*\" = '{refuse}' ] && exit 1\nexit 0\n"),
        );
        let lock_only = kind == "lock-only-quit-fails";
        if !lock_only {
            write(&support.join("caffeine.state"), "mode=off\n");
            write(&support.join("caffeine-code.noindex/keep"), "copy bytes");
        }
        let lock = support.join("caffeine.lock");
        write(&lock, "");
        let held = (kind == "busy" || lock_only).then(|| {
            crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold")
        });
        let before = tree(&support);
        let (ok, out, err) = a.sweep(None, false);
        assert!(
            !ok,
            "unconfirmed cleanup must report a partial result: {kind}: {out}{err}"
        );
        assert_eq!(tree(&support), before, "{kind}");
        if !lock_only {
            assert_eq!(
                std::fs::read_to_string(support.join("caffeine.state")).expect("state"),
                "mode=off\n"
            );
            assert_eq!(
                std::fs::read_to_string(support.join("caffeine-code.noindex/keep")).expect("copy"),
                "copy bytes"
            );
        }
        assert_eq!(
            app.exists(),
            kind == "quit-fails" || lock_only,
            "only a failed step keeps its bundle: {kind}: {out}"
        );
        drop(held);
    }

    let a = Account::new("caffeine-mediated-help-refused");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    exe(&aterm, "#!/bin/sh\nexit 99\n");
    let image = std::fs::read(&aterm).expect("the helper");
    let support = a.home().join("Library/Application Support/aterm");
    let state = support.join("caffeine.state");
    write(&state, "mode=on\n");
    let preflight = as_account_argv(501, Path::new(TRUE), &[]);
    let help = as_account_argv(501, &aterm, &["help", "caffeine"]);
    let mut calls = Vec::new();
    let mut failed = Vec::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok =
        self_uninstall_with_caffeine(&a.places, None, false, &mut out, &mut err, |s, p, dirs| {
            failed = sweep_caffeine_with(
                s,
                p,
                dirs,
                0,
                || Some((501, 20)),
                |program, args| {
                    assert_eq!(program, Path::new(SUDO));
                    calls.push(args.to_vec());
                    if args == preflight.as_slice() {
                        0
                    } else {
                        assert_eq!(args, help.as_slice());
                        1
                    }
                },
            );
            failed.clone()
        });
    assert_eq!(calls, [preflight, help]);
    assert_eq!(
        failed.as_slice(),
        std::slice::from_ref(&app),
        "the unconfirmed helper needs its app"
    );
    assert!(!ok, "a partial sweep reports what it kept");
    assert!(app.is_dir(), "the actual caller kept the helper's bundle");
    assert_eq!(std::fs::read(aterm).expect("kept helper"), image);
    assert_eq!(std::fs::read_to_string(state).expect("state"), "mode=on\n");
}

#[test]
fn a_root_sweep_asks_caffeine_as_the_homes_owner() {
    let me = aterm_uds::peer::our_uid();
    assert_ne!(me, 0, "the tests run unprivileged");
    let own = aterm_uds::account::home_of(me).expect("this account's home");
    let meta = std::fs::metadata(&own).expect("home");
    assert_eq!(
        caffeine_account(0, &own),
        Some((me, meta.gid())),
        "root, the account's own home"
    );
    assert_eq!(caffeine_account(me, &own), None, "not root");
    let a = Account::new("caffeine-account");
    assert_eq!(
        caffeine_account(0, a.home()),
        None,
        "the account's directory, not its home"
    );
    assert_eq!(caffeine_account(0, Path::new("/")), None, "root's own");
    assert_eq!(caffeine_account(0, &a.home().join("missing")), None);
    let aterm = Path::new("/A/aterm.app/Contents/MacOS/aterm");
    let args = ["caffeine", "quit", "own"];
    assert_eq!(
        as_account_argv(501, aterm, &args),
        [
            "-n",
            "-H",
            "-u",
            "#501",
            "--",
            "/A/aterm.app/Contents/MacOS/aterm"
        ]
        .into_iter()
        .chain(args)
        .map(OsString::from)
        .collect::<Vec<_>>()
    );
    assert_eq!(
        helper_line(aterm, &args, Some((501, 20))),
        "/usr/bin/sudo -n -H -u #501 -- /A/aterm.app/Contents/MacOS/aterm caffeine quit own"
    );
    assert_eq!(
        helper_line(aterm, &args, None),
        "/A/aterm.app/Contents/MacOS/aterm caffeine quit own"
    );
}

#[test]
fn a_root_sweep_runs_nothing_an_account_could_replace() {
    let me = aterm_uds::peer::our_uid();
    assert_ne!(me, 0, "the tests run unprivileged");
    let own = aterm_uds::account::home_of(me).expect("this account's home");
    let a = Account::new("runs-as");
    let ran = a.dir.join("ran");
    let planted = a.home().join(".local/lib/aterm/bin/aterm");
    exe(
        &planted,
        &format!("#!/bin/sh\n: >'{}'\necho 'aterm 9.9.0'\n", ran.display()),
    );
    let this = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .expect("this test binary");
    let through = a.dir.join("through-a-link");
    std::os::unix::fs::symlink(&this, &through).expect("a link to it");
    let roots = aterm_uds::account::home_of(0).expect("root's home");
    assert_eq!(runs_as(me, a.home(), &planted), RunAs::Me, "not root");
    assert_eq!(
        runs_as(0, &own, &planted),
        RunAs::Account(me),
        "sudo's HOME"
    );
    assert_eq!(runs_as(0, a.home(), &planted), RunAs::Nobody);
    assert_eq!(runs_as(0, &roots, &planted), RunAs::Nobody, "root's HOME");
    assert_eq!(runs_as(0, &roots, Path::new("/bin/sh")), RunAs::Me);
    assert_eq!(runs_as(0, &roots, &this), RunAs::Me, "root runs it already");
    assert_eq!(
        runs_as(0, a.home(), Path::new("/bin/sh")),
        RunAs::Nobody,
        "root's files in another's home"
    );
    assert_eq!(runs_as(0, a.home(), &this), RunAs::Nobody);
    assert_eq!(
        runs_as(0, &roots, &through),
        RunAs::Nobody,
        "a link its owner may repoint"
    );
    assert_eq!(setup_refusal(me, &own, &[]), None, "not root");
    assert_eq!(setup_refusal(0, &roots, &[]), None, "root's own home");
    let asked = ["--path", "--man-root", "/a b"].map(String::from);
    let accounts = setup_refusal(0, &own, &asked).expect("an account's home");
    assert!(
        accounts.contains(&format!("is uid {me}'s own home"))
            && accounts
                .contains("run `aterm pkg shell-setup --path --man-root '/a b'` as that account"),
        "{accounts}"
    );
    let other = setup_refusal(0, a.home(), &[]).expect("a home nobody's own");
    assert!(other.contains("is not root's own home"), "{other}");
    for (uid, gid, mode, dir, link, alone) in [
        (0, 0, 0o040_755, true, false, true),
        (0, 80, 0o040_775, true, false, false),
        (0, 0, 0o040_775, true, false, true),
        (0, 0, 0o040_777, true, false, false),
        (0, 0, 0o041_777, true, false, true),
        (0, 0, 0o101_777, false, false, false),
        (0, 0, 0o100_755, false, false, true),
        (501, 0, 0o040_755, true, false, false),
        (0, 0, 0o120_777, false, true, true),
        (501, 0, 0o120_755, false, true, false),
    ] {
        assert_eq!(
            root_only_entry(uid, gid, mode, dir, link),
            alone,
            "uid {uid} gid {gid} mode {mode:o}"
        );
    }

    let bin = a.dir.join("bin");
    let refreshed = a.dir.join("refreshed");
    exe(
        &bin.join("update-desktop-database"),
        &format!("#!/bin/sh\n: >'{}'\n", refreshed.display()),
    );
    aterm_spec::wait::first_exec(&bin.join("update-desktop-database"), &[]);
    std::fs::remove_file(&refreshed).expect("the warm-up's mark");
    let env = ShellEnv {
        shell: None,
        path: Some(bin.clone().into()),
        manpath: None,
    };
    refresh_desktop_database(&env, &a.places.xdg_data, |_| RunAs::Nobody);
    refresh_desktop_database(&env, &a.places.xdg_data, |_| RunAs::Account(me));
    assert!(!refreshed.exists(), "not run but as this process");
    refresh_desktop_database(&env, &a.places.xdg_data, |_| RunAs::Me);
    assert!(refreshed.exists());
    assert!(home_is_the_accounts(me, &own));
    assert!(home_is_the_accounts(0, &own), "sudo's HOME is the person's");
    assert!(!home_is_the_accounts(me, a.home()));
    assert!(!home_is_the_accounts(0, a.home()));

    aterm_spec::wait::first_exec(&planted, &["--version"]);
    std::fs::remove_file(&ran).expect("the warm-up's mark");
    assert!(!store_entry_is_ours(&planted, RunAs::Nobody));
    assert!(!ran.exists(), "not run for nobody");
    assert!(store_entry_is_ours(&planted, RunAs::Me));
    assert!(ran.exists(), "asked as this process");

    let a = Account::new("caffeine-root-nobody");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    exe(&app.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 0\n");
    write(
        &a.home()
            .join("Library/Application Support/aterm/caffeine.state"),
        "mode=on\n",
    );
    let mut failed = Vec::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok =
        self_uninstall_with_caffeine(&a.places, None, false, &mut out, &mut err, |s, p, dirs| {
            failed = sweep_caffeine_with(
                s,
                p,
                dirs,
                0,
                || None,
                |program, args| panic!("root ran {} {args:?}", program.display()),
            );
            failed.clone()
        });
    let err = String::from_utf8(err).expect("utf-8");
    assert!(!ok, "{err}");
    assert!(err.contains(NOT_AS_ROOT), "{err}");
    assert_eq!(failed, std::slice::from_ref(&app));
    assert!(app.is_dir(), "kept for its steps");
}

#[test]
fn the_caffeine_cleanup_holds_only_the_accounts_lock() {
    thread_local! {
        static MADE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    fn refuse(_: &Path, _: u32) -> bool {
        panic!("only a root sweep's missing lock is made by its account")
    }
    fn cannot(_: &Path, _: u32) -> bool {
        MADE.with(|n| n.set(n.get() + 1));
        false
    }
    fn makes(path: &Path, _: u32) -> bool {
        MADE.with(|n| n.set(n.get() + 1));
        std::fs::write(path, b"").is_ok()
    }
    fn gone(_: &Path, _: u32) -> bool {
        MADE.with(|n| n.set(n.get() + 1));
        true
    }
    let dir = crate::testdir::dir("atpkg-caffeine-hold-");
    let lock = dir.join("caffeine.lock");
    let me = aterm_uds::peer::our_uid();
    let gid = std::fs::metadata(&dir).expect("dir").gid();
    assert!(caffeine_cleanup_hold(&lock, Some((me, gid)), cannot).is_none());
    assert!(caffeine_cleanup_hold(&lock, Some((me, gid)), gone).is_none());
    assert!(!lock.exists(), "made by nobody but its account");
    let held = caffeine_cleanup_hold(&lock, Some((me, gid)), makes).expect("its account's");
    assert_eq!(MADE.with(std::cell::Cell::get), 3);
    let mode = std::fs::metadata(&lock).expect("lock").mode();
    assert_eq!(mode & 0o777, 0o600, "private, as a mug makes it");
    drop(held);
    assert!(caffeine_cleanup_hold(&lock, Some((me + 1, gid)), refuse).is_none());
    let second = dir.join("second-name");
    std::fs::hard_link(&lock, &second).expect("link");
    assert!(caffeine_cleanup_hold(&lock, Some((me, gid)), refuse).is_none());
    assert!(caffeine_cleanup_hold(&lock, None, refuse).is_none());
    std::fs::remove_file(&second).expect("unlink");
    std::fs::remove_file(&lock).expect("unlink");
    assert!(caffeine_cleanup_hold(&lock, None, refuse).is_some());
    assert!(lock.is_file(), "this process's own sweep makes it");
}

#[test]
fn an_uninstall_keeps_a_caffeine_copy_macos_may_still_be_assessing() {
    use std::cell::Cell;
    use std::io::{Error, ErrorKind};

    fn sweep(
        code: &Path,
        now: std::time::SystemTime,
        metadata: &impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
        entries: &impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
    ) -> (usize, usize, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let counts = {
            let mut s = Sweep {
                dry: false,
                as_root: None,
                removed: 0,
                skipped: 0,
                kept: 0,
                shared_kept: 0,
                outsider: None,
                out: &mut out,
                err: &mut err,
            };
            sweep_caffeine_code_with(&mut s, code, now, metadata, entries);
            (s.removed, s.skipped)
        };
        (
            counts.0,
            counts.1,
            String::from_utf8(out).expect("out"),
            String::from_utf8(err).expect("err"),
        )
    }

    let a = Account::new("caffeine-young");
    let code = a
        .home()
        .join("Library/Application Support/aterm/caffeine-code.noindex");
    let plist = "aterm.app/Contents/Info.plist";
    for copy in ["a/9-0", "l/9-1", "l/9-2", "l/9-3.partial", "l/9-4", "m/9-5"] {
        write(&code.join(copy).join(plist), "<plist/>");
    }
    for ran in ["a/9-0", "l/9-2", "m/9-5"] {
        write(&code.join(ran).join(aterm_uds::deadline::RAN), "");
    }
    let stamped = aterm_uds::process::awake_ms().map(|awake| {
        let copy = code.join("l/9-6");
        write(&copy.join(plist), "<plist/>");
        write(
            &copy.join(aterm_uds::deadline::EXEC_STAMP),
            awake.to_string(),
        );
        copy
    });
    write(&code.join("stray"), "");
    let now = std::time::SystemTime::now();
    let made = now - aterm_uds::deadline::FIRST_EXEC_HANG - Duration::from_secs(60);
    for aged in std::iter::once(code.join("l/9-4")).chain(stamped.iter().cloned()) {
        std::fs::File::open(aged)
            .and_then(|dir| dir.set_modified(made))
            .expect("age the copy");
    }
    let before = tree(&code);
    for (bad_meta, bad_list, partial) in [
        (Some(code.clone()), None, false),
        (Some(code.join("l")), None, false),
        (Some(code.join("l/9-2")), None, false),
        (None, Some(code.clone()), false),
        (None, Some(code.join("l")), false),
        (None, Some(code.clone()), true),
        (None, Some(code.join("l")), true),
    ] {
        let refused = Cell::new(false);
        let metadata = |path: &Path| {
            if bad_meta.as_deref() == Some(path) {
                refused.set(true);
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "injected metadata refusal",
                ));
            }
            std::fs::symlink_metadata(path)
        };
        let entries = |path: &Path| {
            if bad_list.as_deref() == Some(path) {
                refused.set(true);
                if partial {
                    let first = if path == code {
                        code.join("a")
                    } else {
                        code.join("l/9-2")
                    };
                    return collect_caffeine_code_entries([
                        Ok(first),
                        Err(Error::new(ErrorKind::Interrupted, "injected entry refusal")),
                    ]);
                }
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "injected enumeration refusal",
                ));
            }
            caffeine_code_entries(path)
        };
        let (removed, skipped, out, err) = sweep(&code, now, &metadata, &entries);
        assert!(refused.get(), "the injected refusal was reached");
        assert_eq!((removed, skipped), (0, 1), "{out}{err}");
        assert_eq!(tree(&code), before, "{out}{err}");
        assert_eq!(
            std::fs::read_to_string(code.join("l/9-1").join(plist)).expect("young stays"),
            "<plist/>"
        );
    }

    let entries = |path: &Path| {
        let mut entries = caffeine_code_entries(path)?;
        if path == code {
            entries.push(code.join("already-gone"));
        }
        Ok(entries)
    };
    let (removed, skipped, out, err) = sweep(
        &code,
        now,
        &|path| std::fs::symlink_metadata(path),
        &entries,
    );
    assert!(
        removed > 0,
        "the positive control removes admitted entries: {out}{err}"
    );
    assert_eq!(skipped, 1 + usize::from(stamped.is_some()), "{out}{err}");
    assert!(code.join("l/9-1").join(plist).is_file());
    if let Some(stamped) = stamped {
        assert!(
            stamped.join(plist).is_file(),
            "kept on the awake clock: {err}"
        );
        write(&stamped.join(aterm_uds::deadline::RAN), "");
    }
    for gone in ["a", "l/9-2", "l/9-3.partial", "l/9-4", "m", "stray"] {
        assert!(!present(&code.join(gone)), "{gone}: {out}");
    }
    write(&code.join("l/9-1").join(aterm_uds::deadline::RAN), "");
    for expected_removed in [1, 0] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut s = Sweep {
            dry: false,
            as_root: None,
            removed: 0,
            skipped: 0,
            kept: 0,
            shared_kept: 0,
            outsider: None,
            out: &mut out,
            err: &mut err,
        };
        sweep_caffeine_code(&mut s, &code, now);
        assert_eq!((s.removed, s.skipped), (expected_removed, 0));
        assert!(!present(&code));
    }
}

static AS_ACCOUNT: std::sync::Mutex<Vec<Vec<OsString>>> = std::sync::Mutex::new(Vec::new());

fn as_the_account(program: &Path, argv: &[OsString]) -> i32 {
    assert_eq!(program, Path::new(SUDO));
    AS_ACCOUNT.lock().expect("calls").push(argv.to_vec());
    let path = Path::new(argv.last().expect("a path"));
    let done = if argv.iter().any(|arg| arg == RM) {
        remove(path)
    } else {
        std::fs::remove_dir(path)
    };
    i32::from(done.is_err())
}

const fn sudo_refused(_: &Path, _: &[OsString]) -> i32 {
    1
}

#[test]
fn a_root_sweep_changes_an_accounts_folder_only_as_that_account() {
    use std::os::unix::ffi::OsStrExt as _;
    let me = aterm_uds::peer::our_uid();
    assert_ne!(me, 0, "these tests run as an account");
    assert_eq!(way_account([]), Ok(None));
    assert_eq!(way_account([0, 0]), Ok(None));
    assert_eq!(way_account([0, 501, 501, 0]), Ok(Some(501)));
    assert_eq!(way_account([501, 0, 502]), Err(()));

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    assert_eq!(account_on_the_way(&home.join("x")), Ok(Some(me)));
    assert_eq!(account_on_the_way(Path::new("/usr/bin/true")), Ok(None));
    symlink("/usr", home.join("usr")).expect("symlink");
    assert_eq!(account_on_the_way(&home.join("usr/bin/true")), Ok(Some(me)));

    let file = home.join("store/aterm");
    let empty = home.join("support");
    write(&file, "");
    std::fs::create_dir(&empty).expect("mkdir");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut s = Sweep {
        dry: false,
        as_root: Some(as_the_account),
        removed: 0,
        skipped: 0,
        kept: 0,
        shared_kept: 0,
        outsider: None,
        out: &mut out,
        err: &mut err,
    };
    s.rm(&file, "store entry");
    assert!(s.unlink(&empty, false).is_ok());
    s.rm(Path::new("/usr/aterm-no-such-entry"), "store entry");
    assert_eq!((s.removed, s.skipped), (1, 0));
    assert!(!present(&file) && !present(&empty));
    let account = format!("#{me}");
    let argv = |program: &str, args: &[&str], path: &Path| {
        let mut argv: Vec<OsString> = ["-n", "-H", "-u", &account, "--", program]
            .iter()
            .chain(args)
            .map(OsString::from)
            .collect();
        argv.push(path.into());
        argv
    };
    assert_eq!(
        *AS_ACCOUNT.lock().expect("calls"),
        [
            argv(RM, &["-rf", "--"], &file),
            argv(RMDIR, &["--"], &empty)
        ]
    );
    s.as_root = Some(sudo_refused);
    write(&file, "");
    s.rm(&file, "store entry");
    assert_eq!((s.removed, s.skipped), (1, 1));
    assert!(present(&file));
    let (out, err) = (String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
    assert!(
        out.contains("gone already: store entry: /usr/aterm-no-such-entry"),
        "{out}"
    );
    assert!(
        err.contains(&format!(
            "SKIPPED store entry (cannot remove: uid {me}, which owns a folder on the way to it, \
             could not (exit 1))"
        )),
        "{err}"
    );

    let read = || std::fs::OpenOptions::new().read(true).clone();
    let profile = home.join(".zshrc");
    write(&profile, "x");
    assert_eq!(read_checked(&profile).expect("the account's own file"), "x");
    assert!(open_checked(&profile, std::fs::OpenOptions::new().append(true), true).is_ok());
    let roots = home.join("roots");
    symlink("/usr/bin/true", &roots).expect("symlink");
    assert!(open_checked(&roots, &mut read(), false).is_ok());
    assert!(
        open_checked(&roots, &mut read(), true).is_err(),
        "root's file behind the link"
    );
    let pipe = home.join("pipe");
    let c = std::ffi::CString::new(pipe.as_os_str().as_bytes()).expect("path");
    // SAFETY: `c` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(read_checked(&pipe).is_err()));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(60)),
        Ok(true),
        "a pipe is refused, not waited on"
    );
}

#[test]
fn a_file_gone_before_its_removal_is_not_skipped() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let gone = dir.path().join("caffeine.bridge");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut s = Sweep {
        dry: false,
        as_root: None,
        removed: 0,
        skipped: 0,
        kept: 0,
        shared_kept: 0,
        outsider: None,
        out: &mut out,
        err: &mut err,
    };
    s.rm(&gone, "Caffeine file");
    assert_eq!((s.removed, s.skipped), (0, 0));
    let blocked = dir.path().join("a-file");
    write(&blocked, "");
    s.rm(&blocked.join("inside"), "Caffeine file");
    assert_eq!((s.removed, s.skipped), (0, 1));
    let (out, err) = (String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
    assert!(out.contains("gone already: Caffeine file"), "{out}");
    assert!(!err.contains("caffeine.bridge"), "{err}");
}

#[test]
fn an_uninstall_quits_a_mug_the_stores_aterm_runs() {
    for fails in [false, true] {
        let mut a = Account::new(if fails {
            "caffeine-store-fails"
        } else {
            "caffeine-store"
        });
        let support = a.home().join("Library/Application Support/aterm");
        let store = a.dir.join("store");
        let log = a.dir.join("asked.log");
        let fail = if fails {
            "[ \"$*\" = 'caffeine quit' ] && exit 1\n"
        } else {
            ""
        };
        exe(
            &store.join("aterm"),
            &format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'aterm 0.0.0'; exit 0; }}\n\
                 echo \"$*\" >>'{}'\n{fail}exit 0\n",
                log.display()
            ),
        );
        aterm_spec::wait::first_exec(&store.join("aterm"), &["--version"]);
        a.places.store_dir = store.clone();
        write(&support.join("caffeine.state"), "mode=off\n");
        write(&support.join("caffeine.lock"), "");
        let (ok, out, err) = a.sweep(None, false);
        assert_eq!(
            std::fs::read_to_string(&log).expect("the store's aterm was asked"),
            "help caffeine\ncaffeine startup off\ncaffeine quit\n",
            "{out}{err}"
        );
        assert_eq!(ok, !fails, "{out}{err}");
        assert_eq!(store.join("aterm").exists(), fails, "{out}{err}");
        assert_eq!(
            present(&support.join("caffeine.state")),
            fails,
            "{out}{err}"
        );
    }
}

#[test]
fn a_scoped_uninstall_quits_only_its_own_mug() {
    for old in [false, true] {
        let mut a = Account::new(if old {
            "caffeine-scoped-old"
        } else {
            "caffeine-scoped"
        });
        let home = a.home().to_path_buf();
        let support = home.join("Library/Application Support/aterm");
        let log = a.dir.join("asked.log");
        bundle(&home.join("Applications/aterm.app"), None);
        let scoped = home.join("aterm.app");
        bundle(&scoped, None);
        let refuse = if old {
            "[ \"$*\" = 'caffeine quit own' ] && exit 2\n"
        } else {
            ""
        };
        exe(
            &scoped.join("Contents/MacOS/aterm"),
            &format!(
                "#!/bin/sh\necho \"$*\" >>'{}'\n{refuse}exit 0\n",
                log.display()
            ),
        );
        a.places.install_dir = Some(home.clone());
        write(&support.join("caffeine.state"), "mode=off\n");
        write(&support.join("caffeine.lock"), "");
        let (ok, out, err) = a.sweep(None, false);
        assert!(ok, "{out}{err}");
        assert_eq!(
            std::fs::read_to_string(&log).expect("asked"),
            "help caffeine\ncaffeine startup off\ncaffeine quit own\n"
        );
        assert!(!scoped.exists(), "{out}");
        assert!(
            support.join("caffeine.state").exists(),
            "the staying aterm's: {out}"
        );
        assert_eq!(out.contains("cannot quit only its own mug"), old, "{out}");
    }
}

#[test]
fn a_decline_left_for_a_kept_bundle_goes_with_it() {
    let a = Account::new("caffeine-decline");
    let home = a.home().to_path_buf();
    let support = home.join("Library/Application Support/aterm");
    let app = home.join("Applications/aterm.app");
    bundle(
        &app,
        Some(&sweeping_atpkg(&support, &a.dir.join("swept.log"))),
    );
    let aterm = app.join("Contents/MacOS/aterm");
    exe(
        &aterm,
        "#!/bin/sh\n[ \"$*\" = 'caffeine quit' ] && exit 1\nexit 0\n",
    );
    write(&support.join("caffeine.state"), "mode=off\n");
    write(&support.join("caffeine.lock"), "");
    let decline = support.join("pkg/declined");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "{out}{err}");
    assert!(app.exists(), "kept for its caffeine quit: {out}");
    assert!(
        decline.is_file(),
        "the sweep's decline stays with its aterm"
    );
    exe(&aterm, "#!/bin/sh\nexit 0\n");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(!app.exists(), "{out}");
    assert!(
        !decline.exists(),
        "the tagged decline went with the last aterm: {out}"
    );
}
