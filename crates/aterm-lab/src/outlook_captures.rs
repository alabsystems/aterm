// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `outlook-captures` ([`cmd_outlook_captures`]): the accounts outlook as the window draws it
//! (`docs/DESIGN-accounts-outlook-2026-10-25.md` §11, *Lab captures*). One private headless
//! instance per scene, look and scale, its HOME seeded with synthetic identities only
//! (`me@example.test`, `alt1`, `alt2`, `pool1`): a Claude Code profile, the roster, the accounts
//! store and `me`'s series ([`seed`]). Its one tab runs the lab's `claude` fixture
//! ([`crate::fleet_fixture::claude`], by a link of that name), so the footer reads the profile it
//! was seeded with. Each scene's footer is shot at the three widths of §2.4 and the covered, gap and
//! never-read scenes' Accounts card at 60 and 44 cells; beside each png the card's rows
//! (`controls fact-menu`) and a `manifest.tsv` row. `--aterm` points the same seeds at another
//! build (main's, for the shots side by side).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::account_series;
use aterm_agent::harness::accounts::{self, Class, TapRead, WindowFig, WindowRead};

use crate::band::Band;
use crate::instance::{
    BOOT, Instance, Launch, World, config_after, config_with, take_value, with_harness_keys,
};
use crate::{Fail, Res};

const M: i64 = 60;
const H: i64 = 3600;
const D: i64 = 86_400;

/// The scenes of §2.4: A covered, B a gap, C an alt never read, D a 5-hour stop, E no pace yet,
/// F walled, G an aged figure, H no figure.
pub const SCENES: [char; 8] = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];

/// The scenes whose Accounts card is shot: covered, a gap, an alt never read (§8.2).
pub const CARD_SCENES: [char; 3] = ['A', 'B', 'C'];

/// The footer's three widths of §2.4, as pane columns.
pub const WIDTHS: [(&str, u16); 3] = [("wide", 170), ("medium", 100), ("narrow", 70)];

/// The card's two widths (§8.4): 60 cells beside its drawn rows, and 44, as pane columns.
pub const CARD_WIDTHS: [(&str, u16); 2] = [("card60", 100), ("card44", 48)];

/// The pane's rows.
const LINES: u16 = 40;

/// How long a footer may take to show the account unit: a hang detector, not a budget.
const SHOWN: Duration = Duration::from_secs(60);

/// A look: the theme its instance's `aterm.toml` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Day,
    Night,
}

impl Look {
    const BOTH: [Look; 2] = [Look::Day, Look::Night];

    const fn name(self) -> &'static str {
        match self {
            Look::Day => "day",
            Look::Night => "night",
        }
    }

    const fn theme(self) -> &'static str {
        match self {
            Look::Day => "Aterm Day",
            Look::Night => "Aterm Night",
        }
    }
}

/// One account's seeded reading: `(pct_x10, reset, read)` of its week and of its five hours, as
/// offsets from now.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    week: Option<(u32, i64, i64)>,
    five: Option<(u32, i64, i64)>,
}

