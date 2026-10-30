// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

type Release = (u64, u64, u64);

struct FakeSupervisor {
    pid: RefCell<Option<u32>>,
    loads: RefCell<usize>,
    unloads: RefCell<usize>,
    replaced: std::cell::Cell<bool>,
    misplaced: RefCell<Option<String>>,
    renews: RefCell<usize>,
    stop_error: RefCell<Option<&'static str>>,
    releases: RefCell<std::collections::BTreeMap<PathBuf, Option<Release>>>,
    release_probes: RefCell<Vec<PathBuf>>,
    off_on_probe: RefCell<Option<(PathBuf, PathBuf)>>,
}

impl FakeSupervisor {
    fn new(pid: Option<u32>) -> Self {
        Self {
            pid: RefCell::new(pid),
            loads: RefCell::new(0),
            unloads: RefCell::new(0),
            replaced: std::cell::Cell::new(false),
            misplaced: RefCell::new(None),
            renews: RefCell::new(0),
            stop_error: RefCell::new(None),
            releases: RefCell::new(std::collections::BTreeMap::new()),
            release_probes: RefCell::new(Vec::new()),
            off_on_probe: RefCell::new(None),
        }
    }
}

impl ServiceSupervisor for FakeSupervisor {
    fn unit_file(&self, home: &Path) -> PathBuf {
        plist_path(home)
    }

    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String {
        node_plist_text(program, home, state_dir, Some("com.aterm.aterm"))
    }

    fn running_pid(&self, _home: &Path) -> Result<Option<u32>, String> {
        Ok(*self.pid.borrow())
    }

    fn program_release(&self, program: &Path) -> Result<Release, String> {
        self.release_probes.borrow_mut().push(program.to_path_buf());
        if let Some((home, state)) = self.off_on_probe.borrow_mut().take() {
            remove_with(&home, &state, true, self).unwrap();
        }
        self.releases
            .borrow()
            .get(program)
            .copied()
            .unwrap_or(Some(required_node_release().unwrap()))
            .ok_or_else(|| "program did not identify as aterm".to_owned())
    }

    fn load_and_start(&self, _unit_path: &Path) -> Result<(), String> {
        *self.loads.borrow_mut() += 1;
        *self.pid.borrow_mut() = Some(4242);
        Ok(())
    }

    fn stop_and_unload(&self, _unit_path: &Path) -> Result<(), String> {
        *self.unloads.borrow_mut() += 1;
        if let Some(error) = *self.stop_error.borrow() {
            return Err(error.to_owned());
        }
        *self.pid.borrow_mut() = None;
        Ok(())
    }

    fn misplaced(&self) -> Option<String> {
        self.misplaced.borrow().clone()
    }

    fn runs_replaced(&self, _pid: u32, _program: &Path) -> bool {
        self.replaced.get()
    }

    fn renew(&self) -> Result<(), String> {
        *self.renews.borrow_mut() += 1;
        self.replaced.set(false);
        Ok(())
    }
}

#[cfg(unix)]
#[test]
fn node_job_lock_prevents_concurrent_bind_and_releases_on_drop_with_negative_control() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");

    assert!(!node_job_holds_lock(&state));
    let held = NodeJobLock::try_acquire(&state)
        .unwrap()
        .expect("first node job acquires lock");
    assert!(node_job_holds_lock(&state));
    assert!(
        NodeJobLock::try_acquire(&state).unwrap().is_none(),
        "second node job must not acquire while first holds lock"
    );

    // Negative control: a GUI bind policy that ignores node_job_holds_lock attempts to bind 7100
    // alongside the node job; the real policy yields whenever node_job_holds_lock is true.
    let gui_should_bind = |lock_held: bool| !lock_held;
    let buggy_both_bind = |_lock_held: bool| true;
    assert!(!gui_should_bind(node_job_holds_lock(&state)));
    assert!(buggy_both_bind(node_job_holds_lock(&state)));

    drop(held);
    assert!(!node_job_holds_lock(&state));
    assert!(gui_should_bind(node_job_holds_lock(&state)));

    // Concurrent `node_job_holds_lock` probes use `LOCK_SH` so they never contend with each other
    // (no false-positive `true` when no `NodeJobLock` is held) and never block `try_acquire`.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let false_positives = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::scope(|s| {
        for _ in 0..4 {
            let b = std::sync::Arc::clone(&barrier);
            let fp = std::sync::Arc::clone(&false_positives);
            let st = state.clone();
            s.spawn(move || {
                b.wait();
                for _ in 0..200 {
                    if node_job_holds_lock(&st) {
                        fp.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            });
        }
    });
    assert_eq!(
        false_positives.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "concurrent node_job_holds_lock probes must never see each other as an active node job"
    );
}

