// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary hosts on separate physical Macs. Paired profiles use synthetic prior approval.
//! A world whose vault is the live default ALab vault (one an earlier lab run left admitted there)
//! opens for no verb but behind `--live-alab-vault-keeps-member` ([`LIVE`]): nothing a lab run
//! holds takes that member out again.
//! Preparation never imports the recipient's private identity or supplies Commons admission.

use crate::commons_startup::table::Table;
use crate::instance::{BOOT, CONFIG_OFF, Instance, Launch, World};
use crate::{Fail, Res, stop};
use aterm_codec::hex;
use aterm_db::replica::InventorySummary;
use aterm_json::{Value, json};
use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_types::network_state::Visibility;
use aterm_uds::private_file;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "commons_lan_claims.rs"]
mod claims;
#[path = "commons_lan_emails.rs"]
mod emails;

const HANG: Duration = Duration::from_secs(60);
const CAPSULE_LIMIT: usize = 32 * 1024;
/// The most recipients one synthetic vault approves: labels `m2` … `m9`.
const MAX_RECIPIENTS: usize = 8;
/// The longest ordinary host run: an hour, so one host stays up across a run of edits; its
/// `stop` marker ends it sooner.
const MAX_RUN_S: u64 = 3600;
const APPLICATION: &str = "lan-startup-data";
const USAGE: &str = "commons-lan prepare-recipient <new-world> | prepare-source <new-world> <recipient-key>... | join <world> <capsule-file> | run <aterm> <world> <seconds> | node <aterm> <world> <seconds> | pair <aterm> <world> <seconds> | follow <aterm> <world> <seconds> <owner-pid> | stop <run-directory> | db <aterm> <world> <db-command> [args...] | secrets <aterm> <world> approve <age1…> --name <host>|join <lab-profile>/fixture.vault|status | declare <world> | write <world> <key> | read <world> | claim <world> <counter> <since> <until> | release <world> <counter> <until> | claims <world> | emails-fixture <world> <seed> | emails-set <aterm> <world> <seed> <row> <column> <value-or-tag> | emails-wait <aterm> <world> <digest>|!<digest> <seconds> | emails-reveal <aterm> <world> <seed> [<tag>...]";

/// The flag that lets a `commons-lan` verb drive a world the live default ALab vault admitted: it
/// names the vault and what stays in it, a member no run takes out.
const LIVE: &str = "--live-alab-vault-keeps-member";

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(error.to_string())
}

fn emit(value: &Value) -> Result<(), Fail> {
    println!("{}", aterm_json::to_string(value).map_err(fail)?);
    std::io::stdout().flush().map_err(fail)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    let mut file = crate::util::create_new_regular_file(path, 0o600).map_err(fail)?;
    file.write_all(bytes).map_err(fail)
}

fn prepare(root: &Path, role: &str) -> Result<World, Fail> {
    let config = format!("{CONFIG_OFF}\n[ai]\nenabled = false\n[fleet]\nwatch = false\n");
    let world = World::create_at(root, &config).map_err(fail)?;
    write_new(&world.dir.join("commons-lan-role"), role.as_bytes())?;
    Ok(world)
}

/// Reopen a prepared profile. One whose vault is the live default ALab vault (a world an earlier
/// lab run left admitted there) opens only where `live` ([`LIVE`]): an ordinary host, a table
/// write or a read there keeps that identity a member, and its Commons runtime enrolls it in the
/// vault's membership log, where it counts as a voter.
fn open(root: &Path, live: bool) -> Result<(World, String), Fail> {
    let world = World::open_at(root).map_err(fail)?;
    let role = private_file::read_private(&world.dir, "commons-lan-role", 16).map_err(fail)?;
    let role = std::str::from_utf8(&role).map_err(fail)?;
    if !matches!(role, "source" | "recipient") {
        return Err(Fail::die("not a prepared Commons LAN profile"));
    }
    let config = client::config(&Paths::for_home(&world.home)).map_err(fail)?;
    if !live && config.as_ref().is_some_and(client::alab_vault) {
        return Err(Fail::die(format!(
            "{} is an identity of the live default ALab vault: driving it keeps that member and \
             enrolls it in the vault's membership log. Pass {} to accept that member",
            world.dir.display(),
            LIVE
        )));
    }
    Ok((world, role.into()))
}

fn recipient_key(paths: &Paths) -> Result<String, Fail> {
    Ok(client::machine_identity(paths)
        .map_err(fail)?
        .recipient()
        .to_bech32())
}

fn prepare_recipient(root: &Path) -> Result<Value, Fail> {
    let world = prepare(root, "recipient")?;
    let paths = Paths::for_home(&world.home);
    Ok(
        json!({"schema":1,"role":"recipient","world":world.dir.display().to_string(),
        "recipient":recipient_key(&paths)?,"commons_admitted":false,"application_tables":0}),
    )
}

