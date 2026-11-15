// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `lab this-mac-page`: Settings ▸ This Mac on a private instance whose HOME, configuration,
//! runtime and state folders are all scratch (`World`). It opens `/machine`, waits for the
//! page's Number field, which only a landed read draws, takes a frame, types a nickname through
//! `act`, takes a frame of the live line, then asks `act` to press Save and expects the
//! person-only refusal, the scratch `machine-name.toml` keeping its revision. Every frame shows
//! the page's primary button (`name/primary`), the ACTIONS card's first: the grid is tall enough
//! that it sits above the fold under the NAME and RENAME cards, though the rest of the ACTIONS
//! card can run under it, and each frame first requires `inspect … controls` to draw the
//! primary on screen, printing its words and state. Nothing is written: a Save takes a person's
//! click, so an accepted one is judged by the writer's unit tests. This Mac's own ComputerName,
//! LocalHostName and HostName are read (never set) before and after, and the run fails if any
//! moved.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::instance::{CONFIG_OFF, Instance, Launch, World, resolve_aterm, take_value};
use crate::{Fail, Res};

const HANG: Duration = Duration::from_secs(60);
const SYSTEM_NAMES: [&str; 3] = ["ComputerName", "LocalHostName", "HostName"];
/// The page's primary button (Save, Retry Sync or Authorize & Sync), which every frame shows.
const PRIMARY: &str = "name/primary";
/// The grid: wide as a laptop's Settings window, and tall enough that the ACTIONS card's
/// primary sits above the fold under the NAME and RENAME cards.
const COLUMNS: &str = "160";
const LINES: &str = "64";

fn fail(e: impl std::fmt::Display) -> Fail {
    Fail::die(format!("this-mac-page: {e}"))
}

/// This Mac's three names as `scutil --get` reads them (an unset one reads as its refusal).
fn system_names() -> Result<Vec<String>, Fail> {
    SYSTEM_NAMES
        .iter()
        .map(|which| {
            let out = aterm_uds::spawnfd::command("/usr/sbin/scutil")
                .args(["--get", which])
                .output()
                .map_err(fail)?;
            let text = if out.status.success() {
                String::from_utf8_lossy(&out.stdout).trim().to_owned()
            } else {
                format!("(unread: {})", String::from_utf8_lossy(&out.stderr).trim())
            };
            Ok(format!("{which}={text}"))
        })
        .collect()
}

/// The scratch name file's revision, `None` before one is made.
fn name_rev(state: &Path) -> Result<Option<u64>, Fail> {
    Ok(aterm_agent::machine_name::file::read(state)
        .map_err(fail)?
        .map(|name| name.rev))
}

/// Run the lane; `--out DIR` names where the frames go (the world's folder by default).
///
/// # Errors
/// The instance did not start or answer, the page never read, a frame failed, `act` pressed
/// Save, the scratch name moved, or this Mac's names moved.
pub fn cmd(args: &[String]) -> Res {
    let mut args = args.to_vec();
    let out = take_value(&mut args, "--out")?.map(|p| PathBuf::from(crate::paths::from_caller(&p)));
    if args.len() > 1 {
        return Err(Fail::usage(
            "this-mac-page takes --out DIR and an aterm at most",
        ));
    }
    let aterm = resolve_aterm(args.pop()).map_err(|e| Fail::exit(3, e))?;
    let before = system_names()?;
    println!("this-mac-page: before: {}", before.join(" "));
    let world = World::new(CONFIG_OFF).map_err(fail)?;
    if aterm_types::dirs::home_dir().is_some_and(|live| live == world.home) {
        return Err(fail("the world's home is this user's"));
    }
    let out = out.unwrap_or_else(|| world.dir.clone());
    std::fs::create_dir_all(&out).map_err(fail)?;
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm,
            headless: true,
            args: vec![
                "--columns".into(),
                COLUMNS.into(),
                "--lines".into(),
                LINES.into(),
            ],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
    )
    .map_err(fail)?;
    let driven = drive(&mut inst, &out, &world.state());
    crate::instance::after_stop(driven, inst.stop())?;
    let after = system_names()?;
    println!("this-mac-page: after: {}", after.join(" "));
    if before != after {
        return Err(fail(format!(
            "this Mac's names moved: {} -> {}",
            before.join(" "),
            after.join(" ")
        )));
    }
    Ok(0)
}