/// What a scene seeds.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The roster's rows in order: `me` (this Mac's own login), then logins with a folder each.
    rows: Vec<(&'static str, Class)>,
    /// The store's readings.
    seen: Vec<(&'static str, Seen)>,
    /// `me`'s series: `(offset, pct_x10)` of its week.
    series: Vec<(i64, u32)>,
    /// The profile's cached figures `(week %, five-hour %, age in seconds)`, or none read.
    profile: Option<(u32, u32, i64)>,
}

/// This Mac's own login's address: synthetic.
const ME_EMAIL: &str = "me@example.test";

/// `me`'s week resets Thu 09:00 when now is Sun 19:30; its five hours at 22:00.
const ME_WEEK: i64 = 3 * D + 13 * H + 30 * M;
const ME_FIVE: i64 = 2 * H + 30 * M;

/// Scene `scene`'s seeds, the appendix of the design moved to now.
#[must_use]
pub fn plan(scene: char) -> Plan {
    let me = |week: u32, five: u32, read: i64| Seen {
        week: Some((week, ME_WEEK, read)),
        five: Some((five, ME_FIVE, read)),
    };
    let week = |pct: u32, reset: i64, read: i64| Seen {
        week: Some((pct, reset, read)),
        five: None,
    };
    let alt1 = |pct: u32| week(pct, 4 * D + 18 * H + 30 * M, -3 * H);
    let pace = vec![(-24 * H, 420), (-16 * H, 473), (-8 * H, 527), (0, 580)];
    let two = vec![("me", Class::Dedicated), ("alt1", Class::Dedicated)];
    match scene {
        'A' | 'G' => {
            let aged = if scene == 'G' { 16 * M } else { 0 };
            Plan {
                rows: vec![
                    ("me", Class::Dedicated),
                    ("alt1", Class::Dedicated),
                    ("alt2", Class::Dedicated),
                    ("pool1", Class::Shared),
                ],
                seen: vec![
                    ("me", me(580, 380, -aged)),
                    ("alt1", alt1(310)),
                    ("alt2", week(710, -(3 * D + 8 * H + 30 * M), -6 * D)),
                    ("pool1", week(400, 5 * D + 12 * H + 30 * M, -5 * H)),
                ],
                series: pace,
                profile: Some((58, 38, aged)),
            }
        }
        'B' => Plan {
            rows: two,
            seen: vec![("me", me(580, 380, 0)), ("alt1", alt1(920))],
            series: pace,
            profile: Some((58, 38, 0)),
        },
        'C' => Plan {
            rows: vec![
                ("me", Class::Dedicated),
                ("alt1", Class::Dedicated),
                ("pool1", Class::Shared),
            ],
            seen: vec![("me", me(580, 380, 0)), ("alt1", alt1(920))],
            series: pace,
            profile: Some((58, 38, 0)),
        },
        'D' => Plan {
            rows: two,
            seen: vec![
                ("me", me(580, 810, 0)),
                (
                    "alt1",
                    Seen {
                        five: Some((950, H + 40 * M, -20 * M)),
                        ..alt1(310)
                    },
                ),
            ],
            series: pace,
            profile: Some((58, 81, 0)),
        },
        'E' => Plan {
            rows: two,
            seen: vec![
                (
                    "me",
                    Seen {
                        week: Some((30, 6 * D + 22 * H + 40 * M, 0)),
                        five: Some((380, ME_FIVE, 0)),
                    },
                ),
                ("alt1", alt1(310)),
            ],
            series: vec![(-40 * M, 20), (0, 30)],
            profile: Some((3, 38, 0)),
        },
        'F' => Plan {
            rows: two,
            seen: vec![("me", me(1000, 380, 0)), ("alt1", alt1(310))],
            series: pace,
            profile: Some((100, 38, 0)),
        },
        _ => Plan {
            rows: two,
            seen: vec![("alt1", alt1(310))],
            series: Vec::new(),
            profile: None,
        },
    }
}

/// The harness state an instance in `world` keeps its accounts store under.
fn harness_state(world: &World) -> PathBuf {
    world
        .state()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS)
}

/// The synthetic Claude Code profile of `me`: its account, its Max 20x plan and, where `plan`
/// read them, its cached figures.
fn profile(plan: &Plan, now: i64) -> String {
    let account = aterm_json::json!({
        "accountUuid": "outlook-fixture-me",
        "emailAddress": ME_EMAIL,
        "organizationType": "claude_max",
        "userRateLimitTier": "default_claude_max_20x",
        "planDisplayName": "Max 20x"
    });
    let body = match plan.profile {
        Some((week, five, age)) => aterm_json::json!({
            "oauthAccount": account,
            "cachedUsageUtilization": {
                "fetchedAtMs": (now - age) * 1000,
                "accountUuid": "outlook-fixture-me",
                "utilization": {
                    "seven_day": {
                        "utilization": week,
                        "resets_at": crate::util::utc_stamp(now + ME_WEEK)
                    },
                    "five_hour": {
                        "utilization": five,
                        "resets_at": crate::util::utc_stamp(now + ME_FIVE)
                    }
                }
            }
        }),
        None => aterm_json::json!({ "oauthAccount": account }),
    };
    aterm_json::to_string(&body).unwrap_or_default()
}

