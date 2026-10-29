// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary executable startup, enrollment and application repair on private custody.
//! The lab never constructs a Runtime or drives a repair exchange.

use crate::instance::{BOOT, CONFIG_OFF, Instance, Launch, World};
use crate::{Fail, Res, stop};
use astream_wire::sync::Digest;
use aterm_db::accounts::proposals::{Proposals, Snapshot};
use aterm_db::database::Change;
use aterm_net::lan::{Multicast, Scope};
use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_types::network_state::Visibility;
use aterm_uds::spawnfd::{Child, Stdio};
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::{Duration, Instant};

mod table;

const HANG: Duration = Duration::from_secs(60);
const USAGE: &str = "commons-startup <aterm> <new-output-directory> [--log-backed] [--suppressed | --observe SECONDS]";

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(error.to_string())
}

fn environment(world: &World, port: u16) -> Vec<(String, String)> {
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    env.push((aterm_net::loopback::SEAM.into(), "1".into()));
    env.push((
        aterm_db::runtime::LOOPBACK_PORT_ENV.into(),
        port.to_string(),
    ));
    env
}

fn keep(world: &mut World) {
    world.keep();
    println!("commons-startup profile={}", world.dir.display());
}

fn save(out: &Path, name: &str, value: &aterm_json::Value) -> Result<(), Fail> {
    let mut file = std::fs::File::create_new(out.join(name)).map_err(fail)?;
    file.write_all(&aterm_json::to_vec(value).map_err(fail)?)
        .map_err(fail)
}

fn require_attachment(world: &World, aterm: &Path, port: u16, out: &Path) -> Result<(), Fail> {
    let mut command = aterm_uds::spawnfd::command(aterm);
    command
        .env_clear()
        .envs(environment(world, port))
        .current_dir(&world.home)
        .args(["db", "runtime-isolation"]);
    let reply = aterm_uds::deadline::output_within(&mut command, HANG).map_err(fail)?;
    let value = aterm_json::from_slice::<aterm_json::Value>(&reply.stdout).ok();
    if !reply.status.success()
        || value.as_ref().is_none_or(|value| {
            value["loopback_only"].as_bool() != Some(true)
                || value["port"].as_u64() != Some(u64::from(port))
        })
    {
        return Err(Fail::die(
            "commons-startup requires an executable that acknowledges the guarded loopback attachment; no runtime was started",
        ));
    }
    save(
        out,
        "attachment.json",
        &value.expect("acknowledged attachment"),
    )
}

struct Node(Child);

impl Node {
    fn start(world: &World, aterm: &Path, port: u16) -> Result<Self, Fail> {
        let log = std::fs::File::create_new(world.dir.join("node.log")).map_err(fail)?;
        let err = log.try_clone().map_err(fail)?;
        let child = aterm_uds::spawnfd::command(aterm)
            .env_clear()
            .envs(environment(world, port))
            .current_dir(&world.home)
            .args(["node", "serve"])
            .setsid(true)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(fail)?;
        println!("commons-startup node_pid={}", child.id());
        Ok(Self(child))
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.0, HANG);
    }
}

fn start_window(
    world: &World,
    aterm: &Path,
    port: u16,
    suppressed: bool,
) -> Result<Instance, Fail> {
    let launch = Launch {
        aterm: aterm.to_owned(),
        headless: false,
        args: vec!["-e".into(), "/bin/sh".into()],
        env: environment(world, port),
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    };
    let mut instance = if suppressed {
        Instance::start(world, launch)
    } else {
        Instance::start_ordinary(world, launch)
    }
    .map_err(fail)?;
    instance.await_socket(BOOT)?;
    println!(
        "commons-startup window_keeper={} window_pid={:?} socket={} suppressed={suppressed}",
        instance.pid,
        instance.aterm_pid(),
        instance.sock.display(),
    );
    Ok(instance)
}

