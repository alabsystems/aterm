// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db`: private and Commons databases, reads, edits and LAN repair.

use crate::{
    Error, Result, crypto,
    database::{Change, Command, decode_hex},
    local::{self, Credentials, Database},
};
use aterm_codec::hex;
use aterm_json::{Value, json};
use std::{
    collections::BTreeMap, ffi::OsString, io::Write, net::SocketAddr, path::Path, process::ExitCode,
};

#[path = "cli_lineage.rs"]
mod lineage;

#[path = "cli_network_place.rs"]
mod network_place;

/// Command help. Mutation request IDs are caller-held, so lost replies are safely retried.
pub const USAGE: &str = "aterm db — encrypted embedded application databases (Unix)\n\
  Commons tables use enrolled secrets and default LAN repair (no --dir):\n\
  table <verb> ...                     typed ATT/1 tables: ls, schema, get, scan, history,\n\
                                       check, revert, export, apply, edit (table --help)\n\
  alab <verb> ...                      the ALab lab model: import, check, adopt (alab --help)\n\
  commons-status                       local membership, control history and sharing setting\n\
  commons-lineage                      verified predecessor-to-current public writer identities\n\
  network-state                        local place, observer and retained history; never grants access\n\
  commons-authority                    inspect cached admin control metadata; changes nothing\n\
  commons-tables                       list declarations, provenance and subscriptions\n\
  commons-inventory --table <digest>    local retained operation counts and fingerprints\n\
                    [--records <1..256> [--after <digest>]]\n\
                                       page operation IDs and eligibility; no values or keys\n\
  commons-custody --table <digest>      inspect local recovery evidence without changing custody\n\
  commons-open --application <id> --role <id> [--version <n>]\n\
                                       open a deterministic offline application table\n\
  commons-create --application <id> --role <id> [--version <n>] [--name <name>]\n\
                                       create and publish an independent table\n\
  accounts-init                       open the shared account/contact proposals table\n\
  accounts-get --collection <name> --key <id>\n\
  accounts-scan --collection <name>    values and explicit concurrent candidates\n\
  accounts-write --changes <json-array> --request <hex>\n\
                                       atomic proposals; null is reversible deletion\n\
  accounts-revert --operation <digest> --request <hex>\n\
  accounts-history                    signed provenance and local receipts\n\
  accounts-check                      diagnose explicit contact links; grants no login rights\n\
  accounts-plan --key <id>             review a bound account/contact proposal; grants no rights\n\
  accounts-activation-init             open the root account-decision history\n\
  accounts-activation-status --account <digest>\n\
  accounts-authorize --account <digest> --key <id> --actor <actor-hex>\n\
        --parents <digest,...|none> --request <hex>\n\
                                      root-sign one reviewed login-preparation decision\n\
  accounts-stop --account <digest> --parents <digest,...|none> --request <hex>\n\
  accounts-intent --account <digest> --key <id> --parents <digest,...|none> --request <hex>\n\
                                      root-authorize automatic preparation on admitted machines\n\
  accounts-prepare --account <digest>  validate current permission and binding; no login\n\
  accounts-billing-propose --allocation <hex> --provider <id> --adapter <id> [--base <file>]\n\
                                      print an untrusted policy and account billing references\n\
  accounts-billing-inspect --file <path>\n\
                                      inspect proposed policy terms without opening a vault\n\
  accounts-billing-prepare --account <digest>\n\
                                      observe current root-pinned billing; no ledger or provider effect\n\
  accounts-broker-propose --account <digest> --grant <hex> --producer <digest> [--seconds <1..3600>] [--base <file>]\n\
                                      print an untrusted grant policy from current billing\n\
  accounts-broker-inspect --file <path> inspect proposed grants offline; grants no rights\n\
  accounts-broker-refresh --grant <hex>\n\
                                      propose current bindings without changing producer identity\n\
  accounts-request (--grant <hex> | --account <digest>) --request <hex> --file <private-json> [--seconds <1..60>]\n\
                                      one Google text request through a root-authorized broker grant\n\
  accounts-preview-csv --file <path>    exact inert candidates, without enrollment or writes\n\
  accounts-compare-csv --file <path>    counts-only differences from retained imports\n\
  accounts-import-csv --file <path> --request <hex>\n\
                                       one atomic, inert CSV candidate document\n\
  accounts-imports                    shared candidates and signed provenance\n\
  resources-propose --terms <json>    prepare an untrusted fixed-allocation proposal\n\
  resources-init --allocation <hex> --pin <digest>\n\
                                      retain an independently trusted allocation pin\n\
  resources-status --allocation <hex> --pin <digest>\n\
                                      locally retained usage; no remote-share reclaim\n\
  resources-reserve --allocation <hex> --pin <digest> --cost <units> --request <hex>\n\
                                      reserve fixed rights; performs no provider request\n\
  resources-attempt --allocation <hex> --pin <digest> --request <hex>\n\
                                      read an attempt; unknown can include a running adapter\n\
  The private database commands below take --dir, except nonce and inspect-csv.\n\
  init [--namespace accounts]           create a complete single-machine database\n\
  request                              create/print this installation's public actor\n\
  invite --actor <actor-hex>            write join.invite privately in this database\n\
  join --invite <file> --trust-root <public-key-hex>\n\
  status                               identity, causal health and budget summaries\n\
  history                              signed author/time/causal metadata and local receipts\n\
  provenance --operation <digest>       provenance for one retained operation\n\
  revert --operation <digest> --request <hex>\n\
                                       compensate a current edit; preserve its audit history\n\
  get --collection <name> --key <id>     whole values, explicit siblings and stops\n\
  scan --collection <name>              all objects in a collection\n\
  put --collection <name> --key <id> --value <json> --request <hex>\n\
  stop --collection <name> --key <id> --request <hex>\n\
  budget-create --name <resource-window> --total <units> [--shares <actor-to-units-json>] --request <hex>\n\
  spend --name <resource-window> --cost <units> --request <hex>\n\
  revoke-writes --actor <actor> [--through <digest>] --request <hex>\n\
  machine-name --key <machine-id> [--name mN] --request <hex>\n\
  machine-history --key <machine-id>    include legacy and superseded assignments\n\
  resolve-name --name <mN>              historical owners and explicit ambiguity\n\
  inspect-csv --file <path>             counts/shapes only, never source values\n\
  import-csv --file <path> --request <hex>\n\
                                       inert proposals; never activate imported credentials\n\
  sync --peer <LAN-ip:port>             repair directly with any admitted peer\n\
  serve --listen <LAN-ip:port> [--peers <ip:port,ip:port>]\n\
  nonce                                generate a public 16-byte request ID\n\
Accounts values: provider, method (oauth|device|api-key|browser|cli|email-link|phone|passkey), optional\n\
email, realm, plan, secret_ref {vault,entry}, machines (preferred-first list),\n\
billing {principal,policy} (root-approved request-count terms; API-key accounts only).\n\
Account records are proposals, not permission to dispatch credentials. Raw secrets\n\
belong in aterm secrets. Budget units are managed admissions, not vendor billing.\n\
One machine works alone. Two synchronize as peers; no third voter is required.\n\
Invitations contain a read key: exchange privately and authenticate the root key.\n";

