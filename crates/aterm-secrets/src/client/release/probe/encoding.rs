// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A versioned journal of public probe identities, never credential material.

use super::model::{Action, Artifact, Journal, Repository, Subject};
use crate::{Error, Result};
use aterm_json::{Value, json};

pub(super) fn encode(journal: &Journal) -> Result<Vec<u8>> {
    journal.validate()?;
    aterm_json::to_vec(&json!({
        "schema":1, "nonce":journal.nonce,
        "subject":{
            "uid":journal.subject.uid,"node":journal.subject.node,
            "member":journal.subject.member,"writer":journal.subject.writer,
            "incarnation":journal.subject.incarnation,"network":journal.subject.network,
            "authority":journal.subject.authority,"genesis":journal.subject.genesis,
            "vault":journal.subject.vault
        },
        "created_ms":journal.created_ms,"updated_ms":journal.updated_ms,
        "artifact":journal.artifact.as_ref().map(|a| json!({
            "sha256":a.sha256,"executable_sha256":a.executable_sha256,"bytes":a.bytes,"signing_identity":a.signing_identity,
            "team":a.team,"executed":a.executed
        })),
        "submission":journal.submission,"apple_accepted":journal.apple_accepted,
        "pending":journal.pending.map(action_value),"cleanup_only":journal.cleanup_only,
        "repositories":journal.repositories.iter().map(|r| json!({
            "owner":r.owner,"commit":r.commit,"principal":r.principal,"tag_owned":r.tag_owned,
            "release":r.release,"asset":r.asset,"verified":r.verified,
            "asset_absent":r.asset_absent,"release_absent":r.release_absent,
            "tag_absent":r.tag_absent
        })).collect::<Vec<_>>()
    }))
    .map_err(|_| Error::new("cannot encode release probe journal"))
}

pub(super) fn decode(bytes: &[u8]) -> Result<Journal> {
    let value: Value = aterm_json::from_slice(bytes)
        .map_err(|_| Error::new("invalid release probe journal JSON"))?;
    fields(
        &value,
        &[
            "schema",
            "nonce",
            "subject",
            "created_ms",
            "updated_ms",
            "artifact",
            "submission",
            "apple_accepted",
            "pending",
            "cleanup_only",
            "repositories",
        ],
    )?;
    if number(&value, "schema")? != 1 {
        return Err(Error::new("unsupported release probe journal schema"));
    }
    let subject = &value["subject"];
    fields(
        subject,
        &[
            "uid",
            "node",
            "member",
            "writer",
            "incarnation",
            "network",
            "authority",
            "genesis",
            "vault",
        ],
    )?;
    let uid =
        u32::try_from(number(subject, "uid")?).map_err(|_| Error::new("invalid probe uid"))?;
    let repositories = value["repositories"]
        .as_array()
        .filter(|rows| rows.len() <= 2)
        .ok_or_else(|| Error::new("invalid probe repository list"))?
        .iter()
        .map(repository)
        .collect::<Result<Vec<_>>>()?;
    let journal = Journal {
        nonce: text(&value, "nonce")?,
        subject: Subject {
            uid,
            node: text(subject, "node")?,
            member: text(subject, "member")?,
            writer: text(subject, "writer")?,
            incarnation: text(subject, "incarnation")?,
            network: text(subject, "network")?,
            authority: text(subject, "authority")?,
            genesis: text(subject, "genesis")?,
            vault: text(subject, "vault")?,
        },
        repositories,
        created_ms: number(&value, "created_ms")?,
        updated_ms: number(&value, "updated_ms")?,
        artifact: if value["artifact"].is_null() {
            None
        } else {
            Some(artifact(&value["artifact"])?)
        },
        submission: optional_text(&value, "submission")?,
        apple_accepted: flag(&value, "apple_accepted")?,
        pending: if value["pending"].is_null() {
            None
        } else {
            Some(action(&value["pending"])?)
        },
        cleanup_only: flag(&value, "cleanup_only")?,
    };
    journal.validate()?;
    Ok(journal)
}

