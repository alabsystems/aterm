// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-rotation complete` served whole, from the lane's entry for a proven claim
//! ([`serve_claimed`](super::super::super::serve_claimed)) to the pool's proof: a real shell and a
//! process under it stand for the consumed receipt's bound successor (so the kernel's pid and birth
//! are the kernel's), a pool of synthetic logins under a directory of the test's own stands for the
//! accounts, and the pool's records stand as a usage-wall move's commit and consume left them:
//! `work` the pool's active row, unproven, its canary owned by the receipt, whose pool record
//! consume retired. Nothing here reads a real login or reaches a provider.

use super::*;
use aterm_agent::harness::codex_app_login;

const THREAD: &str = "019a4b54-5fa1-7123-8123-123456789abc";
const WALL: &str = "019a4b54-5fa2-7123-8123-123456789abc";
const OTHER: &str = "019a4b54-5fa3-7123-8123-123456789abc";
const NOW: i64 = 1_792_700_000;
/// How long a test waits for its agent to run: far longer than a fork and an exec take.
const PATIENCE: Duration = Duration::from_secs(60);

/// A shell and the process it runs, as the kernel shows a codex-alab under the tab's shell.
struct Agent {
    shell: std::process::Child,
    pid: u32,
}

impl Agent {
    fn spawn() -> Self {
        use std::io::BufRead;
        let shell = ["/bin/zsh", "/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .expect("a zsh or a bash, the shells a relaunch is typed in");
        let mut shell = std::process::Command::new(shell)
            .args(["-f", "-c", "sleep 600 & echo $!; wait"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the shell");
        let mut line = String::new();
        std::io::BufReader::new(shell.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid: u32 = line.trim().parse().expect("the agent's pid");
        let agent = Self { shell, pid };
        // The pid is printed at the fork: wait until the kernel shows the image it then runs.
        let since = Instant::now();
        while !Process::observe(pid).is_some_and(|process| process.image.path.ends_with(b"sleep")) {
            assert!(since.elapsed() < PATIENCE, "hang: the agent never ran");
            aterm_uds::deadline::poll_yield();
        }
        agent
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        // SAFETY: the pid is this test's own child's child, still a child of the shell it holds.
        unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }
}

/// A Codex login in `dir` of `account`, naming `email`: unsigned, as a reader decodes it.
fn lay_login(dir: &Path, account: &str, email: &str) {
    let claims = aterm_json::json!({"email": email, "https://api.openai.com/auth": {
        "chatgpt_account_id": account, "chatgpt_user_id": "user"}});
    let encoded = aterm_codec::base64::encode(claims.to_string().as_bytes())
        .unwrap()
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_owned();
    let token = format!("e30.{encoded}.fixture");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(codex_app_login::AUTH_FILE),
        aterm_json::json!({"auth_mode": "chatgpt", "tokens": {"id_token": token,
            "access_token": token, "refresh_token": format!("{account}-refresh")}})
        .to_string(),
    )
    .unwrap();
}

/// The sealed native restart an interactive commit runs, under `id`: what makes a receipt this
/// lane's rather than an exec handoff's.
fn sealed(id: &str) -> String {
    let image = Image {
        path: b"/fixture-image".to_vec(),
        device: 1,
        inode: 2,
    };
    let process = Process {
        pid: 4242,
        birth: 12,
        parent: 42,
        group: 4242,
        session: 42,
        image: image.clone(),
    };
    let command = RestartCommand {
        settles: None,
        account_intent: None,
        image: image.clone(),
        program: crate::test_host::abs("/fixture-image"),
        argv: vec!["resume".into()],
        env: vec![("CODEX_AUTH_PATH".into(), "/new/auth.json".into())],
        unset: None,
        cwd: None,
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
    };
    let prepare = Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: id.into(),
        host_epoch: host_epoch().unwrap().into(),
        holder: crate::harness_host::holder(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: u64::MAX,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            cold: None,
            process,
            shell: Process {
                pid: 42,
                birth: 10,
                parent: 1,
                group: 42,
                session: 42,
                image: image.clone(),
            },
            observation: None,
            launcher: image,
            socket: crate::test_host::abs("/fixture.sock"),
            forward: Box::new(command),
            rollback: None,
        },
        boot: None,
        custody: None,
    };
    aterm_agent::harness::native_act::Request::prepare(prepare)
        .encode()
        .unwrap()
}

/// One tab whose agent is the bound successor of a consumed move off `main`, walled, onto `work`:
/// its claimed process, its record, the receipt on record and the pool's records.
struct World {
    env: Env,
    ctx: Arc<crate::SessionCtx>,
    proof: crate::self_claim::Proven,
    /// The claimed process's resume record: the destination's login file and the moved thread.
    copy: self_claimed::Copy,
    receipt: Receipt,
    /// `main`'s and `work`'s login files.
    source: PathBuf,
    target: PathBuf,
    /// The pool's records as the move's commit and consume left them ([`World::records`]).
    committed: (String, String),
    // Dropped last, in this order: the tab, then its agent and the files.
    _handle: crate::session_store::SessionHandle,
    _agent: Agent,
    _tmp: aterm_tempfile::TempDir,
}

impl World {
    /// The world that serves a report.
    fn new(local: u64) -> Self {
        Self::with(local, true, |_, _| {})
    }

    /// A world whose host hands a session off at a usage limit where `quota_rotation` says, and
    /// whose receipt `edit` makes of the consumed one before it is written, given the agent's pid
    /// and birth.
    fn with(local: u64, quota_rotation: bool, edit: impl FnOnce(&mut Receipt, (u32, u64))) -> Self {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let mut env = Env::from_process().unwrap();
        env.home = Some(root.join("home"));
        env.state = root.join("state");
        env.config = Some(root.join("config/aterm.toml"));
        env.now = NOW;
        std::fs::create_dir_all(root.join("config")).unwrap();
        crate::test_scratch::write_config(
            env.config.as_ref().unwrap(),
            "[harness]\naccounts='early'\nrelaunch=true\nresume_limits=true\nhuman_grace_s=0\n",
        );
        let home = env.home.clone().unwrap();
        let default_home = home.join(accounts::Cli::Codex.accounts().default_home);
        let work = root.join("logins/codex/work");
        std::fs::write(
            env.accounts_roster_for(accounts::Cli::Codex).unwrap(),
            format!(
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\
                 email = \"main@example.test\"\n\n\
                 [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\n\
                 email = \"work@example.test\"\ndir = {}\n",
                crate::test_host::toml(&work)
            ),
        )
        .unwrap();
        lay_login(&default_home, "source", "main@example.test");
        lay_login(&work, "target", "work@example.test");
        let source = default_home.join(codex_app_login::AUTH_FILE);
        let target = work.join(codex_app_login::AUTH_FILE);

        let mut handle = crate::session_store::test_handle_on(local, -1);
        let grok = aterm_agent::harness::contract::row_named("grok-alab").expect("rostered");
        let fixture = crate::native_session_policy::FixturePolicy::new(
            std::slice::from_ref(grok),
            vec![crate::native_session_policy::FixtureImages {
                row: "grok-alab",
                images: vec![Image {
                    path: b"/fixture".to_vec(),
                    device: 1,
                    inode: 1,
                }],
            }],
            aterm_agent::supervise::SupervisorConfig {
                accounts: accounts::Mode::Early,
                ..Default::default()
            },
            &[Constructor::PrivateRestart],
        )
        .unwrap();
        let fixture = if quota_rotation {
            fixture.with_quota_rotation()
        } else {
            fixture
        };
        assert!(
            Arc::get_mut(&mut handle.ctx)
                .unwrap()
                .install_fixture_policy(fixture)
                .is_ok()
        );
        let ctx = Arc::clone(&handle.ctx);

        let agent = Agent::spawn();
        let birth = Process::observe(agent.pid).expect("the agent").birth;
        let proof = crate::self_claim::Proven {
            pid: agent.pid,
            start: aterm_agent::harness::restart_stamp::kernel_start(agent.pid)
                .expect("the agent's kernel start"),
        };
        let copy = self_claimed::Copy {
            lifetime: None,
            pid: proof.pid,
            start: proof.start.clone(),
            home: default_home.clone(),
            seen: self_claimed::Seen::Removed,
            resume: Ok(self_claimed::Resume {
                argv: vec![
                    "/opt/codex/bin/codex".into(),
                    "resume".into(),
                    THREAD.into(),
                ],
                session: THREAD.into(),
                codex_home: None,
                codex_auth_path: Some(target.to_string_lossy().into_owned()),
                cwd: None,
            }),
        };
        // The resumed agent's own record, as consume left it: its owed act bound to the receipt.
        let identity = renewal::Identity::from_native("source", "user");
        let records = default_home.join("alab/sessions");
        std::fs::create_dir_all(&records).unwrap();
        std::fs::write(
            records.join(format!("{}.json", proof.pid)),
            aterm_json::json!({"sessionId": THREAD, "status": "idle", "draftEmpty": true,
                "autopilot": "active", "owed": {"thread": THREAD, "mode": "land",
                "nativeOrigin": {"turnId": WALL, "phase": "complete"}, "rule": "usage-rotate@v1",
                "rotation": {"account": identity.account, "user": identity.user,
                    "openaiScope": true, "starts": 1, "accepted": true}}})
            .to_string(),
        )
        .unwrap();

        let seat = accounts_pool::Seat {
            config_dir: &root.join("config"),
            state: &env.state,
            home: &home,
            now: NOW,
        };
        let records = accounts_pool::move_records(&seat, accounts::Cli::Codex).unwrap();
        let id = "ab".repeat(16);
        let mut receipt = Receipt {
            destination: Some(renewal::Identity::from_native("target", "user")),
            id: id.clone(),
            intent: Intent {
                purpose: renewal::Purpose::WeeklyQuota,
                thread: THREAD.into(),
                wall: WALL.into(),
                identity,
                limit: None,
            },
            sid: ctx.self_id.as_str().into(),
            // The source: a process of the walled turn, ended by the move.
            old_pid: 0x3fff_ff31,
            old_birth: 1,
            claim_revision: 1,
            human_seq: 0,
            old_auth: source.clone(),
            new_auth: target.clone(),
            generation: 0,
            row_stamp: renewal::row_stamp(records.roster.row("work").unwrap()),
            native_prepare: sealed(&id),
            expires_ms: u64::MAX,
            phase: Phase::Consumed,
            successor_pid: Some(proof.pid),
            successor_birth: Some(birth),
            accepted_turn: Some(WALL.into()),
            launch_attempted: true,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        edit(&mut receipt, (proof.pid, birth));
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
            .unwrap()
            .unwrap();
        let store = seat.store(accounts::Cli::Codex);
        store
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
                // The move's commit: `work` active and unproven, its canary this receipt's.
                state.active = Some("work".into());
                state.proven = false;
                state.rotations = vec![NOW];
                state.canary = Some(accounts::Canary {
                    sid: ctx.self_id.as_str().into(),
                    at: NOW,
                    took: false,
                    native_receipt: Some(id.clone()),
                });
                Ok(accounts::note_sample(
                    state,
                    "work",
                    accounts::Sample {
                        at: NOW,
                        session_pct: Some(10),
                        session_reset: Some(NOW + 3600),
                        week_pct: Some(10),
                        week_reset: Some(NOW + 86_400),
                        ..accounts::Sample::default()
                    },
                    NOW,
                ))
            })
            .unwrap();
        let mut world = Self {
            env,
            ctx,
            proof,
            copy,
            receipt,
            source,
            target,
            committed: (String::new(), String::new()),
            _handle: handle,
            _agent: agent,
            _tmp: tmp,
        };
        world.committed = world.records();
        world
    }

    /// The pool's state and its logins' records, as each serializes on disk.
    fn records(&self) -> (String, String) {
        let store = self.store();
        (
            store.load(NOW).unwrap().0.to_json(),
            store.load_logins(NOW).unwrap().0.to_json(),
        )
    }

    /// `line`, asked of the lane as the claimed agent with resume record `copy` asks it.
    fn ask_as(&self, copy: &self_claimed::Copy, line: &str) -> String {
        let request = renewal::Request::parse_rotation(line).expect("a well-formed request");
        let host: Arc<dyn super::super::super::login_rotation::Host> =
            Arc::new(super::super::super::login_rotation::System);
        super::super::super::renamed(
            super::super::super::serve_claimed(
                &self.ctx,
                (self.env.clone(), self.proof.clone(), copy.clone()),
                request,
                renewal::Purpose::WeeklyQuota,
                &host,
            ),
            "codex-rotation",
        )
    }

    /// The report of the receipt's id for `turn`.
    fn report(&self, turn: &str) -> String {
        self.ask_as(
            &self.copy,
            &format!("complete id={} turn={turn}", self.receipt.id),
        )
    }

    fn complete(&self) -> String {
        self.report(WALL)
    }

    fn store(&self) -> accounts::Store {
        self.env.accounts_store_for(accounts::Cli::Codex).unwrap()
    }

    fn state(&self) -> accounts::State {
        self.store().load(NOW).unwrap().0
    }

    /// What `main`, at its weekly wall, would move to as the records stand: `None` while the
    /// canary holds every session walled behind it.
    fn follower_target(&self) -> Option<String> {
        let records = accounts_pool::move_records(
            &accounts_pool::Seat {
                config_dir: self.env.config.as_deref().unwrap().parent().unwrap(),
                state: &self.env.state,
                home: self.env.home.as_deref().unwrap(),
                now: NOW,
            },
            accounts::Cli::Codex,
        )
        .unwrap();
        let home = self.env.home.as_deref().unwrap();
        accounts_pool::native_quota_target(
            (accounts::Cli::Codex, accounts::Mode::Early),
            (
                &records.roster,
                &records.state,
                &accounts::enrolled_in(accounts::Cli::Codex, &records.roster, home),
            ),
            records.roster.row("main").unwrap(),
            (
                accounts::default_login_address(accounts::Cli::Codex, home).as_deref(),
                home,
            ),
            NOW,
        )
    }

    /// The pool as the move left it, unproven behind the receipt's canary, with the receipt on
    /// record unchanged: what a refused report leaves.
    fn recorded_nothing(&self) {
        assert_eq!(self.records(), self.committed, "the pool's records");
        let state = self.state();
        assert!(!state.proven, "the pool stays unproven");
        assert_eq!(
            state
                .canary
                .as_ref()
                .and_then(|c| c.native_receipt.as_deref()),
            Some(self.receipt.id.as_str()),
            "the canary stands"
        );
        assert!(state.canary_holds(NOW));
        assert_eq!(self.follower_target(), None, "the follower is still held");
        assert_eq!(
            renewal::read_receipt(&self.env, &self.receipt.id).as_ref(),
            Some(&self.receipt)
        );
    }
}

/// THE BOUND SUCCESSOR'S REPORT PROVES THE DESTINATION AND RELEASES ITS FOLLOWERS AT ONCE: before
/// it, a session walled on `main` is held behind `work`'s canary (its forecast names no row). The
/// claimed process that is the consumed receipt's bound successor, on `work`'s login file in the
/// moved thread, reports its walled turn served, and is answered `state=completed` with the
/// receipt's own eight fields. The pool then holds `work` proven with no canary, no second
/// rotation charged, and the walled session's forecast (what its next ask moves it to) names
/// `work`. Reporting again answers the same and changes nothing.
#[test]
fn the_successors_report_proves_the_destination_and_releases_its_followers() {
    let world = World::new(9201);
    assert_eq!(world.follower_target(), None, "held behind the canary");
    let reply = world.complete();
    let target = renewal::Identity::from_native("target", "user");
    assert_eq!(
        reply,
        format!(
            "OK codex-rotation state=completed id={} thread={THREAD} turn={WALL} \
             source_account={} source_user={} target_account={} target_user={}\n",
            world.receipt.id,
            world.receipt.intent.identity.account,
            world.receipt.intent.identity.user,
            target.account,
            target.user,
        )
    );
    let state = world.state();
    assert!(state.proven);
    assert!(state.canary.is_none());
    assert_eq!(state.active.as_deref(), Some("work"));
    assert_eq!(state.rotations, [NOW]);
    assert_eq!(world.follower_target().as_deref(), Some("work"));
    assert_eq!(
        renewal::read_receipt(&world.env, &world.receipt.id).as_ref(),
        Some(&world.receipt),
        "the receipt stays consumed"
    );
    assert_eq!(world.complete(), reply, "a repeated report");
    assert_eq!(world.state().to_json(), state.to_json());
}

/// ONLY THE BOUND SUCCESSOR REPORTS, AND ONLY AS IT RESUMED: the source process of the move, a
/// process with the successor's pid but another kernel birth (the pid reused since), a process on
/// the source's login file, and one resumed in another thread are each `wrong process`, and
/// nothing is recorded.
#[test]
fn only_the_bound_successor_on_the_destination_in_the_moved_thread_reports() {
    // The claimed process is the move's source, and another is its successor.
    let world = World::with(9202, true, |receipt, (pid, birth)| {
        receipt.old_pid = pid;
        receipt.old_birth = birth;
        receipt.successor_pid = Some(1);
        receipt.successor_birth = Some(1);
    });
    assert_eq!(world.complete(), "ERR codex-rotation wrong process\n");
    world.recorded_nothing();

    // The bound successor was born before the claimed process, which took its pid after it
    // ended: in the same tab, on the destination's login, in the moved thread.
    let world = World::with(9214, true, |receipt, (pid, birth)| {
        receipt.successor_pid = Some(pid);
        receipt.successor_birth = Some(birth - 1);
    });
    assert_eq!(world.complete(), "ERR codex-rotation wrong process\n");
    world.recorded_nothing();

    let world = World::new(9203);
    let line = format!("complete id={} turn={WALL}", world.receipt.id);
    let resumed = |edit: &dyn Fn(&mut self_claimed::Resume)| {
        let mut copy = world.copy.clone();
        edit(copy.resume.as_mut().unwrap());
        copy
    };
    for copy in [
        resumed(&|resume| {
            resume.codex_auth_path = Some(world.source.to_string_lossy().into_owned());
        }),
        resumed(&|resume| resume.codex_auth_path = None),
        resumed(&|resume| resume.session = OTHER.into()),
    ] {
        assert_eq!(
            world.ask_as(&copy, &line),
            "ERR codex-rotation wrong process\n",
            "{copy:?}"
        );
        world.recorded_nothing();
    }
    assert!(
        world
            .complete()
            .starts_with("OK codex-rotation state=completed "),
        "CONTROL"
    );
}

/// A REPORT IS OF THIS SESSION'S RECEIPT, CONSUMED BY THE WALLED TURN: an id on no record is
/// `unknown receipt`, another session's receipt `wrong session`, and a receipt not yet consumed (or
/// a report for another turn than the one that consumed it) `receipt changed`. Nothing is recorded.
#[test]
fn a_report_is_of_this_sessions_receipt_consumed_by_the_walled_turn() {
    let world = World::new(9204);
    assert_eq!(
        world.ask_as(
            &world.copy,
            &format!("complete id={} turn={WALL}", "cd".repeat(16))
        ),
        "ERR codex-rotation unknown receipt\n"
    );
    assert_eq!(world.report(OTHER), "ERR codex-rotation receipt changed\n");
    world.recorded_nothing();

    let world = World::with(9205, true, |receipt, _| receipt.sid = "another-tab".into());
    assert_eq!(world.complete(), "ERR codex-rotation wrong session\n");
    world.recorded_nothing();

    let world = World::with(9206, true, |receipt, _| {
        receipt.phase = Phase::Committed;
        receipt.accepted_turn = None;
    });
    assert_eq!(world.complete(), "ERR codex-rotation receipt changed\n");
    world.recorded_nothing();
}

/// A REPORT PROVES ONLY THIS LANE'S POOL MOVE: a lost login's receipt moves the session alone and
/// has no canary to prove, and an exec handoff's (its sealed data an exec operation's, not a
/// native restart's) reports through its own operation under that operation's gates. Each is
/// `wrong recovery purpose`, and nothing is recorded.
#[test]
fn a_report_proves_only_this_lanes_pool_move() {
    let world = World::with(9207, true, |receipt, _| {
        receipt.intent.purpose = renewal::Purpose::LoginRotation;
    });
    assert_eq!(
        world.complete(),
        "ERR codex-rotation wrong recovery purpose\n"
    );
    world.recorded_nothing();

    let world = World::with(9208, true, |receipt, _| {
        receipt.native_prepare = aterm_json::json!({"kind": "codex-exec", "host": "ab".repeat(16),
            "command": "cd".repeat(32), "checkpoint": "ef".repeat(32), "generation": 1})
        .to_string();
    });
    assert_eq!(
        world.complete(),
        "ERR codex-rotation wrong recovery purpose\n"
    );
    world.recorded_nothing();
}

/// THE POOL'S RECORDS ARE JUDGED AS A COMPLETION'S WRITER JUDGES THEM: a destination login that
/// names another account now, or a source login re-signed to another account since the wall (the
/// custody lock holds both to their receipt's identities), is `selection changed`, and the pool
/// stays unproven. CONTROL: with the logins as the receipt names them, the same report completes.
#[test]
fn a_login_changed_since_the_wall_refuses_the_report() {
    let world = World::new(9209);
    lay_login(
        world.target.parent().unwrap(),
        "another",
        "work@example.test",
    );
    assert_eq!(world.complete(), "ERR codex-rotation selection changed\n");
    world.recorded_nothing();
    lay_login(
        world.target.parent().unwrap(),
        "target",
        "work@example.test",
    );
    lay_login(
        world.source.parent().unwrap(),
        "re-signed",
        "main@example.test",
    );
    assert_eq!(world.complete(), "ERR codex-rotation selection changed\n");
    world.recorded_nothing();
    lay_login(
        world.source.parent().unwrap(),
        "source",
        "main@example.test",
    );
    assert!(
        world
            .complete()
            .starts_with("OK codex-rotation state=completed "),
        "CONTROL"
    );
}

/// A SOURCE ROW HELD SINCE THE WALL REFUSES THE REPORT: the source row taken into another Mac's
/// custody, on the roster (`held_by`, as a sync from the lab's pool records it) or only in the
/// lab's pool document so far (the custody lock's `source-held`), is no longer the move the
/// receipt sealed. Either is `selection changed`, and the pool stays unproven. CONTROL: with the
/// hold gone the same report completes. The scratch config names this Mac (`m16`): in this
/// crate's tests aterm-agent is built without `cfg(test)`, so a Mac its config does not name is
/// named by its host names, and on the fleet Mac `m2` the pool's warm Mac would be this one.
#[cfg(unix)] // seeds its pool entry through `vault_entry::put`, a unix-only seam
#[test]
fn a_source_held_since_the_wall_refuses_the_report() {
    for (local, in_pool) in [(9215, false), (9216, true)] {
        let world = World::new(local);
        let roster = world.env.accounts_roster_for(accounts::Cli::Codex).unwrap();
        let text = std::fs::read_to_string(&roster).unwrap();
        let home = world.env.home.as_deref().unwrap();
        let pool = aterm_agent::harness::ai_pool::AI_POOL_FILE;
        if in_pool {
            let config = world.env.config.as_deref().unwrap();
            let mut named = std::fs::read_to_string(config).unwrap();
            named.push_str("[fleet]\nmachine = \"m16\"\n");
            crate::test_scratch::write_config(config, named);
            aterm_agent::vault_entry::put(
                home,
                pool,
                "version=1\n[[account]]\nprovider='codex'\nlabel='main'\n\
                 email='main@example.test'\nwarm=['m2']\n",
            );
        } else {
            let held = text.replacen(
                "label = \"main\"\n",
                "label = \"main\"\nheld_by = \"m2\"\n",
                1,
            );
            assert_ne!(held, text, "main's row");
            std::fs::write(&roster, held).unwrap();
        }
        assert_eq!(
            world.complete(),
            "ERR codex-rotation selection changed\n",
            "held in the pool: {in_pool}"
        );
        world.recorded_nothing();
        if in_pool {
            aterm_agent::vault_entry::put(home, pool, "");
        } else {
            std::fs::write(&roster, &text).unwrap();
        }
        assert!(
            world
                .complete()
                .starts_with("OK codex-rotation state=completed "),
            "CONTROL, held in the pool: {in_pool}"
        );
    }
}

/// A LOCK ANOTHER WRITER HOLDS IS ANSWERED WAITING, NOT REFUSED: the destination's or the source's
/// auth lock (a token refresh), the machine's sign-in turn, or the receipt writer's
/// `accounts.lock` held elsewhere answers `state=waiting` and records nothing; once it is let go
/// the same report completes.
#[test]
fn a_busy_lock_answers_waiting_and_the_report_completes_once_it_is_free() {
    let world = World::new(9210);
    for login in [&world.target, &world.source] {
        let held = codex_app_login::AuthLock::try_take(login).unwrap().unwrap();
        assert_eq!(world.complete(), WAITING, "{}", login.display());
        world.recorded_nothing();
        drop(held);
    }
    let turn = world.store().signin_now().unwrap().unwrap();
    assert_eq!(world.complete(), WAITING, "the sign-in turn");
    world.recorded_nothing();
    drop(turn);
    let writer = world.store().acquire_bounded().unwrap().unwrap();
    assert_eq!(world.complete(), WAITING, "the receipt writer");
    world.recorded_nothing();
    drop(writer);
    assert!(
        world
            .complete()
            .starts_with("OK codex-rotation state=completed ")
    );
    assert!(world.state().proven);
}

/// A REPORT IS A FACT OF THE POOL, NOT AUTHORITY: a fabric hold on the tab and a person's newer
/// input refuse neither it nor its proof, as neither refuses a consumed receipt's terminal
/// observation. It needs this host to hand a session off at a usage limit at all: with that off it
/// is `denied`, and nothing is recorded.
#[test]
fn a_hold_or_a_persons_input_does_not_refuse_a_report_but_quota_rotation_off_does() {
    let world = World::with(9211, false, |_, _| {});
    assert_eq!(world.complete(), "ERR codex-rotation denied\n");
    world.recorded_nothing();

    let world = World::new(9212);
    assert!(crate::fabric::apply_hold_for_test(
        &world.ctx,
        Some(crate::fabric::Hold {
            reason: "test".into(),
            origin: "local".into(),
        }),
    ));
    world.ctx.human_input.note_reaching(
        crate::metrics::now_us(),
        crate::human_input::Gesture::Typed,
        true,
    );
    assert!(
        world
            .complete()
            .starts_with("OK codex-rotation state=completed ")
    );
    assert!(world.state().proven);
}

/// THE REPORT IS A POOL MOVE'S VERB ALONE, AND A LINE IT CANNOT PARSE IS REFUSED BEFORE ANY CLAIM:
/// `codex-renewal complete` and `codex-login-rotation complete` are `invalid request` under their
/// own verbs, and so is a `codex-rotation complete` with a field too many or too few, or a turn
/// that is no uuid, all before the lane reads a claim, so nothing is read or recorded.
#[test]
fn a_report_under_another_verb_or_malformed_is_an_invalid_request() {
    let world = World::new(9213);
    let line = format!("complete id={} turn={WALL}", world.receipt.id);
    assert_eq!(
        super::super::super::serve(&world.ctx, -1, &line),
        "ERR codex-renewal invalid request\n"
    );
    assert_eq!(
        super::super::super::serve_login_rotation(&world.ctx, -1, &line),
        "ERR codex-login-rotation invalid request\n"
    );
    for malformed in [
        format!("{line} extra=1"),
        format!("complete id={}", world.receipt.id),
        format!("complete id={} turn=not-a-turn", world.receipt.id),
    ] {
        assert_eq!(
            super::super::super::serve_rotation(&world.ctx, -1, &malformed),
            "ERR codex-rotation invalid request\n",
            "{malformed}"
        );
    }
    world.recorded_nothing();
}
