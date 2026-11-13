// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use atpkg::image::ProgramTag;
use atpkg::vendor_direct::Version;

/// The group book holds one token per group and only the fence's marks: an `exec` in a
/// group's leader re-notes it, and a mark the fence dropped (by a cut or a retirement)
/// takes its group with it, so the book never outgrows the fence's eight marks.
#[test]
fn the_group_book_holds_only_the_fences_marks() {
    let mut groups = DrawerGroups::default();
    groups.note(Some((10, 1)), &[1]);
    groups.note(Some((10, 2)), &[2]);
    assert_eq!(
        (groups.led_by(10), groups.0.len()),
        (Some(2), 1),
        "the exec'd image"
    );
    groups.note(Some((11, 3)), &[2, 3]);
    groups.note(Some((1, 4)), &[2, 3, 4]);
    assert_eq!(groups.led_by(1), None, "no group is led by pid 1");
    for token in 5..40 {
        groups.note(Some((10 + token as i32, token)), &[3, token]);
    }
    assert_eq!(groups.0.len(), 2, "only the marks: {:?}", groups.0);
    assert_eq!((groups.led_by(11), groups.led_by(10)), (Some(3), None));
    let mut notes = GroupNotes::default();
    assert!(notes.is_empty());
    notes.cut(Some((50, 50)), [39, 50].into_iter());
    notes.cut(None, [50].into_iter());
    notes.apply(&mut groups);
    assert_eq!(
        groups.0,
        [(50, 50)],
        "a hold's cuts apply in order, the last fence's marks kept"
    );
}

fn image(pgid: i32, start: u64, exe: &str, program: &str, source: ImageSource) -> HolderImage {
    HolderImage::of(
        pgid,
        Some(start),
        Some(PathBuf::from(exe)),
        ImageBuild {
            program: ProgramTag::new(program),
            build: 0,
            source,
        },
        0,
    )
}

#[test]
fn the_token_names_one_image() {
    let build = ImageBuild::unknown(ProgramTag::new("claude"));
    let t = token_of(100, 5, Path::new("/s/claude"), &build);
    assert_eq!(t, token_of(100, 5, Path::new("/s/claude"), &build));
    assert_ne!(
        t,
        token_of(100, 6, Path::new("/s/claude"), &build),
        "pid reuse"
    );
    assert_ne!(
        t,
        token_of(100, 5, Path::new("/s/other"), &build),
        "another path"
    );
    assert_ne!(t, token_of(101, 5, Path::new("/s/claude"), &build));
    let launcher = ImageBuild {
        source: ImageSource::Launcher,
        ..build.clone()
    };
    assert_ne!(t, token_of(100, 5, Path::new("/s/claude"), &launcher));
    let blind = HolderImage::of(
        100,
        None,
        Some(PathBuf::from("/s/claude")),
        ImageBuild::unknown(ProgramTag::new("claude")),
        0,
    );
    assert_eq!(blind.token, None, "no start, no token");
    let zero = HolderImage::of(
        100,
        Some(0),
        Some(PathBuf::from("/s/claude")),
        ImageBuild::unknown(None),
        0,
    );
    assert_eq!(zero.token, None, "a hash with start = 0 is never formed");
    let read =
        |exe: &str| HolderImage::of(100, Some(5), Some(PathBuf::from(exe)), build.clone(), 0);
    let unlinked = read("/s/versions/2.1.304 (deleted)");
    #[cfg(target_os = "linux")]
    {
        assert_eq!(unlinked.token, read("/s/versions/2.1.304").token);
        assert_eq!(&*unlinked.exe, Path::new("/s/versions/2.1.304"));
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_ne!(unlinked.token, read("/s/versions/2.1.304").token);
        assert_eq!(&*unlinked.exe, Path::new("/s/versions/2.1.304 (deleted)"));
    }
}

#[test]
fn a_launcher_is_never_a_drawer() {
    let store = image(
        1,
        1,
        "/p/store/claude/1/bin/claude",
        "claude",
        ImageSource::Store,
    );
    assert_eq!(store.drawer, Some(CardProgram::Claude));
    assert_eq!(store.launcher_for, None);
    let shim = image(1, 1, "/bin/sh", "claude", ImageSource::Launcher);
    assert_eq!(shim.drawer, None);
    assert_eq!(shim.launcher_for, Some(CardProgram::Claude));
    let muse = image(
        1,
        1,
        "/x/muse-bin-1.4.10-R6427.1",
        "muse-bin-1.4.10-R6427.1",
        ImageSource::Unknown,
    );
    assert_eq!(muse.drawer, Some(CardProgram::Muse));
    let zsh = image(1, 1, "/bin/zsh", "zsh", ImageSource::Unknown);
    assert!(!zsh.card_relevant());
    let codex = image(1, 1, "/x/codex", "codex", ImageSource::Unknown);
    assert!(!codex.card_relevant(), "codex draws no card");
}

