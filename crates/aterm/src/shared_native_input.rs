// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit root intake. A key publication and its ordinary proposal attachment
//! are recoverable separately; neither issues preparation or execution rights.

use super::*;
use aterm_db::accounts::proposals::Proposals;
use aterm_uds::secret::Secret;
use std::io::Read;

#[path = "shared_native_import.rs"]
mod imported;

const PENDING: &str = "native-key-intake.age";
const MAX: usize = 16 * 1024;

pub(super) fn entry(scope: &aterm_db::table::Scope, key: &str, provider: Provider) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/native-input-entry/1\0");
    hash.update(scope.digest());
    hash.update(provider.name().as_bytes());
    hash.update([0]);
    hash.update(key.as_bytes());
    let mut out = String::with_capacity(63);
    out.push_str(aterm_secrets::entry_class::NATIVE_API_KEY_PREFIX);
    aterm_codec::hex::push_lower(&mut out, &hash.finalize()[..24]);
    out
}

pub(super) fn pending(paths: &Paths) -> Result<bool, String> {
    Ok(fsx::read_private(&paths.dir.join(PENDING), 128 * 1024)
        .map_err(|e| e.to_string())?
        .is_some())
}

fn context(config: &Config) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/native-input-context/1\0");
    let mut field = |tag: u8, bytes: &[u8]| {
        hash.update([tag]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    match &config.source {
        aterm_secrets::transport::Source::Url(url) => field(0, url.as_bytes()),
        aterm_secrets::transport::Source::Path(path) => field(1, path.as_os_str().as_bytes()),
    }
    field(2, config.name.as_bytes());
    field(
        3,
        config
            .signer
            .as_ref()
            .map_or(&[], |signer| signer.as_slice()),
    );
    for signer in &config.followed {
        field(4, signer);
    }
    aterm_codec::hex::encode_lower(&hash.finalize())
}

fn hex_eq(text: Option<&str>, expected: &[u8; 32]) -> bool {
    let Some(text) = text else {
        return false;
    };
    let mut decoded = [0u8; 32];
    !text.bytes().any(|b| b.is_ascii_uppercase())
        && aterm_codec::hex::decode_into(text.as_bytes(), &mut decoded)
        && decoded == *expected
}

fn plan(paths: &Paths, deadline: Instant) -> Result<Option<(Value, Secret)>, String> {
    let Some(bytes) =
        fsx::read_private(&paths.dir.join(PENDING), 128 * 1024).map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let identity = client::read_machine_identity(paths)
        .map_err(|e| e.to_string())?
        .ok_or("native intake identity is unavailable")?;
    let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::required_until(paths, deadline)
        .map_err(|e| e.to_string())?;
    let request = bootstrap.request().map_err(|e| e.to_string())?;
    let plain = client::open_account_input(
        &bootstrap.trust().authority,
        request.binding(),
        &identity,
        bytes.as_slice(),
    )
    .map_err(|_| "native intake has no current root/device authorization")?;
    let count = plain
        .as_slice()
        .get(..4)
        .map(|n| u32::from_le_bytes(n.try_into().unwrap()) as usize)
        .ok_or("native intake is truncated")?;
    if count > 64 * 1024 || plain.as_slice().len() < 4 + count {
        return Err("native intake metadata exceeds its bound".into());
    }
    let value: Value = aterm_json::from_slice(&plain.as_slice()[4..4 + count])
        .map_err(|_| "native input attachment is malformed")?;
    if value["schema"].as_u64() != Some(1)
        || value.as_object().is_none_or(|v| v.len() != 11)
        || value["key"]
            .as_str()
            .is_none_or(|v| !aterm_db::crypto::name(v))
        || value["provider"].as_str().and_then(Provider::of).is_none()
        || ["table", "commitment", "context"].iter().any(|k| {
            value[*k]
                .as_str()
                .is_none_or(|v| super::super::digest::<32>(v).is_err())
        })
        || value["request"]
            .as_str()
            .is_none_or(|v| super::super::digest::<16>(v).is_err())
        || value["value"].as_object().is_none()
        || (!value["operation"].is_null()
            && value["operation"]
                .as_str()
                .is_none_or(|v| super::super::digest::<32>(v).is_err()))
        || (value["operation"].is_null() && value["origin"].is_null())
        || (!value["previous"].is_null()
            && value["previous"]
                .as_str()
                .is_none_or(|v| super::super::digest::<32>(v).is_err()))
        || value["value"]["provider"] != value["provider"]
        || value["value"]["method"].as_str() != Some("api-key")
    {
        return Err("native input attachment is invalid".into());
    }
    aterm_db::accounts::validate(&value["value"])
        .map_err(|_| "native input contains an invalid account proposal")?;
    let input = Secret::new(&plain.as_slice()[4 + count..]);
    if !hex_eq(
        value["commitment"].as_str(),
        &aterm_digest::Sha256::digest(input.expose()),
    ) {
        return Err("native intake payload differs from its metadata".into());
    }
    Inputs::decode(
        input.expose(),
        Provider::of(value["provider"].as_str().unwrap()).unwrap(),
    )?;
    Ok(Some((value, input)))
}

pub(super) fn recover(paths: &Paths, config: Option<&Path>) -> Result<bool, String> {
    if let Some(config) = config
        && !native_keys::preparation_enabled(config)?
    {
        return Err("automatic native attachment is off".into());
    }
    let _lock = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "native-key-attachment.lock",
        Duration::ZERO,
        "native intake is busy",
    )
    .map_err(|e| e.to_string())?;
    recover_locked(paths, config, step_deadline(Instant::now()), None)
}

