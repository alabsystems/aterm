// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Minimal perfect hash table for CSI dispatch routing.

/// CSI handler identifier for table-driven dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum CsiHandler {
    AnsiDecrqm,
    BackwardTab,
    CharPositionAbs,
    CursorBackward,
    CursorDown,
    CursorForward,
    CharPositionRelative,
    CursorHorizAbs,
    CursorNextLine,
    CursorPosition,
    CursorPositionAlt,
    CursorPrevLine,
    CursorUp,
    DecDsr,
    DecModeReset,
    DecModeSet,
    Deccara,
    Deccra,
    Decdc,
    Decera,
    Decfra,
    Decic,
    Decrqm,
    Decsca,
    Decscl,
    Decscusr,
    Decsera,
    Decst8c,
    Decstr,
    DeleteChars,
    DeleteLines,
    DeviceStatusReport,
    EraseChars,
    EraseDisplay,
    EraseLine,
    ForwardTab,
    InsertChars,
    InsertLines,
    KittyKeyboardPop,
    KittyKeyboardPush,
    KittyKeyboardQuery,
    KittyKeyboardSet,
    KittyUnscroll,
    LinePositionAbs,
    LinePositionRel,
    PrimaryDA,
    RepeatChar,
    ResetMode,
    RestoreCursor,
    SaveCursor,
    Scp,
    ScrollDown,
    ScrollUp,
    SecondaryDA,
    SelectiveEraseDisplay,
    SelectiveEraseLine,
    SetMode,
    SetScrollRegion,
    Sgr,
    TabClear,
    TertiaryDA,
    XtermFmtkeysQuery,
    XtermModkeysQuery,
    XtfmtkeysSet,
    XtmodkeysDisable,
    XtmodkeysSet,
    Xtpopsgr,
    Xtpushsgr,
    Xtrestore,
    Xtsave,
    Xtsmgraphics,
    Xtversion,
    Xtwinops,
}

/// Hash parameters found by brute-force and verified by z4 SMT solver.
const HASH_A: u32 = 0x7ac3_dd23;
const HASH_B: u32 = 0xe2df_c703;
const HASH_C: u32 = 0;
const TABLE_SIZE: usize = 126;

/// Encode CSI intermediate bytes as a single u8 for hashing.
#[inline]
pub(crate) const fn encode_intermediates(intermediates: &[u8]) -> u8 {
    match intermediates {
        [] => 0x00,
        [b'?', b'$'] => 0x01,
        [single] => *single,
        _ => 0xFF, // unrecognized
    }
}

/// Compute the hash index for a CSI key.
#[inline]
pub(crate) const fn csi_hash(intermediate_enc: u8, final_byte: u8) -> usize {
    let key = (intermediate_enc as u32) << 8 | final_byte as u32;
    (key.wrapping_mul(HASH_A).wrapping_add(HASH_B) >> HASH_C) as usize % TABLE_SIZE
}