#[test]
fn publication_is_by_relevance_and_deduplicated_by_token() {
    let shell = image(10, 1, "/bin/zsh", "zsh", ImageSource::Unknown);
    let ls = image(20, 2, "/bin/ls", "ls", ImageSource::Unknown);
    let claude = image(
        30,
        3,
        "/p/store/claude/1/bin/claude",
        "claude",
        ImageSource::Store,
    );
    let shim = image(30, 3, "/bin/sh", "claude", ImageSource::Launcher);
    assert_eq!(decide(None, &shell), Offer::Irrelevant);
    assert_eq!(decide(Some(&shell), &ls), Offer::Irrelevant);
    assert_eq!(decide(Some(&shell), &claude), Offer::Published, "entering");
    assert_eq!(decide(Some(&claude), &claude.clone()), Offer::Same);
    assert_eq!(decide(Some(&claude), &shell), Offer::Published, "leaving");
    assert_eq!(decide(Some(&shell), &shim), Offer::Published, "a launcher");
    assert_eq!(decide(Some(&shim), &claude), Offer::Published, "its exec");
    let untracked = HolderImage::unread(30);
    assert_eq!(
        decide(Some(&claude), &untracked),
        Offer::Published,
        "a gone holder leaves"
    );
    assert_eq!(
        decide(Some(&untracked), &untracked.clone()),
        Offer::Irrelevant
    );

    let agy_shim = image(30, 3, "/bin/sh", "agy", ImageSource::Launcher);
    assert_ne!(shim.token, agy_shim.token);
    assert_eq!(decide(Some(&shim), &agy_shim), Offer::Published);
    let mut changed = claude.clone();
    changed.build.build = Version::new(2, 1, 304).unwrap().build_id();
    changed.token = Some(token_of(30, 3, &changed.exe, &changed.build));
    assert_eq!(decide(Some(&claude), &changed), Offer::Published);
    assert!(changed.matches_card(3, "2.1.304"));
    assert!(!changed.matches_card(4, "2.1.304"), "another drawer");
    assert!(!changed.matches_card(3, "2.1.303"), "another build");
    assert!(!claude.matches_card(3, "2.1.304"), "unknown build");
}

#[test]
fn the_byte_arm_keeps_an_unresolved_command_and_stops_at_a_drawer() {
    use std::cell::Cell;
    let shell = image(10, 1, "/bin/zsh", "zsh", ImageSource::Unknown);
    let drawer = image(
        10,
        1,
        "/store/claude/1/bin/claude",
        "claude",
        ImageSource::Store,
    );
    let calls = Cell::new(0usize);
    let read = |image: &HolderImage| {
        calls.set(calls.get() + 1);
        image.clone()
    };
    let observe = |arm: &mut ByteArm, image: &HolderImage| {
        let image = arm.read_due(image.pgid, |_, _| read(image));
        if let Some(image) = &image {
            arm.note_read(&Arc::new(image.clone()));
        }
        image
    };
    let mut arm = ByteArm::default();
    arm.begin_batch();
    assert_eq!(
        observe(&mut arm, &drawer),
        Some(drawer.clone()),
        "initial -e"
    );
    for _ in 0..1_000 {
        arm.begin_batch();
        assert!(observe(&mut arm, &drawer).is_none());
    }
    assert_eq!(calls.get(), 1, "no settled drawer probes");

    arm.note_shell(true, true);
    assert_eq!(observe(&mut arm, &shell), Some(shell.clone()));
    for _ in 0..8 {
        arm.begin_batch();
        assert!(observe(&mut arm, &shell).is_some());
        assert!(observe(&mut arm, &shell).is_none(), "once per batch");
    }
    arm.begin_batch();
    assert_eq!(
        observe(&mut arm, &drawer),
        Some(drawer.clone()),
        "late exec"
    );
    let resolved = calls.get();
    for _ in 0..1_000 {
        arm.begin_batch();
        assert!(observe(&mut arm, &drawer).is_none());
    }
    assert_eq!(calls.get(), resolved);
    arm.note_shell(true, true);
    arm.note_shell(false, false);
    arm.begin_batch();
    assert!(observe(&mut arm, &shell).is_none(), "signed completion");

    let shim = image(10, 1, "/bin/sh", "claude", ImageSource::Launcher);
    arm.note_read(&Arc::new(shim.clone()));
    for _ in 0..8 {
        arm.begin_batch();
        assert!(
            observe(&mut arm, &shim).is_some(),
            "launcher remains unresolved"
        );
        assert!(observe(&mut arm, &shim).is_none());
    }
    arm.begin_batch();
    assert_eq!(observe(&mut arm, &drawer), Some(drawer));
}