fn active(world: &World, pid: u32, out: &Path, name: &str) -> Result<(), Fail> {
    let config = world.cfg.join("aterm/aterm.toml");
    let until = Instant::now() + HANG;
    loop {
        stop::check()?;
        if let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(pid)
        {
            return save(out, name, &aterm_db::network_state::value(&observation));
        }
        if Instant::now() >= until {
            return Err(Fail::die(format!(
                "ordinary startup has no active Commons observer from pid {pid} for {}",
                world.dir.display(),
            )));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

fn proposal_value(key: &str) -> aterm_json::Value {
    aterm_json::json!({
        "provider": "example",
        "method": "oauth",
        "label": format!("synthetic-commons-startup-{key}"),
    })
}

fn publish(paths: &Paths, request: u8, key: &str) -> Result<(Digest, aterm_json::Value), Fail> {
    let value = proposal_value(key);
    let operation = Proposals::at(paths.clone())
        .write(
            [request; 16],
            vec![Change {
                collection: "accounts".into(),
                key: key.into(),
                value: Some(value.clone()),
            }],
        )
        .map_err(fail)?;
    Ok((operation.id, value))
}

fn contains(snapshot: &Snapshot, operation: Digest, key: &str, value: &aterm_json::Value) -> bool {
    snapshot.withheld == 0
        && snapshot.operations.iter().any(|op| {
            op.header.id == operation && op.available && op.eligible && op.catalog_eligible
        })
        && snapshot
            .cells
            .get(&("accounts".into(), key.into()))
            .is_some_and(|cell| {
                !cell.stopped
                    && cell.candidates.len() == 1
                    && cell.candidates[0].operation == operation
                    && &cell.candidates[0].value == value
            })
}

fn received(
    paths: &Paths,
    operation: Digest,
    key: &str,
    value: &aterm_json::Value,
    out: &Path,
) -> Result<(), Fail> {
    let until = Instant::now() + HANG;
    loop {
        stop::check()?;
        if let Ok(snapshot) = Proposals::at(paths.clone()).snapshot()
            && contains(&snapshot, operation, key, value)
        {
            return save(
                out,
                &format!("{key}.json"),
                &aterm_json::json!({
                    "home": paths.home.display().to_string(),
                    "operation": aterm_codec::hex::encode_lower(&operation),
                    "eligible": true,
                    "decrypted_value": value,
                    "retained": snapshot.retained,
                    "withheld": snapshot.withheld,
                }),
            );
        }
        if Instant::now() >= until {
            return Err(Fail::die(format!(
                "ordinary runtime did not receive signed operation {} ({key})",
                aterm_codec::hex::encode_lower(&operation),
            )));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

fn image(window: &Instance, out: &Path, name: &str) -> Result<(), Fail> {
    let reply = window.ctl(&["image", name]);
    let image = reply.stdout.split_whitespace().nth(3);
    match (reply.code, image) {
        (0, Some(image)) => {
            std::fs::copy(image, out.join(name)).map_err(fail)?;
            Ok(())
        }
        _ => Err(Fail::die(format!(
            "ordinary window image: {}",
            reply.first()
        ))),
    }
}

fn require_enrolled(paths: &Paths, log_backed: bool) -> Result<(), Fail> {
    let enrolled = Bootstrap::required(paths).map_err(fail)?;
    if enrolled.is_log_backed().map_err(fail)? != log_backed
        || (log_backed && enrolled.log_witness().map_err(fail)?.is_none())
    {
        return Err(Fail::die(
            "recipient has no admission in the required control format",
        ));
    }
    let access = enrolled
        .access()
        .map_err(fail)?
        .ok_or_else(|| Fail::die("recipient received no Commons admission"))?;
    if access.local().role() != aterm_secrets::commons::roster::Role::Writer {
        return Err(Fail::die(
            "recipient did not become an approved Commons writer",
        ));
    }
    Ok(())
}

/// Exercise the ordinary executable's automatic runtime, preserving all profiles.
/// `--suppressed` supplies the known private-socket startup suppression as a
/// negative control; it must fail the same active-observer requirement.
/// `--log-backed` seeds a synthetic vault log only on the source. The recipient
/// starts without log records; ordinary enrollment must obtain its membership
/// history before it can repair application data.
/// # Errors
/// Usage or I/O, custody setup, failed host startup, interruption, or an observer,
/// enrollment or exact eligible application value missing after the hang detector.
pub fn cmd(args: &[String]) -> Res {
    let [aterm, out, options @ ..] = args else {
        return Err(Fail::usage(USAGE));
    };
    let (log_backed, options) = match options {
        [flag, options @ ..] if flag == "--log-backed" => (true, options),
        _ => (false, options),
    };
    let (suppressed, observe) = match options {
        [] => (false, 0),
        [flag] if flag == "--suppressed" => (true, 0),
        [flag, seconds] if flag == "--observe" => {
            let seconds = seconds
                .parse::<u64>()
                .ok()
                .filter(|seconds| (1..=60).contains(seconds))
                .ok_or_else(|| Fail::usage("--observe takes 1 to 60 seconds"))?;
            (false, seconds)
        }
        _ => return Err(Fail::usage(USAGE)),
    };
    stop::install();
    for key in ["forward", "reverse", "after-restart"] {
        aterm_db::accounts::validate(&proposal_value(key)).map_err(fail)?;
    }
    let aterm = std::fs::canonicalize(crate::paths::from_caller(aterm)).map_err(fail)?;
    let out = crate::paths::from_caller(out);
    std::fs::create_dir(&out).map_err(fail)?;
    let out = std::fs::canonicalize(out).map_err(fail)?;
    let config = format!("{CONFIG_OFF}\n[ai]\nenabled = false\n[fleet]\nwatch = false\n");
    let mut source = World::new(&config).map_err(fail)?;
    let mut recipient = World::new(&config).map_err(fail)?;
    keep(&mut source);
    keep(&mut recipient);
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        Scope::Loopback,
    )
    .map_err(fail)?;
    let port = reservation.port();
    require_attachment(&source, &aterm, port, &out)?;
    let paths = [
        Paths::for_home(&source.home),
        Paths::for_home(&recipient.home),
    ];

    // This is synthetic prior vault approval, not automatic permission inferred
    // from discovery. No Commons admission or application table is supplied.
    let vault = source.dir.join("startup.vault");
    let created =
        client::create(&paths[0], &vault.to_string_lossy(), None, Some("m1")).map_err(fail)?;
    let approved = client::machine_identity(&paths[1])
        .map_err(fail)?
        .recipient()
        .to_bech32();
    let admin = client::admin_open(&paths[0]).map_err(fail)?;
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &approved).map_err(fail)?;
    client::admin_publish_approved(&paths[0], &admin, &doc, &[approved]).map_err(fail)?;
    client::join(
        &paths[1],
        &JoinOptions {
            source: format!("{}#{}", vault.display(), created.signer),
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
    let genesis = if log_backed {
        let signed =
            client::seed_vault_genesis(&paths[0], &out.join("vault.genesis")).map_err(fail)?;
        if !Bootstrap::required(&paths[0])
            .map_err(fail)?
            .is_log_backed()
            .map_err(fail)?
        {
            return Err(Fail::die(
                "source did not activate the seeded membership log",
            ));
        }
        Some(signed)
    } else {
        None
    };
    let before = Bootstrap::required(&paths[1]).map_err(fail)?;
    if before.witness().map_err(fail)?.is_some()
        || before.log_witness().map_err(fail)?.is_some()
        || before.is_log_backed().map_err(fail)?
        || before.access().is_ok_and(|access| access.is_some())
        || paths[1].records_file().exists()
        || paths[1].dir.join("commons/tables").exists()
    {
        return Err(Fail::die(
            "recipient already has Commons admission, membership records or a table",
        ));
    }
    save(
        &out,
        "before.json",
        &aterm_json::json!({
            "source_profile": source.dir.display().to_string(),
            "recipient_profile": recipient.dir.display().to_string(),
            "vault_approved": true,
            "commons_admitted": false,
            "application_tables": 0,
            "log_backed": log_backed,
            "recipient_log_backed": false,
            "recipient_records_file": false,
            "genesis": genesis.map(|signed| aterm_codec::hex::encode_lower(&signed.digest)),
            "genesis_members": genesis.map(|signed| signed.members),
            "suppressed": suppressed,
        }),
    )?;
    drop(before);

    let node = Node::start(&source, &aterm, port)?;
    active(&source, node.0.id(), &out, "source-active.json")?;
    let window = start_window(&recipient, &aterm, port, suppressed)?;
    image(&window, &out, "initial.png")?;
    let pid = window
        .aterm_pid()
        .ok_or_else(|| Fail::die("window has no child pid"))?;
    active(&recipient, pid as u32, &out, "recipient-active.json")?;

    // The table is first published only after both ordinary executable hosts run.
    // Reads below observe existing authenticated data; none drives a repair.
    let (forward, forward_value) = publish(&paths[0], 1, "forward")?;
    received(&paths[1], forward, "forward", &forward_value, &out)?;
    require_enrolled(&paths[1], log_backed)?;
    let application = table::Table::declare(&paths, &out)?;
    let configs = [
        source.cfg.join("aterm/aterm.toml"),
        recipient.cfg.join("aterm/aterm.toml"),
    ];
    let mut rows = table::Rows::new();
    let (generic_forward, value) = application.write(&paths[0], "forward")?;
    rows.insert(generic_forward, value);
    application.received(&paths[1], &configs[1], &rows, &out, "table-forward.json")?;
    let (generic_reverse, value) = application.write(&paths[1], "reverse")?;
    rows.insert(generic_reverse, value);
    application.received(&paths[0], &configs[0], &rows, &out, "table-reverse.json")?;
    let (reverse, reverse_value) = publish(&paths[1], 2, "reverse")?;
    received(&paths[0], reverse, "reverse", &reverse_value, &out)?;
    drop(window);

    let (after_restart, after_restart_value) = publish(&paths[0], 3, "after-restart")?;
    let (generic_after_restart, value) = application.write(&paths[0], "after-restart")?;
    rows.insert(generic_after_restart, value);
    application.received(
        &paths[0],
        &configs[0],
        &rows,
        &out,
        "table-source-final.json",
    )?;
    // A fresh launch directory preserves the first window's logs while the
    // restarted instance uses exactly the recipient's existing home and config.
    let mut restarted = World::new(&config).map_err(fail)?;
    keep(&mut restarted);
    restarted.home.clone_from(&recipient.home);
    restarted.cfg.clone_from(&recipient.cfg);
    restarted.run.clone_from(&recipient.run);
    let window = start_window(&restarted, &aterm, port, false)?;
    let pid = window
        .aterm_pid()
        .ok_or_else(|| Fail::die("restarted window has no child pid"))?;
    active(&restarted, pid as u32, &out, "restarted-active.json")?;
    received(
        &paths[1],
        after_restart,
        "after-restart",
        &after_restart_value,
        &out,
    )?;
    require_enrolled(&paths[1], log_backed)?;
    application.received(
        &paths[1],
        &configs[1],
        &rows,
        &out,
        "table-recipient-final.json",
    )?;
    image(&window, &out, "restarted.png")?;
    save(
        &out,
        "result.json",
        &aterm_json::json!({
            "passed": true,
            "ordinary_window": true,
            "ordinary_node_serve": true,
            "loopback_only": true,
            "log_backed": log_backed,
            "genesis": genesis.map(|signed| aterm_codec::hex::encode_lower(&signed.digest)),
            "application_table": application.id(),
            "application_rows": rows.len(),
            "fresh_table_after_startup": true,
            "forward": aterm_codec::hex::encode_lower(&forward),
            "reverse": aterm_codec::hex::encode_lower(&reverse),
            "after_restart": aterm_codec::hex::encode_lower(&after_restart),
            "runtime_constructed_by_lab": false,
            "repair_driven_by_lab": false,
        }),
    )?;
    if observe != 0 {
        println!("commons-startup observing the running window for {observe}s");
        let until = Instant::now() + Duration::from_secs(observe);
        while Instant::now() < until {
            stop::sleep(Duration::from_millis(100))?;
        }
        let reply = window.cli(None, &["db", "network-state"]);
        if reply.code != 0 {
            return Err(Fail::die(format!(
                "observed network-state: {}",
                reply.first()
            )));
        }
        let observed = aterm_json::from_str(&reply.stdout).map_err(fail)?;
        save(&out, "observed-live.json", &observed)?;
        image(&window, &out, "observed.png")?;
    }
    println!("commons-startup PASS output={}", out.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_host_environment_keeps_private_roots_without_suppressing_startup() {
        let mut world = World::new(CONFIG_OFF).unwrap();
        world.keep();
        let env = environment(&world, 49152);
        assert!(
            !env.iter()
                .any(|(key, _)| key == "ATERM_STATE_HOME" || key == "CI")
        );
        assert!(
            env.iter()
                .any(|(key, value)| key == "HOME" && Path::new(value) == world.home)
        );
        assert!(
            env.iter()
                .any(|(key, value)| key == "XDG_RUNTIME_DIR" && Path::new(value) == world.run)
        );
        assert!(
            env.iter()
                .any(|(key, value)| key == aterm_net::loopback::SEAM && value == "1")
        );
        assert!(env.iter().any(|(key, value)| key == aterm_db::runtime::LOOPBACK_PORT_ENV && value == "49152"));
    }
}