/// Run the shipped verb; bounded errors never quote payloads or private invitation bytes.
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let words = match args
        .into_iter()
        .map(OsString::into_string)
        .collect::<std::result::Result<Vec<_>, _>>()
    {
        Ok(w) => w,
        Err(_) => {
            return aterm_uds::stdout::answer_exit("aterm db", "arguments must be UTF-8\n", 2);
        }
    };
    if words.first().is_some_and(|w| w == "table") {
        return crate::att::command::main_entry(&words[1..]);
    }
    if words.first().is_some_and(|w| w == "alab") {
        // The `aterm` executable routes `alab` itself, with the harness's sheet reading; this
        // path serves `check` and `adopt`, and `import` names where it runs.
        return crate::alab::command::main_entry(&words[1..], None);
    }
    if words.is_empty() || words.iter().any(|w| w == "--help" || w == "-h") {
        return aterm_uds::stdout::answer_exit("aterm db", USAGE, 0);
    }
    if words[0] == "accounts-request" {
        return crate::accounts::broker::command::main_entry(&words[1..]);
    }
    match run(&words) {
        Ok(value) => match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db", &format!("{text}\n"), 0),
            Err(_) => ExitCode::from(1),
        },
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "aterm db: {error}");
            ExitCode::from(1)
        }
    }
}
/// Run a command for embedding/testing. Every option is consumed exactly once.
pub fn run(words: &[String]) -> Result<Value> {
    let command = words
        .first()
        .map(String::as_str)
        .ok_or_else(|| Error::new("missing database command"))?;
    let mut options = BTreeMap::new();
    let (pairs, remainder) = words[1..].as_chunks::<2>();
    for pair in pairs {
        if !pair[0].starts_with("--")
            || options.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(Error::new("invalid or repeated database option"));
        }
    }
    if !remainder.is_empty() {
        return Err(Error::new("database option needs a value"));
    }
    if matches!(command, "inspect-csv" | "accounts-preview-csv") {
        let file = take(&mut options, "--file")?;
        empty(&options)?;
        return if command == "inspect-csv" {
            crate::import::inspect(Path::new(file))
        } else {
            crate::import::commons::preview(Path::new(file))
        };
    }
    if command == "nonce" {
        if !options.is_empty() {
            return Err(Error::new("nonce takes no options"));
        }
        return Ok(json!({"request":hex::encode_lower(&crypto::random::<16>()?)}));
    }
    // A development probe, deliberately before paths, custody and network work.
    // A release or an older executable must refuse the lab before it can start
    // a host whose runtime would ignore the loopback-only attachment.
    if command == "runtime-isolation" {
        empty(&options)?;
        #[cfg(any(test, debug_assertions))]
        return Ok(json!({
            "loopback_only": true,
            "port": crate::runtime::private_attachment_port()?,
        }));
        #[cfg(not(any(test, debug_assertions)))]
        return Err(Error::new(
            "private Commons runtime isolation requires a debug build",
        ));
    }
    if command == "network-state" {
        empty(&options)?;
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or_else(|| Error::new("network observation path unavailable"))?;
        let paths = aterm_secrets::client::Paths::live()
            .map_err(|_| Error::new("service observation path unavailable"))?;
        let network = crate::runtime::read_live_publication(&paths, &config);
        return read_network_state_with(
            network,
            || {
                Ok(crate::network_state::Publication::new(
                    crate::network_state::read_history(&config)?,
                    std::time::Instant::now(),
                ))
            },
            || crate::service_state::observe_publication(&paths, &config),
            network_place::current,
            || (std::time::Instant::now(), aterm_time::unix_millis()),
        );
    }
    if command == "commons-lineage" {
        empty(&options)?;
        return lineage::run();
    }
    if matches!(
        command,
        "accounts-billing-propose" | "accounts-billing-inspect" | "accounts-billing-prepare"
    ) {
        return crate::accounts::billing::command::run(command, options);
    }
    if matches!(
        command,
        "accounts-broker-propose" | "accounts-broker-inspect" | "accounts-broker-refresh"
    ) {
        return crate::accounts::broker::command::policy_command(command, options);
    }
    if matches!(
        command,
        "commons-status"
            | "commons-authority"
            | "commons-tables"
            | "commons-inventory"
            | "commons-custody"
            | "commons-open"
            | "commons-create"
    ) {
        return crate::catalog::command::run(command, options);
    }
    if matches!(
        command,
        "accounts-import-csv" | "accounts-imports" | "accounts-compare-csv"
    ) {
        return crate::import::commons::run(command, options);
    }
    if matches!(
        command,
        "resources-propose"
            | "resources-init"
            | "resources-status"
            | "resources-reserve"
            | "resources-attempt"
    ) {
        return crate::resources::command::run(command, options);
    }
    if matches!(
        command,
        "accounts-activation-init"
            | "accounts-activation-status"
            | "accounts-authorize"
            | "accounts-intent"
            | "accounts-stop"
            | "accounts-prepare"
    ) {
        return crate::accounts::activations::command::run(command, options);
    }
    if matches!(
        command,
        "accounts-init"
            | "accounts-get"
            | "accounts-scan"
            | "accounts-write"
            | "accounts-revert"
            | "accounts-history"
            | "accounts-check"
            | "accounts-plan"
    ) {
        return crate::accounts::proposals::command::run(command, options);
    }
    let dir_word = take(&mut options, "--dir")?;
    let dir = Path::new(dir_word);
    let value = match command {
        "request" => {
            empty(&options)?;
            json!({"actor":hex::encode_lower(&local::request(dir)?)})
        }
        "init" => {
            let namespace = options.remove("--namespace").unwrap_or("accounts");
            empty(&options)?;
            local::init(dir, namespace)?;
            status(&Database::open(dir)?)
        }
        "invite" => {
            let actor = decode_hex::<48>(take(&mut options, "--actor")?)?;
            empty(&options)?;
            let credentials = Credentials::load(dir)?;
            let invitation = credentials.invitation(&actor)?;
            aterm_uds::private_file::write_private(dir, "join.invite", &invitation)?;
            json!({"invitation":dir.join("join.invite").to_string_lossy().as_ref(),"root":hex::encode_lower(&credentials.manifest.root),"actor":hex::encode_lower(&actor)})
        }
        "join" => {
            let path = Path::new(take(&mut options, "--invite")?);
            let trusted = decode_hex(take(&mut options, "--trust-root")?)?;
            empty(&options)?;
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::new("invalid invitation path"))?;
            let invitation = aterm_uds::private_file::read_private(parent, name, 4096)
                .map_err(|_| Error::new("invitation missing or not private"))?;
            local::join(dir, &invitation, trusted)?;
            status(&Database::open(dir)?)
        }
        "status" => {
            empty(&options)?;
            status(&Database::open(dir)?)
        }
        "history" => {
            empty(&options)?;
            let db = Database::open(dir)?;
            json!({"operations":db.history.index.inventory().filter_map(|id|db.provenance(id)).collect::<Vec<_>>()})
        }
        "provenance" => {
            let id = decode_hex(take(&mut options, "--operation")?)?;
            empty(&options)?;
            Database::open(dir)?
                .provenance(&id)
                .ok_or_else(|| Error::new("operation is not retained here"))?
        }
        "revert" => {
            let operation = decode_hex(take(&mut options, "--operation")?)?;
            let request = decode_hex(take(&mut options, "--request")?)?;
            empty(&options)?;
            receipt(Database::open(dir)?.revert(request, operation)?, request)
        }
        "revoke-writes" => {
            let actor = decode_hex(take(&mut options, "--actor")?)?;
            let request = decode_hex(take(&mut options, "--request")?)?;
            let through = options.remove("--through").map(decode_hex).transpose()?;
            empty(&options)?;
            receipt(
                Database::open(dir)?.revoke_writes(request, actor, through)?,
                request,
            )
        }
        "import-csv" => {
            let path = Path::new(take(&mut options, "--file")?);
            let request = decode_hex(take(&mut options, "--request")?)?;
            empty(&options)?;
            crate::import::apply(dir, path, request)?
        }
        "machine-history" => {
            let key = take(&mut options, "--key")?;
            empty(&options)?;
            json!({"machine":key,"history":Database::open(dir)?.history.machine_history(key)})
        }
        "resolve-name" => {
            let name = take(&mut options, "--name")?;
            empty(&options)?;
            let name = crate::machines::Name::parse(name)
                .map_or_else(|_| name.to_owned(), |n| n.canonical());
            let db = Database::open(dir)?;
            let view = db.view();
            let owners = view.machine_names.get(&name).cloned().unwrap_or_default();
            json!({"name":name,"ambiguous":owners.len()>1,"machines":owners.into_iter().collect::<Vec<_>>()})
        }
        "machine-name" => {
            let key = take(&mut options, "--key")?.to_owned();
            let request = decode_hex(take(&mut options, "--request")?)?;
            let requested = options.remove("--name");
            empty(&options)?;
            let mut db = Database::open(dir)?;
            if !db.credentials.has_control_authority() {
                return Err(Error::new(
                    "this replica does not hold administrative signing authority",
                ));
            }
            let actor = db.credentials.manifest.control_actor();
            if let Some((_, old)) = db
                .history
                .find_request(&actor, request)
                .map(|(id, c)| (id, c.clone()))
            {
                if let Command::Write { changes, .. } = &old
                    && changes.len() == 1
                    && changes[0].collection == "machines"
                    && changes[0].key == key
                {
                    let name = changes[0]
                        .value
                        .as_ref()
                        .and_then(|v| v.get("name"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::new("stored machine assignment has no name"))?
                        .to_owned();
                    if requested
                        .map(crate::machines::Name::parse)
                        .transpose()?
                        .is_none_or(|n| n.canonical() == name)
                    {
                        let id = db.commit(old)?;
                        return Ok(
                            json!({"machine":key,"name":name,"operation":hex::encode_lower(&id),"request":hex::encode_lower(&request)}),
                        );
                    }
                }
                return Err(Error::new(
                    "request identity already belongs to another assignment",
                ));
            }
            let view = db.view();
            let previous = view.cells.get(&("machines".into(), key.clone()));
            if previous.is_some_and(|cell| cell.stopped || cell.candidates.len() != 1) {
                return Err(Error::new("machine assignment is stopped or conflicted"));
            }
            let mut value = previous
                .and_then(|cell| cell.candidates.first())
                .map_or_else(|| json!({}), |c| c.value.clone());
            let name = if let Some(n) = requested {
                crate::machines::Name::parse(n)?.canonical()
            } else if let Some(name) = value.get("name").and_then(Value::as_str) {
                name.to_owned()
            } else {
                let number = (1..=u32::MAX)
                    .find(|n| !view.machine_names.contains_key(&format!("m{n}")))
                    .ok_or_else(|| Error::new("no machine names remain"))?;
                format!("m{number}")
            };
            value
                .as_object_mut()
                .ok_or_else(|| Error::new("machine assignment is not an object"))?
                .insert("name".into(), Value::from(name.clone()));
            let id = db.commit(Command::Write {
                request,
                changes: vec![Change {
                    collection: "machines".into(),
                    key: key.clone(),
                    value: Some(value),
                }],
            })?;
            json!({"machine":key,"name":name,"operation":hex::encode_lower(&id),"request":hex::encode_lower(&request)})
        }
        "get" | "scan" => {
            let collection = take(&mut options, "--collection")?;
            let key = if command == "get" {
                Some(take(&mut options, "--key")?)
            } else {
                None
            };
            empty(&options)?;
            let db = Database::open(dir)?;
            let view = db.view();
            let records:Vec<_>=view.cells.iter().filter(|((c,k),_)|c==collection&&key.is_none_or(|key|key==k)).map(|((_,key),cell)|json!({"key":key,"stopped":cell.stopped,"conflicted":cell.candidates.len()>1,"values":cell.candidates.iter().map(|v|json!({"operation":hex::encode_lower(&v.operation),"value":v.value})).collect::<Vec<_>>()})).collect();
            json!({"records":records,"pending":view.causal.pending.len(),"quarantined":view.causal.quarantined.len()})
        }
        "put" | "stop" => {
            let collection = take(&mut options, "--collection")?.to_owned();
            let key = take(&mut options, "--key")?.to_owned();
            let request = decode_hex(take(&mut options, "--request")?)?;
            let value = if command == "put" {
                let raw = take(&mut options, "--value")?;
                if raw.len() > crypto::MAX_BODY {
                    return Err(Error::new("value exceeds limit"));
                }
                Some(aterm_json::from_str(raw).map_err(|_| Error::new("invalid value JSON"))?)
            } else {
                None
            };
            empty(&options)?;
            let mut db = Database::open(dir)?;
            if db
                .view()
                .cells
                .get(&(collection.clone(), key.clone()))
                .is_some_and(|c| c.stopped)
            {
                return Err(Error::new(
                    "object generation stopped; create a new object key",
                ));
            }
            let id = db.commit(Command::Write {
                request,
                changes: vec![Change {
                    collection,
                    key,
                    value,
                }],
            })?;
            receipt(id, request)
        }
        "budget-create" => {
            let name = take(&mut options, "--name")?.to_owned();
            let total = unsigned(take(&mut options, "--total")?)?;
            let request = decode_hex(take(&mut options, "--request")?)?;
            let raw = options.remove("--shares");
            empty(&options)?;
            let mut db = Database::open(dir)?;
            let shares = if let Some(raw) = raw {
                if raw.len() > 16 * 1024 {
                    return Err(Error::new("shares exceed limit"));
                }
                let value: Value =
                    aterm_json::from_str(raw).map_err(|_| Error::new("invalid shares JSON"))?;
                value
                    .as_object()
                    .ok_or_else(|| Error::new("shares must be an actor-to-units object"))?
                    .iter()
                    .map(|(actor, n)| {
                        Ok((
                            decode_hex(actor)?,
                            n.as_u64()
                                .ok_or_else(|| Error::new("share must be unsigned units"))?,
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?
            } else {
                BTreeMap::from([(db.credentials.capability.actor, total)])
            };
            let id = db.allocate(request, name, total, shares)?;
            receipt(id, request)
        }
        "spend" => {
            let name = take(&mut options, "--name")?;
            let cost = unsigned(take(&mut options, "--cost")?)?;
            let request = decode_hex(take(&mut options, "--request")?)?;
            empty(&options)?;
            let id = Database::open(dir)?.spend(request, name, cost)?;
            receipt(id, request)
        }
        "sync" => {
            let peer = address(take(&mut options, "--peer")?)?;
            empty(&options)?;
            let mut sent = 0;
            let mut received = 0;
            let mut caught_up = false;
            let mut deferred = 0;
            let mut repair = crate::network::Repair::default();
            for _ in 0..80 {
                let report = crate::network::sync_round_with(dir, peer, &mut repair)?;
                sent += report.sent;
                received += report.received;
                deferred = report.deferred;
                if report.sent == 0 && report.received == 0 {
                    caught_up = deferred == 0;
                    break;
                }
            }
            json!({"sent":sent,"received":received,"deferred":deferred,"caught_up":caught_up})
        }
        "serve" => {
            let listen = address(take(&mut options, "--listen")?)?;
            let peers = options
                .remove("--peers")
                .map(|raw| raw.split(',').map(address).collect::<Result<Vec<_>>>())
                .transpose()?
                .unwrap_or_default();
            empty(&options)?;
            Credentials::load(dir)?;
            let scope = if listen.ip().is_loopback() {
                aterm_net::lan::Scope::Loopback
            } else {
                aterm_net::lan::Scope::Lan
            };
            let listener = aterm_net::lan::Listener::bind(listen, scope)?;
            crate::network::serve(dir, listener, peers)?;
            json!({"stopped":true})
        }
        _ => return Err(Error::new("unknown database command; run aterm db --help")),
    };
    Ok(value)
}

#[derive(Clone, Copy)]
struct LiveObservationError {
    kind: std::io::ErrorKind,
    stage: Option<&'static str>,
}

fn read_network_state_with(
    network: std::io::Result<crate::network_state::Publication>,
    read_history: impl FnOnce() -> Result<crate::network_state::Publication>,
    read_services: impl FnOnce() -> crate::service_state::Publication,
    read_place: impl FnOnce() -> Option<aterm_netplace::Place>,
    clock: impl FnOnce() -> (std::time::Instant, Option<u64>),
) -> Result<Value> {
    let (network, error) = match network {
        Ok(network) => (network, None),
        Err(error) => (
            read_history()?,
            Some(LiveObservationError {
                kind: error.kind(),
                stage: crate::network_state::live_error_stage(&error),
            }),
        ),
    };
    Ok(network_state_value_with(
        network,
        error,
        read_services,
        read_place,
        clock,
    ))
}

fn network_state_value_with(
    network: crate::network_state::Publication,
    live_error: Option<LiveObservationError>,
    read_services: impl FnOnce() -> crate::service_state::Publication,
    read_place: impl FnOnce() -> Option<aterm_netplace::Place>,
    clock: impl FnOnce() -> (std::time::Instant, Option<u64>),
) -> Value {
    let services = read_services();
    let place = read_place();
    // Both leases include service and place I/O and use the same final clock sample.
    let (now, wall) = clock();
    let network = network.current_at(now, wall);
    let services = services.current_at(now, wall);
    let service = |state: aterm_types::network_state::ServiceState| {
        // A new service field needs an explicit wire decision rather than silently disappearing.
        let aterm_types::network_state::ServiceState {
            availability,
            observed_at_ms,
            last_success_ms,
            network_materialization,
            http204_response,
            network_refresh,
            revision,
            detail,
            refresh_failed,
        } = state;
        json!({
        "availability":format!("{availability:?}"),"observed_at_ms":observed_at_ms,
        "last_success_ms":last_success_ms,"revision":revision,"detail":detail,"refresh_failed":refresh_failed,
        "network_materialization":network_materialization.map(|receipt|json!({
            "source":match receipt.source {
                aterm_types::network_state::VaultNetworkSource::ConfiguredUrl => "configured_url",
                aterm_types::network_state::VaultNetworkSource::PeerUrl => "peer_url",
            },
            "sequence":receipt.sequence,"sampled_at_ms":receipt.sampled_at_ms,
        })),
        "http204_response":http204_response.map(|response|json!({
            "source":match response.source {
                aterm_types::network_state::VaultNetworkSource::ConfiguredUrl => "configured_url",
                aterm_types::network_state::VaultNetworkSource::PeerUrl => "peer_url",
            },
            "responded_at_ms":response.responded_at_ms,
        })),
        "network_refresh":network_refresh.map(|refresh|json!({
            "outcome":format!("{:?}",refresh.outcome),
            "completed_at_ms":refresh.completed_at_ms,
        }))})
    };
    json!({"place":network_place::value(place.as_ref()),
        "live_observation":network.via_local_process.is_some(),
        "live_observation_error":live_error.map(|error|format!("{:?}",error.kind)),
        "live_observation_error_stage":live_error.and_then(|error|error.stage),
        "live_network":network.via_local_process.is_some() && network.visibility == aterm_types::network_state::Visibility::Active,
        "network":crate::network_state::value(&network),
        "services":{"accounts":service(services.accounts),"secrets":service(services.secrets)}})
}

fn take<'a>(options: &mut BTreeMap<&'a str, &'a str>, name: &str) -> Result<&'a str> {
    options
        .remove(name)
        .ok_or_else(|| Error::new(&format!("missing required database option {name}")))
}
fn empty(options: &BTreeMap<&str, &str>) -> Result<()> {
    if options.is_empty() {
        Ok(())
    } else {
        Err(Error::new("unknown database option"))
    }
}
fn unsigned(s: &str) -> Result<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::new("expected unsigned admission units"));
    }
    s.parse()
        .map_err(|_| Error::new("expected unsigned admission units"))
}
fn address(s: &str) -> Result<SocketAddr> {
    s.parse()
        .map_err(|_| Error::new("peer address must be a numeric LAN IP and port"))
}
fn receipt(id: [u8; 32], request: [u8; 16]) -> Value {
    json!({"operation":hex::encode_lower(&id),"request":hex::encode_lower(&request),"durable":true})
}
fn status(db: &Database) -> Value {
    let view = db.view();
    let budgets:Vec<_>=view.budgets.iter().map(|(name,grants)|{
        let allocations:Vec<_>=grants.iter().filter_map(|grant|match db.history.command(grant){Some((_,Command::Budget{total,shares,..}))=>Some(json!({"grant":hex::encode_lower(grant),"total":total,"shares":shares.iter().map(|(actor,limit)|{let share=(*grant,*actor);let spent=view.spent.get(&share).copied().unwrap_or(0);let disputed=view.authority_fault||grants.len()!=1||view.disputed_shares.contains(&share)||view.causal.quarantined.contains(actor);json!({"actor":hex::encode_lower(actor),"assigned":limit,"accepted_spent":spent,"possible_spent":view.liability.get(&share).copied().unwrap_or(0),"disputed":disputed,"admission_remaining":if disputed{None}else{Some(limit.saturating_sub(spent))},"silence_reclaims_rights":false})}).collect::<Vec<_>>()})),_=>None}).collect();
        json!({"name":name,"conflicted":grants.len()!=1,"allocations":allocations})
    }).collect();
    json!({"dataset":hex::encode_lower(&db.credentials.manifest.id),"root":hex::encode_lower(&db.credentials.manifest.root),"actor":hex::encode_lower(&db.credentials.capability.actor),"namespace":db.credentials.manifest.namespace,"retained":db.history.index.inventory().count(),"visible":view.causal.ordered.len(),"pending":view.causal.pending.len(),"quarantined":view.causal.quarantined.iter().map(|a|hex::encode_lower(a)).collect::<Vec<_>>(),"invalid":view.invalid.len(),"refused":db.history.refusals().map(|(id,reason)|json!({"operation":hex::encode_lower(id),"reason":reason})).collect::<Vec<_>>(),"authority_fault":view.authority_fault,"retired_writers":view.revocations.keys().map(|actor|hex::encode_lower(actor)).collect::<Vec<_>>(),"budgets":budgets})
}

