// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser tests — split into focused submodules.

use super::*;

mod basic;
mod batch;
mod c1;
mod csi;
mod invariants;
mod performance;
mod refinement;
mod subparams;
mod table_validity;
mod utf8_errors;
mod utf8_parity;

/// Test sink that records all actions for verification.
#[derive(Default)]
struct RecordingSink {
    prints: Vec<char>,
    executes: Vec<u8>,
    csi_dispatches: Vec<(Vec<u16>, Vec<u8>, u8)>,
    /// CSI dispatches with subparam mask: (params, intermediates, final_byte, subparam_mask)
    csi_dispatches_with_subparams: Vec<(Vec<u16>, Vec<u8>, u8, u32)>,
    esc_dispatches: Vec<(Vec<u8>, u8)>,
    osc_dispatches: Vec<Vec<Vec<u8>>>,
    dcs_hooks: Vec<(Vec<u16>, Vec<u8>, u8)>,
    dcs_puts: Vec<u8>,
    dcs_unhooks: usize,
    apc_starts: usize,
    apc_data: Vec<u8>,
    apc_ends: usize,
}

impl ActionSink for RecordingSink {
    fn print(&mut self, c: char) {
        self.prints.push(c);
    }
    fn execute(&mut self, byte: u8) {
        self.executes.push(byte);
    }
    fn csi_dispatch(
        &mut self,
        params: &aterm_provenance::Provenance<[u16], aterm_provenance::Pty>,
        intermediates: &aterm_provenance::Provenance<[u8], aterm_provenance::Pty>,
        final_byte: u8,
    ) {
        self.csi_dispatches.push((
            params.as_ref().to_vec(),
            intermediates.as_ref().to_vec(),
            final_byte,
        ));
    }
    fn csi_dispatch_with_subparams(
        &mut self,
        params: &aterm_provenance::Provenance<[u16], aterm_provenance::Pty>,
        intermediates: &aterm_provenance::Provenance<[u8], aterm_provenance::Pty>,
        final_byte: u8,
        subparam_mask: u32,
    ) {
        self.csi_dispatches_with_subparams.push((
            params.as_ref().to_vec(),
            intermediates.as_ref().to_vec(),
            final_byte,
            subparam_mask,
        ));
    }
    fn esc_dispatch(
        &mut self,
        intermediates: &aterm_provenance::Provenance<[u8], aterm_provenance::Pty>,
        final_byte: u8,
    ) {
        self.esc_dispatches
            .push((intermediates.as_ref().to_vec(), final_byte));
    }
    fn osc_dispatch(
        &mut self,
        params: &aterm_provenance::Provenance<[&[u8]], aterm_provenance::Pty>,
    ) {
        self.osc_dispatches
            .push(params.as_ref().iter().map(|p| p.to_vec()).collect());
    }
    fn dcs_hook(
        &mut self,
        params: &aterm_provenance::Provenance<[u16], aterm_provenance::Pty>,
        intermediates: &aterm_provenance::Provenance<[u8], aterm_provenance::Pty>,
        final_byte: u8,
    ) {
        self.dcs_hooks.push((
            params.as_ref().to_vec(),
            intermediates.as_ref().to_vec(),
            final_byte,
        ));
    }
    fn dcs_put(&mut self, byte: u8) {
        self.dcs_puts.push(byte);
    }
    fn dcs_unhook(&mut self, _canceled: bool) {
        self.dcs_unhooks += 1;
    }
    fn apc_start(&mut self) {
        self.apc_starts += 1;
    }
    fn apc_put(&mut self, byte: u8) {
        self.apc_data.push(byte);
    }
    fn apc_end(&mut self) {
        self.apc_ends += 1;
    }
}

impl BatchActionSink for RecordingSink {
    fn print_str(&mut self, s: &aterm_provenance::Provenance<str, aterm_provenance::Pty>) {
        self.prints.extend(s.as_ref().chars());
    }
}
