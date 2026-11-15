// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `codex-login-rotation` job driven whole, from the lane's entry for a proven claim
//! ([`serve_claimed`](super::super::super::serve_claimed)) to its published receipt: a real shell
//! and a process under it stand for the agent (so the kernel's facts are the kernel's), a pool of
//! synthetic logins under a directory of the test's own stands for the accounts, and a counting
//! [`Host`] stands for the agent's record and for the provider. Nothing here reads a real login
//! or reaches a provider.

use super::*;
use aterm_agent::harness::codex_app_login::{
    self, IdentifiedRead, IdentifiedUsage, OtherRefusal, RefusalReason, RefusalStanding,
};
use aterm_agent::harness::contract::{self, Scopes};
use aterm_agent::harness::provider_read;
use std::sync::atomic::{AtomicUsize, Ordering};

const THREAD: &str = "019a4b54-5fa1-7123-8123-123456789abc";
const WALL: &str = "019a4b54-5fa2-7123-8123-123456789abc";
const NOW: i64 = 1_792_700_000;
const CONN: u64 = 71;
/// How long a test waits for the job's worker: far longer than a decision over files takes.
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

/// The agent's record and the provider, counted.
struct Fake {
    copy: Mutex<Option<self_claimed::Copy>>,
    /// The session's own login file.
    source: PathBuf,
    /// Every login read, in order.
    reads: Mutex<Vec<PathBuf>>,
    /// How often the session's refresh token was presented.
    presented: AtomicUsize,
    /// What the issuer refuses a presented token for.
    refuses: Mutex<RefusalReason>,
    /// Whether the session's login serves usage.
    works: std::sync::atomic::AtomicBool,
    /// Held by a test while a read must wait (a person types during the observation).
    gate: Mutex<()>,
    /// How many reads have begun, waiting at the gate or past it.
    begun: AtomicUsize,
}

fn usage_of(account: &str, now: i64) -> IdentifiedUsage {
    IdentifiedUsage {
        identity: renewal::Identity::from_native(account, "user"),
        usage: codex_app_login::usage_from_backend(
            &aterm_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": true,
                    "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                        "reset_at": now + 3600},
                    "secondary_window": {"used_percent": 0,
                        "limit_window_seconds": 604_800, "reset_at": now + 86_400}
                }
            }),
            now,
        )
        .expect("provider percentages"),
    }
}

struct FakeReader<'a>(&'a Fake);

impl pool::UsageReader for FakeReader<'_> {
    fn read(
        &self,
        path: &Path,
        _: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: provider_read::Use,
    ) -> IdentifiedRead {
        if use_token
            .begin_home(provider_read::Read::CodexEndpoint, path, now)
            .is_err()
        {
            return IdentifiedRead::Unread;
        }
        assert!(!wait.is_zero());
        let fake = self.0;
        fake.begun.fetch_add(1, Ordering::AcqRel);
        let _gate = fake.gate.lock().unwrap_or_else(|p| p.into_inner());
        fake.reads.lock().unwrap().push(path.to_owned());
        if path != fake.source {
            return IdentifiedRead::Usage(usage_of("target", now));
        }
        if fake.works.load(Ordering::Acquire) {
            return IdentifiedRead::Usage(usage_of("source", now));
        }
        let home = path.parent().unwrap();
        match codex_app_login::refusal_standing(home, None, now) {
            RefusalStanding::Unpresented { .. } => {
                fake.presented.fetch_add(1, Ordering::AcqRel);
                codex_app_login::record_refusal_for_test(home, *fake.refuses.lock().unwrap(), now)
                    .unwrap();
                IdentifiedRead::Unread
            }
            RefusalStanding::Settled { .. } => IdentifiedRead::SignedOut,
            RefusalStanding::Pending { .. } | RefusalStanding::Unknown => IdentifiedRead::Unread,
        }
    }
}

impl Host for Fake {
    fn copy(&self, pid: u32, start: &str) -> Option<self_claimed::Copy> {
        self.copy
            .lock()
            .unwrap()
            .clone()
            .filter(|copy| copy.pid == pid && copy.start == start)
    }