#[cfg(test)]
mod network_state_tests {
    use super::*;
    use aterm_types::network_state::{
        HistoryState, NetworkObservation, ServiceAvailability, ServiceState, ServiceStates,
        Visibility,
    };
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    fn network(at: Instant, wall: u64) -> crate::network_state::Publication {
        crate::network_state::Publication::new(
            NetworkObservation {
                observed_at_ms: wall,
                visibility: Visibility::Active,
                history: HistoryState::Ready,
                via_local_process: Some(7),
                revision: 23,
                ..NetworkObservation::default()
            },
            at,
        )
    }

    fn services(at: Instant, wall: u64) -> crate::service_state::Publication {
        let state = ServiceState {
            availability: ServiceAvailability::Cached,
            observed_at_ms: wall,
            revision: Some("retained revision".into()),
            ..ServiceState::default()
        };
        crate::service_state::Publication::new(
            ServiceStates {
                accounts: state.clone(),
                secrets: state,
            },
            at,
        )
    }

    #[test]
    fn unavailable_network_place_is_reported_explicitly() {
        let at = Instant::now();
        let value = network_state_value_with(
            network(at, 100_000),
            None,
            || services(at, 100_000),
            || None,
            || (at, Some(100_000)),
        );
        assert_eq!(value.get("place"), Some(&json!({"why":"not_sampled"})));
        assert_eq!(value["live_network"], json!(true));
    }

