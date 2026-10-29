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

    fn sweep_within(&self, hang: Duration) -> (bool, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ok = self_uninstall_with_acaffiene(
            &self.places,
            None,
            false,
            &mut out,
            &mut err,
            sweep_acaffiene,
            hang,
        );
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
        "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo atpkg 0; exit 0; }}\necho swept >>'{}'\np='{}/pkg'\n\
         mkdir -p \"$p\" && {{ [ -f \"$p/declined\" ] || echo declined >\"$p/declined\"; }}\n",
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
    // The sweep asks this atpkg `--version` (no sweep of it runs), under a minute's wait.
    aterm_spec::wait::first_exec(&home.join("aterm.app/Contents/MacOS/atpkg"), &["--version"]);
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
                "release\nselected\n{}:{}:{}:{}:{}:/usr/bin:{}:/bin\nkept\n",
                reroute.display(),
                agents.display(),
                package.display(),
                bin.display(),
                release.display(),
                if is_fish { "." } else { "" },
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
    let bin = a.places.bin_dir.display().to_string();
    let on_path = zsh(&format!("{bin}:/usr/bin:/bin"));
    let (_, planned, _) = a.setup(
        &on_path,
        exe,
        &Setup {
            dry_run: true,
            ..path.clone()
        },
    );
    assert_eq!(
        planned,
        format!("atpkg: PATH: no edit — {bin} is on PATH and no ALab toolset is installed\n")
    );
    let (ok, out, _) = a.setup(&on_path, exe, &path);
    assert!(
        !ok,
        "no hook, none coming, the bin dir on PATH: no block: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(&rc).expect("rc"),
        "export EDITOR=vi\n"
    );

    // --no-toolchain's install: no hook and none coming, and the bin dir is off PATH, so the
    // block still carries it (round 12: no block left `aterm` off PATH behind a hand edit).
    assert_eq!(
        plan(&path),
        format!(
            "atpkg: PATH: appends ONE marker-fenced block to {} — {bin}\n",
            rc.display()
        )
    );
    let (ok, out, _) = a.setup(&env, exe, &path);
    assert!(ok, "{out}");
    assert_eq!(
        out,
        format!(
            "atpkg: added {bin} to PATH in {} — open a new shell\n",
            rc.display()
        )
    );
    let toolless = std::fs::read_to_string(&rc).expect("rc");
    assert_eq!(toolless.matches(PATH_BLOCK_START).count(), 1, "{toolless}");
    assert!(
        toolless.contains(&format!("__aterm_cli_bin=\"{bin}\"")),
        "{toolless}"
    );
    // A rerun names what the standing block carries, the bin dir alone (round 12: it said "ALab
    // toolset already on PATH" with none installed or coming); the hook's case below is the
    // control.
    let (ok, out, _) = a.setup(&env, exe, &path);
    assert!(ok, "{out}");
    assert_eq!(
        out,
        format!("atpkg: {bin} already on PATH via {}\n", rc.display())
    );
    assert_eq!(std::fs::read_to_string(&rc).expect("rc"), toolless);
    let (ok, _, _) = a.sweep(None, false);
    assert!(ok);
    assert_eq!(
        std::fs::read_to_string(&rc).expect("rc"),
        "export EDITOR=vi\n\n"
    );
    write(&rc, "export EDITOR=vi\n");

    let coming = Setup {
        toolset_coming: true,
        ..path.clone()
    };
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
    assert!(ok, "{out}");
    assert_eq!(
        out,
        format!(
            "atpkg: {bin} already on PATH via {}; the ALab toolset joins it after aterm's first \
             launch\n",
            rc.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(&rc).expect("rc"),
        text,
        "written once"
    );
    assert!(plan(&coming).contains("already wired"));
    // The control for the reruns above: with the toolset's hook laid, the standing block puts
    // the toolset on PATH, and the rerun says so.
    let hook = hook_of(&a.places, "zsh");
    write(&hook, "# the toolset hook\n");
    let (ok, out, _) = a.setup(&env, exe, &path);
    assert!(ok, "{out}");
    assert_eq!(
        out,
        format!("atpkg: ALab toolset already on PATH via {}\n", rc.display())
    );
    std::fs::remove_file(&hook).expect("rm hook");

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

/// zsh reads `$ZDOTDIR/.zshrc` and never `~/.zshrc` when `ZDOTDIR` is set, so the block goes
/// there and the sweep finds it there (round 12: it went to `~/.zshrc`, and a new zsh had no
/// `aterm` on PATH).
#[test]
fn a_zdotdir_zshrc_takes_the_path_block_and_the_sweep_finds_it() {
    let mut a = Account::new("zdotdir");
    let zdot = a.home().join(".config/zsh");
    let env = zsh("/usr/bin:/bin");
    let coming = Setup {
        path: true,
        toolset_coming: true,
        ..Setup::default()
    };
    let exe = Path::new("/nonexistent/aterm");

    // The control: with no ZDOTDIR the block lands in ~/.zshrc.
    let (ok, out, _) = a.setup(&env, exe, &coming);
    assert!(ok, "{out}");
    assert!(
        std::fs::read_to_string(a.home().join(".zshrc"))
            .expect("~/.zshrc")
            .contains(PATH_BLOCK_START)
    );
    assert!(a.sweep(None, false).0);
    std::fs::remove_file(a.home().join(".zshrc")).expect("rm ~/.zshrc");

    a.places = places_from(a.home(), |name| {
        (name == "ZDOTDIR").then(|| zdot.clone().into_os_string())
    });
    a.places.install_dir = Some(a.home().join("Applications"));
    let (ok, out, _) = a.setup(&env, exe, &coming);
    assert!(ok, "{out}");
    let rc = zdot.join(".zshrc");
    assert!(out.contains(&rc.display().to_string()), "{out}");
    let text = std::fs::read_to_string(&rc).expect("$ZDOTDIR/.zshrc");
    assert_eq!(text.matches(PATH_BLOCK_START).count(), 1, "{text}");
    assert!(
        !a.home().join(".zshrc").exists(),
        "~/.zshrc is not zsh's here"
    );
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(
        out.contains(&format!("removed PATH block: {}", rc.display())),
        "{out}"
    );
    assert!(
        !std::fs::read_to_string(&rc)
            .expect("$ZDOTDIR/.zshrc")
            .contains(PATH_BLOCK_START)
    );
}

/// `ZDOTDIR` names zsh's folder only as an absolute path; empty or relative, `~` is it.
#[test]
fn only_an_absolute_zdotdir_moves_the_zshrc() {
    let home = Path::new("/h");
    for (value, want) in [
        (None, "/h"),
        (Some(""), "/h"),
        (Some("dots"), "/h"),
        (Some("/h/.config/zsh"), "/h/.config/zsh"),
    ] {
        let places = places_from(home, |name| {
            (name == "ZDOTDIR")
                .then_some(value)
                .flatten()
                .map(OsString::from)
        });
        assert_eq!(places.zdotdir, Path::new(want), "{value:?}");
    }
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

/// Under a `ZDOTDIR`, the zsh-completions NOTE asks `$ZDOTDIR/.zshrc` for the `fpath` line and
/// names it, since zsh never reads `~/.zshrc` there: the loader in `$ZDOTDIR/.zshrc` quiets the
/// NOTE, and one only in `~/.zshrc` does not (the control: with no `ZDOTDIR`, that one does).
#[test]
fn the_completions_note_reads_the_zshrc_zsh_reads() {
    let mut a = Account::new("compzdot");
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
    let loader = format!(
        "fpath=(\"{}\" $fpath)\n",
        a.places.xdg_data.join("zsh/site-functions").display()
    );
    let home_rc = a.home().join(".zshrc");
    let zdot = a.home().join(".config/zsh");
    let zdot_rc = zdot.join(".zshrc");
    let note = |a: &Account| {
        let (ok, out, err) = a.setup(&zsh("/usr/bin"), &stand_in, &setup);
        assert!(ok, "{out}{err}");
        assert!(a.sweep(None, false).0);
        err.lines()
            .find(|l| l.contains("NOTE: zsh completions"))
            .map(String::from)
    };

    write(&home_rc, &loader);
    assert_eq!(note(&a), None, "no ZDOTDIR: ~/.zshrc's loader counts");

    a.places = places_from(a.home(), |name| {
        (name == "ZDOTDIR").then(|| zdot.clone().into_os_string())
    });
    a.places.install_dir = Some(a.home().join("Applications"));
    let said = note(&a).expect("~/.zshrc's loader is not zsh's under a ZDOTDIR");
    assert!(
        said.contains(&format!("which {} does not put", zdot_rc.display())),
        "{said}"
    );
    std::fs::remove_file(&home_rc).expect("rm ~/.zshrc");
    write(&zdot_rc, &loader);
    assert_eq!(note(&a), None, "$ZDOTDIR/.zshrc's loader counts");
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
    "selfsetup::tests::shell_setup_under_an_open_umask_shares_no_directory";

fn probe_shell_setup_under_an_open_umask_shares_no_directory_if_asked() -> bool {
    let Some(dir) = std::env::var_os(OPEN_UMASK_ENV) else {
        return false;
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
    true
}

#[test]
fn shell_setup_under_an_open_umask_shares_no_directory() {
    if probe_shell_setup_under_an_open_umask_shares_no_directory_if_asked() {
        return;
    }
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
fn an_uninstall_quits_the_acaffiene_mug_and_removes_its_files() {
    let a = Account::new("acaffiene");
    let home = a.home().to_path_buf();
    let log = a.dir.join("asked.log");
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    exe(
        &aterm,
        &format!("#!/bin/sh\necho \"$*\" >>'{}'\n", log.display()),
    );
    aterm_spec::wait::first_exec(&aterm, &["warm"]);
    std::fs::remove_file(&log).expect("the warm-up's line");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&log).expect("asked"),
        "--version\n",
        "no mug ran here, so its aterm was asked only before its bundle went: {out}"
    );
    std::fs::remove_file(&log).expect("the ask's line");

    let a = Account::new("acaffiene-left");
    let home = a.home().to_path_buf();
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    exe(
        &aterm,
        &format!("#!/bin/sh\necho \"$*\" >>'{}'\n", log.display()),
    );
    let support = home.join("Library/Application Support/aterm");
    write(&support.join("acaffiene.state"), "mode=off\n");
    write(&support.join("acaffiene.lock"), "");
    write(&support.join("acaffiene.bridge"), "4242\n");
    write(&support.join("acaffiene/l.plist"), "<plist/>");
    write(
        &support.join("acaffiene-code.noindex/l/1-2/aterm.app/Contents/Info.plist"),
        "<plist/>",
    );
    write(&support.join("acaffiene-code.noindex/l/1-2/ran"), "");
    let (ok, out, err) = a.sweep(None, true);
    assert!(ok, "{out}{err}");
    for step in ["acaffiene startup off", "acaffiene quit"] {
        assert!(
            out.contains(&format!("would run {} {step}", aterm.display())),
            "{out}"
        );
    }
    for name in [
        "acaffiene-code.noindex",
        "acaffiene",
        "acaffiene.state",
        "acaffiene.bridge",
    ] {
        let path = support.join(name);
        assert!(
            out.contains(&format!("would remove aCaffiene file: {}", path.display())),
            "the dry run names what the real one removes: {out}"
        );
    }
    assert!(
        out.contains(&format!(
            "would leave aCaffiene lock rendezvous: {}",
            support.join("acaffiene.lock").display()
        )),
        "{out}"
    );
    assert!(
        support.join("acaffiene.state").exists(),
        "a dry run keeps it"
    );
    assert!(!log.exists(), "a dry run starts no helper");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&log).expect("asked"),
        "help acaffiene\nacaffiene startup off\nacaffiene quit\n--version\n"
    );
    for name in [
        "acaffiene-code.noindex",
        "acaffiene",
        "acaffiene.state",
        "acaffiene.bridge",
    ] {
        assert!(!present(&support.join(name)), "{name}: {out}");
    }
    assert!(
        support.join("acaffiene.lock").is_file(),
        "the rendezvous stays"
    );
    assert!(!app.exists(), "the bundle went after its aterm was asked");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(out.contains("left aCaffiene lock rendezvous"), "{out}");
    assert!(support.join("acaffiene.lock").is_file());
    let lock = support.join("acaffiene.lock");
    let held =
        crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a held lock is a running mug: {out}{err}");
    assert!(err.contains("aCaffiene lock is busy"), "{err}");
    drop(held);
    let plist = home.join("Library/LaunchAgents/com.aterm.aterm.acaffiene.plist");
    write(&plist, "<plist/>");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a login plist starts a mug: {out}{err}");
    assert!(err.contains("aCaffiene stop was not confirmed"), "{err}");
    assert!(plist.is_file() && lock.is_file());

    let a = Account::new("acaffiene-quit-acts");
    let home = a.home().to_path_buf();
    let support = home.join("Library/Application Support/aterm");
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    exe(
        &app.join("Contents/MacOS/aterm"),
        &format!(
            "#!/bin/sh\n[ \"$*\" = 'acaffiene quit' ] || exit 0\nrm -rf '{code}'\n\
             echo mode=off >'{state}'\n",
            code = support.join("acaffiene-code.noindex").display(),
            state = support.join("acaffiene.state").display()
        ),
    );
    write(
        &support.join("acaffiene-code.noindex/l/aterm.app/Contents/Info.plist"),
        "<plist/>",
    );
    write(&support.join("acaffiene/l.plist"), "<plist/>");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    for name in ["acaffiene-code.noindex", "acaffiene", "acaffiene.state"] {
        assert!(!present(&support.join(name)), "{name}: {out}");
    }
    assert!(
        support.join("acaffiene.lock").is_file(),
        "the rendezvous made for the cleanup stays: {out}"
    );

    for kind in ["quit-fails", "unsupported", "busy", "lock-only-quit-fails"] {
        let a = Account::new(kind);
        let support = a.home().join("Library/Application Support/aterm");
        let app = a.home().join("Applications/aterm.app");
        bundle(&app, None);
        let refuse = match kind {
            "quit-fails" | "lock-only-quit-fails" => "acaffiene quit",
            "unsupported" => "help acaffiene",
            _ => "never",
        };
        exe(
            &app.join("Contents/MacOS/aterm"),
            &format!("#!/bin/sh\n[ \"$*\" = '{refuse}' ] && exit 1\nexit 0\n"),
        );
        let lock_only = kind == "lock-only-quit-fails";
        if !lock_only {
            write(&support.join("acaffiene.state"), "mode=off\n");
            write(&support.join("acaffiene-code.noindex/keep"), "copy bytes");
        }
        let lock = support.join("acaffiene.lock");
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
                std::fs::read_to_string(support.join("acaffiene.state")).expect("state"),
                "mode=off\n"
            );
            assert_eq!(
                std::fs::read_to_string(support.join("acaffiene-code.noindex/keep")).expect("copy"),
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

    let a = Account::new("acaffiene-mediated-help-refused");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    exe(&aterm, "#!/bin/sh\nexit 99\n");
    let image = std::fs::read(&aterm).expect("the helper");
    let support = a.home().join("Library/Application Support/aterm");
    let state = support.join("acaffiene.state");
    write(&state, "mode=on\n");
    let preflight = as_account_argv(501, Path::new(TRUE), &[]);
    let help = as_account_argv(501, &aterm, &["help", "acaffiene"]);
    let mut calls = Vec::new();
    let mut failed = Vec::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok = self_uninstall_with_acaffiene(
        &a.places,
        None,
        false,
        &mut out,
        &mut err,
        |s, p, dirs| {
            failed = sweep_acaffiene_with(
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
        },
        CHILD_HANG,
    );
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

/// An uninstall removes acaffiene.state's temporaries whose writer is gone and that are over
/// an hour old, first and whatever else it does, and nothing a writer may still rename.
#[test]
fn an_uninstall_removes_saves_cut_short_and_nothing_a_writer_may_rename() {
    let a = Account::new("acaffiene-cut-short");
    let support = a.home().join("Library/Application Support/aterm");
    let ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
    let date = |file: &Path| {
        std::fs::File::options()
            .write(true)
            .open(file)
            .expect("open")
            .set_modified(ago)
            .expect("date");
    };
    let cut_short = support.join(".acaffiene.state.tmp.4294967295.0123abcd");
    write(&cut_short, "mode=forever\n");
    date(&cut_short);
    let young = support.join(".acaffiene.state.tmp.4294967295.89abcdef");
    write(&young, "");
    let saving = support.join(format!(
        ".acaffiene.state.tmp.{}.0123abcd",
        std::process::id()
    ));
    write(&saving, "");
    date(&saving);
    let bridged = support.join(".acaffiene.bridge.tmp.4294967295.0123abcd");
    let old = [
        support.join(".caffeine.state.tmp.4294967295.0123abcd"),
        support.join(".caffeine.bridge.tmp.4294967295.0123abcd"),
    ];
    let agents = a.home().join("Library/LaunchAgents");
    let items = [
        agents.join(".com.aterm.aterm.acaffiene.plist.tmp.4294967295.0123abcd"),
        agents.join(".com.aterm.aterm.acaffiene.plist.tmp.4294967295.fedcba98"),
        agents.join(".com.aterm.aterm.acaffiene.dev.0123abcd.plist.tmp.4294967295.0123abcd"),
        agents.join(".com.aterm.aterm.caffeine.dev.0123abcd.plist.tmp.4294967295.0123abcd"),
    ];
    let others = [
        agents.join(".com.example.other.plist.tmp.4294967295.0123abcd"),
        agents.join(".com.aterm.aterm.acaffiene.dev.4567CDEF.plist.tmp.4294967295.0123abcd"),
        agents.join(format!(
            ".com.aterm.aterm.acaffiene.plist.tmp.{}.0123abcd",
            std::process::id()
        )),
    ];
    for temp in old.iter().chain([&bridged]).chain(&items).chain(&others) {
        write(temp, "");
        date(temp);
    }
    let young_item = agents.join(".com.aterm.aterm.acaffiene.plist.tmp.4294967295.89abcdef");
    write(&young_item, "");
    let (ok, out, err) = a.sweep(None, true);
    assert!(ok, "{out}{err}");
    for (temp, what) in [
        (&cut_short, "aCaffiene save"),
        (&bridged, "aCaffiene save"),
        (&old[0], "old Caffeine save"),
        (&old[1], "old Caffeine save"),
        (&items[0], "aCaffiene login item"),
        (&items[1], "aCaffiene login item"),
        (&items[2], "aCaffiene login item"),
        (&items[3], "old Caffeine login item"),
    ] {
        let line = format!("would remove {what} cut short: {}", temp.display());
        assert_eq!(out.matches(&line).count(), 1, "{line}: {out}");
        assert!(temp.is_file(), "a dry run keeps it");
    }
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    for temp in [&cut_short, &bridged, &old[0], &old[1]]
        .into_iter()
        .chain(&items)
    {
        assert!(!temp.exists(), "{}: {out}", temp.display());
    }
    for temp in &others {
        assert!(
            temp.is_file(),
            "another label's, or a live writer's: {}",
            temp.display()
        );
    }
    assert!(young_item.is_file(), "written within the hour");
    assert!(young.is_file(), "written within the hour");
    assert!(saving.is_file(), "a live writer's is its own to rename");
    for lock in ["acaffiene.lock", "caffeine.lock"] {
        assert!(
            !support.join(lock).exists(),
            "with no other aCaffiene file there, no hold was taken: {out}"
        );
    }

    let a = Account::new("acaffiene-cut-short-held");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    exe(&app.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 0\n");
    let support = a.home().join("Library/Application Support/aterm");
    let lock = support.join("acaffiene.lock");
    write(&lock, "");
    write(&support.join("acaffiene.state"), "mode=off\n");
    let cut_short = support.join(".acaffiene.state.tmp.4294967295.0123abcd");
    write(&cut_short, "");
    date(&cut_short);
    let held =
        crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "a held lock keeps the mug's files: {out}{err}");
    assert!(err.contains("aCaffiene lock is busy"), "{err}");
    assert!(support.join("acaffiene.state").is_file());
    assert!(
        !cut_short.exists(),
        "its writer is gone, mug or no mug: {out}"
    );
    drop(held);

    let a = Account::new("acaffiene-cut-short-unlistable");
    let support = a.home().join("Library/Application Support/aterm");
    let cut_short = support.join(".caffeine.bridge.tmp.4294967295.0123abcd");
    write(&cut_short, "");
    date(&cut_short);
    let mode = |bits| {
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(bits)).expect("chmod");
    };
    let item = a
        .home()
        .join("Library/LaunchAgents/.com.aterm.aterm.acaffiene.plist.tmp.4294967295.0123abcd");
    write(&item, "");
    date(&item);
    mode(0o300);
    let (_, out, err) = a.sweep(None, false);
    mode(0o700);
    assert_eq!(
        err.matches("its aCaffiene saves cut short could not be listed")
            .count(),
        1,
        "{out}{err}"
    );
    assert!(cut_short.is_file(), "{out}{err}");
    assert!(item.is_file(), "none goes after the skip: {out}{err}");

    let a = Account::new("acaffiene-cut-short-locked-agents");
    let support = a.home().join("Library/Application Support/aterm");
    let cut_short = support.join(".acaffiene.state.tmp.4294967295.0123abcd");
    write(&cut_short, "");
    date(&cut_short);
    let agents = a.home().join("Library/LaunchAgents");
    write(
        &agents.join(".com.aterm.aterm.acaffiene.plist.tmp.4294967295.0123abcd"),
        "",
    );
    let lock = |bits| {
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(bits)).expect("chmod");
    };
    lock(0o300);
    let (ok, out, err) = a.sweep(None, false);
    lock(0o700);
    assert!(
        ok,
        "a LaunchAgents locked against login items holds none: {out}{err}"
    );
    assert!(!cut_short.exists(), "the support folder's still go: {out}");

    let a = Account::new("acaffiene-cut-short-no-folder");
    let support = a.home().join("Library/Application Support/aterm");
    write(&support, "");
    write(&a.home().join("Library/LaunchAgents"), "");
    let (ok, out, err) = a.sweep(None, false);
    assert!(
        ok,
        "a support or LaunchAgents path that is no folder holds none: {out}{err}"
    );
}

/// A dry run says what the real one does with the mug's files, under both names: with no aterm
/// here to stop the mug it removes none, it names the lock rendezvous the real run makes, it
/// keeps a copy macOS may still be assessing, and a lock a mug holds keeps them all.
#[test]
fn a_dry_run_of_the_mugs_files_says_what_the_real_one_does() {
    let young = |code: &Path| {
        let copy = code.join("l/1-3");
        write(&copy.join("aterm.app/Contents/Info.plist"), "<plist/>");
        copy
    };
    let removes = |out: &str, path: &Path| {
        let tail = format!(": {}", path.display());
        out.lines()
            .any(|line| line.starts_with("atpkg: would remove") && line.ends_with(&tail))
    };
    let hold = |lock: &Path| {
        crate::lock::Flock::try_lock(std::fs::File::open(lock).expect("open")).expect("hold")
    };

    let a = Account::new("acaffiene-unasked");
    let support = a.home().join("Library/Application Support/aterm");
    write(&support.join("acaffiene.state"), "mode=off\n");
    write(&support.join("acaffiene.lock"), "");
    let (ok, out, err) = a.sweep(None, true);
    assert!(!ok, "{out}{err}");
    assert!(
        err.contains("aCaffiene stop would not be confirmed; its files would be kept"),
        "no aterm here to stop the mug: {err}"
    );
    assert!(!out.contains("would remove aCaffiene file"), "{out}");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "{out}{err}");
    assert!(err.contains("aCaffiene stop was not confirmed"), "{err}");
    assert!(support.join("acaffiene.state").is_file(), "{out}");
    std::fs::remove_file(support.join("acaffiene.state")).expect("rm");
    let held = hold(&support.join("acaffiene.lock"));
    let (ok, out, err) = a.sweep(None, true);
    assert!(
        !ok,
        "a lone lock a mug holds, with no aterm to quit it: {out}{err}"
    );
    assert!(
        err.contains("aCaffiene lock is busy or could not be acquired; its files would be kept"),
        "{err}"
    );
    assert!(!out.contains("would leave aCaffiene lock"), "{out}");
    let (ok, _, err) = a.sweep(None, false);
    assert!(!ok && err.contains("aCaffiene lock is busy"), "{err}");
    drop(held);

    let a = Account::new("acaffiene-unlocked");
    let home = a.home().to_path_buf();
    let app = home.join("Applications/aterm.app");
    bundle(&app, None);
    exe(&app.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 0\n");
    let support = home.join("Library/Application Support/aterm");
    let (lock, state) = (
        support.join("acaffiene.lock"),
        support.join("acaffiene.state"),
    );
    write(&state, "mode=off\n");
    let code = support.join("acaffiene-code.noindex");
    let copy = young(&code);
    let (ok, out, err) = a.sweep(None, true);
    assert!(
        !ok,
        "a copy macOS may still be assessing is kept: {out}{err}"
    );
    assert!(
        out.contains(&format!(
            "would make and leave aCaffiene lock rendezvous: {}",
            lock.display()
        )),
        "{out}"
    );
    assert!(!present(&lock), "a dry run makes none");
    assert!(err.contains("macOS may still be assessing"), "{err}");
    assert!(!removes(&out, &code), "{out}");
    assert!(
        out.contains(&format!("would remove aCaffiene file: {}", state.display())),
        "{out}"
    );
    assert!(
        out.contains("requires a helper above that stops aCaffiene"),
        "a removal that hangs on the helper's answer says so: {out}"
    );
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "{out}{err}");
    assert!(out.contains("left aCaffiene lock rendezvous"), "{out}");
    assert!(
        lock.is_file() && copy.is_dir() && !present(&state),
        "{out}{err}"
    );

    let a = Account::new("acaffiene-held-asked");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    exe(&app.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 0\n");
    let support = a.home().join("Library/Application Support/aterm");
    let (lock, state) = (
        support.join("acaffiene.lock"),
        support.join("acaffiene.state"),
    );
    write(&state, "mode=on\n");
    write(&lock, "");
    let held = hold(&lock);
    let (ok, out, err) = a.sweep(None, true);
    assert!(
        ok,
        "the helper's quit ends the mug that holds the lock: {out}{err}"
    );
    assert!(removes(&out, &state), "{out}");
    drop(held);

    let a = Account::new("old-caffeine-unlocked");
    let support = a.home().join("Library/Application Support/aterm");
    let (lock, state) = (
        support.join("caffeine.lock"),
        support.join("caffeine.state"),
    );
    write(&state, "mode=on\n");
    let code = support.join("caffeine-code.noindex");
    let copy = young(&code);
    let (ok, out, err) = a.sweep(None, true);
    assert!(!ok, "{out}{err}");
    assert!(
        out.contains(&format!(
            "would make and leave old Caffeine lock rendezvous: {}",
            lock.display()
        )),
        "{out}"
    );
    assert!(!present(&lock), "a dry run makes none");
    assert!(err.contains("macOS may still be assessing"), "{err}");
    assert!(!removes(&out, &code), "{out}");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "{out}{err}");
    assert!(out.contains("left old Caffeine lock rendezvous"), "{out}");
    assert!(
        lock.is_file() && copy.is_dir() && !present(&state),
        "{out}{err}"
    );

    write(&state, "mode=on\n");
    let held = hold(&lock);
    let (ok, out, err) = a.sweep(None, true);
    assert!(!ok, "an old mug holds its lock: {out}{err}");
    assert!(
        err.contains(
            "an old Caffeine mug holds this lock, or it could not be acquired; its files would \
             be kept"
        ),
        "{err}"
    );
    assert!(!removes(&out, &state), "{out}");
    let (ok, _, err) = a.sweep(None, false);
    assert!(!ok && err.contains("its files are kept"), "{err}");
    assert!(state.is_file());
    drop(held);

    std::fs::remove_file(&state).expect("rm");
    std::fs::remove_dir_all(&code).expect("rm");
    let (ok, out, err) = a.sweep(None, true);
    assert!(ok, "{out}{err}");
    assert!(
        out.contains(&format!(
            "would leave old Caffeine lock rendezvous: {}",
            lock.display()
        )),
        "a lone lock: {out}"
    );
    assert!(!out.contains("left old Caffeine lock"), "{out}");
}

#[test]
fn an_uninstall_removes_what_the_mug_left_under_its_old_name() {
    let a = Account::new("old-caffeine");
    let home = a.home().to_path_buf();
    let support = home.join("Library/Application Support/aterm");
    let agents = home.join("Library/LaunchAgents");
    let jobs = [
        agents.join("com.aterm.aterm.caffeine.plist"),
        agents.join("com.aterm.aterm.caffeine.dev.0123abcd.plist"),
    ];
    let others = [
        agents.join("com.aterm.aterm.caffeine.dev.0123abc.plist"),
        agents.join("com.example.caffeine.plist"),
    ];
    for plist in jobs.iter().chain(&others) {
        write(plist, "<plist/>");
    }
    let linked = agents.join("com.aterm.aterm.caffeine.dev.0000000a.plist");
    symlink(&others[1], &linked).expect("symlink");
    let lock = support.join("caffeine.lock");
    let plant = || {
        write(&support.join("caffeine.state"), "mode=on\n");
        write(&support.join("caffeine.bridge"), "4242\n");
        write(
            &support.join("caffeine/com.aterm.aterm.caffeine.plist"),
            "<plist/>",
        );
        write(
            &support.join("caffeine-code.noindex/l/1-2/aterm.app/Contents/Info.plist"),
            "<plist/>",
        );
        write(
            &support
                .join("caffeine-code.noindex/l/1-2")
                .join(aterm_uds::deadline::RAN),
            "",
        );
    };
    plant();
    write(&lock, "");
    let files = [
        "caffeine-code.noindex",
        "caffeine",
        "caffeine.state",
        "caffeine.bridge",
    ]
    .map(|name| support.join(name));

    let (ok, out, err) = a.sweep(None, true);
    assert!(ok, "{out}{err}");
    for plist in &jobs {
        assert!(
            out.contains(&format!(
                "would remove old Caffeine login item: {}",
                plist.display()
            )),
            "{out}"
        );
    }
    assert!(
        out.contains("old Caffeine file removal requires an acquired idle lock"),
        "{out}"
    );
    for path in &files {
        let what = if path.ends_with("caffeine-code.noindex") {
            "aCaffiene file"
        } else {
            "old Caffeine file"
        };
        assert!(
            out.contains(&format!("would remove {what}: {}", path.display())),
            "the dry run names what the real one removes: {out}"
        );
    }
    assert!(
        out.contains(&format!(
            "would leave old Caffeine lock rendezvous: {}",
            lock.display()
        )),
        "{out}"
    );
    assert!(
        jobs.iter().chain(&files).all(|path| present(path)),
        "a dry run keeps them"
    );

    let held =
        crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "an old mug holds its lock: {out}{err}");
    assert!(err.contains("an old Caffeine mug holds this lock"), "{err}");
    assert!(
        err.contains(&format!("from {} by hand", support.display())),
        "it names where the files it kept are: {err}"
    );
    assert!(jobs.iter().all(|plist| !present(plist)), "{out}");
    assert!(files.iter().all(|path| present(path)), "{out}");
    drop(held);

    let young = support.join("caffeine-code.noindex/l/1-3");
    write(&young.join("aterm.app/Contents/Info.plist"), "<plist/>");
    let (ok, out, err) = a.sweep(None, false);
    assert!(
        !ok,
        "a copy macOS may still be assessing is kept: {out}{err}"
    );
    assert!(err.contains("macOS may still be assessing"), "{err}");
    assert!(young.is_dir(), "{out}");
    assert!(
        !present(&support.join("caffeine-code.noindex/l/1-2")),
        "a copy a mug ran from goes: {out}"
    );
    assert!(!present(&support.join("caffeine.state")), "{out}");
    write(&young.join(aterm_uds::deadline::RAN), "");

    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    for path in &files {
        assert!(!present(path), "{}: {out}", path.display());
    }
    assert!(others.iter().all(|plist| plist.is_file()), "{out}");
    assert!(present(&linked), "a link is no job's plist: {out}");
    assert!(lock.is_file(), "the rendezvous stays");
    assert!(
        out.contains(&format!(
            "left old Caffeine lock rendezvous: {}",
            lock.display()
        )),
        "{out}"
    );

    plant();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut s = Sweep {
        dry: false,
        as_root: None,
        removed: 0,
        skipped: 0,
        kept: 0,
        shared_kept: 0,
        outsider: Some(home.join("Applications/aterm.app")),
        out: &mut out,
        err: &mut err,
    };
    write(&jobs[0], "<plist/>");
    let held =
        crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open")).expect("hold");
    sweep_old_caffeine(&mut s, &a.places, None, make_as_account);
    drop(held);
    assert_eq!(
        (s.removed, s.skipped, s.shared_kept),
        (0, 0, 5),
        "an outsider sweep leaves them without asking for the lock"
    );
    assert!(
        jobs[0].is_file() && files.iter().all(|path| present(path)),
        "an aterm left outside the sweep keeps them: {}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn a_root_sweep_asks_acaffiene_as_the_homes_owner() {
    let me = aterm_uds::peer::our_uid();
    assert_ne!(me, 0, "the tests run unprivileged");
    let own = aterm_uds::account::home_of(me).expect("this account's home");
    let meta = std::fs::metadata(&own).expect("home");
    assert_eq!(
        acaffiene_account(0, &own),
        Some((me, meta.gid())),
        "root, the account's own home"
    );
    assert_eq!(acaffiene_account(me, &own), None, "not root");
    let a = Account::new("acaffiene-account");
    assert_eq!(
        acaffiene_account(0, a.home()),
        None,
        "the account's directory, not its home"
    );
    assert_eq!(acaffiene_account(0, Path::new("/")), None, "root's own");
    assert_eq!(acaffiene_account(0, &a.home().join("missing")), None);
    let aterm = Path::new("/A/aterm.app/Contents/MacOS/aterm");
    let args = ["acaffiene", "quit", "own"];
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
        "/usr/bin/sudo -n -H -u #501 -- /A/aterm.app/Contents/MacOS/aterm acaffiene quit own"
    );
    assert_eq!(
        helper_line(aterm, &args, None),
        "/A/aterm.app/Contents/MacOS/aterm acaffiene quit own"
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

    let a = Account::new("acaffiene-root-nobody");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    exe(&app.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 0\n");
    write(
        &a.home()
            .join("Library/Application Support/aterm/acaffiene.state"),
        "mode=on\n",
    );
    let mut failed = Vec::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok = self_uninstall_with_acaffiene(
        &a.places,
        None,
        false,
        &mut out,
        &mut err,
        |s, p, dirs| {
            failed = sweep_acaffiene_with(
                s,
                p,
                dirs,
                0,
                || None,
                |program, args| panic!("root ran {} {args:?}", program.display()),
            );
            failed.clone()
        },
        CHILD_HANG,
    );
    let err = String::from_utf8(err).expect("utf-8");
    assert!(!ok, "{err}");
    assert!(err.contains(NOT_AS_ROOT), "{err}");
    assert_eq!(failed, std::slice::from_ref(&app));
    assert!(app.is_dir(), "kept for its steps");
}

#[test]
fn the_acaffiene_cleanup_holds_only_the_accounts_lock() {
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
    let dir = crate::testdir::dir("atpkg-acaffiene-hold-");
    let lock = dir.join("acaffiene.lock");
    let me = aterm_uds::peer::our_uid();
    let gid = std::fs::metadata(&dir).expect("dir").gid();
    assert!(acaffiene_cleanup_hold(&lock, Some((me, gid)), cannot).is_none());
    assert!(acaffiene_cleanup_hold(&lock, Some((me, gid)), gone).is_none());
    assert!(!lock.exists(), "made by nobody but its account");
    let held = acaffiene_cleanup_hold(&lock, Some((me, gid)), makes).expect("its account's");
    assert_eq!(MADE.with(std::cell::Cell::get), 3);
    let mode = std::fs::metadata(&lock).expect("lock").mode();
    assert_eq!(mode & 0o777, 0o600, "private, as a mug makes it");
    drop(held);
    assert!(acaffiene_cleanup_hold(&lock, Some((me + 1, gid)), refuse).is_none());
    let second = dir.join("second-name");
    std::fs::hard_link(&lock, &second).expect("link");
    assert!(acaffiene_cleanup_hold(&lock, Some((me, gid)), refuse).is_none());
    assert!(acaffiene_cleanup_hold(&lock, None, refuse).is_none());
    std::fs::remove_file(&second).expect("unlink");
    std::fs::remove_file(&lock).expect("unlink");
    assert!(acaffiene_cleanup_hold(&lock, None, refuse).is_some());
    assert!(lock.is_file(), "this process's own sweep makes it");
}

/// A dry run's probe of a lock that is there answers as the real run's hold would, and leaves the
/// lock as it found it.
#[test]
fn a_dry_runs_lock_probe_answers_as_the_cleanup_hold_does() {
    fn refuse(_: &Path, _: u32) -> bool {
        panic!("a lock that is there is never made")
    }
    let dir = crate::testdir::dir("atpkg-acaffiene-probe-");
    let lock = dir.join("acaffiene.lock");
    let me = aterm_uds::peer::our_uid();
    let gid = std::fs::metadata(&dir).expect("dir").gid();
    let agree = |account: Option<(u32, u32)>| {
        let probed = cleanup_lock_free(&lock, account);
        let held = acaffiene_cleanup_hold(&lock, account, refuse).is_some();
        assert_eq!(probed, held, "{account:?}");
        probed
    };
    assert!(
        !cleanup_lock_free(&lock, None),
        "no lock: the real run makes one"
    );
    assert!(!lock.exists(), "a probe makes none");
    write(&lock, "");
    assert!(agree(None) && agree(Some((me, gid))));
    assert!(!agree(Some((me + 1, gid))), "another account's");
    let held = crate::lock::Flock::try_lock(std::fs::File::open(&lock).expect("open"))
        .expect("held by a mug");
    assert!(!agree(None), "a mug holds it");
    drop(held);
    assert!(cleanup_lock_free(&lock, None), "the probe let go of it");
    let second = dir.join("second-name");
    std::fs::hard_link(&lock, &second).expect("link");
    assert!(!agree(None), "two names");
    std::fs::remove_file(&lock).expect("unlink");
    symlink(&second, &lock).expect("symlink");
    assert!(!agree(None), "a symlink");
    std::fs::remove_file(&lock).expect("unlink");
    let fifo = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).expect("path");
    // SAFETY: `fifo` is a NUL-terminated path that lives across the call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "mkfifo");
    assert!(
        !cleanup_lock_free(&lock, None),
        "a FIFO, opened without waiting for a writer"
    );
    assert!(
        std::fs::read(&second).expect("kept").is_empty(),
        "nothing written"
    );
}

#[test]
fn an_uninstall_keeps_a_acaffiene_copy_macos_may_still_be_assessing() {
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
            sweep_acaffiene_code_with(&mut s, code, now, metadata, entries);
            (s.removed, s.skipped)
        };
        (
            counts.0,
            counts.1,
            String::from_utf8(out).expect("out"),
            String::from_utf8(err).expect("err"),
        )
    }

    let a = Account::new("acaffiene-young");
    let code = a
        .home()
        .join("Library/Application Support/aterm/acaffiene-code.noindex");
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
                    return collect_acaffiene_code_entries([
                        Ok(first),
                        Err(Error::new(ErrorKind::Interrupted, "injected entry refusal")),
                    ]);
                }
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "injected enumeration refusal",
                ));
            }
            acaffiene_code_entries(path)
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
        let mut entries = acaffiene_code_entries(path)?;
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
        sweep_acaffiene_code(&mut s, &code, now);
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
    let gone = dir.path().join("acaffiene.bridge");
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
    s.rm(&gone, "aCaffiene file");
    assert_eq!((s.removed, s.skipped), (0, 0));
    let blocked = dir.path().join("a-file");
    write(&blocked, "");
    s.rm(&blocked.join("inside"), "aCaffiene file");
    assert_eq!((s.removed, s.skipped), (0, 1));
    let (out, err) = (String::from_utf8_lossy(&out), String::from_utf8_lossy(&err));
    assert!(out.contains("gone already: aCaffiene file"), "{out}");
    assert!(!err.contains("acaffiene.bridge"), "{err}");
}