/// Seed `world` for `plan` at `now`: the profile in its HOME, the roster beside its
/// `aterm.toml`, a placeholder file in each other login's folder, the store's readings with `me`
/// active, and `me`'s series. Nothing outside `world` is written.
/// # Errors
/// A file could not be written, or the store refused a reading.
pub fn seed(world: &World, plan: &Plan, now: i64) -> Result<(), String> {
    let io = |what: &str, e: std::io::Error| format!("outlook seed: {what}: {e}");
    std::fs::write(world.home.join(".claude.json"), profile(plan, now))
        .map_err(|e| io("profile", e))?;
    let mut rows = Vec::new();
    for &(label, class) in &plan.rows {
        let dir = (label != "me").then(|| world.home.join("logins").join(label));
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).map_err(|e| io(label, e))?;
            std::fs::write(dir.join("a-login-file"), "{}").map_err(|e| io(label, e))?;
        }
        rows.push(accounts::Row {
            label: label.into(),
            class,
            dir: dir.map(|d| d.to_string_lossy().into_owned()),
            // `me` names the profile's address, so the footer's gate reads its profile under a
            // roster of several rows.
            email: (label == "me").then(|| ME_EMAIL.into()),
            ..accounts::Row::default()
        });
    }
    let roster = accounts::append_rows("", &rows)?;
    std::fs::write(world.cfg.join("aterm").join("accounts.toml"), roster)
        .map_err(|e| io("roster", e))?;
    let state_dir = harness_state(world);
    std::fs::create_dir_all(&state_dir).map_err(|e| io("state", e))?;
    let store = accounts::Store::at(&state_dir);
    let (mut state, _) = store.load(now)?;
    state.active = Some("me".into());
    let window = |seen: Option<(u32, i64, i64)>| {
        seen.map(|(pct_x10, reset, _)| WindowRead {
            pct_x10,
            reset: now + reset,
            refilled: None,
        })
    };
    for (label, seen) in &plan.seen {
        let at = now + seen.week.or(seen.five).map_or(0, |(_, _, read)| read);
        let read = TapRead {
            five_hour: window(seen.five),
            seven_day: window(seen.week),
            at,
            seen_before: true,
            born: None,
            key: None,
        };
        let _ = accounts::note_reading_held(&mut state, label, &read, at);
    }
    store.save(&state).map_err(|e| io("store", e))?;
    for &(offset, pct_x10) in &plan.series {
        let at = now + offset;
        let reset = plan
            .seen
            .iter()
            .find(|(label, _)| *label == "me")
            .and_then(|(_, s)| s.week)
            .map_or(now + ME_WEEK, |(_, reset, _)| now + reset);
        let fig = WindowFig {
            pct_x10,
            reset,
            at,
            by: state.machine.clone(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        };
        account_series::note(&state_dir, "me", &state.machine, None, Some(&fig), at)
            .map_err(|e| io("series", e))?;
    }
    // The appendix's plans, as earlier footers' gated profile reads wrote them (`me`'s an hour
    // ago, where its profile was read; the alts' on their own sessions); `pool1`'s was never
    // seen. The instance's own read of `me` writes nothing new for the same tier.
    for (label, tier, ago) in [
        ("me", "20x", H),
        ("alt1", "20x", 2 * D),
        ("alt2", "5x", 6 * D),
    ] {
        let read = label != "me" || plan.profile.is_some();
        if read && plan.rows.iter().any(|(l, _)| *l == label) {
            let said = account_series::PlanSaid {
                word: "max".into(),
                tier: Some(tier.into()),
                src: "profile".into(),
            };
            account_series::note_plan(&state_dir, label, &state.machine, &said, now - ago)
                .map_err(|e| io("plan", e))?;
        }
    }
    Ok(())
}

/// One shot the manifest lists.
struct Row {
    scene: char,
    what: String,
    look: Look,
    scale: u8,
    png: PathBuf,
    note: String,
}

/// What `outlook-captures` was asked for.
struct Opts {
    aterm: Option<String>,
    out: PathBuf,
    tag: String,
    scenes: Vec<char>,
    looks: Vec<Look>,
    scales: Vec<u8>,
}

const HELP: &str =
    "usage: targo --unverified lab outlook-captures --out DIR [--aterm ATERM] [--tag NAME]
       [--scenes ABCDEFGH] [--look day|night|both] [--scale 1|2|both]

The accounts outlook on a private headless instance per scene, look and scale, seeded with
synthetic identities only: the footer at 170, 100 and 70 columns, and the A, B and C scenes'
Accounts card at 60 and 44 cells. Writes outlook-<tag>-<scene>-<width>-<look>-<scale>x.png, the
card's rows beside each card png, and manifest.tsv.
";

fn opts(args: &[String]) -> Result<Opts, Fail> {
    let mut a = args.to_vec();
    if a.iter().any(|w| w == "-h" || w == "--help") {
        return Err(Fail::usage(HELP));
    }
    let aterm = take_value(&mut a, "--aterm")?;
    let out = take_value(&mut a, "--out")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage("outlook-captures: --out DIR is required"))?;
    let tag = take_value(&mut a, "--tag")?.unwrap_or_else(|| "branch".into());
    let scenes: Vec<char> =
        take_value(&mut a, "--scenes")?.map_or_else(|| SCENES.to_vec(), |s| s.chars().collect());
    if let Some(bad) = scenes.iter().find(|c| !SCENES.contains(c)) {
        return Err(Fail::usage(format!("outlook-captures: no scene {bad}")));
    }
    let looks = match take_value(&mut a, "--look")?.as_deref() {
        None | Some("both") => Look::BOTH.to_vec(),
        Some("day") => vec![Look::Day],
        Some("night") => vec![Look::Night],
        Some(other) => return Err(Fail::usage(format!("outlook-captures: no look {other}"))),
    };
    let scales = match take_value(&mut a, "--scale")?.as_deref() {
        None | Some("both") => vec![1, 2],
        Some("1") => vec![1],
        Some("2") => vec![2],
        Some(other) => return Err(Fail::usage(format!("outlook-captures: no scale {other}"))),
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "outlook-captures: unexpected '{extra}'"
        )));
    }
    Ok(Opts {
        aterm,
        out,
        tag,
        scenes,
        looks,
        scales,
    })
}