    #[test]
    fn network_place_uses_the_common_fields_without_rewriting_access() {
        let at = Instant::now();
        let value = network_state_value_with(
            network(at, 100_000),
            None,
            || services(at, 100_000),
            || Some(aterm_netplace::sample::hotspot(100_000, 90_000)),
            || (at, Some(100_000)),
        );
        let place = &value["place"];
        assert_eq!(place["state"], json!("remote"));
        assert_eq!(place["kind"], json!("hotspot"));
        assert_eq!(place["address"], json!("172.20.10.2"));
        assert_eq!(place["network_strong"], json!(true));
        assert_eq!(place["expensive"], json!(true));
        assert_eq!(place["constrained"], json!(false));
        assert_eq!(place["internet"]["state"], json!("path-up"));
        assert_eq!(place["internet"]["source"], json!("path"));
        assert_eq!(
            place["internet"]["checked_ms"],
            json!({"why":"not_sampled"})
        );
        assert_eq!(place["alab"]["state"], json!("away"));
        assert_eq!(place["alab"]["known"], json!(41));
        assert_eq!(place["alab"]["reachable"], json!(0));
        assert_eq!(place["alab"]["since_ms"], json!(90_000));
        assert_eq!(value["live_network"], json!(true));
        assert_eq!(
            value["services"]["accounts"]["availability"],
            json!("Cached")
        );
    }

