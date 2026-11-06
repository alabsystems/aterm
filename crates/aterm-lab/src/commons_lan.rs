// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ordinary hosts on separate physical Macs. Paired profiles use synthetic prior approval.
//! A world whose vault is the live default ALab vault (one an earlier lab run left admitted there)
//! opens for no verb but behind `--live-alab-vault-keeps-member` ([`LIVE`]): nothing a lab run
//! holds takes that member out again.
//! Preparation never imports the recipient's private identity or supplies Commons admission.
//!
//! A lab vault is a file the recipients copy (`prepare-source` alone) or one the source world
//! serves at a LAN address (`prepare-source --serve <ip:port>`, step E1b of
//! `docs/DESIGN-alab-one-mesh-2026-11-05.md` §11): the source world's `node` runs its server
//! (`aterm secrets serve`) beside its node, the approved recipients join it at its URL, and a Mac
//! nobody approved is pinned to it (`pin`) and joins by its proof with no question (`home-lan`
//! names the lab's home LAN on each member, `members … join --at` proves the Mac to them). Every
//! world's node listens for the fleet's `ask` on a lab port ([`LAB_FLEET_PORT`], or `--port`),
//! never the live fleet's, which the live aterm on each Mac holds.

use crate::commons_startup::table::Table;
use crate::instance::{BOOT, CONFIG_OFF, Instance, Launch, World};
use crate::{Fail, Res, stop};
use aterm_codec::hex;
use aterm_db::replica::InventorySummary;
use aterm_json::{Value, json};
use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
use aterm_secrets::commons::bootstrap::Bootstrap;
use aterm_types::network_state::Visibility;
use aterm_uds::private_file;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "commons_lan_claims.rs"]
mod claims;
#[path = "commons_lan_emails.rs"]
mod emails;

const HANG: Duration = Duration::from_secs(60);
const CAPSULE_LIMIT: usize = 32 * 1024;
/// The most recipients one synthetic vault approves: labels `m2` … `m9`.
const MAX_RECIPIENTS: usize = 8;
/// The longest ordinary host run: an hour, so one host stays up across a run of edits; its
/// `stop` marker ends it sooner.
const MAX_RUN_S: u64 = 3600;
const APPLICATION: &str = "lan-startup-data";
const USAGE: &str = "commons-lan prepare-recipient <new-world> [--port <lab-fleet-port>] | prepare-source <new-world> [--serve <ip:port>] [--port <lab-fleet-port>] <recipient-key>... | join <world> <capsule-file> | pin <world> <card-file> | home-lan <world> <cidr> | run <aterm> <world> <seconds> | node <aterm> <world> <seconds> | pair <aterm> <world> <seconds> | follow <aterm> <world> <seconds> <owner-pid> | stop <run-directory> | db <aterm> <world> <db-command> [args...] | secrets <aterm> <world> approve <age1…> --name <host>|join <lab-profile>/fixture.vault|join <its-lab-url>#<signer>|status | members <aterm> <world> join --at <ip:port>... | declare <world> | write <world> <key> | read <world> | claim <world> <counter> <since> <until> | release <world> <counter> <until> | claims <world> | emails-fixture <world> <seed> | emails-set <aterm> <world> <seed> <row> <column> <value-or-tag> | emails-wait <aterm> <world> <digest>|!<digest> <seconds> | emails-reveal <aterm> <world> <seed> [<tag>...]";

/// The port a lab world's node serves the fleet's `ask` on unless `--port` names another.
const LAB_FLEET_PORT: u16 = 7190;
/// The live fleet's port (`aterm members join`'s default, the node's default listen), which the
/// live aterm on each Mac holds: no lab world listens or dials there.
const LIVE_FLEET_PORT: u16 = 7100;
/// The live vault servers' port (`aterm secrets serve`'s default, the ALab vault's URL): no lab
/// vault is served or joined there.
const LIVE_VAULT_PORT: u16 = 7484;
/// What every lab vault's name starts with, so a lab URL never names the live ALab vault.
const LAB_VAULT_PREFIX: &str = "lab-";
/// The folder in a source world its vault server keeps its store in.
const SERVER_STORE: &str = "server";
/// The file in a world that names the URL-served lab vault it follows, `<url>#<signer>`.
const LAB_VAULT: &str = "lab-vault";
/// The file in a source world that names the address its vault server listens at.
const LAB_SERVE: &str = "lab-serve";
/// The card a source world writes for a Mac nobody approved: the lab vault's URL and signer.
const LAB_CARD: &str = "lab-card.json";

/// The flag that lets a `commons-lan` verb drive a world the live default ALab vault admitted: it
/// names the vault and what stays in it, a member no run takes out.
const LIVE: &str = "--live-alab-vault-keeps-member";

fn fail(error: impl std::fmt::Display) -> Fail {
    Fail::die(error.to_string())
}

fn emit(value: &Value) -> Result<(), Fail> {
    println!("{}", aterm_json::to_string(value).map_err(fail)?);
    std::io::stdout().flush().map_err(fail)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    let mut file = crate::util::create_new_regular_file(path, 0o600).map_err(fail)?;
    file.write_all(bytes).map_err(fail)
}

/// A lab fleet port `text` names: a decimal port from 1024, never the live fleet's or the live
/// vault servers'.
fn lab_port(text: &str) -> Result<u16, Fail> {
    let port = Some(text)
        .filter(|text| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|text| text.parse::<u16>().ok())
        .filter(|port| *port >= 1024 && ![LIVE_FLEET_PORT, LIVE_VAULT_PORT].contains(port));
    port.ok_or_else(|| {
        Fail::usage(format!(
            "a lab port is 1024 to 65535 and never the live fleet's ({LIVE_FLEET_PORT}) or the \
             live vault's ({LIVE_VAULT_PORT}): {text:?}"
        ))
    })
}

/// A prepared profile of `role` whose node serves the fleet's `ask` on `port` (`[fleet] listen`,
/// which the shipped `aterm fleet init` that `node` runs keeps).
fn prepare(root: &Path, role: &str, port: u16) -> Result<World, Fail> {
    let config = format!(
        "{CONFIG_OFF}\n[ai]\nenabled = false\n[fleet]\nwatch = false\nlisten = \"0.0.0.0:{port}\"\n"
    );
    let world = World::create_at(root, &config).map_err(fail)?;
    write_new(&world.dir.join("commons-lan-role"), role.as_bytes())?;
    Ok(world)
}

