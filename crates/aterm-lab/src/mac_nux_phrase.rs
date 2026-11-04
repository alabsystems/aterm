// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab mac nux`'s `vault-phrase` row: the owner's acceptance test ("a new LAN Mac downloads
//! aterm, joins with one keyphrase approval from any existing member, and GitHub, publishing and
//! AI accounts work") as one command, in one guest (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10).
//!
//! Three identities share the guest, each a scratch `$HOME` under the lane's folder (as
//! `vault-local` gives its admin one of its own):
//!
//! - The seed founds a vault served on `127.0.0.1` (`aterm secrets serve --no-advertise`, then
//!   `secrets create`, which founds its Commons), whose `[everyone]` holds three synthetic files
//!   ([`CREDENTIALS`]: a GitHub token, a publish token and an AI accounts list, each made of
//!   random bytes in the guest). It is the vault's one admin, and its approver (`aterm secrets
//!   watch`) is what admits the joiner on the member's Allow.
//! - The member ([`MEMBER_NAME`]) is an existing member that is no admin, so the Allow comes
//!   from "any existing member" and the approver admits an Allow another Mac signed: it asks
//!   (`aterm secrets join --name`), the seed approves it by its code (`aterm secrets approve`),
//!   it syncs until In, and it enrols in the vault's Commons with the seed (the guest helper's
//!   `enrol`: the control exchange the Commons runtime runs over the LAN, run over loopback in
//!   one process, as `lab swarm partition` runs it). Then the guest helper's `member` runs F10's
//!   members desk on its custody behind an `ask`-only listener at the guest's own address on
//!   tart's network ([`lan_address`]), registered under the run's lab members service as the
//!   node job registers a member's fleet port, presses Allow for the first Mac that asks and
//!   writes the phrase to its record ([`read_desk`]). The shipped member shows a phrase only in
//!   its own sheet (§3.3.5) and the lane reads no window, so the phrase is read there, as `lab
//!   swarm partition` presses Allow in its own process.
//! - The joiner is the build's own CLI: `aterm members join --no-wait` (install.sh's windowless
//!   ask), then `aterm members join` on a terminal with the phrase typed at its prompt
//!   ([`super::DRIVE`]'s `$NUX_PHRASE`), then its own subscriber (`aterm secrets watch`), as a
//!   joined Mac's agent runs it. It is named no member (`--at`): each run finds the member as a
//!   new Mac does, by `discover::browse_members` (`dns-sd -B`, `parse_browse_members_of`, `dns-sd
//!   -L` of each instance, `parse_reached`), and asks it over the guest's network at the address
//!   that resolve gives.
//!
//! Isolation, which is why the browse runs on a lab service. This Mac is a live ALab member, and
//! mDNSResponder announces its own registrations (its members desk's `_alab._tcp`, its vault's
//! `_alab-vault._tcp`) on tart's bridge while a guest runs, so a guest that browsed `_alab._tcp`
//! could hear this Mac's desk and ask it, putting a real Allow row on its owner's screen. The
//! joiner instead runs with `ATERM_LAB_MEMBER_SERVICE` naming `_alab-lab-<six random base-36
//! digits>._tcp`, made for the run ([`lab_service_of`]): `browse_members` then browses that type
//! and no other, takes no value that is not a lab service (it fails the browse rather than fall
//! back to `_alab._tcp`), and no ALab Mac registers or browses it, so whatever else the bridge
//! carries, no live member is in the joiner's browse and no live aterm process hears the
//! member's advert. Every one of the joiner's commands carries the variable ([`as_joiner`]), and
//! the row runs none of them on an app whose executable does not name it ([`names_knob`]), so a
//! build without the knob, which would browse `_alab._tcp`, never runs here. Choosing the type
//! rather than a network: the guest's mDNS reaches this Mac's bridge whatever it registers, and
//! a firewall in the guest that blocked it would make this Mac's live desk the fallback of any
//! rule that failed to load; a type nobody else uses needs no rule to hold. The member's advert
//! of the lab type does reach this Mac's mDNSResponder over the bridge (the row shows it, `lab
//! adverts this Mac heard`), where no aterm process browses that type.
//!
//! The row times the ask to the member's row, to the phrase shown and typed, to each synthetic
//! file in the joiner's `~/.secrets` (compared with the seed's in the guest, never printed) and to
//! `aterm members status` reading In. Before the joiner runs it waits until the guest's own
//! browse of the lab service hears the member's instance resolving to the guest, and refuses to
//! go on when it hears any other. While every server still runs it reads where the member heard
//! each ask from, each aterm listener's address, the guest's registrations, and the
//! `_alab-vault._tcp` and `_alab._tcp` adverts heard in the guest and, from a guest, on this
//! Mac, each placed by where its host resolves ([`super::advert`]): this Mac's own
//! registrations, which mDNSResponder also announces on tart's bridge, are named in the row and
//! never fail it. [`verdict`] fails the row on a phrase that came from any Mac but the member, a
//! file missing or not the seed's, a joiner that never reads In, a browse the row cannot show
//! found the member and nothing else ([`browse_verdict`]: the member asked over loopback, or the
//! guest's browse heard no member or another one), or anything that reached beyond the guest
//! ([`isolation_verdict`]: an ask from an address that is not the guest's, a listener beyond
//! loopback but the member's, a registration but the member's lab one, a real-service advert
//! that is not this Mac's own, or a browse whose transcript shows it never looked).
//!
//! The member's Commons enrolment stays the guest helper's in-process exchange over loopback
//! (the row's `enrolled by the runtime` cell says NOT MEASURED). The Commons runtime finds its
//! peers by beacons to one multicast group and port (`aterm_db::runtime`'s transport), which a
//! runtime on the guest's network would send onto tart's bridge, where this Mac's live runtime
//! listens; the only other port it takes is a debug build's, loopback alone by design
//! (`ATERM_DEBUG_COMMONS_LOOPBACK_PORT`, which refuses any LAN attachment) and absent from the
//! release-shaped app the row runs. Running it over the guest's network needs a lab port the
//! runtime keeps apart from the live one on the LAN, in `aterm-db`'s transport.

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_secrets::discover::{LAB_MEMBER_SERVICE_ENV, MEMBER_SERVICE};

use super::advert::{self, Split, Whose};
use super::{
    ALAB_SERVICE, GUEST_HELPER, GUEST_NUX, Guest, LOCAL_VAULT, LOCAL_VAULT_PORT, Row, Value,
    browse_adds, distinct_messages, drive_cells_within, guest_browse, guest_dns_sd, guest_heard,
    host_guests, local_network_blocks, or_missing, secs_to_ms, squeeze,
};
use crate::mac::{POLL, STEP};
use crate::{Fail, stop};

/// The Allowing member's `ask` port, beside the vault's [`LOCAL_VAULT_PORT`]: never the fleet
/// port (7100) a window serves.
pub const MEMBER_PORT: u16 = 7486;

/// The seed's machine name in its vault: its one admin.
const SEED_NAME: &str = "nux-seed";

/// The Allowing member's machine name: a member, never an admin.
pub const MEMBER_NAME: &str = "nux-member";

/// The Allowing member's desk record, in [`GUEST_NUX`].
const DESK_RECORD: &str = "member-desk.txt";

/// The hang detector on each wait the row needs to end (the seed's server and member answering,
/// the phrase shown, each run of `aterm members join`, the joiner's In): each comes within
/// seconds. Every wait is the lane's own loop or a driver's limit, never a shell loop.
const HANG: Duration = Duration::from_secs(60);

/// One synthetic `[everyone]` file: its name in `~/.secrets`, what a Mac does with it, and the
/// `printf` format its value is made by (`%s`: 24 random hex digits).
pub struct Credential {
    pub file: &'static str,
    pub what: &'static str,
    synthetic: &'static str,
}

/// The files a new Mac needs for GitHub, publishing and its AI accounts, as the seed's
/// `[everyone]` holds them (the names `tests/flows.rs`'s install-lane test hands over).
pub const CREDENTIALS: [Credential; 3] = [
    Credential {
        file: "gh_access_token",
        what: "GitHub",
        synthetic: "nux-synthetic-github-%s",
    },
    Credential {
        file: "gh_access_token_alabsystems",
        what: "publishing",
        synthetic: "nux-synthetic-publish-%s",
    },
    Credential {
        file: "ai-accounts.csv",
        what: "AI accounts",
        synthetic: "claude,nux-synthetic-%s@example.invalid\\n",
    },
];

/// The services an ALab vault server and an ALab member advertise: none may be heard.
pub const SERVICES: [&str; 2] = [ALAB_SERVICE, MEMBER_SERVICE];

/// The guest commands that make the row's homes: the seed's, with the three synthetic files
/// (random bytes from the guest's own `/dev/urandom`, each `chmod 600`) under `seed/seed`, and
/// empty ones for the member and the joiner; and that clear the member's desk record.
fn homes(h: &Homes, record: &str) -> String {
    let Homes {
        seed,
        member,
        joiner,
    } = h;
    let files: String = CREDENTIALS
        .iter()
        .map(|c| {
            format!(
                "printf '{}' \"$(rand)\" > {seed}/seed/{}; ",
                c.synthetic, c.file
            )
        })
        .collect();
    format!(
        "set -e; rm -rf {seed} {member} {joiner} {record}; mkdir -p {seed}/seed {member} {joiner}; \
         rand() {{ od -An -N12 -tx1 /dev/urandom | tr -d ' \\n'; }}; {files}chmod 600 {seed}/seed/*"
    )
}

/// `aterm`'s command line with `$HOME` set to `home`: one identity of the guest's.
fn as_home(aterm: &str, home: &str) -> String {
    format!("/usr/bin/env HOME={home} {aterm}")
}

/// `aterm`'s command line as the joiner: `$HOME` set to `home` and [`LAB_MEMBER_SERVICE_ENV`] to
/// the run's lab members service, so every browse for members it makes (its ask, its join, its
/// subscriber's asks while it waits) looks for the lab's member alone.
fn as_joiner(aterm: &str, home: &str, service: &str) -> String {
    format!("/usr/bin/env HOME={home} {LAB_MEMBER_SERVICE_ENV}={service} {aterm}")
}

/// The run's lab members service, from six random bytes: `_alab-lab-<six base-36 digits>._tcp`
/// (`aterm_secrets::discover::lab_service`), a type no ALab Mac registers or browses.
#[must_use]
pub fn lab_service_of(bytes: [u8; 6]) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let nonce: String = bytes
        .iter()
        .map(|b| char::from(DIGITS[usize::from(*b) % DIGITS.len()]))
        .collect();
    format!("_alab-lab-{nonce}._tcp")
}

/// A fresh lab members service for this run ([`lab_service_of`] of this Mac's random bytes).
///
/// # Errors
/// This Mac gave no random bytes.
fn fresh_lab_service() -> Result<String, Fail> {
    let mut bytes = [0u8; 6];
    aterm_uds::rand::fill(&mut bytes).map_err(|e| Fail::die(format!("random bytes: {e}")))?;
    Ok(lab_service_of(bytes))
}

/// The guest command that says whether the app's executable `aterm` names
/// [`LAB_MEMBER_SERVICE_ENV`] (`named`, else `absent`). A build without the knob would browse the
/// live fleet's `_alab._tcp` whatever the variable says, so the joiner never runs on one.
fn names_knob(aterm: &str) -> String {
    format!(
        "/usr/bin/grep -q -a -F {LAB_MEMBER_SERVICE_ENV} '{aterm}' && echo named || echo absent"
    )
}

/// The guest command that prints its interfaces (`ifconfig`), for [`inet_addresses`].
const GUEST_ADDRESSES: &str = "/sbin/ifconfig -a 2>/dev/null || true";

/// Every address `ifconfig`'s `inet` and `inet6` lines name, each once (an IPv6 scope such as
/// `%en0` dropped).
#[must_use]
pub fn inet_addresses(ifconfig: &str) -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = Vec::new();
    for line in ifconfig.lines() {
        let mut words = line.split_whitespace();
        if !matches!(words.next(), Some("inet" | "inet6")) {
            continue;
        }
        let Some(address) = words
            .next()
            .and_then(|w| w.split('%').next())
            .and_then(|w| w.parse::<IpAddr>().ok())
        else {
            continue;
        };
        if !found.contains(&address) {
            found.push(address);
        }
    }
    found
}

/// The guest's address on tart's network, where the member listens and its advert resolves: the
/// first IPv4 address of `addresses` that is not loopback, link-local or unspecified.
#[must_use]
pub fn lan_address(addresses: &[IpAddr]) -> Option<Ipv4Addr> {
    addresses.iter().find_map(|address| match address {
        IpAddr::V4(v4) if !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()) => {
            Some(*v4)
        }
        _ => None,
    })
}

/// The guest command that starts the seed's vault server on 127.0.0.1, advertising nothing, in
/// the background (its log `seed/serve.log`).
fn serve(aterm: &str, seed: &str) -> String {
    format!(
        "({} secrets serve --listen 127.0.0.1:{LOCAL_VAULT_PORT} --no-advertise --no-keep-awake \
         --store {seed}/served </dev/null >{seed}/serve.log 2>&1 &)",
        as_home(aterm, seed)
    )
}

