// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit Rust-only real-process qualification host; never a CLI/environment
//! activation path in the installed application.
use super::*;
use aterm_session::LaunchNonce;
use aterm_window::application::ApplicationHandler;
use aterm_window::event::WindowEvent;
use aterm_window::event_loop::ActiveEventLoop;
use aterm_window::platform::pump_events::EventLoopExtPumpEvents;
use aterm_window::window::WindowId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "native_renewal_live_snapshot.rs"]
mod live_snapshot;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRenewalFixture {
    pub root: PathBuf,
    /// Explicit test-only shared pool, preseeded by the fixture driver. Each
    /// native host still owns its own root, control socket, PTY and receipts.
    pub shared_pool_root: Option<PathBuf>,
    pub codex: PathBuf,
    pub cwd: PathBuf,
    pub auth_path: PathBuf,
    pub successor_auth_path: Option<PathBuf>,
    #[serde(default)]
    pub additional_auth_paths: Vec<PathBuf>,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub guardian_socket: Option<PathBuf>,
    pub adopt: Option<NativeRenewalAdoption>,
    pub human_grace_s: Option<u32>,
    pub continue_policy: Option<bool>,
    #[serde(default)]
    pub weekly_quota: bool,
}

fn fixture_pool_root<'a>(
    root: &'a std::path::Path,
    shared: Option<&'a std::path::Path>,
    weekly_quota: bool,
    adopting: bool,
) -> Result<&'a std::path::Path, String> {
    let Some(shared) = shared else {
        return Ok(root);
    };
    let plain_absolute = |path: &std::path::Path| {
        path.is_absolute()
            && path.components().all(|part| {
                matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
    };
    if !weekly_quota
        || adopting
        || !plain_absolute(shared)
        || !plain_absolute(root)
        || root == shared
        || !root.starts_with(shared)
    {
        return Err("shared fixture pool must contain a distinct quota host root".into());
    }
    Ok(shared)
}

#[cfg(test)]
#[test]
fn native_renewal_shared_pool_requires_distinct_owned_quota_hosts() {
    use std::path::Path;
    let pool = Path::new("/fixture-pool");
    let host = pool.join("host-one");
    assert_eq!(fixture_pool_root(&host, None, false, false).unwrap(), host);
    assert_eq!(
        fixture_pool_root(&host, Some(pool), true, false).unwrap(),
        pool
    );
    for (root, weekly, adopting) in [
        (pool, true, false),
        (Path::new("/elsewhere"), true, false),
        (Path::new("/fixture-pool/../elsewhere"), true, false),
        (host.as_path(), false, false),
        (host.as_path(), true, true),
    ] {
        assert!(fixture_pool_root(root, Some(pool), weekly, adopting).is_err());
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRenewalAdoption {
    pub master: i32,
    pub shell_pid: i32,
    pub sid: String,
    pub nonce: String,
    pub prior_host_pid: u32,
    pub prior_host_birth: u64,
    pub waited_pid: u32,
    pub wait_outcome: NativeRenewalWaitOutcome,
}

/// Positive wait/reap evidence from the fixture guardian which spawned the host.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeRenewalWaitOutcome {
    Exited { code: u8 },
    Signaled { signal: u8 },
}

struct FixturePty {
    master: i32,
    shell: aterm_pty::ShellIdentity,
}
impl Drop for FixturePty {
    fn drop(&mut self) {
        aterm_pty::hangup_shell(&self.shell, self.master);
        aterm_pty::close_fd(self.master);
    }
}

fn note_fixture_person(
    ctx: &crate::SessionCtx,
    event: &crate::input::InputEvent,
    source: crate::input::Source,
) {
    if matches!(source, crate::input::Source::Human) && crate::human_input::is_person_gesture(event)
    {
        let _ = crate::app_input::note_person_event(ctx, event);
    }
}

#[cfg(test)]
#[test]
fn codex_renewal_fixture_counts_real_human_source_only() {
    let handle = crate::session_store::test_handle_on(8876, -1);
    let event = crate::input::InputEvent::KeySequence(vec![b'x']);
    let before = handle.ctx.human_input.seq();
    note_fixture_person(&handle.ctx, &event, crate::input::Source::Controller);
    assert_eq!(handle.ctx.human_input.seq(), before);
    note_fixture_person(&handle.ctx, &event, crate::input::Source::Human);
    assert!(handle.ctx.human_input.seq() > before);
}

pub(super) struct Pump {
    ctx: Arc<SessionCtx>,
    term: Arc<Mutex<Terminal>>,
    handle: crate::session_store::SessionHandle,
    unhandled_wakes: std::collections::BTreeSet<String>,
}
impl ApplicationHandler<Wake> for Pump {
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    fn user_event(&mut self, _: &ActiveEventLoop, wake: Wake) {
        self.dispatch(wake);
    }
}

impl Pump {
    pub(super) fn source_fixture(
        handle: crate::session_store::SessionHandle,
        term: Arc<Mutex<Terminal>>,
    ) -> Self {
        Self {
            ctx: Arc::clone(&handle.ctx),
            term,
            handle,
            unhandled_wakes: Default::default(),
        }
    }

    fn dispatch(&mut self, wake: Wake) {
        let wake = crate::control::control_media::take_hop(wake);
        let named = match &wake {
            Wake::ReadSessionStatus { .. } => Some("ReadSessionStatus"),
            Wake::ReadSessionStatuses { .. } => Some("ReadSessionStatuses"),
            Wake::ReadHarnessPolicy { .. } => Some("ReadHarnessPolicy"),
            Wake::ReadSessionWindows { .. } => Some("ReadSessionWindows"),
            Wake::ReadAppStatus { .. } => Some("ReadAppStatus"),
            Wake::PermissionWalkAsked { .. } => Some("PermissionWalkAsked"),
            Wake::FrontControlSurface { .. } => Some("FrontControlSurface"),
            _ => None,
        };
        if let Some(name) = named {
            eprintln!(
                "native-renewal-fixture pid={} wake={name}",
                std::process::id()
            );
        }
        let tag = format!("{:?}", std::mem::discriminant(&wake));
        match wake {
            Wake::Input {
                batch,
                mark,
                reply,
                fence,
                src,
                ..
            } => {
                let _fenced = match fence.as_ref() {
                    None => None,
                    Some(crate::session_timeline::SupervisorInputDelivery::Legacy(holder)) => {
                        let lease = crate::session_timeline::FencedWrite::enter(
                            &self.ctx,
                            holder,
                            crate::metrics::now_us(),
                        )
                        .ok();
                        if lease.is_none() {
                            if let Some(reply) = reply {
                                let _ = reply.send(InputOutcome::WriteFailed);
                            }
                            return;
                        }
                        lease
                    }
                    Some(fence @ crate::session_timeline::SupervisorInputDelivery::Scoped(_)) => {
                        if fence.check(&self.ctx, crate::metrics::now_us()).is_err() {
                            if let Some(reply) = reply {
                                let _ = reply.send(InputOutcome::WriteFailed);
                            }
                            return;
                        }
                        None
                    }
                };
                let mut outcome = InputOutcome::Ok;
                for event in batch {
                    note_fixture_person(&self.ctx, &event, src);
                    let receipt = crate::app_input::tracked_egress(
                        &self.term,
                        &self.ctx.modes,
                        &self.ctx.sink,
                        &self.ctx.output_echo,
                        &event,
                        crate::input::EgressMode::Backpressured,
                    );
                    let event_outcome = crate::app_input::receipt_outcome(receipt);
                    if event_outcome.failed() {
                        outcome = event_outcome;
                    }
                    if let Some(mark) = mark.as_ref() {
                        mark.completion().finish(receipt);
                    }
                }
                if let Some(reply) = reply {
                    let _ = reply.send(outcome);
                }
            }
            Wake::ReadSessionStatus { session, reply } => {
                let result = if session == self.handle.local_id {
                    crate::session_status::native_control_status(
                        &self.ctx,
                        &self.term,
                        self.handle.master,
                        session,
                    )
                } else {
                    Err("no such fixture session".to_owned())
                };
                let mut observed = status_observation(&result);
                let sent = reply.send(result).is_ok();
                observed
                    .as_object_mut()
                    .expect("fixture status observation")
                    .insert("replySent".into(), aterm_json::json!(sent));
                self.ctx.native_policy.renewal_control_observed(&observed);
            }
            Wake::ReadSessionStatuses { reply } => {
                let result = crate::session_status::native_control_status(
                    &self.ctx,
                    &self.term,
                    self.handle.master,
                    self.handle.local_id,
                )
                .map(|row| {
                    format!(
                        "OK 1\n{} {} {} {row}\n",
                        self.handle.local_id,
                        self.handle.sid.as_str(),
                        self.handle.nonce.to_hex()
                    )
                })
                .unwrap_or_else(|error| format!("ERR {error}\n"));
                let _ = reply.send(result);
            }
            Wake::ReadHarnessPolicy { session, reply } => {
                let result = if session == self.handle.local_id {
                    self.ctx
                        .native_policy
                        .config()
                        .map(|config| {
                            crate::harness_policy_verb::configured_facts(
                                &self.ctx, config, 1, true, true, false,
                            )
                        })
                        .ok_or_else(|| "fixture policy unavailable".to_owned())
                } else {
                    Err("no such fixture session".to_owned())
                };
                let _ = reply.send(result);
            }
            _ => {
                if self.unhandled_wakes.insert(tag.clone()) {
                    eprintln!(
                        "native-renewal-fixture pid={} unhandled={} tag={tag}",
                        std::process::id(),
                        named.unwrap_or("Other")
                    );
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn codex_renewal_fixture_dispatches_real_counted_status_hop() {
    static HOPS: crate::control::control_media::Hops = crate::control::control_media::Hops::new();
    let handle = crate::session_store::test_handle_on(8878, -1);
    let mut pump = Pump {
        ctx: Arc::clone(&handle.ctx),
        term: Arc::clone(&handle.term),
        handle,
        unhandled_wakes: Default::default(),
    };
    let mut queued = None;
    let (answer, _hop) = {
        let mut post = crate::control::control_media::MainPost::injected(
            |wake| {
                queued = Some(wake);
                true
            },
            &HOPS,
        );
        post.ask(|reply| Wake::ReadSessionStatus {
            session: 8878,
            reply,
        })
        .unwrap()
    };
    pump.dispatch(queued.unwrap());
    assert!(answer.recv().unwrap().unwrap().contains("schema=1"));
}

#[cfg(test)]
#[test]
fn codex_renewal_native_status_matches_app_claim_and_counter_fields() {
    let app = crate::App::headless_for_test();
    let pooled = app.pool.get(0).unwrap();
    let full = app.session_status_record(0).unwrap();
    let native =
        crate::session_status::native_control_status(&pooled.ctx, &pooled.term, pooled.master, 0)
            .unwrap();
    let field = |text: &str, key: &str| {
        text.split_whitespace().find_map(|word| {
            word.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value.to_owned())
        })
    };
    for key in [
        "hold",
        "hand",
        "hand_pid",
        "human_ms",
        "human_seq",
        "supervisor",
        "supervisor_proven",
        "supervisor_contract",
        "supervisor_scope",
        "supervisor_revision",
        "gen",
        "seq",
        "hash",
    ] {
        assert_eq!(field(&full, key), field(&native, key), "{key}");
    }
    assert_eq!(field(&native, "observed").as_deref(), Some("false"));
    assert_eq!(field(&native, "phase").as_deref(), Some("unknown"));
}

/// Run an owned real PTY and control host until `<root>/stop` exists. The caller
/// starts this helper process with HOME and ATERM_HARNESS_STATE inside root.
/// Credentials, provider config and account bindings are fixture-owned inputs.
pub fn run_native_renewal_host_fixture(config: NativeRenewalFixture) -> Result<(), String> {
    let env = aterm_agent::harness::cli::Env::from_process()?;
    let home = env.home.as_ref().ok_or("fixture HOME missing")?;
    let pool_root = fixture_pool_root(
        &config.root,
        config.shared_pool_root.as_deref(),
        config.weekly_quota,
        config.adopt.is_some(),
    )?;
    if !config.root.is_absolute()
        || !home.starts_with(pool_root)
        || !env.state.starts_with(pool_root)
        || env
            .config
            .as_ref()
            .is_none_or(|path| !path.starts_with(pool_root))
        || !config.cwd.starts_with(&config.root)
        || !config.auth_path.starts_with(pool_root)
        || !config.codex.is_absolute()
        || config
            .successor_auth_path
            .as_ref()
            .is_some_and(|path| !path.starts_with(pool_root))
        || config
            .additional_auth_paths
            .iter()
            .any(|path| !path.starts_with(pool_root))
    {
        return Err("fixture paths must remain in the owned root".into());
    }
    std::fs::create_dir_all(&config.root).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(home.join(".native-renewal-fixture")).map_err(|e| e.to_string())?;
    let roster = env
        .accounts_roster_for(aterm_agent::harness::accounts::Cli::Codex)
        .ok_or("fixture roster path")?;
    let old_home = config.auth_path.parent().ok_or("fixture auth path")?;
    let email = aterm_agent::harness::codex_app_login::read_account_email(old_home)
        .ok_or("fixture native email")?;
    if config.shared_pool_root.is_some() && !roster.is_file() {
        return Err("shared fixture pool requires a preseeded roster".into());
    }
    if roster.exists() && config.adopt.is_none() && config.shared_pool_root.is_none() {
        return Err("fixture requires an unseeded roster".into());
    }
    std::fs::create_dir_all(roster.parent().ok_or("fixture roster parent")?)
        .map_err(|e| e.to_string())?;
    if config.adopt.is_none() && config.shared_pool_root.is_none() {
        let mut roster_text = format!(
            "[[account]]\nlabel='main'\nclass='dedicated'\ndir={}\nemail={}\n",
            aterm_toml::Value::from(old_home.to_str().ok_or("fixture old home")?),
            aterm_toml::Value::from(email.as_str()),
        );
        if config.weekly_quota {
            let next = config
                .successor_auth_path
                .as_ref()
                .ok_or("quota fixture successor")?;
            for (index, auth_path) in std::iter::once(next)
                .chain(config.additional_auth_paths.iter())
                .enumerate()
            {
                let home = auth_path.parent().ok_or("quota fixture successor")?;
                let email = aterm_agent::harness::codex_app_login::read_account_email(home)
                    .ok_or("quota fixture successor email")?;
                let label = if index == 0 {
                    "target".into()
                } else {
                    format!("target-{index}")
                };
                roster_text.push_str(&format!(
                    "\n[[account]]\nlabel={label:?}\nclass='dedicated'\ndir={}\nemail={}\n",
                    aterm_toml::Value::from(home.to_str().ok_or("quota fixture successor path")?),
                    aterm_toml::Value::from(email.as_str()),
                ));
            }
            let path = env.config.as_deref().ok_or("quota fixture config")?;
            aterm_uds::atomic_file::write(path, format!(
                "[harness]\naccounts='early'\nrelaunch=true\nresume_limits=true\nhuman_grace_s={}\n",
                config.human_grace_s.unwrap_or(0),
            ).as_bytes(), 0o600).map_err(|e| e.to_string())?;
        }
        aterm_uds::atomic_file::write(&roster, roster_text.as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        if config.weekly_quota {
            env.accounts_store_for(aterm_agent::harness::accounts::Cli::Codex)
                .ok_or("quota fixture store")?
                .transact(env.now, &mut None, |state| {
                    state.active = Some("main".into());
                    Ok(Vec::new())
                })?;
        }
    }

    let mut builder = crate::quiet_driver_event_loop_builder::<Wake>();
    builder.with_headless();
    let mut event_loop = builder.build().map_err(|e| e.to_string())?;
    let term = Arc::new(Mutex::new(Terminal::new(40, 140)));
    let mut handle = super::super::control_redraw_conformance::registered_session(1, &term);
    if let Some(adopt) = &config.adopt {
        if adopt.waited_pid != adopt.prior_host_pid
            || matches!(
                adopt.wait_outcome,
                NativeRenewalWaitOutcome::Signaled {
                    signal: 0 | 128..=255
                }
            )
        {
            return Err("fixture guardian wait evidence differs".into());
        }
        if aterm_agent::harness::native_act::Process::observe(adopt.prior_host_pid)
            .is_some_and(|process| process.birth == adopt.prior_host_birth)
        {
            return Err("fixture prior host is still alive".into());
        }
        let sid = SessionId::new(adopt.sid.clone());
        if !sid.is_minted() {
            return Err("fixture invalid retained session".into());
        }
        let nonce = LaunchNonce::from_hex(&adopt.nonce).ok_or("fixture invalid retained nonce")?;
        handle.sid = sid.clone();
        handle.nonce = nonce;
        let ctx = Arc::get_mut(&mut handle.ctx).ok_or("fixture shared too soon")?;
        ctx.self_id = sid;
        ctx.nonce = nonce;
    }
    let sid = handle.sid.as_str().to_owned();
    let socket = config.root.join("control.sock");
    let socket_text = socket.to_str().ok_or("fixture socket path")?.to_owned();
    let names = aterm_types::control_socket::token_names_for_sock("control.sock");
    let token_path = config.root.join(&names[0]);
    let token = if config.adopt.is_some() {
        std::fs::read_to_string(&token_path)
            .map_err(|e| e.to_string())?
            .trim()
            .to_owned()
    } else {
        let token = control_auth::random_token_hex().ok_or("fixture entropy")?;
        aterm_uds::atomic_file::write(&token_path, format!("{token}\n").as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        token
    };
    crate::proxy::set_self_sock(&config.root, &socket_text);
    let runtime = config.root.join("runtime");
    let graph_dir = runtime.join("aterm");
    std::fs::create_dir_all(&graph_dir).map_err(|e| e.to_string())?;
    crate::proxy::publish_graph_entry(&graph_dir, &handle.sid, &socket_text, &handle.nonce);
    let mut child_env: Vec<(String, String)> = config
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    child_env.extend([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "CODEX_AUTH_PATH".into(),
            config.auth_path.to_string_lossy().into_owned(),
        ),
        ("ATERM_SESSION_ID".into(), sid.clone()),
        ("ATERM_PARENT_SESSION_ID".into(), sid.clone()),
        (
            "XDG_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
        ("ATERM_CHILD".into(), "1".into()),
        ("TERM_PROGRAM".into(), "aterm".into()),
    ]);
    // SAFETY: an explicit Rust debug factory owns this shell, PTY and isolated root.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let shell_args = ["-f".into(), "-i".into()];
    let (master, shell_pid, shell_identity) = if let Some(adopt) = &config.adopt {
        let identity = aterm_pty::record_adopted_shell(adopt.shell_pid, adopt.master);
        if !identity.verify() {
            return Err("fixture retained PTY identity changed".into());
        }
        (adopt.master, adopt.shell_pid, identity)
    } else {
        let shell = aterm_pty::spawn_shell_with_pid(
            40,
            140,
            &spawn,
            &sandbox,
            &child_env,
            Some("/bin/zsh"),
            Some(&shell_args),
            None,
            None,
            config.cwd.to_str(),
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .map_err(|e| e.to_string())?;
        (
            shell.master,
            shell.pid,
            aterm_pty::record_spawned_shell(shell.pid),
        )
    };
    let pty = FixturePty {
        master,
        shell: shell_identity,
    };
    handle.master = master;
    handle.shell_identity = shell_identity;
    let image =
        aterm_agent::harness::native_act::Image::at(&config.codex).ok_or("fixture codex image")?;
    // The fixture's catalog is grok-alab's row alone, the one its captured image stands for.
    let grok = aterm_agent::harness::contract::row_named("grok-alab").ok_or("grok-alab row")?;
    let policy = crate::native_session_policy::FixturePolicy::new(
        std::slice::from_ref(grok),
        vec![crate::native_session_policy::FixtureImages {
            row: "grok-alab",
            images: vec![image],
        }],
        aterm_agent::supervise::SupervisorConfig {
            accounts: if config.weekly_quota {
                aterm_agent::harness::accounts::Mode::Early
            } else {
                aterm_agent::supervise::SupervisorConfig::default().accounts
            },
            continue_policy: config.continue_policy.unwrap_or(false),
            human_grace_s: config.human_grace_s.unwrap_or(0),
            ..Default::default()
        },
        &[aterm_agent::harness::native_act::Constructor::PrivateRestart],
    )
    .map_err(str::to_owned)?
    .with_renewal_fault_root(config.root.clone());
    let policy = if config.weekly_quota {
        let root = config.root.join("fixture-ca.der");
        let metadata = std::fs::symlink_metadata(&root).map_err(|_| "fixture TLS root missing")?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Err("fixture TLS root invalid".into());
        }
        let der = std::fs::read(root).map_err(|_| "fixture TLS root unreadable")?;
        policy.with_quota_rotation().with_quota_roots(vec![der])
    } else {
        policy
    };
    let ctx = Arc::get_mut(&mut handle.ctx).ok_or("fixture shared too soon")?;
    ctx.sink = Arc::new(aterm_session::sink::SinkWriter::new(master));
    ctx.install_fixture_policy(policy)
        .map_err(|_| "fixture policy")?;
    let ctx = Arc::clone(&handle.ctx);
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle.clone());
    let active = Arc::new(Mutex::new(Some(ActiveSession {
        term: Arc::clone(&term),
        master,
        id: handle.local_id,
        ctx: Arc::clone(&ctx),
    })));
    if config.adopt.is_some() && socket.exists() {
        std::fs::remove_file(&socket).map_err(|e| e.to_string())?;
    }
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).map_err(|e| e.to_string())?;
    let context = Arc::new(ControlWorkerContext {
        active,
        store,
        proxy: event_loop.create_proxy(),
        account_services: AccountServices {
            source_registry: Arc::new(Default::default()),
            lifetime: Default::default(),
            interactive_browser: false,
        },
        token: Arc::new(token),
        sock_dir: config.root.clone(),
        recovery_count_admission: Arc::new(
            crate::keeper_link::RecoveryCountAdmission::from_retained(Vec::new()),
        ),
    });
    let stopping = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stopping);
    let server_context = Arc::clone(&context);
    let server = std::thread::spawn(move || {
        let mut workers: Vec<(CtlStream, std::thread::JoinHandle<io::Result<bool>>)> = Vec::new();
        while !server_stop.load(Ordering::Acquire) {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].1.is_finished() {
                    let (_, worker) = workers.swap_remove(index);
                    let _ = worker.join();
                } else {
                    index += 1;
                }
            }
            match aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                Ok((stream, _)) => {
                    let retained = stream.try_clone()?;
                    let context = Arc::clone(&server_context);
                    let worker =
                        std::thread::spawn(move || serve_connection_mode(&context, stream, true));
                    workers.push((retained, worker));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => return Err(e),
            }
        }
        stop_fixture_connections(workers.iter().map(|(stream, _)| stream));
        for (_, worker) in workers {
            let _ = worker.join();
        }
        Ok(())
    });
    let reader_stop = Arc::clone(&stopping);
    let reader_term = Arc::clone(&term);
    let reader_ctx = Arc::clone(&ctx);
    let output = config.root.join("pty.log");
    let reader = std::thread::spawn(move || {
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output)?;
        let mut bytes = [0u8; 65536];
        while !reader_stop.load(Ordering::Acquire) {
            // SAFETY: this reader owns the live fixture PTY descriptor and output buffer.
            let n = unsafe { libc::read(master, bytes.as_mut_ptr().cast(), bytes.len()) };
            if n <= 0 {
                break;
            }
            let chunk = &bytes[..n as usize];
            log.write_all(chunk)?;
            log.flush()?;
            let mut term = reader_term.lock().unwrap_or_else(|p| p.into_inner());
            term.process(chunk);
            reader_ctx
                .frame
                .publish_text(Arc::new(crate::frame_memo::ScreenText::of(&term)));
        }
        Ok::<_, io::Error>(())
    });
    let quote = |word: &str| format!("'{}'", word.replace('\'', "'\\''"));
    let line = std::iter::once(config.codex.to_string_lossy().into_owned())
        .chain(config.args.clone())
        .map(|word| quote(&word))
        .collect::<Vec<_>>()
        .join(" ")
        + if config.args.first().is_some_and(|arg| arg == "exec") {
            "; printf '\\nEXEC_FIXTURE_EXIT=%s\\n' \"$?\"\n"
        } else {
            "\n"
        };
    if config.adopt.is_none() {
        aterm_pty::write_all(master, line.as_bytes());
    }
    let launched = Instant::now();
    let child_pid = loop {
        let foreground = crate::quit_safety::foreground_pgrp(master);
        if foreground > 1 && foreground != shell_pid {
            break Some(foreground);
        }
        if launched.elapsed() >= MUST_HAPPEN {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if config.adopt.is_none() && child_pid.is_some() {
        // This facade launched the actual prompt above, just as a managed worker
        // launch creates its task. The owner Stop uses the production task check.
        crate::task_record::mark_session_task_within(
            &ctx,
            handle.local_id,
            &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
            ctx.fabric.fence_wait(),
        )
        .map_err(|_| "fixture initial task mark fenced")?;
    }
    let host = aterm_agent::harness::native_act::Process::observe(std::process::id())
        .ok_or("fixture host identity unavailable")?;
    if let Some(guardian) = &config.guardian_socket {
        if !guardian.starts_with(&config.root) {
            return Err("fixture guardian outside root".into());
        }
        use std::os::fd::BorrowedFd;
        let stream = CtlStream::connect(guardian).map_err(|e| e.to_string())?;
        let payload =
            aterm_json::to_vec(&aterm_json::json!({"sid":sid,"nonce":handle.nonce.to_hex(),
            "shell_pid":shell_pid,"prior_host_pid":host.pid,"prior_host_birth":host.birth}))
            .map_err(|e| e.to_string())?;
        // SAFETY: the live fixture retains this master throughout sendmsg; SCM_RIGHTS duplicates it.
        let descriptor = unsafe { BorrowedFd::borrow_raw(master) };
        if aterm_uds::fdpass::send_with_fds(&stream, &payload, &[descriptor])
            .map_err(|e| e.to_string())?
            != payload.len()
        {
            return Err("fixture guardian short transfer".into());
        }
    }
    println!(
        "{}",
        aterm_json::json!({"socket":socket_text,"sid":sid,"shell_pid":shell_pid,"child_pid":child_pid,
        "host_pid":host.pid,"host_birth":host.birth,"nonce":handle.nonce.to_hex(),"root":config.root,"token_path":config.root.join(&names[0])})
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut pump = Pump {
        ctx: Arc::clone(&ctx),
        term,
        handle: handle.clone(),
        unhandled_wakes: Default::default(),
    };
    let opts =
        crate::harness_host::window_drive_opts(home.clone(), env.state.clone(), Some(socket_text));
    let live_reader =
        match live_snapshot::Reader::start(Arc::clone(&ctx), master, opts, config.root.clone()) {
            Ok(reader) => reader,
            Err(why) => {
                stopping.store(true, Ordering::Release);
                drop(pty);
                let _ = reader.join();
                let _ = server.join();
                return Err(why);
            }
        };
    let run_result = (|| -> Result<(), String> {
        while !config.root.join("stop").exists() {
            event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut pump);
            let native_observation = config.root.join("observe-native-operation.json");
            if native_observation.exists()
                && !config.root.join("native-operation-observed.json").exists()
            {
                let request: aterm_json::Value = aterm_json::from_slice(
                    &std::fs::read(&native_observation).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let id = request["operation"]
                    .as_str()
                    .ok_or("fixture operation observation ID")?;
                if let Some(observed) = crate::native_act::fixture_observe_operation(&ctx, id) {
                    aterm_uds::atomic_file::write(
                        &config.root.join("native-operation-observed.json"),
                        &aterm_json::to_vec(&observed).map_err(|e| e.to_string())?,
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            if config.root.join("person-gesture").exists()
                && !config.root.join("person-gesture-completed.json").exists()
            {
                let before = ctx.human_input.seq();
                let before_monotonic = aterm_agent::harness::native_act::monotonic_us();
                note_fixture_person(
                    &ctx,
                    &crate::input::InputEvent::KeySequence(vec![b'x']),
                    crate::input::Source::Human,
                );
                let event = aterm_json::json!({"before":before,"after":ctx.human_input.seq(),
                "beforeMonotonicUs":before_monotonic,"humanAgeMs":ctx.human_input.ms_since(crate::metrics::now_us()),
                "monotonicUs":aterm_agent::harness::native_act::monotonic_us()});
                aterm_uds::atomic_file::write(
                    &config.root.join("person-gesture-completed.json"),
                    &aterm_json::to_vec(&event).map_err(|e| e.to_string())?,
                    0o600,
                )
                .map_err(|e| e.to_string())?;
            }
            if config.root.join("owner-stop").exists()
                && !config.root.join("owner-stop-completed.json").exists()
            {
                let operation = crate::native_act::fixture_active_signalled_operation(&ctx);
                let before = operation
                    .as_deref()
                    .and_then(|id| crate::native_act::fixture_operation_state(&ctx, id, false));
                // An explicit fixture owner gesture, never a control-client signal
                // or a forged native receipt. Use the same person clock and confirmed
                // Stop Worker operation as the UI, on this facade's actual session.
                note_fixture_person(
                    &ctx,
                    &crate::input::InputEvent::KeySequence(vec![b'x']),
                    crate::input::Source::Human,
                );
                let outcome =
                    crate::app_overview::stop_worker_in(master, &ctx, aterm_slog::Actor::Human);
                let after = operation
                    .as_deref()
                    .and_then(|id| crate::native_act::fixture_operation_state(&ctx, id, false));
                let event = match outcome {
                    Ok(reply) => {
                        aterm_json::json!({"ok":true,"reply":reply,"operation":operation,"before":before,"after":after})
                    }
                    Err(refusal) => {
                        aterm_json::json!({"ok":false,"refusal":refusal.words(),"operation":operation,"before":before,"after":after})
                    }
                };
                aterm_uds::atomic_file::write(
                    &config.root.join("owner-stop-completed.json"),
                    &aterm_json::to_vec(&event).map_err(|e| e.to_string())?,
                    0o600,
                )
                .map_err(|e| e.to_string())?;
            }
            if config.root.join("renew").exists()
                && !config.root.join("renewed").exists()
                && let Some(next) = &config.successor_auth_path
            {
                if config.weekly_quota {
                    aterm_uds::atomic_file::write(
                        &config.root.join("renewed"),
                        b"quota-probe-open\n",
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                    continue;
                }
                let next_home = next.parent().ok_or("fixture successor home")?;
                let identity = aterm_agent::harness::codex_app_login::renewal_identity(old_home)
                    .ok_or("fixture old identity")?;
                if aterm_agent::harness::codex_app_login::renewal_identity(next_home).as_ref()
                    != Some(&identity)
                {
                    return Err("fixture successor identity differs".into());
                }
                let accounts = env
                    .accounts_store_for(aterm_agent::harness::accounts::Cli::Codex)
                    .ok_or("fixture account store")?;
                let _signin = accounts
                    .signin_within()
                    .map_err(|e| e.to_string())?
                    .ok_or("fixture sign-in busy")?;
                accounts.transact_logins(env.now, &mut None, |state, logins| {
                    Ok(aterm_agent::harness::accounts::note_renewed(
                        state,
                        logins,
                        "main",
                        old_home.to_str(),
                        next_home.to_str().ok_or("fixture successor text")?,
                        Some("pro"),
                        env.now,
                    ))
                })?;
                aterm_uds::atomic_file::write(&config.root.join("renewed"), b"published\n", 0o600)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })();
    let live_reader_result = live_reader.shutdown(|| {
        event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut pump);
    });
    stopping.store(true, Ordering::Release);
    drop(pty);
    let _ = reader.join();
    let _ = server.join();
    run_result?;
    live_reader_result?;
    Ok(())
}

fn stop_fixture_connections<'a>(connections: impl IntoIterator<Item = &'a CtlStream>) {
    for stream in connections {
        aterm_uds::hangup::hang_up(stream);
    }
}

#[cfg(test)]
#[test]
fn codex_renewal_fixture_stop_closes_kept_open_subscription() {
    use std::io::Read as _;
    let (mut client, mut server) = CtlStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let retained = server.try_clone().unwrap();
    let (finished, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let active = Arc::new(Mutex::new(None));
        let store = crate::session_store::new_store();
        super::run_subscribe_socket(
            aterm_types::control_request::head("subscribe @* sessions"),
            &active,
            &store,
            Scope::Owner,
            &mut server,
        );
        finished.send(()).unwrap();
    });
    let mut initial = [0; b"OK subscribe 0\n".len()];
    client.read_exact(&mut initial).unwrap();
    assert_eq!(&initial, b"OK subscribe 0\n");
    assert!(done.try_recv().is_err(), "subscription remains live");
    stop_fixture_connections(&[retained]);
    done.recv_timeout(Duration::from_secs(60)).unwrap();
    worker.join().unwrap();
    drop(client);
}

fn status_observation(result: &Result<String, String>) -> aterm_json::Value {
    let text = result.as_ref().map(String::as_str).unwrap_or("");
    let field = |key: &str| {
        text.split_whitespace().find_map(|word| {
            word.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value)
        })
    };
    let number = |key| field(key).and_then(|value| value.parse::<u64>().ok());
    let holder = field("supervisor").and_then(|value| {
        if value == "-" {
            None
        } else if crate::self_claim::holder_pid(value).is_some() {
            Some(value)
        } else {
            Some("other")
        }
    });
    let scope = field("supervisor_scope").filter(|value| {
        *value == "-"
            || value.split(',').all(|word| {
                aterm_agent::harness::contract::Scope::ALL
                    .into_iter()
                    .any(|scope| scope.word() == word)
            })
    });
    aterm_json::json!({"kind":"session-status","outcome":if result.is_ok(){"ok"}else{"error"},
        "holder":holder,"proven":field("supervisor_proven")==Some("1"),
        "contract":number("supervisor_contract"),"scope":scope,
        "revision":number("supervisor_revision"),"hold":field("hold")==Some("1"),
        "hand":if field("hand")==Some("-"){"none"}else{"present"},
        "handPid":number("hand_pid"),"humanSeq":number("human_seq"),
        "monotonicUs":aterm_agent::harness::native_act::monotonic_us()})
}

#[cfg(test)]
#[test]
fn codex_renewal_completed_status_trace_copies_only_whitelisted_fields() {
    let status = "supervisor=codex-alab@4242 supervisor_proven=1 supervisor_contract=3 \
        supervisor_scope=relaunch,upgrade supervisor_revision=9 hold=0 hand=- hand_pid=- \
        human_seq=7 native_cap=do-not-copy token=do-not-copy command=do-not-copy";
    let observed = status_observation(&Ok(status.into()));
    assert_eq!(observed["holder"].as_str(), Some("codex-alab@4242"));
    assert_eq!(observed["revision"].as_u64(), Some(9));
    assert_eq!(observed["humanSeq"].as_u64(), Some(7));
    assert_eq!(observed["scope"].as_str(), Some("relaunch,upgrade"));
    assert!(!observed.to_string().contains("do-not-copy"));
    let other = status_observation(&Ok(
        "supervisor=arbitrary-private-value supervisor_scope=arbitrary-private-value".into(),
    ));
    assert!(!other.to_string().contains("arbitrary-private-value"));
    let error = status_observation(&Err("private error body".into()));
    assert!(!error.to_string().contains("private error body"));
}
