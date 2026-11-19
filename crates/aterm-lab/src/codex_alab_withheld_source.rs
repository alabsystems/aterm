// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Withheld-credential controls on ordinary signed CLI and interactive Responses turns.

use std::io::{self, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::native_act::Process;
use aterm_uds::spawnfd::{Child, Stdio};

use crate::codex_alab_proof::{PROOF_LOCK, PROOF_LOCK_WAIT, policy, proof_lock, published_target};
use crate::codex_rig::{self, ApiLog, SourceBearer, SourceResponses};
use crate::instance::World;
use crate::pyjson::J;
use crate::{Fail, Res, stop};

#[path = "codex_alab_withheld_tui.rs"]
mod tui;

const HANG: Duration = Duration::from_secs(60);

fn inconclusive(why: impl std::fmt::Display) -> Fail {
    Fail::exit(
        3,
        format!("INCONCLUSIVE: signed Codex withheld-Source control: {why}"),
    )
}

pub(in crate::codex_alab_proof) fn run(
    candidate: &Path,
    keep: bool,
    terminal: Option<&Path>,
) -> Res {
    let _lock = proof_lock(
        &crate::paths::state_root(crate::sys::getuid()).join(PROOF_LOCK),
        PROOF_LOCK_WAIT,
    )?;
    let packages = published_target::Packages::prepare(None, candidate).map_err(inconclusive)?;
    let mut world = World::new(crate::instance::CONFIG_OFF).map_err(inconclusive)?;
    let staged = packages
        .stage(&world.dir, &world.home)
        .map_err(inconclusive)?;
    let package = world.dir.join("pkg-b");
    let payload = staged
        .payload(&package)
        .map_err(inconclusive)?
        .ok_or_else(|| inconclusive("the active package has no signed payload"))?;
    let exe = package.join("bin/codex");
    let abi =
        aterm_agent::harness::codex_alab::source_credential_capable(&exe).map_err(inconclusive)?;
    println!(
        "signed Codex build {}: session Source constructor ABI {}; exercising {} with the credential withheld",
        payload.build(),
        if abi { "present" } else { "absent" },
        if terminal.is_some() {
            "the ordinary interactive TUI"
        } else {
            "ordinary codex exec"
        },
    );
    let result = control(&world, &exe, payload, terminal);
    if keep || result.is_err() {
        world.keep();
        println!("signed control evidence: {}", world.dir.display());
    }
    result
}

fn control(
    world: &World,
    exe: &Path,
    payload: &published_target::Payload,
    terminal: Option<&Path>,
) -> Res {
    let listener =
        || aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).map_err(inconclusive);
    let (provider, canary) = (listener()?, listener()?);
    let port = provider.local_addr().map_err(inconclusive)?.port();
    let jail = policy(port, crate::paths::paths(), &world.dir).map_err(inconclusive)?;
    let env = super::fresh_environment(&world.dir).map_err(inconclusive)?;
    // Normal provider credentials are resolved by Codex; its file credential store is empty.
    let config = format!(
        "model_provider = \"lab\"\nmodel = \"gpt-5.5\"\napproval_policy = \"never\"\n\
         check_for_update_on_startup = false\ncli_auth_credentials_store = \"file\"\n\
         mcp_oauth_credentials_store = \"file\"\n[analytics]\nenabled = false\n\
         [feedback]\nenabled = false\n[model_providers.lab]\nname = \"private Responses provider\"\n\
         base_url = \"http://127.0.0.1:{port}/v1\"\nwire_api = \"responses\"\n\
         requires_openai_auth = true\nrequest_max_retries = 0\nstream_max_retries = 0\n"
    );
    let work = world
        .dir
        .join("work")
        .canonicalize()
        .map_err(inconclusive)?;
    // Trust only this fresh lab project. Ordinary interactive login and provider
    // credential resolution must still run, including a login gate with an empty home.
    let project = J::s(work.to_string_lossy()).dump(crate::pyjson::PY);
    crate::util::write_fresh(
        &world.dir.join("codex-home/config.toml"),
        format!("{config}\n[projects.{project}]\ntrust_level = \"trusted\"\n"),
    )
    .map_err(inconclusive)?;
    let log = Arc::new(Mutex::new(ApiLog::new(
        world.dir.join("requests.jsonl"),
        Instant::now(),
    )));
    let bearer = SourceBearer::fresh().map_err(inconclusive)?;
    let journal = Arc::clone(&log);
    std::thread::Builder::new()
        .name("codex-withheld-provider".into())
        .spawn(move || {
            codex_rig::serve_with_source(
                provider,
                journal,
                crate::codex_alab_model::answerer(),
                bearer,
            );
        })
        .map_err(inconclusive)?;
    if !super::jail_holds(&jail, port, &canary) {
        return Err(inconclusive(
            "the private network jail failed its positive/negative connection probes",
        ));
    }
    let prompt = format!(
        "Acknowledge this qualification turn {}",
        aterm_uds::rand::hex_token::<24>().map_err(inconclusive)?
    );
    if let Some(aterm) = terminal {
        return tui::run(world, aterm, exe, payload, jail, &prompt, &log);
    }
    let output = world.dir.join("exec-stdout.jsonl");
    let stdout = crate::util::create_stdio_file(&output).map_err(inconclusive)?;
    let stderr =
        crate::util::create_stdio_file(&world.dir.join("exec-stderr.txt")).map_err(inconclusive)?;
    let mut child = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .arg("-p")
        .arg(&jail)
        .arg(exe)
        .args([
            "exec",
            "--json",
            "--color",
            "never",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "-",
        ])
        .current_dir(world.dir.join("work"))
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn_in_dedicated_process_group()
        .map_err(inconclusive)?;
    let pid = i32::try_from(child.id()).expect("Darwin pid_t fits i32");
    super::hold(Some(pid));
    let run =
        start_turn(&mut child, payload, pid, &prompt).and_then(|process| {
            let exit = super::end_with_group(&mut child, &super::Witness::default(), HANG)
                .map_err(|cut| match cut {
                    super::Cut::Failed(reason) => inconclusive(reason),
                    super::Cut::Stopped(signal) => Fail::from(signal),
                })?;
            Ok((process, exit))
        });
    if run.is_err() {
        super::end_running_group();
        child.wait().map_err(inconclusive)?;
    }
    let (process, exit) = run?;
    let stdout = crate::util::read_regular_text(&output).map_err(inconclusive)?;
    let events = stdout
        .lines()
        .map(J::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(inconclusive)?;
    let threads: Vec<_> = events
        .iter()
        .filter(|e| e.get("type").and_then(J::as_str) == Some("thread.started"))
        .collect();
    let thread = match threads.as_slice() {
        [event] => event
            .get("thread_id")
            .and_then(J::as_str)
            .filter(|id| aterm_agent::harness::codex_rollout::is_thread_id(id)),
        _ => None,
    };
    let deadline = Instant::now() + HANG;
    let receipts = loop {
        let responses = log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .source_responses(thread.unwrap_or_default(), &prompt)
            .map_err(inconclusive)?;
        if responses.accepted != 0
            || responses.written == responses.refused
            || Instant::now() >= deadline
        {
            break responses;
        }
        stop::sleep(Duration::from_millis(50))?;
    };
    judge(exit, &events, thread.is_some(), &receipts)?;
    println!(
        "PASS withheld-Source CLI control: signed build {} pid {} birth {}, thread {}, matching 401 writes {}. Packaged TUI Source and live update remain unqualified.",
        payload.build(),
        process.pid,
        process.birth,
        thread.unwrap_or_default(),
        receipts.written
    );
    Ok(0)
}

/// Release the one-time prompt through this child's private stdin only after the kernel maps
/// the leased signed image at its PID. EOF lets the ordinary CLI start its turn.
fn start_turn(
    child: &mut Child,
    payload: &published_target::Payload,
    pid: i32,
    prompt: &str,
) -> Result<Process, Fail> {
    let deadline = Instant::now() + HANG;
    loop {
        stop::check()?;
        if child.exited().map_err(inconclusive)? || Instant::now() >= deadline {
            return Err(inconclusive(
                "the signed process could not be observed before input",
            ));
        }
        if let Some(process) = payload.observe_process(pid) {
            child
                .stdin
                .take()
                .ok_or_else(|| inconclusive(io::Error::other("child has no private stdin")))?
                .write_all(prompt.as_bytes())
                .map_err(inconclusive)?;
            return Ok(process);
        }
        stop::sleep(Duration::from_millis(50))?;
    }
}

fn judge(
    exit: Option<i32>,
    events: &[J],
    identified_thread: bool,
    responses: &SourceResponses,
) -> Result<(), Fail> {
    if responses.accepted != 0 {
        return Err(Fail::die(
            "FAIL: provider accepted a Responses request while the session Source was withheld",
        ));
    }
    if !identified_thread {
        return Err(inconclusive(
            "the signed CLI did not identify one valid original thread",
        ));
    }
    if responses.refused == 0 || responses.written != responses.refused {
        return Err(inconclusive(
            "no complete matching provider 401 for the signed process's thread and one-time prompt",
        ));
    }
    let count = |kind| {
        events
            .iter()
            .filter(|event| event.get("type").and_then(J::as_str) == Some(kind))
            .count()
    };
    if !exit.is_some_and(|status| status > 0)
        || count("turn.started") != 1
        || count("turn.failed") != 1
        || count("turn.completed") != 0
    {
        return Err(Fail::die(
            "FAIL: the signed CLI did not end one failed turn after its provider denied the request",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "codex_alab_withheld_source_tests.rs"]
mod tests;