#[test]
fn alab_tcp_txt_carries_only_four_anonymous_fields_and_info_plist_lists_service() {
    let txt = AlabTcpTxt {
        role: NodeRole::Steward,
        fleet_port: 7100,
        window: false,
    };
    let pairs = txt.txt_pairs();
    assert_eq!(
        pairs,
        [
            "v=1".to_owned(),
            "role=steward".to_owned(),
            "fleet=7100".to_owned(),
            "window=0".to_owned(),
        ]
    );
    let args = alab_tcp_dns_sd_args("alab-deadbeef", txt);
    assert_eq!(
        args,
        vec![
            "-R",
            "alab-deadbeef",
            "_alab._tcp",
            "local",
            "7100",
            "v=1",
            "role=steward",
            "fleet=7100",
            "window=0",
        ]
    );
    for token in &args {
        assert!(!token.starts_with("label="), "leaked label: {token}");
        assert!(!token.starts_with("node="), "leaked node id: {token}");
        assert!(!token.starts_with("version="), "leaked version: {token}");
    }

    let plist = include_str!("../../../apps/aterm-mac/Info.plist");
    assert!(
        plist.contains("<string>_alab._tcp</string>"),
        "Info.plist NSBonjourServices must declare _alab._tcp (C23)"
    );
}

#[test]
fn alab_tcp_is_advertised_on_every_network_a_member_is_on() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let unconfigured = aterm_secrets::client::Paths::for_home(&tmp.path().join("no-vault"));
    let foreign_home = tmp.path().join("foreign-vault");
    let foreign_paths = aterm_secrets::client::Paths::for_home(&foreign_home);
    let foreign_file = tmp.path().join("foreign.vault");
    aterm_secrets::client::create(
        &foreign_paths,
        foreign_file.to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();

    let office_routes = vec!["192.168.1.42".to_owned()];
    let cafe_routes = vec!["172.20.10.4".to_owned()];
    let now = 1_800_000_000;
    let heard = Some(now - 60);
    for routes in [&office_routes, &cafe_routes] {
        assert!(
            advertises(true, routes, heard, now),
            "a member advertises on {routes:?}: no network is special"
        );
    }
    assert!(!advertises(true, &[], heard, now), "offline");
    assert!(!advertises(false, &office_routes, heard, now), "swarm off");
    assert!(
        !advertises(true, &office_routes, None, now),
        "no ALab vault member"
    );
    assert!(
        !advertises(
            true,
            &office_routes,
            Some(now - ALAB_MEMBER_TTL_SECS - 1),
            now
        ),
        "its vault unheard a day"
    );
    for paths in [&unconfigured, &foreign_paths] {
        assert!(
            !should_advertise_alab_tcp_with(paths, None, &cafe_routes, now).0,
            "{}: no ALab vault, no advert",
            paths.home.display()
        );
    }
}

#[test]
fn node_units_stay_separate_from_secrets_and_put_atpkg_bin_first_on_path() {
    let home = Path::new("/Users//a&b");
    let state = Path::new("/Users//a&b/.local/state/aterm");
    let prog = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");

    let plist = node_plist_text(prog, home, state, Some("com.aterm.aterm"));
    assert!(plist.contains("<string>systems.alab.aterm-node</string>"));
    assert!(!plist.contains("systems.alab.aterm-secrets"));
    assert!(
        plist.contains(
            "<string>/Users//a&amp;b/Library/Application Support/aterm/pkg/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>"
        ),
        "plist PATH must put atpkg pkg/bin first: {plist}"
    );
    assert_eq!(kept_program_in_unit(&plist).as_deref(), Some(prog));

    let unit = node_systemd_unit_text(prog, Path::new("/home/alice"));
    assert!(
        unit.contains(
            "ExecStart=\"/Applications/aterm.app/Contents/MacOS/aterm\" \"node\" \"serve\""
        )
    );
    assert!(unit.contains(
        "Environment=\"PATH=/home/alice/.local/share/aterm/pkg/bin:/usr/local/bin:/usr/bin:/bin\""
    ));
    assert_eq!(kept_program_in_unit(&unit).as_deref(), Some(prog));
}

