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
use crate::mac::{NOT_RUN, PERSON_HELPER, POLL, ROW, STEP, Vm, not_run};
use crate::mac_nux_guest::{
    Add, DEVTOOLS_CANCEL, DEVTOOLS_GOTO, DEVTOOLS_OPEN, DevtoolsActs, HidProof, Procs,
    ReopenPresses, STALL_MS, Stall, Typed, Waited, Went, acts, click_latencies, completed_at,
    devtools_acts, dialogs, first_session, hid_proof, read_census, read_person, read_procs,
    read_record, read_watch, reopen_presses, runs, setup_field, stage_name, stage_starts,
};
use crate::paths::paths;
use crate::{Fail, LAB, Res, stop};

pub const USAGE: &str = "\
  nux [--app PATH] [--scenario ROW,...] [--standard-user] [--person [--later]] [--no-clt | --vanilla]
      [--repeat N] [--image REF] [--keep] [--record [FILE]]
                what a person meets on a new Mac, each row in a fresh clone of REF's prepared base (as dmg):
                the app built here (ship dev-app made release-shaped: aterm.app, com.aterm.aterm, hardened
                runtime, signed ad hoc) or --app PATH (read, never changed), unpacked into the guest's
                /Applications (--standard-user: a non-admin account logged in at the console, and its
                ~/Applications). --person: the person base, whose guest helper (aterm-lab-guest, granted
                Accessibility and event posting by its signing identifier while the image's SIP is off,
                so each run installs its own build) is the person at the console: a pointer move every
                5 s, Allow and OK pressed in dialogs, the account's password typed into a password field
                (--standard-user: the image's administrator's name and password into a dialog that asks for
                an administrator), System Settings switches naming aterm turned on, System Settings'
                Quit & Reopen sheet answered with Quit & Reopen (--later: Later) and then the app's own
                question before it quits answered with Quit, on System Settings' Developer Tools page
                aterm added to the list (the helper panel's tile dragged onto it, else + and the app chosen
                in the open panel) and its switch turned on, each input with the HIDIdleTime it left (a
                row whose input did not reset it says NOT MEASURED).
                --no-clt: a base with the Command Line Tools removed (xcode-select -p fails); --vanilla:
                REF's cirruslabs -vanilla image (NOT RUN when it must be pulled with under 71 GB free).
                --repeat N: each row N times, each in its own clone. Every row also reads the dialog census
                (each window owner that appears, the guest helper polling the window list every 250 ms with
                no Screen Recording grant), tccd's consent prompts, xcode-select -p, and the ALab guard:
                alab-secrets.local unreached and no _alab-vault._tcp advert in the guest, none from a guest
                on this Mac's tart bridge (an advert whose host resolves to this Mac's own addresses is this
                Mac's, named and never a failure; the vault port is null-routed in every guest but to
                loopback and tart's own network). Rows (default all), each step under a 300 s hang detector:
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
                  pass-held        aterm pkg install --default-set started, and while it holds the store:
                                   aterm doctor --json and aterm init (no terminal), aterm init --no-wait,
                                   then aterm doctor on a terminal: each one's end, exit and lines, whether
                                   the pass still held the store at its start, the change-journal records
                                   (the guest's disk grown to 100 GB first)
                  doctor           aterm doctor on a terminal (Developer Tools not granted) x10: the first
                                   run's first byte, end, lines, prompts, exit, System Settings opened, and
                                   p50/p95; aterm doctor --json | cat (no terminal): System Settings opened;
                                   aterm doctor --read-only --json x10: p50/p95, rows, timed rows, phases
                  launch-repairs   a copied command planted at ~/.local/bin/aterm, the app opened until its
                                   launch repairs log (after its seed), then quit and opened again: each
                                   launch's open to prompt, first present and present p50/p95/p99,
                                   its repairs' lines and when, what
                                   ~/.local/bin/aterm is after it, the change-journal records (the guest's
                                   disk grown to 100 GB first)
                  first-launch     the app opened, then watched until the guest's own aterm ctl status says
                                   setup=done (the progress record's completion on a build without the
                                   field), or until setup stood still a minute with no new press by the
                                   person and no app process starting or going: first prompt, first
                                   present, each stage's start (a guest-side watch of initial-setup.json)
                                   and the first one's after first present, n_present and present and
                                   input p50/p95/p99 while setup ran, aterm's state folder at boot and at
                                   window focus, WARN lines, consent dialogs (with a ~/Desktop control),
                                   which process macOS held back to ask its Local Network question and
                                   when, beside System State's own ask (the unified log's
                                   networkextension lines, each pid's command as ps reads it),
                                   folder dialogs after Full Disk Access was granted (target 0), the
                                   toolset pass, the setup lines and hand-overs in order, the mDNS
                                   questions for the ALab vault, change-journal records, defaults and
                                   killall execs, relaunches; the Quit & Reopen sheet's press and the
                                   app's processes (a guest-side watch of the kernel's process table
                                   every 50 ms): the quit, the reopen with its pid, the reopened
                                   pid's first aterm.log line, setup followed across it (the guest's
                                   disk grown to 100 GB first)
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
                  vault-phrase     a new Mac let in by a member's Allow and the phrase it shows (F10), on
                                   the guest's loopback alone: a seed identity founds a vault on
                                   127.0.0.1 (never advertised) with a synthetic GitHub token, publish
                                   token and AI accounts list in [everyone] and runs its approver; a
                                   second identity, a member that is no admin (asked, approved by the
                                   seed, enrolled in its Commons), allows (the guest helper running
                                   F10's members desk on its custody behind an ask-only listener on
                                   127.0.0.1, which presses Allow for the first Mac that asks; the
                                   phrase read off its record, never a window); a third runs aterm
                                   members join --no-wait --at <the member> (install.sh's ask), aterm
                                   members join with the phrase typed at its prompt, then its
                                   subscriber: the ask to the member's row, to the phrase shown and
                                   typed, to each file on disk, to members status reading In; FAILED
                                   unless the phrase came from that member, every file is the seed's
                                   value, the joiner reads In, every aterm listener is on loopback, and
                                   every _alab-vault._tcp and _alab._tcp browse, in the guest and on
                                   this Mac, ran and heard no guest advert
                  successor        install.sh --version N (the release before the latest), then the
                                   updater to the latest with a window open, and the successor's setup
                                   and Full Disk Access lines in its first 30 s
                  install-sh       tools/install.sh on a terminal as the account at the console: first
                                   byte, end, its last line, the aterm process after its exit (and the
                                   typed steps a person still has), then the opened app's first log line
                                   and its toolset pass's start and end within 3 min (the guest's disk
                                   grown to 100 GB first)
                Fault rows (`faults`, or each by name; never in all), one thing going wrong in each guest:
                  kill-stage       the app SIGKILLed at each stage it runs (the guest helper's watch of
                                   initial-setup.json kills within 50 ms; the first stage twice) and
                                   opened again: each launch's first stage, any done stage re-run, the
                                   stage killed twice running a third time or parked, setup's end
                  kill-clt         (--no-clt or --vanilla) the app SIGKILLed while the Command Line Tools
                                   install runs: whether it outlived the kill, the relaunch's installers
                  kill-publish     NOT RUN: a clone's publish needs a test organization's token
                  rerun            a first launch to setup's end, then a second launch (stages, changes,
                                   dialogs) and a second aterm init (prompts, lines, changes)
                  legacy-upgrade   a 0.109 pending first-run.json and a Dock the person changed: whether
                                   setup starts and the person's Dock keys after it
                  person-change    setup to its end, the person's Dock change, a relaunch: the keys after
                  offline          the guest's network down for the first 30 s of a launch: tart exec
                                   while it is down, setup's first progress once it is back, rows left
                                   not in place
                  lossy            a launch through dnctl's 1 Mbit/s, 150 ms, 1% loss pipe: setup's end,
                                   rows not in place shaped and once clean, the packets the pipe carried
                  mdns-silent      the vault row with mDNS dropped by the packet filter
                  mdns-no-route    the vault row with mDNS refused (block return)
                  dmg              the app in a disk image hdiutil builds in the guest, opened twice from
                                   the mounted image, then twice quarantined: where it ran, Gatekeeper
                                   dialogs, an installed copy
                  agent-ssh        aterm init under a parent named claude and over ssh -tt (the presence
                                   each read, what each changed), then a guest-local vault joined over
                                   SSH (prompts, Dock and defaults calls, the secret on disk)
                  clock-back       the guest's clock moved back an hour once a stage runs: setup's end,
                                   any done stage re-run, rows not in place
                Prints a markdown table; --record FILE appends it (--record alone: docs/measured/
                mac-nux-<date>.md in this checkout). Exit 0 every row measured, 1 a row
                failed (a hang detector fired, a guest step failed), 3 NOT RUN (a row the guest could not
                run, no tart)
";

/// The guest folder the drivers and every record live in (outside the share). Not `/tmp`, which
/// macOS empties at boot: a kept clone (`--keep`) still holds its rows' records when it boots
/// again.
const GUEST_NUX: &str = "/var/tmp/aterm-nux";

/// The guest helper's file name (`src/guest/main.rs`), in the share and in [`GUEST_NUX`].
const GUEST_HELPER: &str = "aterm-lab-guest";

/// The launchd labels of the census and the person, agents of the account's GUI session.
const CENSUS_AGENT: &str = "com.aterm.nux.census";
const PERSON_AGENT: &str = "com.aterm.nux.person";

/// The app's process watch's record, in [`GUEST_NUX`].
const PROCS_RECORD: &str = "procs.txt";

/// The progress record setup keeps (`initial.rs`), under the account's home.
const SETUP_RECORD: &str = "Library/Application Support/aterm/doctor/initial-setup.json";

/// How often the wait on setup looks at the guest.
const SETUP_POLL: Duration = Duration::from_secs(1);

/// The guest's packet filter: the ALab vault's port refused to every address but loopback and
/// tart's own network (where a lab vault of another guest lives), the host's end of it included,
/// so no guest reaches a vault on this Mac's LAN whatever its names resolve to. Names are left as
/// they are, so a guest off the ALab LAN looks the ALab vault up as such a Mac does.
const PF_GUARD: &str = "block return out quick proto tcp from any to 192.168.64.1 port 7484
pass out quick proto tcp from any to 127.0.0.0/8 port 7484
pass out quick proto tcp from any to 192.168.64.0/24 port 7484
block return out quick proto tcp from any to any port 7484
";

/// The ceiling of `first-launch`'s wait on setup: past it the wait ends even while setup still
/// moves (it ends sooner once setup is done, or stood still a minute with nothing new from the
/// person or in the app's processes).
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

/// The image's administrator (cirruslabs' `admin`) and its password: what a person at the standard
/// account gives a dialog that asks for an administrator.
const ADMIN_USER: &str = "admin";
const ADMIN_PASSWORD: &str = "admin";

/// The public release pointer install.sh reads first.
const LATEST_POINTER: &str =
    "https://github.com/alabsystems/aterm/releases/latest/download/aterm-appcast.toml";

/// The ALab vault's host and service type: a guest that reaches either never runs the vault row.
const ALAB_HOST: &str = "alab-secrets.local";
const ALAB_SERVICE: &str = "_alab-vault._tcp";

/// The pty driver: `drive.exp <record> <limit_s> <cmd> [arg...]`. A known prompt is answered at
/// once; any other partial line left 1.5 s is a prompt, answered with Enter (its default). Each
/// prompt's record names when it came and how long the command was silent before it.
/// `$NUX_JOIN` answers a vault join password (default `none`), `$NUX_PHRASE` the join phrase
/// `aterm members join` asks for (`vault-phrase`), `$NUX_SUDO` a sudo password. With
/// `$NUX_SHELL` (a shell session's run), the first [`SHELL_READY`] line is its shell's first
/// prompt: recorded, then answered with `exit`; nothing unknown is answered.
pub const DRIVE: &str = r#"set record [lindex $argv 0]
set limit [lindex $argv 1]
set f [open $record w]
proc rec {args} { global f; puts $f [join $args " "]; flush $f }
proc ms {} { global t0; expr {[clock milliseconds] - $t0} }
set join none
if {[info exists env(NUX_JOIN)]} { set join $env(NUX_JOIN) }
set phrase ""
if {[info exists env(NUX_PHRASE)]} { set phrase $env(NUX_PHRASE) }
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
        -re {[^\r\n]*The phrase [^\r\n]* shows: $} { answer $expect_out(0,string) $phrase }
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
    PassHeld,
    Doctor,
    LaunchRepairs,
    FirstLaunch,
    TerminalLaunch,
    AppLaunch,
    Vault,
    VaultLocal,
    VaultPhrase,
    Successor,
    InstallSh,
    KillStage,
    KillClt,
    KillPublish,
    Rerun,
    LegacyUpgrade,
    PersonChange,
    Offline,
    Lossy,
    MdnsSilent,
    MdnsNoRoute,
    Dmg,
    AgentSsh,
    ClockBack,
}

/// Every row, in the order a run takes them.
pub const ALL: [Scenario; 14] = [
    Scenario::InitApply,
    Scenario::InitNotty,
    Scenario::InitSsh,
    Scenario::PassHeld,
    Scenario::Doctor,
    Scenario::LaunchRepairs,
    Scenario::FirstLaunch,
    Scenario::TerminalLaunch,
    Scenario::AppLaunch,
    Scenario::Vault,
    Scenario::VaultLocal,
    Scenario::VaultPhrase,
    Scenario::Successor,
    Scenario::InstallSh,
];

/// The fault rows (`faults`; `mac_nux_fault.rs`), each one thing going wrong in its own guest, in
/// the order a run takes them, after [`ALL`]'s.
pub const FAULTS: [Scenario; 13] = [
    Scenario::KillStage,
    Scenario::KillClt,
    Scenario::KillPublish,
    Scenario::Rerun,
    Scenario::LegacyUpgrade,
    Scenario::PersonChange,
    Scenario::Offline,
    Scenario::Lossy,
    Scenario::MdnsSilent,
    Scenario::MdnsNoRoute,
    Scenario::Dmg,
    Scenario::AgentSsh,
    Scenario::ClockBack,
];

/// Every row [`ALL`] and [`FAULTS`] name, in run order.
fn every() -> impl Iterator<Item = Scenario> {
    ALL.into_iter().chain(FAULTS)
}

impl Scenario {
    /// Its name on the command line and in the table.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Scenario::InitApply => "init-apply",
            Scenario::InitNotty => "init-notty",
            Scenario::InitSsh => "init-ssh",
            Scenario::PassHeld => "pass-held",
            Scenario::Doctor => "doctor",
            Scenario::LaunchRepairs => "launch-repairs",
            Scenario::FirstLaunch => "first-launch",
            Scenario::TerminalLaunch => "terminal-launch",
            Scenario::AppLaunch => "app-launch",
            Scenario::Vault => "vault",
            Scenario::VaultLocal => "vault-local",
            Scenario::VaultPhrase => "vault-phrase",
            Scenario::Successor => "successor",
            Scenario::InstallSh => "install-sh",
            Scenario::KillStage => "kill-stage",
            Scenario::KillClt => "kill-clt",
            Scenario::KillPublish => "kill-publish",
            Scenario::Rerun => "rerun",
            Scenario::LegacyUpgrade => "legacy-upgrade",
            Scenario::PersonChange => "person-change",
            Scenario::Offline => "offline",
            Scenario::Lossy => "lossy",
            Scenario::MdnsSilent => "mdns-silent",
            Scenario::MdnsNoRoute => "mdns-no-route",
            Scenario::Dmg => "dmg",
            Scenario::AgentSsh => "agent-ssh",
            Scenario::ClockBack => "clock-back",
        }
    }

    /// Why a guest cannot run it at all (it is NOT RUN before a clone is made); `None` for a row
    /// a guest runs.
    #[must_use]
    pub const fn not_run(self) -> Option<&'static str> {
        match self {
            Scenario::KillPublish => Some(fault::KILL_PUBLISH),
            _ => None,
        }
    }

    /// Whether its clone's disk grows to [`FIRST_LAUNCH_DISK_GB`] first: a row that opens the app
    /// and lets its toolset pass run.
    #[must_use]
    pub const fn grows_disk(self) -> bool {
        matches!(
            self,
            Scenario::FirstLaunch
                | Scenario::InstallSh
                | Scenario::PassHeld
                | Scenario::LaunchRepairs
                | Scenario::KillStage
                | Scenario::KillClt
                | Scenario::Rerun
                | Scenario::LegacyUpgrade
                | Scenario::PersonChange
                | Scenario::Offline
                | Scenario::Lossy
                | Scenario::Dmg
                | Scenario::ClockBack
        )
    }

    /// Whether it runs the app this run built or was handed (the others install a public release).
    #[must_use]
    pub const fn needs_app(self) -> bool {
        !matches!(self, Scenario::Successor | Scenario::InstallSh)
    }
}

/// The rows `--scenario` names (comma-separated, `all` for [`ALL`], `faults` for [`FAULTS`]), in
/// run order, each once.
///
/// # Errors
/// An empty list or a name no row has.
pub fn parse_scenarios(list: &str) -> Result<Vec<Scenario>, String> {
    let mut picked = Vec::new();
    for word in list.split(',').map(str::trim) {
        match word {
            "all" => picked.extend(ALL),
            "faults" => picked.extend(FAULTS),
            _ => picked.push(every().find(|s| s.name() == word).ok_or_else(|| {
                format!(
                    "no row '{word}' (rows: all, faults, {})",
                    every().map(Scenario::name).collect::<Vec<_>>().join(", ")
                )
            })?),
        }
    }
    if picked.is_empty() {
        return Err("--scenario names no row".into());
    }
    Ok(every().filter(|s| picked.contains(s)).collect())
}

/// What a run was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct Ask {
    pub app: Option<PathBuf>,
    pub scenarios: Vec<Scenario>,
    pub standard_user: bool,
    /// The person base: the guest helper acting as the person at the console.
    pub person: bool,
    /// The person answers System Settings' Quit & Reopen sheet with Later (else Quit & Reopen).
    pub later: bool,
    /// A base with the Command Line Tools removed.
    pub no_clt: bool,
    /// The image is REF's `-vanilla` sibling.
    pub vanilla: bool,
    /// How many times each row runs, each in its own clone.
    pub repeat: usize,
    pub image: String,
    pub keep: bool,
    pub record: Option<PathBuf>,
}

/// The most `--repeat` takes.
const MOST_REPEATS: usize = 10;

/// The cirruslabs `-vanilla` image beside `image` (a `-base` one): the same macOS with no Command
/// Line Tools or Homebrew of its own.
#[must_use]
pub fn vanilla_image(image: &str) -> Option<String> {
    let (name, tag) = image.rsplit_once(':').unwrap_or((image, "latest"));
    let stem = name.strip_suffix("-base")?;
    Some(format!("{stem}-vanilla:{tag}"))
}