#[test]
fn an_uninstall_quits_a_mug_the_stores_aterm_runs() {
    for fails in [false, true] {
        let mut a = Account::new(if fails {
            "acaffiene-store-fails"
        } else {
            "acaffiene-store"
        });
        let support = a.home().join("Library/Application Support/aterm");
        let store = a.dir.join("store");
        let log = a.dir.join("asked.log");
        let fail = if fails {
            "[ \"$*\" = 'acaffiene quit' ] && exit 1\n"
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
        write(&support.join("acaffiene.state"), "mode=off\n");
        write(&support.join("acaffiene.lock"), "");
        let (ok, out, err) = a.sweep(None, false);
        assert_eq!(
            std::fs::read_to_string(&log).expect("the store's aterm was asked"),
            "help acaffiene\nacaffiene startup off\nacaffiene quit\n",
            "{out}{err}"
        );
        assert_eq!(ok, !fails, "{out}{err}");
        assert_eq!(store.join("aterm").exists(), fails, "{out}{err}");
        assert_eq!(
            present(&support.join("acaffiene.state")),
            fails,
            "{out}{err}"
        );
    }
}

#[test]
fn a_scoped_uninstall_quits_only_its_own_mug() {
    for old in [false, true] {
        let mut a = Account::new(if old {
            "acaffiene-scoped-old"
        } else {
            "acaffiene-scoped"
        });
        let home = a.home().to_path_buf();
        let support = home.join("Library/Application Support/aterm");
        let log = a.dir.join("asked.log");
        bundle(&home.join("Applications/aterm.app"), None);
        let scoped = home.join("aterm.app");
        bundle(&scoped, None);
        let refuse = if old {
            "[ \"$*\" = 'acaffiene quit own' ] && exit 2\n"
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
        write(&support.join("acaffiene.state"), "mode=off\n");
        write(&support.join("acaffiene.lock"), "");
        let (ok, out, err) = a.sweep(None, false);
        assert!(ok, "{out}{err}");
        assert_eq!(
            std::fs::read_to_string(&log).expect("asked"),
            "help acaffiene\nacaffiene startup off\nacaffiene quit own\n--version\n"
        );
        assert!(!scoped.exists(), "{out}");
        assert!(
            support.join("acaffiene.state").exists(),
            "the staying aterm's: {out}"
        );
        assert_eq!(out.contains("cannot quit only its own mug"), old, "{out}");
    }
}

#[test]
fn a_decline_left_for_a_kept_bundle_goes_with_it() {
    let a = Account::new("acaffiene-decline");
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
        "#!/bin/sh\n[ \"$*\" = 'acaffiene quit' ] && exit 1\nexit 0\n",
    );
    write(&support.join("acaffiene.state"), "mode=off\n");
    write(&support.join("acaffiene.lock"), "");
    let decline = support.join("pkg/declined");
    let (ok, out, err) = a.sweep(None, false);
    assert!(!ok, "{out}{err}");
    assert!(app.exists(), "kept for its acaffiene quit: {out}");
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

#[test]
fn an_uninstall_keeps_a_bundle_whose_program_inventory_cannot_be_read() {
    let a = Account::new("unread-programs");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    let programs = app.join("Contents/MacOS");
    std::fs::write(&programs, b"not a directory").expect("private inventory obstruction");
    let (ok, out, err) = a.sweep(None, false);
    assert!(
        !ok,
        "an unread inventory is a partial uninstall: {out}{err}"
    );
    assert!(
        app.is_dir(),
        "an unread inventory keeps its bundle: {out}{err}"
    );
    assert_eq!(
        std::fs::read(&programs).expect("retained obstruction"),
        b"not a directory"
    );
    assert!(
        err.contains(&format!("cannot read {}:", programs.display()))
            && err.contains("whether every program has finished its first start cannot be told"),
        "{err}"
    );
    std::fs::remove_file(&programs).expect("remove only our inventory obstruction");
    let (ok, out, err) = a.sweep(None, false);
    assert!(
        ok,
        "a missing program directory holds no programs: {out}{err}"
    );
    assert!(
        !app.exists(),
        "the known-empty bundle can be removed: {out}{err}"
    );
}

#[test]
fn an_uninstall_keeps_a_bundle_whose_program_has_not_answered() {
    let hang = Duration::from_millis(100);
    // `aterm-ctl` stands for any sibling an older toolset's `aterm-cli` may have started.
    for program in ["aterm", "aterm-cli", "aterm-ctl"] {
        let a = Account::new(&format!("unanswered-{program}"));
        let app = a.home().join("Applications/aterm.app");
        bundle(&app, None);
        let stand_in = app.join("Contents/MacOS").join(program);
        exe(
            &stand_in,
            &format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && exec sleep {}\nexit 0\n",
                aterm_spec::wait::PARKED_SECS
            ),
        );
        aterm_spec::wait::first_exec(&stand_in, &[]);
        let (ok, out, err) = a.sweep_within(hang);
        assert!(!ok, "a kept bundle is a partial uninstall: {out}{err}");
        assert!(app.is_dir(), "{program}: {out}{err}");
        assert!(
            err.contains(&format!(
                "SKIPPED {} — kept: `{} --version` did not answer within {hang:?}, so macOS may \
                 still be checking its first start, and removing the bundle now can stall every \
                 program start until a restart — uninstall again in 10 minutes, or after a restart",
                app.display(),
                stand_in.display()
            )),
            "{err}"
        );
    }
}

#[test]
fn an_uninstall_removes_a_bundle_whose_programs_ended_however_they_ended() {
    let a = Account::new("answered");
    let log = a.dir.join("asked.log");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    let macos = app.join("Contents/MacOS");
    for (program, end) in [("aterm", "exit 3"), ("aterm-cli", "kill -KILL $$")] {
        exe(
            &macos.join(program),
            &format!(
                "#!/bin/sh\necho \"{program} $*\" >>'{}'\n{end}\n",
                log.display()
            ),
        );
        aterm_spec::wait::first_exec(&macos.join(program), &["warm"]);
    }
    std::fs::remove_file(&log).expect("the warm-ups' lines");
    let (ok, out, err) = a.sweep(None, true);
    assert!(ok && app.is_dir(), "{out}{err}");
    assert!(!log.exists(), "a dry run asks nothing: {out}");
    for program in ["aterm", "aterm-cli"] {
        assert!(
            out.contains(&format!(
                "would ask {} --version, then if it answers:",
                macos.join(program).display()
            )),
            "{out}"
        );
    }
    assert!(
        out.contains(&format!("would remove app bundle: {}", app.display())),
        "{out}"
    );
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&log).expect("asked"),
        "aterm --version\naterm-cli --version\n"
    );
    assert!(!app.exists(), "{out}");
}

