// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Composition of signed release storage, fleet authorization and service activation.

use aterm_agent::service_activation::{
    self, NativeBackend as Live, PROTOCOL, REQUEST_DOMAIN as DOMAIN, authorize_rpc, identity,
    signed_outcome, state_dir as state, store_root as root,
};
use aterm_json::{Value, json};
use aterm_secrets::{
    Error, Result,
    client::Paths,
    service::Job,
    service_repair::{self as transaction, Backend, Request},
};
use aterm_update::service_artifact::{SignedRelease, Store};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
};

pub(super) const VALUE_FLAGS: &[&str] = &[
    "--host",
    "--evidence",
    "--image",
    "--job",
    "--nonce",
    "--out",
    "--request",
    "--outcome",
    "--target-node",
    "--target-key",
];
fn fail(e: impl std::fmt::Display) -> Error {
    Error::new(e.to_string())
}
pub(super) fn valid_image_sha256(image: &str) -> bool {
    image.len() == 64
        && image
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn valid_nonce(nonce: &str) -> bool {
    (16..=128).contains(&nonce.len())
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub(super) fn strip_standalone_json(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();
        if VALUE_FLAGS.contains(&token) {
            out.push(args[i].clone());
            if let Some(value) = args.get(i + 1) {
                out.push(value.clone());
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if token != "--json" {
            out.push(args[i].clone());
        }
        i += 1;
    }
    out
}
#[derive(Debug, PartialEq, Eq)]
enum CliInput {
    Help,
    Command(Vec<String>),
}
fn parse_cli_input(args: Vec<OsString>) -> Result<CliInput> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(raw) = iter.next() {
        let token = raw
            .into_string()
            .map_err(|_| fail("arguments must be UTF-8"))?;
        if VALUE_FLAGS.contains(&token.as_str()) {
            out.push(token);
            if let Some(value) = iter.next() {
                out.push(
                    value
                        .into_string()
                        .map_err(|_| fail("arguments must be UTF-8"))?,
                );
            }
            continue;
        }
        if matches!(token.as_str(), "--help" | "-h") || (out.is_empty() && token == "help") {
            return Ok(CliInput::Help);
        }
        if token != "--json" {
            out.push(token);
        }
    }
    Ok(CliInput::Command(out))
}
fn now() -> Result<i64> {
    i64::try_from(aterm_time::unix_millis().ok_or_else(|| fail("clock unavailable"))? / 1000)
        .map_err(fail)
}
fn read(path: &Path) -> Result<Vec<u8>> {
    aterm_secrets::fsx::read_owned(path, 16 * 1024 * 1024)?
        .ok_or_else(|| fail(format!("{} is missing", path.display())))
}
fn value(path: &Path) -> Result<Value> {
    aterm_json::from_slice(&read(path)?).map_err(fail)
}
fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}
fn job(s: &str) -> Result<Job> {
    match s {
        "server" => Ok(Job::Server),
        "subscriber" => Ok(Job::Subscriber),
        _ => Err(fail("job must be server or subscriber")),
    }
}
fn option<'a, S: AsRef<str>>(operands: &'a [S], name: &str) -> Result<&'a str> {
    let mut found = operands
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|pair| pair[0].as_ref() == name);
    let pair = found
        .next()
        .ok_or_else(|| fail(format!("missing {name}")))?;
    if found.next().is_some() {
        return Err(fail(format!("duplicate {name}")));
    }
    Ok(pair[1].as_ref())
}

fn backend(paths: Paths, signed: Option<Value>) -> Live {
    Live::new(paths, signed, aterm_gui::running_build_number())
}

