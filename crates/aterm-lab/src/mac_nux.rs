// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab mac nux`: what a person meets on a new Mac, measured in fresh macOS guests ([`Scenario`]).
//!
//! Each row runs in its own clone of the lane's prepared base (`crate::mac`: SIP on, booted with
//! no window, driven only through `tart exec`, never a connection to the guest's address). The
//! app is built here (`ship dev-app`, made release-shaped: `aterm.app`, the release's bundle id,
//! no dev mark, hardened runtime with the release's entitlements, signed ad hoc) or read from
//! `--app` (never changed), shared read-only as a zip (the share does not carry the bundle's links)
//! and unpacked into the guest's `/Applications`; `--standard-user` makes a non-admin account,
//! logs it in at the console and unpacks into its `~/Applications`. A guest-side driver times each
//! command: [`DRIVE`] (expect, on a terminal, answering each prompt with its default and a vault
//! join password with `none`) or [`NOTTY`] (perl, no terminal). What a row could not measure says
//! NOT MEASURED with the reason, never 0.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::instance::{take_flag, take_value};
use crate::mac::{NOT_RUN, POLL, ROW, STEP, Vm, not_run};
use crate::paths::paths;
use crate::{Fail, LAB, Res, stop};

pub const USAGE: &str = "\
  nux [--app PATH] [--scenario ROW,...] [--standard-user] [--image REF] [--keep] [--record FILE]
                what a person meets on a new Mac, each row in a fresh clone of REF's prepared base (as dmg):
                the app built here (ship dev-app made release-shaped: aterm.app, com.aterm.aterm, hardened
                runtime, signed ad hoc) or --app PATH (read, never changed), unpacked into the guest's
                /Applications (--standard-user: a non-admin account logged in at the console, and its
                ~/Applications). Rows (default all), each step under a 300 s hang detector:
                  init-apply       aterm init --json on a terminal, each prompt answered with its default
                                   (a vault join password with none): first byte, end, prompts, lines,
                                   exit, every row's elapsed_ms (the slowest named), Dock/Finder/
                                   SystemUIServer relaunches, the Dock's pins, change-journal records,
                                   defaults and killall execs, init's phases; then a second, no-op
                                   aterm init and aterm init --check, each with its lines, prompts, end
                  init-notty       aterm init with no terminal (an agent's shape): the same, but no prompts
                  init-ssh         aterm init over ssh -tt to the guest itself (Remote Login turned on
                                   first), then a bare aterm over ssh -tt to its shell prompt, with what
                                   each changed
                  doctor           aterm doctor on a terminal (Developer Tools not granted): first byte, end,
                                   lines, prompts, exit, System Settings opened; aterm doctor --json | cat
                                   (no terminal): System Settings opened; aterm doctor --read-only --json
                                   x10: p50/p95, rows, timed rows, phases
                  first-launch     the app opened and watched 10 min: first prompt, first present, WARN
                                   lines, consent dialogs (with a ~/Desktop control), the toolset pass,
                                   the setup lines and hand-overs in order, change-journal records,
                                   defaults and killall execs, relaunches (the guest's disk grown to
                                   100 GB first)
                  terminal-launch  a bare aterm on a terminal (Terminal.app's shape) to its shell's first
                                   prompt, cold then warm
                  app-launch       the app opened to its first tab's prompt, cold then warm
                  vault            aterm init --only workstation off the ALab network, Enter at each vault
                                   prompt: time to the vault prompt and the silence before it, the mDNS
                                   questions for the ALab vault, the end (a guest that can reach an ALab
                                   vault is NOT RUN)
                  vault-local      a vault served in the guest on 127.0.0.1 only (never advertised) by a
                                   second identity that approves joins: aterm init --only workstation
                                   --vault <it> with its join password typed: to the password prompt,
                                   typed to a synced secret on disk, typed to the end
                  successor        install.sh --version N (the release before the latest), then the
                                   updater to the latest with a window open, and the successor's setup
                                   and Full Disk Access lines in its first 30 s
                  install-sh       tools/install.sh on a terminal as the account at the console: first
                                   byte, end, its last line, the aterm process after its exit (and the
                                   typed steps a person still has), then the opened app's first log line
                                   and its toolset pass's start and end within 3 min (the guest's disk
                                   grown to 100 GB first)
                Prints a markdown table; --record FILE appends it. Exit 0 every row measured, 1 a row
                failed (a hang detector fired, a guest step failed), 3 NOT RUN (a row the guest could not
                run, no tart)
";

/// The guest folder the drivers and every record live in (outside the share).
const GUEST_NUX: &str = "/tmp/aterm-nux";

/// How long `first-launch` watches the app.
const WATCH: Duration = Duration::from_secs(600);

/// The disk `first-launch` and `install-sh` grow their clone to: the toolset pass refuses under
/// 8.9 GiB free, and the image leaves 4.4 GiB.
const FIRST_LAUNCH_DISK_GB: &str = "100";

/// How long `install-sh` looks for the aterm process once install.sh has exited, and then for the
/// opened app's toolset pass.
const OPENED_WATCH: Duration = Duration::from_secs(30);
const TOOLSET_WATCH: Duration = Duration::from_secs(180);

/// The standard account `--standard-user` makes, and its password (macOS refuses one under 4
/// characters: `New account password error (5402)`).
const STANDARD_USER: &str = "nux";
const STANDARD_PASSWORD: &str = "nux-lane-2026";

/// The public release pointer install.sh reads first.
const LATEST_POINTER: &str =
    "https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml";

/// The ALab vault's host and service type: a guest that reaches either never runs the vault row.
const ALAB_HOST: &str = "alab-secrets.local";
const ALAB_SERVICE: &str = "_alab-vault._tcp";

/// The pty driver: `drive.exp <record> <limit_s> <cmd> [arg...]`. A known prompt is answered at
/// once; any other partial line left 1.5 s is a prompt, answered with Enter (its default). Each
/// prompt's record names when it came and how long the command was silent before it.
/// `$NUX_JOIN` answers a vault join password (default `none`), `$NUX_SUDO` a sudo password. With
/// `$NUX_SHELL` (a shell session's run), the first [`SHELL_READY`] line is its shell's first
/// prompt: recorded, then answered with `exit`; nothing unknown is answered.
pub const DRIVE: &str = r#"set record [lindex $argv 0]
set limit [lindex $argv 1]
set f [open $record w]
proc rec {args} { global f; puts $f [join $args " "]; flush $f }
proc ms {} { global t0; expr {[clock milliseconds] - $t0} }
set join none
if {[info exists env(NUX_JOIN)]} { set join $env(NUX_JOIN) }
set sudo admin
if {[info exists env(NUX_SUDO)]} { set sudo $env(NUX_SUDO) }
set shell [info exists env(NUX_SHELL)]
set exited 0
set quiet 1500
log_user 1
match_max 100000
set t0 [clock milliseconds]
rec start_epoch_ms $t0
if {[catch {spawn -noecho {*}[lrange $argv 2 end]} why]} { rec spawn_failed $why; exit 4 }
set deadline [expr {$t0 + $limit * 1000}]
set timeout $limit
expect {
    -notransfer -re . { rec first_byte_ms [ms] }
    eof { rec first_byte_ms none }
    timeout { rec hang first_byte; exit 3 }
}
set prompts 0
set waited 0
set last 0
proc answer {text reply} {
    global prompts last
    incr prompts
    regsub -all {[^ -~]} $text {} clean
    set now [ms]
    rec prompt $now [expr {$now - $last}] [string range [string trim $clean] 0 79]
    send -- "$reply\r"
    set last $now
}
while {1} {
    if {[clock milliseconds] >= $deadline} { rec hang [ms]; exit 3 }
    set timeout [expr {$quiet / 1000.0}]
    expect {
        -re {nux-shell-ready} {
            if {$shell && !$exited} { set exited 1; rec shell_prompt_ms [ms]; send -- "exit\r" }
            set last [ms]
        }
        -re {[^\r\n]*Vault join password[^\r\n]*: $} { answer $expect_out(0,string) $join }
        -re {[^\r\n]*Retype the join password[^\r\n]*: $} { answer $expect_out(0,string) $join }
        -re {[^\r\n]*\(Enter keeps [^\r\n]*\): $} { answer $expect_out(0,string) "" }
        -re {[^\r\n]*\[y/N\] $} { answer $expect_out(0,string) "" }
        -re {[^\r\n]*[Pp]assword[^\r\n]*: $} { answer $expect_out(0,string) $sudo }
        -re {[^\r\n]*[\r\n]+} { set last [ms] }
        eof { break }
        timeout {
            set timeout 0
            expect {
                -re {[^\r\n]+$} {
                    if {!$shell} { incr waited $quiet; answer $expect_out(0,string) "" }
                }
                eof { break }
                timeout {}
            }
        }
    }
}
rec end_ms [ms]
rec prompts $prompts
rec quiet_waited_ms $waited
set status [wait]
rec exit [lindex $status 3]
"#;

/// The no-terminal driver: `notty.pl <record> <limit_s> <cmd> [arg...]`, stdin `/dev/null`,
/// stdout and stderr through to its own stdout, the same record as [`DRIVE`]'s.
pub const NOTTY: &str = r#"use strict; use warnings; use POSIX ":sys_wait_h"; use Time::HiRes qw(time);
my ($record, $limit, @cmd) = @ARGV;
open(my $f, '>', $record) or die "$record: $!";
$f->autoflush(1); $| = 1;
my $t0 = time;
printf $f "start_epoch_ms %d\n", int($t0 * 1000);
pipe(my $r, my $w) or die "pipe: $!";
my $pid = fork() // die "fork: $!";
if ($pid == 0) {
    close $r; setpgrp(0, 0);
    open(STDIN, '<', '/dev/null'); open(STDOUT, '>&', $w); open(STDERR, '>&', $w);
    exec { $cmd[0] } @cmd or exit 127;
}
close $w;
my ($first, $status, $ended, $hang, $eof);
my $rin = ''; vec($rin, fileno($r), 1) = 1;
while (1) {
    if (!defined $status && waitpid($pid, WNOHANG) == $pid) { $status = $?; $ended = time; }
    if (time - $t0 >= $limit) { $hang = 1; kill 'KILL', -$pid; kill 'KILL', $pid; last; }
    if ($eof) { last if defined $status; select(undef, undef, undef, 0.02); next; }
    my $n = select(my $rout = $rin, undef, undef, 0.02);
    if ($n > 0) {
        my $got = sysread($r, my $buf, 65536);
        if ($got) { $first //= time; print $buf; } else { $eof = 1; }
        next;
    }
    last if defined $status;
}
printf $f "first_byte_ms %s\n", defined $first ? int(($first - $t0) * 1000) : 'none';
if ($hang) { printf $f "hang %d\n", int((time - $t0) * 1000); exit 3; }
printf $f "end_ms %d\n", int(($ended - $t0) * 1000);
print $f "prompts 0\n";
printf $f "exit %d\n", ($status & 127) ? 128 + ($status & 127) : $status >> 8;
"#;

/// One row of the lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    InitApply,
    InitNotty,
    InitSsh,
    Doctor,
    FirstLaunch,
    TerminalLaunch,
    AppLaunch,
    Vault,
    VaultLocal,
    Successor,
    InstallSh,
}

/// Every row, in the order a run takes them.
pub const ALL: [Scenario; 11] = [
    Scenario::InitApply,
    Scenario::InitNotty,
    Scenario::InitSsh,
    Scenario::Doctor,
    Scenario::FirstLaunch,
    Scenario::TerminalLaunch,
    Scenario::AppLaunch,
    Scenario::Vault,
    Scenario::VaultLocal,
    Scenario::Successor,
    Scenario::InstallSh,
];

impl Scenario {
    /// Its name on the command line and in the table.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scenario::InitApply => "init-apply",
            Scenario::InitNotty => "init-notty",
            Scenario::InitSsh => "init-ssh",
            Scenario::Doctor => "doctor",
            Scenario::FirstLaunch => "first-launch",
            Scenario::TerminalLaunch => "terminal-launch",
            Scenario::AppLaunch => "app-launch",
            Scenario::Vault => "vault",
            Scenario::VaultLocal => "vault-local",
            Scenario::Successor => "successor",
            Scenario::InstallSh => "install-sh",
        }
    }

    /// Whether it runs the app this run built or was handed (the others install a public release).
    #[must_use]
    pub const fn needs_app(self) -> bool {
        !matches!(self, Scenario::Successor | Scenario::InstallSh)
    }
}

/// The rows `--scenario` names (comma-separated, `all` for every row), in [`ALL`]'s order, each
/// once.
///
/// # Errors
/// An empty list or a name no row has.
pub fn parse_scenarios(list: &str) -> Result<Vec<Scenario>, String> {
    let mut picked = Vec::new();
    for word in list.split(',').map(str::trim) {
        if word == "all" {
            picked.extend(ALL);
            continue;
        }
        let row = ALL.into_iter().find(|s| s.name() == word).ok_or_else(|| {
            format!(
                "no row '{word}' (rows: {})",
                ALL.map(Scenario::name).join(", ")
            )
        })?;
        picked.push(row);
    }
    if picked.is_empty() {
        return Err("--scenario names no row".into());
    }
    Ok(ALL.into_iter().filter(|s| picked.contains(s)).collect())
}

/// What a run was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct Ask {
    pub app: Option<PathBuf>,
    pub scenarios: Vec<Scenario>,
    pub standard_user: bool,
    pub image: String,
    pub keep: bool,
    pub record: Option<PathBuf>,
}

/// Parse `mac nux`'s arguments (`image` is the default image).
///
/// # Errors
/// Usage.
pub fn parse(args: &[String], image: String) -> Result<Ask, Fail> {
    let mut a = args.to_vec();
    // The lab works from `/` (`sys`), so a relative path is the caller's, made absolute here.
    let from_caller = |p: String| PathBuf::from(crate::paths::from_caller(&p));
    let app = take_value(&mut a, "--app")?.map(from_caller);
    let scenarios = match take_value(&mut a, "--scenario")? {
        Some(list) => parse_scenarios(&list).map_err(|e| Fail::usage(format!("mac nux: {e}")))?,
        None => ALL.to_vec(),
    };
    let standard_user = take_flag(&mut a, "--standard-user");
    let image = take_value(&mut a, "--image")?.unwrap_or(image);
    let keep = take_flag(&mut a, "--keep");
    let record = take_value(&mut a, "--record")?.map(from_caller);
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "mac nux: unexpected '{extra}' ({LAB} mac help)"
        )));
    }
    Ok(Ask {
        app,
        scenarios,
        standard_user,
        image,
        keep,
        record,
    })
}