/// A run through sudo's shell ([`ENDED`]) shows the program ran once it printed or the shell
/// wrote `ended <n>`, except a 126 or 127 that is the shell's own exec failure, which counts
/// only for a kernel refusal. No such line at all is sudo's end or the shell's.
#[test]
fn a_run_through_sudo_shows_the_program_ran_by_its_end_or_a_kernel_refusal() {
    use std::os::unix::process::ExitStatusExt as _;

    let hang = Duration::from_millis(100);
    let program = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let p = program.display();
    let account = RunAs::Account(501);
    let ended = |code: i32, stdout: &str, stderr: &str| {
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    };
    // sudo's refusal, or a shell that could not fork (bash exits 128): no `ended` line.
    for code in [1, 2, 126, 127, 128] {
        assert!(
            ran_unproven(
                program,
                account,
                ended(code, "", "sudo: a password is required\n"),
                hang
            )
            .is_some_and(|why| why.starts_with("may not have started: sudo ended")
                && why.contains("saying `sudo: a password is required`")),
            "{code}"
        );
        assert!(ran_unproven(program, RunAs::Me, ended(code, "", ""), hang).is_none());
    }
    // A run the shell started and saw end, whatever its status, or one that printed.
    for status in [0, 1, 2, 75, 125, 128, 137] {
        let out = format!("ended {status}\n");
        assert!(
            ran_unproven(program, account, ended(0, &out, ""), hang).is_none(),
            "{status}"
        );
    }
    for out in ["aterm 1\nended 0\n", "aterm 1\n", "partialended 126\n"] {
        assert!(
            ran_unproven(program, account, ended(0, out, ""), hang).is_none(),
            "{out}"
        );
    }
    // The shell could not exec it (denied, gone, a folder): no run, so nothing shown.
    for (status, said) in [
        (126, strerror(libc::EACCES)),
        (126, strerror(libc::EPERM)),
        (127, strerror(libc::ENOENT)),
        (126, "is a directory".to_owned()),
    ] {
        let line = format!("{p}: {p}: {said}");
        let out = format!("ended {status}\n");
        assert!(
            ran_unproven(program, account, ended(0, &out, &format!("{line}\n")), hang)
                .is_some_and(|why| why.contains(&format!("saying `{line}`"))),
            "{line}"
        );
    }
    // A refusal of the kernel's the shell names for this program comes before macOS's check.
    let refusals = EXEC_REFUSALS
        .iter()
        .map(|&code| strerror(code))
        .chain(["cannot execute binary file".to_owned()]);
    for why in refusals {
        let line = format!("{p}: {p}: {why}\n");
        assert!(
            ran_unproven(program, account, ended(0, "ended 126\n", &line), hang).is_none(),
            "{line}"
        );
        let other = line.replace("MacOS/aterm", "MacOS/atpkg");
        assert!(
            ran_unproven(program, account, ended(0, "ended 126\n", &other), hang).is_some(),
            "{other}"
        );
    }
}

