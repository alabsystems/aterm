// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::{Fail, Res, codex_alab_proof, codex_rig, instance, paths};
use aterm_agent::harness::{
    codex_alab, codex_enrollment::NativeSourceProofWorld,
    codex_source_registry::SOURCE_PROOF_OBSERVATION_LIMIT, native_act::Process,
};
use aterm_uds::spawnfd::{Child, Stdio, Tether};
use aterm_uds::{CtlListener, CtlStream};
use serde::Deserialize;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

const SDK_REPLY_LIMIT: usize = 4096;

const MUST_HAPPEN: Duration = Duration::from_secs(60);

fn publish_task(root: &std::path::Path, task: &str) -> std::io::Result<()> {
    if aterm_uds::private_file::write_private_new(
        root,
        "source-proof.task",
        task.as_bytes(),
        |_| Ok(()),
    )? {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "source proof task already exists",
        ))
    }
}

fn line(input: &mut impl Read, fd: i32, limit: usize) -> Result<Vec<u8>, Fail> {
    let until = Instant::now() + MUST_HAPPEN;
    let mut bytes = Vec::new();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Fail::die("source proof reply hang detector expired"));
        }
        let mut fds = [crate::sys::pollfd(fd)];
        let ready = crate::sys::poll(&mut fds, aterm_uds::deadline::poll_timeout_ms(left))
            .map_err(|_| Fail::die("source proof reply poll failed"))?;
        if ready == 0 {
            continue;
        }
        let mut byte = [0u8; 1];
        if input
            .read(&mut byte)
            .map_err(|_| Fail::die("source proof reply read failed"))?
            != 1
        {
            return Err(Fail::die("source proof reply closed early"));
        }
        if byte[0] == b'\n' {
            return Ok(bytes);
        }
        bytes.push(byte[0]);
        if bytes.len() > limit {
            return Err(Fail::die("source proof reply too large"));
        }
    }
}

fn nonce() -> Result<String, Fail> {
    aterm_uds::rand::hex_token::<16>().map_err(|_| Fail::die("source proof entropy unavailable"))
}

fn request(output: &mut impl Write, action: &str) -> Result<String, Fail> {
    let request = nonce()?;
    let mut bytes =
        aterm_json::to_vec(&aterm_json::json!({"schema":1,"request":request,"action":action}))
            .map_err(|_| Fail::die("source proof request encoding failed"))?;
    bytes.push(b'\n');
    output
        .write_all(&bytes)
        .map_err(|_| Fail::die("source proof request write failed"))?;
    output
        .flush()
        .map_err(|_| Fail::die("source proof request flush failed"))?;
    Ok(request)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    auth: u64,
    owner: u64,
    source: u64,
    login: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SdkReply {
    schema: u32,
    request: String,
    action: String,
    outcome: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(rename = "authState", default)]
    auth_state: Option<State>,
}
fn sdk_step(input: &mut File, output: &mut File, action: &str) -> Result<(), Fail> {
    let expected = request(input, action)?;
    let fd = output.as_raw_fd();
    let bytes = line(output, fd, SDK_REPLY_LIMIT)?;
    let reply: SdkReply =
        aterm_json::from_slice(&bytes).map_err(|_| Fail::die("invalid SDK source proof reply"))?;
    if reply.schema != 1
        || reply.request != expected
        || reply.action != action
        || reply.outcome != "ok"
        || reply.reason.is_some()
    {
        return Err(Fail::die(format!(
            "SDK source proof step {action} refused or mismatched"
        )));
    }
    if let Some(state) = reply.auth_state {
        println!(
            "STEP {action} actual auth={} owner={} source={} login={}",
            state.auth, state.owner, state.source, state.login
        );
    }
    Ok(())
}

