// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An owned observer retains an absent peer across restart, then sees its return.

use super::{HANG, Node, contains, fail, save};
use crate::instance::{Instance, Reply, World};
use crate::{Fail, stop};
use astream_wire::sync::Digest;
use aterm_db::accounts::proposals::{Proposals, Snapshot};
use aterm_json::{Value, json};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkObservation, Reachability, ServiceAvailability,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod activation;
pub(super) mod gui;
mod history;
mod lens;
mod reopen;

pub(super) fn bundle_for_details(executable: &Path) -> Result<PathBuf, Fail> {
    reopen::bundle(executable)
        .map(|bundle| bundle.path)
        .map_err(fail)
}

struct StageEvidence {
    network: NetworkObservation,
    account_observed_at_ms: u64,
    secret_observed_at_ms: u64,
}

struct GuiRead {
    reply: Reply,
    projection: Result<gui::Projection, String>,
}

impl GuiRead {
    fn save(&self, out: &Path, name: &str, matches: bool) -> Result<(), Fail> {
        save(
            out,
            name,
            &json!({
                "command":["inspect","app/v1","network"],
                "exit_code":self.reply.code,"stdout":self.reply.stdout,"stderr":self.reply.stderr,
                "matches_stage":matches,
                "parse_error":self.projection.as_ref().err(),
                "scope":"Retained GUI projection at this inspection; not an atomic image or scanout receipt",
            }),
        )
    }
}

pub(super) struct Cycle<'a> {
    pub source: &'a World,
    pub recipient: &'a World,
    pub window: Instance,
    pub port: u16,
    pub out: &'a Path,
    pub peer_details: Option<PathBuf>,
    pub accounts: &'a [(Digest, &'a str, &'a Value)],
}

#[derive(Clone, Copy)]
enum Stage {
    Before,
    Lost,
    Restored,
    Reconnected,
}

impl Stage {
    const fn name(self) -> &'static str {
        match self {
            Self::Before => "peer-cycle-before",
            Self::Lost => "peer-cycle-lost",
            Self::Restored => "peer-cycle-restored",
            Self::Reconnected => "peer-cycle-reconnected",
        }
    }
}

fn same_identity(left: &MachineIdentity, right: &MachineIdentity) -> bool {
    left.machine_id == right.machine_id && left.incarnation == right.incarnation
}

fn matches_stage(
    observed: &NetworkObservation,
    pid: u32,
    peer: &MachineIdentity,
    stage: Stage,
    previous: Option<&NetworkObservation>,
) -> bool {
    if observed.visibility != aterm_types::network_state::Visibility::Active
        || observed.history != HistoryState::Ready
        || observed.via_local_process != Some(pid)
        || observed.observed_at_ms == 0
        || observed.observer.is_none()
    {
        return false;
    }
    let Some(current) = observed
        .peers
        .iter()
        .find(|p| same_identity(&p.identity, peer))
    else {
        return false;
    };
    let expected = match stage {
        Stage::Before | Stage::Reconnected => Reachability::Reachable,
        Stage::Lost | Stage::Restored => Reachability::LastSeen,
    };
    if current.reachability != expected
        || current.first_seen_ms == 0
        || current.last_seen_ms == 0
        || current.last_seen_ms > observed.observed_at_ms
        || if matches!(stage, Stage::Restored) {
            current.last_checked_ms.is_some()
        } else {
            !current
                .last_checked_ms
                .is_some_and(|at| at != 0 && at <= observed.observed_at_ms)
        }
    {
        return false;
    }
    let Some(previous) = previous else {
        return matches!(stage, Stage::Before);
    };
    let Some(old) = previous
        .peers
        .iter()
        .find(|p| same_identity(&p.identity, peer))
    else {
        return false;
    };
    let observer_unchanged = observed
        .observer
        .as_ref()
        .zip(previous.observer.as_ref())
        .is_some_and(|(now, before)| same_identity(now, before));
    observer_unchanged
        && observed.revision > previous.revision
        && current.slot == old.slot
        && current.first_seen_ms == old.first_seen_ms
        && current.last_seen_ms >= old.last_seen_ms
        && (!matches!(stage, Stage::Restored) || current.last_seen_ms == old.last_seen_ms)
        && (!matches!(stage, Stage::Reconnected) || current.last_seen_ms > previous.observed_at_ms)
}

fn cache_matches(snapshot: &Snapshot, accounts: &[(Digest, &str, &Value)]) -> bool {
    snapshot.history_complete()
        && !accounts.is_empty()
        && accounts
            .iter()
            .all(|(operation, key, value)| contains(snapshot, *operation, key, value))
}

