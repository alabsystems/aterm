// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab mac nux`'s `vault-phrase` row: the owner's acceptance test ("a new LAN Mac downloads
//! aterm, joins with one keyphrase approval from any existing member, and GitHub, publishing and
//! AI accounts work") as one command, in one guest, on its loopback alone
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10).
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
//!   members desk on its custody behind an `ask`-only listener on `127.0.0.1`, presses Allow for
//!   the first Mac that asks and writes the phrase to its record ([`read_desk`]). The shipped
//!   member shows a phrase only in its own sheet (§3.3.5) and the lane reads no window, so the
//!   phrase is read there, as `lab swarm partition` presses Allow in its own process.
//! - The joiner is the build's own CLI: `aterm members join --no-wait --at <the member>`
//!   (install.sh's windowless ask), then `aterm members join --at <the member>` on a terminal
//!   with the phrase typed at its prompt ([`super::DRIVE`]'s `$NUX_PHRASE`), then its own
//!   subscriber (`aterm secrets watch`), as a joined Mac's agent runs it. It dials the member at
//!   its loopback address where a new Mac browses the LAN for members
//!   (`discover::browse_members`): a browse from the guest would reach the members this Mac's
//!   LAN advertises, so the row never runs one.
//!
//! The row times the ask to the member's row, to the phrase shown and typed, to each synthetic
//! file in the joiner's `~/.secrets` (compared with the seed's in the guest, never printed) and to
//! `aterm members status` reading In, then reads its isolation while every server still runs:
//! each aterm listener's address, and the `_alab-vault._tcp` and `_alab._tcp` adverts heard in
//! the guest and, from a guest, on this Mac, each placed by where its host resolves
//! ([`super::advert`]): this Mac's own registrations, which mDNSResponder also announces on
//! tart's bridge, are named in the row and never fail it. [`verdict`] fails the row on a phrase
//! that came from any Mac but the member, a file missing or not the seed's, a joiner that never
//! reads In, a listener beyond loopback, an advert that is not this Mac's own, or a browse whose
//! transcript shows it never looked.

use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_secrets::discover::MEMBER_SERVICE;

