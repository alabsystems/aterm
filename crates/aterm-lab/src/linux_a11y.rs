// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux accessibility runtime checks (`linux proof` and `linux a11y`).

use std::path::Path;
use std::time::{Duration, Instant};

use crate::linux::{self, Up};
use crate::linux_baseline::{self as baseline, APP};
use crate::linux_guest::{self as guest, Arch, Display};
use crate::linux_proof::{self as proof, Gui, Row, Verdict};
use crate::stop::Stopped;

/// The packages whose tests the first row runs, every test included.
pub const TESTED: [&str; 3] = ["aterm-dbus", "aterm-update-core", "aterm-a11y"];

/// The acceptance bench the second row runs.
pub const LIVE_ATSPI: [&str; 3] = ["aterm-a11y", "live_atspi", "a11y-atspi"];

/// What an acceptance bench prints for each row and once at its end.
pub const MEASURE_ROW: &str = "measure row: ";
pub const MEASURE_RESULT: &str = "measure result: ";

/// The publisher's lines in aterm.log (aterm-a11y's `atspi::publisher`).
pub const JOINED: &str = "accessibility: publishing on the AT-SPI bus as";
pub const FAILED: &str = "accessibility: the AT-SPI bus failed";
pub const STOPPED: &str = "accessibility: the AT-SPI bus stopped reading";

/// What is typed before the bus is ended, and its output, which the typed text never contains.
pub const BEFORE: &str = "echo A11Y-$((6*7))";
pub const BEFORE_ECHO: &str = "A11Y-42";

/// What is typed once the publisher rejoined, and its output.
pub const AFTER: &str = "echo REJOINED-$((6*7))";
pub const AFTER_ECHO: &str = "REJOINED-42";

/// The file under the scratch HOME whose appearance ends the flood.
pub const FLOOD_STOP: &str = "flood-stop";

/// The flood typed while the bus is frozen ([`FLOOD_STOP`], [`FLOOD_ECHO`]).
pub const FLOOD: &str = "i=0; while [ ! -e ~/flood-stop ]; do i=$((i+1)); echo flood-$i; sleep 0.01; \
                         done; echo FLOOD-$((6*7))";
pub const FLOOD_ECHO: &str = "FLOOD-42";

/// What is typed with Orca listening, as X key events a person's keyboard makes, and its output.
pub const ORCA_TYPED: &str = "echo ORCA-$((6*7))";
pub const ORCA_ECHO: &str = "ORCA-42";

/// Speech-dispatcher configuration for the Orca row ([`QUEUED`]).
pub const SPEECHD_CONF: &str = "\
AddModule \"dummy\" \"sd_dummy\" \"\"\n\
DefaultModule dummy\n\
LogLevel 5\n";

/// What speech-dispatcher's log writes before each message a client asked it to speak.
pub const QUEUED: &str = "Queueing message |";

/// The `portals.conf` the lane's xdg-desktop-portal reads.
pub const PORTALS_CONF: &str = "[preferred]\ndefault=gtk\n";

/// The scratch `XDG_CONFIG_HOME` the lane's portal reads [`PORTALS_CONF`] from.
pub(crate) const PORTAL_CONFIG: &str = "/tmp/aterm-lane-portal-config";

const HANG_SECS: u32 = 60;

/// How a row ends the accessibility bus under aterm-gui.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ordeal {
    /// Its `dbus-daemon` SIGKILLed, as a crash ends it.
    Killed,
    /// Its `dbus-daemon` stopped (SIGSTOP) while the screen changes, then thawed.
    Frozen,
}

impl Ordeal {
    pub const ALL: [Ordeal; 2] = [Ordeal::Killed, Ordeal::Frozen];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Ordeal::Killed => "the AT-SPI bus killed under aterm-gui",
            Ordeal::Frozen => "the AT-SPI bus frozen under aterm-gui",
        }
    }

    /// For a scratch directory's name.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Ordeal::Killed => "killed",
            Ordeal::Frozen => "frozen",
        }
    }

    /// The publisher's line that says it lost the bus this way.
    #[must_use]
    pub const fn said(self) -> &'static str {
        match self {
            Ordeal::Killed => FAILED,
            Ordeal::Frozen => STOPPED,
        }
    }
}

/// What a bus row found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// `lane-bus atspi-walk` before the bus was ended, and after the rejoin.
    pub before: String,
    pub after: String,
    /// aterm-gui's aterm.log and its stderr, whole.
    pub log: String,
    pub stderr: String,
    /// aterm-gui's unit still active once the walk after had run.
    pub alive: bool,
}

/// The frame's name, when `walk` reads aterm-gui's application, its frame, and a terminal
/// whose text holds `echo`.
/// # Errors
/// Which of the three the walk did not read.
pub fn read_back(walk: &str, echo: &str) -> Result<String, String> {
    let node = |role: &str| {
        walk.lines()
            .filter(|l| l.starts_with("node "))
            .find(|l| proof::field(l, "role") == Some(role))
    };
    let app = node("application")
        .and_then(|l| baseline::quoted_field(l, "name"))
        .filter(|n| n == APP)
        .ok_or_else(|| format!("no application named {APP}"))?;
    let frame = node("frame")
        .and_then(|l| baseline::quoted_field(l, "name"))
        .ok_or_else(|| format!("{app} has no frame"))?;
    let text = walk
        .lines()
        .filter(|l| l.starts_with("node ") && proof::field(l, "role") == Some("terminal"))
        .find_map(|l| baseline::quoted_field(l, "text"))
        .ok_or_else(|| format!("{app}'s frame {frame:?} has no terminal with text"))?;
    if text.contains(echo) {
        Ok(frame)
    } else {
        Err(format!(
            "the terminal's text does not hold {echo}: {}",
            crate::util::one_line(&text, 160)
        ))
    }
}

/// The publisher's join, its loss as `said` names it, and its rejoin, in that order in `log`.
/// # Errors
/// The first of the three missing.
pub fn rejoined<'a>(log: &'a str, said: &str) -> Result<[&'a str; 3], String> {
    let lines: Vec<&str> = log.lines().collect();
    let after = |from: usize, text: &str| {
        lines
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, l)| l.contains(text))
            .map(|(i, l)| (i, l.trim()))
    };
    let (i, joined) = after(0, JOINED).ok_or("aterm.log never says the publisher joined")?;
    let (j, lost) = after(i + 1, said)
        .ok_or_else(|| format!("aterm.log never says {said:?} after the publisher joined"))?;
    let (_, again) = after(j + 1, JOINED)
        .ok_or_else(|| format!("aterm.log says {said:?} and never that the publisher rejoined"))?;
    Ok([joined, lost, again])
}

/// The row's verdict on `e`.
/// # Errors
/// Walk, log order, or aterm-gui check failed.
pub fn judge(e: &Evidence, ordeal: Ordeal) -> Result<String, String> {
    let frame = read_back(&e.before, BEFORE_ECHO).map_err(|why| format!("before: {why}"))?;
    if let Some(line) = e.stderr.lines().find(|l| l.contains("panicked")) {
        return Err(format!(
            "aterm-gui panicked: {}",
            crate::util::one_line(line, 300)
        ));
    }
    if !e.alive {
        return Err("aterm-gui was not running after the rejoin".into());
    }
    let [_, lost, _] = rejoined(&e.log, ordeal.said())?;
    read_back(&e.after, AFTER_ECHO).map_err(|why| format!("after the rejoin: {why}"))?;
    let lost = lost.find("accessibility:").map_or(lost, |i| &lost[i..]);
    Ok(format!(
        "said \"{}\"; rejoined, and the walk read {APP}, its frame {frame:?} and {AFTER_ECHO} \
         typed after; no panic",
        crate::util::one_line(lost, 160)
    ))
}

/// The messages speech-dispatcher queued, as its log (`log`) names them.
#[must_use]
pub fn spoken(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = log;
    while let Some(i) = rest.find(QUEUED) {
        rest = &rest[i + QUEUED.len()..];
        let Some(end) = rest.find("| with priority") else {
            break;
        };
        let mut text = String::new();
        let mut in_tag = false;
        for c in rest[..end].chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => text.push(c),
                _ => {}
            }
        }
        out.push(
            text.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&"),
        );
        rest = &rest[end..];
    }
    out
}

