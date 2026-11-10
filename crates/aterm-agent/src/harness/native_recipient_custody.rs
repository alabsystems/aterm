// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Independent, pre-recipe custody for complete recipient history.

use super::*;
use crate::harness::native_act::ReceiptMode;
use ring::rand::{SecureRandom, SystemRandom};

#[path = "native_recipient_custody_binding.rs"]
mod binding;
pub use binding::{BoundRecipientCustody, bind_recipient_custody};

#[path = "native_recipient_writer.rs"]
mod writer;
pub use writer::{RecipientWriter, open_recipient_writer};

#[path = "native_recipient_closure.rs"]
mod closure;
pub use closure::close_recipient_reservation;

const DIRECTORY: &str = "native-recipients";
const ENROLLMENT: &str = "native-recipients.enrolled";
const LOCK: &str = "native-recipients.lock";
const NAME: &str = "recipients";
const ENROLLMENT_BYTES: usize = 69;

/// A complete-journal identity established before freezing a restart recipe.
/// It is neither an execution grant nor evidence that an operation was absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipientCustody {
    stamp: String,
}

impl RecipientCustody {
    /// Value for a complete-mode recipe's immutable `custody` field.
    #[must_use]
    pub fn stamp(&self) -> &str {
        &self.stamp
    }
}

struct Enrollment {
    nonce: Digest,
    active: bool,
}

impl Enrollment {
    fn encode(&self, base: &Key, location: &[u8]) -> Vec<u8> {
        let mut body = b"ARC1".to_vec();
        body.push(u8::from(self.active));
        body.extend_from_slice(&self.nonce);
        let auth = hmac::Key::new(
            hmac::HMAC_SHA256,
            &base.digest(b"recipient-enrollment-auth\0", location),
        );
        body.extend_from_slice(hmac::sign(&auth, &body).as_ref());
        body
    }

    fn decode(bytes: &[u8], base: &Key, location: &[u8]) -> io::Result<Self> {
        if bytes.len() != ENROLLMENT_BYTES
            || &bytes[..4] != b"ARC1"
            || bytes[4] > 1
            || bytes[5..37] == [0; 32]
        {
            return Err(invalid());
        }
        let auth = hmac::Key::new(
            hmac::HMAC_SHA256,
            &base.digest(b"recipient-enrollment-auth\0", location),
        );
        hmac::verify(&auth, &bytes[..37], &bytes[37..]).map_err(|_| invalid())?;
        Ok(Self {
            nonce: bytes[5..37].try_into().unwrap(),
            active: bytes[4] == 1,
        })
    }

    fn key(&self, base: &Key, location: &[u8]) -> Key {
        let mut context = location.to_vec();
        context.extend_from_slice(&self.nonce);
        let mut anchor = b"aterm/native-recipients/1\0".to_vec();
        anchor.extend_from_slice(&base.anchor);
        anchor.extend_from_slice(&base.digest(b"recipient-location\0", &context));
        Key {
            mac: hmac::Key::new(
                hmac::HMAC_SHA256,
                &base.digest(b"recipient-log-key\0", &context),
            ),
            anchor,
        }
    }
}

struct Opened {
    custody: RecipientCustody,
    key: Key,
    log: Log,
    view: binding::View,
}

