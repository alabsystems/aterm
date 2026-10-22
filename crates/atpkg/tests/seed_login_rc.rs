// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The seed lays `base.shell.rc` (docs/DESIGN-init-system-2026-10-20.md §4.2, S6): on a
//! home with no rc file at all, it creates the one rc the account's LOGIN shell reads,
//! which comes from the account record (`getpwuid`), never `$SHELL`. No other shell
//! gains a file. A block the person deletes stays deleted under every seed, and comes
//! back under `aterm pkg repair`'s pass.
//!
//! The real `atpkg seed` over a scratch `HOME`, `XDG_CONFIG_HOME` and state root
//! (`ATERM_STATE_HOME`, where the pass's journal lands), with an empty local registry. The
//! account record's shell is the development seam [`atpkg::hooks::LOGIN_SHELL_SEAM`] (a
//! test's `chsh`), so the case does not depend on the host account's shell; `$SHELL`
//! names another shell in every run.

#![cfg(unix)]
use aterm_uds::spawnfd::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::process::Output;

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-seed-login-rc-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        // fish's config directory stands, so a pass that wrote for any shell could.
        std::fs::create_dir_all(home.join(".config/fish")).unwrap();
        // The default consent: an empty aterm.toml.
        std::fs::create_dir_all(home.join(".config/aterm")).unwrap();
        std::fs::write(home.join(".config/aterm/aterm.toml"), "").unwrap();
        std::fs::create_dir_all(root.path().join("registry")).unwrap();
        std::fs::create_dir_all(root.path().join("state")).unwrap();
        Self { root, home }
    }

    /// `atpkg seed` with the account record's shell `login` and `$SHELL` set to `shell`.
    fn seed(&self, login: &str, shell: &str) -> Output {
        self.seed_under(login, shell, Some(&self.root.path().join("state")))
    }

    /// [`Self::seed`] with the private state root `state` (`ATERM_STATE_HOME`), or none: a
    /// scratch `HOME` alone, as most spawn tests run the seed.
    fn seed_under(&self, login: &str, shell: &str, state: Option<&Path>) -> Output {
        self.seed_with(login, shell, state, None)
    }

    /// [`Self::seed_under`] with `$PATH` set to `path` (`None`: the test's own).
    fn seed_with(
        &self,
        login: &str,
        shell: &str,
        state: Option<&Path>,
        path: Option<&Path>,
    ) -> Output {
        let mut seed = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        seed.arg("seed");
        if let Some(path) = path {
            seed.env("PATH", path);
        }
        match state {
            Some(state) => seed.env("ATERM_STATE_HOME", state),
            None => seed.env_remove("ATERM_STATE_HOME"),
        };
        seed.env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env(atpkg::hooks::LOGIN_SHELL_SEAM, login)
            .env("SHELL", shell)
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
            )
            .env_remove("RUSTUP_HOME")
            .env_remove("ATERM_CHILD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run dev atpkg seed")
    }

    /// `atpkg shell-setup --path`, as install.sh's PATH half runs it on a fresh lean
    /// install (`wire_shell_path`), with the account record's shell `login` and `$SHELL` set
    /// to `shell`.
    fn shell_setup(&self, login: &str, shell: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .args(["shell-setup", "--path", "--toolset-coming", "--bin-dir"])
            .arg(self.home.join(".local/bin"))
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env(atpkg::hooks::LOGIN_SHELL_SEAM, login)
            .env("SHELL", shell)
            .env_remove("ATERM_CHILD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run dev atpkg shell-setup")
    }

    fn rc(&self, name: &str) -> PathBuf {
        self.home.join(name)
    }

    /// Every rc atpkg could wire, and install.sh's two bash login profiles, that exists.
    fn rc_files(&self) -> Vec<&'static str> {
        [
            ".zshrc",
            ".bashrc",
            ".bash_profile",
            ".bash_login",
            ".profile",
            ".config/fish/config.fish",
        ]
        .into_iter()
        .filter(|rc| std::fs::symlink_metadata(self.rc(rc)).is_ok())
        .collect()
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// End the calling test COULD NOT RUN in a build that pins no root key: the manager is off
/// there and the seed adopts, and so lays, nothing.
fn needs_a_pinned_root() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        aterm_uds::host::could_not_run(
            "this build pins no root key, so the package manager is off and the seed lays nothing",
        );
    }
}

