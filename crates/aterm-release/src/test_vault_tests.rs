// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A scratch vault under a test's home: this crate's readers read the store in place, so a
//! test seeds the store, never a file under the home.

use std::path::Path;

/// Make `home` a vault member (`m1`) whose store publishes `entries` to everyone. A call on a
/// home that already holds a vault republishes over it.
pub(crate) fn seed(home: &Path, entries: &[(&str, &[u8])]) -> aterm_secrets::client::Paths {
    let paths = aterm_secrets::client::Paths::for_home(home);
    if aterm_secrets::client::config(&paths).unwrap().is_none() {
        let source = home.join("team.vault");
        aterm_secrets::client::create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    }
    let admin = aterm_secrets::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    for (name, value) in entries {
        doc.set_entry(&aterm_secrets::text::Audience::Everyone, name, value)
            .unwrap();
    }
    aterm_secrets::client::admin_publish(&paths, &admin, &doc).unwrap();
    paths
}

/// Remove `names` from the store under `home`.
pub(crate) fn drop_entries(home: &Path, names: &[&str]) {
    let paths = aterm_secrets::client::Paths::for_home(home);
    let admin = aterm_secrets::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    for name in names {
        doc.unset_entry(&aterm_secrets::text::Audience::Everyone, name)
            .unwrap();
    }
    aterm_secrets::client::admin_publish(&paths, &admin, &doc).unwrap();
}

/// The store's release ring under `home` for the PKCS#8 key `pkcs8`, its record naming `id` and
/// `record_key`, its profile the key alone.
pub(crate) fn seed_ring_as(home: &Path, id: &str, pkcs8: &[u8], record_key: &str) {
    let encoded = aterm_codec::base64::encode(pkcs8).unwrap();
    seed_ring_profile(
        home,
        id,
        pkcs8,
        record_key,
        &format!("signing_key = \"{encoded}\"\n"),
    );
}

/// The store's release ring under `home` for the PKCS#8 key `pkcs8`, its record naming `id` and
/// `record_key`, its profile `profile` as written.
pub(crate) fn seed_ring_profile(
    home: &Path,
    id: &str,
    pkcs8: &[u8],
    record_key: &str,
    profile: &str,
) {
    let record = format!("id = \"{id}\"\npubkey = \"{record_key}\"\n");
    let [key, toml, creds] = aterm_secrets::entry_class::RELEASE_RING;
    seed(
        home,
        &[
            (key, pkcs8),
            (toml, record.as_bytes()),
            (creds, profile.as_bytes()),
        ],
    );
}
