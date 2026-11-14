// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab mac nux`'s `vault-phrase` row: private member admission and synthetic credential
//! materialization in one guest (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10). It exercises the
//! explicit private-vault protocol, not default ALab enrollment, automatic subscriber setup,
//! GitHub or publishing authentication, AI account access, or repository cloning.
//!
//! Before the realm is made, a broker-only baseline runs once as root through the same guest
//! app and the existing password driver. Its prompts and result are recorded separately from
//! member admission; no first-user password or update-prompt claim follows from this setup.
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
//! - The joiner first pins the seed's explicit URL and signer with ordinary `secrets join
//!   --no-agent --no-wait`, remaining Pending with no credentials. The build's own CLI then runs
//!   `aterm members join --no-wait`, then `aterm members join <phrase>` on a terminal, as the
//!   member's words say (its shell expands `$NUX_PHRASE`, so the lane's command lines never
//!   hold it; a join with no phrase has proved and asked by itself, never prompted, since
//!   `f5eabf5697`), then an explicitly owned subscriber (`aterm secrets watch`). Each joiner
//!   command that discovers the member uses the LAN broker on the run's lab
//!   members service, never the live `_alab._tcp` service.
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
//! loopback but the member's, a `dns-sd` registration but the member's lab one, a real-service
//! advert that is not this Mac's own, or a browse whose transcript shows it never looked).
//! aterm's own adverts are held by its LAN broker, so no `dns-sd` process shows them and the
//! registration check reads only the lab's own `dns-sd` tools; an aterm advert of the live
//! services is caught by the guest's browses of [`SERVICES`], whoever registered it.
//!
//! The lab's table ([`joiner_table`]): once the member is in, the seed founds the lab's
//! membership log (Genesis v2, through the guest helper's `genesis`, as `lab mesh churn` founds
//! its lab), whose first `[everyone]` epoch the table's digest is keyed by: `secrets create`
//! mints none, and a digest on the seed without it said "this Mac holds no [everyone] key" (E3,
//! 2026-11-05). The seed then imports a one-row synthetic emails sheet ([`TABLE_ADDRESS`]) and
//! seals a password of random bytes into it with the owner's own verb (`aterm db emails set …
//! password --stdin`, the value on stdin from a mode-600 file in the guest, never printed)
//! before the joiner asks, so the table is older than the joiner, as the owner's is older than
//! any new Mac. Once the joiner reads In, the guest helper's `repair` moves
//! the seed's Commons tables to it and each look compares the two `aterm db emails --digest`s
//! ([`table_verdict`]): one `[everyone]` epoch, the row with its password, one keyed digest. The
//! record shows epochs, counts and digest prefixes, never a value.
//!
//! The member's Commons enrolment stays the guest helper's in-process exchange over loopback
//! (the row's `enrolled by the runtime` cell says NOT MEASURED). The Commons runtime finds its
//! peers by beacons to one multicast group and port (`aterm_db::runtime`'s transport), which a
//! runtime on the guest's network would send onto tart's bridge, where this Mac's live runtime
//! listens; the only other port it takes is a debug build's, loopback alone by design
//! (`ATERM_DEBUG_COMMONS_LOOPBACK_PORT`, which refuses any LAN attachment) and absent from the
//! release-shaped app the row runs. Running it over the guest's network needs a lab port the
//! runtime keeps apart from the live one on the LAN, in `aterm-db`'s transport.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_secrets::discover::{LAB_MEMBER_SERVICE_ENV, MEMBER_SERVICE};