fn prepare_source(root: &Path, recipients: &[String]) -> Result<Value, Fail> {
    if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
        return Err(Fail::usage(format!(
            "prepare-source approves 1 to {MAX_RECIPIENTS} recipients"
        )));
    }
    let world = prepare(root, "source")?;
    let paths = Paths::for_home(&world.home);
    let vault = world.dir.join("fixture.vault");
    let created =
        client::create(&paths, &vault.to_string_lossy(), None, Some("m1")).map_err(fail)?;
    let admin = client::admin_open(&paths).map_err(fail)?;
    let mut doc = admin.doc.clone();
    // Each recipient takes the next label after the source's `m1`, in the order named.
    let hosts: Vec<String> = (2..)
        .take(recipients.len())
        .map(|n| format!("m{n}"))
        .collect();
    for (host, recipient) in hosts.iter().zip(recipients) {
        doc.add_machine(host, recipient).map_err(fail)?;
    }
    client::admin_publish_approved(&paths, &admin, &doc, recipients).map_err(fail)?;
    let genesis =
        client::seed_vault_genesis(&paths, &world.dir.join("fixture.genesis")).map_err(fail)?;
    use std::os::unix::fs::PermissionsExt as _;
    aterm_uds::regular_file::open(&vault)
        .and_then(|(f, _)| f.set_permissions(std::fs::Permissions::from_mode(0o600)))
        .map_err(fail)?;
    let bytes = aterm_secrets::fsx::read_private(&vault, CAPSULE_LIMIT)
        .map_err(fail)?
        .ok_or_else(|| Fail::die("synthetic vault publication is missing"))?;
    let vault_hex = hex::encode_lower(bytes.as_slice());
    let mut capsules = Vec::with_capacity(recipients.len());
    for (host, recipient) in hosts.iter().zip(recipients) {
        let capsule = json!({"schema":1,"signer":created.signer,"recipient":recipient,
            "host":host,"vault_hex":vault_hex});
        if aterm_json::to_vec(&capsule).map_err(fail)?.len() > CAPSULE_LIMIT {
            return Err(Fail::die(
                "synthetic vault capsule exceeds its transfer bound",
            ));
        }
        capsules.push(capsule);
    }
    Ok(
        json!({"schema":1,"role":"source","world":world.dir.display().to_string(),
        "genesis":hex::encode_lower(&genesis.digest),"genesis_members":genesis.members,
        "capsules":capsules}),
    )
}