/// The atpkg an uninstall runs through sudo is sudo's own child: an exec of it the kernel
/// refused shows in sudo's words, or its `/bin/sh`'s for ENOEXEC, and a denial does not count.
#[test]
fn sudo_names_a_kernel_refusal_of_the_atpkg_it_runs() {
    let program = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
    let p = program.display();
    let account = RunAs::Account(501);
    for code in [1, 126, 127] {
        assert!(sudo_may_not_have_run(account, Some(code)));
        assert!(!sudo_may_not_have_run(RunAs::Me, Some(code)));
    }
    for code in [0, 2, 75, 125, 128] {
        assert!(!sudo_may_not_have_run(account, Some(code)), "{code}");
    }
    assert!(!sudo_may_not_have_run(account, None));
    for code in EXEC_REFUSALS {
        let line = format!("sudo: unable to execute {p}: {}\n", strerror(code));
        assert!(
            sudo_refused_exec(account, Some(1), line.as_bytes(), program),
            "{line}"
        );
        let other = line.replace("MacOS/atpkg", "MacOS/aterm");
        assert!(
            !sudo_refused_exec(account, Some(1), other.as_bytes(), program),
            "{other}"
        );
    }
    for code in [libc::EACCES, libc::EPERM] {
        let line = format!("sudo: unable to execute {p}: {}\n", strerror(code));
        assert!(
            !sudo_refused_exec(account, Some(1), line.as_bytes(), program),
            "{line}"
        );
    }
    let sh = format!("/bin/sh: {p}: cannot execute binary file\n");
    assert!(sudo_refused_exec(
        account,
        Some(126),
        sh.as_bytes(),
        program
    ));
    assert!(!sudo_refused_exec(
        RunAs::Me,
        Some(126),
        sh.as_bytes(),
        program
    ));
    assert!(!sudo_refused_exec(account, Some(0), sh.as_bytes(), program));
}

