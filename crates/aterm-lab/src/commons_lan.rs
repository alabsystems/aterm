// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary hosts on separate physical Macs, with explicit synthetic prior vault approval.
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
use std::path::Path;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);
const CAPSULE_LIMIT: usize = 32 * 1024;
const APPLICATION: &str = "lan-startup-data";
const USAGE: &str = "commons-lan prepare-recipient <new-world> | prepare-source <new-world> <recipient-key> | join <world> <capsule-file> | run <aterm> <world> <seconds> | follow <aterm> <world> <seconds> <owner-pid> | stop <run-directory> | declare <world> | write <world> <key> | read <world>";

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(error.to_string())
}

fn emit(value: &Value) -> Result<(), Fail> {
    println!("{}", aterm_json::to_string(value).map_err(fail)?);
    std::io::stdout().flush().map_err(fail)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(fail)?;
    file.write_all(bytes).map_err(fail)
}

fn prepare(root: &Path, role: &str) -> Result<World, Fail> {
    let config = format!("{CONFIG_OFF}\n[ai]\nenabled = false\n[fleet]\nwatch = false\n");
    let world = World::create_at(root, &config).map_err(fail)?;
    write_new(&world.dir.join("commons-lan-role"), role.as_bytes())?;
    Ok(world)
}

fn open(root: &Path) -> Result<(World, String), Fail> {
    let world = World::open_at(root).map_err(fail)?;
    let role = private_file::read_private(&world.dir, "commons-lan-role", 16).map_err(fail)?;
    let role = std::str::from_utf8(&role).map_err(fail)?;
    if !matches!(role, "source" | "recipient") {
        return Err(Fail::die("not a prepared Commons LAN profile"));
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

fn prepare_source(root: &Path, recipient: &str) -> Result<Value, Fail> {
    let world = prepare(root, "source")?;
    let paths = Paths::for_home(&world.home);
    let vault = world.dir.join("fixture.vault");
    let created =
        client::create(&paths, &vault.to_string_lossy(), None, Some("m1")).map_err(fail)?;
    let admin = client::admin_open(&paths).map_err(fail)?;
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", recipient).map_err(fail)?;
    client::admin_publish_approved(&paths, &admin, &doc, &[recipient.into()]).map_err(fail)?;
    let genesis =
        client::seed_vault_genesis(&paths, &world.dir.join("fixture.genesis")).map_err(fail)?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o600)).map_err(fail)?;
    let bytes = aterm_secrets::fsx::read_private(&vault, CAPSULE_LIMIT)
        .map_err(fail)?
        .ok_or_else(|| Fail::die("synthetic vault publication is missing"))?;
    let capsule = json!({"schema":1,"signer":created.signer,"recipient":recipient,
        "vault_hex":hex::encode_lower(bytes.as_slice())});
    if aterm_json::to_vec(&capsule).map_err(fail)?.len() > CAPSULE_LIMIT {
        return Err(Fail::die(
            "synthetic vault capsule exceeds its transfer bound",
        ));
    }
    Ok(
        json!({"schema":1,"role":"source","world":world.dir.display().to_string(),
        "genesis":hex::encode_lower(&genesis.digest),"genesis_members":genesis.members,
        "capsule":capsule}),
    )
}