struct Spoke<'a> {
    named: &'a str,
    text: &'a str,
    output: Option<&'a str>,
}

fn never(speech: &[String], what: &str) -> String {
    format!(
        "Orca never spoke {what}; it spoke {} strings: {}",
        speech.len(),
        crate::util::one_line(&speech.join(" | "), 300)
    )
}

fn spoke<'a>(speech: &'a [String], frame: &str) -> Result<Spoke<'a>, String> {
    let named = speech
        .iter()
        .find(|s| *s == frame || s.starts_with(&format!("{frame} ")))
        .ok_or_else(|| never(speech, &format!("the frame's name {frame:?}")))?;
    let typed_at = speech
        .iter()
        .position(|s| TYPED_WORDS.iter().any(|w| s.contains(w)))
        .unwrap_or(speech.len());
    let text = speech[..typed_at]
        .iter()
        .find(|s| s.contains(proof::PROMPT))
        .ok_or_else(|| {
            never(
                speech,
                &format!(
                    "the terminal's text ({}) before the typed line",
                    proof::PROMPT
                ),
            )
        })?;
    let output = speech.iter().find(|s| s.contains(ORCA_ECHO));
    Ok(Spoke {
        named,
        text,
        output: output.map(String::as_str),
    })
}

/// Orca's row verdict: it spoke the frame's name, the terminal's text before the typed line
/// ([`spoke`]), and the typed line's output.
/// # Errors
/// The first of the three Orca never spoke.
pub fn heard(speech: &[String], frame: &str) -> Result<String, String> {
    let s = spoke(speech, frame)?;
    let output = s
        .output
        .ok_or_else(|| never(speech, &format!("the typed line's output {ORCA_ECHO}")))?;
    Ok(format!(
        "Orca spoke the frame {}, the terminal {} and the output {} ({} strings)",
        crate::util::one_line(s.named, 60),
        crate::util::one_line(s.text, 60),
        crate::util::one_line(output, 60),
        speech.len()
    ))
}

/// Keyless verdict ([`OrcaScreen::MutterWayland`], [`heard`], [`KEY_HEARD`], [`NO_CAUSE`]).
/// # Errors
/// The frame or the text never spoken, or the output unspoken by an Orca that heard keys or never
/// said why.
pub fn heard_keyless(speech: &[String], frame: &str, debug: &str) -> Result<String, String> {
    let s = spoke(speech, frame)?;
    if s.output.is_some() {
        return heard(speech, frame);
    }
    let unspoken = never(speech, &format!("the typed line's output {ORCA_ECHO}"));
    let keys = debug.lines().filter(|l| l.contains(KEY_HEARD)).count();
    if keys > 0 {
        return Err(format!(
            "{unspoken}, though it heard {keys} keys ({KEY_HEARD:?} in its debug log)"
        ));
    }
    if !debug.contains(NO_CAUSE) {
        return Err(format!(
            "{unspoken}, and its debug log never says {NO_CAUSE:?}"
        ));
    }
    Ok(format!(
        "Orca spoke the frame {} and the terminal {}; it heard none of the typed line's keys \
         (its debug log presents no key) and left the output {ORCA_ECHO} unspoken for want of \
         a cause ({} strings)",
        crate::util::one_line(s.named, 60),
        crate::util::one_line(s.text, 60),
        speech.len()
    ))
}

/// Words of [`ORCA_TYPED`] and [`ORCA_ECHO`] absent before typing.
pub const TYPED_WORDS: [&str; 2] = ["echo", "ORCA-"];

/// What one arch's rows run with.
pub(crate) struct Cell<'a> {
    pub(crate) up: &'a Up,
    pub(crate) arch: Arch,
    /// `aarch64 (native)`: every row's name starts with it.
    pub(crate) tag: String,
    /// The checkout, in the guest.
    pub(crate) repo: String,
    /// The arch's cargo environment.
    pub(crate) env: Vec<(String, String)>,
    /// Where the arch's aterm-gui, aterm-ctl and lane-bus are.
    pub(crate) bin: String,
    pub(crate) results: &'a Path,
}

impl Cell<'_> {
    /// `cargo <verb> --locked` on the checkout for this arch, with `env`.
    pub(crate) fn cargo(
        &self,
        env: &[(String, String)],
        verb: &str,
        extra: &[&str],
    ) -> Vec<String> {
        let mut argv = vec![
            "cargo".to_string(),
            verb.into(),
            "--locked".into(),
            "--manifest-path".into(),
            format!("{}/Cargo.toml", self.repo),
        ];
        argv.extend(self.arch.target_args());
        argv.extend(extra.iter().map(|s| (*s).to_string()));
        guest::with_env(env, &argv)
    }

    fn session_bus(&self) -> (String, String) {
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={}/bus", self.up.runtime),
        )
    }

    fn test_row(&self, title: &str, name: &str, args: &[&str]) -> Row {
        println!("\n=== {}: {title} ===", self.tag);
        let mut env = self.env.clone();
        env.push(self.session_bus());
        let out = proof::test_run(self.up, |a| self.cargo(&env, "test", a), args);
        proof::translator_not_runs(
            proof::no_skips(
                proof::test_verdict(&format!("{} {name}", self.tag), &out),
                &out.text,
            ),
            &out.text,
            self.arch,
        )
    }

    /// An acceptance bench run as a row ([`measured`]).
    fn bench_row(&self, title: &str, name: &str, [package, bench, target]: [&str; 3]) -> Row {
        println!("\n=== {}: {title} ===", self.tag);
        let mut env = self.env.clone();
        env.push(self.session_bus());
        let out = proof::test_run(
            self.up,
            |a| self.cargo(&env, "bench", a),
            &["-p", package, "--bench", bench, "--profile", "test"],
        );
        let (verdict, detail) = measured(target, &out.text, out.ok());
        Row {
            name: format!("{} {name}", self.tag),
            verdict,
            secs: out.secs,
            detail: if verdict == Verdict::Pass || out.text.contains(MEASURE_RESULT) {
                detail
            } else {
                format!("{detail}\n{}", out.tail(5))
            },
        }
    }
}

/// The verdict of an acceptance bench whose rows name `target`.
#[must_use]
pub fn measured(target: &str, text: &str, exited_ok: bool) -> (Verdict, String) {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let row = format!("{MEASURE_ROW}{target}::");
    let rows: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with(&row))
        .map(|(i, l)| match lines.get(i + 1) {
            Some(next) if !next.starts_with(MEASURE_ROW) => format!("    {l}: {next}"),
            _ => format!("    {l}"),
        })
        .collect();
    let rows = rows.join("\n");
    let summary = format!("{MEASURE_RESULT}{target} ");
    let summaries: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.strip_prefix(&summary))
        .collect();
    let [counts] = summaries.as_slice() else {
        return (
            Verdict::Fail,
            format!(
                "{} summaries of {target}, not one (exit 0: {exited_ok})\n{rows}",
                summaries.len()
            ),
        );
    };
    let count = |key: &str| {
        counts.split_whitespace().find_map(|f| {
            f.strip_prefix(key)?
                .strip_prefix('=')?
                .parse::<usize>()
                .ok()
        })
    };
    let (Some(planned), Some(passed), Some(failed), Some(refused)) = (
        count("planned"),
        count("passed"),
        count("failed"),
        count("refused"),
    ) else {
        return (
            Verdict::Fail,
            format!("a malformed summary: {counts}\n{rows}"),
        );
    };
    let complete = planned > 0
        && passed
            .checked_add(failed)
            .and_then(|n| n.checked_add(refused))
            == Some(planned);
    let detail = format!("{counts}\n{rows}");
    if failed > 0 || !complete {
        (Verdict::Fail, detail)
    } else if refused > 0 {
        (Verdict::NotRun, detail)
    } else if exited_ok {
        (Verdict::Pass, detail)
    } else {
        (
            Verdict::Fail,
            format!("every row passed but it exited non-zero: {detail}"),
        )
    }
}

