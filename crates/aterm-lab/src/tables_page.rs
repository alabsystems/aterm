// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab tables-page`: the Tables page (docs/PLAN.md LT-08) on a private instance whose home holds
//! a scratch registry with synthetic rows, two of its Macs' concurrent edits to one cell among
//! them, the compiled reference rows, and a chip the owner stated that its Mac sees otherwise.
//! It opens the page, takes a frame, stages an edit through `act`, takes a frame, opens a Mac's
//! role (an enum) on its list of words and takes a frame, stages a word that is none of them
//! and takes a frame of its refusal, asks `act` to write them (which `act` refuses: only a
//! person's own press writes) and discards them, selects the grid's last column, takes a frame,
//! sorts the rows by a header, takes a frame, selects the stated chip (the line under the grid says both what is stated and what the Mac sees), takes
//! a frame, in a narrow window opens the table picker and takes a frame, opens the Macs' own
//! facts and selects a cell, takes a frame, opens the providers and takes a frame, opens the
//! CLIs and selects one's key, takes a frame, opens the Mac models (the rail's last table) and
//! takes a frame, opens Check and takes a frame, selects its first finding and takes a frame,
//! goes back to the machines, selects a cell and finds another row (the cell bar names the
//! selected cell as not among the rows found), takes a frame; the frames go to the folder named.
//! `--lines N` sets the window's height (56 lines by default), so a short one shows the rail
//! scrolled; `--columns N` its width (200 by default), so a narrow one (under 900 px) shows the
//! table picker and the pane below the grid. No live vault, fleet or Commons is reached, and every
//! value is synthetic (`example.invalid`).

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

