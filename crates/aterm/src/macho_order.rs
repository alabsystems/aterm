// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mach-O 64-bit object/archive/executable symbol reader and locality orderer for
//! macOS release links (`ld -order_file`).
//!
//! Under fat LTO (`lto = true, codegen-units = 1`), LLVM merges functions and generic
//! instantiations from every crate in the binary into a single `.rcgu.o`, scattering
//! cold CLI crates and unrelated UI modules across the same 16 KiB `__TEXT` pages as
//! startup/idle paths.
//!
//! Two complementary, zero-registry ordering passes cooperate here:
//! 1. Structural Rust v0 / Itanium `(tier, crate, module, sub_tier)` grouping with
//!    in-module object-address preservation, which packs every crate and module
//!    (together with its monomorphized `core`/`alloc`/`std` helpers and destructors)
//!    onto contiguous pages.
//! 2. Multi-permutation headless startup page-fault intersection (`CS_VALIDATED` across
//!    independent symbol permutations), which isolates the ~3% of functions actually
//!    executed during startup and idle park into a dense prefix at the front of `__TEXT`
//!    without any hand-maintained symbol or module list.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MH_MAGIC_64: u32 = 0xfeed_facf;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const MH_OBJECT: u32 = 0x1;
const MH_EXECUTE: u32 = 0x2;
const LC_SYMTAB: u32 = 0x2;
const LC_SEGMENT_64: u32 = 0x19;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_SECT: u8 = 0x0e;
const ARM64_PAGE_MASK: u64 = !0x3fff;
const ARM64_PAGE_BYTES: u64 = 16_384;

/// Execution-temperature tier used as the static sort key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SymbolTier {
    /// Process entry (`_main`) and pure runtime / standard-library primitives.
    Runtime = 0,
    /// Normal forward-execution functions, methods, closures, vtables, and module-local
    /// destructor glue, grouped by `(domain_crate, domain_module, sub_tier)`.
    Live = 1,
    /// `.cold` split blocks, panic formatting, backtrace capture, DWARF/symbolication,
    /// and demangling.
    Cold = 2,
}

/// Extracted `(tier, crate_name, module_name, sub_tier)` locality key for a symbol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalityKey {
    pub tier: SymbolTier,
    pub crate_name: String,
    pub module_name: String,
    /// `0` for forward-execution items, `1` for `drop_glue` / `Drop::drop` so teardown
    /// code sits immediately at the tail of its own `(crate, module)` rather than
    /// splitting forward paths or faulting a distant segment.
    pub sub_tier: u8,
}

#[derive(Clone, Debug)]
struct RawSym {
    name: String,
    file_order: u32,
    addr: u64,
}

/// Expands `@response_file` tokens (up to depth 2) in linker arguments.
#[must_use]
pub(crate) fn expand_linker_args(args: &[String]) -> Vec<String> {
    let mut expanded = Vec::with_capacity(args.len());
    expand_response_args(args, 0, &mut expanded);
    expanded
}

/// Collects `.o`, `.rlib`, and `.a` input paths from linker arguments, expanding `@file`
/// response files and `-filelist <path>` lists.
#[must_use]
pub(crate) fn collect_input_paths(args: &[String]) -> Vec<PathBuf> {
    let expanded = expand_linker_args(args);

    let mut out = Vec::new();
    let mut i = 0;
    while i < expanded.len() {
        let arg = &expanded[i];
        match arg.as_str() {
            "-o"
            | "-lto_library"
            | "-order_file"
            | "-exported_symbols_list"
            | "-unexported_symbols_list"
            | "-alias_list"
            | "-dtrace" => {
                i += 2;
                continue;
            }
            "-filelist" => {
                if let Some(list_path) = expanded.get(i + 1)
                    && let Ok(text) = std::fs::read_to_string(list_path)
                {
                    for line in text.lines() {
                        let entry = line.split(',').next().unwrap_or("").trim();
                        if !entry.is_empty() {
                            out.push(PathBuf::from(entry));
                        }
                    }
                }
                i += 2;
                continue;
            }
            _ => {}
        }

        if !arg.starts_with('-') && has_object_or_archive_ext(arg) {
            out.push(PathBuf::from(arg));
        }
        i += 1;
    }
    out
}

fn expand_response_args(args: &[String], depth: usize, out: &mut Vec<String>) {
    for arg in args {
        if depth < 2
            && let Some(resp_path) = arg.strip_prefix('@')
            && let Ok(content) = std::fs::read_to_string(resp_path)
        {
            let nested: Vec<String> = split_response_tokens(&content);
            expand_response_args(&nested, depth + 1, out);
            continue;
        }
        out.push(arg.clone());
    }
}