    #[test]
    fn a_known_network_place_does_not_grant_observer_or_service_access() {
        let at = Instant::now();
        let value = network_state_value_with(
            crate::network_state::Publication::new(
                NetworkObservation {
                    observed_at_ms: 100_000,
                    history: HistoryState::Ready,
                    visibility: Visibility::Unavailable,
                    ..NetworkObservation::default()
                },
                at,
            ),
            None,
            || crate::service_state::Publication::new(ServiceStates::default(), at),
            || Some(aterm_netplace::sample::home(100_000, 12)),
            || (at, Some(100_000)),
        );
        assert_eq!(value["place"]["state"], json!("alab-lan"));
        assert_eq!(value["place"]["alab"]["reachable"], json!(12));
        assert_eq!(value["live_observation"], json!(false));
        assert_eq!(value["live_network"], json!(false));
        for service in ["accounts", "secrets"] {
            assert_eq!(value["services"][service]["availability"], json!("Unknown"));
        }
    }

    #[test]
    fn network_place_io_cannot_renew_observer_or_service_leases() {
        let at = Instant::now();
        let now = Cell::new((at, Some(100_000)));
        let value = network_state_value_with(
            network(at, 100_000),
            None,
            || services(at, 100_000),
            || {
                now.set((at + Duration::from_secs(61), Some(161_000)));
                Some(aterm_netplace::sample::hotspot(100_000, 90_000))
            },
            || now.get(),
        );
        assert_eq!(value["place"]["state"], json!("remote"));
        assert_eq!(value["live_network"], json!(false));
        assert_eq!(value["network"]["visibility"], json!("Unavailable"));
        assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
        for service in ["accounts", "secrets"] {
            assert_eq!(value["services"][service]["availability"], json!("Unknown"));
            assert_eq!(value["services"][service]["observed_at_ms"], json!(100_000));
        }
    }