/// One measured value of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Ms(u64),
    Count(u64),
    Text(String),
    /// Not measured, and why: never shown as 0.
    Missing(String),
    /// Not run, and why (a credential the guest has not).
    NotRun(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Ms(ms) => write!(f, "{ms} ms"),
            Value::Count(n) => write!(f, "{n}"),
            Value::Text(t) => f.write_str(t),
            Value::Missing(why) => write!(f, "NOT MEASURED ({why})"),
            Value::NotRun(why) => write!(f, "NOT RUN ({why})"),
        }
    }
}

/// `Some` as `make`'s value, else NOT MEASURED for `why`.
fn or_missing<T>(v: Option<T>, make: impl FnOnce(T) -> Value, why: &str) -> Value {
    v.map_or_else(|| Value::Missing(why.to_owned()), make)
}

/// How a row ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Measured,
    Failed(String),
    NotRun(String),
}

/// One row of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub scenario: Scenario,
    pub outcome: Outcome,
    pub cells: Vec<(String, Value)>,
}

impl Row {
    fn new(scenario: Scenario) -> Row {
        Row {
            scenario,
            outcome: Outcome::Measured,
            cells: Vec::new(),
        }
    }

    fn cell(&mut self, name: impl Into<String>, value: Value) {
        self.cells.push((name.into(), value));
    }

    /// Fail the row for `why`, unless an earlier step already failed it (the first cause stays).
    fn fail(&mut self, why: String) {
        if !matches!(self.outcome, Outcome::Failed(_)) {
            self.outcome = Outcome::Failed(why);
        }
    }

    /// The row a lane step's refusal leaves: NOT RUN when the guest could not run it (a
    /// [`NOT_RUN`] refusal), else failed.
    #[must_use]
    pub fn refused(scenario: Scenario, fail: &Fail) -> Row {
        let why = fail
            .msg
            .clone()
            .unwrap_or_else(|| format!("exit {}", fail.code));
        let why = squeeze(why.strip_prefix("NOT RUN: ").unwrap_or(&why), 400);
        Row {
            scenario,
            outcome: if fail.code == NOT_RUN {
                Outcome::NotRun(why)
            } else {
                Outcome::Failed(why)
            },
            cells: Vec::new(),
        }
    }
}

/// The run's exit: 1 when a row failed, else 3 when a row was not run, else 0.
#[must_use]
pub fn exit_code(rows: &[Row]) -> i32 {
    if rows.iter().any(|r| matches!(r.outcome, Outcome::Failed(_))) {
        1
    } else if rows.is_empty() || rows.iter().any(|r| matches!(r.outcome, Outcome::NotRun(_))) {
        NOT_RUN
    } else {
        0
    }
}

/// `text` safe in a markdown table cell.
fn cell_text(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// The rows as a markdown table, under `head` (one line of what ran where).
#[must_use]
pub fn table(head: &str, rows: &[Row]) -> String {
    let mut out = format!("{head}\n\n| Row | Result | Measured |\n| --- | --- | --- |\n");
    for row in rows {
        let result = match &row.outcome {
            Outcome::Measured => "measured".to_owned(),
            Outcome::Failed(why) => format!("FAILED: {why}"),
            Outcome::NotRun(why) => format!("NOT RUN: {why}"),
        };
        let cells = row
            .cells
            .iter()
            .map(|(name, value)| format!("{name} {value}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            row.scenario.name(),
            cell_text(&result),
            cell_text(&cells)
        );
    }
    out
}

/// What a driver's record says ([`DRIVE`], [`NOTTY`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drive {
    pub start_epoch_ms: Option<u64>,
    pub first_byte_ms: Option<u64>,
    pub end_ms: Option<u64>,
    /// Each prompt: when it came, how long the command wrote nothing before it, its text.
    pub prompts: Vec<(u64, u64, String)>,
    pub quiet_waited_ms: Option<u64>,
    pub exit: Option<i32>,
    /// The hang detector fired, at this point.
    pub hang: Option<String>,
    pub spawn_failed: Option<String>,
    /// A shell session's run (`$NUX_SHELL`): its shell's first prompt.
    pub shell_prompt_ms: Option<u64>,
}

/// [`Drive`] over a record's lines.
#[must_use]
pub fn read_drive(record: &str) -> Drive {
    let mut d = Drive::default();
    for line in record.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "start_epoch_ms" => d.start_epoch_ms = rest.trim().parse().ok(),
            "first_byte_ms" => d.first_byte_ms = rest.trim().parse().ok(),
            "end_ms" => d.end_ms = rest.trim().parse().ok(),
            "quiet_waited_ms" => d.quiet_waited_ms = rest.trim().parse().ok(),
            "exit" => d.exit = rest.trim().parse().ok(),
            "hang" => d.hang = Some(rest.trim().to_owned()),
            "spawn_failed" => d.spawn_failed = Some(rest.trim().to_owned()),
            "shell_prompt_ms" => d.shell_prompt_ms = rest.trim().parse().ok(),
            "prompt" => {
                let mut parts = rest.splitn(3, ' ');
                let at = parts.next().and_then(|a| a.parse().ok());
                let gap = parts.next().and_then(|g| g.parse().ok());
                if let (Some(at), Some(gap)) = (at, gap) {
                    d.prompts
                        .push((at, gap, parts.next().unwrap_or("").to_owned()));
                }
            }
            _ => {}
        }
    }
    d
}

/// A row's cells from its driver's record, each name after `label` (one run of several); a hang,
/// a driver that did not start or one that left no record fails it.
fn drive_cells(row: &mut Row, label: &str, d: &Drive) {
    if let Some(why) = &d.spawn_failed {
        row.fail(format!(
            "{label}the driver could not start the command: {why}"
        ));
    } else if let Some(at) = &d.hang {
        row.fail(format!(
            "{label}the {} s hang detector fired ({at})",
            ROW.as_secs()
        ));
    } else if d.start_epoch_ms.is_none() {
        row.fail(format!("{label}the driver left no record"));
    }
    row.cell(
        format!("{label}first byte"),
        or_missing(d.first_byte_ms, Value::Ms, "the command wrote nothing"),
    );
    row.cell(
        format!("{label}end"),
        or_missing(d.end_ms, Value::Ms, "the record has no end"),
    );
    row.cell(
        format!("{label}prompts"),
        Value::Count(u64::try_from(d.prompts.len()).unwrap_or(u64::MAX)),
    );
    if let Some(waited) = d.quiet_waited_ms.filter(|w| *w > 0) {
        row.cell(
            format!("{label}of it waiting to tell a prompt"),
            Value::Ms(waited),
        );
    }
    row.cell(
        format!("{label}exit"),
        or_missing(
            d.exit,
            |c| Value::Text(c.to_string()),
            "the record has no exit",
        ),
    );
}

/// The lines a person reads of a command's output: each line with a printing character outside
/// its terminal escapes (CSI to its final byte, OSC to its bell or string terminator).
#[must_use]
pub fn output_lines(text: &str) -> usize {
    #[derive(Clone, Copy, PartialEq)]
    enum At {
        Text,
        Escape,
        Csi,
        Osc,
        OscEscape,
    }
    text.lines()
        .filter(|line| {
            let mut at = At::Text;
            let mut printed = false;
            for c in line.chars() {
                at = match (at, c) {
                    (At::Text, '\u{1b}') => At::Escape,
                    (At::Text, c) => {
                        printed |= !c.is_whitespace() && !c.is_control();
                        At::Text
                    }
                    (At::Escape, '[') => At::Csi,
                    (At::Escape, ']') => At::Osc,
                    (At::Escape, _) => At::Text,
                    (At::Csi, '\u{40}'..='\u{7e}') => At::Text,
                    (At::Csi, _) => At::Csi,
                    (At::Osc, '\u{7}') => At::Text,
                    (At::Osc, '\u{1b}') => At::OscEscape,
                    (At::Osc, _) => At::Osc,
                    (At::OscEscape, _) => At::Text,
                };
            }
            printed
        })
        .count()
}

/// The `lines` cell of a run's output under `label`.
fn lines_cell(row: &mut Row, label: &str, out: &str) {
    row.cell(
        format!("{label}lines"),
        Value::Count(u64::try_from(output_lines(out)).unwrap_or(u64::MAX)),
    );
}

/// One `aterm.log` line: `<epoch.ms> <pid> <LEVEL> <target>: <message>`.
#[derive(Debug, PartialEq, Eq)]
pub struct LogLine<'a> {
    pub at_ms: u64,
    pub pid: u32,
    pub level: &'a str,
    pub target: &'a str,
    pub message: &'a str,
}

/// The log's lines that parse as [`LogLine`]s.
#[must_use]
pub fn log_lines(text: &str) -> Vec<LogLine<'_>> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, ' ');
            let stamp = parts.next()?;
            let pid = parts.next()?.parse().ok()?;
            let level = parts.next()?;
            let (target, message) = parts.next()?.split_once(": ")?;
            let (secs, frac) = stamp.split_once('.').unwrap_or((stamp, "0"));
            let frac = format!("{frac:0<3}");
            let at_ms = secs
                .parse::<u64>()
                .ok()?
                .checked_mul(1000)?
                .checked_add(frac.get(..3)?.parse::<u64>().ok()?)?;
            Some(LogLine {
                at_ms,
                pid,
                level,
                target,
                message,
            })
        })
        .collect()
}

/// The phases the last `<verb>` summary line names (`aterm init: 40 rows, exit 1; context 12 ms,
/// …, total 90 ms`), by name, `total` among them; `None` when no such line is in the log.
#[must_use]
pub fn run_phases(lines: &[LogLine<'_>], verb: &str) -> Option<BTreeMap<String, u64>> {
    let line = lines
        .iter()
        .rev()
        .find(|l| l.message.starts_with(verb) && l.message.contains(" ms"))?;
    let (_, laps) = line.message.rsplit_once("; ")?;
    let mut phases = BTreeMap::new();
    for lap in laps.split(", ") {
        let lap = lap.trim().strip_suffix(" ms")?;
        let (name, ms) = lap.rsplit_once(' ')?;
        phases.insert(name.to_owned(), ms.parse().ok()?);
    }
    phases.contains_key("total").then_some(phases)
}

/// A row's phase cells under `label` from `log`, NOT MEASURED when it has no summary line for
/// `verb` (a build before the line existed, or a run that ended first).
fn phase_cells(row: &mut Row, label: &str, log: &str, verb: &str) {
    let lines = log_lines(log);
    match run_phases(&lines, verb) {
        Some(phases) => {
            let text = phases
                .iter()
                .map(|(name, ms)| format!("{name} {ms}"))
                .collect::<Vec<_>>()
                .join(", ");
            row.cell(format!("{label}phases (ms)"), Value::Text(text));
        }
        None => row.cell(
            format!("{label}phases"),
            Value::Missing(format!("aterm.log has no `{verb}` summary line")),
        ),
    }
}

/// The lines `log` holds past its first `mark` lines: what a run appended to `aterm.log` after
/// the log held `mark` lines, so an earlier run's summary line is never read as this one's (a log
/// that rotated meanwhile holds fewer lines and reads as empty: NOT MEASURED, never stale).
fn appended(log: &str, mark: usize) -> String {
    log.lines().skip(mark).fold(String::new(), |mut out, line| {
        out.push_str(line);
        out.push('\n');
        out
    })
}

/// The first shell prompt an aterm tab showed at or after `since` (epoch seconds), in ms after
/// it, from the prompt marker's lines (`<epoch seconds> <pid> <TERM_PROGRAM>`).
#[must_use]
pub fn first_prompt_ms(prompts: &str, since: f64) -> Option<u64> {
    prompts
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let at: f64 = parts.next()?.parse().ok()?;
            let program = parts.nth(1)?;
            (program == "aterm" && at >= since).then_some(at)
        })
        .min_by(f64::total_cmp)
        .map(|at| secs_to_ms(at - since))
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative span of seconds, rounded to whole ms"
)]
fn secs_to_ms(secs: f64) -> u64 {
    (secs.max(0.0) * 1000.0).round() as u64
}

/// The value of `key=` in `aterm ctl metrics`' line.
#[must_use]
pub fn metric(text: &str, key: &str) -> Option<f64> {
    text.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a non-negative metric in ms, rounded"
)]
fn ms_value(v: f64) -> u64 {
    v.max(0.0).round() as u64
}

/// The processes (`<name> <pid>` lines) whose pid changed or that went away between `before` and
/// `after`: what restarted.
#[must_use]
pub fn relaunched(before: &str, after: &str) -> Vec<String> {
    let pids = |text: &str| -> BTreeMap<String, String> {
        text.lines()
            .filter_map(|l| {
                let (name, pid) = l.trim().split_once(' ')?;
                Some((name.to_owned(), pid.trim().to_owned()))
            })
            .collect()
    };
    let after = pids(after);
    pids(before)
        .into_iter()
        .filter(|(name, pid)| after.get(name) != Some(pid))
        .map(|(name, _)| name)
        .collect()
}

/// The Dock's pinned apps, from `defaults read com.apple.dock persistent-apps`' `"file-label"`
/// lines.
#[must_use]
pub fn dock_labels(read: &str) -> Vec<String> {
    read.lines()
        .filter_map(|l| {
            let (_, value) = l.trim().split_once("\"file-label\" = ")?;
            let value = value.trim_end_matches(';').trim();
            Some(value.trim_matches('"').to_owned())
        })
        .collect()
}

/// The mDNS packets (tcpdump's lines) that ask after the ALab vault ([`ALAB_HOST`],
/// [`ALAB_SERVICE`]).
#[must_use]
pub fn alab_questions(tcpdump: &str) -> usize {
    tcpdump
        .lines()
        .filter(|l| l.contains("? ") && (l.contains(ALAB_HOST) || l.contains(ALAB_SERVICE)))
        .count()
}

/// tccd's consent dialogs (`AUTHREQ_PROMPTING: … service=<service>, …`), by service.
#[must_use]
pub fn tcc_prompts(log: &str) -> BTreeMap<String, usize> {
    let mut seen = BTreeMap::new();
    for line in log.lines() {
        let line = crate::mac::event_message(line);
        let Some((_, rest)) = line.split_once("AUTHREQ_PROMPTING:") else {
            continue;
        };
        let service = rest
            .split_once("service=")
            .map(|(_, s)| s.split([',', ' ', '"']).next().unwrap_or(""))
            .unwrap_or("?");
        let service = service.strip_prefix("kTCCService").unwrap_or(service);
        *seen.entry(service.to_owned()).or_insert(0) += 1;
    }
    seen
}