/// The dated record `--record` alone appends to.
fn dated_record() -> PathBuf {
    paths().repo.join(format!(
        "docs/measured/mac-nux-{}.md",
        crate::util::ymd(crate::util::now_secs())
    ))
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
    let person = take_flag(&mut a, "--person");
    let later = take_flag(&mut a, "--later");
    if later && !person {
        return Err(Fail::usage(
            "mac nux: --later is the person's answer to the Quit & Reopen sheet: it needs --person",
        ));
    }
    let no_clt = take_flag(&mut a, "--no-clt");
    let vanilla = take_flag(&mut a, "--vanilla");
    if no_clt && vanilla {
        return Err(Fail::usage(
            "mac nux: --no-clt and --vanilla each give a Mac without the Command Line Tools: pick one",
        ));
    }
    let repeat = match take_value(&mut a, "--repeat")? {
        Some(n) => n
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MOST_REPEATS).contains(n))
            .ok_or_else(|| {
                Fail::usage(format!(
                    "mac nux: --repeat {n}: a count from 1 to {MOST_REPEATS}"
                ))
            })?,
        None => 1,
    };
    let image = take_value(&mut a, "--image")?.unwrap_or(image);
    let image = if vanilla {
        vanilla_image(&image).ok_or_else(|| {
            Fail::usage(format!(
                "mac nux: --vanilla needs a cirruslabs -base image to find its -vanilla sibling, not {image}"
            ))
        })?
    } else {
        image
    };
    let keep = take_flag(&mut a, "--keep");
    let record = match a.iter().position(|w| w == "--record") {
        Some(at) => {
            a.remove(at);
            match a.get(at).filter(|w| !w.starts_with("--")) {
                Some(_) => Some(from_caller(a.remove(at))),
                None => Some(dated_record()),
            }
        }
        None => None,
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "mac nux: unexpected '{extra}' ({LAB} mac help)"
        )));
    }
    Ok(Ask {
        app,
        scenarios,
        standard_user,
        person,
        later,
        no_clt,
        vanilla,
        repeat,
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
    drive_cells_within(row, label, d, ROW);
}

/// [`drive_cells`] of a run whose hang detector was `limit`.
fn drive_cells_within(row: &mut Row, label: &str, d: &Drive, limit: Duration) {
    if let Some(why) = &d.spawn_failed {
        row.fail(format!(
            "{label}the driver could not start the command: {why}"
        ));
    } else if let Some(at) = &d.hang {
        row.fail(format!(
            "{label}the {} s hang detector fired ({at})",
            limit.as_secs()
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

/// The rows an `aterm init` run left open, each as its outcome and id (`failed base.clt`): from
/// the last `--json` report in `out` (every row whose outcome is not ok, changed or not
/// applicable), else from the text rows it printed (`<word>: <title> (<id>)`, every word but
/// `changed`). A `planned` row is open: a `--check` run's rows that still differ read so and
/// fail it (only `--plan`, which the lane never runs, counts them done). A waiting row an
/// admin's approval finishes is named too: it is not the exit's.
#[must_use]
pub fn open_rows(out: &str) -> Vec<String> {
    const DONE: [&str; 3] = ["ok", "changed", "not-applicable"];
    const OPEN: [&str; 7] = [
        "failed",
        "unknown",
        "blocked",
        "deferred",
        "needs action",
        "waiting",
        "planned",
    ];
    let report = out.lines().rev().find_map(|line| {
        let v: aterm_json::Value = aterm_json::from_str(line.trim()).ok()?;
        v.get("items")?.as_array().cloned()
    });
    if let Some(items) = report {
        return items
            .iter()
            .filter_map(|item| {
                let outcome = item.get("outcome")?.as_str()?;
                let id = item.get("id")?.as_str()?;
                (!DONE.contains(&outcome)).then(|| format!("{outcome} {id}"))
            })
            .collect();
    }
    out.lines()
        .filter_map(|line| {
            let (word, rest) = line.trim_end().split_once(": ")?;
            let (_, id) = rest.strip_suffix(')')?.rsplit_once(" (")?;
            (OPEN.contains(&word) && !id.is_empty() && !id.contains(char::is_whitespace))
                .then(|| format!("{word} {id}"))
        })
        .collect()
}

/// When `d` ended nonzero, the rows its `aterm init` output left open ([`open_rows`]) under
/// `label`, so the exit names its cause.
fn open_rows_cell(row: &mut Row, label: &str, d: &Drive, out: &str) {
    if d.exit.is_none_or(|code| code == 0) {
        return;
    }
    let open = open_rows(out);
    row.cell(
        format!("{label}rows left open"),
        if open.is_empty() {
            Value::Missing("init's output names no row left open".into())
        } else {
            Value::Text(squeeze(&open.join(", "), 300))
        },
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

/// Main to first present in `aterm ctl metrics`' line (`rust_main_to_first_present_ms`): `Err`
/// when the line has no such field, `Ok(None)` while it reads 0, as it does until the window's
/// first terminal present (a window System State fronts from its launch has none).
fn first_present(metrics: &str) -> Result<Option<u64>, ()> {
    let ms = metric(metrics, "rust_main_to_first_present_ms").ok_or(())?;
    Ok((ms > 0.0).then(|| ms_value(ms)))
}

/// [`first_present`] as a cell's value.
fn first_present_value(metrics: &str) -> Value {
    match first_present(metrics) {
        Err(()) => Value::Missing("aterm ctl metrics did not answer".into()),
        Ok(None) => Value::Missing("no terminal frame presented yet".into()),
        Ok(Some(ms)) => Value::Ms(ms),
    }
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

/// Start recording the guest's mDNS packets on its network ([`stop_mdns`] ends it).
fn start_mdns(g: &Guest) -> Result<(), Fail> {
    g.sh(
        &format!(
            // Its own output away from `tart exec`'s, which would otherwise wait on it.
            "({} </dev/null >/dev/null 2>&1 &) ; sleep 1",
            g.as_root(&format!(
                "exec /usr/sbin/tcpdump -l -n -i en0 udp port 5353 > {GUEST_NUX}/mdns.txt 2>/dev/null"
            ))
        ),
        STEP,
    )
    .map(drop)
}

/// End [`start_mdns`]'s recording: the packets it wrote.
fn stop_mdns(g: &Guest) -> Result<String, Fail> {
    let _ = g.vm.exec(
        &format!("{}; sleep 0.5", g.as_root("pkill -INT -x tcpdump")),
        STEP,
    );
    g.read(&format!("{GUEST_NUX}/mdns.txt"))
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

/// One consent dialog tccd put up: when (epoch ms, from the event's `timestamp`), the service,
/// and the subject it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccPrompt {
    pub at_ms: Option<u64>,
    pub service: String,
    pub subject: String,
}

/// tccd's consent dialogs (`AUTHREQ_PROMPTING`) in a `log stream --style ndjson` record, in order.
#[must_use]
pub fn tcc_prompt_events(log: &str) -> Vec<TccPrompt> {
    log.lines()
        .filter_map(|line| {
            let message = crate::mac::event_message(line);
            let (_, rest) = message.split_once("AUTHREQ_PROMPTING:")?;
            let service = rest
                .split_once("service=")
                .map_or("?", |(_, s)| s.split([',', ' ', '"']).next().unwrap_or(""));
            let subject = rest
                .split_once("subject=Sub:{")
                .and_then(|(_, s)| s.split_once('}'))
                .map_or("?", |(s, _)| s);
            let at_ms = aterm_json::from_str::<aterm_json::Value>(line)
                .ok()
                .and_then(|v| v.get("timestamp")?.as_str().and_then(log_timestamp_ms));
            Some(TccPrompt {
                at_ms,
                service: service
                    .strip_prefix("kTCCService")
                    .unwrap_or(service)
                    .to_owned(),
                subject: subject.replace("\\/", "/"),
            })
        })
        .collect()
}

/// macOS's Local Network checks that held a process back to ask its person, from
/// `log show --style ndjson` of UserEventAgent's `Got local network blocked notification: pid: <n>,
/// …`: when (epoch ms, from the event's `timestamp`) and the pid, in order.
#[must_use]
pub fn local_network_blocks(log: &str) -> Vec<(Option<u64>, u32)> {
    log.lines()
        .filter_map(|line| {
            let message = crate::mac::event_message(line);
            let (_, rest) = message.split_once("local network blocked notification: pid: ")?;
            let pid = rest.split([',', ' ']).next()?.parse().ok()?;
            let at_ms = aterm_json::from_str::<aterm_json::Value>(line)
                .ok()
                .and_then(|v| v.get("timestamp")?.as_str().and_then(log_timestamp_ms));
            Some((at_ms, pid))
        })
        .collect()
}

/// Each process `ps -o pid=,command=` listed: its pid and command line.
#[must_use]
pub fn ps_commands(ps: &str) -> BTreeMap<u32, String> {
    ps.lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            Some((pid.parse().ok()?, command.trim().to_owned()))
        })
        .collect()
}

/// The folder (TCC) dialogs tccd put up for the app at or after `since_ms` (epoch ms): a
/// `SystemPolicy…` service other than Full Disk Access itself, asked for a subject naming aterm
/// (never the guest agent's ~/Desktop control).
#[must_use]
pub fn folder_prompts_after(prompts: &[TccPrompt], since_ms: u64) -> usize {
    prompts
        .iter()
        .filter(|p| {
            p.service.starts_with("SystemPolicy")
                && p.service != "SystemPolicyAllFiles"
                && p.subject.contains("aterm")
                && p.at_ms.is_none_or(|at| at >= since_ms)
        })
        .count()
}

/// Epoch ms of a unified log `timestamp` (`2026-10-26 08:30:01.123456-0700`).
#[must_use]
pub fn log_timestamp_ms(ts: &str) -> Option<u64> {
    let (date, rest) = ts.trim().split_once(' ')?;
    let mut ymd = date.split('-').map(str::parse::<i64>);
    let (y, m, d) = (ymd.next()?.ok()?, ymd.next()?.ok()?, ymd.next()?.ok()?);
    let zone_at = rest.rfind(['+', '-'])?;
    let (clock, zone) = rest.split_at(zone_at);
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, "0"));
    let mut hms = hms.split(':').map(str::parse::<i64>);
    let (h, mi, sec) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    let ms: i64 = format!("{frac:0<3}").get(..3)?.parse().ok()?;
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let zone: i64 = zone.get(1..)?.parse().ok()?;
    let offset = sign * ((zone / 100) * 3600 + (zone % 100) * 60);
    // Days from the civil date (Howard Hinnant's algorithm).
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset;
    u64::try_from(secs * 1000 + ms).ok()
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
    /// System State's own Local Network step handing its question to macOS.
    pub local_network_asked_ms: Option<u64>,
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
        local_network_asked_ms: first(&|l| {
            l.message
                .starts_with("system state: Local Network asked for")
        }),
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
    /// Each row whose outcome is not in place ([`IN_PLACE`]): its id and outcome, in report order.
    pub not_ok: Vec<(String, String)>,
}

/// The outcomes of a report's rows that leave nothing to do.
pub const IN_PLACE: [&str; 4] = ["ok", "changed", "not-applicable", "n/a"];

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
        let not_ok = items
            .iter()
            .filter_map(|i| {
                let outcome = i.get("outcome")?.as_str()?;
                if IN_PLACE.contains(&outcome) {
                    return None;
                }
                Some((i.get("id")?.as_str()?.to_owned(), outcome.to_owned()))
            })
            .collect();
        Some(ReportJson {
            rows: items.len(),
            timed: items.iter().filter(|i| elapsed(i).is_some()).count(),
            slowest,
            phases,
            not_ok,
        })
    })
}

/// The rows a report left not in place, as one cell under `label` (a run that exited non-zero
/// names them; `from` says which report).
fn not_ok_cell(row: &mut Row, label: &str, from: &str, report: Option<&ReportJson>) {
    row.cell(
        format!("{label}rows not ok ({from})"),
        match report {
            Some(j) if j.not_ok.is_empty() => Value::Text("none".into()),
            Some(j) => Value::Text(
                j.not_ok
                    .iter()
                    .map(|(id, outcome)| format!("{id} {outcome}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            None => Value::Missing("the report did not parse as JSON".into()),
        },
    );
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
    /// The account's uid: its GUI session's launchd domain is `gui/<uid>`.
    uid: String,
    /// What a row's dialogs count from (the guest's clock, epoch ms): the open, for a launch; the
    /// census' own start when a row sets none.
    mark_ms: std::cell::Cell<Option<u64>>,
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
        self.drive_within(tag, env, cmd, ROW)
    }

    /// [`Self::drive`] with `limit` its hang detector.
    fn drive_within(
        &self,
        tag: &str,
        env: &str,
        cmd: &str,
        limit: Duration,
    ) -> Result<(Drive, String), Fail> {
        self.run(
            &format!(
                "cd {GUEST_NUX} && {env} /usr/bin/expect -f drive.exp {tag}.rec {} {cmd} > {tag}.out 2>&1",
                limit.as_secs()
            ),
            limit + STEP,
        )?;
        Ok((
            read_drive(&self.read(&format!("{GUEST_NUX}/{tag}.rec"))?),
            self.read(&format!("{GUEST_NUX}/{tag}.out"))?,
        ))
    }

    /// `cmd` under [`NOTTY`]; its record and output under `tag`.
    fn notty(&self, tag: &str, cmd: &str) -> Result<(Drive, String), Fail> {
        self.notty_within(tag, cmd, ROW)
    }

    /// [`Self::notty`] with `limit` its hang detector.
    fn notty_within(&self, tag: &str, cmd: &str, limit: Duration) -> Result<(Drive, String), Fail> {
        self.run(
            &format!(
                "cd {GUEST_NUX} && /usr/bin/perl notty.pl {tag}.rec {} {cmd} > {tag}.out 2>&1",
                limit.as_secs()
            ),
            limit + STEP,
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
            ADMIN_PASSWORD
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

    /// Run `args` as the launchd agent `label` in the account's GUI session ([`agent_plist`]).
    fn agent_start(&self, label: &str, args: &[&str]) -> Result<(), Fail> {
        self.sh(
            &format!(
                "cat > {GUEST_NUX}/{label}.plist <<'EOF'\n{}EOF",
                agent_plist(label, args)
            ),
            STEP,
        )?;
        self.sh(
            &self.as_root(&format!(
                "launchctl bootout gui/{uid}/{label} 2>/dev/null; launchctl bootstrap gui/{uid} {GUEST_NUX}/{label}.plist",
                uid = self.uid
            )),
            STEP,
        )?;
        Ok(())
    }

    fn agent_stop(&self, label: &str) {
        let _ = self.run(
            &self.as_root(&format!(
                "launchctl bootout gui/{}/{label} 2>/dev/null || true",
                self.uid
            )),
            STEP,
        );
    }

    /// Wait until `file` holds a line (within [`STEP`]): `Err` with the agent's stderr otherwise.
    fn await_line(&self, file: &str, label: &str) -> Result<(), String> {
        let started = Instant::now();
        while started.elapsed() < STEP {
            if self
                .vm
                .exec(&format!("test -s {file}"), STEP)
                .is_ok_and(|o| o.status.success())
            {
                return Ok(());
            }
            if stop::sleep(POLL).is_err() {
                break;
            }
        }
        let err = self
            .read(&format!("{GUEST_NUX}/{label}.err"))
            .unwrap_or_default();
        Err(format!(
            "{label} wrote nothing within {} s{}",
            STEP.as_secs(),
            if err.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", squeeze(err.trim(), 160))
            }
        ))
    }

    /// The guest's clock, in epoch ms.
    fn clock_ms(&self) -> Result<u64, Fail> {
        let out = self.sh(
            "perl -MTime::HiRes=time -e 'printf \"%d\\n\", time * 1000'",
            STEP,
        )?;
        out.trim()
            .parse()
            .map_err(|_| Fail::die(format!("the guest's clock did not read back: {out}")))
    }

    /// Watch the progress record ([`SETUP_RECORD`]) with the guest helper (the watch before it
    /// stopped), its record a fresh `file` in [`GUEST_NUX`]; with `kill`, that pid is killed the
    /// first time the record runs a stage of that mask of stage bits.
    fn watch_record(&self, file: &str, kill: Option<(u32, u64)>) -> Result<(), Fail> {
        let kill = kill.map_or(String::new(), |(pid, mask)| {
            format!(" --kill {pid} --when-running {mask}")
        });
        self.sh(
            &format!(
                "pkill -f '{GUEST_HELPER} watch' || true; : > {GUEST_NUX}/{file}; \
                 ({GUEST_NUX}/{GUEST_HELPER} watch \"$HOME/{SETUP_RECORD}\" {GUEST_NUX}/{file}{kill} </dev/null >/dev/null 2>&1 &); sleep 0.3"
            ),
            STEP,
        )?;
        Ok(())
    }

    /// Watch the app's processes with the guest helper's `procs` (the watch before it stopped),
    /// its record a fresh [`PROCS_RECORD`]: each process running the app's executable with no
    /// argument, as it starts and goes.
    fn watch_procs(&self) -> Result<(), Fail> {
        self.sh(
            &format!(
                "pkill -f '{GUEST_HELPER} procs' || true; : > {GUEST_NUX}/{PROCS_RECORD}; \
                 ({GUEST_NUX}/{GUEST_HELPER} procs '{}' {GUEST_NUX}/{PROCS_RECORD} </dev/null >/dev/null 2>&1 &); sleep 0.3",
                self.aterm
            ),
            STEP,
        )?;
        Ok(())
    }

    /// Stop the process watch and read its record.
    fn procs_read(&self) -> Result<Procs, Fail> {
        self.run(&format!("pkill -f '{GUEST_HELPER} procs' || true"), STEP)?;
        Ok(read_procs(
            &self.read(&format!("{GUEST_NUX}/{PROCS_RECORD}"))?,
        ))
    }
}

/// A launchd agent's plist: `args` run at load in the account's GUI session, where the window list
/// and posted events reach its console and launchd, not the guest agent, is its responsible
/// process (tccd judges the helper's own grants); its stderr to `<label>.err`.
fn agent_plist(label: &str, args: &[&str]) -> String {
    let args: String = args
        .iter()
        .map(|a| format!("<string>{a}</string>"))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array>{args}</array>\
<key>RunAtLoad</key><true/><key>ProcessType</key><string>Interactive</string>\
<key>StandardErrorPath</key><string>{GUEST_NUX}/{label}.err</string></dict></plist>
"
    )
}

/// What every row watches from its start: tccd's log and the dialog census.
struct Watchers {
    tccd: aterm_uds::spawnfd::Child,
    /// The census agent started and wrote its first line, or why not.
    census: Result<(), String>,
}

impl Watchers {
    /// tccd's log streamed to `tccd.ndjson` (as root) and the census agent started, each once it
    /// wrote its first line.
    fn start(g: &Guest) -> Result<Watchers, Fail> {
        let mut tccd = aterm_uds::spawnfd::command(&g.vm.tart)
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
                let _ = tccd.kill();
                let _ = tccd.wait();
                return Err(Fail::die(format!(
                    "tccd's log stream wrote nothing within {} s",
                    STEP.as_secs()
                )));
            }
            stop::sleep(POLL)?;
        }
        let census_out = format!("{GUEST_NUX}/census.txt");
        let census = g
            .agent_start(
                CENSUS_AGENT,
                &[
                    &format!("{GUEST_NUX}/{GUEST_HELPER}"),
                    "census",
                    &census_out,
                ],
            )
            .map_err(|f| f.msg.unwrap_or_default())
            .and_then(|()| g.await_line(&census_out, CENSUS_AGENT));
        Ok(Watchers { tccd, census })
    }

    /// Stop both and read them: the dialogs since the row's mark, and (for a row that read no
    /// consent dialogs of its own) tccd's prompts.
    fn finish(mut self, g: &Guest, row: &mut Row) -> Result<(), Fail> {
        g.agent_stop(CENSUS_AGENT);
        let dialogs_cell = match &self.census {
            Err(why) => Value::Missing(why.clone()),
            Ok(()) => {
                let census = read_census(&g.read(&format!("{GUEST_NUX}/census.txt"))?);
                match &census.error {
                    Some(why) => {
                        Value::Missing(format!("the census could not read the window list: {why}"))
                    }
                    None => {
                        let since = g
                            .mark_ms
                            .get()
                            .or(census.started.map(|(at, _)| at))
                            .unwrap_or(0);
                        Value::Text(dialog_text(&dialogs(&census, since)))
                    }
                }
            }
        };
        row.cell("dialogs", dialogs_cell);
        let _ =
            g.vm.exec(&g.as_root("pkill -INT -f '^/usr/bin/log stream'"), STEP);
        crate::mac::reap_within(&mut self.tccd, STEP);
        if !row.cells.iter().any(|(name, _)| name == "consent dialogs") {
            let tcc = g.sh(
                &format!("grep -h 'AUTHREQ_PROMPTING' {GUEST_NUX}/tccd.ndjson || true"),
                STEP,
            )?;
            let prompts = tcc_prompts(&tcc);
            row.cell(
                "consent prompts",
                Value::Text(if prompts.is_empty() {
                    "none".to_owned()
                } else {
                    prompts
                        .iter()
                        .map(|(s, n)| format!("{s} {n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }),
            );
        }
        Ok(())
    }
}

/// The dialogs a census saw, as one cell.
fn dialog_text(dialogs: &[crate::mac_nux_guest::Dialog]) -> String {
    if dialogs.is_empty() {
        return "none".to_owned();
    }
    dialogs
        .iter()
        .map(|d| {
            let more = if d.windows > 1 {
                format!(" x{}", d.windows)
            } else {
                String::new()
            };
            format!("{} ({}) {} ms{more}", d.owner, d.kind, d.first_ms)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The person at the guest's console: the helper this run built (the row's copy in
/// [`GUEST_NUX`]), put at [`PERSON_HELPER`] as root and signed with the identifier the base's
/// grants require, run as an agent of the account's GUI session, the account's password its
/// answer to a password field. In a standard account's row it also has the image's administrator
/// (`--admin`), whose name and password it gives a dialog that asks for an administrator, as a
/// person at that account does with the administrator at hand. With `later`, it answers System
/// Settings' Quit & Reopen sheet with Later; else Quit & Reopen, and then the app's own question
/// before it quits, in a window of a process running the app's executable.
fn person_start(g: &Guest, later: bool) -> Result<(), Fail> {
    g.sh(
        &g.as_root(&crate::mac::person_install(&format!(
            "{GUEST_NUX}/{GUEST_HELPER}"
        ))),
        STEP,
    )?;
    let out = format!("{GUEST_NUX}/person.txt");
    let args = person_args(&out, g.standard, later, &g.aterm);
    g.agent_start(
        PERSON_AGENT,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    g.await_line(&out, PERSON_AGENT).map_err(Fail::die)
}

/// The person's arguments, writing `out`: the administrator's credentials only in a standard
/// account's row (`standard`), `--sheet later` when it answers the Quit & Reopen sheet with Later
/// (`later`), and the app's executable (`app`), whose question before it quits the person answers
/// after its own Quit & Reopen (never with `--sheet later`).
fn person_args(out: &str, standard: bool, later: bool, app: &str) -> Vec<String> {
    let mut args = vec![
        PERSON_HELPER.to_owned(),
        "person".to_owned(),
        out.to_owned(),
    ];
    if standard {
        args.extend([
            "--password".to_owned(),
            STANDARD_PASSWORD.to_owned(),
            "--admin".to_owned(),
            format!("{ADMIN_USER}:{ADMIN_PASSWORD}"),
        ]);
    } else {
        args.extend(["--password".to_owned(), ADMIN_PASSWORD.to_owned()]);
    }
    if later {
        args.extend(["--sheet".to_owned(), "later".to_owned()]);
    }
    args.extend(["--app".to_owned(), app.to_owned()]);
    args
}

/// Stop the person and read what it did: its grants, whether its input reset `HIDIdleTime` (the
/// proof every person stage needs, else NOT MEASURED), each press with its latency, and the
/// passwords it typed, whose each was (the controls of each dialog it typed into printed for the
/// run's record).
fn person_cells(g: &Guest, row: &mut Row) -> Result<HidProof, Fail> {
    g.agent_stop(PERSON_AGENT);
    let p = read_person(&g.read(&format!("{GUEST_NUX}/person.txt"))?);
    for (_, kind, reading) in &p.dialogs {
        println!("mac nux: the person's dialog ({kind}): {reading}");
    }
    let word = |v: Option<bool>| match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    };
    row.cell(
        "person's grants",
        Value::Text(format!(
            "Accessibility {}, event posting {}",
            word(p.trusted),
            word(p.may_post)
        )),
    );
    let proof = hid_proof(&p);
    row.cell(
        "person's input resets HIDIdleTime",
        match &proof {
            HidProof::Reset {
                inputs,
                worst_after_ns,
            } => Value::Text(format!(
                "yes, {inputs} inputs, idle at most {} ms after one",
                worst_after_ns / 1_000_000
            )),
            HidProof::NotMeasured(why) => Value::Missing(why.clone()),
        },
    );
    let presses = click_latencies(&p);
    row.cell(
        "person's presses (seen to press)",
        Value::Text(if presses.is_empty() {
            "none".to_owned()
        } else {
            presses
                .iter()
                .map(|(owner, what, ms)| format!("{owner} {what} {ms} ms"))
                .collect::<Vec<_>>()
                .join("; ")
        }),
    );
    let skipped = p
        .dialogs
        .iter()
        .filter(|(_, kind, _)| kind == "skip")
        .count();
    row.cell("passwords typed", typed_cell(&p.typed, skipped));
    Ok(proof)
}

/// The `passwords typed` cell: how many, and for each the owner, whose password it was, the
/// dialog's name field (as shown, then `>` what it read once the administrator's name went in) and
/// whether the dialog went after the Return (an administrator's dialog, the one watched); then how
/// many tries typed nothing (`skipped`: the focus read a text field not secure).
fn typed_cell(typed: &[Typed], skipped: usize) -> Value {
    if typed.is_empty() && skipped == 0 {
        return Value::Count(0);
    }
    let mut each = typed
        .iter()
        .map(|t| {
            let name = if t.name == "-" {
                String::new()
            } else {
                format!(" (name field {})", t.name)
            };
            let went = match t.went {
                Went::Unwatched => String::new(),
                Went::Stayed => ", the dialog stayed".to_owned(),
                Went::After(ms) => format!(", the dialog went {ms} ms after Return"),
            };
            format!("{}, the {}'s{name}{went}", t.owner, t.whose)
        })
        .collect::<Vec<_>>();
    if skipped > 0 {
        each.push(format!(
            "{skipped} {} typed nothing (the focus read a text field not secure)",
            if skipped == 1 { "try" } else { "tries" }
        ));
    }
    Value::Text(format!("{}: {}", typed.len(), each.join("; ")))
}

/// Where setup stands in the guest, in one look: `setup=` from its own `aterm ctl status` of the
/// first session `ctl ls` lists, and the progress record. A bare `status` from outside every tab
/// reads the window's active terminal, which a window System State fronts does not have. With
/// them, the person's presses and the app's processes so far (`acts`), which move a first launch
/// on while both stand still: the person at System Settings' Full Disk Access page, its sheet and
/// the app's question, and the app's quit and reopen (2026-10-28's 20:00 first-launch row:
/// setup= stood still from 154.6 s, and the wait ended at 219.3 s, mid-password, so the sheet's
/// press at 263.5 s and the reopen came after the row stopped looking).
fn setup_reading(g: &Guest) -> Result<crate::mac_nux_guest::Reading, Fail> {
    let sessions = g.run(&format!("{} ctl ls 2>/dev/null", g.aterm), STEP)?;
    let at = first_session(&sessions).map_or_else(String::new, |sid| format!("@{sid} "));
    let out = g.run(
        &format!(
            "{} ctl {at}status 2>/dev/null | head -1; echo '--- record'; cat \"$HOME/{SETUP_RECORD}\" 2>/dev/null; \
             echo; echo '--- acts'; cat {GUEST_NUX}/person.txt {GUEST_NUX}/{PROCS_RECORD} 2>/dev/null \
             | grep -E '^(click|typed|up|down) '; true",
            g.aterm
        ),
        STEP,
    )?;
    let (status, rest) = out.split_once("--- record").unwrap_or((&out, ""));
    let (record, moved) = rest.split_once("--- acts").unwrap_or((rest, ""));
    Ok(crate::mac_nux_guest::Reading {
        setup: setup_field(status).map(str::to_owned),
        record: read_record(record),
        acts: acts(moved),
    })
}

/// Wait for setup: until `setup=done` (the record's completion on a build without the field),
/// until it stood still for [`STALL_MS`] with nothing new from the person or in the app's
/// processes either (the one-minute no-progress hang detector, [`setup_reading`]), or until
/// [`WATCH`]; its cell under `label` counts from `from_ms` (ms before the wait began). The time
/// it ended, in ms after `from_ms`, once done.
fn wait_setup(g: &Guest, row: &mut Row, label: &str, from_ms: u64) -> Result<Option<u64>, Fail> {
    let mut stall = Stall::new(STALL_MS);
    let started = Instant::now();
    loop {
        stop::check()?;
        let reading = setup_reading(g)?;
        let now = millis(started.elapsed());
        let at = from_ms + now;
        match stall.see(now, &reading) {
            Waited::Done => {
                row.cell(format!("{label}setup done"), Value::Ms(at));
                return Ok(Some(at));
            }
            Waited::Stalled { stood, since_ms } => {
                row.cell(
                    format!("{label}setup done"),
                    Value::Missing(format!(
                        "it stood at {stood} from {} ms to {at} ms, with no new press by the person and no aterm process starting or going, and the one-minute no-progress hang detector ended the wait",
                        from_ms + since_ms
                    )),
                );
                return Ok(None);
            }
            Waited::Moving => {}
        }
        if started.elapsed() >= WATCH {
            row.cell(
                format!("{label}setup done"),
                Value::Missing(format!(
                    "still moving at {} min: {}",
                    WATCH.as_secs() / 60,
                    reading.said()
                )),
            );
            return Ok(None);
        }
        stop::sleep(SETUP_POLL)?;
    }
}

/// Present and input latency from the guest's own `aterm ctl metrics percentiles`, as cells under
/// `label`: `n_present`, `present_p50/p95/p99_ms`, `n_input` and `input_p50/p95/p99_ms`.
fn percentile_cells(g: &Guest, row: &mut Row, label: &str) -> Result<(), Fail> {
    let text = g.run(&format!("{} ctl metrics percentiles 2>&1", g.aterm), STEP)?;
    for kind in ["present", "input"] {
        let n = metric(&text, &format!("n_{kind}"));
        row.cell(
            format!("{label}n_{kind}"),
            or_missing(
                n.map(ms_value),
                Value::Count,
                "aterm ctl metrics percentiles did not answer",
            ),
        );
        let spread = ["p50", "p95", "p99"]
            .iter()
            .map(|p| metric(&text, &format!("{kind}_{p}_ms")))
            .collect::<Option<Vec<f64>>>();
        row.cell(
            format!("{label}{kind} p50/p95/p99"),
            match (n, spread) {
                (Some(n), Some(v)) if n > 0.0 => {
                    Value::Text(format!("{:.2}/{:.2}/{:.2} ms", v[0], v[1], v[2]))
                }
                (Some(_), _) => Value::Missing(format!("no {kind} sample while setup ran")),
                (None, _) => Value::Missing("aterm ctl metrics percentiles did not answer".into()),
            },
        );
    }
    Ok(())
}

/// aterm's state folder at one moment, `at`: its first two levels and its setup records, printed
/// whole to the run's output, and as a cell.
fn state_snapshot(g: &Guest, row: &mut Row, at: &str) -> Result<(), Fail> {
    let out = g.run(
        "d=\"$HOME/Library/Application Support/aterm\"; [ -d \"$d\" ] || { echo '(absent)'; exit 0; }; \
         cd \"$d\" && find . -mindepth 1 -maxdepth 2 | sort | head -80; \
         for f in doctor/*.json; do [ -f \"$f\" ] && { echo \"== $f\"; head -c 1500 \"$f\"; echo; }; done; true",
        STEP,
    )?;
    println!("mac nux: aterm's state folder at {at}:\n{}", out.trim_end());
    row.cell(format!("state at {at}"), Value::Text(state_summary(&out)));
    Ok(())
}

/// [`state_snapshot`]'s listing in a few words: how many entries, and the setup records.
#[must_use]
pub fn state_summary(listing: &str) -> String {
    if listing.trim() == "(absent)" {
        return "no aterm folder".to_owned();
    }
    let entries: Vec<&str> = listing
        .lines()
        .take_while(|l| !l.starts_with("== "))
        .map(|l| l.trim().trim_start_matches("./"))
        .filter(|l| !l.is_empty())
        .collect();
    let records: Vec<&str> = entries
        .iter()
        .filter_map(|l| l.strip_prefix("doctor/"))
        .collect();
    format!(
        "{} entries; doctor/: {}",
        entries.len(),
        if records.is_empty() {
            "none".to_owned()
        } else {
            records.join(", ")
        }
    )
}

/// `xcode-select -p`'s answer (its output, then an `exit=<status>` line): the developer folder, or
/// that it fails.
#[must_use]
pub fn xcode_select_text(out: &str) -> String {
    let mut lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let status = lines
        .pop()
        .and_then(|l| l.strip_prefix("exit="))
        .unwrap_or("?");
    let first = lines.first().copied().unwrap_or("");
    if status == "0" {
        first.to_owned()
    } else {
        format!("fails (exit {status}: {})", squeeze(first, 80))
    }
}

/// What the guest saw of the ALab vault: `alab-secrets.local`'s address (`none` unresolved), the
/// HTTP status of a GET of its URL (`000`: no answer), and the `_alab-vault._tcp` adverts it
/// heard, placed ([`guest_heard`]: why its browse shows none, where it did not run). Unreached
/// when nothing answered and every advert heard is this Mac's own (its port refused by the
/// guest's [`PF_GUARD`]); why not, else.
///
/// # Errors
/// The guest reached it, heard an advert that is not this Mac's, or its browse did not run, so no
/// advert is unproven: the row must not go on.
pub fn alab_probe_verdict(
    address: &str,
    http: &str,
    adverts: &Result<advert::Split, String>,
) -> Result<String, String> {
    let adverts = adverts.as_ref().map_err(|why| {
        format!(
            "the guest's browse of {ALAB_SERVICE} did not run ({why}), so no advert is unproven"
        )
    })?;
    if http.trim() != "000" || !adverts.others.is_empty() {
        return Err(format!(
            "the guest reached an ALab vault (alab-secrets.local {address}, HTTP {}, {} {ALAB_SERVICE} advert(s){})",
            http.trim(),
            adverts.others.len(),
            if adverts.others.is_empty() {
                String::new()
            } else {
                format!(": {}", adverts.others.join(", "))
            }
        ));
    }
    let own = adverts.this_macs_text();
    Ok(format!(
        "Unreached (alab-secrets.local {}, no answer, no {ALAB_SERVICE} advert{})",
        if address == "none" {
            "unresolved"
        } else {
            address
        },
        if own.is_empty() {
            String::new()
        } else {
            format!(" but {own}")
        }
    ))
}

/// Whether a `dns-sd -B` transcript shows a browse that ran: its `...STARTING...` line, which
/// `dns-sd` prints once its browse call succeeded, and no failed call (`DNSService call failed`,
/// mDNSResponder unreachable) or error code. A browse that hears nothing prints no line after
/// `...STARTING...` (the `Timestamp … Instance Name` header comes with the first answer), so
/// that line is the proof it looked.
///
/// # Errors
/// Why the transcript shows no browse that ran.
pub fn browse_ran(transcript: &str) -> Result<(), String> {
    if let Some(line) = transcript
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c.is_control()))
        .find(|l| l.contains("DNSService call failed") || l.contains("Error code"))
    {
        return Err(format!("dns-sd failed: {}", squeeze(line, 80)));
    }
    if transcript.lines().any(|l| l.contains("...STARTING...")) {
        return Ok(());
    }
    let first = transcript
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c.is_control()))
        .find(|l| !l.is_empty());
    Err(match first {
        None => "its transcript is empty".to_owned(),
        Some(line) => format!("no browse started: {}", squeeze(line, 80)),
    })
}

/// The adverts (`Add` rows) a `dns-sd -B` transcript heard ([`advert::heard`]).
///
/// # Errors
/// It shows no browse that ran ([`browse_ran`]): its count would prove nothing.
pub fn browse_adds(transcript: &str) -> Result<Vec<advert::Heard>, String> {
    browse_ran(transcript)?;
    Ok(advert::heard(transcript))
}

/// The adverts a `dns-sd -B` on this Mac heard over a tart bridge (a vmnet `bridge1NN`): a
/// guest's, or this Mac's own registration announced there too ([`host_guests`] tells which).
/// `name_of` names an interface index.
#[must_use]
pub fn bridge_heard(browse: &str, name_of: &dyn Fn(u32) -> Option<String>) -> Vec<advert::Heard> {
    advert::heard(browse)
        .into_iter()
        .filter(|h| {
            name_of(h.interface).is_some_and(|name| {
                name.strip_prefix("bridge")
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|n| n >= 100)
            })
        })
        .collect()
}

/// The ALab guard every row ends with: the guest's look at the ALab vault ([`alab_probe_verdict`])
/// and this Mac's for a guest's production advert ([`host_guests`]); either one failing fails
/// the row.
fn alab_guard(g: &Guest, run: &Path, row: &mut Row) -> Result<(), Fail> {
    let seen = g.run(
        &format!(
            "/usr/bin/python3 -c \"import socket\ntry: print(socket.getaddrinfo('{ALAB_HOST}', 7484)[0][4][0])\nexcept Exception: print('none')\"; \
             curl -s -o /dev/null -m 3 -w '%{{http_code}}\\n' '{url}'",
            url = aterm_types::macos_setup::DEFAULT_VAULT_URL
        ),
        STEP,
    )?;
    let mut lines = seen.lines().map(str::trim);
    let address = lines.next().unwrap_or("none").to_owned();
    let http = lines.next().unwrap_or("000").to_owned();
    let adverts = guest_heard(g, ALAB_SERVICE)?;
    match alab_probe_verdict(&address, &http, &adverts) {
        Ok(text) => row.cell("ALab probe", Value::Text(text)),
        Err(why) => row.fail(why),
    }
    match host_guests(run, ALAB_SERVICE)? {
        Err(why) => row.fail(format!(
            "this Mac's browse of {ALAB_SERVICE} did not run ({why}), so no advert from a guest is unproven"
        )),
        Ok(heard) if heard.others.is_empty() => {
            let own = heard.this_macs_text();
            row.cell(
                "production adverts from guests",
                Value::Text(if own.is_empty() {
                    "none".into()
                } else {
                    format!("none ({own})")
                }),
            );
        }
        Ok(heard) => row.fail(format!(
            "this Mac heard a guest advertise {ALAB_SERVICE}: {}",
            heard.others.join(", ")
        )),
    }
    Ok(())
}

/// How long each browse listens, in the guest and on this Mac.
const BROWSE_SECS: u64 = 3;

/// The transcript of a [`BROWSE_SECS`] `dns-sd -B <service> local.` in the guest (the guest
/// helper's `browse`, `src/guest/steps.rs`), for [`guest_heard`]: "" when it left none.
fn guest_browse(g: &Guest, service: &str) -> Result<String, Fail> {
    g.sh(
        &format!("{GUEST_NUX}/{GUEST_HELPER} browse {BROWSE_SECS} {GUEST_NUX} {service}"),
        STEP,
    )?;
    g.read(&format!("{GUEST_NUX}/heard{service}.txt"))
}

/// The adverts of `service` the guest heard ([`guest_browse`]), each placed ([`advert::split`]):
/// resolved in the guest on the interface it came over (the guest helper's `dns-sd`), against
/// this Mac's own addresses, so this Mac's own registration the guest hears over tart's bridge is
/// named apart from one the guest made. `Err` says why its browse shows none, where it did not
/// run ([`browse_ran`]).
fn guest_heard(g: &Guest, service: &str) -> Result<Result<advert::Split, String>, Fail> {
    let heard = match browse_adds(&guest_browse(g, service)?) {
        Ok(heard) => heard,
        Err(why) => return Ok(Err(why)),
    };
    let out = format!("{GUEST_NUX}/resolved{service}.txt");
    let split = advert::split(
        &heard,
        service,
        &|index| Some(format!("the guest's interface {index}")),
        &advert::this_macs_addresses(),
        &mut |args: &[String]| {
            let words: Vec<String> = args.iter().map(|a| crate::util::shell_quote(a)).collect();
            g.sh(
                &format!(
                    "{GUEST_NUX}/{GUEST_HELPER} dns-sd {} {out} {}",
                    advert::RESOLVE_SECS,
                    words.join(" ")
                ),
                STEP,
            )?;
            g.read(&out)
        },
    )?;
    Ok(Ok(split))
}

/// What a [`BROWSE_SECS`] `dns-sd -B <service> local.` on this Mac heard (its transcript, under
/// `run`), for [`host_guests`]; `Err` says why it shows no browse that ran ([`browse_ran`]).
fn host_heard(run: &Path, service: &str) -> Result<Result<String, String>, Fail> {
    let heard = host_dns_sd(
        &run.join(format!("host-browse{service}.txt")),
        &["-B".to_owned(), service.to_owned(), "local.".to_owned()],
        BROWSE_SECS,
    )?;
    Ok(browse_ran(&heard).map(|()| heard))
}

/// The adverts of `service` this Mac heard over a tart bridge ([`host_heard`], [`bridge_heard`]),
/// each placed ([`advert::split`]): resolved here on the bridge it came over, against this Mac's
/// own addresses. Another's is a guest's; this Mac's own is a registration of its own that
/// mDNSResponder announces on the bridge as on every interface. `Err` says why this Mac's browse
/// shows none, where it did not run.
fn host_guests(run: &Path, service: &str) -> Result<Result<advert::Split, String>, Fail> {
    let transcript = match host_heard(run, service)? {
        Ok(transcript) => transcript,
        Err(why) => return Ok(Err(why)),
    };
    let file = run.join(format!("host-resolved{service}.txt"));
    let split = advert::split(
        &bridge_heard(&transcript, &crate::sys::interface_name),
        service,
        &crate::sys::interface_name,
        &advert::this_macs_addresses(),
        &mut |args: &[String]| host_dns_sd(&file, args, advert::RESOLVE_SECS),
    )?;
    Ok(Ok(split))
}

/// One `dns-sd ARGS...` on this Mac for `secs`, under `script -q -t 0` into `file`, then its
/// group killed: its transcript ("" when it left none). `script -t 0` writes the transcript at
/// each line: with its default 30 s flush, the group's kill left it empty, which read as
/// "nothing heard" (measured 2026-10-28).
fn host_dns_sd(file: &Path, args: &[String], secs: u64) -> Result<String, Fail> {
    let _ = std::fs::remove_file(file);
    let mut child = aterm_uds::spawnfd::command("/usr/bin/script")
        .args(["-q", "-t", "0"])
        .arg(file)
        .arg("/usr/bin/dns-sd")
        .args(args)
        .setsid(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Fail::die(format!("dns-sd on this Mac: {e}")))?;
    stop::sleep(Duration::from_secs(secs))?;
    let _ = crate::sys::kill_group(i32::try_from(child.id()).unwrap_or(0), libc::SIGTERM);
    crate::mac::reap_within(&mut child, STEP);
    Ok(std::fs::read_to_string(file).unwrap_or_default())
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
    "mkdir -p /var/tmp/aterm-nux && : > /var/tmp/aterm-nux/prompts && cat >> ~/.zshrc <<'EOF'
zmodload zsh/datetime
nux_prompt() { print -r -- \"$EPOCHREALTIME $$ ${TERM_PROGRAM:-none}\" >> /var/tmp/aterm-nux/prompts; print -r -- nux-shell-ready }
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
    if scenario.grows_disk() {
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
            "rm -rf {GUEST_NUX} && mkdir -p {GUEST_NUX} && cp '{share}'/drive.exp '{share}'/notty.pl '{share}'/install.sh '{share}'/{GUEST_HELPER} {GUEST_NUX}/ \
             && chmod 755 {GUEST_NUX}/{GUEST_HELPER}",
            share = crate::mac::GUEST_SHARE
        ),
        STEP,
    )?;
    vm.sh(
        &format!("cat > {GUEST_NUX}/pf.conf <<'EOF'\n{PF_GUARD}EOF"),
        STEP,
    )?;
    let rules = vm.sh(
        &root_command(
            standard,
            &format!("/sbin/pfctl -q -E -f {GUEST_NUX}/pf.conf 2>/dev/null; /sbin/pfctl -q -s rules 2>/dev/null"),
        ),
        STEP,
    )?;
    if !rules.contains("7484") {
        return Err(Fail::die(format!(
            "the guest's packet filter did not take the vault port's null route: {}",
            squeeze(rules.trim(), 200)
        )));
    }
    let uid = vm.sh("id -u", STEP)?.trim().to_owned();
    let developer = vm
        .exec("xcode-select -p 2>&1; echo \"exit=$?\"", STEP)
        .map_err(Fail::die)?;
    row.cell(
        "xcode-select -p",
        Value::Text(xcode_select_text(&String::from_utf8_lossy(
            &developer.stdout,
        ))),
    );
    if scenario.needs_app() && !standard {
        let started = Instant::now();
        vm.sh(
            &format!("ditto -x -k '{zip}' /Applications && codesign --verify --strict {app}"),
            STEP,
        )?;
        row.cell("app unpacked", Value::Ms(millis(started.elapsed())));
    }
    if scenario.grows_disk() {
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
        uid,
        mark_ms: std::cell::Cell::new(None),
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
    row.cell("presence read", fault::presence_cell(&out));
    drive_cells(row, "", &d);
    open_rows_cell(row, "", &d, &out);
    lines_cell(row, "", &out);
    let report = read_report_json(&out);
    if d.exit != Some(0) {
        not_ok_cell(row, "", "its --json", report.as_ref());
    }
    report_cells(row, "", report);
    around.cells(g, row, "")?;
    phase_cells(row, "", &g.log()?, "aterm init:");
    let mut failed = false;
    for (tag, args, label) in [
        ("init-noop", "init", "no-op: "),
        ("init-check", "init --check", "check: "),
    ] {
        let mark = g.log_mark()?;
        let (d, out) = g.drive(tag, &sudo, &format!("{} {args}", g.aterm))?;
        drive_cells(row, label, &d);
        open_rows_cell(row, label, &d, &out);
        lines_cell(row, label, &out);
        g.phase_cells_since(row, label, mark, "aterm init:")?;
        failed |= d.exit != Some(0);
    }
    if failed {
        checked_not_ok(g, row)?;
    }
    Ok(())
}

/// The rows a text `aterm init` that exited non-zero left, named by a read-only
/// `aterm init --check --json` (a text run names no ids).
fn checked_not_ok(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let (_, out) = g.notty(
        "init-check-json",
        &format!("{} init --check --json", g.aterm),
    )?;
    not_ok_cell(
        row,
        "",
        "init --check --json",
        read_report_json(&out).as_ref(),
    );
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
    row.cell("presence read", fault::presence_cell(&out));
    drive_cells(row, "", &d);
    open_rows_cell(row, "", &d, &out);
    lines_cell(row, "", &out);
    around.cells(g, row, "")?;
    if d.exit != Some(0) {
        checked_not_ok(g, row)?;
    }
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
    row.cell("presence read", fault::presence_cell(&out));
    drive_cells(row, "", &d);
    open_rows_cell(row, "", &d, &out);
    lines_cell(row, "", &out);
    around.cells(g, row, "")?;
    if d.exit != Some(0) {
        checked_not_ok(g, row)?;
    }
    phase_cells(row, "", &g.log()?, "aterm init:");
    Ok(())
}

/// A shell test that exits 0 while another process holds the account's package store lock
/// (`flock`), 1 while it is free, 2 when there is none.
const STORE_HELD: &str = "/usr/bin/perl -MFcntl=:flock -e 'open(F, \"<\", $ARGV[0]) or exit 2; \
     exit(flock(F, LOCK_EX | LOCK_NB) ? 1 : 0)' \"$HOME/Library/Application Support/aterm/pkg/store.lock\"";

/// What a run beside the pass said, for its row: a JSON report's completeness and each
/// unresolved row with its outcome and why, else its printed lines as a terminal shows them
/// (escape sequences dropped, a line rewritten after a carriage return read as its last
/// rewrite) joined, at most 600 characters.
#[must_use]
pub fn said_beside_the_pass(out: &str) -> String {
    let report = out.lines().rev().find_map(|line| {
        let v: aterm_json::Value = aterm_json::from_str(line.trim()).ok()?;
        v.get("items")?.as_array()?;
        Some(v)
    });
    if let Some(v) = report {
        fn text(v: Option<&aterm_json::Value>) -> Option<&str> {
            v.and_then(aterm_json::Value::as_str)
        }
        let items = v
            .get("items")
            .and_then(aterm_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let unresolved: Vec<String> = v
            .get("readiness")
            .and_then(|r| r.get("unresolved"))
            .and_then(aterm_json::Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(aterm_json::Value::as_str)
            .map(|id| {
                let item = items.iter().find(|i| text(i.get("id")) == Some(id));
                let outcome = text(item.and_then(|i| i.get("outcome"))).unwrap_or("?");
                let why = text(item.and_then(|i| i.get("why")))
                    .or_else(|| text(item.and_then(|i| i.get("from"))))
                    .unwrap_or("");
                format!(
                    "{id} {outcome}: {}",
                    why.chars().take(140).collect::<String>()
                )
            })
            .collect();
        let complete = v
            .get("complete")
            .and_then(aterm_json::Value::as_bool)
            .map_or("?", |c| if c { "true" } else { "false" });
        return format!("complete {complete}; unresolved: {}", unresolved.join("; "))
            .replace('|', "\u{a6}")
            .chars()
            .take(600)
            .collect();
    }
    let mut plain = String::with_capacity(out.len());
    let mut rewrite = false;
    let mut chars = out.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // A CSI or OSC sequence, or a two-character escape: up to its final byte.
                match chars.next() {
                    Some('[') => {
                        for c in chars.by_ref() {
                            if ('@'..='~').contains(&c) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        for c in chars.by_ref() {
                            if c == '\u{7}' || c == '\\' {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // A carriage return the next character does not end the line after: the terminal
            // rewrites this line from its start.
            '\r' => rewrite = true,
            '\n' => {
                rewrite = false;
                plain.push('\n');
            }
            c => {
                if std::mem::take(&mut rewrite) {
                    let start = plain.rfind('\n').map_or(0, |at| at + 1);
                    plain.truncate(start);
                }
                plain.push(c);
            }
        }
    }
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines
        .join(" / ")
        .replace('|', "\u{a6}")
        .chars()
        .take(600)
        .collect()
}

/// `pass-held`: `aterm pkg install --default-set` started in the background and, once it holds
/// the store, what doctor and init do beside it: the ones that never wait first (no terminal, and
/// `--no-wait`), then doctor on a terminal, which waits for the pass. Each with whether the pass
/// still held the store as it started; NOT RUN when the pass never took the store.
fn pass_held(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.run(
        // Its own session, so the end of this `tart exec` ends nothing of it.
        &format!(
            "/usr/bin/perl -MPOSIX -e 'fork and exit; POSIX::setsid(); open(P, \">\", shift) and \
             print P $$; close P; exec @ARGV' {GUEST_NUX}/pass.pid {} pkg install --default-set \
             > {GUEST_NUX}/pass.out 2>&1 < /dev/null",
            g.aterm
        ),
        STEP,
    )?;
    let started = Instant::now();
    loop {
        if g.run(&format!("{STORE_HELD} && echo held || true"), STEP)?
            .trim()
            == "held"
        {
            break;
        }
        if started.elapsed() >= STEP {
            return Err(not_run(format!(
                "aterm pkg install --default-set held no store lock within {} s",
                STEP.as_secs()
            )));
        }
        stop::sleep(POLL)?;
    }
    row.cell("pass took the store", Value::Ms(millis(started.elapsed())));
    let around = Around::take(g)?;
    for (tag, tty, args, label) in [
        (
            "held-doctor-json",
            false,
            "doctor --json",
            "doctor --json: ",
        ),
        ("held-init", false, "init", "init: "),
        (
            "held-init-no-wait",
            false,
            "init --no-wait",
            "init --no-wait: ",
        ),
        ("held-doctor-tty", true, "doctor", "doctor on a terminal: "),
    ] {
        let held = g.run(&format!("{STORE_HELD} && echo yes || echo no"), STEP)?;
        row.cell(
            format!("{label}pass held the store at its start"),
            Value::Text(held.trim().to_owned()),
        );
        let cmd = format!("{} {args}", g.aterm);
        let (d, out) = if tty {
            g.drive(tag, &format!("NUX_SUDO={}", g.sudo_password()), &cmd)?
        } else {
            g.notty(tag, &cmd)?
        };
        drive_cells(row, label, &d);
        lines_cell(row, label, &out);
        row.cell(
            format!("{label}said"),
            Value::Text(said_beside_the_pass(&out)),
        );
    }
    around.cells(g, row, "")?;
    let pass = g.run(
        &format!(
            "p=$(cat {GUEST_NUX}/pass.pid); n=0; while kill -0 $p 2>/dev/null; do n=$((n+1)); \
             [ $n -gt {limit} ] && break; sleep 0.1; done; kill -0 $p 2>/dev/null && echo running || echo ended",
            limit = ROW.as_secs() * 10
        ),
        ROW + STEP,
    )?;
    row.cell("pass", Value::Text(pass.trim().to_owned()));
    Ok(())
}

/// How many runs of the bare `aterm doctor` and of `aterm doctor --read-only --json` `doctor`
/// times.
const READ_ONLY_RUNS: usize = 10;

/// Quit System Settings and wait (up to 30 s) for it to be gone.
const QUIT_SETTINGS: &str = "pkill -x 'System Settings'; n=0; while pgrep -x 'System Settings' >/dev/null; do \
                             n=$((n+1)); [ $n -gt 300 ] && break; sleep 0.1; done";

/// The p50 and p95 cells of `ends` (each run's end, ms) under `label`.
fn spread_cells(row: &mut Row, label: &str, mut ends: Vec<u64>, why: &str) {
    ends.sort_unstable();
    for pct in [50, 95] {
        row.cell(
            format!("{label}p{pct} of {}", ends.len()),
            or_missing(nearest_rank(&ends, pct), Value::Ms, why),
        );
    }
}

/// `doctor`: on a terminal [`READ_ONLY_RUNS`] times (Developer Tools never granted in a fresh
/// clone; System Settings quit between runs), then `--json | cat` with no terminal, each with
/// whether System Settings opened and the phases its own summary line names, then
/// [`READ_ONLY_RUNS`] read-only reports.
fn doctor(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let opened = |g: &Guest| -> Result<Value, Fail> {
        Ok(Value::Text(
            if g.system_settings()? { "yes" } else { "no" }.into(),
        ))
    };
    let label = "terminal: ";
    let mut ends = Vec::new();
    for at in 0..READ_ONLY_RUNS {
        let mark = g.log_mark()?;
        let (d, out) = g.drive(
            &format!("doctor-tty-{at}"),
            &format!("NUX_SUDO={}", g.sudo_password()),
            &format!("{} doctor", g.aterm),
        )?;
        if at == 0 {
            drive_cells(row, label, &d);
            lines_cell(row, label, &out);
            g.phase_cells_since(row, label, mark, "aterm doctor")?;
            row.cell(format!("{label}System Settings opened"), opened(g)?);
        }
        g.run(QUIT_SETTINGS, STEP)?;
        match d.end_ms {
            Some(end) if d.hang.is_none() => ends.push(end),
            _ => {
                if at > 0 {
                    drive_cells(row, &format!("terminal run {}: ", at + 1), &d);
                }
                break;
            }
        }
    }
    spread_cells(row, label, ends, "no terminal run ended");
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
    spread_cells(row, label, ends, "no read-only run ended");
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
        first_present_value(&metrics),
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

/// `launch-repairs`: a copied command planted at `~/.local/bin/aterm` (a copy of the app's
/// executable, as a hand install leaves one), then the app opened until its launch repairs log
/// (after its seed), and quit and opened again: each launch's `launch repairs:` lines in
/// `aterm.log`, how long after the prompt they came, and what `~/.local/bin/aterm` is after it.
fn launch_repairs(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    g.sh(PROMPT_MARKER, STEP)?;
    g.sh(
        &format!("mkdir -p ~/.local/bin && cp {} ~/.local/bin/aterm", g.aterm),
        STEP,
    )?;
    let around = Around::take(g)?;
    let said = |mark: usize| -> Result<Vec<String>, Fail> {
        Ok(appended(&g.log()?, mark)
            .lines()
            .filter_map(|line| {
                line.split_once("launch repairs: ")
                    .map(|(_, said)| said.to_owned())
            })
            .collect())
    };
    for (tag, label) in [("first", "first launch: "), ("second", "second launch: ")] {
        let mark = g.log_mark()?;
        let (_, prompt) = open_until_prompt(g, tag)?;
        prompt_cell(row, &format!("{label}open to prompt"), prompt);
        metrics_cells(g, row, label)?;
        let shown = g.run(&format!("{} ctl metrics percentiles 2>&1", g.aterm), STEP)?;
        row.cell(
            format!("{label}presents"),
            match [
                "n_present",
                "present_p50_ms",
                "present_p95_ms",
                "present_p99_ms",
            ]
            .map(|key| metric(&shown, key))
            {
                [Some(n), Some(p50), Some(p95), Some(p99)] => Value::Text(format!(
                    "n_present={n} present_p50/p95/p99={p50:.2}/{p95:.2}/{p99:.2} ms"
                )),
                _ => Value::Missing("aterm ctl metrics percentiles did not answer".to_owned()),
            },
        );
        let prompted = Instant::now();
        let mut lines = said(mark)?;
        while lines.is_empty() && prompted.elapsed() < WATCH {
            stop::sleep(Duration::from_secs(2))?;
            lines = said(mark)?;
        }
        let ran = millis(prompted.elapsed());
        if !lines.is_empty() {
            // The rest of one run's lines, logged together.
            stop::sleep(Duration::from_secs(1))?;
            lines = said(mark)?;
        }
        row.cell(
            format!("{label}repairs logged after the prompt"),
            if lines.is_empty() {
                Value::Missing(format!(
                    "no `launch repairs:` line in the {} min watched",
                    WATCH.as_secs() / 60
                ))
            } else {
                Value::Ms(ran)
            },
        );
        row.cell(
            format!("{label}repairs"),
            Value::Text(if lines.is_empty() {
                "none".to_owned()
            } else {
                lines.join("; ")
            }),
        );
        let local = g.sh(
            "if [ -L ~/.local/bin/aterm ]; then echo link; elif [ -e ~/.local/bin/aterm ]; then \
             echo file; else echo none; fi; ls ~/.local/bin | grep -c moved-aside || true",
            STEP,
        )?;
        let mut local = local.lines().map(str::trim);
        row.cell(
            format!("{label}~/.local/bin/aterm"),
            Value::Text(format!(
                "{}, {} moved aside",
                local.next().unwrap_or("unread"),
                local.next().unwrap_or("?")
            )),
        );
        quit_app(g)?;
    }
    around.cells(g, row, "")?;
    Ok(())
}

/// `first-launch`: the app opened by a person (with `--person`, one at the console), then
/// watched until setup is done or stood still a minute with nothing new from the person or in the
/// app's processes ([`wait_setup`]).
fn first_launch(g: &Guest, ask: &Ask, row: &mut Row) -> Result<(), Fail> {
    g.sh(PROMPT_MARKER, STEP)?;
    let around = Around::take(g)?;
    state_snapshot(g, row, "boot")?;
    g.watch_record("watch.txt", None)?;
    g.watch_procs()?;
    if ask.person {
        person_start(g, ask.later)?;
    }
    // What the app asks after the ALab vault while it is watched: System State's watch for the
    // vault looks on a Mac that has none.
    start_mdns(g)?;
    let (opened, prompt) = open_until_prompt(g, "first")?;
    let open_ms = secs_to_ms(opened);
    g.mark_ms.set(Some(open_ms));
    prompt_cell(row, "open to prompt", prompt);
    let present = first_present_ms(g)?;
    metrics_cells(g, row, "")?;
    state_snapshot(g, row, "window focus")?;
    let waited_from = g.clock_ms()?.saturating_sub(open_ms);
    let done = wait_setup(g, row, "", waited_from)?;
    reopen_cells(g, ask, row, open_ms)?;
    percentile_cells(g, row, "during setup: ")?;
    let mdns = stop_mdns(g)?;
    row.cell(
        "mDNS questions for the ALab vault",
        Value::Count(alab_questions(&mdns) as u64),
    );
    let desktop_read_passed = crate::mac::desktop_read(&g.vm, &format!("{GUEST_NUX}/tccd.ndjson"))?;
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
    local_network_cell(g, row, open_ms)?;
    let log = g.log()?;
    if done.is_none() {
        // What the app said while setup did not finish, whole, for the run's record: the cause
        // of a launch that never starts setup is in it (the admin first-launch gap, SCR-1).
        let lines: Vec<&str> = log.lines().collect();
        println!(
            "mac nux: aterm.log's last {} lines (setup not done):\n{}",
            lines.len().min(LOG_SHOWN),
            lines[lines.len().saturating_sub(LOG_SHOWN)..].join("\n")
        );
    }
    let launch = read_launch(&log, open_ms);
    // A line the watch never saw is the app's silence over it, said with the watch's length.
    let watched = done.unwrap_or_else(|| g.clock_ms().unwrap_or(open_ms).saturating_sub(open_ms));
    let within = format!("in the {} s watched", watched / 1000);
    row.cell(
        "first log line",
        or_missing(
            launch.first_log_ms,
            Value::Ms,
            &format!("aterm.log has no line after the open {within}"),
        ),
    );
    // First present after the open: its first log line (Rust's main) and main to first present.
    let present_ms = launch.first_log_ms.zip(present).map(|(log, p)| log + p);
    stage_cells(g, row, open_ms, present_ms)?;
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
        "System State's Local Network ask",
        or_missing(
            launch.local_network_asked_ms,
            Value::Ms,
            &format!("no `system state: Local Network asked for` line {within}"),
        ),
    );
    row.cell(
        "Full Disk Access granted",
        or_missing(
            launch.fda_granted_ms,
            Value::Ms,
            &format!(
                "not granted {within}{}",
                if ask.person {
                    ""
                } else {
                    " (nobody answers in the guest)"
                }
            ),
        ),
    );
    row.cell(
        "folder dialogs once Full Disk Access was granted (target 0)",
        match launch.fda_granted_ms {
            Some(granted) => Value::Count(folder_prompts_after(
                &tcc_prompt_events(&tcc),
                open_ms + granted,
            ) as u64),
            None => Value::Missing("Full Disk Access was not granted in the walk".into()),
        },
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
    person_stage_cells(g, ask, row)?;
    state_snapshot(g, row, "the end")?;
    around.cells(g, row, "")?;
    Ok(())
}

/// What a first launch's row saw of System Settings' Quit & Reopen: the person's press on the
/// sheet, and the app's processes across it ([`reopen_values`]); then what the person did on the
/// Developer Tools page the walk opens next ([`devtools_value`]).
fn reopen_cells(g: &Guest, ask: &Ask, row: &mut Row, open_ms: u64) -> Result<(), Fail> {
    let procs = g.procs_read()?;
    let person = if ask.person {
        Some(read_person(&g.read(&format!("{GUEST_NUX}/person.txt"))?))
    } else {
        None
    };
    let presses = person.as_ref().map(reopen_presses);
    let end_ms = g.clock_ms()?;
    let log = g.log()?;
    for (name, value) in reopen_values(presses.as_ref(), ask.later, &procs, &log, open_ms, end_ms) {
        row.cell(name, value);
    }
    row.cell(
        "Developer Tools",
        devtools_value(
            person.as_ref().map(devtools_acts).as_ref(),
            &log,
            open_ms,
            end_ms,
        ),
    );
    Ok(())
}

/// The start of aterm's log line for the end of a drag of the helper panel's tile
/// (`aterm_gui::devtools_helper`): what macOS did with the drop, in its brackets.
const DRAG_ENDED: &str = "devtools: the owner's drag of aterm ended (";

/// How long after an add of the person's a System Settings password dialog is still that add's,
/// when it comes after everything the person said of the Developer Tools page: one the page asks
/// for comes within seconds of the add.
const ASKED_WITHIN_MS: u64 = 60_000;

/// The `Developer Tools` cell: what the person did on System Settings' Developer Tools page
/// (`acts`; `None`: no person at the console), each time in ms after the open (`open_ms`, epoch
/// ms on the guest's clock):
/// - how aterm came onto the list: `aterm added by drag` (or `by +`) when the page came to hold
///   aterm for good (the last run of states that hold it, after the person's first add: a state
///   that held it and then lacked it again is no add), with how long after that add, and for a
///   drag what aterm's own log (`log`) said the drop did ([`DRAG_ENDED`]; NOT MEASURED when the
///   log holds no line from the page on); `on the list when the page came` when it held aterm
///   before any add; or why it is not there by `end_ms` (each add and its steps, and what stopped
///   a try);
/// - `no drag` when the person added aterm with + alone, and why (no window showed the tile);
/// - `password`: each password the person typed into System Settings after its first add (and
///   before its last word of the page, or within [`ASKED_WITHIN_MS`] of its last add); `asked,
///   not typed` for a dialog it read there and typed nothing into; `not asked` only when it had
///   a password to type and read no dialog;
/// - `switch on`: the person's press of aterm's switch there, or the switch as the page read it;
/// - `page seen`: when the person first read the page.
///
/// A person without Accessibility reads nothing, and a page whose list did not read when the
/// person last said it (with no add) shows nothing of aterm: both NOT MEASURED. A page the person
/// never read is `not seen`, with how long the row watched. Pure.
#[must_use]
pub fn devtools_value(acts: Option<&DevtoolsActs>, log: &str, open_ms: u64, end_ms: u64) -> Value {
    let Some(a) = acts else {
        return Value::Missing("no person at the console (--person puts one there)".into());
    };
    match a.trusted {
        Some(true) => {}
        Some(false) => {
            return Value::Missing(
                "the person had no Accessibility grant, so it read no window".into(),
            );
        }
        None => return Value::Missing("the person's record says none of its grants".into()),
    }
    let after = |at: u64| at.saturating_sub(open_ms);
    let watched = format!("in the {} s watched", after(end_ms) / 1000);
    let Some((seen, _)) = a.panes.first() else {
        return Value::Text(format!("not seen {watched}"));
    };
    let holds = |state: &str| state.starts_with("list=holds");
    let first_add = a.adds.first().map(|(at, _)| *at);
    // The start of the last run of states that hold aterm, when the last state holds it.
    let listed = {
        let mut since = None;
        for (at, state) in a
            .panes
            .iter()
            .filter(|(at, _)| first_add.is_none_or(|t| *at >= t))
        {
            since = if holds(state) {
                since.or(Some(*at))
            } else {
                None
            };
        }
        since
    };
    let from_page: Vec<_> = log_lines(log)
        .into_iter()
        .filter(|l| l.at_ms >= *seen)
        .collect();
    // What aterm's log said of a drag's drop.
    let said = |add: Add| match add {
        Add::Plus => String::new(),
        Add::Drag if from_page.is_empty() => {
            " (aterm's log: NOT MEASURED, no line of it from the page on)".to_owned()
        }
        Add::Drag => match from_page.iter().find(|l| l.message.starts_with(DRAG_ENDED)) {
            Some(l) => {
                let said = &l.message[DRAG_ENDED.len()..];
                format!(" (aterm's log: {})", said.strip_suffix(')').unwrap_or(said))
            }
            None => " (aterm's log: no drag ended)".to_owned(),
        },
    };
    let mut parts = Vec::new();
    match (first_add, listed) {
        (None, Some(at)) if at == *seen => {
            parts.push("aterm on the list when the page came".into())
        }
        (None, Some(at)) => parts.push(format!(
            "aterm on the list {} ms, with no add by the person",
            after(at)
        )),
        (Some(_), Some(at)) => {
            let (t, add) = a
                .adds
                .iter()
                .rev()
                .find(|(t, _)| *t <= at)
                .copied()
                .unwrap_or(a.adds[0]);
            parts.push(format!(
                "aterm added by {} {} ms, {} ms after the {}{}",
                add.words(),
                after(at),
                at.saturating_sub(t),
                add.words(),
                said(add)
            ));
        }
        (Some(_), None) => {
            let tries = a
                .adds
                .iter()
                .map(|(t, add)| format!("{} pressed {} ms{}", add.words(), after(*t), said(*add)))
                .chain(a.steps.iter().map(|(t, step)| {
                    let step = match step.as_str() {
                        DEVTOOLS_GOTO => "the path's Return",
                        DEVTOOLS_OPEN => "Open",
                        DEVTOOLS_CANCEL => "Cancel",
                        other => other,
                    };
                    format!("{step} {} ms", after(*t))
                }))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("aterm not on the list {watched}: {tries}"));
        }
        (None, None) => {
            if let Some(described) = &a.unread {
                return Value::Missing(format!(
                    "the page's list did not read (it read {described}); page seen {} ms",
                    after(*seen)
                ));
            }
            parts.push(format!("aterm not added {watched}"));
        }
    }
    let note = |start: &str| {
        a.notes
            .iter()
            .map(|(_, note)| note.as_str())
            .find(|note| note.starts_with(start))
    };
    if first_add.is_some() && !a.adds.iter().any(|(_, add)| *add == Add::Drag) {
        parts.push(match note("no-tile: ") {
            Some(why) => format!("no drag: {}", &why["no-tile: ".len()..]),
            None => "no drag".to_owned(),
        });
    }
    let why: Vec<&str> = a
        .notes
        .iter()
        .map(|(_, note)| note.as_str())
        .filter(|note| {
            [
                "skip:",
                "open-panel not seen",
                "open-panel went",
                "stray open-panel",
            ]
            .iter()
            .any(|start| note.starts_with(start))
        })
        .map(|note| note.split_once(": AX").map_or(note, |(head, _)| head))
        .collect();
    if listed.is_none() && !why.is_empty() {
        parts.push(why.join(", "));
    }
    if let Some(t) = first_add {
        let last_word = a
            .panes
            .iter()
            .map(|(at, _)| *at)
            .chain(a.notes.iter().map(|(at, _)| *at))
            .chain(a.steps.iter().map(|(at, _)| *at))
            .chain(a.switches.iter().copied())
            .chain(a.adds.iter().map(|(at, _)| *at))
            .max()
            .unwrap_or(t);
        let last_add = a.adds.last().map_or(t, |(at, _)| *at);
        let its = |at: u64| at >= t && (at <= last_word || at <= last_add + ASKED_WITHIN_MS);
        let ms = |ats: &[u64]| {
            ats.iter()
                .filter(|at| its(**at))
                .map(|at| format!("{} ms", after(*at)))
                .collect::<Vec<_>>()
        };
        let (typed, asked, skipped) = (ms(&a.passwords), ms(&a.asked), ms(&a.skipped));
        parts.push(if !typed.is_empty() {
            format!("password {}", typed.join(", "))
        } else if a.password != Some(true) {
            "password NOT MEASURED (the person had none to type)".to_owned()
        } else if !skipped.is_empty() {
            format!(
                "password asked {}, not typed (the focus read a text field not secure)",
                skipped.join(", ")
            )
        } else if !asked.is_empty() {
            format!("password asked {}, not typed", asked.join(", "))
        } else {
            "password not asked".to_owned()
        });
    }
    let last = a.panes.last().map_or("", |(_, state)| state.as_str());
    let switch = last.split_once("switch=").map_or("?", |(_, s)| s);
    match a.switches.first() {
        Some(at) => parts.push(format!(
            "switch on {} ms{}",
            after(*at),
            if switch == "on" {
                String::new()
            } else {
                format!(" (then read {switch})")
            }
        )),
        None if listed.is_some() && switch == "on" => parts.push("switch on already".into()),
        None if listed.is_some() => parts.push(format!("switch {switch}, not pressed")),
        None => {}
    }
    parts.push(format!("page seen {} ms", after(*seen)));
    Value::Text(parts.join("; "))
}

/// The cells of System Settings' Quit & Reopen, each time in ms after the open (`open_ms`, epoch
/// ms on the guest's clock, as every record's):
/// - `Quit & Reopen sheet`: each press the person gave it (`presses`; `None`: no person at the
///   console), and the answer the row asked of it (`later`).
/// - `aterm's quit question`: each Quit the person pressed in the app's question before it quits,
///   after its Quit & Reopen press before it; not answered without one (and with `--later`).
/// - `aterm processes`: each app process the watch (`procs`) saw, its start and its going.
/// - `aterm quit`: when the first one went, after the person's press before it (on the sheet or
///   on the app's question).
/// - `aterm reopened`: the next one's start and pid, after the quit.
/// - `reopened pid's first log line`: that pid's first line in `aterm.log` (`log`).
///
/// A watch with no record, or a process table it could not read, is NOT MEASURED; a quit or
/// reopen the watch did not see by `end_ms` is said as such, with how long it watched. Pure.
#[must_use]
pub fn reopen_values(
    presses: Option<&ReopenPresses>,
    later: bool,
    procs: &Procs,
    log: &str,
    open_ms: u64,
    end_ms: u64,
) -> Vec<(String, Value)> {
    let after = |at: u64| at.saturating_sub(open_ms);
    let watched = format!("in the {} s watched", after(end_ms) / 1000);
    let answer = if later {
        "Later (--later)"
    } else {
        "Quit & Reopen"
    };
    let no_person = || Value::Missing("no person at the console (--person puts one there)".into());
    // The sheet's button, as `sheet_presses` names its press.
    let reopen = "Quit & Reopen";
    // The person's last Quit & Reopen press at or before `at`.
    let reopened_before = |p: &ReopenPresses, at: u64| {
        p.sheet
            .iter()
            .rev()
            .find(|(t, what)| *t <= at && what == reopen)
            .map(|(t, _)| *t)
    };
    let mut out = vec![
        (
            "Quit & Reopen sheet".to_owned(),
            match presses.map(|p| p.sheet.as_slice()) {
                None => no_person(),
                Some([]) => {
                    Value::Text(format!("not seen {watched} (the person answers {answer})"))
                }
                Some(presses) => Value::Text(format!(
                    "{} (the person answers {answer})",
                    presses
                        .iter()
                        .map(|(at, what)| format!("{what} pressed {} ms", after(*at)))
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
            },
        ),
        (
            "aterm's quit question".to_owned(),
            match presses {
                None => no_person(),
                Some(p) if p.quit.is_empty() && p.sheet.iter().all(|(_, what)| what != reopen) => {
                    Value::Text(format!(
                        "not answered: the person answers it only after its own {reopen}"
                    ))
                }
                Some(p) if p.quit.is_empty() => Value::Text(format!("not seen {watched}")),
                Some(p) => Value::Text(
                    p.quit
                        .iter()
                        .map(|&at| {
                            let since = reopened_before(p, at).map_or_else(String::new, |t| {
                                format!(", {} ms after {reopen}", at - t)
                            });
                            format!("Quit pressed {} ms{since}", after(at))
                        })
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            },
        ),
    ];
    let names = ["aterm processes", "aterm quit", "aterm reopened"];
    let missing = match (&procs.error, procs.started) {
        (Some(why), _) => Some(format!("the process watch: {why}")),
        (None, None) => Some("the process watch wrote nothing".to_owned()),
        (None, Some(_)) => None,
    };
    if let Some(why) = missing {
        out.extend(names.map(|n| (n.to_owned(), Value::Missing(why.clone()))));
        return out;
    }
    let runs = runs(procs);
    let Some(first) = runs.first() else {
        let why = format!("the process watch saw no aterm app process {watched}");
        out.extend(names.map(|n| (n.to_owned(), Value::Missing(why.clone()))));
        return out;
    };
    out.push((
        names[0].to_owned(),
        Value::Text(
            runs.iter()
                .map(|r| {
                    let down = r
                        .down_ms
                        .map_or_else(|| "the end".to_owned(), |d| format!("{} ms", after(d)));
                    format!("pid {} {} ms to {down}", r.pid, after(r.start_ms))
                })
                .collect::<Vec<_>>()
                .join("; "),
        ),
    ));
    let Some(quit) = first.down_ms else {
        out.push((
            names[1].to_owned(),
            Value::Text(format!("none: pid {} ran to the end {watched}", first.pid)),
        ));
        out.push((
            names[2].to_owned(),
            Value::Text("none: nothing quit".to_owned()),
        ));
        return out;
    };
    let pressed = presses
        .and_then(|p| {
            p.sheet
                .iter()
                .map(|(at, what)| (*at, what.as_str()))
                .chain(p.quit.iter().map(|&at| (at, "Quit on aterm's question")))
                .filter(|(at, _)| *at <= quit)
                .max_by_key(|(at, _)| *at)
        })
        .map_or_else(String::new, |(at, what)| {
            format!(", {} ms after {what}", quit - at)
        });
    out.push((
        names[1].to_owned(),
        Value::Text(format!(
            "pid {} gone {} ms{pressed}",
            first.pid,
            after(quit)
        )),
    ));
    let Some(next) = runs.get(1) else {
        out.push((
            names[2].to_owned(),
            Value::Text(format!(
                "none: no aterm app process started again {watched}"
            )),
        ));
        return out;
    };
    out.push((
        names[2].to_owned(),
        Value::Text(format!(
            "pid {} started {} ms, {} ms after the quit",
            next.pid,
            after(next.start_ms),
            next.start_ms.saturating_sub(quit)
        )),
    ));
    out.push((
        "reopened pid's first log line".to_owned(),
        or_missing(
            log_lines(log)
                .iter()
                .find(|l| l.pid == next.pid && l.at_ms >= next.start_ms)
                .map(|l| after(l.at_ms)),
            Value::Ms,
            &format!("aterm.log has no line from pid {} {watched}", next.pid),
        ),
    ));
    out
}

/// A person-stage row's person: with `--person`, what it did and whether its input reset
/// `HIDIdleTime` (else the person stages are NOT MEASURED, never a pass); without, that no
/// person was there.
fn person_stage_cells(g: &Guest, ask: &Ask, row: &mut Row) -> Result<(), Fail> {
    if !ask.person {
        row.cell(
            "person stages",
            Value::Missing("no person at the console (--person puts one there)".into()),
        );
        return Ok(());
    }
    if let HidProof::NotMeasured(why) = person_cells(g, row)? {
        row.cell(
            "person stages",
            Value::Missing(format!("the person's input did not show: {why}")),
        );
    }
    Ok(())
}

/// Which process macOS held back to ask its Local Network question since the open (`open_ms`,
/// epoch ms), from the guest's unified log, each by its command as `ps` reads it now: the
/// question System State's step is meant to ask first.
fn local_network_cell(g: &Guest, row: &mut Row, open_ms: u64) -> Result<(), Fail> {
    let since_s = g.clock_ms()?.saturating_sub(open_ms) / 1000 + 5;
    let log = g.sh(
        &format!(
            "/usr/bin/log show --last {since_s}s --info --style ndjson --predicate \
             'subsystem == \"com.apple.networkextension\" AND eventMessage CONTAINS \"local network blocked notification\"' \
             2>/dev/null || true"
        ),
        STEP,
    )?;
    let blocks: Vec<(Option<u64>, u32)> = local_network_blocks(&log)
        .into_iter()
        .filter(|(at, _)| at.is_none_or(|at| at >= open_ms))
        .collect();
    let pids: Vec<String> = blocks.iter().map(|(_, pid)| pid.to_string()).collect();
    let commands = if pids.is_empty() {
        BTreeMap::new()
    } else {
        ps_commands(&g.sh(
            &format!("ps -o pid=,command= -p {} || true", pids.join(",")),
            STEP,
        )?)
    };
    row.cell(
        "Local Network alert raised by",
        Value::Text(if blocks.is_empty() {
            "none".to_owned()
        } else {
            blocks
                .iter()
                .map(|(at, pid)| {
                    let who = commands
                        .get(pid)
                        .map_or_else(|| format!("pid {pid} (gone)"), |c| squeeze(c, 80));
                    match at {
                        Some(at) => format!("{who} {} ms", at.saturating_sub(open_ms)),
                        None => who,
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        }),
    );
    Ok(())
}

/// Main to first present (`rust_main_to_first_present_ms` in `aterm ctl metrics`).
fn first_present_ms(g: &Guest) -> Result<Option<u64>, Fail> {
    let metrics = g.run(&format!("{} ctl metrics 2>&1", g.aterm), STEP)?;
    Ok(first_present(&metrics).ok().flatten())
}

/// Each stage start the progress record's watch saw, in ms after the open (`open_ms`, epoch ms),
/// the first one's after first present (`present_ms` after the open), and the record's completion.
fn stage_cells(
    g: &Guest,
    row: &mut Row,
    open_ms: u64,
    present_ms: Option<u64>,
) -> Result<(), Fail> {
    let (looks, _) = read_watch(&g.read(&format!("{GUEST_NUX}/watch.txt"))?);
    let starts = stage_starts(&looks);
    let after = |at: u64| at.saturating_sub(open_ms);
    row.cell(
        "stage starts",
        if starts.is_empty() {
            Value::Missing("initial-setup.json never named a running stage".into())
        } else {
            Value::Text(
                starts
                    .iter()
                    .map(|(at, bit)| format!("{} {} ms", stage_name(*bit), after(*at)))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        },
    );
    row.cell(
        "first stage after first present",
        match (starts.first(), present_ms) {
            (None, _) => Value::Missing("no stage started".into()),
            (Some(_), None) => Value::Missing("first present did not read back".into()),
            (Some((at, _)), Some(present)) => Value::Ms(after(*at).saturating_sub(present)),
        },
    );
    row.cell(
        "setup record completed",
        or_missing(
            completed_at(&looks).map(after),
            Value::Ms,
            "initial-setup.json never said completed",
        ),
    );
    Ok(())
}

/// How many of the setup lines a first launch's row names.
const SETUP_SHOWN: usize = 8;

/// How many of `aterm.log`'s last lines a first launch whose setup did not finish prints.
const LOG_SHOWN: usize = 80;

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
    start_mdns(g)?;
    let (d, out) = g.drive(
        "vault",
        &format!("NUX_JOIN= NUX_SUDO={}", g.sudo_password()),
        &format!("{} init --only workstation", g.aterm),
    )?;
    let mdns = stop_mdns(g)?;
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
    open_rows_cell(row, "", &d, &out);
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
/// they print the vault's signer (off the `join` line `create` prints), which the joining Mac
/// pins. A new vault is open, so `password new` gives it the join password the lane types (with
/// no terminal it prints none). `password show` writes the join
/// password only to a terminal, so the setup reads it as the person at one would, on the
/// pseudo-terminal `expect` spawns it on (its line end's `\r` dropped; a terminal's EOF, which
/// `script` with no input would type and the terminal echo as `^D`, never reaches it), and a
/// line that is no one word (its refusal, which the terminal also carries) fails the setup.
fn local_vault_setup(aterm: &str, admin: &str) -> String {
    let url = format!("http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}");
    format!(
        "set -e; A={admin}; mkdir -p $A/seed; printf nux-probe-value > $A/seed/{LOCAL_VAULT_ENTRY}; export HOME=$A; \
         ({aterm} secrets serve --listen 127.0.0.1:{LOCAL_VAULT_PORT} --no-advertise --no-keep-awake --store $A/served </dev/null >$A/serve.log 2>&1 &); \
         n=0; until curl -s -o /dev/null --max-time 1 http://127.0.0.1:{LOCAL_VAULT_PORT}/; do \
         n=$((n+1)); [ $n -gt 600 ] && {{ echo 'the vault server did not answer within 60 s'; cat $A/serve.log; exit 1; }}; sleep 0.1; done; \
         {aterm} secrets create '{url}' --import $A/seed --no-agent --name nux-admin > $A/create.out 2>&1 || {{ cat $A/create.out; exit 1; }}; \
         {aterm} secrets password new --no-agent </dev/null > $A/password-new.out 2>&1 || {{ cat $A/password-new.out; exit 1; }}; \
         expect -c 'set timeout 60; spawn -noecho {aterm} secrets password show; expect eof' > $A/password.tty 2>$A/password.err || {{ cat $A/password.err; exit 1; }}; \
         tr -d '\\r' < $A/password.tty > $A/password; \
         case \"$(cat $A/password)\" in ''|*' '*) echo 'password show printed no join password:'; cat $A/password.tty; exit 1;; esac; \
         ({aterm} secrets watch </dev/null >$A/watch.log 2>&1 &); \
         sed -n 's/.*aterm secrets join [^#]*#\\([^ ]*\\).*/\\1/p' $A/create.out"
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
    open_rows_cell(row, "", &d, &out);
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
    row.cell(
        "advertised",
        Value::Text(
            "no: served on 127.0.0.1 only, as main's server has no lab service type to advertise under (only _alab-vault._tcp, which a guest never advertises)"
                .into(),
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
    if let Some(why) = scenario.not_run() {
        return Err(not_run(why.to_owned()));
    }
    let mut row = Row::new(scenario);
    let g = guest(tart, base, run, share, ask, scenario, &mut row)?;
    let watchers = Watchers::start(&g)?;
    let body = match scenario {
        Scenario::InitApply => init_apply(&g, &mut row),
        Scenario::InitSsh => init_ssh(&g, &mut row),
        Scenario::InitNotty => init_notty(&g, &mut row),
        Scenario::PassHeld => pass_held(&g, &mut row),
        Scenario::Doctor => doctor(&g, &mut row),
        Scenario::LaunchRepairs => launch_repairs(&g, &mut row),
        Scenario::FirstLaunch => first_launch(&g, ask, &mut row),
        Scenario::TerminalLaunch => terminal_launch(&g, &mut row),
        Scenario::AppLaunch => app_launch(&g, &mut row),
        Scenario::Vault => vault(&g, &mut row),
        Scenario::VaultLocal => vault_local(&g, &mut row),
        Scenario::VaultPhrase => phrase::vault_phrase(&g, run, &mut row),
        Scenario::Successor => successor(&g, &mut row),
        Scenario::InstallSh => install_sh(&g, &mut row, None, true),
        Scenario::KillStage => fault::kill_stage(&g, &mut row),
        Scenario::KillClt => fault::kill_clt(&g, &mut row),
        // Refused before its guest ([`Scenario::not_run`]).
        Scenario::KillPublish => Ok(()),
        Scenario::Rerun => fault::rerun(&g, &mut row),
        Scenario::LegacyUpgrade => fault::legacy_upgrade(&g, &mut row),
        Scenario::PersonChange => fault::person_change(&g, &mut row),
        Scenario::Offline => fault::offline(&g, &mut row),
        Scenario::Lossy => fault::lossy(&g, &mut row),
        Scenario::MdnsSilent => fault::mdns(&g, &mut row, false),
        Scenario::MdnsNoRoute => fault::mdns(&g, &mut row, true),
        Scenario::Dmg => fault::dmg(&g, &mut row),
        Scenario::AgentSsh => fault::agent_ssh(&g, &mut row),
        Scenario::ClockBack => fault::clock_back(&g, &mut row),
    };
    // The census, tccd and the guard are read whatever the row came to, before the clone goes.
    let watched = watchers.finish(&g, &mut row);
    let guarded = alab_guard(&g, run, &mut row);
    drop(g);
    body?;
    watched?;
    guarded?;
    Ok(row)
}

/// The executable of the binary target `name` in cargo's `--message-format=json` lines.
#[must_use]
pub fn bin_executable(json_lines: &str, name: &str) -> Option<PathBuf> {
    json_lines
        .lines()
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(|v| {
            v.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
        })
        .filter(|v| {
            v.get("target")
                .and_then(|t| t.get("name"))
                .and_then(aterm_json::Value::as_str)
                == Some(name)
        })
        .find_map(|v| Some(PathBuf::from(v.get("executable")?.as_str()?)))
}

/// Build the guest helper here (`aterm-lab-guest`, this package's other binary) and put it in the
/// share, where a person base's preparation and each person row install it from
/// ([`crate::mac::person_install`]).
fn build_helper(share: &Path) -> Result<(), Fail> {
    let out = crate::util::targo(&[
        "--unverified",
        "build",
        "-p",
        "aterm-lab",
        "--bin",
        GUEST_HELPER,
        "--message-format",
        "json-render-diagnostics",
    ])
    .stdin(Stdio::null())
    .output()
    .map_err(|e| Fail::die(format!("targo: {e}")))?;
    let lines = String::from_utf8_lossy(&out.stdout);
    let built = bin_executable(&lines, GUEST_HELPER).filter(|_| out.status.success());
    let Some(built) = built else {
        return Err(Fail::die(format!(
            "the guest helper did not build (targo --unverified build -p aterm-lab --bin {GUEST_HELPER}): {}",
            squeeze(String::from_utf8_lossy(&out.stderr).trim(), 400)
        )));
    };
    let copy = share.join(GUEST_HELPER);
    std::fs::copy(&built, &copy).map_err(|e| format!("{}: {e}", built.display()))?;
    Ok(())
}

/// Refuse (NOT RUN) to make `base` from `image` when tart must pull the image first and this Mac
/// has under [`PULL_ROOM_GB`] free: the pull takes about 41 GB, and 30 GB stays for every build.
fn pull_room(tart: &str, image: &str, base: &str) -> Result<(), Fail> {
    let listed = |source: &str| -> String {
        aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(tart)
                .args(["list", "--source", source, "--quiet"])
                .stdin(Stdio::null()),
            Instant::now() + STEP,
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
    };
    if listed("local").lines().any(|n| n.trim() == base)
        || listed("oci").lines().any(|n| n.trim() == image)
    {
        return Ok(());
    }
    let df = crate::windows::run(
        Path::new("/bin/df"),
        &["-g".into(), "/System/Volumes/Data".into()],
        None,
    );
    let free = df_free_gb(&df.text);
    match free {
        Some(gb) if gb >= PULL_ROOM_GB => Ok(()),
        _ => Err(not_run(format!(
            "{image} is not on this Mac and its pull needs {PULL_ROOM_GB} GB free (about 41 GB for the image, 30 GB kept for builds); df says {}",
            free.map_or_else(|| "nothing readable".to_owned(), |gb| format!("{gb} GB"))
        ))),
    }
}

/// The free space before an image pull: 41 GB for the image and 30 GB that stays for builds.
const PULL_ROOM_GB: u64 = 71;

/// The available GB `df -g` names for its one volume.
#[must_use]
pub fn df_free_gb(df: &str) -> Option<u64> {
    df.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

/// This Mac's load average (`sysctl -n vm.loadavg`), as it reads.
fn host_load() -> String {
    let out = crate::windows::run(
        Path::new("/usr/sbin/sysctl"),
        &["-n".into(), "vm.loadavg".into()],
        None,
    );
    out.text
        .trim()
        .trim_matches(|c| c == '{' || c == '}')
        .trim()
        .to_owned()
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
    build_helper(&share)?;
    let app = if ask.scenarios.iter().any(|s| s.needs_app()) {
        Some(stage_app(ask.app.as_deref(), run, &share)?)
    } else {
        None
    };
    let prep = crate::mac::Prep {
        fda: false,
        no_clt: ask.no_clt,
        person: ask.person.then(|| GUEST_HELPER.to_owned()),
    };
    pull_room(&tart, &ask.image, &crate::mac::prep_name(&ask.image, &prep))?;
    let base =
        crate::mac::prepared_base_with(&tart, &ask.image, &prep, run, &share, &mut || Ok(()))?;
    let load = host_load();
    let mut rows = Vec::new();
    for &scenario in &ask.scenarios {
        for at in 0..ask.repeat {
            stop::check()?;
            println!(
                "mac nux: {} ({} of {}) in a fresh clone of {base}",
                scenario.name(),
                at + 1,
                ask.repeat
            );
            let row = match run_row(&tart, &base, run, &share, &ask, scenario) {
                Ok(row) => row,
                Err(fail) if fail.stopped.is_some() => return Err(fail),
                Err(fail) => Row::refused(scenario, &fail),
            };
            println!("{}", table("", std::slice::from_ref(&row)).trim());
            rows.push(row);
        }
    }
    let head = format!(
        "### lab mac nux, {} ({}{})\n\nGuest: a clone of {base} per row (SIP on, no window){}{}, one guest at a time, \
         this Mac's load average {load} at the start. App: {}.",
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
        if ask.person {
            ", the guest helper as the person at the console"
        } else {
            ", nobody at the console"
        },
        if ask.repeat > 1 {
            format!(", each row {} times", ask.repeat)
        } else {
            String::new()
        },
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

#[path = "mac_nux_advert.rs"]
mod advert;

#[path = "mac_nux_fault.rs"]
mod fault;

#[path = "mac_nux_phrase.rs"]
mod phrase;

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
        for s in every() {
            assert_eq!(parse_scenarios(s.name()).unwrap(), [s]);
        }
        // The fault rows come by name or as `faults`, never with `all`, and after its rows.
        assert_eq!(parse_scenarios("faults").unwrap(), FAULTS);
        assert!(
            parse_scenarios("all")
                .unwrap()
                .iter()
                .all(|s| !FAULTS.contains(s))
        );
        assert_eq!(
            parse_scenarios("dmg,doctor").unwrap(),
            [Scenario::Doctor, Scenario::Dmg]
        );
        let names: Vec<_> = every().map(Scenario::name).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        // Only kill-publish is refused before a guest; every fault row runs the app.
        assert_eq!(
            every()
                .filter(|s| s.not_run().is_some())
                .collect::<Vec<_>>(),
            [Scenario::KillPublish]
        );
        assert!(FAULTS.iter().all(|s| s.needs_app()));
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
    fn the_base_flags_pick_a_base_a_count_and_a_dated_record() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let golden = "ghcr.io/cirruslabs/macos-golden-gate-base:latest";
        let ask = parse(&args(&[]), golden.into()).unwrap();
        assert!(!ask.person && !ask.no_clt && !ask.vanilla);
        assert_eq!(ask.repeat, 1);
        let ask = parse(
            &args(&["--person", "--no-clt", "--repeat", "3"]),
            golden.into(),
        )
        .unwrap();
        assert!(ask.person && ask.no_clt && !ask.vanilla && !ask.later);
        assert_eq!((ask.repeat, ask.image.as_str()), (3, golden));
        // Later is the person's answer: it takes a person.
        assert!(
            parse(&args(&["--person", "--later"]), golden.into())
                .unwrap()
                .later
        );
        assert_eq!(
            parse(&args(&["--later"]), golden.into()).unwrap_err().code,
            2
        );
        // The vanilla image is the -base image's sibling: the same macOS, no Command Line Tools.
        let ask = parse(&args(&["--vanilla"]), golden.into()).unwrap();
        assert_eq!(
            ask.image,
            "ghcr.io/cirruslabs/macos-golden-gate-vanilla:latest"
        );
        assert_eq!(
            vanilla_image("ghcr.io/cirruslabs/macos-tahoe-base"),
            Some("ghcr.io/cirruslabs/macos-tahoe-vanilla:latest".into())
        );
        assert_eq!(vanilla_image("ghcr.io/x/other:1"), None);
        for refused in [
            &["--vanilla", "--image", "ghcr.io/x/other:1"][..],
            &["--no-clt", "--vanilla"],
            &["--repeat", "0"],
            &["--repeat", "11"],
            &["--repeat", "x"],
        ] {
            assert_eq!(
                parse(&args(refused), golden.into()).unwrap_err().code,
                2,
                "{refused:?}"
            );
        }
        // `--record` alone appends to today's dated table, wherever it stands among the flags.
        let dated = format!(
            "docs/measured/mac-nux-{}.md",
            crate::util::ymd(crate::util::now_secs())
        );
        for alone in [&["--record"][..], &["--record", "--keep"]] {
            let ask = parse(&args(alone), golden.into()).unwrap();
            let file = ask.record.unwrap();
            assert!(file.ends_with(&dated), "{}", file.display());
            assert!(file.starts_with(&paths().repo), "{}", file.display());
            assert_eq!(ask.keep, alone.len() == 2);
        }
    }

    #[test]
    fn a_report_names_each_row_left_not_in_place() {
        let json = r#"{"items":[{"id":"workstation.dock","outcome":"changed"},{"id":"base.clt","outcome":"person"},
            {"id":"base.ok","outcome":"ok"},{"id":"secrets.vault","outcome":"failed"},{"id":"x.na","outcome":"not-applicable"},
            {"id":"repos.clone","outcome":"deferred"}]}"#
            .replace('\n', "");
        let j = read_report_json(&json).unwrap();
        assert_eq!(
            j.not_ok,
            [
                ("base.clt".to_owned(), "person".to_owned()),
                ("secrets.vault".to_owned(), "failed".to_owned()),
                ("repos.clone".to_owned(), "deferred".to_owned()),
            ]
        );
        let mut row = Row::new(Scenario::InitApply);
        not_ok_cell(&mut row, "", "init --check --json", Some(&j));
        assert_eq!(
            row.cells[0],
            (
                "rows not ok (init --check --json)".to_owned(),
                Value::Text("base.clt person, secrets.vault failed, repos.clone deferred".into())
            )
        );
        // NEGATIVE CONTROL: a report every row of which is in place names none, and one that did
        // not parse is not measured, never "none".
        let all_ok = read_report_json(
            r#"{"items":[{"id":"a","outcome":"ok"},{"id":"b","outcome":"changed"}]}"#,
        );
        let mut row = Row::new(Scenario::InitApply);
        not_ok_cell(&mut row, "", "init", all_ok.as_ref());
        not_ok_cell(&mut row, "", "init", None);
        assert_eq!(row.cells[0].1, Value::Text("none".into()));
        assert!(matches!(row.cells[1].1, Value::Missing(_)));
    }

    #[test]
    fn xcode_select_and_the_state_folder_read_in_a_few_words() {
        assert_eq!(
            xcode_select_text("/Library/Developer/CommandLineTools\nexit=0\n"),
            "/Library/Developer/CommandLineTools"
        );
        let failed = xcode_select_text(
            "xcode-select: error: unable to get active developer directory, use `sudo xcode-select --switch path/to/Xcode.app` to set one (or see `man xcode-select`)\nexit=2\n",
        );
        assert!(
            failed.starts_with(
                "fails (exit 2: xcode-select: error: unable to get active developer directory"
            ) && failed.ends_with("…)"),
            "{failed}"
        );
        assert_eq!(xcode_select_text(""), "fails (exit ?: )");

        assert_eq!(state_summary("(absent)\n"), "no aterm folder");
        let listing = "./doctor\n./doctor/initial-setup.json\n./doctor/first-run.json\n./pkg\n\
                       == doctor/initial-setup.json\n{\"state\":\"pending\"}\n";
        assert_eq!(
            state_summary(listing),
            "4 entries; doctor/: initial-setup.json, first-run.json"
        );
        assert_eq!(state_summary("./pkg\n"), "1 entries; doctor/: none");
    }

    #[test]
    fn the_alab_guard_fails_a_row_that_reached_the_vault_or_heard_a_guest_advert() {
        let none = Ok(advert::Split::default());
        let unreached = alab_probe_verdict("none", "000\n", &none).unwrap();
        assert!(
            unreached.starts_with("Unreached (alab-secrets.local unresolved"),
            "{unreached}"
        );
        // A name that resolves but nothing answers (the pf null route) is still unreached, and
        // so is this Mac's own vault heard over the bridge, whose port pf refuses.
        assert!(alab_probe_verdict("192.168.64.1", "000", &none).is_ok());
        let this_macs = Ok(advert::Split {
            others: Vec::new(),
            this_macs: vec!["alab-secrets heard over the guest's interface 6".into()],
        });
        assert!(alab_probe_verdict("192.168.1.242", "000", &this_macs).is_ok());
        // NEGATIVE CONTROLS: any HTTP answer, any advert that is not this Mac's, or a browse
        // that did not run fails the row.
        let another = Ok(advert::Split {
            others: vec!["nux lab on the guest's interface 1 (nux.local 192.168.64.3)".into()],
            this_macs: Vec::new(),
        });
        assert!(alab_probe_verdict("192.168.64.1", "200", &none).is_err());
        assert!(alab_probe_verdict("none", "403", &none).is_err());
        assert!(alab_probe_verdict("none", "000", &another).is_err());
        assert_eq!(
            alab_probe_verdict("none", "000", &Err("its transcript is empty".into())).unwrap_err(),
            "the guest's browse of _alab-vault._tcp did not run (its transcript is empty), so no \
             advert is unproven"
        );

        let browse = "Browsing for _alab-vault._tcp.local.\n\
                      DATE: ---Mon 26 Oct 2026---\n \
                      8:30:01.123  ...STARTING...\n\
                      Timestamp     A/R    Flags  if Domain               Service Type         Instance Name\n \
                      8:30:01.200  Add        3  15 local.               _alab-vault._tcp.    nux lab\n \
                      8:30:01.201  Add        3   4 local.               _alab-vault._tcp.    alab-secrets\n \
                      8:30:01.202  Add        2   9 local.               _alab-vault._tcp.    thunderbolt\n \
                      8:30:01.203  Rmv        0  15 local.               _alab-vault._tcp.    nux lab\n";
        let name_of = |index: u32| match index {
            15 => Some("bridge100".to_owned()),
            4 => Some("en0".to_owned()),
            9 => Some("bridge0".to_owned()),
            _ => None,
        };
        // The office's own vault on en0 and a Thunderbolt bridge are not on tart's bridge.
        let on_bridge = bridge_heard(browse, &name_of);
        assert_eq!(on_bridge.len(), 1, "{on_bridge:?}");
        assert_eq!(
            (on_bridge[0].instance.as_str(), on_bridge[0].interface),
            ("nux lab", 15)
        );
        assert!(bridge_heard(&browse.replace("15 local.", "77 local."), &name_of).is_empty());
    }

    /// A browse counts only once its transcript shows it ran: a quiet one (no header, as `dns-sd`
    /// prints the header with its first answer) is zero adverts, while an empty transcript (a
    /// `script` killed before it flushed) or a failed call is no count at all.
    #[test]
    fn a_browse_that_did_not_run_proves_no_advert_absent() {
        // As `script -q -t 0` wrote them on this Mac (2026-10-28): its `^D^H^H` and `\r`s kept.
        let quiet = "\u{4}\u{8}\u{8}Browsing for _nux-none-t0._tcp.local.\r\n\
                     DATE: ---Wed 28 Oct 2026---\r\n\
                     20:23:54.199  ...STARTING...\r\n";
        assert_eq!(browse_adds(quiet), Ok(Vec::new()));
        let heard = format!(
            "{quiet}Timestamp     A/R    Flags  if Domain               Service Type         Instance Name\r\n\
             20:23:54.300  Add        3  15 local.               _alab._tcp.          nux lab\r\n\
             20:23:54.301  Rmv        0  15 local.               _alab._tcp.          nux lab\r\n"
        );
        assert_eq!(browse_adds(&heard).map(|h| h.len()), Ok(1));
        assert_eq!(browse_ran(&heard), Ok(()));

        // NEGATIVE CONTROLS: what a browse that never looked leaves reads as unread, never 0.
        assert_eq!(browse_adds(""), Err("its transcript is empty".to_owned()));
        assert_eq!(
            browse_adds("\u{4}\u{8}\u{8}\r\n"),
            Err("its transcript is empty".to_owned())
        );
        assert_eq!(
            browse_adds("Browsing for _alab._tcp.local.\r\n"),
            Err("no browse started: Browsing for _alab._tcp.local.".to_owned())
        );
        assert_eq!(
            browse_adds("DNSService call failed -65563\r\n"),
            Err("dns-sd failed: DNSService call failed -65563".to_owned())
        );
        let errored = format!("{quiet}20:23:55.000  Error code -65537\r\n");
        assert_eq!(
            browse_adds(&errored),
            Err("dns-sd failed: 20:23:55.000  Error code -65537".to_owned())
        );
    }

    #[test]
    fn the_local_network_asker_reads_from_the_unified_log_and_ps() {
        let blocks = [
            r#"{"timestamp":"2026-10-28 14:18:24.716000+0000","eventMessage":"Got local network blocked notification: pid: 777, uuid: 758D74AB-18EA-3966-BDA9-9DB45651C793, bundle_id: (null)"}"#,
            r#"{"timestamp":"2026-10-28 14:18:24.716500+0000","eventMessage":"LocalNetwork: found bundle id com.aterm.aterm by PID"}"#,
        ]
        .join("\n");
        assert_eq!(
            local_network_blocks(&blocks),
            [(Some(1_793_197_104_716), 777)]
        );
        let ps = "  777 /Applications/aterm.app/Contents/MacOS/aterm\n  827 /Applications/aterm.app/Contents/MacOS/aterm acaffiene serve\n";
        let commands = ps_commands(ps);
        assert_eq!(
            commands.get(&777).map(String::as_str),
            Some("/Applications/aterm.app/Contents/MacOS/aterm")
        );
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn a_folder_dialog_counts_for_aterm_after_the_mark_only() {
        assert_eq!(
            log_timestamp_ms("2026-10-26 08:30:01.123456-0700"),
            Some(1_793_028_601_123)
        );
        assert_eq!(
            log_timestamp_ms("2026-10-26 15:30:01.1+0000"),
            Some(1_793_028_601_100)
        );
        assert_eq!(
            log_timestamp_ms("2026-10-26 15:30:01+0000"),
            Some(1_793_028_601_000)
        );
        assert_eq!(log_timestamp_ms("yesterday"), None);

        let log = [
            r#"{"timestamp":"2026-10-26 08:30:00.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.1, service=kTCCServiceSystemPolicyDocumentsFolder, subject=Sub:{com.aterm.aterm}, "}"#,
            r#"{"timestamp":"2026-10-26 08:30:05.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyDownloadsFolder, subject=Sub:{com.aterm.aterm}, "}"#,
            r#"{"timestamp":"2026-10-26 08:30:06.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.3, service=kTCCServiceSystemPolicyDesktopFolder, subject=Sub:{\/usr\/local\/bin\/tart-guest-agent}, "}"#,
            r#"{"timestamp":"2026-10-26 08:30:07.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.4, service=kTCCServiceSystemPolicyAllFiles, subject=Sub:{com.aterm.aterm}, "}"#,
            r#"{"timestamp":"2026-10-26 08:30:08.000000-0700","eventMessage":"AUTHREQ_PROMPTING: msgID=1.5, service=kTCCServiceAccessibility, subject=Sub:{com.aterm.aterm}, "}"#,
            r#"{"eventMessage":"AUTHREQ_CTX: msgID=1.6, service=kTCCServiceSystemPolicyDocumentsFolder"}"#,
        ]
        .join("\n");
        let prompts = tcc_prompt_events(&log);
        assert_eq!(prompts.len(), 5);
        assert_eq!(prompts[2].subject, "/usr/local/bin/tart-guest-agent");
        assert_eq!(prompts[0].at_ms, Some(1_793_028_600_000));
        // Before the walk granted Full Disk Access (the mark) the Documents dialog came; after it
        // only the Downloads one, never the guest agent's Desktop or Full Disk Access itself.
        assert_eq!(folder_prompts_after(&prompts, 0), 2);
        assert_eq!(folder_prompts_after(&prompts, 1_793_028_601_000), 1);
        assert_eq!(folder_prompts_after(&prompts, 1_793_028_609_000), 0);
    }

    #[test]
    fn the_helper_is_found_in_cargos_json_and_free_space_in_df() {
        let json = r#"{"reason":"compiler-artifact","target":{"name":"aterm-lab","kind":["bin"]},"executable":"/c/debug/aterm-lab"}
{"reason":"compiler-artifact","target":{"name":"aterm-lab-guest","kind":["bin"]},"executable":"/c/debug/aterm-lab-guest"}
{"reason":"build-finished","success":true}"#;
        assert_eq!(
            bin_executable(json, "aterm-lab-guest"),
            Some(PathBuf::from("/c/debug/aterm-lab-guest"))
        );
        assert_eq!(bin_executable(json, "aterm"), None);
        assert_eq!(bin_executable("", "aterm-lab-guest"), None);

        let df = "Filesystem   1G-blocks Used Available Capacity iused ifree %iused  Mounted on\n\
                  /dev/disk3s5       926  833        67    93%  3.4M  698M    0%   /System/Volumes/Data\n";
        assert_eq!(df_free_gb(df), Some(67));
        assert_eq!(df_free_gb("df: /nope: No such file or directory\n"), None);
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
    fn the_developer_tools_cell_says_how_aterm_came_onto_the_list_its_password_and_its_switch() {
        let open = 1_000_000;
        let raw = |record: &str, log: &str| {
            devtools_value(
                Some(&devtools_acts(&read_person(record))),
                log,
                open,
                1_400_000,
            )
            .to_string()
        };
        let person = "person 1000000 trusted=yes post=yes password=yes\n";
        let cell = |record: &str, log: &str| raw(&format!("{person}{record}"), log);
        let page = "devtools 1248000 120 pane list=lacks switch=none\n\
                    devtools 1248100 120 drag tile=2082 from=1100,300 to=600,420\n\
                    click 1254000 120 System Settings devtools-drag\n";
        let taken = "1793243999.000 2082 INFO aterm_gui::devtools_helper: x\n\
                     1254.000 2082 INFO aterm_gui::devtools_helper: devtools: the owner's drag of aterm ended (operation 1: taken)\n";
        // The drop took: a password, aterm on the list, its switch read off and pressed on.
        assert_eq!(
            cell(
                &format!(
                    "{page}typed 1258000 120 account 5 Managed_via_Tart unwatched System Settings\n\
                     devtools 1259000 120 pane list=holds switch=off\n\
                     click 1265000 120 System Settings devtools-switch\n\
                     devtools 1265500 120 pane list=holds switch=on\n"
                ),
                taken
            ),
            "aterm added by drag 259000 ms, 5000 ms after the drag (aterm's log: operation 1: \
             taken); password 258000 ms; switch on 265000 ms; page seen 248000 ms"
        );
        // On the list with its switch on, no password asked.
        assert_eq!(
            cell(
                &format!("{page}devtools 1255000 120 pane list=holds switch=on\n"),
                taken
            ),
            "aterm added by drag 255000 ms, 1000 ms after the drag (aterm's log: operation 1: \
             taken); password not asked; switch on already; page seen 248000 ms"
        );
        // The drop did not take, and + did: the add is the +'s.
        let plus = format!(
            "{page}click 1276000 120 System Settings devtools-plus\n\
             devtools 1277000 120 open-panel 120 System Settings: AXSheet; AXButton \"Open\"\n\
             click 1295000 120 System Settings devtools-goto\n\
             click 1301000 120 System Settings devtools-open\n\
             devtools 1302000 120 pane list=holds switch=on\n"
        );
        let not_taken = "1254.000 2082 INFO aterm_gui::devtools_helper: devtools: the owner's drag of aterm ended (operation 0: not taken)\n";
        assert_eq!(
            cell(&plus, not_taken),
            "aterm added by + 302000 ms, 26000 ms after the +; password not asked; switch on \
             already; page seen 248000 ms"
        );
        // Negative controls. Never on the list: each try, its steps and why it stopped; a drag
        // aterm's log never ended.
        assert_eq!(
            cell(
                &format!(
                    "{page}click 1276000 120 System Settings devtools-plus\n\
                     devtools 1296000 120 open-panel not seen within 20 s of +\n"
                ),
                ""
            ),
            "aterm not on the list in the 400 s watched: drag pressed 254000 ms (aterm's log: \
             NOT MEASURED, no line of it from the page on), + pressed 276000 ms; open-panel not \
             seen within 20 s of +; password not asked; page seen 248000 ms"
        );
        // aterm's log read from the page on, with no drag's end in it.
        assert_eq!(
            cell(
                page,
                "1793243999.000 2082 INFO aterm_gui::devtools_helper: x\n"
            ),
            "aterm not on the list in the 400 s watched: drag pressed 254000 ms (aterm's log: no \
             drag ended); password not asked; page seen 248000 ms"
        );
        // A list that read as holding aterm once (an open panel's file view, a sheet closing) and
        // lacked it again: no add.
        assert_eq!(
            cell(
                &format!(
                    "{page}devtools 1255000 120 pane list=holds switch=off\n\
                     devtools 1255250 120 pane list=lacks switch=none\n"
                ),
                taken
            ),
            "aterm not on the list in the 400 s watched: drag pressed 254000 ms (aterm's log: \
             operation 1: taken); password not asked; page seen 248000 ms"
        );
        // On the list later with no add of the person's, its switch on already.
        assert_eq!(
            cell(
                "devtools 1248000 120 pane list=lacks switch=none\n\
                 devtools 1250000 120 pane list=holds switch=on\n",
                ""
            ),
            "aterm on the list 250000 ms, with no add by the person; switch on already; page seen \
             248000 ms"
        );
        // Added with + alone, no tile to drag; its switch read off, then pressed and read off.
        let plus_only = "devtools 1248000 120 pane list=lacks switch=none\n\
                         devtools 1268000 120 no-tile: no window of the app (1 running) shows the \
                         helper panel's tile\n\
                         click 1268100 120 System Settings devtools-plus\n\
                         click 1275000 120 System Settings devtools-goto\n\
                         click 1276000 120 System Settings devtools-open\n\
                         devtools 1277000 120 pane list=holds switch=off\n";
        assert_eq!(
            cell(plus_only, ""),
            "aterm added by + 277000 ms, 8900 ms after the +; no drag: no window of the app (1 \
             running) shows the helper panel's tile; password not asked; switch off, not pressed; \
             page seen 248000 ms"
        );
        assert_eq!(
            cell(
                &format!("{plus_only}click 1280000 120 System Settings devtools-switch\n"),
                ""
            ),
            "aterm added by + 277000 ms, 8900 ms after the +; no drag: no window of the app (1 \
             running) shows the helper panel's tile; password not asked; switch on 280000 ms \
             (then read off); page seen 248000 ms"
        );
        // A try that stopped: its steps by name; a password dialog read and not typed into; a
        // password typed on another page long after is not this page's.
        let stopped = "devtools 1248000 120 pane list=lacks switch=none\n\
                       click 1268100 120 System Settings devtools-plus\n\
                       skip 1270000 120 System Settings: the administrator's password not typed\n\
                       click 1275000 120 System Settings devtools-goto\n\
                       devtools 1295000 120 skip: the open panel's Open stayed off after the \
                       path's Return\n\
                       click 1295100 120 System Settings devtools-cancel\n\
                       typed 1390000 120 account 5 Managed_via_Tart unwatched System Settings\n";
        assert_eq!(
            cell(stopped, ""),
            "aterm not on the list in the 400 s watched: + pressed 268100 ms, the path's Return \
             275000 ms, Cancel 295100 ms; no drag; skip: the open panel's Open stayed off after \
             the path's Return; password asked 270000 ms, not typed (the focus read a text field \
             not secure); page seen 248000 ms"
        );
        assert_eq!(
            cell(
                "devtools 1248000 120 pane list=lacks switch=none\n\
                 click 1268100 120 System Settings devtools-plus\n\
                 dialog 1269000 120 System Settings: AXSheet; AXTextField/AXSecureTextField \
                 (secure)\n",
                ""
            ),
            "aterm not on the list in the 400 s watched: + pressed 268100 ms; no drag; password \
             asked 269000 ms, not typed; page seen 248000 ms"
        );
        // A person with no password to type reads no password dialog: never "not asked".
        assert_eq!(
            raw(
                &format!("person 1000000 trusted=yes post=yes password=no\n{plus_only}"),
                ""
            ),
            "aterm added by + 277000 ms, 8900 ms after the +; no drag: no window of the app (1 \
             running) shows the helper panel's tile; password NOT MEASURED (the person had none \
             to type); switch off, not pressed; page seen 248000 ms"
        );
        // A person without Accessibility, or with no grants line, read nothing.
        assert_eq!(
            raw("person 1000000 trusted=no post=yes password=yes\n", ""),
            "NOT MEASURED (the person had no Accessibility grant, so it read no window)"
        );
        assert_eq!(
            raw(plus_only, ""),
            "NOT MEASURED (the person's record says none of its grants)"
        );
        assert_eq!(
            cell("devtools 1248000 120 pane list=holds switch=on\n", taken),
            "aterm on the list when the page came; switch on already; page seen 248000 ms",
            "on the list before any add: no add, no password said"
        );
        assert_eq!(
            cell(
                "devtools 1248000 120 pane list=unread switch=none: AXGroup\n",
                ""
            ),
            "NOT MEASURED (the page's list did not read (it read AXGroup); page seen 248000 ms)",
            "judged from the last state"
        );
        assert_eq!(
            cell(
                "devtools 1248000 120 pane list=unread switch=none: AXGroup\n\
                 devtools 1249000 120 pane list=lacks switch=none\n",
                ""
            ),
            "aterm not added in the 400 s watched; page seen 248000 ms",
            "a list that read in the end"
        );
        assert_eq!(
            cell(
                "devtools 1248000 120 pane list=lacks switch=none\n\
                 devtools 1248100 120 skip: no --app, so the person knows no aterm to add\n",
                ""
            ),
            "aterm not added in the 400 s watched; skip: no --app, so the person knows no aterm \
             to add; page seen 248000 ms"
        );
        // Another page's switch and the Full Disk Access password before the page are not it.
        assert_eq!(
            cell(
                "click 1100000 120 System Settings switch:aterm\n\
                 typed 1120000 120 account 5 Managed_via_Tart unwatched System Settings\n",
                taken
            ),
            "not seen in the 400 s watched"
        );
        assert_eq!(
            devtools_value(None, "", open, 1_400_000).to_string(),
            "NOT MEASURED (no person at the console (--person puts one there))"
        );
    }

    #[test]
    fn a_quit_and_reopen_reads_as_the_press_the_quit_the_reopen_and_the_new_pids_first_line() {
        let open = 1_000_000;
        let procs = read_procs(
            "procs 999000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm\n\
             up 1000400 793 start=1000300\n\
             down 1092400 793\n\
             up 1092600 1201 start=1092550\n",
        );
        let log = "1000.500 793 INFO aterm_gui: system state: submitting a plan (Automatic)\n\
                   1092.300 793 INFO aterm_gui: quit & reopen: pid 1199 opens /Applications/aterm.app once this process is gone\n\
                   1093.100 1201 INFO aterm_gui: permission walk: Quit & Reopen left fda; they wait for the walk's start\n";
        let presses = |sheet: &[(u64, &str)], quit: &[u64]| ReopenPresses {
            sheet: sheet.iter().map(|(at, w)| (*at, (*w).to_owned())).collect(),
            quit: quit.to_vec(),
        };
        // The person's Quit & Reopen, then Quit on aterm's question 6.31 s later (the person waits
        // its quiet between two inputs), and aterm gone 90 ms after that.
        let pressed = presses(&[(1_086_000, "Quit & Reopen")], &[1_092_310]);
        let cells = |presses: Option<&ReopenPresses>, later: bool, procs: &Procs, log: &str| {
            reopen_values(presses, later, procs, log, open, 1_200_000)
                .into_iter()
                .map(|(name, value)| format!("{name} {value}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cells(Some(&pressed), false, &procs, log),
            [
                "Quit & Reopen sheet Quit & Reopen pressed 86000 ms (the person answers Quit & Reopen)",
                "aterm's quit question Quit pressed 92310 ms, 6310 ms after Quit & Reopen",
                "aterm processes pid 793 300 ms to 92400 ms; pid 1201 92550 ms to the end",
                "aterm quit pid 793 gone 92400 ms, 90 ms after Quit on aterm's question",
                "aterm reopened pid 1201 started 92550 ms, 150 ms after the quit",
                "reopened pid's first log line 93100 ms",
            ]
        );
        // A quit with no question answered before it is timed from the sheet's press.
        assert_eq!(
            cells(
                Some(&presses(&[(1_092_310, "Quit & Reopen")], &[])),
                false,
                &procs,
                log
            )[3],
            "aterm quit pid 793 gone 92400 ms, 90 ms after Quit & Reopen"
        );
        // Negative controls. Later: the app runs on, its question is not answered, and nothing is
        // said to have quit.
        let ran = read_procs("procs 999000 every=50 exe=/A/aterm\nup 1000400 793 start=1000300\n");
        assert_eq!(
            cells(
                Some(&presses(&[(1_092_310, "Later")], &[])),
                true,
                &ran,
                log
            )[1..],
            [
                "aterm's quit question not answered: the person answers it only after its own Quit & Reopen",
                "aterm processes pid 793 300 ms to the end",
                "aterm quit none: pid 793 ran to the end in the 200 s watched",
                "aterm reopened none: nothing quit",
            ]
        );
        // Run 15's shape had the person answered no question: Quit & Reopen pressed, aterm never
        // asked (or asked unseen), and it ran on.
        assert_eq!(
            cells(
                Some(&presses(&[(1_092_310, "Quit & Reopen")], &[])),
                false,
                &ran,
                log
            )[1..4],
            [
                "aterm's quit question not seen in the 200 s watched",
                "aterm processes pid 793 300 ms to the end",
                "aterm quit none: pid 793 ran to the end in the 200 s watched",
            ]
        );
        assert!(
            cells(Some(&presses(&[], &[])), true, &ran, log)[0]
                .ends_with("not seen in the 200 s watched (the person answers Later (--later))")
        );
        // A quit with nothing after it is said, never a reopen; a pid with no log line is NOT
        // MEASURED, never the first process's line.
        let gone = read_procs(
            "procs 999000 every=50 exe=/A/aterm\nup 1000400 793 start=1000300\ndown 1092400 793\n",
        );
        assert_eq!(
            cells(None, false, &gone, log)[4],
            "aterm reopened none: no aterm app process started again in the 200 s watched"
        );
        let unlogged = cells(
            Some(&pressed),
            false,
            &procs,
            &log.lines().take(2).collect::<Vec<_>>().join("\n"),
        );
        assert_eq!(
            unlogged[5],
            "reopened pid's first log line NOT MEASURED (aterm.log has no line from pid 1201 in the 200 s watched)"
        );
        // No person, no watch record, an unreadable process table: NOT MEASURED, never a guess.
        let nobody = cells(None, false, &procs, log);
        for cell in &nobody[..2] {
            assert!(
                cell.contains("NOT MEASURED (no person at the console"),
                "{cell}"
            );
        }
        for (procs, why) in [
            (read_procs(""), "the process watch wrote nothing"),
            (
                read_procs(
                    "procs 999000 every=50 exe=/A/aterm\nprocs-error 999050 the kernel's process table did not read\n",
                ),
                "the process watch: the kernel's process table did not read",
            ),
            (
                read_procs("procs 999000 every=50 exe=/A/aterm\n"),
                "the process watch saw no aterm app process in the 200 s watched",
            ),
        ] {
            let said = cells(Some(&pressed), false, &procs, log);
            assert_eq!(said.len(), 5, "{why}");
            for cell in &said[2..] {
                assert!(cell.ends_with(&format!("NOT MEASURED ({why})")), "{cell}");
            }
        }
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
        assert_eq!(first_present(m), Ok(Some(1315)));
        // 0 until the first terminal present: a guest whose window System State fronts.
        let none_yet = "OK backend=gpu frames=3688 rust_main_to_first_present_ms=0.00";
        assert_eq!(first_present(none_yet), Ok(None));
        assert_eq!(first_present("ERR no window"), Err(()));

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
    fn only_a_standard_accounts_person_has_the_administrator_at_hand() {
        let out = "/var/tmp/aterm-nux/person.txt";
        let app = "/Applications/aterm.app/Contents/MacOS/aterm";
        assert_eq!(
            person_args(out, false, false, app),
            [
                PERSON_HELPER,
                "person",
                out,
                "--password",
                "admin",
                "--app",
                app
            ]
        );
        assert_eq!(
            person_args(out, false, true, app),
            [
                PERSON_HELPER,
                "person",
                out,
                "--password",
                "admin",
                "--sheet",
                "later",
                "--app",
                app
            ]
        );
        assert_eq!(
            person_args(out, true, false, app),
            [
                PERSON_HELPER,
                "person",
                out,
                "--password",
                STANDARD_PASSWORD,
                "--admin",
                "admin:admin",
                "--app",
                app
            ]
        );
        // The cell names whose password went in and the name field, never a password; the
        // account's own password is typed once, its dialog not watched.
        let p = read_person(
            "dialog 900 812 System Settings: AXSheet; AXTextField \"NUX Standard\"\n\
             focus 950 812 name AXTextField \"admin\"\n\
             typed 1000 812 administrator 5 NUX_Standard>admin stayed System Settings\n\
             focus 1200 812 password AXTextField \"admin\"\n\
             skip 1210 812 System Settings: the administrator's password not typed (the focus read \
             a text field not secure)\n\
             typed 1500 812 administrator 5 admin went:640 System Settings\n\
             typed 2000 640 account 13 - unwatched SecurityAgent\n",
        );
        let skipped = p.dialogs.iter().filter(|(_, k, _)| k == "skip").count();
        assert_eq!(
            typed_cell(&p.typed, skipped),
            Value::Text(
                "3: System Settings, the administrator's (name field NUX_Standard>admin), the \
                 dialog stayed; System Settings, the administrator's (name field admin), the \
                 dialog went 640 ms after Return; SecurityAgent, the account's; 1 try typed \
                 nothing (the focus read a text field not secure)"
                    .into()
            )
        );
        assert_eq!(
            p.typed.iter().map(|t| t.went).collect::<Vec<_>>(),
            [Went::Stayed, Went::After(640), Went::Unwatched]
        );
        assert_eq!(typed_cell(&[], 0), Value::Count(0));
        assert_eq!(
            p.dialogs,
            [
                (
                    900,
                    "dialog".to_owned(),
                    "System Settings: AXSheet; AXTextField \"NUX Standard\"".to_owned()
                ),
                (
                    950,
                    "focus".to_owned(),
                    "name AXTextField \"admin\"".to_owned()
                ),
                (
                    1200,
                    "focus".to_owned(),
                    "password AXTextField \"admin\"".to_owned()
                ),
                (
                    1210,
                    "skip".to_owned(),
                    "System Settings: the administrator's password not typed (the focus read a \
                     text field not secure)"
                        .to_owned()
                )
            ]
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
    fn an_init_exit_names_the_rows_it_left_open() {
        // On a terminal: the prompts' lines, a change, and the rows left open with their whys.
        let text = "Secrets vault URL or path (none skips): none\r\n\
                    changed: Secrets vault (secrets.vault)\r\n    synced into ~/.secrets\r\n\
                    failed: Command Line Tools (base.clt)\r\n    failed: not (this.why)\r\n\
                    waiting: GitHub sign-in (github.auth@alice)\r\n\
                    aterm init: 30 ok \u{b7} 1 changed \u{b7} 1 failed\r\n";
        assert_eq!(
            open_rows(text),
            ["failed base.clt", "waiting github.auth@alice"]
        );
        // A `--check` run's rows that still differ read `planned`, and fail it.
        let check = "planned: Caps Lock on keyboard 1452-1-0 (keyboard.caps-lock.1452-1-0@admin)\r\n\
                     needs action: Debugger access (init.permissions.debugger)\r\n";
        assert_eq!(
            open_rows(check),
            [
                "planned keyboard.caps-lock.1452-1-0@admin",
                "needs action init.permissions.debugger"
            ]
        );
        // A `--json` report names each row's outcome, after the prompts' lines.
        let json = "Secrets vault URL or path (none skips): none\r\n\
                    {\"items\":[{\"id\":\"a\",\"outcome\":\"ok\"},{\"id\":\"base.clt\",\"outcome\":\"failed\"},\
                    {\"id\":\"p\",\"outcome\":\"person\"},{\"id\":\"n\",\"outcome\":\"not-applicable\"},\
                    {\"id\":\"c\",\"outcome\":\"changed\"}]}\r\n";
        assert_eq!(open_rows(json), ["failed base.clt", "person p"]);
        assert!(open_rows("aterm init: 41 ok\r\n").is_empty());

        let mut row = Row::new(Scenario::VaultLocal);
        let mut d = Drive {
            exit: Some(0),
            ..Drive::default()
        };
        open_rows_cell(&mut row, "", &d, text);
        assert!(row.cells.is_empty(), "a clean exit names nothing");
        d.exit = Some(1);
        open_rows_cell(&mut row, "", &d, text);
        open_rows_cell(&mut row, "again: ", &d, "aterm init: 1 failed\r\n");
        assert_eq!(
            row.cells
                .iter()
                .map(|(k, v)| format!("{k} = {v}"))
                .collect::<Vec<_>>(),
            [
                "rows left open = failed base.clt, waiting github.auth@alice",
                "again: rows left open = NOT MEASURED (init's output names no row left open)",
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
        assert!(PROMPT_MARKER.contains(&format!(">> {GUEST_NUX}/prompts;")));
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

    /// A run beside the pass says, in its row, a JSON report's completeness and its unresolved
    /// rows, or its printed lines with the terminal's escape sequences dropped.
    #[test]
    fn a_run_beside_the_pass_says_its_unresolved_rows_or_its_lines() {
        let json = r#"{"schema":1,"mode":"apply","complete":false,"items":[{"id":"base.shell.rc","outcome":"blocked","why":"the hooks are not current"},{"id":"base.pkg.toolset","outcome":"planned","from":"no ALab programs are installed"}],"readiness":{"all_green":false,"unresolved":["base.shell.rc","base.pkg.toolset"]}}"#;
        assert_eq!(
            said_beside_the_pass(&format!("a prompt line\n{json}\n")),
            "complete false; unresolved: base.shell.rc blocked: the hooks are not current; \
             base.pkg.toolset planned: no ALab programs are installed"
        );
        let tty = "\u{1b}[1mrepair:\u{1b}[0m ok\r\n\u{1b}]0;aterm\u{7}\n\nwaits /\rwaits -\r\u{1b}[2K\
                   2 rows | done\n";
        assert_eq!(said_beside_the_pass(tty), "repair: ok / 2 rows \u{a6} done");
    }
}