fn join_recipient(root: &Path, capsule: &[u8]) -> Result<Value, Fail> {
    // Never with the live flag: this would point a live member's profile at a synthetic vault.
    let (world, role) = open(root, false)?;
    if role != "recipient" || capsule.len() > CAPSULE_LIMIT {
        return Err(Fail::die(
            "join requires a recipient profile and a bounded capsule",
        ));
    }
    let capsule: Value = aterm_json::from_slice(capsule)
        .map_err(|_| Fail::die("invalid synthetic vault capsule"))?;
    let paths = Paths::for_home(&world.home);
    if capsule["schema"].as_u64() != Some(1)
        || capsule["recipient"].as_str() != Some(recipient_key(&paths)?.as_str())
    {
        return Err(Fail::die(
            "synthetic vault capsule is for another recipient",
        ));
    }
    let host = capsule["host"]
        .as_str()
        .filter(|host| {
            host.strip_prefix('m')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                && host.len() <= 8
        })
        .ok_or_else(|| Fail::die("synthetic vault capsule names no recipient label"))?;
    let signer = capsule["signer"]
        .as_str()
        .filter(|key| !key.is_empty() && key.len() <= 256 && !key.chars().any(char::is_control))
        .ok_or_else(|| Fail::die("synthetic vault signer is invalid"))?;
    let vault = capsule["vault_hex"]
        .as_str()
        .and_then(|value| hex::decode_vec(value.as_bytes()))
        .ok_or_else(|| Fail::die("synthetic vault capsule bytes are invalid"))?;
    let file = world.dir.join("fixture.vault");
    write_new(&file, &vault)?;
    client::join(
        &paths,
        &JoinOptions {
            source: format!("{}#{signer}", file.display()),
            host: Some(host.into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .map_err(fail)?;
    let bootstrap = Bootstrap::required(&paths).map_err(fail)?;
    if bootstrap.witness().map_err(fail)?.is_some()
        || bootstrap.log_witness().map_err(fail)?.is_some()
        || bootstrap.is_log_backed().map_err(fail)?
        || bootstrap.access().is_ok_and(|access| access.is_some())
        || paths.records_file().exists()
        || paths.dir.join("commons/tables").exists()
    {
        return Err(Fail::die(
            "recipient already has Commons admission, records or tables",
        ));
    }
    Ok(
        json!({"schema":1,"role":"recipient","world":world.dir.display().to_string(),
        "vault_approved":true,"commons_admitted":false,"membership_records":false,
        "application_tables":0}),
    )
}

fn prepare_run(root: &Path, live: bool) -> Result<(World, String), Fail> {
    let (mut world, role) = open(root, live)?;
    let nonce = hex::encode_lower(&aterm_db::crypto::random::<8>().map_err(fail)?);
    let run = world.dir.join(format!("run-{nonce}.noindex"));
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&run)
        .map_err(fail)?;
    write_new(&run.join("lab-run"), b"commons-lan-run/1\n")?;
    // Keep each process's app output and lifeline beside its receipts while sharing
    // the prepared profile's original home, configuration and runtime directory.
    world.dir.clone_from(&run);
    Ok((world, role))
}

/// Observe what the ordinary host established without preparing its custody.
fn observed_admission(paths: &Paths, deadline: Instant) -> bool {
    let Ok((bootstrap, journal)) = Bootstrap::established_with_journal_until(paths, deadline)
    else {
        return false;
    };
    drop(journal);
    bootstrap.is_log_backed().unwrap_or(false)
        && bootstrap
            .log_witness()
            .is_ok_and(|witness| witness.is_some())
        && bootstrap.access().is_ok_and(|access| access.is_some())
}

fn run(
    aterm: &Path,
    root: &Path,
    seconds: u64,
    follows: Option<u32>,
    live: bool,
    mut ready: Option<std::sync::mpsc::SyncSender<(u32, PathBuf)>>,
) -> Res {
    if !(60..=MAX_RUN_S).contains(&seconds) {
        return Err(Fail::usage(format!(
            "commons-lan run takes 60 to {MAX_RUN_S} seconds"
        )));
    }
    let aterm = std::fs::canonicalize(aterm).map_err(fail)?;
    let (world, role) = prepare_run(root, live)?;
    let run = world.dir.clone();
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    let launch = Launch {
        aterm,
        headless: false,
        args: vec!["-e".into(), "/bin/sh".into()],
        env,
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    };
    let mut instance = Instance::start_ordinary(&world, launch).map_err(fail)?;
    instance.await_socket(BOOT)?;
    let pid = instance
        .aterm_pid()
        .ok_or_else(|| Fail::die("ordinary host has no pid"))?;
    let responsible_pid = crate::sys::responsible_pid(pid)
        .ok_or_else(|| Fail::die("cannot read the ordinary host's privacy responsibility"))?;
    if responsible_pid != pid {
        return Err(Fail::die(format!(
            "ordinary host {pid} is charged to process {responsible_pid}, not itself"
        )));
    }
    if follows == Some(pid as u32) {
        return Err(Fail::die("a follower must name another process"));
    }
    let started = json!({"phase":"started","role":role,"run":run.display().to_string(),
        "aterm_pid":pid,"keeper_pid":instance.pid,"responsible_pid":responsible_pid,
        "follows_pid":follows,
        "log":instance.log.display().to_string(),"socket":instance.sock.display().to_string()});
    write_new(
        &run.join("started.json"),
        &aterm_json::to_vec(&started).map_err(fail)?,
    )?;
    emit(&started)?;
    let config = world.cfg.join("aterm/aterm.toml");
    let hang = Instant::now() + HANG;
    loop {
        stop::check()?;
        if !instance.running() {
            return Err(Fail::die("ordinary host exited during startup"));
        }
        let admitted = observed_admission(&Paths::for_home(&world.home), hang);
        if admitted
            && let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(follows.unwrap_or(pid as u32))
        {
            let active = json!({"phase":"active","role":role,"run":run.display().to_string(),
                "aterm_pid":pid,"follows_pid":follows,"commons_admitted":true,
                "network":aterm_db::network_state::value(&observation)});
            write_new(
                &run.join("active.json"),
                &aterm_json::to_vec(&active).map_err(fail)?,
            )?;
            emit(&active)?;
            if let Some(ready) = ready.take() {
                ready
                    .send((pid as u32, run.clone()))
                    .map_err(|_| Fail::die("paired host stopped waiting for its owner"))?;
            }
            break;
        }
        if Instant::now() >= hang {
            let network = aterm_db::network_state::read_live_observation(&config)
                .map(|observation| aterm_db::network_state::value(&observation))
                .unwrap_or_else(|error| json!({"relay_error":format!("{:?}",error.kind())}));
            let unavailable = json!({"phase":"unavailable","role":role,
                "run":run.display().to_string(),"aterm_pid":pid,
                "commons_admitted":admitted,"network":network});
            write_new(
                &run.join("unavailable.json"),
                &aterm_json::to_vec(&unavailable).map_err(fail)?,
            )?;
            emit(&unavailable)?;
            if let Err(error) = crate::commons_startup::image(&instance, &run, "unavailable.png") {
                eprintln!(
                    "commons-lan: unavailable window image was not saved: {}",
                    error.msg.as_deref().unwrap_or("capture failed")
                );
            }
            return Err(Fail::die(
                "ordinary physical LAN host did not become active",
            ));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    let until = stop::deadline_after(Duration::from_secs(seconds));
    let mut took_over = false;
    while Instant::now() < until && !run.join("stop").try_exists().map_err(fail)? {
        if !instance.running() {
            return Err(Fail::die("ordinary host exited before the requested stop"));
        }
        if follows.is_some()
            && !took_over
            && let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(pid as u32)
        {
            let takeover = json!({"phase":"takeover","role":role,"run":run.display().to_string(),
                "aterm_pid":pid,"follows_pid":follows,
                "network":aterm_db::network_state::value(&observation)});
            write_new(
                &run.join("takeover.json"),
                &aterm_json::to_vec(&takeover).map_err(fail)?,
            )?;
            emit(&takeover)?;
            took_over = true;
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    drop(instance);
    emit(&json!({"phase":"stopped","role":role,"run":run.display().to_string()}))?;
    Ok(0)
}

/// Keep the lab's node owned across every reporting and observation error.
struct OwnedNode(aterm_uds::spawnfd::Child);

impl Drop for OwnedNode {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.0, HANG);
    }
}

/// Run the shipped headless node service (`aterm node serve`) in a prepared profile until
/// `seconds` pass or the run's `stop` marker appears, as the 2026-11-04 three-Mac runs did. Unlike
/// [`run`] it starts no window and disclaims nothing: the service is this command's child, so
/// macOS charges its Local Network use to whatever started this command (a tab of the installed
/// app), where an unbundled debug window's own identity holds no grant (`No route to host`,
/// docs/measured/commons-window-lan-boundary-2026-11-04.md). Each phase is one JSON line and a
/// file in the run's folder: `started`, `active` once the profile is admitted and its own
/// observer is Active, then `stopped` with the service's exit, after `SIGTERM` and, past the
/// one-minute hang detector, `SIGKILL`.
fn node(aterm: &Path, root: &Path, seconds: u64, live: bool) -> Res {
    if !(60..=MAX_RUN_S).contains(&seconds) {
        return Err(Fail::usage(format!(
            "commons-lan node takes 60 to {MAX_RUN_S} seconds"
        )));
    }
    let aterm = std::fs::canonicalize(aterm).map_err(fail)?;
    let (world, role) = prepare_run(root, live)?;
    let run = world.dir.clone();
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    let log = crate::util::create_new_regular_file(&run.join("node.log"), 0o600).map_err(fail)?;
    let err = log.try_clone().map_err(fail)?;
    let mut owned = OwnedNode(
        aterm_uds::spawnfd::command(&aterm)
            .env_clear()
            .envs(env)
            .current_dir(&world.home)
            .args(["node", "serve"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(fail)?,
    );
    let child = &mut owned.0;
    let pid = child.id();
    let phase = |name: &str, value: Value| -> Result<(), Fail> {
        write_new(
            &run.join(format!("{name}.json")),
            &aterm_json::to_vec(&value).map_err(fail)?,
        )?;
        emit(&value)
    };
    let stopped = |child: &mut aterm_uds::spawnfd::Child| -> Result<Value, Fail> {
        if child.try_wait().map_err(fail)?.is_none() {
            child.terminate().map_err(fail)?;
            let hang = Instant::now() + HANG;
            while child.try_wait().map_err(fail)?.is_none() && Instant::now() < hang {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let status = match child.try_wait().map_err(fail)? {
            Some(status) => status.to_string(),
            None => {
                aterm_uds::deadline::kill_within(child, HANG);
                "killed after the hang detector".into()
            }
        };
        Ok(
            json!({"phase":"stopped","role":role,"run":run.display().to_string(),
            "node_pid":pid,"exit":status}),
        )
    };
    phase(
        "started",
        json!({"phase":"started","role":role,"run":run.display().to_string(),"node_pid":pid,
            "log":run.join("node.log").display().to_string()}),
    )?;
    let config = world.cfg.join("aterm/aterm.toml");
    let hang = Instant::now() + HANG;
    loop {
        if let Err(error) = stop::check() {
            emit(&stopped(child)?)?;
            return Err(error.into());
        }
        if child.try_wait().map_err(fail)?.is_some() {
            emit(&stopped(child)?)?;
            return Err(Fail::die("the node service exited during startup"));
        }
        let admitted = observed_admission(&Paths::for_home(&world.home), hang);
        if admitted
            && let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(pid)
        {
            phase(
                "active",
                json!({"phase":"active","role":role,"run":run.display().to_string(),
                    "node_pid":pid,"commons_admitted":true,
                    "network":aterm_db::network_state::value(&observation)}),
            )?;
            break;
        }
        if Instant::now() >= hang {
            let network = aterm_db::network_state::read_live_observation(&config)
                .map(|observation| aterm_db::network_state::value(&observation))
                .unwrap_or_else(|error| json!({"relay_error":format!("{:?}",error.kind())}));
            phase(
                "unavailable",
                json!({"phase":"unavailable","role":role,"run":run.display().to_string(),
                    "node_pid":pid,"commons_admitted":admitted,"network":network}),
            )?;
            emit(&stopped(child)?)?;
            return Err(Fail::die("the node service did not become active"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until
        && !run.join("stop").try_exists().map_err(fail)?
        && stop::check().is_ok()
        && child.try_wait().map_err(fail)?.is_none()
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    let exited_early = child.try_wait().map_err(fail)?.is_some();
    emit(&stopped(child)?)?;
    if exited_early {
        return Err(Fail::die(
            "the node service exited before the requested stop",
        ));
    }
    Ok(0)
}

/// Start the owner and its local follower under one process's quiet reservation.
/// Independent lab commands reserve the machine separately and cannot overlap.
fn pair(aterm: &Path, root: &Path, seconds: u64, live: bool) -> Res {
    std::thread::scope(|scope| {
        let (ready, owner_ready) = std::sync::mpsc::sync_channel(1);
        let owner = scope.spawn(|| run(aterm, root, seconds, None, live, Some(ready)));
        let joined = |result: std::thread::Result<Res>| {
            result.map_err(|_| Fail::die("paired Commons owner panicked"))?
        };
        let Ok((pid, owner_run)) = owner_ready.recv() else {
            return joined(owner.join());
        };
        let followed = run(aterm, root, seconds, Some(pid), live, None);
        let stop = if owner.is_finished() || owner_run.join("stop").try_exists().map_err(fail)? {
            Ok(())
        } else {
            request_stop(&owner_run)
        };
        let original = joined(owner.join());
        followed?;
        stop?;
        original
    })
}

fn request_stop(run: &Path) -> Result<(), Fail> {
    let marker = private_file::read_private(run, "lab-run", 64).map_err(fail)?;
    if marker.as_slice() != b"commons-lan-run/1\n" {
        return Err(Fail::die("not an owned Commons LAN run"));
    }
    write_new(&run.join("stop"), b"")
}

fn table_snapshot(world: &World) -> Result<Value, Fail> {
    let paths = Paths::for_home(&world.home);
    let table = Table::application(&paths, APPLICATION, "items")?;
    let (rows, known) = table.observe(&paths, &world.cfg.join("aterm/aterm.toml"))?;
    if known
        != rows
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(Fail::die(
            "application inventory contains unavailable or ineligible rows",
        ));
    }
    let inventory = InventorySummary::of(&known);
    Ok(
        json!({"table":table.id(),"known":inventory.count,"eligible_decrypted":rows.len(),
        "inventory_sha256":hex::encode_lower(&inventory.fingerprint),
        "reader_declared_or_joined_table":false,
        "operations":rows.iter().map(|(id,value)|json!({"operation":hex::encode_lower(id),"value":value})).collect::<Vec<_>>()}),
    )
}

/// Whether `commons-lan db` passes `args` to the shipped database CLI: inert proposal and data
/// commands, the lab model's import and check, and the owner's emails view. No credential
/// activation, membership or service command passes, and `emails --show-passwords` reaches a
/// process with no terminal, which the view refuses (one-mesh design §4.8).
fn admitted_db(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some(
            "nonce" | "commons-status" | "commons-tables" | "commons-inventory" | "accounts-init"
            | "accounts-write" | "accounts-get" | "accounts-scan" | "accounts-history" | "emails",
        ) => true,
        Some("alab") => matches!(args.get(1).map(String::as_str), Some("import" | "check")),
        _ => false,
    }
}

/// Run the shipped database CLI against a prepared profile, including while its
/// app is stopped, with `stdin` as its standard input; what it wrote.
fn db_output(
    aterm: &Path,
    world: &World,
    args: &[String],
    stdin: aterm_uds::spawnfd::Stdio,
) -> Result<std::process::Output, Fail> {
    world_output(aterm, world, "db", args, stdin)
}

/// Run the shipped `aterm <noun> <args>` in a prepared profile's environment, with `stdin` as its
/// standard input; what it wrote.
fn world_output(
    aterm: &Path,
    world: &World,
    noun: &str,
    args: &[String],
    stdin: aterm_uds::spawnfd::Stdio,
) -> Result<std::process::Output, Fail> {
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    let mut command = aterm_uds::spawnfd::command(std::fs::canonicalize(aterm).map_err(fail)?);
    command
        .env_clear()
        .envs(env)
        .current_dir(&world.home)
        .arg(noun)
        .args(args);
    aterm_uds::deadline::output_until_from(&mut command, stdin, Instant::now() + HANG).map_err(fail)
}

/// The shipped vault verbs a lab's own membership takes ([`vault_cli`]): an admin's approval of
/// one `age1…` key under a machine name, a join of a lab profile's own vault file, pinned to its
/// signer as `<file>#<signer>` or not (never a URL, so never the live ALab vault), and `status`.
fn admitted_secrets(args: &[String]) -> bool {
    match args {
        [verb] => verb == "status",
        [verb, vault] => {
            let (file, signer) = vault.split_once('#').unwrap_or((vault, ""));
            verb == "join"
                && Path::new(file).file_name() == Some(std::ffi::OsStr::new("fixture.vault"))
                && Path::new(file).is_absolute()
                && std::fs::symlink_metadata(file).is_ok_and(|meta| meta.is_file())
                && signer
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        }
        [verb, key, flag, _] => verb == "approve" && key.starts_with("age1") && flag == "--name",
        _ => false,
    }
}

/// Run the shipped vault CLI in a prepared profile: only what [`admitted_secrets`] names.
fn vault_cli(aterm: &Path, root: &Path, args: &[String], live: bool) -> Res {
    if !admitted_secrets(args) {
        return Err(Fail::usage(
            "commons-lan secrets accepts `approve <age1…> --name <host>`, `join <lab \
             profile>/fixture.vault` and `status` only",
        ));
    }
    let (world, _) = open(root, live)?;
    let output = world_output(
        aterm,
        &world,
        "secrets",
        args,
        aterm_uds::spawnfd::Stdio::null(),
    )?;
    std::io::stdout().write_all(&output.stdout).map_err(fail)?;
    std::io::stderr().write_all(&output.stderr).map_err(fail)?;
    Ok(output.status.code().unwrap_or(1))
}

/// Run the shipped database CLI against a prepared profile, including while its
/// app is stopped. Only the commands [`admitted_db`] names pass through this entry point.
fn database(aterm: &Path, root: &Path, args: &[String], live: bool) -> Res {
    if !admitted_db(args) {
        return Err(Fail::usage(
            "commons-lan db accepts proposal/data commands, `alab import|check` and `emails` only",
        ));
    }
    let (world, _) = open(root, live)?;
    let output = db_output(aterm, &world, args, aterm_uds::spawnfd::Stdio::null())?;
    std::io::stdout().write_all(&output.stdout).map_err(fail)?;
    std::io::stderr().write_all(&output.stderr).map_err(fail)?;
    Ok(output.status.code().unwrap_or(1))
}

/// Prepare and drive preserved private profiles without changing installed state.
pub fn cmd(args: &[String]) -> Res {
    stop::install();
    let live = args.iter().any(|arg| arg == LIVE);
    let args: Vec<String> = args.iter().filter(|arg| *arg != LIVE).cloned().collect();
    let value = match args.as_slice() {
        [verb, root] if verb == "prepare-recipient" => prepare_recipient(Path::new(root))?,
        [verb, root, recipients @ ..] if verb == "prepare-source" => {
            prepare_source(Path::new(root), recipients)?
        }
        [verb, root, capsule] if verb == "join" => {
            let bytes = aterm_secrets::fsx::read_private(Path::new(capsule), CAPSULE_LIMIT)
                .map_err(fail)?
                .ok_or_else(|| Fail::die("synthetic vault capsule file is missing"))?;
            join_recipient(Path::new(root), bytes.as_slice())?
        }
        [verb, aterm, root, rest @ ..] if verb == "db" => {
            return database(Path::new(aterm), Path::new(root), rest, live);
        }
        [verb, aterm, root, rest @ ..] if verb == "secrets" => {
            return vault_cli(Path::new(aterm), Path::new(root), rest, live);
        }
        [verb, aterm, root, seconds] if verb == "run" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("run needs a whole number of seconds"));
            }
            return run(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                None,
                live,
                None,
            );
        }
        [verb, aterm, root, seconds] if verb == "node" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("node needs a whole number of seconds"));
            }
            return node(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                live,
            );
        }
        [verb, aterm, root, seconds] if verb == "pair" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("pair needs a whole number of seconds"));
            }
            return pair(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                live,
            );
        }
        [verb, aterm, root, seconds, owner] if verb == "follow" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit())
                || !owner.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Fail::usage("follow needs whole seconds and an owner pid"));
            }
            let owner: u32 = owner.parse().map_err(fail)?;
            if owner == 0 {
                return Err(Fail::usage("follow needs a nonzero owner pid"));
            }
            return run(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                Some(owner),
                live,
                None,
            );
        }
        [verb, run] if verb == "stop" => {
            request_stop(Path::new(run))?;
            json!({"stop_requested":true})
        }
        [verb, root] if verb == "declare" => {
            let (world, role) = open(Path::new(root), live)?;
            if role != "source" {
                return Err(Fail::die(
                    "only the prepared source declares this test table",
                ));
            }
            let config = world.cfg.join("aterm/aterm.toml");
            let observation =
                aterm_db::network_state::read_live_observation(&config).map_err(fail)?;
            if observation.visibility != Visibility::Active {
                return Err(Fail::die("declare requires a running ordinary host"));
            }
            let paths = Paths::for_home(&world.home);
            let table = Table::application(&paths, APPLICATION, "items")?;
            table.publish(&paths)?;
            json!({"table":table.id(),"declared":true})
        }
        [verb, root, key] if verb == "write" => {
            let (world, role) = open(Path::new(root), live)?;
            let paths = Paths::for_home(&world.home);
            let table = Table::application(&paths, APPLICATION, "items")?;
            if role == "recipient" {
                table_snapshot(&world)?;
            }
            let (operation, value) = table.write(&paths, key)?;
            json!({"table":table.id(),"operation":hex::encode_lower(&operation),"value":value})
        }
        [verb, root] if verb == "read" => {
            let (world, _) = open(Path::new(root), live)?;
            table_snapshot(&world)?
        }
        [verb, root, counter, since, until] if verb == "claim" => {
            claims::write(Path::new(root), counter, Some(since.as_str()), until, live)?
        }
        [verb, root, counter, until] if verb == "release" => {
            claims::write(Path::new(root), counter, None, until, live)?
        }
        [verb, root] if verb == "claims" => claims::read(Path::new(root), live)?,
        [verb, root, seed] if verb == "emails-fixture" => {
            let (world, _) = open(Path::new(root), live)?;
            emails::fixture(&world, seed)?
        }
        [verb, aterm, root, seed, row, column, value] if verb == "emails-set" => {
            let (world, _) = open(Path::new(root), live)?;
            emails::set(Path::new(aterm), &world, seed, row, column, value)?
        }
        [verb, aterm, root, digest, seconds] if verb == "emails-wait" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("emails-wait needs a whole number of seconds"));
            }
            let (world, _) = open(Path::new(root), live)?;
            let (value, code) = emails::wait(
                Path::new(aterm),
                &world,
                digest,
                seconds.parse().map_err(fail)?,
            )?;
            emit(&value)?;
            return Ok(code);
        }
        #[cfg(debug_assertions)]
        [verb, aterm, root, seed, tags @ ..] if verb == "emails-reveal" => {
            let (world, _) = prepare_run(Path::new(root), live)?;
            let (value, code) = emails::reveal(Path::new(aterm), &world, seed, tags)?;
            emit(&value)?;
            return Ok(code);
        }
        #[cfg(not(debug_assertions))]
        [verb, ..] if verb == "emails-reveal" => {
            return Err(Fail::die(
                "emails-reveal needs a debug lab image, as reveal-tab does",
            ));
        }
        _ => return Err(Fail::usage(USAGE)),
    };
    emit(&value)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn failed_node_reporting_still_stops_the_owned_child() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let report = dir.path().join("started.json");
        write_new(&report, b"already published").unwrap();
        let (result, mut input) = {
            let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
            let mut owned = OwnedNode(
                aterm_uds::spawnfd::command(cat)
                    .stdin(aterm_uds::spawnfd::Stdio::piped())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let input = aterm_uds::deadline::cat_echoes(&mut owned.0);
            (write_new(&report, b"replacement"), input)
        };
        assert!(result.is_err(), "the phase publication must fail");
        assert!(
            input.write_all(b"still running?\n").is_err(),
            "the child must be stopped even though its stdin remains held"
        );
        assert_eq!(std::fs::read(&report).unwrap(), b"already published");
    }

    #[test]
    fn database_cli_refuses_authority_and_arbitrary_commands_before_launch() {
        for verb in [
            "accounts-authorize",
            "accounts-intent",
            "accounts-request",
            "resources-reserve",
            "table",
            "init",
            "anything",
        ] {
            let error = database(
                Path::new("missing-executable"),
                Path::new("missing-world"),
                &[verb.into()],
                false,
            )
            .unwrap_err();
            assert_eq!(error.code, 2, "{verb} must be a usage refusal before I/O");
        }
    }

    /// The vault CLI passes an admin's approval of a key, a join of a lab profile's own vault
    /// file and `status`, and refuses before any I/O every other verb, a join of the live ALab's
    /// URL or of no file, and a write of a secret.
    #[test]
    fn vault_cli_passes_only_a_labs_own_membership_verbs() {
        let parent = crate::instance::world_parent();
        let vault = parent.path().join("fixture.vault");
        std::fs::write(&vault, b"").unwrap();
        let vault = vault.to_string_lossy().into_owned();
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
        for passed in [
            words(&["status"]),
            words(&["approve", "age1qqqq", "--name", "m4"]),
            words(&["join", &vault]),
            words(&[
                "join",
                &format!("{vault}#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo="),
            ]),
        ] {
            assert!(admitted_secrets(&passed), "{passed:?}");
        }
        for refused in [
            words(&["join"]),
            words(&["join", aterm_secrets::ALAB_DEFAULT_URL]),
            words(&["join", &format!("{}#x", aterm_secrets::ALAB_DEFAULT_URL)]),
            words(&["join", &format!("{vault}#x y")]),
            words(&["join", "fixture.vault"]),
            words(&["join", "/no/such/place/fixture.vault"]),
            words(&["approve", "--all"]),
            words(&["approve", "age1qqqq"]),
            words(&["set", "name"]),
            words(&["edit", "name"]),
            words(&["revoke", "m2"]),
        ] {
            assert!(!admitted_secrets(&refused), "{refused:?}");
            let error = vault_cli(
                Path::new("missing-executable"),
                Path::new("missing-world"),
                &refused,
                false,
            )
            .unwrap_err();
            assert_eq!(
                error.code, 2,
                "{refused:?} must be a usage refusal before I/O"
            );
        }
    }

    #[test]
    fn successive_launches_share_the_profile_without_reusing_app_logs() {
        let parent = crate::instance::world_parent();
        let root = parent.path().join("recipient");
        prepare_recipient(&root).unwrap();
        let (profile, _) = open(&root, false).unwrap();
        let retained = root.join("aterm.log");
        std::fs::write(&retained, b"earlier profile output").unwrap();
        let (first, _) = prepare_run(&root, false).unwrap();
        let first_log = first.dir.join("aterm.log");
        write_new(&first_log, b"first app output").unwrap();
        let (second, _) = prepare_run(&root, false).unwrap();
        assert_ne!(first.dir, second.dir);
        assert_eq!(first.home, profile.home);
        assert_eq!(second.home, profile.home);
        assert_eq!(first.cfg, second.cfg);
        assert_eq!(first.run, second.run);
        write_new(&second.dir.join("aterm.log"), b"second app output").unwrap();
        drop(first);
        drop(second);
        assert_eq!(std::fs::read(&first_log).unwrap(), b"first app output");
        assert_eq!(std::fs::read(&retained).unwrap(), b"earlier profile output");
    }

    /// A world pinned to the live default ALab vault (as an earlier lab run left one) opens for no
    /// verb without the flag that names that vault and the member it keeps, while a synthetic
    /// vault's world opens as before. The live pin is written as a file: nothing is fetched.
    /// Negative control: disabling `open`'s live-vault refusal opens the live world without the
    /// flag and fails the refusal's `let`-`else` (run 2026-11-02, then restored).
    #[test]
    fn a_world_on_the_live_vault_opens_only_behind_the_flag() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let parent = crate::instance::world_parent();
        let synthetic = parent.path().join("synthetic");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&synthetic).unwrap();
        let prepared =
            prepare_source(&source, &[pending["recipient"].as_str().unwrap().into()]).unwrap();
        join_recipient(
            &synthetic,
            &aterm_json::to_vec(&prepared["capsules"][0]).unwrap(),
        )
        .unwrap();
        assert!(
            open(&synthetic, false).is_ok(),
            "a synthetic vault's world opens"
        );

        let live = parent.path().join("live");
        prepare_recipient(&live).unwrap();
        let pin = format!(
            "url = \"{}\"\nname = \"alab\"\nsigner = \"{}\"\nfollowed = []\nhost = \"lab-auto-test\"\n",
            aterm_secrets::ALAB_DEFAULT_URL,
            aterm_secrets::ALAB_SIGNER
        );
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(Paths::for_home(&live.join("home")).dir.join("source.toml"))
            .unwrap()
            .write_all(pin.as_bytes())
            .unwrap();
        let configured = client::config(&Paths::for_home(&live.join("home")))
            .unwrap()
            .expect("the live pin reads back");
        assert!(client::alab_vault(&configured));
        let Err(refused) = open(&live, false) else {
            panic!("a live world needs the flag");
        };
        assert_eq!(refused.code, 1);
        assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
        let refused = database(
            Path::new("missing-executable"),
            &live,
            &[
                "accounts-scan".into(),
                "--collection".into(),
                "accounts".into(),
            ],
            false,
        )
        .unwrap_err();
        assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
        assert!(open(&live, true).is_ok());
    }

    #[test]
    fn prepared_recipient_has_no_membership_records_or_application_table() {
        let parent = crate::instance::world_parent();
        let recipient = parent.path().join("recipient");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&recipient).unwrap();
        let prepared =
            prepare_source(&source, &[pending["recipient"].as_str().unwrap().into()]).unwrap();
        let capsule = aterm_json::to_vec(&prepared["capsules"][0]).unwrap();
        let wrong = parent.path().join("wrong-recipient");
        prepare_recipient(&wrong).unwrap();
        assert!(join_recipient(&wrong, &capsule).is_err());
        assert!(!wrong.join("fixture.vault").exists());
        let joined = join_recipient(&recipient, &capsule).unwrap();
        let source_bootstrap = Bootstrap::required(&Paths::for_home(&source.join("home"))).unwrap();
        let recipient_bootstrap =
            Bootstrap::required(&Paths::for_home(&recipient.join("home"))).unwrap();
        assert_eq!(
            source_bootstrap.trust().network,
            recipient_bootstrap.trust().network
        );
        assert_eq!(joined["commons_admitted"], json!(false));
        assert_eq!(joined["membership_records"], json!(false));
        assert_eq!(joined["application_tables"], json!(0));
        assert!(join_recipient(&recipient, &capsule).is_err());
        assert_eq!(
            recipient_key(&Paths::for_home(&recipient.join("home"))).unwrap(),
            pending["recipient"].as_str().unwrap()
        );
    }

    #[test]
    fn startup_observer_cannot_recreate_missing_custody() {
        let parent = crate::instance::world_parent();
        let recipient = parent.path().join("recipient");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&recipient).unwrap();
        prepare_source(&source, &[pending["recipient"].as_str().unwrap().into()]).unwrap();
        let paths = Paths::for_home(&source.join("home"));
        drop(Bootstrap::required(&paths).unwrap());
        assert!(observed_admission(&paths, Instant::now() + HANG));
        let lock = paths.dir.join("commons.lock");
        std::fs::remove_file(&lock).unwrap();

        assert!(!observed_admission(&paths, Instant::now() + HANG));
        assert!(!lock.exists(), "observing startup must not prepare custody");

        // The old probe is the negative control: merely asking it to read the
        // same prepared source recreates the missing custody lock.
        drop(Bootstrap::required(&paths).unwrap());
        assert!(lock.exists());
        assert!(observed_admission(&paths, Instant::now() + HANG));
    }
}