use super::advert::Split;
use super::{
    ALAB_SERVICE, GUEST_HELPER, GUEST_NUX, Guest, LOCAL_VAULT, LOCAL_VAULT_PORT, Row, Value,
    drive_cells_within, guest_heard, host_guests, or_missing, secs_to_ms, squeeze,
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

/// The guest command that starts `home`'s vault subscriber (an admin's: its approver), as a
/// Mac's agent runs it, in the background (its log `home/watch.log`).
fn watch(aterm: &str, home: &str) -> String {
    format!(
        "({} secrets watch </dev/null >{home}/watch.log 2>&1 &)",
        as_home(aterm, home)
    )
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

/// The guest command that starts the guest helper's `member` on `home`'s custody, listening on
/// 127.0.0.1 alone, in the background (its record `record`, its errors `home/member.err`).
fn desk(helper: &str, home: &str, record: &str) -> String {
    format!(
        "(/usr/bin/env HOME={home} {helper} member {home} 127.0.0.1:{MEMBER_PORT} {record} \
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

/// The seed and the member, ready: the seed's homes, server (answering within [`HANG`]), vault
/// and approver; the member asked, approved by the seed, In (within [`HANG`]) and enrolled in the
/// seed's Commons; and the member's desk serving (within [`HANG`]). Each step is its own guest
/// command, so a failure names the step and its output or log; the member's steps are timed in
/// `row`.
///
/// # Errors
/// A step failed, or a wait's detector fired: the row cannot go on.
fn setup(g: &Guest, h: &Homes, record: &str, row: &mut Row) -> Result<Desk, Fail> {
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

    g.sh(&watch(&g.aterm, seed), STEP)?;
    g.sh(&desk(&helper, member, record), STEP)?;
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

/// One Mac the member heard ask: when, the name it asks as, its join code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub at: u64,
    pub name: String,
    pub code: String,
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
    /// When it began to serve, at which address, as which member.
    pub serving: Option<(u64, String, String)>,
    /// Why it could not serve.
    pub refused: Option<String>,
    pub heard: Vec<Heard>,
    pub presented: Vec<Presented>,
    pub allow_failed: Option<String>,
    /// When a run sent its bundle, and to which Mac.
    pub let_in: Option<(u64, String)>,
    /// The request it posted with its `Allowed`: when it was taken, or why not.
    pub posted: Option<Result<u64, String>>,
    /// The exchanges that ended in an error.
    pub ended: Vec<String>,
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
                    });
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

/// What a `vault-phrase` run saw, for [`verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saw {
    /// The Mac whose desk showed the phrase (its record's `member … name=`).
    pub allowed_by: Option<String>,
    /// Each [`CREDENTIALS`] file in the joiner's `~/.secrets`.
    pub files: Vec<Arrived>,
    /// Where the joiner stood last.
    pub standing: Standing,
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
/// [`CREDENTIALS`] file is in the joiner's `~/.secrets` with the seed's value, the joiner
/// reads In, both of the seed's ports were read listening and every aterm listener is on
/// loopback, no guest process held a Bonjour registration, and every [`SERVICES`] browse ran and
/// heard no advert but this Mac's own, in the guest (its own or one from beyond it: the row is not
/// alone on its loopback) or from a guest on this Mac.
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
        .filter(|a| !on_loopback(a))
        .map(String::as_str)
        .collect();
    if !beyond.is_empty() {
        why.push(format!(
            "an aterm process listened beyond loopback: {}",
            beyond.join(", ")
        ));
    }
    if !saw.registrations.is_empty() {
        why.push(format!(
            "a guest process registered a Bonjour service: {}",
            saw.registrations.join("; ")
        ));
    }
    for service in SERVICES {
        match saw.guest_adverts.iter().find(|(s, _)| s == service) {
            Some((_, Ok(heard))) if heard.others.is_empty() => {}
            Some((_, Ok(heard))) => why.push(format!(
                "the guest heard {} {service} advert(s) not this Mac's ({}): the row is not \
                 alone on its loopback",
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
    setup(g, &h, &record, row)?;
    let me = as_home(&g.aterm, joiner);
    let at = format!("127.0.0.1:{MEMBER_PORT}");

    // The ask, as install.sh's windowless lanes run it.
    let (ask, ask_out) = g.notty_within(
        "phrase-ask",
        &format!("{me} members join --no-wait --at {at}"),
        HANG,
    )?;
    drive_cells_within(row, "ask: ", &ask, HANG);
    row.cell(
        "ask: said",
        or_missing(
            ask_out
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| squeeze(l.trim(), 100)),
            Value::Text,
            "the ask printed nothing",
        ),
    );
    let asked = ask.start_epoch_ms;

    // The member hears it and presses Allow: the phrase it shows, read off its desk record.
    let desk = match await_phrase(g, &record)? {
        Ok(desk) => desk,
        Err(why) => {
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
        &format!("{me} members join --at {at}"),
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
    g.sh(&watch(&g.aterm, joiner), STEP)?;
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
        or_missing(
            since(desk.let_in.as_ref().map(|l| l.0), asked),
            Value::Ms,
            "no run sent its bundle",
        ),
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
    let mut guest = Vec::new();
    let mut host = Vec::new();
    for service in SERVICES {
        guest.push((service.to_owned(), guest_heard(g, service)?));
        host.push((service.to_owned(), host_guests(run, service)?));
    }
    row.cell(
        "adverts heard in the guest",
        Value::Text(
            guest
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

    let heard_own = guest
        .iter()
        .chain(&host)
        .any(|(_, heard)| heard.as_ref().is_ok_and(|h| !h.this_macs.is_empty()));
    let failed = verdict(&Saw {
        allowed_by,
        files,
        standing,
        listening,
        registrations,
        guest_adverts: guest,
        host_adverts: host,
    });
    if failed.is_empty() {
        row.cell(
            "acceptance",
            Value::Text(format!(
                "pass: allowed by {MEMBER_NAME}, {n} of {n} files the seed's, In, loopback only, \
                 no advert{}",
                if heard_own { " but this Mac's own" } else { "" },
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

    /// A run that met the acceptance test: every file the seed's, In, its two ports on loopback,
    /// no advert anywhere.
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
            listening: vec!["127.0.0.1:7485".into(), "127.0.0.1:7486".into()],
            registrations: Vec::new(),
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
    const RECORD: &str = "member 1000 listen=127.0.0.1:7486 name=nux-member\n\
                          heard 1250 id=1 name=admins-virtual-machine code=K7Q2-9F3A from=127.0.0.1\n\
                          asked 1251 from=127.0.0.1\n\
                          phrase 1300 id=1 name=admins-virtual-machine maple river ocean\n\
                          let-in 4100 name=admins-virtual-machine\n\
                          posted 4180 ok\n";

    #[test]
    fn the_phrase_reader_types_only_the_one_phrase_shown_for_the_mac_that_asked() {
        let seed = read_desk(RECORD);
        assert_eq!(
            seed.serving,
            Some((1000, "127.0.0.1:7486".into(), "nux-member".into()))
        );
        assert_eq!(
            seed.heard,
            [Heard {
                at: 1250,
                name: "admins-virtual-machine".into(),
                code: "K7Q2-9F3A".into()
            }]
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

    /// The seed's server and the member's desk listen on 127.0.0.1 and name no mDNS service, the
    /// joiner dials the member there (so it browses for no member), and the driver types the
    /// phrase at the prompt `aterm members join` prints.
    #[test]
    fn the_servers_listen_on_loopback_and_the_joiner_dials_the_member_there() {
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
        let member = desk("/n/helper", "/n/member", "/n/rec");
        assert!(
            member.contains("HOME=/n/member /n/helper member /n/member 127.0.0.1:7486 /n/rec"),
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
}