/// The guest command that prints the HTTP status the seed's server answers with (`000`: none).
fn answers() -> String {
    format!(
        "curl -s -o /dev/null --max-time 1 -w '%{{http_code}}' http://127.0.0.1:{LOCAL_VAULT_PORT}/ \
         || true"
    )
}

/// The guest command that creates the vault on the seed's server with the synthetic files in
/// `[everyone]`, the seed its first admin named [`SEED_NAME`]; what it printed when it fails.
fn create(aterm: &str, seed: &str) -> String {
    let url = format!("http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}");
    format!(
        "{} secrets create '{url}' --import {seed}/seed --no-agent --name {SEED_NAME} \
         > {seed}/create.out 2>&1 || {{ cat {seed}/create.out; exit 1; }}",
        as_home(aterm, seed)
    )
}

/// The guest command that starts the vault subscriber of the identity `me` runs as ([`as_home`],
/// [`as_joiner`]), whose home is `home`, as a Mac's agent runs it, in the background (its log
/// `home/watch.log`): the seed's is its approver.
fn watch(me: &str, home: &str) -> String {
    format!("({me} secrets watch </dev/null >{home}/watch.log 2>&1 &)")
}

/// The guest command that asks to join the seed's vault as `member`, named [`MEMBER_NAME`],
/// pinned to its `signer` ([`signer_of`]); what it printed when it fails.
fn member_join(aterm: &str, member: &str, signer: &str) -> String {
    format!(
        "{} secrets join 'http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}#{signer}' \
         --name {MEMBER_NAME} --no-agent --no-wait > {member}/join.out 2>&1 \
         || {{ cat {member}/join.out; exit 1; }}",
        as_home(aterm, member)
    )
}

/// The guest command that approves, as the seed, the request whose join `code` the member was
/// given ([`approve_code`]); what it printed when it fails. By its code, as a person at an admin
/// Mac approves one: `approve --all` asks for a yes at a terminal, which `tart exec` has not.
fn approve(aterm: &str, seed: &str, code: &str) -> String {
    format!(
        "{} secrets approve {code} > {seed}/approve.out 2>&1 || {{ cat {seed}/approve.out; exit 1; }}",
        as_home(aterm, seed)
    )
}

/// The guest command that prints the member's `secrets status`, whose membership line names the
/// command an admin approves it with.
fn member_status(aterm: &str, member: &str) -> String {
    format!("{} secrets status 2>&1 || true", as_home(aterm, member))
}

/// The join code in the `aterm secrets approve <code>` that `secrets join` and `secrets status`
/// print for a Mac waiting to join.
#[must_use]
pub fn approve_code(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|l| {
            l.split_once("aterm secrets approve ")?
                .1
                .split_whitespace()
                .next()
        })
        .map(|word| word.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')))
        .find(|code| !code.is_empty() && !code.starts_with('<'))
}

/// The guest command that syncs `member` once and prints its `members status` line and the
/// guest's clock, for [`read_status`].
fn sync_now(aterm: &str, member: &str) -> String {
    format!(
        "{} secrets sync >> {member}/sync.out 2>&1; {}",
        as_home(aterm, member),
        status_now(&as_home(aterm, member))
    )
}

/// The guest command that enrols `member` in the seed's Commons (the guest helper's `enrol`):
/// `enrolled <epoch ms>`.
fn enrol(helper: &str, member: &str, seed: &str) -> String {
    format!("/usr/bin/env HOME={member} {helper} enrol {member} {seed}")
}

/// The signer `secrets create` printed in its `aterm secrets join <url>#<signer>` line, which a
/// Mac joining the vault pins.
#[must_use]
pub fn signer_of(created: &str) -> Option<&str> {
    created
        .lines()
        .filter_map(|l| {
            l.split_once("aterm secrets join ")?
                .1
                .split_whitespace()
                .next()
        })
        .find_map(|url| url.rsplit_once('#').map(|(_, signer)| signer))
        .filter(|signer| !signer.is_empty())
}

/// The guest command that starts the guest helper's `member` on `home`'s custody, listening at
/// `listen` (the guest's own address, [`MEMBER_PORT`]) and registered there under the lab members
/// service `service`, in the background (its record `record`, its errors `home/member.err`).
fn desk(helper: &str, home: &str, listen: &str, record: &str, service: &str) -> String {
    format!(
        "(/usr/bin/env HOME={home} {helper} member {home} {listen} {record} --advertise {service} \
         </dev/null >/dev/null 2>{home}/member.err &)"
    )
}

/// `look` again every [`POLL`] until it reads what the row waits for, under the [`HANG`]
/// detector: `Some` once it does, `None` when the detector fired first.
///
/// # Errors
/// `look`'s own (the guest could not be read), or the run was stopped.
fn within_hang<T>(mut look: impl FnMut() -> Result<Option<T>, Fail>) -> Result<Option<T>, Fail> {
    let started = Instant::now();
    loop {
        if let Some(seen) = look()? {
            return Ok(Some(seen));
        }
        if started.elapsed() >= HANG {
            return Ok(None);
        }
        stop::sleep(POLL)?;
    }
}

/// The last lines of a guest log, for a failure that names it ("" when it is empty).
fn tail_of(g: &Guest, path: &str) -> Result<String, Fail> {
    let text = g.read(path)?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    Ok(squeeze(
        &lines[lines.len().saturating_sub(3)..].join(" | "),
        300,
    ))
}

/// The row's three homes.
struct Homes {
    seed: String,
    member: String,
    joiner: String,
}

/// Where the member serves the joiner: its listen address (the guest's own, [`MEMBER_PORT`]) and
/// the run's lab members service it is registered under.
struct Lab {
    listen: String,
    service: String,
}

/// The seed and the member, ready: the seed's homes, server (answering within [`HANG`]), vault
/// and approver; the member asked, approved by the seed, In (within [`HANG`]) and enrolled in the
/// seed's Commons; and the member's desk serving at `lab`'s address under its service (within
/// [`HANG`]). Each step is its own guest command, so a failure names the step and its output or
/// log; the member's steps are timed in `row`.
///
/// # Errors
/// A step failed, or a wait's detector fired: the row cannot go on.
fn setup(g: &Guest, h: &Homes, record: &str, lab: &Lab, row: &mut Row) -> Result<Desk, Fail> {
    let helper = format!("{GUEST_NUX}/{GUEST_HELPER}");
    let Homes { seed, member, .. } = h;
    g.sh(&homes(h, record), STEP)?;
    g.sh(&serve(&g.aterm, seed), STEP)?;
    let up = within_hang(|| {
        let code = g.run(&answers(), STEP)?;
        Ok((!matches!(code.trim(), "" | "000")).then_some(()))
    })?;
    if up.is_none() {
        return Err(Fail::die(format!(
            "the seed's vault server did not answer within {} s: {}",
            HANG.as_secs(),
            tail_of(g, &format!("{seed}/serve.log"))?
        )));
    }
    g.sh(&create(&g.aterm, seed), STEP)?;
    let created = g.read(&format!("{seed}/create.out"))?;
    let signer = signer_of(&created).ok_or_else(|| {
        Fail::die(format!(
            "secrets create printed no signer: {}",
            squeeze(created.trim(), 300)
        ))
    })?;

    // The member: an existing member that is no admin.
    let started = std::time::Instant::now();
    g.sh(&member_join(&g.aterm, member, signer), STEP)?;
    let waiting = format!(
        "{}\n{}",
        g.read(&format!("{member}/join.out"))?,
        g.run(&member_status(&g.aterm, member), STEP)?
    );
    let code = approve_code(&waiting).ok_or_else(|| {
        Fail::die(format!(
            "the member's join named no code to approve: {}",
            squeeze(waiting.trim(), 300)
        ))
    })?;
    g.sh(&approve(&g.aterm, seed, code), STEP)?;
    let mut last = Standing::Unread(String::new());
    let joined = within_hang(|| {
        let (standing, _) = read_status(&g.run(&sync_now(&g.aterm, member), STEP)?);
        let done = (standing == Standing::In).then_some(());
        last = standing;
        Ok(done)
    })?;
    if joined.is_none() {
        return Err(Fail::die(format!(
            "the member stayed {last} {} s after the seed approved it: approve said {}; its sync: {}",
            HANG.as_secs(),
            tail_of(g, &format!("{seed}/approve.out"))?,
            tail_of(g, &format!("{member}/sync.out"))?
        )));
    }
    let enrolled = g.sh(&enrol(&helper, member, seed), STEP)?;
    if !enrolled.trim_start().starts_with("enrolled ") {
        return Err(Fail::die(format!(
            "the member's Commons enrolment said {}",
            squeeze(enrolled.trim(), 200)
        )));
    }
    row.cell(
        "member: asked to In and enrolled",
        Value::Ms(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
    );

    g.sh(&watch(&as_home(&g.aterm, seed), seed), STEP)?;
    g.sh(
        &desk(&helper, member, &lab.listen, record, &lab.service),
        STEP,
    )?;
    let read = within_hang(|| {
        let at = read_desk(&g.read(record)?);
        Ok((at.serving.is_some() || at.refused.is_some()).then_some(at))
    })?;
    match read {
        None => Err(Fail::die(format!(
            "the member's desk wrote nothing within {} s: {}",
            HANG.as_secs(),
            tail_of(g, &format!("{member}/member.err"))?
        ))),
        Some(Desk {
            refused: Some(why), ..
        }) => Err(Fail::die(format!(
            "the member's desk refused to serve: {why}"
        ))),
        Some(at) => Ok(at),
    }
}

/// The guest command that prints, for each [`CREDENTIALS`] file, `<file> <mtime s|none>
/// <same|differs|absent>`: its arrival in the joiner's `~/.secrets` and whether it is the seed's
/// value (the guest helper's `arrivals`, which prints neither value).
fn arrivals(helper: &str, seed: &str, joiner: &str) -> String {
    let names: Vec<&str> = CREDENTIALS.iter().map(|c| c.file).collect();
    format!(
        "{helper} arrivals {seed}/seed {joiner}/.secrets {}",
        names.join(" ")
    )
}

/// The guest command that prints `aterm members status`'s first line as `me` and then the
/// guest's clock (epoch ms), for [`read_status`].
fn status_now(me: &str) -> String {
    format!(
        "{me} members status 2>&1 | head -1; /usr/bin/perl -MTime::HiRes=time -e 'printf \"%d\\n\", time * 1000'"
    )
}

/// The guest command that lists every TCP listener of an aterm process (the seed's server,
/// approver and member, the joiner's subscriber).
const LISTENERS: &str = "lsof -nP -a -iTCP -sTCP:LISTEN -c aterm 2>/dev/null || true";

/// The guest command that lists every `dns-sd` process with its arguments (`pid args…`), for
/// [`registrations`].
const DNS_SD_PROCESSES: &str = "pgrep -lf dns-sd || true";

/// The Bonjour registrations among [`DNS_SD_PROCESSES`]' lines: each `dns-sd` whose arguments
/// hold `-R` or `-P` anywhere, as aterm registers (`aterm_secrets::server::registration_args`:
/// `dns-sd -P …`, and `dns-sd -i en0 -P …` scoped to one interface on a Mac on several
/// networks). One running is the guest advertising, whatever a browse hears. A browse
/// (`dns-sd -B`), the `script` a browse runs under and the shell that ran `pgrep` are none.
#[must_use]
pub fn registrations(pgrep: &str) -> Vec<String> {
    pgrep
        .lines()
        .map(str::trim)
        .filter(|line| {
            let mut words = line.split_whitespace().skip(1);
            words
                .next()
                .is_some_and(|program| program.rsplit('/').next() == Some("dns-sd"))
                && words.any(|w| w == "-R" || w == "-P")
        })
        .map(|line| squeeze(line, 120))
        .collect()
}

/// One Mac the member heard ask: when, the name it asks as, its join code, and the address it
/// asked from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub at: u64,
    pub name: String,
    pub code: String,
    pub from: Option<String>,
}

/// The phrase the member showed: when, for which Mac, and its words (never in a `Debug` or a
/// cell: it lets a Mac in for 5 minutes).
#[derive(Clone, PartialEq, Eq)]
pub struct Presented {
    pub at: u64,
    pub name: String,
    pub words: String,
}

impl std::fmt::Debug for Presented {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presented")
            .field("at", &self.at)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// What the member's desk record says (the guest helper's `member` verb,
/// `src/guest/member.rs`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Desk {
    /// The instance and service its ask port was registered under.
    pub advertised: Option<(String, String)>,
    /// When it began to serve, at which address, as which member.
    pub serving: Option<(u64, String, String)>,
    /// Why it could not serve.
    pub refused: Option<String>,
    pub heard: Vec<Heard>,
    pub presented: Vec<Presented>,
    pub allow_failed: Option<String>,
    /// When a run's bundle reached the new Mac (it acknowledged it), and which Mac.
    pub let_in: Option<(u64, String)>,
    /// A run whose bundle the new Mac never acknowledged: when, to which Mac, and why.
    pub unacknowledged: Option<(u64, String, String)>,
    /// The request it posted with its `Allowed`: when it was taken, or why not.
    pub posted: Option<Result<u64, String>>,
    /// The address of each exchange that let no Mac in.
    pub asked: Vec<String>,
    /// The exchanges that ended in an error.
    pub ended: Vec<String>,
}