struct Guardian {
    stream: CtlStream,
    prefix: String,
    counter: u64,
}
fn native(guardian: &mut Guardian, action: &str) -> Result<aterm_json::Value, Fail> {
    native_command(guardian, action, None)
}
fn native_command(
    guardian: &mut Guardian,
    action: &str,
    ready_loss: Option<(&str, u64)>,
) -> Result<aterm_json::Value, Fail> {
    guardian.counter = guardian
        .counter
        .checked_add(1)
        .ok_or_else(|| Fail::die("source proof request counter exhausted"))?;
    let expected = format!("{}{:016x}", guardian.prefix, guardian.counter);
    let body = if let Some((member, generation)) = ready_loss {
        aterm_json::json!({"schema":1,"request":expected,"action":action,
            "member":member,"credential_generation":generation})
    } else {
        aterm_json::json!({"schema":1,"request":expected,"action":action})
    };
    let mut command =
        aterm_json::to_vec(&body).map_err(|_| Fail::die("source proof request encoding failed"))?;
    command.push(b'\n');
    guardian
        .stream
        .write_all(&command)
        .map_err(|_| Fail::die("source proof request write failed"))?;
    let fd = guardian.stream.as_raw_fd();
    let bytes = line(&mut guardian.stream, fd, SOURCE_PROOF_OBSERVATION_LIMIT)?;
    let value: aterm_json::Value = aterm_json::from_slice(&bytes)
        .map_err(|_| Fail::die("invalid native proof observation"))?;
    if value["schema"].as_u64() != Some(1)
        || value["request"].as_str() != Some(expected.as_str())
        || value["action"].as_str() != Some(action)
    {
        return Err(Fail::die("native proof observation correlation changed"));
    }
    Ok(value)
}

