// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared Mach-O template accounting for the GUI entry and the composed app image.

/// The unchanged upper bound for one image's per-thread TLS template.
pub(super) const TEMPLATE_BUDGET_BYTES: u64 = 8 << 10;

/// How many thread-locals an over-budget report names.
pub(super) const NAMED: usize = 10;

pub(super) const MH_MAGIC_64: u32 = 0xfeed_facf;
pub(super) const LC_SYMTAB: u32 = 0x2;
pub(super) const LC_SEGMENT_64: u32 = 0x19;
pub(super) const SECTION_TYPE: u32 = 0xff;
pub(super) const S_THREAD_LOCAL_REGULAR: u32 = 0x11;
pub(super) const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
/// `struct mach_header_64`.
pub(super) const HEADER_LEN: usize = 32;
/// `struct segment_command_64` up to its sections.
pub(super) const SEGMENT_LEN: usize = 72;
/// `struct section_64`.
pub(super) const SECTION_LEN: usize = 80;
/// `struct nlist_64`.
pub(super) const NLIST_LEN: usize = 16;
/// `n_type`'s debugger-entry bits: a symbol with any set is a stab, not a definition.
pub(super) const N_STAB: u8 = 0xe0;
/// `n_type`'s type bits, and their value for a symbol defined in the section `n_sect` numbers.
pub(super) const N_TYPE: u8 = 0x0e;
pub(super) const N_SECT: u8 = 0x0e;

pub(super) fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let word = bytes
        .get(at..at + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .unwrap_or_else(|| panic!("the image ends inside a load command at {at}"));
    u32::from_le_bytes(word)
}

pub(super) fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let word = bytes
        .get(at..at + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .unwrap_or_else(|| panic!("the image ends inside a section header at {at}"));
    u64::from_le_bytes(word)
}

/// A 16-byte, NUL-padded Mach-O name.
pub(super) fn name_at(bytes: &[u8], at: usize) -> &[u8] {
    let raw = bytes.get(at..at + 16).unwrap_or_default();
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    raw.get(..end).unwrap_or_default()
}

/// One section of thread-local initial values, regular or zero-filled.
pub(super) struct Section {
    /// Its place among all of the image's sections, from 1: what a symbol's `n_sect` holds.
    pub(super) number: usize,
    pub(super) name: String,
    pub(super) addr: u64,
    pub(super) size: u64,
}

impl Section {
    pub(super) fn end(&self) -> u64 {
        self.addr
            .checked_add(self.size)
            .expect("a thread-local section's address range fits in u64")
    }
}

/// What a thin 64-bit Mach-O image says of its thread-locals.
pub(super) struct Template {
    /// The bytes in regular thread-local sections, normally `__DATA,__thread_data`.
    pub(super) data: u64,
    /// The bytes in zero-filled thread-local sections, normally `__DATA,__thread_bss`.
    pub(super) bss: u64,
    pub(super) sections: Vec<Section>,
    /// `LC_SYMTAB`'s symbol table offset, symbol count, string table offset and string bytes.
    pub(super) symbols: Option<(usize, usize, usize, usize)>,
}

impl Template {
    /// dyld copies the address span from the first TLS section through the last, including
    /// gaps between them (`libdyld/ThreadLocalVariables.cpp`, `findInitialContent`).
    pub(super) fn bytes(&self) -> u64 {
        match (self.sections.first(), self.sections.last()) {
            (Some(first), Some(last)) => last.end() - first.addr,
            _ => 0,
        }
    }
}

pub(super) fn thread_local_template(image: &[u8]) -> Template {
    assert_eq!(u32_at(image, 0), MH_MAGIC_64, "a thin 64-bit Mach-O image");
    let ncmds = u32_at(image, 16) as usize;
    let mut template = Template {
        data: 0,
        bss: 0,
        sections: Vec::new(),
        symbols: None,
    };
    let mut number = 0;
    let mut at = HEADER_LEN;
    for _ in 0..ncmds {
        let (cmd, cmdsize) = (u32_at(image, at), u32_at(image, at + 4) as usize);
        assert!(cmdsize >= 8, "a load command at {at} names its size");
        if cmd == LC_SEGMENT_64 {
            let nsects = u32_at(image, at + 64) as usize;
            for i in 0..nsects {
                number += 1;
                let section = at + SEGMENT_LEN + i * SECTION_LEN;
                let size = u64_at(image, section + 40);
                match u32_at(image, section + 64) & SECTION_TYPE {
                    S_THREAD_LOCAL_REGULAR => template.data += size,
                    S_THREAD_LOCAL_ZEROFILL => template.bss += size,
                    _ => continue,
                }
                template.sections.push(Section {
                    number,
                    name: format!(
                        "{},{}",
                        String::from_utf8_lossy(name_at(image, section + 16)),
                        String::from_utf8_lossy(name_at(image, section)),
                    ),
                    addr: u64_at(image, section + 32),
                    size,
                });
            }
        } else if cmd == LC_SYMTAB {
            template.symbols = Some((
                u32_at(image, at + 8) as usize,
                u32_at(image, at + 12) as usize,
                u32_at(image, at + 16) as usize,
                u32_at(image, at + 20) as usize,
            ));
        }
        at += cmdsize;
    }
    // dyld replaces an empty initial span when it reaches the first nonempty TLS section.
    let first = template
        .sections
        .iter()
        .position(|section| section.size > 0)
        .unwrap_or(template.sections.len());
    drop(template.sections.drain(..first));
    for pair in template.sections.windows(2) {
        assert!(
            pair[0].end() <= pair[1].addr,
            "thread-local sections are ordered without overlap: {} then {}",
            pair[0].name,
            pair[1].name,
        );
    }
    template
}