/// The tests' rows of one arch, pushed to `rows` as each ends.
/// # Errors
/// The person stopped the run.
pub(crate) fn test_rows(cell: &Cell<'_>, rows: &mut Vec<Row>) -> Result<(), Stopped> {
    let rosetta = (cell.arch == Arch::X86_64).then_some(proof::ROSETTA_ONE_AT_A_TIME);
    let mut args = vec!["--no-fail-fast", "--tests"];
    for p in TESTED {
        args.extend(["-p", p]);
    }
    args.extend(["--", "--include-ignored", "--nocapture"]);
    args.extend(rosetta);
    rows.push(cell.test_row(
        "aterm-dbus, aterm-update-core, aterm-a11y tests",
        "tests: dbus update-core a11y",
        &args,
    ));
    crate::stop::check()?;
    rows.push(cell.bench_row(
        "aterm-a11y's live_atspi on at-spi2-core (a walk, the bus killed, the bus frozen)",
        "bench: live_atspi on at-spi2-core",
        LIVE_ATSPI,
    ));
    crate::stop::check()?;
    let mut args = vec![
        "--no-fail-fast",
        "-p",
        "aterm-gui",
        "--lib",
        "--",
        "a11y",
        "--nocapture",
    ];
    args.extend(rosetta);
    rows.push(cell.test_row(
        "aterm-gui's accessibility suites",
        "tests: aterm-gui a11y",
        &args,
    ));
    crate::stop::check()?;
    let mut args = vec![
        "--no-fail-fast",
        "-p",
        "aterm-gui",
        "--lib",
        "--",
        "portal_settings",
        "--include-ignored",
        "--nocapture",
    ];
    args.extend(rosetta);
    let name = "tests: aterm-gui portal_settings, live on xdg-desktop-portal";
    let started = Instant::now();
    match portal_up(cell.up) {
        Ok(()) => rows.push(cell.test_row(
            "aterm-gui's motion read from the Settings portal (xdg-desktop-portal)",
            name,
            &args,
        )),
        Err(why) => rows.push(Row {
            name: format!("{} {name}", cell.tag),
            verdict: Verdict::NotRun,
            secs: started.elapsed().as_secs_f64(),
            detail: format!("the Settings portal did not come up: {why}"),
        }),
    }
    cell.up.stop_units();
    let _ = cell
        .up
        .sh(&["rm".into(), "-rf".into(), PORTAL_CONFIG.into()]);
    crate::stop::check()
}

/// The desktop's Settings portal, up for the portal row ([`PORTALS_CONF`]).
pub(crate) fn portal_up(up: &Up) -> Result<(), String> {
    linux::start_display(up, Display::Xvfb)?;
    let dir = format!("{PORTAL_CONFIG}/xdg-desktop-portal");
    let _ = up.sh(&["install".into(), "-d".into(), dir.clone()]);
    let conf = up.guest.shell(
        &up.home,
        &["tee".into(), format!("{dir}/portals.conf")],
        Some(PORTALS_CONF),
    );
    if !conf.ok() {
        return Err(format!("portals.conf: {}", conf.tail(2)));
    }
    let _ = up.sh(&[
        "systemctl".into(),
        "--user".into(),
        "stop".into(),
        "xdg-desktop-portal.service".into(),
        "xdg-desktop-portal-gtk.service".into(),
    ]);
    let bus = (
        "DBUS_SESSION_BUS_ADDRESS".to_string(),
        format!("unix:path={}/bus", up.runtime),
    );
    let units = [
        (
            "portal-gtk",
            vec![Display::Xvfb.client_env(&up.runtime), bus.clone()],
            vec!["/usr/libexec/xdg-desktop-portal-gtk".to_string()],
            "org.freedesktop.impl.portal.desktop.gtk",
        ),
        (
            "portal",
            vec![("XDG_CONFIG_HOME".into(), PORTAL_CONFIG.into()), bus],
            vec![
                "/usr/libexec/xdg-desktop-portal".to_string(),
                "--replace".into(),
            ],
            "org.freedesktop.portal.Desktop",
        ),
    ];
    for (name, env, argv, owns) in units {
        let unit = guest::Unit {
            name: format!("{}{name}", guest::UNIT_PREFIX),
            env,
            argv,
            socket: String::new(),
        };
        let started = up.sh(&guest::run_unit(&unit, None));
        if !started.ok() {
            return Err(format!("{}: {}", unit.name, started.tail(2)));
        }
        if !up.sh(&guest::wait_name(owns, HANG_SECS)).ok() {
            return Err(format!("{owns} never owned ({HANG_SECS} s)"));
        }
    }
    Ok(())
}

/// One of aterm-gui's rows: an ordeal of the bus under Xvfb, or Orca on one
/// of its screens.
#[derive(Clone, Copy)]
enum GuiRow {
    Ordeal(Ordeal),
    Orca(OrcaScreen),
}

/// aterm-gui's rows of one arch ([`OrcaScreen`]).
/// # Errors
/// The person stopped the run.
pub(crate) fn gui_rows(cell: &Cell<'_>, built: bool, rows: &mut Vec<Row>) -> Result<(), Stopped> {
    let all = Ordeal::ALL
        .map(GuiRow::Ordeal)
        .into_iter()
        .chain(OrcaScreen::ALL.map(GuiRow::Orca));
    for row in all {
        let (title, screen, slug) = match row {
            GuiRow::Ordeal(o) => (o.name(), "xvfb (X11)", o.slug()),
            GuiRow::Orca(s) => ("Orca reads aterm-gui", s.title(), s.slug()),
        };
        let name = format!("{} {title}: {screen}", cell.tag);
        if !built {
            rows.push(Row {
                name,
                verdict: Verdict::NotRun,
                secs: 0.0,
                detail: "no aterm-gui: its build failed".into(),
            });
            continue;
        }
        println!("\n=== {name} ===");
        let started = Instant::now();
        let up = match row {
            GuiRow::Orca(OrcaScreen::MutterX11 | OrcaScreen::MutterWayland) => {
                mutter_up(cell.up).map(Some)
            }
            _ => linux::start_display(cell.up, Display::Xvfb).map(|_| None),
        };
        let (verdict, detail) = match up {
            Err(why) => (Verdict::NotRun, why),
            Ok(mutter) => {
                let _ = baseline::accessibility(cell.up, true);
                let gui = Gui {
                    up: cell.up,
                    bin: cell.bin.clone(),
                    dir: format!(
                        "/tmp/{}{}-a11y-{slug}",
                        guest::UNIT_PREFIX,
                        cell.arch.name()
                    ),
                    name: format!("{}-a11y-{slug}", cell.arch.name()),
                    cpu: false,
                    deck: false,
                };
                let result = match row {
                    GuiRow::Ordeal(o) => drive(&gui, cell.arch, o, cell.results),
                    GuiRow::Orca(s) => orca(&gui, cell.arch, s, mutter.as_ref(), cell.results),
                };
                cell.up.stop_units();
                let _ = screen_reader(cell.up, false);
                let _ = baseline::accessibility(cell.up, false);
                result
            }
        };
        cell.up.stop_units();
        println!("  {} {detail}", verdict.label());
        rows.push(Row {
            name,
            verdict,
            secs: started.elapsed().as_secs_f64(),
            detail,
        });
        crate::stop::check()?;
    }
    Ok(())
}

fn screen_reader(up: &Up, on: bool) -> linux::Out {
    up.sh(&[
        "busctl".into(),
        "--user".into(),
        "set-property".into(),
        "org.a11y.Bus".into(),
        "/org/a11y/bus".into(),
        "org.a11y.Status".into(),
        "ScreenReaderEnabled".into(),
        "b".into(),
        on.to_string(),
    ])
}

fn log_path(gui: &Gui<'_>) -> String {
    format!("{}/home/.local/state/aterm/logs/aterm.log", gui.dir)
}

fn run_line(gui: &Gui<'_>, line: &str, echo: &str) -> Result<(), String> {
    gui.expect(&["send", line], &|r| r.starts_with("OK"))?;
    gui.expect(&["key", "enter"], &|r| r.starts_with("OK"))?;
    gui.expect(&["await", "match", echo, "timeout=60000"], &|r| {
        r.starts_with("OK match")
    })
    .map(|_| ())
}

fn wait_lines(gui: &Gui<'_>, path: &str, text: &str, n: usize, what: &str) -> Result<(), String> {
    if gui.up.sh(&guest::wait_lines(path, text, n, HANG_SECS)).ok() {
        Ok(())
    } else {
        Err(format!("{what} ({HANG_SECS} s)"))
    }
}

