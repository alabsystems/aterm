// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab tables-page`: the Tables page (docs/PLAN.md LT-08) on a private instance whose home holds
//! a scratch registry with synthetic rows, two of its Macs' concurrent edits to one cell among
//! them, the compiled reference rows, and a chip the owner stated that its Mac sees otherwise.
//! It opens the page, takes a frame, opens the Columns list and takes a frame, selects a Mac's
//! status (STORED's folded stub, the columns no row fills, stands before it) and takes a frame,
//! unfolds STORED with a press on its stub and takes a frame, stages an edit through `act`, takes a
//! frame, opens a Mac's role (an enum) on its list of words and takes a frame, stages a word that
//! is none of them and takes a frame of its refusal, asks `act` to write them (which `act` refuses:
//! only a person's own press writes) and discards them, stages a label another Mac already reads
//! as and takes a frame of what the cell and the cell bar say of it (docs/PLAN.md LT-46) and
//! discards it, turns the fold off from the Columns list,
//! selects a Mac's linked count (the cell bar says the count, the line under the grid which rows
//! name the Mac), takes a frame, selects the grid's last column, takes a frame, sorts the rows by a
//! header, takes a frame, selects the stated chip (the line under the grid says both what is stated
//! and what the Mac sees), takes a frame, in a narrow window opens the table picker and takes a
//! frame, opens the Macs' own facts and selects a cell, takes a frame, opens the providers and
//! takes a frame, opens the CLIs and selects one's key, takes a frame, opens the machine models
//! (the rail's last table) and takes a frame, opens the plans (each one's limits in words) and the
//! imports (each one's time a date) and takes a frame of each, opens Check and takes a frame,
//! selects its first finding and takes a frame, goes back to the machines, opens the status's list
//! of values from the mark at its header's end and takes a frame, keeps the active machines in the
//! office or in rack a (two chips under the tools) and takes a frame, adds a filter no row passes
//! (the grid says so, with Clear filters) and takes a frame, and clears them (docs/PLAN.md LT-41);
//! then selects a cell and finds another row (the cell bar names the selected cell as not among
//! the rows found), takes a frame,
//! selects the cell the two Macs wrote apart (its row is the one found, and the find stays) and
//! opens the history drawer under the grid on the whole table, takes a frame, narrows it to that
//! cell, takes a frame, opens the Columns list over the drawer (the grid and the drawer stay where
//! they stand) and takes a frame, hides that cell's column from it (the selection, and the drawer's
//! cell with it, move to the next column drawn) and takes a frame, shows it again (docs/PLAN.md
//! LT-39, LT-40), opens the location's list of values over the drawer and takes a frame, keeps the
//! office's machines, which leaves the found row out (its chip under the tools, the drawer under
//! the grid still listing that row's records), narrows the drawer to that row and takes a frame
//! where the chip's line leaves the drawer room, takes the filter off (LT-41 with LT-39; a drawer
//! line's key runs past the 64 bytes `act` takes for an action, so the lane presses no line),
//! closes the drawer and clears the find; then opens the links, adds one and opens its machine on
//! the list of the machines (two under one label, each saying which it is) and takes a frame,
//! chooses one and opens its account on its list and takes a frame, discards them and selects a
//! written link, whose cells read as the rows they name and
//! whose pane offers each, and takes a frame (docs/PLAN.md LT-30); then selects the machine that
//! link names, whose Retire waits on the link, and takes a frame, ends the link from there and
//! takes a frame of the machine's Retire freed, stages that retire, opens the history drawer on the
//! machine's row (the retire first, not written) and takes a frame, closes it, discards them, steps
//! the lifecycle filter to retired rows and takes a frame, and adds an email, whose pane names the
//! verb that sets its password, and takes a frame, opens the drawer on the new row (its birth, not
//! written) and takes a frame, and closes it (docs/PLAN.md LT-24, LT-39); the frames go to the
//! folder named. A window too short for the drawer (judged, as the cell bar is, from the window and
//! the filters' chips alone) has no drawer, and the lane takes none of its frames there; it asks
//! again once the location's chip stands, which can take the drawer away.
//! `--lines N` sets the window's height (56 lines by default), so a short one shows the rail
//! scrolled; `--columns N` its width (200 by default), so a narrow one (under 900 px) shows the
//! table picker and the pane below the grid. No live vault, fleet or Commons is reached, and every
//! value is synthetic (`example.invalid`). With `--startup-machines N` the private registry
//! holds N machine rows and the reference rows. `--select-account` adds a synthetic account
//! and chooses its sheet while the registry opens. Both drives observe selection and editing.
//! `--registry` follows a machine from aRegistry to its record in an existing Tables tab,
//! then returns to its assignments and signed observation. `--out DIR` also saves those frames.
//! With `--registry --startup-machines N`, aRegistry opens first and the inspector measures
//! the first seeded machine's row, its selection and its details. Find then locates the last
//! seeded machine by name and Enter opens its stable-key detail.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_db::alab::Registry;
use aterm_db::att::REGISTRY;
use aterm_db::database::Change;
use aterm_db::table::fixture::{Lab, repair_commons};
use aterm_db::typed::cell::{self, CellKey, CellValue};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

