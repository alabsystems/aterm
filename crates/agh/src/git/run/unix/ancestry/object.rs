// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Recover bytes only when the original Git object ID independently matches.
//! REST field normalization is never itself object or ancestry authority.

use crate::error::{Error, Result};
use aterm_json::Value;

const MAX: usize = 256 * 1024;

pub(super) fn oid(body: &[u8]) -> String {
    let mut input = format!("commit {}\0", body.len()).into_bytes();
    input.extend_from_slice(body);
    aterm_codec::hex::encode_lower(&sha1(&input))
}

pub(super) fn parents(body: &[u8]) -> Result<Vec<String>> {
    let end = body
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .ok_or_else(|| Error::protocol("commit has no header boundary"))?;
    let mut lines = body[..end].split(|b| *b == b'\n');
    let tree = lines
        .next()
        .and_then(|line| line.strip_prefix(b"tree "))
        .and_then(|id| std::str::from_utf8(id).ok())
        .filter(|id| crate::path::is_sha(id))
        .ok_or_else(|| Error::protocol("commit lacks an exact tree identity"))?;
    let _ = tree; // A tree ID is metadata here; no tree is fetched or licensed.
    let mut parents = Vec::new();
    for line in lines {
        if let Some(id) = line.strip_prefix(b"parent ") {
            let id = std::str::from_utf8(id)
                .ok()
                .filter(|id| crate::path::is_sha(id))
                .ok_or_else(|| Error::protocol("commit has an invalid original parent"))?;
            if parents.len() == 64 {
                return Err(Error::protocol("commit parent count exceeds bound"));
            }
            parents.push(id.to_owned());
        }
    }
    Ok(parents)
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::protocol(format!("commit API lacks {name}")))
}

fn identity(value: &Value) -> Result<(String, i64, String)> {
    let name = text(value, "name")?;
    let email = text(value, "email")?;
    if name.contains(['\n', '\r', '<', '>', '\0']) || email.contains(['\n', '\r', '<', '>', '\0']) {
        return Err(Error::protocol(
            "commit API identity cannot be reconstructed losslessly",
        ));
    }
    let date = text(value, "date")?;
    if !date.is_ascii() {
        return Err(Error::protocol("commit date is not ASCII"));
    }
    let (utc, zone, offset) = if let Some(epoch) = aterm_types::rfc3339::parse_utc(date) {
        (epoch, "+0000".to_owned(), 0)
    } else if date.len() == 25 && matches!(date.as_bytes()[19], b'+' | b'-') && &date[22..23] == ":"
    {
        let utc = aterm_types::rfc3339::parse_utc(&format!("{}Z", &date[..19]))
            .ok_or_else(|| Error::protocol("commit date is not supported"))?;
        let hour = date[20..22].parse::<i64>().ok().filter(|h| *h <= 23);
        let minute = date[23..25].parse::<i64>().ok().filter(|m| *m <= 59);
        let (Some(hour), Some(minute)) = (hour, minute) else {
            return Err(Error::protocol("invalid commit date offset"));
        };
        let offset = (hour * 60 + minute) * 60 * if date.as_bytes()[19] == b'-' { -1 } else { 1 };
        (
            utc,
            format!("{}{}{}", &date[19..20], &date[20..22], &date[23..25]),
            offset,
        )
    } else {
        return Err(Error::protocol("commit date is not supported"));
    };
    Ok((format!("{name} <{email}>"), utc - offset, zone))
}

