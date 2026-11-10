// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Synthetic metadata only for registry lifecycle tests. This bypasses no
//! production constructor and proves no owner, image, source, or SDK admission.

use super::*;

pub(in crate::harness) fn binding(label: &str) -> owner::GrantBinding {
    aterm_json::from_value(aterm_json::json!({
        "request":{"id":"a".repeat(32),"caller":"b".repeat(64),"label":label,"revision":"c".repeat(64)},
        "reference":{"operation":"a".repeat(32),"capability":"d".repeat(64),"issuance_digest":"e".repeat(64),"producer_mac":"f".repeat(64)},"generation":1
    })).unwrap()
}

pub(in crate::harness) fn source(
    binding: owner::GrantBinding,
    path: PathBuf,
    generation: u64,
) -> SourceHandle {
    SourceHandle {
        policy: None,
        binding: std::sync::Arc::new(binding),
        slot: "1".repeat(64),
        path,
        account: "2".repeat(64),
        user: "3".repeat(64),
        revision: Revision {
            generation,
            digest: format!("{generation:064x}"),
        },
        namespaces: None,
        assignment_generation: 0,
        source_authority: codex_assignment::SourceAuthority::Startup {
            nonce: "4".repeat(32),
        },
    }
}

/// Synthetic constructor-epoch mutation for immutable-scope refusal controls.
pub(in crate::harness) fn with_authority(
    mut source: SourceHandle,
    authority: codex_assignment::SourceAuthority,
) -> SourceHandle {
    source.source_authority = authority;
    source
}

/// Synthetic positive assignment correlation for ledger tests. This provides
/// no signed Book, installed role, supported image, or publication authority.
pub(in crate::harness) fn with_assignment(
    mut source: SourceHandle,
    generation: u64,
    operation: &str,
) -> SourceHandle {
    source.assignment_generation = generation;
    source.source_authority = codex_assignment::SourceAuthority::Assignment {
        operation: operation.into(),
    };
    source
}

/// Adds only actual physical metadata for synthetic census controls. No signed
/// Book/Slot, owner, supported image, or production source authority is minted.
pub(in crate::harness) fn with_namespaces(
    mut source: SourceHandle,
    book: super::super::pending_native_store::PhysicalIdentity,
    slot: super::super::pending_native_store::PhysicalIdentity,
) -> SourceHandle {
    source.namespaces = Some(std::sync::Arc::new(SourceNamespaces { book, slot }));
    source
}
