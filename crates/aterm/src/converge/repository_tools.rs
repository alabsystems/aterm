// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A joined headless account owns the same unattended package attempt as init. It
//! runs after the base lock and fleet work, separately from repository acquisition.

use aterm_converge::{Class, Explain, Item, Layer, Probe, Remedy, Requires, StoodAside};
use aterm_machines::init::BaseLock;
use aterm_machines::init_toolset::Toolset;
use aterm_machines::items::{Ctx, Item as MachineItem};
use aterm_machines::port::{Live, System};
use aterm_secrets::client::{self, Paths};
use aterm_types::macos_setup::Setting;
use std::sync::{Arc, Mutex, PoisonError};

const ID: &str = "sync.repository-prerequisites";
type Current = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

struct Admission {
    ctx: Ctx,
    current: Current,
    continuing: Current,
}

enum Scope {
    Off(String),
    Waiting(String),
    Admitted(Box<Admission>),
}

enum Gate {
    Off(String),
    Unread(String),
}

impl Gate {
    fn why(self) -> String {
        match self {
            Self::Off(why) | Self::Unread(why) => why,
        }
    }
}

type Capture = Box<dyn Fn(&dyn System) -> Result<Scope, String> + Send + Sync>;

pub(super) struct Prerequisites {
    sys: Arc<dyn System + Send>,
    toolset: Toolset,
    capture: Capture,
    admitted: Mutex<Option<Box<Admission>>>,
    aside: Mutex<Option<StoodAside>>,
}

impl Prerequisites {
    pub(super) fn live() -> Self {
        Self::new(
            Arc::new(Live::new()),
            crate::doctor::toolset(BaseLock::Try),
            Box::new(capture),
        )
    }

    fn new(sys: Arc<dyn System + Send>, toolset: Toolset, capture: Capture) -> Self {
        Self {
            sys,
            toolset,
            capture,
            admitted: Mutex::new(None),
            aside: Mutex::new(None),
        }
    }

    fn waiting(why: String) -> Probe {
        Probe::Deferred { why }
    }
}

/// Package choices are read afresh, including a damaged removal record. The actual
/// seed/update commands retain their own policy checks under the package store lock.
fn packages() -> Result<(), Gate> {
    let cfg = atpkg::config::load();
    let layout = atpkg::store::resolve_from(&cfg);
    packages_in(&cfg, layout.as_ref())
}

fn packages_in(
    cfg: &atpkg::config::PackagesConfig,
    layout: Option<&atpkg::store::Layout>,
) -> Result<(), Gate> {
    if cfg.unreadable {
        return Err(Gate::Unread(
            "package choices cannot be read; repository tools wait".into(),
        ));
    }
    if !cfg.enabled() || !cfg.installs_unattended() {
        return Err(Gate::Off("automatic package installation is off".into()));
    }
    let layout = layout.ok_or_else(|| Gate::Unread("no package store is configured".into()))?;
    if cfg.exclude().iter().any(|name| name == "git") {
        return Err(Gate::Off(
            "Git is excluded in package settings; repository tools stay unchanged".into(),
        ));
    }
    if layout
        .removed_programs()
        .map_err(|_| {
            Gate::Unread("package removal choices cannot be read; repository tools wait".into())
        })?
        .contains("git")
    {
        return Err(Gate::Off(
            "Git was removed by you; automatic setup leaves it unchanged".into(),
        ));
    }
    if let Some(why) = atpkg::adoption::unattended_install_off(layout, cfg) {
        return Err(Gate::Off(why.into()));
    }
    Ok(())
}

fn current_scope(sys: &dyn System, ctx: &Ctx, paths: &Paths) -> Result<(), Gate> {
    check_scope(
        sys,
        ctx,
        paths,
        super::vault_held(),
        &super::setup_holds(),
        aterm_secrets::swarm::enabled_in_home(&paths.home).map_err(|why| why.to_string()),
    )?;
    packages()
}