    fn reader<'a>(
        &'a self,
        _: pool::LiveUsage<'a>,
        _: Option<Vec<Vec<u8>>>,
    ) -> Box<dyn pool::UsageReader + 'a> {
        Box::new(FakeReader(self))
    }
}

/// What is on record of the loss of `main`'s login when the session first asks.
#[derive(Clone, Copy)]
enum Loss {
    /// A refusal for this reason settled long ago, and the login read signed out since.
    Ended(RefusalReason),
    /// A refusal for this reason settled long ago, the login not yet read signed out.
    Refused(RefusalReason),
    /// Nothing: aterm never presented the token.
    Unpresented,
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

/// One session at a lost login: its tab, its claimed agent, the pool `main` (the session's, on
/// the default login) and `work` (signed in, another account), and the agent's own record of a
/// turn that ended on an auth wall, owing a `login-rotate@v1` act.
struct World {
    env: Env,
    ctx: Arc<crate::SessionCtx>,
    proof: crate::self_claim::Proven,
    host: Arc<Fake>,
    identity: renewal::Identity,
    /// `main`'s and `work`'s login files.
    source: PathBuf,
    target: PathBuf,
    // Dropped last, in this order: the tab, then its agent, the discovery state and the files.
    _handle: crate::session_store::SessionHandle,
    _agent: Agent,
    _own: crate::proxy::OwnDiscovery,
    _tmp: aterm_tempfile::TempDir,
}

/// How a [`World`] differs from one that serves the move.
#[derive(Clone, Copy)]
struct Policy {
    /// The host's native policy moves a session at a lost login (`codex_rotation_login=1`).
    login_rotation: bool,
    /// The host's native policy hands a session off at a usage limit (`codex_rotation=1`).
    quota_rotation: bool,
    /// aterm.toml's `[harness]` lines.
    harness: &'static str,
    human_grace_s: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            login_rotation: true,
            quota_rotation: false,
            harness: "accounts='early'\nrelaunch=true\nresume_limits=true\n",
            human_grace_s: 0,
        }
    }
}

impl World {
    fn new(local: u64, loss: Loss) -> Self {
        Self::with(local, loss, Policy::default())
    }