use crate::instance::{CONFIG_OFF, Instance, Launch, World, resolve_aterm, take_flag, take_value};
use crate::{Fail, Res};

const HANG: Duration = Duration::from_secs(60);
const ADD: &str = "tables/rows/add";

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

/// A machine row seated again under a label another row holds, as a reinstall leaves one
/// (docs/KNOWN_ISSUES.md ATERM-KI-1112-155): the choice of a link's machine tells the two apart.
const SPLIT: &str = "mc-000000000000000d";
/// The synthetic account and the link that names the third Mac and it.
const ACCOUNT: &str = "ac-0000000000000001";
const MACHINE: &str = "mc-000000000000000c";

fn address(pk: &str, addr: &str, kind: &str) -> Result<Vec<Change>, Fail> {
    Ok(vec![
        put("address", pk, "addr", json!(addr))?,
        put("address", pk, "domain", json!("example.invalid"))?,
        put("address", pk, "kind", json!(kind))?,
        put("address", pk, "row", json!("live"))?,
    ])
}

fn account() -> Result<Vec<Change>, Fail> {
    Ok(vec![
        put("account", ACCOUNT, "provider", json!("openai"))?,
        put("account", ACCOUNT, "realm", json!("codex"))?,
        put("account", ACCOUNT, "login", json!("ad-0000000000000001"))?,
        put("account", ACCOUNT, "status", json!("active"))?,
        put("account", ACCOUNT, "row", json!("live"))?,
    ])
}

fn assignment(machine: &str) -> Result<Vec<Change>, Fail> {
    let link = aterm_db::typed::keys::preference_pk(machine, ACCOUNT).map_err(fail)?;
    Ok(vec![
        put("preference", &link, "machine", json!(machine))?,
        put("preference", &link, "account", json!(ACCOUNT))?,
        put("preference", &link, "why", json!("owner"))?,
        put("preference", &link, "row", json!("live"))?,
    ])
}