/// `outlook-captures`: see [`HELP`].
/// # Errors
/// Usage, or an instance that would not start or shoot.
pub fn cmd_outlook_captures(args: &[String]) -> Res {
    crate::stop::install();
    let o = opts(args)?;
    let aterm = crate::instance::resolve_aterm(o.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    std::fs::create_dir_all(&o.out)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", o.out.display())))?;
    let mut rows = Vec::new();
    for &scene in &o.scenes {
        for &look in &o.looks {
            for &scale in &o.scales {
                rows.extend(shoot(&o, &aterm, scene, look, scale)?);
            }
        }
    }
    let mut manifest = String::from("scene\twhat\tlook\tscale\tpng\tnote\n");
    for r in &rows {
        manifest.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            r.scene,
            r.what,
            r.look.name(),
            r.scale,
            r.png.display(),
            r.note
        ));
    }
    let path = o.out.join(format!("manifest-{}.tsv", o.tag));
    std::fs::write(&path, manifest)
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
    println!("outlook-captures: {} shots, {}", rows.len(), path.display());
    Ok(0)
}

/// A seeded world and the private headless instance running in it, the instance first so it
/// stops before its world is removed.
struct Lab {
    inst: Instance,
    _world: World,
}

/// Seed a world for `scene` in `look` and start `aterm` in it at `scale`, its one tab the lab's
/// `claude` fixture at `cols` columns.
fn launch(aterm: &Path, scene: char, look: Look, scale: u8, cols: u16) -> Result<Lab, Fail> {
    let config = config_after(
        &config_with(&[format!("theme = {:?}", look.theme())]),
        true,
        &[],
    );
    let config = with_harness_keys(&config, &["autopilot = \"off\"".into()]);
    let world = World::new(&config)?;
    let now = crate::util::now_secs();
    seed(&world, &plan(scene), now).map_err(Fail::die)?;
    let bin = world.dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| Fail::die(format!("bin: {e}")))?;
    let claude = bin.join("claude");
    std::os::unix::fs::symlink(&crate::paths::paths().exe, &claude)
        .map_err(|e| Fail::die(format!("claude link: {e}")))?;
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: true,
            args: vec![
                "--columns".into(),
                cols.to_string(),
                "--lines".into(),
                LINES.to_string(),
                "--scale".into(),
                scale.to_string(),
                "-e".into(),
                claude.to_string_lossy().into_owned(),
                "idle".into(),
            ],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        Band::Agent,
    )?;
    inst.await_socket(BOOT)?;
    Ok(Lab {
        inst,
        _world: world,
    })
}

/// One scene in one look and scale: its instance, its footer at each width, its card.
fn shoot(o: &Opts, aterm: &Path, scene: char, look: Look, scale: u8) -> Result<Vec<Row>, Fail> {
    let (_, first) = WIDTHS[0];
    let mut lab = launch(aterm, scene, look, scale, first)?;
    let inst = &lab.inst;
    let mut rows = Vec::new();
    let name = |what: &str| {
        o.out.join(format!(
            "outlook-{}-{scene}-{what}-{}-{scale}x.png",
            o.tag,
            look.name()
        ))
    };
    let scratch = o.out.join(format!(".outlook-{}-{scene}-frame.png", o.tag));
    settle(inst)?;
    let mut at = first;
    for (what, cols) in WIDTHS {
        resize(inst, (at, cols), &scratch)?;
        at = cols;
        let png = name(what);
        let note = image(inst, &png)?;
        rows.push(Row {
            scene,
            what: what.into(),
            look,
            scale,
            png,
            note,
        });
    }
    if CARD_SCENES.contains(&scene) {
        for (what, cols) in CARD_WIDTHS {
            resize(inst, (at, cols), &scratch)?;
            at = cols;
            let card = open_card(inst, &scratch)?;
            let png = name(what);
            let note = image(inst, &png)?;
            let rows_file = png.with_extension("controls.txt");
            std::fs::write(&rows_file, &card)
                .map_err(|e| Fail::die(format!("cannot write {}: {e}", rows_file.display())))?;
            let _ = inst.ctl(&["key", "escape"]);
            rows.push(Row {
                scene,
                what: what.into(),
                look,
                scale,
                png,
                note,
            });
        }
    }
    let _ = std::fs::remove_file(&scratch);
    lab.inst.stop();
    Ok(rows)
}

/// A frame of the window: its width in pixels and its pixels.
fn frame(inst: &Instance, scratch: &Path) -> Result<(usize, Vec<u8>), Fail> {
    image(inst, scratch)?;
    let bytes = std::fs::read(scratch)
        .map_err(|e| Fail::die(format!("cannot read {}: {e}", scratch.display())))?;
    let (rgba, w, _) = aterm_png::decode_rgba8(&bytes, 1 << 15)
        .ok_or_else(|| Fail::die(format!("{}: not a png", scratch.display())))?;
    Ok((w, rgba))
}