fn zones(preferred: String) -> Vec<String> {
    let mut zones = vec![preferred, "+0000".into(), "-0800".into(), "-0700".into()];
    // JSON can normalize the original offset away. This finite representation
    // set covers current quarter-hour zones; an unsupported original remains
    // unknown, never a synthesized equivalent commit or a negative answer.
    for minute in -14_i32 * 60..=14 * 60 {
        if minute % 15 == 0 {
            zones.push(format!(
                "{}{:02}{:02}",
                if minute < 0 { '-' } else { '+' },
                minute.abs() / 60,
                minute.abs() % 60
            ));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    zones.retain(|zone| seen.insert(zone.clone()));
    zones
}

pub(super) fn from_api(id: &str, value: &Value) -> Result<Vec<u8>> {
    if text(value, "sha")? != id {
        return Err(Error::protocol(
            "commit API returned another original object ID",
        ));
    }
    let matches = |bytes: &[u8]| bytes.len() <= MAX && oid(bytes) == id;
    if let Some(verification) = value.get("verification")
        && let Some(payload) = verification.get("payload").and_then(Value::as_str)
    {
        if matches(payload.as_bytes()) {
            parents(payload.as_bytes())?;
            return Ok(payload.as_bytes().to_vec());
        }
        if let Some(signature) = verification.get("signature").and_then(Value::as_str)
            && let Some((header, message)) = payload.split_once("\n\n")
        {
            for signature in [signature, signature.trim_end_matches('\n')] {
                let raw = format!(
                    "{header}\ngpgsig {}\n\n{message}",
                    signature.replace('\n', "\n ")
                );
                if matches(raw.as_bytes()) {
                    parents(raw.as_bytes())?;
                    return Ok(raw.into_bytes());
                }
            }
        }
    }
    let tree = value
        .get("tree")
        .and_then(|v| v.get("sha"))
        .and_then(Value::as_str)
        .filter(|id| crate::path::is_sha(id))
        .ok_or_else(|| Error::protocol("commit API lacks exact tree ID"))?;
    let parents = value
        .get("parents")
        .and_then(Value::as_array)
        .filter(|p| p.len() <= 64)
        .ok_or_else(|| Error::protocol("commit API lacks bounded original parents"))?;
    let mut header = format!("tree {tree}\n");
    for parent in parents {
        let id = text(parent, "sha")?;
        if !crate::path::is_sha(id) {
            return Err(Error::protocol("invalid API parent ID"));
        }
        header.push_str(&format!("parent {id}\n"));
    }
    let author = identity(
        value
            .get("author")
            .ok_or_else(|| Error::protocol("missing commit author"))?,
    )?;
    let committer = identity(
        value
            .get("committer")
            .ok_or_else(|| Error::protocol("missing commit committer"))?,
    )?;
    let message = text(value, "message")?;
    if header.len() + author.0.len() + committer.0.len() + message.len() + 128 > MAX {
        return Err(Error::protocol("commit reconstruction exceeds bound"));
    }
    let authors = zones(author.2);
    let committers = zones(committer.2);
    let mut hashed = 0_usize;
    // Same-zone candidates first; independent zones remain explicitly bounded.
    for (a, c) in authors.iter().zip(&committers).chain(
        authors
            .iter()
            .flat_map(|a| committers.iter().map(move |c| (a, c))),
    ) {
        for newline in ["", "\n"] {
            let raw = format!(
                "{header}author {} {} {a}\ncommitter {} {} {c}\n\n{message}{newline}",
                author.0, author.1, committer.0, committer.1
            );
            hashed = hashed
                .checked_add(raw.len())
                .filter(|n| *n <= 64 * 1024 * 1024)
                .ok_or_else(|| {
                    Error::protocol(
                        "exact commit reconstruction work bound reached; ancestry unknown",
                    )
                })?;
            if matches(raw.as_bytes()) {
                return Ok(raw.into_bytes());
            }
        }
    }
    Err(Error::protocol(
        "commit API representation cannot reproduce the exact original bytes; ancestry unknown",
    ))
}

// A bounded candidate filter only. Every accepted remote body is additionally
// hashed by native Git before it enters the reusable original object store.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h = [
        0x6745_2301_u32,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut bytes = data.to_vec();
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for chunk in bytes.as_chunks::<64>().0 {
        let mut words = [0_u32; 80];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            words[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            words[i] = (words[i - 3] ^ words[i - 8] ^ words[i - 14] ^ words[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, w) in words.into_iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999_u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let n = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = n;
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *state = state.wrapping_add(value);
        }
    }
    let mut out = [0; 20];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_git_digest_and_api_identity_are_exact_not_normalized_equivalents() {
        assert_eq!(
            aterm_codec::hex::encode_lower(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            aterm_codec::hex::encode_lower(&sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        let raw = format!(
            "tree {}\nauthor A <a@b> 1700000000 -0800\ncommitter C <c@d> 1700000000 +0000\n\noriginal\n",
            "1".repeat(40)
        );
        let id = oid(raw.as_bytes());
        let mut value = aterm_json::json!({"sha":id,"tree":{"sha":"1".repeat(40)},"parents":[],
            "author":{"name":"A","email":"a@b","date":"2023-11-14T22:13:20Z"},
            "committer":{"name":"C","email":"c@d","date":"2023-11-14T22:13:20Z"},"message":"original"});
        assert_eq!(from_api(&id, &value).unwrap(), raw.as_bytes());
        value
            .as_object_mut()
            .unwrap()
            .insert("message".into(), Value::String("mutated".into()));
        assert!(from_api(&id, &value).is_err());
        value
            .as_object_mut()
            .unwrap()
            .get_mut("author")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("date".into(), Value::String("ééééééééééééx".into()));
        assert!(from_api(&id, &value).is_err());
    }
}