fn observed_value(which: Job, observed: transaction::Observed) -> Value {
    let unit = observed.unit.map_or(Value::Null, |u| {
        Value::Object(aterm_json::Map::from([
            ("program".to_string(), u.program.into()),
            (
                "args".to_string(),
                Value::Array(u.args.into_iter().map(Into::into).collect()),
            ),
            ("definition".to_string(), u.definition.into()),
            ("image".to_string(), u.image.into()),
            ("executable_sha256".to_string(), u.executable_sha256.into()),
        ]))
    });
    Value::Object(aterm_json::Map::from([
        ("job".to_string(), which.label().into()),
        ("unit".to_string(), unit),
        ("off".to_string(), observed.off.into()),
        (
            "pin".to_string(),
            observed.pin.map_or(Value::Null, Into::into),
        ),
    ]))
}

fn prepare_request(
    paths: &Paths,
    job: Job,
    desired: &str,
    nonce: &str,
    requester: &str,
) -> Result<Request> {
    if !valid_image_sha256(desired) {
        return Err(fail("invalid service image sha256"));
    }
    if !valid_nonce(nonce) {
        return Err(fail(
            "service repair nonce must be 16..128 ASCII letters, digits or hyphens",
        ));
    }
    service_activation::prepare_request(
        paths,
        job,
        desired,
        nonce,
        requester,
        aterm_gui::running_build_number(),
    )
}
/// Start only from a GUI or node coordinator, never from a service or a probe verb.
pub(super) fn start_recovery(paths: Paths, state_dir: PathBuf) {
    let started = std::thread::Builder::new()
        .name("aterm-service-recovery".into())
        .spawn(move || {
            let live_context = || -> Result<()> {
                if Paths::live()? != paths || state()? != state_dir {
                    return Err(fail("coordinator context no longer matches live services"));
                }
                if !aterm_secrets::swarm::enabled_in_home(&paths.home)? {
                    return Err(fail("sharing is Off"));
                }
                Ok(())
            };
            if live_context().is_err() {
                return;
            }
            let mut backend = backend(paths.clone(), None);
            for job in [Job::Server, Job::Subscriber] {
                if let Err(why) =
                    transaction::recover_checked(&paths.home, job, &mut backend, live_context)
                {
                    aterm_log::info!("service recovery {}: {why}", job.label());
                }
            }
        });
    if let Err(why) = started {
        aterm_log::info!("service recovery worker did not start: {why}");
    }
}

/// Invoked only after the fleet transport proves a current Drive grant.
pub(super) fn rpc(
    operation: &str,
    payload: &Value,
    requester: &str,
    key: &str,
) -> std::result::Result<Value, String> {
    let perform = || -> Result<Value> {
        let paths = Paths::live()?;
        authorize_rpc(&paths, requester, key)?;
        let word = |key: &str| {
            payload[key]
                .as_str()
                .ok_or_else(|| fail(format!("missing {key}")))
        };
        match operation {
            "inspect" => {
                let which = job(word("job")?)?;
                let mut live = backend(paths, None);
                let observed = live.observe(which)?;
                Ok(observed_value(which, observed))
            }
            "prepare" => Ok(transaction::request_value(&prepare_request(
                &paths,
                job(word("job")?)?,
                word("image")?,
                word("nonce")?,
                requester,
            )?)),
            "activate" => {
                let signed = &payload["signed"];
                let request = transaction::parse_request(&signed["request"])?;
                if request.requester != requester || signed["key"].as_str() != Some(key) {
                    return Err(fail(
                        "signed requester differs from authenticated transport",
                    ));
                }
                let mut live = backend(paths.clone(), Some(signed.clone()));
                Ok(transaction::outcome_value(&transaction::activate(
                    &paths.home,
                    &request,
                    &mut live,
                )?))
            }
            "recover" => {
                let which = job(word("job")?)?;
                let mut live = backend(paths.clone(), None);
                Ok(
                    transaction::recover_checked(&paths.home, which, &mut live, || {
                        authorize_rpc(&paths, requester, key)
                    })?
                    .as_ref()
                    .map(transaction::outcome_value)
                    .unwrap_or_else(|| json!({"state":"clean","job":which.label()})),
                )
            }
            _ => Err(fail("unsupported service repair RPC")),
        }
    };
    let payload = perform().map_err(|e| e.to_string())?;
    let identity = identity().map_err(|e| e.to_string())?;
    let signature = identity.sign_record(
        "aterm-service-rpc-result/1",
        &aterm_json::to_vec(&payload).map_err(|e| e.to_string())?,
    );
    Ok(Value::Object(aterm_json::Map::from([
        ("payload".to_string(), payload),
        ("node".to_string(), identity.node().into()),
        ("key".to_string(), identity.key().hex().into()),
        ("signature".to_string(), hex(&signature).into()),
    ])))
}

