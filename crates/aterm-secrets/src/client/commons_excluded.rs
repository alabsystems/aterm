// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member whose membership log seats it while the current signed Commons epochs leave it out of
//! control (`control-excluded`), as they left 55 of ALab's 58 members on 2026-11-04: the seam
//! other crates' tests build one with, on a scratch vault served on loopback. Absent from
//! production release builds.

use super::*;

/// What [`test_commons_excluded`] built under its root.
pub struct CommonsExcluded {
    /// The vault's server, on loopback.
    pub server: crate::server::Running,
    /// The vault's admin, which holds Commons control.
    pub admin: Paths,
    /// The member: its log seats it, it pinned the network's anchor while it held Commons control,
    /// and the current Commons epochs leave it out. Its `[everyone]` holds `shared` = `everyone`.
    pub member: Paths,
}

/// Build a [`CommonsExcluded`] under `root`: the admin creates the vault and approves the member,
/// the member opens Commons control once (pinning the anchor) and the admin enrolls its device in
/// the Commons roster, the admin signs the membership log's genesis, which seats both, and then
/// addresses Commons control to itself alone, minting as a 0.154.0 Mac did (the current carry keeps
/// every seat, one-mesh M1), so the member's next sync reads `control-excluded`.
///
/// # Errors
/// A step failed, or the member does not end up `control-excluded` and seated.
pub fn test_commons_excluded(root: &Path) -> crate::Result<CommonsExcluded> {
    use crate::commons::{Observation, Unavailable};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let server = crate::server::start(
        listener,
        &crate::server::Options {
            listen: "127.0.0.1:0"
                .parse()
                .map_err(|_| Error::new("loopback address"))?,
            store: root.join("server"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(100),
            keep_awake: false,
        },
    )?;
    let url = format!("http://{}/v1/excluded", server.addr);
    let admin = Paths::for_home(&root.join("admin"));
    let created = create(&admin, &url, None, Some("admin"))?;
    let member = Paths::for_home(&root.join("member"));
    let recipient = machine_identity(&member)?.recipient().to_bech32();
    let opened = admin_open(&admin)?;
    let mut doc = opened.doc.clone();
    doc.add_machine("member", &recipient).map_err(text_error)?;
    doc.set_entry(&Audience::Everyone, "shared", b"everyone")
        .map_err(text_error)?;
    admin_publish_approved(&admin, &opened, &doc, std::slice::from_ref(&recipient))?;
    join(
        &member,
        &JoinOptions {
            source: format!("{url}#{}", created.signer),
            host: Some("member".to_owned()),
            interactive: false,
            wait: Duration::from_secs(2),
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )?;
    // The member opens Commons (pinning the anchor), and the admin enrolls its device in the
    // Commons roster, which the genesis then seats it by.
    let owner = crate::commons::bootstrap::Bootstrap::required(&admin)?;
    let peer = crate::commons::bootstrap::Bootstrap::required(&member)?;
    let request = peer.request()?;
    let (mut pending, challenge) = crate::commons::binding::Pending::issue(
        &request,
        owner.trust().network,
        owner.trust().authority,
    )?;
    let answer = peer.device().answer(&request, &challenge)?;
    let mut journal =
        crate::commons::journal::Journal::open_existing(owner.journal_dir(), owner.trust())?;
    admin_cached(&admin)?.enroll_commons_member(&owner, &mut journal, pending.verify(&answer)?)?;
    drop((journal, owner, peer));
    seed_vault_genesis(&admin, &root.join("genesis"))?;
    sync(&member, None)?;
    let opened = admin_open(&admin)?;
    let mut doc = opened.doc.clone();
    let control = doc
        .entries()
        .into_iter()
        .find(|entry| entry.name == crate::commons::ENTRY)
        .ok_or_else(|| Error::new("the vault has no Commons control entry"))?;
    doc.unset_entry(&control.audience, crate::commons::ENTRY)
        .map_err(text_error)?;
    doc.set_entry(
        &Audience::Hosts(vec!["admin".to_owned()]),
        crate::commons::ENTRY,
        control.value.as_slice(),
    )
    .map_err(text_error)?;
    // Minted as a 0.154.0 Mac minted it: the current carry keeps every seat it does not remove
    // (one-mesh M1), so only 0.154.0's carry leaves the seated member out of Commons control.
    super::log_entries::as_0154(|| admin_publish(&admin, &opened, &doc))?;
    sync(&member, None)?;
    let observed = crate::commons::observe(&member)?;
    if !matches!(
        observed,
        Observation::Unavailable(Unavailable::ControlExcluded)
    ) || !admits_by_log(&member)
    {
        return Err(Error::new(format!(
            "the fixture's member is not a seated, control-excluded member: Commons {}, seated {}",
            match observed {
                Observation::Unavailable(reason) => reason.code(),
                Observation::Visible(_) => "visible",
            },
            admits_by_log(&member),
        )));
    }
    Ok(CommonsExcluded {
        server,
        admin,
        member,
    })
}

fn text_error(error: text::TextError) -> Error {
    Error::new(error.to_string())
}