/// The shell sudo runs ([`ENDED`]) writes once the run ends: a quiet program or one the kernel
/// refuses shows it ran, one the shell could not exec does not, and neither the account's
/// `.bashrc` (read under sshd's `SSH_CLIENT`) nor `BASH_ENV` writes to its output.
#[test]
fn the_shell_a_run_through_sudo_asks_by_writes_once_the_run_ends() {
    let a = Account::new("ended");
    let quiet = a.dir.join("quiet");
    exe(&quiet, "#!/bin/sh\necho 'unknown command' >&2\nexit 1\n");
    aterm_spec::wait::first_exec(&quiet, &[]);
    // A Mach-O header for a CPU no Mac runs now (32-bit PowerPC): the kernel refuses its exec.
    let refused = a.dir.join("refused");
    let header: Vec<u8> = [0xfeed_face_u32, 18, 0, 2, 0, 0, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    std::fs::write(&refused, header).expect("the file");
    std::fs::set_permissions(&refused, std::fs::Permissions::from_mode(0o755)).expect("mode");
    // A file it may not run, and none at all: the shell's own failure, no run.
    let denied = a.dir.join("denied");
    std::fs::write(&denied, "#!/bin/sh\nexit 0\n").expect("the file");
    let gone = a.dir.join("gone");
    let home = a.dir.join("home");
    std::fs::create_dir_all(&home).expect("the home");
    std::fs::write(home.join(".bashrc"), "echo bashrc\n").expect("the file");
    let env = a.dir.join("env");
    std::fs::write(&env, "echo env\n").expect("the file");
    for (program, ended, ran) in [
        (&quiet, "ended 1", true),
        (&refused, "ended 126", true),
        (&denied, "ended 126", false),
        (&gone, "ended 127", false),
    ] {
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(Path::new(SH[0]))
                .args(&SH[1..])
                .args(["-c", ENDED])
                .arg(program)
                .arg("--version")
                .env("HOME", &home)
                .env("SSH_CLIENT", "10.0.0.2 50000 22")
                .env("BASH_ENV", &env)
                .env_remove("SHLVL"),
            std::time::Instant::now() + aterm_spec::wait::FIRST_EXEC,
        );
        let text = out
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        assert_eq!(text.as_deref().ok(), Some(ended), "{}", program.display());
        assert_eq!(
            ran_unproven(program, RunAs::Account(501), out, Duration::from_secs(1)).is_none(),
            ran,
            "{}",
            program.display()
        );
    }
}