fn non_empty_option<'a, S: AsRef<str>>(operands: &'a [S], name: &str) -> Result<&'a str> {
    let v = option(operands, name)?;
    if v.is_empty() {
        return Err(fail(format!("{name} must be non-empty")));
    }
    Ok(v)
}

fn execute(args: &[String]) -> Result<Value> {
    execute_with_live(args, Paths::live)
}

fn execute_with_live(
    args: &[String],
    live_context: impl FnOnce() -> Result<Paths>,
) -> Result<Value> {
    let normalized = strip_standalone_json(args);
    let command = normalized.first().map(String::as_str).ok_or_else(|| {
        fail(
            "expected stage, inspect, prepare, sign, activate, recover, verify-outcome or protocol",
        )
    })?;
    if command == "protocol" {
        if normalized.len() != 1 {
            return Err(fail("protocol takes no options"));
        }
        return Ok(json!({"schema":1,"min":PROTOCOL,"max":PROTOCOL}));
    }
    let allowed: &[&str] = match command {
        "stage" => &["--evidence", "--image"],
        "inspect" | "recover" => &["--job"],
        "prepare" => &["--job", "--image", "--nonce", "--out"],
        "activate" => &["--request"],
        "sign" => &["--request", "--out"],
        "verify-outcome" => &["--request", "--outcome", "--target-node", "--target-key"],
        _ => {
            return Err(fail(
                "expected stage, inspect, prepare, sign, activate, recover, verify-outcome or protocol",
            ));
        }
    };
    let operands = &normalized[1..];
    if !operands.len().is_multiple_of(2)
        || operands
            .as_chunks::<2>()
            .0
            .iter()
            .any(|pair| !allowed.contains(&pair[0].as_str()))
    {
        return Err(fail(
            "unknown option or missing service-repair option value",
        ));
    }
    for required in allowed {
        option(operands, required)?;
    }
    // Offline verification must precede Paths::live, Store, identity and service discovery.
    if command == "verify-outcome" {
        let bounded = |flag, limit| -> Result<Vec<u8>> {
            let path = Path::new(non_empty_option(operands, flag)?);
            aterm_secrets::fsx::read_owned(path, limit)?
                .ok_or_else(|| fail(format!("{} is missing", path.display())))
        };
        let outcome = service_activation::verify_outcome(
            &bounded("--outcome", service_activation::MAX_SIGNED_OUTCOME_BYTES)?,
            &bounded("--request", service_activation::MAX_EXPECTED_REQUEST_BYTES)?,
            non_empty_option(operands, "--target-node")?,
            non_empty_option(operands, "--target-key")?,
        )?;
        let outcome_val = transaction::outcome_value(&outcome);
        return Ok(Value::Object(aterm_json::Map::from([
            ("verified".to_string(), true.into()),
            ("fresh_activation_proven".to_string(), false.into()),
            ("node".to_string(), outcome.request.node.into()),
            ("key".to_string(), option(operands, "--target-key")?.into()),
            ("outcome".to_string(), outcome_val),
        ])));
    }
    match command {
        "stage" => {
            non_empty_option(operands, "--evidence")?;
            non_empty_option(operands, "--image")?;
        }
        "inspect" | "recover" => {
            job(option(operands, "--job")?)?;
        }
        "prepare" => {
            job(option(operands, "--job")?)?;
            if !valid_image_sha256(option(operands, "--image")?) {
                return Err(fail("invalid service image sha256"));
            }
            if !valid_nonce(option(operands, "--nonce")?) {
                return Err(fail(
                    "service repair nonce must be 16..128 ASCII letters, digits or hyphens",
                ));
            }
            non_empty_option(operands, "--out")?;
        }
        "sign" => {
            non_empty_option(operands, "--request")?;
            non_empty_option(operands, "--out")?;
        }
        "activate" => {
            non_empty_option(operands, "--request")?;
        }
        _ => {}
    }
    let paths = live_context()?;
    let _store = Store::new(root(&paths)).map_err(fail)?;
    match command {
        "stage" => {
            let dir = root(&paths);
            let evidence = Path::new(non_empty_option(operands, "--evidence")?);
            let signed = SignedRelease {
                manifest: String::from_utf8(read(&evidence.join("aterm-appcast.toml"))?)
                    .map_err(fail)?,
                manifest_signature: read(&evidence.join("aterm-appcast.toml.sig"))?,
                roster: read(&evidence.join(aterm_update_core::roster::ROSTER_ASSET))?,
                roster_signature: read(
                    &evidence.join(aterm_update_core::roster::ROSTER_SIG_ASSET),
                )?,
            };
            let _authority_lock =
                aterm_update_core::FileLock::acquire(&dir.join("authority.lock"))?;
            let floor = match aterm_secrets::fsx::read_owned(
                &dir.join("authority.json"),
                16 * 1024 * 1024,
            )? {
                Some(bytes) => aterm_json::from_slice::<Value>(&bytes).map_err(fail)?["roster"]
                    .as_u64()
                    .ok_or_else(|| fail("invalid authority floor"))?,
                None => 0,
            };
            let receipt = _store
                .materialize(
                    &signed,
                    Path::new(non_empty_option(operands, "--image")?),
                    &read(&evidence.join("aterm-service-qualification.json"))?,
                    now()?,
                    floor,
                )
                .map_err(fail)?;
            let verified = aterm_update_core::verify_roster(
                aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
                signed.roster,
                &signed.roster_signature,
            )
            .map_err(|e| fail(format!("{e:?}")))?;
            let roster =
                aterm_update_core::Roster::parse(&verified).map_err(|e| fail(format!("{e:?}")))?;
            aterm_secrets::fsx::write_atomic(&dir.join("authority.json"),&aterm_json::to_vec(&json!({"roster":roster.roster_seq,"bytes":verified.as_slice(),"signature":signed.roster_signature})).map_err(fail)?,0o600)?;
            aterm_json::to_value(receipt).map_err(fail)
        }
        "inspect" => {
            let mut live = backend(paths, None);
            let which = job(option(operands, "--job")?)?;
            let observed = live.observe(which)?;
            Ok(observed_value(which, observed))
        }
        "prepare" => {
            let job = job(option(operands, "--job")?)?;
            let desired = option(operands, "--image")?;
            let identity = identity()?;
            let request = prepare_request(
                &paths,
                job,
                desired,
                option(operands, "--nonce")?,
                identity.node(),
            )?;
            let body = transaction::request_value(&request);
            let sig = identity.sign_record(DOMAIN, &aterm_json::to_vec(&body).map_err(fail)?);
            let signed = Value::Object(aterm_json::Map::from([
                ("request".to_string(), body),
                ("key".to_string(), identity.key().hex().into()),
                ("signature".to_string(), hex(&sig).into()),
            ]));
            aterm_secrets::fsx::write_atomic(
                Path::new(non_empty_option(operands, "--out")?),
                &aterm_json::to_vec(&signed).map_err(fail)?,
                0o600,
            )?;
            Ok(signed)
        }
        "sign" => {
            let input = value(Path::new(non_empty_option(operands, "--request")?))?;
            let mut request = transaction::parse_request(input.get("request").unwrap_or(&input))?;
            let identity = identity()?;
            request.requester = identity.node().into();
            let body = transaction::request_value(&request);
            let sig = identity.sign_record(DOMAIN, &aterm_json::to_vec(&body).map_err(fail)?);
            let signed = Value::Object(aterm_json::Map::from([
                ("request".to_string(), body),
                ("key".to_string(), identity.key().hex().into()),
                ("signature".to_string(), hex(&sig).into()),
            ]));
            aterm_secrets::fsx::write_atomic(
                Path::new(non_empty_option(operands, "--out")?),
                &aterm_json::to_vec(&signed).map_err(fail)?,
                0o600,
            )?;
            Ok(signed)
        }
        "activate" => {
            let signed = value(Path::new(non_empty_option(operands, "--request")?))?;
            let request = transaction::parse_request(&signed["request"])?;
            let mut live = backend(paths.clone(), Some(signed));
            let outcome = transaction::activate(&paths.home, &request, &mut live)?;
            signed_outcome(&outcome)
        }
        "recover" => {
            let mut live = backend(paths.clone(), None);
            let which = job(option(operands, "--job")?)?;
            Ok(
                transaction::recover_checked(&paths.home, which, &mut live, || {
                    if aterm_secrets::swarm::enabled_in_home(&paths.home)? {
                        Ok(())
                    } else {
                        Err(fail("sharing is Off"))
                    }
                })?
                .as_ref()
                .map(transaction::outcome_value)
                .unwrap_or_else(|| json!({"state":"clean","job":which.label()})),
            )
        }
        _ => Err(fail(
            "expected stage, inspect, prepare, sign, activate, recover, verify-outcome or protocol",
        )),
    }
}