#[test]
fn sync_node_installs_respects_off_and_fleet_leave_removes_job() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let config = tmp.path().join("aterm.toml");
    let prog = tmp.path().join("aterm-bin");
    std::fs::write(&prog, b"stub").unwrap();
    std::fs::write(&config, "[fleet]\nlisten = \"0.0.0.0:7100\"\n").unwrap();
    let sup = FakeSupervisor::new(None);

    // Without fleet.key, sync.node is NotApplicable.
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::NotApplicable(why) if why.contains("in no fleet")
    ));

    // Write a valid fleet.key -> Due -> ensure_for_sync_with installs and starts it!
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    {
        use std::io::Write as _;
        let mut f =
            aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE).unwrap();
        f.write_all(b"0123456789abcdef0123456789abcdef01234567\n")
            .unwrap();
    }
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Due { .. }
    ));
    let applied =
        ensure_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap();
    assert!(applied.contains("installed and started"), "{applied}");
    assert_eq!(*sup.loads.borrow(), 1);
    assert!(plist_path(&home).is_file());
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Current(_)
    ));

    // `[swarm] enabled = false` makes sync.node NotApplicable.
    std::fs::write(&config, "[swarm]\nenabled = false\n").unwrap();
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::NotApplicable(why) if why == aterm_secrets::swarm::SWARM_OFF
    ));
    std::fs::write(&config, "[fleet]\nlisten = \"0.0.0.0:7100\"\n").unwrap();

    // `aterm fleet leave` removes the unit without recording node-off.
    let unit_before = std::fs::read(plist_path(&home)).unwrap();
    *sup.stop_error.borrow_mut() = Some("the supervisor refused to stop the loaded node");
    let refused = remove_with(&home, &state, false, &sup).unwrap_err();
    assert_eq!(refused, sup.stop_error.borrow().unwrap());
    assert_eq!(*sup.pid.borrow(), Some(4242));
    assert_eq!(std::fs::read(plist_path(&home)).unwrap(), unit_before);
    assert!(!node_turned_off(&state));
    *sup.stop_error.borrow_mut() = None;
    let removed = remove_with(&home, &state, false, &sup).unwrap();
    assert_eq!(removed, Some(plist_path(&home)));
    assert!(!plist_path(&home).is_file());
    assert!(!node_turned_off(&state));
    assert_eq!(remove_with(&home, &state, false, &sup).unwrap(), None);

    // `aterm node off` records node-off so sync.node stays NotApplicable until `aterm node on`.
    remove_with(&home, &state, true, &sup).unwrap();
    assert!(node_turned_off(&state));
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::NotApplicable(why) if why.contains("aterm node on")
    ));
    run_admin_subcommand("on", &[], &home, &state, &prog, &sup).unwrap();
    assert!(!node_turned_off(&state));
    assert!(plist_path(&home).is_file());
}

#[test]
fn systemd_unload_requires_a_stop_or_an_explicitly_absent_unit() {
    let absent = "LoadState=not-found\nActiveState=inactive\nMainPID=0\n";
    let loaded = "LoadState=loaded\nActiveState=active\nMainPID=77\n";
    for (stop, state, reload, removed) in [
        (true, Ok("unused"), true, true),
        (false, Ok(absent), true, true),
        (false, Ok(loaded), true, false),
        (false, Ok("LoadState=not-found\n"), true, false),
        (false, Ok("Access denied"), true, false),
        (
            false,
            Err("the user manager could not be queried"),
            true,
            false,
        ),
        (true, Ok("unused"), false, false),
    ] {
        let run = |args: &[&str]| {
            match args {
                ["--user", "disable", "--now", _] => Ok((stop, "stop refused".to_owned())),
                ["--user", "show", ..] => {
                    assert!(!stop, "a completed stop needs no missing-unit exception");
                    // systemctl may exit nonzero for a missing unit: only its complete
                    // structured state, rather than that failure, establishes absence.
                    state
                        .map(|state| (false, state.to_owned()))
                        .map_err(str::to_owned)
                }
                ["--user", "daemon-reload"] => Ok((reload, "reload refused".to_owned())),
                _ => panic!("unexpected systemctl arguments: {args:?}"),
            }
        };
        assert_eq!(
            unload_systemd(&run).is_ok(),
            removed,
            "{stop:?} {state:?} {reload:?}"
        );
    }
    for uncertain in [
        "LoadState=not-found\nActiveState=active\nMainPID=0\n",
        "LoadState=not-found\nActiveState=inactive\nMainPID=77\n",
        "LoadState=not-found\nLoadState=loaded\nActiveState=inactive\nMainPID=0\n",
    ] {
        assert!(!systemd_absent(uncertain), "{uncertain}");
    }
}