fn join_recipient(root: &Path, capsule: &[u8]) -> Result<Value, Fail> {
    let (world, role) = open(root)?;
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
            host: Some("m2".into()),
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

fn prepare_run(root: &Path) -> Result<(World, String), Fail> {
    let (mut world, role) = open(root)?;
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

fn run(aterm: &Path, root: &Path, seconds: u64, follows: Option<u32>) -> Res {
    if !(60..=600).contains(&seconds) {
        return Err(Fail::usage("commons-lan run takes 60 to 600 seconds"));
    }
    let aterm = std::fs::canonicalize(aterm).map_err(fail)?;
    let (world, role) = prepare_run(root)?;
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
    if follows == Some(pid as u32) {
        return Err(Fail::die("a follower must name another process"));
    }
    let started = json!({"phase":"started","role":role,"run":run.display().to_string(),
        "aterm_pid":pid,"keeper_pid":instance.pid,"follows_pid":follows,
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
        let admitted = Bootstrap::required(&Paths::for_home(&world.home)).is_ok_and(|bootstrap| {
            bootstrap.is_log_backed().unwrap_or(false)
                && bootstrap
                    .log_witness()
                    .is_ok_and(|witness| witness.is_some())
                && bootstrap.access().is_ok_and(|access| access.is_some())
        });
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
            let image = run.join("unavailable.png");
            let _ = instance.ctl(&["image", &image.to_string_lossy()]);
            return Err(Fail::die(
                "ordinary physical LAN host did not become active",
            ));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    let until = Instant::now() + Duration::from_secs(seconds);
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

/// Prepare and drive preserved private profiles without changing installed state.
pub fn cmd(args: &[String]) -> Res {
    stop::install();
    let value = match args {
        [verb, root] if verb == "prepare-recipient" => prepare_recipient(Path::new(root))?,
        [verb, root, recipient] if verb == "prepare-source" => {
            prepare_source(Path::new(root), recipient)?
        }
        [verb, root, capsule] if verb == "join" => {
            let bytes = aterm_secrets::fsx::read_private(Path::new(capsule), CAPSULE_LIMIT)
                .map_err(fail)?
                .ok_or_else(|| Fail::die("synthetic vault capsule file is missing"))?;
            join_recipient(Path::new(root), bytes.as_slice())?
        }
        [verb, aterm, root, seconds] if verb == "run" => {
            return run(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                None,
            );
        }
        [verb, aterm, root, seconds, owner] if verb == "follow" => {
            let owner: u32 = owner.parse().map_err(fail)?;
            if owner == 0 {
                return Err(Fail::usage("follow needs a nonzero owner pid"));
            }
            return run(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                Some(owner),
            );
        }
        [verb, run] if verb == "stop" => {
            let run = Path::new(run);
            let marker = private_file::read_private(run, "lab-run", 64).map_err(fail)?;
            if marker.as_slice() != b"commons-lan-run/1\n" {
                return Err(Fail::die("not an owned Commons LAN run"));
            }
            write_new(&run.join("stop"), b"")?;
            json!({"stop_requested":true})
        }
        [verb, root] if verb == "declare" => {
            let (world, role) = open(Path::new(root))?;
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
            let (world, role) = open(Path::new(root))?;
            let paths = Paths::for_home(&world.home);
            let table = Table::application(&paths, APPLICATION, "items")?;
            if role == "recipient" {
                table_snapshot(&world)?;
            }
            let (operation, value) = table.write(&paths, key)?;
            json!({"table":table.id(),"operation":hex::encode_lower(&operation),"value":value})
        }
        [verb, root] if verb == "read" => {
            let (world, _) = open(Path::new(root))?;
            table_snapshot(&world)?
        }
        _ => return Err(Fail::usage(USAGE)),
    };
    emit(&value)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successive_launches_share_the_profile_without_reusing_app_logs() {
        let parent = crate::instance::world_parent();
        let root = parent.path().join("recipient");
        prepare_recipient(&root).unwrap();
        let (profile, _) = open(&root).unwrap();
        let retained = root.join("aterm.log");
        std::fs::write(&retained, b"earlier profile output").unwrap();
        let (first, _) = prepare_run(&root).unwrap();
        let first_log = first.dir.join("aterm.log");
        write_new(&first_log, b"first app output").unwrap();
        let (second, _) = prepare_run(&root).unwrap();
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

    #[test]
    fn prepared_recipient_has_no_membership_records_or_application_table() {
        let parent = crate::instance::world_parent();
        let recipient = parent.path().join("recipient");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&recipient).unwrap();
        let prepared = prepare_source(&source, pending["recipient"].as_str().unwrap()).unwrap();
        let capsule = aterm_json::to_vec(&prepared["capsule"]).unwrap();
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
}