fn await_native(guardian: &mut Guardian, generation: u64) -> Result<(), Fail> {
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let observed = native(guardian, "observe")?;
        if observed["outcome"].as_str() == Some("ok")
            && observed["observation"]["maximum_credential_generation"]
                .as_u64()
                .is_some_and(|actual| actual >= generation)
            && observed["observation"]["unresolved"].as_u64() == Some(0)
            && observed["observation"]["managers_ready"]
                .as_u64()
                .is_some_and(|ready| ready > 0)
        {
            return Ok(());
        }
        if Instant::now() >= until {
            // Keep the actual failing census without exporting identifiers,
            // source documents or reply payloads. Missing counters stay missing.
            let census = &observed["observation"];
            return Err(Fail::die(format!(
                "native Source readiness was not observed: native_ok={} issued={:?} members={:?} managers_ready={:?} readers_ready={:?} unresolved={:?} unsettled_custody={:?} maximum_credential_generation={:?}",
                observed["outcome"].as_str() == Some("ok"),
                census["issued"].as_u64(),
                census["members"].as_u64(),
                census["managers_ready"].as_u64(),
                census["readers_ready"].as_u64(),
                census["unresolved"].as_u64(),
                census["unsettled_custody"].as_u64(),
                census["maximum_credential_generation"].as_u64(),
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct ActivityProof {
    member: String,
    nonce: String,
}
fn activity_current(state: &aterm_json::Value, activity: &ActivityProof, derived: bool) -> bool {
    let observation = &state["observation"];
    let active = observation["uses"].as_array().is_some_and(|uses| {
        uses.iter().any(|entry| {
            entry["member"].as_str() == Some(activity.member.as_str())
                && entry["nonce"].as_str() == Some(activity.nonce.as_str())
                && entry["manager"].as_bool() == Some(true)
        })
    });
    let child = !derived
        || observation["derived"].as_array().is_some_and(|links| {
            links.iter().any(|link| {
                link["parent"].as_str() == Some(activity.member.as_str())
                    && link["nonce"].as_str() == Some(activity.nonce.as_str())
                    && link["child"].as_str() != Some(activity.member.as_str())
                    && observation["ready_members"]
                        .as_array()
                        .is_some_and(|members| {
                            members
                                .iter()
                                .any(|member| member["member"] == link["child"])
                        })
            })
        });
    let reader = observation["derived"].as_array().is_some_and(|links| {
        links.iter().any(|link| {
            link["parent"].as_str() == Some(activity.member.as_str())
                && link["nonce"].as_str() == Some(activity.nonce.as_str())
                && observation["loaded_readers"]
                    .as_array()
                    .is_some_and(|readers| readers.iter().any(|reader| reader == &link["child"]))
        })
    });
    active && child && reader
}
fn capture_activity(guardian: &mut Guardian, primary: &str) -> Result<ActivityProof, Fail> {
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let state = native(guardian, "observe")?;
        if let Some(uses) = state["observation"]["uses"].as_array() {
            for entry in uses {
                if entry["member"].as_str() == Some(primary)
                    && entry["manager"].as_bool() == Some(true)
                    && let Some(nonce) = entry["nonce"].as_str()
                {
                    let proof = ActivityProof {
                        member: primary.to_owned(),
                        nonce: nonce.to_owned(),
                    };
                    if activity_current(&state, &proof, false) {
                        return Ok(proof);
                    }
                }
            }
        }
        if Instant::now() >= until {
            return Err(Fail::die(
                "native primary Activity/direct reader not observed",
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
fn require_derived(guardian: &mut Guardian, activity: &ActivityProof) -> Result<(), Fail> {
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let state = native(guardian, "observe")?;
        if activity_current(&state, activity, true) {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(Fail::die(
                "native accepted derived-parent relationship not observed",
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn same_ready_body(left: &aterm_json::Value, right: &aterm_json::Value) -> bool {
    [
        "operation",
        "member",
        "preload_sequence",
        "registration",
        "credential_generation",
        "report",
    ]
    .into_iter()
    .all(|field| left[field] == right[field])
}
fn ready_loss_recovered(state: &aterm_json::Value, baseline: u64) -> bool {
    let lost = &state["readyLoss"]["fired"];
    let Some(lost_sequence) = lost["sequence"]
        .as_u64()
        .filter(|sequence| *sequence > baseline)
    else {
        return false;
    };
    let Some(lost_connection) = lost["connection"].as_u64() else {
        return false;
    };
    let Some(events) = state["outcomes"].as_array() else {
        return false;
    };
    let Some(members) = state["observation"]["ready_members"].as_array() else {
        return false;
    };
    events.iter().any(|replay| {
        replay["action"].as_str() == Some("member-ready")
            && same_ready_body(lost, replay)
            && lost["body_digest"].as_str().is_some()
            && lost["body_digest"] == replay["body_digest"]
            && replay["sequence"]
                .as_u64()
                .is_some_and(|sequence| sequence > lost_sequence)
            && replay["connection"].as_u64().is_some_and(|connection| {
                connection != lost_connection
                    && events.iter().any(|bound| {
                        bound["action"].as_str() == Some("member-work-rebind")
                            && bound["member"] == lost["member"]
                            && bound["connection"].as_u64() == Some(connection)
                            && bound["sequence"].as_u64().is_some_and(|sequence| {
                                sequence > lost_sequence
                                    && replay["sequence"]
                                        .as_u64()
                                        .is_some_and(|replayed| sequence < replayed)
                            })
                    })
                    && events.iter().any(|ready| {
                        ready["action"].as_str() == Some("member-rebound-ready")
                            && same_ready_body(lost, ready)
                            && lost["body_digest"] == ready["body_digest"]
                            && ready["connection"].as_u64() == Some(connection)
                            && ready["sequence"].as_u64().is_some_and(|sequence| {
                                replay["sequence"]
                                    .as_u64()
                                    .is_some_and(|replayed| sequence > replayed)
                            })
                            && members.iter().any(|member| {
                                same_ready_body(lost, member)
                                    && member["connection"].as_u64() == Some(connection)
                            })
                    })
            })
    })
}

// Each EOF leg requires two new real native completions on the new mapped
// work connection. A startup Ready latch or unchanged C alone is insufficient.
fn rebound_step(
    input: &mut File,
    output: &mut File,
    guardian: &mut Guardian,
    generation: u64,
    changed: bool,
    activity: Option<&ActivityProof>,
) -> Result<String, Fail> {
    let before = native(guardian, "observe")?;
    if activity.is_some_and(|proof| !activity_current(&before, proof, true)) {
        return Err(Fail::die(
            "original native Activity was not retained before EOF",
        ));
    }
    let sequence = before["outcomeSequence"]
        .as_u64()
        .ok_or_else(|| Fail::die("missing native outcome sequence"))?;
    sdk_step(input, output, "workrebind")?;
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let after = native(guardian, "observe")?;
        if changed && generation == 4 && !ready_loss_recovered(&after, sequence) {
            if Instant::now() >= until {
                return Err(Fail::die(
                    "native committed Ready loss was not recovered with the same report",
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        if activity.is_some_and(|proof| !activity_current(&after, proof, true)) {
            return Err(Fail::die(
                "original native Activity was not retained across EOF",
            ));
        }
        if after["observation"]["unresolved"].as_u64() == Some(0)
            && let Some(events) = after["outcomes"].as_array()
            && let Some(members) = after["observation"]["ready_members"].as_array()
            && let Some(previous) = before["observation"]["ready_members"].as_array()
        {
            for rebound in events.iter().filter(|event| {
                event["action"].as_str() == Some("member-rebound-ready")
                    && event["sequence"]
                        .as_u64()
                        .is_some_and(|value| value > sequence)
            }) {
                let member = rebound["member"].as_str();
                let connection = rebound["connection"].as_u64();
                let current = members.iter().find(|entry| {
                    entry["member"].as_str() == member
                        && entry["connection"].as_u64() == connection
                        && entry["operation"] == rebound["operation"]
                        && entry["registration"] == rebound["registration"]
                        && entry["preload_sequence"] == rebound["preload_sequence"]
                        && entry["credential_generation"] == rebound["credential_generation"]
                        && entry["report"] == rebound["report"]
                });
                let original = previous
                    .iter()
                    .find(|entry| entry["member"].as_str() == member);
                let bound = events.iter().any(|event| {
                    event["action"].as_str() == Some("member-work-rebind")
                        && event["member"].as_str() == member
                        && event["connection"].as_u64() == connection
                        && event["sequence"].as_u64().is_some_and(|value| {
                            value > sequence
                                && rebound["sequence"]
                                    .as_u64()
                                    .is_some_and(|ready| value < ready)
                        })
                });
                if let (Some(current), Some(original), Some(connection)) =
                    (current, original, connection)
                    && bound
                    && original["connection"].as_u64() != Some(connection)
                    && current["credential_generation"].as_u64() == Some(generation)
                    && (if changed {
                        original["credential_generation"]
                            .as_u64()
                            .is_some_and(|old| old < generation)
                            && current["registration"] != original["registration"]
                    } else {
                        current["registration"] == original["registration"]
                            && current["report"] == original["report"]
                            && original["credential_generation"].as_u64() == Some(generation)
                    })
                {
                    return member
                        .map(str::to_owned)
                        .ok_or_else(|| Fail::die("native rebound member missing"));
                }
            }
        }
        if Instant::now() >= until {
            return Err(Fail::die(
                "EOF leg lacks new correlated native rebound completion",
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn advance(guardian: &mut Guardian, generation: u64) -> Result<(), Fail> {
    let _ = native(guardian, "advance")?;
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        let state = native(guardian, "observe")?;
        if state["generation"]
            .as_u64()
            .is_some_and(|current| current >= generation)
        {
            return Ok(());
        }
        if Instant::now() >= until {
            return Err(Fail::die("native proof revision did not settle"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_host(host: &mut Child) -> Result<(), Fail> {
    let until = Instant::now() + MUST_HAPPEN;
    loop {
        if let Some(status) = host
            .try_wait()
            .map_err(|_| Fail::die("native Source host wait failed"))?
        {
            return if status.success() {
                Ok(())
            } else {
                Err(Fail::die("native Source host rejected the proof"))
            };
        }
        if Instant::now() >= until {
            return Err(Fail::die("native Source host exit hang detector expired"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let host_path = instance::take_value(&mut args, "--aterm")?.ok_or_else(|| {
        Fail::usage("codex-source-proof requires --aterm <native_host debug fixture>")
    })?;
    let package_path = instance::take_value(&mut args, "--package")?.ok_or_else(|| {
        Fail::usage("codex-source-proof requires --package <isolated debug SDK candidate>")
    })?;
    let keep = instance::take_flag(&mut args, "--keep");
    if !args.is_empty() {
        return Err(Fail::usage("codex-source-proof: unexpected arguments"));
    }
    let pkg = codex_alab_proof::package(Some(&package_path), paths::paths().need_store_home()?)?;
    let fixture = NativeSourceProofWorld::new().map_err(Fail::die)?;
    let world = instance::World::source_proof(&fixture).map_err(Fail::die)?;
    let root = fixture.directory();
    let copied = root.join("candidate");
    let copy = aterm_uds::spawnfd::command("/bin/cp")
        .arg("-cR")
        .arg(pkg)
        .arg(&copied)
        .stdin(Stdio::null())
        .status()
        .map_err(|_| Fail::die("source proof package copy failed"))?;
    if !copy.success() {
        return Err(Fail::die("source proof package copy failed"));
    }
    let manifest = std::fs::read_to_string(copied.join(codex_alab::PACKAGE_JSON))
        .map_err(|_| Fail::die("source proof candidate manifest unavailable"))?;
    let executable = copied.join(
        codex_alab::package_entrypoint(&manifest)
            .ok_or_else(|| Fail::die("invalid proof candidate layout"))?,
    );
    if !codex_alab::isolated_source_proof_candidate(&executable)
        .map_err(|_| Fail::die("proof candidate unavailable"))?
    {
        return Err(Fail::die(
            "package lacks explicit debug Source proof feature",
        ));
    }
    let copied_host = root.join("native-host");
    std::fs::copy(crate::paths::from_caller(&host_path), &copied_host)
        .map_err(|_| Fail::die("native Source fixture artifact unavailable"))?;
    fixture.publish().map_err(Fail::die)?;
    let task = fixture.launch_task(&executable).map_err(Fail::die)?;
    let task_file = root.join("source-proof.task");
    publish_task(root, &task).map_err(|_| Fail::die("source proof task write failed"))?;
    let guardian_path = root.join("proof-guardian.sock");
    let listener = CtlListener::bind(&guardian_path)
        .map_err(|_| Fail::die("source proof guardian unavailable"))?;
    listener
        .set_nonblocking(true)
        .map_err(|_| Fail::die("source proof guardian unavailable"))?;
    let mut environment = world.env();
    environment.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "ATERM_HARNESS_STATE");
    environment.push((
        "ATERM_STATE_HOME".into(),
        fixture
            .env()
            .aterm_state
            .as_ref()
            .ok_or_else(|| Fail::die("fixture state missing"))?
            .to_string_lossy()
            .into_owned(),
    ));
    environment.push((
        "ATERM_HARNESS_STATE".into(),
        fixture.env().state.to_string_lossy().into_owned(),
    ));
    let jail = format!(
        "{}{}",
        codex_rig::no_ip_jail_policy(),
        codex_rig::home_guards(&paths::paths().need_guarded_homes()?)?
    );
    let log = crate::util::create_stdio_file(&root.join("source-host.log"))
        .map_err(|_| Fail::die("source host log unavailable"))?;
    if aterm_uds::tether::executable().is_none() {
        return Err(Fail::die("source proof lab has no parent-death tether"));
    }
    let mut host = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .args(["-p", &jail])
        .arg(&copied_host)
        .arg("--source-proof-task")
        .arg(&task_file)
        .env_clear()
        .envs(environment)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            log.try_clone()
                .map_err(|_| Fail::die("source host log unavailable"))?,
        ))
        .stderr(Stdio::from(log))
        .tether()
        .spawn()
        .map_err(|_| Fail::die("native Source host could not start"))?;
    let result = (|| -> Result<(), Fail> {
        let until = Instant::now() + MUST_HAPPEN;
        let guardian = loop {
            match aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if host
                        .try_wait()
                        .map_err(|_| Fail::die("source host wait failed"))?
                        .is_some()
                    {
                        return Err(Fail::die(
                            "source host exited before SDK descriptor transfer",
                        ));
                    }
                    if Instant::now() >= until {
                        return Err(Fail::die(
                            "source host descriptor transfer hang detector expired",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return Err(Fail::die("source host guardian accept failed")),
            }
        };
        let peer_pid = aterm_uds::fdpass::peer_pid(&guardian).map_err(|error| {
            Fail::die(format!(
                "source host kernel peer unavailable expected={} kind={:?} errno={:?}",
                host.id(),
                error.kind(),
                error.raw_os_error()
            ))
        })?;
        if peer_pid != host.id() {
            return Err(Fail::die(format!(
                "source host kernel peer changed expected={} observed={peer_pid}",
                host.id()
            )));
        }
        guardian
            .set_read_timeout(Some(MUST_HAPPEN))
            .map_err(|_| Fail::die("source host guardian deadline unavailable"))?;
        let mut metadata = [0u8; 4096];
        let mut received = aterm_uds::fdpass::recv_with_fds(&guardian, &mut metadata, 2)
            .map_err(|_| Fail::die("source host SDK descriptor transfer failed"))?;
        if received.fds.len() != 2 {
            return Err(Fail::die("source host transferred wrong descriptor roles"));
        }
        let header: aterm_json::Value = aterm_json::from_slice(&metadata[..received.bytes])
            .map_err(|_| Fail::die("invalid source host header"))?;
        let host_process = Process::observe(host.id())
            .ok_or_else(|| Fail::die("source host kernel identity unavailable"))?;
        if header["schema"].as_u64() != Some(1)
            || header["hostPid"].as_u64() != Some(u64::from(host.id()))
            || Some(host_process.image) != aterm_agent::harness::native_act::Image::at(&copied_host)
        {
            return Err(Fail::die("source host mapped fixture changed"));
        }
        let pid = header["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| Fail::die("source child kernel PID missing"))?;
        let actual = Process::observe(pid)
            .ok_or_else(|| Fail::die("source child kernel identity unavailable"))?;
        if actual.birth
            != header["birth"]
                .as_u64()
                .ok_or_else(|| Fail::die("source child birth missing"))?
            || Some(actual.image.clone())
                != aterm_agent::harness::native_act::Image::at(&executable)
        {
            return Err(Fail::die("source child mapped candidate changed"));
        }
        let mut output = File::from(
            received
                .fds
                .pop()
                .ok_or_else(|| Fail::die("source child stdout missing"))?,
        );
        let mut input = File::from(
            received
                .fds
                .pop()
                .ok_or_else(|| Fail::die("source child stdin missing"))?,
        );
        let mut guardian = Guardian {
            stream: guardian,
            prefix: aterm_uds::rand::hex_token::<8>()
                .map_err(|_| Fail::die("source proof entropy unavailable"))?,
            counter: 0,
        };
        sdk_step(&mut input, &mut output, "startup")?;
        await_native(&mut guardian, 1)?;
        let primary = rebound_step(&mut input, &mut output, &mut guardian, 1, false, None)?;
        advance(&mut guardian, 2)?;
        if rebound_step(&mut input, &mut output, &mut guardian, 2, true, None)? != primary {
            return Err(Fail::die(
                "primary native member changed across no-use EOF legs",
            ));
        }
        sdk_step(&mut input, &mut output, "contextual")?;
        let activity = capture_activity(&mut guardian, &primary)?;
        sdk_step(&mut input, &mut output, "derived")?;
        require_derived(&mut guardian, &activity)?;
        await_native(&mut guardian, 2)?;
        advance(&mut guardian, 3)?;
        sdk_step(&mut input, &mut output, "resample")?;
        await_native(&mut guardian, 3)?;
        if rebound_step(
            &mut input,
            &mut output,
            &mut guardian,
            3,
            false,
            Some(&activity),
        )? != primary
        {
            return Err(Fail::die(
                "primary native member changed across Activity EOF",
            ));
        }
        // Changed EOF under the retained actual Activity must really reload
        // and report new Ready. Intentional fencing is incomplete, never PASS.
        let armed = native_command(&mut guardian, "arm-ready-loss", Some((&primary, 4)))?;
        if armed["readyLoss"]["credential_generation"].as_u64() != Some(4)
            || armed["readyLoss"]["member"].as_str() != Some(primary.as_str())
            || !armed["readyLoss"]["fired"].is_null()
        {
            return Err(Fail::die("native Ready loss was not armed exactly"));
        }
        advance(&mut guardian, 4)?;
        if rebound_step(
            &mut input,
            &mut output,
            &mut guardian,
            4,
            true,
            Some(&activity),
        )? != primary
        {
            return Err(Fail::die(
                "primary native member changed across changed Activity EOF",
            ));
        }
        sdk_step(&mut input, &mut output, "stop")?;
        drop((input, output));
        wait_host(&mut host)
    })();
    if result.is_err() {
        let _ = host.kill();
        let _ = host.wait();
    }
    if keep || result.is_err() {
        let kept = fixture.keep();
        println!("WORLD isolated_source_proof={}", kept.display());
    }
    result?;
    println!(
        "PASS joined Source SDK/native protocol; synthetic issuer only, no vendor or production SourceV1 activation"
    );
    Ok(0)
}

#[cfg(test)]
#[path = "codex_source_proof_driver_response_limit_tests.rs"]
mod response_limit_tests;