#[test]
fn an_uninstall_removes_a_bundle_whose_program_the_kernel_will_not_start() {
    let a = Account::new("unstartable");
    let app = a.home().join("Applications/aterm.app");
    bundle(&app, None);
    let aterm = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(app.join("Contents/MacOS")).expect("the folder");
    // A Mach-O header for a CPU no Mac runs now (32-bit PowerPC): the kernel refuses its exec.
    let header: Vec<u8> = [0xfeed_face_u32, 18, 0, 2, 0, 0, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    std::fs::write(&aterm, header).expect("the file");
    std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let (ok, out, err) = a.sweep(None, false);
    assert!(ok, "{out}{err}");
    assert!(!app.exists(), "a file no exec starts is in no check: {out}");
}

#[test]
fn an_uninstall_asks_an_atpkg_whose_run_a_signal_ended() {
    let hangs = format!("exec sleep {}", aterm_spec::wait::PARKED_SECS);
    // The parked answer's wait is short (its timeout is the subject, and the run that exits
    // 126 is proven, so no ask may happen); the answer that must arrive gets a hang detector.
    let short = Duration::from_millis(100);
    for (run, rc, answer, removed, within) in [
        ("kill -KILL $$", 137, hangs.as_str(), false, short),
        (
            "kill -KILL $$",
            137,
            "exit 0",
            true,
            aterm_spec::wait::MUST_HAPPEN,
        ),
        ("exit 126", 126, hangs.as_str(), true, short),
    ] {
        let a = Account::new("atpkg-run");
        let app = a.home().join("Applications/aterm.app");
        bundle(
            &app,
            Some(&format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {answer}\n[ \"$1\" = uninstall ] && {run}\n\
                 exit 0\n"
            )),
        );
        let atpkg = app.join("Contents/MacOS/atpkg");
        aterm_spec::wait::first_exec(&atpkg, &[]);
        let (ok, out, err) = a.sweep_within(within);
        assert!(!ok, "the store's removal did not finish: {out}{err}");
        assert!(
            err.contains(&format!(
                "`{} uninstall --all` could not run (exit {rc})",
                atpkg.display()
            )),
            "{err}"
        );
        assert_eq!(app.is_dir(), !removed, "{run}, then {answer}: {out}{err}");
        assert_eq!(
            err.contains(&format!(
                "SKIPPED {} — kept: `{} --version` did not answer within {within:?}",
                app.display(),
                atpkg.display()
            )),
            !removed,
            "{err}"
        );
    }
}