fn split_response_tokens(content: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == '\\'
                && let Some(escaped) = chars.next()
            {
                cur.push(escaped);
            } else if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else if ch == '\\'
            && let Some(escaped) = chars.next()
        {
            cur.push(escaped);
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn has_object_or_archive_ext(path: &str) -> bool {
    path.ends_with(".o") || path.ends_with(".rlib") || path.ends_with(".a")
}

/// Reads all Mach-O object and archive inputs and returns the deduplicated static symbol
/// order list for Apple `ld -order_file`.
#[must_use]
pub(crate) fn build_order_list(paths: &[PathBuf]) -> Vec<String> {
    let mut raw = Vec::new();
    for (idx, path) in paths.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let file_order = u32::try_from(idx).unwrap_or(u32::MAX);
        extract_container_symbols(&bytes, file_order, &mut raw);
    }
    order_raw_symbols(raw)
}

/// Deterministically permutes `symbols` together with `spacers` using `seed` so each
/// probe link scatters cold page spacers among real symbols, diluting hot-page density
/// well below 1 hot function per 16 KiB page.
#[must_use]
pub(crate) fn permute_order_list_with_spacers(
    symbols: &[String],
    spacers: &[String],
    seed: u64,
) -> Vec<String> {
    let mut hashed: Vec<(u64, &String)> = symbols
        .iter()
        .chain(spacers.iter())
        .map(|s| (fnv1a_64(seed, s.as_bytes()), s))
        .collect();
    hashed.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    hashed.into_iter().map(|(_, s)| s.clone()).collect()
}

/// Writes a sparse arm64 Mach-O `MH_OBJECT` containing `text_spacers` inert `__TEXT,__text`
/// atoms and `const_spacers` inert `__TEXT,__const` atoms of `spacer_bytes` each (marked
/// `MH_SUBSECTIONS_VIA_SYMBOLS` + `S_ATTR_NO_DEAD_STRIP` + `N_NO_DEAD_STRIP`), returning
/// their symbol names for `permute_order_list_with_spacers`.
pub(crate) fn write_probe_spacer_object(
    path: &Path,
    text_spacers: usize,
    const_spacers: usize,
    spacer_bytes: usize,
) -> std::io::Result<Vec<String>> {
    use std::io::{Seek, SeekFrom, Write};

    let step = u64::try_from(spacer_bytes.max(16)).unwrap_or(4096);
    let text_size = u64::try_from(text_spacers)
        .unwrap_or(0)
        .saturating_mul(step);
    let const_size = u64::try_from(const_spacers)
        .unwrap_or(0)
        .saturating_mul(step);
    let total_sect_bytes = text_size.saturating_add(const_size);

    let data_fileoff: u64 = 4096;
    let symoff_u64 = data_fileoff.saturating_add(total_sect_bytes);
    let symoff = u32::try_from(symoff_u64).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "spacer object too large")
    })?;
    let total_syms = text_spacers.saturating_add(const_spacers);
    let nsyms = u32::try_from(total_syms)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "too many spacers"))?;

    let mut names = Vec::with_capacity(total_syms);
    let mut strtab = Vec::with_capacity(total_syms.saturating_mul(16).saturating_add(4));
    strtab.push(0);
    let mut sym_records = Vec::with_capacity(total_syms.saturating_mul(16));

    let mut push_sym = |name: String, sect: u8, addr: u64| -> std::io::Result<()> {
        let strx = u32::try_from(strtab.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "strtab overflow")
        })?;
        strtab.extend_from_slice(name.as_bytes());
        strtab.push(0);
        sym_records.extend_from_slice(&strx.to_le_bytes());
        sym_records.push(N_SECT);
        sym_records.push(sect);
        sym_records.extend_from_slice(&0x0020_u16.to_le_bytes()); // N_NO_DEAD_STRIP
        sym_records.extend_from_slice(&addr.to_le_bytes());
        names.push(name);
        Ok(())
    };

    for i in 0..text_spacers {
        push_sym(format!("__atpad_t_{i:05}"), 1, (i as u64) * step)?;
    }
    for j in 0..const_spacers {
        push_sym(
            format!("__atpad_c_{j:05}"),
            2,
            text_size + (j as u64) * step,
        )?;
    }

    let stroff = nsyms
        .checked_mul(16)
        .and_then(|len| symoff.checked_add(len))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "stroff overflow"))?;
    let strsize = u32::try_from(strtab.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "strsize overflow"))?;
    let const_fileoff = u32::try_from(data_fileoff + text_size).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "const_fileoff overflow")
    })?;

    let seg_cmd_size: u32 = 72 + 80 * 2;
    let build_ver_cmd_size: u32 = 24;
    let sym_cmd_size: u32 = 24;
    let sizeofcmds = seg_cmd_size + build_ver_cmd_size + sym_cmd_size;

    let mut hdr = Vec::with_capacity(320);
    hdr.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
    hdr.extend_from_slice(&0x0100_000c_u32.to_le_bytes()); // CPU_TYPE_ARM64
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&MH_OBJECT.to_le_bytes());
    hdr.extend_from_slice(&3u32.to_le_bytes()); // ncmds
    hdr.extend_from_slice(&sizeofcmds.to_le_bytes());
    hdr.extend_from_slice(&0x2000_u32.to_le_bytes()); // MH_SUBSECTIONS_VIA_SYMBOLS
    hdr.extend_from_slice(&0u32.to_le_bytes());

    // LC_SEGMENT_64
    hdr.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
    hdr.extend_from_slice(&seg_cmd_size.to_le_bytes());
    hdr.extend_from_slice(&[0u8; 16]);
    hdr.extend_from_slice(&0u64.to_le_bytes());
    hdr.extend_from_slice(&total_sect_bytes.to_le_bytes());
    hdr.extend_from_slice(&data_fileoff.to_le_bytes());
    hdr.extend_from_slice(&total_sect_bytes.to_le_bytes());
    hdr.extend_from_slice(&7u32.to_le_bytes());
    hdr.extend_from_slice(&7u32.to_le_bytes());
    hdr.extend_from_slice(&2u32.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());

    let push_sect =
        |buf: &mut Vec<u8>, seg: &[u8], sect: &[u8], addr: u64, sz: u64, off: u32, flags: u32| {
            let mut sect_field = [0u8; 16];
            sect_field[..sect.len()].copy_from_slice(sect);
            let mut seg_field = [0u8; 16];
            seg_field[..seg.len()].copy_from_slice(seg);
            buf.extend_from_slice(&sect_field);
            buf.extend_from_slice(&seg_field);
            buf.extend_from_slice(&addr.to_le_bytes());
            buf.extend_from_slice(&sz.to_le_bytes());
            buf.extend_from_slice(&off.to_le_bytes());
            buf.extend_from_slice(&4u32.to_le_bytes()); // 16-byte align
            buf.extend_from_slice(&0u32.to_le_bytes()); // reloff
            buf.extend_from_slice(&0u32.to_le_bytes()); // nreloc
            buf.extend_from_slice(&flags.to_le_bytes());
            buf.extend_from_slice(&[0u8; 12]);
        };
    push_sect(
        &mut hdr,
        b"__TEXT",
        b"__text",
        0,
        text_size,
        4096,
        0x9000_0400, // S_ATTR_PURE_INSTRUCTIONS | S_ATTR_NO_DEAD_STRIP | S_ATTR_SOME_INSTRUCTIONS
    );
    push_sect(
        &mut hdr,
        b"__TEXT",
        b"__const",
        text_size,
        const_size,
        const_fileoff,
        0x1000_0000, // S_ATTR_NO_DEAD_STRIP
    );

    // LC_BUILD_VERSION (platform = macOS, minos = 14.0)
    hdr.extend_from_slice(&0x32_u32.to_le_bytes());
    hdr.extend_from_slice(&build_ver_cmd_size.to_le_bytes());
    hdr.extend_from_slice(&1u32.to_le_bytes());
    hdr.extend_from_slice(&0x000e_0000_u32.to_le_bytes());
    hdr.extend_from_slice(&0x000e_0000_u32.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());

    // LC_SYMTAB
    hdr.extend_from_slice(&LC_SYMTAB.to_le_bytes());
    hdr.extend_from_slice(&sym_cmd_size.to_le_bytes());
    hdr.extend_from_slice(&symoff.to_le_bytes());
    hdr.extend_from_slice(&nsyms.to_le_bytes());
    hdr.extend_from_slice(&stroff.to_le_bytes());
    hdr.extend_from_slice(&strsize.to_le_bytes());

    let mut file = std::fs::File::create(path)?;
    file.write_all(&hdr)?;
    file.seek(SeekFrom::Start(symoff_u64))?;
    file.write_all(&sym_records)?;
    file.write_all(&strtab)?;
    file.flush()?;
    Ok(names)
}