/// A FLEET FORMED AFTER A LEAVE GETS THE JOB: `aterm fleet init` keeps the leave's `fleet-off`, and
/// the window serves the fleet it formed, so sync.node installs the job wherever the key stands;
/// with the key gone too (a leave alone), it says the Mac left.
#[test]
fn sync_node_is_due_for_a_fleet_formed_after_a_leave() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let config = tmp.path().join("aterm.toml");
    let prog = tmp.path().join("aterm-bin");
    std::fs::write(&prog, b"stub").unwrap();
    std::fs::write(&config, "[fleet]\nlisten = \"0.0.0.0:7100\"\n").unwrap();
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    std::fs::write(state.join(aterm_net::fleet_member::OFF_FILE), b"").unwrap();
    let sup = FakeSupervisor::new(None);
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::NotApplicable(why) if why.contains("left its fleet")
    ));
    assert!(left_the_fleet(&state));
    {
        use std::io::Write as _;
        let mut f =
            aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE).unwrap();
        f.write_all(b"0123456789abcdef0123456789abcdef01234567\n")
            .unwrap();
    }
    assert!(!left_the_fleet(&state));
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Due { .. }
    ));
}

/// THE JOB IS THE APP'S: the plist names the identifier the app's own `Info.plist` gives, read
/// through a link to its executable, and none for a program in no app.
#[cfg(unix)]
#[test]
fn the_node_plist_names_the_app_its_program_belongs_to() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let macos = tmp.path().join("Probe.app").join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(macos.join("aterm"), b"stub").unwrap();
    std::fs::write(
        macos.parent().unwrap().join("Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key>\n<string>com.example.probe</string>\
         </dict></plist>\n",
    )
    .unwrap();
    let link = tmp.path().join("aterm");
    std::os::unix::fs::symlink(macos.join("aterm"), &link).unwrap();
    assert_eq!(
        LiveSupervisor::bundle_id_for(&link).as_deref(),
        Some("com.example.probe")
    );
    assert_eq!(
        LiveSupervisor::bundle_id_for(&macos.join("aterm")).as_deref(),
        Some("com.example.probe")
    );
    let bare = tmp.path().join("bare-aterm");
    std::fs::write(&bare, b"stub").unwrap();
    assert_eq!(LiveSupervisor::bundle_id_for(&bare), None);
}

/// A launchd stand-in: the domain that holds the job, whether a login holds the screen, and every
/// call it was asked, as `launchctl` words.
struct ScriptedLaunchctl {
    held: RefCell<Option<&'static str>>,
    gui: bool,
    refuses: Option<&'static str>,
    refuses_stop: bool,
    print_error: Option<i32>,
    pid: Option<u32>,
    calls: RefCell<Vec<String>>,
}

impl ScriptedLaunchctl {
    fn new(held: Option<&'static str>, gui: bool) -> Self {
        Self {
            held: RefCell::new(held),
            gui,
            refuses: None,
            refuses_stop: false,
            print_error: None,
            pid: Some(77),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn acts(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|call| !call.starts_with("print"))
            .cloned()
            .collect()
    }

    fn launchd(&self) -> Launchd<'_> {
        Launchd {
            launchctl: self,
            uid: 501,
            settle: Duration::ZERO,
        }
    }
}

