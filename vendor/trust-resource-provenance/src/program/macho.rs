use super::*;

pub(super) struct Commands {
    pub loads: Vec<String>,
    pub rpaths: Vec<String>,
}

fn word(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let bytes = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| refused("truncated native Mach-O word"))?;
    Ok(u32::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| refused("invalid native Mach-O word"))?,
    ))
}

fn text(command: &[u8], field: usize, minimum: usize) -> io::Result<String> {
    let offset = word(command, field)? as usize;
    if offset < minimum {
        return Err(refused("Mach-O string overlaps command fields"));
    }
    let bytes = command
        .get(offset..)
        .ok_or_else(|| refused("Mach-O string offset is outside command"))?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| refused("unterminated Mach-O string"))?;
    if end == 0 || end > MAX_PATH {
        return Err(refused("Mach-O path exceeds native profile bound"));
    }
    let text =
        std::str::from_utf8(&bytes[..end]).map_err(|_| refused("Mach-O path is not UTF8"))?;
    if text.chars().any(char::is_control) {
        return Err(refused("Mach-O path contains controls"));
    }
    Ok(text.to_owned())
}

pub(super) fn system(path: &str) -> bool {
    (path.starts_with("/usr/lib/") || path.starts_with("/System/Library/"))
        && path
            .split('/')
            .skip(1)
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub(super) fn commands(bytes: &[u8], executable: bool) -> io::Result<Commands> {
    parse_commands(bytes, executable, false)
}

// Only the produced-input owner may use this parser. It must bind every
// expanded search root to its original SDK/output descriptions before use.
pub(super) fn produced_commands(bytes: &[u8], executable: bool) -> io::Result<Commands> {
    parse_commands(bytes, executable, true)
}

fn parse_commands(bytes: &[u8], executable: bool, produced: bool) -> io::Result<Commands> {
    parse_arch_commands(bytes, executable, produced, 0x0100_000c)
}

fn parse_arch_commands(
    bytes: &[u8],
    executable: bool,
    produced: bool,
    architecture: u32,
) -> io::Result<Commands> {
    if word(bytes, 0)? != 0xfeed_facf
        || word(bytes, 4)? != architecture
        || word(bytes, 12)? != if executable { 2 } else { 6 }
    {
        return Err(refused(
            "native metadata requires a thin arm64 Mach-O executable/dylib",
        ));
    }
    let count = word(bytes, 16)? as usize;
    let size = word(bytes, 20)? as usize;
    if count == 0 || count > MAX_COMMANDS || size > MAX_COMMAND_BYTES || count > size / 8 {
        return Err(refused("native Mach-O command population exceeds bound"));
    }
    let end = 32usize
        .checked_add(size)
        .ok_or_else(|| refused("native Mach-O command size overflow"))?;
    let bytes = bytes
        .get(..end)
        .ok_or_else(|| refused("truncated native Mach-O commands"))?;
    let mut offset = 32;
    let mut loads = Vec::new();
    let mut rpaths = Vec::new();
    let mut loader = 0;
    for _ in 0..count {
        let kind = word(bytes, offset)?;
        let size = word(bytes, offset + 4)? as usize;
        if size < 8 || size % 8 != 0 {
            return Err(refused("invalid native Mach-O command length"));
        }
        let next = offset
            .checked_add(size)
            .ok_or_else(|| refused("native Mach-O offset overflow"))?;
        let command = bytes
            .get(offset..next)
            .ok_or_else(|| refused("native Mach-O command extends past header"))?;
        match kind {
            // Every load form, including weak/lazy/reexport/upward, participates
            // in the closed runtime. Missing weak foreign loads refuse too.
            0xc | 0x8000_0018 | 0x8000_001f | 0x20 | 0x8000_0023 => {
                if size < 24 {
                    return Err(refused("truncated native dylib command"));
                }
                loads.push(text(command, 8, 24)?);
            }
            0xd => {
                let _ = text(command, 8, 24)?;
            }
            0xe => {
                if !executable || text(command, 8, 12)? != "/usr/lib/dyld" {
                    return Err(refused("native Mach-O uses an unsupported loader"));
                }
                loader += 1;
            }
            0x8000_001c => {
                let path = text(command, 8, 12)?;
                if !produced
                    && !matches!(
                        path.as_str(),
                        "@loader_path" | "@loader_path/../lib" | "@executable_path/../lib"
                    )
                {
                    return Err(refused(
                        "native Mach-O has an unsupported runtime search path",
                    ));
                }
                rpaths.push(path);
            }
            // Header, address, symbol, signature, platform and link-edit data.
            // Unknown commands are not assumed harmless. Environment, legacy
            // executable/library and encrypted-image commands are unsupported.
            0x2 | 0xb | 0x19 | 0x1a | 0x1b | 0x1d | 0x1e | 0x22 | 0x8000_0022 | 0x24 | 0x26
            | 0x8000_0028 | 0x29 | 0x2a | 0x2b | 0x2d | 0x2e | 0x32 | 0x8000_0033 | 0x8000_0034
            | 0x36 => {}
            _ => {
                return Err(refused(
                    "native Mach-O load command has no admitted metadata profile",
                ));
            }
        }
        offset = next;
    }
    if offset != end || loader != usize::from(executable) {
        return Err(refused(
            "native Mach-O command census or loader cardinality differs",
        ));
    }
    Ok(Commands { loads, rpaths })
}

/// The system-command profile checks EVERY supported slice, never guesses the
/// kernel's preferred arm64 subtype or drops a possible translated runtime.
/// Ordinary metadata and produced-input profiles remain thin arm64 only.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(super) fn system_command_commands(
    original: &File,
    header: &[u8],
    length: u64,
    deadline: Instant,
) -> io::Result<Commands> {
    let mut read = |offset, bytes: &mut [u8]| {
        time(deadline)?;
        let mut at = 0;
        while at < bytes.len() {
            time(deadline)?;
            match original.read_at(&mut bytes[at..], offset + at as u64) {
                Ok(0) => return Err(refused("universal command shortened during binding")),
                Ok(n) => at += n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        time(deadline)
    };
    system_commands(header, length, &mut read)
}

#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
fn system_commands(
    header: &[u8],
    length: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<Commands> {
    let be32 = |bytes: &[u8]| -> io::Result<u32> {
        Ok(u32::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| refused("short universal word"))?,
        ))
    };
    let magic = be32(
        header
            .get(..4)
            .ok_or_else(|| refused("short native command"))?,
    )?;
    if magic != 0xcafe_babe && magic != 0xcafe_babf {
        return commands(header, true);
    }
    let count = be32(
        header
            .get(4..8)
            .ok_or_else(|| refused("short universal header"))?,
    )? as usize;
    if count == 0 || count > 16 {
        return Err(refused(
            "universal system command slice count exceeds bound",
        ));
    }
    let stride = if magic == 0xcafe_babf { 32 } else { 20 };
    let table_end = 8 + count * stride;
    if table_end as u64 > length {
        return Err(refused("universal system command table exceeds file"));
    }
    let mut table = vec![0; count * stride];
    read(8, &mut table)?;
    let mut ranges = Vec::new();
    let mut combined = Commands {
        loads: Vec::new(),
        rpaths: Vec::new(),
    };
    let mut native = false;
    for row in table.chunks_exact(stride) {
        let cpu = be32(&row[..4])?;
        let subtype = be32(&row[4..8])?;
        if !matches!(cpu, 0x0100_000c | 0x0100_0007) {
            return Err(refused(
                "universal system command has an unsupported architecture",
            ));
        }
        let wide = |bytes: &[u8]| -> io::Result<u64> {
            Ok(u64::from_be_bytes(
                bytes
                    .try_into()
                    .map_err(|_| refused("short universal wide word"))?,
            ))
        };
        let (offset, size, align) = if stride == 32 {
            if be32(&row[28..32])? != 0 {
                return Err(refused(
                    "universal system command reserved bits are nonzero",
                ));
            }
            (wide(&row[8..16])?, wide(&row[16..24])?, be32(&row[24..28])?)
        } else {
            (
                u64::from(be32(&row[8..12])?),
                u64::from(be32(&row[12..16])?),
                be32(&row[16..20])?,
            )
        };
        let end = offset
            .checked_add(size)
            .ok_or_else(|| refused("universal slice overflow"))?;
        if offset < table_end as u64
            || size < 32
            || end > length
            || align > 63
            || offset % (1u64 << align.min(63)) != 0
            || ranges.iter().any(|&(other, other_subtype, start, stop)| {
                (cpu, subtype) == (other, other_subtype) || offset < stop && start < end
            })
        {
            return Err(refused(
                "universal command slices overlap or contradict their table",
            ));
        }
        ranges.push((cpu, subtype, offset, end));
        let mut fixed = [0; 32];
        read(offset, &mut fixed)?;
        if word(&fixed, 4)? != cpu || word(&fixed, 8)? != subtype {
            return Err(refused(
                "universal command architecture differs from its slice",
            ));
        }
        let command_bytes = word(&fixed, 20)? as usize;
        if command_bytes > MAX_COMMAND_BYTES || 32 + command_bytes as u64 > size {
            return Err(refused(
                "universal command load table exceeds its slice or bound",
            ));
        }
        let mut bytes = vec![0; 32 + command_bytes];
        bytes[..32].copy_from_slice(&fixed);
        read(offset + 32, &mut bytes[32..])?;
        let parsed = parse_arch_commands(&bytes, true, false, cpu)?;
        // Validate the system-only profile on ALL slices, including foreign
        // ones. Whole-file hashing and actual loaded-file identity remain owed
        // by HeldProgram; a parsed table is never an execution capability.
        if !parsed.rpaths.is_empty() || parsed.loads.iter().any(|load| !system(load)) {
            return Err(refused(
                "universal command requires a non-system runtime search",
            ));
        }
        combined.loads.extend(parsed.loads);
        native |= cpu == 0x0100_000c;
    }
    if !native {
        return Err(refused(
            "universal system command lacks a native arm64 slice",
        ));
    }
    Ok(combined)
}

#[cfg(test)]
#[path = "macho_system_command_tests.rs"]
mod system_command_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: u32, text: &str, fixed: usize) -> Vec<u8> {
        let length = (fixed + text.len() + 1).next_multiple_of(8);
        let mut out = vec![0u8; length];
        out[..4].copy_from_slice(&kind.to_le_bytes());
        out[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        out[8..12].copy_from_slice(&(fixed as u32).to_le_bytes());
        out[fixed..fixed + text.len()].copy_from_slice(text.as_bytes());
        out
    }
    fn image(commands: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0u8; 32];
        for (offset, value) in [
            (0, 0xfeed_facfu32),
            (4, 0x0100_000c),
            (12, 2),
            (16, commands.len() as u32),
            (20, commands.iter().map(Vec::len).sum::<usize>() as u32),
        ] {
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        out.extend(commands.iter().flatten());
        out
    }
    #[test]
    fn all_native_dependency_load_forms_are_included() {
        let mut cmds = vec![
            command(0xe, "/usr/lib/dyld", 12),
            command(0x8000_001c, "@loader_path/../lib", 12),
        ];
        for kind in [0xc, 0x8000_0018, 0x8000_001f, 0x20, 0x8000_0023] {
            cmds.push(command(kind, "@rpath/runtime.dylib", 24));
        }
        assert_eq!(
            commands(&image(&cmds), true).unwrap().loads,
            vec!["@rpath/runtime.dylib"; 5]
        );
    }
    #[test]
    fn produced_paths_require_the_separate_bound_root_profile() {
        let cmds = [
            command(0xe, "/usr/lib/dyld", 12),
            command(0x8000_001c, "/original/output/deps", 12),
        ];
        let bytes = image(&cmds);
        assert!(
            commands(&bytes, true).is_err(),
            "metadata keeps its original closed profile"
        );
        assert_eq!(
            produced_commands(&bytes, true).unwrap().rpaths,
            ["/original/output/deps"]
        );
        for extra in [
            command(0x27, "DYLD_INSERT_LIBRARIES=/foreign", 12),
            command(0x35, "unknown", 12),
        ] {
            assert!(produced_commands(&image(&[cmds[0].clone(), extra]), true).is_err());
        }
    }
    #[test]
    fn malformed_or_unadmitted_native_loader_inputs_refuse() {
        let loader = command(0xe, "/usr/lib/dyld", 12);
        for extra in [
            command(0x27, "DYLD_INSERT_LIBRARIES=/foreign", 12),
            command(0xe, "/other/dyld", 12),
            command(0x8000_001c, "/foreign", 12),
            command(0x35, "unknown", 12),
            loader.clone(),
        ] {
            assert!(commands(&image(&[loader.clone(), extra]), true).is_err());
        }
        let original = image(&[loader]);
        for end in 0..original.len() {
            assert!(commands(&original[..end], true).is_err());
        }
        for (offset, value) in [
            (0, 0xcafe_babeu32),
            (4, 0x0100_0007),
            (12, 6),
            (16, u32::MAX),
            (20, u32::MAX),
            (36, 0),
            (40, 0),
        ] {
            let mut altered = original.clone();
            altered[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(
                commands(&altered, true).is_err(),
                "accepted changed word at {offset}"
            );
        }
        for bad in [
            "/usr/lib/../foreign",
            "/usr/library/other",
            "/System/Library//other",
            "/System/Library/./other",
            "@rpath/libSystem.B.dylib",
        ] {
            assert!(!system(bad));
        }
    }
}
