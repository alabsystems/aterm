// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A real reader window sees a synthetic fleet peer that has a second instance.
//! Every profile and listener is private; the peer is reached on loopback alone.

use crate::instance::{BOOT, CONFIG_OFF, Instance, Launch, World};
use crate::{Fail, Res};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

fn environment(world: &World) -> Vec<(String, String)> {
    let mut env = world.env();
    env.push((aterm_net::loopback::SEAM.into(), "1".into()));
    env
}

fn cli(world: &World, aterm: &Path, args: &[&str]) -> Result<String, Fail> {
    let mut command = aterm_uds::spawnfd::command(aterm);
    command
        .env_clear()
        .envs(environment(world))
        .current_dir(&world.home)
        .args(args);
    let output = aterm_uds::deadline::output_within(&mut command, HANG)
        .map_err(|e| Fail::die(format!("fleet-menu client: {e}")))?;
    if !output.status.success() {
        return Err(Fail::die(format!(
            "fleet-menu client {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(|e| Fail::die(e.to_string()))
}

fn start(world: &World, aterm: &Path, headless: bool) -> Result<Instance, Fail> {
    let mut instance = Instance::start(
        world,
        Launch {
            aterm: aterm.to_owned(),
            headless,
            args: vec!["-e".into(), "/bin/sh".into()],
            env: environment(world),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(Fail::die)?;
    instance.await_socket(BOOT)?;
    Ok(instance)
}

fn control(world: &World, aterm: &Path, socket: &Path, args: &[&str]) -> Result<String, Fail> {
    let socket = socket.to_string_lossy();
    let mut request = vec!["ctl", "--sock", &socket, "--timeout", "60"];
    request.extend_from_slice(args);
    cli(world, aterm, &request)
}

/// Run one isolated fleet menu observation, preserving every generated profile.
/// # Errors
/// Usage, input or output I/O, failed bring-up or control requests, a peer that
/// reports no sibling, a reader that produces no fleet snapshot within the hang
/// detector, or interruption. Owned instances are stopped before returning.
pub fn cmd(args: &[String]) -> Res {
    let [aterm, out] = args else {
        return Err(Fail::usage("fleet-menu <aterm> <new-output-directory>"));
    };
    crate::stop::install();
    let aterm = std::fs::canonicalize(crate::paths::from_caller(aterm))
        .map_err(|e| Fail::die(e.to_string()))?;
    let out = crate::paths::from_caller(out);
    std::fs::create_dir(&out).map_err(|e| Fail::die(format!("{out}: {e}")))?;
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| Fail::die(e.to_string()))?;
    let out = std::fs::canonicalize(out).map_err(|e| Fail::die(e.to_string()))?;

    let mut peer = World::new(CONFIG_OFF).map_err(Fail::die)?;
    let mut sibling = World::new(CONFIG_OFF).map_err(Fail::die)?;
    let mut reader = World::new(CONFIG_OFF).map_err(Fail::die)?;
    for world in [&mut peer, &mut sibling, &mut reader] {
        world.keep();
    }
    // The sibling shares the peer's state and control registry, with a distinct
    // socket. Its separate config requests no fleet listener.
    sibling.home.clone_from(&peer.home);
    sibling.run.clone_from(&peer.run);
    sibling.sock = peer.run.join("sibling.sock");

    let ports = crate::fleet_world::free_ports(2)
        .map_err(|e| Fail::die(format!("fleet-menu ports: {e}")))?;
    let peer_port = ports[0].to_string();
    let reader_port = ports[1].to_string();
    cli(
        &peer,
        &aterm,
        &["fleet", "init", "--host", "127.0.0.1", "--port", &peer_port],
    )?;
    let primary = start(&peer, &aterm, true)?;
    let second = start(&sibling, &aterm, true)?;
    let code = peer.state().join(aterm_net::fleet_member::CODE_FILE);
    let code = code.to_string_lossy();
    let target = format!("127.0.0.1:{peer_port}");
    cli(
        &reader,
        &aterm,
        &[
            "fleet",
            "join",
            "--at",
            &target,
            "--host",
            "127.0.0.1",
            "--port",
            &reader_port,
            "--code-file",
            &code,
        ],
    )?;
    let survey = cli(
        &reader,
        &aterm,
        &["ctl", "survey", "--host", &target, "--json"],
    )?;
    let value: aterm_json::Value =
        aterm_json::from_str(&survey).map_err(|e| Fail::die(format!("fleet-menu survey: {e}")))?;
    if value.as_array().is_none_or(|hosts| {
        !hosts
            .iter()
            .any(|host| host["instance"]["instances"].as_str() == Some("2"))
    }) {
        return Err(Fail::die("fleet-menu peer did not report two instances"));
    }
    std::fs::write(out.join("peer-survey.json"), &survey).map_err(|e| Fail::die(e.to_string()))?;
    let window = start(&reader, &aterm, false)?;
    let until = Instant::now() + HANG;
    let (chrome, complete) = loop {
        crate::stop::check()?;
        let chrome = control(&reader, &aterm, &window.sock, &["chrome"])?;
        if chrome
            .lines()
            .any(|line| line.starts_with("statusitem ") && line.contains("fleet=as_of:"))
        {
            break (chrome, true);
        }
        if Instant::now() >= until {
            break (chrome, false);
        }
        crate::stop::sleep(Duration::from_millis(50))?;
    };
    std::fs::write(out.join("menu.txt"), &chrome).map_err(|e| Fail::die(e.to_string()))?;
    let image = control(&reader, &aterm, &window.sock, &["image", "frame.png"])?;
    let image = image
        .split_whitespace()
        .nth(3)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| Fail::die("fleet-menu image reply has no path"))?;
    std::fs::copy(image, out.join("frame.png")).map_err(|e| Fail::die(e.to_string()))?;
    println!(
        "fleet-menu peer={} sibling={} reader_keeper={} output={}\n{}",
        primary.pid,
        second.pid,
        window.pid,
        out.display(),
        chrome
    );
    if !complete {
        return Err(Fail::die("fleet-menu reader produced no fleet snapshot"));
    }
    Ok(0)
}