/// `mangled` past the v0 `s<base-62>_` disambiguator at its head, where it has one.
pub(super) fn undisambiguated(mangled: &str) -> Option<&str> {
    match mangled.strip_prefix('s') {
        Some(numbered) => Some(numbered.split_once('_')?.1),
        None => Some(mangled),
    }
}

/// A v0-mangled `identifier` at the head of `mangled` and what follows it: a disambiguator or
/// none, a decimal length, `_` where the name starts with one or with a digit, and the name. A
/// closure's or a constant's name is empty.
pub(super) fn identifier(mangled: &str) -> Option<(&str, &str)> {
    let counted = undisambiguated(mangled)?;
    // A length is `0` or starts with another digit: the digit after a `0` is the next length's.
    let digits = match counted.find(|c: char| !c.is_ascii_digit()) {
        Some(0) => return None,
        _ if counted.starts_with('0') => 1,
        digits => digits.unwrap_or(counted.len()),
    };
    let len = counted.get(..digits)?.parse::<usize>().ok()?;
    let name = counted.get(digits..)?;
    let name = name.strip_prefix('_').unwrap_or(name);
    Some((name.get(..len)?, name.get(len..)?))
}

/// A v0-mangled `path` at the head of `mangled`, as the names it is made of, and what follows it:
/// a crate, a path nested in one, or an inherent `impl` of a type a path names. Any other path (a
/// trait's `impl`, a generic's instance) is `None`.
pub(super) fn nested_path(mangled: &str) -> Option<(Vec<&str>, &str)> {
    let (mut names, rest) = match mangled.as_bytes().first()? {
        b'C' => (Vec::new(), mangled.get(1..)?),
        // `N`, the namespace's letter, the path it is in, then its own name.
        b'N' => nested_path(mangled.get(2..)?)?,
        // `M`, a disambiguator or none, the path the `impl` is written in, then its type: read
        // as that path and the type's own name.
        b'M' => {
            let (mut names, of) = nested_path(undisambiguated(mangled.get(1..)?)?)?;
            let (of, rest) = nested_path(of)?;
            names.push(*of.last()?);
            return Some((names, rest));
        }
        // `B<base-62>_` stands for a path written earlier in the symbol, which is not repeated.
        b'B' => return Some((Vec::new(), mangled.get(1..)?.split_once('_')?.1)),
        _ => return None,
    };
    let (name, rest) = identifier(rest)?;
    if !name.is_empty() {
        names.push(name);
    }
    Some((names, rest))
}

/// The path a thread-local's symbol names, as `thread_local!` declares it: for
/// `__RNvNCNKNvNtCs…_9aterm_gui9tab_marks6ACTING0023___RUST_STD_INTERNAL_VAL$tlv$init`,
/// `aterm_gui::tab_marks::ACTING`. A symbol [`nested_path`] does not read is given as it is
/// written.
pub(super) fn path_of(symbol: &str) -> String {
    let symbol = symbol.strip_suffix("$tlv$init").unwrap_or(symbol);
    let Some((names, "")) = symbol.strip_prefix("__R").and_then(nested_path) else {
        return symbol.to_owned();
    };
    // `thread_local!` keeps the value in a static of its own, inside the one declared.
    names
        .strip_suffix(&["__RUST_STD_INTERNAL_VAL"])
        .unwrap_or(&names)
        .join("::")
}

pub(super) struct TemplateSizes {
    /// Each symbol's extent through the next symbol or section end, largest first.
    pub(super) named: Vec<(u64, String)>,
    /// Address spans that cannot be assigned to a symbol, including possible alignment padding.
    pub(super) unnamed: Vec<(u64, String)>,
}