impl Desk {
    /// Every address it heard an ask from (its `heard`, `asked` and `ask-ended` lines), each
    /// once, in the order recorded by kind.
    #[must_use]
    pub fn sources(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let all = self
            .heard
            .iter()
            .filter_map(|h| h.from.as_deref())
            .chain(self.asked.iter().map(String::as_str))
            .chain(self.ended.iter().filter_map(|e| field(e, "from")));
        for from in all {
            if !out.iter().any(|o| o == from) {
                out.push(from.to_owned());
            }
        }
        out
    }
}

/// The value of `key=` among `rest`'s words.
fn field<'a>(rest: &'a str, key: &str) -> Option<&'a str> {
    rest.split_whitespace()
        .find_map(|w| w.strip_prefix(key).and_then(|w| w.strip_prefix('=')))
}

/// [`Desk`] over the record's lines (`<kind> <epoch ms> <rest>`); a line it cannot read is
/// skipped.
#[must_use]
pub fn read_desk(text: &str) -> Desk {
    let mut seed = Desk::default();
    for line in text.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(kind), Some(at)) = (
            parts.next(),
            parts.next().and_then(|a| a.parse::<u64>().ok()),
        ) else {
            continue;
        };
        let rest = parts.next().unwrap_or("").trim();
        match kind {
            "advertised" => {
                if let (Some(instance), Some(service)) =
                    (field(rest, "instance"), field(rest, "service"))
                {
                    seed.advertised = Some((instance.to_owned(), service.to_owned()));
                }
            }
            "member" => {
                if let (Some(listen), Some(name)) = (field(rest, "listen"), field(rest, "name")) {
                    seed.serving = Some((at, listen.to_owned(), name.to_owned()));
                }
            }
            "refused" => seed.refused = Some(rest.to_owned()),
            "heard" => {
                if let (Some(name), Some(code)) = (field(rest, "name"), field(rest, "code")) {
                    seed.heard.push(Heard {
                        at,
                        name: name.to_owned(),
                        code: code.to_owned(),
                        from: field(rest, "from").map(str::to_owned),
                    });
                }
            }
            "asked" => {
                if let Some(from) = field(rest, "from") {
                    seed.asked.push(from.to_owned());
                }
            }
            "phrase" => {
                let words: Vec<&str> = rest
                    .split_whitespace()
                    .filter(|w| !w.contains('='))
                    .collect();
                if let Some(name) = field(rest, "name") {
                    seed.presented.push(Presented {
                        at,
                        name: name.to_owned(),
                        words: words.join(" "),
                    });
                }
            }
            "allow-failed" => seed.allow_failed = Some(rest.to_owned()),
            "let-in" => {
                if let Some(name) = field(rest, "name") {
                    seed.let_in = Some((at, name.to_owned()));
                }
            }
            "sent-unacknowledged" => {
                if let Some(name) = field(rest, "name") {
                    let why = rest
                        .split_whitespace()
                        .skip(1)
                        .collect::<Vec<_>>()
                        .join(" ");
                    seed.unacknowledged = Some((at, name.to_owned(), why));
                }
            }
            "posted" => {
                seed.posted = Some(match rest.strip_prefix("failed") {
                    Some(why) => Err(why.trim().to_owned()),
                    None => Ok(at),
                });
            }
            "ask-ended" => seed.ended.push(rest.to_owned()),
            _ => {}
        }
    }
    seed
}

/// The phrase to type: the one the member shows, for a Mac it heard ask, three listed words.
/// `Ok(None)` while it shows none yet.
///
/// # Errors
/// Why there will be none to type: the member refused to serve or its Allow failed, it shows
/// more than one phrase (the lane presses Allow once), the phrase is for a Mac it never heard
/// ask, or its words are no join phrase.
pub fn presented_phrase(seed: &Desk) -> Result<Option<&Presented>, String> {
    if let Some(why) = &seed.refused {
        return Err(format!("the member's desk refused to serve: {why}"));
    }
    if let Some(why) = &seed.allow_failed {
        return Err(format!("the member's Allow failed: {why}"));
    }
    let shown = match seed.presented.as_slice() {
        [] => return Ok(None),
        [shown] => shown,
        more => {
            return Err(format!(
                "the member shows {} phrases, where the lane pressed Allow once",
                more.len()
            ));
        }
    };
    if !seed.heard.iter().any(|h| h.name == shown.name) {
        return Err(format!(
            "the member shows a phrase for {}, a Mac it never heard ask",
            shown.name
        ));
    }
    aterm_secrets::phrase::Phrase::parse(&shown.words)
        .map_err(|e| format!("the member's phrase for {}: {e}", shown.name))?;
    Ok(Some(shown))
}

/// Whether a file is the seed's value in the joiner's `~/.secrets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    Same,
    Differs,
    Absent,
}

/// One [`CREDENTIALS`] file in the joiner's `~/.secrets`: when it was written (epoch ms) and
/// whether it is the seed's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrived {
    pub file: String,
    pub at: Option<u64>,
    pub held: Held,
}

/// [`Arrived`] over [`arrivals`]' lines.
#[must_use]
pub fn read_arrivals(text: &str) -> Vec<Arrived> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let file = words.next()?.to_owned();
            let at = words.next()?.parse::<f64>().ok().map(secs_to_ms);
            let held = match words.next()? {
                "same" => Held::Same,
                "differs" => Held::Differs,
                _ => Held::Absent,
            };
            Some(Arrived {
                file,
                at: at.filter(|_| held != Held::Absent),
                held,
            })
        })
        .collect()
}

/// Where the joiner stands, as `aterm members status` says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// `member as …`: In.
    In,
    /// `allowed: …`: pending, `~/.secrets` written from the member's bundle.
    Allowed,
    /// `asking: …`: pending, nothing written.
    Asking,
    /// `not in ALab: …`.
    NotIn,
    /// Any other line.
    Unread(String),
}

impl Standing {
    /// Its first line's standing.
    #[must_use]
    pub fn of(status: &str) -> Standing {
        let line = status
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if line.starts_with("member as ") {
            Standing::In
        } else if line.starts_with("allowed:") {
            Standing::Allowed
        } else if line.starts_with("asking:") {
            Standing::Asking
        } else if line.starts_with("not in ALab") {
            Standing::NotIn
        } else {
            Standing::Unread(squeeze(line, 100))
        }
    }
}

impl std::fmt::Display for Standing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Standing::In => f.write_str("In"),
            Standing::Allowed => f.write_str("allowed (pending, ~/.secrets written)"),
            Standing::Asking => f.write_str("asking (pending)"),
            Standing::NotIn => f.write_str("not in ALab"),
            Standing::Unread(line) => write!(f, "unread ({line})"),
        }
    }
}

/// [`status_now`]'s lines: where the joiner stands, and the guest's clock (epoch ms) when it
/// said so (`None` when no clock line followed).
#[must_use]
pub fn read_status(text: &str) -> (Standing, Option<u64>) {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    match lines.split_last() {
        Some((last, said)) if last.parse::<u64>().is_ok() => {
            (Standing::of(&said.join("\n")), last.parse().ok())
        }
        _ => (Standing::of(text), None),
    }
}

/// The addresses [`LISTENERS`] names (the word after `TCP`), each once.
#[must_use]
pub fn listen_addresses(lsof: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in lsof.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Some(addr) = words
            .iter()
            .position(|w| *w == "TCP")
            .and_then(|at| words.get(at + 1))
            && !found.iter().any(|f| f.as_str() == *addr)
        {
            found.push((*addr).to_owned());
        }
    }
    found
}

/// Whether a listener's address (`127.0.0.1:7485`, `[::1]:7486`, `*:7485`) is loopback.
fn on_loopback(addr: &str) -> bool {
    addr.rsplit_once(':')
        .map(|(host, _)| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback())
}

/// A listener address's port.
fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
}

/// One advert of the run's lab members service the guest heard: its instance, and where it
/// resolved when that was not to the guest's own addresses alone (`None`: the guest's own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabAdvert {
    pub instance: String,
    pub elsewhere: Option<String>,
}

/// The adverts of the lab members service `service` the guest heard ([`guest_browse`]), each
/// placed against the guest's own addresses `guest` ([`advert::place`], resolved in the guest on
/// the interface it came over): the member's resolves to the guest, anything else does not.
/// `Err` says why its browse shows none, where it did not run.
fn guest_lab(
    g: &Guest,
    service: &str,
    guest: &[IpAddr],
) -> Result<Result<Vec<LabAdvert>, String>, Fail> {
    let heard = match browse_adds(&guest_browse(g, service)?) {
        Ok(heard) => heard,
        Err(why) => return Ok(Err(why)),
    };
    let out = format!("{GUEST_NUX}/resolved{service}.txt");
    let placed = advert::place(&heard, service, guest, &mut guest_dns_sd(g, &out))?;
    Ok(Ok(placed
        .into_iter()
        .map(|(heard, whose)| LabAdvert {
            instance: heard.instance,
            elsewhere: match whose {
                Whose::ThisMac => None,
                Whose::Another(at) => Some(at),
            },
        })
        .collect()))
}

