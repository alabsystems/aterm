// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An ordinary window reads a shared vault while another configuration owns its transport.

use super::{HANG, fail, image, peer_cycle::gui, save, start_window, table};
use crate::instance::{Instance, World};
use crate::{Fail, stop};
use aterm_db::{network_state, replica::InventorySummary};
use aterm_json::{Value, json};
use aterm_types::network_state::MachineIdentity;
use std::{collections::BTreeSet, path::Path, time::Duration};

fn gui_read(window: &Instance, owner: u32, identity: &MachineIdentity) -> (bool, Value) {
    let reply = window.ctl(&["inspect", "app/v1", "network"]);
    let parsed = gui::follows_owner(&reply.stdout, owner, identity, aterm_time::unix_millis());
    let matched = reply.code == 0 && parsed.as_ref().is_ok_and(|matched| *matched);
    (
        matched,
        json!({
            "command":["inspect","app/v1","network"],"exit_code":reply.code,
            "stdout":reply.stdout,"stderr":reply.stderr,"matches_owner":matched,
            "parse_error":parsed.err(),
        }),
    )
}

fn wait_gui(
    window: &Instance,
    owner: u32,
    identity: &MachineIdentity,
    out: &Path,
    name: &str,
) -> Result<(), Fail> {
    let until = stop::deadline_after(HANG);
    loop {
        stop::check()?;
        let (matched, evidence) = gui_read(window, owner, identity);
        if matched || std::time::Instant::now() >= until {
            save(out, name, &evidence)?;
            return if matched {
                Ok(())
            } else {
                Err(Fail::die(
                    "the new configuration did not adopt the live vault owner",
                ))
            };
        }
        stop::sleep(Duration::from_millis(100))?;
    }
}

fn read_cli(window: &Instance, args: &[&str], out: &Path, name: &str) -> Result<Value, Fail> {
    let reply = window.cli(None, args);
    let parsed = aterm_json::from_str::<Value>(&reply.stdout);
    save(
        out,
        name,
        &json!({
            "command":args,"exit_code":reply.code,"stdout":reply.stdout,"stderr":reply.stderr,
            "parsed":parsed.as_ref().ok(),"parse_error":parsed.as_ref().err().map(ToString::to_string),
        }),
    )?;
    if reply.code != 0 {
        return Err(Fail::die(format!("local follower CLI: {}", reply.first())));
    }
    parsed.map_err(fail)
}

pub(super) fn run(
    source: &World,
    aterm: &Path,
    port: u16,
    owner: u32,
    table: &table::Table,
    rows: &table::Rows,
    out: &Path,
) -> Result<Value, Fail> {
    let config = std::fs::read_to_string(source.cfg.join("aterm/aterm.toml")).map_err(fail)?;
    let mut follower = World::new(&config).map_err(fail)?;
    // Share only the synthetic vault home; configuration, socket and launch logs stay distinct.
    follower.home.clone_from(&source.home);
    follower.keep();
    let window = start_window(&follower, aterm, port, false)?;
    let pid = window
        .aterm_pid()
        .ok_or_else(|| Fail::die("local follower has no application pid"))?;
    if u32::try_from(pid).map_err(fail)? == owner || follower.cfg == source.cfg {
        return Err(Fail::die(
            "local follower did not use a distinct process and configuration",
        ));
    }
    let observed =
        network_state::read_live_observation(&source.cfg.join("aterm/aterm.toml")).map_err(fail)?;
    let identity = observed
        .observer
        .ok_or_else(|| Fail::die("transport owner has no admitted identity"))?;

    wait_gui(
        &window,
        owner,
        &identity,
        out,
        "local-follower-gui-ready.json",
    )?;
    let read = read_cli(
        &window,
        &["db", "network-state"],
        out,
        "local-follower-cli.json",
    )?;
    if read["live_network"].as_bool() != Some(true)
        || read["network"]["via_local_process"].as_u64() != Some(u64::from(owner))
    {
        return Err(Fail::die(
            "local follower CLI did not read the active vault owner",
        ));
    }

    let id = table.id();
    let inventory = read_cli(
        &window,
        &["db", "commons-inventory", "--table", &id],
        out,
        "local-follower-inventory.json",
    )?;
    let operations: BTreeSet<_> = rows.keys().copied().collect();
    let expected = InventorySummary::of(&operations);
    let fingerprint = aterm_codec::hex::encode_lower(&expected.fingerprint);
    if inventory["table"].as_str() != Some(id.as_str())
        || inventory["known"]["count"] != json!(expected.count)
        || inventory["eligible"]["count"] != json!(expected.count)
        || inventory["known"]["sha256"].as_str() != Some(fingerprint.as_str())
        || inventory["eligible"]["sha256"].as_str() != Some(fingerprint.as_str())
    {
        return Err(Fail::die(
            "local follower did not read the existing table inventory",
        ));
    }

    let opened = window.ctl(&["invoke", "NetworkState"]);
    if opened.code != 0 {
        return Err(Fail::die(format!(
            "local follower Network State: {}",
            opened.first()
        )));
    }
    wait_gui(
        &window,
        owner,
        &identity,
        out,
        "local-follower-gui-before.json",
    )?;
    image(&window, out, "local-follower.png")?;
    let (matched, evidence) = gui_read(&window, owner, &identity);
    save(out, "local-follower-gui-after.json", &evidence)?;
    if !matched {
        return Err(Fail::die(
            "local follower lost its owner observation during capture",
        ));
    }
    Ok(json!({
        "owner_pid":owner,"follower_pid":pid,"source_home":source.home.display().to_string(),
        "source_config":source.cfg.display().to_string(),"follower_config":follower.cfg.display().to_string(),
        "same_home":true,"distinct_configuration":true,"gui_followed_owner":true,
        "table":id,"known":expected.count,"eligible":expected.count,
        "inventory_sha256":fingerprint,"reader_copied_or_joined_data":false,
    }))
}