/// Wait until the window draws a steady frame: two frames in a row alike, 300 ms apart, the
/// first of them `fresh` (a frame drawn after the change waited on). A one-minute hang
/// detector ([`SHOWN`]), never a budget: the fixture repaints the same screen and a headless
/// window never animates, so a frame that will not hold still is a failure.
/// # Errors
/// No steady frame within [`SHOWN`], or the lab was stopped.
fn steady(
    inst: &Instance,
    scratch: &Path,
    what: &str,
    fresh: impl Fn(usize) -> bool,
) -> Result<(), Fail> {
    let end = Instant::now() + SHOWN;
    let mut last: Option<Vec<u8>> = None;
    while Instant::now() < end {
        let (w, px) = frame(inst, scratch)?;
        if !fresh(w) {
            last = None;
        } else if last.as_ref() == Some(&px) {
            return Ok(());
        } else {
            last = Some(px);
        }
        crate::stop::sleep(Duration::from_millis(300))?;
    }
    Err(Fail::die(format!(
        "outlook-captures: {what}: no steady frame within {} s",
        SHOWN.as_secs()
    )))
}

/// Resize the pane from `from` to `cols` columns: the verb must take it, and the window must
/// draw it there (a frame of another width where the width moves) and hold still.
/// # Errors
/// A refused resize, no steady frame at the new width, or the lab was stopped.
fn resize(inst: &Instance, (from, cols): (u16, u16), scratch: &Path) -> Result<(), Fail> {
    let (before, _) = frame(inst, scratch)?;
    let said = inst.ctl(&["resize", &LINES.to_string(), &cols.to_string()]);
    if said.code != 0 {
        return Err(Fail::die(format!(
            "outlook-captures: resize to {cols}: {}",
            said.first()
        )));
    }
    steady(inst, scratch, &format!("resize to {cols}"), |w| {
        from == cols || w != before
    })
}

/// Open the Accounts card and wait for it to be drawn: `controls fact-menu` names it open on
/// the account with its rows, and the frame holds still. Its controls text.
/// # Errors
/// A refused open, a card that never opened within [`SHOWN`], or the lab was stopped.
fn open_card(inst: &Instance, scratch: &Path) -> Result<String, Fail> {
    let opened = inst.ctl(&["open", "fact-menu", "account"]);
    if opened.code != 0 {
        return Err(Fail::die(format!(
            "outlook-captures: open the card: {}",
            opened.first()
        )));
    }
    let end = Instant::now() + SHOWN;
    loop {
        let card = inst.ctl(&["controls", "fact-menu"]).stdout;
        let head = card.lines().next().unwrap_or_default();
        if head.contains("open=true") && head.contains("fact=account") && !head.contains("rows=0") {
            steady(inst, scratch, "the card", |_| true)?;
            return Ok(inst.ctl(&["controls", "fact-menu"]).stdout);
        }
        if Instant::now() >= end {
            return Err(Fail::die(format!(
                "outlook-captures: the card did not open within {} s: {head}",
                SHOWN.as_secs()
            )));
        }
        crate::stop::sleep(Duration::from_millis(300))?;
    }
}

/// Wait until the window knows the fixture as Claude Code (`status program=claude`), then for
/// the footer's resolver to look at the roster: it looks every 10 s and its memo replays at its
/// first look, so the frames after 12 s hold the account unit. [`SHOWN`] is a hang detector.
/// # Errors
/// The fixture never showed as Claude Code within [`SHOWN`], or the lab was stopped.
fn settle(inst: &Instance) -> Result<(), Fail> {
    let end = Instant::now() + SHOWN;
    while Instant::now() < end {
        let status = inst.ctl(&["status"]);
        if status.stdout.contains("program=claude") {
            crate::stop::sleep(Duration::from_secs(12))?;
            return Ok(());
        }
        crate::stop::sleep(Duration::from_millis(500))?;
    }
    Err(Fail::die(format!(
        "outlook-captures: the fixture never showed as Claude Code within {} s",
        SHOWN.as_secs()
    )))
}

/// Take the frame as `png`: the verb's verdict line.
fn image(inst: &Instance, png: &Path) -> Result<String, Fail> {
    let shot = inst.ctl(&["image", "outlook.png"]);
    let path = shot.stdout.split_whitespace().nth(3).unwrap_or_default();
    if shot.code != 0 || path.is_empty() {
        return Err(Fail::die(format!(
            "outlook-captures: image: {}",
            shot.first()
        )));
    }
    std::fs::copy(path, png)
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", png.display())))?;
    Ok(shot.first())
}

/// What `outlook-perf` was asked for.
struct PerfOpts {
    main: PathBuf,
    aterm: Option<String>,
    out: PathBuf,
    pairs: usize,
}

const PERF_HELP: &str =
    "usage: targo --unverified lab outlook-perf --main ATERM --out DIR [--aterm ATERM] [--pairs N]

