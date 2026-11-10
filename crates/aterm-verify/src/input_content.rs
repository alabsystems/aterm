// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-reader input evidence retained in A19 source-check records.

use crate::Ctx;

/// One successful deterministic reader and its complete package/file input snapshots.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    pub check: String,
    pub item: String,
    pub key: String,
    pub inputs: Vec<aterm_census::inputs::Input>,
}

impl Proof {
    pub fn new(
        ctx: &Ctx,
        check: &str,
        item: &str,
        inputs: Vec<aterm_census::inputs::Input>,
    ) -> Option<Self> {
        let context = ctx.carry_context.as_deref()?;
        let mut hash = aterm_digest::Sha256::new();
        for part in [
            "aterm-preflight-reader-v1",
            context,
            crate::report::running_driver()?,
            check,
            item,
        ] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        let bytes = aterm_json::to_vec(&inputs).ok()?;
        hash.update(bytes);
        Some(Self {
            check: check.into(),
            item: item.into(),
            key: aterm_codec::hex::encode_lower(&hash.finalize()),
            inputs,
        })
    }

    pub fn reusable(&self, ctx: &Ctx) -> bool {
        Self::new(ctx, &self.check, &self.item, self.inputs.clone())
            .is_some_and(|current| current.key == self.key)
            && aterm_census::inputs::matches(&ctx.root, &self.inputs)
    }
}

pub fn prior<'a>(ctx: &'a Ctx, check: &str, item: &str) -> Option<&'a Proof> {
    ctx.input_items
        .iter()
        .find(|proof| proof.check == check && proof.item == item && proof.reusable(ctx))
}

pub fn exact(ctx: &Ctx, proof: &Proof) -> bool {
    ctx.input_items
        .iter()
        .any(|old| old == proof && old.reusable(ctx))
}

pub fn event(ctx: &Ctx, proof: &Proof, carried: bool) -> String {
    let Ok(body) = aterm_json::to_string(proof) else {
        return String::new();
    };
    if body.len() > MAX_PROOF_BYTES {
        return String::new();
    }
    let key = body_key(&body);
    let mut pieces = Vec::new();
    let mut begin = 0;
    for (offset, _) in body.char_indices() {
        if offset - begin >= CHUNK_BYTES {
            pieces.push(&body[begin..offset]);
            begin = offset;
        }
    }
    pieces.push(&body[begin..]);
    let mut output = String::new();
    for (part, data) in pieces.iter().enumerate() {
        output.push_str(&ctx.record_event(&format!(
            "verify-event: {}\n",
            aterm_json::json!({
                "schema":1, "event":"input-item-part", "key":key,
                "part":part, "parts":pieces.len(), "data":data,
            })
        )));
    }
    output.push_str(&ctx.record_event(&format!(
        "verify-event: {}\n",
        aterm_json::json!({
            "schema":1, "event":"input-item-finished", "key":key, "carried":carried,
        })
    )));
    output
}

const CHUNK_BYTES: usize = 20_000;
const MAX_PROOF_BYTES: usize = 1024 * 1024;

fn body_key(body: &str) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(body.as_bytes());
    aterm_codec::hex::encode_lower(&hash.finalize())
}

/// Reassemble bounded transport records. A missing, duplicate or reordered part cannot
/// manufacture a receipt, and only a completed item is returned to A19.
#[derive(Default)]
pub struct Collector {
    pending: std::collections::BTreeMap<String, (usize, usize, String)>,
}