/// `pkg repair`'s pass over the rc item for `home`, whose login shell reads `login`: the
/// asked-for pass, in-process (`atpkg repair` itself changes nothing from a scratch `HOME`,
/// which is not this account's: `Requires::Account`).
fn repair_rc(home: &Path, login: &'static str) -> Vec<aterm_converge::ItemReport> {
    use atpkg::converge::repair::{self, Ground, Pass};
    let ground = std::sync::Arc::new(Ground {
        layout: atpkg::store::Layout {
            prefix: atpkg::store::default_prefix(home),
        },
        home: Some(home.to_path_buf()),
        exe: None,
        rustup_home: None,
        login_rc: Some(login),
        pass: Pass::Repair,
    });
    let items: Vec<Box<dyn aterm_converge::Item>> = repair::items(&ground)
        .into_iter()
        .filter(|item| [repair::HOOKS, repair::RC].contains(&item.id().as_str()))
        .collect();
    aterm_converge::run_scoped(
        &items,
        aterm_converge::Mode::Apply,
        aterm_converge::Scope::REPAIR,
        &mut aterm_converge::NoJournal,
        &mut |_| {},
    )
    .expect("the rc item waits on the hooks item, both kept")
}

#[test]
fn seed_creates_the_login_shell_s_rc_alone_and_never_reads_shell() {
    needs_a_pinned_root();
    let fx = Fixture::new("zsh");
    let out = fx.seed("/bin/zsh", "/bin/bash");
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        fx.rc_files(),
        [".zshrc"],
        "the login shell's rc, and only it, with $SHELL naming bash: {text}"
    );
    let zshrc = std::fs::read_to_string(fx.rc(".zshrc")).unwrap();
    assert!(
        zshrc.contains("# >>> atpkg shell integration >>>")
            && zshrc.contains(".aterm/shell.d/00-atpkg.zsh"),
        "{zshrc:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("atpkg: created ~/.zshrc"),
        "the seed says what it made: {text}"
    );
    // The change was journaled under the scratch state root.
    let journal = fx.root.path().join("state").join("converge");
    assert!(journal.is_dir(), "journaled under ATERM_STATE_HOME: {text}");

    // The person deletes the block: every seed leaves it deleted.
    std::fs::write(fx.rc(".zshrc"), "export EDITOR=vi\n").unwrap();
    for pass in 0..2 {
        let out = fx.seed("/bin/zsh", "/bin/bash");
        assert!(out.status.success(), "pass {pass}: {}", describe(&out));
        assert_eq!(
            std::fs::read_to_string(fx.rc(".zshrc")).unwrap(),
            "export EDITOR=vi\n",
            "pass {pass}: the seed undid the person's opt-out: {}",
            describe(&out)
        );
    }

    // `pkg repair`'s pass lays it back, and says so.
    let rows = repair_rc(&fx.home, ".zshrc");
    let rc = rows
        .iter()
        .find(|row| row.id == atpkg::converge::repair::RC)
        .expect("the rc row");
    assert_eq!(rc.outcome, aterm_converge::Outcome::Changed, "{rows:?}");
    assert!(
        rc.detail
            .as_deref()
            .is_some_and(|d| d.contains("re-laid atpkg's block in ~/.zshrc")),
        "{rows:?}"
    );
    let after = std::fs::read_to_string(fx.rc(".zshrc")).unwrap();
    assert!(
        after.starts_with("export EDITOR=vi\n")
            && after.contains("# >>> atpkg shell integration >>>"),
        "{after:?}"
    );
    assert_eq!(fx.rc_files(), [".zshrc"]);
}

/// The negative control: a login shell atpkg has no rc row for gets no file, whatever
/// `$SHELL` says, so the file above is the account record's doing.
#[test]
fn seed_creates_no_rc_for_a_login_shell_it_does_not_wire() {
    needs_a_pinned_root();
    let fx = Fixture::new("sh");
    let out = fx.seed("/bin/sh", "/bin/zsh");
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        fx.home.join(".aterm/shell.d/00-atpkg.zsh").is_file(),
        "the seed ran its relay: {text}"
    );
    assert_eq!(fx.rc_files(), Vec::<&str>::new(), "{text}");
}