    #[test]
    fn vault_network_receipt_json_remains_historical_after_local_observation_expires() {
        use aterm_types::network_state::{
            VaultNetworkMaterialization, VaultNetworkRefresh, VaultNetworkRefreshOutcome,
            VaultNetworkSource,
        };
        let at = Instant::now();
        let mut states = ServiceStates::default();
        states.secrets.availability = ServiceAvailability::Cached;
        states.secrets.observed_at_ms = 100_000;
        states.secrets.network_materialization = Some(VaultNetworkMaterialization {
            source: VaultNetworkSource::PeerUrl,
            sequence: 17,
            sampled_at_ms: 80_000,
        });
        states.secrets.refresh_failed = true;
        states.secrets.network_refresh = Some(VaultNetworkRefresh {
            completed_at_ms: 90_000,
            outcome: VaultNetworkRefreshOutcome::Failed,
        });
        let value = network_state_value_with(
            network(at, 100_000),
            None,
            || crate::service_state::Publication::new(states, at),
            || None,
            || (at + Duration::from_secs(61), Some(161_000)),
        );
        assert_eq!(
            value["services"]["secrets"]["availability"],
            json!("Unknown")
        );
        assert_eq!(value["services"]["secrets"]["last_success_ms"], Value::Null);
        assert_eq!(value["services"]["secrets"]["refresh_failed"], json!(true));
        assert_eq!(
            value["services"]["secrets"]["network_refresh"],
            json!({"outcome":"Failed", "completed_at_ms":90_000})
        );
        assert_eq!(
            value["services"]["secrets"]["network_materialization"],
            json!({
                "source":"peer_url", "sequence":17, "sampled_at_ms":80_000,
            })
        );
        assert_eq!(
            value["services"]["accounts"]["network_materialization"],
            Value::Null
        );
        assert_eq!(
            value["services"]["accounts"]["network_refresh"],
            Value::Null
        );
    }