#[test]
fn the_arm_gives_its_read_the_groups_last_image() {
    let shell = Arc::new(image(10, 1, "/bin/zsh", "zsh", ImageSource::Unknown));
    let given = std::cell::RefCell::new(Vec::new());
    let read = |pgid: i32, last: Option<&HolderImage>| {
        given.borrow_mut().push(last.cloned());
        HolderImage::unread(pgid)
    };
    let mut arm = ByteArm::default();
    arm.begin_batch();
    assert!(arm.read_due(10, read).is_some());
    arm.note_read(&shell);
    arm.note_shell(true, true);
    assert!(arm.read_due(10, read).is_some(), "the boundary read");
    arm.note_read(&shell);
    arm.note_shell(true, true);
    assert!(arm.read_due(11, read).is_some(), "another group's");
    assert_eq!(
        *given.borrow(),
        vec![None, Some((*shell).clone()), None],
        "nothing noted yet; the group's own last; never another group's"
    );
}

#[test]
// Where a process's start is read ([`start_us_of`]); elsewhere no image is tracked.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn a_reread_of_an_unchanged_process_keeps_its_image() {
    let pid = i32::try_from(std::process::id()).expect("a pid");
    let fresh = read(pid, None);
    assert!(
        fresh.token.is_some(),
        "PRECONDITION: this process is readable"
    );
    assert!(fresh.launcher_for.is_none(), "PRECONDITION: not a launcher");
    let kept = HolderImage {
        build: ImageBuild {
            program: atpkg::image::ProgramTag::new("sh"),
            build: 0,
            source: ImageSource::Launcher,
        },
        launcher_for: Some(CardProgram::Claude),
        read_ns: 0,
        ..fresh.clone()
    };
    let again = read(pid, Some(&kept));
    assert!(again.same_image(&kept), "kept: {again:?}");
    assert!(again.read_ns > 0, "re-stamped with this read");
    for (why, last) in [
        (
            "another start",
            HolderImage {
                start_us: kept.start_us + 1,
                ..kept.clone()
            },
        ),
        (
            "another executable",
            HolderImage {
                exe: Arc::from(Path::new("/bin/zsh")),
                ..kept.clone()
            },
        ),
        (
            "another group",
            HolderImage {
                pgid: pid + 1,
                ..kept.clone()
            },
        ),
        (
            "untracked",
            HolderImage {
                token: None,
                ..kept.clone()
            },
        ),
    ] {
        assert!(
            read(pid, Some(&last)).same_image(&fresh),
            "{why}: read afresh"
        );
    }
}

#[test]
fn a_kept_image_reads_the_executable_as_its_token_does() {
    let last = image(10, 1, "/s/versions/2.1.304", "claude", ImageSource::Native);
    let unlinked = Path::new("/s/versions/2.1.304 (deleted)");
    assert!(last.is_process(10, Some(1), Path::new("/s/versions/2.1.304")));
    let same_token = HolderImage::of(
        10,
        Some(1),
        Some(unlinked.to_path_buf()),
        last.build.clone(),
        0,
    )
    .same_image(&last);
    assert_eq!(same_token, cfg!(target_os = "linux"));
    assert_eq!(last.is_process(10, Some(1), unlinked), same_token);
}

#[test]
fn the_inbox_queues_each_session_once() {
    let inbox = EdgeInbox::default();
    assert!(inbox.is_empty());
    inbox.clone().push(7);
    inbox.push(9);
    inbox.push(7);
    assert_eq!(inbox.take(), vec![7, 9]);
    assert!(inbox.is_empty());
}

fn cuts(scan: &mut CScan, batch: &[u8]) -> Vec<usize> {
    let mut ends = Vec::new();
    scan.ends(batch, &mut ends);
    ends
}