pub(super) fn recover_root(
    paths: &Paths,
    admin: &client::Admin,
    config: &Path,
    deadline: Instant,
) -> Result<bool, String> {
    if !native_keys::preparation_enabled(config)? {
        return Err("automatic native attachment is off".into());
    }
    if !pending(paths)? {
        return Ok(false);
    }
    check_time(deadline)?;
    let _lock = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "native-key-attachment.lock",
        Duration::ZERO,
        "native intake is busy",
    )
    .map_err(|e| e.to_string())?;
    recover_locked(paths, Some(config), deadline, Some(admin))
}

fn recover_locked(
    paths: &Paths,
    config: Option<&Path>,
    deadline: Instant,
    current_admin: Option<&client::Admin>,
) -> Result<bool, String> {
    let allowed = || -> Result<(), String> {
        check_time(deadline)?;
        if let Some(config) = config
            && !native_keys::preparation_enabled(config)?
        {
            return Err("automatic native attachment is off".into());
        }
        Ok(())
    };
    allowed()?;
    let Some((mut plan, input)) = plan(paths, deadline)? else {
        return Ok(false);
    };
    let proposals = Proposals::at_until(paths.clone(), deadline);
    let snapshot = proposals.snapshot().map_err(|e| e.to_string())?;
    if !snapshot.history_complete() {
        return Err("native intake proposal history is incomplete; wait for sync repair".into());
    }
    if !hex_eq(plan["table"].as_str(), &snapshot.scope.digest()) {
        return Err("native input belongs to another proposal table".into());
    }
    let key = plan["key"].as_str().unwrap();
    imported::current(paths, key, &plan["origin"], deadline)?;
    let provider = Provider::of(plan["provider"].as_str().unwrap()).unwrap();
    let entry = entry(&snapshot.scope, key, provider);
    let expected = client::config(paths)
        .map_err(|e| e.to_string())?
        .ok_or("native intake authority is unavailable")?;
    let expected_context = context(&expected);
    if plan["context"].as_str() != Some(expected_context.as_str()) {
        return Err("native intake configured authority changed".into());
    }
    allowed()?;
    let mut credential =
        client::read_verified_entry(paths, &expected, &entry).map_err(|e| e.to_string())?;
    if !credential
        .as_ref()
        .is_some_and(|entry| entry.value().as_slice() == input.expose())
    {
        let cell = snapshot.cells.get(&("accounts".into(), key.into()));
        let unchanged = match (cell, plan["operation"].as_str()) {
            (None, None) => true,
            (Some(cell), Some(operation)) => {
                !cell.stopped
                    && cell.candidates.len() == 1
                    && hex_eq(Some(operation), &cell.candidates[0].operation)
            }
            _ => false,
        };
        if !unchanged {
            return Err("native intake proposal changed before publication".into());
        }
        let opened =
            if current_admin.is_none() {
                Some(client::admin_cached_until(paths, deadline).map_err(
                    |_| "native intake needs current root authority to resume publication",
                )?)
            } else {
                None
            };
        let admin = current_admin.or(opened.as_ref()).unwrap();
        if context(&admin.config) != expected_context {
            return Err("native intake authority changed before publication".into());
        }
        let previous_raw = admin
            .native_entry_digest(&entry)
            .map_err(|e| e.to_string())?;
        let desired_raw = aterm_digest::Sha256::digest(input.expose());
        if previous_raw != Some(desired_raw) {
            if !match previous_raw.as_ref() {
                Some(digest) => hex_eq(plan["previous"].as_str(), digest),
                None => plan["previous"].is_null(),
            } {
                return Err("native intake entry changed before publication".into());
            }
            allowed()?;
            imported::current(paths, key, &plan["origin"], deadline)?;
            client::admin_set_entry_until(
                paths,
                admin,
                &entry,
                previous_raw,
                input.expose(),
                deadline,
            )
            .map_err(|_| "native intake publication remains pending")?;
        } else {
            client::sync_reader_until(paths, deadline)
                .map_err(|_| "native intake materialization remains pending")?;
        }
        credential =
            client::read_verified_entry(paths, &expected, &entry).map_err(|e| e.to_string())?;
    }
    let credential = credential.ok_or("native intake publication is not readable yet")?;
    let commitment = aterm_digest::Sha256::digest(credential.value().as_slice());
    if !hex_eq(plan["commitment"].as_str(), &commitment) {
        return Ok(false);
    }
    Inputs::decode(credential.value().as_slice(), provider)?;
    let request = super::super::digest(plan["request"].as_str().unwrap())?;
    let operation = plan["operation"]
        .as_str()
        .map(super::super::digest)
        .transpose()?;
    allowed()?;
    imported::current(paths, key, &plan["origin"], deadline)?;
    let key = key.to_owned();
    let mut value = plan
        .as_object_mut()
        .and_then(|object| object.remove("value"))
        .unwrap_or(Value::Null);
    value.as_object_mut().unwrap().insert(
        "secret_ref".into(),
        Value::Object(aterm_json::Map::from([
            ("vault".to_string(), Value::String(expected.name)),
            ("entry".to_string(), Value::String(entry)),
        ])),
    );
    let attached = match operation {
        Some(operation) => {
            proposals.replace_if_current(request, "accounts", &key, operation, value)
        }
        None => proposals.create_if_absent(request, "accounts", &key, value),
    };
    attached.map_err(
        |_| "native key is stored; proposal attachment is pending a conflict resolution",
    )?;
    check_time(deadline)?;
    std::fs::remove_file(paths.dir.join(PENDING))
        .map_err(|_| "native attachment completed but its receipt could not be cleared")?;
    Ok(true)
}