fn repository(value: &Value) -> Result<Repository> {
    fields(
        value,
        &[
            "owner",
            "commit",
            "principal",
            "tag_owned",
            "release",
            "asset",
            "verified",
            "asset_absent",
            "release_absent",
            "tag_absent",
        ],
    )?;
    Ok(Repository {
        owner: text(value, "owner")?,
        commit: optional_text(value, "commit")?,
        principal: optional_text(value, "principal")?,
        tag_owned: flag(value, "tag_owned")?,
        release: optional_number(value, "release")?,
        asset: optional_number(value, "asset")?,
        verified: flag(value, "verified")?,
        asset_absent: flag(value, "asset_absent")?,
        release_absent: flag(value, "release_absent")?,
        tag_absent: flag(value, "tag_absent")?,
    })
}

fn artifact(value: &Value) -> Result<Artifact> {
    fields(
        value,
        &[
            "sha256",
            "executable_sha256",
            "bytes",
            "signing_identity",
            "team",
            "executed",
        ],
    )?;
    Ok(Artifact {
        sha256: text(value, "sha256")?,
        executable_sha256: text(value, "executable_sha256")?,
        bytes: number(value, "bytes")?,
        signing_identity: text(value, "signing_identity")?,
        team: text(value, "team")?,
        executed: flag(value, "executed")?,
    })
}

fn action_value(action: Action) -> Value {
    let kind = match action {
        Action::Sign => "sign",
        Action::Submit => "submit",
        Action::Fetch(_) => "fetch",
        Action::Tag(_) => "tag",
        Action::Draft(_) => "draft",
        Action::Upload(_) => "upload",
        Action::Readback(_) => "readback",
        Action::DeleteAsset(_) => "delete-asset",
        Action::DeleteDraft(_) => "delete-draft",
        Action::DeleteTag(_) => "delete-tag",
    };
    json!({"kind":kind,"repository":action.index()})
}

fn action(value: &Value) -> Result<Action> {
    fields(value, &["kind", "repository"])?;
    let kind = text(value, "kind")?;
    let index = optional_number(value, "repository")?
        .map(usize::try_from)
        .transpose()
        .map_err(|_| Error::new("invalid probe repository index"))?;
    match (kind.as_str(), index) {
        ("sign", None) => Ok(Action::Sign),
        ("submit", None) => Ok(Action::Submit),
        ("fetch", Some(i)) => Ok(Action::Fetch(i)),
        ("tag", Some(i)) => Ok(Action::Tag(i)),
        ("draft", Some(i)) => Ok(Action::Draft(i)),
        ("upload", Some(i)) => Ok(Action::Upload(i)),
        ("readback", Some(i)) => Ok(Action::Readback(i)),
        ("delete-asset", Some(i)) => Ok(Action::DeleteAsset(i)),
        ("delete-draft", Some(i)) => Ok(Action::DeleteDraft(i)),
        ("delete-tag", Some(i)) => Ok(Action::DeleteTag(i)),
        _ => Err(Error::new("invalid pending release probe action")),
    }
}

fn fields(value: &Value, expected: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("invalid probe object"))?;
    if object.len() != expected.len() || expected.iter().any(|name| !object.contains_key(*name)) {
        return Err(Error::new(
            "release probe journal has missing or unknown fields",
        ));
    }
    Ok(())
}

fn text(value: &Value, name: &str) -> Result<String> {
    value[name]
        .as_str()
        .filter(|s| s.len() <= 256 && !s.contains(['\0', '\r', '\n']))
        .map(str::to_owned)
        .ok_or_else(|| Error::new("invalid release probe text field"))
}

fn number(value: &Value, name: &str) -> Result<u64> {
    value[name]
        .as_u64()
        .ok_or_else(|| Error::new("invalid release probe integer field"))
}

fn flag(value: &Value, name: &str) -> Result<bool> {
    value[name]
        .as_bool()
        .ok_or_else(|| Error::new("invalid release probe boolean field"))
}

fn optional_text(value: &Value, name: &str) -> Result<Option<String>> {
    if value[name].is_null() {
        Ok(None)
    } else {
        text(value, name).map(Some)
    }
}

fn optional_number(value: &Value, name: &str) -> Result<Option<u64>> {
    if value[name].is_null() {
        Ok(None)
    } else {
        number(value, name).map(Some)
    }
}
