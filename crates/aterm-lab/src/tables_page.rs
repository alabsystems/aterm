// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab tables-page`: the Tables page (docs/PLAN.md LT-08) on a private instance whose home holds
//! a scratch registry with synthetic rows, two of its Macs' concurrent edits to one cell among
//! them. It opens the page, takes a frame, stages an edit through `act`, takes a frame, writes
//! it, and takes a frame; the frames go to the folder named. No live vault, fleet or Commons is
//! reached, and every value is synthetic (`example.invalid`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_db::alab::Registry;
use aterm_db::att::REGISTRY;
use aterm_db::database::Change;
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_db::typed::cell::{self, CellKey, CellValue};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

use crate::instance::{CONFIG_OFF, Instance, Launch, World, resolve_aterm, take_value};
use crate::{Fail, Res};

const HANG: Duration = Duration::from_secs(60);

fn fail(e: impl std::fmt::Display) -> Fail {
    Fail::die(format!("tables-page: {e}"))
}

fn put(collection: &str, pk: &str, column: &str, value: Value) -> Result<Change, Fail> {
    let collection = REGISTRY.collection(collection).map_err(fail)?;
    cell::change(
        collection,
        CellKey {
            pk,
            col: column,
            elem: None,
        },
        &CellValue::owner(collection.version, value),
    )
    .map_err(fail)
}

/// The synthetic rows: three Macs, two addresses and a phone.
fn rows() -> Result<Vec<Vec<Change>>, Fail> {
    let mut records = Vec::new();
    for (pk, label, nickname, location, chip) in [
        ("mc-000000000000000a", "m9", "Spare", "rack a", "M2"),
        ("mc-000000000000000b", "m55", "Build", "rack b", "M4 Max"),
        ("mc-000000000000000c", "m73", "Desk", "office", "M4 Pro"),
    ] {
        records.push(vec![
            put("machine", pk, "label", json!(label))?,
            put("machine", pk, "status", json!("active"))?,
            put("machine", pk, "nickname", json!(nickname))?,
            put("machine", pk, "location", json!(location))?,
            put("machine", pk, "chip", json!(chip))?,
            put("machine", pk, "row", json!("live"))?,
        ]);
    }
    for (pk, addr, kind) in [
        ("ad-0000000000000001", "owner@example.invalid", "standalone"),
        ("ad-0000000000000002", "agents@example.invalid", "alias"),
    ] {
        records.push(vec![
            put("address", pk, "addr", json!(addr))?,
            put("address", pk, "domain", json!("example.invalid"))?,
            put("address", pk, "kind", json!(kind))?,
            put("address", pk, "row", json!("live"))?,
        ]);
    }
    records.push(vec![
        put("phone", "ph-0000000000000001", "label", json!("work phone"))?,
        put(
            "phone",
            "ph-0000000000000001",
            "carrier",
            json!("Example Mobile"),
        )?,
        put("phone", "ph-0000000000000001", "row", json!("live"))?,
    ]);
    Ok(records)
}

/// Run the lane; `--out DIR` names where the frames go (the world's folder by default).
///
/// # Errors
/// The registry could not be seeded, the instance did not start or answer, or a frame failed.
pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let out = take_value(&mut args, "--out")?.map(|p| PathBuf::from(crate::paths::from_caller(&p)));
    if args.len() > 1 {
        return Err(Fail::usage(
            "tables-page takes --out DIR and an aterm at most",
        ));
    }
    let aterm = resolve_aterm(args.pop()).map_err(|e| Fail::exit(3, e))?;
    let mut world = World::new(CONFIG_OFF).map_err(fail)?;
    let vaults = aterm_tempfile::Builder::new()
        .prefix("v")
        .tempdir_in(&world.dir)
        .map_err(fail)?;
    world.rehome(&vaults.path().join("m1")).map_err(fail)?;
    if aterm_types::dirs::home_dir().is_some_and(|live| live == world.home) {
        return Err(fail("the world's home is this user's"));
    }
    let lab = Lab::in_home(vaults, &[Role::Writer, Role::Writer]);
    if lab.paths[0].home != world.home {
        return Err(fail("the fixture's Mac is not the world's home"));
    }
    let here = Registry::at(lab.paths[0].clone());
    here.open().map_err(fail)?;
    here.write(rows()?).map_err(fail)?;
    // The second Mac learns the table, then both name one cell apart: a conflict to show.
    let there = Registry::at(lab.paths[1].clone());
    there.open().map_err(fail)?;
    repair_commons(&lab, 4);
    here.write(vec![vec![put(
        "machine",
        "mc-000000000000000a",
        "nickname",
        json!("Backup"),
    )?]])
    .map_err(fail)?;
    there
        .write(vec![vec![put(
            "machine",
            "mc-000000000000000a",
            "nickname",
            json!("Spare 2"),
        )?]])
        .map_err(fail)?;
    repair_commons(&lab, 4);

    let out = out.unwrap_or_else(|| world.dir.clone());
    std::fs::create_dir_all(&out).map_err(fail)?;
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: true,
            args: vec![
                "--columns".into(),
                "200".into(),
                "--lines".into(),
                "56".into(),
            ],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    let shots = drive(&mut inst, &out);
    crate::instance::after_stop(shots.map(|_| ()), inst.stop())?;
    drop(lab);
    Ok(0)
}