fn check_scope(
    sys: &dyn System,
    ctx: &Ctx,
    paths: &Paths,
    vault_hold: Option<String>,
    holds: &aterm_machines::init::Holds,
    sharing: Result<bool, String>,
) -> Result<(), Gate> {
    let now = super::repository_context(sys).map_err(Gate::Unread)?;
    if now.users != ctx.users || paths.home != std::path::Path::new(&ctx.users[0].home) {
        return Err(Gate::Unread("the repository tools account changed".into()));
    }
    if let Some(why) = vault_hold {
        return Err(Gate::Off(why));
    }
    if holds.holds(Setting::Repositories) {
        return Err(Gate::Off(holds.why(Setting::Repositories)));
    }
    if !sharing.map_err(Gate::Unread)? {
        return Err(Gate::Off(aterm_secrets::swarm::SWARM_OFF.into()));
    }
    if !aterm_machines::init_github::uses_vault_inventory(sys, ctx).map_err(Gate::Unread)? {
        return Err(Gate::Off(
            "the repository source no longer uses the private vault inventory".into(),
        ));
    }
    Ok(())
}

fn capture(sys: &dyn System) -> Result<Scope, String> {
    let ctx = super::repository_context(sys)?;
    let paths = Paths::live().map_err(|why| why.to_string())?;
    match current_scope(sys, &ctx, &paths) {
        Ok(()) => {}
        Err(Gate::Off(why)) => return Ok(Scope::Off(why)),
        Err(Gate::Unread(why)) => return Ok(Scope::Waiting(why)),
    }
    let expected = ctx.clone();
    let selected = paths.clone();
    admit(
        ctx,
        paths,
        Box::new(move || current_scope(&Live::new(), &expected, &selected).map_err(Gate::why)),
    )
}

fn admit(ctx: Ctx, paths: Paths, policy: Current) -> Result<Scope, String> {
    policy()?;
    let Some(config) = super::vault_config(&paths)? else {
        return Ok(Scope::Off(
            "no private repository vault is configured".into(),
        ));
    };
    let Some(ready) = client::with_materialization_ready_for_entries(
        &paths,
        &config,
        &aterm_machines::init_github::VAULT_INPUT_ENTRIES,
        || Ok(()),
    )
    .map_err(|why| why.to_string())?
    else {
        return Ok(Scope::Waiting(super::REPOSITORIES_SYNCING.into()));
    };
    policy()?;
    let policy: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::from(policy);
    let continuing_policy = Arc::clone(&policy);
    let continuing_snapshot = ready.snapshot.clone();
    let continuing_paths = paths.clone();
    Ok(Scope::Admitted(Box::new(Admission {
        ctx,
        current: Box::new(move || {
            policy()?;
            if !ready
                .snapshot
                .is_current_now(&paths)
                .map_err(|why| why.to_string())?
            {
                return Err(super::REPOSITORIES_SYNCING.into());
            }
            policy()
        }),
        continuing: Box::new(move || {
            continuing_policy()?;
            if !continuing_snapshot
                .source_continues(&continuing_paths)
                .map_err(|why| why.to_string())?
            {
                return Err(super::REPOSITORIES_SYNCING.into());
            }
            continuing_policy()
        }),
    })))
}