impl Launchctl for ScriptedLaunchctl {
    fn run(&self, args: &[&str]) -> Result<(bool, String), String> {
        self.calls.borrow_mut().push(args.join(" "));
        let label = format!("/{NODE_LABEL}");
        let mut held = self.held.borrow_mut();
        let domain_of = |target: &str| target.strip_suffix(&label).map(str::to_owned);
        Ok(match args {
            ["print", "gui/501"] => (self.gui, String::new()),
            ["print", "user/501"] => (true, String::new()),
            ["print", target] => match domain_of(target) {
                Some(domain) if held.is_some_and(|at| at == domain) => {
                    let text = self.pid.map_or_else(
                        || "\tstate = waiting\n".to_owned(),
                        |pid| format!("\tstate = running\n\tpid = {pid}\n"),
                    );
                    (true, text)
                }
                _ => (false, String::new()),
            },
            ["bootout", _] if self.refuses_stop => (false, "unload refused".to_owned()),
            ["bootout", target] => {
                let ours =
                    domain_of(target).is_some_and(|domain| held.is_some_and(|at| at == domain));
                if ours {
                    *held = None;
                }
                (ours, String::new())
            }
            ["bootstrap", domain, _plist] if self.refuses == Some(*domain) => (
                false,
                "Bootstrap failed: 5: Input/output error\n".to_owned(),
            ),
            ["bootstrap", domain, _plist] => {
                *held = Some(if *domain == "gui/501" {
                    "gui/501"
                } else {
                    "user/501"
                });
                (true, String::new())
            }
            ["kickstart", "-k", target] => (
                domain_of(target).is_some_and(|domain| held.is_some_and(|at| at == domain)),
                String::new(),
            ),
            other => panic!("an unexpected launchctl call: {other:?}"),
        })
    }

    fn print_service(&self, target: &str) -> Result<Option<String>, String> {
        let (ok, said) = self.run(&["print", target])?;
        let code = self.print_error.unwrap_or_else(|| {
            if ok {
                0
            } else if target.starts_with("gui/") && !self.gui {
                112
            } else {
                113
            }
        });
        printed_service(target, Some(code), said)
    }
}

/// A JOB AN SSH-ONLY PASS PUT IN THE BACKGROUND MOVES TO THE LOGIN AT THE SCREEN: it reads
/// misplaced once a login holds the screen, and a load lets it go, waits for launchd to, and then
/// loads it in `gui/<uid>`; with nobody at the screen it stays.
#[test]
fn a_background_job_moves_to_the_login_at_the_screen() {
    let alone = ScriptedLaunchctl::new(Some("user/501"), false);
    assert_eq!(alone.launchd().misplaced(), None);
    let mut seen = ScriptedLaunchctl::new(Some("user/501"), true);
    let why = seen.launchd().misplaced().expect("misplaced");
    assert!(why.contains("user/501") && why.contains("gui/501"), "{why}");
    seen.launchd()
        .load(Path::new("/x/systems.alab.aterm-node.plist"))
        .unwrap();
    assert_eq!(
        seen.acts(),
        [
            format!("bootout user/501/{NODE_LABEL}"),
            "bootstrap gui/501 /x/systems.alab.aterm-node.plist".to_owned()
        ]
    );
    let calls = seen.calls.borrow().clone();
    let bootout = calls.iter().position(|c| c.starts_with("bootout")).unwrap();
    let settled = calls
        .iter()
        .rposition(|c| c == &format!("print user/501/{NODE_LABEL}"))
        .unwrap();
    let bootstrap = calls
        .iter()
        .position(|c| c.starts_with("bootstrap"))
        .unwrap();
    assert!(bootout < settled && settled < bootstrap, "{calls:?}");
    assert_eq!(
        seen.launchd().misplaced(),
        None,
        "it stands where it belongs"
    );
    seen.refuses_stop = true;
    assert!(seen.launchd().unload().is_err());
    assert_eq!(*seen.held.borrow(), Some("gui/501"));
    seen.refuses_stop = false;
    seen.launchd().unload().unwrap();
    assert_eq!(*seen.held.borrow(), None);
    seen.launchd().unload().unwrap();

    let mut unknown = ScriptedLaunchctl::new(Some("user/501"), true);
    unknown.print_error = Some(1);
    assert!(unknown.launchd().unload().is_err());
    assert_eq!(*unknown.held.borrow(), Some("user/501"));

    let mut idle = ScriptedLaunchctl::new(Some("user/501"), false);
    idle.pid = None;
    idle.refuses_stop = true;
    assert!(idle.launchd().unload().is_err());
    assert_eq!(*idle.held.borrow(), Some("user/501"));
    idle.refuses_stop = false;
    idle.launchd().unload().unwrap();
    assert_eq!(*idle.held.borrow(), None);
    idle.launchd().unload().unwrap();
}