pub(super) fn set(paths: &Paths, key: &str, raw: &[u8]) -> Result<Value, String> {
    set_after_pending(paths, key, raw, || Ok(()))
}

pub(super) fn set_after_pending(
    paths: &Paths,
    key: &str,
    raw: &[u8],
    after: impl FnOnce() -> Result<(), String>,
) -> Result<Value, String> {
    let deadline = step_deadline(Instant::now());
    // Opening root authority precedes reading/writing the selected entry.
    let admin = client::admin_cached_until(paths, deadline)
        .map_err(|_| "native key intake requires current root admin authority")?;
    let _lock = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "native-key-attachment.lock",
        Duration::ZERO,
        "native intake is busy",
    )
    .map_err(|e| e.to_string())?;
    if pending(paths)? {
        return Err("native key intake already has a pending attachment".into());
    }
    let proposals = Proposals::at_until(paths.clone(), deadline);
    let snapshot = proposals.snapshot().map_err(|e| e.to_string())?;
    if !snapshot.history_complete() {
        return Err("native intake proposal history is incomplete; wait for sync repair".into());
    }
    let mut imported = imported::resolve(paths, key, deadline)?;
    let origin = imported.as_mut().map_or(Value::Null, |row| {
        std::mem::replace(&mut row.origin, Value::Null)
    });
    let key = imported.as_ref().map_or(key, |row| row.key.as_str());
    let cell = snapshot.cells.get(&("accounts".into(), key.into()));
    let (candidate, operation) = match cell {
        Some(cell) if !cell.stopped && cell.candidates.len() == 1 => (
            &cell.candidates[0].value,
            Some(cell.candidates[0].operation),
        ),
        None if imported.is_some() => (&imported.as_ref().unwrap().value, None),
        _ => return Err("native intake needs one current account proposal".into()),
    };
    if let Some(imported) = &imported {
        let mut existing = candidate.clone();
        existing.as_object_mut().unwrap().remove("secret_ref");
        if existing != imported.value {
            return Err("derived imported proposal already has different metadata".into());
        }
    }
    let provider = candidate["provider"]
        .as_str()
        .and_then(Provider::of)
        .ok_or("native intake needs Google or Meta")?;
    if candidate
        .get("method")
        .is_some_and(|method| !method.is_null() && method.as_str() != Some("api-key"))
        || requirement(candidate, "login-input-format")?.is_some_and(|f| f != "api-key-v1")
    {
        return Err("native intake needs the API-key method".into());
    }
    let wrapped = Inputs::encode_key(provider, raw)?;
    let mut value = candidate.clone();
    value
        .as_object_mut()
        .unwrap()
        .insert("method".into(), Value::String("api-key".into()));
    let entry = entry(&snapshot.scope, key, provider);
    let previous = admin
        .native_entry_digest(&entry)
        .map_err(|e| e.to_string())?;
    let mut request = [0; 16];
    aterm_uds::rand::fill(&mut request).map_err(|_| "native intake request is unavailable")?;
    if request == [0; 16] {
        return Err("native intake request is unavailable".into());
    }
    let plan = Value::Object(aterm_json::Map::from([
        ("schema".to_string(), Value::Number(1u64.into())),
        ("key".to_string(), Value::String(key.to_string())),
        (
            "provider".to_string(),
            Value::String(provider.name().to_string()),
        ),
        (
            "table".to_string(),
            Value::String(aterm_codec::hex::encode_lower(&snapshot.scope.digest())),
        ),
        (
            "operation".to_string(),
            operation.map_or(Value::Null, |op| {
                Value::String(aterm_codec::hex::encode_lower(&op))
            }),
        ),
        (
            "request".to_string(),
            Value::String(aterm_codec::hex::encode_lower(&request)),
        ),
        ("origin".to_string(), origin),
        (
            "commitment".to_string(),
            Value::String(aterm_codec::hex::encode_lower(
                &aterm_digest::Sha256::digest(wrapped.expose()),
            )),
        ),
        ("value".to_string(), value),
        ("context".to_string(), Value::String(context(&admin.config))),
        (
            "previous".to_string(),
            previous.map_or(Value::Null, |digest| {
                Value::String(aterm_codec::hex::encode_lower(&digest))
            }),
        ),
    ]));
    let metadata =
        aterm_json::to_vec(&plan).map_err(|_| "native attachment could not be encoded")?;
    if metadata.len() > 64 * 1024 {
        return Err("native input metadata exceeds its bound".into());
    }
    let mut plain = Vec::with_capacity(4 + metadata.len() + wrapped.len());
    plain.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    plain.extend_from_slice(&metadata);
    plain.extend_from_slice(wrapped.expose());
    let plain = Secret::from_vec(plain);
    let identity = client::read_machine_identity(paths)
        .map_err(|e| e.to_string())?
        .ok_or("native intake identity is unavailable")?;
    let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::required_until(paths, deadline)
        .map_err(|e| e.to_string())?;
    let request = bootstrap.request().map_err(|e| e.to_string())?;
    if identity.recipient() != request.binding().recipient() {
        return Err("native intake device changed".into());
    }
    let sealed = admin
        .seal_account_input(request.binding(), plain.expose())
        .map_err(|_| "native input could not be sealed and authorized")?;
    check_time(deadline)?;
    fsx::write_atomic(&paths.dir.join(PENDING), &sealed, 0o600).map_err(|e| e.to_string())?;
    after()?;
    let attached = recover_locked(paths, None, deadline, Some(&admin))?;
    Ok(
        json!({"kind":"native-key-intake","provider":provider.name(),"key":key,"state":if attached{"attached"}else{"pending-attachment"},"authority":"no-new-grant"}),
    )
}