/// `lane-bus atspi-walk` of aterm-gui ([`read_back`]).
fn walk_until(gui: &Gui<'_>, pid: &str, echo: &str) -> String {
    let argv = vec![
        "env".to_string(),
        format!("DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus", gui.up.runtime),
        format!("{}/lane-bus", gui.bin),
        "atspi-walk".into(),
        APP.into(),
        pid.into(),
    ];
    let started = Instant::now();
    loop {
        let out = gui.up.sh(&argv);
        let walk = if out.ok() {
            out.text
        } else {
            format!("walk failed (exit {:?}): {}", out.code, out.tail(2))
        };
        if read_back(&walk, echo).is_ok() || started.elapsed() > Duration::from_secs(60) {
            return walk;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn bus_daemon(up: &Up) -> Result<String, String> {
    let uid = up.runtime.rsplit('/').next().unwrap_or_default();
    let out = up.sh(&[
        "pgrep".into(),
        "-u".into(),
        uid.into(),
        "-f".into(),
        "dbus-daemon.*accessibility.conf".into(),
    ]);
    let pids: Vec<&str> = out.text.split_whitespace().collect();
    match pids.as_slice() {
        [pid] => Ok((*pid).to_string()),
        [] => Err(format!(
            "no accessibility bus dbus-daemon is running: {}",
            out.tail(2)
        )),
        many => Err(format!(
            "{} accessibility bus dbus-daemons are running: {}",
            many.len(),
            many.join(" ")
        )),
    }
}

fn signal(up: &Up, signal: &str, pid: &str) -> Result<(), String> {
    let out = up.sh(&["kill".into(), format!("-{signal}"), pid.into()]);
    if out.ok() {
        Ok(())
    } else {
        Err(format!("kill -{signal} {pid}: {}", out.tail(2)))
    }
}

fn drive(gui: &Gui<'_>, arch: Arch, ordeal: Ordeal, results: &Path) -> (Verdict, String) {
    if let Err(e) = gui.prepare() {
        return (Verdict::NotRun, format!("scratch world: {e}"));
    }
    let stderr = format!("{}/gui.stderr", gui.dir);
    if let Err(why) = gui.start_logged(Some(Display::Xvfb), &[], &stderr) {
        return (Verdict::Fail, why);
    }
    let result = ordeal_run(gui, ordeal, &stderr).and_then(|e| judge(&e, ordeal));
    let ended = if result.is_err() {
        unit_end(gui)
    } else {
        String::new()
    };
    let log = gui.up.sh(&["cat".into(), log_path(gui)]);
    gui.stop();
    match result {
        Ok(detail) => (Verdict::Pass, detail),
        Err(why) => {
            let said = gui.up.sh(&["cat".into(), stderr]);
            let publisher: Vec<&str> = log
                .text
                .lines()
                .filter(|l| l.contains("accessibility:"))
                .collect();
            let detail = format!(
                "{why}\n    aterm.log's accessibility lines:\n    {}\n    aterm-gui's unit: {ended}\n    \
                 aterm-gui's stderr ends:\n    {}{}",
                publisher[publisher.len().saturating_sub(12)..].join("\n    "),
                said.tail(8).replace('\n', "\n    "),
                gui.keep_crash_reports(results)
            );
            translated_away(arch, &ended, &said.text, detail)
        }
    }
}

fn unit_end(gui: &Gui<'_>) -> String {
    let out = gui.up.sh(&[
        "systemctl".into(),
        "--user".into(),
        "show".into(),
        "--property=ActiveState,Result,ExecMainCode,ExecMainStatus".into(),
        Gui::unit_name(),
    ]);
    out.text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rosetta's own abort, when it is what ended aterm-gui ([`unit_end`]).
#[must_use]
pub fn translator_abort(arch: Arch, ended: &str, stderr: &str) -> Option<String> {
    let dumped = ended.split_whitespace().any(|f| f == "Result=core-dump");
    if arch != Arch::X86_64 || !dumped {
        return None;
    }
    let lines: Vec<&str> = stderr.lines().map(str::trim).collect();
    lines.windows(2).find_map(|w| {
        let site = w[1].starts_with('(') && w[1].ends_with(')') && w[1].contains(".cpp:");
        (w[0].starts_with("assertion failed [") && site).then(|| format!("{} {}", w[0], w[1]))
    })
}

/// A failed row's verdict: NOT RUN when the translator ended aterm-gui ([`translator_abort`]).
fn translated_away(arch: Arch, ended: &str, stderr: &str, detail: String) -> (Verdict, String) {
    match translator_abort(arch, ended, stderr) {
        Some(abort) => (
            Verdict::NotRun,
            format!(
                "Rosetta aborted aterm-gui ({abort}), so the row decided nothing about aterm\n    {detail}"
            ),
        ),
        None => (Verdict::Fail, detail),
    }
}

fn ordeal_run(gui: &Gui<'_>, ordeal: Ordeal, stderr: &str) -> Result<Evidence, String> {
    if !gui.up.sh(&guest::wait_socket(&gui.sock(), 60)).ok() {
        return Err("the control socket never listened (60 s)".into());
    }
    gui.expect(&["ready", "60000"], &|r| r.starts_with("OK ready"))?;
    let log = log_path(gui);
    wait_lines(
        gui,
        &log,
        JOINED,
        1,
        "the publisher never joined the AT-SPI bus",
    )?;
    run_line(gui, BEFORE, BEFORE_ECHO)?;
    let pid =
        proof::unit_pid(gui.up, &Gui::unit_name()).ok_or("aterm-gui's unit has no main pid")?;
    let before = walk_until(gui, &pid, BEFORE_ECHO);
    read_back(&before, BEFORE_ECHO).map_err(|why| format!("before: {why}"))?;
    let daemon = bus_daemon(gui.up)?;
    match ordeal {
        Ordeal::Killed => signal(gui.up, "KILL", &daemon)?,
        Ordeal::Frozen => {
            signal(gui.up, "STOP", &daemon)?;
            let frozen = frozen(gui, &log);
            let thawed = signal(gui.up, "CONT", &daemon);
            frozen?;
            thawed?;
        }
    }
    wait_lines(
        gui,
        &log,
        JOINED,
        2,
        &format!("the publisher never rejoined after {}", ordeal.slug()),
    )?;
    run_line(gui, AFTER, AFTER_ECHO)?;
    let after = walk_until(gui, &pid, AFTER_ECHO);
    let read = |path: &str| gui.up.sh(&["cat".into(), path.into()]).text;
    let alive = gui
        .up
        .sh(&[
            "systemctl".into(),
            "--user".into(),
            "is-active".into(),
            "--quiet".into(),
            Gui::unit_name(),
        ])
        .ok();
    Ok(Evidence {
        before,
        after,
        log: read(&log),
        stderr: read(stderr),
        alive,
    })
}

fn frozen(gui: &Gui<'_>, log: &str) -> Result<(), String> {
    gui.expect(&["send", FLOOD], &|r| r.starts_with("OK"))?;
    gui.expect(&["key", "enter"], &|r| r.starts_with("OK"))?;
    let said = wait_lines(
        gui,
        log,
        STOPPED,
        1,
        "the publisher never said the frozen bus stopped reading",
    );
    let _ = gui
        .up
        .sh(&["touch".into(), format!("{}/home/{FLOOD_STOP}", gui.dir)]);
    said?;
    gui.expect(&["await", "match", FLOOD_ECHO, "timeout=60000"], &|r| {
        r.starts_with("OK match")
    })
    .map_err(|why| format!("aterm-gui stopped drawing with the bus frozen: {why}"))?;
    Ok(())
}

/// Where an Orca row runs aterm-gui and Orca.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrcaScreen {
    /// Xvfb, which runs no window manager: the row gives aterm-gui's window the X keyboard focus itself (`xdotool windowfocus`), as a person's click would, and types on the X keyboard.
    Xvfb,
    /// Headless Mutter, GNOME's compositor, with its own Xwayland ([`mutter_up`]).
    MutterX11,
    /// Headless Mutter with aterm-gui as a Wayland client, typed into from a RemoteDesktop seat (`lane-bus seat`), whose first key gives the window the keyboard's focus.
    MutterWayland,
}

impl OrcaScreen {
    pub const ALL: [OrcaScreen; 3] = [
        OrcaScreen::Xvfb,
        OrcaScreen::MutterX11,
        OrcaScreen::MutterWayland,
    ];

    /// The screen, as a row's name ends.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            OrcaScreen::Xvfb => "xvfb (X11)",
            OrcaScreen::MutterX11 => "mutter, aterm-gui on its Xwayland (X11)",
            OrcaScreen::MutterWayland => "mutter (Wayland)",
        }
    }

    /// The row's scratch directory and kept files are named with it.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            OrcaScreen::Xvfb => "orca",
            OrcaScreen::MutterX11 => "orca-mutter-x11",
            OrcaScreen::MutterWayland => "orca-mutter-wayland",
        }
    }
}