The outlook's cost against another build on the same seeds (scene A: a 4-account roster, the
footer's unit with its bar and outlook, Aterm Day, 1x, 170x40). Each pair starts both builds'
private headless instances at once and, behind a barrier, types into both at once (200 keys, then
200 backspaces, 50 ms apart, under one `video 60 keys`), then leaves both idle 20 s. Prints and
writes perf.txt: per arm the typing take's `metrics percentiles`, the idle window's timer wakes,
renders, deadline owners and wakes by variant, the 1-minute load, and the resolver's own cost on
the seed, each account's series grown to a full one (the store and logins read, the series read
whole and by its tail, and the replay on this machine's clock), in microseconds.
";

/// The typing take's `metrics percentiles` keys.
const TYPING_KEYS: [&str; 8] = [
    "n_input",
    "input_p50_ms",
    "input_p95_ms",
    "input_p99_ms",
    "n_render",
    "render_p50_ms",
    "render_p95_ms",
    "render_p99_ms",
];

/// The idle window's `metrics` keys.
const IDLE_KEYS: [&str; 5] = [
    "timer_wakes",
    "frames",
    "deadline_arms_by_owner",
    "deadline_fires_by_owner",
    "wakes_by_variant",
];

/// The idle window.
const IDLE: Duration = Duration::from_secs(20);

fn perf_opts(args: &[String]) -> Result<PerfOpts, Fail> {
    let mut a = args.to_vec();
    if a.iter().any(|w| w == "-h" || w == "--help") {
        return Err(Fail::usage(PERF_HELP));
    }
    let main = take_value(&mut a, "--main")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage("outlook-perf: --main ATERM is required"))?;
    let aterm = take_value(&mut a, "--aterm")?;
    let out = take_value(&mut a, "--out")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage("outlook-perf: --out DIR is required"))?;
    let pairs = take_value(&mut a, "--pairs")?
        .map_or(Ok(3), |n| n.parse::<usize>())
        .map_err(|e| Fail::usage(format!("outlook-perf: --pairs: {e}")))?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!("outlook-perf: unexpected '{extra}'")));
    }
    Ok(PerfOpts {
        main,
        aterm,
        out,
        pairs: pairs.max(1),
    })
}