    fn with(local: u64, loss: Loss, policy: Policy) -> Self {
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
            format!(
                "[harness]\n{}human_grace_s={}\n",
                policy.harness, policy.human_grace_s
            ),
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
        let seat = accounts_pool::Seat {
            config_dir: &root.join("config"),
            state: &env.state,
            home: &home,
            now: NOW,
        };
        let store = seat.store(accounts::Cli::Codex);
        store
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
                state.active = Some("main".into());
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
        match loss {
            Loss::Ended(reason) | Loss::Refused(reason) => {
                codex_app_login::record_refusal_for_test(&default_home, reason, NOW - 3600)
                    .unwrap();
            }
            Loss::Unpresented => {}
        }
        if matches!(loss, Loss::Ended(_)) {
            let records = accounts_pool::move_records(&seat, accounts::Cli::Codex).unwrap();
            store
                .record_lapse_for_lab(records.roster.row("main").unwrap(), NOW)
                .expect("signed out here");
        }

        let mut handle = crate::session_store::test_handle_on(local, -1);
        let grok = contract::row_named("grok-alab").expect("rostered");
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
                human_grace_s: policy.human_grace_s,
                ..Default::default()
            },
            &[Constructor::PrivateRestart],
        )
        .unwrap();
        let fixture = if policy.login_rotation {
            fixture.with_login_rotation()
        } else {
            fixture
        };
        let fixture = if policy.quota_rotation {
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
        let proof = crate::self_claim::Proven {
            pid: agent.pid,
            start: aterm_agent::harness::restart_stamp::kernel_start(agent.pid)
                .expect("the agent's kernel start"),
        };
        let holder = format!("grok-alab@{}", agent.pid);
        let contract::Words::Scoped(mut claim) =
            contract::read(&holder, &["contract=3", "scope=upgrade"])
        else {
            panic!("rostered fixture");
        };
        claim.scopes = Scopes::for_test(&[Scope::Relaunch]);
        crate::session_timeline::claim_supervisor_scoped(
            &ctx,
            &holder,
            Some(CONN),
            crate::harness_claim::Scoped {
                proven: proof.clone(),
                claim,
            },
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the agent's claim");

        // The host's own socket, which a restart's plan names: this test's discovery state.
        let own = crate::proxy::self_sock_test_guard();
        crate::proxy::set_self_sock(&root, &root.join("control.sock").to_string_lossy());

        let identity = renewal::Identity::from_native("source", "user");
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
                codex_auth_path: None,
                cwd: None,
            }),
        };
        let host = Arc::new(Fake {
            copy: Mutex::new(Some(copy)),
            source: default_home.join(codex_app_login::AUTH_FILE),
            reads: Mutex::new(Vec::new()),
            presented: AtomicUsize::new(0),
            refuses: Mutex::new(RefusalReason::Expired),
            works: std::sync::atomic::AtomicBool::new(false),
            gate: Mutex::new(()),
            begun: AtomicUsize::new(0),
        });
        let world = Self {
            source: default_home.join(codex_app_login::AUTH_FILE),
            target: work.join(codex_app_login::AUTH_FILE),
            env,
            ctx,
            proof,
            host,
            identity,
            _handle: handle,
            _agent: agent,
            _own: own,
            _tmp: tmp,
        };
        world.record(WALL, "login-rotate@v1", "rotation");
        world.rollout(aterm_json::json!({"type": "error", "codex_error_info": "unauthorized"}));
        world
    }

    fn codex_home(&self) -> PathBuf {
        self.source.parent().unwrap().to_owned()
    }

    /// The agent's own record: its thread at rest after the turn `wall`, owing the act `rule`
    /// names under `marker`.
    fn record(&self, wall: &str, rule: &str, marker: &str) {
        let records = self.codex_home().join("alab/sessions");
        std::fs::create_dir_all(&records).unwrap();
        let mut owed = aterm_json::json!({"thread": THREAD, "mode": "land",
            "nativeOrigin": {"turnId": wall, "phase": "complete"}, "rule": rule});
        owed.as_object_mut().unwrap().insert(
            marker.to_owned(),
            aterm_json::json!({"account": self.identity.account, "user": self.identity.user,
                "receipt": null, "openaiScope": true, "starts": 1, "goalId": null}),
        );
        std::fs::write(
            records.join(format!("{}.json", self.proof.pid)),
            aterm_json::json!({"sessionId": THREAD, "status": "waiting", "draftEmpty": true,
                "autopilot": "active", "owed": owed})
            .to_string(),
        )
        .unwrap();
    }

    /// The thread's rollout: the turn [`WALL`], started once and ended on `error`.
    fn rollout(&self, error: aterm_json::Value) {
        let history = self.codex_home().join("sessions/2026/11/03");
        std::fs::create_dir_all(&history).unwrap();
        let text = [
            aterm_json::json!({"type": "task_started", "turn_id": WALL}),
            error,
            aterm_json::json!({"type": "task_complete", "turn_id": WALL}),
        ]
        .iter()
        .map(|payload| {
            format!(
                "{}\n",
                aterm_json::json!({"type": "event_msg", "payload": payload})
            )
        })
        .collect::<String>();
        std::fs::write(
            history.join(format!("rollout-fixture-{THREAD}.jsonl")),
            text,
        )
        .unwrap();
    }

    fn prepare_line(&self) -> String {
        format!(
            "prepare thread={THREAD} turn={WALL} source_account={} source_user={}",
            self.identity.account, self.identity.user
        )
    }

    /// `line`, asked of the lane at `now` as the claimed agent asks it.
    fn ask_at(&self, line: &str, now: i64) -> String {
        let request = renewal::Request::parse_login_rotation(line).expect("a well-formed request");
        let copy = self
            .host
            .copy(self.proof.pid, &self.proof.start)
            .expect("the agent's record");
        let mut env = self.env.clone();
        env.now = now;
        let host: Arc<dyn Host> = Arc::clone(&self.host) as Arc<dyn Host>;
        super::super::super::renamed(
            super::super::super::serve_claimed(
                &self.ctx,
                (env, self.proof.clone(), copy),
                request,
                renewal::Purpose::LoginRotation,
                &host,
            ),
            "codex-login-rotation",
        )
    }

    fn ask(&self) -> String {
        self.ask_at(&self.prepare_line(), NOW)
    }

    fn job(&self) -> Option<Arc<Preparation>> {
        self.ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .login_rotation_prepare
            .clone()
    }

    /// Waits for the job's worker to have decided.
    fn decided(&self) {
        let job = self.job().expect("a job");
        let since = Instant::now();
        while matches!(*job.progress.lock().unwrap(), Progress::Reading) {
            assert!(since.elapsed() < PATIENCE, "hang: the decision never came");
            aterm_uds::deadline::poll_yield();
        }
    }

    /// The first ask, which starts the decision, and the decision itself.
    fn ask_and_decide(&self) -> String {
        let reply = self.ask();
        if self
            .job()
            .is_some_and(|job| matches!(*job.progress.lock().unwrap(), Progress::Reading))
        {
            self.decided();
        }
        reply
    }

    /// Lets the job's retry come now, in place of waiting [`RETRY_MS`] out.
    fn retry_now(&self) {
        let job = self.job().expect("a job");
        let mut progress = job.progress.lock().unwrap();
        assert!(matches!(*progress, Progress::Retry(_)), "{progress:?}");
        *progress = Progress::Retry(Instant::now());
    }

    fn reads(&self) -> Vec<PathBuf> {
        self.host.reads.lock().unwrap().clone()
    }

    fn state(&self) -> accounts::State {
        self.store().load(NOW).unwrap().0
    }

    fn store(&self) -> accounts::Store {
        self.env.accounts_store_for(accounts::Cli::Codex).unwrap()
    }

    fn edit(&self, edit: impl FnOnce(&mut accounts::State)) {
        self.store()
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut None, |state| {
                edit(state);
                Ok(Vec::new())
            })
            .unwrap();
    }

    /// The receipt a `state=ready` reply names.
    fn receipt_of(&self, reply: &str) -> Receipt {
        let id = reply
            .split_ascii_whitespace()
            .find_map(|word| word.strip_prefix("id="))
            .unwrap_or_else(|| panic!("no receipt in `{reply}`"));
        renewal::read_receipt(&self.env, id).expect("the receipt on record")
    }

    /// The exact reply a Ready receipt `id` of this session's move to `work` is answered with.
    fn ready_line(&self, id: &str) -> String {
        let target = renewal::Identity::from_native("target", "user");
        format!(
            "OK codex-login-rotation state=ready id={id} thread={THREAD} turn={WALL} \
             source_account={} source_user={} target_account={} target_user={}\n",
            self.identity.account, self.identity.user, target.account, target.user
        )
    }
}