/// A variable a client is started with: its name and value.
pub type Var = (String, String);

/// The Orca rows' Mutter ([`mutter_up`]).
pub(crate) struct Mutter {
    pid: String,
    /// `DISPLAY` and `XAUTHORITY`: its Xwayland, and the authority file
    /// Mutter wrote for it.
    x: Vec<Var>,
    /// `WAYLAND_DISPLAY`, as its socket's absolute path.
    wayland: Var,
}

/// The Orca rows' Mutter's unit, and its Wayland socket's name.
const MUTTER_ORCA: &str = "aterm-lane-mutter-orca";

const MUTTER_ENV: &str = "/tmp/aterm-lane-mutter-orca.env";

const MUTTER_COMMAND: &str = "env | grep -E '^(DISPLAY|XAUTHORITY|WAYLAND_DISPLAY)=' \
                              >\"$0.part\" && mv \"$0.part\" \"$0\"; exec sleep infinity";

fn mutter_up(up: &Up) -> Result<Mutter, String> {
    let _ = up.sh(&["rm".into(), "-f".into(), MUTTER_ENV.into()]);
    let unit = guest::Unit {
        name: MUTTER_ORCA.into(),
        env: vec![("XDG_SESSION_TYPE".into(), "wayland".into())],
        argv: [
            "mutter",
            "--headless",
            "--wayland",
            "--virtual-monitor",
            "1280x800",
            "--wayland-display",
            MUTTER_ORCA,
            "--",
            "sh",
            "-c",
            MUTTER_COMMAND,
            MUTTER_ENV,
        ]
        .map(str::to_string)
        .to_vec(),
        socket: String::new(),
    };
    let started = up.sh(&guest::run_unit(&unit, None));
    if !started.ok() {
        return Err(format!("Mutter's unit did not start: {}", started.tail(2)));
    }
    if !up.sh(&guest::wait_file(MUTTER_ENV, HANG_SECS)).ok() {
        let said = up.sh(&[
            "journalctl".into(),
            "--user".into(),
            "--no-pager".into(),
            "-o".into(),
            "cat".into(),
            "-n".into(),
            "5".into(),
            "-u".into(),
            MUTTER_ORCA.into(),
        ]);
        return Err(format!(
            "Mutter never ran its command ({HANG_SECS} s): {}",
            crate::util::one_line(&said.text, 300)
        ));
    }
    let given = up.sh(&["cat".into(), MUTTER_ENV.into()]);
    let (x, wayland) = mutter_env(&given.text, &up.runtime)?;
    let pid = proof::unit_pid(up, MUTTER_ORCA).ok_or("Mutter's unit has no main pid")?;
    Ok(Mutter { pid, x, wayland })
}

/// What Mutter gave its command ([`MUTTER_COMMAND`]'s file).
/// # Errors
/// One of `DISPLAY`, `XAUTHORITY` and `WAYLAND_DISPLAY` missing.
pub fn mutter_env(given: &str, runtime: &str) -> Result<(Vec<Var>, Var), String> {
    let var = |name: &str| {
        given
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("Mutter gave its command no {name}: {given:?}"))
    };
    let display = var("DISPLAY")?;
    let authority = var("XAUTHORITY")?;
    let socket = var("WAYLAND_DISPLAY")?;
    let socket = if socket.starts_with('/') {
        socket
    } else {
        format!("{runtime}/{socket}")
    };
    Ok((
        vec![
            ("DISPLAY".into(), display),
            ("XAUTHORITY".into(), authority),
        ],
        ("WAYLAND_DISPLAY".into(), socket),
    ))
}

/// What Orca's debug log says for each key it heard and presents.
pub const KEY_HEARD: &str = "Presenting keyboard event";

/// What Orca's debug log says when it leaves new text unspoken because no key it heard caused it.
pub const NO_CAUSE: &str = "Not speaking inserted string due to lack of cause";

/// The seat's steps that type `line`, one key at a time.
#[must_use]
pub fn seat_keys(line: &str) -> Vec<String> {
    line.chars()
        .map(|c| match c {
            ' ' => "tap:space".to_string(),
            c => format!("tap:{c}"),
        })
        .collect()
}

/// Orca row with dummy speech-dispatcher ([`heard`], [`heard_keyless`]).
fn orca(
    gui: &Gui<'_>,
    arch: Arch,
    screen: OrcaScreen,
    mutter: Option<&Mutter>,
    results: &Path,
) -> (Verdict, String) {
    if let Err(e) = gui.prepare() {
        return (Verdict::NotRun, format!("scratch world: {e}"));
    }
    let speechd_log = format!("{}/speechd-log/speech-dispatcher.log", gui.dir);
    let debug = format!("{}/orca.debug", gui.dir);
    let result = match (screen, mutter) {
        (OrcaScreen::Xvfb, _) => orca_run(gui, On::Xvfb, &speechd_log, &debug),
        (OrcaScreen::MutterX11, Some(m)) => orca_run(gui, On::MutterX11(m), &speechd_log, &debug),
        (OrcaScreen::MutterWayland, Some(m)) => {
            orca_run(gui, On::MutterWayland(m), &speechd_log, &debug)
        }
        (_, None) => Err("no Mutter for a Mutter row".into()),
    };
    let ended = if result.is_err() {
        unit_end(gui)
    } else {
        String::new()
    };
    let log = gui.up.sh(&["cat".into(), speechd_log.clone()]);
    let _ = gui.up.sh(&guest::signal_unit(
        &format!("{}orca", guest::UNIT_PREFIX),
        "SIGKILL",
    ));
    let said_debug = gui.up.sh(&["cat".into(), debug.clone()]);
    let mut kept = Vec::new();
    for (path, name) in [(&speechd_log, "speechd.log"), (&debug, "orca.debug")] {
        let to = results.join(format!("{}-{name}.txt", gui.name));
        if gui.up.guest.copy_out(path, &to).ok() {
            kept.push(to.display().to_string());
        }
    }
    let kept = if kept.is_empty() {
        String::new()
    } else {
        format!("; kept {}", kept.join(", "))
    };
    gui.stop();
    match result {
        Err(why) => {
            let said = gui
                .up
                .sh(&["cat".into(), format!("{}/gui.stderr", gui.dir)]);
            let detail = format!(
                "{why}{kept}\n    aterm-gui's unit: {ended}\n    aterm-gui's stderr ends:\n    {}",
                said.tail(8).replace('\n', "\n    ")
            );
            translated_away(arch, &ended, &said.text, detail)
        }
        Ok(frame) => {
            let speech = spoken(&log.text);
            let judged = if screen == OrcaScreen::MutterWayland {
                heard_keyless(&speech, &frame, &said_debug.text)
            } else {
                heard(&speech, &frame)
            };
            match judged {
                Ok(detail) => (Verdict::Pass, format!("{detail}{kept}")),
                Err(why) => (Verdict::Fail, format!("{why}{kept}")),
            }
        }
    }
}

/// An Orca row's screen, with the Mutter it runs on.
#[derive(Clone, Copy)]
enum On<'a> {
    Xvfb,
    MutterX11(&'a Mutter),
    MutterWayland(&'a Mutter),
}

impl On<'_> {
    fn x(self, up: &Up) -> Vec<(String, String)> {
        match self {
            On::Xvfb => vec![Display::Xvfb.client_env(&up.runtime)],
            On::MutterX11(m) | On::MutterWayland(m) => m.x.clone(),
        }
    }
}

fn x_client(x: &[(String, String)], argv: &[&str]) -> Vec<String> {
    guest::with_env(
        x,
        &argv.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
    )
}

