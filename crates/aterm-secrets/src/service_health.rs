// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Service health evidence from authenticated vault data and a subscriber's own round.

use crate::{
    Error, Result,
    client::{self, Paths},
    service_repair::HealthRequirement,
};
use aterm_json::json;
use std::path::PathBuf;

fn challenge(paths: &Paths) -> PathBuf {
    paths.dir.join("service-health-challenge")
}
fn response(paths: &Paths) -> PathBuf {
    paths.dir.join("service-health-response.json")
}
fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

/// Resolve the existing server arguments for the configured vault without changing its
/// store or listener.
/// # Errors
/// The vault name is invalid or differs from the configured vault, configuration cannot be
/// read, or the unit does not contain supported server arguments.
pub fn server_address(paths: &Paths, args: &[String], vault: &str) -> Result<(PathBuf, String)> {
    if !crate::envelope::valid_name(vault) {
        return Err(Error::new("invalid service health vault name"));
    }
    let configured =
        client::config(paths)?.ok_or_else(|| Error::new("no configured service vault"))?;
    if configured.name != vault {
        return Err(Error::new(
            "service health vault differs from configured vault",
        ));
    }
    let (store, url) = crate::cli::served_by(paths, args)
        .ok_or_else(|| Error::new("unsupported server arguments"))?;
    let (origin, _) = url
        .rsplit_once("/v1/")
        .ok_or_else(|| Error::new("unsupported local service URL"))?;
    Ok((store, format!("{origin}/v1/{vault}")))
}

/// Start a fresh local challenge before restarting a subscriber.
/// # Errors
/// The private challenge cannot be persisted.
pub fn prepare(paths: &Paths) -> Result<String> {
    crate::fsx::ensure_private_dir(&paths.dir)?;
    let mut bytes = [0; 32];
    aterm_uds::rand::fill(&mut bytes).map_err(|e| Error::new(e.to_string()))?;
    let nonce = hex(&bytes);
    crate::fsx::write_atomic(&challenge(paths), nonce.as_bytes(), 0o600)?;
    Ok(nonce)
}

/// Record only a successful subscriber round whose signed vault is materialized.
/// Called by the subscriber itself, so a separate repair process cannot mistake its
/// own successful fetch for proof that the installed subscriber applied the vault.
pub(crate) fn after_round(paths: &Paths) -> Result<()> {
    let Some(nonce) = crate::fsx::read_private(&challenge(paths), 128)? else {
        return Ok(());
    };
    let version = hex(&client::service_health_version(paths)?);
    if answered_version(&response(paths), nonce.as_slice(), &version)? {
        return Ok(());
    }
    if !client::materialization_ready(paths)? {
        return Err(Error::new("subscriber materialization is incomplete"));
    }
    let config = client::config(paths)?.ok_or_else(|| Error::new("subscriber has no vault"))?;
    let envelope = crate::fsx::read_private(&paths.cache_file(), crate::envelope::MAX_ENVELOPE)?
        .ok_or_else(|| Error::new("subscriber has no verified envelope"))?;
    let envelope = verify_subscriber_envelope(envelope.as_slice(), &config)?;
    let records = client::load_records(paths)?;
    let executable = std::env::current_exe()?;
    let (exe_bytes, _) = aterm_uds::atomic_file::read_regular(&executable, 512 * 1024 * 1024)?;
    let image = hex(&aterm_digest::Sha256::digest(exe_bytes));
    if hex(&client::service_health_version(paths)?) != version {
        return Err(Error::new(
            "subscriber state changed while producing health evidence",
        ));
    }
    let value = json!({"nonce":std::str::from_utf8(nonce.as_slice()).map_err(|e|Error::new(e.to_string()))?,
        "pid":std::process::id(),"image":image,"vault":envelope.name,"signer":hex(&envelope.signer),
        "sequence":envelope.seq,"records":records.inventory().iter().map(|id|hex(id)).collect::<Vec<_>>(),"version":version});
    crate::fsx::write_atomic(
        &response(paths),
        &aterm_json::to_vec(&value).map_err(|e| Error::new(e.to_string()))?,
        0o600,
    )
}

fn verify_subscriber_envelope(
    bytes: &[u8],
    config: &client::Config,
) -> Result<crate::envelope::Envelope> {
    let signer = config
        .signer
        .ok_or_else(|| Error::new("subscriber vault is unsigned"))?;
    let envelope = crate::envelope::parse(bytes)?;
    envelope.verify_signature()?;
    envelope.check_pin(&signer, &config.followed)?;
    if envelope.name != config.name {
        return Err(Error::new(
            "subscriber envelope belongs to another configured vault",
        ));
    }
    Ok(envelope)
}