use super::advert::{self, Split, Whose};
use super::{
    ALAB_SERVICE, GUEST_HELPER, GUEST_NUX, Guest, LOCAL_VAULT, LOCAL_VAULT_PORT, Row, Value,
    browse_adds, drive_cells_within, guest_browse, guest_dns_sd, guest_heard, host_guests,
    local_network_blocks, or_missing, secs_to_ms, squeeze,
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

/// The unapproved joiner's name in the private vault.
const JOINER_NAME: &str = "nux-joiner";

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
        file: aterm_types::vault_entry::DEV_GH_TOKEN,
        what: "GitHub",
        synthetic: "nux-synthetic-github-%s",
    },
    Credential {
        file: aterm_types::vault_entry::RELEASE_GH_TOKEN,
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

/// Ask to join the seed's vault as `me`, named `name`, pinned to its `signer` ([`signer_of`]);
/// what it printed when it fails. `home` holds only this scratch identity's output.
fn pinned_join(me: &str, home: &str, name: &str, signer: &str) -> String {
    format!(
        "{me} secrets join 'http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}#{signer}' \
         --name {name} --no-agent --no-wait > {home}/join.out 2>&1 \
         || {{ cat {home}/join.out; exit 1; }}"
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

/// The guest command that signs the seed's Genesis v2 into `seed/genesis.v2` (the guest helper's
/// `genesis`): the lab's first `[everyone]` epoch, wrapped to each Mac its vault and Commons
/// list, which `secrets create` does not mint and the table's digest is keyed by.
fn genesis(helper: &str, seed: &str) -> String {
    format!("/usr/bin/env HOME={seed} {helper} genesis {seed} {seed}/genesis.v2")
}

/// What [`genesis`] printed, for its cell: `<n> members, digest <8 hex>`, else its words.
#[must_use]
pub fn genesis_said(out: &str) -> String {
    let line = out
        .lines()
        .find(|l| l.starts_with("genesis "))
        .unwrap_or_default();
    let word = |key: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key))
            .map(str::to_owned)
    };
    match (word("members="), word("digest=")) {
        (Some(members), Some(digest)) => format!("{members} members, digest {digest}"),
        _ => format!("said {}", squeeze(out.trim(), 120)),
    }
}

/// The one row of the lab's synthetic emails table: an address no mail reaches.
pub const TABLE_ADDRESS: &str = "nux-proof@example.invalid";

/// The rounds of each guest-helper `repair` the joiner's table look runs (`guest::steps`).
const REPAIR_ROUNDS: usize = 8;

/// The guest commands that write the seed's synthetic emails sheet and a password beside it,
/// under `seed/sheet` (never `seed/seed`, which `secrets create --import` publishes), mode 600:
/// the sheet is one row, [`TABLE_ADDRESS`], whose Password column it leaves out (a sheet's is a
/// has-password flag), and the password is 24 random bytes from the guest's own `/dev/urandom`,
/// written to its file and never printed.
fn sheet(seed: &str) -> String {
    format!(
        "set -e; umask 077; mkdir -p {seed}/sheet; \
         printf 'Email Address,Status\\n{TABLE_ADDRESS},active\\n' > {seed}/sheet/emails.csv; \
         head -c 24 /dev/urandom | base64 | tr -d '\\n' > {seed}/sheet/pw"
    )
}

/// The guest command that imports the seed's sheet into its registry (`aterm db alab import
/// --emails … --apply`, the one verb that declares the registry), as the lab's first Mac with no
/// peer yet; what it printed when it fails.
fn import_table(aterm: &str, seed: &str) -> String {
    format!(
        "{} db alab import --emails {seed}/sheet/emails.csv --apply > {seed}/import.out 2>&1 \
         || {{ cat {seed}/import.out; exit 1; }}",
        as_home(aterm, seed)
    )
}

/// The guest command that seals the seed's password into [`TABLE_ADDRESS`]'s `password` cell
/// from its file on stdin, as the owner sets one (`aterm db emails set … password --stdin`); what
/// it printed when it fails (its words, never the value, which is never on argv).
fn seal(aterm: &str, seed: &str) -> String {
    format!(
        "{} db emails set {TABLE_ADDRESS} password --stdin < {seed}/sheet/pw > {seed}/seal.out 2>&1 \
         || {{ cat {seed}/seal.out; exit 1; }}",
        as_home(aterm, seed)
    )
}

/// The marker between the two outputs [`table_now`] prints.
const TABLE_MARK: &str = "--nux-table--";

/// The guest command that prints, as `me`, the emails view (whose `complete` [`read_table`]
/// reads; every password in it masked) and then its keyed digest, a [`TABLE_MARK`] line between.
fn table_now(me: &str) -> String {
    format!("{me} db emails 2>&1; echo '{TABLE_MARK}'; {me} db emails --digest 2>&1")
}

/// The guest command that runs the guest helper's `repair` of the joiner from the seed:
/// [`REPAIR_ROUNDS`] Commons rounds over loopback, the joiner enrolled by the seed first if it is
/// still pending in Commons.
fn repair(helper: &str, joiner: &str, seed: &str) -> String {
    format!(
        "/usr/bin/env HOME={joiner} {helper} repair {joiner} {seed} {REPAIR_ROUNDS} {seed} 2>&1"
    )
}

/// One identity's emails table, value-free: whether its history reads complete, and its keyed
/// digest's epoch (eight hex digits), rows, rows with a password and digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRead {
    pub complete: bool,
    pub epoch: String,
    pub rows: u64,
    pub with_password: u64,
    pub digest: String,
}