fn fnv1a_64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    // Final avalanche mix so adjacent symbol suffixes scatter uniformly.
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash
}

/// Promotes symbols whose pages faulted across `probe_count` independent permuted runs
/// (`hits == probe_count` in bucket 0, `hits + 1 == probe_count` when `probe_count >= 4`
/// in bucket 1, and cold symbols in bucket 2), preserving `base_ordered`'s `(crate, module)`
/// and intra-module order inside each bucket.
#[must_use]
pub(crate) fn refine_order_with_probe_hits(
    base_ordered: &[String],
    hit_sets: &[HashSet<String>],
) -> Vec<String> {
    let probe_count = hit_sets.len();
    if probe_count < 2 {
        return base_ordered.to_vec();
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for set in hit_sets {
        for sym in set {
            *counts.entry(sym.as_str()).or_insert(0) += 1;
        }
    }

    let bucket_of = |sym: &str| -> u8 {
        if sym == "_main" || sym == "main" {
            return 0;
        }
        let hits = counts.get(sym).copied().unwrap_or(0);
        if hits == probe_count {
            0
        } else if probe_count >= 4 && hits + 1 == probe_count {
            1
        } else {
            2
        }
    };

    let mut indexed: Vec<(u8, usize, &String)> = base_ordered
        .iter()
        .enumerate()
        .map(|(idx, sym)| (bucket_of(sym), idx, sym))
        .collect();
    indexed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    indexed.into_iter().map(|(_, _, sym)| sym.clone()).collect()
}

/// Parses an unstripped Mach-O 64-bit executable (`MH_EXECUTE` or `MH_OBJECT`) and
/// returns all orderable `__TEXT` symbols whose `[addr, next_addr)` range overlaps at
/// least one unslid 16 KiB page in `faulted_pages`.
#[must_use]
pub(crate) fn extract_executable_faulted_symbols(
    bytes: &[u8],
    faulted_pages: &HashSet<u64>,
) -> HashSet<String> {
    let mut hits = HashSet::new();
    if faulted_pages.is_empty() || read_u32_le(bytes, 0) != Some(MH_MAGIC_64) {
        return hits;
    }
    if !matches!(read_u32_le(bytes, 12), Some(MH_OBJECT | MH_EXECUTE)) {
        return hits;
    }
    let Some(ncmds) = read_u32_le(bytes, 16) else {
        return hits;
    };

    let mut text_sects: [Option<(u64, u64)>; 256] = [None; 256];
    let mut next_sect_idx: usize = 1;
    let mut symtab: Option<(usize, usize, usize, usize)> = None;

    let mut cmd_off = 32usize;
    for _ in 0..ncmds {
        let (Some(cmd), Some(cmdsize)) =
            (read_u32_le(bytes, cmd_off), read_u32_le(bytes, cmd_off + 4))
        else {
            return hits;
        };
        let cmdsize = cmdsize as usize;
        let Some(cmd_end) = cmd_off
            .checked_add(cmdsize)
            .filter(|&e| cmdsize >= 8 && e <= bytes.len())
        else {
            return hits;
        };

        if cmd == LC_SEGMENT_64
            && cmdsize >= 72
            && let Some(nsects) = read_u32_le(bytes, cmd_off + 64)
        {
            for s in 0..nsects as usize {
                let Some(sect_off) = s
                    .checked_mul(80)
                    .and_then(|off| (cmd_off + 72).checked_add(off))
                else {
                    break;
                };
                let Some(sect_end) = sect_off.checked_add(80).filter(|&e| e <= cmd_end) else {
                    break;
                };
                let Some(sect_hdr) = bytes.get(sect_off..sect_end) else {
                    break;
                };
                if next_sect_idx < text_sects.len() {
                    let sectname = nul_padded_ascii(&sect_hdr[0..16]);
                    let segname = nul_padded_ascii(&sect_hdr[16..32]);
                    if is_orderable_section(segname, sectname)
                        && let (Some(addr), Some(sz)) =
                            (read_u64_le(sect_hdr, 32), read_u64_le(sect_hdr, 40))
                    {
                        text_sects[next_sect_idx] = Some((addr, addr.saturating_add(sz)));
                    }
                }
                next_sect_idx = next_sect_idx.saturating_add(1);
            }
        } else if cmd == LC_SYMTAB
            && cmdsize >= 24
            && let (Some(symoff), Some(nsyms), Some(stroff), Some(strsize)) = (
                read_u32_le(bytes, cmd_off + 8),
                read_u32_le(bytes, cmd_off + 12),
                read_u32_le(bytes, cmd_off + 16),
                read_u32_le(bytes, cmd_off + 20),
            )
        {
            symtab = Some((
                symoff as usize,
                nsyms as usize,
                stroff as usize,
                strsize as usize,
            ));
        }

        cmd_off = cmd_end;
    }

    let Some((symoff, nsyms, stroff, strsize)) = symtab else {
        return hits;
    };
    let Some(strtab) = stroff
        .checked_add(strsize)
        .and_then(|e| bytes.get(stroff..e))
    else {
        return hits;
    };
    let Some(sym_table) = nsyms
        .checked_mul(16)
        .and_then(|len| symoff.checked_add(len))
        .and_then(|e| bytes.get(symoff..e))
    else {
        return hits;
    };

    let mut sect_syms: Vec<(u8, u64, String)> = Vec::new();
    for entry in sym_table.as_chunks::<16>().0 {
        let n_type = entry[4];
        if (n_type & N_STAB) != 0 || (n_type & N_TYPE) != N_SECT {
            continue;
        }
        let n_sect = entry[5];
        if text_sects.get(n_sect as usize).and_then(|x| *x).is_none() {
            continue;
        }
        let (Some(strx), Some(addr)) = (
            read_u32_le(entry, 0).map(|v| v as usize),
            read_u64_le(entry, 8),
        ) else {
            continue;
        };
        let Some(tail) = strtab.get(strx..) else {
            continue;
        };
        let nul = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        let Ok(name) = std::str::from_utf8(&tail[..nul]) else {
            continue;
        };
        if name == "__mh_execute_header" || name.is_empty() {
            continue;
        }
        sect_syms.push((n_sect, addr, name.to_string()));
    }

    sect_syms.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    for i in 0..sect_syms.len() {
        let (sect, start, ref name) = sect_syms[i];
        if !should_order_symbol_name(name) {
            continue;
        }
        let sect_end = text_sects[sect as usize].map_or(start.saturating_add(4), |(_, end)| end);
        let mut end = sect_end;
        for next in &sect_syms[i + 1..] {
            if next.0 != sect {
                break;
            }
            if next.1 > start {
                end = next.1;
                break;
            }
        }
        let end = end
            .min(start.saturating_add(262_144))
            .max(start.saturating_add(1));
        let first_page = start & ARM64_PAGE_MASK;
        let last_page = end.saturating_sub(1) & ARM64_PAGE_MASK;
        let mut p = first_page;
        while p <= last_page {
            if faulted_pages.contains(&p) {
                hits.insert(name.clone());
                break;
            }
            let Some(next_p) = p.checked_add(ARM64_PAGE_BYTES) else {
                break;
            };
            p = next_p;
        }
    }

    hits
}

/// Parses Mach-O / BSD archive bytes into raw symbols and returns the ordered symbol list.
#[cfg(test)]
#[must_use]
pub(crate) fn build_order_list_from_slices(inputs: &[&[u8]]) -> Vec<String> {
    let mut raw = Vec::new();
    for (idx, bytes) in inputs.iter().enumerate() {
        let file_order = u32::try_from(idx).unwrap_or(u32::MAX);
        extract_container_symbols(bytes, file_order, &mut raw);
    }
    order_raw_symbols(raw)
}

fn order_raw_symbols(raw: Vec<RawSym>) -> Vec<String> {
    let mut keyed: Vec<(LocalityKey, u32, u64, String)> = raw
        .into_iter()
        .map(|sym| {
            let key = classify_symbol(&sym.name);
            (key, sym.file_order, sym.addr, sym.name)
        })
        .collect();

    keyed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });

    let mut seen = HashSet::with_capacity(keyed.len());
    let mut ordered = Vec::with_capacity(keyed.len());
    for (_, _, _, name) in keyed {
        if seen.insert(name.clone()) {
            ordered.push(name);
        }
    }
    ordered
}