/// The synthetic rows: four Macs (two under one label), two addresses, a phone, an account and a
/// link.
fn rows() -> Result<Vec<Vec<Change>>, Fail> {
    let mut records = Vec::new();
    for (pk, label, nickname, location, chip, status) in [
        (
            "mc-000000000000000a",
            "m9",
            "Spare",
            "rack a",
            "M2",
            "active",
        ),
        (
            "mc-000000000000000b",
            "m55",
            "Build",
            "rack b",
            "M4 Max",
            "active",
        ),
        (MACHINE, "m73", "Desk", "office", "M4 Pro", "active"),
        (SPLIT, "m73", "Desk again", "office", "M4 Pro", "planned"),
    ] {
        records.push(vec![
            put("machine", pk, "label", json!(label))?,
            put("machine", pk, "status", json!(status))?,
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
        records.push(address(pk, addr, kind)?);
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
    records.push(account()?);
    records.push(assignment(MACHINE)?);
    Ok(records)
}

/// Run the lane; `--out DIR` names where the frames go (the world's folder by default).
///
/// # Errors
/// The registry could not be seeded, the instance did not start or answer, or a frame failed.
pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let out = take_value(&mut args, "--out")?.map(|p| PathBuf::from(crate::paths::from_caller(&p)));
    let startup_machines = take_value(&mut args, "--startup-machines")?
        .map(|value| {
            let limit = aterm_db::att::CAPS.writer_records;
            value
                .parse::<usize>()
                .ok()
                .filter(|count| (1..=limit).contains(count))
                .ok_or_else(|| {
                    Fail::usage(format!(
                        "--startup-machines takes a count from 1 to {limit}"
                    ))
                })
        })
        .transpose()?;
    let select_account = take_flag(&mut args, "--select-account");
    let registry = take_flag(&mut args, "--registry");
    if select_account && (registry || startup_machines.is_none()) {
        return Err(Fail::usage(
            "--select-account requires the Tables --startup-machines N measurement",
        ));
    }
    if startup_machines.is_some() && out.is_some() {
        return Err(Fail::usage(
            "--startup-machines writes timings to stdout; --out is for frames",
        ));
    }
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
            "tables-page takes [--registry] [--out DIR | --startup-machines N], --lines N, --columns N and an aterm at most",
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
    let start = || {
        Instance::start(
            &world,
            Launch {
                aterm,
                headless: true,
                args: vec!["--columns".into(), columns, "--lines".into(), lines],
                env: world.env(),
                cwd: world.home.clone(),
                sandbox: None,
                reroute: false,
            },
        )
        .map_err(fail)
    };
    if let Some(count) = startup_machines {
        if select_account {
            here.write(vec![
                address("ad-0000000000000001", "owner@example.invalid", "standalone")?,
                account()?,
            ])
            .map_err(fail)?;
        }
        seed_startup(&here, &lab.paths[0], count)?;
        let mut inst = start()?;
        let read = inst.await_socket(HANG).and_then(|_| {
            if registry {
                let view = measure_registry_open(&inst, count, FIRST_MACHINE, false, |text| {
                    shows(text, "registry/detail/identity", FIRST_MACHINE)
                        && text.contains("registry/detail/no-accounts")
                        && text.contains("registry/detail/no-observations")
                })?;
                find_last_machine(&inst, &view, count)
            } else {
                startup(&inst, count, select_account, across < NARROW_COLUMNS)
            }
        });
        crate::instance::after_stop(read, inst.stop())?;
        return Ok(0);
    }
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

    if registry {
        if stated != MACHINE {
            here.write(vec![assignment(&stated)?]).map_err(fail)?;
        }
        if let Some(dir) = &out {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let count = view.project(&aterm_db::alab::registry::MACHINE).rows.len();
        let mut inst = start()?;
        let result = registry_round_trip(
            &mut inst,
            &stated,
            &facts,
            count,
            out.as_deref(),
            across < NARROW_COLUMNS,
        );
        crate::instance::after_stop(result, inst.stop())?;
        return Ok(0);
    }
    let out = out.unwrap_or_else(|| world.dir.clone());
    std::fs::create_dir_all(&out).map_err(fail)?;
    let mut inst = start()?;
    let shots = drive(&mut inst, &out, &stated, &facts, across < NARROW_COLUMNS);
    crate::instance::after_stop(shots.map(|_| ()), inst.stop())?;
    drop(lab);
    Ok(0)
}

/// One explicit write per machine. Compiled reference records already occupy part of the
/// writer's quota. Read back the projected row count before starting the headless instance.
fn seed_startup(
    here: &Registry,
    paths: &aterm_secrets::client::Paths,
    count: usize,
) -> Result<(), Fail> {
    let handle = aterm_db::att::Handle::at(paths.clone(), &REGISTRY);
    let (_, usage) = handle.usage().map_err(fail)?;
    let caps = aterm_db::att::CAPS;
    let room = caps
        .writer_records
        .saturating_sub(usage.mine_records)
        .min(caps.records.saturating_sub(usage.records));
    if count > room {
        return Err(Fail::usage(format!(
            "the seeded registry has room for {room} machine records"
        )));
    }
    let mut records = Vec::with_capacity(count);
    for number in 1..=count {
        let pk = format!("mc-{number:016x}");
        records.push(vec![
            put("machine", &pk, "label", json!(format!("m{number}")))?,
            put("machine", &pk, "status", json!("active"))?,
            put("machine", &pk, "row", json!("live"))?,
        ]);
    }
    here.write(records).map_err(fail)?;
    let actual = handle
        .view()
        .map_err(fail)?
        .project(handle.compiled().collection("machine").map_err(fail)?)
        .rows
        .len();
    if actual != count {
        return Err(fail(format!(
            "seeded {count} machines, but the registry projected {actual}"
        )));
    }
    Ok(())
}

const FIRST_MACHINE: &str = "mc-0000000000000001";

fn startup(inst: &Instance, count: usize, select_account: bool, narrow: bool) -> Result<(), Fail> {
    let cpu_before = crate::sys::process_cpu_ns(inst.pid);
    let opening = Instant::now();
    ok(inst, &["open", "app", "tables"])?;
    let view = active_view(inst, "tables")?;
    let (sheet, cell) = if select_account {
        choose(inst, &view, "tables/sheet/account", narrow)?;
        ("account", format!("tables/cell/{ACCOUNT}/label"))
    } else {
        ("machine", format!("tables/cell/{FIRST_MACHINE}/label"))
    };
    // The label stays pinned at both widths. The Add button requires a writable read. It can
    // already be present in the first observation of the label.
    let mut ready_on_first_read = false;
    until(inst, &view, "the selected sheet's first label", |text| {
        ready_on_first_read = text.contains(ADD);
        text.contains(&cell)
    })?;
    let row = opening.elapsed();
    act(inst, &view, &cell, None)?;
    let selected = opening.elapsed();
    let app_cpu_ms = process_cpu_ms_since(inst, cpu_before);
    let ready = if ready_on_first_read {
        row
    } else {
        until(inst, &view, "the selected sheet's Add button", |text| {
            text.contains(ADD)
        })?;
        opening.elapsed()
    };
    println!(
        "tables-page: seeded_machines={count} selected_sheet={sheet} open_to_observed_cell_ms={} open_to_selection_ack_ms={} open_to_observed_add_ms={} app_cpu_to_selection_ack_ms={app_cpu_ms}",
        row.as_millis(),
        selected.as_millis(),
        ready.as_millis()
    );
    Ok(())
}

fn process_cpu_ms_since(inst: &Instance, before: Option<u64>) -> String {
    before
        .zip(crate::sys::process_cpu_ns(inst.pid))
        .and_then(|(before, after)| after.checked_sub(before))
        .map(|ns| Duration::from_nanos(ns).as_millis().to_string())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn ok(inst: &Instance, args: &[&str]) -> Result<String, Fail> {
    let reply = inst.ctl(args);
    if reply.code != 0 {
        return Err(fail(format!("{}: {}", args.join(" "), reply.first())));
    }
    Ok(reply.stdout)
}

fn shows(text: &str, key: &str, value: &str) -> bool {
    text.lines()
        .any(|line| line.contains(key) && line.contains(value))
}

/// Measure the product from issuing its open command through observing the selected detail.
/// Acknowledging the row action also gives the CPU endpoint used by the Tables measurement.
fn measure_registry_open(
    inst: &Instance,
    count: usize,
    machine: &str,
    tables_preopened: bool,
    detail: impl FnMut(&str) -> bool,
) -> Result<String, Fail> {
    let cpu_before = crate::sys::process_cpu_ns(inst.pid);
    let opening = Instant::now();
    ok(inst, &["open", "app", "registry"])?;
    let opened = opening.elapsed();
    let view = active_view(inst, "registry")?;
    let choice = format!("registry/machine/{machine}");
    until(inst, &view, "the registered machine", |text| {
        text.contains(&choice)
    })?;
    let observed = opening.elapsed();
    act(inst, &view, &choice, None)?;
    let acknowledged = opening.elapsed();
    let cpu_ms = process_cpu_ms_since(inst, cpu_before);
    until(inst, &view, "the selected machine's details", detail)?;
    let selected = opening.elapsed();
    println!(
        "tables-page: app=registry fixture_machines={count} tables_preopened={tables_preopened} view={view} machine={machine} open_command_ack_ms={} open_to_observed_machine_ms={} open_to_selection_ack_ms={} open_to_observed_detail_ms={} app_cpu_to_selection_ack_ms={cpu_ms}",
        opened.as_millis(),
        observed.as_millis(),
        acknowledged.as_millis(),
        selected.as_millis()
    );
    Ok(view)
}

fn find_last_machine(inst: &Instance, view: &str, count: usize) -> Result<(), Fail> {
    let name = format!("m{count}");
    let key = format!("mc-{count:016x}");
    let choice = format!("registry/machine/{key}");
    let searching = Instant::now();
    ok(inst, &["invoke", "Find"])?;
    ok(inst, &["paste", &name])?;
    until(
        inst,
        view,
        "the last seeded machine as the only Find result",
        |text| {
            shows(text, "registry/search", &name)
                && shows(text, &choice, &name)
                && text.matches("text key=\"registry/machine/").count() == 1
        },
    )?;
    ok(inst, &["key", "enter"])?;
    until(
        inst,
        view,
        "the found machine's stable key in its details",
        |text| {
            shows(text, "registry/detail/name", &name)
                && shows(text, "registry/detail/identity", &key)
        },
    )?;
    println!(
        "tables-page: app=registry fixture_machines={count} search={name} selected_machine={key} find_to_observed_detail_ms={}",
        searching.elapsed().as_millis()
    );
    Ok(())
}

fn registry_round_trip(
    inst: &mut Instance,
    machine: &str,
    writer: &str,
    count: usize,
    out: Option<&Path>,
    narrow: bool,
) -> Result<(), Fail> {
    inst.await_socket(HANG)?;
    ok(inst, &["open", "app", "tables"])?;
    let tables = active_view(inst, "tables")?;
    choose(inst, &tables, "tables/sheet/account", narrow)?;
    let previous = format!("tables/cell/{ACCOUNT}/label");
    until(inst, &tables, "the account in Tables", |text| {
        text.contains(&previous)
    })?;
    act(inst, &tables, &previous, None)?;

    let account = format!("registry/detail/account/{ACCOUNT}");
    let detail = |text: &str| {
        shows(text, "registry/detail/identity", machine)
            && shows(text, &account, "owner@example.invalid")
            && shows(text, "registry/detail/observation/", "Apple M4 Pro")
            && shows(text, "registry/detail/writer/", writer)
    };
    let registry = measure_registry_open(inst, count, machine, true, detail)?;
    if let Some(dir) = out {
        frame(inst, dir, "aregistry-machine.png")?;
    }

    act(inst, &registry, "registry/open-in-tables", None)?;
    if active_view(inst, "tables")? != tables {
        return Err(fail("Open in Tables created another Tables view"));
    }
    until(
        inst,
        &tables,
        "the selected machine in the existing Tables tab",
        |text| shows(text, "tables/pane/id", machine),
    )?;
    if let Some(dir) = out {
        frame(inst, dir, "aregistry-machine-in-tables.png")?;
    }
    ok(inst, &["open", "app", "aRegistry"])?;
    if active_view(inst, "registry")? != registry {
        return Err(fail("returning to aRegistry created another view"));
    }
    until(
        inst,
        &registry,
        "the same machine on return to aRegistry",
        detail,
    )?;
    println!(
        "tables-page: registry_view={registry} tables_view={tables} machine={machine} selection_preserved=true"
    );
    Ok(())
}

/// The focused leaf of the private window's active tab, checked against the expected app.
fn active_view(inst: &Instance, kind: &str) -> Result<String, Fail> {
    let tabs = ok(inst, &["inspect", "app/v1", "tabs"])?;
    let mut active = false;
    for line in tabs.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("tab") => active = fields.any(|f| f == "active=true"),
            Some("leaf") if active && fields.clone().any(|f| f == "focused=true") => {
                if fields.clone().find_map(|f| f.strip_prefix("app=")) == Some(kind)
                    && let Some(id) = fields.find_map(|f| f.strip_prefix("view="))
                {
                    return Ok(id.to_owned());
                }
            }
            _ => {}
        }
    }
    Err(fail(format!("expected {kind} in front: {tabs}")))
}

/// Until `what` holds of the page's text, within the hang detector.
fn until(
    inst: &Instance,
    view: &str,
    what: &str,
    mut holds: impl FnMut(&str) -> bool,
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

/// Open the cell `key` on its list: a press selects it and a second opens it, where the first
/// did not (a cell already selected opens at one).
fn open_list(inst: &Instance, view: &str, key: &str) -> Result<(), Fail> {
    let listed = |inst: &Instance| {
        inst.ctl(&["inspect", "app/v1", "view", view, "text"])
            .stdout
            .contains("tables/choice/0")
    };
    act(inst, view, key, None)?;
    if !listed(inst) {
        act(inst, view, key, None)?;
    }
    until(inst, view, "a list to choose from", |text| {
        text.contains("tables/choice/0")
    })
}

/// The rail's line that opens Check.
const CHECK: &str = "tables/view/check";

/// Whether the history drawer stands, closed (its Show history) or open (its Hide history): a
/// window too short for its line over three lines of grid, judged from the window and the
/// filters' chips alone, has none.
fn drawer_stands(inst: &Instance, view: &str) -> bool {
    let shown = inst.ctl(&["inspect", "app/v1", "view", view, "text"]);
    shown.code == 0
        && (shown.stdout.contains("tables/history/show")
            || shown.stdout.contains("tables/history/hide"))
}

fn drive(
    inst: &mut Instance,
    out: &Path,
    stated: &str,
    facts: &str,
    narrow: bool,
) -> Result<Vec<PathBuf>, Fail> {
    inst.await_socket(HANG)?;
    let opening = Instant::now();
    ok(inst, &["open", "app", "tables"])?;
    let view = active_view(inst, "tables")?;
    // The compiled catalog can be visible before any rows have arrived. Wait for the cell
    // this drive selects below, so the first frame and its timing both reflect usable rows.
    until(inst, &view, "a selectable machine status", |text| {
        text.contains("tables/cell/mc-000000000000000c/status")
    })?;
    println!(
        "tables-page: open to first observed selectable machine status: {} ms",
        opening.elapsed().as_millis()
    );
    let mut shots = vec![frame(inst, out, "tables-1-read.png")?];
    // The rest of the walkthrough opens editors and stages changes on this scratch registry.
    until(inst, &view, "the machine sheet's Add button", |text| {
        text.contains(ADD)
    })?;
    // The Columns list: each column under its group, with how many rows fill it.
    act(inst, &view, "tables/columns", None)?;
    until(inst, &view, "the list of columns", |text| {
        text.contains("tables/columns/fold")
    })?;
    shots.push(frame(inst, out, "tables-1b-columns.png")?);
    act(inst, &view, "tables/columns", None)?;
    // The columns no row fills fold into a stub at the end of their group: STORED's stands
    // before the status.
    act(inst, &view, "tables/cell/mc-000000000000000c/status", None)?;
    until(inst, &view, "STORED's folded stub", |text| {
        text.contains("tables/fold/stored")
    })?;
    shots.push(frame(inst, out, "tables-1c-folded.png")?);
    // A press on the stub unfolds STORED, whose empty columns (a Mac's role among them) the
    // frames below then show as any other.
    act(inst, &view, "tables/fold/stored", None)?;
    until(inst, &view, "STORED unfolded", |text| {
        !text.contains("tables/fold/stored")
    })?;
    shots.push(frame(inst, out, "tables-1d-unfolded.png")?);
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
    // A grid too short for every word (a narrow page's, with the row's pane below it) shows
    // the words round the marked one, which need not be the first.
    until(inst, &view, "the role's words", |text| {
        text.contains("tables/choice/")
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
    // A label another machine already reads as, staged, says so before Write in the note's
    // ink, with the clash label this row reads as once written (docs/PLAN.md LT-46).
    let label = "tables/cell/mc-000000000000000b/label";
    act(inst, &view, label, None)?;
    act(inst, &view, label, None)?;
    act(inst, &view, "tables/edit", Some("m9"))?;
    act(inst, &view, location, None)?;
    act(inst, &view, label, None)?;
    until(inst, &view, "a label in use", |text| {
        text.contains("in use")
    })?;
    shots.push(frame(inst, out, "tables-4-in-use.png")?);
    act(inst, &view, "tables/discard", None)?;
    // The fold turned off draws every column, the empty ones too: the linked count and the
    // schema's last column below are among them, whatever the rows fill.
    act(inst, &view, "tables/columns", None)?;
    act(inst, &view, "tables/columns/fold", None)?;
    act(inst, &view, "tables/columns", None)?;
    until(inst, &view, "every column drawn", |text| {
        !text.contains("tables/fold/")
    })?;
    // A linked count: the cell bar says the count as its cell does, and the line under the
    // grid how many rows of which table name the row.
    act(
        inst,
        &view,
        "tables/cell/mc-000000000000000a/~machine.merged_into",
        None,
    )?;
    until(inst, &view, "a linked count", |text| {
        text.contains("names this row") || text.contains("name this row")
    })?;
    shots.push(frame(inst, out, "tables-4a-linked.png")?);
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
    until(inst, &view, "the machine models", |text| {
        text.contains("Machine models")
    })?;
    shots.push(frame(inst, out, "tables-11-models.png")?);
    // A plan's limits read in words, and an import's time as a date.
    choose(inst, &view, "tables/sheet/plan", narrow)?;
    until(inst, &view, "the plans", |text| text.contains("Plans"))?;
    act(inst, &view, "tables/cell/anthropic-max/limits", None)?;
    shots.push(frame(inst, out, "tables-11a-plans.png")?);
    choose(inst, &view, "tables/sheet/source", narrow)?;
    until(inst, &view, "the imports", |text| text.contains("Imports"))?;
    shots.push(frame(inst, out, "tables-11b-imports.png")?);
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
    // Back on the machines: a column's filters (docs/PLAN.md LT-41), then a find that leaves
    // the selected row out: the line under the grid still tells of the cell, and the cell bar
    // names it as not among the rows found.
    choose(inst, &view, "tables/sheet/machine", narrow)?;
    // A column's filter: the mark at its header's end opens the list of the values its rows
    // hold, each with how many rows hold it.
    act(inst, &view, "tables/mark/status", None)?;
    until(inst, &view, "the status's values", |text| {
        text.contains("tables/filter/value/active")
    })?;
    shots.push(frame(inst, out, "tables-13a-filter-list.png")?);
    // Two filters, each a chip in words under the tools: the active machines in the office or
    // in rack a.
    act(inst, &view, "tables/filter/value/active", None)?;
    act(inst, &view, "tables/mark/location", None)?;
    act(inst, &view, "tables/filter/value/office", None)?;
    act(inst, &view, "tables/filter/value/rack%20a", None)?;
    act(inst, &view, "tables/mark/location", None)?;
    until(inst, &view, "two chips", |text| {
        text.contains("tables/chips/chip/status") && text.contains("tables/chips/chip/location")
    })?;
    shots.push(frame(inst, out, "tables-13b-filtered.png")?);
    // A third filter no row passes: the grid says so, with Clear filters.
    act(inst, &view, "tables/mark/nickname", None)?;
    act(inst, &view, "tables/filter/empty", None)?;
    act(inst, &view, "tables/mark/nickname", None)?;
    until(inst, &view, "no row passing", |text| {
        text.contains("No row passes these filters")
    })?;
    shots.push(frame(inst, out, "tables-13c-none-pass.png")?);
    act(inst, &view, "tables/none/clear", None)?;
    until(inst, &view, "the filters cleared", |text| {
        !text.contains("tables/chips/")
    })?;
    act(inst, &view, "tables/cell/mc-000000000000000c/chip", None)?;
    act(inst, &view, "tables/find", Some("mc-000000000000000a"))?;
    until(inst, &view, "the selected cell, its row left out", |text| {
        text.contains("not among the rows found")
    })?;
    shots.push(frame(inst, out, "tables-14-found.png")?);
    // The history drawer, open under the grid at Table scope on the row found: every cell a
    // record set in the table, whatever the find shows, the newest line of the selected cell
    // (the one the two Macs wrote apart) in the grid's ring.
    act(
        inst,
        &view,
        "tables/cell/mc-000000000000000a/nickname",
        None,
    )?;
    // A window too short for the drawer's line over three lines of grid, judged from the
    // window and the filters' chips (none stands yet), has no drawer.
    let drawer = drawer_stands(inst, &view);
    if drawer {
        act(inst, &view, "tables/history/show", None)?;
        until(inst, &view, "the history drawer's list", |text| {
            text.contains("tables/history/hide")
        })?;
        shots.push(frame(inst, out, "tables-14a-history.png")?);
        // Cell scope on that cell: every value written to it, both of the conflict.
        act(inst, &view, "tables/history/scope/cell", None)?;
        shots.push(frame(inst, out, "tables-14b-history-cell.png")?);
        // The Columns list over the open drawer: laid over the grid and the drawer's list,
        // neither of which moves for it.
        act(inst, &view, "tables/columns", None)?;
        until(inst, &view, "the list of columns over the drawer", |text| {
            text.contains("tables/columns/fold")
        })?;
        shots.push(frame(inst, out, "tables-14c-history-columns.png")?);
        // The selected cell's column hidden from the list: the selection moves to the next
        // column drawn, and the drawer's Cell scope with it.
        act(inst, &view, "tables/columns/column/nickname", None)?;
        act(inst, &view, "tables/columns", None)?;
        // The key whole, in its quotes: `nickname_aliases` is another column.
        let nickname = "\"tables/head/nickname\"";
        until(inst, &view, "the nickname hidden", |text| {
            !text.contains(nickname)
        })?;
        shots.push(frame(inst, out, "tables-14d-history-hidden.png")?);
        act(inst, &view, "tables/columns", None)?;
        act(inst, &view, "tables/columns/column/nickname", None)?;
        act(inst, &view, "tables/columns", None)?;
        until(inst, &view, "the nickname shown again", |text| {
            text.contains(nickname)
        })?;
        // A column's list of values over the open drawer (docs/PLAN.md LT-41 with LT-39): laid
        // over the grid and the drawer, neither of which moves for it.
        act(inst, &view, "tables/mark/location", None)?;
        until(
            inst,
            &view,
            "the list of locations over the drawer",
            |text| text.contains("tables/filter/value/office"),
        )?;
        shots.push(frame(inst, out, "tables-14e-history-filter-list.png")?);
        // The office's machines alone, which leaves the found row out: the chip stands under
        // the tools, and the drawer still lists the row's records.
        act(inst, &view, "tables/filter/value/office", None)?;
        act(inst, &view, "tables/mark/location", None)?;
        until(inst, &view, "the location's chip", |text| {
            text.contains("tables/chips/chip/location")
        })?;
        // The chip's line counts in the window's judgment of the drawer, so a window that
        // only just held the drawer holds it no longer: no frame of it there.
        if drawer_stands(inst, &view) {
            act(inst, &view, "tables/history/scope/row", None)?;
            shots.push(frame(inst, out, "tables-14f-history-filtered.png")?);
        } else {
            println!(
                "tables-page: the filter's chip leaves this window too short for the history \
                 drawer; no frame of it filtered"
            );
        }
        // The filter taken off again. A press on one of the drawer's lines would clear it too
        // (docs/PLAN.md LT-41 with LT-39), but a line's key, which names its record by the
        // record's whole digest, is longer than the 64 bytes `act` takes for an action, so the
        // lane presses no line: the page's tests press them.
        act(inst, &view, "tables/chips/remove/location", None)?;
        until(inst, &view, "the location's chip taken off", |text| {
            !text.contains("tables/chips/")
        })?;
        act(inst, &view, "tables/history/hide", None)?;
    } else {
        println!("tables-page: this window is too short for the history drawer; no frame of it");
    }
    act(inst, &view, "tables/find", Some(""))?;
    // A link's machine is chosen from the machines' rows, each read by its words; two under one
    // label say each one's key ending, status and whether a Mac holds it.
    choose(inst, &view, "tables/sheet/preference", narrow)?;
    until(inst, &view, "the links' Add", |text| {
        text.contains("Add link")
    })?;
    act(inst, &view, "tables/rows/add", None)?;
    let machine = "tables/cell/+new-1/machine";
    until(inst, &view, "a new link", |text| text.contains(machine))?;
    open_list(inst, &view, machine)?;
    shots.push(frame(inst, out, "tables-15-choose-machine.png")?);
    act(inst, &view, "tables/choice/1", None)?;
    open_list(inst, &view, "tables/cell/+new-1/account")?;
    shots.push(frame(inst, out, "tables-16-choose-account.png")?);
    act(inst, &view, "tables/choice/0", None)?;
    act(inst, &view, "tables/discard", None)?;
    // A written link reads as the rows it names; its pane offers each.
    let link = aterm_db::typed::keys::preference_pk(MACHINE, ACCOUNT).map_err(fail)?;
    act(inst, &view, &format!("tables/cell/{link}/machine"), None)?;
    until(inst, &view, "the rows a link names", |text| {
        text.contains("tables/pane/row/to/machine")
    })?;
    shots.push(frame(inst, out, "tables-17-link.png")?);
    // The machine the link names: its Retire waits on the link, which the pane lists.
    choose(inst, &view, "tables/sheet/machine", narrow)?;
    act(inst, &view, &format!("tables/cell/{MACHINE}/label"), None)?;
    until(inst, &view, "the retire waiting on the link", |text| {
        text.contains("1 live row names it")
    })?;
    shots.push(frame(inst, out, "tables-18-retire-waits.png")?);
    // Ending the link from there frees the machine's Retire in the same Write.
    act(inst, &view, "tables/pane/row/by/0", None)?;
    until(inst, &view, "the link's End", |text| {
        text.contains("End this link")
    })?;
    act(inst, &view, "tables/pane/row/retire", None)?;
    choose(inst, &view, "tables/sheet/machine", narrow)?;
    act(inst, &view, &format!("tables/cell/{MACHINE}/label"), None)?;
    until(inst, &view, "the retire freed", |text| {
        text.contains("Retire this machine") && !text.contains("1 live row names it")
    })?;
    shots.push(frame(inst, out, "tables-19-retire-freed.png")?);
    // The machine's retire, staged from its pane, is the drawer's too: the first line of its
    // row's history, not written, with the Undo its pane offers under the fields.
    if drawer {
        act(inst, &view, "tables/pane/row/retire", None)?;
        let staged = format!("tables/history/staged/{MACHINE}/row");
        act(inst, &view, "tables/history/show", None)?;
        act(inst, &view, "tables/history/scope/row", None)?;
        until(inst, &view, "the staged retire in the drawer", |text| {
            text.contains(&staged)
        })?;
        shots.push(frame(inst, out, "tables-19a-history-retire.png")?);
        act(inst, &view, "tables/history/hide", None)?;
    }
    act(inst, &view, "tables/discard", None)?;
    // The lifecycle filter's next step: the retired rows.
    act(inst, &view, "tables/rows/filter", None)?;
    until(inst, &view, "the retired rows", |text| {
        text.contains("Retired rows shown")
    })?;
    shots.push(frame(inst, out, "tables-20-retired.png")?);
    // A new email: its password is set by a verb, never typed.
    choose(inst, &view, "tables/sheet/address", narrow)?;
    act(inst, &view, "tables/rows/add", None)?;
    until(inst, &view, "a new email's password line", |text| {
        text.contains("tables/pane/row/set/password")
    })?;
    shots.push(frame(inst, out, "tables-21-new-email.png")?);
    // The new row is the drawer's first line not written, though no cell of it is staged yet.
    if drawer {
        act(inst, &view, "tables/history/show", None)?;
        until(inst, &view, "the new row in the drawer", |text| {
            text.contains("tables/history/staged/+new-")
        })?;
        shots.push(frame(inst, out, "tables-21a-history-new.png")?);
        act(inst, &view, "tables/history/hide", None)?;
    }
    // Nothing of it is staged, so its pane's Remove takes it back.
    act(inst, &view, "tables/pane/row/drop", None)?;
    Ok(shots)
}