/// A MOVE THE LOGIN'S SESSION REFUSES LEAVES THE JOB RUNNING: the job goes back to the background
/// session it ran in, and the pass still reports the refusal.
#[test]
fn a_refused_move_keeps_the_job_in_the_background() {
    let mut launchctl = ScriptedLaunchctl::new(Some("user/501"), true);
    launchctl.refuses = Some("gui/501");
    let err = launchctl
        .launchd()
        .load(Path::new("/x/systems.alab.aterm-node.plist"))
        .unwrap_err();
    assert!(
        err.contains("Bootstrap failed") && err.contains("kept it in user/501"),
        "{err}"
    );
    assert_eq!(*launchctl.held.borrow(), Some("user/501"), "still loaded");
    assert_eq!(launchctl.launchd().running_pid().unwrap(), Some(77));
}

/// RENEW ON A REPLACED BUILD: `kickstart -k` on the domain that holds the job, and nothing else.
#[test]
fn a_renewal_is_kickstart_k_alone() {
    let launchctl = ScriptedLaunchctl::new(Some("gui/501"), true);
    launchctl.launchd().renew().unwrap();
    assert_eq!(
        launchctl.acts(),
        [format!("kickstart -k gui/501/{NODE_LABEL}")]
    );
    let none = ScriptedLaunchctl::new(None, true);
    assert!(none.launchd().renew().is_err());
    assert!(none.acts().is_empty());
}

/// sync.node renews a job that runs a replaced build in place, and reloads one that stands in the
/// wrong domain.
#[test]
fn sync_node_renews_a_replaced_build_and_moves_a_misplaced_job() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let config = tmp.path().join("aterm.toml");
    let prog = tmp.path().join("aterm-bin");
    std::fs::write(&prog, b"stub").unwrap();
    std::fs::write(&config, "[fleet]\nlisten = \"0.0.0.0:7100\"\n").unwrap();
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    {
        use std::io::Write as _;
        let mut f =
            aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE).unwrap();
        f.write_all(b"0123456789abcdef0123456789abcdef01234567\n")
            .unwrap();
    }
    let sup = FakeSupervisor::new(None);
    ensure_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap();
    assert_eq!(*sup.loads.borrow(), 1);
    sup.replaced.set(true);
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Due { from, .. } if from.contains("replaced")
    ));
    let said =
        ensure_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap();
    assert!(said.contains("kickstart -k"), "{said}");
    assert_eq!((*sup.renews.borrow(), *sup.loads.borrow()), (1, 1));
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Current(_)
    ));
    *sup.misplaced.borrow_mut() = Some("loaded in user/501".to_owned());
    ensure_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap();
    assert_eq!((*sup.renews.borrow(), *sup.loads.borrow()), (1, 2));
}

/// A PLIST AN EARLIER BUILD WROTE IS REWRITTEN: a running job whose unit is not the one this build
/// writes (here, the identifier no app has) is reloaded from a fresh unit, even on a replaced build.
#[test]
fn sync_node_rewrites_a_unit_an_earlier_build_wrote() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let config = tmp.path().join("aterm.toml");
    let prog = tmp.path().join("aterm-bin");
    std::fs::write(&prog, b"stub").unwrap();
    std::fs::write(&config, "[fleet]\nlisten = \"0.0.0.0:7100\"\n").unwrap();
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    {
        use std::io::Write as _;
        let mut f =
            aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE).unwrap();
        f.write_all(b"0123456789abcdef0123456789abcdef01234567\n")
            .unwrap();
    }
    let sup = FakeSupervisor::new(None);
    ensure_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap();
    let unit = plist_path(&home);
    let earlier = node_plist_text(&prog, &home, &state, Some("systems.alab.aterm"));
    std::fs::write(&unit, &earlier).unwrap();
    sup.replaced.set(true);
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Due { from, .. } if from.contains("not the unit this build writes")
    ));
    ensure_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap();
    assert_eq!((*sup.renews.borrow(), *sup.loads.borrow()), (0, 2));
    assert_eq!(
        std::fs::read_to_string(&unit).unwrap(),
        sup.render_unit(&prog, &home, &state)
    );
    sup.replaced.set(false);
    assert!(matches!(
        read_for_sync_with(&home, &state, Some(&config), Some(&prog), false, &sup).unwrap(),
        SyncNodeState::Current(_)
    ));
}