fn extract_container_symbols(bytes: &[u8], file_order: u32, out: &mut Vec<RawSym>) {
    if bytes.starts_with(b"!<arch>\n") {
        extract_archive_symbols(bytes, file_order, out);
        return;
    }
    extract_macho_or_fat_symbols(bytes, file_order, out);
}

fn extract_archive_symbols(bytes: &[u8], file_order: u32, out: &mut Vec<RawSym>) {
    let mut pos = 8usize;
    while let Some(hdr) = pos.checked_add(60).and_then(|end| bytes.get(pos..end)) {
        if hdr.get(58..60) != Some(b"`\n") {
            break;
        }
        let Ok(size_str) = std::str::from_utf8(&hdr[48..58]) else {
            break;
        };
        let Ok(member_size) = size_str.trim().parse::<usize>() else {
            break;
        };
        let body_start = pos + 60;
        let Some(body_end) = body_start.checked_add(member_size) else {
            break;
        };
        let Some(mut body) = bytes.get(body_start..body_end) else {
            break;
        };
        if let Ok(name_field) = std::str::from_utf8(&hdr[0..16])
            && let Some(ext_len_str) = name_field.trim_end_matches(' ').strip_prefix("#1/")
            && let Ok(ext_len) = ext_len_str.parse::<usize>()
            && let Some(trimmed) = body.get(ext_len..)
        {
            body = trimmed;
        }
        extract_macho_or_fat_symbols(body, file_order, out);
        let Some(next_pos) = body_end.checked_add(body_end & 1) else {
            break;
        };
        pos = next_pos;
    }
}

fn extract_macho_or_fat_symbols(bytes: &[u8], file_order: u32, out: &mut Vec<RawSym>) {
    let Some(magic_be) = read_u32_be(bytes, 0) else {
        return;
    };
    if magic_be == FAT_MAGIC {
        let Some(nfat) = read_u32_be(bytes, 4) else {
            return;
        };
        for i in 0..usize::try_from(nfat).unwrap_or(0).min(16) {
            let base = 8 + i * 20;
            let (Some(off), Some(sz)) =
                (read_u32_be(bytes, base + 8), read_u32_be(bytes, base + 12))
            else {
                break;
            };
            let (off, sz) = (off as usize, sz as usize);
            if let Some(slice) = off.checked_add(sz).and_then(|end| bytes.get(off..end)) {
                extract_thin_macho_symbols(slice, file_order, out);
            }
        }
        return;
    }
    if magic_be == FAT_MAGIC_64 {
        let Some(nfat) = read_u32_be(bytes, 4) else {
            return;
        };
        for i in 0..usize::try_from(nfat).unwrap_or(0).min(16) {
            let base = 8 + i * 32;
            let (Some(off), Some(sz)) =
                (read_u64_be(bytes, base + 8), read_u64_be(bytes, base + 16))
            else {
                break;
            };
            let (Ok(off), Ok(sz)) = (usize::try_from(off), usize::try_from(sz)) else {
                continue;
            };
            if let Some(slice) = off.checked_add(sz).and_then(|end| bytes.get(off..end)) {
                extract_thin_macho_symbols(slice, file_order, out);
            }
        }
        return;
    }
    extract_thin_macho_symbols(bytes, file_order, out);
}