/// Check evidence produced by the running subscriber after the challenge.
/// # Errors
/// No matching round was recorded, or its image, vault or signed frontier differs.
pub fn subscriber(
    paths: &Paths,
    nonce: &str,
    pid: u32,
    image: &str,
    health: &HealthRequirement,
) -> Result<()> {
    let bytes = crate::fsx::read_private(&response(paths), 8 * 1024 * 1024)?
        .ok_or_else(|| Error::new("subscriber has not answered the health challenge"))?;
    let value: aterm_json::Value =
        aterm_json::from_slice(bytes.as_slice()).map_err(|e| Error::new(e.to_string()))?;
    let records = value["records"]
        .as_array()
        .ok_or_else(|| Error::new("subscriber health lacks frontier"))?;
    // Name only the failed check: signed repair outcomes retain this diagnostic,
    // but must not copy response contents or expected identity values into it.
    let mismatch = if nonce.is_empty() || value["nonce"].as_str() != Some(nonce) {
        Some("challenge")
    } else if value["pid"].as_u64() != Some(u64::from(pid)) {
        Some("process")
    } else if value["image"].as_str() != Some(image) {
        Some("image")
    } else if value["vault"].as_str() != Some(&health.vault) {
        Some("vault")
    } else if value["signer"].as_str() != Some(&health.signer) {
        Some("signer")
    } else if value["sequence"]
        .as_u64()
        .is_none_or(|seq| seq < health.frontier)
    {
        Some("sequence")
    } else if health.records.is_empty()
        || health
            .records
            .iter()
            .any(|id| !records.iter().any(|r| r.as_str() == Some(id)))
    {
        Some("records")
    } else {
        None
    };
    if let Some(field) = mismatch {
        return Err(Error::new(format!("subscriber health {field} mismatch")));
    }
    Ok(())
}

/// Prove a server supplies the requested signed vault and complete membership frontier.
/// # Errors
/// Transport, signature, vault identity or frontier verification failed.
pub fn server(url: &str, health: &HealthRequirement) -> Result<()> {
    server_reads_within(url, health, std::time::Duration::from_secs(5))
}

/// [`server`] with each of its two reads bounded by `within`: a test whose own server must
/// answer passes its hang detector (`aterm_spec::wait::MUST_HAPPEN`).
/// # Errors
/// As [`server`].
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn server_within(
    url: &str,
    health: &HealthRequirement,
    within: std::time::Duration,
) -> Result<()> {
    server_reads_within(url, health, within)
}

fn server_reads_within(
    url: &str,
    health: &HealthRequirement,
    within: std::time::Duration,
) -> Result<()> {
    let crate::transport::Fetched::Body(bytes) = crate::transport::fetch_within(url, None, within)?
    else {
        return Err(Error::new("server did not supply its signed vault"));
    };
    let envelope = crate::envelope::parse(&bytes)?;
    envelope.verify_signature()?;
    if envelope.name != health.vault
        || hex(&envelope.signer) != health.signer
        || envelope.seq < health.frontier
    {
        return Err(Error::new("server vault identity or sequence mismatch"));
    }
    let bytes = crate::transport::fetch_records(url, &[], None, within, None)?;
    let mut store = crate::membership_log::Store::for_vault(&health.vault)?;
    store.ingest(&bytes)?;
    let inventory = store.complete_inventory();
    if health.records.is_empty()
        || health
            .records
            .iter()
            .any(|id| !inventory.iter().any(|r| hex(r) == *id))
    {
        return Err(Error::new("server signed membership frontier is stale"));
    }
    Ok(())
}

/// Start a challenge for a server which must first bind its configured listener.
/// # Errors
/// The private challenge cannot be persisted.
pub fn prepare_server(paths: &Paths) -> Result<String> {
    crate::fsx::ensure_private_dir(&paths.dir)?;
    let mut bytes = [0; 32];
    aterm_uds::rand::fill(&mut bytes).map_err(|e| Error::new(e.to_string()))?;
    let nonce = hex(&bytes);
    crate::fsx::write_atomic(
        &paths.dir.join("server-health-challenge"),
        nonce.as_bytes(),
        0o600,
    )?;
    Ok(nonce)
}