/// The `vMAJOR.MINOR.PATCH` a release URL (a `location:` header) names.
#[must_use]
pub fn release_tag(text: &str) -> Option<(u32, u32, u32)> {
    text.split('/').find_map(|part| {
        let mut nums = part.strip_prefix('v')?.trim().split('.');
        let v = (
            nums.next()?.parse().ok()?,
            nums.next()?.parse().ok()?,
            nums.next()?.parse().ok()?,
        );
        nums.next().is_none().then_some(v)
    })
}

/// The aterm.log messages that say what first-run setup did, by how each starts.
pub const SETUP_LINES: [&str; 6] = [
    "system state:",
    "System State:",
    "first-run macOS doctor",
    "setup",
    "full disk access",
    "macOS access card:",
];

/// What `first-launch` saw in aterm.log, each in ms after the open (`open_ms`, epoch ms).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Launch {
    pub first_log_ms: Option<u64>,
    pub warns: usize,
    pub errors: usize,
    /// The plan's submission log, not its admission receipt or completion.
    pub system_state_ms: Option<u64>,
    /// Each Full Disk Access hand-over and its answer (`queued`, `opened`, …), in order.
    pub handovers: Vec<(u64, String)>,
    /// The grant of Full Disk Access, as aterm observed it.
    pub fda_granted_ms: Option<u64>,
    /// Every setup line ([`SETUP_LINES`]), in order.
    pub setup: Vec<(u64, String)>,
    pub toolset: Option<(u64, String)>,
    pub first_run_doctor: Option<(u64, String)>,
}

/// [`Launch`] over the log's lines at or after `open_ms`.
#[must_use]
pub fn read_launch(log: &str, open_ms: u64) -> Launch {
    let lines: Vec<LogLine<'_>> = log_lines(log)
        .into_iter()
        .filter(|l| l.at_ms >= open_ms)
        .collect();
    let since = |l: &LogLine<'_>| l.at_ms - open_ms;
    let first = |p: &dyn Fn(&LogLine<'_>) -> bool| lines.iter().find(|l| p(l)).map(since);
    let last_text = |prefix: &str| {
        lines
            .iter()
            .rev()
            .find(|l| l.message.starts_with(prefix))
            .map(|l| (since(l), squeeze(l.message, 120)))
    };
    Launch {
        first_log_ms: lines.first().map(since),
        warns: lines.iter().filter(|l| l.level == "WARN").count(),
        errors: lines.iter().filter(|l| l.level == "ERROR").count(),
        system_state_ms: first(&|l| l.message.starts_with("system state: submitting a plan (")),
        handovers: lines
            .iter()
            .filter_map(|l| {
                let rest = l.message.strip_prefix("full disk access hand-over")?;
                let (_, answer) = rest.split_once("): ")?;
                Some((since(l), squeeze(answer, 60)))
            })
            .collect(),
        fda_granted_ms: first(&|l| l.message.contains("full disk access observed granted")),
        setup: lines
            .iter()
            .filter(|l| SETUP_LINES.iter().any(|p| l.message.starts_with(p)))
            .map(|l| (since(l), squeeze(l.message, 80)))
            .collect(),
        toolset: last_text("atpkg update pass finished")
            .or_else(|| last_text("atpkg install pass")),
        first_run_doctor: last_text("first-run macOS doctor:"),
    }
}

/// `text` on one line, cut to `max` characters: a control character, or the U+FFFD aterm.log
/// writes in its place (`aterm_log::sanitize_record`), is a space.
fn squeeze(text: &str, max: usize) -> String {
    let one: String = text
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{FFFD}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut cut: String = one.chars().take(max).collect();
    if one.chars().count() > max {
        cut.push('…');
    }
    cut
}

/// What a `--json` report (doctor's or init's) said: its rows, how many carry a time, the slowest
/// of them, and its phases.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReportJson {
    pub rows: usize,
    pub timed: usize,
    /// The three rows that took longest, slowest first: each id and its `elapsed_ms`.
    pub slowest: Vec<(String, u64)>,
    pub phases: BTreeMap<String, u64>,
}

