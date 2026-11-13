// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two real private instances: a signed rename onto a sheet number, its registry merge,
//! membership display and remote resolution. Synthetic registration probes; no system rename
//! or live state.

use aterm_db::{
    alab::{Registry, register::Probe, registry::MACHINE},
    table::fixture::{Lab, repair_commons},
};
use aterm_json::{Value, json};
use aterm_secrets::{client, commons::roster::Role};
use aterm_types::machine_name::Number;

use crate::{
    Fail, Res,
    instance::{BOOT, CONFIG_OFF, Instance, Launch, World},
};

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(format!("machine-labels: {error}"))
}

fn check(reply: crate::instance::Reply) -> Result<String, Fail> {
    if reply.code == 0 {
        Ok(reply.stdout)
    } else {
        Err(fail(format!(
            "exit {}: {}{}",
            reply.code, reply.stdout, reply.stderr
        )))
    }
}

fn cli(world: &World, aterm: &std::path::Path, args: &[&str]) -> Result<String, Fail> {
    let mut command = aterm_uds::spawnfd::command(aterm);
    command
        .env_clear()
        .envs(environment(world))
        .current_dir(&world.home)
        .args(args);
    let output = aterm_uds::deadline::output_within(&mut command, BOOT).map_err(fail)?;
    if !output.status.success() {
        return Err(fail(format!(
            "{:?}: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn environment(world: &World) -> Vec<(String, String)> {
    let mut env = world.env();
    env.extend([
        (aterm_net::loopback::SEAM.into(), "1".into()),
        ("USER".into(), "lab".into()),
        ("LOGNAME".into(), "lab".into()),
    ]);
    env
}

fn start(world: &World, aterm: &std::path::Path) -> Result<Instance, Fail> {
    let mut instance = Instance::start(
        world,
        Launch {
            aterm: aterm.to_owned(),
            headless: true,
            args: vec!["-e".into(), "/bin/sh".into()],
            env: environment(world),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    instance.await_socket(BOOT)?;
    Ok(instance)
}

fn write_cell(pk: &str, col: &str, x: Value) -> Result<aterm_db::database::Change, Fail> {
    use aterm_db::typed::cell::{self, CellKey, CellValue};
    cell::change(
        &MACHINE,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::owner(MACHINE.version, x),
    )
    .map_err(fail)
}

/// Run a functional drive, printing the actual fleet, members and --host replies.
/// # Errors
/// Usage, a private instance or fixture cannot start, a rename or merge is refused, or any
/// real command fails its expected identity check.
pub fn cmd(args: &[String]) -> Res {
    if !args.is_empty() {
        return Err(Fail::usage("machine-labels"));
    }
    crate::stop::install();
    let aterm = crate::paths::paths()
        .need_aterm()
        .map_err(fail)?
        .to_path_buf();
    let lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
    client::seed_vault_genesis(&lab.paths[0], &lab.paths[0].home.join("genesis")).map_err(fail)?;
    client::admin_mirror_log(&lab.paths[0], &[]).map_err(fail)?;
    let initial = client::load_records(&lab.paths[0])
        .map_err(fail)?
        .log()
        .ok_or_else(|| fail("founder has no log"))?
        .records()
        .cloned()
        .collect();
    client::ingest_mesh_records(&lab.paths[1], initial).map_err(fail)?;
    client::sync_records(&lab.paths[1], None).map_err(fail)?;
    let mut first = World::new(CONFIG_OFF).map_err(fail)?;
    let mut second = World::new(CONFIG_OFF).map_err(fail)?;
    first.home.clone_from(&lab.paths[0].home);
    second.home.clone_from(&lab.paths[1].home);
    let worlds = [&first, &second];
    let mut probes = Vec::new();
    for (n, world) in worlds.iter().enumerate() {
        crate::util::create_private_dir_all(&world.state()).map_err(fail)?;
        crate::util::create_private_dir_all(&world.home.join("tmp")).map_err(fail)?;
        let node = aterm_net::fleet_member::node_identity(&world.state()).map_err(fail)?;
        client::publish_local_node_key(&lab.paths[n], *node.key().bytes(), |network, actor| {
            let mut body = network.to_vec();
            body.extend_from_slice(actor);
            node.sign_record("membership node", &body)
        })
        .map_err(fail)?;
        probes.push(Probe {
            hw_model: "Mac16,8".into(),
            node: Some(aterm_net::fleet_member::node_principal(node.key())[2..].into()),
            hosts: vec![format!("m{}", n + 1)],
            platform_uuid: format!("synthetic-machine-{n}"),
            ..Probe::default()
        });
    }
    // Deliver only authenticated membership records, as the control exchange does.
    for n in [0, 1] {
        let records = client::load_records(&lab.paths[1 - n])
            .map_err(fail)?
            .log()
            .ok_or_else(|| fail("no log"))?
            .records()
            .cloned()
            .collect();
        client::ingest_mesh_records(&lab.paths[n], records).map_err(fail)?;
        client::sync_records(&lab.paths[n], None).map_err(fail)?;
    }
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().map_err(fail)?;
    let now = aterm_time::unix_millis().unwrap_or(1);
    registry
        .register(&first.cfg.join("aterm/aterm.toml"), &probes[0], true, now)
        .map_err(fail)?;
    let sheet = "mc-ce20a1499bba52af";
    registry
        .write(vec![vec![
            write_cell(sheet, "label", json!("m3"))?,
            write_cell(sheet, "row", json!("live"))?,
            write_cell(sheet, "status", json!("planned"))?,
        ]])
        .map_err(fail)?;
    repair_commons(&lab, 6);

    let ports = crate::fleet_world::free_ports(2).map_err(fail)?;
    cli(
        &first,
        &aterm,
        &[
            "fleet",
            "init",
            "--host",
            "127.0.0.1",
            "--port",
            &ports[0].to_string(),
            "--allow",
            "content",
        ],
    )?;
    let mut one = start(&first, &aterm)?;
    let deadline = std::time::Instant::now() + BOOT;
    while std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, ports[0])).is_err() {
        if std::time::Instant::now() >= deadline {
            return Err(fail("the private fleet listener did not start"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let code = first.state().join(aterm_net::fleet_member::CODE_FILE);
    cli(
        &second,
        &aterm,
        &[
            "fleet",
            "join",
            "--code-file",
            &code.to_string_lossy(),
            "--at",
            &format!("127.0.0.1:{}", ports[0]),
            "--host",
            "127.0.0.1",
            "--port",
            &ports[1].to_string(),
            "--allow",
            "content",
        ],
    )?;
    let mut two = start(&second, &aterm)?;
    let result = drive(&lab, &first, &one, &two, &registry, &probes[0], now);
    let stopped_one = one.stop().map_err(fail);
    let stopped_two = two.stop().map_err(fail);
    result?;
    stopped_one?;
    stopped_two?;
    println!(
        "PASS: two private members, chosen m3, bound sheet row, remote resolution and survey; no system-name writes"
    );
    Ok(0)
}

fn drive(
    lab: &Lab,
    first: &World,
    one: &Instance,
    two: &Instance,
    registry: &Registry,
    probe: &Probe,
    now: u64,
) -> Result<(), Fail> {
    // A grant says who may drive; each lab Mac's fleet access says how far, and starts Read Only.
    for mac in [one, two] {
        check(mac.cli(None, &["fleet", "allow", "drive", "--all"]))?;
        check(mac.cli(None, &["fleet", "access", "full"]))?;
    }
    let number = Number::parse("m3").map_err(fail)?;
    let to = aterm_agent::machine_name::NameTo {
        num: number,
        nick: None,
    };
    aterm_agent::machine_name::set_name(
        &first.state(),
        &to,
        || client::lab_labels(&lab.paths[0]).map_err(std::io::Error::other),
        aterm_net::fleet_member::name_record::ByKind::Agent,
        || Ok(Some("m1".into())),
        now,
        false,
    )
    .map_err(fail)?;
    // Publish with the real device key; the functional drive deliberately injects no OS setter.
    client::publish_local_chosen_name(&lab.paths[0], number).map_err(fail)?;
    let config = first.cfg.join("aterm/aterm.toml");
    let policy = aterm_agent::machine_name::publish::Policy::read(&config, number).map_err(fail)?;
    aterm_agent::machine_name::publish::publish(&first.state(), &config, &policy, None)
        .map_err(|e| fail(e.why()))?;
    registry.register(&config, probe, true, now).map_err(fail)?;
    println!(
        "merge preview: {}",
        check(one.cli(None, &["db", "alab", "merge"]))?
    );
    println!(
        "merge applied: {}",
        check(one.cli(None, &["db", "alab", "merge", "--apply"]))?
    );
    client::admin_mirror_log(&lab.paths[0], &[]).map_err(fail)?;
    client::sync_records(&lab.paths[1], None).map_err(fail)?;
    repair_commons(lab, 6);
    for (word, instance) in [("renamed", one), ("observer", two)] {
        let fleet = check(instance.cli(None, &["fleet"]))?;
        if !fleet.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some("m3") && fields.next() == Some("m3")
        }) {
            return Err(fail(format!(
                "{word} fleet omitted the m3 label and name: {fleet}"
            )));
        }
        println!("{word} fleet:\n{fleet}");
        let members = check(instance.cli(None, &["members", "list"]))?;
        if !members.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields.next() == Some("m3") && fields.next() == Some("m3")
        }) {
            return Err(fail(format!("{word} members omitted m3: {members}")));
        }
        println!("{word} members:\n{members}");
    }
    let resolved = two.cli(None, &["ctl", "--host", "m3", "version"]);
    let expected = format!(
        "target=m3 node=n-{}",
        probe
            .node
            .as_deref()
            .ok_or_else(|| fail("missing synthetic node"))?
    );
    if !resolved.stderr.contains(&expected) {
        return Err(fail(format!(
            "--host m3 resolved another node: {}",
            resolved.stderr
        )));
    }
    println!(
        "observer --host m3 version:\n{}{}",
        resolved.stderr,
        check(resolved.clone())?
    );
    let survey = check(two.cli(None, &["ctl", "survey", "--host", "m3"]))?;
    if !survey.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some("==") && fields.next() == Some("fleet") && fields.next() == Some("m3")
    }) {
        return Err(fail(format!("survey omitted the m3 member: {survey}")));
    }
    println!("observer survey m3:\n{survey}");
    Ok(())
}