/// `key=value` words of `text` for `keys`, in order, `-` for one it lacks.
fn picked(text: &str, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| {
            let v = crate::forever::word(text, k).unwrap_or_else(|| "-".into());
            format!("{k}={v}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The burst: 200 keys, then 200 backspaces, 50 ms apart.
fn burst(inst: &Instance) -> Result<(), Fail> {
    let keys = "the quick brown fox jumps over the lazy dog ";
    for c in keys.chars().cycle().take(200) {
        if c == ' ' {
            let _ = inst.ctl(&["key", "space"]);
        } else {
            let _ = inst.ctl(&["send", &c.to_string()]);
        }
        crate::stop::sleep(Duration::from_millis(50))?;
    }
    for _ in 0..200 {
        let _ = inst.ctl(&["key", "backspace"]);
        crate::stop::sleep(Duration::from_millis(50))?;
    }
    Ok(())
}

/// One arm of one pair: its instance on scene A, the typing take and the idle window behind
/// `gate` (both arms at once), its two lines.
fn arm(aterm: &Path, gate: &std::sync::Barrier) -> Result<(String, String), Fail> {
    let lab = launch(aterm, 'A', Look::Day, 1, WIDTHS[0].1);
    let ready = lab.as_ref().map_err(|f| f.msg.clone().unwrap_or_default());
    let shown = match &ready {
        Ok(lab) => settle(&lab.inst),
        Err(_) => Ok(()),
    };
    gate.wait();
    let lab = lab?;
    shown?;
    let inst = &lab.inst;
    let _ = inst.ctl(&["metrics", "reset"]);
    let video = std::thread::scope(|s| {
        let recording = s.spawn(|| inst.ctl(&["video", "60", "keys"]).first());
        crate::stop::sleep(Duration::from_millis(500))?;
        let typed = burst(inst);
        let pct = inst.ctl(&["metrics", "percentiles"]).stdout;
        let _ = inst.ctl(&["video", "stop"]);
        let said = recording
            .join()
            .unwrap_or_else(|_| "ERR the recording's thread panicked".into());
        typed.map(|()| (pct, said))
    });
    let (pct, said) = video?;
    let typing = format!(
        "typing: {} video={:?}",
        picked(&pct, &TYPING_KEYS),
        crate::util::one_line(&said, 120)
    );
    gate.wait();
    let _ = inst.ctl(&["metrics", "reset"]);
    crate::stop::sleep(IDLE)?;
    let metrics = inst.ctl(&["metrics"]).stdout;
    let pct = inst.ctl(&["metrics", "percentiles"]).stdout;
    let idle = format!(
        "idle {} s: {} {}",
        IDLE.as_secs(),
        picked(&metrics, &IDLE_KEYS),
        picked(&pct, &TYPING_KEYS[4..])
    );
    Ok((typing, idle))
}

/// Nearest-rank p50, p95, p99 and the max of `us`, in microseconds.
fn spread(us: &mut [u128]) -> String {
    us.sort_unstable();
    let at = |q: usize| {
        us[(us.len() * q)
            .div_ceil(100)
            .saturating_sub(1)
            .min(us.len() - 1)]
    };
    format!(
        "p50={} p95={} p99={} max={} n={}",
        at(50),
        at(95),
        at(99),
        us[us.len() - 1],
        us.len()
    )
}

/// Grow each roster row's series under `dir` to a full one: [`account_series::KEEP_ROWS`] figure
/// rows over the eight days a series keeps, before the rows the seed wrote, each in its own week.
fn full_series(
    dir: &Path,
    roster: &accounts::Roster,
    state: &accounts::State,
    now: i64,
) -> Result<(), Fail> {
    let keep = i64::try_from(account_series::KEEP_ROWS).unwrap_or(i64::MAX);
    for row in &roster.rows {
        let path = account_series::path_of(dir, &row.label, &state.machine)
            .ok_or_else(|| Fail::die(format!("{}: no series path", row.label)))?;
        let reset = state
            .accounts
            .get(&row.label)
            .and_then(|a| a.windows.get(accounts::WEEK_LIMIT))
            .map_or(now + 5 * D, |w| w.reset);
        let (from, to) = (now - account_series::KEEP_S + H, now - 25 * H);
        let mut text = String::new();
        for i in 0..keep {
            let t = from + (to - from) * i / keep;
            let behind = (reset - t).div_euclid(7 * D);
            let window_reset = reset - 7 * D * behind;
            let into = 7 * D - (window_reset - t);
            let line = account_series::SeriesRow {
                t,
                by: state.machine.clone(),
                said: account_series::Said::Fig {
                    five_hour: None,
                    seven_day: Some(account_series::Fig {
                        pct_x10: u32::try_from(into * 1000 / (7 * D)).unwrap_or(0),
                        reset: window_reset,
                        at: t,
                        src: accounts::SOURCE_TAP.into(),
                        epoch: None,
                    }),
                },
            }
            .line()
            .ok_or_else(|| Fail::die("a series row that will not print"))?;
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
        std::fs::write(&path, text)
            .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(())
}

/// The resolver's own cost on scene A's seed (four accounts), each series grown to a full one
/// ([`full_series`]), as its memo spends it, `n` times each: the store and logins read; the
/// series read whole (what the memo did before it kept a [`account_series::Tail`]) and read as
/// the memo now reads them, a tail's refresh and the rows handed to the replay; and the replay
/// on this machine's own clock (each instant's offset,
/// [`outlook::Offsets::around`](aterm_agent::harness::outlook::Offsets::around)).
fn resolver_cost(n: usize) -> Result<String, Fail> {
    use aterm_agent::harness::outlook;
    let world = World::new(crate::instance::CONFIG_OFF)?;
    let now = crate::util::now_secs();
    seed(&world, &plan('A'), now).map_err(Fail::die)?;
    let roster = accounts::Roster::load(&world.cfg.join("aterm").join("accounts.toml"))
        .map_err(Fail::die)?;
    let dir = harness_state(&world);
    let store = accounts::Store::at(&dir);
    let (state, _) = store.load(now).map_err(Fail::die)?;
    full_series(&dir, &roster, &state, now)?;
    let mut tails: Vec<(String, account_series::Tail)> = roster
        .rows
        .iter()
        .filter_map(|r| {
            let path = account_series::path_of(&dir, &r.label, &state.machine)?;
            let mut tail = account_series::Tail::new(path);
            tail.refresh();
            Some((r.label.clone(), tail))
        })
        .collect();
    let rows_held: usize = tails.iter().map(|(_, t)| t.rows().len()).sum();
    let (mut read, mut whole, mut tailed, mut replay) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let (state, _) = store.load(now).map_err(Fail::die)?;
        let enrolled = accounts::enrolled(&roster);
        let renewals = store
            .load_logins(now)
            .map(|(logins, _)| outlook::renewals_of(&roster, &logins))
            .unwrap_or_default();
        read.push(t.elapsed().as_micros());
        let t = Instant::now();
        let all = outlook::series_of(&dir, &roster, &state.machine);
        whole.push(t.elapsed().as_micros());
        let t = Instant::now();
        let series: std::collections::BTreeMap<String, Vec<account_series::SeriesRow>> = tails
            .iter_mut()
            .filter_map(|(label, tail)| {
                tail.refresh();
                (!tail.rows().is_empty()).then(|| (label.clone(), tail.rows().to_vec()))
            })
            .collect();
        tailed.push(t.elapsed().as_micros());
        if series != all {
            return Err(Fail::die("the tails' rows are not the whole read's"));
        }
        let t = Instant::now();
        let o = outlook::outlook(&outlook::Inputs {
            roster: &roster,
            state: &state,
            enrolled: &enrolled,
            series: &series,
            renewals: &renewals,
            mode: accounts::Mode::Early,
            credits: false,
            now,
            offset: |t| aterm_agent::supervise::limit::local_offset_at(t).unwrap_or(0),
        });
        replay.push(t.elapsed().as_micros());
        out = Some(o.verdict);
    }
    Ok(format!(
        "resolver cost, scene A ({} rows, {rows_held} series rows, verdict {:?}): store read us \
         {}; series whole us {}; series tail us {}; replay us {}",
        roster.rows.len(),
        out,
        spread(&mut read),
        spread(&mut whole),
        spread(&mut tailed),
        spread(&mut replay)
    ))
}

/// `outlook-perf`: see [`PERF_HELP`].
/// # Errors
/// Usage, or an instance that would not start.
pub fn cmd_outlook_perf(args: &[String]) -> Res {
    crate::stop::install();
    let o = perf_opts(args)?;
    let branch = crate::instance::resolve_aterm(o.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    std::fs::create_dir_all(&o.out)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", o.out.display())))?;
    let mut lines = vec![format!(
        "# outlook-perf: main {} vs branch {}, {} pairs; {}",
        o.main.display(),
        branch.display(),
        o.pairs,
        PERF_HELP.lines().nth(2).unwrap_or_default()
    )];
    let say = |lines: &mut Vec<String>, l: String| {
        println!("outlook-perf: {l}");
        lines.push(l);
    };
    for pair in 1..=o.pairs {
        let load = crate::aos_workers_bench::load_average()
            .map_or_else(|e| format!("load=? ({e})"), |l| format!("load1={l:.2}"));
        let gate = std::sync::Barrier::new(2);
        let (main, mine) = std::thread::scope(|s| {
            let main = s.spawn(|| arm(&o.main, &gate));
            let mine = s.spawn(|| arm(&branch, &gate));
            (
                main.join()
                    .unwrap_or_else(|_| Err(Fail::die("the main arm panicked"))),
                mine.join()
                    .unwrap_or_else(|_| Err(Fail::die("the branch arm panicked"))),
            )
        });
        for (name, took) in [("main", main), ("branch", mine)] {
            match took {
                Ok((typing, idle)) => {
                    say(&mut lines, format!("pair {pair} {name} {load} {typing}"));
                    say(&mut lines, format!("pair {pair} {name} {load} {idle}"));
                }
                Err(f) => say(
                    &mut lines,
                    format!(
                        "pair {pair} {name} NOT TAKEN: {}",
                        f.msg.unwrap_or_default()
                    ),
                ),
            }
        }
    }
    let cost = resolver_cost(100)?;
    say(&mut lines, cost);
    let path = o.out.join("perf.txt");
    std::fs::write(&path, format!("{}\n", lines.join("\n")))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
    println!("outlook-perf: {}", path.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scene seeds `me` first and as this Mac's own login, writes only inside its world,
    /// and C's `pool1` is never read; B's and C's `alt1` stand at 92%. Control: A reads `pool1`.
    #[test]
    fn the_seeds_are_synthetic_and_stay_in_their_world() {
        for scene in SCENES {
            let world = World::new(crate::instance::CONFIG_OFF).expect("a world");
            let now = 1_792_956_600;
            let plan = plan(scene);
            assert_eq!(plan.rows.first().map(|r| r.0), Some("me"), "{scene}");
            seed(&world, &plan, now).expect("seeded");
            let profile = std::fs::read_to_string(world.home.join(".claude.json")).expect("read");
            assert!(profile.contains(ME_EMAIL), "{scene}: {profile}");
            let roster = std::fs::read_to_string(world.cfg.join("aterm").join("accounts.toml"))
                .expect("read");
            assert!(
                roster.contains(ME_EMAIL) && roster.matches('@').count() == 1,
                "{scene}: only me names an address: {roster}"
            );
            let (state, _) = accounts::Store::at(&harness_state(&world))
                .load(now)
                .expect("the store loads");
            assert_eq!(state.active.as_deref(), Some("me"));
            let read = |label: &str| state.accounts.contains_key(label);
            match scene {
                'C' => assert!(!read("pool1"), "C: pool1 is never read"),
                'A' => assert!(read("pool1"), "CONTROL: A reads pool1"),
                'H' => assert!(!read("me"), "H: me has no figure"),
                _ => {}
            }
            if matches!(scene, 'B' | 'C') {
                let alt1 = &state.accounts["alt1"];
                let week = alt1.windows.get(accounts::WEEK_LIMIT).expect("a week");
                assert_eq!(week.pct_x10, 920, "{scene}");
            }
        }
    }
}