/// A seed under a scratch `HOME` with no private state root journals into that `HOME`'s own
/// state root, never the account's (`aterm_types::dirs::account_state_dir`): the tree the pass changed keeps
/// the record of the change, and the owner's journal never hears of a test's home.
#[test]
fn a_scratch_home_s_seed_journals_under_that_home() {
    needs_a_pinned_root();
    let fx = Fixture::new("journal");
    let out = fx.seed_under("/bin/zsh", "/bin/bash", None);
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        fx.rc_files(),
        [".zshrc"],
        "the pass changed the home: {text}"
    );
    let state = if cfg!(target_os = "macos") {
        fx.home.join("Library/Application Support/aterm")
    } else {
        fx.home.join(".local/state/aterm")
    };
    let journal = state.join("converge/journal.ndjson");
    let rows = std::fs::read_to_string(&journal)
        .unwrap_or_else(|e| panic!("no journal at {}: {e}\n{text}", journal.display()));
    assert!(
        rows.contains(atpkg::converge::repair::RC),
        "the rc change was journaled there: {rows}"
    );
}

/// install.sh's PATH half, `shell-setup --path`, elects its rc from the same account record
/// as the seed (design init-system §4.2: the login shell "never from `$SHELL`"): with
/// `$SHELL` naming bash and the login shell zsh, it writes `~/.zshrc`, and no bash profile
/// appears. NEGATIVE CONTROL: a login bash with `$SHELL` naming zsh gets bash's profile.
#[test]
fn shell_setup_elects_the_login_shell_s_rc_never_shell() {
    let fx = Fixture::new("shell-setup-zsh");
    let out = fx.shell_setup("/bin/zsh", "/bin/bash");
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    assert_eq!(fx.rc_files(), [".zshrc"], "{text}");
    let zshrc = std::fs::read_to_string(fx.rc(".zshrc")).unwrap();
    assert!(
        zshrc.contains(atpkg::selfsetup::PATH_BLOCK_START)
            && zshrc.contains(".aterm/shell.d/00-atpkg.zsh"),
        "{zshrc:?}"
    );

    let fx = Fixture::new("shell-setup-bash");
    let out = fx.shell_setup("/bin/bash", "/bin/zsh");
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    let bash_rc = if cfg!(target_os = "macos") {
        ".bash_profile"
    } else {
        ".bashrc"
    };
    assert_eq!(fx.rc_files(), [bash_rc], "{text}");
}

/// The seed reads the Command Line Tools without running anything (design init-system §4.2
/// `base.clt`, A15): with a toolset in the store and no `cc` on `PATH` it says `c-toolchain:
/// missing`, so a toolset that arrived with no window watching (an install over SSH) is
/// filed at the next launch; with a `cc` found it says `ready`, which ends the entry.
/// NEGATIVE CONTROL: an empty store says nothing, leaving the filing to the pass that lays
/// the set down.
#[cfg(target_os = "macos")]
#[test]
fn the_seed_files_missing_command_line_tools_once_a_toolset_is_here() {
    needs_a_pinned_root();
    let marker = |out: &Output| -> Vec<String> {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.contains(atpkg::cli::C_TOOLCHAIN_MARKER))
            .map(str::to_owned)
            .collect()
    };
    let missing = format!(
        "atpkg: {}{}",
        atpkg::cli::C_TOOLCHAIN_MARKER,
        atpkg::cli::C_TOOLCHAIN_MISSING
    );
    let ready = format!(
        "atpkg: {}{}",
        atpkg::cli::C_TOOLCHAIN_MARKER,
        atpkg::cli::C_TOOLCHAIN_READY
    );
    let fx = Fixture::new("clt");
    let no_cc = fx.root.path().join("no-cc");
    std::fs::create_dir_all(&no_cc).unwrap();
    let state = fx.root.path().join("state");
    let out = fx.seed_with("/bin/zsh", "/bin/zsh", Some(&state), Some(&no_cc));
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(marker(&out), Vec::<String>::new(), "{}", describe(&out));

    // A build in the store, its tool on the managed bin/ (a legacy link shim).
    let prefix = atpkg::store::default_prefix(&fx.home);
    let tool = prefix.join("store/fakeprog/7/bin/fakeprog");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(&tool, prefix.join("bin/fakeprog")).unwrap();
    let out = fx.seed_with("/bin/zsh", "/bin/zsh", Some(&state), Some(&no_cc));
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(marker(&out), [missing], "{}", describe(&out));

    // A compiler on PATH (not the install shim): found, so `ready`, and nothing is run.
    let with_cc = fx.root.path().join("with-cc");
    std::fs::create_dir_all(&with_cc).unwrap();
    let ran = fx.root.path().join("cc-ran");
    let cc = with_cc.join("cc");
    std::fs::write(&cc, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    std::fs::set_permissions(&cc, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = fx.seed_with("/bin/zsh", "/bin/zsh", Some(&state), Some(&with_cc));
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(marker(&out), [ready], "{}", describe(&out));
    assert!(!ran.exists(), "the seed ran the compiler");
}
