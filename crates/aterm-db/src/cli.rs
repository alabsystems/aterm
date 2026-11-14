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
  The owner's tables, read from the registry and never from the fleet tables below:\n\
  emails|computers|phones|ai-accounts [--show-addresses] | --digest | --edit\n\
                    | set <row> <column> [<value> | --stdin]\n\
                                       each answer says its `state` (live, held, never or off);\n\
                                       ai-accounts and computers are the shared files\n\
                                       ai/accounts and lab/machines, read from this Mac's\n\
                                       snapshot whatever its Commons is doing (`aterm file`)\n\
  emails --show-passwords | phones --show-numbers\n\
                                       plaintext only in a person's own tab, at a terminal\n\
  Fleet relational tables (backed by astream + aterm-net at ~/.aterm/lab-db, no --dir needed):\n\
  tables                               summary, row counts and foreign-key integrity\n\
  scan [<table>] [--show-passwords]    every row of computers|emails|phones|ai_accounts\n\
  get <table> <key> [--show-passwords] get one row from computers|emails|phones|ai_accounts\n\
  put <table> <key> <json>             replace one row by its primary key; an email row without a\n\
                                       password keeps the stored one, and null clears it\n\
  delete <table> <key>                 delete one row by its primary key\n\
  dump [<file>] [--show-passwords]     export all 4 tables as JSON; only a dump to <file> can be\n\
                                       loaded back, because stdout redacts passwords\n\
  load <file>                          write the rows of a dump that differ from what is stored\n\
  bootstrap [<secrets-dir>]            add rows from the legacy CSVs; a key already used is left alone\n\
  rekey                                move this database's rows onto the current fleet key after a\n\
                                       rotation or a new fleet, keeping the old folder beside it;\n\
                                       run it again to finish a move cut short\n\
  sync <peer[:port]>                   sync operations.astream with a fleet peer over TLS\n\
  serve [--listen <ip:port>] [--peers <ip:port,...>]\n\
                                       serve operations.astream to fleet peers (default LAN :7485)\n\
  Commons tables use enrolled secrets and default LAN repair (no --dir):\n\
  table <verb> ...                     typed ATT/1 tables: ls, schema, get, scan, history,\n\
                                       check, revert, export, apply, edit (table --help)\n\
  alab <verb> ...                      the ALab lab model: import, check, adopt (alab --help)\n\
  commons-status                       local membership, history capacity and fingerprint, control and sharing\n\
  commons-lineage                      verified predecessor-to-current public writer identities\n\
  network-state                        local place, observer and retained history; never grants access\n\
  commons-authority                    inspect cached admin control metadata; changes nothing\n\
  commons-tables                       list declared and rolling tables, provenance and subscriptions\n\
  commons-inventory --table <digest>    local retained operation counts and fingerprints\n\
                    [--records <1..256> [--after <digest>]]\n\
                                       page operation IDs and eligibility; no values or keys\n\
                    --tables <digest,...>\n\
                                       inspect up to 64 tables through one read context\n\
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
  accounts-broker-publish --grant <hex> --base <digest> --file <policy.toml>\n\
                                      conditionally publish the exact reviewed refresh as root\n\
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
    if words.first().is_some_and(|w| w == "fleet") {
        return crate::fleet::command::main_entry(&words[1..]);
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
    // The owner's tables as registry views (one-mesh design §4.7, A7). A noun never falls
    // through to fleet_db: `ai-accounts` and `computers` answer from their shared file's
    // snapshot, `emails` and `phones` from the registry or with the word `never`
    // (docs/DESIGN-shared-files-2026-11-11.md §12).
    if let Some(code) = crate::alab::command::views::main_entry(&words) {
        return code;
    }
    // fleet_db prints passwords on `--show-passwords`, and `dump <file>` writes every one in
    // plaintext: the same session rule decides both (§4.8), so no spawned, driven or agent
    // session reads them that way. Its phones' numbers are fleet_db's keys, printed by every
    // verb that names a phone; they go with fleet_db (A13).
    #[cfg(unix)]
    if let Err(error) = fleet_db_reveals(&words).and_then(|reveals| {
        if reveals {
            crate::alab::command::session::decide(&crate::alab::command::session::facts())?;
        }
        Ok(())
    }) {
        let _ = writeln!(std::io::stderr(), "aterm db: {error}");
        return ExitCode::from(1);
    }
    match run(&words) {
        Ok(value) => match if words[0] == "commons-status" && words.len() == 1 {
            Ok(commons_status_text(&value, aterm_time::unix_millis()))
        } else {
            aterm_json::to_string(&value)
        } {
            Ok(mut text) => {
                let code = if value
                    .as_object()
                    .and_then(|o| o.get("ok"))
                    .and_then(Value::as_bool)
                    == Some(false)
                {
                    1
                } else {
                    0
                };
                text.push('\n');
                aterm_uds::stdout::answer_exit("aterm db", &text, code)
            }
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
    let words = if words.len() == 2 && words[0] == "commons-status" && words[1] == "--json" {
        &words[..1]
    } else {
        words
    };
    if let Some(value) = crate::fleet_db::try_run(words)? {
        return Ok(value);
    }
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
        return read_network_state_with(
            || crate::runtime::read_live_publication(&paths, &config),
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
        "accounts-broker-propose"
            | "accounts-broker-inspect"
            | "accounts-broker-refresh"
            | "accounts-broker-publish"
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
    // The fleet tables' rules (a rekey cut short, a covered record) live in fleet_db's verbs, and
    // these write cells beneath them. A read stays, as the only one of a rekey's kept folder, under
    // the session rule (`fleet_db_reveals`).
    if matches!(
        command,
        "put"
            | "stop"
            | "revert"
            | "import-csv"
            | "budget-create"
            | "spend"
            | "machine-name"
            | "revoke-writes"
    ) && local::holds_fleet_tables(dir)?
    {
        return Err(Error::new(
            "this folder holds the fleet tables, which no private verb writes. `aterm db put \
             <table> <key> <json>` takes no folder and writes this Mac's replica; once an import \
             moved them, `aterm db emails`, `computers`, `phones` and `ai-accounts` do (`aterm db \
             --help` says which)",
        ));
    }
    let value = match command {
        "request" => {
            empty(&options)?;
            json!({"actor":hex::encode_lower(&local::request(dir)?)})
        }
        "init" => {
            let namespace = options.remove("--namespace").unwrap_or("accounts");
            empty(&options)?;
            if namespace == local::FLEET_NAMESPACE {
                return Err(Error::new(
                    "that namespace is the fleet tables', which a fleet key makes",
                ));
            }
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
            json!({"operations":db.all_provenance()})
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
                use std::fmt::Write as _;
                let mut candidate = String::with_capacity(12);
                (1..=u32::MAX)
                    .find_map(|n| {
                        candidate.clear();
                        let _ = write!(candidate, "m{n}");
                        (!view.machine_names.contains_key(&candidate))
                            .then(|| std::mem::take(&mut candidate))
                    })
                    .ok_or_else(|| Error::new("no machine names remain"))?
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
            let row_json = |key: &str, cell: &crate::database::Cell| {
                json!({
                    "key": key,
                    "stopped": cell.stopped,
                    "conflicted": cell.candidates.len() > 1,
                    "values": cell
                        .candidates
                        .iter()
                        .map(|v| json!({"operation": hex::encode_lower(&v.operation), "value": v.value}))
                        .collect::<Vec<_>>(),
                })
            };
            let records: Vec<_> = match key {
                Some(key) => view
                    .cells
                    .get(&(collection.to_owned(), key.to_owned()))
                    .map(|cell| row_json(key, cell))
                    .into_iter()
                    .collect(),
                None => view
                    .cells
                    .range((collection.to_owned(), String::new())..)
                    .take_while(|((c, _), _)| c == collection)
                    .map(|((_, key), cell)| row_json(key, cell))
                    .collect(),
            };
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
            crate::fleet_db::sync_peer_with(peer, |repair| {
                crate::network::sync_round_with(dir, peer, repair)
            })
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

/// `commons-status`'s text: the Commons state, why it is not ready, what repairs wait for while
/// they are not active, and each peer's last attempt with its age at `now_ms` (the reader's
/// clock) and its cause, identical lines folded into one that names their slots.
fn commons_status_text(value: &Value, now_ms: Option<u64>) -> String {
    let mut lines = vec![format!(
        "Commons: {}{}",
        value["state"].as_str().unwrap_or("unknown"),
        value["role"]
            .as_str()
            .map(|role| format!(" ({role})"))
            .unwrap_or_default()
    )];
    if value["ready"].as_bool() != Some(true)
        && let Some(detail) = value["detail"].as_str()
    {
        lines.push(format!("  {detail}"));
    }
    let repairs = &value["repairs"];
    if let Some(visibility) = repairs["visibility"].as_str()
        && visibility != "Active"
    {
        let detail = repairs["detail"].as_str().map(str::to_owned).or_else(|| {
            repairs["cause"]
                .as_str()
                .map(|cause| format!("repair history unreadable ({cause})"))
        });
        lines.push(format!(
            "Repairs ({}){}",
            visibility.to_ascii_lowercase(),
            detail
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default()
        ));
    }
    // Each distinct attempt line, in first-slot order, with the slots that share it.
    let mut attempts: Vec<(String, Vec<u64>)> = Vec::new();
    for peer in repairs["peers"].as_array().into_iter().flatten() {
        let repair = &peer["last_repair"];
        if repair.is_null() {
            continue;
        }
        let outcome = if repair["needs_update"].as_bool() == Some(true) {
            "needs-update"
        } else {
            repair["outcome"].as_str().unwrap_or("unknown")
        };
        let ago = repair["completed_at_ms"]
            .as_u64()
            .zip(now_ms)
            .and_then(|(at, now)| now.checked_sub(at))
            .map(|ms| format!(" {} ago", age(ms)))
            .unwrap_or_default();
        let cause = repair["cause"]
            .as_str()
            .map(|cause| format!(" ({cause})"))
            .unwrap_or_default();
        let line = format!("{outcome}{ago}{cause}");
        let slot = peer["slot"].as_u64().unwrap_or_default();
        match attempts.iter_mut().find(|(seen, _)| *seen == line) {
            Some((_, slots)) => slots.push(slot),
            None => attempts.push((line, vec![slot])),
        }
    }
    for (line, slots) in attempts {
        lines.push(format!(
            "{} {}: {line}",
            if slots.len() == 1 { "peer" } else { "peers" },
            slot_ranges(&slots)
        ));
    }
    lines.join("\n")
}

/// `ms` as one coarse unit: seconds under a minute, minutes under an hour, hours under two days,
/// then days.
fn age(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..172_800 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Ascending `slots` as comma-separated runs, each consecutive run as `first-last`.
fn slot_ranges(slots: &[u64]) -> String {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for &slot in slots {
        match runs.last_mut() {
            Some((_, last)) if last.checked_add(1) == Some(slot) => *last = slot,
            _ => runs.push((slot, slot)),
        }
    }
    runs.iter()
        .map(|&(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}-{last}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Copy)]
struct LiveObservationError {
    kind: std::io::ErrorKind,
    stage: Option<&'static str>,
}

fn read_network_state_with(
    read_network: impl FnOnce() -> std::io::Result<crate::network_state::Publication>,
    read_history: impl FnOnce() -> Result<crate::network_state::Publication>,
    read_services: impl FnOnce() -> crate::service_state::Publication,
    read_place: impl FnOnce() -> Option<aterm_netplace::Place>,
    clock: impl FnOnce() -> (std::time::Instant, Option<u64>),
) -> Result<Value> {
    // A service read can outlast the network's 15 s lease, so the network is read after it. A
    // relayed publication keeps its owner's age, so reading it late renews nothing.
    let services = read_services();
    let (network, error) = match read_network() {
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
        network, error, services, read_place, clock,
    ))
}

fn network_state_value_with(
    network: crate::network_state::Publication,
    live_error: Option<LiveObservationError>,
    services: crate::service_state::Publication,
    read_place: impl FnOnce() -> Option<aterm_netplace::Place>,
    clock: impl FnOnce() -> (std::time::Instant, Option<u64>),
) -> Value {
    let place = read_place();
    // Both leases include the place I/O and use the same final clock sample.
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
    json!({"dataset":hex::encode_lower(&db.credentials.manifest.id),"root":hex::encode_lower(&db.credentials.manifest.root),"actor":hex::encode_lower(&db.credentials.capability.actor),"namespace":db.credentials.manifest.namespace,"retained":db.history.index().inventory().count(),"visible":view.causal.ordered.len(),"pending":view.causal.pending.len(),"quarantined":view.causal.quarantined.iter().map(|a|hex::encode_lower(a)).collect::<Vec<_>>(),"invalid":view.invalid.len(),"refused":db.history.refusals().map(|(id,reason)|json!({"operation":hex::encode_lower(id),"reason":reason})).collect::<Vec<_>>(),"authority_fault":view.authority_fault,"retired_writers":view.revocations.keys().map(|actor|hex::encode_lower(actor)).collect::<Vec<_>>(),"budgets":budgets})
}

/// Whether `words` show or write fleet_db's plaintext: `--show-passwords` (`--reveal`), a `dump`
/// to a file, its file read as fleet_db reads it (the word after `--dir` is its folder), or a
/// private `get` or `scan` of a folder that holds the fleet tables, which print what is stored.
///
/// # Errors
/// A folder that cannot be told apart: no verb runs on it, and this says why.
#[cfg(unix)]
fn fleet_db_reveals(words: &[String]) -> Result<bool> {
    if words
        .iter()
        .any(|w| w == "--show-passwords" || w == "--reveal")
    {
        return Ok(true);
    }
    if matches!(words.first().map(String::as_str), Some("get" | "scan"))
        && let Some(dir) = words.windows(2).find(|pair| pair[0] == "--dir")
        && local::holds_fleet_tables(Path::new(&dir[1]))?
    {
        return Ok(true);
    }
    if words.first().map(String::as_str) != Some("dump") {
        return Ok(false);
    }
    let mut rest = words[1..].iter();
    while let Some(word) = rest.next() {
        if word == "--dir" {
            rest.next();
        } else if !word.starts_with("--") {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(test, unix))]
#[path = "cli_reveal_gate_tests.rs"]
mod reveal_gate_tests;

#[cfg(test)]
#[path = "cli_private_verb_tests.rs"]
mod private_verb_tests;

#[cfg(test)]
#[path = "cli_network_state_tests.rs"]
mod network_state_tests;