/// A raw key piped in for `native-key set`: at most [`MAX`] bytes, one trailing LF or CR LF
/// dropped. A longer input is refused whole, never cut to the part that fits.
pub(super) fn read_piped_key(reader: impl Read) -> Result<Secret, String> {
    // Room for the key, its CR LF, and one byte past them that shows the input runs longer.
    let room = MAX + 3;
    let mut bytes = Vec::with_capacity(room);
    let read = reader.take(room as u64).read_to_end(&mut bytes);
    let bytes = Secret::from_vec(bytes);
    read.map_err(|_| "native key input could not be read")?;
    let raw = bytes.expose().strip_suffix(b"\n").unwrap_or(bytes.expose());
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    if raw.len() > MAX {
        return Err("native key exceeds its size bound".into());
    }
    Ok(Secret::new(raw))
}

pub(super) fn command(paths: &Paths, args: &[OsString]) -> Result<Value, String> {
    if args.first().is_some_and(|a| a == "attach") && args.len() == 1 {
        return recover(paths,None).map(|done|json!({"kind":"native-key-intake","state":if done{"attached"}else{"pending-or-absent"},"authority":"no-new-grant"}));
    }
    if args.len() != 3 || args[0] != "set" || args[1] != "--key" {
        return Err("usage: accounts native-key set --key <proposal> (raw key on stdin), or native-key attach".into());
    }
    let key = args[2].to_str().ok_or("native proposal key is not UTF-8")?;
    let raw = read_piped_key(std::io::stdin().lock())?;
    set(paths, key, raw.expose())
}