#[test]
fn a_slice_ends_right_after_each_complete_c() {
    let mut scan = CScan::default();
    let c = b"\x1b]133;C;id=ab\x07";
    let batch = [&b"ls\r\n"[..], c, b"out", b"\x1b]633;C\x1b\\", b"x"].concat();
    assert_eq!(
        cuts(&mut scan, &batch),
        vec![4 + c.len(), 4 + c.len() + 3 + 9]
    );
    assert_eq!(cuts(&mut CScan::default(), b"\x1b]133;CX\x07"), vec![9]);
    for no in [
        &b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;D;0\x07"[..],
        b"\x1b]133;C;id=\x18x",
        b"\x1b]1337;C\x07",
        b"\x1b[31m\x1b]0;133;C\x07",
    ] {
        assert_eq!(
            cuts(&mut CScan::default(), no),
            Vec::<usize>::new(),
            "{no:?}"
        );
    }
    let mut scan = CScan::default();
    assert!(cuts(&mut scan, b"abc\x1b]133;C;id=0").is_empty());
    assert_eq!(cuts(&mut scan, b"123\x07next"), vec![4]);
    let mut scan = CScan::default();
    assert!(cuts(&mut scan, b"abc\x1b]13").is_empty());
    assert_eq!(cuts(&mut scan, b"3;C\x07"), vec![4]);
    let mut scan = CScan::default();
    assert!(cuts(&mut scan, b"\x1b]13").is_empty());
    assert_eq!(cuts(&mut scan, b"4;x\x07\x1b]133;C\x07"), vec![12]);
    let mut scan = CScan::default();
    assert!(cuts(&mut scan, b"abc\x1b").is_empty());
    assert_eq!(cuts(&mut scan, b"]133;C\x07z"), vec![7]);
    let noisy = [&b"a[1] \x1b[31mred\x1b[0m ]]"[..], c].concat();
    assert_eq!(cuts(&mut CScan::default(), &noisy), vec![noisy.len()]);
    for pad in 0..70 {
        let batch = [vec![b'.'; pad], c.to_vec()].concat();
        assert_eq!(
            cuts(&mut CScan::default(), &batch),
            vec![pad + c.len()],
            "pad {pad}"
        );
    }
    let arm = ByteArm {
        ends: vec![2, 7, 9],
        ..ByteArm::default()
    };
    assert_eq!(arm.next_cut(0), Some(2));
    assert_eq!(arm.next_cut(2), Some(7));
    assert_eq!(arm.next_cut(8), Some(9));
    assert_eq!(arm.next_cut(9), None);
    assert_eq!(arm.next_cut(usize::MAX), None);
}

/// The oracle: where the engine's parser dispatches each OSC the engine
/// takes as a 133/633 `C` (`TerminalHandler::osc_dispatch_inner`'s command
/// number, `parse_shell_osc`'s first character), byte by byte.
#[derive(Default)]
struct EngineCs {
    at: usize,
    dispatched: Vec<usize>,
}

impl aterm_parser::ActionSink for EngineCs {
    fn print(&mut self, _: char) {}
    fn execute(&mut self, _: u8) {}
    fn csi_dispatch(
        &mut self,
        _: &aterm_provenance::Provenance<[u16], aterm_provenance::Pty>,
        _: &aterm_provenance::Provenance<[u8], aterm_provenance::Pty>,
        _: u8,
    ) {
    }
    fn osc_dispatch(
        &mut self,
        params: &aterm_provenance::Provenance<[&[u8]], aterm_provenance::Pty>,
    ) {
        let params = params.as_ref();
        let number = params
            .first()
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|s| s.parse::<u32>().ok());
        let code = params
            .get(1)
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|s| s.chars().next());
        if matches!(number, Some(133 | 633)) && code == Some('C') {
            self.dispatched.push(self.at);
        }
    }
}

fn engine_cs(stream: &[u8]) -> Vec<(usize, usize)> {
    let mut parser = aterm_parser::Parser::new();
    let mut sink = EngineCs::default();
    for (i, b) in stream.iter().enumerate() {
        sink.at = i;
        parser.advance_fast(std::slice::from_ref(b), &mut sink);
    }
    sink.dispatched
        .into_iter()
        .map(|d| {
            let st = stream[d] == 0x1b && stream.get(d + 1) == Some(&b'\\');
            (d, d + if st { 2 } else { 1 })
        })
        .collect()
}

fn scanned(stream: &[u8], batches: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut scan = CScan::default();
    let (mut cuts, mut bounds, mut ends) = (Vec::new(), Vec::new(), Vec::new());
    let mut at = 0;
    for &n in batches {
        let batch = &stream[at..(at + n).min(stream.len())];
        scan.ends(batch, &mut ends);
        assert!(
            ends.windows(2).all(|w| w[0] < w[1])
                && ends.iter().all(|&e| (1..=batch.len()).contains(&e)),
            "cuts ascend within their batch: {ends:?} in {}",
            batch.len()
        );
        cuts.extend(ends.iter().map(|e| at + e));
        at += batch.len();
        bounds.push(at);
        if at == stream.len() {
            break;
        }
    }
    (cuts, bounds)
}