/// Fewer columns than this lay the page out under 900 px across: the picker stands for the
/// rail, so a table is chosen through it.
const NARROW_COLUMNS: u16 = 110;

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
    // A short window shows the rail scrolled to a table off its foot.
    let lines = take_value(&mut args, "--lines")?.unwrap_or_else(|| "56".to_owned());
    if lines.parse::<u16>().is_err() {
        return Err(Fail::usage("tables-page: --lines takes a number of lines"));
    }
    // A narrow window stands the page in one column, the rail one table picker.
    let columns = take_value(&mut args, "--columns")?.unwrap_or_else(|| "200".to_owned());
    let Ok(across) = columns.parse::<u16>() else {
        return Err(Fail::usage(
            "tables-page: --columns takes a number of columns",
        ));
    };
    if args.len() > 1 {
        return Err(Fail::usage(
            "tables-page takes --out DIR, --lines N, --columns N and an aterm at most",
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
    here.seed().map_err(fail)?;
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
    // Each Mac registers as aterm does: its facts row binds it to its machine row, which is how
    // the page names a writer by its label.
    aterm_secrets::client::admin_seed_swarm_roster(&lab.paths[0], &[]).map_err(fail)?;
    aterm_secrets::client::sync(&lab.paths[1], None).map_err(fail)?;
    for (n, host) in ["m73", "m55"].into_iter().enumerate() {
        let probe = aterm_db::alab::register::Probe {
            hw_model: "Mac16,8".into(),
            chip: Some("Apple M4 Pro".into()),
            ram_gb: Some(48),
            hosts: vec![host.into()],
            node: Some(format!("{:016x}", n + 1)),
            platform_uuid: format!("00000000-0000-4000-8000-00000000000{n}"),
            ..aterm_db::alab::register::Probe::default()
        };
        Registry::at(lab.paths[n].clone())
            .register(
                &lab.paths[n].home.join("config.toml"),
                &probe,
                true,
                aterm_time::unix_millis().ok_or_else(|| fail("no clock"))?,
            )
            .map_err(fail)?;
        repair_commons(&lab, 4);
    }
    // The owner states the first Mac's chip as another: the page shows what is stated and what
    // that Mac sees, neither over the other.
    let view = aterm_db::att::Handle::at(lab.paths[0].clone(), &REGISTRY)
        .view()
        .map_err(fail)?;
    let facts = aterm_db::typed::keys::uuid32(&lab.machine(0));
    let stated = view
        .owned
        .get("machine-facts")
        .and_then(|read| read.rows.get(&facts))
        .and_then(|row| aterm_db::alab::registry::bound_machine(&row.value))
        .map(str::to_owned)
        .ok_or_else(|| fail("the first Mac's facts bind no machine"))?;
    here.write(vec![vec![put(
        "machine",
        &stated,
        "chip",
        json!("Apple M2"),
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
            args: vec!["--columns".into(), columns, "--lines".into(), lines],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    let shots = drive(&mut inst, &out, &stated, &facts, across < NARROW_COLUMNS);
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

/// Choose table `sheet` (or Check, [`CHECK`]) as a person does: on the rail, or under 900 px
/// through the table picker that stands for it.
fn choose(inst: &Instance, view: &str, key: &str, narrow: bool) -> Result<(), Fail> {
    if narrow {
        act(inst, view, "tables/picker", None)?;
    }
    act(inst, view, key, None)
}

/// The rail's line that opens Check.
const CHECK: &str = "tables/view/check";

fn drive(
    inst: &mut Instance,
    out: &Path,
    stated: &str,
    facts: &str,
    narrow: bool,
) -> Result<Vec<PathBuf>, Fail> {
    inst.await_socket(HANG)?;
    ok(inst, &["open", "app", "tables"])?;
    let view = tables_view(inst)?;
    // The rail names each table with its rows; under 900 px the picker names the one shown.
    until(inst, &view, "its machines", |text| {
        text.contains("Machines: ") || text.contains("Table: Machines")
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
    // An enum's cell opens on the list of its words; a word that is none of them, typed and
    // staged, is refused on its cell at once.
    let role = "tables/cell/mc-000000000000000b/role";
    act(inst, &view, role, None)?;
    act(inst, &view, role, None)?;
    until(inst, &view, "the role's words", |text| {
        text.contains("tables/choice/0")
    })?;
    shots.push(frame(inst, out, "tables-3a-choices.png")?);
    act(inst, &view, "tables/edit", Some("server"))?;
    act(inst, &view, location, None)?;
    act(inst, &view, role, None)?;
    until(inst, &view, "a refused cell", |text| {
        text.contains("refused")
    })?;
    shots.push(frame(inst, out, "tables-3b-refused.png")?);
    // Only a person's own press writes: `act`, a control client's, is refused it.
    let refused = inst.ctl(&[
        "act",
        "app/v1",
        "view",
        &view,
        "tables/write",
        "tables/write",
    ]);
    if refused.code == 0 {
        return Err(fail("act wrote the registry: only a person's press writes"));
    }
    println!("tables-page: act tables/write: {}", refused.first());
    act(inst, &view, "tables/discard", None)?;
    act(inst, &view, location, None)?;
    shots.push(frame(inst, out, "tables-4-unwritten.png")?);
    // The grid's last column selected scrolls it across: the row's label stays at the left.
    act(inst, &view, "tables/cell/mc-000000000000000c/hosts", None)?;
    shots.push(frame(inst, out, "tables-5-across.png")?);
    // A header pressed twice orders the rows by its column from the greatest value down, and
    // wears the mark that says so.
    act(inst, &view, "tables/head/label", None)?;
    act(inst, &view, "tables/head/label", None)?;
    shots.push(frame(inst, out, "tables-6-sorted.png")?);
    // The chip the owner stated: the line under the grid says it, and what its Mac reports.
    act(inst, &view, &format!("tables/cell/{stated}/chip"), None)?;
    until(inst, &view, "a stated value and an observation", |text| {
        text.contains(" reports ")
    })?;
    shots.push(frame(inst, out, "tables-7-differs.png")?);
    // Under 900 px the picker lists every table, and Check, in the grid's place.
    if narrow {
        act(inst, &view, "tables/picker", None)?;
        until(inst, &view, "the list of tables", |text| {
            text.contains("tables/view/check")
        })?;
        shots.push(frame(inst, out, "tables-7b-picker.png")?);
        act(inst, &view, "tables/picker", None)?;
    }
    // What each Mac states of itself, one row each, read only.
    choose(inst, &view, "tables/sheet/machine-facts", narrow)?;
    act(inst, &view, &format!("tables/cell/{facts}/chip"), None)?;
    until(inst, &view, "a Mac's own row", |text| {
        text.contains(": seen ")
    })?;
    shots.push(frame(inst, out, "tables-8-facts.png")?);
    // The compiled reference rows.
    choose(inst, &view, "tables/sheet/provider", narrow)?;
    until(inst, &view, "the providers", |text| {
        text.contains("Providers")
    })?;
    shots.push(frame(inst, out, "tables-9-reference.png")?);
    // A CLI is named by its own key; the last table on the rail, chosen as a press chooses it.
    choose(inst, &view, "tables/sheet/cli", narrow)?;
    act(inst, &view, "tables/cell/codex/~key", None)?;
    until(inst, &view, "a CLI's key", |text| {
        text.contains("is this row's key")
    })?;
    shots.push(frame(inst, out, "tables-10-cli.png")?);
    choose(inst, &view, "tables/sheet/model", narrow)?;
    until(inst, &view, "the Mac models", |text| {
        text.contains("Mac models")
    })?;
    shots.push(frame(inst, out, "tables-11-models.png")?);
    // Check: every finding in words, then the store's state; a finding selected shows the rows
    // it names.
    choose(inst, &view, CHECK, narrow)?;
    until(inst, &view, "what check finds", |text| {
        text.contains("THE STORE")
    })?;
    shots.push(frame(inst, out, "tables-12-check.png")?);
    act(inst, &view, "tables/check/finding/0", None)?;
    until(inst, &view, "a finding's rows", |text| {
        text.contains("tables/check/row/0/0")
    })?;
    shots.push(frame(inst, out, "tables-13-finding.png")?);
    // Back on the machines, a find that leaves the selected row out: the line under the grid
    // still tells of the cell, and the cell bar names it as not among the rows found.
    choose(inst, &view, "tables/sheet/machine", narrow)?;
    act(inst, &view, "tables/cell/mc-000000000000000c/chip", None)?;
    act(inst, &view, "tables/find", Some("mc-000000000000000a"))?;
    until(inst, &view, "the selected cell, its row left out", |text| {
        text.contains("not among the rows found")
    })?;
    shots.push(frame(inst, out, "tables-14-found.png")?);
    Ok(shots)
}