impl Cycle<'_> {
    fn continuing_window(&mut self, pid: u32) -> Result<(), Fail> {
        if !self.window.running() || self.window.aterm_pid() != Some(pid as i32) {
            return Err(Fail::die(
                "recipient observer process changed during peer cycle",
            ));
        }
        Ok(())
    }

    fn wait(
        &mut self,
        pid: u32,
        peer: &MachineIdentity,
        stage: Stage,
        previous: Option<&NetworkObservation>,
    ) -> Result<StageEvidence, Fail> {
        let config = self.recipient.cfg.join("aterm/aterm.toml");
        let paths = Paths::for_home(&self.recipient.home);
        let until = Instant::now() + HANG;
        let mut last_network = None;
        loop {
            stop::check()?;
            self.continuing_window(pid)?;
            if let Ok(snapshot) = Proposals::at_until(paths.clone(), until).snapshot()
                && cache_matches(&snapshot, self.accounts)
            {
                let publication = aterm_db::service_state::observe_publication(&paths, &config);
                // Network evidence is sampled after the potentially slow local service reads.
                // The service lease below includes that final network read as well.
                let observed = aterm_db::network_state::read_live_observation(&config);
                let services = publication.current_at(Instant::now(), aterm_time::unix_millis());
                if let Ok(observed) = &observed {
                    last_network = Some(aterm_db::network_state::value(observed));
                }
                if services.accounts.availability == ServiceAvailability::Cached
                    && services.secrets.availability == ServiceAvailability::Unknown
                    && let Ok(observed) = observed
                    && matches_stage(&observed, pid, peer, stage, previous)
                {
                    self.continuing_window(pid)?;
                    let selected = observed
                        .peers
                        .iter()
                        .find(|p| same_identity(&p.identity, peer))
                        .expect("stage predicate established selected peer");
                    save(
                        self.out,
                        &format!("{}.json", stage.name()),
                        &json!({
                            "network":aterm_db::network_state::value(&observed),
                            "observer_pid":pid,
                            "selected_peer_last_checked_ms":selected.last_checked_ms,
                            "account_cache":{
                                "availability":"Cached",
                                "observed_at_ms":services.accounts.observed_at_ms,
                                "revision":services.accounts.revision,
                                "retained":snapshot.retained,
                                "withheld":snapshot.withheld,
                                "expected":self.accounts.iter().map(|(operation,key,value)|json!({
                                    "operation":aterm_codec::hex::encode_lower(operation),
                                    "key":key,"value":value,
                                })).collect::<Vec<_>>(),
                                "scope":"Local catalog and exact retained synthetic values; remote currency and provider sign-in are not established",
                            },
                            "secrets_observation":{
                                "availability":"Unknown",
                                "observed_at_ms":services.secrets.observed_at_ms,
                                "scope":"The synthetic vault has no manifest entries; no credential-usability claim",
                            },
                        }),
                    )?;
                    return Ok(StageEvidence {
                        network: observed,
                        account_observed_at_ms: services.accounts.observed_at_ms,
                        secret_observed_at_ms: services.secrets.observed_at_ms,
                    });
                }
            }
            if Instant::now() >= until {
                save(
                    self.out,
                    &format!("{}-failed.json", stage.name()),
                    &json!({
                        "passed":false,"stage":stage.name(),"observer_pid":pid,
                        "last_network":last_network,
                        "reason":"Same observer, peer and retained local account data were not all established before the hang detector",
                    }),
                )?;
                return Err(Fail::die(format!(
                    "{} did not establish the same observer, peer and retained account data",
                    stage.name(),
                )));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    fn inspect_gui(&mut self, pid: u32) -> Result<GuiRead, Fail> {
        self.continuing_window(pid)?;
        let reply = self.window.ctl(&["inspect", "app/v1", "network"]);
        self.continuing_window(pid)?;
        let projection = if reply.code == 0 {
            gui::parse(&reply.stdout)
        } else {
            Err(format!("GUI network inspection failed: {}", reply.first()))
        };
        Ok(GuiRead { reply, projection })
    }

    fn wait_gui(
        &mut self,
        pid: u32,
        peer: &MachineIdentity,
        name: &str,
        evidence: &StageEvidence,
    ) -> Result<(), Fail> {
        let until = Instant::now() + HANG;
        loop {
            stop::check()?;
            let read = self.inspect_gui(pid)?;
            let matches = read.projection.as_ref().is_ok_and(|projection| {
                gui::matches(projection, evidence, pid, peer, aterm_time::unix_millis())
            });
            if matches {
                return read.save(self.out, &format!("{name}-gui-before.json"), true);
            }
            if read.projection.is_err() || Instant::now() >= until {
                read.save(self.out, &format!("{name}-gui-wait-failed.json"), false)?;
                return Err(Fail::die(format!(
                    "{name} GUI projection did not establish the backend stage and account observation",
                )));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    fn read_lens(&mut self, pid: u32) -> Result<(Reply, Result<lens::State, String>), Fail> {
        self.continuing_window(pid)?;
        let reply = self.window.ctl(&["chrome"]);
        self.continuing_window(pid)?;
        let state = if reply.code == 0 {
            lens::parse(&reply.stdout)
        } else {
            Err(format!("native lens readback failed: {}", reply.first()))
        };
        Ok((reply, state))
    }

    fn save_lens(
        &self,
        name: &str,
        reply: &Reply,
        state: &Result<lens::State, String>,
        matches: bool,
        presentation_required: bool,
    ) -> Result<(), Fail> {
        save(
            self.out,
            name,
            &json!({
                "command":["chrome"],"exit_code":reply.code,
                "stdout":reply.stdout,"stderr":reply.stderr,
                "matches_selection":matches,"presentation_required":presentation_required,"parse_error":state.as_ref().err(),
                "scope":"Retained desired and last successful own-presentation selections. Desired-only acquisition acknowledges no presentation; no network revision or image atomicity claim",
            }),
        )
    }

    fn wait_lens(
        &mut self,
        pid: u32,
        target: lens::Target,
        window: Option<u64>,
        name: &str,
        presentation_required: bool,
    ) -> Result<u64, Fail> {
        let until = Instant::now() + HANG;
        loop {
            stop::check()?;
            let (reply, state) = self.read_lens(pid)?;
            let matches = state.as_ref().is_ok_and(|state| {
                if presentation_required {
                    state.matches(target, window)
                } else {
                    state.matches_desired(target, window)
                }
            });
            if matches || state.is_err() || Instant::now() >= until {
                self.save_lens(name, &reply, &state, matches, presentation_required)?;
                return if matches {
                    Ok(state.expect("matching lens state").window)
                } else {
                    Err(Fail::die(
                        "native lens did not establish the required selection evidence",
                    ))
                };
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    fn private_session(
        &mut self,
        pid: u32,
        logical_window: u64,
        name: &str,
        expected: Option<&activation::Session>,
    ) -> Result<activation::Session, Fail> {
        self.continuing_window(pid)?;
        let reply = self.window.ctl(&["sessions"]);
        self.continuing_window(pid)?;
        let session = if reply.code == 0 {
            activation::session(&reply.stdout, logical_window)
        } else {
            Err(format!("private session read failed: {}", reply.first()))
        };
        let matches = session
            .as_ref()
            .is_ok_and(|session| expected.is_none_or(|old| old == session));
        save(
            self.out,
            name,
            &json!({
                "command":["sessions"],"exit_code":reply.code,"stdout":reply.stdout,"stderr":reply.stderr,
                "logical_window":logical_window,"matches_session":matches,"parse_error":session.as_ref().err(),
                "scope":"One live active terminal in the verified logical window; its actual SID and launch nonce identify the activation target",
            }),
        )?;
        if !matches {
            return Err(Fail::die(
                "private activation session is missing, ambiguous or changed",
            ));
        }
        session.map_err(fail)
    }

    fn key_window(
        &mut self,
        pid: u32,
        native_window: Option<u64>,
        name: &str,
        wait: bool,
    ) -> Result<u64, Fail> {
        let until = Instant::now() + HANG;
        loop {
            stop::check()?;
            self.continuing_window(pid)?;
            let reply = self.window.ctl(&["native-focus"]);
            self.continuing_window(pid)?;
            let focus = if reply.code == 0 {
                activation::focus(&reply.stdout)
            } else {
                Err(format!("native focus read failed: {}", reply.first()))
            };
            let ready = focus.as_ref().is_ok_and(|focus| focus.ready(native_window));
            if ready || !wait || focus.is_err() || Instant::now() >= until {
                save(
                    self.out,
                    name,
                    &json!({
                        "command":["native-focus"],"exit_code":reply.code,"stdout":reply.stdout,"stderr":reply.stderr,
                        "expected_appkit_window":native_window,"ready":ready,"parse_error":focus.as_ref().err(),
                        "scope":"Read-only AppKit activation and key-window observation. AppKit windowNumber is not the logical chrome WindowId, and focus is not leased",
                    }),
                )?;
                if !ready {
                    return Err(Fail::die(
                        "private native window is not the active key window",
                    ));
                }
                return Ok(focus
                    .expect("ready native focus")
                    .window
                    .expect("available native window"));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
    }

    fn system(&self, program: &str, args: &[&str], name: &str) -> Result<Reply, Fail> {
        let mut command = aterm_uds::spawnfd::command(program);
        command
            .args(args)
            .env_clear()
            .envs(self.recipient.env())
            .current_dir(&self.recipient.home)
            .stdin(aterm_uds::spawnfd::Stdio::null());
        let output = aterm_uds::deadline::output_within(&mut command, HANG);
        let reply = match output {
            Ok(output) => Reply {
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(error) => {
                save(
                    self.out,
                    name,
                    &json!({"command":std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>(),
                    "completed":false,"error":error.to_string()}),
                )?;
                return Err(fail(error));
            }
        };
        save(
            self.out,
            name,
            &json!({"command":std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>(),
            "exit_code":reply.code,"stdout":reply.stdout,"stderr":reply.stderr}),
        )?;
        Ok(reply)
    }

    fn registered_instance(
        &mut self,
        pid: u32,
        bundle: &reopen::Bundle,
        name: &str,
    ) -> Result<(), Fail> {
        let observed: Result<String, Fail> = (|| {
            self.continuing_window(pid)?;
            let query = format!("bundleid={}", bundle.id);
            let found = self.system(
                "/usr/bin/lsappinfo",
                &["find", &query],
                &format!("{name}-find.json"),
            )?;
            if found.code != 0 {
                return Err(Fail::die("registered bundle lookup failed"));
            }
            let asn = reopen::single_asn(&found.stdout).map_err(fail)?;
            let info = self.system(
                "/usr/bin/lsappinfo",
                &["info", &asn],
                &format!("{name}-info.json"),
            )?;
            if info.code != 0 {
                return Err(Fail::die("registered bundle metadata could not be read"));
            }
            reopen::registered(&info.stdout, &asn, pid, bundle).map_err(fail)?;
            self.continuing_window(pid)?;
            Ok(asn)
        })();
        save(
            self.out,
            &format!("{name}.json"),
            &json!({"passed":observed.is_ok(),"pid":pid,"bundle_id":bundle.id,
            "bundle_path":bundle.path,"executable":bundle.executable,"asn":observed.as_ref().ok(),
            "reason":observed.as_ref().err().and_then(|error|error.msg.as_deref()),
            "scope":"Exactly one registered bundle ID, matching this owned process and its full canonical bundle/executable paths at this read; no instance lease"}),
        )?;
        observed.map(|_| ())
    }

    fn owned_process(
        &mut self,
        pid: u32,
        bundle: &reopen::Bundle,
        name: &str,
        expected: Option<&activation::Process>,
    ) -> Result<activation::Process, Fail> {
        self.continuing_window(pid)?;
        let parent = u32::try_from(self.window.pid).map_err(fail)?;
        let process = activation::process(pid, parent, &bundle.executable);
        let matches = process
            .as_ref()
            .is_ok_and(|process| expected.is_none_or(|before| before == process));
        save(
            self.out,
            name,
            &json!({"passed":matches,"pid":pid,"keeper_pid":parent,"executable":bundle.executable,
            "start_s":process.as_ref().ok().map(|process|process.start_s),"start_us":process.as_ref().ok().map(|process|process.start_us),
            "uid":process.as_ref().ok().map(|process|process.uid),"reason":process.as_ref().err(),
            "scope":"Kernel birth, own keeper parent and executable observation; a later read must preserve them, not a process lease"}),
        )?;
        if !matches {
            return Err(Fail::die(
                "owned bundled process identity changed or is unavailable",
            ));
        }
        self.continuing_window(pid)?;
        process.map_err(fail)
    }

    fn appkit_window(&mut self, pid: u32, name: &str) -> Result<u64, Fail> {
        self.continuing_window(pid)?;
        let reply = self.window.ctl(&["native-focus"]);
        let focus = if reply.code == 0 {
            activation::focus(&reply.stdout)
        } else {
            Err("native window identity read failed".into())
        };
        let window = focus.as_ref().ok().and_then(|focus| focus.window);
        save(
            self.out,
            name,
            &json!({"command":["native-focus"],"exit_code":reply.code,"stdout":reply.stdout,"stderr":reply.stderr,
            "appkit_window":window,"parse_error":focus.as_ref().err(),"scope":"Original AppKit windowNumber, before activation; no active/key claim"}),
        )?;
        self.continuing_window(pid)?;
        window.ok_or_else(|| Fail::die("original AppKit window identity is unavailable"))
    }

    fn activate_lens(&mut self, pid: u32, logical_window: u64, name: &str) -> Result<u64, Fail> {
        let bundle = reopen::bundle(&self.window.aterm).map_err(fail)?;
        if self.peer_details.as_ref() != Some(&bundle.path) {
            return Err(Fail::die("peer detail bundle provenance changed"));
        }
        let process = self.owned_process(
            pid,
            &bundle,
            &format!("{name}-activation-process-before.json"),
            None,
        )?;
        let session = self.private_session(
            pid,
            logical_window,
            &format!("{name}-activation-sessions-before.json"),
            None,
        )?;
        let native_window = self.appkit_window(
            pid,
            &format!("{name}-activation-original-native-window.json"),
        )?;
        self.registered_instance(
            pid,
            &bundle,
            &format!("{name}-activation-registered-before"),
        )?;
        // One standard reopen request. If the admitted app dies now, --version exits before GUI startup.
        let path = bundle
            .path
            .to_str()
            .ok_or_else(|| Fail::die("bundle path is not UTF-8"))?;
        let opened = self.system(
            "/usr/bin/open",
            &["-a", path, "--args", "--version"],
            &format!("{name}-activation-open.json"),
        )?;
        if opened.code != 0 {
            return Err(Fail::die(format!(
                "owned bundle reopen failed: {}",
                opened.first()
            )));
        }
        self.key_window(
            pid,
            Some(native_window),
            &format!("{name}-activation-native-focus.json"),
            true,
        )?;
        self.owned_process(
            pid,
            &bundle,
            &format!("{name}-activation-process-after.json"),
            Some(&process),
        )?;
        self.registered_instance(pid, &bundle, &format!("{name}-activation-registered-after"))?;
        self.private_session(
            pid,
            logical_window,
            &format!("{name}-activation-sessions-after.json"),
            Some(&session),
        )?;
        self.wait_lens(
            pid,
            lens::Target::Overview,
            Some(logical_window),
            &format!("{name}-activation-lens.json"),
            true,
        )?;
        Ok(native_window)
    }

    fn navigate_lens(
        &mut self,
        pid: u32,
        native_window: u64,
        key: &str,
        name: &str,
    ) -> Result<(), Fail> {
        self.key_window(
            pid,
            Some(native_window),
            &format!("{name}-native-focus.json"),
            false,
        )?;
        self.continuing_window(pid)?;
        let reply = self.window.ctl(&["hwkey", key, "count=2"]);
        save(
            self.out,
            &format!("{name}.json"),
            &json!({
                "command":["hwkey",key,"count=2"],"exit_code":reply.code,
                "stdout":reply.stdout,"stderr":reply.stderr,
                "scope":"One native controller delivery request. Posting, active-key-window acknowledgement and selected view are checked separately",
            }),
        )?;
        if reply.code != 0 || !lens::two_keys_posted(&reply.stdout) {
            return Err(Fail::die(format!(
                "native peer-detail navigation refused or inactive: {}",
                reply.first()
            )));
        }
        self.continuing_window(pid)
    }

    fn capture_view(
        &mut self,
        pid: u32,
        peer: &MachineIdentity,
        name: &str,
        evidence: &StageEvidence,
        selection: Option<(lens::Target, u64)>,
    ) -> Result<(), Fail> {
        if let Some((target, window)) = selection {
            self.wait_lens(
                pid,
                target,
                Some(window),
                &format!("{name}-lens-before.json"),
                true,
            )?;
        }
        self.wait_gui(pid, peer, name, evidence)?;
        // Capture exactly once. A failed post-capture predicate retains that image and fails.
        let image = self.window.ctl(&["image", "--meta", "--bytes"]);
        save(
            self.out,
            &format!("{name}-image-reply.json"),
            &json!({
                "command":["image","--meta","--bytes"],"exit_code":image.code,
                "stdout":image.stdout,"stderr":image.stderr,
            }),
        )?;
        if image.code != 0 {
            return Err(Fail::die(format!("peer-cycle image: {}", image.first())));
        }
        let (png, metadata) = crate::trail_gallery::image_bytes(&image.stdout).map_err(fail)?;
        std::fs::write(self.out.join(format!("{name}.png")), png).map_err(fail)?;
        let after = match self.inspect_gui(pid) {
            Ok(after) => after,
            Err(error) => {
                save(
                    self.out,
                    &format!("{name}-capture.json"),
                    &json!({
                        "passed":false,"image_metadata":metadata,
                        "reason":"GUI inspection did not complete after the one retained image",
                    }),
                )?;
                return Err(error);
            }
        };
        let network_matches = after.projection.as_ref().is_ok_and(|projection| {
            gui::matches(projection, evidence, pid, peer, aterm_time::unix_millis())
        });
        after.save(self.out, &format!("{name}-gui-after.json"), network_matches)?;
        let lens_matches = if let Some((target, window)) = selection {
            let (reply, observed) = match self.read_lens(pid) {
                Ok(read) => read,
                Err(error) => {
                    save(
                        self.out,
                        &format!("{name}-capture.json"),
                        &json!({
                            "passed":false,"image_metadata":metadata,
                            "reason":"Native lens readback did not complete after the one retained image",
                        }),
                    )?;
                    return Err(error);
                }
            };
            let matches = observed
                .as_ref()
                .is_ok_and(|state| state.matches(target, Some(window)));
            self.save_lens(
                &format!("{name}-lens-after.json"),
                &reply,
                &observed,
                matches,
                true,
            )?;
            Some(matches)
        } else {
            None
        };
        let matches = network_matches && lens_matches.unwrap_or(true);
        save(
            self.out,
            &format!("{name}-capture.json"),
            &json!({
                "passed":matches,"image_metadata":metadata,"network_matches":network_matches,"selection_required":selection.is_some(),"selection_matches":lens_matches,
                "scope":"One image bracketed by matching retained GUI network projections; optional native selection evidence is recorded separately. The network inspection excludes recorded repair metadata; no atomic source/frame, presentation or scanout claim",
            }),
        )?;
        if !matches {
            return Err(Fail::die(format!(
                "{name} GUI projection or native lens changed during capture; image retained without retry",
            )));
        }
        self.continuing_window(pid)?;
        Ok(())
    }

    fn capture(
        &mut self,
        pid: u32,
        peer: &MachineIdentity,
        stage: Stage,
        evidence: &StageEvidence,
        window: Option<u64>,
    ) -> Result<Option<u64>, Fail> {
        if self.peer_details.is_some()
            && !evidence
                .network
                .peers
                .iter()
                .any(|observed| observed.slot == 1 && same_identity(&observed.identity, peer))
        {
            return Err(Fail::die(
                "private peer-cycle source is not retained in peer slot 1",
            ));
        }
        self.continuing_window(pid)?;
        let reply = self.window.ctl(&["invoke", "NetworkState"]);
        save(
            self.out,
            &format!("{}-control.json", stage.name()),
            &json!({
                "command":["invoke","NetworkState"],"exit_code":reply.code,
                "stdout":reply.stdout,"stderr":reply.stderr,
                "scope":"Opens Network State; GUI network evidence and native lens selection are checked separately",
            }),
        )?;
        if reply.code != 0 {
            return Err(Fail::die(format!(
                "peer-cycle Network State: {}",
                reply.first()
            )));
        }
        if self.peer_details.is_none() {
            self.capture_view(pid, peer, stage.name(), evidence, None)?;
            return Ok(None);
        }
        let name = format!("{}-peer-detail", stage.name());
        // Occluded windows may retain an unacknowledged selection until activation.
        let window = self.wait_lens(
            pid,
            lens::Target::Overview,
            window,
            &format!("{name}-desired-overview.json"),
            false,
        )?;
        let native_window = self.activate_lens(pid, window, &name)?;
        self.capture_view(
            pid,
            peer,
            stage.name(),
            evidence,
            Some((lens::Target::Overview, window)),
        )?;
        self.navigate_lens(pid, native_window, "down", &format!("{name}-select"))?;
        let captured = self.capture_view(
            pid,
            peer,
            &name,
            evidence,
            Some((lens::Target::Peer, window)),
        );
        // Restoration is a separate verified action, even when the one retained peer image failed.
        let restored = self
            .navigate_lens(pid, native_window, "up", &format!("{name}-restore"))
            .and_then(|()| {
                self.wait_lens(
                    pid,
                    lens::Target::Overview,
                    Some(window),
                    &format!("{name}-restored-lens.json"),
                    true,
                )
            });
        captured?;
        restored?;
        Ok(Some(window))
    }

    pub(super) fn run(mut self, node: Node) -> Result<(Node, Instance, Option<Value>), Fail> {
        let pid = self
            .window
            .aterm_pid()
            .ok_or_else(|| Fail::die("peer-cycle recipient has no app pid"))?
            as u32;
        let bootstrap = Bootstrap::required(&Paths::for_home(&self.source.home)).map_err(fail)?;
        let request = bootstrap.request().map_err(fail)?;
        let peer = MachineIdentity {
            machine_id: *request.binding().machine(),
            incarnation: *request.binding().incarnation(),
            label: None,
        };
        drop(bootstrap);
        let before = self.wait(pid, &peer, Stage::Before, None)?;
        let logical_window = self.capture(pid, &peer, Stage::Before, &before, None)?;
        let source_pid = node.0.id();
        let stopped = node.stop()?;
        save(self.out, "peer-cycle-source-stopped.json", &stopped)?;
        let lost = self.wait(pid, &peer, Stage::Lost, Some(&before.network))?;
        self.capture(pid, &peer, Stage::Lost, &lost, logical_window)?;
        let mut cycle = self.restart(pid, source_pid, &peer, &lost)?;
        let restarted_pid = cycle
            .window
            .aterm_pid()
            .ok_or_else(|| Fail::die("restored observer has no app pid"))?
            as u32;
        let restored = cycle.wait(restarted_pid, &peer, Stage::Restored, Some(&lost.network))?;
        let restored_window =
            cycle.capture(restarted_pid, &peer, Stage::Restored, &restored, None)?;
        let node = Node::start(
            cycle.source,
            &cycle.window.aterm,
            cycle.port,
            "node-peer-cycle.log",
        )?;
        super::active(
            cycle.source,
            node.0.id(),
            cycle.out,
            "source-peer-cycle-active.json",
        )?;
        let reconnected = cycle.wait(
            restarted_pid,
            &peer,
            Stage::Reconnected,
            Some(&restored.network),
        )?;
        cycle.capture(
            restarted_pid,
            &peer,
            Stage::Reconnected,
            &reconnected,
            restored_window,
        )?;
        let source_pid = node.0.id();
        Ok((
            node,
            cycle.window,
            Some(json!({
                "observer_pid":pid,"same_observer_process":false,"logical_window":logical_window,
                "restarted_observer_pid":restarted_pid,"restored_logical_window":restored_window,
                "peer_details":cycle.peer_details.is_some(),
                "peer_machine":aterm_codec::hex::encode_lower(&peer.machine_id),
                "peer_incarnation":aterm_codec::hex::encode_lower(&peer.incarnation),
                "before_revision":before.network.revision,"lost_revision":lost.network.revision,
                "restored_revision":restored.network.revision,
                "reconnected_revision":reconnected.network.revision,
                "source_stopped":stopped,"source_restarted_pid":source_pid,
                "scope":"Authenticated contact loss, absent-peer history restoration across observer restart, and reconnect with retained local synthetic accounts; no provider-login or global-currency claim",
            })),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astream_wire::sync::Header;
    use aterm_db::{
        accounts::proposals::Operation,
        database::{Candidate, Cell},
        table::{Scope, store::Receipt},
    };
    use aterm_secrets::commons::journal::Trust;
    use aterm_types::network_state::{PeerObservation, Visibility};

    fn identity(n: u8) -> MachineIdentity {
        MachineIdentity {
            machine_id: [n; 16],
            incarnation: [n; 16],
            label: None,
        }
    }

    pub(super) fn observation() -> NetworkObservation {
        NetworkObservation {
            revision: 10,
            observed_at_ms: 100_000,
            via_local_process: Some(42),
            observer: Some(identity(1)),
            visibility: Visibility::Active,
            history: HistoryState::Ready,
            peers: vec![PeerObservation {
                slot: 7,
                identity: identity(2),
                first_seen_ms: 90_000,
                last_seen_ms: 99_000,
                last_checked_ms: Some(99_000),
                last_authenticated_endpoint: None,
                last_repair: None,
                reachability: Reachability::Reachable,
                instances: None,
                samples: Vec::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn peer_cycle_requires_a_continuing_observer_and_fresh_contact_after_loss() {
        let before = observation();
        let peer = identity(2);
        assert!(matches_stage(&before, 42, &peer, Stage::Before, None));
        let mut lost = before.clone();
        lost.revision += 1;
        lost.observed_at_ms = 116_000;
        lost.peers[0].reachability = Reachability::LastSeen;
        assert!(matches_stage(&lost, 42, &peer, Stage::Lost, Some(&before)));
        let mut reconnected = lost.clone();
        reconnected.revision += 1;
        reconnected.observed_at_ms = 117_000;
        reconnected.peers[0].reachability = Reachability::Reachable;
        // A replay with a newer publication revision cannot prove a new exchange.
        assert!(!matches_stage(
            &reconnected,
            42,
            &peer,
            Stage::Reconnected,
            Some(&lost)
        ));
        reconnected.peers[0].last_seen_ms = 116_500;
        reconnected.peers[0].last_checked_ms = Some(116_500);
        assert!(matches_stage(
            &reconnected,
            42,
            &peer,
            Stage::Reconnected,
            Some(&lost)
        ));
        for field in 0..7 {
            let mut wrong = reconnected.clone();
            match field {
                0 => wrong.via_local_process = Some(43),
                1 => wrong.observer = Some(identity(3)),
                2 => wrong.peers.clear(),
                3 => wrong.peers[0].identity.machine_id = [3; 16],
                4 => wrong.peers[0].identity.incarnation = [3; 16],
                5 => wrong.peers[0].slot += 1,
                _ => wrong.peers[0].first_seen_ms += 1,
            }
            assert!(!matches_stage(
                &wrong,
                42,
                &peer,
                Stage::Reconnected,
                Some(&lost)
            ));
        }
        for visibility in [Visibility::Offline, Visibility::Unavailable] {
            let mut unknown = lost.clone();
            unknown.visibility = visibility;
            unknown.peers[0].reachability = Reachability::Unknown;
            assert!(!matches_stage(
                &unknown,
                42,
                &peer,
                Stage::Lost,
                Some(&before)
            ));
        }
    }

    #[test]
    fn restarted_observer_must_retain_absent_peer_without_current_lifetime_contact() {
        let mut lost = observation();
        lost.peers[0].reachability = Reachability::LastSeen;
        let peer = &lost.peers[0].identity;
        let mut restored = lost.clone();
        restored.revision += 1;
        restored.observed_at_ms += 1_000;
        restored.via_local_process = Some(43);
        restored.peers[0].last_checked_ms = None;
        assert!(matches_stage(
            &restored,
            43,
            peer,
            Stage::Restored,
            Some(&lost)
        ));
        assert!(!matches_stage(
            &restored,
            42,
            peer,
            Stage::Restored,
            Some(&lost)
        ));
        for field in 0..9 {
            let mut wrong = restored.clone();
            match field {
                0 => wrong.history = HistoryState::Unavailable,
                1 => wrong.observer = Some(identity(3)),
                2 => wrong.peers.clear(),
                3 => wrong.peers[0].slot += 1,
                4 => wrong.peers[0].first_seen_ms += 1,
                5 => wrong.peers[0].last_seen_ms += 1,
                6 => wrong.peers[0].last_checked_ms = Some(100_500),
                7 => wrong.peers[0].reachability = Reachability::Reachable,
                _ => wrong.revision = lost.revision,
            }
            assert!(
                !matches_stage(&wrong, 43, peer, Stage::Restored, Some(&lost)),
                "field {field}"
            );
        }
        let mut reconnected = restored.clone();
        reconnected.revision += 1;
        reconnected.observed_at_ms += 1_000;
        reconnected.peers[0].reachability = Reachability::Reachable;
        reconnected.peers[0].last_checked_ms = Some(reconnected.observed_at_ms);
        assert!(!matches_stage(
            &reconnected,
            43,
            peer,
            Stage::Reconnected,
            Some(&restored)
        ));
        reconnected.peers[0].last_seen_ms = reconnected.observed_at_ms;
        assert!(matches_stage(
            &reconnected,
            43,
            peer,
            Stage::Reconnected,
            Some(&restored)
        ));
    }

    #[test]
    fn cached_availability_cannot_replace_an_exact_eligible_account_value() {
        let id = [1; 32];
        let value = json!({"provider":"example","method":"oauth","label":"synthetic"});
        let expected = [(id, "one", &value)];
        let mut snapshot = Snapshot {
            scope: Scope::new(
                Trust {
                    network: [1; 16],
                    authority: [2; 32],
                    genesis: [3; 32],
                },
                [4; 16],
                1,
            )
            .unwrap(),
            retained: 1,
            withheld: 0,
            cells: [(
                ("accounts".into(), "one".into()),
                Cell {
                    stopped: false,
                    candidates: vec![Candidate {
                        operation: id,
                        value: value.clone(),
                    }],
                },
            )]
            .into(),
            operations: vec![Operation {
                header: Header {
                    id,
                    actor: [5; 48],
                    sequence: 1,
                    previous: None,
                    parents: Vec::new(),
                },
                roster: [6; 32],
                epoch: 1,
                authored_ms: 10,
                receipt: Receipt {
                    id,
                    received_ms: 10,
                    offset: 1,
                },
                request: None,
                reverts: None,
                available: true,
                catalog_eligible: true,
                eligible: true,
                duplicate_of: None,
                refusal: None,
            }],
        };
        assert!(cache_matches(&snapshot, &expected));
        let wrong = json!({"provider":"example","method":"oauth","label":"different"});
        assert!(!cache_matches(&snapshot, &[(id, "one", &wrong)]));
        assert!(!cache_matches(&snapshot, &[([9; 32], "one", &value)]));
        for field in 0..4 {
            let mut missing = snapshot.clone();
            match field {
                0 => missing.cells.clear(),
                1 => missing.operations.clear(),
                2 => missing.operations[0].eligible = false,
                _ => missing.withheld = 1,
            }
            assert!(!cache_matches(&missing, &expected));
        }
        snapshot.operations[0].available = false;
        assert!(!cache_matches(&snapshot, &expected));
    }
}