impl Item for Prerequisites {
    fn id(&self) -> String {
        ID.into()
    }
    fn family(&self) -> &'static str {
        aterm_machines::sync::FAMILY
    }
    fn title(&self) -> String {
        "Git prerequisite for automatic repositories".into()
    }
    fn layer(&self) -> Layer {
        Layer::Workstation
    }
    fn class(&self) -> Class {
        Class::Install
    }
    fn requires(&self) -> Requires {
        Requires::Account
    }
    fn probe(&self) -> Probe {
        let mut admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        if admitted.is_none() {
            match (self.capture)(self.sys.as_ref()) {
                Ok(Scope::Off(why)) => return Probe::NotApplicable { why },
                Ok(Scope::Waiting(why)) | Err(why) => return Self::waiting(why),
                Ok(Scope::Admitted(ready)) => *admitted = Some(ready),
            }
        }
        let ready = admitted.as_ref().expect("captured above");
        if let Err(why) = (ready.current)() {
            return Self::waiting(why);
        }
        // The actual capability check starts no Git child while the vault is
        // locked. Its result cannot outlive source or package-policy admission.
        let usable =
            aterm_machines::init_github::git_ready_for_inventory(self.sys.as_ref(), &ready.ctx);
        if let Err(why) = (ready.current)() {
            return Self::waiting(why);
        }
        let usable = match usable {
            Ok(usable) => usable,
            Err(why) => return Self::waiting(why),
        };
        if usable {
            return Probe::Ok {
                now: "Git meets the selected repositories' requirements".into(),
            };
        }
        match self.toolset.probe(self.sys.as_ref(), &ready.ctx) {
            // The aggregate counts active packages. A missing ready marker or an
            // unusable executable can still require the signed install/update path.
            Probe::Ok { .. } => Probe::Change {
                from: "the active toolset has no usable Git for repository setup".into(),
                to: "retry its admitted unattended package attempt".into(),
            },
            probe => probe,
        }
    }
    fn apply(&self) -> Result<(), String> {
        let admitted = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        let ready = admitted
            .as_ref()
            .ok_or("repository tools have no admitted source")?;
        let result = self.toolset.apply_with_guards(
            self.sys.as_ref(),
            ready.current.as_ref(),
            ready.continuing.as_ref(),
        );
        if let Err(why) = (ready.current)() {
            *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(StoodAside::Waiting(why));
            return Ok(());
        }
        result
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .or_else(|| self.toolset.stood_aside())
    }
    fn explain(&self, probe: &Probe) -> Explain {
        let why = match probe {
            Probe::Ok { now } => now,
            Probe::Change { from, .. } => from,
            Probe::NotApplicable { why }
            | Probe::Deferred { why }
            | Probe::Blocked { why }
            | Probe::Waits { why, .. }
            | Probe::Unusable { why } => why,
            Probe::Person { how, .. } => how,
        };
        Explain {
            why: why.clone(),
            remedy: Remedy::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_machines::init::Holds;
    use aterm_machines::init_toolset::{Atpkg, Found};
    use aterm_machines::port::{Fake, Ran};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Choices {
        holds: Mutex<Holds>,
        packages: Mutex<atpkg::config::PackagesConfig>,
        // This is the injected package-reader's existing consent result. No
        // package command runs in these fixtures or overrides a real manager.
        removed: AtomicBool,
        calls: Mutex<Vec<String>>,
    }

    fn fixture() -> (Arc<Fake>, Arc<Choices>, Prerequisites) {
        fixture_at("/fixture/repository-user")
    }

    fn inventory_bytes(required: Option<&[&str]>) -> Vec<u8> {
        let repositories: Vec<_> = required
            .into_iter()
            .map(|paths| {
                aterm_json::json!({
                    "id":1,"name":"private-code","owner":{"login":"fixture"},
                    "full_name":"fixture/private-code","private":true,
                    "clone_url":"https://github.com/fixture/private-code.git",
                    "default_branch":"main","required_submodules":paths
                })
            })
            .collect();
        aterm_json::to_vec(&aterm_json::json!({
            "schema":2,"account":"fixture","repositories":repositories
        }))
        .unwrap()
    }

    fn inventory(sys: &Fake, home: &str, required: Option<&[&str]>) {
        let bytes = inventory_bytes(required);
        let path = std::path::Path::new(home).join(".secrets/github-repositories.json");
        sys.file(path.to_str().unwrap(), &bytes).stat_of(
            path.to_str().unwrap(),
            aterm_machines::port::Stat {
                uid: 501,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
    }

    fn fixture_at(home: &str) -> (Arc<Fake>, Arc<Choices>, Prerequisites) {
        let mut sys = Fake::default();
        sys.euid = 501;
        let who = aterm_machines::model::User {
            name: "fixture".into(),
            uid: 501,
            gid: 20,
            home: home.into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        sys.logged_in(&who);
        inventory(&sys, home, Some(&[]));
        let ctx = super::super::repository_context(&sys).unwrap();
        let sys = Arc::new(sys);
        let choices = Arc::new(Choices {
            holds: Mutex::new(Holds::NONE),
            packages: Mutex::new(atpkg::config::PackagesConfig::default()),
            removed: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        });
        let found = Arc::clone(&sys);
        let landing = Arc::clone(&sys);
        let called = Arc::clone(&choices);
        let tools = Toolset::new(
            Box::new(move || {
                if aterm_machines::init_github::git_installed(found.as_ref()) {
                    Found::Complete("Git is ready".into())
                } else {
                    Found::Missing("Git is missing".into())
                }
            }),
            Box::new(move |args, _, _| {
                called.calls.lock().unwrap().push(args.join(" "));
                if args.first().is_some_and(|verb| verb == "update") {
                    landing.file(aterm_machines::items::github::CLT_GIT, b"fixture Git");
                    landing.file(aterm_machines::items::github::SYSTEM_GIT, b"fixture Git");
                }
                Ok(Ran::ok(""))
            }),
            None,
            Atpkg {
                update_flags: &["--defer-busy-flip"],
                left_for_next_pass: |_| Vec::new(),
            },
        );
        let check_sys = Arc::clone(&sys);
        let check_choices = Arc::clone(&choices);
        let capture: Capture = Box::new(move |_| {
            let paths = Paths::locate_for_home(std::path::Path::new(&ctx.users[0].home));
            let selected = ctx.clone();
            let sys = Arc::clone(&check_sys);
            let choices = Arc::clone(&check_choices);
            let current: Current = Box::new(move || {
                check_scope(
                    sys.as_ref(),
                    &selected,
                    &paths,
                    None,
                    &choices.holds.lock().unwrap(),
                    Ok(true),
                )
                .map_err(Gate::why)?;
                let cfg = choices.packages.lock().unwrap();
                if !cfg.enabled()
                    || !cfg.installs_unattended()
                    || choices.removed.load(Ordering::SeqCst)
                {
                    return Err("package installation held".into());
                }
                Ok(())
            });
            if let Err(why) = current() {
                return Ok(Scope::Off(why));
            }
            let current: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::from(current);
            let continuing = Arc::clone(&current);
            Ok(Scope::Admitted(Box::new(Admission {
                ctx: ctx.clone(),
                current: Box::new(move || current()),
                continuing: Box::new(move || continuing()),
            })))
        });
        let item = Prerequisites::new(sys.clone(), tools, capture);
        (sys, choices, item)
    }

    #[test]
    fn explicit_holds_and_package_choices_start_no_prerequisite_child() {
        let (_, choices, allowed) = fixture();
        assert!(matches!(allowed.probe(), Probe::Change { .. }));
        allowed.apply().unwrap();
        assert!(
            matches!(allowed.probe(), Probe::Ok { .. }),
            "new Git is read back through installed_git"
        );
        assert_eq!(
            *choices.calls.lock().unwrap(),
            ["seed", "update --defer-busy-flip"]
        );
        let (sys, choices, empty) = fixture();
        inventory(&sys, "/fixture/repository-user", None);
        assert!(
            matches!(empty.probe(), Probe::Ok { .. }),
            "an empty inventory needs no Git"
        );
        assert!(choices.calls.lock().unwrap().is_empty());
        for choice in [
            "package-off",
            "removed",
            "repositories-off",
            "stop",
            "profile-off",
        ] {
            let (sys, choices, item) = fixture();
            match choice {
                "package-off" => {
                    *choices.packages.lock().unwrap() =
                        atpkg::config::parse_packages("[packages]\nenabled=false\n")
                }
                "removed" => choices.removed.store(true, Ordering::SeqCst),
                "repositories-off" => choices
                    .holds
                    .lock()
                    .unwrap()
                    .off
                    .set(Setting::Repositories, true),
                "stop" => choices.holds.lock().unwrap().stopped = Some("setup stopped".into()),
                "profile-off" => {
                    let bytes = b"[github]\nenabled=false\n";
                    let path = "/fixture/repository-user/.config/aterm/init.toml";
                    sys.file(path, bytes).stat_of(
                        path,
                        aterm_machines::port::Stat {
                            uid: 501,
                            gid: 20,
                            mode: 0o600,
                            dir: false,
                            symlink: false,
                            len: bytes.len() as u64,
                        },
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(item.probe(), Probe::NotApplicable { .. }),
                "{choice}"
            );
            assert!(choices.calls.lock().unwrap().is_empty(), "{choice}");
        }
    }

    #[test]
    fn package_cancellation_is_nonwaiting_and_full_readiness_still_guards_each_pass() {
        for change in ["source", "file", "writer", "stop"] {
            use client::{JoinOptions, JoinPassword};
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let source = root.path().join("private-tools.secrets");
            let paths = Paths::for_home(&home);
            let before = format!(
                "gh_access_token=fixture-one\ngithub-repositories.json={}\n",
                String::from_utf8(inventory_bytes(Some(&[]))).unwrap()
            );
            aterm_uds::atomic_file::write(&source, before.as_bytes(), 0o600).unwrap();
            client::join(
                &paths,
                &JoinOptions {
                    source: source.to_string_lossy().into_owned(),
                    host: Some("fixture".into()),
                    interactive: false,
                    wait: std::time::Duration::ZERO,
                    confirm: &|_| false,
                    password: JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
            let (_, choices, mut item) = fixture_at(home.to_str().unwrap());
            let original = item.capture;
            let scoped = paths.clone();
            item.capture = Box::new(move |sys| match original(sys)? {
                Scope::Admitted(ready) => admit(ready.ctx, scoped.clone(), ready.current),
                other => Ok(other),
            });
            let calls = Arc::clone(&choices);
            item.toolset = Toolset::new(
                Box::new(|| Found::Missing("Git is missing".into())),
                Box::new(move |args, _, stopped| {
                    calls.calls.lock().unwrap().push(args.join(" "));
                    assert!(!stopped(), "unchanged admission continues");
                    match change {
                        "source" => {
                            aterm_uds::atomic_file::write(
                            &source,
                            b"gh_access_token=fixture-two\ngithub-repositories.json={\"schema\":2,\"account\":\"fixture\",\"repositories\":[]}\n",
                            0o600,
                        )
                        .unwrap();
                            client::sync(&paths, None).unwrap();
                            assert!(stopped(), "changed recorded inputs cancel the child");
                        }
                        "file" => {
                            std::fs::write(paths.home.join(".secrets/gh_access_token"), b"changed")
                                .unwrap();
                            assert!(!stopped(), "continuation does not grant file integrity");
                        }
                        "writer" => {
                            let writer = client::test_hold_state_lock(&paths).unwrap();
                            assert!(
                                stopped(),
                                "a busy writer cancels without waiting for release"
                            );
                            drop(writer);
                            assert!(
                                !stopped(),
                                "the unchanged source is observable after release"
                            );
                            return Err(aterm_machines::port::RunFailed::TimedOut(
                                std::time::Duration::ZERO,
                            ));
                        }
                        "stop" => {
                            calls.holds.lock().unwrap().stopped = Some("setup stopped".into());
                            assert!(stopped(), "Stop is checked during the child");
                        }
                        _ => unreachable!(),
                    }
                    Ok(Ran::ok(""))
                }),
                None,
                Atpkg {
                    update_flags: &[],
                    left_for_next_pass: |_| Vec::new(),
                },
            );
            assert!(matches!(item.probe(), Probe::Change { .. }));
            let result = item.apply();
            assert_eq!(*choices.calls.lock().unwrap(), ["seed"], "{change}");
            if change == "writer" {
                assert!(
                    result.is_err(),
                    "cancelled package attempt remains unfinished"
                );
                assert!(matches!(item.probe(), Probe::Change { .. }));
            } else {
                result.unwrap();
                assert!(matches!(item.stood_aside(), Some(StoodAside::Waiting(_))));
                assert!(
                    matches!(item.probe(), Probe::Deferred { .. }),
                    "the full boundary refuses {change} and starts no update pass"
                );
            }
        }
    }

    #[test]
    fn active_toolset_without_the_git_ready_marker_still_gets_a_package_attempt() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let (sys, choices, mut item) = fixture_at(home.to_str().unwrap());
        let layout = atpkg::store::Layout {
            prefix: home.join("Library/Application Support/aterm/pkg"),
        };
        let build = layout.build_dir("git", 17);
        let payload = build.join("bin/git");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::write(&payload, b"fixture payload, never executed").unwrap();
        atpkg::install_shims(&layout, &build, &["git".into()], atpkg::Aliases::Off).unwrap();
        atpkg::activate_build(&layout, &build).unwrap();
        let row = atpkg::doctor::toolset_row(&layout).unwrap();
        assert_eq!(
            row.outcome,
            aterm_converge::Outcome::Ok,
            "actual aggregate producer"
        );
        assert_eq!(atpkg::active_builds(&layout).get("git"), Some(&17));
        assert!(!build.with_extension("ready").exists());

        // Reuse the real scratch execution layout through the injected port, with
        // no fallback to the host's installed developer tools or protected state.
        for path in [&payload, &layout.bin_dir().join("git")] {
            sys.file(path.to_str().unwrap(), &std::fs::read(path).unwrap())
                .stat_of(path.to_str().unwrap(), Live::new().stat(path).unwrap());
        }
        let current = layout.program_current("git");
        sys.link(
            current.to_str().unwrap(),
            std::fs::read_link(&current).unwrap().to_str().unwrap(),
        );
        assert!(
            !aterm_machines::init_github::git_installed(sys.as_ref()),
            "actual Git selector refuses the incomplete activation"
        );
        let calls = Arc::clone(&choices);
        let online = Arc::new(AtomicBool::new(false));
        let available = Arc::clone(&online);
        let landing = Arc::clone(&sys);
        let selected_build = build.clone();
        let selected_payload = payload.clone();
        item.toolset = Toolset::new(
            Box::new(move || {
                crate::doctor::toolset_reads(
                    atpkg::doctor::toolset_row(&layout).as_ref(),
                    atpkg::Adoption::Consented,
                    true,
                    None,
                )
            }),
            Box::new(move |args, _, _| {
                calls.calls.lock().unwrap().push(args.join(" "));
                if !available.load(Ordering::SeqCst) {
                    return Ok(Ran::exit(69, "offline"));
                }
                if args.first().is_some_and(|verb| verb == "update") {
                    let mut stat = Live::new().stat(&selected_payload).unwrap();
                    stat.mode = 0o755;
                    landing.stat_of(selected_payload.to_str().unwrap(), stat);
                    landing.file(
                        selected_build.with_extension("ready").to_str().unwrap(),
                        b"ready",
                    );
                }
                Ok(Ran::ok(""))
            }),
            None,
            Atpkg {
                update_flags: &[],
                left_for_next_pass: |_| Vec::new(),
            },
        );
        assert!(
            matches!(item.probe(), Probe::Change { .. }),
            "missing Git must get one bounded attempt despite aggregate completeness"
        );
        item.apply().unwrap();
        assert_eq!(*choices.calls.lock().unwrap(), ["seed"]);

        // A native Git can be installed yet incapable of the read-only object
        // operations required by this private inventory. Presence is not readiness.
        for path in [
            aterm_machines::items::github::CLT_GIT,
            aterm_machines::items::github::SYSTEM_GIT,
        ] {
            sys.file(path, b"old native Git");
        }
        sys.simulate(|_, command| {
            command
                .args
                .iter()
                .any(|arg| arg == "--no-lazy-fetch")
                .then(|| {
                    if [
                        aterm_machines::items::github::CLT_GIT,
                        aterm_machines::items::github::SYSTEM_GIT,
                    ]
                    .iter()
                    .any(|path| command.program == std::path::Path::new(path))
                    {
                        Ran::exit(129, "unknown option: --no-lazy-fetch")
                    } else {
                        Ran::ok("capable Git")
                    }
                })
        });
        inventory(
            sys.as_ref(),
            home.to_str().unwrap(),
            Some(&["vendor/library"]),
        );
        assert!(aterm_machines::init_github::git_installed(sys.as_ref()));
        assert!(
            matches!(item.probe(), Probe::Change { .. }),
            "installed Git must not strand required dependencies when it refuses the safe object-read capability"
        );
        let probes = || {
            sys.calls()
                .iter()
                .filter(|call| call.contains("--no-lazy-fetch"))
                .count()
        };
        let before = probes();
        inventory(sys.as_ref(), home.to_str().unwrap(), Some(&[]));
        assert!(
            matches!(item.probe(), Probe::Ok { .. }),
            "ordinary clones need no dependency capability"
        );
        assert_eq!(probes(), before, "no unnecessary capability process");

        inventory(
            sys.as_ref(),
            home.to_str().unwrap(),
            Some(&["vendor/library"]),
        );
        sys.simulate(|_, _| None);
        assert!(
            matches!(item.probe(), Probe::Deferred { .. }),
            "a probe that could not run is not an install instruction"
        );
        let selected_home = home.clone();
        sys.simulate(move |_, command| {
            command
                .args
                .iter()
                .any(|arg| arg == "--no-lazy-fetch")
                .then(|| {
                    if command.program.starts_with(&selected_home) {
                        Ran::ok("capable Git")
                    } else {
                        Ran::exit(129, "unknown option: --no-lazy-fetch")
                    }
                })
        });
        online.store(true, Ordering::SeqCst);
        choices.calls.lock().unwrap().clear();
        assert!(matches!(item.probe(), Probe::Change { .. }));
        item.apply().unwrap();
        assert_eq!(*choices.calls.lock().unwrap(), ["seed", "update"]);
        assert!(
            matches!(item.probe(), Probe::Ok { .. }),
            "normal package update selects capable managed Git"
        );
    }

    #[test]
    fn package_exclusion_removal_and_unreadable_choices_are_not_overridden() {
        let root = aterm_tempfile::tempdir().unwrap();
        let layout = atpkg::store::Layout {
            prefix: root.path().join("packages"),
        };
        std::fs::create_dir_all(&layout.prefix).unwrap();
        for text in [
            "[packages]\nenabled=false\n",
            "[packages]\nauto_install=false\n",
            "[packages]\nexclude=['git']\n",
        ] {
            assert!(matches!(
                packages_in(&atpkg::config::parse_packages(text), Some(&layout)),
                Err(Gate::Off(_))
            ));
        }
        std::fs::write(layout.removed(), b"git\n").unwrap();
        assert!(matches!(
            packages_in(&atpkg::config::PackagesConfig::default(), Some(&layout)),
            Err(Gate::Off(_))
        ));
        std::fs::remove_file(layout.removed()).unwrap();
        std::fs::create_dir(layout.removed()).unwrap();
        assert!(matches!(
            packages_in(&atpkg::config::PackagesConfig::default(), Some(&layout)),
            Err(Gate::Unread(_))
        ));
    }
}