impl Collector {
    pub fn observe(&mut self, event: &aterm_json::Value) -> Result<Option<Proof>, String> {
        let kind = event["event"].as_str().unwrap_or_default();
        if !["input-item-part", "input-item-finished"].contains(&kind) {
            return Ok(None);
        }
        let key = event["key"]
            .as_str()
            .filter(|key| aterm_codec::hex::is_lower(key, 64))
            .ok_or("invalid input item key")?;
        if kind == "input-item-part" {
            let part = event["part"].as_u64().ok_or("missing part")? as usize;
            let parts = event["parts"].as_u64().ok_or("missing part count")? as usize;
            let data = event["data"].as_str().ok_or("missing part data")?;
            if parts == 0 || parts > 64 || data.len() > CHUNK_BYTES + 4 {
                self.pending.remove(key);
                return Err("input item exceeds transport bounds".into());
            }
            if part == 0 {
                if self.pending.len() >= 256 || self.pending.contains_key(key) {
                    self.pending.remove(key);
                    return Err("duplicate input item start or too many pending items".into());
                }
                self.pending.insert(key.into(), (0, parts, String::new()));
            }
            let state = self
                .pending
                .get_mut(key)
                .ok_or("input item started midstream")?;
            if state.0 != part || state.1 != parts || state.2.len() + data.len() > MAX_PROOF_BYTES {
                self.pending.remove(key);
                return Err("input item part missing, reordered or oversized".into());
            }
            state.0 += 1;
            state.2.push_str(data);
            return Ok(None);
        }
        let (received, expected, data) =
            self.pending.remove(key).ok_or("input item has no parts")?;
        if received != expected || body_key(&data) != key {
            return Err("input item is incomplete".into());
        }
        let proof: Proof = aterm_json::from_str(&data).map_err(|e| e.to_string())?;
        if !aterm_codec::hex::is_lower(&proof.key, 64)
            || proof.item.is_empty()
            || !["fmt", "census", "export-scan"].contains(&proof.check.as_str())
        {
            return Err("input item footer disagrees with proof".into());
        }
        Ok(Some(proof))
    }
}

pub fn read(text: &str) -> Result<Vec<Proof>, String> {
    let proofs: Vec<Proof> = aterm_json::from_str(text).map_err(|e| e.to_string())?;
    if proofs.iter().any(|p| {
        !aterm_codec::hex::is_lower(&p.key, 64)
            || p.item.is_empty()
            || !["fmt", "census", "export-scan"].contains(&p.check.as_str())
    }) {
        return Err("invalid source-check input proof".into());
    }
    Ok(proofs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_input_proof_survives_event_bounds_only_with_every_intact_part() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut ctx = Ctx::new(
            dir.path().into(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            dir.path().join("scratch"),
        );
        ctx.carry_context = Some("a".repeat(64));
        let proof = Proof::new(
            &ctx,
            "census",
            "large",
            (0..1000)
                .map(|i| ("file".into(), format!("crates/a/{i}.rs"), "b".repeat(64)))
                .collect(),
        )
        .unwrap();
        let path = dir.path().join("events");
        ctx.events = Some(crate::event_log::EventLog::open(&path).unwrap());
        let emitted = event(&ctx, &proof, false);
        assert!(emitted.lines().count() > 2);
        let rows: Vec<aterm_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap())
            .collect();
        assert!(emitted.lines().all(|line| line.len() < 64 << 10));
        let mut collector = Collector::default();
        let accepted: Vec<_> = rows
            .iter()
            .filter_map(|row| collector.observe(row).unwrap())
            .collect();
        assert_eq!(accepted, vec![proof]);
        let mut missing = Collector::default();
        assert!(rows.iter().skip(1).any(|row| missing.observe(row).is_err()));
        let mut corrupted = rows;
        let original = corrupted[0]["data"].as_str().unwrap();
        let changed = original.replacen("\"file\"", "\"fyle\"", 1);
        assert_ne!(original, changed);
        corrupted[0]
            .as_object_mut()
            .unwrap()
            .insert("data".into(), aterm_json::Value::String(changed));
        let mut collector = Collector::default();
        assert!(corrupted.iter().any(|row| collector.observe(row).is_err()));
    }

    #[test]
    fn source_proof_requires_equal_content_and_context_and_cannot_replace_cargo_validation() {
        let dir = aterm_tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.rs"), "one").unwrap();
        let mut ctx = Ctx::new(
            dir.path().into(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::default(),
            dir.path().join("scratch"),
        );
        ctx.carry_context = Some("a".repeat(64));
        let input = aterm_census::inputs::file(dir.path(), "source.rs").unwrap();
        let proof = Proof::new(&ctx, "fmt", "package", vec![input]).unwrap();
        assert!(proof.reusable(&ctx));
        ctx.carry_context = Some("b".repeat(64));
        assert!(!proof.reusable(&ctx));
        ctx.carry_context = Some("a".repeat(64));
        std::fs::write(dir.path().join("source.rs"), "two").unwrap();
        assert!(!proof.reusable(&ctx));
    }
}