/// [`table_now`]'s output read.
///
/// # Errors
/// No [`TABLE_MARK`], or a side that is no view or digest: what it said instead, shortened.
pub fn read_table(text: &str) -> Result<TableRead, String> {
    let (view, digest) = text
        .split_once(&format!("{TABLE_MARK}\n"))
        .or_else(|| text.split_once(TABLE_MARK))
        .ok_or_else(|| format!("no table read ({})", squeeze(text.trim(), 160)))?;
    let json = |side: &str| {
        side.lines()
            .rev()
            .find_map(|l| aterm_json::from_str::<aterm_json::Value>(l.trim()).ok())
            .filter(aterm_json::Value::is_object)
    };
    let view_said = || squeeze(view.trim(), 160);
    let complete = json(view)
        .and_then(|v| v.get("complete").and_then(aterm_json::Value::as_bool))
        .ok_or_else(|| format!("the emails view said {}", view_said()))?;
    let d = json(digest)
        .filter(|d| d.get("digest").is_some())
        .ok_or_else(|| format!("its digest said {}", squeeze(digest.trim(), 160)))?;
    let text_of = |key: &str| {
        d.get(key)
            .and_then(aterm_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("its digest has no {key}"))
    };
    let count = |key: &str| {
        d.get(key)
            .and_then(aterm_json::Value::as_u64)
            .ok_or_else(|| format!("its digest has no {key}"))
    };
    Ok(TableRead {
        complete,
        epoch: text_of("epoch")?,
        rows: count("rows")?,
        with_password: count("with_password")?,
        digest: text_of("digest")?,
    })
}

/// What the joiner's table look saw, for [`table_verdict`]: the seed's table and the joiner's as
/// they read at its last look (why not, where one did not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSaw {
    pub seed: Result<TableRead, String>,
    pub joiner: Result<TableRead, String>,
}

/// Why the joiner does not read the lab's table, each reason once: none when it reads the seed's
/// table, a password among its rows, under the same epoch's key. Digests compare only under one
/// epoch, so another epoch is unproven rather than a match; a history that is not complete is
/// not the table.
#[must_use]
pub fn table_verdict(t: &TableSaw) -> Vec<String> {
    let mut why = Vec::new();
    let seed = match &t.seed {
        Ok(seed) => Some(seed),
        Err(e) => {
            why.push(format!("the seed's table did not read: {e}"));
            None
        }
    };
    if let Some(seed) = seed {
        if seed.rows == 0 {
            why.push("the seed's table has no row".to_owned());
        }
        if seed.with_password == 0 {
            why.push(
                "the seed's table holds no password, so the joiner opening one is unproven"
                    .to_owned(),
            );
        }
    }
    match (&t.joiner, seed) {
        (Err(e), _) => why.push(format!("the joiner's table did not read: {e}")),
        (Ok(joiner), _) if !joiner.complete => {
            why.push("the joiner's table history is not complete".to_owned());
        }
        (Ok(joiner), Some(seed)) if joiner.epoch != seed.epoch => why.push(format!(
            "the joiner digested under epoch {}, the seed under {}, so the tables are unproven",
            joiner.epoch, seed.epoch
        )),
        (Ok(joiner), Some(seed))
            if (joiner.rows, joiner.with_password, &joiner.digest)
                != (seed.rows, seed.with_password, &seed.digest) =>
        {
            why.push(format!(
                "the joiner's table is not the seed's ({} row(s), {} with a password, digest \
                 {}…; the seed's {}, {}, {}…)",
                joiner.rows,
                joiner.with_password,
                prefix(&joiner.digest),
                seed.rows,
                seed.with_password,
                prefix(&seed.digest)
            ));
        }
        (Ok(_), _) => {}
    }
    why
}

/// A digest's first eight characters, all a record shows of it.
fn prefix(digest: &str) -> &str {
    digest.get(..8).unwrap_or(digest)
}