/// A newer helper's fallback path is not evidence that the fallback executable is newer.
#[test]
fn sync_node_measures_both_programs_before_moving_the_service() {
    for (offered, moves) in [
        (Some((0, 91, 0)), false),
        (None, false),
        (Some((0, 147, 0)), true),
    ] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let state = tmp.path().join("state");
        let current = tmp.path().join("active-app");
        let fallback = tmp.path().join("Applications-app");
        std::fs::write(&current, b"current").unwrap();
        std::fs::write(&fallback, b"fallback").unwrap();
        aterm_uds::private_file::ensure_private_dir(&state).unwrap();
        use std::io::Write as _;
        aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE)
            .unwrap()
            .write_all(b"0123456789abcdef0123456789abcdef01234567\n")
            .unwrap();
        let sup = FakeSupervisor::new(None);
        install_with(&current, &home, &state, false, &sup).unwrap();
        sup.releases
            .borrow_mut()
            .insert(current.clone(), Some((0, 146, 0)));
        sup.releases.borrow_mut().insert(fallback.clone(), offered);

        let read = read_for_sync_with(&home, &state, None, Some(&fallback), false, &sup).unwrap();
        assert_eq!(
            matches!(read, SyncNodeState::Due { .. }),
            moves,
            "{offered:?}: {read:?}"
        );
        ensure_for_sync_with(&home, &state, None, Some(&fallback), false, &sup).unwrap();
        let expected = if moves { &fallback } else { &current };
        assert!(matches!(
            state_with(&home, &sup).unwrap(),
            NodeJobState::Installed { program: Some(kept), .. } if &kept == expected
        ));
        assert_eq!(*sup.loads.borrow(), if moves { 2 } else { 1 });

        if !moves {
            // Preserving the active path must still repair its replaced image without
            // restarting the obsolete fallback or declaring an unrepaired job current.
            sup.replaced.set(true);
            ensure_for_sync_with(&home, &state, None, Some(&fallback), false, &sup).unwrap();
            assert_eq!((*sup.renews.borrow(), *sup.loads.borrow()), (1, 1));
        }
    }
}

#[test]
fn sync_node_gates_off_and_missing_membership_before_program_probes() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let current = tmp.path().join("active-app");
    let fallback = tmp.path().join("Applications-app");
    let sup = FakeSupervisor::new(None);
    install_with(&current, &home, &state, false, &sup).unwrap();
    assert!(matches!(
        read_for_sync_with(&home, &state, None, Some(&fallback), false, &sup).unwrap(),
        SyncNodeState::NotApplicable(_)
    ));
    std::fs::write(off_path(&state), b"off\n").unwrap();
    assert!(matches!(
        read_for_sync_with(&home, &state, None, Some(&fallback), false, &sup).unwrap(),
        SyncNodeState::NotApplicable(_)
    ));
    assert!(sup.release_probes.borrow().is_empty());
    assert_eq!(*sup.loads.borrow(), 1);
}

#[test]
fn sync_node_keeps_an_unknown_running_program_but_repairs_a_missing_one() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let current = tmp.path().join("active-app");
    let proposed = tmp.path().join("Applications-app");
    std::fs::write(&current, b"current").unwrap();
    let state = NodeJobState::Installed {
        pid: Some(123),
        unit: tmp.path().join("node.plist"),
        program: Some(current.clone()),
    };
    let sup = FakeSupervisor::new(Some(123));
    sup.releases.borrow_mut().insert(current.clone(), None);
    assert!(
        sync_program(&proposed, &state, &sup)
            .unwrap_err()
            .contains("release is unknown")
    );
    std::fs::remove_file(&current).unwrap();
    assert_eq!(sync_program(&proposed, &state, &sup).unwrap(), proposed);
}