/// Whether `url` is a lab vault's: `http://<IPv4>:<port>/v1/lab-…`, the host a private or
/// loopback address, the port a lab one ([`lab_port`]), the name one only the lab mints
/// ([`LAB_VAULT_PREFIX`]). The live ALab vault's URL is none of these.
fn lab_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let Some((host, name)) = rest.split_once("/v1/") else {
        return false;
    };
    let Ok(addr) = host.parse::<std::net::SocketAddrV4>() else {
        return false;
    };
    (addr.ip().is_private() || addr.ip().is_loopback())
        && lab_port(&addr.port().to_string()).is_ok()
        && name.strip_prefix(LAB_VAULT_PREFIX).is_some_and(|tail| {
            !tail.is_empty()
                && tail.len() <= 32
                && tail.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && !aterm_secrets::is_alab_default_url(url)
}

/// Whether `signer` is a padded base64 vault key, as a join source names it after `#`.
fn signer_word(signer: &str) -> bool {
    !signer.is_empty()
        && signer.len() <= 64
        && signer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// The URL-served lab vault `world` follows, `<url>#<signer>` ([`LAB_VAULT`]), where it holds one.
fn lab_vault_of(world: &World) -> Result<Option<String>, Fail> {
    match private_file::read_private(&world.dir, LAB_VAULT, 1024) {
        Ok(bytes) => Ok(Some(
            std::str::from_utf8(&bytes)
                .map_err(fail)?
                .trim_end()
                .to_owned(),
        )),
        Err(private_file::PrivateFileError::Missing) => Ok(None),
        Err(error) => Err(fail(error)),
    }
}

/// Reopen a prepared profile. One whose vault is the live default ALab vault (a world an earlier
/// lab run left admitted there) opens only where `live` ([`LIVE`]): an ordinary host, a table
/// write or a read there keeps that identity a member, and its Commons runtime enrolls it in the
/// vault's membership log, where it counts as a voter.
fn open(root: &Path, live: bool) -> Result<(World, String), Fail> {
    let world = World::open_at(root).map_err(fail)?;
    let role = private_file::read_private(&world.dir, "commons-lan-role", 16).map_err(fail)?;
    let role = std::str::from_utf8(&role).map_err(fail)?;
    if !matches!(role, "source" | "recipient") {
        return Err(Fail::die("not a prepared Commons LAN profile"));
    }
    let config = client::config(&Paths::for_home(&world.home)).map_err(fail)?;
    if !live && config.as_ref().is_some_and(client::alab_vault) {
        return Err(Fail::die(format!(
            "{} is an identity of the live default ALab vault: driving it keeps that member and \
             enrolls it in the vault's membership log. Pass {} to accept that member",
            world.dir.display(),
            LIVE
        )));
    }
    Ok((world, role.into()))
}

fn recipient_key(paths: &Paths) -> Result<String, Fail> {
    Ok(client::machine_identity(paths)
        .map_err(fail)?
        .recipient()
        .to_bech32())
}

fn prepare_recipient(root: &Path, port: u16) -> Result<Value, Fail> {
    let world = prepare(root, "recipient", port)?;
    let paths = Paths::for_home(&world.home);
    Ok(
        json!({"schema":1,"role":"recipient","world":world.dir.display().to_string(),
        "recipient":recipient_key(&paths)?,"commons_admitted":false,"application_tables":0}),
    )
}

/// Found the lab's vault in a new source world under `root`, approving `recipients` under `m2`
/// on. With `serve` it is served at that address: a vault server runs in this process while the
/// vault is founded (its store the world's [`SERVER_STORE`]), the world records the address
/// ([`LAB_SERVE`]) for its `node` to serve it again and the vault ([`LAB_VAULT`]), the capsules
/// carry the URL, and a card ([`LAB_CARD`]) names the URL and signer for a Mac nobody approved.
/// Without it the vault is a file the capsules carry whole.
fn prepare_source(
    root: &Path,
    recipients: &[String],
    serve: Option<std::net::SocketAddr>,
    port: u16,
) -> Result<Value, Fail> {
    if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
        return Err(Fail::usage(format!(
            "prepare-source approves 1 to {MAX_RECIPIENTS} recipients"
        )));
    }
    if let Some(addr) = serve
        && !match addr {
            std::net::SocketAddr::V4(addr) => {
                (addr.ip().is_private() || addr.ip().is_loopback())
                    && (addr.port() == 0 || lab_port(&addr.port().to_string()).is_ok())
            }
            std::net::SocketAddr::V6(_) => false,
        }
    {
        return Err(Fail::usage(format!(
            "--serve takes this Mac's private IPv4 address and a lab port: {addr}"
        )));
    }
    let world = prepare(root, "source", port)?;
    let paths = Paths::for_home(&world.home);
    let vault = world.dir.join("fixture.vault");
    let server = match serve {
        Some(addr) => {
            let listener = std::net::TcpListener::bind(addr).map_err(fail)?;
            let listen = listener.local_addr().map_err(fail)?;
            let running = aterm_secrets::server::start(
                listener,
                &aterm_secrets::server::Options {
                    listen,
                    store: world.dir.join(SERVER_STORE),
                    advertise: None,
                    pins: Vec::new(),
                    hold: Duration::from_millis(100),
                    keep_awake: false,
                },
            )
            .map_err(fail)?;
            Some(running)
        }
        None => None,
    };
    let url = match &server {
        Some(running) => {
            let nonce = hex::encode_lower(&aterm_db::crypto::random::<4>().map_err(fail)?);
            Some(format!(
                "http://{}/v1/{LAB_VAULT_PREFIX}{nonce}",
                running.addr
            ))
        }
        None => None,
    };
    let founded = (|| -> Result<_, Fail> {
        let source = url
            .clone()
            .unwrap_or_else(|| vault.to_string_lossy().into_owned());
        let created = client::create(&paths, &source, None, Some("m1")).map_err(fail)?;
        let admin = client::admin_open(&paths).map_err(fail)?;
        let mut doc = admin.doc.clone();
        // Each recipient takes the next label after the source's `m1`, in the order named.
        let hosts: Vec<String> = (2..)
            .take(recipients.len())
            .map(|n| format!("m{n}"))
            .collect();
        for (host, recipient) in hosts.iter().zip(recipients) {
            doc.add_machine(host, recipient).map_err(fail)?;
        }
        client::admin_publish_approved(&paths, &admin, &doc, recipients).map_err(fail)?;
        let genesis =
            client::seed_vault_genesis(&paths, &world.dir.join("fixture.genesis")).map_err(fail)?;
        Ok((created, hosts, genesis))
    })();
    if let Some(running) = server {
        running.stop();
    }
    let (created, hosts, genesis) = founded?;
    let (carried_url, vault_hex) = match &url {
        Some(url) => {
            let pinned = format!("{url}#{}", created.signer);
            write_new(&world.dir.join(LAB_VAULT), format!("{pinned}\n").as_bytes())?;
            let listen = url
                .strip_prefix("http://")
                .and_then(|rest| rest.split_once('/'))
                .map(|(listen, _)| listen)
                .ok_or_else(|| Fail::die("the lab vault's URL names no address"))?;
            write_new(&world.dir.join(LAB_SERVE), format!("{listen}\n").as_bytes())?;
            (Some(url.clone()), None)
        }
        None => {
            use std::os::unix::fs::PermissionsExt as _;
            aterm_uds::regular_file::open(&vault)
                .and_then(|(f, _)| f.set_permissions(std::fs::Permissions::from_mode(0o600)))
                .map_err(fail)?;
            let bytes = aterm_secrets::fsx::read_private(&vault, CAPSULE_LIMIT)
                .map_err(fail)?
                .ok_or_else(|| Fail::die("synthetic vault publication is missing"))?;
            (None, Some(hex::encode_lower(bytes.as_slice())))
        }
    };
    let mut capsules = Vec::with_capacity(recipients.len());
    for (host, recipient) in hosts.iter().zip(recipients) {
        let capsule = json!({"schema":1,"signer":created.signer,"recipient":recipient,
            "host":host,"url":carried_url,"vault_hex":vault_hex});
        if aterm_json::to_vec(&capsule).map_err(fail)?.len() > CAPSULE_LIMIT {
            return Err(Fail::die(
                "synthetic vault capsule exceeds its transfer bound",
            ));
        }
        capsules.push(capsule);
    }
    let card = url
        .as_ref()
        .map(|url| json!({"schema":1,"url":url,"signer":created.signer}));
    if let Some(card) = &card {
        write_new(
            &world.dir.join(LAB_CARD),
            &aterm_json::to_vec(card).map_err(fail)?,
        )?;
    }
    Ok(
        json!({"schema":1,"role":"source","world":world.dir.display().to_string(),
        "genesis":hex::encode_lower(&genesis.digest),"genesis_members":genesis.members,
        "url":url,"card":card,"capsules":capsules}),
    )
}

/// Join a recipient world under `root` to the lab vault its `capsule` (from `prepare-source`)
/// approved it in: the file the capsule carries, or the vault served at its URL. Either way it
/// is in the vault with no Commons admission and no tables yet; at a URL it reads the vault's
/// records (its genesis) as it joins, which a file vault hands no Mac.
fn join_recipient(root: &Path, capsule: &[u8]) -> Result<Value, Fail> {
    // Never with the live flag: this would point a live member's profile at a synthetic vault.
    let (world, role) = open(root, false)?;
    if role != "recipient" || capsule.len() > CAPSULE_LIMIT {
        return Err(Fail::die(
            "join requires a recipient profile and a bounded capsule",
        ));
    }
    let capsule: Value = aterm_json::from_slice(capsule)
        .map_err(|_| Fail::die("invalid synthetic vault capsule"))?;
    let paths = Paths::for_home(&world.home);
    if capsule["schema"].as_u64() != Some(1)
        || capsule["recipient"].as_str() != Some(recipient_key(&paths)?.as_str())
    {
        return Err(Fail::die(
            "synthetic vault capsule is for another recipient",
        ));
    }
    let host = capsule["host"]
        .as_str()
        .filter(|host| {
            host.strip_prefix('m')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                && host.len() <= 8
        })
        .ok_or_else(|| Fail::die("synthetic vault capsule names no recipient label"))?;
    let signer = capsule["signer"]
        .as_str()
        .filter(|key| signer_word(key))
        .ok_or_else(|| Fail::die("synthetic vault signer is invalid"))?;
    let served = match capsule["url"].as_str() {
        Some(url) if lab_url(url) => Some(format!("{url}#{signer}")),
        Some(_) => return Err(Fail::die("synthetic vault capsule names no lab URL")),
        None => None,
    };
    let source = match &served {
        Some(pinned) => pinned.clone(),
        None => {
            let vault = capsule["vault_hex"]
                .as_str()
                .and_then(|value| hex::decode_vec(value.as_bytes()))
                .ok_or_else(|| Fail::die("synthetic vault capsule bytes are invalid"))?;
            let file = world.dir.join("fixture.vault");
            write_new(&file, &vault)?;
            format!("{}#{signer}", file.display())
        }
    };
    client::join(
        &paths,
        &JoinOptions {
            source,
            host: Some(host.into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .map_err(fail)?;
    if let Some(pinned) = &served {
        write_new(&world.dir.join(LAB_VAULT), format!("{pinned}\n").as_bytes())?;
    }
    let bootstrap = Bootstrap::required(&paths).map_err(fail)?;
    if bootstrap.witness().map_err(fail)?.is_some()
        || bootstrap.log_witness().map_err(fail)?.is_some()
        || bootstrap.is_log_backed().map_err(fail)?
        || bootstrap.access().is_ok_and(|access| access.is_some())
        || (served.is_none() && paths.records_file().exists())
        || paths.dir.join("commons/tables").exists()
    {
        return Err(Fail::die(
            "recipient already has Commons admission, records or tables",
        ));
    }
    Ok(
        json!({"schema":1,"role":"recipient","world":world.dir.display().to_string(),
        "vault_approved":true,"served":served.is_some(),"commons_admitted":false,
        "membership_records":paths.records_file().exists(),"application_tables":0}),
    )
}

/// Pin a fresh recipient world under `root` to the URL-served lab vault its `card` (from
/// `prepare-source --serve`) names, with no approval: it joins the vault at its URL, pinned to
/// its signer, and waits there (Pending), so a later proof's bundle is taken only from that very
/// vault (`aterm members join --at …`, `client::readmit_from_log_bundle`). A world that follows a
/// vault already, a card that names no lab vault, and one the live ALab vault's are refused.
fn pin(root: &Path, card: &[u8]) -> Result<Value, Fail> {
    let (world, role) = open(root, false)?;
    let paths = Paths::for_home(&world.home);
    if role != "recipient" || card.len() > CAPSULE_LIMIT {
        return Err(Fail::die(
            "pin requires a recipient profile and a bounded card",
        ));
    }
    if client::config(&paths).map_err(fail)?.is_some() {
        return Err(Fail::die("this world follows a vault already"));
    }
    let card: Value =
        aterm_json::from_slice(card).map_err(|_| Fail::die("invalid lab vault card"))?;
    let url = card["url"]
        .as_str()
        .filter(|url| card["schema"].as_u64() == Some(1) && lab_url(url))
        .ok_or_else(|| Fail::die("the card names no lab vault URL"))?;
    let signer = card["signer"]
        .as_str()
        .filter(|key| signer_word(key) && *key != aterm_secrets::ALAB_SIGNER)
        .ok_or_else(|| Fail::die("the card names no lab vault signer"))?;
    let pinned = format!("{url}#{signer}");
    let joined = client::join(
        &paths,
        &JoinOptions {
            source: pinned.clone(),
            host: None,
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .map_err(fail)?;
    let configured = client::config(&paths)
        .map_err(fail)?
        .ok_or_else(|| Fail::die("the pin wrote no vault configuration"))?;
    if client::alab_vault(&configured) {
        return Err(Fail::die("the card's vault is the live ALab vault's"));
    }
    write_new(&world.dir.join(LAB_VAULT), format!("{pinned}\n").as_bytes())?;
    let membership = match joined {
        client::JoinOutcome::Pending { .. } => "pending",
        client::JoinOutcome::Joined { .. } => "in",
        _ => "other",
    };
    Ok(
        json!({"schema":1,"role":"recipient","world":world.dir.display().to_string(),
        "pinned":true,"membership":membership,"commons_admitted":false}),
    )
}

/// Name `cidr` as the home LAN of the lab world under `root` (`aterm_secrets::members::lab_lan`),
/// so its node's desk lets a Mac on that LAN in by its proof, with nobody's Allow, as a live
/// member on its home ALab LAN does. Only a world that follows a URL-served lab vault (never the
/// live ALab vault, never one whose records pin the live genesis), whose identity carries the lab
/// mark, takes one, and only a CIDR that holds this Mac's default route now; nothing is written
/// otherwise. A release image compiles no lab home and refuses.
#[cfg(debug_assertions)]
fn home_lan(root: &Path, cidr: &str) -> Result<Value, Fail> {
    use aterm_secrets::members::lab_lan;
    // Never with the live flag: a live member's profile has no lab home.
    let (world, role) = open(root, false)?;
    let paths = Paths::for_home(&world.home);
    let config = client::config(&paths)
        .map_err(fail)?
        .ok_or_else(|| Fail::die("home-lan needs a world that follows its lab vault"))?;
    let served = match &config.source {
        aterm_secrets::transport::Source::Url(url) => lab_url(url),
        aterm_secrets::transport::Source::Path(_) => false,
    };
    if client::alab_vault(&config) || !served || client::on_live_genesis(&paths) {
        return Err(Fail::die(
            "a lab home LAN is a URL-served lab vault's: never the live ALab vault's, its \
             genesis's or a vault read from a file",
        ));
    }
    if client::lab_mark(&paths).map_err(fail)?.is_none() {
        return Err(Fail::die(
            "this world's identity carries no lab mark, so its desk takes no lab home",
        ));
    }
    let cidr = lab_lan::Cidr::parse(cidr).map_err(|error| Fail::usage(error.to_string()))?;
    let own = lab_lan::here_in(&cidr).ok_or_else(|| {
        Fail::die(format!(
            "this Mac's default route (Ethernet or Wi-Fi, no tunnel) is not on {cidr}: nothing \
             was written"
        ))
    })?;
    lab_lan::write(&paths, &cidr).map_err(fail)?;
    Ok(
        json!({"schema":1,"role":role,"world":world.dir.display().to_string(),
        "home_lan":cidr.to_string(),"own":own.to_string()}),
    )
}

fn prepare_run(root: &Path, live: bool) -> Result<(World, String), Fail> {
    let (mut world, role) = open(root, live)?;
    let nonce = hex::encode_lower(&aterm_db::crypto::random::<8>().map_err(fail)?);
    let run = world.dir.join(format!("run-{nonce}.noindex"));
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&run)
        .map_err(fail)?;
    write_new(&run.join("lab-run"), b"commons-lan-run/1\n")?;
    // Keep each process's app output and lifeline beside its receipts while sharing
    // the prepared profile's original home, configuration and runtime directory.
    world.dir.clone_from(&run);
    Ok((world, role))
}

/// Observe what the ordinary host established without preparing its custody.
fn observed_admission(paths: &Paths, deadline: Instant) -> bool {
    let Ok((bootstrap, journal)) = Bootstrap::established_with_journal_until(paths, deadline)
    else {
        return false;
    };
    drop(journal);
    bootstrap.is_log_backed().unwrap_or(false)
        && bootstrap
            .log_witness()
            .is_ok_and(|witness| witness.is_some())
        && bootstrap.access().is_ok_and(|access| access.is_some())
}

fn run(
    aterm: &Path,
    root: &Path,
    seconds: u64,
    follows: Option<u32>,
    live: bool,
    mut ready: Option<std::sync::mpsc::SyncSender<(u32, PathBuf)>>,
) -> Res {
    if !(60..=MAX_RUN_S).contains(&seconds) {
        return Err(Fail::usage(format!(
            "commons-lan run takes 60 to {MAX_RUN_S} seconds"
        )));
    }
    let aterm = std::fs::canonicalize(aterm).map_err(fail)?;
    let (world, role) = prepare_run(root, live)?;
    let run = world.dir.clone();
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    let launch = Launch {
        aterm,
        headless: false,
        args: vec!["-e".into(), "/bin/sh".into()],
        env,
        cwd: world.home.clone(),
        sandbox: None,
        reroute: false,
    };
    let mut instance = Instance::start_ordinary(&world, launch).map_err(fail)?;
    instance.await_socket(BOOT)?;
    let pid = instance
        .aterm_pid()
        .ok_or_else(|| Fail::die("ordinary host has no pid"))?;
    // The host is ordinary only as its own responsible process, as `start_ordinary` promises:
    // one charged to its starter would be judged by the starter's privacy grants (files, the
    // Keychain), not its own, as no person's launch of the app is.
    let responsible_pid = crate::sys::responsible_pid(pid)
        .ok_or_else(|| Fail::die("cannot read the ordinary host's privacy responsibility"))?;
    if responsible_pid != pid {
        return Err(Fail::die(format!(
            "ordinary host {pid} is charged to process {responsible_pid}, not itself"
        )));
    }
    if follows == Some(pid as u32) {
        return Err(Fail::die("a follower must name another process"));
    }
    let started = json!({"phase":"started","role":role,"run":run.display().to_string(),
        "aterm_pid":pid,"keeper_pid":instance.pid,"responsible_pid":responsible_pid,
        "follows_pid":follows,
        "log":instance.log.display().to_string(),"socket":instance.sock.display().to_string()});
    write_new(
        &run.join("started.json"),
        &aterm_json::to_vec(&started).map_err(fail)?,
    )?;
    emit(&started)?;
    let config = world.cfg.join("aterm/aterm.toml");
    let hang = Instant::now() + HANG;
    loop {
        stop::check()?;
        if !instance.running() {
            return Err(Fail::die("ordinary host exited during startup"));
        }
        let admitted = observed_admission(&Paths::for_home(&world.home), hang);
        if admitted
            && let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(follows.unwrap_or(pid as u32))
        {
            let active = json!({"phase":"active","role":role,"run":run.display().to_string(),
                "aterm_pid":pid,"follows_pid":follows,"commons_admitted":true,
                "network":aterm_db::network_state::value(&observation)});
            write_new(
                &run.join("active.json"),
                &aterm_json::to_vec(&active).map_err(fail)?,
            )?;
            emit(&active)?;
            if let Some(ready) = ready.take() {
                ready
                    .send((pid as u32, run.clone()))
                    .map_err(|_| Fail::die("paired host stopped waiting for its owner"))?;
            }
            break;
        }
        if Instant::now() >= hang {
            let network = aterm_db::network_state::read_live_observation(&config)
                .map(|observation| aterm_db::network_state::value(&observation))
                .unwrap_or_else(|error| json!({"relay_error":format!("{:?}",error.kind())}));
            let unavailable = json!({"phase":"unavailable","role":role,
                "run":run.display().to_string(),"aterm_pid":pid,
                "commons_admitted":admitted,"network":network});
            write_new(
                &run.join("unavailable.json"),
                &aterm_json::to_vec(&unavailable).map_err(fail)?,
            )?;
            emit(&unavailable)?;
            if let Err(error) = crate::commons_startup::image(&instance, &run, "unavailable.png") {
                eprintln!(
                    "commons-lan: unavailable window image was not saved: {}",
                    error.msg.as_deref().unwrap_or("capture failed")
                );
            }
            return Err(Fail::die(
                "ordinary physical LAN host did not become active",
            ));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    let until = stop::deadline_after(Duration::from_secs(seconds));
    let mut took_over = false;
    while Instant::now() < until && !run.join("stop").try_exists().map_err(fail)? {
        if !instance.running() {
            return Err(Fail::die("ordinary host exited before the requested stop"));
        }
        if follows.is_some()
            && !took_over
            && let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(pid as u32)
        {
            let takeover = json!({"phase":"takeover","role":role,"run":run.display().to_string(),
                "aterm_pid":pid,"follows_pid":follows,
                "network":aterm_db::network_state::value(&observation)});
            write_new(
                &run.join("takeover.json"),
                &aterm_json::to_vec(&takeover).map_err(fail)?,
            )?;
            emit(&takeover)?;
            took_over = true;
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    drop(instance);
    emit(&json!({"phase":"stopped","role":role,"run":run.display().to_string()}))?;
    Ok(0)
}

/// Keep a child of the lab's (its node, a source world's vault server) owned across every
/// reporting and observation error: `SIGKILL`ed by its PID and reaped when it drops.
struct Owned(aterm_uds::spawnfd::Child);

impl Drop for Owned {
    fn drop(&mut self) {
        aterm_uds::deadline::kill_within(&mut self.0, HANG);
    }
}

/// The address a source world serves its lab vault at ([`LAB_SERVE`]), where it serves one.
fn lab_serve_of(dir: &Path) -> Result<Option<std::net::SocketAddrV4>, Fail> {
    match private_file::read_private(dir, LAB_SERVE, 64) {
        Ok(bytes) => std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.trim_end().parse::<std::net::SocketAddrV4>().ok())
            .filter(|addr| addr.ip().is_private() || addr.ip().is_loopback())
            .map(Some)
            .ok_or_else(|| Fail::die("the world's lab vault address is unreadable")),
        Err(private_file::PrivateFileError::Missing) => Ok(None),
        Err(error) => Err(fail(error)),
    }
}

/// The lab port a prepared world's node serves the fleet's `ask` on: the port its `[fleet] listen`
/// names ([`prepare`]), refused unless it is a lab port ([`lab_port`]). A world prepared before
/// the lab fleet port names no listen, and a node holding a fleet key there listens on the live
/// fleet's port, which the live aterm on each Mac holds.
fn fleet_listen_port(world: &World) -> Result<u16, Fail> {
    let config = world.cfg.join("aterm/aterm.toml");
    let text = std::fs::read_to_string(&config).map_err(fail)?;
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&text).map_err(fail)?;
    let listen = doc
        .get("fleet")
        .and_then(|fleet| fleet.get("listen"))
        .and_then(aterm_toml::Value::as_str);
    listen
        .and_then(|listen| listen.parse::<std::net::SocketAddr>().ok())
        .map(|addr| addr.port())
        .filter(|port| lab_port(&port.to_string()).is_ok())
        .ok_or_else(|| {
            Fail::die(format!(
                "{}'s `[fleet] listen` ({listen:?}) names no lab fleet port, so its node would \
                 serve the fleet's `ask` on the live fleet's ({LIVE_FLEET_PORT}): prepare the \
                 world again (`prepare-recipient`/`prepare-source`, `--port`)",
                config.display()
            ))
        })
}

/// Give a prepared world's node its fleet key the shipped way, `aterm fleet init --port` in its
/// environment at its lab port ([`fleet_listen_port`]), where its state holds none yet: the node
/// serves the fleet's `ask` only with one, so a world that holds one, or gets one, has its
/// listen checked first. A world of the live ALab vault ([`LIVE`]) gets none: a keyed node there
/// would sign this Mac's node into the live vault's log, which no lab run does, so it runs keyless
/// as before. What the init wrote goes to `log`; `true` where a key was minted.
fn fleet_key(aterm: &Path, world: &World, log: &Path) -> Result<bool, Fail> {
    let keyed = match std::fs::symlink_metadata(world.state().join(aterm_net::fleet::KEY_FILE)) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(fail(error)),
    };
    let live = client::config(&Paths::for_home(&world.home))
        .map_err(fail)?
        .as_ref()
        .is_some_and(client::alab_vault);
    if live && !keyed {
        return Ok(false);
    }
    let port = fleet_listen_port(world)?;
    if keyed {
        return Ok(false);
    }
    let output = world_output(
        aterm,
        world,
        "fleet",
        &["init".to_owned(), "--port".to_owned(), port.to_string()],
        aterm_uds::spawnfd::Stdio::null(),
    )?;
    let mut file = crate::util::create_new_regular_file(log, 0o600).map_err(fail)?;
    file.write_all(&output.stdout).map_err(fail)?;
    file.write_all(&output.stderr).map_err(fail)?;
    if !output.status.success() {
        return Err(Fail::die(format!(
            "`aterm fleet init` gave this world no fleet key ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(true)
}

/// Serve a source world's lab vault (its [`LAB_SERVE`] address, its [`SERVER_STORE`] store) with
/// the shipped `aterm secrets serve`, advertising nothing and holding no keep-awake, as an owned
/// child logging to `log`, once it accepts a connection within the hang detector.
fn vault_server(
    aterm: &Path,
    world: &World,
    (listen, store): (std::net::SocketAddrV4, &Path),
    env: Vec<(String, String)>,
    log: &Path,
) -> Result<Owned, Fail> {
    let out = crate::util::create_new_regular_file(log, 0o600).map_err(fail)?;
    let err = out.try_clone().map_err(fail)?;
    let mut owned = Owned(
        aterm_uds::spawnfd::command(aterm)
            .env_clear()
            .envs(env)
            .current_dir(&world.home)
            .args([
                "secrets",
                "serve",
                "--listen",
                listen.to_string().as_str(),
                "--store",
            ])
            .arg(store)
            .args(["--no-advertise", "--no-keep-awake"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(fail)?,
    );
    let addr = std::net::SocketAddr::V4(listen);
    let hang = Instant::now() + HANG;
    while std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err() {
        if owned.0.try_wait().map_err(fail)?.is_some() || Instant::now() >= hang {
            return Err(Fail::die(format!(
                "the lab vault server did not listen on {listen} (see {})",
                log.display()
            )));
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    Ok(owned)
}

/// Run the shipped headless node service (`aterm node serve`) in a prepared profile until
/// `seconds` pass or the run's `stop` marker appears, as the 2026-11-04 three-Mac runs did. Unlike
/// [`run`] it starts no window: the service is this command's child, and its LAN use goes
/// through the LAN broker (docs/DESIGN-lan-broker-2026-11-05.md), so no Local Network grant of
/// any identity decides whether it reaches a peer. Each phase is one JSON line and a file in the
/// run's folder: `started`, `active` once the profile is admitted and its own observer is
/// Active, then `stopped` with the service's exit, after `SIGTERM` and, past the one-minute hang
/// detector, `SIGKILL`.
/// Once a second it appends the live network observation to `network.jsonl`.
///
/// A world with no fleet key gets one first at its lab port, and one whose `[fleet] listen` names
/// no lab port is refused before its node starts, keyed or not, but for a keyless world of the
/// live ALab vault, which gets no key and so listens nowhere ([`fleet_key`]). A source world that
/// serves its lab vault ([`LAB_SERVE`]) runs its vault server beside the node ([`vault_server`]),
/// stopped after it.
fn node(aterm: &Path, root: &Path, seconds: u64, live: bool) -> Res {
    if !(60..=MAX_RUN_S).contains(&seconds) {
        return Err(Fail::usage(format!(
            "commons-lan node takes 60 to {MAX_RUN_S} seconds"
        )));
    }
    let aterm = std::fs::canonicalize(aterm).map_err(fail)?;
    let (world, role) = prepare_run(root, live)?;
    let run = world.dir.clone();
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    let prepared = run
        .parent()
        .ok_or_else(|| Fail::die("the run has no world"))?
        .to_path_buf();
    let keyed = fleet_key(&aterm, &world, &run.join("fleet-init.log"))?;
    let server = match lab_serve_of(&prepared)? {
        Some(listen) => Some(vault_server(
            &aterm,
            &world,
            (listen, &prepared.join(SERVER_STORE)),
            env.clone(),
            &run.join("vault-server.log"),
        )?),
        None => None,
    };
    let server_pid = server.as_ref().map(|server| server.0.id());
    let log = crate::util::create_new_regular_file(&run.join("node.log"), 0o600).map_err(fail)?;
    let err = log.try_clone().map_err(fail)?;
    let mut owned = Owned(
        aterm_uds::spawnfd::command(&aterm)
            .env_clear()
            .envs(env)
            .current_dir(&world.home)
            .args(["node", "serve"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(fail)?,
    );
    let child = &mut owned.0;
    let pid = child.id();
    let phase = |name: &str, value: Value| -> Result<(), Fail> {
        write_new(
            &run.join(format!("{name}.json")),
            &aterm_json::to_vec(&value).map_err(fail)?,
        )?;
        emit(&value)
    };
    let stopped = |child: &mut aterm_uds::spawnfd::Child| -> Result<Value, Fail> {
        if child.try_wait().map_err(fail)?.is_none() {
            child.terminate().map_err(fail)?;
            let hang = Instant::now() + HANG;
            while child.try_wait().map_err(fail)?.is_none() && Instant::now() < hang {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let status = match child.try_wait().map_err(fail)? {
            Some(status) => status.to_string(),
            None => {
                aterm_uds::deadline::kill_within(child, HANG);
                "killed after the hang detector".into()
            }
        };
        Ok(
            json!({"phase":"stopped","role":role,"run":run.display().to_string(),
            "node_pid":pid,"exit":status}),
        )
    };
    phase(
        "started",
        json!({"phase":"started","role":role,"run":run.display().to_string(),"node_pid":pid,
            "fleet_key_minted":keyed,"vault_server_pid":server_pid,
            "log":run.join("node.log").display().to_string()}),
    )?;
    let config = world.cfg.join("aterm/aterm.toml");
    let mut network =
        crate::util::create_new_regular_file(&run.join("network.jsonl"), 0o600).map_err(fail)?;
    let mut sampled = Instant::now();
    let mut sample = || -> Result<(), Fail> {
        if Instant::now() < sampled {
            return Ok(());
        }
        sampled += Duration::from_secs(1);
        let value = aterm_db::network_state::read_live_observation(&config)
            .map(|observation| aterm_db::network_state::value(&observation))
            .unwrap_or_else(|error| json!({"relay_error":format!("{:?}",error.kind())}));
        let line = json!({"at_ms":aterm_time::unix_millis(),"network":value});
        writeln!(network, "{}", aterm_json::to_string(&line).map_err(fail)?).map_err(fail)
    };
    let hang = Instant::now() + HANG;
    loop {
        sample()?;
        if let Err(error) = stop::check() {
            emit(&stopped(child)?)?;
            return Err(error.into());
        }
        if child.try_wait().map_err(fail)?.is_some() {
            emit(&stopped(child)?)?;
            return Err(Fail::die("the node service exited during startup"));
        }
        let admitted = observed_admission(&Paths::for_home(&world.home), hang);
        if admitted
            && let Ok(observation) = aterm_db::network_state::read_live_observation(&config)
            && observation.visibility == Visibility::Active
            && observation.via_local_process == Some(pid)
        {
            phase(
                "active",
                json!({"phase":"active","role":role,"run":run.display().to_string(),
                    "node_pid":pid,"commons_admitted":true,
                    "network":aterm_db::network_state::value(&observation)}),
            )?;
            break;
        }
        if Instant::now() >= hang {
            let network = aterm_db::network_state::read_live_observation(&config)
                .map(|observation| aterm_db::network_state::value(&observation))
                .unwrap_or_else(|error| json!({"relay_error":format!("{:?}",error.kind())}));
            phase(
                "unavailable",
                json!({"phase":"unavailable","role":role,"run":run.display().to_string(),
                    "node_pid":pid,"commons_admitted":admitted,"network":network}),
            )?;
            emit(&stopped(child)?)?;
            return Err(Fail::die("the node service did not become active"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until
        && !run.join("stop").try_exists().map_err(fail)?
        && stop::check().is_ok()
        && child.try_wait().map_err(fail)?.is_none()
    {
        sample()?;
        std::thread::sleep(Duration::from_millis(100));
    }
    let exited_early = child.try_wait().map_err(fail)?.is_some();
    emit(&stopped(child)?)?;
    drop(server);
    if exited_early {
        return Err(Fail::die(
            "the node service exited before the requested stop",
        ));
    }
    Ok(0)
}

/// Start the owner and its local follower under one process's quiet reservation.
/// Independent lab commands reserve the machine separately and cannot overlap.
fn pair(aterm: &Path, root: &Path, seconds: u64, live: bool) -> Res {
    std::thread::scope(|scope| {
        let (ready, owner_ready) = std::sync::mpsc::sync_channel(1);
        let owner = scope.spawn(|| run(aterm, root, seconds, None, live, Some(ready)));
        let joined = |result: std::thread::Result<Res>| {
            result.map_err(|_| Fail::die("paired Commons owner panicked"))?
        };
        let Ok((pid, owner_run)) = owner_ready.recv() else {
            return joined(owner.join());
        };
        let followed = run(aterm, root, seconds, Some(pid), live, None);
        let stop = if owner.is_finished() || owner_run.join("stop").try_exists().map_err(fail)? {
            Ok(())
        } else {
            request_stop(&owner_run)
        };
        let original = joined(owner.join());
        followed?;
        stop?;
        original
    })
}

fn request_stop(run: &Path) -> Result<(), Fail> {
    let marker = private_file::read_private(run, "lab-run", 64).map_err(fail)?;
    if marker.as_slice() != b"commons-lan-run/1\n" {
        return Err(Fail::die("not an owned Commons LAN run"));
    }
    write_new(&run.join("stop"), b"")
}

fn table_snapshot(world: &World) -> Result<Value, Fail> {
    let paths = Paths::for_home(&world.home);
    let table = Table::application(&paths, APPLICATION, "items")?;
    let (rows, known) = table.observe(&paths, &world.cfg.join("aterm/aterm.toml"))?;
    if known
        != rows
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
    {
        return Err(Fail::die(
            "application inventory contains unavailable or ineligible rows",
        ));
    }
    let inventory = InventorySummary::of(&known);
    Ok(
        json!({"table":table.id(),"known":inventory.count,"eligible_decrypted":rows.len(),
        "inventory_sha256":hex::encode_lower(&inventory.fingerprint),
        "reader_declared_or_joined_table":false,
        "operations":rows.iter().map(|(id,value)|json!({"operation":hex::encode_lower(id),"value":value})).collect::<Vec<_>>()}),
    )
}

/// Whether `commons-lan db` passes `args` to the shipped database CLI: inert proposal and data
/// commands, the lab model's import and check, and the owner's emails view. No credential
/// activation, membership or service command passes, and `emails --show-passwords` reaches a
/// process with no terminal, which the view refuses (one-mesh design §4.8).
fn admitted_db(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some(
            "nonce" | "commons-status" | "commons-tables" | "commons-inventory" | "accounts-init"
            | "accounts-write" | "accounts-get" | "accounts-scan" | "accounts-history" | "emails",
        ) => true,
        Some("alab") => matches!(args.get(1).map(String::as_str), Some("import" | "check")),
        _ => false,
    }
}

/// Run the shipped database CLI against a prepared profile, including while its
/// app is stopped, with `stdin` as its standard input; what it wrote.
fn db_output(
    aterm: &Path,
    world: &World,
    args: &[String],
    stdin: aterm_uds::spawnfd::Stdio,
) -> Result<std::process::Output, Fail> {
    world_output(aterm, world, "db", args, stdin)
}

/// Run the shipped `aterm <noun> <args>` in a prepared profile's environment, with `stdin` as its
/// standard input; what it wrote.
fn world_output(
    aterm: &Path,
    world: &World,
    noun: &str,
    args: &[String],
    stdin: aterm_uds::spawnfd::Stdio,
) -> Result<std::process::Output, Fail> {
    let mut env = world.env();
    env.retain(|(key, _)| key != "ATERM_STATE_HOME" && key != "CI");
    let mut command = aterm_uds::spawnfd::command(std::fs::canonicalize(aterm).map_err(fail)?);
    command
        .env_clear()
        .envs(env)
        .current_dir(&world.home)
        .arg(noun)
        .args(args);
    aterm_uds::deadline::output_until_from(&mut command, stdin, Instant::now() + HANG).map_err(fail)
}

/// The shape of the shipped vault verbs a lab's own membership takes ([`vault_cli`]): an admin's
/// approval of one `age1…` key under a machine name, a join of a lab profile's own vault file,
/// pinned to its signer as `<file>#<signer>` or not, a join of a URL-served lab vault pinned to
/// its signer (`<url>#<signer>`, a lab URL [`lab_url`] takes: never the live ALab vault's, nor
/// any URL that is no lab's), and `status`. Which lab vault a URL join may name is the world's
/// to say: [`vault_cli`] passes only the one it follows ([`LAB_VAULT`]).
fn admitted_secrets(args: &[String]) -> bool {
    match args {
        [verb] => verb == "status",
        [verb, vault] if vault.starts_with("http://") => {
            verb == "join"
                && vault
                    .split_once('#')
                    .is_some_and(|(url, signer)| lab_url(url) && signer_word(signer))
        }
        [verb, vault] => {
            let (file, signer) = vault.split_once('#').unwrap_or((vault, ""));
            verb == "join"
                && Path::new(file).file_name() == Some(std::ffi::OsStr::new("fixture.vault"))
                && Path::new(file).is_absolute()
                && std::fs::symlink_metadata(file).is_ok_and(|meta| meta.is_file())
                && signer
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        }
        [verb, key, flag, _] => verb == "approve" && key.starts_with("age1") && flag == "--name",
        _ => false,
    }
}

/// Run the shipped vault CLI in a prepared profile: only what [`admitted_secrets`] names. A URL
/// join is checked for its shape before the world is opened, then against the lab vault the world
/// follows ([`lab_vault_of`]): a world that follows another lab vault, or none, joins no URL.
fn vault_cli(aterm: &Path, root: &Path, args: &[String], live: bool) -> Res {
    let refused = || {
        Fail::usage(
            "commons-lan secrets accepts `approve <age1…> --name <host>`, `join <lab \
             profile>/fixture.vault`, `join <its-lab-url>#<signer>` of the lab vault the world \
             follows and `status` only",
        )
    };
    if !admitted_secrets(args) {
        return Err(refused());
    }
    let (world, _) = open(root, live)?;
    if let [_, vault] = args
        && vault.starts_with("http://")
        && lab_vault_of(&world)?.as_deref() != Some(vault.as_str())
    {
        return Err(refused());
    }
    let output = world_output(
        aterm,
        &world,
        "secrets",
        args,
        aterm_uds::spawnfd::Stdio::null(),
    )?;
    std::io::stdout().write_all(&output.stdout).map_err(fail)?;
    std::io::stderr().write_all(&output.stderr).map_err(fail)?;
    Ok(output.status.code().unwrap_or(1))
}

/// Whether `at` names a lab member's node for `--at`: `<IPv4>:<port>`, a private or loopback
/// address and a lab port ([`lab_port`]: never the live fleet's).
fn lab_member(at: &str) -> bool {
    at.parse::<std::net::SocketAddrV4>().is_ok_and(|addr| {
        (addr.ip().is_private() || addr.ip().is_loopback())
            && lab_port(&addr.port().to_string()).is_ok()
    })
}

/// Whether `commons-lan members` passes `args` to the shipped `aterm members` ([`members_cli`]):
/// a phraseless `join` naming one to [`MAX_RECIPIENTS`] lab members' nodes, each as
/// `--at <IPv4>:<port>` ([`lab_member`]), and nothing else: no phrase, no other flag, no other
/// verb, never the live fleet's port.
fn admitted_members(args: &[String]) -> bool {
    let Some((verb, ats)) = args.split_first() else {
        return false;
    };
    verb == "join"
        && !ats.is_empty()
        && ats.len() % 2 == 0
        && ats.len() <= 2 * MAX_RECIPIENTS
        && ats
            .chunks(2)
            .all(|pair| pair[0] == "--at" && lab_member(&pair[1]))
}

/// Run the shipped `aterm members join --at …` in a prepared world pinned to its URL-served lab
/// vault ([`pin`], [`LAB_VAULT`]): with no phrase it proves this Mac to the members named, and a
/// member's desk on the lab's home LAN lets it in by that proof. Only what [`admitted_members`]
/// names passes, and never in a world of the live ALab vault, whatever flag is given.
fn members_cli(aterm: &Path, root: &Path, args: &[String]) -> Res {
    if !admitted_members(args) {
        return Err(Fail::usage(
            "commons-lan members accepts `join --at <ip:port>...` only: lab members' private \
             IPv4 addresses and lab ports, no phrase, no other flag or verb",
        ));
    }
    let (world, _) = open(root, false)?;
    let pinned = lab_vault_of(&world)?;
    let follows = client::config(&Paths::for_home(&world.home))
        .map_err(fail)?
        .is_some_and(|config| match &config.source {
            aterm_secrets::transport::Source::Url(url) => {
                lab_url(url)
                    && pinned
                        .as_deref()
                        .and_then(|pinned| pinned.split_once('#'))
                        .is_some_and(|(pinned, _)| pinned == url)
            }
            aterm_secrets::transport::Source::Path(_) => false,
        });
    if !follows {
        return Err(Fail::die(
            "members join needs a world pinned to its URL-served lab vault (`commons-lan pin`)",
        ));
    }
    let output = world_output(
        aterm,
        &world,
        "members",
        args,
        aterm_uds::spawnfd::Stdio::null(),
    )?;
    std::io::stdout().write_all(&output.stdout).map_err(fail)?;
    std::io::stderr().write_all(&output.stderr).map_err(fail)?;
    Ok(output.status.code().unwrap_or(1))
}

/// Run the shipped database CLI against a prepared profile, including while its
/// app is stopped. Only the commands [`admitted_db`] names pass through this entry point.
fn database(aterm: &Path, root: &Path, args: &[String], live: bool) -> Res {
    if !admitted_db(args) {
        return Err(Fail::usage(
            "commons-lan db accepts proposal/data commands, `alab import|check` and `emails` only",
        ));
    }
    let (world, _) = open(root, live)?;
    let output = db_output(aterm, &world, args, aterm_uds::spawnfd::Stdio::null())?;
    std::io::stdout().write_all(&output.stdout).map_err(fail)?;
    std::io::stderr().write_all(&output.stderr).map_err(fail)?;
    Ok(output.status.code().unwrap_or(1))
}

/// A `prepare-…` verb's leading flags, `--serve <ip:port>` and `--port <lab-fleet-port>` in either
/// order, each at most once, and the words after them. The fleet port defaults to
/// [`LAB_FLEET_PORT`]; a served vault's port is never the fleet port the world listens on.
fn prepare_flags(words: &[String]) -> Result<(Option<std::net::SocketAddr>, u16, &[String]), Fail> {
    let (mut serve, mut port, mut rest) = (None, None, words);
    loop {
        match rest {
            [flag, value, tail @ ..] if flag == "--serve" && serve.is_none() => {
                serve = Some(value.parse::<std::net::SocketAddr>().map_err(|_| {
                    Fail::usage(format!("--serve takes an IPv4 address and port: {value:?}"))
                })?);
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--port" && port.is_none() => {
                port = Some(lab_port(value)?);
                rest = tail;
            }
            [flag, ..] if flag.starts_with("--") => return Err(Fail::usage(USAGE)),
            _ => break,
        }
    }
    let port = port.unwrap_or(LAB_FLEET_PORT);
    if serve.is_some_and(|serve| serve.port() == port) {
        return Err(Fail::usage(format!(
            "--serve and the fleet port name one port, {port}: the vault server and the node \
             each need their own"
        )));
    }
    Ok((serve, port, rest))
}

/// Prepare and drive preserved private profiles without changing installed state.
pub fn cmd(args: &[String]) -> Res {
    stop::install();
    let live = args.iter().any(|arg| arg == LIVE);
    let args: Vec<String> = args.iter().filter(|arg| *arg != LIVE).cloned().collect();
    let value = match args.as_slice() {
        [verb, root, flags @ ..] if verb == "prepare-recipient" => {
            let (serve, port, rest) = prepare_flags(flags)?;
            if serve.is_some() || !rest.is_empty() {
                return Err(Fail::usage(USAGE));
            }
            prepare_recipient(Path::new(root), port)?
        }
        [verb, root, flags @ ..] if verb == "prepare-source" => {
            let (serve, port, recipients) = prepare_flags(flags)?;
            prepare_source(Path::new(root), recipients, serve, port)?
        }
        [verb, root, capsule] if verb == "join" => {
            let bytes = aterm_secrets::fsx::read_private(Path::new(capsule), CAPSULE_LIMIT)
                .map_err(fail)?
                .ok_or_else(|| Fail::die("synthetic vault capsule file is missing"))?;
            join_recipient(Path::new(root), bytes.as_slice())?
        }
        [verb, root, card] if verb == "pin" => {
            let bytes = aterm_secrets::fsx::read_private(Path::new(card), CAPSULE_LIMIT)
                .map_err(fail)?
                .ok_or_else(|| Fail::die("the lab vault card file is missing"))?;
            pin(Path::new(root), bytes.as_slice())?
        }
        #[cfg(debug_assertions)]
        [verb, root, cidr] if verb == "home-lan" => home_lan(Path::new(root), cidr)?,
        #[cfg(not(debug_assertions))]
        [verb, ..] if verb == "home-lan" => {
            return Err(Fail::die(
                "home-lan needs a debug lab image: a release build's desk reads no lab home",
            ));
        }
        [verb, aterm, root, rest @ ..] if verb == "db" => {
            return database(Path::new(aterm), Path::new(root), rest, live);
        }
        [verb, aterm, root, rest @ ..] if verb == "secrets" => {
            return vault_cli(Path::new(aterm), Path::new(root), rest, live);
        }
        [verb, aterm, root, rest @ ..] if verb == "members" => {
            return members_cli(Path::new(aterm), Path::new(root), rest);
        }
        [verb, aterm, root, seconds] if verb == "run" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("run needs a whole number of seconds"));
            }
            return run(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                None,
                live,
                None,
            );
        }
        [verb, aterm, root, seconds] if verb == "node" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("node needs a whole number of seconds"));
            }
            return node(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                live,
            );
        }
        [verb, aterm, root, seconds] if verb == "pair" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("pair needs a whole number of seconds"));
            }
            return pair(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                live,
            );
        }
        [verb, aterm, root, seconds, owner] if verb == "follow" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit())
                || !owner.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Fail::usage("follow needs whole seconds and an owner pid"));
            }
            let owner: u32 = owner.parse().map_err(fail)?;
            if owner == 0 {
                return Err(Fail::usage("follow needs a nonzero owner pid"));
            }
            return run(
                Path::new(aterm),
                Path::new(root),
                seconds.parse().map_err(fail)?,
                Some(owner),
                live,
                None,
            );
        }
        [verb, run] if verb == "stop" => {
            request_stop(Path::new(run))?;
            json!({"stop_requested":true})
        }
        [verb, root] if verb == "declare" => {
            let (world, role) = open(Path::new(root), live)?;
            if role != "source" {
                return Err(Fail::die(
                    "only the prepared source declares this test table",
                ));
            }
            let config = world.cfg.join("aterm/aterm.toml");
            let observation =
                aterm_db::network_state::read_live_observation(&config).map_err(fail)?;
            if observation.visibility != Visibility::Active {
                return Err(Fail::die("declare requires a running ordinary host"));
            }
            let paths = Paths::for_home(&world.home);
            let table = Table::application(&paths, APPLICATION, "items")?;
            table.publish(&paths)?;
            json!({"table":table.id(),"declared":true})
        }
        [verb, root, key] if verb == "write" => {
            let (world, role) = open(Path::new(root), live)?;
            let paths = Paths::for_home(&world.home);
            let table = Table::application(&paths, APPLICATION, "items")?;
            if role == "recipient" {
                table_snapshot(&world)?;
            }
            let (operation, value) = table.write(&paths, key)?;
            json!({"table":table.id(),"operation":hex::encode_lower(&operation),"value":value})
        }
        [verb, root] if verb == "read" => {
            let (world, _) = open(Path::new(root), live)?;
            table_snapshot(&world)?
        }
        [verb, root, counter, since, until] if verb == "claim" => {
            claims::write(Path::new(root), counter, Some(since.as_str()), until, live)?
        }
        [verb, root, counter, until] if verb == "release" => {
            claims::write(Path::new(root), counter, None, until, live)?
        }
        [verb, root] if verb == "claims" => claims::read(Path::new(root), live)?,
        [verb, root, seed] if verb == "emails-fixture" => {
            let (world, _) = open(Path::new(root), live)?;
            emails::fixture(&world, seed)?
        }
        [verb, aterm, root, seed, row, column, value] if verb == "emails-set" => {
            let (world, _) = open(Path::new(root), live)?;
            emails::set(Path::new(aterm), &world, seed, row, column, value)?
        }
        [verb, aterm, root, digest, seconds] if verb == "emails-wait" => {
            if !seconds.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::usage("emails-wait needs a whole number of seconds"));
            }
            let (world, _) = open(Path::new(root), live)?;
            let (value, code) = emails::wait(
                Path::new(aterm),
                &world,
                digest,
                seconds.parse().map_err(fail)?,
            )?;
            emit(&value)?;
            return Ok(code);
        }
        #[cfg(debug_assertions)]
        [verb, aterm, root, seed, tags @ ..] if verb == "emails-reveal" => {
            let (world, _) = prepare_run(Path::new(root), live)?;
            let (value, code) = emails::reveal(Path::new(aterm), &world, seed, tags)?;
            emit(&value)?;
            return Ok(code);
        }
        #[cfg(not(debug_assertions))]
        [verb, ..] if verb == "emails-reveal" => {
            return Err(Fail::die(
                "emails-reveal needs a debug lab image, as reveal-tab does",
            ));
        }
        _ => return Err(Fail::usage(USAGE)),
    };
    emit(&value)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn failed_node_reporting_still_stops_the_owned_child() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let report = dir.path().join("started.json");
        write_new(&report, b"already published").unwrap();
        let (result, mut input) = {
            let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
            let mut owned = Owned(
                aterm_uds::spawnfd::command(cat)
                    .stdin(aterm_uds::spawnfd::Stdio::piped())
                    .stdout(aterm_uds::spawnfd::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let input = aterm_uds::deadline::cat_echoes(&mut owned.0);
            (write_new(&report, b"replacement"), input)
        };
        assert!(result.is_err(), "the phase publication must fail");
        assert!(
            input.write_all(b"still running?\n").is_err(),
            "the child must be stopped even though its stdin remains held"
        );
        assert_eq!(std::fs::read(&report).unwrap(), b"already published");
    }

    #[test]
    fn database_cli_refuses_authority_and_arbitrary_commands_before_launch() {
        for verb in [
            "accounts-authorize",
            "accounts-intent",
            "accounts-request",
            "resources-reserve",
            "table",
            "init",
            "anything",
        ] {
            let error = database(
                Path::new("missing-executable"),
                Path::new("missing-world"),
                &[verb.into()],
                false,
            )
            .unwrap_err();
            assert_eq!(error.code, 2, "{verb} must be a usage refusal before I/O");
        }
    }

    /// The vault CLI passes an admin's approval of a key, a join of a lab profile's own vault
    /// file, a join of the URL-served lab vault the world follows and `status`, and refuses
    /// before any I/O every other verb, a join of the live ALab's URL or of no file, and a write
    /// of a secret. A URL join of a lab vault passes its shape check, then only in a world that
    /// follows that vault: a world that follows another lab vault, or none, is refused before
    /// anything runs, and the world's own passes to the executable.
    ///
    /// NEGATIVE CONTROL: with `vault_cli`'s check against the vault the world follows removed, the
    /// join of another lab vault reaches the executable:
    /// `http://192.168.1.42:7192/v1/lab-ffff0000#… in …/follows: Err((1, Some("No such file or
    /// directory (os error 2)")))`.
    #[test]
    fn vault_cli_passes_only_a_labs_own_membership_verbs() {
        let parent = crate::instance::world_parent();
        let vault = parent.path().join("fixture.vault");
        std::fs::write(&vault, b"").unwrap();
        let vault = vault.to_string_lossy().into_owned();
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
        let own =
            "http://192.168.1.42:7192/v1/lab-1a2b3c4d#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo=";
        let other =
            "http://192.168.1.42:7192/v1/lab-ffff0000#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo=";
        for passed in [
            words(&["status"]),
            words(&["approve", "age1qqqq", "--name", "m4"]),
            words(&["join", &vault]),
            words(&[
                "join",
                &format!("{vault}#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo="),
            ]),
            words(&["join", own]),
            words(&["join", other]),
        ] {
            assert!(admitted_secrets(&passed), "{passed:?}");
        }
        for refused in [
            words(&["join"]),
            words(&["join", aterm_secrets::ALAB_DEFAULT_URL]),
            words(&["join", &format!("{}#x", aterm_secrets::ALAB_DEFAULT_URL)]),
            words(&[
                "join",
                &format!(
                    "{}#{}",
                    aterm_secrets::ALAB_DEFAULT_URL,
                    aterm_secrets::ALAB_SIGNER
                ),
            ]),
            words(&["join", "http://192.168.1.42:7484/v1/lab-1a2b#k="]),
            words(&["join", "http://192.168.1.42:7100/v1/lab-1a2b#k="]),
            words(&["join", "http://8.8.8.8:7192/v1/lab-1a2b#k="]),
            words(&["join", "http://192.168.1.42:7192/v1/lab-1a2b"]),
            words(&["join", "http://192.168.1.42:7192/v1/lab-1a2b#k y"]),
            words(&["join", &format!("{vault}#x y")]),
            words(&["join", "fixture.vault"]),
            words(&["join", "/no/such/place/fixture.vault"]),
            words(&["approve", "--all"]),
            words(&["approve", "age1qqqq"]),
            words(&["set", "name"]),
            words(&["edit", "name"]),
            words(&["revoke", "m2"]),
        ] {
            assert!(!admitted_secrets(&refused), "{refused:?}");
            let error = vault_cli(
                Path::new("missing-executable"),
                Path::new("missing-world"),
                &refused,
                false,
            )
            .unwrap_err();
            assert_eq!(
                error.code, 2,
                "{refused:?} must be a usage refusal before I/O"
            );
        }
        // Which lab vault a URL join names is the world's to say: one that follows `own` joins
        // it and no other, and one that follows none joins no URL. The world's own reaches the
        // executable, which is missing here, so that run fails, but not as a usage refusal.
        let (follows, unpinned) = (
            parent.path().join("follows"),
            parent.path().join("unpinned"),
        );
        for root in [&follows, &unpinned] {
            prepare_recipient(root, LAB_FLEET_PORT).unwrap();
        }
        write_new(
            &World::open_at(&follows).unwrap().dir.join(LAB_VAULT),
            own.as_bytes(),
        )
        .unwrap();
        let joined = |root: &Path, vault: &str| {
            vault_cli(
                Path::new("missing-executable"),
                root,
                &words(&["join", vault]),
                false,
            )
            .map(|_| ())
            .map_err(|error| (error.code, error.msg))
        };
        for (root, vault) in [(&follows, other), (&unpinned, own)] {
            let refused = joined(root, vault);
            assert!(
                refused.as_ref().is_err_and(|(code, _)| *code == 2),
                "{vault} in {}: {refused:?}",
                root.display()
            );
        }
        let ran = joined(&follows, own);
        assert!(
            ran.as_ref().is_err_and(|(code, _)| *code != 2),
            "CONTROL: the world's own lab vault reaches the executable: {ran:?}"
        );
    }

    /// A lab port is any from 1024 but the live fleet's and the live vault servers', and a lab
    /// URL is `http://` at a private or loopback IPv4 address on a lab port naming a `lab-` vault:
    /// the live ALab vault's URL and every near miss are refused. A `prepare-…` verb's flags take
    /// the lab fleet port by default and never let the served vault share it.
    ///
    /// NEGATIVE CONTROL: with `lab_port`'s live-port exclusion removed, the live fleet's port is
    /// taken: `7100 is refused: Some(7100)`.
    #[test]
    fn a_lab_port_and_url_are_never_the_live_fleets_or_vaults() {
        for port in ["1024", "7190", "7192", "65535"] {
            assert!(lab_port(port).is_ok(), "{port}");
        }
        for port in [
            "7100", "7484", "1023", "80", "0", "", "65536", "+7190", "7190 ", "0x1c16",
        ] {
            let taken = lab_port(port);
            assert!(
                taken.is_err(),
                "{port} is refused: {:?}",
                taken.as_ref().ok()
            );
        }
        for url in [
            "http://192.168.1.42:7192/v1/lab-1a2b3c4d",
            "http://10.0.0.5:7192/v1/lab-e1b",
            "http://127.0.0.1:50000/v1/lab-x",
        ] {
            assert!(lab_url(url), "{url}");
        }
        for url in [
            aterm_secrets::ALAB_DEFAULT_URL,
            "http://alab-secrets.local:7192/v1/lab-1a2b",
            "http://192.168.1.42:7484/v1/lab-1a2b",
            "http://192.168.1.42:7100/v1/lab-1a2b",
            "http://8.8.8.8:7192/v1/lab-1a2b",
            "https://192.168.1.42:7192/v1/lab-1a2b",
            "http://192.168.1.42:7192/v1/alab",
            "http://192.168.1.42:7192/v1/lab-",
            "http://192.168.1.42:7192/v1/lab-a/b",
            "http://192.168.1.42:7192/v1/lab-a#k=",
            "http://[fe80::1]:7192/v1/lab-a",
            "http://192.168.1.42/v1/lab-a",
        ] {
            assert!(!lab_url(url), "{url}");
        }
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
        let keys = words(&["age1a", "age1b"]);
        let (serve, port, rest) = prepare_flags(&keys).unwrap();
        assert_eq!((serve, port, rest.len()), (None, LAB_FLEET_PORT, 2));
        let given = words(&["--port", "7191", "--serve", "192.168.1.42:7192", "age1a"]);
        let (serve, port, rest) = prepare_flags(&given).unwrap();
        assert_eq!(serve, Some("192.168.1.42:7192".parse().unwrap()));
        assert_eq!((port, rest), (7191, &given[4..]));
        for flags in [
            words(&["--serve", "192.168.1.42:7190"]),
            words(&["--port", "7100"]),
            words(&["--port", "7191", "--port", "7192"]),
            words(&["--serve", "m9.local:7192"]),
            words(&["--live", "age1a"]),
        ] {
            assert!(prepare_flags(&flags).is_err(), "{flags:?}");
        }
    }

    /// `commons-lan members` passes a phraseless `join` naming lab members' nodes by
    /// `--at <IPv4>:<port>`, and refuses before any I/O a phrase, the live fleet's or vault's port,
    /// a host name, a public or IPv6 address, any other flag and any other verb.
    ///
    /// NEGATIVE CONTROL: with `lab_port`'s live-port exclusion removed (the check `lab_member`
    /// makes of each port), the live fleet's port passes: `["join", "--at",
    /// "192.168.1.117:7100"]`.
    #[test]
    fn members_cli_passes_only_a_phraseless_join_at_lab_members() {
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
        for passed in [
            words(&["join", "--at", "192.168.1.117:7190"]),
            words(&[
                "join",
                "--at",
                "192.168.1.117:7190",
                "--at",
                "192.168.1.229:7191",
            ]),
            words(&["join", "--at", "127.0.0.1:7190"]),
        ] {
            assert!(admitted_members(&passed), "{passed:?}");
        }
        for refused in [
            words(&["join"]),
            words(&["join", "--at"]),
            words(&["join", "--at", "192.168.1.117:7100"]),
            words(&["join", "--at", "192.168.1.117:7484"]),
            words(&["join", "--at", "192.168.1.117:80"]),
            words(&["join", "--at", "192.168.1.117"]),
            words(&["join", "--at", "m113.local:7190"]),
            words(&["join", "--at", "8.8.8.8:7190"]),
            words(&["join", "--at", "[fe80::1]:7190"]),
            words(&["join", "apple", "banana", "--at", "192.168.1.117:7190"]),
            words(&["join", "--at", "192.168.1.117:7190", "apple"]),
            words(&["join", "--no-wait", "--at", "192.168.1.117:7190"]),
            words(&["join", "--member", "m2", "--at", "192.168.1.117:7190"]),
            words(&["invite"]),
            words(&["allow", "m2"]),
            words(&["remove", "m2"]),
            words(&["status"]),
        ] {
            assert!(!admitted_members(&refused), "{refused:?}");
            let error = members_cli(
                Path::new("missing-executable"),
                Path::new("missing-world"),
                &refused,
            )
            .map(|_| ())
            .err()
            .map_or(0, |error| error.code);
            assert_eq!(error, 2, "{refused:?} must be a usage refusal before I/O");
        }
    }

    /// A vault key no build names for ALab.
    const LAB_SIGNER: &str = "wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo=";

    /// Pin the prepared world at `root` by hand, as `pin` leaves one: its vault read at `url`
    /// pinned to `signer` (written as the file the shipped join writes, so nothing is fetched)
    /// and, where given, [`LAB_VAULT`] naming `follows`.
    fn configure(root: &Path, (url, signer): (&str, &str), follows: Option<&str>) {
        let config = format!(
            "url = \"{url}\"\nname = \"lab-1a2b\"\nsigner = \"{signer}\"\nfollowed = []\nhost = \"m4\"\n"
        );
        write_new(
            &Paths::for_home(&root.join("home")).dir.join("source.toml"),
            config.as_bytes(),
        )
        .unwrap();
        assert!(
            client::config(&Paths::for_home(&root.join("home")))
                .unwrap()
                .is_some_and(|config| matches!(
                    &config.source,
                    aterm_secrets::transport::Source::Url(at) if at == url
                )),
            "PRECONDITION: the pin reads back"
        );
        if let Some(follows) = follows {
            write_new(
                &World::open_at(root).unwrap().dir.join(LAB_VAULT),
                format!("{follows}#{signer}").as_bytes(),
            )
            .unwrap();
        }
    }

    /// `commons-lan members` runs the shipped join only in a world pinned to its URL-served lab
    /// vault: the vault it reads is at a lab URL and [`LAB_VAULT`] names that URL. A world with no
    /// vault, one whose [`LAB_VAULT`] names another lab vault than the one it reads, one that
    /// records none, and one that reads its vault at a URL that is no lab's are refused before
    /// anything runs; the pinned world reaches the executable, which is missing here.
    ///
    /// NEGATIVE CONTROL: with `members_cli`'s `follows` check removed (`if false && !follows`), the
    /// world with no vault reaches the executable: `bare: No such file or directory (os error 2)`.
    #[test]
    fn members_cli_runs_only_in_a_world_pinned_to_its_lab_vault() {
        let parent = crate::instance::world_parent();
        let (lab, other) = (
            "http://127.0.0.1:7192/v1/lab-1a2b",
            "http://127.0.0.1:7192/v1/lab-ffff",
        );
        let not_lab = "http://127.0.0.1:7192/v1/alab";
        let world = |name: &str, url: Option<&str>, follows: Option<&str>| {
            let root = parent.path().join(name);
            prepare_recipient(&root, LAB_FLEET_PORT).unwrap();
            if let Some(url) = url {
                configure(&root, (url, LAB_SIGNER), follows);
            }
            root
        };
        let at = [
            "join".to_owned(),
            "--at".to_owned(),
            "127.0.0.1:7190".to_owned(),
        ];
        let ran = |root: &Path| {
            members_cli(Path::new("missing-executable"), root, &at)
                .map(|_| ())
                .err()
                .and_then(|error| error.msg)
                .unwrap_or_default()
        };
        let unpinned = "members join needs a world pinned to its URL-served lab vault";
        for (name, url, follows) in [
            ("bare", None, None),
            ("crossed", Some(lab), Some(other)),
            ("unrecorded", Some(lab), None),
            ("unlabbed", Some(not_lab), Some(not_lab)),
        ] {
            let refused = ran(&world(name, url, follows));
            assert!(refused.contains(unpinned), "{name}: {refused}");
        }
        let pinned = ran(&world("pinned", Some(lab), Some(lab)));
        assert!(
            !pinned.is_empty() && !pinned.contains(unpinned),
            "CONTROL: the pinned world reaches the executable: {pinned}"
        );
    }

    /// A world's node holds a fleet key only at a lab port: `fleet_key` mints one by `aterm fleet
    /// init --port <the world's lab port>` where its `[fleet] listen` names one, and mints no
    /// second. A world whose listen names the live fleet's port, or none (one prepared before the
    /// lab fleet port), is refused before anything runs, keyed or not. A keyless world of the live
    /// ALab vault gets no key and runs nothing, whatever its listen.
    ///
    /// NEGATIVE CONTROL (each run alone): with `fleet_key`'s port read as [`LAB_FLEET_PORT`] where
    /// `fleet_listen_port` refuses (`.unwrap_or(LAB_FLEET_PORT)`), the world on the live fleet's
    /// port is keyed: `live-port is refused: Ok(true)`. With its live-vault return removed, the
    /// keyless live world is refused for its listen: `a live world is keyless:
    /// Err(Some("…/live/cfg/aterm/aterm.toml's` … `(None) names no lab fleet port, …"))`.
    #[test]
    fn a_lab_node_takes_a_fleet_key_only_at_a_lab_port() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let echo = aterm_uds::deadline::stand_in(Path::new("/bin/echo"));
        let missing = Path::new("missing-executable");
        let parent = crate::instance::world_parent();
        let world = |name: &str, listen: Option<&str>| {
            let root = parent.path().join(name);
            prepare_recipient(&root, 7191).unwrap();
            let world = World::open_at(&root).unwrap();
            let config = world.cfg.join("aterm/aterm.toml");
            let text = std::fs::read_to_string(&config).unwrap();
            let lab = "listen = \"0.0.0.0:7191\"\n";
            assert!(text.contains(lab), "PRECONDITION: {text}");
            std::fs::write(&config, text.replace(lab, listen.unwrap_or(""))).unwrap();
            world
        };
        let key = |world: &World| {
            std::fs::create_dir_all(world.state()).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(world.state().join(aterm_net::fleet::KEY_FILE))
                .unwrap();
        };

        let lab = world("lab", Some("listen = \"0.0.0.0:7191\"\n"));
        let log = parent.path().join("lab-init.log");
        assert_eq!(fleet_key(&echo, &lab, &log).ok(), Some(true), "minted");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "fleet init --port 7191\n",
            "the shipped init at the world's lab port"
        );
        key(&lab);
        let again = fleet_key(missing, &lab, &parent.path().join("again.log"));
        assert_eq!(again.ok(), Some(false), "a keyed world mints no second key");

        for (name, listen, keyed) in [
            ("live-port", Some("listen = \"0.0.0.0:7100\"\n"), false),
            ("old", None, false),
            ("keyed-live-port", Some("listen = \"0.0.0.0:7100\"\n"), true),
        ] {
            let refused_world = world(name, listen);
            if keyed {
                key(&refused_world);
            }
            let log = parent.path().join(format!("{name}.log"));
            let refused = fleet_key(&echo, &refused_world, &log)
                .map_err(|error| error.msg.unwrap_or_default());
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|msg| msg.contains("names no lab fleet port")),
                "{name} is refused: {refused:?}"
            );
            assert!(!log.exists(), "{name}: nothing ran");
        }

        let live = world("live", None);
        configure(
            &parent.path().join("live"),
            (aterm_secrets::ALAB_DEFAULT_URL, aterm_secrets::ALAB_SIGNER),
            None,
        );
        assert!(
            client::config(&Paths::for_home(&live.home))
                .unwrap()
                .as_ref()
                .is_some_and(client::alab_vault),
            "PRECONDITION: a world of the live vault"
        );
        let kept =
            fleet_key(missing, &live, &parent.path().join("live.log")).map_err(|error| error.msg);
        assert_eq!(kept, Ok(false), "a live world is keyless: {kept:?}");
    }

    /// A served vault's own server, again on the address its source world names ([`LAB_SERVE`])
    /// over the store the vault was founded into, as the source world's `node` serves it.
    fn serve_again(source: &Path) -> aterm_secrets::server::Running {
        let listen = lab_serve_of(source)
            .expect("readable")
            .expect("a served source world");
        aterm_secrets::server::start(
            std::net::TcpListener::bind(listen).expect("the vault's address is free again"),
            &aterm_secrets::server::Options {
                listen: std::net::SocketAddr::V4(listen),
                store: source.join(SERVER_STORE),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(100),
                keep_awake: false,
            },
        )
        .expect("served")
    }

    /// A lab vault founded at a URL (`prepare-source --serve`) hands its approved recipients
    /// capsules that carry the URL and no vault bytes; each joins it there pinned to its signer
    /// and records it as the vault it follows. Its card pins a fresh Mac nobody approved to that
    /// same vault, where it waits (Pending); a second pin, and a card that names the live ALab
    /// vault's URL or signer, are refused.
    ///
    /// NEGATIVE CONTROL: with `join_recipient`'s [`LAB_VAULT`] write removed, the recipient
    /// records no vault it follows: `the recipient follows its lab vault: None`.
    #[test]
    fn a_served_lab_vault_lets_its_approved_macs_join_and_pins_one_nobody_approved() {
        let parent = crate::instance::world_parent();
        let (recipient, fresh, source) = (
            parent.path().join("recipient"),
            parent.path().join("fresh"),
            parent.path().join("source"),
        );
        let pending = prepare_recipient(&recipient, LAB_FLEET_PORT).unwrap();
        prepare_recipient(&fresh, LAB_FLEET_PORT + 1).unwrap();
        let prepared = prepare_source(
            &source,
            &[pending["recipient"].as_str().unwrap().into()],
            Some("127.0.0.1:0".parse().unwrap()),
            LAB_FLEET_PORT,
        )
        .unwrap();
        let url = prepared["url"]
            .as_str()
            .expect("a served vault's URL")
            .to_owned();
        assert!(lab_url(&url), "{url}");
        let capsule = &prepared["capsules"][0];
        assert_eq!(capsule["url"].as_str(), Some(url.as_str()));
        assert!(
            capsule["vault_hex"].is_null(),
            "a served capsule carries no vault bytes"
        );
        let signer = capsule["signer"].as_str().unwrap().to_owned();
        let pinned = format!("{url}#{signer}");
        assert_eq!(
            lab_vault_of(&World::open_at(&source).unwrap())
                .unwrap()
                .as_deref(),
            Some(pinned.as_str())
        );
        let card = std::fs::read(source.join(LAB_CARD)).unwrap();
        assert_eq!(
            aterm_json::from_slice::<Value>(&card).unwrap(),
            prepared["card"]
        );

        let server = serve_again(&source);
        let joined = join_recipient(&recipient, &aterm_json::to_vec(capsule).unwrap()).unwrap();
        assert_eq!(joined["served"], json!(true));
        assert_eq!(joined["commons_admitted"], json!(false));
        let follows = lab_vault_of(&World::open_at(&recipient).unwrap()).unwrap();
        assert_eq!(
            follows.as_deref(),
            Some(pinned.as_str()),
            "the recipient follows its lab vault: {follows:?}"
        );
        let config = client::config(&Paths::for_home(&recipient.join("home")))
            .unwrap()
            .expect("configured");
        assert!(
            matches!(&config.source, aterm_secrets::transport::Source::Url(at) if *at == url),
            "{:?}",
            config.source
        );

        let waits = pin(&fresh, &card).unwrap();
        assert_eq!(waits["membership"], json!("pending"), "{waits:?}");
        assert_eq!(
            lab_vault_of(&World::open_at(&fresh).unwrap())
                .unwrap()
                .as_deref(),
            Some(pinned.as_str())
        );
        let config = client::config(&Paths::for_home(&fresh.join("home")))
            .unwrap()
            .expect("pinned");
        assert!(!client::alab_vault(&config));
        assert_eq!(
            config
                .signer
                .map(|key| aterm_secrets::b64::encode_padded(&key)),
            Some(signer.clone())
        );
        assert!(
            pin(&fresh, &card).is_err(),
            "a pinned world takes no second pin"
        );
        for (n, (url, signer)) in [
            (aterm_secrets::ALAB_DEFAULT_URL, signer.as_str()),
            (url.as_str(), aterm_secrets::ALAB_SIGNER),
        ]
        .into_iter()
        .enumerate()
        {
            let other = parent.path().join(format!("other-{n}"));
            prepare_recipient(&other, LAB_FLEET_PORT).unwrap();
            let card = aterm_json::to_vec(&json!({"schema":1,"url":url,"signer":signer})).unwrap();
            assert!(pin(&other, &card).is_err(), "{url}#{signer}");
            assert!(
                client::config(&Paths::for_home(&other.join("home")))
                    .unwrap()
                    .is_none(),
                "a refused card configures nothing"
            );
        }
        server.stop();
    }

    /// `home-lan` names a lab's home LAN only on a world that follows its URL-served lab vault
    /// with a marked identity, and only a network this Mac's default route is on now: a network
    /// the route is not on, a world whose vault is a file, one whose identity a release build
    /// made, and the live ALab vault's world are refused, and none of them gets the file.
    ///
    /// NEGATIVE CONTROL: with `home_lan`'s lab-mark refusal removed, a release identity's world
    /// gets a home: `a release identity's world is refused: Some(Object({"home_lan":
    /// String("192.168.1.0/24"), "own": String("192.168.1.20"), …}))`.
    #[test]
    fn home_lan_names_only_a_network_this_mac_is_on_for_a_marked_served_lab_world() {
        use aterm_netplace::facts::{parse_script, script_here};
        use aterm_secrets::members::lab_lan;
        let at_home = || {
            parse_script(
                "route en0 192.168.1.20/24 via 192.168.1.1 hw aa:bb:cc:dd:ee:01\npath usable wifi\n",
            )
        };
        let parent = crate::instance::world_parent();
        let (member, file, release, source) = (
            parent.path().join("member"),
            parent.path().join("file"),
            parent.path().join("release"),
            parent.path().join("source"),
        );
        let pending = prepare_recipient(&member, LAB_FLEET_PORT).unwrap();
        let prepared = prepare_source(
            &source,
            &[pending["recipient"].as_str().unwrap().into()],
            Some("127.0.0.1:0".parse().unwrap()),
            LAB_FLEET_PORT,
        )
        .unwrap();
        let server = serve_again(&source);
        join_recipient(
            &member,
            &aterm_json::to_vec(&prepared["capsules"][0]).unwrap(),
        )
        .unwrap();
        let unmarked = prepare(&release, "recipient", LAB_FLEET_PORT).unwrap();
        let unmarked = Paths::for_home(&unmarked.home);
        client::machine_identity_as(&unmarked, aterm_secrets::Making::default()).unwrap();
        assert!(
            client::lab_mark(&unmarked).unwrap().is_none(),
            "PRECONDITION"
        );
        pin(&release, &std::fs::read(source.join(LAB_CARD)).unwrap()).unwrap();
        server.stop();
        let filed = prepare_recipient(&file, LAB_FLEET_PORT).unwrap();
        let file_source = parent.path().join("file-source");
        let file_vault = prepare_source(
            &file_source,
            &[filed["recipient"].as_str().unwrap().into()],
            None,
            LAB_FLEET_PORT,
        )
        .unwrap();
        join_recipient(
            &file,
            &aterm_json::to_vec(&file_vault["capsules"][0]).unwrap(),
        )
        .unwrap();
        let home_of =
            |root: &Path| lab_lan::read(&Paths::for_home(&root.join("home"))).expect("readable");

        let outside = script_here(at_home(), || home_lan(&member, "192.168.2.0/24"));
        assert!(
            outside.is_err(),
            "a network the route is not on: {:?}",
            outside.ok()
        );
        assert_eq!(home_of(&member), None, "nothing written");
        let filed = script_here(at_home(), || home_lan(&file, "192.168.1.0/24"));
        assert!(
            filed.is_err(),
            "a file vault's world is refused: {:?}",
            filed.ok()
        );
        assert_eq!(home_of(&file), None);
        let released = script_here(at_home(), || home_lan(&release, "192.168.1.0/24"));
        assert!(
            released.is_err(),
            "a release identity's world is refused: {:?}",
            released.ok()
        );
        assert_eq!(home_of(&release), None);
        let named = script_here(at_home(), || home_lan(&member, "192.168.1.0/24")).unwrap();
        assert_eq!(named["home_lan"], json!("192.168.1.0/24"));
        assert_eq!(named["own"], json!("192.168.1.20"));
        assert_eq!(
            home_of(&member),
            Some(lab_lan::Cidr::parse("192.168.1.0/24").unwrap())
        );
    }

    #[test]
    fn successive_launches_share_the_profile_without_reusing_app_logs() {
        let parent = crate::instance::world_parent();
        let root = parent.path().join("recipient");
        prepare_recipient(&root, LAB_FLEET_PORT).unwrap();
        let (profile, _) = open(&root, false).unwrap();
        let retained = root.join("aterm.log");
        std::fs::write(&retained, b"earlier profile output").unwrap();
        let (first, _) = prepare_run(&root, false).unwrap();
        let first_log = first.dir.join("aterm.log");
        write_new(&first_log, b"first app output").unwrap();
        let (second, _) = prepare_run(&root, false).unwrap();
        assert_ne!(first.dir, second.dir);
        assert_eq!(first.home, profile.home);
        assert_eq!(second.home, profile.home);
        assert_eq!(first.cfg, second.cfg);
        assert_eq!(first.run, second.run);
        write_new(&second.dir.join("aterm.log"), b"second app output").unwrap();
        drop(first);
        drop(second);
        assert_eq!(std::fs::read(&first_log).unwrap(), b"first app output");
        assert_eq!(std::fs::read(&retained).unwrap(), b"earlier profile output");
    }

    /// A world pinned to the live default ALab vault (as an earlier lab run left one) opens for no
    /// verb without the flag that names that vault and the member it keeps, while a synthetic
    /// vault's world opens as before. The live pin is written as a file: nothing is fetched.
    /// Negative control: disabling `open`'s live-vault refusal opens the live world without the
    /// flag and fails the refusal's `let`-`else` (run 2026-11-02, then restored).
    #[test]
    fn a_world_on_the_live_vault_opens_only_behind_the_flag() {
        use std::os::unix::fs::OpenOptionsExt as _;
        let parent = crate::instance::world_parent();
        let synthetic = parent.path().join("synthetic");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&synthetic, LAB_FLEET_PORT).unwrap();
        let prepared = prepare_source(
            &source,
            &[pending["recipient"].as_str().unwrap().into()],
            None,
            LAB_FLEET_PORT,
        )
        .unwrap();
        join_recipient(
            &synthetic,
            &aterm_json::to_vec(&prepared["capsules"][0]).unwrap(),
        )
        .unwrap();
        assert!(
            open(&synthetic, false).is_ok(),
            "a synthetic vault's world opens"
        );

        let live = parent.path().join("live");
        prepare_recipient(&live, LAB_FLEET_PORT).unwrap();
        let pin = format!(
            "url = \"{}\"\nname = \"alab\"\nsigner = \"{}\"\nfollowed = []\nhost = \"lab-auto-test\"\n",
            aterm_secrets::ALAB_DEFAULT_URL,
            aterm_secrets::ALAB_SIGNER
        );
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(Paths::for_home(&live.join("home")).dir.join("source.toml"))
            .unwrap()
            .write_all(pin.as_bytes())
            .unwrap();
        let configured = client::config(&Paths::for_home(&live.join("home")))
            .unwrap()
            .expect("the live pin reads back");
        assert!(client::alab_vault(&configured));
        let Err(refused) = open(&live, false) else {
            panic!("a live world needs the flag");
        };
        assert_eq!(refused.code, 1);
        assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
        let refused = database(
            Path::new("missing-executable"),
            &live,
            &[
                "accounts-scan".into(),
                "--collection".into(),
                "accounts".into(),
            ],
            false,
        )
        .unwrap_err();
        assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
        // `home-lan` and `members` open no live world, whatever flag is given.
        let refused = home_lan(&live, "192.168.1.0/24").unwrap_err();
        assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
        let at = [
            "join".to_owned(),
            "--at".to_owned(),
            "192.168.1.117:7190".to_owned(),
        ];
        let refused = members_cli(Path::new("missing-executable"), &live, &at).unwrap_err();
        assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
        assert!(open(&live, true).is_ok());
    }

    #[test]
    fn prepared_recipient_has_no_membership_records_or_application_table() {
        let parent = crate::instance::world_parent();
        let recipient = parent.path().join("recipient");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&recipient, LAB_FLEET_PORT).unwrap();
        let prepared = prepare_source(
            &source,
            &[pending["recipient"].as_str().unwrap().into()],
            None,
            LAB_FLEET_PORT,
        )
        .unwrap();
        let capsule = aterm_json::to_vec(&prepared["capsules"][0]).unwrap();
        let wrong = parent.path().join("wrong-recipient");
        prepare_recipient(&wrong, LAB_FLEET_PORT).unwrap();
        assert!(join_recipient(&wrong, &capsule).is_err());
        assert!(!wrong.join("fixture.vault").exists());
        let joined = join_recipient(&recipient, &capsule).unwrap();
        let source_bootstrap = Bootstrap::required(&Paths::for_home(&source.join("home"))).unwrap();
        let recipient_bootstrap =
            Bootstrap::required(&Paths::for_home(&recipient.join("home"))).unwrap();
        assert_eq!(
            source_bootstrap.trust().network,
            recipient_bootstrap.trust().network
        );
        assert_eq!(joined["commons_admitted"], json!(false));
        assert_eq!(joined["membership_records"], json!(false));
        assert_eq!(joined["application_tables"], json!(0));
        assert!(join_recipient(&recipient, &capsule).is_err());
        assert_eq!(
            recipient_key(&Paths::for_home(&recipient.join("home"))).unwrap(),
            pending["recipient"].as_str().unwrap()
        );
    }

    #[test]
    fn startup_observer_cannot_recreate_missing_custody() {
        let parent = crate::instance::world_parent();
        let recipient = parent.path().join("recipient");
        let source = parent.path().join("source");
        let pending = prepare_recipient(&recipient, LAB_FLEET_PORT).unwrap();
        prepare_source(
            &source,
            &[pending["recipient"].as_str().unwrap().into()],
            None,
            LAB_FLEET_PORT,
        )
        .unwrap();
        let paths = Paths::for_home(&source.join("home"));
        drop(Bootstrap::required(&paths).unwrap());
        assert!(observed_admission(&paths, Instant::now() + HANG));
        let lock = paths.dir.join("commons.lock");
        std::fs::remove_file(&lock).unwrap();

        assert!(!observed_admission(&paths, Instant::now() + HANG));
        assert!(!lock.exists(), "observing startup must not prepare custody");

        // The old probe is the negative control: merely asking it to read the
        // same prepared source recreates the missing custody lock.
        drop(Bootstrap::required(&paths).unwrap());
        assert!(lock.exists());
        assert!(observed_admission(&paths, Instant::now() + HANG));
    }
}