/// The lab adverts for a cell: `alab-1a2b3c4d (the guest's own)`, each, or "none".
fn lab_text(adverts: &[LabAdvert]) -> String {
    if adverts.is_empty() {
        return "none".to_owned();
    }
    adverts
        .iter()
        .map(|a| match &a.elsewhere {
            None => format!("{} (the guest's own)", a.instance),
            Some(at) => format!("{} ({at})", a.instance),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The lab adverts that are not the guest's own (none when the browse did not run).
fn not_the_guests(adverts: &Result<Vec<LabAdvert>, String>) -> Vec<LabAdvert> {
    adverts
        .as_ref()
        .map(|adverts| {
            adverts
                .iter()
                .filter(|a| a.elsewhere.is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a `dns-sd` registration line ([`registrations`]) names `service` among its words.
fn registers(line: &str, service: &str) -> bool {
    line.split_whitespace().any(|word| word == service)
}

/// What a `vault-phrase` run saw, for [`verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saw {
    /// The Mac whose desk showed the phrase (its record's `member … name=`).
    pub allowed_by: Option<String>,
    /// Each [`CREDENTIALS`] file in the joiner's `~/.secrets`.
    pub files: Vec<Arrived>,
    /// Where the joiner stood last.
    pub standing: Standing,
    /// The run's lab members service (the joiner's `ATERM_LAB_MEMBER_SERVICE`).
    pub service: String,
    /// The guest's own addresses ([`inet_addresses`]).
    pub guest: Vec<IpAddr>,
    /// Where the member listened for the joiner: [`MEMBER_PORT`] at the guest's [`lan_address`].
    pub member_at: String,
    /// The instance the member's record says it registered under [`Self::service`].
    pub advertised: Option<String>,
    /// Each advert of [`Self::service`] the guest heard right before the joiner first browsed
    /// ([`guest_lab`]; why its browse shows none, where it did not run).
    pub lab_adverts: Result<Vec<LabAdvert>, String>,
    /// The address the member heard the Mac it showed the phrase for ask from.
    pub joiner_from: Option<String>,
    /// Every address the member heard an ask from ([`Desk::sources`]).
    pub asks_from: Vec<String>,
    /// Every aterm listener's address in the guest ([`listen_addresses`]).
    pub listening: Vec<String>,
    /// The Bonjour registrations guest processes held ([`registrations`]).
    pub registrations: Vec<String>,
    /// The adverts heard in the guest, by service, each placed ([`guest_heard`]: why its browse
    /// shows none, where it did not run).
    pub guest_adverts: Vec<(String, Result<Split, String>)>,
    /// The adverts this Mac heard over tart's bridge, by service, each placed ([`host_guests`];
    /// why its browse shows none, where it did not run).
    pub host_adverts: Vec<(String, Result<Split, String>)>,
}

/// Why the run fails the acceptance test, each reason once: none for a pass. It passes only when
/// the phrase came from [`MEMBER_NAME`] (a member that is no admin, never the seed), every
/// [`CREDENTIALS`] file is in the joiner's `~/.secrets` with the seed's value, the joiner reads
/// In, the joiner's own browse found the member and nothing else ([`browse_verdict`]), and
/// nothing reached beyond the guest ([`isolation_verdict`]).
#[must_use]
pub fn verdict(saw: &Saw) -> Vec<String> {
    let mut why = Vec::new();
    match saw.allowed_by.as_deref() {
        Some(MEMBER_NAME) => {}
        Some(other) => why.push(format!(
            "the phrase came from {other}, not {MEMBER_NAME}, the member that is no admin"
        )),
        None => why.push("no desk record names the Mac that showed the phrase".to_owned()),
    }
    for c in &CREDENTIALS {
        match saw.files.iter().find(|a| a.file == c.file).map(|a| a.held) {
            Some(Held::Same) => {}
            Some(Held::Differs) => why.push(format!(
                "~/.secrets/{} ({}) is not the seed's value",
                c.file, c.what
            )),
            Some(Held::Absent) | None => why.push(format!(
                "the joiner got no ~/.secrets/{} ({})",
                c.file, c.what
            )),
        }
    }
    if saw.standing != Standing::In {
        why.push(format!("the joiner stayed {}, never In", saw.standing));
    }
    why.extend(browse_verdict(saw));
    why.extend(isolation_verdict(saw));
    why
}

/// Why the row cannot show that the joiner's own `browse_members` found the member and nothing
/// else, each reason once: none when it can. The joiner is named no member, and the member
/// listens at the guest's address on tart's network alone, so an ask it heard from a guest
/// address beyond loopback came by the address the joiner's browse resolved. The member must have
/// registered under the run's lab service, and the guest's own browse of that service (the same
/// mDNSResponder the joiner's browse asks, just before it first browsed) must have heard the
/// member's instance resolving to the guest; another advert it heard fails
/// [`isolation_verdict`].
#[must_use]
pub fn browse_verdict(saw: &Saw) -> Vec<String> {
    let mut why = Vec::new();
    let service = &saw.service;
    match (&saw.advertised, &saw.lab_adverts) {
        (None, _) => why.push(format!(
            "the member's record names no {service} registration, so no browse could find it"
        )),
        (Some(_), Err(unread)) => why.push(format!(
            "the guest's browse of {service} did not run ({unread}), so what the joiner's browse \
             could find is unproven"
        )),
        (Some(instance), Ok(adverts)) => {
            if !adverts
                .iter()
                .any(|a| &a.instance == instance && a.elsewhere.is_none())
            {
                why.push(format!(
                    "the guest's browse of {service} never heard the member's {instance} \
                     resolving to the guest (it heard {})",
                    lab_text(adverts)
                ));
            }
        }
    }
    match saw.joiner_from.as_deref() {
        None => why.push("the member heard the joiner ask from no address".to_owned()),
        Some(from) => match from.parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() => why.push(format!(
                "the joiner asked over loopback ({from}), not at the address its browse resolved"
            )),
            Ok(_) => {}
            Err(_) => why.push(format!(
                "the member heard the joiner ask from {from}, no address"
            )),
        },
    }
    why
}

/// Why the run reached beyond its guest, each reason once: none when it did not. Every ask the
/// member heard came from one of the guest's own addresses; both of the seed's ports were read
/// listening and every aterm listener is on loopback but the member's, at the guest's own
/// address; the guest's one registration is the member's of the run's lab service; the guest's
/// browse of that service heard no advert that is not the guest's own; and every [`SERVICES`]
/// browse ran and heard no advert but this Mac's own, in the guest (something in it or beyond it
/// advertising the live fleet's service) or from a guest on this Mac.
#[must_use]
pub fn isolation_verdict(saw: &Saw) -> Vec<String> {
    let mut why = Vec::new();
    let strangers: Vec<&str> = saw
        .asks_from
        .iter()
        .filter(|from| {
            !from
                .parse::<IpAddr>()
                .is_ok_and(|ip| saw.guest.contains(&ip))
        })
        .map(String::as_str)
        .collect();
    if !strangers.is_empty() {
        why.push(format!(
            "the member heard an ask from {}, not one of the guest's own addresses",
            strangers.join(", ")
        ));
    }
    for port in [LOCAL_VAULT_PORT, MEMBER_PORT] {
        if !saw.listening.iter().any(|a| port_of(a) == Some(port)) {
            why.push(format!(
                "no listener on port {port} read back, so where the seed listened is unproven"
            ));
        }
    }
    let beyond: Vec<&str> = saw
        .listening
        .iter()
        .filter(|a| !on_loopback(a) && **a != saw.member_at)
        .map(String::as_str)
        .collect();
    if !beyond.is_empty() {
        why.push(format!(
            "an aterm process listened beyond loopback: {}",
            beyond.join(", ")
        ));
    }
    let others: Vec<&str> = saw
        .registrations
        .iter()
        .filter(|r| !registers(r, &saw.service))
        .map(String::as_str)
        .collect();
    if !others.is_empty() {
        why.push(format!(
            "a guest process registered a Bonjour service: {}",
            others.join("; ")
        ));
    }
    let elsewhere = not_the_guests(&saw.lab_adverts);
    if !elsewhere.is_empty() {
        why.push(format!(
            "the guest heard {} {} advert(s) not its own ({}): the joiner's browse could have \
             asked them",
            elsewhere.len(),
            saw.service,
            lab_text(&elsewhere)
        ));
    }
    for service in SERVICES {
        match saw.guest_adverts.iter().find(|(s, _)| s == service) {
            Some((_, Ok(heard))) if heard.others.is_empty() => {}
            Some((_, Ok(heard))) => why.push(format!(
                "the guest heard {} {service} advert(s) not this Mac's ({}): something in the \
                 guest or beyond it advertises the live fleet's service",
                heard.others.len(),
                heard.others.join(", ")
            )),
            Some((_, Err(unread))) => why.push(format!(
                "the guest's browse of {service} did not run ({unread}), so no advert is unproven"
            )),
            None => why.push(format!(
                "the guest's browse of {service} left no record, so no advert is unproven"
            )),
        }
        match saw.host_adverts.iter().find(|(s, _)| s == service) {
            Some((_, Ok(heard))) if heard.others.is_empty() => {}
            Some((_, Ok(heard))) => why.push(format!(
                "this Mac heard a guest advertise {service}: {}",
                heard.others.join(", ")
            )),
            Some((_, Err(unread))) => why.push(format!(
                "this Mac's browse of {service} did not run ({unread}), so no advert from a \
                 guest is unproven"
            )),
            None => why.push(format!(
                "this Mac's browse of {service} left no record, so no advert from a guest is \
                 unproven"
            )),
        }
    }
    why
}

/// A verdict's reasons as a cell: `pass: <what passed>`, or each reason.
fn verdict_cell(failed: &[String], passed: impl FnOnce() -> String) -> Value {
    if failed.is_empty() {
        Value::Text(format!("pass: {}", passed()))
    } else {
        Value::Text(format!("FAILED: {}", failed.join("; ")))
    }
}

/// `text`, then this Mac's own adverts in `heard` in brackets, where it heard any.
fn with_own(text: String, heard: &Split) -> String {
    let own = heard.this_macs_text();
    if own.is_empty() {
        text
    } else {
        format!("{text} ({own})")
    }
}

/// `epoch` less `from`, both on the guest's clock.
fn since(epoch: Option<u64>, from: Option<u64>) -> Option<u64> {
    epoch.zip(from).map(|(at, from)| at.saturating_sub(from))
}

/// The member's desk record, read again until it shows the phrase (the [`HANG`] detector): the
/// record, or why the member will show none (it refused, its Allow failed, its phrase is no
/// phrase for the Mac that asked: [`presented_phrase`]; or it showed none within the detector).
///
/// # Errors
/// The guest could not be read, or the run was stopped.
fn await_phrase(g: &Guest, record: &str) -> Result<Result<Desk, String>, Fail> {
    let mut last = Desk::default();
    let shown = within_hang(|| {
        last = read_desk(&g.read(record)?);
        Ok(match presented_phrase(&last) {
            Ok(Some(_)) => Some(Ok(last.clone())),
            Ok(None) => None,
            Err(why) => Some(Err(why)),
        })
    })?;
    Ok(shown.unwrap_or_else(|| {
        Err(format!(
            "the member showed no phrase within {} s (it heard {})",
            HANG.as_secs(),
            if last.heard.is_empty() {
                "no Mac ask".to_owned()
            } else {
                last.heard
                    .iter()
                    .map(|h| h.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ))
    }))
}

/// How many of mDNSResponder's messages of the ask's `dns-sd` a row names.
const MDNS_SHOWN: usize = 8;

/// Where the ask's pid is written ([`with_pid`]).
const ASK_PID: &str = "phrase-ask.pid";

/// `cmd` (a command line) under a shell that writes its own pid to [`ASK_PID`] and then becomes
/// it (`exec`, and `/usr/bin/env` execs in turn), so the pid is the command's own.
fn with_pid(cmd: &str) -> String {
    format!(
        "/bin/sh -c {}",
        crate::util::shell_quote(&format!("echo $$ > {GUEST_NUX}/{ASK_PID}; exec {cmd}"))
    )
}

/// Whether macOS's Local Network question is what kept the ask from the member, read with nobody
/// at the console and nothing pressed: a plain TCP connect to the member's desk from a process
/// macOS does not hold (`nc`, a platform binary the guest agent starts), and every Local Network
/// hold macOS logged since the ask (UserEventAgent's `local network blocked notification`), the
/// ask's own pid named. A connect that reaches the desk while macOS held the ask's pid puts the
/// cause on the unanswered question ([`lan_cause`]).
fn lan_cause_cells(
    g: &Guest,
    row: &mut Row,
    lab: &Lab,
    asked: Option<u64>,
    said: &str,
) -> Result<(), Fail> {
    let (host, port) = lab.listen.rsplit_once(':').unwrap_or((&lab.listen, ""));
    let nc = g.run(
        &format!("/usr/bin/nc -z -G 5 {host} {port} >/dev/null 2>&1; echo $?"),
        STEP,
    )?;
    let reached = nc.trim() == "0";
    row.cell(
        "a plain connect to the member's desk (nc, which macOS does not hold)",
        Value::Text(if reached {
            format!("connected to {}", lab.listen)
        } else {
            format!("did not connect to {} (nc exit {})", lab.listen, nc.trim())
        }),
    );
    let pid: Option<u32> = g
        .run(
            &format!("cat {GUEST_NUX}/{ASK_PID} 2>/dev/null || true"),
            STEP,
        )?
        .trim()
        .parse()
        .ok();
    let now = g.clock_ms()?;
    let since_s = asked.map_or(600, |at| now.saturating_sub(at) / 1000 + 5);
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
        .filter(|(at, _)| at.zip(asked).is_none_or(|(at, asked)| at >= asked))
        .collect();
    row.cell(
        "macOS's Local Network holds since the ask",
        Value::Text(if blocks.is_empty() {
            "none".to_owned()
        } else {
            blocks
                .iter()
                .map(|(at, p)| {
                    let who = if Some(*p) == pid {
                        "the ask"
                    } else {
                        "another aterm process"
                    };
                    match at.zip(asked) {
                        Some((at, asked)) => {
                            format!("{who} (pid {p}) +{} ms", at.saturating_sub(asked))
                        }
                        None => format!("{who} (pid {p})"),
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        }),
    );
    // What mDNSResponder said of the `dns-sd` the ask's browse ran: a client macOS keeps off the
    // local network is refused there (PolicyDenied), whatever the ask printed.
    let mdns = g.sh(
        &format!(
            "/usr/bin/log show --last {since_s}s --info --style ndjson --predicate \
             'process == \"mDNSResponder\" AND eventMessage CONTAINS[c] \"dns-sd\"' \
             2>/dev/null || true"
        ),
        STEP,
    )?;
    let said_of = distinct_messages(&mdns, MDNS_SHOWN);
    row.cell(
        "mDNSResponder of the ask's dns-sd",
        Value::Text(if said_of.is_empty() {
            "nothing".to_owned()
        } else {
            said_of.join(" | ")
        }),
    );
    row.cell(
        "why the member heard no ask",
        Value::Text(lan_cause(said, reached, pid, &blocks).to_owned()),
    );
    Ok(())
}

/// Each line `out` printed, trimmed, one ` | ` apart: a row reads every line, not only the last
/// (the ask's first line says whether its browse heard a member). Pure.
#[must_use]
pub fn said_lines(out: &str) -> String {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// What `members join --no-wait` prints when its browse heard no member
/// (`aterm_agent::members_cli`'s `said_no_wait`).
const NO_MEMBER_ANSWERS: &str = "no ALab member answers on this network";

/// What a missing ask's readings say of its cause: what the ask `said`, `reached` (a plain
/// connect got to the desk), the ask's `pid`, and macOS's Local Network holds. The ask's own
/// words come first: a browse macOS refused names Local Network privacy
/// ([`aterm_http::LOCAL_NETWORK_REFUSED`]), and one that heard nothing while the guest's own
/// browse heard the member and a plain connect reached it puts the miss in the ask's browse. Pure.
#[must_use]
pub fn lan_cause(
    said: &str,
    reached: bool,
    pid: Option<u32>,
    blocks: &[(Option<u64>, u32)],
) -> &'static str {
    let held = pid.is_some_and(|pid| blocks.iter().any(|(_, p)| *p == pid));
    if said.contains(aterm_http::LOCAL_NETWORK_REFUSED) {
        return "macOS's Local Network privacy: it refused the ask's browse while its question \
                stood unanswered";
    }
    if said.contains(NO_MEMBER_ANSWERS) && reached && !held {
        return "the ask's browse heard no member though the guest's own browse heard it and a \
                plain connect reached it: macOS answered aterm's browse with nothing, and logged \
                no refusal or hold";
    }
    match (reached, held) {
        (true, true) => {
            "macOS's unanswered Local Network question: it held the ask while a plain connect \
             reached the desk"
        }
        (true, false) if pid.is_none() => "not established: the ask's pid did not read",
        (true, false) => {
            "not the Local Network question: macOS logged no hold of the ask, and the desk was \
             reachable"
        }
        (false, _) => "not established: the desk was not reachable from the guest either",
    }
}

/// The joiner's ask, as install.sh's windowless lanes run it, `me` its command line
/// ([`as_joiner`]): named no member, so it browses for one.
fn ask_cmd(me: &str) -> String {
    format!("{me} members join --no-wait")
}

/// The joiner's join, the phrase typed at its prompt, `me` its command line ([`as_joiner`]):
/// named no member, so it browses for one.
fn join_cmd(me: &str) -> String {
    format!("{me} members join")
}

/// The guest's browse of the lab members service `service` read again until it hears the
/// member's `instance` resolving to the guest (the [`HANG`] detector), so the joiner's browse,
/// which listens 1.5 s, finds a registration that is up: what it heard last, or why it heard
/// nothing (its browse did not run, or the detector fired first).
///
/// # Errors
/// The guest could not be read, or the run was stopped.
fn await_lab(
    g: &Guest,
    service: &str,
    instance: &str,
    guest: &[IpAddr],
) -> Result<Result<Vec<LabAdvert>, String>, Fail> {
    let mut last = Err("no browse ran".to_owned());
    let heard = within_hang(|| {
        last = guest_lab(g, service, guest)?;
        Ok(last
            .as_ref()
            .is_ok_and(|adverts| {
                adverts
                    .iter()
                    .any(|a| a.instance == instance && a.elsewhere.is_none())
            })
            .then_some(()))
    })?;
    Ok(match (heard, last) {
        (Some(()), last) => last,
        (None, Ok(adverts)) => Ok(adverts),
        (None, Err(why)) => Err(format!("{why} ({} s on)", HANG.as_secs())),
    })
}

/// `vault-phrase` (the module docs).
pub(super) fn vault_phrase(g: &Guest, run: &Path, row: &mut Row) -> Result<(), Fail> {
    let h = Homes {
        seed: format!("{GUEST_NUX}/phrase-seed"),
        member: format!("{GUEST_NUX}/phrase-member"),
        joiner: format!("{GUEST_NUX}/phrase-joiner"),
    };
    let (seed_home, member_home, joiner) = (&h.seed, &h.member, &h.joiner);
    let record = format!("{GUEST_NUX}/{DESK_RECORD}");
    let helper = format!("{GUEST_NUX}/{GUEST_HELPER}");

    // The run's lab members service, and an app that takes it: one that does not would browse
    // the live fleet's `_alab._tcp`, so the row stops before anything of the joiner's runs.
    let service = fresh_lab_service()?;
    let named = g.run(&names_knob(&g.aterm), STEP)?;
    row.cell(
        "lab members service",
        Value::Text(format!(
            "{service} (the joiner's {LAB_MEMBER_SERVICE_ENV}; the app {})",
            if named.trim() == "named" {
                "names it"
            } else {
                "does not name it"
            }
        )),
    );
    if named.trim() != "named" {
        return Err(crate::mac::not_run(format!(
            "the app's executable names no {LAB_MEMBER_SERVICE_ENV}, so its browse would look for \
             the live fleet's {MEMBER_SERVICE} members: the row ran none of the joiner's commands"
        )));
    }
    let guest = inet_addresses(&g.run(GUEST_ADDRESSES, STEP)?);
    let Some(lan) = lan_address(&guest) else {
        return Err(Fail::die(format!(
            "the guest has no address on tart's network for the member to listen at (its \
             addresses: {})",
            guest
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };
    let lab = Lab {
        listen: format!("{lan}:{MEMBER_PORT}"),
        service,
    };
    let service = lab.service.as_str();
    let served = setup(g, &h, &record, &lab, row)?;
    let me = as_joiner(&g.aterm, joiner, service);

    // The member's registration, heard in the guest before the joiner browses, and nothing else
    // of the lab's type: anything else is not the guest's, and the joiner's browse would ask it.
    let Some((instance, _)) = served.advertised.clone() else {
        return Err(Fail::die(format!(
            "the member's desk serves but its record names no registration: {}",
            tail_of(g, &format!("{member_home}/member.advert"))?
        )));
    };
    row.cell(
        "member advertised",
        Value::Text(format!("{instance} under {service} at {}", lab.listen)),
    );
    let lab_adverts = await_lab(g, service, &instance, &guest)?;
    row.cell(
        "lab adverts heard in the guest",
        Value::Text(match &lab_adverts {
            Ok(adverts) => lab_text(adverts),
            Err(why) => format!("unread ({why})"),
        }),
    );
    let elsewhere = not_the_guests(&lab_adverts);
    if !elsewhere.is_empty() {
        row.fail(format!(
            "the guest heard {service} adverts not its own ({}): the row ran none of the \
             joiner's commands",
            lab_text(&elsewhere)
        ));
        return Ok(());
    }

    // The ask, as install.sh's windowless lanes run it, its pid kept for [`lan_cause_cells`].
    let (ask, ask_out) = g.notty_within("phrase-ask", &with_pid(&ask_cmd(&me)), HANG)?;
    drive_cells_within(row, "ask: ", &ask, HANG);
    row.cell(
        "ask: said",
        or_missing(
            Some(said_lines(&ask_out)).filter(|said| !said.is_empty()),
            |said| Value::Text(squeeze(&said, 400)),
            "the ask printed nothing",
        ),
    );
    let asked = ask.start_epoch_ms;

    // The member hears it and presses Allow: the phrase it shows, read off its desk record.
    let desk = match await_phrase(g, &record)? {
        Ok(desk) => desk,
        Err(why) => {
            lan_cause_cells(g, row, &lab, asked, &ask_out)?;
            row.fail(why);
            return Ok(());
        }
    };
    let Ok(Some(shown)) = presented_phrase(&desk) else {
        return Err(Fail::die("the member's phrase did not read back twice"));
    };
    let allowed_by = desk.serving.as_ref().map(|(_, _, name)| name.clone());
    if let Some((_, listen, name)) = &desk.serving {
        row.cell(
            "allowed by",
            Value::Text(format!(
                "{name} on {listen} (a member, not the admin {SEED_NAME})"
            )),
        );
    }
    let heard = desk.heard.iter().find(|h| h.name == shown.name);
    row.cell(
        "joiner",
        Value::Text(match heard {
            Some(h) => format!("{} (code {})", h.name, h.code),
            None => shown.name.clone(),
        }),
    );
    row.cell(
        "ask to the member's row",
        or_missing(since(heard.map(|h| h.at), asked), Value::Ms, "no ask start"),
    );
    row.cell(
        "ask to the phrase shown",
        or_missing(since(Some(shown.at), asked), Value::Ms, "no ask start"),
    );

    // The phrase typed at the joiner's prompt, as a person types what the member shows. Its
    // `wait_for_allow` has no deadline of its own, so the driver's limit is the hang detector.
    let (join, join_out) = g.drive_within(
        "phrase-join",
        &format!("NUX_PHRASE={}", crate::util::shell_quote(&shown.words)),
        &join_cmd(&me),
        HANG,
    )?;
    drive_cells_within(row, "join: ", &join, HANG);
    let typed = join
        .prompts
        .iter()
        .find(|(_, _, text)| text.contains("The phrase"))
        .zip(join.start_epoch_ms)
        .map(|(p, start)| start.saturating_add(p.0));
    row.cell(
        "ask to the phrase typed",
        or_missing(
            since(typed, asked),
            Value::Ms,
            "the joiner asked for no phrase",
        ),
    );
    // Its `Joined ALab` line (`Allowed into ALab` while the vault lists it as pending), else its
    // last refusal: never another line, as the terminal echoed the phrase on the prompt's.
    row.cell(
        "join: said",
        or_missing(
            join_out
                .lines()
                .find(|l| l.contains("Joined ALab") || l.contains("Allowed into ALab"))
                .or_else(|| {
                    join_out
                        .lines()
                        .rev()
                        .find(|l| l.trim_start().starts_with("aterm members join:"))
                })
                .map(|l| squeeze(l.trim(), 120)),
            Value::Text,
            "no `Joined ALab` line and no refusal",
        ),
    );

    // Each file on disk, and where the joiner stands once the phrase ran.
    let files = read_arrivals(&g.run(&arrivals(&helper, seed_home, joiner), STEP)?);
    for c in &CREDENTIALS {
        let landed = files.iter().find(|a| a.file == c.file).and_then(|a| a.at);
        let why = format!("no ~/.secrets/{}", c.file);
        row.cell(
            format!("ask to {} ({})", c.file, c.what),
            or_missing(since(landed, asked), Value::Ms, &why),
        );
        row.cell(
            format!("typed to {}", c.file),
            or_missing(since(landed, typed), Value::Ms, &why),
        );
    }
    let same = files.iter().filter(|a| a.held == Held::Same).count();
    row.cell(
        "the seed's values",
        Value::Text(format!("{same} of {}", CREDENTIALS.len())),
    );
    let after = Standing::of(&g.run(&format!("{me} members status 2>&1"), STEP)?);
    row.cell(
        "membership after the phrase",
        Value::Text(after.to_string()),
    );

    // The joiner's own subscriber, as its agent runs it, until it reads In.
    g.sh(&watch(&me, joiner), STEP)?;
    let mut last = after;
    let read_in = within_hang(|| {
        let (standing, at) = read_status(&g.run(&status_now(&me), STEP)?);
        let done = (standing == Standing::In).then_some(at);
        last = standing;
        Ok(done)
    })?;
    let standing = match read_in {
        Some(at) => {
            row.cell(
                "ask to In",
                or_missing(since(at, asked), Value::Ms, "no ask start or clock"),
            );
            Standing::In
        }
        None => {
            row.cell(
                "ask to In",
                Value::Missing(format!("{last} after {} s", HANG.as_secs())),
            );
            last
        }
    };
    let desk = read_desk(&g.read(&record)?);
    row.cell(
        "member: let in",
        match (&desk.let_in, &desk.unacknowledged) {
            (None, Some((_, _, why))) => Value::Missing(format!(
                "a run sent its bundle and the new Mac never acknowledged it: {}",
                squeeze(why, 100)
            )),
            (let_in, _) => or_missing(
                since(let_in.as_ref().map(|l| l.0), asked),
                Value::Ms,
                "no run sent its bundle",
            ),
        },
    );
    row.cell(
        "member: posted the Allow",
        match &desk.posted {
            Some(Ok(at)) => or_missing(since(Some(*at), asked), Value::Ms, "no ask start"),
            Some(Err(why)) => Value::Text(format!("failed: {}", squeeze(why, 100))),
            None => Value::Missing("posted nothing".into()),
        },
    );
    if let Some(ended) = desk.ended.first() {
        row.cell("member: an ask ended", Value::Text(squeeze(ended, 100)));
    }
    for (label, log) in [
        ("seed's approver", format!("{seed_home}/watch.log")),
        ("member's desk", format!("{member_home}/member.err")),
        ("joiner's subscriber", format!("{joiner}/watch.log")),
    ] {
        if let Some(last) = g.read(&log)?.lines().rev().find(|l| !l.trim().is_empty()) {
            row.cell(
                format!("{label}'s last line"),
                Value::Text(squeeze(last.trim(), 100)),
            );
        }
    }

    // Isolation, read while every server and the joiner's subscriber still run.
    let asks_from = desk.sources();
    row.cell(
        "member heard asks from",
        Value::Text(if asks_from.is_empty() {
            "none".into()
        } else {
            asks_from.join(", ")
        }),
    );
    let joiner_from = heard.and_then(|h| h.from.clone());
    let listening = listen_addresses(&g.run(LISTENERS, STEP)?);
    row.cell(
        "aterm listeners",
        Value::Text(if listening.is_empty() {
            "none read".into()
        } else {
            listening.join(", ")
        }),
    );
    let registrations = registrations(&g.run(DNS_SD_PROCESSES, STEP)?);
    row.cell(
        "Bonjour registrations in the guest",
        Value::Text(if registrations.is_empty() {
            "none".into()
        } else {
            registrations.join("; ")
        }),
    );
    // Each browse by the guest helper, which stops its own `dns-sd` alone: a subscriber's
    // standing browse runs beside it.
    let mut in_guest = Vec::new();
    let mut host = Vec::new();
    for real in SERVICES {
        in_guest.push((real.to_owned(), guest_heard(g, real)?));
        host.push((real.to_owned(), host_guests(run, real)?));
    }
    // What this Mac heard of the member's lab advert over tart's bridge: no aterm here browses
    // that type, so it reaches no process of the live fleet; named, never judged.
    row.cell(
        "lab adverts this Mac heard",
        Value::Text(match host_guests(run, service)? {
            Ok(heard) if heard.others.is_empty() => with_own("none".to_owned(), &heard),
            Ok(heard) => with_own(heard.others.join(", "), &heard),
            Err(why) => format!("unread ({why})"),
        }),
    );
    row.cell(
        "adverts heard in the guest",
        Value::Text(
            in_guest
                .iter()
                .map(|(s, heard)| match heard {
                    Ok(heard) => with_own(format!("{s} {}", heard.others.len()), heard),
                    Err(why) => format!("{s} unread ({why})"),
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
    );
    row.cell(
        "adverts from guests on this Mac",
        Value::Text(
            host.iter()
                .map(|(s, heard)| match heard {
                    Ok(heard) if heard.others.is_empty() => with_own(format!("{s} none"), heard),
                    Ok(heard) => with_own(format!("{s} {}", heard.others.join(", ")), heard),
                    Err(why) => format!("{s} unread ({why})"),
                })
                .collect::<Vec<_>>()
                .join("; "),
        ),
    );

    let heard_own = in_guest
        .iter()
        .chain(&host)
        .any(|(_, heard)| heard.as_ref().is_ok_and(|h| !h.this_macs.is_empty()));
    let saw = Saw {
        allowed_by,
        files,
        standing,
        service: service.to_owned(),
        guest,
        member_at: lab.listen.clone(),
        advertised: Some(instance),
        lab_adverts,
        joiner_from,
        asks_from,
        listening,
        registrations,
        guest_adverts: in_guest,
        host_adverts: host,
    };
    row.cell(
        "browse heard the member",
        verdict_cell(&browse_verdict(&saw), || {
            format!(
                "the joiner, named no member, asked {} at {} from {}, found by browse_members of \
                 {service}; the guest's browse heard {}",
                saw.allowed_by.as_deref().unwrap_or(MEMBER_NAME),
                saw.member_at,
                saw.joiner_from.as_deref().unwrap_or("?"),
                saw.lab_adverts
                    .as_ref()
                    .map_or_else(Clone::clone, |a| lab_text(a))
            )
        }),
    );
    row.cell(
        "enrolled by the runtime",
        Value::Missing(
            "open: the member enrols through the guest helper's in-process exchange over \
             loopback; a Commons runtime on the guest's network would beacon on the live \
             fleet's group and port (module docs)"
                .into(),
        ),
    );
    row.cell(
        "isolation",
        verdict_cell(&isolation_verdict(&saw), || {
            format!(
                "every ask from the guest ({}), every listener on loopback but the member's {}, \
                 the one registration the member's {service}, no {} advert{}",
                saw.asks_from.join(", "),
                saw.member_at,
                SERVICES.join(" or "),
                if heard_own { " but this Mac's own" } else { "" },
            )
        }),
    );
    let failed = verdict(&saw);
    if failed.is_empty() {
        row.cell(
            "acceptance",
            Value::Text(format!(
                "pass: found by its browse of {service}, allowed by {MEMBER_NAME}, {n} of {n} \
                 files the seed's, In, nothing beyond the guest",
                n = CREDENTIALS.len()
            )),
        );
    } else {
        row.fail(format!("acceptance: {}", failed.join("; ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_missing_ask_is_the_local_network_question_only_when_it_held_the_ask_and_the_desk_answered()
     {
        let held = [(Some(10), 700), (Some(12), 812)];
        let sent = "Joined ALab: a member let this Mac in by its proof.";
        assert!(
            lan_cause(sent, true, Some(812), &held).starts_with("macOS's unanswered Local Network")
        );
        // Negative controls: no hold of the ask's own pid, an unreachable desk, an unread pid.
        assert!(lan_cause(sent, true, Some(900), &held).starts_with("not the Local Network"));
        assert!(lan_cause(sent, false, Some(812), &held).starts_with("not established: the desk"));
        assert!(lan_cause(sent, true, None, &held).starts_with("not established: the ask's pid"));
        // The ask's own words: a refused browse is Local Network privacy whatever else read; a
        // browse that heard no member is named as such only where the desk was reachable.
        let refused = format!(
            "browsing _x._tcp: {}; this Mac's request waits",
            aterm_http::LOCAL_NETWORK_REFUSED
        );
        assert!(lan_cause(&refused, false, None, &[]).starts_with("macOS's Local Network privacy"));
        let none = "no ALab member answers on this network yet: x | this Mac's vault subscriber does not run";
        assert!(
            lan_cause(none, true, Some(900), &held).starts_with("the ask's browse heard no member")
        );
        assert!(lan_cause(none, false, Some(900), &held).starts_with("not established: the desk"));
        assert_eq!(said_lines("a\n\n  b  \n"), "a | b");
        let wrapped = with_pid("/usr/bin/env HOME=/h /a/aterm members join --no-wait");
        assert!(wrapped.starts_with("/bin/sh -c "), "{wrapped}");
        assert!(
            wrapped
                .contains("echo $$ > /var/tmp/aterm-nux/phrase-ask.pid; exec /usr/bin/env HOME=/h"),
            "{wrapped}"
        );
    }

    /// The run's lab members service in these tests.
    const LAB: &str = "_alab-lab-k3x9q2._tcp";

    /// A run that met the acceptance test: every file the seed's, In, the joiner's browse found
    /// the member's lab advert alone and asked it at the guest's own address, the seed's port on
    /// loopback and the member's at that address, the member's lab registration the guest's one,
    /// no advert of a real service anywhere.
    fn passing() -> Saw {
        Saw {
            allowed_by: Some(MEMBER_NAME.into()),
            files: CREDENTIALS
                .iter()
                .map(|c| Arrived {
                    file: c.file.into(),
                    at: Some(1_000),
                    held: Held::Same,
                })
                .collect(),
            standing: Standing::In,
            service: LAB.into(),
            guest: ["127.0.0.1", "::1", "fe80::1", "192.168.64.5"]
                .iter()
                .map(|a| a.parse().unwrap())
                .collect(),
            member_at: "192.168.64.5:7486".into(),
            advertised: Some("alab-1a2b3c4d".into()),
            lab_adverts: Ok(vec![LabAdvert {
                instance: "alab-1a2b3c4d".into(),
                elsewhere: None,
            }]),
            joiner_from: Some("192.168.64.5".into()),
            asks_from: vec!["192.168.64.5".into()],
            listening: vec!["127.0.0.1:7485".into(), "192.168.64.5:7486".into()],
            registrations: vec![format!(
                "900 /usr/bin/dns-sd -R alab-1a2b3c4d {LAB} local 7486 v=1 role=member \
                 fleet=7486 window=1"
            )],
            guest_adverts: SERVICES
                .iter()
                .map(|s| ((*s).to_owned(), Ok(Split::default())))
                .collect(),
            host_adverts: SERVICES
                .iter()
                .map(|s| ((*s).to_owned(), Ok(Split::default())))
                .collect(),
        }
    }

    #[test]
    fn a_run_whose_joiner_got_no_file_fails() {
        assert_eq!(verdict(&passing()), Vec::<String>::new());
        let mut saw = passing();
        saw.files[1].held = Held::Absent;
        saw.files[1].at = None;
        assert_eq!(
            verdict(&saw),
            ["the joiner got no ~/.secrets/gh_access_token_alabsystems (publishing)"]
        );
        // A file the guest did not report at all is as missing as an absent one.
        let mut saw = passing();
        saw.files.retain(|a| a.file != "ai-accounts.csv");
        assert_eq!(
            verdict(&saw),
            ["the joiner got no ~/.secrets/ai-accounts.csv (AI accounts)"]
        );
        let mut saw = passing();
        saw.files[0].held = Held::Differs;
        assert_eq!(
            verdict(&saw),
            ["~/.secrets/gh_access_token (GitHub) is not the seed's value"]
        );
    }

    /// The acceptance test's "from any existing member": a phrase the vault's admin showed, or
    /// one no desk record names, is not the member's Allow.
    #[test]
    fn a_run_whose_phrase_came_from_the_admin_fails() {
        let mut saw = passing();
        saw.allowed_by = Some("nux-seed".into());
        assert_eq!(
            verdict(&saw),
            ["the phrase came from nux-seed, not nux-member, the member that is no admin"]
        );
        let mut saw = passing();
        saw.allowed_by = None;
        assert_eq!(
            verdict(&saw),
            ["no desk record names the Mac that showed the phrase"]
        );
    }

    /// The member asks as itself, pinned to the vault's signer, the seed approves it, and it
    /// enrols with the seed: each a guest command of its own.
    #[test]
    fn the_member_joins_by_the_seeds_approval_and_enrols_with_it() {
        let created = "Created vault nux-local at http://127.0.0.1:7485/v1/nux-local.\n\
                       On another Mac: aterm secrets join \
                       http://admins-virtual-machine.local:7485/v1/nux-local#age1sx2k9 --name <host>\n";
        assert_eq!(signer_of(created), Some("age1sx2k9"));
        assert_eq!(signer_of("aterm secrets join http://h:7485/v1/v\n"), None);
        assert_eq!(signer_of("aterm secrets join http://h:7485/v1/v#\n"), None);
        assert_eq!(signer_of(""), None);
        let join = member_join("/A/aterm", "/n/member", "age1sx2k9");
        assert!(
            join.starts_with(
                "/usr/bin/env HOME=/n/member /A/aterm secrets join \
                 'http://127.0.0.1:7485/v1/"
            ) && join.contains("#age1sx2k9' --name nux-member --no-agent --no-wait"),
            "{join}"
        );
        // The code `secrets join` and `secrets status` print (cli.rs's `waiting_line`,
        // `pending_membership`), approved by itself: `approve --all` needs a terminal.
        let waiting = "waiting for approval of nux-member (code abcd-efgh-ijkm-npqr): on an admin \
                       Mac run aterm secrets approve abcd-efgh-ijkm-npqr\n\
                       membership: waiting for approval (on an admin Mac: aterm secrets approve \
                       abcd-efgh-ijkm-npqr)\n";
        assert_eq!(approve_code(waiting), Some("abcd-efgh-ijkm-npqr"));
        assert_eq!(
            approve_code(
                "membership: waiting for approval (on an admin Mac: aterm secrets approve wxyz-2345)\n"
            ),
            Some("wxyz-2345")
        );
        assert_eq!(approve_code("aterm secrets approve <code>\n"), None);
        assert_eq!(
            approve_code("joined: seq 3, 3 secrets in ~/.secrets\n"),
            None
        );
        assert_eq!(
            approve("/A/aterm", "/n/seed", "abcd-efgh-ijkm-npqr"),
            "/usr/bin/env HOME=/n/seed /A/aterm secrets approve abcd-efgh-ijkm-npqr \
             > /n/seed/approve.out 2>&1 || { cat /n/seed/approve.out; exit 1; }"
        );
        assert!(!approve("/A/aterm", "/n/seed", "c").contains("--all"));
        let sync = sync_now("/A/aterm", "/n/member");
        assert!(
            sync.starts_with("/usr/bin/env HOME=/n/member /A/aterm secrets sync ")
                && sync.contains("HOME=/n/member /A/aterm members status"),
            "{sync}"
        );
        assert_eq!(
            enrol("/n/helper", "/n/member", "/n/seed"),
            "/usr/bin/env HOME=/n/member /n/helper enrol /n/member /n/seed"
        );
        assert_ne!(MEMBER_NAME, SEED_NAME);
    }

    #[test]
    fn a_run_whose_joiner_stayed_pending_fails() {
        for still in [Standing::Allowed, Standing::Asking, Standing::NotIn] {
            let mut saw = passing();
            saw.standing = still.clone();
            assert_eq!(
                verdict(&saw),
                [format!("the joiner stayed {still}, never In")]
            );
        }
    }

    /// An advert that is not this Mac's own, heard in the guest or from a guest on this Mac,
    /// fails the row.
    #[test]
    fn a_run_whose_servers_advertised_fails() {
        let another = || Split {
            others: vec!["nuxlab on bridge100 (nuxlab.local 192.168.64.3)".into()],
            this_macs: Vec::new(),
        };
        for (service, on_guest) in [(0, true), (1, true), (0, false), (1, false)] {
            let mut saw = passing();
            let adverts = if on_guest {
                &mut saw.guest_adverts
            } else {
                &mut saw.host_adverts
            };
            adverts[service].1 = Ok(another());
            assert_eq!(verdict(&saw).len(), 1, "{:?}", verdict(&saw));
        }
    }

    /// This Mac's own registration, heard over tart's bridge in the guest and on this Mac (the
    /// ALab vault the lab Mac serves), is named and never fails the row; one beside it that is
    /// not this Mac's still does.
    #[test]
    fn a_run_that_heard_only_this_macs_own_adverts_passes() {
        let own = || Split {
            others: Vec::new(),
            this_macs: vec!["alab-secrets heard over bridge100".into()],
        };
        let mut saw = passing();
        saw.guest_adverts[0].1 = Ok(own());
        saw.host_adverts[0].1 = Ok(own());
        assert!(verdict(&saw).is_empty(), "{:?}", verdict(&saw));
        // NEGATIVE CONTROL: a guest's advert beside this Mac's own fails, on either side.
        for on_guest in [true, false] {
            let mut saw = passing();
            let mut both = own();
            both.others
                .push("nuxlab on bridge100 (nuxlab.local 192.168.64.3)".into());
            if on_guest {
                saw.guest_adverts[0].1 = Ok(both);
            } else {
                saw.host_adverts[0].1 = Ok(both);
            }
            assert_eq!(verdict(&saw).len(), 1, "{:?}", verdict(&saw));
        }
    }

    /// A browse that never looked (a `dns-sd` that could not reach mDNSResponder, a transcript
    /// `script` never flushed) proves no advert absent, on either side.
    #[test]
    fn a_run_whose_browse_did_not_run_fails() {
        let mut saw = passing();
        saw.guest_adverts[0].1 = Err("dns-sd failed: DNSService call failed -65563".into());
        assert_eq!(
            verdict(&saw),
            [
                "the guest's browse of _alab-vault._tcp did not run (dns-sd failed: DNSService \
                 call failed -65563), so no advert is unproven"
            ]
        );
        let mut saw = passing();
        saw.guest_adverts.pop();
        assert_eq!(
            verdict(&saw),
            ["the guest's browse of _alab._tcp left no record, so no advert is unproven"]
        );
        let mut saw = passing();
        saw.host_adverts[1].1 = Err("its transcript is empty".into());
        assert_eq!(
            verdict(&saw),
            [
                "this Mac's browse of _alab._tcp did not run (its transcript is empty), so no \
                 advert from a guest is unproven"
            ]
        );
        let mut saw = passing();
        saw.host_adverts.clear();
        assert_eq!(verdict(&saw).len(), 2, "{:?}", verdict(&saw));
    }

    /// A registration a guest process holds is the guest advertising, whatever a browse heard.
    #[test]
    fn a_run_whose_guest_registered_a_bonjour_service_fails() {
        // As `pgrep -lf dns-sd` lists them: aterm's scoped registration on a Mac on two
        // networks and its plain one count; a browse, the `script` it runs under and the shell
        // that ran `pgrep` do not.
        let pgrep = "\
            812 /usr/bin/dns-sd -i en0 -P nuxlab _alab-vault._tcp local 7485 nuxlab.local 192.168.64.5 path=/v1\n\
            813 dns-sd -R nuxlab _alab._tcp local 7100\n\
            820 /usr/bin/dns-sd -B _alab-vault._tcp local.\n\
            821 /usr/bin/script -q -t 0 /tmp/heard_alab._tcp.txt /usr/bin/dns-sd -B _alab._tcp local.\n\
            830 sh -c pgrep -lf dns-sd || true\n";
        assert_eq!(
            registrations(pgrep),
            [
                "812 /usr/bin/dns-sd -i en0 -P nuxlab _alab-vault._tcp local 7485 nuxlab.local \
                 192.168.64.5 path=/v1",
                "813 dns-sd -R nuxlab _alab._tcp local 7100",
            ]
        );
        assert!(registrations("").is_empty());
        let mut saw = passing();
        saw.registrations = vec!["812 dns-sd -P nuxlab _alab-vault._tcp local 7485".into()];
        assert_eq!(
            verdict(&saw),
            ["a guest process registered a Bonjour service: \
                 812 dns-sd -P nuxlab _alab-vault._tcp local 7485"]
        );
        // The member's registration of the run's lab service is the one the row expects; one of
        // the live fleet's service, or of another lab's, beside it still fails. NEGATIVE CONTROL
        // (2026-10-29): `registers` taking any `_alab-lab-` registration fails here at another
        // lab's: `left: []`.
        for beside in [
            "813 dns-sd -R nuxlab _alab._tcp local 7100",
            "814 dns-sd -R alab-00000000 _alab-lab-zzzzzz._tcp local 7486",
        ] {
            let mut saw = passing();
            saw.registrations.push(beside.into());
            assert_eq!(
                verdict(&saw),
                [format!(
                    "a guest process registered a Bonjour service: {beside}"
                )]
            );
        }
    }

    #[test]
    fn a_run_whose_servers_listened_beyond_loopback_fails() {
        let mut saw = passing();
        saw.listening = vec!["*:7485".into(), "127.0.0.1:7486".into()];
        assert_eq!(
            verdict(&saw),
            ["an aterm process listened beyond loopback: *:7485"]
        );
        let mut saw = passing();
        saw.listening.push("192.168.64.5:7100".into());
        assert_eq!(
            verdict(&saw),
            ["an aterm process listened beyond loopback: 192.168.64.5:7100"]
        );
        // Only the member's port at the guest's own address listens beyond loopback: the same
        // port on every interface, or at another address, does not pass for it. NEGATIVE CONTROL
        // (2026-10-29): `isolation_verdict` passing any listener on the member's port fails here
        // at `*:7486`: `left: []`.
        for wide in ["*:7486", "192.168.64.9:7486"] {
            let mut saw = passing();
            saw.listening.push(wide.into());
            assert_eq!(
                verdict(&saw),
                [format!("an aterm process listened beyond loopback: {wide}")]
            );
        }
        // A run that read no listener on a seed port has not shown where it listened.
        let mut saw = passing();
        saw.listening = vec!["127.0.0.1:7485".into()];
        assert_eq!(
            verdict(&saw),
            ["no listener on port 7486 read back, so where the seed listened is unproven"]
        );
        let mut saw = passing();
        saw.listening.push("[::1]:7487".into());
        assert_eq!(verdict(&saw), Vec::<String>::new());
    }

    /// The member's desk record as the guest helper writes it (`src/guest/member.rs`'s module
    /// docs).
    const RECORD: &str = "advertised 990 instance=alab-1a2b3c4d service=_alab-lab-k3x9q2._tcp port=7486\n\
                          member 1000 listen=192.168.64.5:7486 name=nux-member\n\
                          heard 1250 id=1 name=admins-virtual-machine code=K7Q2-9F3A from=192.168.64.5\n\
                          asked 1251 from=192.168.64.5\n\
                          phrase 1300 id=1 name=admins-virtual-machine maple river ocean\n\
                          let-in 4100 name=admins-virtual-machine\n\
                          posted 4180 ok\n";

    /// A run whose bundle the new Mac never acknowledged (`src/guest/member.rs`'s
    /// `sent-unacknowledged`) is read as sent, with why, never as a Mac let in.
    ///
    /// NEGATIVE CONTROL: drop `read_desk`'s `sent-unacknowledged` arm: `left: None`, `right:
    /// Some((4100, "admins-virtual-machine", "the other Mac closed the join exchange"))`.
    #[test]
    fn a_run_the_new_mac_never_acknowledged_is_read_as_no_mac_let_in() {
        let seed = read_desk(
            "sent-unacknowledged 4100 name=admins-virtual-machine the other Mac closed the join \
             exchange\nposted 4180 ok\n",
        );
        assert_eq!(seed.let_in, None);
        assert_eq!(
            seed.unacknowledged,
            Some((
                4100,
                "admins-virtual-machine".into(),
                "the other Mac closed the join exchange".into()
            ))
        );
    }

    #[test]
    fn the_phrase_reader_types_only_the_one_phrase_shown_for_the_mac_that_asked() {
        let seed = read_desk(RECORD);
        assert_eq!(
            seed.advertised,
            Some(("alab-1a2b3c4d".into(), "_alab-lab-k3x9q2._tcp".into()))
        );
        assert_eq!(
            seed.serving,
            Some((1000, "192.168.64.5:7486".into(), "nux-member".into()))
        );
        assert_eq!(
            seed.heard,
            [Heard {
                at: 1250,
                name: "admins-virtual-machine".into(),
                code: "K7Q2-9F3A".into(),
                from: Some("192.168.64.5".into()),
            }]
        );
        assert_eq!(seed.asked, ["192.168.64.5"]);
        // Every address an ask came from, each once, an ended exchange's included.
        let more =
            format!("{RECORD}ask-ended 4200 from=192.168.64.1 eof\nasked 4300 from=10.0.0.7\n");
        assert_eq!(
            read_desk(&more).sources(),
            ["192.168.64.5", "10.0.0.7", "192.168.64.1"]
        );
        assert_eq!(seed.let_in, Some((4100, "admins-virtual-machine".into())));
        assert_eq!(seed.posted, Some(Ok(4180)));
        let shown = presented_phrase(&seed).unwrap().unwrap();
        assert_eq!(
            (shown.at, shown.name.as_str()),
            (1300, "admins-virtual-machine")
        );
        assert_eq!(shown.words, "maple river ocean");
        assert!(
            !format!("{shown:?}").contains("maple"),
            "a Debug names no word"
        );

        // Not yet: the seed heard nothing, or heard the Mac and shows no phrase.
        assert_eq!(
            presented_phrase(&read_desk("member 1 listen=127.0.0.1:7486 name=s\n")),
            Ok(None)
        );
        let asking: String = RECORD.lines().take(2).map(|l| format!("{l}\n")).collect();
        assert_eq!(presented_phrase(&read_desk(&asking)), Ok(None));
        // Never: a phrase for a Mac it never heard, words that are no phrase, two phrases, a
        // refusal, an Allow that failed.
        let other = RECORD.replace(
            "phrase 1300 id=1 name=admins-virtual-machine",
            "phrase 1300 id=1 name=mx",
        );
        assert_eq!(
            presented_phrase(&read_desk(&other)).unwrap_err(),
            "the member shows a phrase for mx, a Mac it never heard ask"
        );
        let garbled = RECORD.replace("maple river ocean", "maple river zzzz");
        let refused = presented_phrase(&read_desk(&garbled)).unwrap_err();
        assert!(
            refused.starts_with("the member's phrase for admins-virtual-machine:"),
            "{refused}"
        );
        assert!(
            !refused.contains("maple"),
            "an error names no word: {refused}"
        );
        let short = RECORD.replace("maple river ocean", "maple river");
        assert!(presented_phrase(&read_desk(&short)).is_err());
        let twice =
            format!("{RECORD}phrase 1400 id=1 name=admins-virtual-machine abandon ability able\n");
        assert_eq!(
            presented_phrase(&read_desk(&twice)).unwrap_err(),
            "the member shows 2 phrases, where the lane pressed Allow once"
        );
        assert_eq!(
            presented_phrase(&read_desk(
                "refused 9 this member holds no Commons control (60 s on)\n"
            ))
            .unwrap_err(),
            "the member's desk refused to serve: this member holds no Commons control (60 s on)"
        );
        let failed = format!("{asking}allow-failed 1290 this Mac cannot allow yet\n");
        assert_eq!(
            presented_phrase(&read_desk(&failed)).unwrap_err(),
            "the member's Allow failed: this Mac cannot allow yet"
        );
        assert_eq!(
            read_desk("posted 5 failed no vault server took it\nask-ended 6 from=127.0.0.1 eof\n"),
            Desk {
                posted: Some(Err("no vault server took it".into())),
                ended: vec!["from=127.0.0.1 eof".into()],
                ..Desk::default()
            }
        );
    }

    #[test]
    fn the_guest_reads_say_each_file_where_the_joiner_stands_and_who_listens() {
        let arrivals = "gh_access_token 1761650000.250 same\n\
                        gh_access_token_alabsystems 1761650000.5 differs\n\
                        ai-accounts.csv none absent\n";
        assert_eq!(
            read_arrivals(arrivals),
            [
                Arrived {
                    file: "gh_access_token".into(),
                    at: Some(1_761_650_000_250),
                    held: Held::Same
                },
                Arrived {
                    file: "gh_access_token_alabsystems".into(),
                    at: Some(1_761_650_000_500),
                    held: Held::Differs
                },
                Arrived {
                    file: "ai-accounts.csv".into(),
                    at: None,
                    held: Held::Absent
                },
            ]
        );
        assert_eq!(
            Standing::of("member as admins-virtual-machine\n"),
            Standing::In
        );
        assert_eq!(
            Standing::of("allowed: ~/.secrets is written, and this Mac follows vault changes"),
            Standing::Allowed
        );
        assert_eq!(Standing::of("asking: press Allow"), Standing::Asking);
        assert_eq!(
            Standing::of("not in ALab: run `aterm members join` here"),
            Standing::NotIn
        );
        assert_eq!(
            Standing::of("unknown: no home"),
            Standing::Unread("unknown: no home".into())
        );
        assert_eq!(
            read_status("member as admins-virtual-machine\n1761650004000\n"),
            (Standing::In, Some(1_761_650_004_000))
        );
        assert_eq!(
            read_status("allowed: ~/.secrets is written\n1761650003000\n"),
            (Standing::Allowed, Some(1_761_650_003_000))
        );
        // A status that printed nothing, or a clock that never came, reads as such.
        assert_eq!(
            read_status("1761650003000\n"),
            (Standing::Unread(String::new()), Some(1_761_650_003_000))
        );
        assert_eq!(read_status("member as m\n"), (Standing::In, None));
        assert_eq!(read_status(""), (Standing::Unread(String::new()), None));

        let lsof = "COMMAND     PID  USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME\n\
                    aterm       812 admin    9u  IPv4 0x1c3a5e0f1d2b3c4d      0t0  TCP 127.0.0.1:7485 (LISTEN)\n\
                    aterm-lab   830 admin    3u  IPv4 0x1c3a5e0f1d2b3c4e      0t0  TCP 127.0.0.1:7486 (LISTEN)\n\
                    aterm       812 admin   10u  IPv6 0x1c3a5e0f1d2b3c4f      0t0  TCP *:7485 (LISTEN)\n\
                    aterm       812 admin   11u  IPv4 0x1c3a5e0f1d2b3c50      0t0  TCP 127.0.0.1:7485 (LISTEN)\n";
        assert_eq!(
            listen_addresses(lsof),
            ["127.0.0.1:7485", "127.0.0.1:7486", "*:7485"],
            "a wildcard listener is read, so the verdict sees it"
        );
        assert!(listen_addresses("").is_empty());
        assert!(on_loopback("127.0.0.1:7485") && on_loopback("[::1]:7486"));
        assert!(!on_loopback("*:7485") && !on_loopback("[::]:7486") && !on_loopback("10.0.0.2:1"));
    }

    #[test]
    fn vault_phrase_is_a_row_of_all_beside_vault_local_that_runs_the_app() {
        use super::super::{Scenario, parse, parse_scenarios};
        let all = parse_scenarios("all").unwrap();
        let at = all
            .iter()
            .position(|s| *s == Scenario::VaultPhrase)
            .expect("all runs vault-phrase");
        assert_eq!(all[at - 1], Scenario::VaultLocal);
        assert_eq!(Scenario::VaultPhrase.name(), "vault-phrase");
        let ask = parse(
            &[
                "--scenario".to_owned(),
                "vault-phrase,vault-local".to_owned(),
            ],
            "img".into(),
        )
        .unwrap();
        assert_eq!(ask.scenarios, [Scenario::VaultLocal, Scenario::VaultPhrase]);
        let s = Scenario::VaultPhrase;
        assert!(s.needs_app() && !s.grows_disk() && s.not_run().is_none());
    }

    /// The seed's server listens on 127.0.0.1 and names no mDNS service, the member's desk
    /// listens at the guest's own address registered under the run's lab service, and the
    /// driver types the phrase at the prompt `aterm members join` prints.
    #[test]
    fn the_seed_serves_on_loopback_and_the_member_at_the_guests_address() {
        let serve = serve("/A/aterm", "/n/seed");
        assert!(
            serve.starts_with("(/usr/bin/env HOME=/n/seed /A/aterm secrets serve "),
            "{serve}"
        );
        let words: Vec<&str> = serve.split_whitespace().collect();
        let after = |flag: &str| {
            words
                .iter()
                .position(|w| *w == flag)
                .map(|at| words[at + 1])
        };
        assert_eq!(after("--listen"), Some("127.0.0.1:7485"), "{serve}");
        assert!(words.contains(&"--no-advertise"), "{serve}");
        let member = desk("/n/helper", "/n/member", "192.168.64.5:7486", "/n/rec", LAB);
        assert!(
            member.contains(
                "HOME=/n/member /n/helper member /n/member 192.168.64.5:7486 /n/rec \
                 --advertise _alab-lab-k3x9q2._tcp "
            ),
            "{member}"
        );
        let create = create("/A/aterm", "/n/seed");
        assert!(
            create.contains("secrets create 'http://127.0.0.1:7485/v1/")
                && create.contains("--import /n/seed/seed --no-agent --name nux-seed"),
            "{create}"
        );
        assert!(
            !create.contains("--advertise") && !create.contains("--serve"),
            "{create}"
        );
        let h = Homes {
            seed: "/n/seed".into(),
            member: "/n/member".into(),
            joiner: "/n/joiner".into(),
        };
        let homes = homes(&h, "/n/rec");
        assert!(
            homes.contains("mkdir -p /n/seed/seed /n/member /n/joiner;"),
            "{homes}"
        );
        for c in &CREDENTIALS {
            assert!(
                homes.contains(&format!("> /n/seed/seed/{};", c.file)),
                "{homes}"
            );
        }
        assert!(homes.contains("chmod 600 /n/seed/seed/*"), "{homes}");
        assert_eq!(
            as_home("/A/aterm", "/n/joiner"),
            "/usr/bin/env HOME=/n/joiner /A/aterm"
        );
        assert_eq!(
            arrivals("/n/helper", "/n/seed", "/n/joiner"),
            "/n/helper arrivals /n/seed/seed /n/joiner/.secrets gh_access_token \
             gh_access_token_alabsystems ai-accounts.csv"
        );

        // The prompt `aterm members join` prints (`members_cli.rs`'s `read_phrase`), answered
        // with `$NUX_PHRASE` before any rule that answers Enter.
        let drive = super::super::DRIVE;
        let rule = drive
            .lines()
            .find(|l| l.contains("$phrase }"))
            .expect("a rule answers with the phrase");
        let pattern = rule
            .trim()
            .strip_prefix("-re {")
            .and_then(|r| r.split_once("} {"))
            .map(|(p, _)| p)
            .unwrap();
        let re = aterm_regex::Regex::new(pattern).unwrap();
        assert!(re.is_match("The phrase nux-seed shows: "), "{pattern}");
        assert!(!re.is_match("Vault join password: "), "{pattern}");
        let at = |needle: &str| drive.find(needle).unwrap();
        assert!(at("$phrase }") < at("[Pp]assword"), "{drive}");
        assert!(drive.contains("set phrase $env(NUX_PHRASE)"), "{drive}");
    }

    /// Every command of the joiner's (its ask, its join, its subscriber, its status) runs with
    /// `ATERM_LAB_MEMBER_SERVICE` naming the run's lab service and is named no member, so each
    /// finds the member by its own browse of that service, never of `_alab._tcp`; the seed's
    /// commands carry neither.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `as_joiner` without its `ATERM_LAB_MEMBER_SERVICE=` word
    /// (the joiner then browses the live fleet's `_alab._tcp`) fails here at the ask.
    #[test]
    fn every_command_of_the_joiners_browses_the_runs_lab_service_alone() {
        let me = as_joiner("/A/aterm", "/n/joiner", LAB);
        let ask = ask_cmd(&me);
        let join = join_cmd(&me);
        let watching = watch(&me, "/n/joiner");
        let status = status_now(&me);
        for cmd in [&ask, &join, &watching, &status] {
            let words: Vec<&str> = cmd.split_whitespace().collect();
            assert!(
                words.contains(&"ATERM_LAB_MEMBER_SERVICE=_alab-lab-k3x9q2._tcp")
                    && words.contains(&"HOME=/n/joiner"),
                "{cmd}"
            );
            assert!(!words.contains(&"--at"), "{cmd}");
        }
        assert!(ask.ends_with(" members join --no-wait"), "{ask}");
        assert!(join.ends_with(" members join"), "{join}");
        assert_eq!(
            watching,
            "(/usr/bin/env HOME=/n/joiner ATERM_LAB_MEMBER_SERVICE=_alab-lab-k3x9q2._tcp \
             /A/aterm secrets watch </dev/null >/n/joiner/watch.log 2>&1 &)"
        );
        let seed = watch(&as_home("/A/aterm", "/n/seed"), "/n/seed");
        assert!(!seed.contains(LAB_MEMBER_SERVICE_ENV), "{seed}");
        assert_eq!(
            names_knob("/g/aterm.app/Contents/MacOS/aterm"),
            "/usr/bin/grep -q -a -F ATERM_LAB_MEMBER_SERVICE \
             '/g/aterm.app/Contents/MacOS/aterm' && echo named || echo absent"
        );
    }

    /// The run's lab service is one `browse_members` takes, never the live fleet's, and each
    /// run's differs with its random bytes.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `lab_service_of` with a seventh character after its nonce
    /// (`_alab-lab-000000x._tcp`, past DNS-SD's 15) fails here at its first `lab_service`
    /// assertion.
    #[test]
    fn the_runs_lab_service_is_a_lab_one() {
        for bytes in [[0u8; 6], [255; 6], [35, 36, 71, 72, 200, 9]] {
            let service = lab_service_of(bytes);
            assert!(aterm_secrets::discover::lab_service(&service), "{service}");
            assert_eq!(
                aterm_secrets::discover::member_service(Some(std::ffi::OsStr::new(&service)))
                    .unwrap(),
                service
            );
            assert_ne!(service, MEMBER_SERVICE);
        }
        assert_eq!(
            lab_service_of([0, 1, 10, 35, 36, 255]),
            "_alab-lab-01az03._tcp"
        );
        assert_ne!(lab_service_of([1; 6]), lab_service_of([2; 6]));
    }

    /// The guest's addresses as `ifconfig -a` prints them on a tart guest, and the one the member
    /// listens at: its IPv4 address on tart's network, never loopback or link-local.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `lan_address` without its link-local filter fails here:
    /// `left: Some(169.254.20.7)`, `right: Some(192.168.64.5)`.
    #[test]
    fn the_member_listens_at_the_guests_address_on_tarts_network() {
        let ifconfig = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\
            \toptions=1203<RXCSUM,TXCSUM,TXSTATUS,SW_TIMESTAMP>\n\
            \tinet 127.0.0.1 netmask 0xff000000\n\
            \tinet6 ::1 prefixlen 128 \n\
            \tinet6 fe80::1%lo0 prefixlen 64 scopeid 0x1 \n\
            en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
            \tether 7a:21:9c:40:11:02\n\
            \tinet6 fe80::1c2f:8ad3:6b1e:2a41%en0 prefixlen 64 secured scopeid 0x6 \n\
            \tinet 169.254.20.7 netmask 0xffff0000 broadcast 169.254.255.255\n\
            \tinet 192.168.64.5 netmask 0xffffff00 broadcast 192.168.64.255\n\
            \tinet 192.168.64.5 netmask 0xffffff00 broadcast 192.168.64.255\n";
        let addresses = inet_addresses(ifconfig);
        let expected: Vec<IpAddr> = [
            "127.0.0.1",
            "::1",
            "fe80::1",
            "fe80::1c2f:8ad3:6b1e:2a41",
            "169.254.20.7",
            "192.168.64.5",
        ]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
        assert_eq!(addresses, expected);
        assert_eq!(
            lan_address(&addresses),
            Some(Ipv4Addr::new(192, 168, 64, 5))
        );
        assert_eq!(
            lan_address(&addresses[..5]),
            None,
            "loopback and link-local are not it"
        );
        assert!(inet_addresses("").is_empty());
    }

    /// The joiner's browse is proven to have found the member only when the member registered
    /// under the run's lab service, the guest's browse heard that instance resolving to the guest,
    /// and the member heard the joiner ask from a guest address beyond loopback.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `browse_verdict` with its `Ok(ip) if ip.is_loopback()` arm
    /// removed (an ask over loopback passes for one the browse resolved) fails here:
    /// `left: []`.
    #[test]
    fn a_run_whose_joiner_did_not_find_the_member_by_its_browse_fails() {
        assert_eq!(browse_verdict(&passing()), Vec::<String>::new());
        // The joiner dialed the member at loopback (as `--at 127.0.0.1:7486` would).
        let mut saw = passing();
        saw.joiner_from = Some("127.0.0.1".into());
        saw.asks_from = vec!["127.0.0.1".into()];
        assert_eq!(
            verdict(&saw),
            ["the joiner asked over loopback (127.0.0.1), not at the address its browse resolved"]
        );
        let mut saw = passing();
        saw.joiner_from = None;
        assert_eq!(
            verdict(&saw),
            ["the member heard the joiner ask from no address"]
        );
        // The member registered nothing, or its browse never heard the member's instance.
        let mut saw = passing();
        saw.advertised = None;
        assert_eq!(
            verdict(&saw),
            [format!(
                "the member's record names no {LAB} registration, so no browse could find it"
            )]
        );
        let mut saw = passing();
        saw.lab_adverts = Ok(Vec::new());
        assert_eq!(
            verdict(&saw),
            [format!(
                "the guest's browse of {LAB} never heard the member's alab-1a2b3c4d resolving \
                 to the guest (it heard none)"
            )]
        );
        let mut saw = passing();
        saw.lab_adverts = Err("dns-sd failed: DNSService call failed -65563".into());
        assert_eq!(
            verdict(&saw),
            [format!(
                "the guest's browse of {LAB} did not run (dns-sd failed: DNSService call failed \
                 -65563), so what the joiner's browse could find is unproven"
            )]
        );
        // The member's instance heard resolving elsewhere is not the member the joiner found.
        let mut saw = passing();
        saw.lab_adverts = Ok(vec![LabAdvert {
            instance: "alab-1a2b3c4d".into(),
            elsewhere: Some("admins-virtual-machine.local 192.168.64.1".into()),
        }]);
        assert_eq!(verdict(&saw).len(), 2, "{:?}", verdict(&saw));
        assert_eq!(browse_verdict(&saw).len(), 1, "{:?}", browse_verdict(&saw));
    }

    /// Anything that reached beyond the guest fails the row: an ask the member heard from an
    /// address that is not the guest's (this Mac's end of tart's bridge, another Mac), or an
    /// advert of the lab service the guest heard that is not its own.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `isolation_verdict` without its check of `asks_from`
    /// against the guest's addresses (an ask from this Mac's bridge address passes) fails here:
    /// `left: []`.
    #[test]
    fn a_run_that_reached_beyond_the_guest_fails() {
        assert_eq!(isolation_verdict(&passing()), Vec::<String>::new());
        for stranger in ["192.168.64.1", "192.168.1.242", "fe80::9", "no-address"] {
            let mut saw = passing();
            saw.asks_from.push(stranger.into());
            assert_eq!(
                verdict(&saw),
                [format!(
                    "the member heard an ask from {stranger}, not one of the guest's own addresses"
                )]
            );
        }
        // The joiner's own ask from a stranger fails as isolation, not as a browse that missed.
        let mut saw = passing();
        saw.joiner_from = Some("192.168.64.1".into());
        saw.asks_from = vec!["192.168.64.1".into()];
        assert_eq!(browse_verdict(&saw), Vec::<String>::new());
        assert_eq!(
            isolation_verdict(&saw).len(),
            1,
            "{:?}",
            isolation_verdict(&saw)
        );
        // A lab advert beside the member's that resolved elsewhere is one the joiner could ask.
        let mut saw = passing();
        saw.lab_adverts.as_mut().unwrap().push(LabAdvert {
            instance: "alab-99999999".into(),
            elsewhere: Some("m22.local 192.168.1.242".into()),
        });
        assert_eq!(
            verdict(&saw),
            [format!(
                "the guest heard 1 {LAB} advert(s) not its own (alab-99999999 (m22.local \
                 192.168.1.242)): the joiner's browse could have asked them"
            )]
        );
    }
}