pub(super) fn run(args: Vec<OsString>) -> ExitCode {
    let args = match parse_cli_input(args) {
        Ok(CliInput::Help) => {
            return aterm_uds::stdout::answer_exit(
                "aterm secrets service-repair",
                concat!(
                    "aterm secrets service-repair stage --evidence DIR --image DIR\n",
                    "aterm secrets service-repair inspect --job server|subscriber\n",
                    "aterm secrets service-repair prepare --job JOB --image SHA256 --nonce NONCE --out FILE\n",
                    "aterm secrets service-repair sign --request FILE --out FILE\n",
                    "aterm secrets service-repair activate --request FILE\n",
                    "aterm secrets service-repair recover --job JOB\n",
                    "aterm secrets service-repair protocol\n",
                    "aterm secrets service-repair verify-outcome --request FILE --outcome FILE --target-node NODE --target-key HEX\n",
                    "aterm secrets service-repair --host HOST inspect|prepare|activate|recover [operation flags]\n",
                    "All operations report JSON. verify-outcome is offline and read-only; FILE request may be plain or signed.\n",
                    "Verification authenticates the exact result/request, not a fresh activation; other operations require current authority.\n"
                ),
                0,
            );
        }
        Ok(CliInput::Command(args)) => args,
        Err(e) => {
            eprintln!("aterm secrets service-repair: {e}");
            return ExitCode::FAILURE;
        }
    };
    let operation = if args.first().is_some_and(|s| s == "--host") {
        args.get(2)
    } else {
        args.first()
    };
    let recovering = operation.is_some_and(|s| s == "recover");
    let result = if args.first().is_some_and(|arg| arg == "--host") {
        super::service_repair_remote::execute(&args).map_err(fail)
    } else {
        execute(&args)
    };
    match result {
        Ok(value) => {
            let passed = value["verified"].as_bool() == Some(true)
                || value["outcome"]["verdict"]
                    .as_str()
                    .or_else(|| value["payload"]["verdict"].as_str())
                    .or_else(|| value["verdict"].as_str())
                    .is_none_or(|v| v == "committed" || (recovering && v == "rolled-back"));
            match aterm_json::to_string(&value) {
                Ok(mut text) => {
                    text.push('\n');
                    aterm_uds::stdout::answer_exit(
                        "aterm secrets service-repair",
                        &text,
                        if passed { 0 } else { 1 },
                    )
                }
                Err(e) => {
                    eprintln!("service repair: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("aterm secrets service-repair: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "service_repair_tests.rs"]
mod tests;