#[test]
fn off_during_version_probe_is_rechecked_before_applying_a_node_upgrade() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let current = tmp.path().join("active-app");
    let proposed = tmp.path().join("new-app");
    std::fs::write(&current, b"current").unwrap();
    let sup = FakeSupervisor::new(None);
    install_with(&current, &home, &state, false, &sup).unwrap();
    use std::io::Write as _;
    aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE)
        .unwrap()
        .write_all(b"0123456789abcdef0123456789abcdef01234567\n")
        .unwrap();
    sup.releases.borrow_mut().insert(current, Some((0, 146, 0)));
    *sup.off_on_probe.borrow_mut() = Some((home.clone(), state.clone()));
    let said = ensure_for_sync_with(&home, &state, None, Some(&proposed), false, &sup).unwrap();
    assert!(said.contains("off on this Mac"), "{said}");
    assert_eq!((*sup.loads.borrow(), *sup.unloads.borrow()), (1, 1));
    assert!(node_turned_off(&state));
    assert_eq!(state_with(&home, &sup).unwrap(), NodeJobState::Absent);
}

#[cfg(unix)]
#[test]
fn node_off_serializes_with_an_in_progress_service_change() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    let unit = FileOnlySupervisor.unit_file(&home);
    install_with(
        &tmp.path().join("aterm"),
        &home,
        &state,
        false,
        &FileOnlySupervisor,
    )
    .unwrap();
    let guard = node_admin_lock(&state).unwrap();
    let (wait_tx, wait_rx) = std::sync::mpsc::channel();
    let other_home = home.clone();
    let other_state = state.clone();
    let worker = std::thread::spawn(move || {
        NODE_ADMIN_WAITER.with(|waiter| *waiter.borrow_mut() = Some(wait_tx));
        remove_with(&other_home, &other_state, true, &FileOnlySupervisor)
    });
    wait_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(!node_turned_off(&state));
    assert!(unit.is_file());
    drop(guard);
    assert!(worker.join().unwrap().unwrap().is_some());
    assert!(node_turned_off(&state));
    assert!(!unit.exists());
}

#[test]
fn fresh_node_requires_a_verified_compatible_destination_before_mutation() {
    for has_unit in [false, true] {
        for release in [
            None,
            Some((0, 91, 0)),
            Some(required_node_release().unwrap()),
        ] {
            let tmp = aterm_tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            let state = tmp.path().join("state");
            let proposed = tmp.path().join("Applications-app");
            aterm_uds::private_file::ensure_private_dir(&state).unwrap();
            use std::io::Write as _;
            aterm_uds::private_file::open_private_rw(&state, aterm_net::fleet::KEY_FILE)
                .unwrap()
                .write_all(b"0123456789abcdef0123456789abcdef01234567\n")
                .unwrap();
            let sup = FakeSupervisor::new(None);
            sup.releases.borrow_mut().insert(proposed.clone(), release);
            let unit = sup.unit_file(&home);
            if has_unit {
                std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
                std::fs::write(&unit, "unreadable program\n").unwrap();
            }
            let before = std::fs::read(&unit).ok();
            let result = ensure_for_sync_with(&home, &state, None, Some(&proposed), false, &sup);
            if release == Some(required_node_release().unwrap()) {
                assert!(result.is_ok(), "{has_unit} {release:?}: {result:?}");
                assert_eq!(*sup.loads.borrow(), 1);
            } else {
                assert!(result.is_err(), "{has_unit} {release:?}: {result:?}");
                assert_eq!(*sup.loads.borrow(), 0);
                assert_eq!(std::fs::read(&unit).ok(), before);
            }
        }
    }
}

#[test]
fn an_unknown_running_node_path_cannot_be_replaced_as_a_fresh_install() {
    let sup = FakeSupervisor::new(Some(123));
    let state = NodeJobState::Installed {
        pid: Some(123),
        unit: PathBuf::from("node.plist"),
        program: None,
    };
    assert!(
        sync_program(Path::new("candidate"), &state, &sup)
            .unwrap_err()
            .contains("running node program is unknown")
    );
    assert!(sup.release_probes.borrow().is_empty());
    assert_eq!(*sup.loads.borrow(), 0);
}