/// Dispatch table: each slot is `(intermediate_enc, final_byte, handler)`.
pub(crate) static CSI_DISPATCH_TABLE: [Option<(u8, u8, CsiHandler)>; TABLE_SIZE] = [
    Some((0x00, 0x43, CsiHandler::CursorForward)), // [] 'C'
    Some((0x00, 0x42, CsiHandler::CursorDown)),    // [] 'B'
    None,
    Some((0x00, 0x44, CsiHandler::CursorBackward)), // [] 'D'
    None,
    Some((0x00, 0x46, CsiHandler::CursorPrevLine)), // [] 'F'
    Some((0x00, 0x45, CsiHandler::CursorNextLine)), // [] 'E'
    Some((0x00, 0x48, CsiHandler::CursorPosition)), // [] 'H'
    Some((0x00, 0x47, CsiHandler::CursorHorizAbs)), // [] 'G'
    Some((0x00, 0x4a, CsiHandler::EraseDisplay)),   // [] 'J'
    Some((0x00, 0x49, CsiHandler::ForwardTab)),     // [] 'I'
    Some((0x00, 0x4c, CsiHandler::InsertLines)),    // [] 'L'
    Some((0x00, 0x4b, CsiHandler::EraseLine)),      // [] 'K'
    None,
    Some((0x00, 0x4d, CsiHandler::DeleteLines)), // [] 'M'
    Some((0x00, 0x50, CsiHandler::DeleteChars)), // [] 'P'
    Some((0x22, 0x71, CsiHandler::Decsca)),      // ["] 'q'
    Some((0x22, 0x70, CsiHandler::Decscl)),      // ["] 'p'
    None,
    Some((0x00, 0x54, CsiHandler::ScrollDown)), // [] 'T'
    Some((0x00, 0x53, CsiHandler::ScrollUp)),   // [] 'S'
    None,
    None,
    Some((0x00, 0x58, CsiHandler::EraseChars)), // [] 'X'
    None,
    Some((0x00, 0x5a, CsiHandler::BackwardTab)), // [] 'Z'
    None,
    None,
    None,
    Some((0x27, 0x7e, CsiHandler::Decdc)), // ['] '~'
    Some((0x27, 0x7d, CsiHandler::Decic)), // ['] '}'
    None,
    None,
    None,
    Some((0x00, 0x61, CsiHandler::CharPositionRelative)), // [] 'a'
    Some((0x00, 0x60, CsiHandler::CharPositionAbs)),      // [] '`'
    Some((0x00, 0x63, CsiHandler::PrimaryDA)),            // [] 'c'
    Some((0x00, 0x62, CsiHandler::RepeatChar)),           // [] 'b'
    Some((0x00, 0x65, CsiHandler::LinePositionRel)),      // [] 'e'
    Some((0x00, 0x64, CsiHandler::LinePositionAbs)),      // [] 'd'
    Some((0x00, 0x67, CsiHandler::TabClear)),             // [] 'g'
    Some((0x00, 0x66, CsiHandler::CursorPositionAlt)),    // [] 'f'
    Some((0x3c, 0x75, CsiHandler::KittyKeyboardPop)),     // [<] 'u'
    Some((0x00, 0x68, CsiHandler::SetMode)),              // [] 'h'
    Some((0x3d, 0x63, CsiHandler::TertiaryDA)),           // [=] 'c'
    None,
    Some((0x00, 0x6d, CsiHandler::Sgr)),       // [] 'm'
    Some((0x00, 0x6c, CsiHandler::ResetMode)), // [] 'l'
    None,
    Some((0x00, 0x6e, CsiHandler::DeviceStatusReport)), // [] 'n'
    None,
    None,
    Some((0x00, 0x73, CsiHandler::SaveCursor)), // [] 's'
    Some((0x00, 0x72, CsiHandler::SetScrollRegion)), // [] 'r'
    Some((0x23, 0x7b, CsiHandler::Xtpushsgr)),  // [#] '{'
    Some((0x00, 0x74, CsiHandler::Xtwinops)),   // [] 't'
    Some((0x23, 0x7d, CsiHandler::Xtpopsgr)),   // [#] '}'
    None,
    Some((0x00, 0x75, CsiHandler::RestoreCursor)), // [] 'u'
    None,
    None,
    None,
    None,
    None,
    None,
    Some((0x24, 0x70, CsiHandler::AnsiDecrqm)), // [$] 'p'
    Some((0x3d, 0x75, CsiHandler::KittyKeyboardSet)), // [=] 'u'
    Some((0x24, 0x72, CsiHandler::Deccara)),    // [$] 'r'
    Some((0x3f, 0x4b, CsiHandler::SelectiveEraseLine)), // [?] 'K'
    Some((0x3f, 0x4a, CsiHandler::SelectiveEraseDisplay)), // [?] 'J'
    None,
    Some((0x24, 0x76, CsiHandler::Deccra)),       // [$] 'v'
    Some((0x3e, 0x63, CsiHandler::SecondaryDA)),  // [>] 'c'
    Some((0x3e, 0x66, CsiHandler::XtfmtkeysSet)), // [>] 'f'
    None,
    Some((0x01, 0x70, CsiHandler::Decrqm)), // [?$] 'p'
    None,
    Some((0x24, 0x78, CsiHandler::Decfra)),       // [$] 'x'
    Some((0x24, 0x7b, CsiHandler::Decsera)),      // [$] '{'
    Some((0x24, 0x7a, CsiHandler::Decera)),       // [$] 'z'
    Some((0x3f, 0x53, CsiHandler::Xtsmgraphics)), // [?] 'S'
    Some((0x3e, 0x6e, CsiHandler::XtmodkeysDisable)), // [>] 'n'
    Some((0x3e, 0x6d, CsiHandler::XtmodkeysSet)), // [>] 'm'
    Some((0x2b, 0x54, CsiHandler::KittyUnscroll)), // [+] 'T'
    Some((0x3f, 0x57, CsiHandler::Decst8c)),      // [?] 'W'
    None,
    Some((0x3e, 0x71, CsiHandler::Xtversion)), // [>] 'q'
    None,
    Some((0x20, 0x6b, CsiHandler::Scp)), // [ ] 'k'
    None,
    Some((0x3e, 0x75, CsiHandler::KittyKeyboardPush)), // [>] 'u'
    None,
    None,
    None,
    Some((0x20, 0x71, CsiHandler::Decscusr)), // [ ] 'q'
    None,
    None,
    None,
    None,
    None,
    Some((0x3f, 0x67, CsiHandler::XtermFmtkeysQuery)), // [?] 'g'
    None,
    None,
    Some((0x3f, 0x68, CsiHandler::DecModeSet)), // [?] 'h'
    None,
    None,
    Some((0x3f, 0x6d, CsiHandler::XtermModkeysQuery)), // [?] 'm'
    Some((0x3f, 0x6c, CsiHandler::DecModeReset)),      // [?] 'l'
    None,
    Some((0x3f, 0x6e, CsiHandler::DecDsr)), // [?] 'n'
    None,
    None,
    Some((0x3f, 0x73, CsiHandler::Xtsave)),    // [?] 's'
    Some((0x3f, 0x72, CsiHandler::Xtrestore)), // [?] 'r'
    Some((0x3f, 0x75, CsiHandler::KittyKeyboardQuery)), // [?] 'u'
    None,
    None,
    None,
    None,
    Some((0x21, 0x70, CsiHandler::Decstr)), // [!] 'p'
    None,
    None,
    None,
    None,
    Some((0x00, 0x41, CsiHandler::CursorUp)),    // [] 'A'
    Some((0x00, 0x40, CsiHandler::InsertChars)), // [] '@'
];