/// [`ReportJson`] over the last line of `text` that parses as a report (a terminal's output holds
/// the prompts' lines too); `None` when none does.
#[must_use]
pub fn read_report_json(text: &str) -> Option<ReportJson> {
    text.lines().rev().find_map(|line| {
        let v: aterm_json::Value = aterm_json::from_str(line.trim()).ok()?;
        let items = v.get("items")?.as_array()?;
        let elapsed =
            |i: &aterm_json::Value| i.get("elapsed_ms").and_then(aterm_json::Value::as_u64);
        let mut slowest: Vec<(String, u64)> = items
            .iter()
            .filter_map(|i| Some((i.get("id")?.as_str()?.to_owned(), elapsed(i)?)))
            .collect();
        slowest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        slowest.truncate(3);
        let phases = v
            .get("phases")
            .and_then(aterm_json::Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        Some(ReportJson {
            rows: items.len(),
            timed: items.iter().filter(|i| elapsed(i).is_some()).count(),
            slowest,
            phases,
        })
    })
}

/// A run's `--json` report as cells under `label`.
fn report_cells(row: &mut Row, label: &str, report: Option<ReportJson>) {
    let Some(j) = report else {
        row.cell(
            format!("{label}rows"),
            Value::Missing("the report did not parse as JSON".into()),
        );
        return;
    };
    row.cell(format!("{label}rows"), Value::Count(j.rows as u64));
    row.cell(format!("{label}rows timed"), Value::Count(j.timed as u64));
    if !j.slowest.is_empty() {
        row.cell(
            format!("{label}slowest rows"),
            Value::Text(
                j.slowest
                    .iter()
                    .map(|(id, ms)| format!("{id} {ms} ms"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }
    row.cell(
        format!("{label}JSON phases (ms)"),
        if j.phases.is_empty() {
            Value::Missing("the report has no phases".into())
        } else {
            Value::Text(
                j.phases
                    .iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        },
    );
}

/// How many processes named `name` ended while process accounting ran, from `lastcomm`'s lines
/// (each starts with the command's name).
#[must_use]
pub fn exec_count(lastcomm: &str, name: &str) -> usize {
    lastcomm
        .lines()
        .filter(|l| l.split_whitespace().next() == Some(name))
        .count()
}

/// The nearest-rank `pct` percentile of `sorted` (ascending); `None` when it is empty.
#[must_use]
pub fn nearest_rank(sorted: &[u64], pct: usize) -> Option<u64> {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

/// The app a run puts in each guest: where it came from, and what it says it is.
struct App {
    how: String,
    version: String,
    id: String,
}

fn plist_value(app: &Path, key: &str) -> String {
    let out = crate::windows::run(
        Path::new("/usr/libexec/PlistBuddy"),
        &[
            "-c".into(),
            format!("Print :{key}"),
            app.join("Contents/Info.plist")
                .to_string_lossy()
                .into_owned(),
        ],
        None,
    );
    if out.ok() {
        out.text.trim().to_owned()
    } else {
        "?".to_owned()
    }
}

fn host_run(bin: &str, args: &[&str]) -> Result<(), Fail> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let out = crate::windows::run(Path::new(bin), &args, None);
    if out.ok() {
        Ok(())
    } else {
        Err(Fail::die(format!(
            "{bin} {}: {}",
            args.join(" "),
            out.tail(4)
        )))
    }
}

/// Build the app here and make it release-shaped: `ship dev-app` assembles `aterm (dev).app`
/// (no mark, signed ad hoc, not installed or linked); it is renamed `aterm.app`, given the
/// release's bundle id with no dev display name, and signed again ad hoc with the release's
/// hardened runtime and entitlements.
fn build_app(run: &Path) -> Result<PathBuf, Fail> {
    let out = run.join("app");
    println!("mac nux: building the app here (ship dev-app, made release-shaped)");
    let status = crate::util::targo(&[
        "--unverified",
        "ship",
        "dev-app",
        "--no-install",
        "--no-link",
        "--no-pin",
        "--sign-id",
        "-",
        "--out",
    ])
    .arg(&out)
    .stdin(Stdio::null())
    .status()
    .map_err(|e| Fail::die(format!("targo: {e}")))?;
    if !status.success() {
        return Err(Fail::die(format!("ship dev-app: {status}")));
    }
    let app = out.join("aterm.app");
    std::fs::rename(out.join("aterm (dev).app"), &app)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    let plist = app.join("Contents/Info.plist");
    let plist = plist.to_string_lossy();
    host_run(
        "/usr/libexec/PlistBuddy",
        &[
            "-c",
            "Set :CFBundleIdentifier com.aterm.aterm",
            "-c",
            "Delete :CFBundleDisplayName",
            &plist,
        ],
    )?;
    let entitlements = paths().repo.join("apps/aterm-mac/aterm.entitlements");
    let app_s = app.to_string_lossy();
    host_run(
        "/usr/bin/codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--options",
            "runtime",
            "--entitlements",
            &entitlements.to_string_lossy(),
            &app_s,
        ],
    )?;
    host_run("/usr/bin/codesign", &["--verify", "--strict", &app_s])?;
    Ok(app)
}

/// Put the app into the share as `aterm.zip` (the share does not carry a bundle's links): `--app`
/// read where it is, else built here.
fn stage_app(app: Option<&Path>, run: &Path, share: &Path) -> Result<App, Fail> {
    let (bundle, how) = match app {
        Some(p) => {
            if !p.join("Contents/Info.plist").is_file() {
                return Err(Fail::usage(format!(
                    "mac nux: --app {} is not an app bundle",
                    p.display()
                )));
            }
            (p.to_path_buf(), format!("--app {}", p.display()))
        }
        None => (build_app(run)?, "built here, release-shaped".to_owned()),
    };
    host_run(
        "/usr/bin/ditto",
        &[
            "-c",
            "-k",
            "--keepParent",
            &bundle.to_string_lossy(),
            &share.join("aterm.zip").to_string_lossy(),
        ],
    )?;
    Ok(App {
        how,
        version: plist_value(&bundle, "CFBundleShortVersionString"),
        id: plist_value(&bundle, "CFBundleIdentifier"),
    })
}

/// One row's guest: the clone, and where the app and its command are for the account it runs as.
struct Guest {
    vm: Vm,
    app: String,
    aterm: String,
    standard: bool,
}

impl Guest {
    fn sh(&self, script: &str, limit: Duration) -> Result<String, Fail> {
        self.vm.sh(script, limit)
    }

    /// A guest file's text ("" when it is absent).
    fn read(&self, path: &str) -> Result<String, Fail> {
        self.sh(&format!("cat '{path}' 2>/dev/null || true"), STEP)
    }

    /// Run `script`, whose exit is data: its stdout whatever the status, failing only when the
    /// guest could not run it.
    fn run(&self, script: &str, limit: Duration) -> Result<String, Fail> {
        let out = self.vm.exec(script, limit).map_err(Fail::die)?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `cmd` (a shell word list) under [`DRIVE`] on a terminal; its record and output under `tag`.
    fn drive(&self, tag: &str, env: &str, cmd: &str) -> Result<(Drive, String), Fail> {
        self.run(
            &format!(
                "cd {GUEST_NUX} && {env} /usr/bin/expect -f drive.exp {tag}.rec {} {cmd} > {tag}.out 2>&1",
                ROW.as_secs()
            ),
            ROW + STEP,
        )?;
        Ok((
            read_drive(&self.read(&format!("{GUEST_NUX}/{tag}.rec"))?),
            self.read(&format!("{GUEST_NUX}/{tag}.out"))?,
        ))
    }

    /// `cmd` under [`NOTTY`]; its record and output under `tag`.
    fn notty(&self, tag: &str, cmd: &str) -> Result<(Drive, String), Fail> {
        self.run(
            &format!(
                "cd {GUEST_NUX} && /usr/bin/perl notty.pl {tag}.rec {} {cmd} > {tag}.out 2>&1",
                ROW.as_secs()
            ),
            ROW + STEP,
        )?;
        Ok((
            read_drive(&self.read(&format!("{GUEST_NUX}/{tag}.rec"))?),
            self.read(&format!("{GUEST_NUX}/{tag}.out"))?,
        ))
    }

    /// The account's `aterm.log` ("" when it has none).
    fn log(&self) -> Result<String, Fail> {
        self.sh(
            "cat ~/Library/Logs/aterm/aterm.log 2>/dev/null || true",
            STEP,
        )
    }

    /// How many lines the account's `aterm.log` holds now, for [`appended`].
    fn log_mark(&self) -> Result<usize, Fail> {
        Ok(self.log()?.lines().count())
    }

    /// A run's phase cells under `label`: the `verb` summary line `aterm.log` gained past `mark`.
    fn phase_cells_since(
        &self,
        row: &mut Row,
        label: &str,
        mark: usize,
        verb: &str,
    ) -> Result<(), Fail> {
        phase_cells(row, label, &appended(&self.log()?, mark), verb);
        Ok(())
    }

    /// The pids of the processes init and the first launch restart (`<name> <pid>` lines).
    fn shell_pids(&self) -> Result<String, Fail> {
        self.sh(
            "for p in Dock Finder SystemUIServer; do printf '%s %s\\n' $p \"$(pgrep -x $p | head -1)\"; done",
            STEP,
        )
    }

    fn dock(&self) -> Result<Vec<String>, Fail> {
        Ok(dock_labels(&self.run(
            "defaults read com.apple.dock persistent-apps 2>/dev/null",
            STEP,
        )?))
    }

    /// `cmd` as root in the guest ([`root_command`]).
    fn as_root(&self, cmd: &str) -> String {
        root_command(self.standard, cmd)
    }

    fn sudo_password(&self) -> &'static str {
        if self.standard {
            STANDARD_PASSWORD
        } else {
            "admin"
        }
    }

    /// The records in the account's change journal (`aterm_converge::FileJournal`, its rotated
    /// file included).
    fn journal_records(&self) -> Result<u64, Fail> {
        let out = self.run(
            "cat \"$HOME/Library/Application Support/aterm/converge/journal.1.ndjson\" \
             \"$HOME/Library/Application Support/aterm/converge/journal.ndjson\" 2>/dev/null | grep -c . || true",
            STEP,
        )?;
        Ok(out.trim().parse().unwrap_or(0))
    }

    /// Turn on the guest's process accounting (`accton`) from an empty file: whether this macOS
    /// has it.
    fn accounting_start(&self) -> Result<bool, Fail> {
        let out = self.run(
            &self.as_root(
                "[ -x /usr/sbin/accton ] || { echo absent; exit 0; }; mkdir -p /var/account && \
                 : > /var/account/acct && /usr/sbin/accton /var/account/acct && echo on",
            ),
            STEP,
        )?;
        Ok(out.trim() == "on")
    }

    /// Turn process accounting off and read what ended while it ran (`lastcomm`'s lines).
    fn accounting_read(&self) -> Result<String, Fail> {
        self.run(
            &self.as_root(
                "/usr/sbin/accton; /usr/bin/lastcomm -f /var/account/acct 2>/dev/null || true",
            ),
            STEP,
        )
    }

    /// Whether System Settings runs.
    fn system_settings(&self) -> Result<bool, Fail> {
        Ok(self
            .run(
                "pgrep -x 'System Settings' >/dev/null && echo yes || echo no",
                STEP,
            )?
            .trim()
            == "yes")
    }
}

/// What a step changed in the account: the processes init and setup restart, the Dock's pins,
/// the change journal, and (where this macOS keeps process accounting) the `defaults` and
/// `killall` runs.
struct Around {
    pids: String,
    dock: Vec<String>,
    journal: u64,
    accounting: bool,
}

impl Around {
    /// The account before the step, process accounting started.
    fn take(g: &Guest) -> Result<Around, Fail> {
        Ok(Around {
            pids: g.shell_pids()?,
            dock: g.dock()?,
            journal: g.journal_records()?,
            accounting: g.accounting_start()?,
        })
    }

    /// What the step changed, as cells under `label`.
    fn cells(&self, g: &Guest, row: &mut Row, label: &str) -> Result<(), Fail> {
        let moved = relaunched(&self.pids, &g.shell_pids()?);
        row.cell(
            format!("{label}relaunched"),
            Value::Text(if moved.is_empty() {
                "none".to_owned()
            } else {
                moved.join(", ")
            }),
        );
        let pinned = g.dock()?;
        row.cell(
            format!("{label}Dock pins"),
            Value::Text(if pinned == self.dock {
                format!("{} unchanged", pinned.len())
            } else {
                format!(
                    "{} → {}",
                    self.dock.len(),
                    if pinned.is_empty() {
                        "none".to_owned()
                    } else {
                        pinned.join(", ")
                    }
                )
            }),
        );
        row.cell(
            format!("{label}journal records"),
            Value::Count(g.journal_records()?.saturating_sub(self.journal)),
        );
        let ended = if self.accounting {
            Some(g.accounting_read()?)
        } else {
            None
        };
        for name in ["defaults", "killall"] {
            row.cell(
                format!("{label}{name} runs"),
                ended.as_deref().map_or_else(
                    || Value::Missing("this macOS keeps no process accounting (accton)".into()),
                    |ended| Value::Count(exec_count(ended, name) as u64),
                ),
            );
        }
        Ok(())
    }
}

/// The standard account's key to the administrator on the guest itself, under its home
/// ([`standard_user_setup`]).
const ROOT_KEY: &str = ".ssh/aterm-nux-root";

/// `cmd` (a shell script) as root in the guest: the administrator's own passwordless `sudo`, or,
/// for the standard account, that `sudo` over ssh to the administrator on the guest itself with
/// the key its set-up made, so the account under test is given no root of its own.
fn root_command(standard: bool, cmd: &str) -> String {
    let sudo = format!("sudo -n /bin/sh -c {}", crate::util::shell_quote(cmd));
    if standard {
        format!(
            "ssh -n -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
             -o LogLevel=ERROR -i ~/{ROOT_KEY} admin@localhost {}",
            crate::util::shell_quote(&sudo)
        )
    } else {
        sudo
    }
}

/// What each of the account's shell prompts prints first ([`PROMPT_MARKER`]): the line [`DRIVE`]
/// waits for under `$NUX_SHELL`.
pub const SHELL_READY: &str = "nux-shell-ready";

/// The account's shell prompt marker: each zsh prompt appends `<epoch seconds> <pid>
/// <TERM_PROGRAM>` to `prompts` and prints [`SHELL_READY`].
const PROMPT_MARKER: &str =
    "mkdir -p /tmp/aterm-nux && : > /tmp/aterm-nux/prompts && cat >> ~/.zshrc <<'EOF'
zmodload zsh/datetime
nux_prompt() { print -r -- \"$EPOCHREALTIME $$ ${TERM_PROGRAM:-none}\" >> /tmp/aterm-nux/prompts; print -r -- nux-shell-ready }
precmd_functions+=(nux_prompt)
EOF";

/// The standard account's set-up, as the image's administrator: the account (`sysadminctl
/// -addUser`, in no admin group; it, like a `dscl` write of a user's `UniqueID`, writes the record
/// but never returns in the guest, before it sets the password: it runs in the background until the
/// record is there, and `dscl -passwd` sets the password), its home with Setup Assistant skipped,
/// Remote Login for it, a key the administrator's `authorized_keys` takes for the lane's own root
/// steps ([`root_command`]), the app in its `~/Applications`, and its
/// automatic login (`/etc/kcpassword` and loginwindow's `autoLoginUser`, as `sysadminctl
/// -autologin` writes them). The hung `sysadminctl` is killed last, so the shutdown that follows is
/// not held up by it.
fn standard_user_setup(zip: &str) -> String {
    let (u, pw) = (STANDARD_USER, STANDARD_PASSWORD);
    format!(
        "set -e; (sudo -n sysadminctl -addUser {u} -fullName 'NUX Standard' -password {pw} >/dev/null 2>&1 &); \
         n=0; until dscl . -read /Users/{u} GeneratedUID >/dev/null 2>&1; do \
         n=$((n+1)); [ $n -gt 600 ] && {{ echo 'sysadminctl made no account within 60 s'; exit 1; }}; sleep 0.1; done; \
         sudo -n dscl . -passwd /Users/{u} {pw}; \
         sudo -n createhomedir -c -u {u} >/dev/null; \
         sudo -n touch /Users/{u}/.skipbuddy; \
         sudo -n dseditgroup -o edit -a {u} -t user com.apple.access_ssh 2>/dev/null || true; \
         sudo -n mkdir -p /Users/{u}/.ssh; \
         sudo -n ssh-keygen -q -t ed25519 -N '' -C aterm-nux-root -f /Users/{u}/{ROOT_KEY}; \
         mkdir -p ~/.ssh; chmod 700 ~/.ssh; sudo -n cat /Users/{u}/{ROOT_KEY}.pub >> ~/.ssh/authorized_keys; \
         chmod 600 ~/.ssh/authorized_keys; \
         sudo -n mkdir -p /Users/{u}/Applications; \
         sudo -n ditto -x -k '{zip}' /Users/{u}/Applications; \
         sudo -n chown -R {u}:staff /Users/{u}; \
         /usr/bin/python3 -c \"{KCPASSWORD}\" {pw} | sudo -n tee /etc/kcpassword >/dev/null; \
         sudo -n chmod 600 /etc/kcpassword; \
         sudo -n defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser {u}; \
         sudo -n pkill -9 -x sysadminctl || true; sync"
    )
}

/// `/etc/kcpassword` for the password in `argv[1]`: NUL-terminated, padded to 12 bytes, XORed with
/// loginwindow's key.
const KCPASSWORD: &str = "import sys
pw = sys.argv[1].encode() + bytes(1)
pw += bytes((12 - len(pw) % 12) % 12)
key = [125, 137, 82, 35, 210, 188, 221, 234, 163, 185, 31]
sys.stdout.buffer.write(bytes(b ^ key[i % len(key)] for i, b in enumerate(pw)))";

/// A fresh clone for `scenario`, booted with SIP on, the drivers in place and the app installed
/// for the account the row runs as (its install time a cell of `row`).
fn guest(
    tart: &str,
    base: &str,
    run: &Path,
    share: &Path,
    ask: &Ask,
    scenario: Scenario,
    row: &mut Row,
) -> Result<Guest, Fail> {
    let mut vm = Vm::clone_from(tart, base, run, share, ask.keep)?;
    if matches!(scenario, Scenario::FirstLaunch | Scenario::InstallSh) {
        let status = aterm_uds::spawnfd::command(tart)
            .args(["set", &vm.name, "--disk-size", FIRST_LAUNCH_DISK_GB])
            .stdin(Stdio::null())
            .status()
            .map_err(|e| not_run(format!("tart set: {e}")))?;
        if !status.success() {
            return Err(not_run(format!("tart set --disk-size: {status}")));
        }
    }
    crate::mac::boot_sip_on(&mut vm, base)?;
    let zip = format!("{}/aterm.zip", crate::mac::GUEST_SHARE);
    let (app, standard) = if ask.standard_user {
        vm.sh(&standard_user_setup(&zip), STEP)?;
        vm.stop();
        vm.boot()?;
        let who = vm.sh("id -un", STEP)?;
        if who.trim() != STANDARD_USER {
            return Err(not_run(format!(
                "tart exec runs as {} after {STANDARD_USER}'s automatic login: the guest agent answers only for the image's own account",
                who.trim()
            )));
        }
        (
            format!("/Users/{STANDARD_USER}/Applications/aterm.app"),
            true,
        )
    } else {
        ("/Applications/aterm.app".to_owned(), false)
    };
    vm.sh(
        &format!(
            "rm -rf {GUEST_NUX} && mkdir -p {GUEST_NUX} && cp '{share}'/drive.exp '{share}'/notty.pl '{share}'/install.sh {GUEST_NUX}/",
            share = crate::mac::GUEST_SHARE
        ),
        STEP,
    )?;
    if scenario.needs_app() && !standard {
        let started = Instant::now();
        vm.sh(
            &format!("ditto -x -k '{zip}' /Applications && codesign --verify --strict {app}"),
            STEP,
        )?;
        row.cell("app unpacked", Value::Ms(millis(started.elapsed())));
    }
    if matches!(scenario, Scenario::FirstLaunch | Scenario::InstallSh) {
        let free = vm.sh("df -g / | awk 'NR==2 {print $4}'", STEP)?;
        row.cell(
            "guest disk free",
            Value::Text(format!("{} GiB", free.trim())),
        );
    }
    Ok(Guest {
        aterm: format!("{app}/Contents/MacOS/aterm"),
        app,
        vm,
        standard,
    })
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// `init-apply`: `aterm init --json` on a terminal with what it changed, then a second, no-op
/// `aterm init` and `aterm init --check`, each with its lines.
fn init_apply(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let sudo = format!("NUX_SUDO={}", g.sudo_password());
    let around = Around::take(g)?;
    let (d, out) = g.drive("init-apply", &sudo, &format!("{} init --json", g.aterm))?;
    drive_cells(row, "", &d);
    lines_cell(row, "", &out);
    report_cells(row, "", read_report_json(&out));
    around.cells(g, row, "")?;
    phase_cells(row, "", &g.log()?, "aterm init:");
    for (tag, args, label) in [
        ("init-noop", "init", "no-op: "),
        ("init-check", "init --check", "check: "),
    ] {
        let mark = g.log_mark()?;
        let (d, out) = g.drive(tag, &sudo, &format!("{} {args}", g.aterm))?;
        drive_cells(row, label, &d);
        lines_cell(row, label, &out);
        g.phase_cells_since(row, label, mark, "aterm init:")?;
    }
    Ok(())
}

/// Turn Remote Login on (the guest's administrator), and let the account ssh to itself with a
/// key made for it: NOT RUN when ssh does not answer.
fn ssh_to_itself(g: &Guest) -> Result<(), Fail> {
    g.run(
        &g.as_root(
            "launchctl print system/com.openssh.sshd >/dev/null 2>&1 || { \
             launchctl enable system/com.openssh.sshd; \
             launchctl bootstrap system /System/Library/LaunchDaemons/ssh.plist; }",
        ),
        STEP,
    )?;
    g.sh(
        &format!(
            "cd {GUEST_NUX} && ssh-keygen -q -t ed25519 -N '' -f key && mkdir -p ~/.ssh && cat key.pub >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && ssh -o BatchMode=yes -o StrictHostKeyChecking=no -i key localhost true"
        ),
        STEP,
    )
    .map_err(|e| {
        not_run(format!(
            "ssh to the guest itself did not answer: {}",
            e.msg.unwrap_or_default()
        ))
    })?;
    Ok(())
}

/// `init-ssh`: `aterm init` over `ssh -tt` to the guest itself, then a bare `aterm` the same way
/// to its shell's first prompt, each with what it changed.
fn init_ssh(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    ssh_to_itself(g)?;
    let ssh = format!(
        "ssh -tt -o BatchMode=yes -o StrictHostKeyChecking=no -i {GUEST_NUX}/key localhost {}",
        g.aterm
    );
    let around = Around::take(g)?;
    let (d, out) = g.drive(
        "init-ssh",
        &format!("NUX_SUDO={}", g.sudo_password()),
        &format!("{ssh} init"),
    )?;
    drive_cells(row, "", &d);
    lines_cell(row, "", &out);
    around.cells(g, row, "")?;
    phase_cells(row, "", &g.log()?, "aterm init:");
    g.sh(PROMPT_MARKER, STEP)?;
    let label = "bare aterm: ";
    let around = Around::take(g)?;
    let (d, out) = g.drive("ssh-aterm", "NUX_SHELL=1", &ssh)?;
    shell_cells(row, label, &d, &out);
    drive_cells(row, label, &d);
    around.cells(g, row, label)?;
    Ok(())
}

/// A shell session's cells under `label`: its first prompt (a missing one fails the row: the hang
/// detector fired) and the lines before it.
fn shell_cells(row: &mut Row, label: &str, d: &Drive, out: &str) {
    row.cell(
        format!("{label}to the shell prompt"),
        or_missing(
            d.shell_prompt_ms,
            Value::Ms,
            &format!("no shell prompt within {} s", ROW.as_secs()),
        ),
    );
    if d.shell_prompt_ms.is_none() {
        row.fail(format!("{label}no shell prompt"));
    }
    let before = out.split(SHELL_READY).next().unwrap_or("");
    lines_cell(row, &format!("{label}before it: "), before);
}

fn init_notty(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let around = Around::take(g)?;
    let (d, out) = g.notty("init-notty", &format!("{} init", g.aterm))?;
    drive_cells(row, "", &d);
    lines_cell(row, "", &out);
    around.cells(g, row, "")?;
    phase_cells(row, "", &g.log()?, "aterm init:");
    Ok(())
}

/// How many `aterm doctor --read-only --json` runs `doctor` times.
const READ_ONLY_RUNS: usize = 10;

/// `doctor`: on a terminal, then `--json | cat` with no terminal, each with whether System
/// Settings opened and the phases its own summary line names, then [`READ_ONLY_RUNS`] read-only
/// reports.
fn doctor(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let opened = |g: &Guest| -> Result<Value, Fail> {
        Ok(Value::Text(
            if g.system_settings()? { "yes" } else { "no" }.into(),
        ))
    };
    let label = "terminal: ";
    let mark = g.log_mark()?;
    let (d, out) = g.drive(
        "doctor-tty",
        &format!("NUX_SUDO={}", g.sudo_password()),
        &format!("{} doctor", g.aterm),
    )?;
    drive_cells(row, label, &d);
    lines_cell(row, label, &out);
    g.phase_cells_since(row, label, mark, "aterm doctor")?;
    row.cell(format!("{label}System Settings opened"), opened(g)?);
    g.run(
        "pkill -x 'System Settings'; n=0; while pgrep -x 'System Settings' >/dev/null; do \
         n=$((n+1)); [ $n -gt 300 ] && break; sleep 0.1; done",
        STEP,
    )?;
    let label = "--json | cat: ";
    let mark = g.log_mark()?;
    let (d, out) = g.notty(
        "doctor-json",
        &format!(
            "/bin/sh -c '\"$0\" doctor --json 2>>{GUEST_NUX}/doctor.err | cat' {}",
            g.aterm
        ),
    )?;
    drive_cells(row, label, &d);
    lines_cell(row, label, &out);
    g.phase_cells_since(row, label, mark, "aterm doctor")?;
    row.cell(format!("{label}System Settings opened"), opened(g)?);
    let label = "read-only: ";
    let mut ends = Vec::new();
    for at in 0..READ_ONLY_RUNS {
        let mark = g.log_mark()?;
        let (d, out) = g.notty(
            &format!("doctor-ro-{at}"),
            &format!(
                "/bin/sh -c 'exec \"$0\" doctor --read-only --json 2>>{GUEST_NUX}/doctor.err' {}",
                g.aterm
            ),
        )?;
        if at == 0 {
            drive_cells(row, label, &d);
            report_cells(row, label, read_report_json(&out));
            g.phase_cells_since(row, label, mark, "aterm doctor")?;
        }
        match d.end_ms {
            Some(end) if d.hang.is_none() => ends.push(end),
            _ => {
                if at > 0 {
                    drive_cells(row, &format!("read-only run {}: ", at + 1), &d);
                }
                break;
            }
        }
    }
    ends.sort_unstable();
    for pct in [50, 95] {
        row.cell(
            format!("{label}p{pct} of {}", ends.len()),
            or_missing(
                nearest_rank(&ends, pct),
                Value::Ms,
                "no read-only run ended",
            ),
        );
    }
    Ok(())
}

/// Open the app; its open time (epoch seconds) once a tab showed its first prompt, else `None`
/// within [`ROW`].
fn open_until_prompt(g: &Guest, tag: &str) -> Result<(f64, Option<u64>), Fail> {
    let out = g.sh(
        &format!(
            "/bin/zsh -c 'zmodload zsh/datetime; t=$EPOCHREALTIME; print -r -- $t > {GUEST_NUX}/{tag}.open; open {app}; \
             n=0; until grep -q \" aterm$\" {GUEST_NUX}/prompts 2>/dev/null && awk -v t=$t \"\\$1 >= t && \\$3 == \\\"aterm\\\" {{f=1}} END {{exit !f}}\" {GUEST_NUX}/prompts; do \
             (( n++ > {limit} )) && break; sleep 0.05; done; print -r -- $t'",
            app = g.app,
            limit = ROW.as_secs() * 20
        ),
        ROW + STEP,
    )?;
    let opened: f64 = out
        .trim()
        .lines()
        .last()
        .and_then(|l| l.trim().parse().ok())
        .ok_or_else(|| Fail::die(format!("the open time did not read back: {out}")))?;
    let prompts = g.read(&format!("{GUEST_NUX}/prompts"))?;
    Ok((opened, first_prompt_ms(&prompts, opened)))
}

fn metrics_cells(g: &Guest, row: &mut Row, label: &str) -> Result<(), Fail> {
    let metrics = g.run(&format!("{} ctl metrics 2>&1", g.aterm), STEP)?;
    row.cell(
        format!("{label}first present"),
        or_missing(
            metric(&metrics, "rust_main_to_first_present_ms").map(ms_value),
            Value::Ms,
            "aterm ctl metrics did not answer",
        ),
    );
    Ok(())
}

fn prompt_cell(row: &mut Row, label: &str, ms: Option<u64>) {
    row.cell(
        label.to_owned(),
        or_missing(
            ms,
            Value::Ms,
            &format!("no aterm tab showed a prompt within {} s", ROW.as_secs()),
        ),
    );
    if ms.is_none() {
        row.fail(format!(
            "the {} s hang detector fired: no prompt",
            ROW.as_secs()
        ));
    }
}

/// Stop the running app (TERM, then KILL after 30 s) and wait for it to be gone.
fn quit_app(g: &Guest) -> Result<(), Fail> {
    g.run(
        &format!(
            "p=$(pgrep -f '^{aterm}$' | head -1); [ -n \"$p\" ] || exit 0; kill -TERM $p; \
             n=0; while kill -0 $p 2>/dev/null; do n=$((n+1)); [ $n -gt 300 ] && kill -KILL $p; sleep 0.1; done",
            aterm = g.aterm
        ),
        STEP,
    )?;
    Ok(())
}

/// `terminal-launch`: a bare `aterm` on a terminal, as typed in Terminal.app (a shell session
/// behind aterm's pass-through), to its shell's first prompt, the account's first run and then
/// its second.
fn terminal_launch(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.sh(PROMPT_MARKER, STEP)?;
    for (tag, label) in [("terminal-cold", "cold: "), ("terminal-warm", "warm: ")] {
        let (d, out) = g.drive(tag, "NUX_SHELL=1", &g.aterm)?;
        shell_cells(row, label, &d, &out);
        drive_cells(row, label, &d);
    }
    Ok(())
}

/// `app-launch`: the app opened to its first tab's prompt, cold, then again once it quit.
fn app_launch(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.sh(PROMPT_MARKER, STEP)?;
    let (_, cold) = open_until_prompt(g, "cold")?;
    prompt_cell(row, "cold: open to prompt", cold);
    metrics_cells(g, row, "cold: ")?;
    quit_app(g)?;
    let (_, warm) = open_until_prompt(g, "warm")?;
    prompt_cell(row, "warm: open to prompt", warm);
    metrics_cells(g, row, "warm: ")?;
    Ok(())
}

fn first_launch(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.sh(PROMPT_MARKER, STEP)?;
    let around = Around::take(g)?;
    let mut stream = aterm_uds::spawnfd::command(&g.vm.tart)
        .args(["exec", &g.vm.name, "/bin/sh", "-c"])
        .arg(format!(
            "exec {}",
            g.as_root(&format!(
                "exec /usr/bin/log stream --info --style ndjson --predicate 'process == \"tccd\"' > {GUEST_NUX}/tccd.ndjson 2>&1"
            ))
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Fail::die(format!("tccd's log: {e}")))?;
    let started = Instant::now();
    while !g
        .vm
        .exec(&format!("test -s {GUEST_NUX}/tccd.ndjson"), STEP)
        .is_ok_and(|o| o.status.success())
    {
        if started.elapsed() >= STEP {
            return Err(Fail::die(format!(
                "tccd's log stream wrote nothing within {} s",
                STEP.as_secs()
            )));
        }
        stop::sleep(POLL)?;
    }
    let (opened, prompt) = open_until_prompt(g, "first")?;
    prompt_cell(row, "open to prompt", prompt);
    metrics_cells(g, row, "")?;
    let watched = Instant::now();
    while watched.elapsed() < WATCH {
        stop::sleep(Duration::from_secs(5).min(WATCH.saturating_sub(watched.elapsed())))?;
    }
    let desktop_read_passed = crate::mac::desktop_read(&g.vm, &format!("{GUEST_NUX}/tccd.ndjson"))?;
    let _ =
        g.vm.exec(&g.as_root("pkill -INT -f '^/usr/bin/log stream'"), STEP);
    crate::mac::reap_within(&mut stream, STEP);
    let tcc = g.sh(
        &format!(
            "grep -h 'AUTHREQ_PROMPTING\\|Handling access request' {GUEST_NUX}/tccd.ndjson || true"
        ),
        STEP,
    )?;
    let seen = crate::mac::tcc_seen(&tcc);
    let control = crate::mac::control_verdict(false, desktop_read_passed, &seen);
    let mut prompts = tcc_prompts(&tcc);
    if let Some(n) = prompts.get_mut("SystemPolicyDesktopFolder") {
        // The control's own ~/Desktop dialog is not the app's.
        *n = n.saturating_sub(usize::from(!desktop_read_passed));
    }
    prompts.retain(|_, n| *n > 0);
    row.cell(
        "consent dialogs",
        match control {
            Ok(_) if prompts.is_empty() => Value::Text("none".into()),
            Ok(_) => Value::Text(
                prompts
                    .iter()
                    .map(|(s, n)| format!("{s} {n}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Err(why) => Value::Missing(format!("control FAILED: {why}")),
        },
    );
    let open_ms = secs_to_ms(opened);
    let launch = read_launch(&g.log()?, open_ms);
    // A line the watch never saw is the app's silence over it, said with the watch's length.
    let within = format!("in the {} min watched", WATCH.as_secs() / 60);
    row.cell(
        "first log line",
        or_missing(
            launch.first_log_ms,
            Value::Ms,
            &format!("aterm.log has no line after the open {within}"),
        ),
    );
    row.cell("WARN lines", Value::Count(launch.warns as u64));
    row.cell("ERROR lines", Value::Count(launch.errors as u64));
    row.cell(
        "System State submission",
        or_missing(
            launch.system_state_ms,
            Value::Ms,
            &format!("no plan submission log line {within}"),
        ),
    );
    row.cell(
        "Full Disk Access hand-overs",
        if launch.handovers.is_empty() {
            Value::Missing(format!("no `full disk access hand-over` line {within}"))
        } else {
            Value::Text(timeline(&launch.handovers, usize::MAX))
        },
    );
    row.cell(
        "Full Disk Access granted",
        or_missing(
            launch.fda_granted_ms,
            Value::Ms,
            &format!("not granted {within} (nobody answers in the guest)"),
        ),
    );
    row.cell(
        "toolset pass",
        or_missing(
            launch.toolset,
            |(ms, text)| Value::Text(format!("{ms} ms: {text}")),
            &format!("no toolset pass line {within}"),
        ),
    );
    row.cell(
        "first-run doctor",
        or_missing(
            launch.first_run_doctor,
            |(ms, text)| Value::Text(format!("{ms} ms: {text}")),
            &format!("no `first-run macOS doctor:` line {within}"),
        ),
    );
    row.cell(
        "setup lines",
        if launch.setup.is_empty() {
            Value::Missing(format!("no setup line {within}"))
        } else {
            Value::Text(format!(
                "{}: {}",
                launch.setup.len(),
                timeline(&launch.setup, SETUP_SHOWN)
            ))
        },
    );
    around.cells(g, row, "")?;
    Ok(())
}

/// How many of the setup lines a first launch's row names.
const SETUP_SHOWN: usize = 8;

/// The first `max` of `lines` (each its ms and text) as one cell, and how many more there were.
fn timeline(lines: &[(u64, String)], max: usize) -> String {
    let mut text = lines
        .iter()
        .take(max)
        .map(|(ms, line)| format!("{ms} ms {line}"))
        .collect::<Vec<_>>()
        .join("; ");
    if lines.len() > max {
        let _ = write!(text, "; and {} more", lines.len() - max);
    }
    text
}

fn vault(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let seen = g.run(
        &format!(
            "/usr/bin/python3 -c \"import socket\ntry: print(socket.getaddrinfo('{ALAB_HOST}', 7484)[0][4][0])\nexcept Exception: print('none')\"; \
             script -q {GUEST_NUX}/browse.txt dns-sd -B {ALAB_SERVICE} >/dev/null 2>&1 & sleep 4; pkill -x dns-sd; sleep 0.5; \
             grep -c ' Add ' {GUEST_NUX}/browse.txt || true"
        ),
        STEP,
    )?;
    let mut lines = seen.lines().map(str::trim);
    let address = lines.next().unwrap_or("none");
    let adverts: u32 = lines.next_back().and_then(|n| n.parse().ok()).unwrap_or(0);
    if address != "none" || adverts > 0 {
        return Err(not_run(format!(
            "an ALab vault answers on the guest's network ({ALAB_HOST} {address}, {adverts} {ALAB_SERVICE} advert(s)): the lane never asks a real vault to admit a guest"
        )));
    }
    g.sh(
        &format!(
            // Its own output away from `tart exec`'s, which would otherwise wait on it.
            "({} </dev/null >/dev/null 2>&1 &) ; sleep 1",
            g.as_root(&format!(
                "exec /usr/sbin/tcpdump -l -n -i en0 udp port 5353 > {GUEST_NUX}/mdns.txt 2>/dev/null"
            ))
        ),
        STEP,
    )?;
    let (d, out) = g.drive(
        "vault",
        &format!("NUX_JOIN= NUX_SUDO={}", g.sudo_password()),
        &format!("{} init --only workstation", g.aterm),
    )?;
    let _ = g.vm.exec(
        &format!("{}; sleep 0.5", g.as_root("pkill -INT -x tcpdump")),
        STEP,
    );
    let mdns = g.read(&format!("{GUEST_NUX}/mdns.txt"))?;
    let asked = d.prompts.iter().find(|(_, _, text)| text.contains("vault"));
    row.cell(
        "to the vault prompt",
        or_missing(asked.map(|p| p.0), Value::Ms, "init asked for no vault"),
    );
    row.cell(
        "silent before it",
        or_missing(asked.map(|p| p.1), Value::Ms, "init asked for no vault"),
    );
    drive_cells(row, "", &d);
    let join = d
        .prompts
        .iter()
        .find(|(_, _, text)| text.contains("join password"));
    row.cell(
        "join password to the end",
        or_missing(
            join.zip(d.end_ms).map(|(p, end)| end.saturating_sub(p.0)),
            Value::Ms,
            "init asked for no join password",
        ),
    );
    row.cell(
        "mDNS questions for the ALab vault",
        Value::Count(alab_questions(&mdns) as u64),
    );
    let outcome = out
        .lines()
        .rev()
        .find(|l| l.contains("Secrets vault"))
        .map(|l| squeeze(l.trim(), 100));
    row.cell(
        "vault step",
        or_missing(
            outcome,
            Value::Text,
            "no `Secrets vault` row in init's output",
        ),
    );
    phase_cells(row, "", &g.log()?, "aterm init:");
    row.cell(
        "ALab join",
        Value::NotRun(
            "it needs the ALab vault's join password and its signer, which the guest has not (vault-local joins a vault of the guest's own)"
                .into(),
        ),
    );
    Ok(())
}

/// The port and name of the vault `vault-local` serves in the guest, on 127.0.0.1 only.
const LOCAL_VAULT_PORT: u16 = 7485;
const LOCAL_VAULT: &str = "nuxlab";
/// The entry its admin puts in `[everyone]`, whose file on disk is the joined Mac's "in".
const LOCAL_VAULT_ENTRY: &str = "NUX_PROBE";

/// The guest commands that make `vault-local`'s vault as a second identity (its own `$HOME`
/// under `admin`): a server on 127.0.0.1 that advertises nothing, the vault (the lane's entry in
/// `[everyone]`, a join password in `admin/password`) and its approver (`aterm secrets watch`);
/// they print the vault's signer, which the joining Mac pins.
fn local_vault_setup(aterm: &str, admin: &str) -> String {
    let url = format!("http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}");
    format!(
        "set -e; A={admin}; mkdir -p $A/seed; printf nux-probe-value > $A/seed/{LOCAL_VAULT_ENTRY}; export HOME=$A; \
         ({aterm} secrets serve --listen 127.0.0.1:{LOCAL_VAULT_PORT} --no-advertise --no-keep-awake --store $A/served </dev/null >$A/serve.log 2>&1 &); \
         n=0; until curl -s -o /dev/null --max-time 1 http://127.0.0.1:{LOCAL_VAULT_PORT}/; do \
         n=$((n+1)); [ $n -gt 600 ] && {{ echo 'the vault server did not answer within 60 s'; cat $A/serve.log; exit 1; }}; sleep 0.1; done; \
         {aterm} secrets create '{url}' --import $A/seed --no-agent --name nux-admin > $A/create.out 2>&1 || {{ cat $A/create.out; exit 1; }}; \
         {aterm} secrets password show > $A/password 2>$A/password.err || {{ cat $A/password.err; exit 1; }}; \
         ({aterm} secrets watch </dev/null >$A/watch.log 2>&1 &); \
         sed -n 's/.*add #\\([^ ]*\\) to the URL.*/\\1/p' $A/create.out"
    )
}

/// `vault-local`: the vault [`local_vault_setup`] makes, joined by `aterm init --only workstation
/// --vault <it>#<signer>` on a terminal with its join password typed.
fn vault_local(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let admin = format!("{GUEST_NUX}/vault-admin");
    let signer = g.sh(&local_vault_setup(&g.aterm, &admin), STEP)?;
    let signer = signer.trim();
    if signer.is_empty() || signer.contains(char::is_whitespace) {
        return Err(Fail::die(format!(
            "the local vault's signer did not read back: {}",
            g.read(&format!("{admin}/create.out"))?.trim()
        )));
    }
    let url = format!("http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}#{signer}");
    let (d, out) = g.drive(
        "vault-local",
        &format!(
            "NUX_JOIN=\"$(cat {admin}/password)\" NUX_SUDO={}",
            g.sudo_password()
        ),
        &format!("{} init --only workstation --vault '{url}'", g.aterm),
    )?;
    let typed = d
        .prompts
        .iter()
        .find(|(_, _, text)| text.contains("join password"));
    row.cell(
        "to the join password prompt",
        or_missing(
            typed.map(|p| p.0),
            Value::Ms,
            "init asked for no join password",
        ),
    );
    let written = g.run(
        &format!(
            "perl -MTime::HiRes=stat -e 'my @s = stat shift; print $s[9] if @s' ~/.secrets/{LOCAL_VAULT_ENTRY}"
        ),
        STEP,
    )?;
    let on_disk = written
        .trim()
        .parse::<f64>()
        .ok()
        .zip(d.start_epoch_ms)
        .zip(typed)
        .map(|((at, start), typed)| secs_to_ms(at).saturating_sub(start.saturating_add(typed.0)));
    row.cell(
        "typed to the secret on disk",
        or_missing(
            on_disk,
            Value::Ms,
            &format!("no ~/.secrets/{LOCAL_VAULT_ENTRY} after init"),
        ),
    );
    row.cell(
        "typed to the end",
        or_missing(
            typed.zip(d.end_ms).map(|(p, end)| end.saturating_sub(p.0)),
            Value::Ms,
            "init asked for no join password",
        ),
    );
    drive_cells(row, "", &d);
    row.cell(
        "vault step",
        or_missing(
            out.lines()
                .rev()
                .find(|l| l.contains("Secrets vault"))
                .map(|l| squeeze(l.trim(), 100)),
            Value::Text,
            "no `Secrets vault` row in init's output",
        ),
    );
    let approver = g.read(&format!("{admin}/watch.log"))?;
    if let Some(last) = approver.lines().rev().find(|l| !l.trim().is_empty()) {
        row.cell(
            "approver's last line",
            Value::Text(squeeze(last.trim(), 100)),
        );
    }
    phase_cells(row, "", &g.log()?, "aterm init:");
    Ok(())
}

/// The latest public release, read in the guest from the pointer install.sh reads.
fn latest_release(g: &Guest) -> Result<(u32, u32, u32), Fail> {
    let head = g.run(
        &format!("curl -fsSI --max-time 30 '{LATEST_POINTER}' | grep -i '^location:' || true"),
        STEP,
    )?;
    release_tag(&head).ok_or_else(|| {
        not_run(format!(
            "the guest could not read the public release pointer ({LATEST_POINTER}): {}",
            head.trim()
        ))
    })
}

fn installed_version(g: &Guest) -> Result<String, Fail> {
    let out = g.run(
        "for a in /Applications/aterm.app ~/Applications/aterm.app; do [ -d $a ] && /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' $a/Contents/Info.plist && break; done",
        STEP,
    )?;
    Ok(out.trim().to_owned())
}

/// install.sh, on a terminal (`install-sh`: a person at the console) or with none (`successor`,
/// which opens the app itself).
fn install_sh(
    g: &Guest,
    row: &mut Row,
    version: Option<&str>,
    on_terminal: bool,
) -> Result<(), Fail> {
    let args = version.map_or(String::new(), |v| format!(" --version {v}"));
    let cmd = format!("/bin/bash {GUEST_NUX}/install.sh{args}");
    let (d, out) = if on_terminal {
        g.drive("install-sh", "", &cmd)?
    } else {
        g.notty("install-sh", &cmd)?
    };
    drive_cells(row, "", &d);
    let installed = installed_version(g)?;
    row.cell(
        "installed",
        if installed.is_empty() {
            Value::Missing("no aterm.app after install.sh".into())
        } else {
            Value::Text(installed)
        },
    );
    if let Some(last) = out.lines().rev().find(|l| !l.trim().is_empty()) {
        row.cell("last line", Value::Text(squeeze(last.trim(), 100)));
    }
    if on_terminal {
        opened_cells(g, row, &d)?;
    }
    Ok(())
}

/// What a person finds once install.sh exits: the aterm process (looked for in the guest every
/// 100 ms, timed on the guest's clock from the exit), the typed steps left (one `open` when no
/// aterm runs), and the opened app's first log line and its toolset pass ([`toolset_pass`]).
fn opened_cells(g: &Guest, row: &mut Row, d: &Drive) -> Result<(), Fail> {
    let Some(exit_ms) = d
        .start_epoch_ms
        .zip(d.end_ms)
        .map(|(start, end)| start + end)
    else {
        row.cell(
            "aterm after the exit",
            Value::Missing("the record has no end".into()),
        );
        return Ok(());
    };
    let polls = OPENED_WATCH.as_millis() / 100;
    let seen = g.run(
        &format!(
            "i=0; until pgrep -f '/aterm\\.app/Contents/MacOS/aterm' >/dev/null; do \
             i=$((i+1)); [ $i -ge {polls} ] && exit 0; sleep 0.1; done; \
             perl -MTime::HiRes=time -e 'printf \"%d\\n\", time * 1000'"
        ),
        OPENED_WATCH + STEP,
    )?;
    let at = seen.trim().parse::<u64>().ok();
    row.cell(
        "aterm after the exit",
        or_missing(
            at.map(|at| at.saturating_sub(exit_ms)),
            Value::Ms,
            &format!(
                "no aterm process in the {} s after install.sh exited",
                OPENED_WATCH.as_secs()
            ),
        ),
    );
    row.cell("typed steps left", Value::Count(u64::from(at.is_none())));
    if at.is_none() {
        return Ok(());
    }
    let watched = Instant::now();
    let mut log = g.log()?;
    while toolset_pass(&log, exit_ms).1.is_none() && watched.elapsed() < TOOLSET_WATCH {
        stop::sleep(Duration::from_secs(5))?;
        log = g.log()?;
    }
    let within = format!("in the {} s watched", TOOLSET_WATCH.as_secs());
    row.cell(
        "first log line",
        or_missing(
            read_launch(&log, exit_ms).first_log_ms,
            Value::Ms,
            &format!("aterm.log has no line after the exit {within}"),
        ),
    );
    let (start, end) = toolset_pass(&log, exit_ms);
    let text = |(ms, text): (u64, String)| Value::Text(format!("{ms} ms: {text}"));
    row.cell(
        "toolset start",
        or_missing(
            start,
            text,
            &format!("no `atpkg install pass starting` line {within}"),
        ),
    );
    row.cell(
        "toolset end",
        or_missing(end, text, &format!("no end of the toolset pass {within}")),
    );
    Ok(())
}

/// The lines that end a toolset pass in `aterm.log`.
const TOOLSET_ENDS: [&str; 4] = [
    "atpkg seed done",
    "atpkg update pass finished",
    "atpkg install pass failed",
    "atpkg pass installed nothing",
];

/// A line found in `aterm.log`: its ms after a mark and its text.
pub type Found = Option<(u64, String)>;

/// The first toolset pass in `aterm.log` at or after `since_ms`: its start and its end
/// ([`TOOLSET_ENDS`]).
#[must_use]
pub fn toolset_pass(log: &str, since_ms: u64) -> (Found, Found) {
    let lines: Vec<LogLine<'_>> = log_lines(log)
        .into_iter()
        .filter(|l| l.at_ms >= since_ms)
        .collect();
    let first = |prefixes: &[&str]| {
        lines
            .iter()
            .find(|l| prefixes.iter().any(|p| l.message.starts_with(p)))
            .map(|l| (l.at_ms - since_ms, squeeze(l.message, 100)))
    };
    (
        first(&["atpkg install pass starting"]),
        first(&TOOLSET_ENDS),
    )
}

/// How long `successor` reads the updated app's log once it is in place: its startup, when a
/// successor would offer setup or read Full Disk Access.
const SUCCESSOR_WATCH: Duration = Duration::from_secs(30);

/// The guest's clock (epoch ms) and the installed app's version, read together.
fn installed_at(g: &Guest) -> Result<(Option<u64>, String), Fail> {
    let out = g.run(
        "perl -MTime::HiRes=time -e 'printf \"%d\\n\", time * 1000'; \
         for a in /Applications/aterm.app ~/Applications/aterm.app; do [ -d $a ] && /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' $a/Contents/Info.plist && break; done",
        STEP,
    )?;
    let mut lines = out.lines().map(str::trim);
    let at = lines.next().and_then(|l| l.parse().ok());
    Ok((at, lines.next().unwrap_or("").to_owned()))
}

fn successor(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let (major, minor, _) = latest_release(g)?;
    let Some(before) = minor.checked_sub(1) else {
        return Err(not_run(format!(
            "the latest release is {major}.0.0: no release N before it"
        )));
    };
    let from = format!("{major}.{before}.0");
    let to = format!("{major}.{minor}.0");
    install_sh(g, row, Some(&from), false)?;
    if installed_version(g)? != from {
        row.fail(format!(
            "install.sh --version {from} did not install {from}"
        ));
        return Ok(());
    }
    let app = if g.standard {
        format!("/Users/{STANDARD_USER}/Applications/aterm.app")
    } else {
        "/Applications/aterm.app".to_owned()
    };
    let started = Instant::now();
    g.sh(&format!("open {app}"), STEP)?;
    let mut updated = None;
    // The guest's clock at the last look that still found N: the successor's lines come after.
    let mut still_n = None;
    while started.elapsed() < ROW {
        stop::sleep(Duration::from_secs(2))?;
        let (at, version) = installed_at(g)?;
        if version == to {
            updated = Some(millis(started.elapsed()));
            break;
        }
        still_n = at.or(still_n);
    }
    row.cell("from", Value::Text(from));
    row.cell(
        format!("open to {to} installed"),
        or_missing(
            updated,
            Value::Ms,
            &format!("still not {to} after {} s", ROW.as_secs()),
        ),
    );
    if updated.is_none() {
        row.fail(format!(
            "the {} s hang detector fired: the updater did not install {to}",
            ROW.as_secs()
        ));
        return Ok(());
    }
    let Some(since) = still_n else {
        row.cell(
            "successor's lines",
            Value::Missing("the guest's clock did not read back before the update".into()),
        );
        return Ok(());
    };
    stop::sleep(SUCCESSOR_WATCH)?;
    let launch = read_launch(&g.log()?, since);
    let (fda, setup): (Vec<_>, Vec<_>) = launch.setup.into_iter().partition(|(_, line)| {
        line.starts_with("full disk access") || line.starts_with("macOS access card")
    });
    let within = SUCCESSOR_WATCH.as_secs();
    for (name, lines) in [("setup lines", setup), ("Full Disk Access lines", fda)] {
        row.cell(
            format!("successor's {name}"),
            Value::Text(if lines.is_empty() {
                format!("none in its first {within} s")
            } else {
                format!("{}: {}", lines.len(), timeline(&lines, SETUP_SHOWN))
            }),
        );
    }
    Ok(())
}

fn run_row(
    tart: &str,
    base: &str,
    run: &Path,
    share: &Path,
    ask: &Ask,
    scenario: Scenario,
) -> Result<Row, Fail> {
    let mut row = Row::new(scenario);
    let g = guest(tart, base, run, share, ask, scenario, &mut row)?;
    match scenario {
        Scenario::InitApply => init_apply(&g, &mut row)?,
        Scenario::InitSsh => init_ssh(&g, &mut row)?,
        Scenario::InitNotty => init_notty(&g, &mut row)?,
        Scenario::Doctor => doctor(&g, &mut row)?,
        Scenario::FirstLaunch => first_launch(&g, &mut row)?,
        Scenario::TerminalLaunch => terminal_launch(&g, &mut row)?,
        Scenario::AppLaunch => app_launch(&g, &mut row)?,
        Scenario::Vault => vault(&g, &mut row)?,
        Scenario::VaultLocal => vault_local(&g, &mut row)?,
        Scenario::Successor => successor(&g, &mut row)?,
        Scenario::InstallSh => install_sh(&g, &mut row, None, true)?,
    }
    drop(g);
    Ok(row)
}

/// Append `text` to `file`, made with its parent when absent.
fn record(file: &Path, text: &str) -> Result<(), Fail> {
    use std::io::Write as _;
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    f.write_all(text.as_bytes())
        .map_err(|e| Fail::die(format!("{}: {e}", file.display())))
}

/// `mac nux`: the lane (the module docs).
///
/// # Errors
/// Usage, or the lane could not run.
pub fn cmd_nux(args: &[String]) -> Res {
    stop::install();
    let image = std::env::var("LAB_MAC_IMAGE")
        .ok()
        .filter(|i| !i.is_empty())
        .unwrap_or_else(|| crate::mac::IMAGE.to_owned());
    let ask = parse(args, image)?;
    let tart = crate::mac::tart_to_run()?;
    crate::mac::report_ended_lanes(&tart);
    let runs = paths().state.join("mac-nux");
    std::fs::create_dir_all(&runs).map_err(|e| format!("{}: {e}", runs.display()))?;
    let run_guard = aterm_tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&runs)
        .map_err(|e| format!("{}: {e}", runs.display()))?;
    let run = run_guard.path();
    let share = run.join("share");
    std::fs::create_dir(&share).map_err(|e| format!("{}: {e}", share.display()))?;
    for (name, text) in [("drive.exp", DRIVE), ("notty.pl", NOTTY)] {
        std::fs::write(share.join(name), text).map_err(|e| format!("{name}: {e}"))?;
    }
    let install = paths().repo.join("tools/install.sh");
    std::fs::copy(&install, share.join("install.sh"))
        .map_err(|e| format!("{}: {e}", install.display()))?;
    let app = if ask.scenarios.iter().any(|s| s.needs_app()) {
        Some(stage_app(ask.app.as_deref(), run, &share)?)
    } else {
        None
    };
    let base = crate::mac::prepared_base(&tart, &ask.image, false, run, &share, &mut || Ok(()))?;
    let mut rows = Vec::new();
    for &scenario in &ask.scenarios {
        stop::check()?;
        println!("mac nux: {} in a fresh clone of {base}", scenario.name());
        let row = match run_row(&tart, &base, run, &share, &ask, scenario) {
            Ok(row) => row,
            Err(fail) if fail.stopped.is_some() => return Err(fail),
            Err(fail) => Row::refused(scenario, &fail),
        };
        println!("{}", table("", std::slice::from_ref(&row)).trim());
        rows.push(row);
    }
    let head = format!(
        "### lab mac nux, {} ({}{})\n\nGuest: a clone of {} per row (SIP on, no window). App: {}.",
        {
            let now = crate::util::now_secs();
            format!("{} {}", crate::util::ymd(now), crate::util::hms(now))
        },
        if ask.standard_user {
            "standard user"
        } else {
            "the image's administrator"
        },
        if ask.keep { ", clones kept" } else { "" },
        ask.image,
        app.as_ref().map_or_else(
            || "a public release (install.sh)".to_owned(),
            |a| format!("{} {} ({})", a.id, a.version, a.how)
        ),
    );
    let text = table(&head, &rows);
    println!("\n{text}");
    if let Some(file) = &ask.record {
        record(file, &format!("\n{text}"))?;
        println!("mac nux: appended to {}", file.display());
    }
    Ok(exit_code(&rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_parse_by_name_in_run_order_each_once() {
        assert_eq!(
            parse_scenarios("doctor,init-apply,doctor").unwrap(),
            [Scenario::InitApply, Scenario::Doctor]
        );
        assert_eq!(parse_scenarios("all").unwrap(), ALL);
        assert_eq!(parse_scenarios("vault, all").unwrap(), ALL);
        assert!(
            parse_scenarios("init")
                .unwrap_err()
                .contains("no row 'init'")
        );
        assert!(parse_scenarios("").is_err());
        for s in ALL {
            assert_eq!(parse_scenarios(s.name()).unwrap(), [s]);
        }
    }

    #[test]
    fn the_arguments_take_every_flag_and_refuse_a_stray_word() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let ask = parse(&args(&[]), "img".into()).unwrap();
        assert_eq!(ask.scenarios, ALL);
        assert_eq!(ask.image, "img");
        assert!(!ask.standard_user && !ask.keep);
        assert_eq!((ask.app, ask.record), (None, None));
        let ask = parse(
            &args(&[
                "--app",
                "/x/aterm.app",
                "--scenario",
                "vault,doctor",
                "--standard-user",
                "--keep",
                "--record",
                "docs/measured/n.md",
                "--image",
                "other",
            ]),
            "img".into(),
        )
        .unwrap();
        assert_eq!(ask.app.as_deref(), Some(Path::new("/x/aterm.app")));
        assert_eq!(ask.scenarios, [Scenario::Doctor, Scenario::Vault]);
        assert!(ask.standard_user && ask.keep);
        // A relative path is the caller's, never the lab's own `/`.
        assert_eq!(
            ask.record,
            Some(PathBuf::from(format!(
                "{}/docs/measured/n.md",
                crate::paths::caller_dir().trim_end_matches('/')
            )))
        );
        assert_eq!(ask.image, "other");
        assert_eq!(parse(&args(&["stray"]), "i".into()).unwrap_err().code, 2);
        assert_eq!(
            parse(&args(&["--scenario", "nope"]), "i".into())
                .unwrap_err()
                .code,
            2
        );
        // Only the rows that run the app need it built or handed over.
        let needing: Vec<_> = ALL.into_iter().filter(|s| !s.needs_app()).collect();
        assert_eq!(needing, [Scenario::Successor, Scenario::InstallSh]);
    }

    #[test]
    fn a_drivers_record_reads_back_whole() {
        let record = "start_epoch_ms 1792950876218\nfirst_byte_ms 435\n\
                      prompt 8550 7000 Secrets vault URL or path (Enter keeps http://x, none skips):\n\
                      prompt 8551 1 Vault join password\nend_ms 18197\nprompts 2\n\
                      quiet_waited_ms 1500\nexit 1\n";
        let d = read_drive(record);
        assert_eq!(d.start_epoch_ms, Some(1_792_950_876_218));
        assert_eq!(d.first_byte_ms, Some(435));
        assert_eq!(d.end_ms, Some(18197));
        assert_eq!(d.prompts.len(), 2);
        assert_eq!(d.prompts[0].1, 7000);
        assert_eq!(d.prompts[1], (8551, 1, "Vault join password".to_owned()));
        assert_eq!(d.quiet_waited_ms, Some(1500));
        assert_eq!(d.exit, Some(1));
        assert_eq!(d.hang, None);
        let mut row = Row::new(Scenario::InitApply);
        drive_cells(&mut row, "", &d);
        assert_eq!(row.outcome, Outcome::Measured);

        // A hang fails the row; a record cut before its end measures nothing there, never 0.
        let hung = read_drive("start_epoch_ms 1\nfirst_byte_ms none\nhang first_byte\n");
        assert_eq!(hung.first_byte_ms, None);
        let mut row = Row::new(Scenario::InitApply);
        drive_cells(&mut row, "", &hung);
        assert!(matches!(row.outcome, Outcome::Failed(_)));
        let shown = table("", &[row]);
        assert!(shown.contains("first byte NOT MEASURED"), "{shown}");
        assert!(shown.contains("end NOT MEASURED"), "{shown}");
        assert!(!shown.contains("end 0 ms"), "{shown}");
    }

    /// Lines as aterm writes them (2026-10-25, a macOS 27 guest), the phase line as LAB-1's.
    const LOG: &str = "1792950909.704 1322 INFO aterm_gui: GPU rendering on Apple Paravirtual device (Metal)\n\
        1792950919.160 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n\
        1792950920.360 1322 WARN aterm: first-run macOS doctor: macOS workstation profile (apply)\u{FFFD}  Local Network: ok\n\
        1792950943.008 1322 INFO aterm_gui: atpkg update pass finished (the store's record owes this launch a pass): exit=failed (next try in 10 min)\n\
        1792950953.369 1322 INFO aterm_gui: full disk access hand-over (automatic): opened\n\
        1792950960.5 1400 INFO aterm_machines::init: aterm init: 41 rows, exit 1; context 120 ms, questions 3 ms, base 2100 ms, workstation 6400 ms, total 8623 ms\n\
        not a log line\n";

    #[test]
    fn log_lines_parse_and_phases_come_from_the_last_summary_line() {
        let lines = log_lines(LOG);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0].at_ms, 1_792_950_909_704);
        assert_eq!(lines[5].at_ms, 1_792_950_960_500);
        assert_eq!(lines[2].level, "WARN");
        assert_eq!(lines[1].target, "aterm_gui::system_state_host");
        let phases = run_phases(&lines, "aterm init:").unwrap();
        assert_eq!(phases["base"], 2100);
        assert_eq!(phases["total"], 8623);
        assert_eq!(phases.len(), 5);
        let mut row = Row::new(Scenario::InitNotty);
        phase_cells(&mut row, "", LOG, "aterm init:");
        assert_eq!(
            row.cells[0].1,
            Value::Text("base 2100, context 120, questions 3, total 8623, workstation 6400".into())
        );
    }

    #[test]
    fn a_runs_phases_come_from_the_line_it_appended_never_an_earlier_runs() {
        let mark = LOG.lines().count();
        // NEGATIVE CONTROL: a run that wrote no summary line reads as NOT MEASURED, not as the
        // line the run before it left.
        let mut row = Row::new(Scenario::InitApply);
        phase_cells(&mut row, "check: ", &appended(LOG, mark), "aterm init:");
        assert_eq!(row.cells[0].0, "check: phases");
        assert!(matches!(row.cells[0].1, Value::Missing(_)));
        let grown = format!(
            "{LOG}1792950970.000 1400 INFO aterm: aterm init: 39 rows, exit 0; context 5 ms, total 9 ms\n"
        );
        let mut row = Row::new(Scenario::InitApply);
        phase_cells(&mut row, "no-op: ", &appended(&grown, mark), "aterm init:");
        assert_eq!(row.cells[0].0, "no-op: phases (ms)");
        assert_eq!(row.cells[0].1, Value::Text("context 5, total 9".into()));
        // A log that rotated meanwhile (fewer lines than the mark) reads as empty.
        assert_eq!(appended("1.0 1 INFO aterm: x\n", mark), "");
    }

    #[test]
    fn a_log_without_the_line_is_not_measured_never_zero() {
        // NEGATIVE CONTROL: the same log without its summary line (a build before LAB-1).
        let without: String = LOG
            .lines()
            .filter(|l| !l.contains("aterm init:"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(run_phases(&log_lines(&without), "aterm init:"), None);
        let mut row = Row::new(Scenario::InitNotty);
        phase_cells(&mut row, "", &without, "aterm init:");
        assert!(matches!(row.cells[0].1, Value::Missing(_)));
        let shown = table("", &[row]);
        assert!(shown.contains("phases NOT MEASURED"), "{shown}");
        assert!(!shown.contains(" 0"), "{shown}");
        // A doctor run's line is not init's.
        assert_eq!(
            run_phases(
                &log_lines(
                    "1.0 1 INFO aterm: aterm doctor: 3 rows, exit 0; gather 5 ms, total 5 ms\n"
                ),
                "aterm init:"
            ),
            None
        );
        // A launch with no such lines measures none of them.
        let launch = read_launch(
            "1792950909.704 1322 INFO aterm_gui: GPU\n",
            1_792_950_909_000,
        );
        assert_eq!(launch.system_state_ms, None);
        assert_eq!(launch.toolset, None);
        assert_eq!(launch.first_log_ms, Some(704));
    }

    #[test]
    fn first_launch_plan_ignores_an_observation_that_did_not_start() {
        let log = "1792950909.704 1322 INFO aterm_gui::system_state_host: system state: the observation did not start\n";
        let launch = read_launch(log, 1_792_950_909_000);
        assert_eq!(launch.first_log_ms, Some(704));
        assert_eq!(launch.system_state_ms, None);
        // The diagnostic remains part of the setup timeline, without becoming a submission.
        assert_eq!(launch.setup.len(), 1);
    }

    #[test]
    fn first_launch_plan_measures_an_actual_submission() {
        let log = "1792950919.160 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n";
        let launch = read_launch(log, 1_792_950_909_538);
        assert_eq!(launch.system_state_ms, Some(9622));
        assert_eq!(launch.fda_granted_ms, None);
    }

    #[test]
    fn first_launch_plan_uses_the_first_submission_after_open_despite_other_setup_lines() {
        let log = "1792950908.900 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n\
            1792950909.704 1322 INFO aterm_gui::system_state_host: system state: the observation did not start\n\
            1792950911.000 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n\
            1792950913.000 1322 INFO aterm_gui::system_state_host: system state: the look for a renewed GitHub token did not start\n\
            1792950919.160 1322 INFO aterm_gui::system_state_host: system state: submitting a plan (Automatic)\n";
        let launch = read_launch(log, 1_792_950_909_000);
        assert_eq!(launch.system_state_ms, Some(2000));
        assert_eq!(
            launch.setup.iter().map(|(ms, _)| *ms).collect::<Vec<_>>(),
            [704, 2000, 4000, 10160]
        );
    }

    #[test]
    fn a_first_launch_reads_each_milestone_after_the_open() {
        let open = 1_792_950_909_538;
        let launch = read_launch(LOG, open);
        assert_eq!(launch.first_log_ms, Some(166));
        assert_eq!(launch.warns, 1);
        assert_eq!(launch.errors, 0);
        assert_eq!(launch.system_state_ms, Some(9622));
        assert_eq!(launch.handovers, [(43831, "opened".to_owned())]);
        assert_eq!(launch.fda_granted_ms, None);
        // Every setup line, in order: the plan, the first-run doctor and the hand-over.
        assert_eq!(
            launch.setup.iter().map(|(ms, _)| *ms).collect::<Vec<_>>(),
            [9622, 10822, 43831]
        );
        assert_eq!(launch.toolset.as_ref().map(|t| t.0), Some(33470));
        assert!(launch.toolset.unwrap().1.contains("exit=failed"));
        let doctor = launch.first_run_doctor.unwrap();
        assert_eq!(doctor.0, 10822);
        // The record's newline, which aterm.log writes as U+FFFD, reads as a space.
        assert!(
            doctor.1.contains("(apply)   Local Network: ok"),
            "{}",
            doctor.1
        );
        // Lines before the open belong to another launch.
        assert_eq!(read_launch(LOG, 1_792_950_960_000).warns, 0);
    }

    #[test]
    fn a_toolset_pass_reads_its_start_and_its_end_after_the_exit() {
        let log = "1792950900.000 1300 INFO aterm_gui: atpkg seed done: an earlier launch's\n\
            1792950901.200 1322 INFO aterm_gui: GPU rendering on Apple Paravirtual device (Metal)\n\
            1792950902.500 1322 INFO aterm_gui: atpkg install pass starting (first run): installing 15 program(s)\n\
            1792950931.000 1322 INFO aterm_gui: atpkg seed done: 15 installed\n\
            1792950940.000 1322 INFO aterm_gui: atpkg update pass finished: exit=ok\n";
        let (start, end) = toolset_pass(log, 1_792_950_901_000);
        assert_eq!(start.map(|s| s.0), Some(1500));
        assert_eq!(end.as_ref().map(|e| e.0), Some(30_000));
        assert!(end.unwrap().1.starts_with("atpkg seed done: 15"));
        // A pass still running has a start and no end; lines before the exit are another run's.
        let running = log.lines().take(3).collect::<Vec<_>>().join("\n");
        assert_eq!(toolset_pass(&running, 1_792_950_901_000).1, None);
        assert_eq!(toolset_pass(log, 1_792_950_950_000), (None, None));
    }

    #[test]
    fn a_prompt_counts_from_an_aterm_tab_after_the_open() {
        let prompts = "1792950905.000 900 none\n1792950911.3212769 1329 aterm\n\
                       1792950912.5 1330 aterm\nbad line\n";
        assert_eq!(first_prompt_ms(prompts, 1_792_950_909.538_574), Some(1783));
        assert_eq!(first_prompt_ms(prompts, 1_792_950_913.0), None);
        assert_eq!(first_prompt_ms("", 0.0), None);
    }

    #[test]
    fn metrics_relaunches_dock_pins_mdns_and_consent_read_from_their_tools() {
        let m = "OK backend=gpu rust_main_to_first_present_ms=1314.64 first_present_ms=1312.79";
        assert_eq!(metric(m, "rust_main_to_first_present_ms"), Some(1314.64));
        assert_eq!(metric(m, "first_present_ms"), Some(1312.79));
        assert_eq!(metric(m, "present_ms"), None);
        assert_eq!(metric("ERR no window", "first_present_ms"), None);

        let before = "Dock 394\nFinder 401\nSystemUIServer 410\n";
        let after = "Dock 1500\nFinder 401\nSystemUIServer \n";
        assert_eq!(relaunched(before, after), ["Dock", "SystemUIServer"]);
        assert_eq!(relaunched(before, before), Vec::<String>::new());

        let dock = "(\n    {\n        \"tile-data\" = {\n            \"file-label\" = aterm;\n\
                    \n            \"file-label\" = \"System Settings\";\n            \"file-label\" = Safari;\n";
        assert_eq!(dock_labels(dock), ["aterm", "System Settings", "Safari"]);

        let tcpdump = "10:00:00 IP 192.168.64.5.5353 > 224.0.0.251.5353: 0 PTR (QM)? _alab-vault._tcp.local. (40)\n\
                       10:00:01 IP 192.168.64.5.5353 > 224.0.0.251.5353: 0 A (QM)? alab-secrets.local. (36)\n\
                       10:00:01 IP 192.168.64.5.5353 > 224.0.0.251.5353: 0 PTR (QM)? _ssh._tcp.local. (32)\n\
                       10:00:02 IP 192.168.64.1.5353 > 224.0.0.251.5353: 0*- [0q] 1/0/0 PTR m106._ssh._tcp.local. (60)\n";
        assert_eq!(alab_questions(tcpdump), 2);
        assert_eq!(alab_questions(""), 0);

        // macOS 27 writes the format string ahead of the message: its `%{public}@` is no service.
        let tcc = r#"{"formatString":"AUTHREQ_PROMPTING: msgID=%{public}@, service=%{public}@, subject=%{public}@, ","eventMessage":"AUTHREQ_PROMPTING: msgID=158.53, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{x}"}
{"eventMessage":"AUTHREQ_PROMPTING: msgID=9.1, service=kTCCServiceAccessibility, subject=Sub:{com.aterm.aterm}"}
{"eventMessage":"AUTHREQ_PROMPTING: msgID=9.2, service=kTCCServiceAccessibility, subject=Sub:{com.aterm.aterm}"}
{"eventMessage":"AUTHREQ_CTX: msgID=9.3, service=kTCCServiceAppleEvents"}"#;
        let seen = tcc_prompts(tcc);
        assert_eq!(seen["Accessibility"], 2);
        assert_eq!(seen["SystemPolicyDesktopFolder"], 1);
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn a_root_step_is_the_administrators_sudo_and_never_the_standard_accounts_own() {
        let cmd = "pkill -INT -f '^/usr/bin/log stream'";
        assert_eq!(
            root_command(false, cmd),
            r"sudo -n /bin/sh -c 'pkill -INT -f '\''^/usr/bin/log stream'\'''"
        );
        let standard = root_command(true, cmd);
        // The standard account reaches the administrator's sudo over ssh, the whole sudo line one
        // word, so its own quotes survive the second shell.
        let (ssh, word) = standard.split_once(" admin@localhost ").unwrap();
        assert!(ssh.starts_with("ssh -n -o BatchMode=yes"), "{ssh}");
        assert!(ssh.ends_with(&format!("-i ~/{ROOT_KEY}")), "{ssh}");
        assert_eq!(
            crate::util::shell_unquote(word),
            root_command(false, cmd),
            "{standard}"
        );
    }

    #[test]
    fn a_release_is_named_by_its_tag_and_a_report_by_its_json() {
        assert_eq!(
            release_tag(
                "location: https://github.com/alabsystems/aterm/releases/download/v0.138.0/aterm-appcast.toml\r\n"
            ),
            Some((0, 138, 0))
        );
        assert_eq!(
            release_tag("location: https://x/download/latest/a.toml"),
            None
        );
        assert_eq!(release_tag("v1.2"), None);
        assert_eq!(release_tag(""), None);

        let json = r#"{"schema":1,"items":[{"id":"a","elapsed_ms":4},{"id":"b"}],"phases":{"gather":10,"lock":2}}"#;
        let j = read_report_json(json).unwrap();
        assert_eq!((j.rows, j.timed), (2, 1));
        assert_eq!(j.phases["gather"], 10);
        assert_eq!(j.slowest, [("a".to_owned(), 4)]);
        // A report from before rows were timed: rows, no time, no phases.
        let old = read_report_json(r#"{"schema":1,"items":[{"id":"a"}]}"#).unwrap();
        assert_eq!((old.rows, old.timed, old.phases.len()), (1, 0, 0));
        assert!(old.slowest.is_empty());
        assert_eq!(read_report_json("not json"), None);
        // On a terminal the report follows the prompts' lines; the slowest rows come first.
        let tty = "Secrets vault URL or path (none skips): none\r\n\
                   {\"items\":[{\"id\":\"a\",\"elapsed_ms\":4},{\"id\":\"b\",\"elapsed_ms\":90},\
                   {\"id\":\"c\",\"elapsed_ms\":7},{\"id\":\"d\",\"elapsed_ms\":7}]}\r\n";
        let j = read_report_json(tty).unwrap();
        assert_eq!((j.rows, j.timed), (4, 4));
        assert_eq!(
            j.slowest,
            [
                ("b".to_owned(), 90),
                ("c".to_owned(), 7),
                ("d".to_owned(), 7)
            ]
        );
    }

    #[test]
    fn a_refused_step_is_not_run_and_a_failed_one_fails_the_run() {
        let refused = Row::refused(
            Scenario::Vault,
            &not_run("an ALab vault answers on the guest's network"),
        );
        assert_eq!(
            refused.outcome,
            Outcome::NotRun("an ALab vault answers on the guest's network".into())
        );
        let failed = Row::refused(Scenario::Doctor, &Fail::die("guest `x`: exit 1"));
        assert_eq!(failed.outcome, Outcome::Failed("guest `x`: exit 1".into()));
        let measured = Row::new(Scenario::InitApply);
        assert_eq!(exit_code(std::slice::from_ref(&measured)), 0);
        assert_eq!(exit_code(&[measured.clone(), refused.clone()]), NOT_RUN);
        assert_eq!(exit_code(&[refused.clone(), failed.clone()]), 1);
        assert_eq!(exit_code(&[]), NOT_RUN);
        let shown = table("head", &[refused, failed]);
        assert!(
            shown.starts_with("head\n\n| Row | Result | Measured |"),
            "{shown}"
        );
        assert!(
            shown.contains("| vault | NOT RUN: an ALab vault"),
            "{shown}"
        );
        assert!(
            shown.contains("| doctor | FAILED: guest `x`: exit 1 |"),
            "{shown}"
        );
    }

    #[test]
    fn a_cell_never_breaks_the_table() {
        let mut row = Row::new(Scenario::InitApply);
        row.cell("last line", Value::Text("a | b\nc".into()));
        let shown = table("", &[row]);
        assert!(shown.contains("a \\| b c"), "{shown}");
        assert_eq!(shown.lines().count(), 5);
    }

    #[test]
    fn a_line_counts_only_when_it_prints_outside_its_escapes() {
        let out = "\u{1b}]0;aterm\u{7}\r\n\u{1b}[1;32m\u{1b}[0m\r\n  \r\n\
                   aterm init: 3 changed, 40 ok\r\n\u{1b}[2Kdone\n\u{1b}]7;file://h/x\u{1b}\\\n";
        assert_eq!(output_lines(out), 2);
        assert_eq!(output_lines(""), 0);
        let mut row = Row::new(Scenario::InitApply);
        lines_cell(&mut row, "no-op: ", out);
        assert_eq!(row.cells, [("no-op: lines".to_owned(), Value::Count(2))]);
    }

    #[test]
    fn a_shell_session_ends_at_the_marker_its_prompt_prints() {
        // The marker the account's prompt prints is the one the driver waits for.
        assert!(PROMPT_MARKER.contains(&format!("print -r -- {SHELL_READY}")));
        assert!(DRIVE.contains(&format!("-re {{{SHELL_READY}}}")));
        let record = "start_epoch_ms 10\nfirst_byte_ms 40\nshell_prompt_ms 812\nend_ms 900\n\
                      prompts 0\nquiet_waited_ms 0\nexit 0\n";
        let d = read_drive(record);
        assert_eq!(d.shell_prompt_ms, Some(812));
        let out = "aterm: setup continues in aterm.app (aterm help doctor)\r\nnux-shell-ready\r\n% exit\r\n";
        let mut row = Row::new(Scenario::TerminalLaunch);
        shell_cells(&mut row, "cold: ", &d, out);
        assert_eq!(row.outcome, Outcome::Measured);
        assert_eq!(
            row.cells[0],
            ("cold: to the shell prompt".to_owned(), Value::Ms(812))
        );
        assert_eq!(
            row.cells[1],
            ("cold: before it: lines".to_owned(), Value::Count(1))
        );
        // NEGATIVE CONTROL: no prompt is NOT MEASURED and fails the row, never 0.
        let mut row = Row::new(Scenario::TerminalLaunch);
        shell_cells(
            &mut row,
            "",
            &read_drive("start_epoch_ms 1\nhang 300000\n"),
            "",
        );
        assert!(matches!(row.cells[0].1, Value::Missing(_)));
        assert!(matches!(row.outcome, Outcome::Failed(_)));
    }

    #[test]
    fn a_driver_that_left_no_record_fails_the_row_and_the_first_cause_stays() {
        let mut row = Row::new(Scenario::Doctor);
        drive_cells(&mut row, "terminal: ", &read_drive(""));
        assert_eq!(
            row.outcome,
            Outcome::Failed("terminal: the driver left no record".into())
        );
        drive_cells(
            &mut row,
            "read-only: ",
            &read_drive("start_epoch_ms 1\nhang 9\n"),
        );
        assert_eq!(
            row.outcome,
            Outcome::Failed("terminal: the driver left no record".into())
        );
        assert!(row.cells.iter().any(|(name, _)| name == "read-only: end"));
    }

    #[test]
    fn percentiles_are_nearest_rank_and_exec_counts_read_lastcomm() {
        let ten: Vec<u64> = (1..=10).map(|n| n * 100).collect();
        assert_eq!(nearest_rank(&ten, 50), Some(500));
        assert_eq!(nearest_rank(&ten, 95), Some(1000));
        assert_eq!(nearest_rank(&[7], 95), Some(7));
        assert_eq!(nearest_rank(&[], 50), None);

        let lastcomm = "defaults         -       admin    __         0.00 secs Sat Oct 25 10:00 (0:00:00.02)\n\
                        killall          -       admin    __         0.00 secs Sat Oct 25 10:00 (0:00:00.01)\n\
                        defaults         -       admin    __         0.00 secs Sat Oct 25 10:00 (0:00:00.02)\n\
                        aterm            -       admin    __         0.10 secs Sat Oct 25 10:00 (0:00:01.00)\n";
        assert_eq!(exec_count(lastcomm, "defaults"), 2);
        assert_eq!(exec_count(lastcomm, "killall"), 1);
        assert_eq!(exec_count(lastcomm, "default"), 0);
        assert_eq!(exec_count("", "defaults"), 0);
    }

    #[test]
    fn the_local_vault_is_served_on_loopback_and_never_advertised() {
        // A vault the guest advertised could be heard on the host's network: its server binds
        // only 127.0.0.1 and names no mDNS service.
        let setup = local_vault_setup("/Applications/aterm.app/Contents/MacOS/aterm", "/tmp/a");
        let serve = setup
            .split(';')
            .find(|step| step.contains(" secrets serve "))
            .unwrap();
        let words: Vec<&str> = serve.split_whitespace().collect();
        let listen = words
            .iter()
            .position(|w| *w == "--listen")
            .map(|at| words[at + 1])
            .unwrap();
        assert!(listen.starts_with("127.0.0.1:"), "{serve}");
        assert!(words.contains(&"--no-advertise"), "{serve}");
        assert!(!words.contains(&"--advertise"), "{serve}");
    }
}