fn law(stream: &[u8], batches: &[usize]) -> Vec<String> {
    let cs = engine_cs(stream);
    let (cuts, bounds) = scanned(stream, batches);
    let mut out = Vec::new();
    for &(d, t) in &cs {
        if !cuts.iter().chain(&bounds).any(|&p| d < p && p <= t) {
            out.push(format!("the C dispatched at {d} ends no slice by {t}"));
        }
    }
    for &p in &cuts {
        if !cs.iter().any(|&(d, t)| d < p && p <= t) {
            out.push(format!("a cut at {p} after no C"));
        }
    }
    out
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }

    fn pick<'a>(&mut self, of: &[&'a [u8]]) -> &'a [u8] {
        of[self.below(of.len())]
    }
}

fn stream(rng: &mut Rng) -> Vec<u8> {
    const TEXT: [&[u8]; 8] = [
        b"ls -la\r\n",
        b"a[1] ]] ",
        b"\x1b[31mred\x1b[0m",
        b"\xe6\x9c\xac\xc5\x9c",
        b"\x1b",
        b"\x1b\x1b",
        b"\x1b[",
        b"\\",
    ];
    const HEADS: [&[u8]; 18] = [
        b"133;C",
        b"633;C",
        b"133;C",
        b"133;CX",
        b"0133;C",
        b"+133;C",
        b"1\x013\x1f3;\x02C",
        b"133;A",
        b"133;D;0",
        b"133;",
        b"133",
        b"1337;C",
        b"+;C",
        b"0;title ]",
        b"7;file://h/p",
        b"133;C\xff",
        b"133;\xe2\x82\xac",
        b"4294967429;C",
    ];
    const BODIES: [&[u8]; 6] = [
        b"",
        b";id=ab12",
        b";id=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        b";cmd=]]",
        b";\xff\x01x",
        b";\x9c",
    ];
    const ENDS: [&[u8]; 8] = [
        b"\x07", b"\x1b\\", b"\x1b[0m", b"\x1b", b"\x18", b"\x1a", b"", b"\x07",
    ];
    let mut out = Vec::new();
    for _ in 0..rng.below(12) + 1 {
        if rng.below(3) == 0 {
            out.extend_from_slice(rng.pick(&TEXT));
        } else {
            out.extend_from_slice(b"\x1b]");
            out.extend_from_slice(rng.pick(&HEADS));
            out.extend_from_slice(rng.pick(&BODIES));
            out.extend_from_slice(rng.pick(&ENDS));
        }
    }
    out
}

#[test]
fn the_scan_cuts_after_every_engine_c_however_the_batches_split() {
    let mut failures = Vec::new();
    for seed in 1..=4_000u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let stream = stream(&mut rng);
        let whole = [stream.len()];
        let mut split = Vec::new();
        let mut left = stream.len();
        while left > 0 {
            let n = (rng.below(12) + 1).min(left);
            split.push(n);
            left -= n;
        }
        let bytes = vec![1; stream.len()];
        for batches in [&whole[..], &split, &bytes] {
            let found = law(&stream, batches);
            if !found.is_empty() && failures.len() < 8 {
                failures.push(format!("seed {seed} {batches:?} {stream:?}: {found:?}"));
            }
        }
    }
    assert_eq!(failures, Vec::<String>::new());
}

#[test]
fn the_scan_law_rows() {
    let broken = b"\x1b]133;C;id=\x1b]133;C\x1b\\z";
    assert_eq!(law(broken, &[broken.len()]), Vec::<String>::new());
    assert_eq!(
        cuts(&mut CScan::default(), broken),
        vec![12, broken.len() - 1],
        "the first C ends at the ESC that opens the second"
    );
    let mut scan = CScan::default();
    assert_eq!(cuts(&mut scan, b"\x1b]133;C;id=\x1b"), vec![12]);
    assert_eq!(
        cuts(&mut scan, b"]133;C\x07x"),
        vec![7],
        "the carried ESC's OSC"
    );
    let long = [&b"\x1b]133;C;"[..], &[b'x'; C_MAX], b"\x07z"].concat();
    assert!(
        !law(&long, &[long.len()]).is_empty(),
        "a C past the bound is not cut, and the law sees it"
    );
}