/// Look up the CSI handler for the given intermediates and final byte.
#[inline]
pub(crate) fn lookup_csi_handler(intermediates: &[u8], final_byte: u8) -> Option<CsiHandler> {
    let inter_enc = encode_intermediates(intermediates);
    let idx = csi_hash(inter_enc, final_byte);
    let &(expected_inter, expected_fb, handler) = CSI_DISPATCH_TABLE.get(idx)?.as_ref()?;
    if expected_inter == inter_enc && expected_fb == final_byte {
        Some(handler)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_sits_in_its_own_hash_slot() {
        let mut occupied = 0;
        for (slot, entry) in CSI_DISPATCH_TABLE.iter().enumerate() {
            if let Some((inter_enc, final_byte, _)) = *entry {
                occupied += 1;
                assert_eq!(
                    csi_hash(inter_enc, final_byte),
                    slot,
                    "entry ({inter_enc:#04x}, {final_byte:#04x}) is in slot {slot}"
                );
            }
        }
        assert!(occupied > 0);
    }

    #[test]
    fn test_unknown_returns_none() {
        assert_eq!(lookup_csi_handler(&[], b'Q'), None);
        assert_eq!(lookup_csi_handler(&[b'%'], b'A'), None);
        assert_eq!(lookup_csi_handler(&[b'?'], b'z'), None);
        assert_eq!(lookup_csi_handler(&[b'?', b'$', b'!'], b'p'), None);
        assert_eq!(lookup_csi_handler(&[b'>', b'$'], b'p'), None);
    }
}