fn extract_thin_macho_symbols(bytes: &[u8], file_order: u32, out: &mut Vec<RawSym>) {
    if read_u32_le(bytes, 0) != Some(MH_MAGIC_64) {
        return;
    }
    if read_u32_le(bytes, 12) != Some(MH_OBJECT) {
        return;
    }
    let Some(ncmds) = read_u32_le(bytes, 16) else {
        return;
    };

    let mut orderable_sect = [false; 256];
    let mut next_sect_idx: usize = 1;
    let mut symtab: Option<(usize, usize, usize, usize)> = None;

    let mut cmd_off = 32usize;
    for _ in 0..ncmds {
        let (Some(cmd), Some(cmdsize)) =
            (read_u32_le(bytes, cmd_off), read_u32_le(bytes, cmd_off + 4))
        else {
            return;
        };
        let cmdsize = cmdsize as usize;
        if cmdsize < 8 {
            return;
        }
        let Some(cmd_end) = cmd_off.checked_add(cmdsize) else {
            return;
        };
        if cmd_end > bytes.len() {
            return;
        }

        if cmd == LC_SEGMENT_64
            && cmdsize >= 72
            && let Some(nsects) = read_u32_le(bytes, cmd_off + 64)
        {
            for s in 0..nsects as usize {
                let Some(sect_off) = s
                    .checked_mul(80)
                    .and_then(|off| (cmd_off + 72).checked_add(off))
                else {
                    break;
                };
                let Some(sect_end) = sect_off.checked_add(80).filter(|&e| e <= cmd_end) else {
                    break;
                };
                let Some(sect_hdr) = bytes.get(sect_off..sect_end) else {
                    break;
                };
                if next_sect_idx < orderable_sect.len() {
                    let sectname = nul_padded_ascii(&sect_hdr[0..16]);
                    let segname = nul_padded_ascii(&sect_hdr[16..32]);
                    orderable_sect[next_sect_idx] = is_orderable_section(segname, sectname);
                }
                next_sect_idx = next_sect_idx.saturating_add(1);
            }
        } else if cmd == LC_SYMTAB
            && cmdsize >= 24
            && let (Some(symoff), Some(nsyms), Some(stroff), Some(strsize)) = (
                read_u32_le(bytes, cmd_off + 8),
                read_u32_le(bytes, cmd_off + 12),
                read_u32_le(bytes, cmd_off + 16),
                read_u32_le(bytes, cmd_off + 20),
            )
        {
            symtab = Some((
                symoff as usize,
                nsyms as usize,
                stroff as usize,
                strsize as usize,
            ));
        }

        cmd_off = cmd_end;
    }

    let Some((symoff, nsyms, stroff, strsize)) = symtab else {
        return;
    };
    let Some(strtab) = stroff
        .checked_add(strsize)
        .and_then(|end| bytes.get(stroff..end))
    else {
        return;
    };
    let Some(sym_bytes_len) = nsyms.checked_mul(16) else {
        return;
    };
    let Some(sym_table) = symoff
        .checked_add(sym_bytes_len)
        .and_then(|end| bytes.get(symoff..end))
    else {
        return;
    };

    for entry in sym_table.as_chunks::<16>().0 {
        let n_type = entry[4];
        if (n_type & N_STAB) != 0 || (n_type & N_TYPE) != N_SECT {
            continue;
        }
        let n_sect = entry[5] as usize;
        if !orderable_sect.get(n_sect).copied().unwrap_or(false) {
            continue;
        }
        let Some(strx) = read_u32_le(entry, 0).map(|v| v as usize) else {
            continue;
        };
        let Some(addr) = read_u64_le(entry, 8) else {
            continue;
        };
        let Some(tail) = strtab.get(strx..) else {
            continue;
        };
        let nul = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        let Ok(name) = std::str::from_utf8(&tail[..nul]) else {
            continue;
        };
        if should_order_symbol_name(name) {
            out.push(RawSym {
                name: name.to_string(),
                file_order,
                addr,
            });
        }
    }
}

fn is_orderable_section(segname: &str, sectname: &str) -> bool {
    matches!(
        (segname, sectname),
        ("__TEXT", "__text" | "__const") | ("__DATA" | "__DATA_CONST", "__const")
    )
}

fn should_order_symbol_name(name: &str) -> bool {
    name.starts_with('_') && !name.starts_with("_ltmp") && name != "__mh_execute_header"
}

fn nul_padded_ascii(field: &[u8]) -> &str {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).unwrap_or("")
}

fn read_u32_le(bytes: &[u8], off: usize) -> Option<u32> {
    let b: [u8; 4] = bytes.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_u64_le(bytes: &[u8], off: usize) -> Option<u64> {
    let b: [u8; 8] = bytes.get(off..off.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(b))
}

fn read_u32_be(bytes: &[u8], off: usize) -> Option<u32> {
    let b: [u8; 4] = bytes.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(b))
}

fn read_u64_be(bytes: &[u8], off: usize) -> Option<u64> {
    let b: [u8; 8] = bytes.get(off..off.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_be_bytes(b))
}

/// Classifies a Mach-O symbol into `(tier, crate_name, module_name, sub_tier)` using
/// structural Rust v0 / Itanium mangling grammar with no crate or module registry.
#[must_use]
pub(crate) fn classify_symbol(sym: &str) -> LocalityKey {
    if sym == "_main" || sym == "main" {
        return LocalityKey {
            tier: SymbolTier::Runtime,
            crate_name: String::new(),
            module_name: String::new(),
            sub_tier: 0,
        };
    }

    let is_cold = is_cold_or_panic_symbol(sym);
    let sub_tier = u8::from(!is_cold && is_drop_symbol(sym));

    if let Some((crate_name, module_name, is_domain)) = extract_v0_locality(sym) {
        let tier = if is_cold {
            SymbolTier::Cold
        } else if is_domain {
            SymbolTier::Live
        } else {
            SymbolTier::Runtime
        };
        return LocalityKey {
            tier,
            crate_name,
            module_name,
            sub_tier,
        };
    }

    if let Some((crate_name, module_name, is_domain)) = extract_itanium_locality(sym) {
        let tier = if is_cold {
            SymbolTier::Cold
        } else if is_domain {
            SymbolTier::Live
        } else {
            SymbolTier::Runtime
        };
        return LocalityKey {
            tier,
            crate_name,
            module_name,
            sub_tier,
        };
    }

    let c_group = extract_c_prefix(sym);
    LocalityKey {
        tier: if is_cold {
            SymbolTier::Cold
        } else {
            SymbolTier::Runtime
        },
        crate_name: c_group.to_string(),
        module_name: String::new(),
        sub_tier,
    }
}

fn is_cold_or_panic_symbol(sym: &str) -> bool {
    if sym.ends_with(".cold") || sym.contains(".cold.") {
        return true;
    }
    if sym.contains("9panicking")
        || sym.contains("5panic")
        || sym.contains("panic_fmt")
        || sym.contains("panic_bounds_check")
        || sym.contains("slice_index_fail")
        || sym.contains("unwrap_failed")
        || sym.contains("expect_failed")
        || sym.contains("14rustc_demangle")
        || sym.contains("9addr2line")
        || sym.contains("5gimli")
    {
        return true;
    }
    sym.contains("9backtrace") && !sym.contains("___rust_begin_short_backtrace")
}

fn is_drop_symbol(sym: &str) -> bool {
    sym.contains("9drop_glue") || sym.contains("3ops4drop4Drop4drop")
}