/// A SETTLED EXPIRY ON RECORD IS ANSWERED WAITING, THEN READY WITH THE EXACT LINE: the first ask
/// starts the decision and answers waiting; the next publishes what it selected and answers
/// `state=ready` with the receipt's id, the request's thread, turn and source, and `work`'s
/// identity as the target, another account than the source. The receipt on record is a Ready
/// `login_rotation` one from the session's login file to `work`'s. Only `work` was read, once,
/// and asking again answers the same line with no further read. The pool's own selection is as
/// it was, and a Ready receipt holds nothing in flight (`restart account=` is not held off).
#[test]
fn a_settled_expiry_is_answered_waiting_then_ready_with_the_exact_line() {
    let world = World::new(9101, Loss::Ended(RefusalReason::Expired));
    assert_eq!(world.ask_and_decide(), WAITING);
    let ready = world.ask();
    let receipt = world.receipt_of(&ready);
    assert_eq!(ready, world.ready_line(&receipt.id));
    assert_eq!(receipt.intent.purpose, renewal::Purpose::LoginRotation);
    assert_eq!(receipt.phase, Phase::Ready);
    assert_eq!(receipt.old_auth, world.source);
    assert_eq!(receipt.new_auth, world.target);
    assert_eq!(
        receipt.destination,
        Some(renewal::Identity::from_native("target", "user"))
    );
    assert_ne!(receipt.destination.as_ref(), Some(&world.identity));
    assert_eq!(receipt.old_pid, world.proof.pid);
    assert_eq!(receipt.sid, world.ctx.self_id.as_str());
    assert_eq!(world.reads(), vec![world.target.clone()]);
    assert_eq!(world.host.presented.load(Ordering::Acquire), 0);

    assert_eq!(world.ask(), ready, "asked again");
    assert_eq!(world.reads().len(), 1, "and nothing read again");

    let state = world.state();
    assert_eq!(state.active.as_deref(), Some("main"));
    assert!(state.rotations.is_empty() && state.canary.is_none());
    assert!(
        world
            .store()
            .load_logins(NOW)
            .unwrap()
            .0
            .native_rotations
            .is_empty()
    );
    assert!(
        !crate::native_act::renewal_pending(&world.ctx),
        "a Ready receipt holds nothing in flight"
    );
    // The fence `restart account=` asks passes it, and answers the pool's own word.
    let sid = world.ctx.self_id.as_str().to_owned();
    let copy = world
        .host
        .copy(world.proof.pid, &world.proof.start)
        .unwrap();
    let resume = copy.resume.clone().unwrap();
    let snap = aterm_agent::harness::relaunch::snapshot_of_copy(&sid, &copy, &resume, 1, None);
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
    let judged = crate::restart_act::Judged {
        pid: world.proof.pid,
        start: world.proof.start.clone(),
        copied: true,
        home: None,
        refusal: Some(accounts_pool::MOVE_LIMITED),
    };
    let fence = crate::restart_act::account_fence(
        &world.ctx,
        (world.proof.pid, &world.proof.start),
        &aterm_agent::harness::restart_stamp::Wanted::Row("work".into()),
        Some(&judged),
        None,
    );
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    assert_eq!(
        fence,
        Some(accounts_pool::MOVE_LIMITED),
        "`restart account=` is not held off by a Ready receipt"
    );
}