    #[test]
    fn a_retained_204_json_does_not_establish_access_or_signed_materialization() {
        use aterm_types::network_state::{
            VaultHttp204Response, VaultNetworkRefresh, VaultNetworkRefreshOutcome,
            VaultNetworkSource,
        };
        let at = Instant::now();
        let mut states = ServiceStates::default();
        states.secrets.availability = ServiceAvailability::Unavailable;
        states.secrets.observed_at_ms = 100_000;
        states.secrets.http204_response = Some(VaultHttp204Response {
            source: VaultNetworkSource::ConfiguredUrl,
            responded_at_ms: 80_000,
        });
        states.secrets.network_refresh = Some(VaultNetworkRefresh {
            completed_at_ms: 90_000,
            outcome: VaultNetworkRefreshOutcome::Succeeded,
        });
        let value = network_state_value_with(
            network(at, 100_000),
            None,
            || crate::service_state::Publication::new(states, at),
            || None,
            || (at + Duration::from_secs(61), Some(161_000)),
        );
        let secrets = &value["services"]["secrets"];
        assert_eq!(secrets["availability"], json!("Unknown"));
        assert_eq!(secrets["last_success_ms"], Value::Null);
        assert_eq!(secrets["network_materialization"], Value::Null);
        assert_eq!(secrets["refresh_failed"], json!(false));
        assert_eq!(
            secrets["network_refresh"],
            json!({"outcome":"Succeeded", "completed_at_ms":90_000})
        );
        assert_eq!(
            secrets["http204_response"],
            json!({
                "source":"configured_url", "responded_at_ms":80_000,
            })
        );
        assert_eq!(
            value["services"]["accounts"]["http204_response"],
            Value::Null
        );
    }

    #[test]
    fn delayed_service_metadata_cannot_keep_an_expired_network_current() {
        let at = Instant::now();
        let sampled = at + Duration::from_secs(14);
        let now = Cell::new((sampled, Some(114_000)));
        let value = network_state_value_with(
            network(at, 100_000),
            None,
            || {
                let publication = services(sampled, 114_000);
                now.set((at + Duration::from_secs(16), Some(116_000)));
                publication
            },
            || None,
            || now.get(),
        );
        assert_eq!(value["live_network"], json!(false));
        assert_eq!(value["network"]["visibility"], json!("Unavailable"));
        assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
        assert_eq!(value["network"]["revision"], json!(23));
        assert_eq!(
            value["services"]["secrets"]["availability"],
            json!("Cached")
        );
    }