fn ok(inst: &Instance, args: &[&str]) -> Result<String, Fail> {
    let reply = inst.ctl(args);
    if reply.code != 0 {
        return Err(fail(format!("{}: {}", args.join(" "), reply.first())));
    }
    Ok(reply.stdout)
}

/// The Tables page's view id, from `inspect app/v1 tabs`.
fn tables_view(inst: &Instance) -> Result<String, Fail> {
    let tabs = ok(inst, &["inspect", "app/v1", "tabs"])?;
    tabs.lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("leaf ") && line.contains(" app=tables"))
        .find_map(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("view="))
                .map(str::to_owned)
        })
        .ok_or_else(|| fail("no Tables tab"))
}

/// Until `what` holds of the page's text, within the hang detector.
fn until(
    inst: &Instance,
    view: &str,
    what: &str,
    holds: impl Fn(&str) -> bool,
) -> Result<(), Fail> {
    let start = Instant::now();
    loop {
        let reply = inst.ctl(&["inspect", "app/v1", "view", view, "text"]);
        if reply.code == 0 && holds(&reply.stdout) {
            return Ok(());
        }
        if start.elapsed() > HANG {
            return Err(fail(format!(
                "the page never showed {what}: {}",
                reply.stdout
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Press `key`, or set it to `value`, as an accessibility client does.
fn act(inst: &Instance, view: &str, key: &str, value: Option<&str>) -> Result<(), Fail> {
    let mut words = vec!["act", "app/v1", "view", view, key, key];
    words.extend(value);
    ok(inst, &words).map(|_| ())
}

fn frame(inst: &Instance, out: &Path, name: &str) -> Result<PathBuf, Fail> {
    std::thread::sleep(Duration::from_millis(300));
    let reply = inst.ctl(&["image", name]);
    let path = reply
        .image_path()
        .ok_or_else(|| fail(format!("image: {}", reply.first())))?;
    let to = out.join(name);
    std::fs::copy(path, &to).map_err(fail)?;
    println!("tables-page: {}", to.display());
    Ok(to)
}

fn drive(inst: &mut Instance, out: &Path) -> Result<Vec<PathBuf>, Fail> {
    inst.await_socket(HANG)?;
    ok(inst, &["open", "app", "tables"])?;
    let view = tables_view(inst)?;
    until(inst, &view, "its machines", |text| {
        text.contains("Machines: 3 rows")
    })?;
    let mut shots = vec![frame(inst, out, "tables-1-read.png")?];
    act(
        inst,
        &view,
        "tables/cell/mc-000000000000000a/nickname",
        None,
    )?;
    shots.push(frame(inst, out, "tables-2-conflict.png")?);
    if inst
        .ctl(&["inspect", "app/v1", "view", &view, "text"])
        .stdout
        .contains("tables/pane/keep/nickname/0")
    {
        act(inst, &view, "tables/pane/keep/nickname/0", None)?;
    }
    let location = "tables/cell/mc-000000000000000b/location";
    act(inst, &view, location, None)?;
    act(inst, &view, location, None)?;
    act(inst, &view, "tables/edit", Some("rack c"))?;
    act(inst, &view, "tables/cell/mc-000000000000000c/chip", None)?;
    until(inst, &view, "a staged cell", |text| {
        text.contains("not written")
    })?;
    shots.push(frame(inst, out, "tables-3-staged.png")?);
    act(inst, &view, "tables/write", None)?;
    until(inst, &view, "the write", |text| text.contains("Wrote "))?;
    act(inst, &view, location, None)?;
    shots.push(frame(inst, out, "tables-4-written.png")?);
    Ok(shots)
}