/// THE COMMON LOST LOGIN, END TO END: a token aterm never presented, on a row a person pinned
/// before the loss. The first ask presents it once and, the refusal being aterm's own, the job
/// waits for it to settle; the ask once it settled makes the one confirming read, which reads
/// the login signed out (ending the pin, as any sign-out does) and each candidate once; the next
/// ask answers ready. In all: the source read twice, its token presented once, `work` once.
#[test]
fn the_common_lost_login_waits_for_its_refusal_to_settle_then_is_ready() {
    let world = World::new(9102, Loss::Unpresented);
    world.edit(|state| state.pinned = true);
    assert_eq!(world.ask_and_decide(), WAITING);
    assert_eq!(world.reads(), vec![world.source.clone()]);
    assert_eq!(world.host.presented.load(Ordering::Acquire), 1);
    assert_eq!(world.ask(), WAITING, "not before the retry");
    assert_eq!(world.reads().len(), 1);
    assert!(world.state().pinned, "nothing is recorded of the read yet");

    world.retry_now();
    let settled = NOW + 60;
    assert_eq!(world.ask_at(&world.prepare_line(), settled), WAITING);
    world.decided();
    let ready = world.ask_at(&world.prepare_line(), settled);
    let receipt = world.receipt_of(&ready);
    assert_eq!(ready, world.ready_line(&receipt.id));
    assert_eq!(
        world.reads(),
        vec![
            world.source.clone(),
            world.source.clone(),
            world.target.clone()
        ]
    );
    assert_eq!(world.host.presented.load(Ordering::Acquire), 1);
    assert!(!world.state().pinned, "the sign-out ended the row's pin");
}