    #[test]
    fn current_network_and_retained_history_keep_their_distinct_evidence() {
        let at = Instant::now();
        let now = at + Duration::from_secs(14);
        let current = network_state_value_with(
            network(at, 100_000),
            None,
            || {
                crate::service_state::Publication::new(
                    ServiceStates {
                        accounts: ServiceState {
                            availability: ServiceAvailability::Cached,
                            observed_at_ms: 50_000,
                            ..ServiceState::default()
                        },
                        secrets: ServiceState {
                            availability: ServiceAvailability::Cached,
                            observed_at_ms: 114_000,
                            ..ServiceState::default()
                        },
                    },
                    now,
                )
            },
            || None,
            || (now, Some(114_000)),
        );
        assert_eq!(current["live_network"], json!(true));
        assert_eq!(
            current["services"]["accounts"]["availability"],
            json!("Unknown")
        );
        assert_eq!(
            current["services"]["secrets"]["availability"],
            json!("Cached")
        );
        let retained = crate::network_state::Publication::new(
            NetworkObservation {
                visibility: Visibility::Unavailable,
                history: HistoryState::Ready,
                observed_at_ms: 100_000,
                revision: 23,
                ..NetworkObservation::default()
            },
            now,
        );
        let history = network_state_value_with(
            retained,
            None,
            || services(now, 114_000),
            || None,
            || (now, Some(114_000)),
        );
        assert_eq!(history["live_observation"], json!(false));
        assert_eq!(history["live_network"], json!(false));
        assert_eq!(history["network"]["revision"], json!(23));
        assert_eq!(history["network"]["observed_at_ms"], json!(100_000));
    }

    #[test]
    fn failed_live_read_reports_its_kind_without_exposing_private_errors() {
        let at = Instant::now();
        let private = "snapshot-json: private diagnostic payload";
        for (kind, code) in [
            (std::io::ErrorKind::InvalidData, "InvalidData"),
            (std::io::ErrorKind::NotFound, "NotFound"),
        ] {
            let value = read_network_state_with(
                Err(std::io::Error::new(kind, private)),
                || {
                    Ok(crate::network_state::Publication::new(
                        NetworkObservation {
                            visibility: Visibility::Unavailable,
                            history: HistoryState::Ready,
                            observed_at_ms: 100_000,
                            revision: 23,
                            ..NetworkObservation::default()
                        },
                        at,
                    ))
                },
                || services(at, 100_000),
                || None,
                || (at, Some(100_000)),
            )
            .unwrap();
            assert_eq!(value["live_observation_error"], json!(code));
            assert_eq!(
                value.get("live_observation_error_stage"),
                Some(&Value::Null)
            );
            assert_eq!(value["live_observation"], json!(false));
            assert_eq!(value["live_network"], json!(false));
            assert_eq!(value["network"]["history"], json!("Ready"));
            assert_eq!(value["network"]["revision"], json!(23));
            assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
            assert!(!aterm_json::to_string(&value).unwrap().contains(private));
        }
        let live = read_network_state_with(
            Ok(network(at, 100_000)),
            || panic!("a live read must not open retained history"),
            || services(at, 100_000),
            || None,
            || (at, Some(100_000)),
        )
        .unwrap();
        assert_eq!(live.get("live_observation_error"), Some(&Value::Null));
        assert_eq!(live.get("live_observation_error_stage"), Some(&Value::Null));
        assert_eq!(live["live_network"], json!(true));
    }

    #[test]
    fn failed_native_endpoint_reports_safe_stage_and_retains_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let private = "private-observer-diagnostic";
        let config = root.path().join(private).join("aterm.toml");
        let error = crate::network_state::read_live_publication(&config).unwrap_err();
        assert!(error.to_string().contains(private));
        let at = Instant::now();
        let value = read_network_state_with(
            Err(error),
            || {
                Ok(crate::network_state::Publication::new(
                    NetworkObservation {
                        visibility: Visibility::Unavailable,
                        history: HistoryState::Ready,
                        observed_at_ms: 100_000,
                        revision: 23,
                        ..NetworkObservation::default()
                    },
                    at,
                ))
            },
            || services(at, 100_000),
            || None,
            || (at, Some(100_000)),
        )
        .unwrap();
        assert_eq!(value["live_observation_error"], json!("NotFound"));
        assert_eq!(value["live_observation_error_stage"], json!("endpoint"));
        assert_eq!(value["live_observation"], json!(false));
        assert_eq!(value["live_network"], json!(false));
        assert_eq!(value["network"]["revision"], json!(23));
        assert_eq!(value["network"]["observed_at_ms"], json!(100_000));
        assert!(!aterm_json::to_string(&value).unwrap().contains(private));
    }

    #[test]
    fn a_failed_live_read_cannot_hide_a_failed_history_read() {
        let result = read_network_state_with(
            Err(std::io::ErrorKind::NotFound.into()),
            || Err(Error::new("history unavailable")),
            || panic!("failed history must be returned before service reads"),
            || panic!("failed history must be returned before place reads"),
            || panic!("failed history supplies no publication"),
        );
        assert!(result.is_err());
    }
}