impl Opened {
    fn intact(&self, paths: &Paths) -> io::Result<()> {
        let base = base_key(paths, false)?;
        let location = std::fs::canonicalize(&paths.dir)?;
        let location = location.as_os_str().as_encoded_bytes();
        let bytes = private_file::read_private(&paths.dir, ENROLLMENT, ENROLLMENT_BYTES)
            .map_err(|_| invalid())?;
        let enrollment = Enrollment::decode(&bytes, &base, location)?;
        if !enrollment.active
            || enrollment.key(&base, location).anchor != self.key.anchor
            || self.log.is_poisoned()
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn dir(paths: &Paths) -> PathBuf {
    paths.dir.join(DIRECTORY)
}

fn present(paths: &Paths) -> io::Result<bool> {
    for path in [dir(paths), paths.dir.join(ENROLLMENT)] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn base_key(paths: &Paths, initialize: bool) -> io::Result<Key> {
    if let Some(identity) =
        aterm_secrets::client::read_machine_identity(paths).map_err(|_| invalid())?
    {
        return Ok(Key::new(&identity));
    }
    if !initialize || present(paths)? {
        return Err(invalid());
    }
    write_key(paths)
}

/// Enroll or reopen independent complete-journal custody before constructing a recipe.
/// A pending first enrollment resumes with the same identity. Missing history is
/// refused while enrollment persists; full removal creates a distinct identity.
/// This publishes no execution authority.
/// # Errors
/// Refuses damaged, foreign or nonprivate state, missing established identity/history,
/// competing writers, entropy failures and failed durability barriers.
pub fn enroll_recipient_custody() -> io::Result<RecipientCustody> {
    enroll_at(&Paths::locate().map_err(|_| invalid())?)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientCustody",
        action = "Enroll",
        project = "aterm_agent::harness::native_receipts::recipient_custody::tests::model::transition"
    )
)]
pub(super) fn enroll_at(paths: &Paths) -> io::Result<RecipientCustody> {
    Ok(open_at(paths, true)?.custody)
}

pub(super) fn observe_at(paths: &Paths, prepare: &Prepare) -> io::Result<Option<RecipientHistory>> {
    if prepare.receipt_mode != Some(ReceiptMode::Recipients5) {
        return Err(invalid());
    }
    let opened = open_at(paths, false)?;
    if prepare.custody.as_deref() != Some(opened.custody.stamp()) {
        return Err(invalid());
    }
    opened.view.observe(&opened.key, prepare)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn record_count_at(paths: &Paths) -> io::Result<u64> {
    Ok(open_at(paths, false)?.view.sequence)
}

/// Development fixture only: binding is established by the real API, then the
/// event is authenticated and replayed before publication. This grants no Execute.
#[cfg(any(test, debug_assertions))]
pub(super) fn append_fixture(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    event: super::testing::RecipientAppend,
) -> io::Result<Observed> {
    binding::bind_at(paths, prepare, binding)?;
    let mut opened = open_at(paths, false)?;
    let observed = Observed {
        sequence: opened.view.sequence,
        recorded_ms: aterm_time::unix_millis().ok_or_else(invalid)?,
    };
    let body = recipients::encode_fixture(
        &opened.key,
        observed.sequence,
        opened.view.head,
        prepare,
        binding,
        event,
        observed.recorded_ms,
    )?;
    opened
        .view
        .ingest_record(&opened.key, recipients::SUBJECT, &body)?;
    opened.log.append(recipients::SUBJECT, body)?;
    Ok(observed)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "Pending",
        reason = "A crash leaves authenticated enrollment intent before journal creation"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "Journal",
        reason = "A crash leaves an initialized journal before active enrollment publication"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "Reserve",
        reason = "The reservation API claims the exact operation before binding"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "LoseJournal",
        reason = "The environment removes an established complete journal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "LoseEnrollment",
        reason = "The environment removes enrollment while journal bytes remain"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "ForgetAll",
        reason = "Complete custody is removed while the old recipe remains immutable"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "WrongStamp",
        reason = "The caller supplies a different immutable custody stamp"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "LoseReservation",
        reason = "Legacy history is removed after binding"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "ReplaceReservation",
        reason = "The legacy reservation is recreated under another journal genesis"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "RecipientCustody",
        action = "Crash",
        reason = "The caller loses acknowledgements and reopens authenticated state"
    )
)]
fn open_at(paths: &Paths, initialize: bool) -> io::Result<Opened> {
    let base = base_key(paths, initialize)?;
    let _enrollment = private_file::PrivateLock::take(
        &paths.dir,
        LOCK,
        Duration::from_secs(1),
        "recipient custody enrollment is busy",
    )?;
    let location = std::fs::canonicalize(&paths.dir)?;
    let location = location.as_os_str().as_encoded_bytes();
    let mut enrollment = match private_file::read_private(&paths.dir, ENROLLMENT, ENROLLMENT_BYTES)
    {
        Ok(bytes) => Enrollment::decode(&bytes, &base, location)?,
        Err(PrivateFileError::Missing) if initialize && !present(paths)? => {
            let mut nonce = [0; 32];
            SystemRandom::new()
                .fill(&mut nonce)
                .map_err(|_| invalid())?;
            if nonce == [0; 32] {
                return Err(invalid());
            }
            let pending = Enrollment {
                nonce,
                active: false,
            };
            aterm_journal::write_private_durable(
                &paths.dir,
                ENROLLMENT,
                &pending.encode(&base, location),
            )?;
            pending
        }
        _ => return Err(invalid()),
    };
    if !enrollment.active && !initialize {
        return Err(invalid());
    }
    let directory = dir(paths);
    if enrollment.active {
        private_file::check_private_dir(&directory)?;
    } else {
        aterm_journal::create_private_dir_durable(&directory)?;
    }
    let key = enrollment.key(&base, location);
    let mut log = Log::open_until(
        &directory,
        NAME,
        &key.anchor,
        enrollment.active,
        LIMITS,
        Instant::now() + Duration::from_secs(1),
    )?;
    let view = binding::replay(&log, &key)?;
    if !enrollment.active && view.sequence != 0 {
        return Err(invalid());
    }
    log.finish_open()?;
    if !enrollment.active {
        enrollment.active = true;
        aterm_journal::write_private_durable(
            &paths.dir,
            ENROLLMENT,
            &enrollment.encode(&base, location),
        )?;
    }
    // Finish an enrollment whose atomic publication became visible before its
    // caller received the directory barrier acknowledgement.
    aterm_durable::durable_dir(&paths.dir)?;
    let custody = RecipientCustody {
        stamp: aterm_codec::hex::encode_lower(&key.digest(b"recipient-custody\0", &key.anchor)),
    };
    Ok(Opened {
        custody,
        key,
        log,
        view,
    })
}

#[cfg(test)]
#[path = "native_recipient_custody_tests.rs"]
mod tests;