/// THE JOB RUNS ONLY WHERE THE HOST SERVES THE MOVE AND aterm.toml LETS A HANDOFF RUN: without the
/// native policy's word (`codex_rotation_login=1`), though the usage-limit handoff is on, and
/// with accounts off, relaunch off or limit resumption off in aterm.toml, the ask is answered
/// not-needed at once: no job, no read, no receipt. CONTROL: the same loss with both is ready.
#[test]
fn the_job_needs_the_hosts_word_and_a_policy_that_lets_a_handoff_run() {
    for (local, policy) in [
        (
            9111,
            Policy {
                login_rotation: false,
                quota_rotation: true,
                ..Policy::default()
            },
        ),
        (
            9112,
            Policy {
                harness: "accounts='off'\nrelaunch=true\nresume_limits=true\n",
                ..Policy::default()
            },
        ),
        (
            9113,
            Policy {
                harness: "accounts='early'\nrelaunch=false\nresume_limits=true\n",
                ..Policy::default()
            },
        ),
        (
            9114,
            Policy {
                harness: "accounts='early'\nrelaunch=true\nresume_limits=false\n",
                ..Policy::default()
            },
        ),
    ] {
        let world = World::with(local, Loss::Ended(RefusalReason::Expired), policy);
        for _ in 0..2 {
            assert_eq!(world.ask(), NOT_NEEDED, "{local}");
        }
        assert!(world.job().is_none(), "{local}: no job");
        assert!(world.reads().is_empty(), "{local}: no read");
    }
    let control = World::new(9115, Loss::Ended(RefusalReason::Expired));
    assert_eq!(control.ask_and_decide(), WAITING);
    assert!(
        control
            .ask()
            .starts_with("OK codex-login-rotation state=ready id="),
        "CONTROL"
    );
}

/// A HOLD OR A PERSON'S RECENT INPUT ANSWERS WAITING WITH NO JOB AND NO READ, AND A PIN STOPS THE
/// MOVE WITH NO READ: under a hold on the session, and inside the grace after a person typed, the
/// lane answers waiting before the job is reached. A pin on another row, and one on the session's
/// own row set after its sign-out, are decided from the records: waiting, then not-needed.
#[test]
fn a_hold_or_grace_answers_waiting_and_a_pin_stops_the_move_with_no_read() {
    // One world at a time: each holds the test's own discovery state.
    {
        let held = World::new(9121, Loss::Ended(RefusalReason::Expired));
        held.ctx.fabric.seed_holds(
            crate::fabric::parse_holds(["local mine"]),
            &held.ctx.timeline,
        );
        assert!(held.ctx.fabric.hold().is_some());
        for _ in 0..2 {
            assert_eq!(held.ask(), WAITING);
        }
        assert!(held.job().is_none() && held.reads().is_empty());
    }
    {
        let typed = World::with(
            9122,
            Loss::Ended(RefusalReason::Expired),
            Policy {
                human_grace_s: 30,
                ..Policy::default()
            },
        );
        typed.ctx.human_input.note_reaching(
            crate::metrics::now_us(),
            crate::human_input::Gesture::Typed,
            true,
        );
        for _ in 0..2 {
            assert_eq!(typed.ask(), WAITING);
        }
        assert!(typed.job().is_none() && typed.reads().is_empty());
    }

    for (local, active) in [(9123, "work"), (9124, "main")] {
        let pinned = World::new(local, Loss::Ended(RefusalReason::Expired));
        pinned.edit(|state| {
            state.active = Some(active.into());
            state.pinned = true;
        });
        assert_eq!(pinned.ask_and_decide(), WAITING, "{active}");
        assert_eq!(pinned.ask(), NOT_NEEDED, "{active}");
        assert!(pinned.reads().is_empty(), "{active}");
        assert!(pinned.state().pinned, "{active}: the pin stands");
    }
}

/// EVERY LOSS BUT A SETTLED EXPIRY IS ANSWERED NOT-NEEDED: a settled refusal as reused,
/// invalidated, `invalid_grant`, a code aterm does not name or an invalid token, whether or not
/// the login was read signed out since, with no read; a login in aterm's own custody, whose
/// journal is not its record, with no read; and a login that still serves usage, after its one
/// presentation read. No receipt is published for any.
#[test]
fn every_other_loss_is_answered_not_needed() {
    let mut local = 9130;
    let mut stopped = |loss: Loss, arrange: &dyn Fn(&World), reads: usize, what: &str| {
        local += 1;
        let world = World::new(local, loss);
        arrange(&world);
        assert_eq!(world.ask_and_decide(), WAITING, "{what}");
        for _ in 0..2 {
            assert_eq!(world.ask(), NOT_NEEDED, "{what}");
        }
        assert_eq!(world.reads().len(), reads, "{what}");
        assert!(!world.reads().contains(&world.target), "{what}");
    };
    for reason in [
        RefusalReason::Reused,
        RefusalReason::Invalidated,
        RefusalReason::InvalidGrant,
        RefusalReason::of_code("account_deactivated"),
        RefusalReason::Other(OtherRefusal::InvalidToken),
    ] {
        stopped(Loss::Ended(reason), &|_| {}, 0, reason.code());
        stopped(Loss::Refused(reason), &|_| {}, 0, reason.code());
    }
    stopped(
        Loss::Ended(RefusalReason::Expired),
        &|world| {
            std::fs::write(
                world.codex_home().join(format!(
                    "{}{}",
                    codex_app_login::AUTH_FILE,
                    codex_app_login::OWNER_MARKER_SUFFIX
                )),
                codex_app_login::OWNER_MARKER_TEXT,
            )
            .unwrap();
        },
        0,
        "custody",
    );
    stopped(
        Loss::Unpresented,
        &|world| world.host.works.store(true, Ordering::Release),
        1,
        "login works",
    );
}

