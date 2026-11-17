// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep the GUI main-entry image within the shared per-thread TLS budget. The composed
//! application has its own check of the same parser and budget in aterm.

#![cfg(target_os = "macos")]

#[path = "support/thread_local_template.rs"]
mod template;

use template::*;

#[test]
fn every_threads_copy_of_the_thread_local_template_stays_small() {
    assert_binary_template_stays_small("aterm-gui");
}

/// A thin Mach-O with section headers and a symbol table. The parser only needs those, not
/// mapped section contents or a runnable executable.
fn macho_fixture(sections: &[(&str, u64, u64, u32)], symbols: &[(usize, u64, &str)]) -> Vec<u8> {
    let segment_len = SEGMENT_LEN + sections.len() * SECTION_LEN;
    let symtab = HEADER_LEN + segment_len;
    let symoff = symtab + 24;
    let stroff = symoff + symbols.len() * NLIST_LEN;
    let mut strings = vec![0];
    let mut names = Vec::new();
    for (_, _, name) in symbols {
        names.push(u32::try_from(strings.len()).unwrap());
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }
    let mut image = vec![0; stroff + strings.len()];
    let put32 = |image: &mut [u8], at: usize, value: u32| {
        image[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    put32(&mut image, 0, MH_MAGIC_64);
    put32(&mut image, 16, 2);
    put32(&mut image, 20, u32::try_from(segment_len + 24).unwrap());
    put32(&mut image, HEADER_LEN, LC_SEGMENT_64);
    put32(
        &mut image,
        HEADER_LEN + 4,
        u32::try_from(segment_len).unwrap(),
    );
    put32(
        &mut image,
        HEADER_LEN + 64,
        u32::try_from(sections.len()).unwrap(),
    );
    for (i, (name, addr, size, flags)) in sections.iter().enumerate() {
        let at = HEADER_LEN + SEGMENT_LEN + i * SECTION_LEN;
        assert!(name.len() <= 16);
        image[at..at + name.len()].copy_from_slice(name.as_bytes());
        image[at + 16..at + 22].copy_from_slice(b"__DATA");
        image[at + 32..at + 40].copy_from_slice(&addr.to_le_bytes());
        image[at + 40..at + 48].copy_from_slice(&size.to_le_bytes());
        put32(&mut image, at + 64, *flags);
    }
    put32(&mut image, symtab, LC_SYMTAB);
    put32(&mut image, symtab + 4, 24);
    put32(&mut image, symtab + 8, u32::try_from(symoff).unwrap());
    put32(
        &mut image,
        symtab + 12,
        u32::try_from(symbols.len()).unwrap(),
    );
    put32(&mut image, symtab + 16, u32::try_from(stroff).unwrap());
    put32(
        &mut image,
        symtab + 20,
        u32::try_from(strings.len()).unwrap(),
    );
    for (i, ((section, addr, _), name)) in symbols.iter().zip(names).enumerate() {
        let at = symoff + i * NLIST_LEN;
        put32(&mut image, at, name);
        image[at + 4] = N_SECT;
        image[at + 5] = u8::try_from(*section).unwrap();
        image[at + 8..at + 16].copy_from_slice(&addr.to_le_bytes());
    }
    image[stroff..].copy_from_slice(&strings);
    image
}

#[test]
fn template_coverage_counts_unnamed_prefix_and_inter_section_gap_without_inventing_locals() {
    let image = macho_fixture(
        &[
            ("__ordinary", 0x1000, 16, 0),
            ("__thread_data", 0x2000, 20, S_THREAD_LOCAL_REGULAR),
            ("__thread_bss", 0x2020, 8, S_THREAD_LOCAL_ZEROFILL),
        ],
        &[
            (2, 0x2004, "_first"),
            (2, 0x2004, "_first_alias"),
            (2, 0x2010, "_second"),
            (3, 0x2020, "_zero"),
        ],
    );
    let template = thread_local_template(&image);
    let sizes = thread_locals_by_size(&image, &template);
    assert_eq!((template.data, template.bss, template.bytes()), (20, 8, 40));
    assert_eq!(
        sizes.named.len(),
        3,
        "aliases do not allocate another local"
    );
    assert_eq!(
        sizes
            .named
            .iter()
            .map(|(bytes, _)| *bytes)
            .collect::<Vec<_>>(),
        [12, 8, 4],
    );
    assert_eq!(
        sizes
            .unnamed
            .iter()
            .map(|(bytes, _)| *bytes)
            .collect::<Vec<_>>(),
        [4, 12],
    );
    assert_eq!(
        sizes
            .named
            .iter()
            .chain(&sizes.unnamed)
            .map(|(bytes, _)| bytes)
            .sum::<u64>(),
        template.bytes(),
    );
}

#[test]
fn a_symbol_outside_its_tls_section_cannot_mask_unaccounted_template_bytes() {
    for addr in [0x1fff, 0x2011] {
        let image = macho_fixture(
            &[("__thread_data", 0x2000, 16, S_THREAD_LOCAL_REGULAR)],
            &[(1, addr, "_outside")],
        );
        let template = thread_local_template(&image);
        assert!(
            std::panic::catch_unwind(|| thread_locals_by_size(&image, &template)).is_err(),
            "out-of-section symbol {addr:#x} must fail instead of producing a saturated size",
        );
    }
}

#[test]
fn a_wholly_unsymbolized_tls_section_cannot_be_reported_as_padding() {
    let image = macho_fixture(
        &[
            ("__thread_data", 0x2000, 16, S_THREAD_LOCAL_REGULAR),
            ("__thread_bss", 0x2010, 8, S_THREAD_LOCAL_ZEROFILL),
        ],
        &[(1, 0x2000, "_named")],
    );
    let template = thread_local_template(&image);
    assert_eq!(template.bytes(), 24);
    assert!(std::panic::catch_unwind(|| thread_locals_by_size(&image, &template)).is_err());
}

#[test]
fn a_thread_locals_symbol_reads_as_the_path_it_was_declared_at() {
    let named = |symbol: &str| path_of(&format!("{symbol}$tlv$init"));
    // A `thread_local!` in a module, and one in a function, whose closure is numbered.
    assert_eq!(
        named("__RNvNCNKNvNtCs1a_9aterm_gui9tab_marks6ACTING0023___RUST_STD_INTERNAL_VAL"),
        "aterm_gui::tab_marks::ACTING"
    );
    assert_eq!(
        named(
            "__RNvNCNKNvNvNtCs1a_9aterm_gui11chrome_band11band_colors4LAST0s_023___RUST_STD_INTERNAL_VAL"
        ),
        "aterm_gui::chrome_band::band_colors::LAST"
    );
    // The standard library's own `#[thread_local]` statics.
    assert_eq!(
        named("__RNvNtNtCs2b_3std6thread7current7CURRENT"),
        "std::thread::current::CURRENT"
    );
    // One in a method: the `impl`'s module, then its type.
    assert_eq!(
        named(
            "__RNvNCNKNvNvMs2_Cs3c_11aterm_regexNtBd_5Regex6search5CACHE0023___RUST_STD_INTERNAL_VAL"
        ),
        "aterm_regex::Regex::search::CACHE"
    );
    // Any other symbol is given as written.
    assert_eq!(named("_a_c_thread_local"), "_a_c_thread_local");
    let in_trait_impl = "__RNvXCs1a_3libNtB2_4TypeNtB2_5Trait6method4HELD";
    assert_eq!(named(in_trait_impl), in_trait_impl);
}