fn is_stdlib_crate(crate_name: &str) -> bool {
    matches!(
        crate_name,
        "core"
            | "alloc"
            | "std"
            | "std_detect"
            | "panic_unwind"
            | "panic_abort"
            | "unwind"
            | "compiler_builtins"
            | "hashbrown"
            | "rustc_demangle"
            | "addr2line"
            | "gimli"
            | "object"
            | "memchr"
            | "cfg_if"
            | "libc"
    )
}

fn extract_v0_locality(sym: &str) -> Option<(String, String, bool)> {
    let v0_body = sym.strip_prefix("__R").or_else(|| sym.strip_prefix("_R"))?;
    let bytes = v0_body.as_bytes();
    let mut fallback_std: Option<(String, String)> = None;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'C'
            && let Some((crate_name, after_crate)) = parse_v0_crate_at(v0_body, i)
        {
            let preceded_by_nt = i >= 2 && &bytes[i - 2..i] == b"Nt";
            let module_name = if preceded_by_nt {
                parse_v0_ident(after_crate)
                    .filter(|(ident, _)| is_module_like_ident(ident))
                    .map(|(ident, _)| ident.to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            if !is_stdlib_crate(crate_name) {
                return Some((crate_name.to_string(), module_name, true));
            }
            if fallback_std.is_none() {
                fallback_std = Some((crate_name.to_string(), module_name));
            }
        }
        i += 1;
    }

    fallback_std.map(|(c, m)| (c, m, false))
}

fn parse_v0_crate_at(s: &str, c_pos: usize) -> Option<(&str, &str)> {
    let after_c = s.get(c_pos + 1..)?;
    let ident_input = if let Some(after_s) = after_c.strip_prefix('s') {
        let uscore = after_s.bytes().position(|b| b == b'_')?;
        if !after_s.as_bytes()[..uscore]
            .iter()
            .all(u8::is_ascii_alphanumeric)
        {
            return None;
        }
        after_s.get(uscore + 1..)?
    } else if c_pos == 0 || (c_pos >= 2 && matches!(&s.as_bytes()[c_pos - 2..c_pos], b"Nv" | b"Nt"))
    {
        after_c
    } else {
        return None;
    };

    let (crate_ident, rest) = parse_v0_ident(ident_input)?;
    if crate_ident.is_empty()
        || !crate_ident
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    Some((crate_ident, rest))
}

fn parse_v0_ident(mut s: &str) -> Option<(&str, &str)> {
    if let Some(after_s) = s.strip_prefix('s') {
        let uscore = after_s.bytes().position(|b| b == b'_')?;
        if !after_s.as_bytes()[..uscore]
            .iter()
            .all(u8::is_ascii_alphanumeric)
        {
            return None;
        }
        s = after_s.get(uscore + 1..)?;
    }
    let s = s.strip_prefix('u').unwrap_or(s);
    let first = *s.as_bytes().first()?;
    if !first.is_ascii_digit() || first == b'0' {
        return None;
    }
    let digits_end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    let len: usize = s.get(..digits_end)?.parse().ok()?;
    let after_digits = s.get(digits_end..)?;
    let body = after_digits.strip_prefix('_').unwrap_or(after_digits);
    let ident = body.get(..len)?;
    let rest = body.get(len..)?;
    Some((ident, rest))
}

fn is_module_like_ident(ident: &str) -> bool {
    ident
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
}

fn extract_itanium_locality(sym: &str) -> Option<(String, String, bool)> {
    let mut rest = sym
        .strip_prefix("__ZN")
        .or_else(|| sym.strip_prefix("_ZN"))?;
    let (seg0, after0) = parse_itanium_seg(rest)?;
    rest = after0;
    let mod1 = parse_itanium_seg(rest)
        .filter(|(m, _)| is_module_like_ident(m))
        .map(|(m, _)| m.to_string())
        .unwrap_or_default();
    let is_domain = !is_stdlib_crate(seg0);
    Some((seg0.to_string(), mod1, is_domain))
}

fn parse_itanium_seg(s: &str) -> Option<(&str, &str)> {
    let first = *s.as_bytes().first()?;
    if !first.is_ascii_digit() || first == b'0' {
        return None;
    }
    let digits_end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    let len: usize = s.get(..digits_end)?.parse().ok()?;
    let after = s.get(digits_end..)?;
    Some((after.get(..len)?, after.get(len..)?))
}

fn extract_c_prefix(sym: &str) -> &str {
    let trimmed = sym.trim_start_matches('_');
    match trimmed.split_once('_') {
        Some((prefix, _)) if !prefix.is_empty() => prefix,
        _ => "c",
    }
}

/// Writes `symbols` to `path` (one symbol per line) for `ld -order_file`.
pub(crate) fn write_order_file(path: &Path, symbols: &[String]) -> std::io::Result<()> {
    let total_bytes: usize = symbols.iter().map(|s| s.len() + 1).sum();
    let mut buf = String::with_capacity(total_bytes);
    for sym in symbols {
        buf.push_str(sym);
        buf.push('\n');
    }
    std::fs::write(path, buf.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_macho64(
        filetype: u32,
        text_vmaddr: u64,
        text_size: u64,
        symbols: &[(&str, u8, u64)],
    ) -> Vec<u8> {
        let ncmds: u32 = 2;
        let seg_cmd_size: u32 = 72 + 80 * 2;
        let sym_cmd_size: u32 = 24;
        let sizeofcmds = seg_cmd_size + sym_cmd_size;
        let mut out = Vec::new();

        out.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        out.extend_from_slice(&0x0100_000c_u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&filetype.to_le_bytes());
        out.extend_from_slice(&ncmds.to_le_bytes());
        out.extend_from_slice(&sizeofcmds.to_le_bytes());
        out.extend_from_slice(&0x2000u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());

        out.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
        out.extend_from_slice(&seg_cmd_size.to_le_bytes());
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&text_vmaddr.to_le_bytes());
        out.extend_from_slice(&text_size.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&text_size.to_le_bytes());
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());

        let push_sect = |buf: &mut Vec<u8>, seg: &[u8], sect: &[u8], addr: u64, sz: u64| {
            let mut sect_field = [0u8; 16];
            sect_field[..sect.len()].copy_from_slice(sect);
            let mut seg_field = [0u8; 16];
            seg_field[..seg.len()].copy_from_slice(seg);
            buf.extend_from_slice(&sect_field);
            buf.extend_from_slice(&seg_field);
            buf.extend_from_slice(&addr.to_le_bytes());
            buf.extend_from_slice(&sz.to_le_bytes());
            buf.extend_from_slice(&[0u8; 32]);
        };
        push_sect(&mut out, b"__TEXT", b"__text", text_vmaddr, text_size);
        push_sect(&mut out, b"__DATA", b"__data", text_vmaddr + text_size, 64);

        let actual_symoff = 32 + sizeofcmds;
        let nsyms = u32::try_from(symbols.len()).expect("test symbol count fits u32");
        let stroff = actual_symoff + nsyms * 16;

        let mut strtab = vec![0u8];
        let mut sym_records = Vec::new();
        for &(name, sect, addr) in symbols {
            let strx = u32::try_from(strtab.len()).expect("strx fits u32");
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
            sym_records.extend_from_slice(&strx.to_le_bytes());
            sym_records.push(N_SECT);
            sym_records.push(sect);
            sym_records.extend_from_slice(&0u16.to_le_bytes());
            sym_records.extend_from_slice(&addr.to_le_bytes());
        }
        let strsize = u32::try_from(strtab.len()).expect("strsize fits u32");

        out.extend_from_slice(&LC_SYMTAB.to_le_bytes());
        out.extend_from_slice(&sym_cmd_size.to_le_bytes());
        out.extend_from_slice(&actual_symoff.to_le_bytes());
        out.extend_from_slice(&nsyms.to_le_bytes());
        out.extend_from_slice(&stroff.to_le_bytes());
        out.extend_from_slice(&strsize.to_le_bytes());

        assert_eq!(out.len(), actual_symoff as usize);
        out.extend_from_slice(&sym_records);
        assert_eq!(out.len(), stroff as usize);
        out.extend_from_slice(&strtab);
        out
    }

    #[test]
    fn v0_classifier_groups_generics_and_drops_with_instantiating_module_and_isolates_cold() {
        let live_mod =
            classify_symbol("__RNvMs4_NtCseA0bJGRgonR_9aterm_gui13rewind_workerNtB5_5Cache4keep");
        assert_eq!(
            live_mod,
            LocalityKey {
                tier: SymbolTier::Live,
                crate_name: "aterm_gui".into(),
                module_name: "rewind_worker".into(),
                sub_tier: 0,
            }
        );

        let root_method = classify_symbol(
            "__RNvMsL_CseA0bJGRgonR_9aterm_guiNtB5_3App26active_tab_contains_native",
        );
        assert_eq!(
            root_method,
            LocalityKey {
                tier: SymbolTier::Live,
                crate_name: "aterm_gui".into(),
                module_name: String::new(),
                sub_tier: 0,
            }
        );

        let generic_vec = classify_symbol(
            "__RNvMNtCsdzX4Z8B0DGU_5alloc3vecINtB2_3VecAhj20_E13with_capacityCs6mxZS6V2z2a_8aterm_db",
        );
        assert_eq!(
            generic_vec,
            LocalityKey {
                tier: SymbolTier::Live,
                crate_name: "aterm_db".into(),
                module_name: String::new(),
                sub_tier: 0,
            }
        );

        let thread_entry = classify_symbol(
            "__RINvNtNtCs6sTpqAl7fVb_3std3sys9backtrace28___rust_begin_short_backtraceNCINvMs0_NtCseA0bJGRgonR_9aterm_gui13rewind_workerNtB1g_12RewindWorker5start0E0uEB1i_",
        );
        assert_eq!(
            thread_entry,
            LocalityKey {
                tier: SymbolTier::Live,
                crate_name: "aterm_gui".into(),
                module_name: "rewind_worker".into(),
                sub_tier: 0,
            }
        );

        let drop_sym = classify_symbol(
            "__RINvNtCsedNDddgVRmw_4core3ptr9drop_glueINtNtB4_6option6OptionNtNtCseA0bJGRgonR_9aterm_gui10app_config11FleetConfigEECs7IiBsUA9gVZ_5aterm",
        );
        assert_eq!(
            drop_sym,
            LocalityKey {
                tier: SymbolTier::Live,
                crate_name: "aterm_gui".into(),
                module_name: "app_config".into(),
                sub_tier: 1,
            }
        );

        let cold_sym = classify_symbol("__RNvNtCseA0bJGRgonR_9aterm_gui8temporal10unnumbered.cold");
        assert_eq!(cold_sym.tier, SymbolTier::Cold);
    }

    #[test]
    fn macho_object_and_bsd_archive_preserve_in_module_address_order_and_filter_non_text() {
        let obj = build_minimal_macho64(
            MH_OBJECT,
            0,
            0x100,
            &[
                ("ltmp0", 1, 0x0),
                ("__RNvNtCs1_3agh3cli3run", 1, 0x10),
                (
                    "__RINvNtCs2_4core3ptr9drop_glueNtNtCs3_9aterm_gui5spawn4PaneE",
                    1,
                    0x20,
                ),
                ("__RNvNtCs3_9aterm_gui5spawn10second_fn_", 1, 0x40),
                ("__RNvNtCs3_9aterm_gui5spawn9first_fn_", 1, 0x30),
                ("__RNvNtCs3_9aterm_gui5spawn9first_fn_.cold", 1, 0x50),
                ("_writable_global", 2, 0x0),
                ("_main", 1, 0x60),
            ],
        );

        let ext_name = b"aterm_test_long_member.rcgu.o\0\0\0";
        let member_size = ext_name.len() + obj.len();
        let mut archive = Vec::from(&b"!<arch>\n"[..]);
        let hdr_name = format!("#1/{:<13}", ext_name.len());
        let hdr_rest = format!(
            "{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0, 0, 0, "100644", member_size
        );
        archive.extend_from_slice(hdr_name.as_bytes());
        archive.extend_from_slice(hdr_rest.as_bytes());
        archive.extend_from_slice(ext_name);
        archive.extend_from_slice(&obj);

        let ordered = build_order_list_from_slices(&[&archive]);
        assert_eq!(
            ordered,
            vec![
                "_main",
                "__RNvNtCs1_3agh3cli3run",
                "__RNvNtCs3_9aterm_gui5spawn9first_fn_",
                "__RNvNtCs3_9aterm_gui5spawn10second_fn_",
                "__RINvNtCs2_4core3ptr9drop_glueNtNtCs3_9aterm_gui5spawn4PaneE",
                "__RNvNtCs3_9aterm_gui5spawn9first_fn_.cold",
            ]
        );
    }

    #[test]
    fn executable_fault_intersection_promotes_hot_symbols_and_rejects_random_neighbors() {
        let exe = build_minimal_macho64(
            MH_EXECUTE,
            0x1_0000_0000,
            0x1_0000,
            &[
                ("__mh_execute_header", 1, 0x1_0000_0000),
                ("_cold_page0", 1, 0x1_0000_0100),
                ("_hot_init", 1, 0x1_0000_4000),
                ("_cold_neighbor", 1, 0x1_0000_4200),
                ("_cold_page2", 1, 0x1_0000_8000),
            ],
        );
        let mut faulted = HashSet::new();
        faulted.insert(0x1_0000_4000);
        let hits0 = extract_executable_faulted_symbols(&exe, &faulted);
        assert!(hits0.contains("_hot_init"));
        assert!(hits0.contains("_cold_neighbor"));
        assert!(!hits0.contains("_cold_page0"));
        assert!(!hits0.contains("__mh_execute_header"));

        let base = vec![
            "_main".to_string(),
            "_cold_page0".to_string(),
            "_cold_neighbor".to_string(),
            "_hot_init".to_string(),
            "_cold_page2".to_string(),
        ];
        let spacers = vec!["__atpad_t_00000".to_string(), "__atpad_c_00000".to_string()];
        let perm = permute_order_list_with_spacers(&base, &spacers, 0x9e37_79b9_7f4a_7c15);
        assert_eq!(perm.len(), base.len() + spacers.len());

        // Across 4 probes, `_hot_init` faults every time while `_cold_neighbor` only shared
        // a page in probe 0.
        let hits_only_hot: HashSet<String> = ["_hot_init".to_string()].into_iter().collect();
        let refined = refine_order_with_probe_hits(
            &base,
            &[
                hits0,
                hits_only_hot.clone(),
                hits_only_hot.clone(),
                hits_only_hot,
            ],
        );
        assert_eq!(
            refined,
            vec![
                "_main",
                "_hot_init",
                "_cold_page0",
                "_cold_neighbor",
                "_cold_page2",
            ]
        );

        let top_exec = build_minimal_macho64(
            MH_EXECUTE,
            u64::MAX - 64,
            64,
            &[("_top_page", 1, u64::MAX - 2)],
        );
        let top_pages: HashSet<u64> = [0x1000_0000].into_iter().collect();
        assert!(extract_executable_faulted_symbols(&top_exec, &top_pages).is_empty());
        let hit_top_pages: HashSet<u64> = [ARM64_PAGE_MASK].into_iter().collect();
        assert!(
            extract_executable_faulted_symbols(&top_exec, &hit_top_pages).contains("_top_page")
        );
    }

    #[test]
    fn linker_arg_collection_and_order_file_roundtrip() {
        let scratch = std::env::temp_dir().join(format!(
            "aterm-macho-order-test-{}.noindex",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("create scratch dir");

        let obj_a = scratch.join("alpha.o");
        let obj_b = scratch.join("beta.o");
        std::fs::write(
            &obj_a,
            build_minimal_macho64(MH_OBJECT, 0, 64, &[("__RNvNtCs1_3agh3cli3run", 1, 0x0)]),
        )
        .expect("write alpha.o");
        std::fs::write(
            &obj_b,
            build_minimal_macho64(MH_OBJECT, 0, 64, &[("_main", 1, 0x0)]),
        )
        .expect("write beta.o");

        let filelist = scratch.join("inputs.filelist");
        std::fs::write(&filelist, format!("{}\n", obj_b.display())).expect("write filelist");
        let resp = scratch.join("link.args");
        std::fs::write(
            &resp,
            format!(
                "-o \"{}\" \"{}\" -filelist \"{}\"\n",
                scratch.join("out.o").display(),
                obj_a.display(),
                filelist.display()
            ),
        )
        .expect("write response file");

        let args = vec![format!("@{}", resp.display()), "-x".into()];
        let inputs = collect_input_paths(&args);
        assert_eq!(inputs, vec![obj_a, obj_b]);

        let ordered = build_order_list(&inputs);
        assert_eq!(ordered, vec!["_main", "__RNvNtCs1_3agh3cli3run"]);

        let order_out = scratch.join("aterm.order");
        write_order_file(&order_out, &ordered).expect("write order file");
        let written = std::fs::read_to_string(&order_out).expect("read order file");
        assert_eq!(written, "_main\n__RNvNtCs1_3agh3cli3run\n");

        let pad_obj = scratch.join("spacer.o");
        let pad_names = write_probe_spacer_object(&pad_obj, 8, 4, 4096).expect("write spacer.o");
        assert_eq!(pad_names.len(), 12);
        let parsed_pad = build_order_list(std::slice::from_ref(&pad_obj));
        assert_eq!(parsed_pad.len(), 12);

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let sdk = aterm_uds::spawnfd::command(Path::new("/usr/bin/xcrun"))
                .args(["--show-sdk-path"])
                .output()
                .expect("xcrun --show-sdk-path");
            let sdk_path = String::from_utf8_lossy(&sdk.stdout).trim().to_string();
            let dylib_out = scratch.join("probe_test.dylib");
            let order_list = vec![
                "__atpad_t_00003".to_string(),
                "__atpad_t_00000".to_string(),
                "__atpad_c_00001".to_string(),
            ];
            let pad_order = scratch.join("pad.order");
            write_order_file(&pad_order, &order_list).expect("write pad.order");
            let out = aterm_uds::spawnfd::command(Path::new("/usr/bin/xcrun"))
                .args([
                    "ld",
                    "-dylib",
                    "-arch",
                    "arm64",
                    "-platform_version",
                    "macos",
                    "14.0.0",
                    "14.0.0",
                    "-syslibroot",
                    &sdk_path,
                    "-lSystem",
                    "-dead_strip",
                    "-o",
                ])
                .arg(&dylib_out)
                .arg(&pad_obj)
                .arg("-order_file")
                .arg(&pad_order)
                .output()
                .expect("invoke xcrun ld");
            assert!(
                out.status.success(),
                "ld failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let nm_out = aterm_uds::spawnfd::command(Path::new("/usr/bin/nm"))
                .arg("-n")
                .arg(&dylib_out)
                .output()
                .expect("nm dylib");
            let nm_text = String::from_utf8_lossy(&nm_out.stdout);
            let pos_3 = nm_text
                .find("__atpad_t_00003")
                .expect("spacer 3 kept under -dead_strip");
            let pos_0 = nm_text
                .find("__atpad_t_00000")
                .expect("spacer 0 kept under -dead_strip");
            assert!(
                pos_3 < pos_0,
                "-order_file must place __atpad_t_00003 before __atpad_t_00000:\n{nm_text}"
            );
        }

        let _ = std::fs::remove_dir_all(&scratch);
    }
}