/// THE LANE REFUSES A WALL THE AGENT'S OWN RECORDS DO NOT SHOW, AND AN ID OF ANOTHER PURPOSE: a
/// turn that ended on a usage limit is `native wall changed`; a record of another turn is `wall
/// changed`; and a usage-limit handoff's receipt asked by id under this verb, as this verb's
/// under `codex-rotation`, is `wrong recovery purpose`. None starts a job or reads.
#[test]
fn a_wall_the_records_do_not_show_and_an_id_of_another_purpose_are_refused() {
    let world = World::new(9141, Loss::Ended(RefusalReason::Expired));
    world.rollout(aterm_json::json!({"type": "error",
        "codex_error_info": "usage_limit_exceeded"}));
    assert_eq!(
        world.ask(),
        "ERR codex-login-rotation native wall changed\n"
    );
    world.rollout(aterm_json::json!({"type": "error", "codex_error_info": "unauthorized"}));
    world.record(
        "019a4b54-5fa3-7123-8123-123456789abc",
        "login-rotate@v1",
        "rotation",
    );
    assert_eq!(world.ask(), "ERR codex-login-rotation wall changed\n");
    // The usage-limit handoff's own act on the same turn is another rule's.
    world.record(WALL, "usage-rotate@v1", "rotation");
    assert_eq!(world.ask(), "ERR codex-login-rotation wall changed\n");
    assert!(world.job().is_none() && world.reads().is_empty());

    world.record(WALL, "login-rotate@v1", "rotation");
    assert_eq!(world.ask_and_decide(), WAITING);
    let ready = world.ask();
    let mine = world.receipt_of(&ready);
    let quota = Receipt {
        id: "cd".repeat(16),
        intent: Intent {
            purpose: renewal::Purpose::WeeklyQuota,
            ..mine.intent.clone()
        },
        ..mine.clone()
    };
    renewal::with_receipt_writer(&world.env, |_| renewal::write_receipt(&world.env, &quota))
        .unwrap()
        .unwrap();
    for verb in ["status", "commit"] {
        assert_eq!(
            world.ask_at(&format!("{verb} id={}", quota.id), NOW),
            "ERR codex-login-rotation wrong recovery purpose\n",
            "{verb}"
        );
    }
    let copy = world
        .host
        .copy(world.proof.pid, &world.proof.start)
        .unwrap();
    let host: Arc<dyn Host> = Arc::clone(&world.host) as Arc<dyn Host>;
    assert_eq!(
        super::super::super::renamed(
            super::super::super::serve_claimed(
                &world.ctx,
                (world.env.clone(), world.proof.clone(), copy),
                renewal::Request::parse_rotation(&format!("status id={}", mine.id)).unwrap(),
                renewal::Purpose::WeeklyQuota,
                &host,
            ),
            "codex-rotation",
        ),
        "ERR codex-rotation wrong recovery purpose\n"
    );
    assert_eq!(world.reads().len(), 1, "only the decision's one candidate");
}