fn orca_run(gui: &Gui<'_>, on: On<'_>, speechd_log: &str, debug: &str) -> Result<String, String> {
    let up = gui.up;
    let d = &gui.dir;
    let conf = format!("{d}/speechd");
    let socket = format!("{d}/speechd.sock");
    let _ = up.sh(&[
        "install".into(),
        "-d".into(),
        conf.clone(),
        format!("{d}/speechd-log"),
        format!("{d}/seat"),
    ]);
    let made = up.guest.shell(
        &up.home,
        &["tee".into(), format!("{conf}/speechd.conf")],
        Some(SPEECHD_CONF),
    );
    if !made.ok() {
        return Err(format!("speechd.conf: {}", made.tail(2)));
    }
    let speechd = guest::Unit {
        name: format!("{}speechd", guest::UNIT_PREFIX),
        env: Vec::new(),
        argv: [
            "speech-dispatcher",
            "--run-single",
            "--timeout",
            "0",
            "--communication-method",
            "unix_socket",
            "--socket-path",
            &socket,
            "--config-dir",
            &conf,
            "--log-dir",
            &format!("{d}/speechd-log"),
        ]
        .map(str::to_string)
        .to_vec(),
        socket: socket.clone(),
    };
    let started = up.sh(&guest::run_unit(&speechd, None));
    if !started.ok() || !up.sh(&guest::wait_socket(&socket, HANG_SECS)).ok() {
        return Err(format!(
            "speech-dispatcher never listened ({HANG_SECS} s): {}",
            started.tail(2)
        ));
    }
    let _ = screen_reader(up, true);
    let x = on.x(up);
    let mut env = x.clone();
    if let On::MutterWayland(m) = on {
        env.push(m.wayland.clone());
        env.push(("XDG_SESSION_TYPE".into(), "wayland".into()));
    }
    env.extend([
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={}/bus", up.runtime),
        ),
        ("HOME".into(), format!("{d}/home")),
        ("XDG_RUNTIME_DIR".into(), up.runtime.clone()),
        ("SPEECHD_ADDRESS".into(), format!("unix_socket:{socket}")),
    ]);
    let orca = guest::Unit {
        name: format!("{}orca", guest::UNIT_PREFIX),
        env,
        argv: vec![
            "orca".into(),
            "--debug-file".into(),
            debug.into(),
            "--user-prefs".into(),
            format!("{d}/orca-prefs"),
        ],
        socket: String::new(),
    };
    if !up.sh(&guest::run_unit(&orca, None)).ok() {
        return Err("Orca's unit did not start".into());
    }
    wait_lines(
        gui,
        speechd_log,
        QUEUED,
        1,
        "Orca never spoke after starting",
    )?;
    let stderr = format!("{d}/gui.stderr");
    match on {
        On::Xvfb => gui.start_logged(Some(Display::Xvfb), &[], &stderr)?,
        On::MutterX11(m) => gui.start_logged_on(&m.x, &stderr)?,
        On::MutterWayland(m) => gui.start_logged_on(std::slice::from_ref(&m.wayland), &stderr)?,
    };
    if !up.sh(&guest::wait_socket(&gui.sock(), 60)).ok() {
        return Err("the control socket never listened (60 s)".into());
    }
    gui.expect(&["ready", "60000"], &|r| r.starts_with("OK ready"))?;
    wait_lines(
        gui,
        &log_path(gui),
        JOINED,
        1,
        "the publisher never joined the AT-SPI bus",
    )?;
    let pid = proof::unit_pid(up, &Gui::unit_name()).ok_or("aterm-gui's unit has no main pid")?;
    let seat = format!("{d}/seat");
    match on {
        On::Xvfb | On::MutterX11(_) => {
            let found = up.sh(&x_client(
                &x,
                &[
                    "timeout",
                    "60",
                    "xdotool",
                    "search",
                    "--sync",
                    "--onlyvisible",
                    "--pid",
                    &pid,
                ],
            ));
            let window = found
                .text
                .lines()
                .map(str::trim)
                .find(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()))
                .ok_or_else(|| format!("aterm-gui's X window was never mapped: {}", found.tail(2)))?
                .to_string();
            let how = if matches!(on, On::Xvfb) {
                "windowfocus"
            } else {
                "windowactivate"
            };
            let focused = up.sh(&x_client(&x, &["xdotool", how, "--sync", &window]));
            if !focused.ok() {
                return Err(format!("the window refused the focus: {}", focused.tail(2)));
            }
        }
        On::MutterWayland(m) => {
            let mut argv = vec![
                format!("{}/lane-bus", gui.bin),
                "seat".into(),
                m.pid.clone(),
                APP.into(),
                pid.clone(),
                format!("touch:{seat}/ready"),
                format!("wait:{seat}/go"),
            ];
            argv.extend(seat_keys(ORCA_TYPED));
            argv.extend([
                "tap:Return".into(),
                format!("touch:{seat}/done"),
                format!("wait:{seat}/stop"),
            ]);
            let unit = guest::Unit {
                name: format!("{}seat", guest::UNIT_PREFIX),
                env: vec![(
                    "DBUS_SESSION_BUS_ADDRESS".into(),
                    format!("unix:path={}/bus", up.runtime),
                )],
                argv,
                socket: String::new(),
            };
            if !up.sh(&guest::run_unit(&unit, None)).ok() {
                return Err("lane-bus seat did not start".into());
            }
            if !up
                .sh(&guest::wait_file(&format!("{seat}/ready"), HANG_SECS))
                .ok()
            {
                return Err(format!(
                    "the seat's keyboard never had the window's focus ({HANG_SECS} s)"
                ));
            }
        }
    }
    let walk = walk_until(gui, &pid, proof::PROMPT);
    let frame = read_back(&walk, proof::PROMPT).map_err(|why| format!("walk: {why}"))?;
    wait_lines(
        gui,
        speechd_log,
        proof::PROMPT,
        1,
        "Orca never had the terminal's text spoken",
    )?;
    let unspoken_before = up
        .sh(&["grep".into(), "-cF".into(), NO_CAUSE.into(), debug.into()])
        .text
        .trim()
        .parse::<usize>()
        .unwrap_or(0);
    match on {
        On::Xvfb | On::MutterX11(_) => {
            for keys in [
                &["xdotool", "type", "--delay", "30", ORCA_TYPED][..],
                &["xdotool", "key", "Return"][..],
            ] {
                let typed = up.sh(&x_client(&x, keys));
                if !typed.ok() {
                    return Err(format!("{}: {}", keys.join(" "), typed.tail(2)));
                }
            }
        }
        On::MutterWayland(_) => {
            let _ = up.sh(&["touch".into(), format!("{seat}/go")]);
            if !up
                .sh(&guest::wait_file(&format!("{seat}/done"), HANG_SECS))
                .ok()
            {
                return Err(format!("the seat never typed the line ({HANG_SECS} s)"));
            }
        }
    }
    gui.expect(&["await", "match", ORCA_ECHO, "timeout=60000"], &|r| {
        r.starts_with("OK match")
    })?;
    if matches!(on, On::MutterWayland(_)) {
        let decided = up.sh(&[
            "timeout".into(),
            HANG_SECS.to_string(),
            "sh".into(),
            "-c".into(),
            "until grep -qF -- \"$2\" \"$1\" 2>/dev/null || \
             [ \"$(grep -cF -- \"$4\" \"$3\" 2>/dev/null)\" -gt \"$5\" ] 2>/dev/null; \
             do sleep 0.1; done"
                .into(),
            "wait-spoken".into(),
            speechd_log.into(),
            ORCA_ECHO.into(),
            debug.into(),
            NO_CAUSE.into(),
            unspoken_before.to_string(),
        ]);
        let _ = up.sh(&["touch".into(), format!("{seat}/stop")]);
        if !decided.ok() {
            return Err(format!(
                "Orca neither spoke the typed line's output nor said why not ({HANG_SECS} s)"
            ));
        }
    } else {
        wait_lines(
            gui,
            speechd_log,
            ORCA_ECHO,
            1,
            "Orca never had the typed line's output spoken",
        )?;
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(text: &str) -> String {
        format!(
            "apps 1\n\
             node depth=0 path=/org/a11y/atspi/accessible/root role=application name=\"aterm-gui\" \
             description=\"\" states=- children=1\n\
             node depth=1 path=/org/a11y/atspi/accessible/1 role=frame name=\"~\" description=\"\" \
             states=active,enabled children=1\n\
             node depth=2 path=/org/a11y/atspi/accessible/2 role=terminal name=\"aterm terminal\" \
             description=\"\" states=focused characters=30 caret=20 text=\"{text}\"\n"
        )
    }

    #[test]
    fn a_walk_reads_back_the_frame_when_the_terminal_holds_the_echo() {
        let w = walk("lane$ echo A11Y-$((6*7))\\nA11Y-42\\nlane$");
        assert_eq!(read_back(&w, BEFORE_ECHO), Ok("~".to_string()));
        assert!(
            read_back(&w, AFTER_ECHO)
                .unwrap_err()
                .contains("does not hold REJOINED-42")
        );
        assert!(
            read_back("walk failed (exit Some(1)): no application", BEFORE_ECHO)
                .unwrap_err()
                .contains("no application named aterm-gui")
        );
        let other = w.replace("name=\"aterm-gui\"", "name=\"gnome-terminal\"");
        assert!(read_back(&other, BEFORE_ECHO).is_err());
    }

    fn log(lost: &str) -> String {
        format!(
            "2026-10-19T10:00:00Z [INFO] {JOINED} :1.7\n\
             2026-10-19T10:00:05Z [WARN] {lost} (a write waited 5 s); screen readers lose this process\n\
             2026-10-19T10:00:05Z [INFO] accessibility: left the AT-SPI bus: why\n\
             2026-10-19T10:00:31Z [INFO] {JOINED} :1.9\n"
        )
    }

    #[test]
    fn a_rejoin_is_the_join_the_loss_and_the_join_again_in_order() {
        let stopped = log(STOPPED);
        let [joined, lost, again] = rejoined(&stopped, STOPPED).unwrap();
        assert!(joined.ends_with(":1.7") && again.ends_with(":1.9"));
        assert!(lost.contains(STOPPED));
        assert!(rejoined(&log(FAILED), STOPPED).is_err());
        let cut: String = log(STOPPED).lines().take(3).collect::<Vec<_>>().join("\n");
        assert!(
            rejoined(&cut, STOPPED)
                .unwrap_err()
                .contains("never that the publisher rejoined")
        );
        let late = format!("{STOPPED}\n{JOINED} :1.2\n");
        assert!(rejoined(&late, STOPPED).is_err());
    }

    fn evidence(ordeal: Ordeal) -> Evidence {
        Evidence {
            before: walk("lane$ echo A11Y-$((6*7))\\nA11Y-42"),
            after: walk("A11Y-42\\nlane$ echo REJOINED-$((6*7))\\nREJOINED-42"),
            log: log(ordeal.said()),
            stderr: "aterm-gui: GPU rendering on llvmpipe\n".into(),
            alive: true,
        }
    }

    #[test]
    fn a_rejoin_passes_only_read_back_alive_and_without_a_panic() {
        for ordeal in Ordeal::ALL {
            let ok = judge(&evidence(ordeal), ordeal).unwrap();
            assert!(ok.contains(ordeal.said()), "{ok}");
            let mut e = evidence(ordeal);
            e.stderr
                .push_str("thread 'atspi' panicked at publisher.rs:1:1:\n");
            assert!(judge(&e, ordeal).unwrap_err().contains("panicked"));
            let mut e = evidence(ordeal);
            e.alive = false;
            assert!(judge(&e, ordeal).unwrap_err().contains("not running"));
            let mut e = evidence(ordeal);
            e.after = walk("lane$");
            assert!(
                judge(&e, ordeal)
                    .unwrap_err()
                    .starts_with("after the rejoin")
            );
        }
        assert!(judge(&evidence(Ordeal::Killed), Ordeal::Frozen).is_err());
    }

    fn bench(rows: &[(&str, &str, &str)], summary: &str) -> String {
        let mut text =
            String::from("[DEBUG] accessibility: publishing on the AT-SPI bus as :1.4\n");
        for (name, status, detail) in rows {
            text.push_str(&format!(
                "{MEASURE_ROW}a11y-atspi::{name} {status}\n{detail}\n"
            ));
        }
        text + summary
    }

    #[test]
    fn a_bench_passes_only_a_complete_matrix_of_passes_and_exit_0() {
        let passed = [
            ("rejoin", "PASS", "ran"),
            ("frozen", "PASS", "left the frozen bus after 4389 changes"),
            ("control", "PASS", "never let go"),
        ];
        let all = "measure result: a11y-atspi planned=3 passed=3 failed=0 refused=0\n";
        let (verdict, detail) = measured("a11y-atspi", &bench(&passed, all), true);
        assert_eq!(verdict, Verdict::Pass, "{detail}");
        assert!(detail.contains("frozen PASS: left the frozen bus after 4389 changes"));
        assert_eq!(
            measured("a11y-atspi", &bench(&passed, all), false).0,
            Verdict::Fail
        );
        assert_eq!(
            measured("a11y-other", &bench(&passed, all), true).0,
            Verdict::Fail
        );
        let failed = "measure result: a11y-atspi planned=3 passed=1 failed=1 refused=1\n";
        assert_eq!(
            measured("a11y-atspi", &bench(&passed, failed), false).0,
            Verdict::Fail
        );
        let refused = "measure result: a11y-atspi planned=3 passed=0 failed=0 refused=3\n";
        assert_eq!(
            measured("a11y-atspi", &bench(&passed, refused), false).0,
            Verdict::NotRun
        );
        for summary in [
            "error: could not compile `aterm-a11y`\n",
            &format!("{all}{all}"),
            "measure result: a11y-atspi planned=3 passed=2 failed=0 refused=0\n",
            "measure result: a11y-atspi planned=0 passed=0 failed=0 refused=0\n",
            "measure result: a11y-atspi planned=3 passed=3 failed=0\n",
        ] {
            let (verdict, detail) = measured("a11y-atspi", &bench(&passed, summary), true);
            assert_eq!(verdict, Verdict::Fail, "{summary}: {detail}");
        }
    }

    #[test]
    fn only_rosettas_own_abort_of_a_dumped_aterm_gui_is_not_run() {
        let stderr = "\
            aterm-gui: GPU rendering on llvmpipe (LLVM 20.1.2, 256 bits) (Gl)\n\
            libEGL warning: DRI3 error: Could not get DRI3 device\n\
            assertion failed [!readlink_result_self.is_error]: Could not readlinkat /proc/13425/exe\n\
            (ThreadContextFcntl.cpp:142 was_rosetta_process_invoked_directly)\n";
        let dumped = "ActiveState=failed Result=core-dump ExecMainCode=3 ExecMainStatus=5";
        let abort = translator_abort(Arch::X86_64, dumped, stderr).unwrap();
        assert!(abort.contains("readlinkat") && abort.contains("ThreadContextFcntl.cpp:142"));
        let (verdict, detail) = translated_away(Arch::X86_64, dumped, stderr, "why".into());
        assert_eq!(verdict, Verdict::NotRun);
        assert!(detail.contains("Rosetta aborted aterm-gui") && detail.ends_with("why"));
        assert_eq!(translator_abort(Arch::Aarch64, dumped, stderr), None);
        for alive in [
            "ActiveState=active Result=success ExecMainCode=0 ExecMainStatus=0",
            "ActiveState=failed Result=exit-code ExecMainCode=1 ExecMainStatus=101",
            "",
        ] {
            assert_eq!(
                translator_abort(Arch::X86_64, alive, stderr),
                None,
                "{alive}"
            );
        }
        let panicked = "thread 'main' panicked at crates/aterm-a11y/src/atspi/publisher.rs:1:1:\n\
                        assertion failed: joined\n";
        assert_eq!(translator_abort(Arch::X86_64, dumped, panicked), None);
        assert_eq!(
            translated_away(Arch::X86_64, dumped, panicked, "why".into()),
            (Verdict::Fail, "why".to_string())
        );
    }

    fn queued(at: &str, ssml: &str) -> String {
        format!("[Mon Oct 19 {at} 2026 : 1] speechd:     {QUEUED}{ssml}| with priority 2\n")
    }

    #[test]
    fn speech_is_the_queued_messages_unmarked_and_unescaped() {
        let log = [
            queued("21:59:06", "<speak>Screen reader on.</speak>"),
            "[Mon Oct 19 21:59:08 2026 : 2] speechd:     Incoming text: |<speak>x</speak>|\n"
                .into(),
            queued(
                "21:59:08",
                "<speak>a &amp; &lt;b&gt; &quot;c&quot; &apos;d&apos;</speak>",
            ),
        ]
        .concat();
        assert_eq!(spoken(&log), ["Screen reader on.", "a & <b> \"c\" 'd'"]);
    }

    #[test]
    fn orca_must_speak_the_frame_the_text_and_the_typed_output() {
        let log = [
            queued("21:59:06", "<speak><mark name=\"0:6\"/>Screen <mark name=\"7:13\"/>reader <mark name=\"14:17\"/>on.</speak>"),
            queued("21:59:07", "<speak><mark name=\"0:1\"/>~ <mark name=\"2:8\"/>frame.</speak>"),
            queued("21:59:07", "<speak><mark name=\"0:5\"/>lane$\n</speak>"),
            queued("21:59:07", "<speak><mark name=\"0:5\"/>space</speak>"),
            queued("21:59:07", "<speak><mark name=\"0:4\"/>left <mark name=\"5:10\"/>shift</speak>"),
            queued("21:59:07", "<speak> <mark name=\"1:5\"/>echo <mark name=\"6:7\"/>O</speak>"),
            queued("21:59:07", "-"),
            queued("21:59:08", "6"),
            queued("21:59:08", "<speak><mark name=\"0:6\"/>return</speak>"),
            queued("21:59:08", ")"),
            queued("21:59:08", "<speak><mark name=\"0:7\"/>ORCA-42\n<mark name=\"8:13\"/>lane$</speak>"),
        ]
        .concat();
        let speech = spoken(&log);
        assert_eq!(
            speech,
            [
                "Screen reader on.",
                "~ frame.",
                "lane$\n",
                "space",
                "left shift",
                " echo O",
                "-",
                "6",
                "return",
                ")",
                "ORCA-42\nlane$",
            ]
        );
        let passed = heard(&speech, "~").unwrap();
        assert!(passed.contains("the terminal lane$ and"), "{passed}");
        let without = |gone: &dyn Fn(&str) -> bool| -> Vec<String> {
            speech.iter().filter(|s| !gone(s)).cloned().collect()
        };
        assert!(
            heard(&without(&|s| s.starts_with("ORCA-42")), "~")
                .unwrap_err()
                .contains("typed line's output")
        );
        assert!(
            heard(&speech, "lane-frame")
                .unwrap_err()
                .contains("frame's name")
        );
        assert!(
            heard(&without(&|s| s == "lane$\n"), "~")
                .unwrap_err()
                .contains("terminal's text")
        );
        let frame_and_output = ["~ frame.".to_string(), "ORCA-42\nlane$".into()];
        assert!(
            heard(&frame_and_output, "~")
                .unwrap_err()
                .contains("terminal's text")
        );
        let echoed = [
            "~ frame.".to_string(),
            "lane$ echo O".into(),
            "ORCA-42\nlane$".into(),
        ];
        assert!(heard(&echoed, "~").unwrap_err().contains("terminal's text"));
    }

    #[test]
    fn where_orca_hears_no_key_an_unspoken_output_must_be_for_want_of_one() {
        let speech: Vec<String> = [
            "Screen reader on.",
            "~ frame.",
            "lane$\n",
            " e",
            "echo ORCA-$((6*7))",
            "~",
            "lane$",
        ]
        .map(String::from)
        .to_vec();
        let deaf = "22:57:33.180230 - EVENT MANAGER: Processing window:activate\n\
                    22:57:37.397468 - DEFAULT: Not speaking inserted string due to lack of cause\n";
        let passed = heard_keyless(&speech, "~", deaf).unwrap();
        assert!(
            passed.contains("left the output ORCA-42 unspoken"),
            "{passed}"
        );
        assert!(
            heard(&speech, "~")
                .unwrap_err()
                .contains("typed line's output")
        );
        let heard_keys = format!("{deaf}22:57:36.1 - TERMINAL: Presenting keyboard event e\n");
        assert!(
            heard_keyless(&speech, "~", &heard_keys)
                .unwrap_err()
                .contains("though it heard 1 keys")
        );
        assert!(
            heard_keyless(&speech, "~", "22:57:33.18 - EVENT MANAGER: Processing\n")
                .unwrap_err()
                .contains("never says")
        );
        assert!(
            heard_keyless(&speech, "lane-frame", deaf)
                .unwrap_err()
                .contains("frame's name")
        );
        let no_text: Vec<String> = speech.iter().filter(|s| *s != "lane$\n").cloned().collect();
        assert!(
            heard_keyless(&no_text, "~", deaf)
                .unwrap_err()
                .contains("terminal's text")
        );
        let mut spoken_out = speech.clone();
        spoken_out.push("ORCA-42".into());
        assert_eq!(
            heard_keyless(&spoken_out, "~", deaf),
            heard(&spoken_out, "~")
        );
        assert!(heard(&spoken_out, "~").is_ok());
    }

    #[test]
    fn an_output_spoken_with_its_whole_line_is_spoken() {
        let log = [
            queued("23:31:12", "<speak><mark name=\"0:6\"/>Screen <mark name=\"7:13\"/>reader <mark name=\"14:17\"/>on.</speak>"),
            queued("23:31:13", "<speak><mark name=\"0:1\"/>~ <mark name=\"2:8\"/>frame.</speak>"),
            queued("23:31:13", "<speak><mark name=\"0:5\"/>lane$\n</speak>"),
            queued("23:31:13", "<speak> <mark name=\"1:5\"/>echo <mark name=\"6:18\"/>ORCA-$((6*7)</speak>"),
            queued("23:31:13", "<speak><mark name=\"0:5\"/>lane$ <mark name=\"6:10\"/>echo <mark name=\"11:24\"/>ORCA-$((6*7))\n<mark name=\"25:32\"/>ORCA-42\n<mark name=\"33:38\"/>lane$</speak>"),
        ]
        .concat();
        let speech = spoken(&log);
        assert_eq!(
            speech,
            [
                "Screen reader on.",
                "~ frame.",
                "lane$\n",
                " echo ORCA-$((6*7)",
                "lane$ echo ORCA-$((6*7))\nORCA-42\nlane$",
            ]
        );
        let passed = heard(&speech, "~").unwrap();
        assert!(
            passed.contains("the output lane$ echo ORCA-$((6*7)) ORCA-42 lane$"),
            "{passed}"
        );
        assert_eq!(heard_keyless(&speech, "~", ""), heard(&speech, "~"));
        let unanswered = [
            "~ frame.".to_string(),
            "lane$\n".into(),
            "lane$ echo ORCA-$((6*7))\n".into(),
            "echo ORCA-$((6*7))".into(),
        ];
        assert!(
            heard(&unanswered, "~")
                .unwrap_err()
                .contains("typed line's output")
        );
    }

    #[test]
    fn mutter_s_command_names_its_xwayland_and_its_socket() {
        let given = "WAYLAND_DISPLAY=aterm-lane-mutter-orca\nDISPLAY=:0\n\
                     XAUTHORITY=/run/user/501/.mutter-Xwaylandauth.86VRX3\n";
        assert_eq!(
            mutter_env(given, "/run/user/501").unwrap(),
            (
                vec![
                    ("DISPLAY".to_string(), ":0".to_string()),
                    (
                        "XAUTHORITY".to_string(),
                        "/run/user/501/.mutter-Xwaylandauth.86VRX3".to_string()
                    ),
                ],
                (
                    "WAYLAND_DISPLAY".to_string(),
                    "/run/user/501/aterm-lane-mutter-orca".to_string()
                ),
            )
        );
        assert!(
            mutter_env("WAYLAND_DISPLAY=aterm-lane-mutter-orca\n", "/run/user/501")
                .unwrap_err()
                .contains("no DISPLAY")
        );
        assert!(
            mutter_env("DISPLAY=:0\nWAYLAND_DISPLAY=w\nXAUTHORITY=\n", "/r")
                .unwrap_err()
                .contains("no XAUTHORITY")
        );
    }

    #[test]
    fn the_seat_types_the_line_a_key_at_a_time() {
        assert_eq!(
            seat_keys("echo O-$(("),
            [
                "tap:e",
                "tap:c",
                "tap:h",
                "tap:o",
                "tap:space",
                "tap:O",
                "tap:-",
                "tap:$",
                "tap:(",
                "tap:("
            ]
        );
        assert_eq!(seat_keys(ORCA_TYPED).len(), ORCA_TYPED.len());
    }
}