fn ok(inst: &Instance, args: &[&str]) -> Result<String, Fail> {
    let reply = inst.ctl(args);
    if reply.code != 0 {
        return Err(fail(format!("{}: {}", args.join(" "), reply.first())));
    }
    Ok(reply.stdout)
}

/// The Settings view's id, from `inspect app/v1 tabs`.
fn settings_view(inst: &Instance) -> Result<String, Fail> {
    let tabs = ok(inst, &["inspect", "app/v1", "tabs"])?;
    tabs.lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("leaf ") && line.contains(" app=settings"))
        .find_map(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("view="))
                .map(str::to_owned)
        })
        .ok_or_else(|| fail("no Settings tab"))
}

/// Until `what` holds of the page's controls, within the hang detector.
fn until(
    inst: &Instance,
    view: &str,
    what: &str,
    holds: impl Fn(&str) -> bool,
) -> Result<(), Fail> {
    let start = Instant::now();
    loop {
        let reply = inst.ctl(&["inspect", "app/v1", "view", view, "controls"]);
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

/// `key`'s line in an `inspect … controls` reply where the page draws it on screen: listed, and
/// not marked `offscreen=true` (a control scrolled out of its list's room).
fn on_screen<'a>(controls: &'a str, key: &str) -> Option<&'a str> {
    let opens = format!("ui key={key:?} ");
    controls
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with(&opens) && !line.ends_with(" offscreen=true"))
}

/// Field `name` of a controls line, the words between it and the field `next` that follows it
/// (`label="Save"` before `value=`, `state=enabled:true …` before `rect=`).
fn field<'a>(line: &'a str, (name, next): (&str, &str)) -> &'a str {
    line.split_once(&format!(" {name}="))
        .and_then(|(_, rest)| rest.split_once(&format!(" {next}=")))
        .map_or("", |(value, _)| value)
}

/// A frame of the page, once the controls draw its primary on screen (within the hang detector).
fn frame(inst: &Instance, view: &str, out: &Path, name: &str) -> Result<PathBuf, Fail> {
    until(inst, view, "its primary on screen", |controls| {
        on_screen(controls, PRIMARY).is_some()
    })?;
    let controls = ok(inst, &["inspect", "app/v1", "view", view, "controls"])?;
    let primary = on_screen(&controls, PRIMARY)
        .ok_or_else(|| fail(format!("{PRIMARY} left the screen before {name}")))?;
    let enabled = field(primary, ("state", "rect"))
        .split_whitespace()
        .find_map(|state| state.strip_prefix("enabled:"))
        .unwrap_or("?");
    let reply = inst.ctl(&["image", name]);
    let path = reply
        .image_path()
        .ok_or_else(|| fail(format!("image: {}", reply.first())))?;
    let to = out.join(name);
    std::fs::copy(path, &to).map_err(fail)?;
    println!(
        "this-mac-page: {} shows {PRIMARY} label={} enabled={enabled} on screen",
        to.display(),
        field(primary, ("label", "value")),
    );
    Ok(to)
}

fn drive(inst: &mut Instance, out: &Path, state: &Path) -> Result<(), Fail> {
    inst.await_socket(HANG)?;
    ok(inst, &["open", "app", "settings", "/machine"])?;
    let view = settings_view(inst)?;
    until(inst, &view, "its read", |controls| {
        controls.contains("name/field/number")
    })?;
    frame(inst, &view, out, "this-mac-1-read.png")?;
    let nick = "name/field/nick";
    ok(inst, &["act", "app/v1", "view", &view, nick, nick, "Heron"])?;
    until(inst, &view, "the typed nickname", |controls| {
        controls.contains("Heron")
    })?;
    frame(inst, &view, out, "this-mac-2-typed.png")?;
    let rev = name_rev(state)?;
    let save = "name/press/save";
    let reply = inst.ctl(&["act", "app/v1", "view", &view, PRIMARY, save]);
    if reply.code == 0 || !reply.first().contains("machine-name buttons") {
        return Err(fail(format!(
            "act pressed Save, or refused it for another reason: {} {}",
            reply.code,
            reply.first()
        )));
    }
    println!("this-mac-page: act refused Save: {}", reply.first());
    let kept = name_rev(state)?;
    if kept != rev {
        return Err(fail(format!(
            "the scratch name moved from rev {rev:?} to {kept:?}"
        )));
    }
    println!("this-mac-page: the scratch name kept rev {rev:?}");
    frame(inst, &view, out, "this-mac-3-refused.png")?;
    Ok(())
}