/// What one guest-helper `repair` printed: `repaired <ms> standing=… rounds=… control=… rows=…
/// keys=…`, its words by name; `None` for any other line (its refusal).
#[must_use]
pub fn read_repair(line: &str) -> Option<BTreeMap<String, String>> {
    let rest = line.trim().strip_prefix("repaired ")?;
    let mut words = rest.split_whitespace();
    let at = words.next()?.parse::<u64>().ok()?;
    let mut fields: BTreeMap<String, String> = words
        .filter_map(|w| w.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect();
    fields.insert("at".to_owned(), at.to_string());
    Some(fields)
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

/// The seed and the member, ready: the seed's homes, server (answering within [`HANG`]) and
/// vault; the member asked, approved by the seed, In (within [`HANG`]) and enrolled in the seed's
/// Commons; the seed's Genesis v2 and table ([`genesis`], [`sheet`], [`import_table`], [`seal`]:
/// `Err` says why they are not there) and approver; and the member's desk serving at `lab`'s address under its service (within
/// [`HANG`]). Each step is its own guest command, so a failure names the step and its output or
/// log; the member's steps are timed in `row`.
///
/// # Errors
/// A step failed, or a wait's detector fired: the row cannot go on.
fn setup(
    g: &Guest,
    h: &Homes,
    record: &str,
    lab: &Lab,
    row: &mut Row,
) -> Result<(Desk, Result<(), String>), Fail> {
    let helper = format!("{GUEST_NUX}/{GUEST_HELPER}");
    let Homes {
        seed,
        member,
        joiner,
    } = h;
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
    g.sh(
        &pinned_join(&as_home(&g.aterm, member), member, MEMBER_NAME, signer),
        STEP,
    )?;
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

    // The lab's founding and its table, before the joiner asks: the seed signs Genesis v2, whose
    // first `[everyone]` epoch wraps to the seed and the member, then imports the table and seals
    // its password. A step that fails is the table check's to report, never the rest of the
    // row's.
    let made = Instant::now();
    let seeded = g
        .sh(&genesis(&helper, seed), STEP)
        .and_then(|signed| {
            row.cell("seed: Genesis v2", Value::Text(genesis_said(&signed)));
            g.sh(&sheet(seed), STEP)
        })
        .and_then(|_| g.sh(&import_table(&g.aterm, seed), STEP))
        .and_then(|_| g.sh(&seal(&g.aterm, seed), STEP))
        .map_err(|fail| squeeze(fail.msg.as_deref().unwrap_or("a guest step failed"), 300));
    row.cell(
        "seed: founded, table imported and its password sealed",
        match &seeded {
            Ok(_) => Value::Ms(u64::try_from(made.elapsed().as_millis()).unwrap_or(u64::MAX)),
            Err(why) => Value::Text(format!("failed: {why}")),
        },
    );

    // An empty source means the live default vault, which A12 correctly refuses in this guest.
    // Name this run's private realm before asking its members; neither an approval nor a
    // password is supplied, and no subscriber is installed under the scratch home.
    let me = as_joiner(&g.aterm, joiner, &lab.service);
    g.sh(&pinned_join(&me, joiner, JOINER_NAME, signer), STEP)?;
    let source: aterm_toml::Value =
        aterm_toml::from_str(&g.read(&format!("{joiner}/.config/aterm/secrets/source.toml"))?)
            .map_err(|_| Fail::die("the joiner's private vault source did not read back"))?;
    let status: aterm_json::Value =
        aterm_json::from_str(&g.sh(&format!("{me} secrets status --json"), STEP)?)
            .map_err(|_| Fail::die("the joiner's private vault status did not read back"))?;
    let vault = &status["vault"];
    let url = format!("http://127.0.0.1:{LOCAL_VAULT_PORT}/v1/{LOCAL_VAULT}");
    if source.get("url").and_then(aterm_toml::Value::as_str) != Some(url.as_str())
        || source.get("path").is_some()
        || source.get("signer").and_then(aterm_toml::Value::as_str) != Some(signer)
        || vault["state"].as_str() != Some("known")
        || vault["value"]["configured_signer"].as_str() != Some(signer)
        || vault["value"]["membership"]["state"].as_str() != Some("pending")
    {
        return Err(Fail::die(
            "the joiner did not remain Pending at the run's exact private vault and signer",
        ));
    }
    let absent = CREDENTIALS
        .iter()
        .map(|c| {
            let path = format!("{joiner}/.secrets/{}", c.file);
            format!("test ! -e '{path}' && test ! -L '{path}'")
        })
        .collect::<Vec<_>>()
        .join(" && ");
    g.sh(&absent, STEP)?;
    row.cell(
        "joiner: explicit private vault before Allow",
        Value::Text("exact source and signer; Pending; 3 credential files absent".into()),
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
        Some(at) => Ok((at, seeded.map(|_| ()))),
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
/// hold `-R` or `-P` anywhere (`dns-sd -i en0 -P …` included): the lab's own registrations, such
/// as the guest helper's member advert. aterm registers through its LAN broker, so no `dns-sd`
/// process shows its adverts. A browse (`dns-sd -B`), the `script` a browse runs under and the
/// shell that ran `pgrep` are none.
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
    /// The lab's table as the seed and the joiner read it ([`joiner_table`]).
    pub table: TableSaw,
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
/// In, the joiner reads the lab's table with its password ([`table_verdict`]), the joiner's own browse found the member and nothing else ([`browse_verdict`]), and
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
    why.extend(table_verdict(&saw.table));
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
/// address; the guest's one `dns-sd` registration is the member's of the run's lab service
/// (aterm's own, through its LAN broker, show in no `dns-sd` process); the guest's browse of
/// that service heard no advert that is not the guest's own; and every [`SERVICES`] browse ran
/// and heard no advert but this Mac's own, in the guest (something in it or beyond it
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
/// words come first: one that heard nothing while the guest's own browse heard the member and a
/// plain connect reached it puts the miss in the ask's browse, which goes through the LAN broker
/// (`docs/DESIGN-lan-broker-2026-11-05.md`). Pure.
#[must_use]
pub fn lan_cause(
    said: &str,
    reached: bool,
    pid: Option<u32>,
    blocks: &[(Option<u64>, u32)],
) -> &'static str {
    let held = pid.is_some_and(|pid| blocks.iter().any(|(_, p)| *p == pid));
    if said.contains(NO_MEMBER_ANSWERS) && reached && !held {
        return "the ask's browse heard no member though the guest's own browse heard it and a \
                plain connect reached it: the LAN broker's browse answered with nothing, and \
                macOS logged no hold";
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

/// The joiner's join with the phrase, `me` its command line ([`as_joiner`]). Its own lab
/// service discovers the member. A shell expands `$NUX_PHRASE`, so this command never holds it.
fn join_cmd(me: &str) -> String {
    format!(
        "/bin/sh -c {}",
        crate::util::shell_quote(&format!("{me} members join \"$NUX_PHRASE\""))
    )
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

/// The joiner reads the lab's table: once it reads In, the guest helper's `repair` moves the
/// seed's Commons tables to it ([`repair`]; the joiner enrolled by the seed first while it is
/// pending), and each look reads both tables ([`table_now`]) until the joiner's is the seed's
/// ([`table_verdict`]), under the [`HANG`] detector, a repair before each look. Cells: the
/// joiner's Commons standing, what the repairs moved, the time from In and from the ask to the
/// table, and the verdict, which shows epochs, counts and digest prefixes, never a value.
///
/// # Errors
/// The guest could not be read, or the run was stopped.
fn joiner_table(
    g: &Guest,
    h: &Homes,
    helper: &str,
    me: &str,
    seeded: Result<(), String>,
    asked: Option<u64>,
    row: &mut Row,
) -> Result<TableSaw, Fail> {
    if let Err(why) = seeded {
        let saw = TableSaw {
            seed: Err(format!("the seed made no table: {why}")),
            joiner: Err("the seed made no table to read".to_owned()),
        };
        row.cell(
            "joiner reads the lab's table",
            verdict_cell(&table_verdict(&saw), String::new),
        );
        return Ok(saw);
    }
    let from = g.clock_ms()?;
    let seed_me = as_home(&g.aterm, &h.seed);
    let mut repairs: Vec<BTreeMap<String, String>> = Vec::new();
    let mut refused = None;
    let mut last = TableSaw {
        seed: Err("never read".to_owned()),
        joiner: Err("never read".to_owned()),
    };
    let equal = within_hang(|| {
        let said = g.run(&repair(helper, &h.joiner, &h.seed), STEP)?;
        match said.lines().find_map(read_repair) {
            Some(fields) => repairs.push(fields),
            None => refused = Some(squeeze(said.trim(), 200)),
        }
        last = TableSaw {
            seed: read_table(&g.run(&table_now(&seed_me), STEP)?),
            joiner: read_table(&g.run(&table_now(me), STEP)?),
        };
        Ok(table_verdict(&last).is_empty().then_some(()))
    })?;
    let read_at = match equal {
        Some(()) => Some(g.clock_ms()?),
        None => None,
    };
    row.cell(
        "joiner's Commons standing",
        or_missing(
            repairs.first().and_then(|r| r.get("standing").cloned()),
            Value::Text,
            &format!(
                "no repair ran: {}",
                refused.as_deref().unwrap_or("it printed nothing")
            ),
        ),
    );
    let sum = |key: &str| -> u64 {
        repairs
            .iter()
            .filter_map(|r| r.get(key)?.parse::<u64>().ok())
            .sum()
    };
    row.cell(
        "repairs to the joiner",
        Value::Text(format!(
            "{} run(s) of {REPAIR_ROUNDS} rounds: {} control record(s), {} table operation(s), \
             {} historical key(s){}",
            repairs.len(),
            sum("control"),
            sum("rows"),
            sum("keys"),
            refused
                .as_deref()
                .map_or_else(String::new, |why| format!("; last refusal: {why}"))
        )),
    );
    row.cell(
        "In to the joiner's table",
        or_missing(
            since(read_at, Some(from)),
            Value::Ms,
            &format!("not the seed's after {} s", HANG.as_secs()),
        ),
    );
    row.cell(
        "ask to the joiner's table",
        or_missing(
            since(read_at, asked),
            Value::Ms,
            "not read, or no ask start",
        ),
    );
    row.cell(
        "joiner reads the lab's table",
        verdict_cell(&table_verdict(&last), || match &last.joiner {
            Ok(t) => format!(
                "epoch {}, {} row(s), {} with a password, digest {}… equal to the seed's",
                t.epoch,
                t.rows,
                t.with_password,
                prefix(&t.digest)
            ),
            Err(_) => String::new(),
        }),
    );
    Ok(last)
}

/// Prepare the row's real root broker with the guest's own app, without any other setup row.
/// This is a separately recorded prerequisite, never a measured first-user elevation.
fn prepare_broker(g: &Guest, row: &mut Row) -> Result<(), Fail> {
    let baseline = format!("{GUEST_NUX}/phrase-broker.toml");
    g.sh(
        &format!(
            "(umask 077; set -C; printf 'schema = 1\\n[lan]\\nbroker = true\\n' > '{baseline}')"
        ),
        STEP,
    )?;
    let (preparation, _) = g.drive_within(
        "phrase-broker",
        &format!("NUX_SUDO={}", g.sudo_password()),
        &format!(
            "/usr/bin/sudo -k -p 'Mac password: ' {} machines here apply '{baseline}'",
            crate::util::shell_quote(&g.aterm)
        ),
        HANG,
    )?;
    drive_cells_within(row, "broker preparation: ", &preparation, HANG);
    if preparation.exit != Some(0) {
        return Err(Fail::die(
            "the broker-only root preparation did not exit successfully",
        ));
    }
    let status = super::lan::read_lan_status(&g.sh(
        &format!("{} lan status", crate::util::shell_quote(&g.aterm)),
        STEP,
    )?);
    match status {
        super::lan::LanStatus::Serving { protocol, .. } if protocol == aterm_uds::lan::PROTOCOL => {
            row.cell(
                "broker preparation: root protocol",
                Value::Text(format!("serving protocol {protocol}; direct path not used")),
            );
            Ok(())
        }
        _ => Err(Fail::die(
            "the broker-only preparation left no compatible root broker",
        )),
    }
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
    prepare_broker(g, row)?;
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
    let (served, seeded) = setup(g, &h, &record, &lab, row)?;
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

    // The phrase typed with the join, as a person types what the member shows; the driver's
    // limit is the hang detector.
    let (join, join_out) = g.drive_within(
        "phrase-join",
        &format!("NUX_PHRASE={}", crate::util::shell_quote(&shown.words)),
        &join_cmd(&me),
        HANG,
    )?;
    drive_cells_within(row, "join: ", &join, HANG);
    let typed = join.start_epoch_ms;
    row.cell(
        "ask to the phrase typed",
        or_missing(since(typed, asked), Value::Ms, "the join did not start"),
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
    let table = joiner_table(g, &h, &helper, &me, seeded, asked, row)?;
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
        table,
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
            "open: the member enrols, and the joiner's tables move, through the guest helper's \
             in-process exchange over loopback; a Commons runtime on the guest's network would \
             beacon on the live fleet's group and port, and two homes in one guest share its \
             address, whose beacons a runtime drops as its own (module docs)"
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
                 files the seed's, In, the lab's table the seed's with its password, nothing \
                 beyond the guest",
                n = CREDENTIALS.len()
            )),
        );
    } else {
        row.fail(format!("acceptance: {}", failed.join("; ")));
    }
    Ok(())
}

#[cfg(test)]
#[path = "mac_nux_phrase_tests.rs"]
mod tests;