/// Written only by a bound server's supervisor, never by a process waiting to bind.
pub(crate) fn bound_server(paths: &Paths, address: std::net::SocketAddr) -> Result<()> {
    let Some(nonce) = crate::fsx::read_private(&paths.dir.join("server-health-challenge"), 128)?
    else {
        return Ok(());
    };
    if already_answered(
        &paths.dir.join("server-health-response.json"),
        nonce.as_slice(),
    )? {
        return Ok(());
    }
    let value = json!({"nonce":std::str::from_utf8(nonce.as_slice()).map_err(|e|Error::new(e.to_string()))?,"pid":std::process::id(),"port":address.port()});
    crate::fsx::write_atomic(
        &paths.dir.join("server-health-response.json"),
        &aterm_json::to_vec(&value).map_err(|e| Error::new(e.to_string()))?,
        0o600,
    )
}

/// Bind server health to the service process which actually acquired its listener.
/// # Errors
/// The service has not acknowledged this challenge after binding the expected port.
pub fn server_process(paths: &Paths, nonce: &str, pid: u32, port: u16) -> Result<()> {
    let bytes = crate::fsx::read_private(&paths.dir.join("server-health-response.json"), 4096)?
        .ok_or_else(|| Error::new("server has not acknowledged its bound listener"))?;
    let value: aterm_json::Value =
        aterm_json::from_slice(bytes.as_slice()).map_err(|e| Error::new(e.to_string()))?;
    if nonce.is_empty()
        || port == 0
        || value["nonce"].as_str() != Some(nonce)
        || value["pid"].as_u64() != Some(u64::from(pid))
        || value["port"].as_u64() != Some(u64::from(port))
    {
        return Err(Error::new("server listener health process mismatch"));
    }
    Ok(())
}

fn already_answered(path: &std::path::Path, nonce: &[u8]) -> Result<bool> {
    answered(path, nonce, None)
}

fn answered_version(path: &std::path::Path, nonce: &[u8], version: &str) -> Result<bool> {
    answered(path, nonce, Some(version))
}