/// Partition the entire template, retaining unnamed bytes rather than assigning them to an
/// unrelated symbol. Mach-O has symbol addresses, not variable sizes: named extents therefore
/// include internal and trailing padding, while an unsymbolized prefix remains explicit.
pub(super) fn thread_locals_by_size(image: &[u8], template: &Template) -> TemplateSizes {
    let mut sizes = TemplateSizes {
        named: Vec::new(),
        unnamed: Vec::new(),
    };
    let symbols = template.symbols.map(|(symoff, nsyms, stroff, strsize)| {
        let strings = image
            .get(stroff..stroff + strsize)
            .expect("the image contains its string table");
        (symoff, nsyms, strings)
    });
    for section in &template.sections {
        let (symoff, nsyms, strings) = symbols.unwrap_or((0, 0, &[]));
        let mut found: Vec<(u64, String)> = (0..nsyms)
            .map(|i| symoff + i * NLIST_LEN)
            .filter(|&entry| {
                let n_type = image.get(entry + 4).copied().unwrap_or_default();
                let n_sect = image.get(entry + 5).copied().unwrap_or_default();
                n_type & N_STAB == 0
                    && n_type & N_TYPE == N_SECT
                    && usize::from(n_sect) == section.number
            })
            .map(|entry| {
                let name = strings
                    .get(u32_at(image, entry) as usize..)
                    .expect("a thread-local symbol names a string in the string table");
                let end = name
                    .iter()
                    .position(|&b| b == 0)
                    .expect("a thread-local symbol's name is NUL terminated");
                let name = String::from_utf8_lossy(&name[..end]);
                let addr = u64_at(image, entry + 8);
                assert!(
                    section.addr <= addr && addr <= section.end(),
                    "thread-local symbol {name} at {addr:#x} is outside {} address bounds \
                     [{:#x}, {:#x}]",
                    section.name,
                    section.addr,
                    section.end(),
                );
                (addr, path_of(&name))
            })
            .collect();
        found.sort();
        found.dedup_by_key(|(addr, _)| *addr);
        assert!(
            section.size == 0 || found.iter().any(|(addr, _)| *addr < section.end()),
            "{} has {} TLS bytes but no named storage, so largest-local diagnostics are \
             unavailable; the full template still occupies {} bytes against the \
             {TEMPLATE_BUDGET_BYTES}-byte budget",
            section.name,
            section.size,
            template.bytes(),
        );
        let first = found.first().map_or(section.end(), |(addr, _)| *addr);
        if first > section.addr {
            sizes.unnamed.push((
                first - section.addr,
                format!("{} [{:#x}, {first:#x})", section.name, section.addr),
            ));
        }
        let ends = found.iter().skip(1).map(|(addr, _)| *addr);
        let ends = ends.chain([section.end()]);
        for ((addr, name), end) in found.iter().zip(ends) {
            sizes.named.push((end - addr, name.clone()));
        }
    }
    for pair in template.sections.windows(2) {
        let (end, start) = (pair[0].end(), pair[1].addr);
        if start > end {
            sizes.unnamed.push((
                start - end,
                format!(
                    "between {} and {} [{end:#x}, {start:#x})",
                    pair[0].name, pair[1].name
                ),
            ));
        }
    }
    sizes.named.sort();
    sizes.named.reverse();
    sizes
}

/// Inspect the compiler-built image Cargo names and account for its complete TLS template.
pub(super) fn assert_binary_template_stays_small(binary: &str) {
    let exe = aterm_tempfile::bin_exe(binary);
    let path = exe.display();
    let image = std::fs::read(&exe).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let template = thread_local_template(&image);
    let (data, bss) = (template.data, template.bss);
    println!("template: {} bytes", template.bytes());
    assert!(template.bytes() > 0, "fixture: {path} has thread-locals");
    let sized = thread_locals_by_size(&image, &template);
    assert!(
        !sized.named.is_empty(),
        "fixture: {path}'s symbol table names its thread-locals, so a report can say which to move"
    );
    let named: u64 = sized.named.iter().map(|(bytes, _)| bytes).sum();
    let unnamed: u64 = sized.unnamed.iter().map(|(bytes, _)| bytes).sum();
    let unnamed_spans = sized
        .unnamed
        .iter()
        .map(|(bytes, span)| format!("\n  {bytes:>6}  {span}"))
        .collect::<String>();
    println!("unnamed template bytes: {unnamed}{unnamed_spans}");
    assert_eq!(
        named + unnamed,
        template.bytes(),
        "fixture: {path}'s {} named thread-local extents ({named} bytes) and unnamed spans \
         ({unnamed} bytes) cover the template (regular sections {data}, zero-filled {bss})",
        sized.named.len()
    );
    let largest = sized
        .named
        .iter()
        .take(NAMED)
        .map(|(bytes, name)| format!("\n  {bytes:>6}  {name}"))
        .collect::<String>();
    assert!(
        template.bytes() <= TEMPLATE_BUDGET_BYTES,
        "a thread's copy of {path}'s thread-locals is {} bytes (regular sections {data}, \
         zero-filled {bss}), over {TEMPLATE_BUDGET_BYTES}: a large value in a thread_local! \
         belongs behind a Box. The {NAMED} largest of its {} named extents:{largest}\n\
         Unnamed template spans ({unnamed} bytes):{unnamed_spans}",
        template.bytes(),
        sized.named.len()
    );
}
