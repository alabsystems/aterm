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
    let raw = args
        .into_iter()
        .map(|s| s.into_string().map_err(|_| fail("arguments must be UTF-8")))
        .collect::<Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(raw.len());
    let mut iter = raw.into_iter();
    while let Some(token) = iter.next() {
        if VALUE_FLAGS.contains(&token.as_str()) {
            out.push(token);
            if let Some(value) = iter.next() {
                out.push(value);
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
fn option<'a>(operands: &'a [&'a str], name: &str) -> Result<&'a str> {
    let mut found = operands
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|pair| pair[0] == name);
    let pair = found
        .next()
        .ok_or_else(|| fail(format!("missing {name}")))?;
    if found.next().is_some() {
        return Err(fail(format!("duplicate {name}")));
    }
    Ok(pair[1])
}

fn backend(paths: Paths, signed: Option<Value>) -> Live {
    Live::new(paths, signed, aterm_gui::running_build_number())
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
        let mut live = backend(paths.clone(), None);
        match operation {
            "inspect" => {
                let which = job(word("job")?)?;
                let observed = live.observe(which)?;
                Ok(
                    json!({"job":which.label(),"unit":observed.unit.map(|u|json!({"program":u.program,"args":u.args,"definition":u.definition,"image":u.image,"executable_sha256":u.executable_sha256})),"off":observed.off,"pin":observed.pin}),
                )
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
                live.set_signed_request(signed.clone());
                Ok(transaction::outcome_value(&transaction::activate(
                    &paths.home,
                    &request,
                    &mut live,
                )?))
            }
            "recover" => {
                let which = job(word("job")?)?;
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
    Ok(
        json!({"payload":payload,"node":identity.node(),"key":identity.key().hex(),"signature":hex(&signature)}),
    )
}

fn non_empty_option<'a>(operands: &'a [&'a str], name: &str) -> Result<&'a str> {
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
    let operands: Vec<&str> = normalized[1..].iter().map(String::as_str).collect();
    if !operands.len().is_multiple_of(2)
        || operands
            .as_chunks::<2>()
            .0
            .iter()
            .any(|pair| !allowed.contains(&pair[0]))
    {
        return Err(fail(
            "unknown option or missing service-repair option value",
        ));
    }
    for required in allowed {
        option(&operands, required)?;
    }
    // Offline verification must precede Paths::live, Store, identity and service discovery.
    if command == "verify-outcome" {
        let bounded = |flag, limit| -> Result<Vec<u8>> {
            let path = Path::new(non_empty_option(&operands, flag)?);
            aterm_secrets::fsx::read_owned(path, limit)?
                .ok_or_else(|| fail(format!("{} is missing", path.display())))
        };
        let outcome = service_activation::verify_outcome(
            &bounded("--outcome", service_activation::MAX_SIGNED_OUTCOME_BYTES)?,
            &bounded("--request", service_activation::MAX_EXPECTED_REQUEST_BYTES)?,
            non_empty_option(&operands, "--target-node")?,
            non_empty_option(&operands, "--target-key")?,
        )?;
        return Ok(json!({"verified":true,"fresh_activation_proven":false,
            "node":outcome.request.node,"key":option(&operands,"--target-key")?,
            "outcome":transaction::outcome_value(&outcome)}));
    }
    match command {
        "stage" => {
            non_empty_option(&operands, "--evidence")?;
            non_empty_option(&operands, "--image")?;
        }
        "inspect" | "recover" => {
            job(option(&operands, "--job")?)?;
        }
        "prepare" => {
            job(option(&operands, "--job")?)?;
            if !valid_image_sha256(option(&operands, "--image")?) {
                return Err(fail("invalid service image sha256"));
            }
            if !valid_nonce(option(&operands, "--nonce")?) {
                return Err(fail(
                    "service repair nonce must be 16..128 ASCII letters, digits or hyphens",
                ));
            }
            non_empty_option(&operands, "--out")?;
        }
        "sign" => {
            non_empty_option(&operands, "--request")?;
            non_empty_option(&operands, "--out")?;
        }
        "activate" => {
            non_empty_option(&operands, "--request")?;
        }
        _ => {}
    }
    let paths = live_context()?;
    let store = Store::new(root(&paths)).map_err(fail)?;
    match command {
        "stage" => {
            let evidence = Path::new(non_empty_option(&operands, "--evidence")?);
            let signed = SignedRelease {
                manifest: String::from_utf8(read(&evidence.join("aterm-appcast.toml"))?)
                    .map_err(fail)?,
                manifest_signature: read(&evidence.join("aterm-appcast.toml.sig"))?,
                roster: read(&evidence.join(aterm_update_core::roster::ROSTER_ASSET))?,
                roster_signature: read(
                    &evidence.join(aterm_update_core::roster::ROSTER_SIG_ASSET),
                )?,
            };
            let dir = root(&paths);
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
            let receipt = store
                .materialize(
                    &signed,
                    Path::new(non_empty_option(&operands, "--image")?),
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
            let mut live = backend(paths.clone(), None);
            let which = job(option(&operands, "--job")?)?;
            let observed = live.observe(which)?;
            Ok(
                json!({"job":which.label(),"unit":observed.unit.map(|u|json!({"program":u.program,"args":u.args,"definition":u.definition,"image":u.image,"executable_sha256":u.executable_sha256})),"off":observed.off,"pin":observed.pin}),
            )
        }
        "prepare" => {
            let job = job(option(&operands, "--job")?)?;
            let desired = option(&operands, "--image")?;
            let identity = identity()?;
            let request = prepare_request(
                &paths,
                job,
                desired,
                option(&operands, "--nonce")?,
                identity.node(),
            )?;
            let body = transaction::request_value(&request);
            let sig = identity.sign_record(DOMAIN, &aterm_json::to_vec(&body).map_err(fail)?);
            let signed = json!({"request":body,"key":identity.key().hex(),"signature":hex(&sig)});
            aterm_secrets::fsx::write_atomic(
                Path::new(non_empty_option(&operands, "--out")?),
                &aterm_json::to_vec(&signed).map_err(fail)?,
                0o600,
            )?;
            Ok(signed)
        }
        "sign" => {
            let input = value(Path::new(non_empty_option(&operands, "--request")?))?;
            let mut request = transaction::parse_request(input.get("request").unwrap_or(&input))?;
            let identity = identity()?;
            request.requester = identity.node().into();
            let body = transaction::request_value(&request);
            let sig = identity.sign_record(DOMAIN, &aterm_json::to_vec(&body).map_err(fail)?);
            let signed = json!({"request":body,"key":identity.key().hex(),"signature":hex(&sig)});
            aterm_secrets::fsx::write_atomic(
                Path::new(non_empty_option(&operands, "--out")?),
                &aterm_json::to_vec(&signed).map_err(fail)?,
                0o600,
            )?;
            Ok(signed)
        }
        "activate" => {
            let signed = value(Path::new(non_empty_option(&operands, "--request")?))?;
            let request = transaction::parse_request(&signed["request"])?;
            let mut live = backend(paths.clone(), Some(signed));
            let outcome = transaction::activate(&paths.home, &request, &mut live)?;
            signed_outcome(&outcome)
        }
        "recover" => {
            let mut live = backend(paths.clone(), None);
            let which = job(option(&operands, "--job")?)?;
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
mod tests {
    use super::{CliInput, PROTOCOL, execute, execute_with_live, option, parse_cli_input};
    use std::ffi::OsString;

    #[test]
    fn paired_operands_do_not_confuse_option_values_with_flag_names() {
        let operands = ["--job", "server", "--nonce", "--job"];
        assert_eq!(option(&operands, "--job").unwrap(), "server");
        assert_eq!(option(&operands, "--nonce").unwrap(), "--job");
        let missing_second = ["--evidence", "--image"];
        assert!(option(&missing_second, "--image").is_err());
    }

    #[test]
    fn offline_verifier_accepts_historical_rollback_without_resolving_live_context() {
        use aterm_json::json;
        use aterm_secrets::service_repair as transaction;
        let node = "0123456789abcdef0123456789abcdef";
        let own = aterm_net::fleet::NodeIdentity::from_seed(node, &[19; 32]).unwrap();
        let request = json!({"node":node,"requester":node,"nonce":"offline-cli-verifier-test",
            "job":"systems.alab.aterm-secrets-server","current_image":"old","desired_image":"new",
            "precondition":{"unit":null,"off":false,"pin":null},
            "unit":{"program":"/inert/aterm","args":["secrets","serve"],"definition":"inert",
                "image":"new","executable_sha256":"inert"},
            "health":{"vault":"team","signer":"expected","records":["record"],"frontier":7},
            "required_protocols":[1]});
        let outcome = transaction::Outcome {
            request: transaction::parse_request(&request).unwrap(),
            verdict: transaction::Verdict::RolledBack,
            detail: "historical rollback receipt".into(),
        };
        let body = transaction::outcome_value(&outcome);
        let sig = own.sign_record(
            aterm_agent::service_activation::OUTCOME_DOMAIN,
            &aterm_json::to_vec(&body).unwrap(),
        );
        let signed =
            json!({"outcome":body,"node":node,"key":own.key().hex(),"signature":super::hex(&sig)});
        let dir = aterm_tempfile::tempdir().unwrap();
        let request_path = dir.path().join("request.json");
        let outcome_path = dir.path().join("outcome.json");
        for (path, value) in [(&request_path, &request), (&outcome_path, &signed)] {
            aterm_secrets::fsx::write_atomic(path, &aterm_json::to_vec(value).unwrap(), 0o600)
                .unwrap();
        }
        let args = [
            "verify-outcome".into(),
            "--target-node".into(),
            node.into(),
            "--target-key".into(),
            own.key().hex(),
            "--request".into(),
            request_path.to_str().unwrap().into(),
            "--outcome".into(),
            outcome_path.to_str().unwrap().into(),
        ];
        let verified =
            execute_with_live(&args, || panic!("offline verifier consulted live context")).unwrap();
        assert_eq!(verified["verified"].as_bool(), Some(true));
        assert_eq!(verified["fresh_activation_proven"].as_bool(), Some(false));
        assert_eq!(verified["outcome"], body);
        assert_eq!(
            super::run(args.iter().map(|s| OsString::from(s.as_str())).collect()),
            std::process::ExitCode::SUCCESS
        );
        aterm_secrets::fsx::write_atomic(
            &outcome_path,
            &vec![b' '; aterm_agent::service_activation::MAX_SIGNED_OUTCOME_BYTES + 1],
            0o600,
        )
        .unwrap();
        assert!(
            execute_with_live(&args, || panic!("oversized input consulted live context")).is_err()
        );
    }

    /// Standalone `--json` and `--help` tokens normalize only at flag boundaries, while `--json`
    /// or `--help` passed as a paired option value is preserved verbatim, `protocol` rejects
    /// trailing arguments, and empty local path options refuse before touching live paths.
    /// NEGATIVE CONTROL: unfiltered `.filter(|s| *s != "--json")` stripped `--out --json`'s value
    /// and `args.iter().any("--help")` treated `--request --help` as help instead of an option.
    #[test]
    fn cli_boundaries_preserve_paired_values_and_reject_trailing_or_empty_local_options() {
        let os = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse_cli_input(os(&["--help"])).unwrap(), CliInput::Help);
        assert_eq!(
            parse_cli_input(os(&["--json", "-h"])).unwrap(),
            CliInput::Help
        );
        assert_eq!(
            parse_cli_input(os(&["--host", "m2", "inspect", "--help"])).unwrap(),
            CliInput::Help
        );
        assert_eq!(
            parse_cli_input(os(&["activate", "--request", "--help"])).unwrap(),
            CliInput::Command(vec!["activate".into(), "--request".into(), "--help".into()])
        );
        assert_eq!(
            parse_cli_input(os(&[
                "--json", "--host", "m2", "--json", "recover", "--job", "server", "--json"
            ]))
            .unwrap(),
            CliInput::Command(vec![
                "--host".into(),
                "m2".into(),
                "recover".into(),
                "--job".into(),
                "server".into()
            ])
        );
        assert_eq!(
            parse_cli_input(os(&["sign", "--request", "in.json", "--out", "--json"])).unwrap(),
            CliInput::Command(vec![
                "sign".into(),
                "--request".into(),
                "in.json".into(),
                "--out".into(),
                "--json".into()
            ])
        );
        assert_eq!(
            execute(&["protocol".into(), "--json".into()]).unwrap()["max"].as_u64(),
            Some(PROTOCOL)
        );
        assert!(execute(&["protocol".into(), "--bogus".into()]).is_err());
        assert!(
            execute(&[
                "stage".into(),
                "--evidence".into(),
                String::new(),
                "--image".into(),
                "/tmp/image".into()
            ])
            .is_err()
        );
        assert!(execute(&["activate".into(), "--request".into(), String::new()]).is_err());
        assert!(
            execute(&[
                "sign".into(),
                "--request".into(),
                "/tmp/in.json".into(),
                "--out".into(),
                String::new()
            ])
            .is_err()
        );
        assert!(
            execute(&[
                "prepare".into(),
                "--job".into(),
                "server".into(),
                "--image".into(),
                "not-hex".into(),
                "--nonce".into(),
                "valid-nonce-123456".into(),
                "--out".into(),
                "/tmp/out.json".into()
            ])
            .is_err()
        );
    }
}