fn answered(path: &std::path::Path, nonce: &[u8], version: Option<&str>) -> Result<bool> {
    let Some(bytes) = crate::fsx::read_private(path, 8 * 1024 * 1024)? else {
        return Ok(false);
    };
    let Ok(value) = aterm_json::from_slice::<aterm_json::Value>(bytes.as_slice()) else {
        return Ok(false);
    };
    Ok(value["nonce"].as_str().map(str::as_bytes) == Some(nonce)
        && value["pid"].as_u64() == Some(u64::from(std::process::id()))
        && version.is_none_or(|version| value["version"].as_str() == Some(version)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> HealthRequirement {
        HealthRequirement {
            vault: "repair-health".to_owned(),
            signer: "a".repeat(64),
            records: vec!["b".repeat(64), "c".repeat(64)],
            frontier: 17,
        }
    }

    fn write_response(paths: &Paths, name: &str, value: &aterm_json::Value) {
        crate::fsx::write_atomic(
            &paths.dir.join(name),
            &aterm_json::to_vec(value).unwrap(),
            0o600,
        )
        .unwrap();
    }

    #[test]
    fn subscriber_health_binds_challenge_process_image_vault_and_complete_frontier() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        let health = health();
        let nonce = prepare(&paths).unwrap();
        let image = "d".repeat(64);
        let valid = json!({"nonce":nonce,"pid":1234,"image":image,"vault":health.vault,
            "signer":health.signer,"sequence":18,"records":health.records});
        assert!(subscriber(&paths, &nonce, 1234, &image, &health).is_err());
        write_response(&paths, "service-health-response.json", &valid);
        subscriber(&paths, &nonce, 1234, &image, &health).unwrap();
        for (field, bad) in [
            ("nonce", json!("previous challenge")),
            ("pid", json!(1235)),
            ("image", json!("e".repeat(64))),
            ("vault", json!("another-vault")),
            ("signer", json!("f".repeat(64))),
            ("sequence", json!(16)),
            ("records", json!([health.records[0]])),
        ] {
            let mut changed = valid.clone();
            changed.as_object_mut().unwrap().insert(field.into(), bad);
            write_response(&paths, "service-health-response.json", &changed);
            assert!(
                subscriber(&paths, &nonce, 1234, &image, &health).is_err(),
                "accepted wrong {field}"
            );
        }
        let next = prepare(&paths).unwrap();
        assert_ne!(nonce, next);
        write_response(&paths, "service-health-response.json", &valid);
        assert!(subscriber(&paths, &next, 1234, &image, &health).is_err());
    }

    #[test]
    fn server_challenge_is_bound_to_process_port_and_written_once() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        let nonce = prepare_server(&paths).unwrap();
        let pid = std::process::id();
        assert!(server_process(&paths, &nonce, pid, 7100).is_err());
        bound_server(&paths, "127.0.0.1:7100".parse().unwrap()).unwrap();
        server_process(&paths, &nonce, pid, 7100).unwrap();
        assert!(server_process(&paths, "stale challenge", pid, 7100).is_err());
        assert!(server_process(&paths, &nonce, pid.wrapping_add(1), 7100).is_err());
        assert!(server_process(&paths, &nonce, pid, 7101).is_err());
        let marker = json!({"nonce":nonce,"pid":pid,"port":7100,"retained":"first bound proof"});
        write_response(&paths, "server-health-response.json", &marker);
        bound_server(&paths, "127.0.0.1:7101".parse().unwrap()).unwrap();
        let retained: aterm_json::Value = aterm_json::from_slice(
            &std::fs::read(paths.dir.join("server-health-response.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(retained, marker);
        let fresh = prepare_server(&paths).unwrap();
        bound_server(&paths, "127.0.0.1:7101".parse().unwrap()).unwrap();
        server_process(&paths, &fresh, pid, 7101).unwrap();
        assert!(server_process(&paths, &nonce, pid, 7100).is_err());
    }

    #[test]
    fn completed_subscriber_challenge_does_not_hash_or_write_each_round() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        let nonce = prepare(&paths).unwrap();
        let version = hex(&client::service_health_version(&paths).unwrap());
        let marker = json!({"nonce":nonce,"pid":std::process::id(),"retained":"completed round","version":version});
        write_response(&paths, "service-health-response.json", &marker);
        // No vault is configured: a fresh round cannot pass materialization or envelope checks.
        // The already completed process/challenge returns before those reads or any new write.
        after_round(&paths).unwrap();
        let retained: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(response(&paths)).unwrap()).unwrap();
        assert_eq!(retained, marker);
        crate::fsx::write_atomic(&paths.state_file(), b"seq = 2\n", 0o600).unwrap();
        assert!(
            after_round(&paths).is_err(),
            "new progress must not reuse the old answer"
        );
        prepare(&paths).unwrap();
        assert!(after_round(&paths).is_err());
    }

    #[test]
    fn subscriber_health_version_tracks_progress_but_not_heartbeat_timestamps() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        let write = |text: &str| {
            crate::fsx::write_atomic(&paths.state_file(), text.as_bytes(), 0o600).unwrap()
        };
        write("seq = 1\nheard = 10\n");
        let first = client::service_health_version(&paths).unwrap();
        write("seq = 1\nheard = 11\n");
        assert_eq!(client::service_health_version(&paths).unwrap(), first);
        for text in [
            "seq = 2\n".to_owned(),
            format!("seq = 1\ndigest = {:?}\n", "a".repeat(64)),
            format!("seq = 1\nrecords_digest = {:?}\n", "b".repeat(64)),
            format!("seq = 1\nsource_identity = {:?}\n", "c".repeat(64)),
            "seq = 1\nfailed = true\n".to_owned(),
            "seq = 1\nmembership = \"revoked\"\n".to_owned(),
        ] {
            write(&text);
            assert_ne!(
                client::service_health_version(&paths).unwrap(),
                first,
                "{text}"
            );
        }
    }

    #[test]
    fn subscriber_health_refuses_another_vault_signed_by_the_same_key() {
        let seed = [37; 32];
        let config = client::Config {
            source: crate::transport::Source::Path(PathBuf::from("/unused/repair-health.vault")),
            mirrors: Vec::new(),
            name: "repair-health".to_owned(),
            signer: Some(crate::envelope::signer_public(&seed).unwrap()),
            followed: Vec::new(),
            host: "test".to_owned(),
        };
        let signed = |name: &str| {
            crate::envelope::sign(
                &crate::envelope::Unsigned {
                    name: name.to_owned(),
                    seq: 1,
                    time: 1,
                    handoff: Vec::new(),
                    parents: Vec::new(),
                    lockers: Vec::new(),
                },
                &seed,
            )
            .unwrap()
        };
        verify_subscriber_envelope(&signed("repair-health"), &config).unwrap();
        assert!(verify_subscriber_envelope(&signed("other-vault"), &config).is_err());
        let mut forged = signed("repair-health");
        let at = forged.iter().position(|&b| b == b'1').unwrap();
        forged[at] = b'2';
        assert!(verify_subscriber_envelope(&forged, &config).is_err());
    }

    #[test]
    fn server_health_address_uses_only_a_validated_configured_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        crate::fsx::write_atomic(
            &paths.dir.join("source.toml"),
            b"path = '/unused/team.vault'\nname = 'team'\nhost = 'test'\n",
            0o600,
        )
        .unwrap();
        let args = [
            "secrets",
            "serve",
            "--listen",
            "[::]:7484",
            "--store",
            "~/vault-store",
        ]
        .map(str::to_owned);
        let (store, url) = server_address(&paths, &args, "team").unwrap();
        assert_eq!(store, root.path().join("vault-store"));
        assert_eq!(url, "http://[::1]:7484/v1/team");
        for wrong in [
            "alab",
            "other-vault",
            "../team",
            "team?query",
            "team/next",
            "",
        ] {
            assert!(
                server_address(&paths, &args, wrong).is_err(),
                "accepted {wrong:?}"
            );
        }
    }

    #[test]
    fn server_health_verifies_real_signed_vault_and_membership_frontier() {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = root.path().join("server");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = crate::server::start(
            listener,
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: store.clone(),
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(100),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/team", running.addr);
        let paths = Paths::for_home(&root.path().join("admin"));
        let result = (|| -> Result<Vec<bool>> {
            client::create(&paths, &url, None, Some("health-admin"))?;
            client::seed_vault_genesis(&paths, &root.path().join("team.genesis"))?;
            client::sync(&paths, None)?;
            let args = [
                "secrets".to_owned(),
                "serve".to_owned(),
                "--listen".to_owned(),
                format!("0.0.0.0:{}", running.addr.port()),
                "--store".to_owned(),
                store.to_str().unwrap().to_owned(),
            ];
            let (health_store, health_url) = server_address(&paths, &args, "team")?;
            assert_eq!(health_store, store);
            assert_eq!(health_url, url);
            assert!(server_address(&paths, &args, "alab").is_err());
            let bytes =
                crate::fsx::read_private(&paths.cache_file(), crate::envelope::MAX_ENVELOPE)?
                    .unwrap();
            let envelope = crate::envelope::parse(bytes.as_slice())?;
            let records = client::load_records(&paths)?;
            let requirement = HealthRequirement {
                vault: envelope.name.clone(),
                signer: hex(&envelope.signer),
                frontier: envelope.seq,
                records: records.inventory().iter().map(|id| hex(id)).collect(),
            };
            assert!(!requirement.records.is_empty());
            let valid = server(&health_url, &requirement).is_ok();
            let nonce = prepare(&paths)?;
            after_round(&paths)?;
            let before: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(response(&paths))?)
                    .map_err(|e| Error::new(e.to_string()))?;
            let admin = client::admin_open(&paths)?;
            let mut doc = admin.doc.clone();
            doc.set_entry(
                &crate::text::Audience::Everyone,
                "health-progress",
                b"new round",
            )
            .map_err(|error| Error::new(error.to_string()))?;
            client::admin_publish(&paths, &admin, &doc)?;
            after_round(&paths)?;
            let after: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(response(&paths))?)
                    .map_err(|e| Error::new(e.to_string()))?;
            assert_eq!(after["nonce"].as_str(), Some(nonce.as_str()));
            assert_ne!(before["version"], after["version"]);
            assert_ne!(before["sequence"], after["sequence"]);
            let mut wrong_vault = requirement.clone();
            wrong_vault.vault = "another-vault".to_owned();
            let mut wrong_signer = requirement.clone();
            wrong_signer.signer = "0".repeat(64);
            let mut stale = requirement.clone();
            stale.frontier = after["sequence"].as_u64().unwrap() + 1;
            let mut missing = requirement.clone();
            missing.records.push("0".repeat(64));
            Ok(vec![
                valid,
                server(&url, &wrong_vault).is_err(),
                server(&url, &wrong_signer).is_err(),
                server(&url, &stale).is_err(),
                server(&url, &missing).is_err(),
            ])
        })();
        running.stop();
        assert_eq!(result.unwrap(), vec![true; 5]);
    }
}