/// A PIN PLACED AFTER THE SELECTION REFUSES ITS PUBLICATION, WHICH READS NOTHING MORE AND LAPSES:
/// the decision selected `work`; a person then pinned the pool, so the session-scoped writer
/// refuses the receipt. The job answers waiting, retries the publication alone no sooner than the
/// retry, and once the selection is [`OBSERVATION_MS`] old answers not-needed for good. The
/// reads are those of the one observation, no receipt is on record, and a later ask on the same
/// wall turn reads nothing: its one pass over the candidates is spent.
#[test]
fn a_publication_a_pin_refuses_reads_nothing_more_and_lapses() {
    let world = World::new(9151, Loss::Ended(RefusalReason::Expired));
    assert_eq!(world.ask_and_decide(), WAITING);
    assert_eq!(world.reads(), vec![world.target.clone()]);
    world.edit(|state| state.pinned = true);
    let kept = reads(&world.ctx, &world.job().unwrap()).unwrap();
    for _ in 0..3 {
        assert_eq!(world.ask(), WAITING);
        // The publication's own retry has come.
        kept.lock()
            .unwrap()
            .selected
            .as_mut()
            .expect("still selected")
            .publish_at = Some(Instant::now());
    }
    assert_eq!(world.reads().len(), 1, "a failed publication reads nothing");
    assert!(
        renewal::with_receipt_writer(&world.env, |writer| {
            writer.read_indexed(&index(&world.job().unwrap()).unwrap())
        }) == Ok(Some(None)),
        "no receipt was published under the pin"
    );

    let observed = kept.lock().unwrap().selected.as_ref().unwrap().observed;
    let Some(long_ago) = observed.checked_sub(Duration::from_millis(OBSERVATION_MS)) else {
        // A host whose clock began less than the observation ago cannot age the selection.
        return;
    };
    kept.lock().unwrap().selected.as_mut().unwrap().observed = long_ago;
    for _ in 0..2 {
        assert_eq!(world.ask(), NOT_NEEDED, "lapsed");
    }
    assert!(kept.lock().unwrap().selected.is_none());
    // A person's input replaces the job; the wall turn's reads are spent all the same.
    world.edit(|state| state.pinned = false);
    world.ctx.human_input.note_reaching(
        crate::metrics::now_us(),
        crate::human_input::Gesture::Typed,
        true,
    );
    assert_eq!(world.ask_and_decide(), WAITING);
    assert_eq!(world.ask(), NOT_NEEDED);
    assert_eq!(world.reads().len(), 1, "one observation's reads in all");
}

/// A MOVE SELECTED WHILE A PERSON TYPED IS PUBLISHED BY THE JOB THAT FOLLOWS, WITH NO FURTHER
/// READ: a person's input during the observation replaces the job. The job that follows waits
/// while the first one's decision still reads (it starts none of its own), then publishes the
/// move that decision selected, under its own request: `work` was read once in all. Dropping the
/// stale job's move instead would leave the session at its lost login, since the wall turn's one
/// pass over the candidates is spent.
#[test]
fn a_move_selected_while_a_person_typed_is_published_by_the_job_that_follows() {
    let world = World::new(9161, Loss::Ended(RefusalReason::Expired));
    let gate = world.host.gate.lock().unwrap();
    assert_eq!(world.ask(), WAITING);
    let first = world.job().unwrap();
    let since = Instant::now();
    while world.host.begun.load(Ordering::Acquire) == 0 {
        assert!(since.elapsed() < PATIENCE, "hang: the decision never read");
        aterm_uds::deadline::poll_yield();
    }
    world.ctx.human_input.note_reaching(
        crate::metrics::now_us(),
        crate::human_input::Gesture::Typed,
        true,
    );
    assert_eq!(world.ask(), WAITING);
    let second = world.job().unwrap();
    assert!(!Arc::ptr_eq(&first, &second), "the input replaced the job");
    assert!(
        matches!(*second.progress.lock().unwrap(), Progress::Retry(_)),
        "which starts no decision while the first still reads"
    );
    drop(gate);
    let since = Instant::now();
    while matches!(*first.progress.lock().unwrap(), Progress::Reading) {
        assert!(since.elapsed() < PATIENCE, "hang: the decision never came");
        aterm_uds::deadline::poll_yield();
    }
    let ready = world.ask();
    let receipt = world.receipt_of(&ready);
    assert_eq!(ready, world.ready_line(&receipt.id));
    assert_eq!(
        receipt.human_seq,
        world.ctx.human_input.seq(),
        "published under the request that followed the input"
    );
    assert_eq!(world.reads(), vec![world.target.clone()]);
}
